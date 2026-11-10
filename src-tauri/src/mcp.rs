//! 内嵌 dsh 的 MCP 工具服务（方案 B：主进程内 streamable-http）。
//!
//! **为什么在主进程内**：工具最终都要复用现有 Tauri command 的数据库与
//! `ProcessManager`（唯一数据源）。另起一个桥进程会让"两个 ProcessManager / 双写 /
//! UI 与 AI 看到不同状态"成为可能——这正是方案 A（stdio 桥）的主要成本。
//!
//! **为什么是 streamable-http**：少一个自 spawn 进程和一套自定义管道协议；
//! 代价只是本机回环上的一个随机端口 + 每次 AI 会话独立的随机 token。
//! 3.9 版 rmcp 支持 2026-07-28 协议，已用 dsh 0.2.1-rc.5 自带的
//! `@modelcontextprotocol/client` 与 `dsh --patch` 实测 `tools/list` 可用。
//!
//! 生命周期：`ai_start` 建服务 → 句柄挂在 `AiSession` 上 → 会话被停止/替换/丢弃时
//! `McpHandle::drop` 取消服务并删除 patch 文件。**不要让服务活得比 dsh 长**：
//! dsh 的 MCP 条目只在启动时读取 patch，服务换了端口，旧客户端会一直连不上。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rmcp::model::{
    CallToolRequestParams, CallToolRequestMethod, CallToolResponse, CallToolResult, Implementation,
    JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use rmcp::{ErrorData as McpError, ServerHandler};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{Emitter, Manager};
use tokio_util::sync::CancellationToken;

use crate::AppState;

/// 单次返回给模型的日志上限（行 / 字节）。
///
/// 后端日志缓冲本身是 2000 行 / 2MB，直接塞给模型既贵又没用：模型需要的是
/// "最后几十行 + 有没有更多"，不是全量快照。两个上限量纲不同，缺一条就会漏一种形状。
const LOG_TOOL_MAX_LINES: usize = 200;
const LOG_TOOL_MAX_BYTES: usize = 64 * 1024;
/// 单行日志的字符上限：一行的无换行输出可以是 MB 级，必须按行再截一次。
const LOG_TOOL_MAX_LINE_CHARS: usize = 4_000;

/// 失败时**自动附带**的日志上限。
///
/// 与手动 `get_service_logs` 分开：自动附带是"顺手给点线索"，不能把工具结果撑爆
/// （模型上下文里还要留地方放确认/后续对话）。失败服务最多 5 个，每个 30 行/8KB。
const AUTO_LOG_MAX_SERVICES: usize = 5;
const AUTO_LOG_MAX_LINES: usize = 30;
const AUTO_LOG_MAX_BYTES: usize = 8 * 1024;
const AUTO_LOG_MAX_LINE_CHARS: usize = 2_000;

/// 打开「添加服务」编辑面板的预填草稿（`mcp-open-service-draft` 事件）。
///
/// 只打开面板、不写数据库：服务命令等于任意代码执行，必须由用户在表单里
/// 看到完整内容并点「保存」才算确认。
#[derive(Clone, Serialize)]
pub struct ServiceDraftPayload {
    pub project_id: String,
    pub name: String,
    pub command: String,
    pub cwd: String,
    pub watch_paths: String,
    pub watch_include: String,
    pub watch_exclude: String,
    pub env_vars: String,
}


/// 当前 AI 会话绑定的项目（来自 `ai_start` 的 cwd）。
///
/// v1 只允许操作这一个项目：跨项目写操作需要显式参数与额外确认（见需求文档），
/// 先把边界收死。
#[derive(Clone)]
struct BoundProject {
    id: String,
    name: String,
    path: String,
}

/// MCP 服务句柄：随 `AiSession` 存放，drop 即收尾。
pub struct McpHandle {
    patch_path: PathBuf,
    cancel: CancellationToken,
}

impl McpHandle {
    /// 本次会话的 patch 文件路径（交给 dsh 的 `--patch`）。
    pub fn patch_path(&self) -> &Path {
        &self.patch_path
    }
}

impl Drop for McpHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
        match std::fs::remove_file(&self.patch_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log::warn!(
                "[mcp] 清理会话 patch 文件失败（{}）: {}",
                self.patch_path.display(),
                e
            ),
        }
    }
}

/// 启动本次 AI 会话的 MCP 服务并生成 patch 文件。
///
/// 返回的句柄必须挂到 `AiSession` 上；调用方若启动 dsh 失败，句柄会随局部变量
/// drop，服务与 patch 文件一起回收。
pub(crate) fn start_session(
    app: &tauri::AppHandle,
    cwd: Option<&str>,
) -> Result<McpHandle, String> {
    let project = resolve_bound_project(app, cwd);
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );

    // 先绑定端口再写 patch：端口拿到之前写出的 patch 只会指向一个不存在的服务。
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .map_err(|e| format!("MCP 服务绑定本机端口失败: {}", e))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("MCP 监听端口设置非阻塞失败: {}", e))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("读取 MCP 服务端口失败: {}", e))?
        .port();
    let patch_path = write_patch_file(port, &token)?;

    let cancel = CancellationToken::new();
    let server_app = app.clone();
    let server_project = project.clone();
    let server_token = token.clone();
    let server_cancel = cancel.clone();
    // 句柄不参与生命周期（关停由 cancel 控制）；绑定到 `_` 前缀变量，避免
    // clippy 把"丢弃 future"误判成漏掉 await。
    let _server_task = tauri::async_runtime::spawn(async move {
        // std → tokio 监听器：这里已经在 runtime 上下文里，转换失败通常意味着
        // 端口被别的进程抢走或句柄失效；只留日志，dsh 侧会表现为"工具不可用"。
        let listener = match tokio::net::TcpListener::from_std(listener) {
            Ok(l) => l,
            Err(e) => {
                log::error!("[mcp] 转换监听端口失败: {}", e);
                return;
            }
        };
        // non_exhaustive：不能写结构体字面量，只能在 Default 上改公开字段
        let mut config = StreamableHttpServerConfig::default();
        config.cancellation_token = server_cancel.clone();
        let service: StreamableHttpService<NexusMcpServer, LocalSessionManager> =
            StreamableHttpService::new(
                move || Ok(NexusMcpServer::new(server_app.clone(), server_project.clone())),
                Default::default(),
                config,
            );
        let router = axum::Router::new()
            .nest_service("/mcp", service)
            // Bearer 校验：端口在回环上，本机其它进程理论上可访问；token 每次会话随机，
            // 且只写进 dsh 的会话级 patch 文件。失败一律 401，不暴露任何服务信息。
            .layer(axum::middleware::from_fn(
                move |req: axum::extract::Request, next: axum::middleware::Next| {
                    let expected = server_token.clone();
                    async move {
                        if bearer_matches(req.headers(), &expected) {
                            next.run(req).await
                        } else {
                            axum::response::Response::builder()
                                .status(axum::http::StatusCode::UNAUTHORIZED)
                                .body(axum::body::Body::empty())
                                .expect("静态响应构造不会失败")
                        }
                    }
                },
            ));
        log::info!("[mcp] 工具服务已启动: http://127.0.0.1:{}/mcp", port);
        if let Err(e) = axum::serve(listener, router)
            .with_graceful_shutdown(async move { server_cancel.cancelled().await })
            .await
        {
            log::error!("[mcp] 工具服务异常退出: {}", e);
        }
        log::info!("[mcp] 工具服务已停止");
    });

    log::info!(
        "[mcp] 会话绑定项目: {}",
        project
            .as_ref()
            .map(|p| format!("{} ({})", p.name, p.id))
            .unwrap_or_else(|| "（未绑定，服务类工具将拒绝执行）".to_string())
    );
    Ok(McpHandle {
        patch_path,
        cancel,
    })
}

