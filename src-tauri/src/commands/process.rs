use std::collections::VecDeque;
use std::io::BufRead;
use std::time::Duration;
use tauri::{Emitter, Manager, State};
use serde::Serialize;
use crate::AppState;
use crate::core::process::LogLine;
use std::sync::Arc;

/// 工具命令默认执行超时（未配置 timeout_secs 时）：防止长驻命令（npm run dev 等）
/// 永久占用线程池。构建类长任务应在命令上配置 timeout_secs（如 1800）或 0（不限）
const TOOL_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
/// 工具命令输出最多保留的行数（对齐服务日志上限，防止超长输出撑爆 IPC payload 和前端渲染）
const TOOL_CMD_OUTPUT_MAX_LINES: usize = 2000;

/// 运行中的工具命令表项：pid + 进程启动时间。
///
/// 为什么要记启动时间：`stop_tool_command` 只知道 pid，而 pid 会被系统回收——
/// 命令结束与用户点「停止」之间若发生 pid 复用，就会杀掉一个完全无关的进程树。
/// 启动时间（毫秒）在进程存活期间不变，比对它即可确认"还是当初那个进程"。
#[derive(Clone, Copy)]
struct RunningToolCmd {
    pid: u32,
    /// 登记时的进程启动时间；取不到（权限/竞态）时为 None，此时退化为"只按 pid"
    started_at: Option<u64>,
}

/// 运行中的工具命令表（run_id → 进程信息）：供 stop_tool_command 按 run_id 终止进程树，
/// 也是超时兜底杀进程的 pid 来源（不再用 oneshot 单次传递）
fn running_tool_cmds() -> &'static std::sync::Mutex<std::collections::HashMap<String, RunningToolCmd>> {
    static TABLE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, RunningToolCmd>>> =
        std::sync::OnceLock::new();
    TABLE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// 读取运行表中的项（不移除）。超时分支只在阻塞任务真正结束前需要它，
/// 表项的移除交给 RunTableGuard，避免"提前取走 pid → 任务结束时无项可清"
fn peek_running_cmd(run_id: &str) -> Option<RunningToolCmd> {
    running_tool_cmds().lock().ok().and_then(|m| m.get(run_id).copied())
}

/// 运行表生命周期守卫：正常结束、任一早退（`?`）、甚至 panic 都会移除本 run_id 的表项。
///
/// 原实现只在"正常结束"与"超时"两条路径清理，任何 `?` 早退（拿 stdout/stderr 失败、
/// 线程创建失败、wait 失败）都会留下 stale pid；而 `stop_tool_command` 取到 pid 后无存活校验
/// 直接 taskkill —— 过期 PID 一旦被系统回收就会误杀无关进程树。
struct RunTableGuard {
    run_id: String,
}

impl RunTableGuard {
    /// 原子占位：在同一把锁内完成"查重 + 登记"，已存在即失败。
    ///
    /// 为什么要占位而不是 spawn 后再登记：原实现是 spawn **之前**查重、spawn **之后** insert，
    /// 两步之间隔着进程创建（毫秒级），两个并发请求带同一 run_id 时会双双通过查重，
    /// 后者覆盖前者的表项——先启动的那个进程从此失去终止句柄（`timeout_secs: 0` 的长构建
    /// 会一直跑到进程树自己结束）。
    fn reserve(run_id: &str) -> Result<Self, String> {
        let mut table = running_tool_cmds()
            .lock()
            .map_err(|e| format!("工具命令运行表锁中毒: {}", e))?;
        if table.contains_key(run_id) {
            return Err(format!("run_id 已被占用: {}", run_id));
        }
        // pid=0 = 已占位、进程尚未启动；spawn 成功后由 set_pid 补上真实值
        table.insert(run_id.to_string(), RunningToolCmd { pid: 0, started_at: None });
        Ok(Self { run_id: run_id.to_string() })
    }

