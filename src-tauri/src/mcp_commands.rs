//! MCP 的通用命令桥：把 Nexus 的用户级功能暴露给 AI。
//!
//! 为什么不是给每个命令手写一个 MCP 工具：Nexus 有六七十个用户命令，逐个写
//! schema 会把工具列表撑爆，而且加一个功能就要记得改两处。这里收成两个工具：
//! - `nexus_commands`：返回命令目录（名称、说明、参数、是否不可逆）；
//! - `nexus_invoke`：按名称执行，参数用与前端一致的 camelCase。
//!
//! **不在这里的东西**：文件读写/搜索（dsh 自己的文件工具已覆盖，且更顺手）、
//! 原生文件对话框（需要人手操作）、AI 会话自身的启停/升级（递归且会杀掉调用它的
//! 会话）、窗口/内部事件（不是用户功能）。这些要么没有意义，要么会把宿主弄坏。

use std::collections::HashMap;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::AppState;

/// AI 改完数据后广播给界面的统一事件（`nexus-data-changed`）。
///
/// 为什么必须有：AI 走的是后端命令，前端的服务列表/项目列表都是 React 状态；
/// 不通知的话，卡片要等用户切项目（组件重挂载）才出现。
/// 只广播"哪个域变了"，由界面自己决定刷新哪一块，避免每次 AI 操作都全量重载。
#[derive(Clone, Serialize)]
pub struct NexusDataChanged {
    pub kind: String,
    /// 服务类变更所属项目（前端据此只刷新相关项目；None = 全局域）
    pub project_id: Option<String>,
    /// 具体变更的服务 id：界面据此提示"AI 改了正在编辑的服务"（列表刷新不需要它）
    pub changed_ids: Vec<String>,
}

/// 广播数据变更（语义工具也可以复用，见 `mcp.rs` 的 update_service）。
pub fn emit_changed(
    app: &AppHandle,
    kind: &str,
    project_id: Option<String>,
    changed_ids: Vec<String>,
) {
    if let Err(e) = app.emit(
        "nexus-data-changed",
        NexusDataChanged {
            kind: kind.to_string(),
            project_id,
            changed_ids,
        },
    ) {
        log::warn!("[mcp] 广播数据变更失败（kind={}）: {}", kind, e);
    }
}

/// 不可逆命令：必须显式传 `confirm: true` 才执行。
///
/// 不是"弹窗确认"（用户已经明确不要了），而是给调用方一个必须写出来的意图声明：
/// 模型误把 `delete_project` 当成 `get_project` 时不会顺手删掉数据。
const DESTRUCTIVE: &[&str] = &[
    "delete_project",
    "delete_service",
    "delete_service_template",
    "delete_open_tool",
    "uninstall_node_version",
    "uninstall_boot_runner",
];

