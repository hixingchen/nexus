mod commands;
mod core;
mod database;
pub mod logger;
mod models;

/// IPC 字段命名契约测试（响应 snake_case / 请求 camelCase），仅测试构建参与编译
#[cfg(test)]
mod contract;

/// 内部命令收口的守卫测试（源码扫描，GUARD-1），仅测试构建参与编译
#[cfg(test)]
mod spawn_guard;

use std::sync::Arc;
use tauri::Manager;

use crate::core::ai::AiSession;
use crate::core::file_watcher::FileWatcher;
use crate::core::process::ProcessManager;
use crate::database::Database;

pub struct AppState {
    pub db: Database,
    pub process_mgr: ProcessManager,
    pub file_watcher: FileWatcher,
    /// 文件访问白名单：状态（项目根 / 已确认目录 / 解析缓存）与规则同住一处。
    ///
    /// 为什么收成一个字段：这三项此前是三个独立字段，而 39 个命令都能拿到 `AppState`——
    /// "哪些代码能改白名单"在类型上给不出答案，只能 grep 字段名。见 `commands::paths`。
    pub paths: crate::commands::paths::PathAllowlist,
    /// 搜索代数：每次发起内容搜索递增，正在跑的旧搜索据此尽早退出
    /// （Arc 是为了能把它克隆进 spawn_blocking 的闭包）
    pub search_epoch: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// 内嵌 dsh web（AI 助手 iframe 会话）；None = 未运行
    pub ai: std::sync::Mutex<Option<AiSession>>,
    /// 会话代数：每次 start/stop 递增，启动中的进程据此识别「被取代」并自杀
    pub ai_epoch: std::sync::atomic::AtomicU64,
}

/// 统一的资源清理逻辑
///
/// 幂等设计：多次调用安全（stop_all 对空集合是 no-op）。
/// 清理顺序：服务进程 → AI 会话 → 文件监听
///
/// 调用点：`commands::app::prepare_exit`（独立线程，用户确认关闭后）与
/// `RunEvent::Exit`（兜底）。**不要**再放回 `WindowEvent::CloseRequested`——
/// 那里现在只是"请求关闭"，用户可能取消（未保存草稿确认），且同步清理会冻结窗口。
pub(crate) fn cleanup_resources(state: &AppState) {
    let start = std::time::Instant::now();

    // 1. 停止所有服务进程（最高优先级，含 taskkill /T /F + wait）
    state.process_mgr.stop_all();
    log::info!("[nexus] 清理: 服务进程已停止 ({:.0}ms)", start.elapsed().as_millis());

    // 2. 停止 AI 会话（dsh web；Job Object 兜底应用崩溃场景）
    if let Ok(mut h) = state.ai.lock() {
        if let Some(mut s) = h.take() {
            log::info!("[nexus] 清理: 停止 dsh web (pid={})", s.pid);
            s.stop();
        }
    }

    // 3. 停止文件监听
    state.file_watcher.stop_all();

    log::info!("[nexus] 清理完成 (总耗时 {:.0}ms)", start.elapsed().as_millis());
}

/// WebView2 cookie 存储清理（Windows，必须在 WebView 创建前调用）。
///
/// 背景：dsh 每次会话签发新 cookie（名带随机串 `dsh-auth-<rand>`，Max-Age 30 天），
/// localhost 域 cookie 只增不减 → 请求头膨胀 → dsh 服务器对插件 bundle 请求返回
/// 431 (Request Header Fields Too Large) → 「failed to load plugins」永久错误。
/// Nexus 自身不依赖任何 cookie（本地应用），每次启动清空无副作用。
#[cfg(windows)]
fn purge_webview_cookies() {
    let Some(local) = std::env::var_os("LOCALAPPDATA") else { return };
    let dir = std::path::PathBuf::from(local)
        .join("com.nexus.app")
        .join("EBWebView")
        .join("Default")
        .join("Network");
    for name in ["Cookies", "Cookies-journal", "Cookies-wal", "Cookies-shm"] {
        let f = dir.join(name);
        if f.exists() {
            match std::fs::remove_file(&f) {
                Ok(_) => log::info!("[nexus] 已清理 WebView2 cookie 存储: {}", name),
                Err(e) => log::debug!("[nexus] 清理 cookie 失败（webview 可能运行中）: {}", e),
            }
        }
    }
}

