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

/// 需要在 Nexus 界面由用户点「允许一次」的命令。
///
/// 判据 =「能执行代码 / 能改系统级持久化或全局工具链」。注意与 `DESTRUCTIVE` 的区别：
/// `confirm: true` 是**给模型写的意图声明**——模型照写不误，它不是人点的；这里的命令
/// 必须过人（项目文件/日志里的一段提示注入就足以让模型替用户按下按钮）。
const NEEDS_USER_CONFIRM: &[&str] = &[
    "add_service",
    // 从模板建服务同样是"写一条将被执行的命令"：模板可能刚被 AI 导入/修改过
    // （import/update_service_template 不弹窗），不能成为绕过 add_service 闸门的后门
    "add_service_from_template",
    "update_service",
    "save_open_tool",
    "open_service_with_tool",
    "run_tool_command",
    "open_path_with_default_app",
    "install_boot_runner",
    "start_boot_runner",
    "stop_boot_runner",
    "set_service_boot_enabled",
    "set_autostart_enabled",
    "install_node_version",
    "use_node_version",
    "set_nvm_mirrors",
    "run_nvm_installer",
];

/// AI 自身安全边界的布局键：MCP 桥拒绝写入，只能由用户在「设置 > AI 运行时」里改。
///
/// 目前只有「AI 高危操作是否弹窗确认」——如果 AI 能改它，就能先关掉确认再执行，
/// 整个闸门等于不存在。
fn protected_layout_key(key: &str) -> bool {
    key == crate::commands::layout::KEY_MCP_CONFIRM
}

/// 项目/服务域命令的「目标项目」；`None` = 全局域命令（不参与绑定项目校验）。
///
/// 语义工具（list_services/start_service…）早就用 `BoundProject` 收死了范围；
/// 通用桥必须作同一套判定，否则 `nexus_invoke` 就是绕过它的后门。
fn target_project_id(
    command: &str,
    params: &Value,
    service_owner_of: impl Fn(&str) -> Option<String>,
) -> Result<Option<String>, String> {
    let need = |key: &str| {
        params
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("缺少字符串参数 {}", key))
    };
    let service_owner = |key: &str| -> Result<String, String> {
        let id = need(key)?;
        service_owner_of(&id).ok_or_else(|| format!("找不到服务: {}", id))
    };
    Ok(match command {
        // 参数里直接带 projectId
        "get_project_detail"
        | "get_services"
        | "add_service"
        | "reorder_services"
        | "add_service_from_template"
        | "start_project_services"
        | "stop_project_services"
        | "list_service_open_tool_bindings" => Some(need("projectId")?),
        // 项目 id 在 `id` 里
        "update_project" | "delete_project" | "duplicate_project" | "toggle_pin_project" => {
            Some(need("id")?)
        }
        // 服务域：按服务 id 反查所属项目
        "update_service" | "delete_service" => Some(service_owner("id")?),
        "save_service_as_template"
        | "start_service"
        | "stop_service"
        | "restart_service"
        | "run_tool_command"
        | "unfollow_service_log"
        | "set_service_open_tool"
        | "open_service_with_tool" => Some(service_owner("serviceId")?),
        "get_service_logs" => Some(service_owner("serviceKey")?),
        _ => None,
    })
}

/// 服务配置里除 command/cwd 之外、同样能提权或持久化的字段。
///
/// 确认框如果只显示主命令，用户就不知道这一次还顺带写入了"工具命令"（之后可由
/// run_tool_command 执行）或"开机启动"（提权持久化）——这些不是同一件事，必须说出来。
fn extra_risk_notes(params: &Value) -> String {
    let mut out = String::new();
    if let Some(commands) = params.get("toolCommands").and_then(Value::as_str) {
        let commands = commands.trim();
        if !commands.is_empty() && commands != "[]" {
            let shown: String = commands.chars().take(300).collect();
            out.push_str(&format!("\n工具命令：{shown}"));
        }
    }
    if params.get("bootEnabled").and_then(Value::as_bool) == Some(true) {
        let mode = params
            .get("bootCredentialMode")
            .and_then(Value::as_str)
            .unwrap_or("s4u");
        out.push_str(&format!("\n开机启动：开（{mode}）"));
    }
    out
}