/// 命令目录（供模型发现能力）。参数用示例值表达类型，避免维护一份完整 JSON Schema。
pub fn catalog() -> Value {
    let entries = vec![
        // ── 项目 ──────────────────────────────────────────────
        c("get_projects", "列出全部项目", json!({}), false),
        c("get_project_detail", "项目详情（含服务与模板）", json!({ "projectId": "string" }), false),
        c("add_project", "新建项目", json!({ "name": "string", "path": "string" }), false),
        c("update_project", "修改项目名称/路径", json!({ "id": "string", "name": "string", "path": "string" }), false),
        c("delete_project", "删除项目（含其服务，不可逆）", json!({ "id": "string", "confirm": true }), true),
        c("duplicate_project", "复制项目", json!({ "id": "string" }), false),
        c("toggle_pin_project", "切换项目置顶", json!({ "id": "string" }), false),
        // ── 服务 ──────────────────────────────────────────────
        c("get_services", "列出项目下的服务", json!({ "projectId": "string" }), false),
        c("add_service", "直接新建服务（不再需要用户保存面板）", json!({
            "projectId": "string", "name": "string", "command": "string", "cwd": "string",
            "watchPaths": "string(JSON数组)", "envVars": "string(dotenv或JSON)",
            "restartMode": 0, "toolCommands": "string(JSON数组)",
            "watchInclude": "string?", "watchExclude": "string?", "enabled": "bool?",
            "showFileTree": "bool?", "openEnabled": "bool?", "openPort": 0,
            "bootEnabled": "bool?", "bootOrder": 0, "bootCheck": "string?",
            "bootRequired": "bool?", "bootCredentialMode": "s4u?"
        }), false),
        c("update_service", "更新服务配置（全量字段）", json!({
            "id": "string", "name": "string", "command": "string", "cwd": "string",
            "watchPaths": "string", "watchInclude": "string", "watchExclude": "string",
            "envVars": "string", "restartMode": 0, "enabled": true, "showFileTree": false,
            "toolCommands": "string", "openEnabled": false, "openPort": 0,
            "bootEnabled": false, "bootOrder": 0, "bootCheck": "",
            "bootRequired": false, "bootCredentialMode": "s4u"
        }), false),
        c("delete_service", "删除服务（不可逆）", json!({ "id": "string", "confirm": true }), true),
        c("reorder_services", "调整服务顺序", json!({ "projectId": "string", "orderedIds": ["string"] }), false),
        // ── 服务模板 ──────────────────────────────────────────
        c("get_service_templates", "列出服务模板", json!({}), false),
        c("save_service_as_template", "把服务另存为模板", json!({ "serviceId": "string" }), false),
        c("add_service_from_template", "从模板添加服务", json!({ "projectId": "string", "templateId": "string" }), false),
        c("create_service_template", "新建模板", json!({
            "name": "string", "command": "string", "cwd": "string",
            "watchPaths": "string", "watchInclude": "*", "watchExclude": "",
            "envVars": "", "restartMode": 0, "enabled": true, "showFileTree": false,
            "openEnabled": false, "openPort": 0, "toolCommands": "[]", "openToolId": ""
        }), false),
        c("update_service_template", "修改模板", json!({
            "id": "string", "name": "string", "command": "string", "cwd": "string",
            "watchPaths": "string", "watchInclude": "string", "watchExclude": "string",
            "envVars": "string", "restartMode": 0, "enabled": true, "showFileTree": false,
            "openEnabled": false, "openPort": 0, "toolCommands": "string", "openToolId": ""
        }), false),
        c("delete_service_template", "删除模板（不可逆）", json!({ "id": "string", "confirm": true }), true),
        c("reorder_service_templates", "调整模板顺序", json!({ "orderedIds": ["string"] }), false),
        c("export_service_templates", "导出模板到文件", json!({ "path": "string", "ids": ["string"] }), false),
        c("import_service_templates", "从文件导入模板", json!({ "path": "string" }), false),
        // ── 进程 ──────────────────────────────────────────────
        c("start_service", "启动单个服务", json!({ "serviceId": "string" }), false),
        c("stop_service", "停止单个服务", json!({ "serviceId": "string" }), false),
        c("restart_service", "重启单个服务", json!({ "serviceId": "string" }), false),
        c("start_project_services", "启动项目全部已启用服务", json!({ "projectId": "string" }), false),
        c("stop_project_services", "停止项目全部服务", json!({ "projectId": "string" }), false),
        c("get_running", "当前运行/失败状态", json!({}), false),
        c("get_service_logs", "读取服务日志快照", json!({ "serviceKey": "string" }), false),
        c("run_tool_command", "执行服务配置里的工具命令（可能长时间运行）", json!({
            "serviceId": "string", "commandId": "string", "runId": "string"
        }), false),
        c("stop_tool_command", "停止正在运行的工具命令", json!({ "runId": "string" }), false),
        c("unfollow_service_log", "取消跟随服务日志文件", json!({ "serviceId": "string" }), false),
        // ── Node ──────────────────────────────────────────────
        c("get_node_runtime", "当前 Node 运行时状态", json!({}), false),
        c("list_node_versions", "已安装/可安装的 Node 版本", json!({}), false),
        c("set_nvm_mirrors", "设置 Node/npm 镜像", json!({ "nodeMirror": "string", "npmMirror": "string" }), false),
        c("install_node_version", "安装 Node 版本（可能耗时数分钟）", json!({ "version": "string" }), false),
        c("uninstall_node_version", "卸载 Node 版本（不可逆）", json!({ "version": "string", "confirm": true }), true),
        c("use_node_version", "切换当前 Node 版本（全局）", json!({ "version": "string" }), false),
        c("download_nvm_installer", "下载 nvm 安装器", json!({}), false),
        c("run_nvm_installer", "运行 nvm 安装器（系统级）", json!({}), false),
        // ── 打开工具 ──────────────────────────────────────────
        c("list_open_tools", "列出外部打开工具", json!({}), false),
        c("detect_and_add_open_tools", "自动检测本机工具并添加", json!({ "scanDir": "string?" }), false),
        c("save_open_tool", "新增/修改外部打开工具", json!({
            "id": "string?", "name": "string", "executable": "string", "args": "string"
        }), false),
        c("delete_open_tool", "删除外部打开工具（不可逆）", json!({ "id": "string", "confirm": true }), true),
        c("get_open_tool_usage", "查询工具被哪些模板引用", json!({ "id": "string" }), false),
        c("set_service_open_tool", "给服务绑定打开工具（toolId 省略 = 解绑）", json!({
            "serviceId": "string", "toolId": "string?"
        }), false),
        c("list_service_open_tool_bindings", "列出项目内服务与打开工具的绑定", json!({ "projectId": "string" }), false),
        c("open_service_with_tool", "用绑定的工具打开服务目录", json!({ "serviceId": "string" }), false),
        // ── 开机启动 ──────────────────────────────────────────
        c("get_boot_runner_status", "开机启动器状态", json!({}), false),
        c("get_boot_services_status", "开机启动服务状态", json!({}), false),
        c("install_boot_runner", "安装/修复开机启动器（mode: s4u|password，系统级）", json!({ "mode": "s4u" }), false),
        c("uninstall_boot_runner", "卸载开机启动器（不可逆）", json!({ "mode": "s4u", "confirm": true }), true),
        c("start_boot_runner", "启动开机启动器", json!({ "mode": "s4u" }), false),
        c("stop_boot_runner", "停止开机启动器", json!({ "mode": "s4u" }), false),
        c("set_service_boot_enabled", "设置服务是否开机启动", json!({ "serviceId": "string", "enabled": true }), false),
        c("get_autostart_enabled", "Nexus 是否随登录自启", json!({}), false),
        c("set_autostart_enabled", "设置 Nexus 随登录自启（系统级）", json!({ "enabled": true }), false),
        // ── 查看/打开（Nexus 专属的归档查看） ─────────────────
        c("open_terminal", "在指定目录打开终端", json!({ "path": "string" }), false),
        c("open_in_explorer", "在资源管理器中打开", json!({ "path": "string" }), false),
        c("open_path_with_default_app", "用系统默认程序打开", json!({ "path": "string" }), false),
        c("read_class_file", "读取 .class 字节码视图", json!({ "path": "string" }), false),
        c("decompile_class", "反编译 .class", json!({ "path": "string" }), false),
        c("list_jar", "列出 jar 条目", json!({ "path": "string", "nested": ["string"] }), false),
        c("read_jar_entry", "读取 jar 条目", json!({ "path": "string", "nested": ["string"], "name": "string" }), false),
        // ── 布局 / 应用信息 ──────────────────────────────────
        c("get_app_version", "Nexus 版本号", json!({}), false),
        c("get_app_paths", "数据/日志目录", json!({}), false),
        c("open_app_dir", "打开数据/日志目录（kind: data|logs）", json!({ "kind": "data" }), false),
        c("save_layout", "保存布局键值", json!({ "items": { "key": "value" } }), false),
        c("load_layout", "读取全部布局键值", json!({}), false),
        c("reset_layout", "恢复默认布局", json!({}), false),
    ];
    json!({ "ok": true, "commands": entries })
}

