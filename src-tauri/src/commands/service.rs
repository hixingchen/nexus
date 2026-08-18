use tauri::State;
use serde::Deserialize;
use crate::AppState;
use crate::models::{Service, ServiceTemplate};
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
            if cwd.is_empty() {
                "[]".to_string()
            } else {
                // 用 serde_json 序列化，避免 cwd 含引号/反斜杠时生成非法 JSON
                serde_json::to_string(&vec![cwd.clone()]).unwrap_or_else(|_| "[]".to_string())
            }
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

/// 重排项目服务顺序（ordered_ids 为新的展示顺序，sort_index 按序重写）
#[tauri::command]
pub fn reorder_services(state: State<AppState>, project_id: String, ordered_ids: Vec<String>) -> Result<(), String> {
    if project_id.trim().is_empty() { return Err("项目ID不能为空".into()); }
    state.db.with_conn_mut(|conn| {
        let tx = conn.transaction().map_err(|e| format!("开启事务失败: {}", e))?;
        // 校验 id 归属：只更新该项目下的服务，防御跨项目写入
        let mut stmt = tx.prepare("SELECT id FROM services WHERE project_id=?1")
            .map_err(|e| format!("查询服务失败: {}", e))?;
        let existing: std::collections::HashSet<String> = stmt.query_map([&project_id], |r| r.get::<_, String>(0))
            .map_err(|e| format!("查询服务失败: {}", e))?
            .collect::<Result<_, _>>()
            .map_err(|e| format!("查询服务失败: {}", e))?;
        drop(stmt);
        for (i, id) in ordered_ids.iter().enumerate() {
            if !existing.contains(id) { continue; }
            tx.execute("UPDATE services SET sort_index=?1 WHERE id=?2", rusqlite::params![i as i32, id])
                .map_err(|e| format!("更新服务排序失败: {}", e))?;
        }
        tx.commit().map_err(|e| format!("提交事务失败: {}", e))?;
        Ok(())
    })
}

/// 删除服务
#[tauri::command]
pub fn delete_service(state: State<AppState>, id: String) -> Result<(), String> {
    if id.trim().is_empty() { return Err("服务ID不能为空".into()); }
    // 1. 停止运行中的进程（进程 key = service_id，锁外执行避免阻塞 DB 操作）
    let _ = state.process_mgr.stop(&id);
    // 2. 删除数据库记录
    state.db.with_conn(|conn| {
        conn.execute("DELETE FROM services WHERE id=?1", [&id]).map_err(|e| format!("删除服务失败: {}", e))?;
        Ok(())
    })
}

// ─── 服务模板（跨项目复用） ─────────────────────────────────

/// 查询全部服务模板（按 sort_index 排序，与拖拽重排一致）
#[tauri::command]
pub fn get_service_templates(state: State<AppState>) -> Result<Vec<ServiceTemplate>, String> {
    state.db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, tool_commands, created_at
             FROM service_templates ORDER BY sort_index, name"
        ).map_err(|e| format!("查询模板失败: {}", e))?;
        let rows = stmt.query_map([], |row| Ok(ServiceTemplate {
            id: row.get(0)?, name: row.get(1)?, command: row.get(2)?, cwd: row.get(3)?,
            watch_paths: row.get(4)?, watch_include: row.get(5)?, watch_exclude: row.get(6)?,
            env_vars: row.get(7)?, restart_mode: row.get(8)?, enabled: row.get(9)?,
            show_file_tree: row.get(10)?, tool_commands: row.get(11)?, created_at: row.get(12)?,
        })).map_err(|e| format!("查询模板失败: {}", e))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("查询模板失败: {}", e))
    })
}

/// 把项目里的服务保存为模板（值拷贝，不关联原项目，可重复保存产生多个副本）
#[tauri::command]
pub fn save_service_as_template(state: State<AppState>, service_id: String) -> Result<ServiceTemplate, String> {
    state.db.with_conn(|conn| {
        let t = conn.query_row(
            "SELECT name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, tool_commands
             FROM services WHERE id=?1",
            [&service_id],
            |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
                row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?,
                row.get::<_, String>(6)?, row.get::<_, i32>(7)?, row.get::<_, bool>(8)?,
                row.get::<_, bool>(9)?, row.get::<_, String>(10)?,
            )),
        ).map_err(|e| format!("服务不存在: {}", e))?;
        let (name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, tool_commands) = t;

        let id = uuid::Uuid::new_v4().to_string();
        let en = if enabled { 1 } else { 0 };
        let sft = if show_file_tree { 1 } else { 0 };
        conn.execute(
            "INSERT INTO service_templates (id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, sort_index, tool_commands)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,0,?12)",
            rusqlite::params![id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, en, sft, tool_commands],
        ).map_err(|e| format!("保存模板失败: {}", e))?;
        Ok(ServiceTemplate {
            id, name, command, cwd, watch_paths, watch_include, watch_exclude,
            env_vars, restart_mode, enabled, show_file_tree, tool_commands,
            created_at: String::new(),
        })
    })
}

