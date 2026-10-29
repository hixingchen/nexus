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
/// 工具命令输出最多保留的**字节**数（PERF-31）。
///
/// 行数上限挡不住"2000 行 × 8KB 长行 ≈ 16MB"这种形状：这份结果要整体过 IPC 交给前端弹窗，
/// 16MB 字符串会在序列化、传输、渲染三处各付一次。与服务日志缓冲同口径（2MB）——
/// 两条上限量纲不同，缺一条就漏一种形状。
const TOOL_CMD_OUTPUT_MAX_BYTES: usize = 2 * 1024 * 1024;

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

/// 工具命令运行表（run_id → 进程信息）：供 `stop_tool_command` 按 run_id 终止进程树，
/// 也是超时兜底杀进程的 pid 来源。
///
/// **为什么是注入状态而不是进程全局 static（ARCH-25）**：原先这张表挂在
/// `OnceLock<Mutex<HashMap<…>>>` 上，于是命令体只能从"全进程唯一的那张表"取数，两个后果——
/// ① 同一测试二进制里的用例**共享同一张表**，互相干扰；② 命令签名里带着 `State<AppState>`
/// 与 `AppHandle`，测试根本构造不出来。而这条路径**每一段都有失败史**（`reserve` 的原子占位、
/// `kill_then` 的早退回收、超时分支的 pid 身份复核、`stop` 的 SEC-11 复核），四段全部零测试。
/// 改成由 `AppState` 持有、显式传给命令之后，测试可以各自建一张表直接驱动整条执行路径。
///
/// 内部用 `Arc` 是为了能克隆进 `spawn_blocking` 的 `'static` 闭包：`RunTableGuard` 必须在
/// 阻塞任务内部被 drop——表项的回收时机就是"命令跑完（或早退）"那一刻。
#[derive(Clone)]
pub struct ToolCmdTable {
    entries: Arc<std::sync::Mutex<std::collections::HashMap<String, RunningToolCmd>>>,
}

impl ToolCmdTable {
    pub fn new() -> Self {
        Self { entries: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())) }
    }

    /// 原子占位：在同一把锁内完成"查重 + 登记"，已存在即失败。
    ///
    /// 为什么要占位而不是 spawn 后再登记：原实现是 spawn **之前**查重、spawn **之后** insert，
    /// 两步之间隔着进程创建（毫秒级），两个并发请求带同一 run_id 时会双双通过查重，
    /// 后者覆盖前者的表项——先启动的那个进程从此失去终止句柄（`timeout_secs: 0` 的长构建
    /// 会一直跑到进程树自己结束）。
    fn reserve(&self, run_id: &str) -> Result<RunTableGuard, String> {
        let mut table = self.entries
            .lock()
            .map_err(|e| format!("工具命令运行表锁中毒: {}", e))?;
        if table.contains_key(run_id) {
            return Err(format!("run_id 已被占用: {}", run_id));
        }
        // pid=0 = 已占位、进程尚未启动；spawn 成功后由 set_pid 补上真实值
        table.insert(run_id.to_string(), RunningToolCmd { pid: 0, started_at: None });
        Ok(RunTableGuard { table: self.clone(), run_id: run_id.to_string() })
    }

    /// 读取运行表中的项（不移除）。超时分支只在阻塞任务真正结束前需要它，
    /// 表项的移除交给 RunTableGuard，避免"提前取走 pid → 任务结束时无项可清"
    fn peek(&self, run_id: &str) -> Option<RunningToolCmd> {
        self.entries.lock().ok().and_then(|m| m.get(run_id).copied())
    }
}

/// 运行表生命周期守卫：正常结束、任一早退（`?`）、甚至 panic 都会移除本 run_id 的表项。
///
/// 原实现只在"正常结束"与"超时"两条路径清理，任何 `?` 早退（拿 stdout/stderr 失败、
/// 线程创建失败、wait 失败）都会留下 stale pid；而 `stop_tool_command` 取到 pid 后无存活校验
/// 直接 taskkill —— 过期 PID 一旦被系统回收就会误杀无关进程树。
struct RunTableGuard {
    table: ToolCmdTable,
    run_id: String,
}