fn c(name: &str, description: &str, params: Value, destructive: bool) -> Value {
    json!({
        "name": name,
        "description": description,
        "params": params,
        "destructive": destructive,
    })
}

/// 执行一条 Nexus 用户命令。
pub async fn invoke(app: &AppHandle, command: &str, params: &Value) -> Result<Value, String> {
    if DESTRUCTIVE.contains(&command) && params.get("confirm").and_then(Value::as_bool) != Some(true) {
        return Err(format!(
            "「{}」是不可逆操作，需要在 params 里显式传 confirm: true（确认后才执行）",
            command
        ));
    }
    let state = app.state::<AppState>();
    let s = |key: &str| -> Result<String, String> {
        params
            .get(key)
            .and_then(Value::as_str)
            .map(|v| v.to_string())
            .ok_or_else(|| format!("缺少字符串参数 {}", key))
    };
    let opt_s = |key: &str| -> Option<String> {
        params
            .get(key)
            .and_then(Value::as_str)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let b = |key: &str| -> Result<bool, String> {
        params
            .get(key)
            .and_then(Value::as_bool)
            .ok_or_else(|| format!("缺少布尔参数 {}", key))
    };
    let arr = |key: &str| -> Result<Vec<String>, String> {
        params
            .get(key)
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .ok_or_else(|| format!("缺少数组参数 {}", key))
    };
    let nested = |key: &str| -> Vec<String> { arr(key).unwrap_or_default() };

    // 事件里带项目 id（服务域）：前端只刷新相关项目，不做全量重载。
    // 删除/更新按服务 id 操作——必须在命令执行**前**查所属项目，删完就查不到了。
    let event_project = match command {
        "add_service" | "reorder_services" | "add_service_from_template" => params
            .get("projectId")
            .and_then(Value::as_str)
            .map(str::to_string),
        "update_service" | "delete_service" => params
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| service_project_id(&state, id)),
        "set_service_open_tool" => params
            .get("serviceId")
            .and_then(Value::as_str)
            .and_then(|id| service_project_id(&state, id)),
        _ => None,
    };

    let result = match command {
        // ── 项目 ──
        "get_projects" => to_value(crate::commands::project::get_projects(state)?),
        "get_project_detail" => {
            to_value(crate::commands::project::get_project_detail(state, s("projectId")?)?)
        }
        "add_project" => to_value(crate::commands::project::add_project(state, s("name")?, s("path")?)?),
        "update_project" => to_value(crate::commands::project::update_project(
            state,
            s("id")?,
            s("name")?,
            s("path")?,
        )?),
        "delete_project" => {
            to_value(crate::commands::project::delete_project(app.clone(), s("id")?).await?)
        }
        "duplicate_project" => to_value(crate::commands::project::duplicate_project(state, s("id")?)?),
        "toggle_pin_project" => to_value(crate::commands::project::toggle_pin_project(state, s("id")?)?),
        // ── 服务 ──
        "get_services" => to_value(crate::commands::service::get_services(state, s("projectId")?)?),
        "add_service" => to_value(crate::commands::service::add_service(state, from_params(params)?)?),
        "update_service" => {
            to_value(crate::commands::service::update_service(app.clone(), from_params(params)?).await?)
        }
        "delete_service" => {
            to_value(crate::commands::service::delete_service(app.clone(), s("id")?).await?)
        }
        "reorder_services" => to_value(crate::commands::service::reorder_services(
            state,
            s("projectId")?,
            arr("orderedIds")?,
        )?),
        // ── 模板 ──
        "get_service_templates" => to_value(crate::commands::service::get_service_templates(state)?),
        "save_service_as_template" => {
            to_value(crate::commands::service::save_service_as_template(state, s("serviceId")?)?)
        }
        "add_service_from_template" => to_value(crate::commands::service::add_service_from_template(
            state,
            s("projectId")?,
            s("templateId")?,
        )?),
        "create_service_template" => {
            to_value(crate::commands::service::create_service_template(state, from_params(params)?)?)
        }
        "update_service_template" => {
            to_value(crate::commands::service::update_service_template(state, from_params(params)?)?)
        }
        "delete_service_template" => {
            to_value(crate::commands::service::delete_service_template(state, s("id")?)?)
        }
        "reorder_service_templates" => to_value(crate::commands::service::reorder_service_templates(
            state,
            arr("orderedIds")?,
        )?),
        "export_service_templates" => to_value(crate::commands::template_io::export_service_templates(
            state,
            s("path")?,
            arr("ids")?,
        )?),
        "import_service_templates" => {
            to_value(crate::commands::template_io::import_service_templates(state, s("path")?)?)
        }
        // ── 进程 ──
        "start_service" => {
            crate::commands::process::start_service(app.clone(), s("serviceId")?).await?;
            Ok(json!({ "ok": true }))
        }
        "stop_service" => {
            crate::commands::process::stop_service(app.clone(), s("serviceId")?).await?;
            Ok(json!({ "ok": true }))
        }
        "restart_service" => {
            crate::commands::process::restart_service(app.clone(), s("serviceId")?).await?;
            Ok(json!({ "ok": true }))
        }
        "start_project_services" => to_value(
            crate::commands::process::start_project_services(app.clone(), s("projectId")?).await?,
        ),
        "stop_project_services" => to_value(
            crate::commands::process::stop_project_services(app.clone(), s("projectId")?).await?,
        ),
        "get_running" => to_value(crate::commands::process::get_running(app.clone()).await?),
        "get_service_logs" => {
            to_value(crate::commands::process::get_service_logs(app.clone(), s("serviceKey")?).await?)
        }
        "run_tool_command" => to_value(
            crate::commands::process::run_tool_command(
                state,
                app.clone(),
                s("serviceId")?,
                s("commandId")?,
                s("runId")?,
            )
            .await?,
        ),
        "stop_tool_command" => {
            crate::commands::process::stop_tool_command(state, s("runId")?).await?;
            Ok(json!({ "ok": true }))
        }
        "unfollow_service_log" => {
            to_value(crate::commands::process::unfollow_service_log(app.clone(), s("serviceId")?).await?)
        }
        // ── Node ──
        "get_node_runtime" => to_value(crate::commands::node::get_node_runtime()),
        "list_node_versions" => to_value(crate::commands::node::list_node_versions().await?),
        "set_nvm_mirrors" => to_value(
            crate::commands::node::set_nvm_mirrors(s("nodeMirror")?, s("npmMirror")?).await?,
        ),
        "install_node_version" => {
            to_value(crate::commands::node::install_node_version(s("version")?).await?)
        }
        "uninstall_node_version" => {
            to_value(crate::commands::node::uninstall_node_version(s("version")?).await?)
        }
        "use_node_version" => to_value(crate::commands::node::use_node_version(s("version")?).await?),
        "download_nvm_installer" => to_value(crate::commands::node::download_nvm_installer().await?),
        "run_nvm_installer" => to_value(crate::commands::node::run_nvm_installer().await?),
        // ── 打开工具 ──
        "list_open_tools" => to_value(crate::commands::tools::list_open_tools(state)?),
        "detect_and_add_open_tools" => to_value(
            crate::commands::tools::detect_and_add_open_tools(app.clone(), opt_s("scanDir")).await?,
        ),
        "save_open_tool" => to_value(crate::commands::tools::save_open_tool(state, from_params(params)?)?),
        "delete_open_tool" => {
            to_value(crate::commands::tools::delete_open_tool(state, s("id")?)?)
        }
        "get_open_tool_usage" => to_value(crate::commands::tools::get_open_tool_usage(state, s("id")?)?),
        "set_service_open_tool" => {
            crate::commands::tools::set_service_open_tool(state, s("serviceId")?, opt_s("toolId"))?;
            Ok(json!({ "ok": true }))
        }
        "list_service_open_tool_bindings" => to_value(
            crate::commands::tools::list_service_open_tool_bindings(state, s("projectId")?)?,
        ),
        "open_service_with_tool" => {
            crate::commands::tools::open_service_with_tool(state, s("serviceId")?)?;
            Ok(json!({ "ok": true }))
        }
        // ── 开机启动 ──
        "get_boot_runner_status" => {
            to_value(crate::commands::boot::get_boot_runner_status(app.clone()).await?)
        }
        "get_boot_services_status" => {
            to_value(crate::commands::boot::get_boot_services_status(app.clone()).await?)
        }
        "install_boot_runner" => {
            to_value(crate::commands::boot::install_boot_runner(app.clone(), s("mode")?).await?)
        }
        "uninstall_boot_runner" => {
            to_value(crate::commands::boot::uninstall_boot_runner(app.clone(), s("mode")?).await?)
        }
        "start_boot_runner" => {
            to_value(crate::commands::boot::start_boot_runner(app.clone(), s("mode")?).await?)
        }
        "stop_boot_runner" => {
            to_value(crate::commands::boot::stop_boot_runner(app.clone(), s("mode")?).await?)
        }
        "set_service_boot_enabled" => to_value(crate::commands::boot::set_service_boot_enabled(
            state,
            s("serviceId")?,
            b("enabled")?,
        )?),
        "get_autostart_enabled" => to_value(crate::commands::autostart::get_autostart_enabled()?),
        "set_autostart_enabled" => {
            crate::commands::autostart::set_autostart_enabled(b("enabled")?)?;
            Ok(json!({ "ok": true }))
        }
        // ── 打开/查看 ──
        "open_terminal" => {
            crate::commands::editor::open_terminal(state, s("path")?)?;
            Ok(json!({ "ok": true }))
        }
        "open_in_explorer" => {
            crate::commands::editor::open_in_explorer(state, s("path")?)?;
            Ok(json!({ "ok": true }))
        }
        "open_path_with_default_app" => {
            crate::commands::editor::open_path_with_default_app(state, s("path")?)?;
            Ok(json!({ "ok": true }))
        }
        "read_class_file" => to_value(crate::commands::editor::read_class_file(state, s("path")?).await?),
        "decompile_class" => to_value(crate::commands::editor::decompile_class(state, s("path")?).await?),
        "list_jar" => to_value(crate::commands::editor::list_jar(state, s("path")?, nested("nested")).await?),
        "read_jar_entry" => to_value(
            crate::commands::editor::read_jar_entry(state, s("path")?, nested("nested"), s("name")?).await?,
        ),
        // ── 布局/应用信息 ──
        "get_app_version" => to_value(crate::commands::app::get_app_version()),
        "get_app_paths" => to_value(crate::commands::app::get_app_paths()),
        "open_app_dir" => {
            crate::commands::app::open_app_dir(s("kind")?)?;
            Ok(json!({ "ok": true }))
        }
        "save_layout" => {
            let items: HashMap<String, String> = from_params(
                params.get("items").ok_or_else(|| "缺少 items 对象".to_string())?,
            )?;
            crate::commands::layout::save_layout(state, items)?;
            Ok(json!({ "ok": true }))
        }
        "load_layout" => to_value(crate::commands::layout::load_layout(state)?),
        "reset_layout" => to_value(crate::commands::layout::reset_layout(state)?),
        other => Err(format!("未知命令: {}（先用 nexus_commands 查询）", other)),
    }?;

    if let Some(kind) = mutation_kind(command) {
        // 服务域带上具体 id：界面在用户恰好编辑同一个服务时给出提示
        let changed_ids: Vec<String> = match command {
            "add_service" | "add_service_from_template" => result
                .get("id")
                .and_then(Value::as_str)
                .map(|id| vec![id.to_string()])
                .unwrap_or_default(),
            "update_service" | "delete_service" => params
                .get("id")
                .and_then(Value::as_str)
                .map(|id| vec![id.to_string()])
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        log::info!("[mcp] nexus_invoke 写操作: {}（kind={}）", command, kind);
        emit_changed(app, kind, event_project, changed_ids);
    }
    Ok(result)
}

/// 哪些命令会改数据、需要通知界面刷新（只列用户可见的域）。
fn mutation_kind(command: &str) -> Option<&'static str> {
    match command {
        "add_service" | "update_service" | "delete_service" | "reorder_services"
        | "add_service_from_template" => Some("services"),
        "save_service_as_template" | "create_service_template" | "update_service_template"
        | "delete_service_template" | "reorder_service_templates" | "import_service_templates" => {
            Some("templates")
        }
        "add_project" | "update_project" | "delete_project" | "duplicate_project"
        | "toggle_pin_project" => Some("projects"),
        "save_open_tool" | "delete_open_tool" | "detect_and_add_open_tools"
        | "set_service_open_tool" => Some("tools"),
        "set_service_boot_enabled" | "install_boot_runner" | "uninstall_boot_runner"
        | "start_boot_runner" | "stop_boot_runner" => Some("boot"),
        "install_node_version" | "uninstall_node_version" | "use_node_version"
        | "set_nvm_mirrors" | "run_nvm_installer" => Some("node"),
        "set_autostart_enabled" => Some("autostart"),
        "save_layout" | "reset_layout" => Some("layout"),
        _ => None,
    }
}