/// 从模板添加服务到项目（值拷贝模板全部配置，模板不受影响）
#[tauri::command]
pub fn add_service_from_template(
    state: State<AppState>,
    project_id: String,
    template_id: String,
) -> Result<Service, String> {
    state.db.with_conn(|conn| {
        let project_exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM projects WHERE id=?1", [&project_id], |r| r.get(0)
        ).unwrap_or(false);
        if !project_exists { return Err("所属项目不存在".into()); }

        let t = conn.query_row(
            "SELECT name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, tool_commands
             FROM service_templates WHERE id=?1",
            [&template_id],
            |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
                row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?,
                row.get::<_, String>(6)?, row.get::<_, i32>(7)?, row.get::<_, bool>(8)?,
                row.get::<_, bool>(9)?, row.get::<_, String>(10)?,
            )),
        ).map_err(|e| format!("模板不存在: {}", e))?;
        let (name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, tool_commands) = t;

        let id = uuid::Uuid::new_v4().to_string();
        let max_sort: i32 = conn.query_row(
            "SELECT COALESCE(MAX(sort_index), -1) FROM services WHERE project_id=?1",
            [&project_id], |r| r.get(0)
        ).unwrap_or(-1);
        let en = if enabled { 1 } else { 0 };
        let sft = if show_file_tree { 1 } else { 0 };
        conn.execute(
            "INSERT INTO services (id, project_id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, sort_index, tool_commands)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            rusqlite::params![id, project_id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, en, sft, max_sort + 1, tool_commands],
        ).map_err(|e| format!("添加服务失败: {}", e))?;
        Ok(Service {
            id, project_id, name, command, cwd, watch_paths, watch_include, watch_exclude,
            env_vars, restart_mode, enabled, show_file_tree, sort_index: max_sort + 1, tool_commands,
        })
    })
}

/// 重排服务模板顺序（ordered_ids 为新的展示顺序，sort_index 按序重写）
#[tauri::command]
pub fn reorder_service_templates(state: State<AppState>, ordered_ids: Vec<String>) -> Result<(), String> {
    state.db.with_conn_mut(|conn| {
        let tx = conn.transaction().map_err(|e| format!("开启事务失败: {}", e))?;
        let mut stmt = tx.prepare("SELECT id FROM service_templates")
            .map_err(|e| format!("查询模板失败: {}", e))?;
        let existing: std::collections::HashSet<String> = stmt.query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| format!("查询模板失败: {}", e))?
            .collect::<Result<_, _>>()
            .map_err(|e| format!("查询模板失败: {}", e))?;
        drop(stmt);
        for (i, id) in ordered_ids.iter().enumerate() {
            if !existing.contains(id) { continue; }
            tx.execute("UPDATE service_templates SET sort_index=?1 WHERE id=?2", rusqlite::params![i as i32, id])
                .map_err(|e| format!("更新模板排序失败: {}", e))?;
        }
        tx.commit().map_err(|e| format!("提交事务失败: {}", e))?;
        Ok(())
    })
}

/// 删除服务模板
#[tauri::command]
pub fn delete_service_template(state: State<AppState>, id: String) -> Result<(), String> {
    if id.trim().is_empty() { return Err("模板ID不能为空".into()); }
    state.db.with_conn(|conn| {
        conn.execute("DELETE FROM service_templates WHERE id=?1", [&id])
            .map_err(|e| format!("删除模板失败: {}", e))?;
        Ok(())
    })
}

/// 服务模板更新参数
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateServiceTemplateParams {
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

/// 更新服务模板配置（编辑模板本身，不影响已从模板添加的项目服务）
#[tauri::command]
pub fn update_service_template(
    state: State<AppState>,
    params: UpdateServiceTemplateParams,
) -> Result<(), String> {
    if params.id.trim().is_empty() { return Err("模板ID不能为空".into()); }
    let cwd = params.cwd.replace('\\', "/");
    if params.name.trim().is_empty() { return Err("名称不能为空".into()); }
    if params.command.trim().is_empty() { return Err("命令不能为空".into()); }
    let tool_commands = if params.tool_commands.trim().is_empty() { "[]".to_string() } else { params.tool_commands };
    state.db.with_conn(|conn| {
        let en = if params.enabled { 1 } else { 0 };
        let sft = if params.show_file_tree { 1 } else { 0 };
        let affected = conn.execute(
            "UPDATE service_templates SET name=?1, command=?2, cwd=?3, watch_paths=?4, watch_include=?5, watch_exclude=?6, env_vars=?7, restart_mode=?8, enabled=?9, show_file_tree=?10, tool_commands=?11 WHERE id=?12",
            rusqlite::params![params.name.trim(), params.command, cwd, params.watch_paths, params.watch_include, params.watch_exclude, params.env_vars, params.restart_mode, en, sft, tool_commands, params.id],
        ).map_err(|e| format!("更新模板失败: {}", e))?;
        if affected == 0 { return Err("模板不存在".into()); }
        Ok(())
    })
}
