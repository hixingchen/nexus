mod commands;
mod core;
mod database;
mod models;

use std::sync::Arc;
use tauri::Manager;

use crate::core::file_watcher::FileWatcher;
use crate::core::process::ProcessManager;
use crate::database::Database;

pub struct AppState {
    pub db: Database,
    pub process_mgr: ProcessManager,
    pub file_watcher: FileWatcher,
    // std::sync::Mutex: 仅同步操作，无需跨 .await 持有
    pub project_root: std::sync::Mutex<Option<String>>,
}

/// 统一的资源清理逻辑
///
/// 幂等设计：多次调用安全（stop_all 对空集合是 no-op）。
/// 清理顺序：服务进程 → 文件监听
fn cleanup_resources(state: &AppState) {
    let start = std::time::Instant::now();

    // 1. 停止所有服务进程（最高优先级，含 taskkill /T /F + wait）
    state.process_mgr.stop_all();
    log::info!("[nexus] 清理: 服务进程已停止 ({:.0}ms)", start.elapsed().as_millis());

    // 2. 停止文件监听
    state.file_watcher.stop_all();

    log::info!("[nexus] 清理完成 (总耗时 {:.0}ms)", start.elapsed().as_millis());
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let db = Database::try_new().unwrap_or_else(|e| {
        log::error!("数据库初始化失败: {}", e);
        panic!("数据库初始化失败: {}", e);
    });

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
        }
    }

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            db,
            process_mgr,
            file_watcher: FileWatcher::new(),
            project_root: std::sync::Mutex::new(None),
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
            commands::service::delete_service_template,
            commands::service::reorder_service_templates,
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