/// 生成 dsh 的会话级 patch（`--patch` 覆盖层）。
///
/// 文件名带 pid + 随机后缀：`patchReload: live` 下固定文件名会被后启动的实例
/// 覆盖（安装版与 dev 版并存是实测过的场景），导致旧会话热重载到新端口。
fn write_patch_file(port: u16, token: &str) -> Result<PathBuf, String> {
    let dir = crate::data_dir().join("ai");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("创建 MCP 配置目录失败（{}）: {}", dir.display(), e))?;
    let path = dir.join(format!(
        "nexus-mcp-{}-{}.yml",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let body = format!(
        "# Nexus 生成（会话级 MCP 工具入口）：dsh 停止时自动删除，请勿手改。\n\
         - insert:\n\
         \x20   - id: mcp-nexus\n\
         \x20     name: '@deepseek-ai/dsh-mcp-client'\n\
         \x20     config:\n\
         \x20       serverName: nexus\n\
         \x20       transport: streamable-http\n\
         \x20       url: http://127.0.0.1:{port}/mcp\n\
         \x20       headers:\n\
         \x20         Authorization: 'Bearer {token}'\n\
         \x20       toolCallTimeoutMs: 600000\n\
         \x20       failOnStartupError: false\n"
    );
    std::fs::write(&path, body)
        .map_err(|e| format!("写入 MCP patch 文件失败（{}）: {}", path.display(), e))?;
    Ok(path)
}

/// 清理上次进程遗留的 MCP patch 文件（内含一次性 Bearer token）。
///
/// 正常路径由 `McpHandle::drop` 删除；进程被强杀/崩溃/开发热重启时走不到。
/// 按文件名中的 pid 判活：**只删进程已不存在的那份**——本机可能同时跑着安装版与
/// dev 版（实测过），另一个实例的 patch 可能正要被它自己启动的 dsh 读取。
/// pid 被无关进程复用时只会"多留一个文件"，不会误删活实例的配置。
#[cfg(windows)]
pub(crate) fn cleanup_stale_patch_files() {
    let dir = crate::data_dir().join("ai");
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(rest) = name.strip_prefix("nexus-mcp-") else { continue };
        if !name.ends_with(".yml") { continue; }
        let Some(pid) = rest.split('-').next().and_then(|s| s.parse::<u32>().ok()) else { continue };
        // 句柄身份不受 pid 复用影响；查不到（进程已退出/无权限）按"已退出"处理。
        // 这些文件只在启动时被读一次，误删活实例的文件的唯一后果是它本次 MCP 不可用
        let owner_alive = crate::core::winproc::OwnedProcess::open(pid)
            .map(|p| p.is_alive())
            .unwrap_or(false);
        if owner_alive {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => log::info!("[mcp] 已清理遗留 patch 文件: {}", name),
            Err(e) => log::warn!(
                "[mcp] 清理遗留 patch 文件失败（{}）: {}",
                entry.path().display(),
                e
            ),
        }
    }
}

/// 会话 cwd → 所属项目。取**最长前缀**：cwd 可能是项目下的服务工作目录。
fn resolve_bound_project(app: &tauri::AppHandle, cwd: Option<&str>) -> Option<BoundProject> {
    let cwd = cwd?;
    let target = normalize_path(cwd);
    let state = app.state::<AppState>();
    let projects: Vec<(String, String, String)> = match state.db.with_conn(|conn| {
        let mut stmt = conn
            .prepare("SELECT id, name, path FROM projects")
            .map_err(|e| format!("查询项目列表失败: {}", e))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| format!("读取项目数据失败: {}", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| format!("解析项目数据失败: {}", e))?);
        }
        Ok(out)
    }) {
        Ok(p) => p,
        Err(e) => {
            // 不能静默降级成"未绑定项目"：那会让服务类工具报出误导性的原因
            log::warn!("[mcp] 解析会话所属项目失败: {}", e);
            return None;
        }
    };
    projects
        .into_iter()
        .filter(|(_, _, path)| {
            let p = normalize_path(path);
            target == p || target.starts_with(&(p + "\\"))
        })
        .max_by_key(|(_, _, path)| normalize_path(path).len())
        .map(|(id, name, path)| BoundProject { id, name, path })
}

/// Windows 路径归一化：分隔符统一、大小写统一、去尾部反斜杠（`C:\` 保留）。
fn normalize_path(s: &str) -> String {
    let mut n = s.trim().replace('/', "\\").to_lowercase();
    while n.len() > 3 && n.ends_with('\\') {
        n.pop();
    }
    n
}

// ── MCP server ────────────────────────────────────────────────

#[derive(Clone)]
struct NexusMcpServer {
    app: tauri::AppHandle,
    project: Option<BoundProject>,
}

#[derive(Deserialize)]
struct ServiceArgs {
    /// 服务 id 或名称（在当前项目内解析；重名时返回候选而不是猜）
    service: String,
}

/// 通用命令桥参数（`nexus_invoke`）。
#[derive(Deserialize)]
struct InvokeArgs {
    command: String,
    #[serde(default)]
    params: Value,
}

/// 修改既有服务配置（启动失败时 AI 用来修配置）。
///
/// 只开放与"能不能启动"相关的字段：命令、工作目录、env、监听路径、启用状态。
/// 工具命令 / 开机启动 / 端口绑定等不在其中——那些不是启动失败的常见原因，
/// 暴露出去只会扩大改动面。
#[derive(Deserialize)]
struct UpdateServiceArgs {
    /// 服务 id 或名称
    service: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    /// 新增/修改的环境变量（只动这里列出的键，其余保持不变）
    #[serde(default)]
    env_set: Option<std::collections::HashMap<String, String>>,
    /// 要删除的环境变量名
    #[serde(default)]
    env_unset: Option<Vec<String>>,
    #[serde(default)]
    watch_paths: Option<String>,
    #[serde(default)]
    watch_include: Option<String>,
    #[serde(default)]
    watch_exclude: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Deserialize)]
struct PrepareAddServiceArgs {
    /// 服务名称（必填）
    name: String,
    /// 启动命令（必填）
    command: String,
    /// 工作目录；省略时用当前项目根目录
    #[serde(default)]
    cwd: Option<String>,
    /// dotenv 格式的 KEY=VALUE 行；省略 = 不配置
    #[serde(default)]
    env_vars: Option<String>,
    /// 监听路径（JSON 数组字符串）；省略 = 保存时跟随工作目录
    #[serde(default)]
    watch_paths: Option<String>,
    /// 监听包含 glob（每行一个）；省略 = `*`
    #[serde(default)]
    watch_include: Option<String>,
    /// 监听排除 glob（每行一个）；省略 = 由保存时的默认值处理
    #[serde(default)]
    watch_exclude: Option<String>,
}

#[derive(Deserialize)]
struct LogsArgs {
    /// 服务 id 或名称（在本项目内解析；重名时返回候选而不是猜）
    service: String,
    /// 返回最近多少行（1..=200，默认 80）
    #[serde(default)]
    lines: Option<usize>,
}

impl NexusMcpServer {
    fn new(app: tauri::AppHandle, project: Option<BoundProject>) -> Self {
        Self { app, project }
    }

