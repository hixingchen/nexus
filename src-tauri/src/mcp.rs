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

use std::collections::{HashMap, HashSet};
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

/// 「AI 修改后重启」单次最多接受的文件数。
///
/// 文件列表只用于匹配服务；超过这个量基本是模型把整棵目录树塞进来了。
/// 直接拒绝比静默截断安全——静默截断会让部分改动不触发重启，且模型不知道。
const RESTART_AFTER_EDIT_MAX_FILES: usize = 1_000;
/// 单个服务返回给模型的命中文件样例上限（详细匹配结果不需要全量塞进上下文）。
const RESTART_AFTER_EDIT_SAMPLE_FILES: usize = 20;

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
    /// AI 按服务判断后下发（代码服务 3，中间件/基础设施 0）；用户在面板里可改
    pub restart_mode: i32,
    /// 是否允许在浏览器中打开；AI 按服务判断，默认关
    pub open_enabled: bool,
    /// 浏览器打开端口；0 = 自动
    pub open_port: i32,
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

/// 一个服务的监听配置命中了哪些改动文件（只做路径匹配，不看运行态）。
///
/// 与文件监听共用 `ServiceWatchConfig::matches_path`：前缀、include、exclude 的
/// 语义必须完全一致，否则「监听会触发的改动」与「AI 修改后重启的改动」会分叉。
fn affected_files_for_watch_config(
    watch_paths: &str,
    watch_include: &str,
    watch_exclude: &str,
    changed: &[PathBuf],
) -> Result<Vec<String>, String> {
    let paths: Vec<String> = serde_json::from_str(watch_paths)
        .map_err(|e| format!("监听路径配置格式错误: {}", e))?;
    if paths.is_empty() { return Ok(Vec::new()); }
    let include: Vec<String> = watch_include.lines()
        .map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    let exclude: Vec<String> = watch_exclude.lines()
        .map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    let cfg = crate::core::file_watcher::ServiceWatchConfig::new(
        String::new(), String::new(), paths, include, exclude, 3,
    );
    Ok(changed.iter()
        .filter(|p| cfg.matches_path(p))
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect())
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
    #[serde(default)]
    show_file_tree: Option<bool>,
    #[serde(default)]
    restart_mode: Option<i32>,
    #[serde(default)]
    open_enabled: Option<bool>,
    #[serde(default)]
    open_port: Option<i32>,
    #[serde(default)]
    tool_commands: Option<String>,
    #[serde(default)]
    crash_restart: Option<bool>,
    #[serde(default)]
    health_check: Option<String>,
    #[serde(default)]
    boot_enabled: Option<bool>,
    #[serde(default)]
    boot_order: Option<i32>,
    #[serde(default)]
    boot_check: Option<String>,
    #[serde(default)]
    boot_required: Option<bool>,
    #[serde(default)]
    boot_credential_mode: Option<String>,
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
    /// 重启模式；**必填**——AI 判断：代码服务 3，中间件/基础设施 0
    #[serde(default, alias = "restartMode")]
    restart_mode: Option<i32>,
    /// 是否允许在浏览器中打开；只有浏览器可访问的前端/门户才开
    #[serde(default, alias = "openEnabled")]
    open_enabled: Option<bool>,
    /// 浏览器打开端口；0 = 自动
    #[serde(default, alias = "openPort")]
    open_port: Option<i32>,
}