    /// spawn 成功后补上真实 pid 与启动时间
    fn set_pid(&self, pid: u32) {
        if let Ok(mut table) = running_tool_cmds().lock() {
            // 只在自己仍持有该键时写入（正常情况下必然如此）
            if let Some(entry) = table.get_mut(&self.run_id) {
                entry.pid = pid;
                entry.started_at = crate::core::process::process_start_time_millis(pid);
            }
        }
    }
}

impl Drop for RunTableGuard {
    fn drop(&mut self) {
        if let Ok(mut table) = running_tool_cmds().lock() {
            table.remove(&self.run_id);
        }
    }
}

/// 早退路径的统一收尾：终止并**回收**已 spawn 的子进程，然后返回该错误。
///
/// 为什么需要：`RunTableGuard` 只清运行表项，不负责进程生命周期。spawn 之后的
/// 任一 `?` 早退（取管道失败、读取线程创建失败）会让 `child` 直接 drop——
/// 进程变成"存活但已从运行表摘除"，`stop_tool_command` 再也找不到它。
/// 服务进程路径在同样失败时会 `kill_process_tree`，这里此前没有对应处理。
fn kill_then<T>(child: &mut std::process::Child, msg: String) -> Result<T, String> {
    crate::core::process::kill_and_reap(child, Duration::from_millis(2000));
    Err(msg)
}

/// 日志批量事件的发送端：把 core 的回调接到 Tauri 事件总线上（ARCH-14 的装配点）。
///
/// `core::process` 不再知道 Tauri 的存在——它只收一个"把这一批发出去"的闭包；
/// 发给谁、发失败怎么办，由这一层决定。
fn log_sink(app: &tauri::AppHandle) -> crate::core::process::LogSink {
    let app = app.clone();
    Arc::new(move |payload| {
        // 发送失败必须留痕：前端若尚未注册监听（或窗口已销毁），日志会静默消失，
        // 用户看到的是"日志面板空着"，而控制台一条线索都没有
        if let Err(e) = app.emit("service-log-batch", payload) {
            log::warn!("[nexus] 服务日志批量事件发送失败（前端可能尚未注册监听）: {}", e);
        }
    })
}

/// 工具命令输出的共享通道：两个读取线程写入，一个发射线程取走。
///
/// 从 `run_tool_command` 里提出来（CQ-13：该函数原为 183 行 / 9 层嵌套）——
/// 这组 Arc 以前是闭包捕获的，于是"读取线程怎么建""发射线程怎么建"都嵌在命令体里，
/// 抽成结构体后两者都成了模块级函数，命令体只剩流程本身。
struct ToolCmdOutput {
    /// 待发行缓冲：两个读取线程共写、发射线程独取
    pending: Arc<std::sync::Mutex<Vec<ToolCommandLogLine>>>,
    /// 尚未结束的读取线程数：归零 = 不会再有新行
    readers_alive: Arc<std::sync::atomic::AtomicUsize>,
    /// 跨两个流的全局递增行号：结束时按它合并，还原 stdout/stderr 的真实交错顺序
    seq: Arc<std::sync::atomic::AtomicU64>,
}