    fn tools() -> Vec<Tool> {
        vec![
            Tool::new(
                "nexus_commands",
                "列出 Nexus 的全部可调用命令（项目/服务/模板/Node/打开工具/开机启动/布局等）及参数。先调它，再用 nexus_invoke 执行。",
                schema(json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                })),
            ),
            Tool::new(
                "nexus_invoke",
                "执行一条 Nexus 用户命令（先用 nexus_commands 查命令名与参数）。参数键名用 camelCase；删除/卸载等不可逆命令必须在 params 里传 confirm: true。",
                schema(json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "命令名（先用 nexus_commands 查询）" },
                        "params": { "type": "object", "description": "命令参数对象，键名 camelCase" }
                    },
                    "required": ["command"],
                    "additionalProperties": false
                })),
            ),
            Tool::new(
                "list_projects",
                "列出 Nexus 中登记的项目（id、名称、路径）。只读，无需确认。",
                schema(json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                })),
            ),
            Tool::new(
                "list_services",
                "列出当前 AI 会话所属项目下的服务（id、名称、命令、工作目录、是否启用/运行中）。只读，无需确认。",
                schema(json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                })),
            ),
            Tool::new(
                "get_running",
                "查询当前项目各服务的运行状态（运行中 / 启动中 / 已就绪 / 失败、监听端口）。失败时会自动附带最近日志；完整日志用 get_service_logs。只读，无需确认。",
                schema(json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                })),
            ),
            Tool::new(
                "wait_for_ready",
                "等待刚启动的服务结束「启动中」状态（默认最多 20 秒），返回最终运行/失败状态；失败服务的最近日志与识别出的端口冲突（占用进程 PID/名称）会自动附带。启动或重启后应调用它。只读，无需确认。",
                schema(json!({
                    "type": "object",
                    "properties": {
                        "timeout_seconds": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 90,
                            "description": "最长等待秒数，默认 20"
                        }
                    },
                    "additionalProperties": false
                })),
            ),
            Tool::new(
                "get_service_logs",
                "读取当前项目中某个服务最近的日志（已去 ANSI 颜色码；默认 80 行，最多 200 行/64KB）。只读，无需确认。",
                schema(json!({
                    "type": "object",
                    "properties": {
                        "service": {
                            "type": "string",
                            "description": "服务 id 或服务名称（在当前项目内唯一）"
                        },
                        "lines": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 200,
                            "description": "返回最近多少行，默认 80"
                        }
                    },
                    "required": ["service"],
                    "additionalProperties": false
                })),
            ),
            Tool::new(
                "start_service",
                "启动当前项目中的某个服务（直接执行，无需确认）。启动前会检查端口占用：被占用时**不会启动**，直接返回占用进程（PID/进程名/路径）与 port_conflict。启动后应调用 wait_for_ready；失败时自动附带最近日志。",
                service_arg_schema("要启动的服务 id 或名称"),
            ),
            Tool::new(
                "stop_service",
                "停止当前项目中的某个服务（直接执行，无需确认）。",
                service_arg_schema("要停止的服务 id 或名称"),
            ),
            Tool::new(
                "restart_service",
                "重启当前项目中的某个服务（直接执行，无需确认）。重启后应调用 wait_for_ready；失败时自动附带最近日志。",
                service_arg_schema("要重启的服务 id 或名称"),
            ),
            Tool::new(
                "update_service",
                "修改当前项目中某个服务的启动配置（命令、工作目录、环境变量、监听路径、启用状态），直接生效。只修改传入的字段；修改后正在运行的进程不会自动重启，需要调用 restart_service。",
                schema(json!({
                    "type": "object",
                    "properties": {
                        "service": { "type": "string", "description": "服务 id 或名称（在当前项目内唯一）" },
                        "name": { "type": "string", "description": "新的服务名称；省略则保持不变" },
                        "command": { "type": "string", "description": "新的启动命令；省略则保持不变" },
                        "cwd": { "type": "string", "description": "新的工作目录；省略则保持不变" },
                        "env_set": {
                            "type": "object",
                            "additionalProperties": { "type": "string" },
                            "description": "新增或修改的环境变量；只改这里列出的键，其余保持不变"
                        },
                        "env_unset": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "要删除的环境变量名"
                        },
                        "watch_paths": { "type": "string", "description": "监听路径 JSON 数组字符串；省略则保持不变" },
                        "watch_include": { "type": "string", "description": "监听包含 glob；省略则保持不变" },
                        "watch_exclude": { "type": "string", "description": "监听排除 glob（每行一个）；省略则保持不变" },
                        "enabled": { "type": "boolean", "description": "是否启用（影响 start_project 是否会启动它）" }
                    },
                    "required": ["service"],
                    "additionalProperties": false
                })),
            ),
            Tool::new(
                "start_project",
                "启动当前项目的全部已启用服务（已在运行的服务会跳过；直接执行，无需确认）。启动前会逐个检查端口占用，被占用的服务会跳过并在 errors 中写明占用进程。启动后应调用 wait_for_ready；部分失败时会自动附带失败服务的最近日志。",
                schema(json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                })),
            ),
            Tool::new(
                "stop_project",
                "停止当前项目的全部服务（直接执行，无需确认）。",
                schema(json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                })),
            ),
            Tool::new(
                "prepare_add_service",
                "用 AI 收集到的参数打开 Nexus 的「添加服务」编辑面板并预填。**不会直接创建服务**：用户检查后点「保存」才写库。",
                schema(json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "服务名称" },
                        "command": { "type": "string", "description": "启动命令（完整命令行）" },
                        "cwd": { "type": "string", "description": "工作目录；省略时用当前项目根目录" },
                        "env_vars": {
                            "type": "string",
                            "description": "环境变量，dotenv 格式（每行 KEY=VALUE）；省略 = 不配置"
                        },
                        "watch_paths": {
                            "type": "string",
                            "description": "监听路径，JSON 数组字符串；省略 = 跟随工作目录"
                        },
                        "watch_include": { "type": "string", "description": "监听包含 glob；省略 = *" },
                        "watch_exclude": { "type": "string", "description": "监听排除 glob（每行一个）" }
                    },
                    "required": ["name", "command"],
                    "additionalProperties": false
                })),
            ),
        ]
    }

    fn require_project(&self) -> Result<&BoundProject, String> {
        self.project.as_ref().ok_or_else(|| {
            "当前 AI 会话没有绑定项目（启动会话时未带有效工作目录），服务类工具不可用".to_string()
        })
    }

    /// 当前项目的服务列表（含运行态）。
    fn project_services(&self) -> Result<(BoundProject, Vec<crate::models::Service>, HashSet<String>), String> {
        let project = self.require_project()?.clone();
        let state = self.app.state::<AppState>();
        let services = state
            .db
            .with_conn(|conn| crate::database::query_services_by_project(conn, &project.id))?;
        // 用 running()（内部先 tick）而不是 snapshot_running()：后者可能比生命周期
        // 心跳落后最多 2 秒，AI 刚启动/刚崩溃的服务会被误报成相反状态。
        let running: HashSet<String> = state
            .process_mgr
            .running()
            .into_iter()
            .map(|(_, service_id, _)| service_id)
            .collect();
        Ok((project, services, running))
    }

    fn tool_nexus_commands(&self) -> Result<Value, String> {
        Ok(crate::mcp_commands::catalog())
    }

    async fn tool_nexus_invoke(&self, args: Option<JsonObject>) -> Result<Value, String> {
        let args: InvokeArgs = parse_args(args)?;
        // 把绑定项目传进桥里：语义工具（list_services/start_service…）早在
        // `require_project` 收死了范围，通用桥不能成为绕过它的后门。
        let value = crate::mcp_commands::invoke(
            &self.app,
            &args.command,
            &args.params,
            self.project.as_ref().map(|p| p.id.as_str()),
        )
        .await?;
        // 统一包一层对象：MCP 的 structuredContent 必须是 record（对象），而
        // get_services / list_node_versions 这类命令本身返回数组，直接塞进去会被
        // 客户端判为 invalid result（实测：SDK 报 "expected record, received array"）。
        Ok(json!({ "ok": true, "result": value }))
    }

    fn tool_list_projects(&self) -> Result<Value, String> {
        let state = self.app.state::<AppState>();
        let projects = crate::commands::project::get_projects(state)?;
        let items: Vec<Value> = projects
            .into_iter()
            .map(|p| {
                json!({
                    "id": p.id,
                    "name": p.name,
                    "path": p.path,
                    "pinned": p.pinned,
                })
            })
            .collect();
        Ok(json!({ "ok": true, "projects": items }))
    }

    fn tool_list_services(&self) -> Result<Value, String> {
        let (project, services, running) = self.project_services()?;
        let items: Vec<Value> = services
            .into_iter()
            .map(|s| {
                json!({
                    "id": s.id,
                    "name": s.name,
                    "command": s.command,
                    "cwd": s.cwd,
                    "enabled": s.enabled,
                    "running": running.contains(&s.id),
                    "boot_enabled": s.boot_enabled,
                    // 修改配置时需要保留这些；env 只给键名，值可能是密钥，不回传给模型
                    "watch_paths": s.watch_paths,
                    "watch_include": s.watch_include,
                    "watch_exclude": s.watch_exclude,
                    "env_keys": crate::core::process::parse_env_vars(&s.env_vars)
                        .map(|vars| vars.into_iter().map(|(k, _)| k).collect::<Vec<_>>())
                        .unwrap_or_default(),
                })
            })
            .collect();
        Ok(json!({
            "ok": true,
            "project": { "id": project.id, "name": project.name, "path": project.path },
            "services": items,
        }))
    }

    /// 项目运行状态 + 失败服务的自动日志（`get_running` / `wait_for_ready` 共用）。
    async fn running_payload(&self) -> Result<Value, String> {
        let (project, services, _) = self.project_services()?;
        let ids: HashSet<String> = services.iter().map(|s| s.id.clone()).collect();
        let status = crate::commands::process::get_running(self.app.clone()).await?;
        let running: Vec<Value> = status
            .running
            .iter()
            .filter(|r| r.project_id == project.id)
            .map(|r| {
                json!({
                    "service_id": r.service_id,
                    "phase": r.phase,
                    "source": r.source,
                    "listening_ports": r.listening_ports,
                    "followed_log": r.followed_log,
                })
            })
            .collect();
        let failed: Vec<Value> = status
            .failed
            .iter()
            .filter(|f| ids.contains(&f.service_id))
            .map(|f| {
                json!({
                    "service_id": f.service_id,
                    "exit_code": f.exit_code,
                    "timestamp": f.timestamp,
                    "source": f.source,
                    // 日志里识别出的端口冲突（含占用者 PID/进程名/路径）：模型据此
                    // 能直接告诉用户"谁占了哪个端口"，不必再从日志里自己解析
                    "port_conflict": &f.port_conflict,
                })
            })
            .collect();
        // 失败时自动附上最近日志：模型不用再猜"为什么挂了"，也少一轮工具调用。
        // 只附本项目的失败服务，且有严格上限（见 AUTO_LOG_*）。
        let failed_ids: HashSet<String> = status
            .failed
            .iter()
            .filter(|f| ids.contains(&f.service_id))
            .map(|f| f.service_id.clone())
            .collect();
        let failed_services: Vec<&crate::models::Service> =
            services.iter().filter(|s| failed_ids.contains(&s.id)).collect();
        let failed_logs = if failed_services.is_empty() {
            Vec::new()
        } else {
            self.failure_logs(&failed_services).await
        };
        Ok(json!({
            "ok": true,
            "project": project.name,
            "running": running,
            "failed": failed,
            "failed_logs": failed_logs,
            "note": if failed_logs.is_empty() {
                "没有失败服务"
            } else {
                "已自动附带失败服务的最近日志（最多 5 个服务 × 30 行）；完整日志用 get_service_logs"
            },
        }))
    }

    async fn tool_get_running(&self) -> Result<Value, String> {
        self.running_payload().await
    }

    /// 等"启动中"的服务落定（ready 或 failed），并把失败日志一起带回来。
    ///
    /// 为什么需要它：`start_*` 只负责发起，服务可能在几百毫秒后才崩溃；模型若立刻查
    /// 状态会看到"starting"，然后要么误判成功，要么自己乱猜何时再查。这个工具把
    /// "等一会儿"变成一个确定的动作，失败时日志已经在结果里。
    async fn tool_wait_for_ready(&self, args: Option<JsonObject>) -> Result<Value, String> {
        #[derive(Deserialize)]
        struct WaitArgs {
            #[serde(default)]
            timeout_seconds: Option<u64>,
        }
        let args: WaitArgs = parse_args(args)?;
        let timeout = args.timeout_seconds.unwrap_or(20).clamp(1, 90);
        let project = self.require_project()?.clone();
        let start = std::time::Instant::now();
        let deadline = start + std::time::Duration::from_secs(timeout);
        // 最短观察窗：秒退的服务可能在第一次 tick 之前既不在 running、也不在 failed，
        // 此时立刻返回会把"马上要失败"误判成"没有失败"。等 2 秒覆盖这类形态。
        let settle_at = start + std::time::Duration::from_millis(2_000);
        loop {
            let status = crate::commands::process::get_running(self.app.clone()).await?;
            let starting: Vec<String> = status
                .running
                .iter()
                .filter(|r| r.project_id == project.id && r.phase == "starting")
                .map(|r| r.service_id.clone())
                .collect();
            if starting.is_empty() && std::time::Instant::now() >= settle_at {
                let mut payload = self.running_payload().await?;
                let failed_empty = payload
                    .get("failed")
                    .and_then(Value::as_array)
                    .map(|a| a.is_empty())
                    .unwrap_or(true);
                if let Some(obj) = payload.as_object_mut() {
                    obj.insert("ok".into(), json!(failed_empty));
                    obj.insert("timed_out".into(), json!(false));
                }
                return Ok(payload);
            }
            if !starting.is_empty() && std::time::Instant::now() >= deadline {
                let mut payload = self.running_payload().await?;
                if let Some(obj) = payload.as_object_mut() {
                    obj.insert("ok".into(), json!(false));
                    obj.insert("timed_out".into(), json!(true));
                    obj.insert("still_starting".into(), json!(starting));
                    obj.insert("note".into(), json!("等待就绪超时；这些服务仍在启动中"));
                }
                return Ok(payload);
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }

    async fn tool_get_service_logs(&self, args: Option<JsonObject>) -> Result<Value, String> {
        let args: LogsArgs = parse_args(args)?;
        let (project, services, _) = self.project_services()?;
        let wanted = args.service.trim();
        if wanted.is_empty() {
            return Err("service 不能为空".into());
        }
        let matches: Vec<&crate::models::Service> = services
            .iter()
            .filter(|s| s.id == wanted || s.name == wanted)
            .collect();
        let service = match matches.as_slice() {
            [s] => *s,
            [] => {
                return Err(format!(
                    "项目「{}」中找不到服务「{}」",
                    project.name, wanted
                ))
            }
            many => {
                let ids: Vec<String> = many.iter().map(|s| format!("{} ({})", s.name, s.id)).collect();
                return Err(format!(
                    "服务名「{}」不唯一，请改用 id：{}",
                    wanted,
                    ids.join("、")
                ));
            }
        };
        let lines = args.lines.unwrap_or(80).clamp(1, LOG_TOOL_MAX_LINES);
        let logs = crate::commands::process::get_service_logs(self.app.clone(), service.id.clone()).await?;
        Ok(format_logs(service, &logs, lines))
    }

    /// 在本项目内按 id 或名称解析服务；找不到/重名都返回候选，不猜。
    fn resolve_service(&self, wanted: &str) -> Result<crate::models::Service, String> {
        let (project, services, _) = self.project_services()?;
        let wanted = wanted.trim();
        if wanted.is_empty() {
            return Err("service 不能为空".into());
        }
        let matches: Vec<crate::models::Service> = services
            .into_iter()
            .filter(|s| s.id == wanted || s.name == wanted)
            .collect();
        match matches.len() {
            1 => Ok(matches.into_iter().next().expect("长度已确认")),
            0 => Err(format!("项目「{}」中找不到服务「{}」", project.name, wanted)),
            _ => {
                let candidates: Vec<String> = matches
                    .iter()
                    .map(|s| format!("{} ({})", s.name, s.id))
                    .collect();
                Err(format!(
                    "服务名「{}」不唯一，请改用 id：{}",
                    wanted,
                    candidates.join("、")
                ))
            }
        }
    }

    /// 单个失败服务的最近日志（自动附带用，带小上限）。
    async fn failure_log_tail(&self, service: &crate::models::Service) -> Value {
        match crate::commands::process::get_service_logs(self.app.clone(), service.id.clone()).await {
            Ok(logs) => {
                let (lines, truncated) = recent_log_lines(
                    &logs,
                    AUTO_LOG_MAX_LINES,
                    AUTO_LOG_MAX_BYTES,
                    AUTO_LOG_MAX_LINE_CHARS,
                );
                json!({
                    "service_id": service.id,
                    "service_name": service.name,
                    "lines": lines,
                    "truncated": truncated,
                })
            }
            Err(e) => json!({
                "service_id": service.id,
                "service_name": service.name,
                "lines": [],
                "error": format!("读取日志失败: {}", e),
            }),
        }
    }

    /// 批量附带失败服务日志（最多 `AUTO_LOG_MAX_SERVICES` 个）。
    async fn failure_logs(&self, services: &[&crate::models::Service]) -> Vec<Value> {
        let mut out = Vec::new();
        for s in services.iter().take(AUTO_LOG_MAX_SERVICES) {
            out.push(self.failure_log_tail(s).await);
        }
        out
    }

    async fn tool_start_service(&self, args: Option<JsonObject>) -> Result<Value, String> {
        let args: ServiceArgs = parse_args(args)?;
        let service = self.resolve_service(&args.service)?;
        // 与 UI 的启动前预检同一口径：端口被占时**不 spawn**，直接把占用进程交给模型。
        // **绝不自动结束占用进程**——MCP 没有这个能力，也不该有；由用户决定怎么处理。
        match crate::commands::process::check_service_start_conflict(
            self.app.clone(), service.id.clone(),
        ).await {
            Ok(Some(conflict)) => {
                let who = conflict.occupants.iter()
                    .map(|o| format!("{} (PID {})", o.name, o.pid))
                    .collect::<Vec<_>>()
                    .join("、");
                log::warn!(
                    "[mcp] 启动服务 {} ({}) 端口冲突: port={}, occupants={}",
                    service.name, service.id, conflict.port, who
                );
                return Ok(json!({
                    "ok": false,
                    "error": format!("端口 {} 已被 {} 占用", conflict.port, who),
                    "service": { "id": service.id, "name": service.name },
                    "port_conflict": conflict,
                    "note": "端口被占用，未启动。请把占用进程信息告诉用户，由用户决定停止占用进程或修改服务端口；不要反复重试，也不要尝试自行结束进程。",
                }));
            }
            Ok(None) => {}
            Err(e) => {
                // 预检失败不阻断启动（与 UI 同口径）；真正的失败会在 wait_for_ready 里带日志
                log::warn!("[mcp] 启动前端口预检失败（继续启动）: {}", e);
            }
        }
        if let Err(e) = crate::commands::process::start_service(self.app.clone(), service.id.clone()).await {
            let log_tail = self.failure_log_tail(&service).await;
            log::warn!("[mcp] 启动服务 {} ({}) 失败: {}（已附带日志）", service.name, service.id, e);
            return Ok(json!({
                "ok": false,
                "error": e,
                "service": { "id": service.id, "name": service.name },
                "log_tail": log_tail,
                "note": "启动失败，已自动附带该服务最近日志",
            }));
        }
        log::info!("[mcp] 已启动服务 {} ({})", service.name, service.id);
        Ok(json!({
            "ok": true,
            "service": { "id": service.id, "name": service.name },
            "note": "启动已发起；请调用 wait_for_ready 等待就绪；若失败，日志会自动附带",
        }))
    }

    async fn tool_stop_service(&self, args: Option<JsonObject>) -> Result<Value, String> {
        let args: ServiceArgs = parse_args(args)?;
        let service = self.resolve_service(&args.service)?;
        crate::commands::process::stop_service(self.app.clone(), service.id.clone()).await?;
        log::info!("[mcp] 已停止服务 {} ({})", service.name, service.id);
        Ok(json!({
            "ok": true,
            "service": { "id": service.id, "name": service.name },
            "note": "已停止",
        }))
    }

    async fn tool_restart_service(&self, args: Option<JsonObject>) -> Result<Value, String> {
        let args: ServiceArgs = parse_args(args)?;
        let service = self.resolve_service(&args.service)?;
        if let Err(e) = crate::commands::process::restart_service(self.app.clone(), service.id.clone()).await {
            let log_tail = self.failure_log_tail(&service).await;
            log::warn!("[mcp] 重启服务 {} ({}) 失败: {}（已附带日志）", service.name, service.id, e);
            return Ok(json!({
                "ok": false,
                "error": e,
                "service": { "id": service.id, "name": service.name },
                "log_tail": log_tail,
                "note": "重启失败，已自动附带该服务最近日志",
            }));
        }
        log::info!("[mcp] 已重启服务 {} ({})", service.name, service.id);
        Ok(json!({
            "ok": true,
            "service": { "id": service.id, "name": service.name },
            "note": "重启已发起；请调用 wait_for_ready 等待就绪；若失败，日志会自动附带",
        }))
    }

    /// 修改既有服务的启动配置（AI 修复启动失败的主要手段）。
    ///
    /// 只做**增量**：没传的字段保持不变；env 用 `env_set`/`env_unset` 增删，
    /// **不把当前值回传给模型**（里面可能有 token），也避免模型整段覆盖时丢掉未知变量。
    async fn tool_update_service(&self, args: Option<JsonObject>) -> Result<Value, String> {
        let args: UpdateServiceArgs = parse_args(args)?;
        let has_change = args.name.is_some()
            || args.command.is_some()
            || args.cwd.is_some()
            || args.watch_paths.is_some()
            || args.watch_include.is_some()
            || args.watch_exclude.is_some()
            || args.enabled.is_some()
            || args.env_set.as_ref().is_some_and(|m| !m.is_empty())
            || args.env_unset.as_ref().is_some_and(|v| !v.is_empty());
        if !has_change {
            return Err("没有提供要修改的字段".into());
        }
        // 命令 / 工作目录 / 环境变量 = 能把服务换成任意代码执行；改这三类必须由用户
        // 在界面点「允许一次」。名称、监听 glob、启用状态不打断用户。
        let risky_change = args.command.is_some()
            || args.cwd.is_some()
            || args.env_set.as_ref().is_some_and(|m| !m.is_empty())
            || args.env_unset.as_ref().is_some_and(|v| !v.is_empty());

        let current = self.resolve_service(&args.service)?;
        let name = match args.name {
            Some(n) if !n.trim().is_empty() => n.trim().to_string(),
            Some(_) => return Err("name 不能为空".into()),
            None => current.name.clone(),
        };
        let command = match args.command {
            Some(c) if !c.trim().is_empty() => c.trim().to_string(),
            Some(_) => return Err("command 不能为空".into()),
            None => current.command.clone(),
        };
        let cwd = args
            .cwd
            .map(|c| c.replace('\\', "/"))
            .unwrap_or_else(|| current.cwd.clone());
        let watch_paths = args.watch_paths.unwrap_or_else(|| current.watch_paths.clone());
        let watch_include = args.watch_include.unwrap_or_else(|| current.watch_include.clone());
        let watch_exclude = args.watch_exclude.unwrap_or_else(|| current.watch_exclude.clone());
        let enabled = args.enabled.unwrap_or(current.enabled);

        let mut env = crate::core::process::parse_env_vars(&current.env_vars)?;
        if let Some(set) = args.env_set {
            for (k, v) in set {
                validate_env_key(&k)?;
                if let Some(entry) = env.iter_mut().find(|(name, _)| *name == k) {
                    entry.1 = v;
                } else {
                    env.push((k, v));
                }
            }
        }
        if let Some(unset) = args.env_unset {
            for k in unset {
                let k = k.trim();
                if k.is_empty() {
                    return Err("env_unset 里的变量名不能为空".into());
                }
                env.retain(|(name, _)| name != k);
            }
        }
        let env_vars = render_env_vars(&env);
        // 渲染后再解析一次：写进库的形态必须和启动路径能解析的形态一致，
        // 否则会出现"保存成功、下次启动才炸"。
        crate::core::process::parse_env_vars(&env_vars)?;

        if risky_change {
            let detail = format!(
                "修改服务配置\n服务：{}\n命令：{}\n工作目录：{}\n环境变量：{} 项（值不展开）\n\n修改后，下次启动服务将执行上面的命令。",
                name,
                command,
                cwd,
                env.len()
            );
            crate::commands::mcp_confirm::ask(&self.app, "AI 请求修改服务配置", &detail).await?;
        }

        let params = crate::commands::service::UpdateServiceParams {
            id: current.id.clone(),
            name: name.clone(),
            command,
            cwd,
            watch_paths,
            watch_include,
            watch_exclude,
            env_vars,
            restart_mode: current.restart_mode,
            enabled,
            show_file_tree: current.show_file_tree,
            open_enabled: current.open_enabled,
            open_port: current.open_port,
            tool_commands: current.tool_commands.clone(),
            boot_enabled: current.boot_enabled,
            boot_order: current.boot_order,
            boot_check: current.boot_check.clone(),
            boot_required: current.boot_required,
            boot_credential_mode: current.boot_credential_mode.clone(),
        };
        let result = crate::commands::service::update_service(self.app.clone(), params).await?;
        // 界面同步：AI 改了配置，服务卡片/编辑面板的数据必须跟着刷新
        crate::mcp_commands::emit_changed(
            &self.app,
            "services",
            Some(current.project_id.clone()),
            vec![current.id.clone()],
        );
        log::info!("[mcp] 已更新服务配置 {} ({})", current.name, current.id);
        Ok(json!({
            "ok": true,
            "service": { "id": current.id, "name": name },
            "watch_refreshed": result.watch_refreshed,
            "watch_error": result.watch_error,
            "boot_manifest_error": result.boot_manifest_error,
            "note": "配置已保存；若服务正在运行，调用 restart_service 让新配置生效",
        }))
    }

    async fn tool_start_project(&self) -> Result<Value, String> {
        let (project, services, _) = self.project_services()?;
        if !services.iter().any(|s| s.enabled) {
            return Err(format!("项目「{}」没有已启用的服务", project.name));
        }
        let errors =
            crate::commands::process::start_project_services(self.app.clone(), project.id.clone()).await?;
        // 失败时自动附带日志，覆盖两种失败形态：
        // - spawn 失败：不会进 running/failed 表，只能从 errors 的服务名前缀定位；
        // - 启动后立刻崩溃：此时可能在 get_running 的 failed 表里。
        let failed_logs = if errors.is_empty() {
            Vec::new()
        } else {
            let failed_ids: HashSet<String> = crate::commands::process::get_running(self.app.clone())
                .await
                .map(|st| {
                    st.failed
                        .iter()
                        .filter(|f| services.iter().any(|s| s.id == f.service_id))
                        .map(|f| f.service_id.clone())
                        .collect()
                })
                .unwrap_or_default();
            let targets: Vec<&crate::models::Service> = services
                .iter()
                .filter(|s| {
                    failed_ids.contains(&s.id)
                        || errors.iter().any(|e| e.starts_with(&format!("{}:", s.name)))
                })
                .collect();
            self.failure_logs(&targets).await
        };
        let ok = errors.is_empty() && failed_logs.is_empty();
        log::info!(
            "[mcp] 已发起项目启动 {}（失败 {} 项，附带日志 {} 个服务）",
            project.name,
            errors.len(),
            failed_logs.len()
        );
        Ok(json!({
            "ok": ok,
            "project": { "id": project.id, "name": project.name },
            "errors": errors,
            "failed_logs": failed_logs,
            "note": if ok {
                "启动已发起；请调用 wait_for_ready 等待就绪"
            } else {
                "启动已发起；失败项见 errors，已自动附带失败服务的最近日志（最多 5 个服务 × 30 行）；请调用 wait_for_ready 确认最终状态"
            },
        }))
    }

    async fn tool_stop_project(&self) -> Result<Value, String> {
        let (project, services, _) = self.project_services()?;
        if services.is_empty() {
            return Err(format!("项目「{}」没有服务可停止", project.name));
        }
        let errors =
            crate::commands::process::stop_project_services(self.app.clone(), project.id.clone()).await?;
        log::info!("[mcp] 已停止项目 {}（失败 {} 项）", project.name, errors.len());
        Ok(json!({
            "ok": errors.is_empty(),
            "project": { "id": project.id, "name": project.name },
            "errors": errors,
            "note": "已停止；失败项见 errors",
        }))
    }

    /// 只打开预填面板，不写库。路径与 env 用与真实保存同源的校验先挡一遍，
    /// 让模型能拿到可修正的错误，而不是等用户点保存时才失败。
    async fn tool_prepare_add_service(&self, args: Option<JsonObject>) -> Result<Value, String> {
        let args: PrepareAddServiceArgs = parse_args(args)?;
        let project = self.require_project()?.clone();
        let name = args.name.trim().to_string();
        let command = args.command.trim().to_string();
        if name.is_empty() {
            return Err("name 不能为空".into());
        }
        if command.is_empty() {
            return Err("command 不能为空".into());
        }
        let cwd = match args.cwd.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(c) => c.replace('\\', "/"),
            None => project.path.clone(),
        };
        let watch_paths = args.watch_paths.unwrap_or_default();
        let watch_include = args
            .watch_include
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "*".to_string());
        let watch_exclude = args.watch_exclude.unwrap_or_default();
        let env_vars = args.env_vars.unwrap_or_default();

        {
            let state = self.app.state::<AppState>();
            crate::commands::service::validate_config_dirs(
                &state.db,
                &state.paths,
                &cwd,
                &watch_paths,
            )?;
        }
        // 语法错误在打开面板前就返回：面板里再报一次，用户还要自己找哪行错了
        crate::core::process::parse_env_vars(&env_vars)?;

        self.app
            .emit(
                "mcp-open-service-draft",
                ServiceDraftPayload {
                    project_id: project.id.clone(),
                    name: name.clone(),
                    command,
                    cwd,
                    watch_paths,
                    watch_include,
                    watch_exclude,
                    env_vars,
                },
            )
            .map_err(|e| format!("打开服务编辑面板失败: {}", e))?;
        log::info!("[mcp] 已打开「添加服务」预填面板（{}）", name);
        Ok(json!({
            "ok": true,
            "opened": true,
            "note": "已打开预填的「添加服务」面板，等待用户检查并点「保存」；服务尚未创建",
        }))
    }
}

