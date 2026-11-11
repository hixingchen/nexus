//! 系统通知（Windows toast）：只在主窗口不在前台时发，点击通知回到主窗口。
//!
//! 为什么由后端判断前台：`is_visible / is_focused` 是窗口系统的权威状态，
//! 前端 `document.hasFocus()` 在 WebView 隐藏、子 WebView 抢焦点等情况下不可靠。
//! 前端只负责"什么时候值得通知"（服务失败、自动重启放弃），这里负责"要不要打扰"。
//!
//! 为什么用注入的 Tauri 命令而不是 JS 通知插件：发送与点击回调都在 Rust 侧闭环，
//! 前端不需要 `notification:*` 权限，也不会出现"JS 插件版本与 Rust 插件版本不一致"
//! 的第二套依赖。

use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

/// 去掉控制字符（保留换行），并限制长度——通知标题/正文来自应用内部，
/// 但 IPC 参数仍按外部输入处理，避免异常长文本把系统通知撑坏。
fn sanitize(text: &str, max_chars: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(max_chars)
        .collect()
}

/// 在窗口不在前台时发一条系统通知。
///
/// 返回 `false` = 主窗口在前台，故意没发（应用内 toast 已经能看到，不重复打扰）。
///
/// 调用点：服务最终失败、崩溃自动重启放弃、托盘里发起的启动/停止失败、
/// AI 高危操作请求等待确认。前端在调用前不必自己判断焦点，避免两套判据。
#[tauri::command]
pub fn notify_system(
    // 只允许主窗口调用：AI 子 WebView 加载的是 dsh 页面，不能借系统通知做任意文案的
    // 原生提醒（那会把"系统通知"这个信任锚降级成网页可控的钓鱼入口）。
    webview: tauri::Webview,
    app: AppHandle,
    title: String,
    body: String,
) -> Result<bool, String> {
    if webview.label() != "main" {
        log::warn!(
            "[nexus] 拒绝来自 webview「{}」的系统通知请求（只允许主窗口）",
            webview.label()
        );
        return Err("系统通知只允许主窗口调用".into());
    }
    notify_if_background(&app, &title, &body)
}

/// 非命令入口：后台监督线程直接调用（失败通知不能依赖前端轮询——窗口隐藏时
/// WebView2 会节流 `setInterval`，而失败恰恰是这时候最需要通知）。
pub(crate) fn notify_if_background(app: &AppHandle, title: &str, body: &str) -> Result<bool, String> {
    let title = sanitize(title, 120);
    let body = sanitize(body, 1200);
    if title.trim().is_empty() {
        return Err("系统通知标题不能为空".into());
    }
    if let Some(window) = app.get_webview_window("main") {
        let visible = window.is_visible().unwrap_or(true);
        let focused = window.is_focused().unwrap_or(false);
        // 窗口可见且聚焦 = 用户就在应用里，应用内提示足够
        if visible && focused {
            return Ok(false);
        }
    }
    app.notification()
        .builder()
        .title(title)
        .body(body)
        .show()
        .map_err(|e| format!("发送系统通知失败: {}", e))?;
    Ok(true)
}

/// 注册"点击通知 → 回到主窗口"。必须在任何通知发出前调用。
///
/// 插件文档的 Windows 限制：通知还在屏幕上时点击会上报；已经收进操作中心的
/// 旧通知点击不会上报（系统限制，不是本应用能绕过的）。
pub fn init(app: &AppHandle) -> Result<(), String> {
    let handle = app.clone();
    app.notification()
        .on_action(move |_| crate::tray::show_main_window(&handle))
        .map_err(|e| format!("注册系统通知点击处理器失败: {}", e))
}

#[cfg(test)]
mod tests {
    use super::sanitize;

    #[test]
    fn test_sanitize_keeps_newlines_and_caps_length() {
        assert_eq!(sanitize("a\u{0}b\nc", 10), "ab\nc");
        assert_eq!(sanitize("abcdef", 3), "abc");
        // 中文按字符计数，不按字节截断成半个字
        assert_eq!(sanitize("中文标题", 2), "中文");
    }
}