/// 一行读取线程：逐行入待发缓冲，并返回本流收集到的 (序号, 行) 供结束合并。
///
/// 只保留最新 `TOOL_CMD_OUTPUT_MAX_LINES` 行（全量收集在超长输出时撑爆 IPC payload）。
fn spawn_stream_reader(
    out: &ToolCmdOutput,
    stream: &'static str,
    pipe: Box<dyn std::io::Read + Send>,
) -> std::io::Result<std::thread::JoinHandle<VecDeque<(u64, String)>>> {
    let seq = Arc::clone(&out.seq);
    let pending = Arc::clone(&out.pending);
    let alive = Arc::clone(&out.readers_alive);
    std::thread::Builder::new()
        .name(format!("nexus-toolcmd-{stream}"))
        .spawn(move || {
            let reader = std::io::BufReader::new(pipe);
            let mut collected: VecDeque<(u64, String)> = VecDeque::with_capacity(TOOL_CMD_OUTPUT_MAX_LINES);
            for line in reader.lines() {
                let Ok(line) = line else { continue };
                let line = crate::core::process::truncate_line(line, 8192);
                let n = seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                // 只入队不 emit：由发射线程按 50ms 批量发（锁中毒也继续发，
                // 缓冲只是输出通道，不该因别的线程 panic 就吞掉已产生的行）
                let mut p = match pending.lock() {
                    Ok(g) => g,
                    Err(e) => e.into_inner(),
                };
                p.push(ToolCommandLogLine { stream: stream.into(), data: line.clone() });
                drop(p);
                if collected.len() >= TOOL_CMD_OUTPUT_MAX_LINES {
                    collected.pop_front();
                }
                collected.push_back((n, line));
            }
            // 递减必须在所有 push 之后（发射线程的收尾判据依赖这个顺序）
            alive.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            collected
        })
}

/// 查询服务信息（name, command, cwd, project_id, env_vars）
fn get_service_info(db: &crate::database::Database, service_id: &str) -> Result<(String, String, String, String, String), String> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT name, command, cwd, project_id, env_vars FROM services WHERE id=?1",
            [service_id],
            |row| Ok((row.get::<_, String>("name")?, row.get::<_, String>("command")?, row.get::<_, String>("cwd")?, row.get::<_, String>("project_id")?, row.get::<_, String>("env_vars")?)),
        )
        .map_err(|e| format!("服务不存在: {}", e))
    })
}

/// 工具命令执行结果（output 为 stdout/stderr 按时间序合并，与 cmd 终端展示一致）
#[derive(Serialize)]
pub struct ToolCommandResult {
    pub success: bool,
    pub output: String,
    pub exit_code: Option<i32>,
}

/// 工具命令输出的单行（批量事件的元素）
#[derive(Clone, Serialize)]
pub struct ToolCommandLogLine {
    pub stream: String,
    pub data: String,
}

/// 工具命令实时输出事件 payload（run_id 用于前端区分并发执行）
///
/// **批量而非逐行**（PERF-11）：一次 `emit` 的完整链路是 serde_json 序列化 → 拼 JS 脚本串
/// → `EventLoopProxy` 跨线程 → 主线程 `ExecuteScript`。逐行发时 5000 行的构建输出 =
/// 5000 次主线程脚本执行；这里与服务日志（`core/process.rs`）同口径按 50ms 一批摊平，
/// IPC 次数从 O(行数) 降到 O(20/s)。前端本就有的 50ms 合帧只合并了 store 写入，
/// 一行都省不掉 IPC——那一半补在这里。
#[derive(Clone, Serialize)]
pub struct ToolCommandLogBatchPayload {
    pub run_id: String,
    pub lines: Vec<ToolCommandLogLine>,
}

/// 运行中服务（含所属项目，供前端按项目维度判断运行状态）
#[derive(Serialize)]
pub struct RunningService {
    pub service_id: String,
    pub project_id: String,
    /// 正在跟随的日志文件（None = 没在跟随）
    ///
    /// 跟着运行状态一起返回，前端就能在已有的 3 秒轮询里拿到"在跟哪个文件"，
    /// 不必为它单独开一条查询：菜单据此显示「取消跟随日志文件」并标出文件名。
    pub followed_log: Option<String>,
}

/// 运行状态总览：运行中 + 意外失败（前端分别渲染运行态与失败态）
#[derive(Serialize)]
pub struct ProcessStatus {
    pub running: Vec<RunningService>,
    pub failed: Vec<crate::core::process::FailedService>,
}

// ─── Tauri Commands ───────────────────────────────────────────

