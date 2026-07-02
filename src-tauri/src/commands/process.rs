use tauri::State;
use serde::Serialize;
use crate::AppState;
use crate::core::process::LogLine;

/// 查询服务信息（name, command, cwd, project_id）
fn get_service_info(db: &crate::database::Database, service_id: &str) -> Result<(String, String, String, String), String> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT name, command, cwd, project_id FROM services WHERE id=?1",
            [service_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?)),
        )
        .map_err(|e| format!("服务不存在: {}", e))
    })
}

/// 工具命令执行结果
#[derive(Serialize)]
pub struct ToolCommandResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

// ─── Tauri Commands ───────────────────────────────────────────

#[tauri::command]
pub fn start_service(state: State<AppState>, app_handle: tauri::AppHandle, service_id: String) -> Result<(), String> {
    if service_id.trim().is_empty() { return Err("服务ID不能为空".into()); }
    let (name, command, cwd, project_id) = get_service_info(&state.db, &service_id)?;
    let key = format!("{}:{}", project_id, name);
    state.process_mgr.start(&key, &command, &cwd, &app_handle)
}

#[tauri::command]
pub fn stop_service(state: State<AppState>, service_id: String) -> Result<(), String> {
    if service_id.trim().is_empty() { return Err("服务ID不能为空".into()); }
    let (name, _command, _cwd, project_id) = get_service_info(&state.db, &service_id)?;
    let key = format!("{}:{}", project_id, name);
    state.process_mgr.stop(&key)
}

#[tauri::command]
pub fn restart_service(state: State<AppState>, app_handle: tauri::AppHandle, service_id: String) -> Result<(), String> {
    if service_id.trim().is_empty() { return Err("服务ID不能为空".into()); }
    let (name, command, cwd, project_id) = get_service_info(&state.db, &service_id)?;
    let key = format!("{}:{}", project_id, name);
    state.process_mgr.restart(&key, &command, &cwd, &app_handle)
}

#[tauri::command]
pub fn start_project_services(state: State<AppState>, app_handle: tauri::AppHandle, project_id: String) -> Result<Vec<String>, String> {
    if project_id.trim().is_empty() { return Err("项目ID不能为空".into()); }
    let services = state.db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, name, command, cwd FROM services WHERE project_id=?1 AND enabled=1 ORDER BY sort_index"
        ).map_err(|e| format!("查询项目服务列表失败: {}", e))?;
        let rows = stmt.query_map([&project_id], |row| {
            Ok((row.get::<_,String>(0)?, row.get::<_,String>(1)?, row.get::<_,String>(2)?, row.get::<_,String>(3)?))
        }).map_err(|e| format!("读取项目服务数据失败: {}", e))?;
        let mut svcs = Vec::new();
        for r in rows { svcs.push(r.map_err(|e| format!("解析项目服务数据失败: {}", e))?); }
        Ok::<_, String>(svcs)
    })?;
    let mut errors = Vec::new();
    for (_id, name, cmd, cwd) in &services {
        let key = format!("{}:{}", project_id, name);
        if let Err(e) = state.process_mgr.start(&key, cmd, cwd, &app_handle) {
            errors.push(format!("{}: {}", name, e));
        }
    }
    Ok(errors)
}

#[tauri::command]
pub fn stop_project_services(state: State<AppState>, project_id: String) -> Result<(), String> {
    if project_id.trim().is_empty() { return Err("项目ID不能为空".into()); }
    state.process_mgr.stop_by_prefix(&format!("{}:", project_id));
    Ok(())
}

#[tauri::command]
pub fn get_running(state: State<AppState>) -> Result<Vec<String>, String> {
    Ok(state.process_mgr.running())
}

#[tauri::command]
pub fn get_service_logs(state: State<AppState>, service_key: String) -> Result<Vec<LogLine>, String> {
    if service_key.trim().is_empty() { return Err("服务标识不能为空".into()); }
    Ok(state.process_mgr.get_logs(&service_key))
}

/// 执行工具命令（一次性执行，返回输出）
#[tauri::command]
pub fn run_tool_command(
    state: State<AppState>,
    service_id: String,
    command_id: String,
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
        .ok_or_else(|| format!("工具命令不存在: {}", command_id))?;

    log::info!("[nexus] 执行工具命令: {} -> {} (cmd={:?}, cwd={:?})",
        format!("{}:{}", project_id, service_name), tool_cmd.name, tool_cmd.command, cwd);

    // 执行命令
    let mut cmd = crate::core::process::build_command(&tool_cmd.command);
    if !cwd.is_empty() {
        cmd.current_dir(&cwd);
    }

    let output = cmd.output()
        .map_err(|e| format!("执行命令失败: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code();

    Ok(ToolCommandResult {
        success: output.status.success(),
        stdout,
        stderr,
        exit_code,
    })
}
