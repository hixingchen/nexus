//! 应用级命令（退出流程 + 原生目录选择）
//!
//! 为什么单独一个模块：这两件事都牵涉应用级时序/授权语义，集中在这里便于说明。

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;

use crate::commands::editor::remember_confirmed_dir;

/// 退出流程是否已启动（幂等：前端按钮可能连点、RunEvent::Exit 也可能兜底再来一次）
static EXIT_STARTED: AtomicBool = AtomicBool::new(false);

/// 发起退出：在**独立线程**里做资源清理，完成后再退出进程。
///
/// 为什么异步：`cleanup_resources` → `stop_all()` 会对每个服务串行执行
/// `taskkill` + 限时等待（含 50ms 轮询与两个 reader 线程的 1s `recv_timeout`），
/// 再加 AI 会话（最多 2s）与文件监听（每项目 200ms 粒度）。服务多时是秒级阻塞——
/// 内联在窗口事件/IPC 线程上，窗口会整段"未响应"（原实现就在 `CloseRequested` 里同步跑）。
///
/// 为什么由前端调用而不是直接在 `CloseRequested` 里做：窗口关闭请求现在会先交给前端
/// 处理"未保存草稿"确认（见 `src/hooks/useAppCloseGuard.ts`），用户可能**取消**关闭——
/// 那时把服务全停掉是错的。所以清理只在"用户确认关闭"之后由本命令触发；
/// `WindowEvent::CloseRequested` 不再做清理，`RunEvent::Exit` 仍是最后的兜底（幂等）。
#[tauri::command]
pub fn prepare_exit(app: AppHandle) -> Result<(), String> {
    if EXIT_STARTED.swap(true, Ordering::SeqCst) {
        // 已在退出中：重复请求直接返回（第二发不该再起一个清理线程）
        return Ok(());
    }
    let handle = app.clone();
    std::thread::Builder::new()
        .name("nexus-exit".into())
        .spawn(move || {
            if let Some(state) = handle.try_state::<crate::AppState>() {
                crate::cleanup_resources(&state);
            }
            log::info!("[nexus] 退出清理完成，进程退出");
            handle.exit(0);
        })
        .map_err(|e| {
            // 线程起不来（极罕见）：复位标志让调用方可以重试，并让窗口关闭回退到默认路径
            EXIT_STARTED.store(false, Ordering::SeqCst);
            format!("启动退出线程失败: {}", e)
        })?;
    Ok(())
}

/// 原生目录选择器（Rust 侧弹框），并把用户选中的目录记为"已确认"。
///
/// 为什么必须由 Rust 侧弹框：白名单收口要求"项目外的新目录只能由用户显式授权"。
/// 如果授权入口是前端 `plugin-dialog` 的 `open()` + 一个 `confirm_directory(path)` 命令，
/// 那么被攻陷的 webview 可以直接调 `confirm_directory("C:/")` 自己给自己授权——
/// 授权点必须落在**用户亲手操作**的对话框上，且路径不经 IPC 传入。
///
/// 返回用户选择的原始路径字符串（供表单展示与入库）；取消返回 None。
#[tauri::command]
pub async fn pick_directory(
    app: AppHandle,
    title: Option<String>,
    default_path: Option<String>,
) -> Result<Option<String>, String> {
    let picker = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        let mut builder = picker.dialog().file();
        if let Some(t) = title.as_deref().filter(|t| !t.trim().is_empty()) {
            builder = builder.set_title(t);
        }
        if let Some(d) = default_path.as_deref().filter(|d| !d.trim().is_empty()) {
            if std::path::Path::new(d).is_dir() {
                builder = builder.set_directory(d);
            }
        }
        // blocking_* 会阻塞当前线程直到用户操作完成 → 必须放在 spawn_blocking 里，别卡住 IPC 线程
        builder.blocking_pick_folder()
    })
    .await
    .map_err(|e| format!("目录选择任务失败: {}", e))?;

    let Some(file_path) = picked else { return Ok(None) };
    let dir = file_path.into_path().map_err(|e| format!("目录选择结果无法转为路径: {}", e))?;
    if let Some(state) = app.try_state::<crate::AppState>() {
        remember_confirmed_dir(&state, &dir);
    }
    Ok(Some(dir.to_string_lossy().to_string()))
}
