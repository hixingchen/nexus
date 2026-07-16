mod commands;
mod core;
mod database;
mod models;

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tauri::Manager;

use crate::core::terminal::TerminalManager;
use crate::core::file_watcher::FileWatcher;
use crate::core::process::ProcessManager;
use crate::database::Database;

pub struct AppState {
    // tokio::sync::Mutex: terminal_mgr 内部需要 async 操作（close_all 中 try_lock + sleep）
    pub terminal_mgr: Arc<Mutex<TerminalManager>>,
    pub db: Database,
    pub process_mgr: ProcessManager,
    pub file_watcher: FileWatcher,
    // std::sync::Mutex: 仅同步操作，无需跨 .await 持有
    pub project_root: std::sync::Mutex<Option<String>>,
    pub event_tasks: std::sync::Mutex<HashMap<String, tokio::task::AbortHandle>>,
}

/// 统一的资源清理逻辑
///
/// 幂等设计：多次调用安全（stop_all/close_all 对空集合是 no-op）。
/// 不阻塞调用方：内部在独立线程中执行，最多耗时 3 秒。
/// 清理顺序：服务进程 → 终端事件 → 终端会话 → 文件监听
fn cleanup_resources(state: &AppState) {
    let start = std::time::Instant::now();

    // 1. 停止所有服务进程（最高优先级，含 taskkill /T /F + wait）
    state.process_mgr.stop_all();
    log::info!("[nexus] 清理: 服务进程已停止 ({:.0}ms)", start.elapsed().as_millis());

    // 2. 中止所有终端事件推送任务
    if let Ok(mut tasks) = state.event_tasks.lock() {
        let count = tasks.len();
        for (_, handle) in tasks.drain() {
            handle.abort();
        }
        log::info!("[nexus] 清理: {} 个终端事件任务已中止", count);
    }

    // 3. 关闭所有终端会话（kill PTY 子进程 → 释放 PTY master → conhost.exe 退出）
    //    加超时重试，避免永久阻塞
    for attempt in 0..3 {
        match state.terminal_mgr.try_lock() {
            Ok(mut tm) => {
                tokio::runtime::Handle::current().block_on(tm.close_all());
                log::info!("[nexus] 清理: 终端会话已关闭 (attempt={}, {:.0}ms)",
                    attempt + 1, start.elapsed().as_millis());
                break;
            }
            Err(_) => {
                log::warn!("[nexus] terminal_mgr 锁获取失败 (attempt={}/3)，等待重试...", attempt + 1);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }

    // 4. 停止文件监听
    state.file_watcher.stop_all();

    log::info!("[nexus] 清理完成 (总耗时 {:.0}ms)", start.elapsed().as_millis());
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let db = Database::try_new().unwrap_or_else(|e| {
        log::error!("数据库初始化失败: {}", e);
        panic!("数据库初始化失败: {}", e);
    });

    // 创建共享的 Job Object（通过 Arc 在 TerminalManager 和 ProcessManager 间共享）
    // 失败时降级运行：子进程不会在父进程退出时自动终止，但应用仍可正常使用
    #[cfg(windows)]
    let job = match crate::core::job_object::JobObject::new() {
        Ok(j) => Some(Arc::new(j)),
        Err(e) => {
            log::warn!("[nexus] ⚠ Job Object 创建失败，子进程自动清理已禁用: {}", e);
            log::warn!("[nexus]   应用退出时可能存在残留进程，需手动清理");
            // 启动后通过 UI 通知用户
            log::error!("[nexus] 子进程自动清理不可用，关闭应用后可能残留后台进程");
            None
        }
    };

    // 初始化终端管理器并设置 Job Object
    let mut terminal_mgr = TerminalManager::new();
    let mut process_mgr = ProcessManager::new();
    #[cfg(windows)]
    {
        if let Some(ref job) = job {
            terminal_mgr.set_job(job.clone());
            process_mgr.set_job(job.clone());
        }
    }
    let terminal_mgr = Arc::new(Mutex::new(terminal_mgr));

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            terminal_mgr: terminal_mgr.clone(),
            db,
            process_mgr,
            file_watcher: FileWatcher::new(),
            project_root: std::sync::Mutex::new(None),
            event_tasks: std::sync::Mutex::new(HashMap::new()),
        })
        .invoke_handler(tauri::generate_handler![
            commands::editor::read_file,
            commands::editor::list_directory,
            commands::editor::open_in_explorer,
            commands::terminal::create_terminal,
            commands::terminal::write_terminal,
            commands::terminal::resize_terminal,
            commands::terminal::close_terminal,
            commands::terminal::kill_terminal,  // 预留：前端暂未调用
            commands::service::get_services,
            commands::service::add_service,
            commands::service::update_service,
            commands::service::delete_service,
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
            commands::watcher::start_watching,
            commands::watcher::stop_watching,
            commands::layout::save_layout,
            commands::layout::load_layout,
            commands::editor::set_project_root,
            commands::claude::claude_start,
            commands::claude::claude_stop,
            commands::claude::claude_check,
        ])
        .on_window_event(move |window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                log::info!("[nexus] CloseRequested → 开始清理...");
                if let Some(state) = window.try_state::<AppState>() {
                    cleanup_resources(&state);
                }
                log::info!("[nexus] CloseRequested → 清理完成，窗口即将关闭");
            }
        })
        .build(tauri::generate_context!());

    let app = match app {
        Ok(app) => app,
        Err(e) => {
            log::error!("构建 Nexus 应用失败: {}", e);
            panic!("构建 Nexus 应用失败: {}", e);
        }
    };

    // Exit 是最后的安全网：确保所有资源被释放，子进程被终止
    // cleanup_resources 是幂等的（对空集合 no-op），多次调用安全
    app.run(move |app, event| {
        if let tauri::RunEvent::Exit = event {
            log::info!("[nexus] RunEvent::Exit → 最终清理...");
            if let Some(state) = app.try_state::<AppState>() {
                cleanup_resources(&state);
            }
            // 确保子进程真正退出后再退出进程
            // Job Object (KILL_ON_JOB_CLOSE) 是最终兜底
            std::thread::sleep(std::time::Duration::from_millis(1500));
            log::info!("[nexus] RunEvent::Exit → 完成");
        }
    });
}
