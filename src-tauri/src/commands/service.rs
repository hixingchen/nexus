use tauri::{Manager, State};
use serde::Deserialize;
use crate::AppState;
use crate::models::{Service, ServiceTemplate};
use crate::database::{query_services_by_project, Database};

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
    // 工具命令是前端直接提交的 JSON 文本：非法 JSON 只会在运行时解析失败，
    // 在保存入口拦截；空串表示"未配置"，由下方统一归一为 []
    if !params.tool_commands.trim().is_empty() {
        serde_json::from_str::<Vec<crate::models::ToolCommand>>(&params.tool_commands)
            .map_err(|e| format!("工具命令配置不是合法 JSON: {}", e))?;
    }
    let cwd = params.cwd.replace('\\', "/");
    // 工作目录/监听路径会进入文件访问白名单（见 editor.rs 的"配置路径收口"）：
    // 必须存在、不是系统/用户敏感目录，且位于项目内或经用户显式选择过
    crate::commands::editor::ensure_config_dir_allowed(&state, &cwd, "工作目录")?;
    crate::commands::editor::ensure_watch_paths_allowed(&state, &params.watch_paths)?;
    let tool_commands = if params.tool_commands.trim().is_empty() { "[]".to_string() } else { params.tool_commands.clone() };
    insert_service_row(&state.db, &params, &cwd, &tool_commands)
}

/// 插入服务行（只有 Database 依赖，便于测试；命令侧先做路径收口校验再调它）
pub(crate) fn insert_service_row(
    db: &Database,
    params: &AddServiceParams,
    cwd: &str,
    tool_commands: &str,
) -> Result<Service, String> {
    db.with_conn(|conn| {
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
                serde_json::to_string(&vec![cwd.to_string()]).unwrap_or_else(|_| "[]".to_string())
            }
        } else { params.watch_paths.clone() };
        let wi = "*";
        let wx = DEFAULT_WATCH_EXCLUDE;
        conn.execute(
            "INSERT INTO services (id, project_id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, sort_index, tool_commands)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,1,0,?11,?12)",
            rusqlite::params![id, params.project_id, params.name.trim(), params.command, cwd, wp, wi, wx, params.env_vars, params.restart_mode, max_sort + 1, tool_commands],
        ).map_err(|e| format!("添加服务失败: {}", e))?;
        Ok(Service {
            id, project_id: params.project_id.clone(), name: params.name.trim().to_string(), command: params.command.clone(),
            cwd: cwd.to_string(), watch_paths: wp, watch_include: wi.into(), watch_exclude: wx.into(),
            env_vars: params.env_vars.clone(), restart_mode: params.restart_mode, enabled: true,
            show_file_tree: false,
            sort_index: max_sort + 1,
            tool_commands: tool_commands.to_string(),
        })
    })
}

