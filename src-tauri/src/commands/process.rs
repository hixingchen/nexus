use std::collections::VecDeque;
use std::io::BufRead;
use std::time::Duration;
use tauri::{Emitter, Manager, State};
use serde::Serialize;
use crate::AppState;
use crate::core::process::LogLine;

/// 工具命令执行超时：防止长驻命令（npm run dev 等）永久占用线程池
const TOOL_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
/// 工具命令输出最多保留的行数（对齐服务日志上限，防止超长输出撑爆 IPC payload 和前端渲染）
const TOOL_CMD_OUTPUT_MAX_LINES: usize = 2000;

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

#[tauri::command]
pub fn start_service(state: State<AppState>, app_handle: tauri::AppHandle, service_id: String) -> Result<(), String> {
    if service_id.trim().is_empty() { return Err("服务ID不能为空".into()); }
    let (name, command, cwd, project_id, env_vars) = get_service_info(&state.db, &service_id)?;
    let envs = crate::core::process::parse_env_vars(&env_vars);
    state.process_mgr.start(&project_id, &service_id, &name, &command, &cwd, &envs, &app_handle)
        .map_err(|e| format!("服务「{}」{}", name, e))
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
        let envs = crate::core::process::parse_env_vars(&env_vars);
        state.process_mgr.restart(&project_id, &service_id, &name, &command, &cwd, &envs, &app)
            .map_err(|e| format!("服务「{}」{}", name, e))
    }).await.map_err(|e| format!("重启服务任务失败: {}", e))?
}

#[tauri::command]
pub fn start_project_services(state: State<AppState>, app_handle: tauri::AppHandle, project_id: String) -> Result<Vec<String>, String> {
    if project_id.trim().is_empty() { return Err("项目ID不能为空".into()); }
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
        let envs = crate::core::process::parse_env_vars(env_vars);
        if let Err(e) = state.process_mgr.start(&project_id, id, name, cmd, cwd, &envs, &app_handle) {
            errors.push(format!("{}: {}", name, e));
        }
    }
    Ok(errors)
}

/// 停止项目全部服务（每服务清理最长数秒 × N → 异步执行避免阻塞 IPC）
#[tauri::command]
pub async fn stop_project_services(app: tauri::AppHandle, project_id: String) -> Result<(), String> {
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
        for id in &ids {
            let _ = state.process_mgr.stop(id);
        }
        // 停止项目级文件监听（项目停止 = 总开关，所有服务监听一并关闭）
        let _ = state.file_watcher.stop_watching(&project_id);
        Ok(())
    }).await.map_err(|e| format!("停止项目服务任务失败: {}", e))?
}

#[tauri::command]
pub fn get_running(state: State<AppState>) -> Result<ProcessStatus, String> {
    Ok(ProcessStatus {
        running: state.process_mgr.running().into_iter()
            .map(|(project_id, service_id)| RunningService { service_id, project_id })
            .collect(),
        failed: state.process_mgr.failed(),
    })
}

#[tauri::command]
pub fn get_service_logs(state: State<AppState>, service_key: String) -> Result<Vec<LogLine>, String> {
    if service_key.trim().is_empty() { return Err("服务标识不能为空".into()); }
    Ok(state.process_mgr.get_logs(&service_key))
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
        project_id, service_name, tool_cmd.name, tool_cmd.command, cwd);

    // 在线程池中执行命令，避免阻塞主线程（长命令如 npm run build 会冻结整个 UI）
    // 带 60 秒超时：长驻命令（npm run dev 之类）不会永久占用线程池和前端等待
    // 输出由双线程逐行读取并 emit（前端实时展示），主线程只等进程退出拿退出码
    let cmd_str = tool_cmd.command;
    let cwd_clone = cwd.clone();
    let (pid_tx, pid_rx) = tokio::sync::oneshot::channel::<u32>();
    let result = tokio::time::timeout(TOOL_COMMAND_TIMEOUT, async {
        tauri::async_runtime::spawn_blocking(move || {
            let mut cmd = crate::core::process::build_command(&cmd_str);
            if !cwd_clone.is_empty() {
                cmd.current_dir(&cwd_clone);
            }
            let mut child = cmd.stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| format!("执行命令失败: {}", e))?;
            let _ = pid_tx.send(child.id());

            let stdout = child.stdout.take().ok_or("无法获取 stdout")?;
            let stderr = child.stderr.take().ok_or("无法获取 stderr")?;

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
            let stdout_handle = spawn_reader("stdout", Box::new(stdout))?;
            let stderr_handle = spawn_reader("stderr", Box::new(stderr))?;

            let status = child.wait().map_err(|e| format!("等待命令退出失败: {}", e))?;
            let mut all: Vec<_> = stdout_handle.join().map_err(|_| "stdout 读取线程异常".to_string())?
                .into_iter()
                .chain(stderr_handle.join().map_err(|_| "stderr 读取线程异常".to_string())?.into_iter())
                .collect();
            all.sort_by_key(|(n, _)| *n);
            let output = all.into_iter().map(|(_, l)| l).collect::<Vec<_>>().join("\n");
            Ok::<_, String>((status, output))
        }).await.map_err(|e| format!("命令执行任务失败: {}", e))?
    }).await;
    let result = match result {
        Ok(inner) => inner?,
        Err(_) => {
            // 超时：终止命令进程树，避免后台残留（已 emit 的部分输出仍保留在前端）。
            // pid 由 spawn_blocking 任务 spawn 成功后上报：oneshot 等待非阻塞
            // （原 50×100ms sleep 轮询会占用 tokio worker）。若任务仍在阻塞池排队
            // 收不到 pid——命令随后仍会照常启动（无法拦截），如实告知而非谎报"已终止"
            let pid = tokio::time::timeout(Duration::from_secs(5), pid_rx).await.ok().and_then(|r| r.ok());
            return match pid {
                Some(p) => {
                    crate::core::process::kill_process_tree(p);
                    Err("命令执行超时（60 秒），已终止".into())
                }
                None => Err("命令执行超时（60 秒），且未能取得进程 ID——命令可能仍在启动队列中，请留意是否残留运行".into()),
            };
        }
    };

    let (status, output) = result;
    Ok(ToolCommandResult {
        success: status.success(),
        output,
        exit_code: status.code(),
    })
}
