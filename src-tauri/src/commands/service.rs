use tauri::State;
use serde::Deserialize;
use crate::AppState;
use crate::models::Service;
use crate::database::query_services_by_project;

/// 默认文件监听排除规则
const DEFAULT_WATCH_EXCLUDE: &str = "node_modules\n.git\ndist\ntarget\n__pycache__\n.next\nbuild\ncoverage\n*.log";

/// 服务更新参数
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateServiceParams {
    pub id: String,
    pub name: String,
    pub command: String,
    pub cwd: String,
    pub watch_paths: String,
    pub watch_include: String,
    pub watch_exclude: String,
    pub env_vars: String,
    pub restart_mode: i32,
    pub enabled: bool,
    pub show_file_tree: bool,
    pub tool_commands: String,
}

/// 服务添加参数
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddServiceParams {
    pub project_id: String,
    pub name: String,
    pub command: String,
    pub cwd: String,
    pub watch_paths: String,
    pub env_vars: String,
    pub restart_mode: i32,
    pub tool_commands: String,
}

/// 获取某个项目下的所有服务
#[tauri::command]
pub fn get_services(state: State<AppState>, project_id: String) -> Result<Vec<Service>, String> {
    if project_id.trim().is_empty() { return Err("项目ID不能为空".into()); }
    state.db.with_conn(|conn| query_services_by_project(conn, &project_id))
}

/// 给项目添加一个服务
///
/// 安全设计：`command` 字段来自用户在项目中配置的服务命令。
/// 信任边界：用户只能管理自己的项目，命令执行在其配置的工作目录中。
/// 命令通过 `cmd /C`（Windows 含 shell 元字符时）或直接执行，见 `process::build_command`。
#[tauri::command]
pub fn add_service(
    state: State<AppState>,
    params: AddServiceParams,
) -> Result<Service, String> {
    if params.project_id.trim().is_empty() { return Err("项目ID不能为空".into()); }
    if params.name.trim().is_empty() { return Err("名称不能为空".into()); }
    if params.command.trim().is_empty() { return Err("命令不能为空".into()); }
    let cwd = params.cwd.replace('\\', "/");
    let tool_commands = if params.tool_commands.trim().is_empty() { "[]".to_string() } else { params.tool_commands };
    state.db.with_conn(|conn| {
        let exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM projects WHERE id=?1", [&params.project_id], |r| r.get(0)
        ).unwrap_or(false);
        if !exists { return Err("所属项目不存在".into()); }

        let id = uuid::Uuid::new_v4().to_string();
        let max_sort: i32 = conn.query_row(
            "SELECT COALESCE(MAX(sort_index), -1) FROM services WHERE project_id=?1",
            [&params.project_id], |r| r.get(0)
        ).unwrap_or(-1);
        let wp = if params.watch_paths.trim().is_empty() || params.watch_paths == "[]" {
            if cwd.is_empty() { "[]".to_string() } else { format!("[\"{}\"]", cwd) }
        } else { params.watch_paths };
        let wi = "*";
        let wx = DEFAULT_WATCH_EXCLUDE;
        conn.execute(
            "INSERT INTO services (id, project_id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, sort_index, tool_commands)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,1,1,?11,?12)",
            rusqlite::params![id, params.project_id, params.name.trim(), params.command, cwd, wp, wi, wx, params.env_vars, params.restart_mode, max_sort + 1, tool_commands],
        ).map_err(|e| format!("添加服务失败: {}", e))?;
        Ok(Service {
            id, project_id: params.project_id, name: params.name.trim().to_string(), command: params.command,
            cwd, watch_paths: wp, watch_include: wi.into(), watch_exclude: wx.into(),
            env_vars: params.env_vars, restart_mode: params.restart_mode, enabled: true,
            show_file_tree: true,
            sort_index: max_sort + 1,
            tool_commands,
        })
    })
}

/// 更新服务配置
#[tauri::command]
pub fn update_service(
    state: State<AppState>,
    params: UpdateServiceParams,
) -> Result<(), String> {
    if params.id.trim().is_empty() { return Err("服务ID不能为空".into()); }
    let cwd = params.cwd.replace('\\', "/");
    if params.name.trim().is_empty() { return Err("名称不能为空".into()); }
    if params.command.trim().is_empty() { return Err("命令不能为空".into()); }
    let tool_commands = if params.tool_commands.trim().is_empty() { "[]".to_string() } else { params.tool_commands };
    state.db.with_conn(|conn| {
        let en = if params.enabled { 1 } else { 0 };
        let sft = if params.show_file_tree { 1 } else { 0 };
        let affected = conn.execute(
            "UPDATE services SET name=?1, command=?2, cwd=?3, watch_paths=?4, watch_include=?5, watch_exclude=?6, env_vars=?7, restart_mode=?8, enabled=?9, show_file_tree=?10, tool_commands=?11 WHERE id=?12",
            rusqlite::params![params.name.trim(), params.command, cwd, params.watch_paths, params.watch_include, params.watch_exclude, params.env_vars, params.restart_mode, en, sft, tool_commands, params.id],
        ).map_err(|e| format!("更新服务失败: {}", e))?;
        if affected == 0 { return Err("服务不存在".into()); }
        Ok(())
    })
}

/// 删除服务
#[tauri::command]
pub fn delete_service(state: State<AppState>, id: String) -> Result<(), String> {
    if id.trim().is_empty() { return Err("服务ID不能为空".into()); }
    state.db.with_conn(|conn| {
        let (name, project_id): (String, String) = conn.query_row(
            "SELECT name, project_id FROM services WHERE id=?1", [&id],
            |r| Ok((r.get(0)?, r.get(1)?))
        ).map_err(|e| format!("服务不存在: {}", e))?;
        let _ = state.process_mgr.stop(&format!("{}:{}", project_id, name));
        conn.execute("DELETE FROM services WHERE id=?1", [&id]).map_err(|e| format!("删除服务失败: {}", e))?;
        Ok(())
    })
}
