//! 应用级命令（退出流程 + 原生目录选择）
//!
//! 为什么单独一个模块：这两件事都牵涉应用级时序/授权语义，集中在这里便于说明。

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;


/// 退出流程是否已启动（幂等：前端按钮可能连点、RunEvent::Exit 也可能兜底再来一次）
static EXIT_STARTED: AtomicBool = AtomicBool::new(false);

/// 当前应用版本号（"检查更新"拿它跟 GitHub 上的最新 release 比）。
///
/// 为什么由后端答而不是前端读 `package.json`：那是 npm 侧的版本号，跟打包出来的
/// 安装包未必同步（本项目是同一份改动里改四处才保证一致）；`CARGO_PKG_VERSION`
/// 才是**这个二进制自己的**版本——用户装的、界面显示的、"有没有新版"比较的，
/// 从此是同一个数。
#[tauri::command]
pub fn get_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// 发起退出：在**独立线程**里做资源清理，完成后再退出进程。
///
/// 为什么异步：`cleanup_resources` → `stop_all()` 会对每个服务串行执行
/// `taskkill` + 限时等待（含 50ms 轮询与两个 reader 线程的 1s `recv_timeout`），
/// 再加 AI 会话（最多 2s）与文件监听（每项目 200ms 粒度）。服务多时是秒级阻塞——
/// 内联在窗口事件/IPC 线程上，窗口会整段"未响应"（原实现就在 `CloseRequested` 里同步跑）。
///
/// 为什么由前端调用而不是直接在 `CloseRequested` 里做：窗口关闭请求现在会先交给前端
/// 处理"未保存草稿"确认（见 `src/components/layout/CloseGuard.tsx`），用户可能**取消**关闭——
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

/// 目录选择器的**用途** → 对话框标题。
///
/// **标题由后端决定，不接受前端传入的字符串**（SEC-17）：原生对话框无法被网页伪造，
/// 用户对它的信任天然高于网页元素——若标题可任意指定，被攻陷的 webview 就能弹出一个
/// 标题写着「Nexus 需要访问 …\.ssh 才能继续」的**系统级**选择框，把信任锚从"用户确认"
/// 降级为"被诱导点击"。前端因此只能传用途键；未知用途回落到通用文案，
/// **绝不回落成前端传进来的字符串**（那等于把缺口原样还回去）。
///
/// 加新用途就在这里加一行——标题会出现在界面上，属于后端该管的文案。
fn picker_title(purpose: Option<&str>) -> &'static str {
    match purpose {
        Some("projectDir") => "选择项目目录",
        Some("serviceCwd") => "选择工作目录",
        _ => "选择目录",
    }
}

/// 原生目录选择器（Rust 侧弹框），并把用户选中的目录记为"已确认"。
///
/// 为什么必须由 Rust 侧弹框：白名单收口要求"项目外的新目录只能由用户显式授权"。
/// 如果授权入口是前端 `plugin-dialog` 的 `open()` + 一个 `confirm_directory(path)` 命令，
/// 那么被攻陷的 webview 可以直接调 `confirm_directory("C:/")` 自己给自己授权——
/// 授权点必须落在**用户亲手操作**的对话框上，且路径不经 IPC 传入。
///
/// 返回用户选择的原始路径字符串（供表单展示与入库）；取消返回 None。
///
/// 参数 `purpose` 是**用途键**（`projectDir` / `serviceCwd`），不是显示文案——见 `picker_title`。
#[tauri::command]
pub async fn pick_directory(
    app: AppHandle,
    purpose: Option<String>,
    default_path: Option<String>,
) -> Result<Option<String>, String> {
    let title = picker_title(purpose.as_deref());
    let picker = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        let mut builder = picker.dialog().file().set_title(title);
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
        state.paths.remember_confirmed_dir(&dir);
    }
    Ok(Some(dir.to_string_lossy().to_string()))
}

/// 文件保存对话框的**用途** → 对话框标题。理由同 `picker_title`（SEC-17）：标题由后端
/// 决定，前端只能传用途键，未知用途回落通用文案。
fn save_picker_title(purpose: Option<&str>) -> &'static str {
    match purpose {
        Some("exportTemplates") => "导出模板文件",
        _ => "保存文件",
    }
}

/// 原生文件保存对话框（Rust 侧弹框），并把用户选中的**所在目录**记为"已确认"。
///
/// 为什么必须由 Rust 侧弹框（SEC-19）：`export_service_templates` 要往"用户挑的位置"写文件，
/// 而"用户挑了哪里"不能由一个 IPC 参数说了算——前端 `plugin-dialog` 的 `save()` 拿到的路径
/// 经 IPC 回传，服务端分辨不出它是不是用户刚选的（这正是 `pick_directory` 刻意避开的写法）。
/// 因此授权点必须是**用户亲手操作**的对话框本身：
/// - 正常流程照旧：用户在对话框里选桌面/文档，父目录当场进 `confirmed_dirs`，导出照常成功；
/// - 被攻陷的 webview 直接调 `export_service_templates("C:/Windows/...")` 会被白名单拒绝。
///
/// `default_name` 只作为对话框的默认文件名，**不是**写入路径（用户可以改，也可以换目录）。
#[tauri::command]
pub async fn pick_save_file(
    app: AppHandle,
    purpose: Option<String>,
    default_name: Option<String>,
) -> Result<Option<String>, String> {
    let title = save_picker_title(purpose.as_deref());
    let picker = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        let mut builder = picker.dialog().file().set_title(title).add_filter("JSON", &["json"]);
        if let Some(name) = default_name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            builder = builder.set_file_name(name);
        }
        // 同 pick_directory：blocking_* 会阻塞当前线程直到用户操作完成，必须放 spawn_blocking
        builder.blocking_save_file()
    })
    .await
    .map_err(|e| format!("保存对话框任务失败: {}", e))?;

    let Some(file_path) = picked else { return Ok(None) };
    let path = file_path.into_path().map_err(|e| format!("保存对话框结果无法转为路径: {}", e))?;
    if let Some(state) = app.try_state::<crate::AppState>() {
        if let Some(parent) = path.parent() {
            state.paths.remember_confirmed_dir(parent);
        }
    }
    Ok(Some(path.to_string_lossy().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SEC-17：对话框标题**永远来自后端这张表**，前端能传的只有用途键。
    ///
    /// 这条测试守的是"被攻陷的 webview 不能伪造原生对话框标题"——改成让前端传字符串时，
    /// 最后一条断言会红（它故意用一句像钓鱼文案的输入）。
    #[test]
    fn test_picker_title_never_comes_from_frontend() {
        assert_eq!(picker_title(Some("projectDir")), "选择项目目录");
        assert_eq!(picker_title(Some("serviceCwd")), "选择工作目录");
        // 未知用途 / 未传 → 通用文案（而不是回落到前端字符串）
        assert_eq!(picker_title(None), "选择目录");
        assert_eq!(picker_title(Some("whatever")), "选择目录");
        // 钓鱼式输入必须原样被忽略
        assert_eq!(
            picker_title(Some("Nexus 需要访问 C:/Users/me/.ssh 才能继续")),
            "选择目录"
        );
        // 保存对话框同一套口径（SEC-19）
        assert_eq!(save_picker_title(Some("exportTemplates")), "导出模板文件");
        assert_eq!(save_picker_title(Some("whatever")), "保存文件");
    }
}