/// 弹窗文案：人话 + 关键参数。
///
/// **不展开环境变量值**（可能是密钥）：只报条数。命令本身要原样展示——用户是在
/// 批准"这条命令将被执行"，藏起来就等于没批准。
fn confirm_prompt(
    command: &str,
    params: &Value,
    template_of: impl Fn(&str) -> Option<(String, String, String)>,
) -> (String, String) {
    let s = |key: &str| params.get(key).and_then(Value::as_str).unwrap_or("");
    let env_count = |key: &str| {
        params
            .get(key)
            .and_then(Value::as_str)
            .map(|v| {
                v.lines()
                    .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
                    .count()
            })
            .unwrap_or(0)
    };
    let bool_param = |key: &str| {
        params
            .get(key)
            .and_then(Value::as_bool)
            .map(|b| b.to_string())
            .unwrap_or_else(|| "未指定".to_string())
    };
    let detail = match command {
        "add_service" => format!(
            "新建服务（服务启动时会执行下面这条命令）\n名称：{}\n命令：{}\n工作目录：{}\n环境变量：{} 项（值不展开）{}",
            s("name"),
            s("command"),
            s("cwd"),
            env_count("envVars"),
            extra_risk_notes(params)
        ),
        "update_service" => format!(
            "修改服务配置（服务下次启动时会执行新的命令）\n名称：{}\n命令：{}\n工作目录：{}\n环境变量：{} 项（值不展开）{}",
            s("name"),
            s("command"),
            s("cwd"),
            env_count("envVars"),
            extra_risk_notes(params)
        ),
        "add_service_from_template" => {
            let template_id = s("templateId");
            match template_of(template_id) {
                Some((name, command, cwd)) => format!(
                    "从模板新建服务（启动时会执行模板里的命令）\n模板：{}\n命令：{}\n工作目录：{}\n目标项目：{}",
                    name, command, cwd, s("projectId")
                ),
                None => format!(
                    "从模板新建服务\n模板 id：{}\n目标项目：{}\n\n（读取模板内容失败；执行分支会再校验一次）",
                    template_id, s("projectId")
                ),
            }
        }
        "save_open_tool" => format!(
            "保存外部打开工具\n名称：{}\n可执行文件：{}\n参数：{}\n\n保存后，用该工具打开服务时会启动这个程序。",
            s("name"), s("executable"), s("args")
        ),
        "open_service_with_tool" => format!(
            "用外部工具打开服务目录\nserviceId：{}\n\n这会启动该服务绑定的外部程序。",
            s("serviceId")
        ),
        "run_tool_command" => format!(
            "执行服务配置里的工具命令\nserviceId：{}\ncommandId：{}\n\n命令内容来自该服务的工具命令配置。",
            s("serviceId"), s("commandId")
        ),
        "open_path_with_default_app" => format!(
            "用系统默认程序打开文件\n路径：{}\n\n如果该文件是可执行文件，这等于直接运行它。",
            s("path")
        ),
        "install_boot_runner" => format!(
            "安装/修复开机启动器（创建系统级计划任务）\nmode：{}",
            s("mode")
        ),
        "start_boot_runner" => format!("启动开机启动器\nmode：{}", s("mode")),
        "stop_boot_runner" => format!("停止开机启动器\nmode：{}", s("mode")),
        "set_service_boot_enabled" => format!(
            "修改服务的开机启动设置\nserviceId：{}\nenabled：{}",
            s("serviceId"),
            bool_param("enabled")
        ),
        "set_autostart_enabled" => {
            format!("修改 Nexus 是否随登录自启\nenabled：{}", bool_param("enabled"))
        }
        "install_node_version" => format!("安装 Node 版本：{}", s("version")),
        "use_node_version" => format!("切换全局 Node 版本：{}", s("version")),
        "set_nvm_mirrors" => format!(
            "修改 nvm 镜像（影响后续 Node 下载的来源）\nnode 镜像：{}\nnpm 镜像：{}",
            s("nodeMirror"),
            s("npmMirror")
        ),
        "run_nvm_installer" => "运行 nvm 安装器（会弹出系统级安装/UAC 窗口）".to_string(),
        _ => {
            let text: String = params.to_string().chars().take(800).collect();
            format!("参数：{}", text)
        }
    };
    (
        format!("AI 请求执行：{}", command),
        format!("{detail}\n\n这是 AI 通过 MCP 发起的操作。请确认内容可信后再允许。"),
    )
}