/// 启动单个服务。
///
/// 异步 + spawn_blocking：spawn 进程、绑定 Job Object、建 2 个 reader 线程都是阻塞操作，
/// 且 `start()` 的 TOCTOU 分支可能进入 `cleanup_process`（taskkill + 最长 2s + 2×1s）。
/// 同族的 stop/restart/get_running 早已隔离，只有"启动"这一侧此前遗漏。
#[tauri::command]
pub async fn start_service(app_handle: tauri::AppHandle, service_id: String) -> Result<(), String> {
    if service_id.trim().is_empty() { return Err("服务ID不能为空".into()); }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let (name, command, cwd, project_id, env_vars) = get_service_info(&state.db, &service_id)?;
        let envs = crate::core::process::parse_env_vars(&env_vars)?;
        state.process_mgr.start(crate::core::process::ServiceSpawn {
            project_id: &project_id, service_id: &service_id, name: &name,
            command: &command, cwd: &cwd, env_vars: &envs, log_sink: log_sink(&app_handle),
        }).map_err(|e| format!("服务「{}」{}", name, e))
    }).await.map_err(|e| format!("启动服务任务失败: {}", e))?
}

/// 停止服务（清理含 taskkill + 等待 reader 线程，最长数秒 → 异步执行避免阻塞 IPC）
#[tauri::command]
pub async fn stop_service(app: tauri::AppHandle, service_id: String) -> Result<(), String> {
    if service_id.trim().is_empty() { return Err("服务ID不能为空".into()); }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.process_mgr.stop(&service_id)
    }).await.map_err(|e| format!("停止服务任务失败: {}", e))?
}

/// 取消跟随日志文件（服务本身不受影响）。
///
/// 返回被取消的文件路径：前端要能区分"确实取消了一个跟随"与"本来就没在跟随"，
/// 否则用户点了一下没有任何反馈，不知道是成功了还是没生效。
#[tauri::command]
pub async fn unfollow_service_log(app: tauri::AppHandle, service_id: String) -> Result<Option<String>, String> {
    if service_id.trim().is_empty() { return Err("服务ID不能为空".into()); }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        Ok(state.process_mgr.unfollow_log(&service_id))
    }).await.map_err(|e| format!("取消跟随任务失败: {}", e))?
}

/// 重启服务（stop + start 两段等待 → 异步执行）
#[tauri::command]
pub async fn restart_service(app: tauri::AppHandle, service_id: String) -> Result<(), String> {
    if service_id.trim().is_empty() { return Err("服务ID不能为空".into()); }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let (name, command, cwd, project_id, env_vars) = get_service_info(&state.db, &service_id)?;
        let envs = crate::core::process::parse_env_vars(&env_vars)?;
        state.process_mgr.restart(crate::core::process::ServiceSpawn {
            project_id: &project_id, service_id: &service_id, name: &name,
            command: &command, cwd: &cwd, env_vars: &envs, log_sink: log_sink(&app),
        }).map_err(|e| format!("服务「{}」{}", name, e))
    }).await.map_err(|e| format!("重启服务任务失败: {}", e))?
}