impl ServerHandler for NexusMcpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("nexus", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Nexus 工具服务：常用操作有语义工具（查询/启停/改配置/恢复循环）。\
                 其余功能（项目、服务模板、Node 版本、打开工具、开机启动、布局等）\
                 先用 nexus_commands 查命令目录，再用 nexus_invoke 执行；\
                 语义工具默认只作用于当前会话项目，nexus_invoke 可通过参数操作其它项目。\
                 删除/卸载等不可逆命令必须在 params 里传 confirm: true。\
                 启动或重启后必须调用 wait_for_ready 等待服务落定；失败时结果会自动附带最近日志。\
                 先根据日志定位原因：代码或依赖问题用你自己的文件/命令工具修复；\
                 服务配置问题优先用语义 update_service（增量修改），只在需要改监听路径/\
                 开机启动等语义工具未覆盖的字段时用 nexus_invoke 的 update_service（需要完整字段）。\
                 修改后调用 restart_service 重试；最多重试 3 次，无法解决就如实报告，不要无限循环。\
                 添加服务可直接用 nexus_invoke 调 add_service，也可用 prepare_add_service 打开预填面板。\
                 run_tool_command 可能长时间运行；若超时，命令可能仍在后台跑，不要立即重跑，先查日志。",
            )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(Self::tools()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        Self::tools().into_iter().find(|t| t.name.as_ref() == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let name = request.name.to_string();
        let started = std::time::Instant::now();
        let result = match name.as_str() {
            "nexus_commands" => self.tool_nexus_commands(),
            "nexus_invoke" => self.tool_nexus_invoke(request.arguments).await,
            "list_projects" => self.tool_list_projects(),
            "list_services" => self.tool_list_services(),
            "get_running" => self.tool_get_running().await,
            "wait_for_ready" => self.tool_wait_for_ready(request.arguments).await,
            "get_service_logs" => self.tool_get_service_logs(request.arguments).await,
            "start_service" => self.tool_start_service(request.arguments).await,
            "stop_service" => self.tool_stop_service(request.arguments).await,
            "restart_service" => self.tool_restart_service(request.arguments).await,
            "update_service" => self.tool_update_service(request.arguments).await,
            "start_project" => self.tool_start_project().await,
            "stop_project" => self.tool_stop_project().await,
            "prepare_add_service" => self.tool_prepare_add_service(request.arguments).await,
            _ => return Err(McpError::method_not_found::<CallToolRequestMethod>()),
        };
        log::info!(
            "[mcp] {} 完成（{}ms）{}",
            name,
            started.elapsed().as_millis(),
            match &result {
                Ok(_) => String::new(),
                Err(e) => format!("，失败: {}", e),
            }
        );
        match result {
            // `ok:false`（例如项目批量启动里有服务失败）也要让客户端把它标成错误：
            // 只放在 text 里的话，模型容易读成"完成了"。
            Ok(value) => {
                // MCP 要求 structuredContent 是对象；任何工具误返回数组/标量时兜底包一层
                let value = if value.is_object() {
                    value
                } else {
                    json!({ "result": value })
                };
                let failed = value.get("ok").and_then(Value::as_bool) == Some(false);
                Ok(if failed {
                    CallToolResult::structured_error(value)
                } else {
                    CallToolResult::structured(value)
                }
                .into())
            }
            // 工具级失败：模型必须能读到原因（协议错误会被客户端渲染成"内部错误"）
            Err(e) => Ok(CallToolResult::structured_error(json!({
                "ok": false,
                "error": e,
            }))
            .into()),
        }
    }
}