/// Nexus 自己的数据目录（数据库 + 日志）。
///
/// `NEXUS_DATA_DIR` 可覆盖它，用途是**跑一个与真实数据完全隔离的实例**——
/// 端到端验证（"真机跑一次"）不能拿用户的真实库做实验，而 `dirs::home_dir()` 在
/// Windows 上走 Known Folder API、**不认 `USERPROFILE`**，没有这个开关就没法隔离。
/// 未设置时行为与之前完全一致（`~/.nexus`）。
///
/// 注意：**只影响 Nexus 自己的数据**。白名单里的"用户目录"（`commands/editor.rs`）
/// 与 CFR 找 jar 的位置（`core/decompiler.rs`）必须仍指向真实用户目录——那两处跟着
/// 数据目录走会让被隔离的实例看不到真实的用户主目录。
pub(crate) fn data_dir() -> std::path::PathBuf {
    match std::env::var_os("NEXUS_DATA_DIR") {
        Some(d) if !d.is_empty() => std::path::PathBuf::from(d),
        _ => dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".nexus"),
    }
}

/// 启动致命错误落盘（`<数据目录>/logs/startup-error.log`）后退出。
///
/// 打包版没有控制台，启动期 panic 的表现是"双击后闪退、没有任何线索"；
/// 落盘 + 明确退出码让用户/支持者至少能拿到原因。
fn fatal_startup_error(msg: String) -> ! {
    log::error!("[nexus] 启动失败: {}", msg);
    let dir = data_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("startup-error.log"), format!("{}\n", msg));
    eprintln!("[nexus] 启动失败: {}", msg);
    std::process::exit(1);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // WebView 创建前清空 cookie 库（见 purge_webview_cookies 注释）
    #[cfg(windows)]
    purge_webview_cookies();

    let db = match Database::try_new() {
        Ok(db) => db,
        Err(e) => fatal_startup_error(format!("数据库初始化失败: {}", e)),
    };

    // 创建共享的 Job Object（通过 Arc 在 ProcessManager 间共享）
    // 失败时降级运行：子进程不会在父进程退出时自动终止，但应用仍可正常使用
    #[cfg(windows)]
    let job = match crate::core::job_object::JobObject::new() {
        Ok(j) => Some(Arc::new(j)),
        Err(e) => {
            log::error!("[nexus] ⚠ Job Object 创建失败，子进程自动清理已禁用，关闭应用后可能残留后台进程: {}", e);
            None
        }
    };

    // 初始化服务进程管理器并设置 Job Object
    let mut process_mgr = ProcessManager::new();
    #[cfg(windows)]
    {
        if let Some(ref job) = job {
            process_mgr.set_job(job.clone());
            // 反编译 JVM 等"不经过 ProcessManager"的子进程也要纳入同一 Job（见 core::job_object::shared）
            crate::core::job_object::set_shared(job.clone());
        }
    }

    let builder = tauri::Builder::default();
    // 单实例（仅打包版/release 注册）：重复启动（双击 .exe / 再次运行）不开第二个窗口——
    // 第二进程在插件 setup 阶段被拦截退出，第一实例收到回调把窗口唤起到前台。
    // 回调在第一实例主线程执行，window 操作安全；无需往第二进程传参（无协议关联）。
    // debug（tauri dev）不注册：开发阶段不做单实例限制，也不会与已运行的打包版互相拦截。
    #[cfg(not(debug_assertions))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
    }));

    let app = builder
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        // 剪贴板不走 tauri-plugin-clipboard-manager：文件列表粘贴只能由 Rust 侧
        // clipboard-win 写 CF_HDROP（见 commands/fileops.rs），插件只提供文本/图片 API，
        // 注册它等于多开一条用不到的 IPC 命令面（含 allow-write-text 等权限位）
        .manage(AppState {
            db,
            process_mgr,
            file_watcher: FileWatcher::new(),
            paths: crate::commands::paths::PathAllowlist::new(),
            ai: std::sync::Mutex::new(None),
            ai_epoch: std::sync::atomic::AtomicU64::new(0),
            search_epoch: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        })
        .invoke_handler(tauri::generate_handler![
            commands::editor::read_file,
            commands::editor::write_file,
            commands::search::search_files,
            commands::editor::list_directory,
            commands::editor::open_in_explorer,
            commands::editor::read_class_file,
            commands::editor::decompile_class,
            commands::editor::list_jar,
            commands::editor::read_jar_entry,
            commands::editor::read_image_data,
            commands::editor::read_hex_page,
            commands::editor::open_terminal,
            commands::fileops::copy_files_to_clipboard,
            commands::fileops::paste_files,
            commands::service::get_services,
            commands::service::add_service,
            commands::service::update_service,
            commands::service::delete_service,
            commands::service::reorder_services,
            commands::service::get_service_templates,
            commands::service::save_service_as_template,
            commands::service::add_service_from_template,
            commands::service::update_service_template,
            commands::service::create_service_template,
            commands::service::delete_service_template,
            commands::service::reorder_service_templates,
            commands::service::export_service_templates,
            commands::service::import_service_templates,
            commands::project::get_projects,
            commands::project::get_project_detail,
            commands::project::add_project,
            commands::project::update_project,
            commands::project::delete_project,
            commands::project::duplicate_project,
            commands::project::toggle_pin_project,
            commands::process::start_service,
            commands::process::stop_service,
            commands::process::restart_service,
            commands::process::start_project_services,
            commands::process::stop_project_services,
            commands::process::get_running,
            commands::process::get_service_logs,
            commands::process::run_tool_command,
            commands::process::stop_tool_command,
            commands::watcher::start_watching,
            commands::watcher::stop_watching,
            commands::tools::list_open_tools,
            commands::tools::save_open_tool,
            commands::tools::delete_open_tool,
            commands::tools::set_service_open_tool,
            commands::tools::list_service_open_tool_bindings,
            commands::tools::open_service_with_tool,
            commands::layout::save_layout,
            commands::layout::load_layout,
            commands::editor::set_project_root,
            commands::ai::ai_status,
            commands::ai::ai_start,
            commands::ai::ai_stop,
            commands::ai::ai_check_update,
            commands::ai::ai_upgrade_dsh,
            commands::ai::create_ai_panel_webview,
            commands::ai::ai_panel_focus,
            commands::app::prepare_exit,
            commands::app::pick_directory,
            commands::app::get_app_version,
            // Node 运行时（独立工具，不参与服务配置）
            commands::node::get_node_runtime,
            commands::node::list_available_node_versions,
            commands::node::install_node_version,
            commands::node::uninstall_node_version,
            commands::node::use_node_version,
        ])
        .on_window_event(move |_window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                // 这里**不做**清理：前端注册了 tauri://close-requested 监听后，Tauri 会自动
                // prevent_close 并把决定权交给前端（未保存草稿确认，见 src/components/layout/CloseGuard.tsx）。
                // 用户取消时窗口继续存在——此时停掉所有服务是错的。
                // 真正的清理由 commands::app::prepare_exit 在独立线程执行（不冻结窗口），
                // RunEvent::Exit 仍是最后兜底（幂等）。
                log::info!("[nexus] CloseRequested → 交前端确认（未保存草稿）");
            }
        })
        .build(tauri::generate_context!());

    let app = match app {
        Ok(app) => app,
        Err(e) => fatal_startup_error(format!("构建 Nexus 应用失败: {}", e)),
    };

    // Exit 是最后的安全网：确保所有资源被释放，子进程被终止
    // cleanup_resources 是幂等的（对空集合 no-op），多次调用安全
    app.run(move |app, event| {
        if let tauri::RunEvent::Exit = event {
            log::info!("[nexus] RunEvent::Exit → 最终清理...");
            if let Some(state) = app.try_state::<AppState>() {
                cleanup_resources(&state);
            }
            // 确保子进程真正退出后再退出进程：轮询而不是固定 sleep——
            // 清理已完成（stop_all 后进程表为空）即可立刻结束，最多等 1500ms；
            // Job Object (KILL_ON_JOB_CLOSE) 是最终兜底
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
            while std::time::Instant::now() < deadline {
                if let Some(state) = app.try_state::<AppState>() {
                    if state.process_mgr.running().is_empty() {
                        break;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            log::info!("[nexus] RunEvent::Exit → 完成");
        }
    });
}