/// 执行一条 Nexus 用户命令。
///
/// `bound_project` = 本次 AI 会话绑定的项目 id（见 `mcp.rs::BoundProject`）：
/// 项目/服务域命令只能落在它身上；`None`（会话没有有效工作目录）时这类命令一律拒绝。
pub async fn invoke(
    app: &AppHandle,
    command: &str,
    params: &Value,
    bound_project: Option<&str>,
) -> Result<Value, String> {
    if DESTRUCTIVE.contains(&command) && params.get("confirm").and_then(Value::as_bool) != Some(true) {
        return Err(format!(
            "「{}」是不可逆操作，需要在 params 里显式传 confirm: true（确认后才执行）",
            command
        ));
    }
    let state = app.state::<AppState>();

    // ① 项目边界：通用桥不能操作绑定项目之外的项目/服务
    if let Some(target) = target_project_id(command, params, |id| service_project_id(&state, id))? {
        let Some(bound) = bound_project else {
            return Err(
                "当前 AI 会话没有绑定项目（启动会话时未带有效工作目录），项目/服务类命令不可用"
                    .into(),
            );
        };
        if target != bound {
            return Err(format!(
                "权限边界：当前 AI 会话只允许操作绑定项目（{bound}），已拒绝操作其它项目（{target}）。如需操作其它项目，请在 Nexus 里切换到该项目后重新打开 AI 面板。"
            ));
        }
    }

    // ② 能执行代码 / 改系统级持久化的命令：必须用户在界面点「允许一次」
    if NEEDS_USER_CONFIRM.contains(&command) {
        let (title, mut detail) =
            confirm_prompt(command, params, |id| template_summary(&state, id));
        // 工具命令的实际内容在服务配置里，confirm_prompt 拿不到：补一次查询，
        // 让用户看到"将执行什么"，而不是只看到一个 commandId。
        if command == "run_tool_command" {
            let service_id = params.get("serviceId").and_then(Value::as_str).unwrap_or("");
            let command_id = params.get("commandId").and_then(Value::as_str).unwrap_or("");
            match tool_command_text(&state, service_id, command_id) {
                Some(text) => detail.push_str(&format!("\n\n即将执行：{}", text)),
                None => detail.push_str("\n\n（读取工具命令内容失败；执行分支会再校验一次）"),
            }
        }
        crate::commands::mcp_confirm::ask(app, &title, &detail).await?;
    }
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
            // AI 不能改自己的安全设置：否则它可以先关掉确认闸门、再执行任意命令，
            // 闸门就只是个装饰。只能由用户在 AI 面板头部切换。
            if items.keys().any(|key| protected_layout_key(key)) {
                return Err(
                    "「AI 操作确认」开关只能由用户在「设置 > AI 运行时」里修改，AI 不能修改"
                        .into(),
                );
            }
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

/// 服务里某条工具命令的实际命令行（确认框要让用户看到"将执行什么"）。
fn tool_command_text(state: &AppState, service_id: &str, command_id: &str) -> Option<String> {
    let json: String = state
        .db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT tool_commands FROM services WHERE id=?1",
                [service_id],
                |r| r.get(0),
            )
            .map_err(|e| format!("查询服务失败: {}", e))
        })
        .ok()?;
    let commands: Vec<crate::models::ToolCommand> = serde_json::from_str(&json).ok()?;
    commands
        .into_iter()
        .find(|c| c.id == command_id)
        .map(|c| c.command)
}