/// 更新服务配置。
///
/// 异步 + spawn_blocking：DB 写入之后会 `refresh_service_watch`，它要
/// `stop_watching`（**join 监听线程**，最长约 200ms）并按路径重新 `notify::watch`。
/// 同步执行时这段工作内联在 IPC 请求路径上，保存配置会出现可感的停顿。
#[tauri::command]
pub async fn update_service(
    app: tauri::AppHandle,
    params: UpdateServiceParams,
) -> Result<(), String> {
    if params.id.trim().is_empty() { return Err("服务ID不能为空".into()); }
    if params.name.trim().is_empty() { return Err("名称不能为空".into()); }
    if params.command.trim().is_empty() { return Err("命令不能为空".into()); }
    if !params.tool_commands.trim().is_empty() {
        serde_json::from_str::<Vec<crate::models::ToolCommand>>(&params.tool_commands)
            .map_err(|e| format!("工具命令配置不是合法 JSON: {}", e))?;
    }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let cwd = params.cwd.replace('\\', "/");
        // 同 add_service：配置目录会进入文件访问白名单，保存前必须过收口校验
        crate::commands::editor::ensure_config_dir_allowed(&state, &cwd, "工作目录")?;
        // 监听路径为"跟随工作目录"的兜底值时不校验（它由上面的 cwd 派生，且 cwd 已校验）
        let watch_follows_cwd = params.watch_paths.trim().is_empty() || params.watch_paths.trim() == "[]";
        if !watch_follows_cwd {
            crate::commands::editor::ensure_watch_paths_allowed(&state, &params.watch_paths)?;
        }
        let tool_commands = if params.tool_commands.trim().is_empty() { "[]".to_string() } else { params.tool_commands.clone() };
        let project_id = write_service_update(&state.db, &params, &cwd, &tool_commands)?;
        // 配置保存成功：若该服务正在被监听，用新配置热刷新（路径/排除/模式变更立即生效）；
        // 失败只告警不阻塞保存（保存已成功，监听下次项目级启停时自然重建）
        if let Err(e) = crate::commands::watcher::refresh_service_watch(app.clone(), &state, &project_id, &params.id) {
            log::warn!("更新服务后刷新文件监听失败: {}", e);
        }
        Ok(())
    }).await.map_err(|e| format!("更新服务任务失败: {}", e))?
}

/// 更新服务行（只有 Database 依赖，便于测试）。
///
/// 返回该服务所属项目 id（调用方据此刷新文件监听）。"监听路径跟随工作目录"的兜底逻辑
/// 也在这里：`watch_paths` 为空/`[]`，或仍等于旧 cwd 的拼接值时，自动跟随新 cwd。
pub(crate) fn write_service_update(
    db: &Database,
    params: &UpdateServiceParams,
    cwd: &str,
    tool_commands: &str,
) -> Result<String, String> {
    db.with_conn(|conn| {
        let en = if params.enabled { 1 } else { 0 };
        let sft = if params.show_file_tree { 1 } else { 0 };
        let old: (String, String, String) = conn.query_row(
            "SELECT cwd, watch_paths, project_id FROM services WHERE id=?1",
            [&params.id],
            |r| Ok((r.get("cwd")?, r.get("watch_paths")?, r.get("project_id")?)),
        ).map_err(|e| format!("查询服务失败: {}", e))?;
        let default_old = serde_json::to_string(&vec![old.0.clone()]).unwrap_or_default();
        let wp = if params.watch_paths.trim().is_empty() || params.watch_paths.trim() == "[]" {
            if cwd.is_empty() { "[]".to_string() } else {
                serde_json::to_string(&vec![cwd.to_string()]).unwrap_or_else(|_| "[]".to_string())
            }
        } else if params.watch_paths.trim() == default_old {
            // 监听路径仍等于"旧 cwd 拼接值" → 跟随新工作目录更新
            serde_json::to_string(&vec![cwd.to_string()]).unwrap_or_else(|_| params.watch_paths.clone())
        } else {
            params.watch_paths.clone()
        };
        let affected = conn.execute(
            "UPDATE services SET name=?1, command=?2, cwd=?3, watch_paths=?4, watch_include=?5, watch_exclude=?6, env_vars=?7, restart_mode=?8, enabled=?9, show_file_tree=?10, tool_commands=?11 WHERE id=?12",
            rusqlite::params![params.name.trim(), params.command, cwd, wp, params.watch_include, params.watch_exclude, params.env_vars, params.restart_mode, en, sft, tool_commands, params.id],
        ).map_err(|e| format!("更新服务失败: {}", e))?;
        if affected == 0 { return Err("服务不存在".into()); }
        Ok(old.2)
    })
}

/// 删除服务行（只有 Database 依赖，便于测试；进程停止与监听清理由命令侧负责）
pub(crate) fn delete_service_row(db: &Database, id: &str) -> Result<(), String> {
    db.with_conn(|conn| {
        conn.execute("DELETE FROM services WHERE id=?1", [id]).map_err(|e| format!("删除服务失败: {}", e))?;
        Ok(())
    })
}