// ── 辅助 ──────────────────────────────────────────────────────

/// 请求是否带着本次会话的 Bearer token。
///
/// 端点在回环地址上，本机其它进程也可能连到；token 每次会话随机，只写进 dsh 的
/// 会话级 patch 文件。任何缺失/格式不符/值不匹配都一律拒绝，不暴露服务信息。
fn bearer_matches(headers: &axum::http::HeaderMap, expected: &str) -> bool {
    let Some(token) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return false;
    };
    // 常量时间比较：回环上的时序攻击不现实（token 是每会话 64 hex 随机串），
    // 但比较凭据没有理由在第一个不同的字节就返回。长度不等直接失败不泄漏内容。
    let a = token.as_bytes();
    let b = expected.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

fn schema(value: Value) -> JsonObject {
    value.as_object().cloned().unwrap_or_default()
}

/// 单服务写工具的公共参数 schema。
fn service_arg_schema(description: &str) -> JsonObject {
    schema(json!({
        "type": "object",
        "properties": {
            "service": { "type": "string", "description": description }
        },
        "required": ["service"],
        "additionalProperties": false
    }))
}

/// 环境变量名校验：空、含空白/等号/控制字符都会让 dotenv 解析歧义。
fn validate_env_key(key: &str) -> Result<(), String> {
    if key.is_empty()
        || key.contains('=')
        || key.chars().any(|c| c.is_whitespace() || c == '\0')
    {
        return Err(format!(
            "环境变量名不合法（不能为空、不能含空白或等号）: {:?}",
            key
        ));
    }
    Ok(())
}