#[derive(Deserialize)]
struct RestartAfterEditArgs {
    /// 本轮修改/新增/删除的文件绝对路径（也接受相对项目根目录的写法）
    #[serde(alias = "changedFiles")]
    changed_files: Vec<String>,
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
                "列出 Nexus 中登记的项目（id、名称、路径、收藏、排序）。只读，无需确认。新建/改名/改路径/复制/删除项目用 nexus_invoke 的项目命令：path 必须是已存在的目录；改项目路径不会自动改服务的 cwd/watch_paths；删除只删 Nexus 记录、不删文件。",
                schema(json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                })),
            ),
            Tool::new(
                "list_services",
                "列出当前 AI 会话所属项目下的服务及配置摘要（id、名称、命令、目录、监听路径、重启/浏览器/开机/健康检查、工具命令 id/名字等；环境变量只给键名，工具命令不含正文）。只读，无需确认。启动/管理前先查：缺失的服务应先通过 Nexus 添加，不要用终端直接跑。",
                schema(json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                })),
            ),
            Tool::new(
                "get_running",
                "查询当前项目各服务的运行状态（运行中 / 启动中 / 已就绪 / 失败、监听端口）。失败时会自动附带最近日志；完整日志用 get_service_logs。只读，无需确认。启动/重启前先查，已在运行的不重复启动。",
                schema(json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                })),
            ),
            Tool::new(
                "wait_for_ready",
                "等待刚启动的服务结束「启动中」状态（默认最多 20 秒），返回最终运行/失败状态；失败服务的最近日志与识别出的端口冲突（占用进程 PID/名称）会自动附带。启动或重启后必须调用它，并检查 running/failed/failed_logs——进程活着不等于业务可用。只读，无需确认。",
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
                "读取当前项目中某个服务最近的日志（已去 ANSI 颜色码；默认 80 行，最多 200 行/64KB）。只读，无需确认。启动后用它检查连接拒绝/端口/找不到依赖等错误。",
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
                "启动当前项目中的某个服务（直接执行，无需确认）。启动前会检查端口占用：被占用时**不会启动**，直接返回占用进程（PID/进程名/路径）与 port_conflict。启动成功后会自动挂上该服务的文件监听（只对 restart_mode 为 1/2 的服务生效；watch_error 表示监听启动失败）。启动后应调用 wait_for_ready 并读日志；失败时自动附带最近日志。若目标服务不在列表，先通过 Nexus 添加，不要用终端直接跑。若用户要启动整个项目，改用 start_project；响应里的 other_enabled_not_running 是项目内其他已启用但未运行的服务。",
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
                "restart_services_after_edit",
                "代码修改完成后调用：传入本轮修改过的文件绝对路径。Nexus 会找出当前项目中开启了「AI 修改后重启」（restart_mode=3）、正在运行、且监听路径命中这些文件的服务，并按顺序逐个重启。只用于 AI 自己改完代码后的收尾；人工改代码不要调用。仍在启动/构建中的服务会被跳过（避免打断构建），可稍后再次调用；重启后如需确认状态调用 wait_for_ready。",
                schema(json!({
                    "type": "object",
                    "properties": {
                        "changed_files": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 1,
                            "description": "本轮修改/新增/删除的文件绝对路径（正斜杠或反斜杠均可；也接受相对项目根目录的路径）"
                        }
                    },
                    "required": ["changed_files"],
                    "additionalProperties": false
                })),
            ),
            Tool::new(
                "update_service",
                "修改当前项目中某个服务的启动配置（命令、工作目录、环境变量、监听路径、启用/展示、监听/重启模式、浏览器打开、工具命令、崩溃自动重启、就绪检查、开机启动），直接生效。只修改传入的字段；修改后正在运行的进程不会自动重启，需要调用 restart_service。修改命令/工作目录/环境变量/工具命令/开机启动会弹用户确认。",
                schema(json!({
                    "type": "object",
                    "properties": {
                        "service": { "type": "string", "description": "服务 id 或名称（在当前项目内唯一）" },
                        "name": { "type": "string", "description": "服务名称（展示/日志用）；省略则保持不变" },
                        "command": { "type": "string", "description": "启动命令（完整命令行，在工作目录执行；需要构建就写构建+启动）；省略则保持不变。修改会弹确认" },
                        "cwd": { "type": "string", "description": "服务工作目录（启动命令、构建产物、相对监听路径都以它为基准）；省略则保持不变。修改会弹确认" },
                        "env_set": {
                            "type": "object",
                            "additionalProperties": { "type": "string" },
                            "description": "新增或修改的环境变量；只改这里列出的键，其余保持不变。不要把密钥回显到回复/日志"
                        },
                        "env_unset": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "要删除的环境变量名"
                        },
                        "watch_paths": { "type": "string", "description": "监听/匹配路径 JSON 数组字符串。1/2 模式用于文件监听；3 模式用于匹配 AI 改动。建议只放源码目录（如 src），避免构建产物触发重启；省略则保持不变" },
                        "watch_include": { "type": "string", "description": "包含 glob（每行一个，按文件名匹配；默认 *）；省略则保持不变" },
                        "watch_exclude": { "type": "string", "description": "排除规则（每行一个；字面目录名匹配任意层级，*.ext 按文件名匹配；不支持 ** 复杂 glob）；省略则保持不变" },
                        "enabled": { "type": "boolean", "description": "是否跟随 start_project 一起启动；false 仍可单独 start；省略则保持不变" },
                        "show_file_tree": { "type": "boolean", "description": "是否在项目列表显示该服务的目录树；仅源码型服务需要；省略则保持不变" },
                        "restart_mode": {
                            "type": "integer",
                            "minimum": 0,
                            "maximum": 3,
                            "description": "监听/重启模式：0=关闭监听，1=确认后重启，2=自动重启，3=仅 AI 修改后重启（不挂文件监听）；省略则保持不变"
                        },
                        "open_enabled": { "type": "boolean", "description": "是否允许在浏览器中打开；只有浏览器可访问的前端/门户才开，后端 API/中间件保持关闭；省略则保持不变" },
                        "open_port": { "type": "integer", "minimum": 0, "maximum": 65535, "description": "浏览器打开端口；0=自动；非 0 时项目内唯一；省略则保持不变" },
                        "tool_commands": { "type": "string", "description": "工具命令 JSON 数组字符串，每项 {id,name,command,timeout_secs?}（timeout_secs 省略=60，0=不限）。例：[{\"id\":\"build\",\"name\":\"全量构建\",\"command\":\"mvn ...\",\"timeout_secs\":0}]。用于构建/清理/安装等，不放启动命令；省略则保持不变。修改会弹确认" },
                        "crash_restart": { "type": "boolean", "description": "稳定运行后崩溃是否自动重启；默认开；一次性/调试服务可关；省略则保持不变" },
                        "health_check": { "type": "string", "description": "本地就绪检查 JSON。例：{\"type\":\"tcp\",\"host\":\"127.0.0.1\",\"port\":8080,\"timeoutSecs\":60} 或 {\"type\":\"logContains\",\"pattern\":\"Started\",\"timeoutSecs\":120}；配置后 start_project 会等它就绪，自动重启观察窗也用它；空串=进程存活即就绪；省略则保持不变" },
                        "boot_enabled": { "type": "boolean", "description": "登录后由开机启动器拉起；只给需要常驻、不依赖 Nexus 打开的服务；省略则保持不变。修改会弹确认" },
                        "boot_order": { "type": "integer", "description": "开机启动顺序（小的先起）；仅 boot_enabled 时有效；省略则保持不变" },
                        "boot_check": { "type": "string", "description": "开机启动就绪检查 JSON，形状同 health_check；空串=无检查；省略则保持不变" },
                        "boot_required": { "type": "boolean", "description": "开机检查失败时是否阻断后续服务启动；省略则保持不变" },
                        "boot_credential_mode": { "type": "string", "description": "s4u 或 password；password 会保存凭据，仅在 s4u 不可用时使用；省略则保持不变" }
                    },
                    "required": ["service"],
                    "additionalProperties": false
                })),
            ),
            Tool::new(
                "start_project",
                "启动当前项目的全部已启用服务（已在运行的服务会跳过；直接执行，无需确认）。按 sort_index 顺序启动；配了 health_check 会等它就绪再启动下一个。启动前逐个检查端口占用，被占用的服务会跳过并在 errors 中写明占用进程。启动后自动挂项目级文件监听（仅 1/2 模式，3 模式不挂；watch_error 表示监听启动失败）。启动后必须调用 wait_for_ready 并检查 failed/failed_logs；部分失败时会自动附带失败服务的最近日志。not_enabled 里的服务未启用、不会随项目启动，需要时先 update_service 打开 enabled 或单独 start_service。若项目应有服务不在列表，先通过 Nexus 添加，不要用终端直接跑。",
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
                "用 AI 收集到的参数打开 Nexus 的「添加服务」编辑面板并预填。**不会直接创建服务**：用户检查后点「保存」才写库。用户要求启动但服务不存在、且配置需要用户检查/修改时用它；要立即创建并启动用 nexus_invoke 的 add_service。必须按服务类型判断 restart_mode 和 open_enabled，不要一律打开；open_enabled 省略 = 关闭，前端/门户才传 true。其余属性（enabled/show_file_tree/tool_commands/crash_restart/health_check/boot_*）可在面板里设置，或创建后用 update_service 配置。",
                schema(json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "服务名称" },
                        "command": { "type": "string", "description": "启动命令（完整命令行）" },
                        "cwd": { "type": "string", "description": "工作目录；省略时用当前项目根目录" },
                        "env_vars": {
                            "type": "string",
                            "description": "环境变量，dotenv 格式（每行 KEY=VALUE）；省略 = 不配置。不要把密钥回显到回复/日志"
                        },
                        "watch_paths": {
                            "type": "string",
                            "description": "监听/匹配路径 JSON 数组；1/2 模式文件监听、3 模式匹配 AI 改动；建议只放源码目录（如 src）；省略 = 跟随工作目录"
                        },
                        "watch_include": { "type": "string", "description": "包含 glob（每行一个，按文件名匹配）；省略 = *" },
                        "watch_exclude": { "type": "string", "description": "排除规则（每行一个；字面目录名匹配任意层级，*.ext 按文件名；不支持 ** 复杂 glob）" },
                        "restart_mode": {
                            "type": "integer",
                            "minimum": 0,
                            "maximum": 3,
                            "description": "必填。按服务判断：代码会被 AI 修改、改完需要重启的服务传 3（仅 AI 修改后重启）；中间件/基础设施/不需要重启的传 0"
                        },
                        "open_enabled": {
                            "type": "boolean",
                            "description": "是否允许在浏览器中打开。只有浏览器可访问的前端/门户页面才传 true；后端 API、中间件、构建/脚本服务保持 false（默认 false）"
                        },
                        "open_port": {
                            "type": "integer",
                            "minimum": 0,
                            "maximum": 65535,
                            "description": "浏览器打开端口；0 = 自动，非 0 时项目内唯一。只有 open_enabled=true 时才有意义"
                        }
                    },
                    "required": ["name", "command", "restart_mode"],
                    "additionalProperties": false
                })),
            ),
            Tool::new(
                "delete_service",
                "删除当前项目中的某个服务（先停进程；只删 Nexus 记录，不删文件）。删除前会弹用户确认。只用于用户明确要求删除，或你为临时测试添加的服务在测试完成后清理；不要删除用户已有的非临时服务。",
                service_arg_schema("要删除的服务 id 或名称"),
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
                    "sort_index": p.sort_index,
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
                // 工具命令只回 id + 名字：运行需要 id；命令正文可能含密钥/环境参数，不回传。
                let tool_commands: Vec<Value> =
                    serde_json::from_str::<Vec<crate::models::ToolCommand>>(&s.tool_commands)
                        .map(|cmds| cmds.into_iter()
                            .map(|c| json!({ "id": c.id, "name": c.name }))
                            .collect())
                        .unwrap_or_default();
                json!({
                    "id": s.id,
                    "name": s.name,
                    "command": s.command,
                    "cwd": s.cwd,
                    "enabled": s.enabled,
                    "running": running.contains(&s.id),
                    "sort_index": s.sort_index,
                    "show_file_tree": s.show_file_tree,
                    // 重启策略：0=关闭监听，1=确认重启，2=自动重启，3=仅 AI 修改后重启。
                    // AI 需要据此判断哪些服务会在 restart_services_after_edit 里被重启。
                    "restart_mode": s.restart_mode,
                    // 浏览器打开设置：AI 新建服务时据此参考现有同类服务的做法
                    "open_enabled": s.open_enabled,
                    "open_port": s.open_port,
                    "crash_restart": s.crash_restart,
                    "health_check": s.health_check,
                    "boot_enabled": s.boot_enabled,
                    "boot_order": s.boot_order,
                    "boot_check": s.boot_check,
                    "boot_required": s.boot_required,
                    "boot_credential_mode": s.boot_credential_mode,
                    // id + 名字，供 run_tool_command / update_service 选择；不含命令正文
                    "tool_commands": tool_commands,
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
        let empty = items.is_empty();
        Ok(json!({
            "ok": true,
            "project": { "id": project.id, "name": project.name, "path": project.path },
            "services": items,
            "note": if empty {
                "项目没有登记服务。用户要求启动项目时，应先通过 Nexus 添加服务（add_service 或 prepare_add_service），不要用终端直接跑"
            } else {
                ""
            },
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
                    "health_note": r.health_note,
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
        let service = match self.resolve_service(&args.service) {
            Ok(s) => s,
            Err(e) if e.contains("找不到服务") => {
                return Err(format!(
                    "{e}。如果这是项目应有但未登记的服务，请先用 Nexus 添加（add_service 或 prepare_add_service），不要用终端直接跑"
                ))
            }
            Err(e) => return Err(e),
        };
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
        // 与 UI 的 startSingleService 同口径：启动成功后挂上该服务的文件监听
        // （restart_mode 为 1/2 且配置了监听路径才真正生效）。监听失败不回滚启动。
        let watch_error = match crate::commands::watcher::start_watching(
            self.app.clone(), service.project_id.clone(), Some(service.id.clone()),
        ).await {
            Ok(()) => None,
            Err(e) => {
                log::warn!("[mcp] 启动服务 {} 后挂文件监听失败: {}", service.name, e);
                Some(e)
            }
        };
        // 单服务启动最容易漏掉"项目里其他已启用服务还没起"：直接回给模型，
        // 让它自己判断用户是不是要启动整个项目，而不是只报告这一个服务成功。
        let other_enabled_not_running: Vec<String> = self.project_services()
            .map(|(_, all, running_now)| all.into_iter()
                .filter(|s| s.enabled && s.id != service.id && !running_now.contains(&s.id))
                .map(|s| s.name)
                .collect())
            .unwrap_or_default();
        log::info!(
            "[mcp] 已启动服务 {} ({})（项目内其他已启用未运行 {} 个）",
            service.name, service.id, other_enabled_not_running.len()
        );
        let note = if other_enabled_not_running.is_empty() {
            "启动已发起；请调用 wait_for_ready 等待就绪，并读日志确认没有依赖连接错误"
        } else {
            "启动已发起；项目还有已启用服务未运行（other_enabled_not_running）。如果用户要启动整个项目，改用 start_project 或一并启动这些服务，不要只起一个；请调用 wait_for_ready 并检查日志"
        };
        Ok(json!({
            "ok": true,
            "service": { "id": service.id, "name": service.name },
            "watch_error": watch_error,
            "other_enabled_not_running": other_enabled_not_running,
            "note": note,
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

    /// 删除服务：先停进程、删 Nexus 记录（不删文件）。
    ///
    /// 语义工具里保留一个独立入口（而不是只靠通用桥）：测试服务清理是高频动作，
    /// 删除前必须由用户确认；通用桥的 `confirm: true` 只是模型写的意图声明，不是人点的。
    async fn tool_delete_service(&self, args: Option<JsonObject>) -> Result<Value, String> {
        let args: ServiceArgs = parse_args(args)?;
        let service = self.resolve_service(&args.service)?;
        let detail = format!(
            "删除服务\n服务：{}\n命令：{}\n工作目录：{}\n\n会先停止它的运行实例，然后删除 Nexus 记录（不删除文件）。此操作不可逆。",
            service.name, service.command, service.cwd
        );
        crate::commands::mcp_confirm::ask(&self.app, "AI 请求删除服务", &detail).await?;
        let result = crate::commands::service::delete_service(self.app.clone(), service.id.clone()).await?;
        // 界面同步：服务卡片/编辑面板必须立即消失
        crate::mcp_commands::emit_changed(
            &self.app,
            "services",
            Some(service.project_id.clone()),
            vec![service.id.clone()],
        );
        log::info!("[mcp] 已删除服务 {} ({})", service.name, service.id);
        Ok(json!({
            "ok": true,
            "service": { "id": service.id, "name": service.name },
            "stop_warnings": result.stop_warnings,
            "note": "服务已从 Nexus 删除（文件未删除）；stop_warnings 非空时进程可能仍在运行，需要现场处理",
        }))
    }

    async fn tool_restart_service(&self, args: Option<JsonObject>) -> Result<Value, String> {
        let args: ServiceArgs = parse_args(args)?;
        let service = match self.resolve_service(&args.service) {
            Ok(s) => s,
            Err(e) if e.contains("找不到服务") => {
                return Err(format!(
                    "{e}。如果这是项目应有但未登记的服务，请先用 Nexus 添加（add_service 或 prepare_add_service），不要用终端直接跑"
                ))
            }
            Err(e) => return Err(e),
        };
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

    /// 「AI 修改后重启」：把改动文件映射到 mode=3 的服务并逐个重启。
    ///
    /// 只重启**正在运行**的服务；启动中/构建中的跳过，避免打断构建。匹配规则与
    /// 文件监听共用 `ServiceWatchConfig::matches_path`，不在 MCP 侧另写一套。
    async fn tool_restart_services_after_edit(&self, args: Option<JsonObject>) -> Result<Value, String> {
        let args: RestartAfterEditArgs = parse_args(args)?;
        let project = self.require_project()?.clone();
        if args.changed_files.is_empty() {
            return Ok(json!({
                "ok": true,
                "restarted": [],
                "skipped": [],
                "failed": [],
                "note": "没有传入改动文件，未重启任何服务",
            }));
        }
        if args.changed_files.len() > RESTART_AFTER_EDIT_MAX_FILES {
            return Err(format!(
                "changed_files 最多 {} 个（本次 {} 个）；请只传本轮实际修改的文件",
                RESTART_AFTER_EDIT_MAX_FILES,
                args.changed_files.len()
            ));
        }

        // 绝对路径原样使用；相对路径按项目根目录解析。去重按统一分隔符的字符串。
        let mut changed: Vec<PathBuf> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for raw in &args.changed_files {
            let trimmed = raw.trim();
            if trimmed.is_empty() { continue; }
            let path = PathBuf::from(trimmed);
            let path = if path.is_absolute() {
                path
            } else {
                Path::new(&project.path).join(path)
            };
            let key = path.to_string_lossy().replace('\\', "/");
            if seen.insert(key) {
                changed.push(path);
            }
        }
        if changed.is_empty() {
            return Ok(json!({
                "ok": true,
                "restarted": [],
                "skipped": [],
                "failed": [],
                "note": "changed_files 里没有有效路径，未重启任何服务",
            }));
        }

        let (_, services, _) = self.project_services()?;
        let status = crate::commands::process::get_running(self.app.clone()).await?;
        // starting 的服务可能正在构建；重启会杀掉构建，跳过等它先落定。
        let phases: HashMap<String, String> = status
            .running
            .iter()
            .filter(|r| r.project_id == project.id)
            .map(|r| (r.service_id.clone(), r.phase.clone()))
            .collect();

        let mut restarted = Vec::new();
        let mut skipped = Vec::new();
        let mut failed = Vec::new();

        for svc in services.into_iter().filter(|s| s.enabled && s.restart_mode == 3) {
            let matched = match affected_files_for_watch_config(
                &svc.watch_paths, &svc.watch_include, &svc.watch_exclude, &changed,
            ) {
                Ok(v) => v,
                Err(e) => {
                    skipped.push(json!({
                        "id": svc.id, "name": svc.name,
                        "reason": e,
                    }));
                    continue;
                }
            };
            if matched.is_empty() { continue; }
            let sample: Vec<String> = matched.iter()
                .take(RESTART_AFTER_EDIT_SAMPLE_FILES).cloned().collect();

            match phases.get(&svc.id).map(String::as_str) {
                None => {
                    skipped.push(json!({
                        "id": svc.id, "name": svc.name,
                        "reason": "未运行（本工具只重启正在运行的服务；若刚修复了它的启动失败，请调用 restart_service）",
                        "matched_count": matched.len(),
                        "matched_files": sample,
                    }));
                    continue;
                }
                Some("starting") => {
                    skipped.push(json!({
                        "id": svc.id, "name": svc.name,
                        "reason": "仍在启动/构建中，已跳过以避免打断构建；可稍后再次调用",
                        "matched_count": matched.len(),
                        "matched_files": sample,
                    }));
                    continue;
                }
                Some(_) => {}
            }

            match crate::commands::process::restart_service(self.app.clone(), svc.id.clone()).await {
                Ok(()) => {
                    log::info!(
                        "[mcp] AI 修改后重启服务 {} ({})，命中 {} 个文件（样例 {}）",
                        svc.name, svc.id, matched.len(),
                        matched.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
                    );
                    restarted.push(json!({
                        "id": svc.id, "name": svc.name,
                        "matched_count": matched.len(),
                        "matched_files": sample,
                    }));
                }
                Err(e) => {
                    log::warn!("[mcp] AI 修改后重启服务 {} ({}) 失败: {}", svc.name, svc.id, e);
                    failed.push(json!({
                        "id": svc.id, "name": svc.name,
                        "error": e,
                        "matched_count": matched.len(),
                    }));
                }
            }
        }

        let note = if !failed.is_empty() {
            "部分服务重启失败，详见 failed；可查看日志后修复并调用 restart_service 重试"
        } else if !restarted.is_empty() {
            "重启已发起；构建可能需要时间，需要确认时调用 wait_for_ready"
        } else if !skipped.is_empty() {
            "命中的服务都没有实际重启；详见 skipped"
        } else {
            "传入的文件没有命中任何开启「AI 修改后重启」的服务（检查服务的监听路径）"
        };
        Ok(json!({
            "ok": failed.is_empty(),
            "project": { "id": project.id, "name": project.name },
            "changed_files": changed.len(),
            "restarted": restarted,
            "skipped": skipped,
            "failed": failed,
            "note": note,
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
            || args.show_file_tree.is_some()
            || args.restart_mode.is_some()
            || args.open_enabled.is_some()
            || args.open_port.is_some()
            || args.tool_commands.is_some()
            || args.crash_restart.is_some()
            || args.health_check.is_some()
            || args.boot_enabled.is_some()
            || args.boot_order.is_some()
            || args.boot_check.is_some()
            || args.boot_required.is_some()
            || args.boot_credential_mode.is_some()
            || args.env_set.as_ref().is_some_and(|m| !m.is_empty())
            || args.env_unset.as_ref().is_some_and(|v| !v.is_empty());
        if !has_change {
            return Err("没有提供要修改的字段".into());
        }
        // 命令 / 工作目录 / 环境变量 / 工具命令 = 能把服务换成任意代码执行；
        // 开机启动配置会改变登录后的持久行为。这几类必须由用户在界面点「允许一次」。
        // 名称、监听 glob、展示开关、浏览器打开、健康检查不打断用户。
        let boot_change = args.boot_enabled.is_some()
            || args.boot_order.is_some()
            || args.boot_check.is_some()
            || args.boot_required.is_some()
            || args.boot_credential_mode.is_some();
        let risky_change = args.command.is_some()
            || args.cwd.is_some()
            || args.tool_commands.is_some()
            || boot_change
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
        let show_file_tree = args.show_file_tree.unwrap_or(current.show_file_tree);
        let restart_mode = match args.restart_mode {
            Some(v) if (0..=3).contains(&v) => v,
            Some(_) => return Err("restart_mode 只能是 0（关闭监听）、1（确认重启）、2（自动重启）或 3（仅 AI 修改后重启）".into()),
            None => current.restart_mode,
        };
        let open_enabled = args.open_enabled.unwrap_or(current.open_enabled);
        let open_port = args.open_port.unwrap_or(current.open_port);
        if !(0..=65535).contains(&open_port) {
            return Err("open_port 必须在 0-65535 之间（0 = 自动）".into());
        }
        let crash_restart = args.crash_restart.unwrap_or(current.crash_restart);
        let health_check = args.health_check.unwrap_or_else(|| current.health_check.clone());
        let tool_commands_changed = args.tool_commands.is_some();
        let tool_commands = args.tool_commands.unwrap_or_else(|| current.tool_commands.clone());
        // 工具命令是持久化的任意命令：合法性在弹确认框前就拦下，别让用户批完才报错
        crate::commands::service::validate_tool_commands(&tool_commands)?;
        let boot_enabled = args.boot_enabled.unwrap_or(current.boot_enabled);
        let boot_order = args.boot_order.unwrap_or(current.boot_order);
        let boot_check = args.boot_check.unwrap_or_else(|| current.boot_check.clone());
        let boot_required = args.boot_required.unwrap_or(current.boot_required);
        let boot_credential_mode = args
            .boot_credential_mode
            .unwrap_or_else(|| current.boot_credential_mode.clone());
        if boot_change {
            crate::core::boot::normalize_check(&boot_check)?;
            crate::core::boot::BootMode::parse(if boot_credential_mode.trim().is_empty() {
                "s4u"
            } else {
                &boot_credential_mode
            })?;
        }

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
            let mut detail = format!(
                "修改服务配置\n服务：{}\n命令：{}\n工作目录：{}\n环境变量：{} 项（值不展开）",
                name,
                command,
                cwd,
                env.len()
            );
            if tool_commands_changed {
                let shown: String = tool_commands.chars().take(1_200).collect();
                detail.push_str(&format!("\n\n工具命令将变为：{}", shown));
            }
            if boot_change {
                let check: String = boot_check.chars().take(300).collect();
                detail.push_str(&format!(
                    "\n\n开机启动配置：enabled={}，order={}，check={}，required={}，credential={}",
                    boot_enabled, boot_order, check, boot_required, boot_credential_mode
                ));
            }
            detail.push_str("\n\n修改后，下次启动服务将执行上面的命令。");
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
            restart_mode,
            enabled,
            show_file_tree,
            open_enabled,
            open_port,
            tool_commands,
            boot_enabled,
            boot_order,
            boot_check,
            boot_required,
            boot_credential_mode,
            health_check: Some(health_check),
            // 省略时显式带原值：全量更新不能把用户关掉的崩溃自动重启又打开
            crash_restart: Some(crash_restart),
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
        // 未启用的服务不会随 start_project 启动：必须显式回给模型，
        // 否则它会以为"项目起来了"，而依赖服务其实还停着。
        let not_enabled: Vec<String> = services.iter()
            .filter(|s| !s.enabled)
            .map(|s| s.name.clone())
            .collect();
        if !services.iter().any(|s| s.enabled) {
            return Err(format!(
                "项目「{}」没有已启用的服务。若项目应有服务但未登记/未启用，先用 Nexus 添加或启用\
                 （add_service / prepare_add_service / update_service），不要用终端直接跑",
                project.name
            ));
        }
        let errors =
            crate::commands::process::start_project_services(self.app.clone(), project.id.clone()).await?;
        // 与 UI 的 startProjectServices 同口径：项目启动后启动项目级文件监听。
        // 启动本身已发起；监听失败只通过 watch_error 返回，不回滚。
        let watch_error = match crate::commands::watcher::start_watching(
            self.app.clone(), project.id.clone(), None,
        ).await {
            Ok(()) => None,
            Err(e) => {
                log::warn!("[mcp] 启动项目 {} 后挂文件监听失败: {}", project.name, e);
                Some(e)
            }
        };
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
        let note = if !ok {
            "启动已发起；失败项见 errors，已自动附带失败服务的最近日志（最多 5 个服务 × 30 行）；请调用 wait_for_ready 确认最终状态"
        } else if not_enabled.is_empty() {
            "启动已发起；请调用 wait_for_ready 等待就绪，并检查 running/failed/failed_logs"
        } else {
            "启动已发起；未启用的服务（not_enabled）不会随项目启动。若项目需要它们，先用 update_service 打开 enabled 或单独 start_service；然后调用 wait_for_ready 并检查日志"
        };
        Ok(json!({
            "ok": ok,
            "project": { "id": project.id, "name": project.name },
            "errors": errors,
            "failed_logs": failed_logs,
            "not_enabled": not_enabled,
            "watch_error": watch_error,
            "note": note,
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
        // AI 必须逐个判断，不能无脑给所有服务设 3：
        // 代码会被修改且需要重启的服务用 3；中间件/基础设施/不需要重启的用 0。
        let restart_mode = match args.restart_mode {
            Some(v) if (0..=3).contains(&v) => v,
            Some(_) => return Err("restart_mode 只能是 0/1/2/3".into()),
            None => return Err(
                "必须判断该服务是否需要在 AI 修改代码后重启：需要传 3（仅 AI 修改后重启），\
                 中间件/基础设施/不需要重启的传 0"
                    .into(),
            ),
        };
        // 浏览器打开同样按服务判断：只有前端/门户才开，后端 API/中间件保持关闭。
        let open_enabled = args.open_enabled.unwrap_or(false);
        let open_port = args.open_port.unwrap_or(0);
        if !(0..=65535).contains(&open_port) {
            return Err("open_port 必须在 0-65535 之间（0 = 自动）".into());
        }

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
                    restart_mode,
                    open_enabled,
                    open_port,
                },
            )
            .map_err(|e| format!("打开服务编辑面板失败: {}", e))?;
        log::info!(
            "[mcp] 已打开「添加服务」预填面板（{}，restart_mode={}，open_enabled={}，open_port={}）",
            name, restart_mode, open_enabled, open_port
        );
        Ok(json!({
            "ok": true,
            "opened": true,
            "restart_mode": restart_mode,
            "open_enabled": open_enabled,
            "open_port": open_port,
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
                 其余功能（服务模板、Node 版本、打开工具、开机启动、布局等）\
                 先用 nexus_commands 查命令目录，再用 nexus_invoke 执行；\
                 项目/服务类命令都只能操作当前会话项目（要操作其它项目，请先在 Nexus 切换项目并重开 AI 面板）；\
                 可以只读列出全部项目，但写操作不能跨项目。\
                 通过 nexus_invoke 执行的删除/卸载等不可逆命令必须在 params 里传 confirm: true；\
                 语义 delete_service 自带用户确认，不需要 confirm。\
                 启动/管理优先走 Nexus，不要用终端直接跑服务：\
                 用户说“启动项目/服务”时，先 list_services + get_running 查现状；已在运行的不重复启动。\
                 目标服务不在服务列表：不要直接在 shell 起进程（Nexus 看不到、停不掉、没有日志/端口/重启管理）。\
                 先通过 Nexus 添加：用户要现在跑 → nexus_invoke add_service（会弹确认，需完整字段）；\
                 配置需要用户检查/修改 → prepare_add_service；启动命令/工作目录不确定时用 prepare_add_service 让用户确认或先问用户，\
                 不要猜一个命令直接跑。创建后 start_service/start_project，并 wait_for_ready。\
                 服务已存在但 enabled=false：直接 start_service，或 update_service 打开 enabled；不要重复创建。\
                 新增服务名在当前项目内保持唯一（重名会让按名称解析失败）；先查 list_services 避免重复登记。\
                 中间件端口已被外部进程占用：先报告占用进程，不要重复启动或擅自结束外部进程；是否交给 Nexus 管理由用户决定。\
                 一次性构建/测试命令用终端或 run_tool_command；长期运行的服务必须建成 Nexus 服务。\
                 为测试/诊断临时添加的服务：名字加「临时-」前缀；测试完成后（无论成功失败）用 delete_service 删除\
                 （会弹用户确认；只删 Nexus 记录，不删文件），并在回复里说明新增/删除了哪些服务；\
                 用户明确要保留的除外。不要删除用户已有服务。\
                 启动流程：用户说“启动项目”→ 用 start_project（全部已启用），不要逐个 start_service；\
                 用户说“启动某个服务”→ 先 list_services，若项目还有其他已启用服务未运行且该服务依赖它们\
                 （中间件/注册中心/网关等），先一并启动或启动项目；不确定就问用户。\
                 start_project 的 not_enabled / start_service 的 other_enabled_not_running 是提示：这些服务没起；\
                 该起就起，不要只报告一个服务成功。\
                 启动/重启后必须 wait_for_ready，并检查 running/failed/failed_logs；进程活着不等于业务可用；\
                 日志里出现连接拒绝/端口/找不到依赖时，通过 Nexus 启动对应服务（缺失先 add_service）再重启本服务，\
                 不要用 shell 起依赖。汇报时说明：启动了哪些、哪些未启用/跳过/失败/未就绪，以及日志里的关键错误。\
                 项目启动顺序：给中间件/依赖服务配 health_check（TCP/日志）并用 sort_index 排在前面，\
                 start_project 会等它们就绪再起后面的服务；没有 health_check 时只按顺序 spawn，不保证依赖已就绪。\
                 代码修改完成后（本轮结束前），如果改动过任何文件，必须调用 \
                 restart_services_after_edit 并传入本轮修改文件的绝对路径；没有改文件就不要调用。\
                 人工改代码不需要你调用它。\
                 新建服务时必须按服务类型判断两个开关，不要一律打开：\
                 ① 代码会被你修改、改完需要重启的服务 restart_mode 传 3（仅 AI 修改后重启）；\
                 中间件/基础设施/不需要重启的传 0。\
                 ② 只有浏览器可访问的前端/门户才开 open_enabled 并给 open_port；\
                 后端 API、中间件、构建/脚本服务保持关闭（false/0）；省略 open_enabled 按关闭处理。\
                 服务属性选择：command 写完整构建+启动；cwd 用服务模块目录；\
                 watch_paths 只放源码（1/2 模式监听、3 模式匹配 AI 改动）；\
                 watch_include/exclude 按文件名/目录名，别写 ** 复杂 glob；\
                 env 用 env_set/env_unset，且不要把密钥回显到回复/日志；\
                 enabled 控制是否跟随 start_project；show_file_tree 只给源码型服务；\
                 health_check 给有明确就绪标志（端口/日志关键字）的服务；crash_restart 默认开；\
                 tool_commands 放构建/清理/安装，不放启动命令；boot_* 只给需要登录自启的服务。\
                 项目管理：path 用已存在的工作区根；改路径不会自动改服务 cwd/watch_paths；\
                 duplicate 会复制服务与打开工具绑定，delete 只删 Nexus 记录不删文件。\
                 先根据日志定位原因：代码或依赖问题用你自己的文件/命令工具修复；\
                 服务配置问题优先用语义 update_service（已覆盖命令/监听/模式/工具命令/开机启动等，增量修改）；\
                 只有改服务顺序（reorder_services）或绑定打开工具（set_service_open_tool）等语义工具未覆盖的操作才用 nexus_invoke。\
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
            "delete_service" => self.tool_delete_service(request.arguments).await,
            "restart_service" => self.tool_restart_service(request.arguments).await,
            "restart_services_after_edit" => self.tool_restart_services_after_edit(request.arguments).await,
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

    /// `restart_services_after_edit` 的匹配必须与文件监听同语义（前缀 + include/exclude）。
    #[test]
    fn affected_files_follows_watch_rules() {
        let changed = vec![
            PathBuf::from("C:/p/server/src/a.ts"),
            PathBuf::from("C:/p/server/src/b.log"),
            PathBuf::from("C:/p/shared/c.ts"),
            PathBuf::from("C:/p/server/node_modules/pkg/d.js"),
        ];
        let got = affected_files_for_watch_config(
            r#"["C:/p/server"]"#, "*", "*.log\nnode_modules", &changed,
        ).expect("配置合法");
        assert_eq!(got, vec!["C:/p/server/src/a.ts".to_string()], "只收监听路径内且未被排除的文件");

        let include_only = affected_files_for_watch_config(
            r#"["C:/p/server"]"#, "*.ts", "", &changed,
        ).expect("配置合法");
        assert_eq!(include_only, vec!["C:/p/server/src/a.ts".to_string()], "include 同样生效");

        assert!(affected_files_for_watch_config("{不是数组}", "*", "", &changed).is_err());
        assert!(affected_files_for_watch_config("[]", "*", "", &changed).unwrap().is_empty());
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
            health_check: "".into(),
            crash_restart: true,
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