/// 按给定顺序重写 sort_index（只有 Database 依赖，便于测试）。
///
/// 语义：只写属于 `project_id` 的 id（外来 id 跳过，防御跨项目写入）；下标按**实际写入的
/// 条数**连续递增（不是 `ordered_ids` 的下标）——这样前端只传来完整列表时结果一致，
/// 传来部分列表（如只拖动其中几项）也不会在项目内留下空洞或撞号。
pub(crate) fn write_service_order(
    db: &Database,
    project_id: &str,
    ordered_ids: &[String],
) -> Result<(), String> {
    db.with_conn_mut(|conn| {
        let tx = conn.transaction().map_err(|e| format!("开启事务失败: {}", e))?;
        // 校验 id 归属：只更新该项目下的服务，防御跨项目写入
        let mut stmt = tx.prepare("SELECT id FROM services WHERE project_id=?1")
            .map_err(|e| format!("查询服务失败: {}", e))?;
        let existing: std::collections::HashSet<String> = stmt.query_map([project_id], |r| r.get::<_, String>(0))
            .map_err(|e| format!("查询服务失败: {}", e))?
            .collect::<Result<_, _>>()
            .map_err(|e| format!("查询服务失败: {}", e))?;
        drop(stmt);
        let mut next_index: i32 = 0;
        for id in ordered_ids.iter() {
            if !existing.contains(id) { continue; }
            tx.execute("UPDATE services SET sort_index=?1 WHERE id=?2", rusqlite::params![next_index, id])
                .map_err(|e| format!("更新服务排序失败: {}", e))?;
            next_index += 1;
        }
        tx.commit().map_err(|e| format!("提交事务失败: {}", e))?;
        Ok(())
    })
}

/// 重排项目服务顺序（ordered_ids 为新的展示顺序，sort_index 按序重写）
#[tauri::command]
pub fn reorder_services(state: State<AppState>, project_id: String, ordered_ids: Vec<String>) -> Result<(), String> {
    if project_id.trim().is_empty() { return Err("项目ID不能为空".into()); }
    write_service_order(&state.db, &project_id, &ordered_ids)
}

/// 删除服务
///
/// 异步 + spawn_blocking：内部要停进程（taskkill + 超时等待，最坏数秒），
/// 同步命令会跑在 IPC/事件循环线程上，导致界面在这期间完全无响应
#[tauri::command]
pub async fn delete_service(app: tauri::AppHandle, id: String) -> Result<(), String> {
    if id.trim().is_empty() { return Err("服务ID不能为空".into()); }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        // 1. 查所属项目（文件监听以项目为维度；服务不存在时容忍跳过监听清理）
        let project_id: Option<String> = state.db.with_conn(|conn| {
            Ok(conn.query_row("SELECT project_id FROM services WHERE id=?1", [&id], |r| r.get(0)).ok())
        })?;
        // 2. 停止运行中的进程（进程 key = service_id，锁外执行避免阻塞 DB 操作）
        let _ = state.process_mgr.stop(&id);
        // 3. 删除数据库记录
        delete_service_row(&state.db, &id)?;
        // 4. 从文件监听中移除该服务（避免残留监听对已删除服务弹"重启"框）
        if let Some(pid) = project_id {
            // app 已被 state 借用，这里传克隆句柄（AppHandle 克隆是廉价的引用计数）
            if let Err(e) = crate::commands::watcher::remove_service_watch(app.clone(), &state, &pid, &id) {
                log::warn!("删除服务后清理文件监听失败: {}", e);
            }
        }
        Ok(())
    }).await.map_err(|e| format!("删除服务任务失败: {}", e))?
}

// ─── 服务模板（跨项目复用） ─────────────────────────────────