impl RunTableGuard {
    /// spawn 成功后补上真实 pid 与启动时间
    fn set_pid(&self, pid: u32) {
        if let Ok(mut table) = self.table.entries.lock() {
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
        if let Ok(mut table) = self.table.entries.lock() {
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

/// 工具命令子进程要加入的 Job Object 句柄。
///
/// 起别名只为让执行器在两个平台上**签名一致**：`core::job_object` 是 Windows 专有模块，
/// 直接写 `Arc<JobObject>` 会让非 Windows 构建连函数签名都写不出来（`job_arc` 本身也是
/// `#[cfg(windows)]`），而执行器体里没有平台分支——只有 `assign_child` 那一步是 Windows 的。
#[cfg(windows)]
pub(crate) type JobHandle = Arc<crate::core::job_object::JobObject>;
#[cfg(not(windows))]
pub(crate) type JobHandle = ();

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

/// 工具命令实时输出的发送端（ARCH-25②）——`core::process::LogSink` 的**反向那一半**。
///
/// 服务日志那条路径早就不认 Tauri 了（`core/process.rs` 只收一个闭包），而工具命令这条
/// 一直直接拿着 `AppHandle`：于是执行器**结构上不可测**——测试起不来一个 `AppHandle`，
/// 而"输出到底发出去了没有"恰恰是这条路径的一部分。换成注入一个发送端之后，
/// 测试可以塞一个记录用的 sink，直接断言"哪几行、什么时候发出去的"。
pub(crate) type ToolCmdLogSink = Arc<dyn Fn(ToolCommandLogBatchPayload) + Send + Sync>;

/// 把工具命令输出的批量事件接到 Tauri 事件总线上（生产侧的装配点，与 `log_sink` 同形）
fn tool_log_sink(app: &tauri::AppHandle) -> ToolCmdLogSink {
    let app = app.clone();
    Arc::new(move |payload| {
        // 发送失败必须留痕（CQ-29）：前端若尚未注册监听（或窗口已销毁），工具命令弹窗的
        // **实时流式输出**会静默消失，而它在界面上只有"没有输出"这一种表现，
        // 排查时没有任何线索
        if let Err(e) = app.emit("tool-command-log-batch", payload) {
            log::warn!("[nexus] 工具命令日志批量事件发送失败（前端可能尚未注册监听）: {}", e);
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
    /// 唤醒通道（PERF-25）：读取线程 push 后叫醒发射线程，空闲时它一次都不醒
    signal: Arc<crate::core::process::LogSignal>,
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
    let signal = Arc::clone(&out.signal);
    std::thread::Builder::new()
        .name(format!("nexus-toolcmd-{stream}"))
        .spawn(move || {
            let reader = std::io::BufReader::new(pipe);
            let mut collected: VecDeque<(u64, String)> = VecDeque::with_capacity(TOOL_CMD_OUTPUT_MAX_LINES);
            // 已驻留字节数：增量维护，别每行重算（服务日志缓冲那边同样处理）
            let mut collected_bytes = 0usize;
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
                // 与 ServiceLog 的 reader 同口径：**释放缓冲锁之后**再唤醒（锁序见 LogSignal）
                signal.notify();
                if collected.len() >= TOOL_CMD_OUTPUT_MAX_LINES {
                    if let Some((_, old)) = collected.pop_front() {
                        collected_bytes -= old.len();
                    }
                }
                collected_bytes += line.len();
                collected.push_back((n, line));
                // 字节上限（PERF-31）：从头部丢到不超上限，保留最新——与服务日志缓冲同一套
                while collected_bytes > TOOL_CMD_OUTPUT_MAX_BYTES {
                    let Some((_, old)) = collected.pop_front() else { break };
                    collected_bytes -= old.len();
                }
            }
            // 递减必须在所有 push 之后（发射线程的收尾判据依赖这个顺序）
            alive.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            collected
        })
}

/// 启动/重启服务所需的配置（收成结构体，避免 7 元组串位与 clippy 的 type_complexity）。
struct ServiceInfo {
    name: String,
    command: String,
    cwd: String,
    project_id: String,
    env_vars: String,
    /// 服务本身是否启用（"跟随项目启动"）；manifest 只收 `boot_enabled=1 AND enabled=1`
    enabled: bool,
    boot_enabled: bool,
    boot_credential_mode: String,
}

fn get_service_info(
    db: &crate::database::Database,
    service_id: &str,
) -> Result<ServiceInfo, String> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT name, command, cwd, project_id, env_vars, enabled, boot_enabled, boot_credential_mode \
             FROM services WHERE id=?1",
            [service_id],
            |row| Ok(ServiceInfo {
                name: row.get("name")?,
                command: row.get("command")?,
                cwd: row.get("cwd")?,
                project_id: row.get("project_id")?,
                env_vars: row.get("env_vars")?,
                enabled: row.get::<_, i32>("enabled")? != 0,
                boot_enabled: row.get::<_, i32>("boot_enabled")? != 0,
                boot_credential_mode: row.get("boot_credential_mode")?,
            }),
        )
        .map_err(|e| format!("服务不存在: {}", e))
    })
}

/// 工具命令执行结果（output 为 stdout/stderr 按时间序合并，与 cmd 终端展示一致）
///
/// `Debug` 是给测试用的：`expect_err` 要求 `Ok` 那侧可打印，否则失败时只能看到一句
/// "应当出错"——而这条路径的失败恰恰常常是"本该报错却成功了"
#[derive(Debug, Serialize)]
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
    /// 进程来源：local（Nexus 自己拉起） / boot（开机启动器）
    pub source: String,
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
        let info = get_service_info(&state.db, &service_id)?;
        // 本地进程表已有它：无论开机启动配置如何，都不能再交给 runner 起第二份。
        // 先推进一拍（崩溃/秒退的服务可能还没从表里摘掉）
        state.process_mgr.tick();
        if state.process_mgr.is_running(&service_id) {
            return Err(format!("服务「{}」已在本地运行中", info.name));
        }
        let configured = crate::core::boot::BootMode::parse(&info.boot_credential_mode)
            .unwrap_or(crate::core::boot::BootMode::S4u);
        // manifest 只收 `boot_enabled=1 AND enabled=1`。取消任一项后，启动器就不再负责
        // 它的未来启停；此时不能再把 start 发给 runner（manifest 里没有它，必然失败）。
        let boot_active = info.boot_enabled && info.enabled;
        let live = crate::commands::boot::live_boot_service(&service_id);
        if boot_active {
            // 启动按**配置的模式**路由：manifest 是按它生成的，不能被旧模式里的残留条目带偏
            if crate::commands::boot::runner_reachable_now(configured) {
                // 已在跑时给明确结论（PID + 下一步动作），而不是把 runner 的
                // “服务已在运行中”原样抛成一次失败提示
                if let Some((_, st)) = &live {
                    return Err(format!(
                        "服务「{}」已在开机启动器中运行（PID {}）；需要重启请点「重启」，停止请点「停止」。",
                        info.name, st.pid.unwrap_or(0)
                    ));
                }
                return crate::commands::boot::send_service_command(configured, "start", &service_id)
                    .map_err(|e| format!("服务「{}」{}", info.name, e));
            }
            // 启动器不可达：绝不落回本地启动，否则会出现同名服务的第二个进程
            if let Some((source, st)) = &live {
                return Err(format!(
                    "服务「{}」正在由{}启动器运行（PID {}），但启动器当前不可达；已阻止本地重复启动。可在「设置 > 服务开机启动」强制停止，或重新启动启动器。",
                    info.name,
                    if *source == "legacy" { "旧版" } else { "开机" },
                    st.pid.unwrap_or(0)
                ));
            }
            return Err(format!(
                "服务「{}」配置为开机启动，但对应启动器未运行；已阻止本地重复启动。请到「设置 > 服务开机启动」启动/修复启动器，或先关闭该服务的开机启动再本地运行。",
                info.name
            ));
        }
        // 已取消开机启动（或服务被禁用），但启动器里的实例还在跑：不能启动第二份。
        // 先把实例停掉，下一次「启动」就会走本地进程管理器。
        if let Some((_, st)) = &live {
            return Err(format!(
                "服务「{}」仍由开机启动器运行（PID {}）；已阻止本地重复启动。请先点服务卡片上的「停止」，停止后即可本地启动。",
                info.name, st.pid.unwrap_or(0)
            ));
        }
        let envs = crate::core::process::parse_env_vars(&info.env_vars)?;
        state.process_mgr.start(crate::core::process::ServiceSpawn {
            project_id: &info.project_id, service_id: &service_id, name: &info.name,
            command: &info.command, cwd: &info.cwd, env_vars: &envs, log_sink: log_sink(&app_handle),
        }).map_err(|e| format!("服务「{}」{}", info.name, e))
    }).await.map_err(|e| format!("启动服务任务失败: {}", e))?
}