/// 一键启动项目全部已启用服务。
///
/// 异步 + spawn_blocking：本命令会**串行** spawn 每个服务，并且先调 `running()`
/// 为已退出进程收尸（taskkill + 最长 2s + 2×1s per 进程）。同步执行时这段工作
/// 内联在 IPC 请求路径上，会与所有其他 invoke（含 3 秒轮询 get_running）排队，
/// 前端点"全部启动"后整体失去响应。同族的 stop_project_services 早就隔离了。
#[tauri::command]
pub async fn start_project_services(app_handle: tauri::AppHandle, project_id: String) -> Result<Vec<String>, String> {
    if project_id.trim().is_empty() { return Err("项目ID不能为空".into()); }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let services = state.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, name, command, cwd, env_vars FROM services WHERE project_id=?1 AND enabled=1 ORDER BY sort_index"
            ).map_err(|e| format!("查询项目服务列表失败: {}", e))?;
            let rows = stmt.query_map([&project_id], |row| {
                Ok((row.get::<_, String>("id")?, row.get::<_, String>("name")?, row.get::<_, String>("command")?, row.get::<_, String>("cwd")?, row.get::<_, String>("env_vars")?))
            }).map_err(|e| format!("读取项目服务数据失败: {}", e))?;
            let mut svcs = Vec::new();
            for r in rows { svcs.push(r.map_err(|e| format!("解析项目服务数据失败: {}", e))?); }
            Ok::<_, String>(svcs)
        })?;
        let mut errors = Vec::new();
        // 已在运行的服务跳过（start 对已运行返回"已在运行中"，混在 errors 里会误报启动失败）
        let running_ids: std::collections::HashSet<String> = state.process_mgr.running()
            .into_iter().map(|(_, service_id)| service_id).collect();
        for (id, name, cmd, cwd, env_vars) in &services {
            if running_ids.contains(id) { continue; }
            // 环境变量非法同样计入本次批量启动的失败列表（原实现解析失败会静默忽略该服务）
            let envs = match crate::core::process::parse_env_vars(env_vars) {
                Ok(v) => v,
                Err(e) => { errors.push(format!("{}: {}", name, e)); continue; }
            };
            if let Err(e) = state.process_mgr.start(crate::core::process::ServiceSpawn {
                project_id: &project_id, service_id: id, name, command: cmd, cwd,
                env_vars: &envs, log_sink: log_sink(&app_handle),
            }) {
                errors.push(format!("{}: {}", name, e));
            }
        }
        Ok(errors)
    }).await.map_err(|e| format!("启动项目服务任务失败: {}", e))?
}

/// 停止项目全部服务（每服务清理最长数秒 × N → 异步执行避免阻塞 IPC）
#[tauri::command]
pub async fn stop_project_services(app: tauri::AppHandle, project_id: String) -> Result<Vec<String>, String> {
    if project_id.trim().is_empty() { return Err("项目ID不能为空".into()); }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        // 进程 key 为 service_id，需查出项目下所有服务 id 逐个停止
        let ids: Vec<String> = state.db.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT id FROM services WHERE project_id=?1")
                .map_err(|e| format!("查询项目服务失败: {}", e))?;
            let rows = stmt.query_map([&project_id], |row| row.get::<_, String>(0))
                .map_err(|e| format!("读取项目服务失败: {}", e))?;
            let mut ids = Vec::new();
            for r in rows { ids.push(r.map_err(|e| format!("解析项目服务失败: {}", e))?); }
            Ok::<_, String>(ids)
        })?;
        // 逐个停止并**收集失败**：原实现 `let _ = stop(id)` 丢弃错误，命令恒返回成功，
        // 用户看到"已停止"而进程还在跑（与 start_project_services 返回失败列表对齐）
        let mut errors = Vec::new();
        for id in &ids {
            if let Err(e) = state.process_mgr.stop(id) {
                log::warn!("[nexus] 停止服务 {} 失败: {}", id, e);
                errors.push(format!("{}: {}", id, e));
            }
        }
        // 停止项目级文件监听（项目停止 = 总开关，所有服务监听一并关闭）
        if let Err(e) = state.file_watcher.stop_watching(&project_id) {
            log::warn!("[nexus] 停止项目文件监听失败: {}", e);
            errors.push(format!("文件监听: {}", e));
        }
        Ok(errors)
    }).await.map_err(|e| format!("停止项目服务任务失败: {}", e))?
}

/// 运行状态总览：运行中 + 意外失败（前端分别渲染运行态与失败态）
///
/// 异步 + spawn_blocking：`running()` 会为已退出的服务收尸（taskkill + 超时等待，
/// 最坏每进程约 4s），而前端每 3 秒轮询本命令；同步执行会冻结 IPC/事件循环线程。
#[tauri::command]
pub async fn get_running(app: tauri::AppHandle) -> Result<ProcessStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        Ok(ProcessStatus {
            running: state.process_mgr.running().into_iter()
                .map(|(project_id, service_id)| {
                    let followed_log = state.process_mgr.followed_log(&service_id);
                    RunningService { service_id, project_id, followed_log }
                })
                .collect(),
            failed: state.process_mgr.failed(),
        })
    }).await.map_err(|e| format!("获取运行状态任务失败: {}", e))?
}