/// 查询全部服务模板（按 sort_index 排序，与拖拽重排一致）
#[tauri::command]
pub fn get_service_templates(state: State<AppState>) -> Result<Vec<ServiceTemplate>, String> {
    state.db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, tool_commands, open_tool_id, created_at
             FROM service_templates ORDER BY sort_index, name"
        ).map_err(|e| format!("查询模板失败: {}", e))?;
        let rows = stmt.query_map([], |row| Ok(ServiceTemplate {
            // 按列名取值（不是下标）：SELECT 列表换序不会静默错列，见 database::SERVICE_COLUMNS 的说明
            id: row.get("id")?,
            name: row.get("name")?,
            command: row.get("command")?,
            cwd: row.get("cwd")?,
            watch_paths: row.get("watch_paths")?,
            watch_include: row.get("watch_include")?,
            watch_exclude: row.get("watch_exclude")?,
            env_vars: row.get("env_vars")?,
            restart_mode: row.get("restart_mode")?,
            enabled: row.get("enabled")?,
            show_file_tree: row.get("show_file_tree")?,
            tool_commands: row.get("tool_commands")?,
            open_tool_id: row.get("open_tool_id")?,
            created_at: row.get("created_at")?,
        })).map_err(|e| format!("查询模板失败: {}", e))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("查询模板失败: {}", e))
    })
}