/// 停止服务（清理含 taskkill + 等待 reader 线程，最长数秒 → 异步执行避免阻塞 IPC）
#[tauri::command]
pub async fn stop_service(app: tauri::AppHandle, service_id: String) -> Result<(), String> {
    if service_id.trim().is_empty() { return Err("服务ID不能为空".into()); }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        // runner 可达 → 走命令队列；启动器失联但子进程还活着 → 按 PID 兜底结束，
        // 不让“启动器没响应”变成“这个服务再也停不掉”。
        // 本地进程表也收一次（历史上本地/开机启动双开过时，两边都要停）。
        let boot_result = crate::commands::boot::stop_boot_service(&service_id);
        let local_result = state.process_mgr.stop(&service_id);
        boot_result?;
        local_result
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
        let info = get_service_info(&state.db, &service_id)?;
        let configured = crate::core::boot::BootMode::parse(&info.boot_credential_mode)
            .unwrap_or(crate::core::boot::BootMode::S4u);
        let boot_active = info.boot_enabled && info.enabled;
        let owned = crate::commands::boot::boot_owned_mode(&service_id);
        state.process_mgr.tick();
        // 本地与启动器同时有实例（历史遗留）时先收掉本地那份；只有本地实例时直接本地重启，
        // 不能因为配置了开机启动就把 restart 发给 runner——那会双开。
        let boot_live = crate::commands::boot::live_boot_service(&service_id).is_some();
        if state.process_mgr.is_running(&service_id) {
            if boot_live {
                if let Err(e) = state.process_mgr.stop(&service_id) {
                    log::warn!("重启服务时停止重复的本地实例失败: {}", e);
                }
            } else {
                let envs = crate::core::process::parse_env_vars(&info.env_vars)?;
                return state.process_mgr.restart(crate::core::process::ServiceSpawn {
                    project_id: &info.project_id, service_id: &service_id, name: &info.name,
                    command: &info.command, cwd: &info.cwd, env_vars: &envs, log_sink: log_sink(&app),
                }).map_err(|e| format!("服务「{}」{}", info.name, e));
            }
        }
        // 服务改了凭据模式但旧 runner 还在跑：先停旧模式，再在新模式里启动
        if let Some(old) = owned {
            if boot_active {
                if old != configured {
                    crate::commands::boot::send_service_command(old, "stop", &service_id)
                        .map_err(|e| format!("服务「{}」{}", info.name, e))?;
                    if crate::commands::boot::runner_reachable_now(configured) {
                        return crate::commands::boot::send_service_command(configured, "start", &service_id)
                            .map_err(|e| format!("服务「{}」{}", info.name, e));
                    }
                    return Err(format!(
                        "服务「{}」已停止旧模式进程，但 {} 启动器未运行/未安装，无法按新模式启动",
                        info.name,
                        configured.display()
                    ));
                }
                return crate::commands::boot::send_service_command(old, "restart", &service_id)
                    .map_err(|e| format!("服务「{}」{}", info.name, e));
            }
            // 已取消开机启动：让拥有它的 runner 先停下，然后走本地重启（交接）。
            // 不这样做的话，runner 的 manifest 里已经没有它，restart 命令必然失败。
            crate::commands::boot::send_service_command(old, "stop", &service_id)
                .map_err(|e| format!("服务「{}」{}", info.name, e))?;
        }
        // 没有可达的 owner、但 state 里仍有存活实例（旧版遗留 / runner 已退出）：
        // 先按 PID 兜底结束，避免“重启”变成在同一台机器上再起一份。
        // 必须排在“配置为开机启动且新模式可达”之前——否则旧版实例还活着就又启动一份。
        if let Some((_, st)) = crate::commands::boot::live_boot_service(&service_id) {
            if let Some(pid) = st.pid {
                crate::core::process::kill_process_tree(pid)
                    .map_err(|e| format!("结束开机启动实例失败（PID {}）：{}", pid, e))?;
            }
            if boot_active {
                if crate::commands::boot::runner_reachable_now(configured) {
                    // 遗留实例已清理，交给目标模式的启动器重启
                    return crate::commands::boot::send_service_command(configured, "restart", &service_id)
                        .map_err(|e| format!("服务「{}」{}", info.name, e));
                }
                return Err(format!(
                    "已结束启动器退出后留下的实例（PID {}）；该服务配置为开机启动且启动器未运行，本地重启已被阻止。请到「设置 > 服务开机启动」启动/修复启动器，或先关闭开机启动。",
                    st.pid.unwrap_or(0)
                ));
            }
        }
        if boot_active && crate::commands::boot::runner_reachable_now(configured) {
            return crate::commands::boot::send_service_command(configured, "restart", &service_id)
                .map_err(|e| format!("服务「{}」{}", info.name, e));
        }
        if boot_active {
            return Err(format!(
                "服务「{}」配置为开机启动，但对应启动器未运行；本地重启已被阻止。请到「设置 > 服务开机启动」启动/修复启动器，或先关闭该服务的开机启动。",
                info.name
            ));
        }
        let envs = crate::core::process::parse_env_vars(&info.env_vars)?;
        state.process_mgr.restart(crate::core::process::ServiceSpawn {
            project_id: &info.project_id, service_id: &service_id, name: &info.name,
            command: &info.command, cwd: &info.cwd, env_vars: &envs, log_sink: log_sink(&app),
        }).map_err(|e| format!("服务「{}」{}", info.name, e))
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
                "SELECT id, name, command, cwd, env_vars, boot_enabled, boot_credential_mode \
                 FROM services WHERE project_id=?1 AND enabled=1 ORDER BY sort_index"
            ).map_err(|e| format!("查询项目服务列表失败: {}", e))?;
            let rows = stmt.query_map([&project_id], |row| {
                Ok((
                    row.get::<_, String>("id")?,
                    row.get::<_, String>("name")?,
                    row.get::<_, String>("command")?,
                    row.get::<_, String>("cwd")?,
                    row.get::<_, String>("env_vars")?,
                    row.get::<_, i32>("boot_enabled")? != 0,
                    row.get::<_, String>("boot_credential_mode")?,
                ))
            }).map_err(|e| format!("读取项目服务数据失败: {}", e))?;
            let mut svcs = Vec::new();
            for r in rows { svcs.push(r.map_err(|e| format!("解析项目服务数据失败: {}", e))?); }
            Ok::<_, String>(svcs)
        })?;
        let mut errors = Vec::new();
        // 已在运行的服务跳过（start 对已运行返回"已在运行中"，混在 errors 里会误报启动失败）
        let running_ids: std::collections::HashSet<String> = state.process_mgr.running()
            .into_iter().map(|(_, service_id, _)| service_id).collect();
        let boot_ids = crate::commands::boot::boot_running_ids();
        for (id, name, cmd, cwd, env_vars, boot_enabled, boot_mode) in &services {
            if boot_ids.contains(id) { continue; }
            // 本地已在运行：直接跳过（必须排在开机启动路由之前，否则本地实例 +
            // 开机启动配置会再起一份 boot 实例）
            if running_ids.contains(id) { continue; }
            // 开机启动服务：按**配置的模式**路由（manifest 按它生成；旧模式的残留条目不能带偏）
            if *boot_enabled {
                let configured = crate::core::boot::BootMode::parse(boot_mode)
                    .unwrap_or(crate::core::boot::BootMode::S4u);
                if crate::commands::boot::runner_reachable_now(configured) {
                    if let Err(e) = crate::commands::boot::send_service_command(configured, "start", id) {
                        errors.push(format!("{}: {}", name, e));
                    }
                    continue;
                }
                // 启动器不可达：跳过而不是落本地，否则批量启动会造出第二份进程
                errors.push(format!("{}: 配置为开机启动，但对应启动器未运行；已跳过本地启动", name));
                continue;
            }
            // 已取消开机启动，但启动器里的实例还在跑：同样不能起第二份
            if crate::commands::boot::live_boot_service(id).is_some() {
                errors.push(format!("{}: 仍由开机启动器运行；已跳过本地启动（先停止该实例）", name));
                continue;
            }
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
        // runner 拥有的服务交给**拥有它的模式**的 runner 停；只有本地服务走 ProcessManager。
        // 两边的 id 互斥（所有权唯一），因此可以安全地拆开处理。
        let mut errors: Vec<String> = Vec::new();
        let mut local_ids: Vec<String> = Vec::new();
        for id in ids {
            // 启动器可达走命令队列；失联时按 state 里的 PID 兜底结束（同一套口径）。
            // 每个 id 都再过一遍本地清理：历史上双开过时两边都要停，表里没有就是 no-op。
            if let Err(e) = crate::commands::boot::stop_boot_service(&id) {
                errors.push(format!("{}: {}", id, e));
            }
            local_ids.push(id);
        }
        // 停止全部：**一轮只取一次进程快照 + 并行停止**（PERF-21 / PERF-20①）。
        //
        // 串行版本的墙钟最坏是 5s × 服务数（单服务收尸尾段：2s 等子进程 + 三个 1s 等 reader），
        // 18 个服务 ≈ 90s，而这期间界面没有任何进度反馈。各服务的清理互不依赖（各自持自己的
        // 生命周期锁，进程表只在摘表时短暂加锁），可以并行；快照取在**杀任何东西之前**，
        // 后代与父进程的链按原始快照判定，也顺带把"每服务各枚举一次全系统进程表"降成一次。
        //
        // 错误照旧**逐个收集**（原实现 `let _ = stop(id)` 丢弃错误，命令恒返回成功，
        // 用户看到"已停止"而进程还在跑）。
        let snapshot = crate::core::winproc::snapshot_processes();
        errors.extend(std::thread::scope(|scope| {
            let handles: Vec<_> = local_ids
                .iter()
                .map(|id| {
                    let mgr = &state.process_mgr;
                    let snap: &[crate::core::winproc::ProcEntry] = &snapshot;
                    scope.spawn(move || {
                        mgr.stop_with_snapshot(id, Some(snap))
                            .err()
                            .map(|e| format!("{}: {}", id, e))
                    })
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect::<Vec<_>>()
        }));
        for e in &errors {
            log::warn!("[nexus] 停止服务失败: {}", e);
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
        // 本地进程 + runner 状态合并。外部服务**不收养进 ProcessManager**：
        // 这里只是把 runner 的快照并进展示层，Nexus 退出时 stop_all 仍然只碰本地进程。
        let mut running: Vec<RunningService> = state.process_mgr.running().into_iter()
            .map(|(project_id, service_id, followed_log)| RunningService {
                service_id, project_id, followed_log, source: "local".into(),
            })
            .collect();
        let local_ids: std::collections::HashSet<String> =
            running.iter().map(|r| r.service_id.clone()).collect();

        let mut failed = state.process_mgr.failed();
        let local_failed: std::collections::HashSet<String> =
            failed.iter().map(|f| f.service_id.clone()).collect();

        // 开机服务：只要**进程还活着**就并进运行列表，不以 runner 心跳为门槛。
        // 否则 runner 退出/卡住后，它拉起的服务在界面上会全部“消失”，连带日志入口也没了
        // （用户实际遇到的就是这个）。pid + 启动时刻的校验在 boot::service_alive 里。
        for (id, project_id) in crate::commands::boot::boot_running_services() {
            if !local_ids.contains(&id) {
                running.push(RunningService {
                    service_id: id,
                    project_id,
                    followed_log: None,
                    source: "boot".into(),
                });
            }
        }
        for f in crate::commands::boot::boot_failed_services() {
            if !local_failed.contains(&f.service_id) && !local_ids.contains(&f.service_id) {
                failed.push(f);
            }
        }
        Ok(ProcessStatus { running, failed })
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
        let local = state.process_mgr.get_logs(&service_key);
        if !local.is_empty() {
            return Ok(local);
        }
        // 本地没有缓冲：只有“开机启动器拥有/曾拥有”的服务才回落到 boot 日志文件，
        // 否则一个普通本地服务会读到同 id 的历史 boot 日志（曾开过开机启动后关掉的情况）。
        let Some(mode) = crate::commands::boot::service_log_mode(&state.db, &service_key) else {
            return Ok(Vec::new());
        };
        // 日志文件按 service_id 命名，读尾部作为快照；前端在日志面板打开期间会增量轮询。
        Ok(crate::commands::boot::read_boot_log_tail(mode, &service_key, 2 * 1024 * 1024)
            .unwrap_or_default())
    }).await.map_err(|e| format!("读取服务日志任务失败: {}", e))?
}

/// 执行工具命令（流式输出 + 结束返回完整结果）
///
/// - 输出逐行经 `ToolCmdLogSink` 发到 `tool-command-log-batch` 事件（run_id 区分），前端实时展示
/// - 命令结束后一次性返回完整 stdout/stderr（与事件内容等价，兜底保证完整）
/// - 同步命令在主线程执行会阻塞所有 IPC，改为异步 + spawn_blocking 隔离线程
///
/// 本命令只负责"从库里取出要跑什么"；执行本身在 `execute_tool_command`（可测）
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

    // 超时按命令配置：缺省 60s（防长驻命令挂死）；0 = 不限制（长构建）；正数 = 该秒数。
    // 超时/手动停止都走 kill_process_tree（见 execute_tool_command / stop_tool_command）
    let timeout = match tool_cmd.timeout_secs {
        Some(0) => None,
        Some(secs) => Some(Duration::from_secs(secs)),
        None => Some(TOOL_COMMAND_TIMEOUT),
    };
    // Job Object 句柄：工具命令子进程也必须加入共享清理域，否则应用被强杀/崩溃时
    // 长构建（timeout_secs=0/1800）会继续占用端口与 CPU（标准 §2.11：Windows 上所有子进程入 Job）
    #[cfg(windows)]
    let job = state.process_mgr.job_arc();
    #[cfg(not(windows))]
    let job: Option<JobHandle> = None;

    execute_tool_command(
        state.tool_cmds.clone(),
        job,
        tool_log_sink(&app_handle),
        tool_cmd.command,
        cwd,
        timeout,
        run_id,
    ).await
}

