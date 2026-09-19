use tauri::{Manager, State};
use serde::{Deserialize, Serialize};
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
    /// 以下四项不传时用默认值（`None`）。
    ///
    /// 为什么要放开：此前"添加服务"只能填名称/命令/工作目录，其余四项在
    /// `insert_service` 里写死，用户想配监听规则或目录树开关得先把服务建出来、
    /// 再点开卡片开另一个面板。改成服务面板新建后，一次填写就能配全，所以这里
    /// 得能接收它们；不传的老调用点行为完全不变。
    #[serde(default)]
    pub watch_include: Option<String>,
    #[serde(default)]
    pub watch_exclude: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub show_file_tree: Option<bool>,
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
    // 工作目录/监听路径会进入文件访问白名单（见 commands/paths.rs 的"配置路径收口"）：
    // 必须存在、不是系统/用户敏感目录，且位于项目内或经用户显式选择过
    state.paths.ensure_config_dir_allowed(&state.db, &cwd, "工作目录")?;
    state.paths.ensure_watch_paths_allowed(&state.db, &params.watch_paths)?;
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
        // 默认值保持与放开参数之前一致（不传 = 旧行为）
        let wi = params.watch_include.as_deref().unwrap_or("*");
        let wx = params.watch_exclude.clone().unwrap_or_else(|| DEFAULT_WATCH_EXCLUDE.to_string());
        let en = params.enabled.unwrap_or(true);
        let sft = params.show_file_tree.unwrap_or(false);
        conn.execute(
            &format!(
                "INSERT INTO services ({}) VALUES ({})",
                crate::database::SERVICE_COLUMNS,
                crate::database::named_placeholders(crate::database::SERVICE_COLUMNS),
            ),
            rusqlite::named_params! {
                ":id": id, ":project_id": params.project_id, ":name": params.name.trim(),
                ":command": params.command, ":cwd": cwd, ":watch_paths": wp,
                ":watch_include": wi, ":watch_exclude": wx, ":env_vars": params.env_vars,
                ":restart_mode": params.restart_mode,
                // 两个展示开关：默认"启用 + 不显示文件树"，面板新建时按用户勾的走
                ":enabled": en, ":show_file_tree": sft,
                ":sort_index": max_sort + 1, ":tool_commands": tool_commands,
            },
        ).map_err(|e| format!("添加服务失败: {}", e))?;
        Ok(Service {
            id, project_id: params.project_id.clone(), name: params.name.trim().to_string(), command: params.command.clone(),
            cwd: cwd.to_string(), watch_paths: wp, watch_include: wi.into(), watch_exclude: wx,
            env_vars: params.env_vars.clone(), restart_mode: params.restart_mode, enabled: en,
            show_file_tree: sft,
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
        state.paths.ensure_config_dir_allowed(&state.db, &cwd, "工作目录")?;
        // 监听路径为"跟随工作目录"的兜底值时不校验（它由上面的 cwd 派生，且 cwd 已校验）
        let watch_follows_cwd = params.watch_paths.trim().is_empty() || params.watch_paths.trim() == "[]";
        if !watch_follows_cwd {
            state.paths.ensure_watch_paths_allowed(&state.db, &params.watch_paths)?;
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
            &format!(
                "UPDATE services SET {} WHERE id=:id",
                crate::database::named_assignments(crate::database::SERVICE_UPDATABLE_COLUMNS),
            ),
            rusqlite::named_params! {
                ":name": params.name.trim(), ":command": params.command, ":cwd": cwd,
                ":watch_paths": wp, ":watch_include": params.watch_include,
                ":watch_exclude": params.watch_exclude, ":env_vars": params.env_vars,
                ":restart_mode": params.restart_mode, ":enabled": en,
                ":show_file_tree": sft, ":tool_commands": tool_commands, ":id": params.id,
            },
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
        // 2. 停止运行中的进程（进程 key = service_id，锁外执行避免阻塞 DB 操作）。
        // 失败留痕（同 delete_project）：静默丢弃会留下仍在跑的孤儿进程，而服务记录已删、
        // UI 里再没有入口去停它。仍继续删除，残留进程由 Job Object 在应用退出时兜底。
        if let Err(e) = state.process_mgr.stop(&id) {
            log::warn!("删除服务 {} 时停止进程失败（进程可能残留至应用退出）：{}", id, e);
        }
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
    // 取模板行 + 白名单收口，**必须在取 DB 锁之前**。
    //
    // 为什么不能放进下面的 `with_conn_mut` 闭包：`ensure_config_dir_allowed` 内部会调
    // `registered_dirs` → `state.db.with_conn`，而 `Database::conn` 是不可重入的
    // `std::sync::Mutex`——同一线程在闭包内再取同一把锁是**永久死锁**（不是报错），
    // 且守卫永不释放 → 之后所有查库命令一起卡死、界面冻结，只能杀进程。
    // 校验用的就是下面这组值，故收口与取值同源（不存在校验后被换掉的窗口）。
    //
    // 模板里的目录同样是"即将进入白名单的根"：套用同一套收口（历史模板值视为已授权，
    // 但驱动器根/系统目录一律拒绝——模板可能是从别处导入或被写坏的）
    let t = state.db.with_conn(|conn| {
        conn.query_row(
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
        ).map_err(|e| format!("模板不存在: {}", e))
    })?;
    let (name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, tool_commands, open_tool_id) = t;
    state.paths.ensure_config_dir_allowed(&state.db, &cwd, "工作目录")?;
    state.paths.ensure_watch_paths_allowed(&state.db, &watch_paths)?;

    state.db.with_conn_mut(|conn| {
        // 事务包裹：服务行 + 打开工具绑定必须原子写入——绑定插入失败（外键等）
        // 若留下已建服务，用户看到的是"服务加进来了但工具绑定丢了"的半成品
        let tx = conn.transaction().map_err(|e| format!("开启事务失败: {}", e))?;
        let project_exists: bool = tx.query_row(
            "SELECT COUNT(*) > 0 FROM projects WHERE id=?1", [&project_id], |r| r.get(0)
        ).map_err(|e| format!("校验所属项目失败: {}", e))?;
        if !project_exists { return Err("所属项目不存在".into()); }

        let id = uuid::Uuid::new_v4().to_string();
        let max_sort: i32 = tx.query_row(
            "SELECT COALESCE(MAX(sort_index), -1) FROM services WHERE project_id=?1",
            [&project_id], |r| r.get(0)
        ).map_err(|e| format!("读取服务排序失败: {}", e))?;
        let en = if enabled { 1 } else { 0 };
        let sft = if show_file_tree { 1 } else { 0 };
        tx.execute(
            &format!(
                "INSERT INTO services ({}) VALUES ({})",
                crate::database::SERVICE_COLUMNS,
                crate::database::named_placeholders(crate::database::SERVICE_COLUMNS),
            ),
            rusqlite::named_params! {
                ":id": id, ":project_id": project_id, ":name": name, ":command": command,
                ":cwd": cwd, ":watch_paths": watch_paths, ":watch_include": watch_include,
                ":watch_exclude": watch_exclude, ":env_vars": env_vars,
                ":restart_mode": restart_mode, ":enabled": en, ":show_file_tree": sft,
                ":sort_index": max_sort + 1, ":tool_commands": tool_commands,
            },
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
    // 模板目录同样进入白名单根集合：套用配置目录收口（见 commands/paths.rs 的"配置路径收口"）
    state.paths.ensure_config_dir_allowed(&state.db, &cwd, "工作目录")?;
    state.paths.ensure_watch_paths_allowed(&state.db, &params.watch_paths)?;
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

/// 新建服务模板参数（与 UpdateServiceTemplateParams 同形，只是没有 id）
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateServiceTemplateParams {
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

/// 在模板库里直接新建模板（不必先有服务再"另存为模板"）
#[tauri::command]
pub fn create_service_template(
    state: State<AppState>,
    params: CreateServiceTemplateParams,
) -> Result<ServiceTemplate, String> {
    let name = params.name.trim().to_string();
    if name.is_empty() { return Err("名称不能为空".into()); }
    if params.command.trim().is_empty() { return Err("命令不能为空".into()); }
    if !params.tool_commands.trim().is_empty() {
        serde_json::from_str::<Vec<crate::models::ToolCommand>>(&params.tool_commands)
            .map_err(|e| format!("工具命令配置不是合法 JSON: {}", e))?;
    }
    let cwd = params.cwd.replace('\\', "/");
    // 与 update_service_template 同一套收口：模板目录同样是"即将进入白名单的根"
    state.paths.ensure_config_dir_allowed(&state.db, &cwd, "工作目录")?;
    state.paths.ensure_watch_paths_allowed(&state.db, &params.watch_paths)?;
    let tool_commands = if params.tool_commands.trim().is_empty() { "[]".to_string() } else { params.tool_commands };
    // 监听两项留空 = 用默认，与服务新建同口径。
    //
    // 为什么归一化放后端而不是让前端填：默认排除规则（node_modules / .git / …）的**唯一
    // 来源在后端**，前端复制一份迟早与它漂移（前端的 placeholder 只有 4 行、后端 8 行，
    // 已经不一致了）。编辑模板走 update_service_template，那里直传——所以"就是想清空
    // 排除规则"在编辑路径上照样做得到，不受这里影响。
    let watch_include = if params.watch_include.trim().is_empty() { "*".to_string() } else { params.watch_include };
    let watch_exclude = if params.watch_exclude.trim().is_empty() {
        DEFAULT_WATCH_EXCLUDE.to_string()
    } else {
        params.watch_exclude
    };

    state.db.with_conn(|conn| {
        let id = uuid::Uuid::new_v4().to_string();
        let en = if params.enabled { 1 } else { 0 };
        let sft = if params.show_file_tree { 1 } else { 0 };
        // 接在排序末尾（视图是 ORDER BY sort_index, name）：新模板出现在列表最后。
        // 另存为模板走的是固定 0，两者位置能区分开——用户刚建的模板在末尾好找
        let next_sort: i32 = conn
            .query_row("SELECT COALESCE(MAX(sort_index), -1) + 1 FROM service_templates", [], |r| r.get(0))
            .unwrap_or(0);
        conn.execute(
            "INSERT INTO service_templates (id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, sort_index, tool_commands, open_tool_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            rusqlite::params![id, name, params.command, cwd, params.watch_paths, watch_include,
                watch_exclude, params.env_vars, params.restart_mode, en, sft, next_sort,
                tool_commands, params.open_tool_id],
        ).map_err(|e| format!("新建模板失败: {}", e))?;
        let created_at: String = conn
            .query_row("SELECT created_at FROM service_templates WHERE id=?1", [&id], |r| r.get(0))
            .unwrap_or_default();
        Ok(ServiceTemplate {
            id,
            name,
            command: params.command,
            cwd,
            watch_paths: params.watch_paths,
            watch_include,
            watch_exclude,
            env_vars: params.env_vars,
            restart_mode: params.restart_mode,
            enabled: params.enabled,
            show_file_tree: params.show_file_tree,
            tool_commands,
            open_tool_id: params.open_tool_id,
            created_at,
        })
    })
}

// ─── 模板导入导出 ───────────────────────────────────────────

/// 导出文件里的单个模板。与 ServiceTemplate 两点不同：
/// - **没有 id**：导入方自己生成——跨机复制会撞 id，同机重复导入也会与源模板冲突
/// - **openToolName 而不是 openToolId**：工具 id 是本机工具库的主键，换台机器指向别的
///   工具或压根不存在（静默指错）。名字是两台机器之间唯一有意义的标识
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportedTemplate {
    pub name: String,
    pub command: String,
    pub cwd: String,
    #[serde(default)]
    pub watch_paths: String,
    #[serde(default)]
    pub watch_include: String,
    #[serde(default)]
    pub watch_exclude: String,
    #[serde(default)]
    pub env_vars: String,
    #[serde(default)]
    pub restart_mode: i32,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub show_file_tree: bool,
    #[serde(default)]
    pub tool_commands: String,
    #[serde(default)]
    pub open_tool_name: String,
}

/// 导出文件的顶层结构（带 version：将来加字段时能认出旧文件）
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateExportFile {
    pub version: u32,
    pub templates: Vec<ExportedTemplate>,
}

/// 当前导出格式版本
const TEMPLATE_EXPORT_VERSION: u32 = 1;

/// 导入结果——要让界面能如实说明"到底发生了什么"，而不是一句"导入成功"。
///
/// 响应 DTO 一律 snake_case（见 contract.rs 顶部的规则），前端类型原样对应；
/// 上面的导出文件结构是**文件格式**不是 IPC 载荷，所以用 camelCase 便于阅读与手工编辑
#[derive(Serialize)]
pub struct ImportTemplatesResult {
    pub imported: usize,
    /// 与库里已有模板同名的数量（**只统计、不改名**）。
    ///
    /// 为什么不像最初那样自动加「(2)」后缀：库里本来就允许同名（`service_templates.name`
    /// 上没有唯一约束），而同一个功能里的另一个入口——「另存为模板」——从不加后缀。
    /// 加后缀是导入这边**自己加的**约束，还让用户导入完得挨个改回来。
    pub duplicated: usize,
    /// 本机工具库里没有、因而没绑上的工具名（已去重）
    pub missing_tools: Vec<String>,
    /// 工作目录在本机不存在的模板名。导入**不会**因此失败（模板只是配置，
    /// 真正用到目录是在"从模板添加服务"那一步，那里有收口），但用户需要知道
    /// "导入完为什么不能直接用"
    pub missing_dirs: Vec<String>,
}

/// 导出模板到 JSON 文件。`ids` 为空 = 导出全部；否则只导出指定模板
#[tauri::command]
pub fn export_service_templates(
    state: State<AppState>,
    path: String,
    ids: Vec<String>,
) -> Result<usize, String> {
    let target = path.trim();
    if target.is_empty() { return Err("导出路径不能为空".into()); }

    let picked = state.db.with_conn(|conn| collect_export_templates(conn, &ids))?;
    if picked.is_empty() { return Err("没有可导出的模板".into()); }
    let count = picked.len();

    let payload = TemplateExportFile { version: TEMPLATE_EXPORT_VERSION, templates: picked };
    let json = serde_json::to_string_pretty(&payload).map_err(|e| format!("生成导出内容失败: {}", e))?;
    std::fs::write(target, json).map_err(|e| format!("写入文件失败: {}", e))?;
    Ok(count)
}

/// 收集要导出的模板（ids 为空 = 全部）。抽出来是为了可单测——
/// Tauri 命令签名带 `State`，测试构造不出来
fn collect_export_templates(
    conn: &rusqlite::Connection,
    ids: &[String],
) -> Result<Vec<ExportedTemplate>, String> {
    // LEFT JOIN 取工具名：没绑工具时 tool_name 为空
    let mut stmt = conn.prepare(
        "SELECT t.name, t.command, t.cwd, t.watch_paths, t.watch_include, t.watch_exclude,
                t.env_vars, t.restart_mode, t.enabled, t.show_file_tree, t.tool_commands,
                t.id, COALESCE(o.name, '') AS tool_name
         FROM service_templates t
         LEFT JOIN open_tools o ON o.id = t.open_tool_id
         ORDER BY t.sort_index, t.name"
    ).map_err(|e| format!("查询模板失败: {}", e))?;
    let rows = stmt.query_map([], |row| Ok((
        row.get::<_, String>("id")?,
        ExportedTemplate {
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
            open_tool_name: row.get("tool_name")?,
        },
    ))).map_err(|e| format!("查询模板失败: {}", e))?;
    let all = rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("查询模板失败: {}", e))?;

    Ok(if ids.is_empty() {
        all.into_iter().map(|(_, t)| t).collect()
    } else {
        all.into_iter().filter(|(id, _)| ids.contains(id)).map(|(_, t)| t).collect()
    })
}

/// 从 JSON 文件导入模板。
///
/// **不做目录收口**（与 create/update 不同）：导入的是别人机器上的配置，其 cwd 在本机
/// 多半不存在，收口会让整个导入失败。收口留给真正用到目录的那一步——
/// `add_service_from_template` 已经在对模板值做白名单校验（那里的注释也写明"模板可能是
/// 从别处导入或被写坏的"），所以未收口的模板不会被真的用起来。
#[tauri::command]
pub fn import_service_templates(
    state: State<AppState>,
    path: String,
) -> Result<ImportTemplatesResult, String> {
    let source = path.trim();
    if source.is_empty() { return Err("导入路径不能为空".into()); }
    let raw = std::fs::read_to_string(source).map_err(|e| format!("读取文件失败: {}", e))?;
    let file: TemplateExportFile = serde_json::from_str(&raw)
        .map_err(|e| format!("不是有效的模板导出文件: {}", e))?;
    if file.version != TEMPLATE_EXPORT_VERSION {
        return Err(format!("导出文件版本 {} 不受支持（当前支持 {}）", file.version, TEMPLATE_EXPORT_VERSION));
    }

    state.db.with_conn_mut(|conn| import_templates_into(conn, &file))
}

/// 导入的核心逻辑（与命令分开：命令只管读文件与解析，这里拿得到连接、可单测）
fn import_templates_into(
    conn: &mut rusqlite::Connection,
    file: &TemplateExportFile,
) -> Result<ImportTemplatesResult, String> {
    let tx = conn.transaction().map_err(|e| format!("开启事务失败: {}", e))?;
    let mut imported = 0usize;
    let mut duplicated = 0usize;
    let mut missing_tools: Vec<String> = Vec::new();
    let mut missing_dirs: Vec<String> = Vec::new();

    // 整批一个事务：导入要么全成、要么全不成。半途失败留下"导了一半"的模板库，
    // 用户看到的是一堆残缺数据，还不如重来
    for t in &file.templates {
        let name = t.name.trim().to_string();
        if name.is_empty() { continue; } // 没名字的行没法用，跳过（计数里也不含）
        // 同名照收，名字保持原样（见 ImportTemplatesResult.duplicated 的说明）。
        // 只统计一下，好让界面能如实告诉用户"有 N 个与现有的同名"
        let taken: i64 = tx
            .query_row("SELECT COUNT(*) FROM service_templates WHERE name=?1", [&name], |r| r.get(0))
            .unwrap_or(0);
        if taken > 0 { duplicated += 1; }

        // 工具按**名字**在本机工具库里找；找不到就不绑（记下来告诉用户）
        let tool_id = if t.open_tool_name.trim().is_empty() {
            String::new()
        } else {
            let found = tx
                .query_row("SELECT id FROM open_tools WHERE name=?1 LIMIT 1", [t.open_tool_name.trim()], |r| r.get::<_, String>(0))
                .ok();
            match found {
                Some(id) => id,
                None => {
                    let n = t.open_tool_name.trim().to_string();
                    if !missing_tools.contains(&n) { missing_tools.push(n); }
                    String::new()
                }
            }
        };

        let cwd = t.cwd.replace('\\', "/");
        if !cwd.trim().is_empty() && !std::path::Path::new(cwd.trim()).is_dir() {
            missing_dirs.push(name.clone());
        }

        let id = uuid::Uuid::new_v4().to_string();
        let en = if t.enabled { 1 } else { 0 };
        let sft = if t.show_file_tree { 1 } else { 0 };
        let tool_commands = if t.tool_commands.trim().is_empty() { "[]".to_string() } else { t.tool_commands.clone() };
        let next_sort: i32 = tx
            .query_row("SELECT COALESCE(MAX(sort_index), -1) + 1 FROM service_templates", [], |r| r.get(0))
            .unwrap_or(0);
        tx.execute(
            "INSERT INTO service_templates (id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, sort_index, tool_commands, open_tool_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            rusqlite::params![id, name, t.command, cwd, t.watch_paths, t.watch_include, t.watch_exclude,
                t.env_vars, t.restart_mode, en, sft, next_sort, tool_commands, tool_id],
        ).map_err(|e| format!("导入模板失败: {}", e))?;
        imported += 1;
    }

    tx.commit().map_err(|e| format!("提交事务失败: {}", e))?;
    Ok(ImportTemplatesResult { imported, duplicated, missing_tools, missing_dirs })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::{init_schema, Database};

    /// 内存库 + 一个项目行（ARCH-4：这几条 SQL 路径此前零测试）
    fn test_db() -> Database {
        let conn = rusqlite::Connection::open_in_memory().expect("内存库");
        init_schema(&conn).expect("建表");
        conn.execute(
            "INSERT INTO projects (id, name, path, sort_index, created_at) VALUES ('p1','P','C:/p',0,'t')",
            [],
        ).expect("插入项目");
        Database::from_connection(conn)
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
            watch_include: None,
            watch_exclude: None,
            enabled: None,
            show_file_tree: None,
        }
    }

    /// 传了监听规则与两个展示开关时按传的走（不传才用默认）。
    ///
    /// 这三项此前在 insert_service_row 里写死——"添加服务"弹窗只能填三个字段，
    /// 其余由后端补，用户想配全得建完再开一次面板。改成服务面板新建后必须能带全，
    /// 这条钉住"传了就生效"，同时上面的 add_params 仍覆盖"不传 = 旧行为"。
    #[test]
    fn test_add_service_accepts_explicit_watch_rules_and_flags() {
        let db = test_db();
        let mut p = add_params("S", "C:/s");
        p.watch_include = Some("*.ts".into());
        p.watch_exclude = Some("dist".into());
        p.enabled = Some(false);
        p.show_file_tree = Some(true);

        let svc = insert_service_row(&db, &p, "C:/s", "[]").expect("插入服务");
        assert_eq!(svc.watch_include, "*.ts");
        assert_eq!(svc.watch_exclude, "dist");
        assert!(!svc.enabled, "传了 false 就该是不启用");
        assert!(svc.show_file_tree, "传了 true 就该显示文件树");
    }

    /// 测试用的空模板：只填必要的名字/命令，其余留空
    fn exported(name: &str, tool: &str) -> ExportedTemplate {
        ExportedTemplate {
            name: name.into(),
            command: "npm run dev".into(),
            cwd: String::new(),
            watch_paths: "[]".into(),
            watch_include: String::new(),
            watch_exclude: String::new(),
            env_vars: String::new(),
            restart_mode: 0,
            enabled: true,
            show_file_tree: false,
            tool_commands: "[]".into(),
            open_tool_name: tool.into(),
        }
    }

    /// 导出 → 序列化 → 读回 → 导入：配置原样搬过去，**id 不带**（跨机会撞、
    /// 同机重复导入也会与源模板冲突，所以导出格式里根本没有 id 字段）
    #[test]
    fn test_template_export_import_roundtrip() {
        let db = test_db();
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO service_templates (id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, sort_index, tool_commands, open_tool_id)
                 VALUES ('t1','前端 dev','npm run dev','','[]','','','K=V',1,1,0,0,'[]','')",
                [],
            ).map_err(|e| e.to_string())?;
            Ok(())
        }).expect("插入模板");

        let exported_list = db.with_conn(|c| collect_export_templates(c, &[])).expect("导出");
        assert_eq!(exported_list.len(), 1);
        assert_eq!(exported_list[0].name, "前端 dev");
        assert_eq!(exported_list[0].env_vars, "K=V", "配置要原样带出去");

        // 落盘再读回（导出→导入的真实路径）
        let file = TemplateExportFile { version: TEMPLATE_EXPORT_VERSION, templates: exported_list };
        let json = serde_json::to_string(&file).expect("序列化导出文件");
        let back: TemplateExportFile = serde_json::from_str(&json).expect("反序列化导出文件");

        // 导入回同一个库：名字会撞上源模板 → 照收、不改名，只在结果里报"有同名"
        let res = db.with_conn_mut(|c| import_templates_into(c, &back)).expect("导入");
        assert_eq!(res.imported, 1);
        assert_eq!(res.duplicated, 1, "同名只统计、不改名");
        assert!(res.missing_tools.is_empty(), "没绑工具就不该报缺失");
        assert!(res.missing_dirs.is_empty(), "cwd 为空不算缺失目录");

        db.with_conn(|c| {
            let n: i64 = c
                .query_row("SELECT COUNT(*) FROM service_templates WHERE name='前端 dev'", [], |r| r.get(0))
                .map_err(|e| e.to_string())?;
            assert_eq!(n, 2, "两条都该叫原名");
            Ok(())
        }).expect("校验名字保持原样");
    }

    /// 同名不改名、工具按名字匹配、缺工具与缺目录都要报出来
    #[test]
    fn test_import_keeps_duplicate_names_and_matches_tool_by_name() {
        let db = test_db();
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO service_templates (id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, sort_index, tool_commands, open_tool_id)
                 VALUES ('t1','T','c','','[]','','','',0,1,0,0,'[]','')",
                [],
            ).map_err(|e| e.to_string())?;
            c.execute(
                "INSERT INTO open_tools (id, name, command, executable, args) VALUES ('tool1','IDEA','','idea64.exe','{path}')",
                [],
            ).map_err(|e| e.to_string())?;
            Ok(())
        }).expect("准备数据");

        let mut with_missing_dir = exported("带目录的", "");
        with_missing_dir.cwd = format!("C:/nexus-ut-nonexistent-{}", std::process::id());
        let file = TemplateExportFile {
            version: TEMPLATE_EXPORT_VERSION,
            templates: vec![
                exported("T", "IDEA"),          // 名字撞库里的 T；工具在本机有
                exported("U", "不存在的工具"),   // 名字不撞；工具在本机没有
                with_missing_dir,                // 目录在本机不存在
            ],
        };

        let res = db.with_conn_mut(|c| import_templates_into(c, &file)).expect("导入");
        assert_eq!(res.imported, 3);
        assert_eq!(res.duplicated, 1, "只有与 T 撞名的那条被统计");
        assert_eq!(res.missing_tools, vec!["不存在的工具".to_string()]);
        assert_eq!(res.missing_dirs, vec!["带目录的".to_string()]);

        db.with_conn(|c| {
            let names: Vec<String> = c
                .prepare("SELECT name FROM service_templates ORDER BY name").map_err(|e| e.to_string())?
                .query_map([], |r| r.get(0)).map_err(|e| e.to_string())?
                .collect::<Result<_, _>>().map_err(|e| e.to_string())?;
            // 两条 T 都在、名字一模一样："T"(0x54) < "U"(0x55) < "带"(E5B8A6)
            assert_eq!(names, vec!["T".to_string(), "T".to_string(), "U".to_string(), "带目录的".to_string()]);

            // 工具按名字绑到本机的 IDEA（不是导出方的 id）
            let bound: String = c
                .query_row("SELECT open_tool_id FROM service_templates WHERE name='T' AND open_tool_id<>''", [], |r| r.get(0))
                .map_err(|e| e.to_string())?;
            assert_eq!(bound, "tool1");

            let unbound: i64 = c
                .query_row("SELECT COUNT(*) FROM service_templates WHERE name='U' AND open_tool_id=''", [], |r| r.get(0))
                .map_err(|e| e.to_string())?;
            assert_eq!(unbound, 1, "本机没有同名工具就不绑");
            Ok(())
        }).expect("校验落库结果");
    }

    /// 走**真实文件**的导出→导入：命令里做的就是"收集 → 序列化写盘"，再"读盘 → 反序列化"。
    /// 只测内存里的序列化会漏掉"落盘后读不回来"这类问题（这正是铁律 19 说的那条路径）
    #[test]
    fn test_export_import_through_real_file() {
        let db = test_db();
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO service_templates (id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, sort_index, tool_commands, open_tool_id)
                 VALUES ('t1','带出去','echo hi','','[]','','','',0,1,0,0,'[]','')",
                [],
            ).map_err(|e| e.to_string())?;
            Ok(())
        }).expect("插入模板");

        let dir = std::env::temp_dir().join(format!("nexus_ut_tpl_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let file = dir.join("templates.json");

        // 导出
        let picked = db.with_conn(|c| collect_export_templates(c, &[])).expect("收集模板");
        assert_eq!(picked.len(), 1);
        let payload = TemplateExportFile { version: TEMPLATE_EXPORT_VERSION, templates: picked };
        let json = serde_json::to_string_pretty(&payload).expect("序列化");
        std::fs::write(&file, &json).expect("写文件");
        assert!(json.contains("\n  "), "导出文件应当是缩进过的（要能手工看/改）");

        // 读回并导入（导回同一个库，必然同名 → 走改名分支）
        let raw = std::fs::read_to_string(&file).expect("读文件");
        let back: TemplateExportFile = serde_json::from_str(&raw).expect("反序列化");
        let res = db.with_conn_mut(|c| import_templates_into(c, &back)).expect("导入");
        assert_eq!(res.imported, 1);
        assert_eq!(res.duplicated, 1, "导回同一个库必然同名 → 只统计不改名");
        assert!(res.missing_dirs.is_empty(), "cwd 为空不算缺失目录");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 库里是空的、文件里也没有重名时，**一个后缀都不该加**。
    ///
    /// 起因是用户反馈"我模板里一个都没有，导入却直接加后缀，还得一个一个改"。
    /// 把"什么情况下才该加后缀"钉死：只有真的撞名才加，免得日后放宽条件。
    #[test]
    fn test_import_without_conflicts_keeps_original_names() {
        let db = test_db();
        let file = TemplateExportFile {
            version: TEMPLATE_EXPORT_VERSION,
            templates: vec![exported("前端", ""), exported("后端", ""), exported("redis", "")],
        };
        let res = db.with_conn_mut(|c| import_templates_into(c, &file)).expect("导入");
        assert_eq!(res.imported, 3);
        assert_eq!(res.duplicated, 0, "没有同名就没有可报的冲突");

        db.with_conn(|c| {
            let names: Vec<String> = c
                .prepare("SELECT name FROM service_templates ORDER BY name").map_err(|e| e.to_string())?
                .query_map([], |r| r.get(0)).map_err(|e| e.to_string())?
                .collect::<Result<_, _>>().map_err(|e| e.to_string())?;
            // ORDER BY name 是**字节序**："前"(E5 89 8D) < "后"(E5 90 8E)，不是拼音顺序
            assert_eq!(names, vec!["redis".to_string(), "前端".to_string(), "后端".to_string()]);
            Ok(())
        }).expect("校验名字未被改动");
    }

    /// 导出文件**内部**就有重名时：三条照收，名字**全部保持原样**。
    ///
    /// 这是"库里明明空着却被加了后缀"最常见的原因——文件里本身就有几个同名模板
    /// （同一个项目里几个都叫「前端」的服务，各另存了一次）。加后缀的话，用户导入完
    /// 还得挨个把「前端 (2)」「前端 (3)」改回来。
    #[test]
    fn test_import_keeps_names_when_file_has_duplicates() {
        let db = test_db();
        let file = TemplateExportFile {
            version: TEMPLATE_EXPORT_VERSION,
            templates: vec![exported("前端", ""), exported("前端", ""), exported("前端", "")],
        };
        let res = db.with_conn_mut(|c| import_templates_into(c, &file)).expect("导入");
        assert_eq!(res.imported, 3);
        assert_eq!(res.duplicated, 2, "第 2、3 条与刚落库的第 1 条同名");

        db.with_conn(|c| {
            let names: Vec<String> = c
                .prepare("SELECT name FROM service_templates ORDER BY name").map_err(|e| e.to_string())?
                .query_map([], |r| r.get(0)).map_err(|e| e.to_string())?
                .collect::<Result<_, _>>().map_err(|e| e.to_string())?;
            assert_eq!(
                names,
                vec!["前端".to_string(), "前端".to_string(), "前端".to_string()],
                "三条都叫原名，一个后缀都不加",
            );
            Ok(())
        }).expect("校验名字");
    }

    /// 重复导入同一份文件：产生两份**同名**副本（与"把同名服务另存两次"的行为一致），
    /// 删哪份由用户自己决定
    #[test]
    fn test_import_twice_keeps_both_copies_with_same_name() {
        let db = test_db();
        let file = TemplateExportFile {
            version: TEMPLATE_EXPORT_VERSION,
            templates: vec![exported("同一个模板", "")],
        };
        db.with_conn_mut(|c| import_templates_into(c, &file)).expect("第一次导入");
        let res = db.with_conn_mut(|c| import_templates_into(c, &file)).expect("第二次导入");
        assert_eq!(res.imported, 1);
        assert_eq!(res.duplicated, 1, "第二次与第一次同名 → 统计但不改名");

        db.with_conn(|c| {
            let names: Vec<String> = c
                .prepare("SELECT name FROM service_templates ORDER BY name").map_err(|e| e.to_string())?
                .query_map([], |r| r.get(0)).map_err(|e| e.to_string())?
                .collect::<Result<_, _>>().map_err(|e| e.to_string())?;
            assert_eq!(names, vec!["同一个模板".to_string(), "同一个模板".to_string()]);
            Ok(())
        }).expect("校验");
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