/// 读取服务的日志快照。
///
/// 异步 + spawn_blocking：`get_logs` 在全局日志锁内取快照（元素是 `Arc<LogLine>`，
/// 锁内只拷指针），随后的 IPC 序列化在这条阻塞线程上完成——不占用 IPC 请求线程，
/// 也不把 2000 行字符串的序列化成本压在日志锁上。
#[tauri::command]
pub async fn get_service_logs(app: tauri::AppHandle, service_key: String) -> Result<Vec<Arc<LogLine>>, String> {
    if service_key.trim().is_empty() { return Err("服务标识不能为空".into()); }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        Ok(state.process_mgr.get_logs(&service_key))
    }).await.map_err(|e| format!("读取服务日志任务失败: {}", e))?
}

/// 执行工具命令（流式输出 + 结束返回完整结果）
///
/// - 输出逐行 emit `tool-command-log` 事件（run_id 区分），前端实时展示
/// - 命令结束后一次性返回完整 stdout/stderr（与事件内容等价，兜底保证完整）
/// - 同步命令在主线程执行会阻塞所有 IPC，改为异步 + spawn_blocking 隔离线程
#[tauri::command]
pub async fn run_tool_command(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    service_id: String,
    command_id: String,
    run_id: String,
) -> Result<ToolCommandResult, String> {
    if service_id.trim().is_empty() { return Err("服务ID不能为空".into()); }
    if command_id.trim().is_empty() { return Err("命令ID不能为空".into()); }
    if run_id.trim().is_empty() { return Err("run_id 不能为空".into()); }
    // run_id 是运行表的 key：占用检查与登记必须原子完成（见 RunTableGuard::reserve）——
    // 原来"先查重、spawn 后再 insert"中间隔着进程创建，同 run_id 并发时后者会覆盖前者
    let run_guard = RunTableGuard::reserve(&run_id)?;

    // 获取服务信息和工具命令
    let (service_name, _command, cwd, project_id, tool_commands_json) = state.db.with_conn(|conn| {
        conn.query_row(
            "SELECT name, command, cwd, project_id, tool_commands FROM services WHERE id=?1",
            [&service_id],
            |row| Ok((
                row.get::<_, String>("name")?,
                row.get::<_, String>("command")?,
                row.get::<_, String>("cwd")?,
                row.get::<_, String>("project_id")?,
                row.get::<_, String>("tool_commands")?,
            )),
        )
        .map_err(|e| format!("服务不存在: {}", e))
    })?;

    // 解析工具命令列表
    let tool_commands: Vec<crate::models::ToolCommand> = serde_json::from_str(&tool_commands_json)
        .map_err(|e| format!("解析工具命令失败: {}", e))?;

    // 查找指定的工具命令
    let tool_cmd = tool_commands.iter()
        .find(|tc| tc.id == command_id)
        .ok_or_else(|| format!("工具命令不存在: {}", command_id))?
        .clone();

    log::info!("[nexus] 执行工具命令: {}:{} -> {} (cmd={:?}, cwd={:?})",
        project_id, service_name, tool_cmd.name, crate::core::process::mask_command(&tool_cmd.command), cwd);

    // 在线程池中执行命令，避免阻塞主线程（长命令如 npm run build 会冻结整个 UI）
    // 输出由双线程逐行读取并 emit（前端实时展示），主线程只等进程退出拿退出码。
    // 超时按命令配置：缺省 60s（防长驻命令挂死）；0 = 不限制（长构建）；
    // 正数 = 该秒数。超时/手动停止都走 kill_process_tree（见 stop_tool_command）
    let cmd_str = tool_cmd.command;
    let cwd_clone = cwd.clone();
    // Job Object 句柄：工具命令子进程也必须加入共享清理域，否则应用被强杀/崩溃时
    // 长构建（timeout_secs=0/1800）会继续占用端口与 CPU（标准 §2.11：Windows 上所有子进程入 Job）
    let job = state.process_mgr.job_arc();
    let timeout_dur = match tool_cmd.timeout_secs {
        Some(0) => None,
        Some(secs) => Some(Duration::from_secs(secs)),
        None => Some(TOOL_COMMAND_TIMEOUT),
    };
    let run_id_exec = run_id.clone(); // 闭包 move 用；外层 run_id 保留给超时/清理分支
    let exec = async {
        tauri::async_runtime::spawn_blocking(move || {
            let run_id = run_id_exec;
            let mut cmd = crate::core::process::build_command(&cmd_str);
            if !cwd_clone.is_empty() {
                cmd.current_dir(&cwd_clone);
            }
            let mut child = cmd.stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| format!("执行命令失败: {}", e))?;
            let pid = child.id();
            // 登记 pid 与启动时间（stop_tool_command / 超时兜底据此按 run_id 终止进程树）；
            // 守卫在闭包任意返回路径（含 `?` 早退）都会清理表项
            run_guard.set_pid(pid);
            #[cfg(windows)]
            if let Some(job) = &job {
                job.assign_child(&child);
            }

            let stdout = match child.stdout.take() {
                Some(s) => s,
                None => return kill_then(&mut child, "无法获取 stdout".to_string()),
            };
            let stderr = match child.stderr.take() {
                Some(s) => s,
                None => return kill_then(&mut child, "无法获取 stderr".to_string()),
            };

            // stdout/stderr 各起一个读取线程：逐行入待发缓冲 + 收集 (序号, 行)
            // 序号为全局递增计数，结束时按序号合并，还原两流的真实交错顺序
            let out = ToolCmdOutput {
                pending: Arc::new(std::sync::Mutex::new(Vec::new())),
                readers_alive: Arc::new(std::sync::atomic::AtomicUsize::new(2)),
                seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            };
            let stdout_handle = match spawn_stream_reader(&out, "stdout", Box::new(stdout)) {
                Ok(h) => h,
                Err(e) => return kill_then(&mut child, format!("创建 stdout 读取线程失败: {}", e)),
            };
            let stderr_handle = match spawn_stream_reader(&out, "stderr", Box::new(stderr)) {
                Ok(h) => h,
                Err(e) => return kill_then(&mut child, format!("创建 stderr 读取线程失败: {}", e)),
            };

            // 批量发射线程（两个读取线程都起好后再起，避免失败路径留下孤儿线程）。
            // 与服务日志共用 `core::process::spawn_log_flush_thread`——两条通道的突发形态
            // 相同，且"最后一批不能丢"的不变式只该有一份实现（含测试）。
            let flush_app = app_handle.clone();
            let flush_run_id = run_id.clone();
            let flush_started = crate::core::process::spawn_log_flush_thread(
                format!("nexus-toolcmd-flush-{}", run_id),
                Arc::clone(&out.pending),
                Arc::clone(&out.readers_alive),
                move |batch| {
                    let _ = flush_app.emit("tool-command-log-batch", ToolCommandLogBatchPayload {
                        run_id: flush_run_id.clone(),
                        lines: batch,
                    });
                },
            );
            if let Err(e) = flush_started {
                // 发射线程起不来 = 实时输出丢失（最终结果仍由返回值兜底）
                return kill_then(&mut child, format!("创建输出发射线程失败: {}", e));
            }

            let status = child.wait().map_err(|e| format!("等待命令退出失败: {}", e))?;
            let mut all: Vec<_> = stdout_handle.join().map_err(|_| "stdout 读取线程异常".to_string())?
                .into_iter()
                .chain(stderr_handle.join().map_err(|_| "stderr 读取线程异常".to_string())?)
                .collect();
            all.sort_by_key(|(n, _)| *n);
            let output = all.into_iter().map(|(_, l)| l).collect::<Vec<_>>().join("\n");
            Ok::<_, String>((status, output))
        }).await.map_err(|e| format!("命令执行任务失败: {}", e))?
    };
    // 可选超时：None（配置 0）= 不限制，等待命令自然结束
    let result = match timeout_dur {
        Some(d) => tokio::time::timeout(d, exec).await,
        None => Ok(exec.await),
    };
    let result = match result {
        Ok(inner) => inner?,
        Err(_) => {
            // 超时：终止命令进程树，避免后台残留（已 emit 的部分输出仍保留在前端）。
            // pid 取自运行表（spawn 成功即登记）；若命令仍在阻塞池排队（尚未 spawn，
            // 表里是 pid=0 的占位）则取不到可杀目标——命令随后仍会照常启动（无法拦截），
            // 如实告知而非谎报"已终止"
            let pid = peek_running_cmd(&run_id).map(|e| e.pid).filter(|p| *p != 0);
            let secs = timeout_dur.map(|d| d.as_secs()).unwrap_or(0);
            return match pid {
                Some(p) => {
                    // 与 stop_tool_command 同口径：pid 身份复核后再杀，避免 pid 复用误杀
                    let identity_ok = peek_running_cmd(&run_id)
                        .and_then(|e| e.started_at)
                        .map(|t| crate::core::process::process_start_time_millis(p) == Some(t))
                        .unwrap_or(true);
                    if identity_ok {
                        crate::core::process::kill_process_tree(p);
                        Err(format!("命令执行超时（{} 秒），已终止。长构建类命令请在工具命令上配置更长的超时（或 0 = 不限制）", secs))
                    } else {
                        Err(format!("命令执行超时（{} 秒），且 pid {} 已被系统复用——未执行终止，请检查是否有残留进程", secs, p))
                    }
                }
                None => Err(format!("命令执行超时（{} 秒），且未能取得进程 ID——命令可能仍在启动队列中，请留意是否残留运行", secs)),
            };
        }
    };
    // 正常结束：运行表由 RunTableGuard 在阻塞任务返回时清理（此处不再手动 remove）

    let (status, output) = result;
    Ok(ToolCommandResult {
        success: status.success(),
        output,
        exit_code: status.code(),
    })
}

