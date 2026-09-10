use std::collections::VecDeque;
use std::io::BufRead;
use std::time::Duration;
use tauri::{Emitter, Manager, State};
use serde::Serialize;
use crate::AppState;
use crate::core::process::LogLine;

/// 工具命令默认执行超时（未配置 timeout_secs 时）：防止长驻命令（npm run dev 等）
/// 永久占用线程池。构建类长任务应在命令上配置 timeout_secs（如 1800）或 0（不限）
const TOOL_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
/// 工具命令输出最多保留的行数（对齐服务日志上限，防止超长输出撑爆 IPC payload 和前端渲染）
const TOOL_CMD_OUTPUT_MAX_LINES: usize = 2000;

/// 运行中的工具命令表（run_id → pid）：供 stop_tool_command 按 run_id 终止进程树，
/// 也是超时兜底杀进程的 pid 来源（不再用 oneshot 单次传递）
fn running_tool_cmds() -> &'static std::sync::Mutex<std::collections::HashMap<String, u32>> {
    static TABLE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, u32>>> =
        std::sync::OnceLock::new();
    TABLE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// 读取运行表中的 pid（不移除）。超时分支只在阻塞任务真正结束前需要它，
/// 表项的移除交给 RunTableGuard，避免"提前取走 pid → 任务结束时无项可清"
fn peek_running_pid(run_id: &str) -> Option<u32> {
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
    fn register(run_id: &str, pid: u32) -> Self {
        if let Ok(mut table) = running_tool_cmds().lock() {
            table.insert(run_id.to_string(), pid);
        }
        Self { run_id: run_id.to_string() }
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

/// 查询服务信息（name, command, cwd, project_id, env_vars）
fn get_service_info(db: &crate::database::Database, service_id: &str) -> Result<(String, String, String, String, String), String> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT name, command, cwd, project_id, env_vars FROM services WHERE id=?1",
            [service_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, String>(4)?)),
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

/// 工具命令实时输出事件 payload（run_id 用于前端区分并发执行）
#[derive(Clone, Serialize)]
pub struct ToolCommandLogPayload {
    pub run_id: String,
    pub stream: String,
    pub data: String,
}

/// 运行中服务（含所属项目，供前端按项目维度判断运行状态）
#[derive(Serialize)]
pub struct RunningService {
    pub service_id: String,
    pub project_id: String,
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
            command: &command, cwd: &cwd, env_vars: &envs, app_handle: &app_handle,
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
            command: &command, cwd: &cwd, env_vars: &envs, app_handle: &app,
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
                Ok((row.get::<_,String>(0)?, row.get::<_,String>(1)?, row.get::<_,String>(2)?, row.get::<_,String>(3)?, row.get::<_,String>(4)?))
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
                env_vars: &envs, app_handle: &app_handle,
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
                .map(|(project_id, service_id)| RunningService { service_id, project_id })
                .collect(),
            failed: state.process_mgr.failed(),
        })
    }).await.map_err(|e| format!("获取运行状态任务失败: {}", e))?
}

/// 读取服务的日志快照。
///
/// 异步 + spawn_blocking：`get_logs` 会在日志锁内克隆最多 2000 条 `LogLine`
/// （每行 3 个 String，最坏约 16MB）并参与 IPC 序列化。同步执行时这段工作内联在
/// IPC 请求路径上，与日志面板的轮询/切换叠加会造成可感的卡顿。
#[tauri::command]
pub async fn get_service_logs(app: tauri::AppHandle, service_key: String) -> Result<Vec<LogLine>, String> {
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
    // run_id 是运行表的 key：重复会覆盖旧表项，令旧进程失去可终止句柄
    if running_tool_cmds().lock().map(|m| m.contains_key(&run_id)).unwrap_or(false) {
        return Err(format!("run_id 已被占用: {}", run_id));
    }

    // 获取服务信息和工具命令
    let (service_name, _command, cwd, project_id, tool_commands_json) = state.db.with_conn(|conn| {
        conn.query_row(
            "SELECT name, command, cwd, project_id, tool_commands FROM services WHERE id=?1",
            [&service_id],
            |row| Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
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
            // 注册到运行表（stop_tool_command / 超时兜底据此按 run_id 终止进程树）；
            // 守卫在闭包任意返回路径（含 `?` 早退）都会清理表项
            let _run_guard = RunTableGuard::register(&run_id, pid);
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

            // stdout/stderr 各起一个读取线程：逐行 emit 事件 + 收集 (序号, 行)
            // 序号为全局递增计数，结束时按序号合并，还原两流的真实交错顺序
            let seq = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let spawn_reader = {
                let app = app_handle.clone();
                let run_id = run_id.clone();
                move |stream: &'static str, pipe: Box<dyn std::io::Read + Send>| {
                    let app = app.clone();
                    let run_id = run_id.clone();
                    let seq = seq.clone();
                    std::thread::Builder::new()
                        .name(format!("nexus-toolcmd-{stream}"))
                        .spawn(move || {
                            let reader = std::io::BufReader::new(pipe);
                            // 只保留最新 N 行：全量收集在超长输出时撑爆 IPC payload
                            let mut collected: VecDeque<(u64, String)> = VecDeque::with_capacity(TOOL_CMD_OUTPUT_MAX_LINES);
                            for line in reader.lines() {
                                let Ok(line) = line else { continue };
                                let line = crate::core::process::truncate_line(line, 8192);
                                let n = seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                let _ = app.emit("tool-command-log", ToolCommandLogPayload {
                                    run_id: run_id.clone(),
                                    stream: stream.into(),
                                    data: line.clone(),
                                });
                                if collected.len() >= TOOL_CMD_OUTPUT_MAX_LINES {
                                    collected.pop_front();
                                }
                                collected.push_back((n, line));
                            }
                            collected
                        })
                        .map_err(|e| format!("创建{stream}读取线程失败: {}", e))
                }
            };
            let stdout_handle = match spawn_reader("stdout", Box::new(stdout)) {
                Ok(h) => h,
                Err(e) => return kill_then(&mut child, e),
            };
            let stderr_handle = match spawn_reader("stderr", Box::new(stderr)) {
                Ok(h) => h,
                Err(e) => return kill_then(&mut child, e),
            };

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
            // pid 取自运行表（spawn 成功即注册）；若命令仍在阻塞池排队（尚未 spawn）
            // 则取不到 pid——命令随后仍会照常启动（无法拦截），如实告知而非谎报"已终止"
            let pid = peek_running_pid(&run_id);
            let secs = timeout_dur.map(|d| d.as_secs()).unwrap_or(0);
            return match pid {
                Some(p) => {
                    crate::core::process::kill_process_tree(p);
                    Err(format!("命令执行超时（{} 秒），已终止。长构建类命令请在工具命令上配置更长的超时（或 0 = 不限制）", secs))
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
    let pid = running_tool_cmds()
        .lock()
        .map_err(|e| format!("工具命令运行表锁中毒: {}", e))?
        .get(&run_id)
        .copied();
    match pid {
        Some(p) => {
            log::info!("[nexus] 停止工具命令 run_id={} pid={}", run_id, p);
            crate::core::process::kill_process_tree(p);
            Ok(())
        }
        None => Err("命令未在运行（可能已结束）".into()),
    }
}