/// 把项目里的服务保存为模板（值拷贝，不关联原项目，可重复保存产生多个副本）
#[tauri::command]
pub fn save_service_as_template(state: State<AppState>, service_id: String) -> Result<ServiceTemplate, String> {
    if service_id.trim().is_empty() { return Err("服务ID不能为空".into()); }
    state.db.with_conn(|conn| {
        let t = conn.query_row(
            "SELECT name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, tool_commands
             FROM services WHERE id=?1",
            [&service_id],
            |row| Ok((
                row.get::<_, String>("name")?,
                row.get::<_, String>("command")?,
                row.get::<_, String>("cwd")?,
                row.get::<_, String>("watch_paths")?,
                row.get::<_, String>("watch_include")?,
                row.get::<_, String>("watch_exclude")?,
                row.get::<_, String>("env_vars")?,
                row.get::<_, i32>("restart_mode")?,
                row.get::<_, bool>("enabled")?,
                row.get::<_, bool>("show_file_tree")?,
                row.get::<_, String>("tool_commands")?,
            )),
        ).map_err(|e| format!("服务不存在: {}", e))?;
        let (name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, tool_commands) = t;
        // 模板随服务带走当前绑定的打开工具（从模板添加服务时复制为新服务绑定）
        let open_tool_id: String = conn.query_row(
            "SELECT tool_id FROM service_open_tools WHERE service_id=?1", [&service_id],
            |r| r.get("tool_id"),
        ).unwrap_or_default();

        let id = uuid::Uuid::new_v4().to_string();
        let en = if enabled { 1 } else { 0 };
        let sft = if show_file_tree { 1 } else { 0 };
        conn.execute(
            "INSERT INTO service_templates (id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, sort_index, tool_commands, open_tool_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,0,?12,?13)",
            rusqlite::params![id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, en, sft, tool_commands, open_tool_id],
        ).map_err(|e| format!("保存模板失败: {}", e))?;
        // 回读真实创建时间（原实现返回空串，与查询接口的返回不一致）
        let created_at: String = conn
            .query_row("SELECT created_at FROM service_templates WHERE id=?1", [&id], |r| r.get(0))
            .unwrap_or_default();
        Ok(ServiceTemplate {
            id, name, command, cwd, watch_paths, watch_include, watch_exclude,
            env_vars, restart_mode, enabled, show_file_tree, tool_commands,
            open_tool_id,
            created_at,
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
    state.db.with_conn_mut(|conn| {
        // 事务包裹：服务行 + 打开工具绑定必须原子写入——绑定插入失败（外键等）
        // 若留下已建服务，用户看到的是"服务加进来了但工具绑定丢了"的半成品
        let tx = conn.transaction().map_err(|e| format!("开启事务失败: {}", e))?;
        let project_exists: bool = tx.query_row(
            "SELECT COUNT(*) > 0 FROM projects WHERE id=?1", [&project_id], |r| r.get(0)
        ).map_err(|e| format!("校验所属项目失败: {}", e))?;
        if !project_exists { return Err("所属项目不存在".into()); }

        let t = tx.query_row(
            "SELECT name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, tool_commands, open_tool_id
             FROM service_templates WHERE id=?1",
            [&template_id],
            |row| Ok((
                row.get::<_, String>("name")?,
                row.get::<_, String>("command")?,
                row.get::<_, String>("cwd")?,
                row.get::<_, String>("watch_paths")?,
                row.get::<_, String>("watch_include")?,
                row.get::<_, String>("watch_exclude")?,
                row.get::<_, String>("env_vars")?,
                row.get::<_, i32>("restart_mode")?,
                row.get::<_, bool>("enabled")?,
                row.get::<_, bool>("show_file_tree")?,
                row.get::<_, String>("tool_commands")?,
                row.get::<_, String>("open_tool_id")?,
            )),
        ).map_err(|e| format!("模板不存在: {}", e))?;
        let (name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, tool_commands, open_tool_id) = t;
        // 模板里的目录同样是"即将进入白名单的根"：套用同一套收口（历史模板值视为已授权，
        // 但驱动器根/系统目录一律拒绝——模板可能是从别处导入或被写坏的）
        crate::commands::editor::ensure_config_dir_allowed(&state, &cwd, "工作目录")?;
        crate::commands::editor::ensure_watch_paths_allowed(&state, &watch_paths)?;

        let id = uuid::Uuid::new_v4().to_string();
        let max_sort: i32 = tx.query_row(
            "SELECT COALESCE(MAX(sort_index), -1) FROM services WHERE project_id=?1",
            [&project_id], |r| r.get(0)
        ).map_err(|e| format!("读取服务排序失败: {}", e))?;
        let en = if enabled { 1 } else { 0 };
        let sft = if show_file_tree { 1 } else { 0 };
        tx.execute(
            "INSERT INTO services (id, project_id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, sort_index, tool_commands)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            rusqlite::params![id, project_id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, en, sft, max_sort + 1, tool_commands],
        ).map_err(|e| format!("添加服务失败: {}", e))?;
        // 模板携带的默认打开工具 → 复制为新服务的绑定（工具已被删除时跳过，避免外键失败）
        if !open_tool_id.is_empty() {
            let tool_exists: bool = tx.query_row(
                "SELECT COUNT(*) > 0 FROM open_tools WHERE id=?1", [&open_tool_id], |r| r.get(0)
            ).map_err(|e| format!("校验打开工具失败: {}", e))?;
            if tool_exists {
                tx.execute(
                    "INSERT OR REPLACE INTO service_open_tools (service_id, tool_id) VALUES (?1,?2)",
                    rusqlite::params![id, open_tool_id],
                ).map_err(|e| format!("复制模板打开工具失败: {}", e))?;
            }
        }
        tx.commit().map_err(|e| format!("提交事务失败: {}", e))?;
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
    pub open_tool_id: String,
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
    if !params.tool_commands.trim().is_empty() {
        serde_json::from_str::<Vec<crate::models::ToolCommand>>(&params.tool_commands)
            .map_err(|e| format!("工具命令配置不是合法 JSON: {}", e))?;
    }
    // 模板目录同样进入白名单根集合：套用配置目录收口（见 editor.rs 的"配置路径收口"）
    crate::commands::editor::ensure_config_dir_allowed(&state, &cwd, "工作目录")?;
    crate::commands::editor::ensure_watch_paths_allowed(&state, &params.watch_paths)?;
    let tool_commands = if params.tool_commands.trim().is_empty() { "[]".to_string() } else { params.tool_commands };
    state.db.with_conn(|conn| {
        let en = if params.enabled { 1 } else { 0 };
        let sft = if params.show_file_tree { 1 } else { 0 };
        let affected = conn.execute(
            "UPDATE service_templates SET name=?1, command=?2, cwd=?3, watch_paths=?4, watch_include=?5, watch_exclude=?6, env_vars=?7, restart_mode=?8, enabled=?9, show_file_tree=?10, tool_commands=?11, open_tool_id=?13 WHERE id=?12",
            rusqlite::params![params.name.trim(), params.command, cwd, params.watch_paths, params.watch_include, params.watch_exclude, params.env_vars, params.restart_mode, en, sft, tool_commands, params.id, params.open_tool_id],
        ).map_err(|e| format!("更新模板失败: {}", e))?;
        if affected == 0 { return Err("模板不存在".into()); }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::{init_schema, Database};
    use std::sync::Mutex;

    /// 内存库 + 一个项目行（ARCH-4：这几条 SQL 路径此前零测试）
    fn test_db() -> Database {
        let conn = rusqlite::Connection::open_in_memory().expect("内存库");
        init_schema(&conn).expect("建表");
        conn.execute(
            "INSERT INTO projects (id, name, path, sort_index, created_at) VALUES ('p1','P','C:/p',0,'t')",
            [],
        ).expect("插入项目");
        Database { conn: Mutex::new(conn) }
    }

    fn add_params(name: &str, cwd: &str) -> AddServiceParams {
        AddServiceParams {
            project_id: "p1".into(),
            name: name.into(),
            command: "npm run dev".into(),
            cwd: cwd.into(),
            watch_paths: String::new(),
            env_vars: String::new(),
            restart_mode: 1,
            tool_commands: String::new(),
        }
    }

    #[test]
    fn test_insert_service_assigns_increasing_sort_index_and_defaults() {
        let db = test_db();
        let a = insert_service_row(&db, &add_params("a", "C:/p"), "C:/p", "[]").unwrap();
        let b = insert_service_row(&db, &add_params("b", "C:/p"), "C:/p", "[]").unwrap();
        assert_eq!(a.sort_index, 0);
        assert_eq!(b.sort_index, 1, "新增服务应排在末尾（MAX(sort_index)+1）");
        // 监听路径缺省跟随工作目录（JSON 数组形式，供前端直接 parse）
        assert_eq!(a.watch_paths, r#"["C:/p"]"#);
        assert!(a.enabled, "新建服务默认启用");
        assert!(!a.show_file_tree);
        // 项目不存在时拒绝
        let mut p = add_params("c", "C:/p");
        p.project_id = "nope".into();
        assert!(insert_service_row(&db, &p, "C:/p", "[]").is_err());
    }

    #[test]
    fn test_update_service_follows_cwd_and_rejects_missing() {
        let db = test_db();
        let svc = insert_service_row(&db, &add_params("a", "C:/p"), "C:/p", "[]").unwrap();

        let params = UpdateServiceParams {
            id: svc.id.clone(),
            name: "a2".into(),
            command: "npm start".into(),
            cwd: "C:/p/sub".into(),
            // 仍等于"旧 cwd 拼接值" → 应跟随新 cwd
            watch_paths: r#"["C:/p"]"#.into(),
            watch_include: "*".into(),
            watch_exclude: "".into(),
            env_vars: "K=V".into(),
            restart_mode: 2,
            enabled: false,
            show_file_tree: true,
            tool_commands: "[]".into(),
        };
        let project_id = write_service_update(&db, &params, "C:/p/sub", "[]").unwrap();
        assert_eq!(project_id, "p1", "应回传所属项目，供调用方刷新监听");
        let got = db.with_conn(|c| {
            Ok(c.query_row(
                "SELECT name, cwd, watch_paths, enabled, show_file_tree FROM services WHERE id=?1",
                [&svc.id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i32>(3)?, r.get::<_, i32>(4)?)),
            ).unwrap())
        }).unwrap();
        assert_eq!(got.0, "a2");
        assert_eq!(got.1, "C:/p/sub");
        assert_eq!(got.2, r#"["C:/p/sub"]"#, "监听路径处于跟随状态时应随 cwd 更新");
        assert_eq!(got.3, 0, "enabled=false 应写 0");
        assert_eq!(got.4, 1, "show_file_tree=true 应写 1");

        // 自定义监听路径不被 cwd 变化覆盖
        let params2 = UpdateServiceParams { watch_paths: r#"["C:/other"]"#.into(), cwd: "C:/p/x".into(), ..params };
        write_service_update(&db, &params2, "C:/p/x", "[]").unwrap();
        let wp: String = db.with_conn(|c| Ok(c.query_row("SELECT watch_paths FROM services WHERE id=?1", [&svc.id], |r| r.get(0)).unwrap())).unwrap();
        assert_eq!(wp, r#"["C:/other"]"#, "用户显式配置的监听路径不应被 cwd 覆盖");

        // 不存在的 id：报错而不是静默成功
        let missing = UpdateServiceParams { id: "nope".into(), ..params2 };
        assert!(write_service_update(&db, &missing, "C:/p", "[]").is_err());
    }

    #[test]
    fn test_write_service_order_only_touches_own_project() {
        let db = test_db();
        let a = insert_service_row(&db, &add_params("a", "C:/p"), "C:/p", "[]").unwrap();
        let b = insert_service_row(&db, &add_params("b", "C:/p"), "C:/p", "[]").unwrap();
        let c = insert_service_row(&db, &add_params("c", "C:/p"), "C:/p", "[]").unwrap();

        write_service_order(&db, "p1", &[c.id.clone(), a.id.clone(), b.id.clone()]).unwrap();
        let order = db.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT id FROM services WHERE project_id='p1' ORDER BY sort_index").unwrap();
            let ids: Vec<String> = stmt.query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
            Ok(ids)
        }).unwrap();
        assert_eq!(order, vec![c.id.clone(), a.id.clone(), b.id.clone()]);

        // 含别的项目的 id：整条被忽略（不写、不报错），下标按"实际写入条数"连续编号，
        // 因此 b 会落到 0（而不是被外来 id 顶到 1）
        write_service_order(&db, "p1", &["other-project-svc".into(), b.id.clone()]).unwrap();
        let order2 = db.with_conn(|conn| {
            Ok(conn.query_row("SELECT id FROM services WHERE sort_index=0 AND project_id='p1'", [], |r| r.get::<_, String>(0)).unwrap())
        }).unwrap();
        assert_eq!(order2, b.id, "外来 id 不参与排序，本项目服务按下标连续重排");
        let untouched: String = db.with_conn(|conn| {
            Ok(conn.query_row("SELECT watch_paths FROM services WHERE id=?1", [&c.id], |r| r.get(0)).unwrap())
        }).unwrap();
        assert_eq!(untouched, r#"["C:/p"]"#, "未出现在列表中的服务不被改动");
    }

    #[test]
    fn test_delete_service_row_removes_only_target() {
        let db = test_db();
        let a = insert_service_row(&db, &add_params("a", "C:/p"), "C:/p", "[]").unwrap();
        let b = insert_service_row(&db, &add_params("b", "C:/p"), "C:/p", "[]").unwrap();
        delete_service_row(&db, &a.id).unwrap();
        let left = db.with_conn(|conn| {
            Ok(conn.query_row("SELECT COUNT(*) FROM services", [], |r| r.get::<_, i32>(0)).unwrap())
        }).unwrap();
        assert_eq!(left, 1);
        // 删除不存在的行：不报错（幂等，重复删除不应失败）
        delete_service_row(&db, &a.id).unwrap();
        delete_service_row(&db, &b.id).unwrap();
        let left2 = db.with_conn(|conn| {
            Ok(conn.query_row("SELECT COUNT(*) FROM services", [], |r| r.get::<_, i32>(0)).unwrap())
        }).unwrap();
        assert_eq!(left2, 0);
    }
}