/// 停止正在执行的工具命令（按 run_id 终止其进程树）。
/// 前端结果弹窗「停止」按钮调用；命令随后会以退出码正常返回（success=false）
#[tauri::command]
pub async fn stop_tool_command(run_id: String) -> Result<(), String> {
    if run_id.trim().is_empty() {
        return Err("run_id 不能为空".into());
    }
    let entry = peek_running_cmd(&run_id).ok_or_else(|| "命令未在运行（可能已结束）".to_string())?;
    if entry.pid == 0 {
        return Err("命令正在启动中，请稍后再停止".into());
    }
    // pid 身份复核（SEC-11）：登记过启动时间就必须一致——不一致说明这个 pid 已被系统
    // 回收给别的进程，此时 taskkill 会杀掉无关进程树；查不到启动时间（进程已退出）同样不杀
    if let Some(started_at) = entry.started_at {
        match crate::core::process::process_start_time_millis(entry.pid) {
            Some(now) if now == started_at => {}
            Some(_) => {
                return Err(format!(
                    "命令已结束且 pid {} 已被系统复用，已放弃终止（避免误杀无关进程）",
                    entry.pid
                ));
            }
            None => return Err("命令已结束（进程不存在）".into()),
        }
    }
    log::info!("[nexus] 停止工具命令 run_id={} pid={}", run_id, entry.pid);
    crate::core::process::kill_process_tree(entry.pid);
    Ok(())
}