/// 模板 id → (名称, 命令, 工作目录)；查不到返回 None。
///
/// 只用于确认框展示"用户将要批准的到底是什么命令"——查不到时确认框仍会出现，
/// 真正的执行分支会再校验一次。
fn template_summary(state: &AppState, template_id: &str) -> Option<(String, String, String)> {
    state
        .db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT name, command, cwd FROM service_templates WHERE id=?1",
                [template_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(|e| format!("查询模板失败: {}", e))
        })
        .ok()
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

    /// 确认清单不能写出目录里不存在的命令（改名/删命令时这里先炸，而不是运行时才发现）
    #[test]
    fn confirm_required_commands_exist_in_catalog() {
        let cat = catalog();
        let names: Vec<&str> = cat["commands"]
            .as_array()
            .expect("commands 是数组")
            .iter()
            .filter_map(|e| e["name"].as_str())
            .collect();
        for cmd in NEEDS_USER_CONFIRM {
            assert!(
                names.contains(cmd),
                "确认清单里的「{}」不在命令目录里——改名时漏了同步？",
                cmd
            );
        }
        // 从模板建服务是 add_service 的等价物（模板命令可能刚被 AI 改过），不能漏
        assert!(NEEDS_USER_CONFIRM.contains(&"add_service_from_template"));
    }

    /// 确认框必须展示"要执行的命令"，但**不能展开环境变量的值**（可能是密钥）
    #[test]
    fn confirm_prompt_shows_command_but_hides_env_values() {
        let params = json!({
            "name": "svc",
            "command": "python app.py",
            "cwd": "D:/proj",
            "envVars": "API_KEY=super-secret\nDEBUG=1"
        });
        let (_, detail) = confirm_prompt("add_service", &params, |_| None);
        assert!(
            detail.contains("python app.py"),
            "批准的是命令本身，不能藏起来"
        );
        assert!(!detail.contains("super-secret"), "环境变量的值不能进确认框");
        assert!(detail.contains("2 项"), "应只报环境变量条数: {}", detail);
    }

    /// AI 不能通过 save_layout 改自己的确认开关
    #[test]
    fn ai_cannot_change_its_own_confirm_setting() {
        assert!(protected_layout_key(crate::commands::layout::KEY_MCP_CONFIRM));
        assert!(!protected_layout_key("ai_panel_width"));
        assert!(!protected_layout_key("selected_project_id"));
    }

    /// 顺带写入的"工具命令 / 开机启动"也要在确认框里说出来（不是只看主命令）
    #[test]
    fn confirm_prompt_mentions_boot_and_tool_commands() {
        let params = json!({
            "name": "svc",
            "command": "python app.py",
            "cwd": "D:/x",
            "envVars": "SECRET=x",
            "bootEnabled": true,
            "bootCredentialMode": "password",
            "toolCommands": "[{\"name\":\"run\",\"command\":\"yarn build\"}]"
        });
        let (_, detail) = confirm_prompt("add_service", &params, |_| None);
        assert!(detail.contains("开机启动：开（password）"), "{}", detail);
        assert!(detail.contains("yarn build"), "工具命令也要展示: {}", detail);
        assert!(!detail.contains("SECRET"), "环境变量值不能进确认框: {}", detail);
    }

    /// 从模板建服务同样是被执行的命令：确认框必须展示模板里的实际命令
    #[test]
    fn confirm_prompt_for_template_shows_the_command() {
        let params = json!({ "projectId": "p", "templateId": "t1" });
        let (_, detail) = confirm_prompt("add_service_from_template", &params, |id| {
            (id == "t1").then(|| {
                (
                    "tmpl".to_string(),
                    "python manage.py runserver".to_string(),
                    "D:/proj".to_string(),
                )
            })
        });
        assert!(detail.contains("tmpl"), "应显示模板名: {}", detail);
        assert!(
            detail.contains("python manage.py runserver"),
            "批准的是将要执行的命令，不能藏起来: {}",
            detail
        );
    }

    /// 项目边界：直接带 projectId / 项目 id / 服务 id 三类都要解析出目标项目；
    /// 全局域命令返回 None；服务不存在时必须报错而不是当作"无目标"放行。
    #[test]
    fn target_project_resolves_project_and_service_commands() {
        let owner = |id: &str| (id == "svc-1").then(|| "proj-1".to_string());
        assert_eq!(
            target_project_id("get_services", &json!({ "projectId": "proj-2" }), owner).unwrap(),
            Some("proj-2".to_string())
        );
        assert_eq!(
            target_project_id("delete_project", &json!({ "id": "proj-3" }), owner).unwrap(),
            Some("proj-3".to_string())
        );
        assert_eq!(
            target_project_id("start_service", &json!({ "serviceId": "svc-1" }), owner).unwrap(),
            Some("proj-1".to_string())
        );
        assert_eq!(
            target_project_id(
                "save_service_as_template",
                &json!({ "serviceId": "svc-1" }),
                owner
            )
            .unwrap(),
            Some("proj-1".to_string())
        );
        assert_eq!(
            target_project_id("get_service_logs", &json!({ "serviceKey": "svc-1" }), owner)
                .unwrap(),
            Some("proj-1".to_string())
        );
        assert_eq!(
            target_project_id("get_projects", &json!({}), owner).unwrap(),
            None,
            "全局域命令不参与绑定项目校验"
        );
        assert!(
            target_project_id("start_service", &json!({ "serviceId": "svc-404" }), owner).is_err(),
            "解析不到目标项目不能当作全局命令放行"
        );
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