/// 把环境变量序列化回配置字符串。
///
/// 默认 dotenv（`KEY=VALUE` 每行，界面可读）；值里含换行时改用解析器同样支持的
/// JSON 对象格式，避免换行把一行拆成两行、下次解析直接报错。
fn render_env_vars(env: &[(String, String)]) -> String {
    if env.iter().any(|(_, v)| v.contains('\n') || v.contains('\r')) {
        let obj: serde_json::Map<String, Value> = env
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        return Value::Object(obj).to_string();
    }
    env.iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_args<T: serde::de::DeserializeOwned>(args: Option<JsonObject>) -> Result<T, String> {
    serde_json::from_value(Value::Object(args.unwrap_or_default()))
        .map_err(|e| format!("参数不合法: {}", e))
}

/// 日志快照 → 模型可读文本（手动 `get_service_logs`）。
fn format_logs(
    service: &crate::models::Service,
    logs: &[std::sync::Arc<crate::core::logpipe::LogLine>],
    max_lines: usize,
) -> Value {
    let total = logs.len();
    let (lines, truncated) =
        recent_log_lines(logs, max_lines, LOG_TOOL_MAX_BYTES, LOG_TOOL_MAX_LINE_CHARS);
    json!({
        "ok": true,
        "service": { "id": service.id, "name": service.name },
        "lines": lines,
        "total_lines": total,
        "truncated": truncated,
    })
}

/// 取最近日志（从**最新**往前累计，保证截掉的是最老的行；单行过长再按字符截）。
///
/// 返回 `(lines, truncated)`；`truncated` 同时覆盖"丢了老行"和"单行被截断"两种形态。
fn recent_log_lines(
    logs: &[std::sync::Arc<crate::core::logpipe::LogLine>],
    max_lines: usize,
    max_bytes: usize,
    max_line_chars: usize,
) -> (Vec<String>, bool) {
    let total = logs.len();
    let window_start = total.saturating_sub(max_lines);
    let mut kept_rev: Vec<String> = Vec::new();
    let mut bytes = 0usize;
    let mut line_clipped = false;
    for line in logs[window_start..].iter().rev() {
        let text = strip_ansi(&line.text);
        let text = if text.chars().count() > max_line_chars {
            line_clipped = true;
            let clipped: String = text.chars().take(max_line_chars).collect();
            format!("{}…[本行已截断]", clipped)
        } else {
            text
        };
        let rendered = format!("[{}] {} {}", line.timestamp, line.stream, text);
        if bytes + rendered.len() > max_bytes && !kept_rev.is_empty() {
            break;
        }
        bytes += rendered.len() + 1;
        kept_rev.push(rendered);
    }
    kept_rev.reverse();
    let truncated = window_start > 0 || kept_rev.len() < total - window_start || line_clipped;
    (kept_rev, truncated)
}

/// 去掉 ANSI 转义序列（CSI 与常见双字符序列；OSC 也按终止符跳过）。
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('[') => {
                chars.next();
                // CSI：参数字节 + 终止字节 0x40..=0x7E
                for c2 in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c2) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                // OSC：以 BEL 或 ESC \ 结束
                while let Some(c2) = chars.next() {
                    if c2 == '\u{07}' {
                        break;
                    }
                    if c2 == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            // 双字符转义（ESC + 一个字符）：第一个字符本身丢弃
            Some(other) if !other.is_ascii_alphabetic() && other != '(' && other != ')' => {
                chars.next();
            }
            Some(_) => {}
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 端点唯一的安全闸：缺失 / 值不对 / scheme 大小写不符都必须拒绝。
    #[test]
    fn bearer_guard_rejects_missing_or_wrong_tokens() {
        let mut headers = axum::http::HeaderMap::new();
        assert!(!bearer_matches(&headers, "secret"), "没有 Authorization 头");

        headers.insert(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("Bearer other"),
        );
        assert!(!bearer_matches(&headers, "secret"), "token 不匹配");

        headers.insert(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("bearer secret"),
        );
        assert!(!bearer_matches(&headers, "secret"), "scheme 大小写不符");

        headers.insert(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("Bearer secret"),
        );
        assert!(bearer_matches(&headers, "secret"));
    }

    #[test]
    fn normalize_path_handles_mixed_separators_and_case() {
        assert_eq!(normalize_path("C:/Work/Demo/"), "c:\\work\\demo");
        assert_eq!(normalize_path("C:\\Work\\Demo"), "c:\\work\\demo");
        assert_eq!(normalize_path("C:\\"), "c:\\");
    }

    #[test]
    fn strip_ansi_removes_csi_and_keeps_text() {
        assert_eq!(strip_ansi("\u{1b}[32mOK\u{1b}[0m done"), "OK done");
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("\u{1b}]0;title\u{07}text"), "text");
    }

    #[test]
    fn format_logs_prefers_newest_and_marks_truncation() {
        let service = crate::models::Service {
            id: "s1".into(),
            project_id: "p1".into(),
            name: "web".into(),
            command: "npm run dev".into(),
            cwd: "C:\\p".into(),
            watch_paths: "[]".into(),
            watch_include: "*".into(),
            watch_exclude: "".into(),
            env_vars: "".into(),
            restart_mode: 0,
            enabled: true,
            show_file_tree: false,
            open_enabled: false,
            open_port: 0,
            sort_index: 0,
            tool_commands: "[]".into(),
            boot_enabled: false,
            boot_order: 0,
            boot_check: "".into(),
            boot_required: false,
            boot_credential_mode: "s4u".into(),
        };
        let logs: Vec<std::sync::Arc<crate::core::logpipe::LogLine>> = (0..5)
            .map(|i| {
                std::sync::Arc::new(crate::core::logpipe::LogLine {
                    seq: i,
                    timestamp: std::sync::Arc::from("t"),
                    stream: "stdout".into(),
                    text: format!("line-{}", i),
                })
            })
            .collect();
        let value = format_logs(&service, &logs, 3);
        let lines = value["lines"].as_array().expect("lines 是数组");
        assert_eq!(lines.len(), 3);
        assert!(lines[0].as_str().unwrap().contains("line-2"));
        assert!(lines[2].as_str().unwrap().contains("line-4"));
        assert_eq!(value["truncated"], true);
    }

    #[test]
    fn env_helpers_round_trip_and_validate() {
        assert!(validate_env_key("PORT").is_ok());
        assert!(validate_env_key("A B").is_err());
        assert!(validate_env_key("A=B").is_err());
        assert!(validate_env_key("").is_err());

        let env = vec![
            ("PORT".to_string(), "3000".to_string()),
            ("NAME".to_string(), "a b".to_string()),
        ];
        let parsed = crate::core::process::parse_env_vars(&render_env_vars(&env)).unwrap();
        assert_eq!(parsed, env);

        // 值里含换行 → 退回 JSON 对象格式，仍能被解析器读回
        let multiline = vec![("TEXT".to_string(), "a\nb".to_string())];
        let rendered = render_env_vars(&multiline);
        assert!(rendered.starts_with('{'), "含换行的值应序列化为 JSON: {}", rendered);
        assert_eq!(
            crate::core::process::parse_env_vars(&rendered).unwrap(),
            multiline
        );
    }

    #[test]
    fn recent_log_lines_respects_line_and_byte_limits() {
        let logs: Vec<std::sync::Arc<crate::core::logpipe::LogLine>> = (0..10)
            .map(|i| {
                std::sync::Arc::new(crate::core::logpipe::LogLine {
                    seq: i,
                    timestamp: std::sync::Arc::from("t"),
                    stream: "stdout".into(),
                    text: format!("line-{}", i),
                })
            })
            .collect();

        let (lines, truncated) = recent_log_lines(&logs, 3, 10_000, 100);
        assert_eq!(lines.len(), 3);
        assert!(lines[2].contains("line-9"), "保留的应是最近的行");
        assert!(truncated);

        // 字节上限先于行数命中：只会留下最新的一行
        let (lines, truncated) = recent_log_lines(&logs, 10, 20, 100);
        assert!(!lines.is_empty() && lines.len() < 10);
        assert!(truncated);

        // 单行过长：按字符截断，并且 truncated 必须为 true
        let long = vec![std::sync::Arc::new(crate::core::logpipe::LogLine {
            seq: 0,
            timestamp: std::sync::Arc::from("t"),
            stream: "stdout".into(),
            text: "x".repeat(1_000),
        })];
        let (lines, truncated) = recent_log_lines(&long, 10, 1_000_000, 10);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("本行已截断"));
        assert!(truncated);
    }

    /// Bearer 校验只认**完全匹配**的 token：前缀/长度不同/缺头都必须失败
    #[test]
    fn test_bearer_matches_requires_exact_token() {
        use axum::http::header::AUTHORIZATION;
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(AUTHORIZATION, "Bearer abc123".parse().expect("构造 header"));
        assert!(bearer_matches(&headers, "abc123"));
        assert!(
            !bearer_matches(&headers, "abc124"),
            "最后一个字节不同必须拒绝"
        );
        assert!(!bearer_matches(&headers, "abc1234"), "长度不同必须拒绝");
        assert!(!bearer_matches(&headers, ""));
        assert!(
            !bearer_matches(&axum::http::HeaderMap::new(), "abc123"),
            "缺 Authorization 必须拒绝"
        );
    }
}