fn service_project_id(state: &AppState, service_id: &str) -> Option<String> {
    state
        .db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT project_id FROM services WHERE id=?1",
                [service_id],
                |r| r.get::<_, String>(0),
            )
            .map_err(|e| format!("查询服务所属项目失败: {}", e))
        })
        .ok()
}

fn from_params<T: DeserializeOwned>(value: &Value) -> Result<T, String> {
    serde_json::from_value(value.clone()).map_err(|e| format!("参数不合法: {}", e))
}

fn to_value<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| format!("序列化命令结果失败: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 目录与分发必须同步：目录里列了命令、`invoke` 却没有分支，模型会拿到
    /// "未知命令"；加命令时只改一处是很容易犯的错（扫描源码而不是靠记忆）。
    #[test]
    fn catalog_entries_all_have_dispatch_arms() {
        let cat = catalog();
        let commands = cat["commands"].as_array().expect("commands 是数组");
        assert!(commands.len() >= 50, "命令目录过少，可能被误删: {}", commands.len());
        let source = include_str!("mcp_commands.rs");
        for entry in commands {
            let name = entry["name"].as_str().expect("命令名");
            assert!(
                source.contains(&format!("\"{}\" =>", name)),
                "目录里有命令 {}，但 invoke 没有对应分支",
                name
            );
        }
    }

    /// 不可逆标记只能两处一致：目录里 `destructive` 与 `DESTRUCTIVE` 列表。
    #[test]
    fn destructive_flags_match_the_guard_list() {
        let cat = catalog();
        for entry in cat["commands"].as_array().expect("commands 是数组") {
            let name = entry["name"].as_str().expect("命令名");
            let flagged = entry["destructive"].as_bool().expect("destructive");
            assert_eq!(
                flagged,
                DESTRUCTIVE.contains(&name),
                "命令 {} 的 destructive 标记与 DESTRUCTIVE 列表不一致",
                name
            );
            if flagged {
                assert!(
                    entry["params"].get("confirm").is_some(),
                    "不可逆命令 {} 的参数示例里应包含 confirm",
                    name
                );
            }
        }
    }
}