/// 工具命令的执行核心（ARCH-25：把不可测的四样东西变成参数）。
///
/// 与 Tauri 命令分开的理由：命令签名带 `State<AppState>` / `AppHandle`，测试构造不出来——
/// 而这条路径恰恰有四段失败史（原子占位 / 早退回收 / 超时 pid 复核 / SEC-11 复核），
/// 此前一段测试都没有。这里把**运行表、Job 句柄、输出发送端**全部注入，
/// 命令体只剩"从库里取出要跑什么"。
///
/// 参数刚好 7 个（clippy 的门槛），再要加东西应当先想"它是不是该并入既有参数"。
pub(crate) async fn execute_tool_command(
    table: ToolCmdTable,
    job: Option<JobHandle>,
    log_sink: ToolCmdLogSink,
    cmd_str: String,
    cwd: String,
    timeout: Option<Duration>,
    run_id: String,
) -> Result<ToolCommandResult, String> {
    // run_id 是运行表的 key：占用检查与登记必须原子完成（见 ToolCmdTable::reserve）——
    // 原来"先查重、spawn 后再 insert"中间隔着进程创建，同 run_id 并发时后者会覆盖前者
    let run_guard = table.reserve(&run_id)?;

    // 在线程池中执行命令，避免阻塞主线程（长命令如 npm run build 会冻结整个 UI）
    // 输出由双线程逐行读取并交给 sink（前端实时展示），主线程只等进程退出拿退出码
    let run_id_exec = run_id.clone(); // 闭包 move 用；外层 run_id 保留给超时/清理分支
    let exec = async {
        tauri::async_runtime::spawn_blocking(move || {
            let mut cmd = crate::core::process::build_command(&cmd_str);
            if !cwd.is_empty() {
                cmd.current_dir(&cwd);
            }
            let child = cmd.stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| format!("执行命令失败: {}", e))?;
            // 登记 pid 与启动时间（stop_tool_command / 超时兜底据此按 run_id 终止进程树）
            run_guard.set_pid(child.id());
            // 守卫随 child 一起进 run_tool_child：它任意返回路径（含 `?` 早退）都会清理表项
            run_tool_child(child, run_guard, job, log_sink, run_id_exec)
        }).await.map_err(|e| format!("命令执行任务失败: {}", e))?
    };
    // 可选超时：None（配置 0）= 不限制，等待命令自然结束
    let result = match timeout {
        Some(d) => tokio::time::timeout(d, exec).await,
        None => Ok(exec.await),
    };
    let result = match result {
        Ok(inner) => inner?,
        Err(_) => {
            // 超时：终止命令进程树，避免后台残留（已发到前端的那部分输出仍保留）。
            // pid 取自运行表（spawn 成功即登记）；若命令仍在阻塞池排队（尚未 spawn，
            // 表里是 pid=0 的占位）则取不到可杀目标——命令随后仍会照常启动（无法拦截），
            // 如实告知而非谎报"已终止"
            let pid = table.peek(&run_id).map(|e| e.pid).filter(|p| *p != 0);
            let secs = timeout.map(|d| d.as_secs()).unwrap_or(0);
            return match pid {
                Some(p) => {
                    // 与 stop_tool_command 同口径：pid 身份复核后再杀，避免 pid 复用误杀
                    let identity_ok = table.peek(&run_id)
                        .and_then(|e| e.started_at)
                        .map(|t| crate::core::process::process_start_time_millis(p) == Some(t))
                        .unwrap_or(true);
                    if identity_ok {
                        // 终止失败必须说出来（CQ-25）：杀不掉时命令仍在跑，而这句话里写着
                        // "已终止"——只报成功就是撒谎。原实现把 taskkill 的错误吞在 core 里
                        let kill_note = match crate::core::process::kill_process_tree(p) {
                            Ok(()) => String::new(),
                            Err(e) => format!("（终止失败：{}）", e),
                        };
                        Err(format!(
                            "命令执行超时（{} 秒），已终止{}。长构建类命令请在工具命令上配置更长的超时（或 0 = 不限制）",
                            secs, kill_note
                        ))
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

/// spawn 之后的收尾段：纳管 → 接管两个管道 → 起读取/发射线程 → 等退出 → 按序号合并输出。
///
/// 从命令闭包里抽出来（ARCH-25）有两个理由：
/// ① 闭包只剩"起进程"一件事，读取/发射这一大段与它无关；
/// ② **`kill_then` 那条早退路径终于可达**——测试传一个没接管管道的 child 就能走到它。
///    而那条路径正是历史 bug 的现场：`child` 直接 drop → 进程"存活但已从运行表摘除"，
///    `stop_tool_command` 再也找不到它（进程树会一直跑下去）。
///
/// `run_guard` 由本函数持有到返回为止：表项的回收时机就是"命令跑完（或早退）"这一刻。
fn run_tool_child(
    mut child: std::process::Child,
    run_guard: RunTableGuard,
    job: Option<JobHandle>,
    log_sink: ToolCmdLogSink,
    run_id: String,
) -> Result<(std::process::ExitStatus, String), String> {
    #[cfg(windows)]
    if let Some(job) = &job {
        if let Err(e) = job.assign_child(&child) {
            log::warn!("[nexus] 工具命令进程未纳入 Job Object（Nexus 异常退出时可能残留）: {}", e);
        }
    }
    #[cfg(not(windows))]
    let _ = &job;
    // 守卫**绝不能**提前 drop：它一 drop 表项就没了，超时/停止分支再也取不到 pid。
    // 下划线前缀只表示"不直接使用"，绑定仍然活到函数结束
    let _guard = run_guard;

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
        signal: Arc::new(crate::core::process::LogSignal::new()),
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
    let flush_run_id = run_id.clone();
    let flush_started = crate::core::process::spawn_log_flush_thread(
        format!("nexus-toolcmd-flush-{}", run_id),
        Arc::clone(&out.pending),
        Arc::clone(&out.readers_alive),
        Arc::clone(&out.signal),
        move |batch| log_sink(ToolCommandLogBatchPayload {
            run_id: flush_run_id.clone(),
            lines: batch,
        }),
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
    Ok((status, output))
}

/// 停止正在执行的工具命令（按 run_id 终止其进程树）。
/// 前端结果弹窗「停止」按钮调用；命令随后会以退出码正常返回（success=false）
#[tauri::command]
pub async fn stop_tool_command(state: State<'_, AppState>, run_id: String) -> Result<(), String> {
    stop_tool_command_inner(&state.tool_cmds, &run_id)
}

/// 停止的核心逻辑（与命令分开：命令签名带 `State`，测试构造不出来）
fn stop_tool_command_inner(table: &ToolCmdTable, run_id: &str) -> Result<(), String> {
    if run_id.trim().is_empty() {
        return Err("run_id 不能为空".into());
    }
    let entry = table.peek(run_id).ok_or_else(|| "命令未在运行（可能已结束）".to_string())?;
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
    // 杀掉失败要上报（CQ-25）：调用方按成败决定提示什么，而"点了停止但命令还在跑"
    // 此前会被报成成功——用户只能看着输出继续涨
    crate::core::process::kill_process_tree(entry.pid)
        .map_err(|e| format!("终止命令进程失败 (pid={}): {}", entry.pid, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 工具命令执行路径（ARCH-25 / NEW-24④）─────────────────────────────
    //
    // 为什么这组用例此前写不出来：整条路径挂在**进程全局 static 表**上，命令签名又带
    // `State<AppState>` 与 `AppHandle`——表拿不到、应用构造不出来，于是四段修过的 bug
    // （原子占位 / kill_then 早退回收 / 超时 pid 复核 / SEC-11 复核）一段回归测试都没有。
    // 表与发送端改成注入之后，这里可以各自建一张表，用**真实进程**驱动整条路径。
    //
    // 起进程一律走 `core::process::build_command`（与线上同一条构造路径，见 spawn_guard 的
    // EXEMPT），用例内不出现裸的 `Command::new`。

    /// 记录用的发送端：把每一次批量事件原样收下来（不 emit 任何东西）
    fn recording_sink() -> (ToolCmdLogSink, Arc<std::sync::Mutex<Vec<ToolCommandLogBatchPayload>>>) {
        let got: Arc<std::sync::Mutex<Vec<ToolCommandLogBatchPayload>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink: ToolCmdLogSink = {
            let got = Arc::clone(&got);
            Arc::new(move |payload| got.lock().expect("sink 锁").push(payload))
        };
        (sink, got)
    }

    /// 什么都不发的空发送端
    fn noop_sink() -> ToolCmdLogSink {
        Arc::new(|_| {})
    }

    /// 收集发送端收到过的所有行（发射线程是独立线程，调用方需要**等**它送完最后一批）
    fn sink_lines(got: &Arc<std::sync::Mutex<Vec<ToolCommandLogBatchPayload>>>) -> Vec<String> {
        got.lock()
            .expect("sink 锁")
            .iter()
            .flat_map(|b| b.lines.iter().map(|l| l.data.clone()))
            .collect()
    }

    /// 一条"活着、不读 stdin、也不产生任何输出"的长命令。
    ///
    /// 不用 `timeout`/`pause`：`timeout` 在没有控制台输入时直接报错退出，`pause` 会挂住
    /// 整个控制台，两者在不同 Windows 版本上的行为还不一致。`ping -n N` 的墙钟是确定的
    /// （约 N-1 秒），`core/process.rs` 的测试同样拿它当长命令。
    fn sleep_cmd(secs: u32) -> String {
        format!("ping -n {} 127.0.0.1 > NUL", secs)
    }

    /// 按指定启动时间登记一条表项。
    ///
    /// `RunTableGuard::set_pid` 总是取**当前真实**的启动时间，于是"启动时间对不上"
    /// （SEC-11 要防的 pid 复用）没法从外部构造出来——只能直接写表。
    /// `tests` 是 `commands::process` 的子模块，父模块的私有字段在这里可见。
    fn seed(table: &ToolCmdTable, run_id: &str, pid: u32, started_at: Option<u64>) {
        table
            .entries
            .lock()
            .expect("运行表锁")
            .insert(run_id.to_string(), RunningToolCmd { pid, started_at });
    }

    /// 起一个真实的长命进程当"受害者"（pid 身份复核类用例需要它）
    #[cfg(windows)]
    fn spawn_victim(secs: u32) -> std::process::Child {
        let mut cmd = crate::core::process::build_command(&sleep_cmd(secs));
        // 与生产同一条路径；这里额外收口只为不依赖 CWD（`ping` 是系统命令，绝对安全）
        cmd.env("NoDefaultCurrentDirectoryInExePath", "1");
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("起受害者进程")
    }

    /// pid 是否仍在运行。
    ///
    /// 判据用**句柄**（`GetExitCodeProcess`）而不是 `process_start_time_millis`：后者是
    /// "身份未变"的判据，进程被结束后只要还有句柄（例如控制台宿主）没关，`OpenProcess`
    /// 照样成功——`core/process.rs` 的 `wait_gone` 就是被它骗过才改成句柄的。
    #[cfg(windows)]
    fn is_alive(pid: u32) -> bool {
        crate::core::winproc::OwnedProcess::open(pid)
            .map(|h| h.is_alive())
            .unwrap_or(false)
    }

    /// 等进程真的不再运行（taskkill 是异步收尾，杀与"查不到"之间有一个窗口）
    #[cfg(windows)]
    fn wait_gone(pid: u32) -> bool {
        for _ in 0..60 {
            if !is_alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// 收掉受害者进程（用例正常结束时调用；失败时它也会自己跑完，`ping -n 20` 约 19 秒）
    #[cfg(windows)]
    fn reap_victim(pid: u32, child: &mut std::process::Child) {
        let _ = crate::core::process::kill_process_tree(pid);
        let _ = child.wait();
    }

    /// 等运行表里出现真实 pid（spawn 成功即由 `set_pid` 登记）
    #[cfg(windows)]
    fn wait_for_pid(table: &ToolCmdTable, run_id: &str) -> Option<u32> {
        for _ in 0..500 {
            if let Some(e) = table.peek(run_id) {
                if e.pid != 0 {
                    return Some(e.pid);
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    // ── ① 原子占位 ────────────────────────────────────────────────

    /// 同一 run_id 的第二次占位必须被拒，且**不覆盖**第一次的表项。
    ///
    /// 原实现是 spawn 之前查重、spawn 之后 insert，两步之间隔着进程创建：并发同 run_id 时
    /// 双双通过查重，后者覆盖前者的表项——先启动的进程从此失去终止句柄
    /// （`timeout_secs: 0` 的长构建会一直跑到进程树自己结束）。
    #[test]
    fn test_reserve_rejects_duplicate_run_id_and_reclaims_on_drop() {
        let table = ToolCmdTable::new();
        let guard = table.reserve("run-dup").expect("首次占位应当成功");
        assert_eq!(table.peek("run-dup").map(|e| e.pid), Some(0), "占位项先记 pid=0");

        // 手写 match 而不是 expect_err：`RunTableGuard` 不该为了测试去 derive Debug
        let err = match table.reserve("run-dup") {
            Ok(_) => panic!("同一 run_id 二次占位必须被拒"),
            Err(e) => e,
        };
        assert!(err.contains("已被占用"), "错误信息要说清原因，实际: {}", err);
        assert_eq!(table.peek("run-dup").map(|e| e.pid), Some(0), "被拒的请求不该改动表项");

        // 守卫 drop（= 命令跑完）之后同一个 run_id 必须能再用：否则前端重跑一次就永久"已被占用"
        drop(guard);
        assert!(table.peek("run-dup").is_none(), "守卫 drop 后表项必须回收");
        let again = table.reserve("run-dup").expect("回收后应能重新占位");
        drop(again);
    }

    /// 并发占同一 run_id 时**只能有一个赢家**。
    ///
    /// 这条才是原子占位的真回归：顺序调用复现不出原 bug（"先查重、spawn 后再 insert"的
    /// 窗口只在两个请求同时进来时张开），而它一旦复发，输的那个请求会覆盖赢家的表项。
    #[test]
    fn test_concurrent_reserve_has_exactly_one_winner() {
        const N: usize = 8;
        let table = ToolCmdTable::new();
        let winners = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let start = Arc::new(std::sync::Barrier::new(N));
        let release = Arc::new(std::sync::Barrier::new(N));
        let mut handles = Vec::new();
        for _ in 0..N {
            let table = table.clone();
            let winners = Arc::clone(&winners);
            let (start, release) = (Arc::clone(&start), Arc::clone(&release));
            handles.push(std::thread::spawn(move || {
                start.wait(); // 让 N 个线程尽量同时冲进 reserve
                // 赢家必须**握着守卫**等到所有人都试过，否则它一松手后面的人就"成功"了
                let guard = table.reserve("run-race");
                let won = guard.is_ok();
                if won {
                    winners.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                release.wait();
                drop(guard);
                won
            }));
        }
        let results: Vec<bool> = handles.into_iter().map(|h| h.join().expect("线程不应 panic")).collect();
        assert_eq!(results.iter().filter(|w| **w).count(), 1, "只能有一个赢家");
        assert_eq!(winners.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(table.peek("run-race").is_none(), "所有守卫都 drop 之后表项必须清空");
    }

    // ── ② stop_tool_command 的 SEC-11 复核 ────────────────────────

    /// 登记过启动时间、但**当前 pid 的启动时间对不上**时必须拒绝终止。
    ///
    /// 这正是 pid 回收场景：命令早就结束，pid 分给了无关进程，此时 taskkill /T /F 会杀一棵
    /// 别人的进程树。用例里的"无关进程"是真的——所以"没杀"直接可观测（受害者还活着），
    /// 不是只看返回值。
    #[cfg(windows)]
    #[test]
    fn test_stop_refuses_when_pid_start_time_mismatches() {
        let table = ToolCmdTable::new();
        let mut victim = spawn_victim(20);
        let pid = victim.id();
        let real_start = crate::core::process::process_start_time_millis(pid).expect("受害者应存活");

        // 判据是**精确相等**，差 1 毫秒就足够构造出"对不上"
        seed(&table, "run-reuse", pid, Some(real_start + 1));
        let err = stop_tool_command_inner(&table, "run-reuse").expect_err("启动时间对不上时必须拒绝");
        assert!(err.contains("已被系统复用"), "错误要说明是 pid 复用，实际: {}", err);
        assert!(is_alive(pid), "被拒绝的停止绝不能杀掉无关进程 (pid={})", pid);

        reap_victim(pid, &mut victim);
    }

    /// 正面控制：启动时间**一致**时必须真的终止。
    /// 没有这条，上面那条可以靠"一律拒绝"通过——那会让「停止」按钮整个失效。
    #[cfg(windows)]
    #[test]
    fn test_stop_kills_when_pid_identity_matches() {
        let table = ToolCmdTable::new();
        let mut victim = spawn_victim(20);
        let pid = victim.id();
        seed(&table, "run-match", pid, crate::core::process::process_start_time_millis(pid));

        stop_tool_command_inner(&table, "run-match").expect("身份一致时应当终止");
        assert!(wait_gone(pid), "停止后进程不该还在运行 (pid={})", pid);
        let _ = victim.wait();
    }

    // ── ③ 超时分支 ───────────────────────────────────────────────

    /// 超时分支必须把进程真的杀干净，并且如实报告。
    ///
    /// 判据不只是文案：真起一条 `ping -n 20`（约 19 秒）给它 1.5 秒超时，
    /// 然后断言这个 pid 不再运行——"超时返回了但进程还在跑"正是这条分支要防的事。
    #[cfg(windows)]
    #[test]
    fn test_timeout_kills_spawned_process() {
        let table = ToolCmdTable::new();
        let probe = table.clone();
        let handle = std::thread::spawn(move || {
            tauri::async_runtime::block_on(execute_tool_command(
                table,
                None,
                noop_sink(),
                sleep_cmd(20),
                String::new(),
                Some(Duration::from_millis(1500)),
                "run-timeout".to_string(),
            ))
        });
        // 必须在命令还跑着的时候取 pid：表项在它结束那一刻就被守卫摘掉了
        let pid = wait_for_pid(&probe, "run-timeout").expect("命令应当被拉起并登记 pid");

        let err = handle.join().expect("执行线程不应 panic").expect_err("超时应当返回错误");
        assert!(err.contains("命令执行超时"), "实际: {}", err);
        assert!(err.contains("已终止"), "必须如实报告是否终止，实际: {}", err);
        assert!(wait_gone(pid), "超时分支必须把进程真的杀掉 (pid={} 仍在运行)", pid);
    }

    // ── ④ kill_then 早退路径 ─────────────────────────────────────

    /// `kill_then` 早退：既终止回收子进程，也回收运行表项。
    ///
    /// 触发方式是"没接管 stdout"——生产里 `child.stdout.take()` 返回 None 就走这条分支。
    /// 原实现让 `child` 直接 drop：进程变成"存活但已从运行表摘除"，`stop_tool_command`
    /// 再也找不到它（进程树会一直跑下去）；表项不回收则让这个 run_id 永远停在"忙"。
    #[cfg(windows)]
    #[test]
    fn test_kill_then_early_exit_reaps_process_and_reclaims_table_entry() {
        let table = ToolCmdTable::new();
        let guard = table.reserve("run-early").expect("占位");
        let mut cmd = crate::core::process::build_command(&sleep_cmd(20));
        cmd.env("NoDefaultCurrentDirectoryInExePath", "1");
        let child = cmd
            .stdin(std::process::Stdio::null())
            // **故意不接管管道**：拿不到 stdout 就是那条早退路径
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("起子进程");
        let pid = child.id();
        guard.set_pid(pid);
        assert!(is_alive(pid), "子进程应当先真的起来");

        let err = run_tool_child(child, guard, None, noop_sink(), "run-early".to_string())
            .expect_err("拿不到管道时应当走 kill_then 早退");
        assert!(err.contains("无法获取 stdout"), "实际: {}", err);
        assert!(wait_gone(pid), "早退路径必须终止并回收子进程 (pid={} 仍在运行)", pid);
        assert!(
            table.peek("run-early").is_none(),
            "早退路径必须回收运行表项（否则这个 run_id 永远停在「已被占用」）"
        );
    }

    /// spawn 失败的 `?` 早退同样要回收表项。
    ///
    /// 用"不存在的工作目录"制造失败——这正是线上最常见的失败形态（服务的 cwd 被删/改名）。
    /// 不回收的话，用户改错一次，`run_id` 会永久停在"已被占用"，而他看到的只是"跑不起来"。
    #[test]
    fn test_spawn_failure_reclaims_table_entry() {
        let table = ToolCmdTable::new();
        let missing = std::env::temp_dir().join(format!("nexus-ut-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&missing);

        let err = tauri::async_runtime::block_on(execute_tool_command(
            table.clone(),
            None,
            noop_sink(),
            "echo hi".to_string(),
            missing.to_string_lossy().to_string(),
            Some(Duration::from_secs(10)),
            "run-badcwd".to_string(),
        ))
        .expect_err("工作目录不存在时应当失败");
        assert!(err.contains("执行命令失败"), "实际: {}", err);
        assert!(table.peek("run-badcwd").is_none(), "失败路径必须回收运行表项");
        // 不只是"看不见了"：同一个 run_id 必须能再占上
        let again = table.reserve("run-badcwd").expect("失败后 run_id 应当可以重新使用");
        drop(again);
    }

    // ── 正常路径（顺带钉住 ARCH-25② 的注入点）─────────────────────

    /// 正常跑完：退出码、两流合并输出、以及**实时输出真的经注入的发送端发出去了**。
    #[cfg(windows)]
    #[test]
    fn test_execute_reports_exit_code_and_streams_output_through_sink() {
        let table = ToolCmdTable::new();
        let (sink, got) = recording_sink();
        let res = tauri::async_runtime::block_on(execute_tool_command(
            table.clone(),
            None,
            sink,
            "echo tool-cmd-out & echo tool-cmd-err 1>&2 & exit /b 7".to_string(),
            String::new(),
            Some(Duration::from_secs(30)),
            "run-ok".to_string(),
        ))
        .expect("命令应当执行完成");

        assert!(!res.success, "exit /b 7 不该被当成成功");
        assert_eq!(res.exit_code, Some(7), "退出码要原样带回来");
        assert!(res.output.contains("tool-cmd-out"), "stdout 要被收集：{:?}", res.output);
        assert!(res.output.contains("tool-cmd-err"), "stderr 要被收集：{:?}", res.output);

        // 发射线程独立判断"读完了且缓冲空了"才收尾，所以这里等它把最后一批送出来
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let lines = sink_lines(&got);
            let saw_out = lines.iter().any(|l| l.contains("tool-cmd-out"));
            let saw_err = lines.iter().any(|l| l.contains("tool-cmd-err"));
            if saw_out && saw_err {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "实时输出未在 5 秒内发出，实际收到: {:?}",
                lines
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        // 事件里带的 run_id 必须是本次的（前端按它区分并发执行）
        assert!(
            got.lock().expect("sink 锁").iter().all(|b| b.run_id == "run-ok"),
            "批量事件必须带本次 run_id"
        );

        assert!(table.peek("run-ok").is_none(), "正常结束后运行表项必须回收");
    }
}
