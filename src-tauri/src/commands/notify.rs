//! 自制通知小窗：窗口不在前台时，由应用自己画一个置顶小卡片。
//!
//! 为什么不用 Windows toast：
//! - WinRT toast 需要注册 AppUserModelID；dev / 免安装版可能根本弹不出来，
//!   而通知插件在 Windows 上又是异步 `let _ = show()`，失败被吞掉，代码无法感知。
//! - 自制窗口不依赖系统通知开关 / 专注助手 / AUMID，dev 与打包版表现一致。
//!
//! 边界：
//! - 窗口由**后端**创建（前端只渲染内容），主窗口隐藏时也能出现；
//! - 不抢焦点（`focused(false)`）、跳过任务栏、置顶；
//! - 点击卡片 → 显示主窗口并关闭小窗；✕ → 只关闭小窗；
//! - 只有一个通知窗，后续通知更新内容并重新置顶，避免叠一屏；
//! - 创建失败时退回"显示主窗口"——这是最后一道，至少不静默丢消息。
//!
//! 已知限制：全屏独占应用、Win+D、锁屏/UAC 安全桌面之上仍可能看不到或看不到点击；
//! 这些是窗口系统的边界，不是通知逻辑能绕过的。

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

/// 通知小窗的固定 label（同一时刻只存在一个）
pub(crate) const NOTICE_LABEL: &str = "nexus-notice";
/// 更新已有小窗内容的事件名
const NOTICE_EVENT: &str = "nexus-notice";
/// 通知小窗尺寸边界（逻辑像素）。实际大小由前端按内容测量后经 `notice_ready`
/// 调整；这里同时用作初始窗口尺寸与越界钳制，避免被攻陷的页面把窗口调到离谱大小。
const NOTICE_MAX_WIDTH: f64 = 340.0;
const NOTICE_MIN_WIDTH: f64 = 260.0;
const NOTICE_MAX_HEIGHT: f64 = 240.0;
const NOTICE_MIN_HEIGHT: f64 = 48.0;
const NOTICE_MARGIN: f64 = 16.0;

#[derive(Clone, serde::Serialize)]
pub(crate) struct NoticePayload {
    pub title: String,
    pub body: String,
    /// error / warning / success / info
    pub level: String,
}

fn sanitize(text: &str, max_chars: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(max_chars)
        .collect()
}

fn normalize_level(level: &str) -> &'static str {
    match level {
        "error" => "error",
        "warning" => "warning",
        "success" => "success",
        _ => "info",
    }
}

/// 小窗位置：鼠标所在显示器（没有则主显示器）工作区的右下角。
///
/// 用**鼠标所在屏**而不是主窗口所在屏：主窗口可能隐藏/在另一块屏，用户此刻在哪块屏
/// 看东西只有鼠标说得准。用 `work_area` 而不是整屏尺寸：任务栏可能在底部/顶部/侧边，
/// 固定扣 64px 只在底部任务栏下成立。返回逻辑坐标（Tauri 的 position 用逻辑单位）。
///
/// 通知只在主窗口不在前台时出现（见 `notify_if_background`），右下角不会挡住正在
/// 操作的主窗口；这里采用系统通知的惯用位置。
fn notice_position(app: &AppHandle, width: f64, height: f64) -> (f64, f64) {
    let monitor = app
        .cursor_position()
        .ok()
        .and_then(|p| app.monitor_from_point(p.x, p.y).ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        return (100.0, 100.0);
    };
    let scale = monitor.scale_factor();
    let work = monitor.work_area();
    let pos = work.position.to_logical::<f64>(scale);
    let size = work.size.to_logical::<f64>(scale);
    let x = pos.x + size.width - width - NOTICE_MARGIN;
    let y = pos.y + size.height - height - NOTICE_MARGIN;
    // 屏幕太小/坐标异常时别把窗口放到屏幕外
    (x.max(pos.x + NOTICE_MARGIN), y.max(pos.y + NOTICE_MARGIN))
}

/// 把内容写到已有的通知窗（存在且可用时返回 true）。
///
/// **不使用全局锁**：通知可能同时来自前端命令（同步命令在主线程执行）与后端监督线程
/// （后台线程）。后台线程持有锁去等主线程创建窗口、而主线程又在等同一把锁，会互锁。
/// 并发创建时由"先建的人赢、失败者再更新一次"兜住——窗口 label 唯一，不会出现两个。
fn update_existing_notice(app: &AppHandle, payload: &NoticePayload) -> bool {
    let Some(win) = app.get_webview_window(NOTICE_LABEL) else {
        return false;
    };
    let emitted = match app.emit_to(NOTICE_LABEL, NOTICE_EVENT, payload) {
        Ok(()) => true,
        Err(e) => {
            log::warn!("[nexus] 更新通知小窗内容失败: {}", e);
            false
        }
    };
    let shown = match win.show() {
        Ok(()) => true,
        Err(e) => {
            log::warn!("[nexus] 显示通知小窗失败: {}", e);
            false
        }
    };
    // Win+D/显示桌面可能把它一起最小化；更新时顺手恢复
    if let Err(e) = win.unminimize() {
        log::warn!("[nexus] 恢复通知小窗失败: {}", e);
    }
    if let Err(e) = win.set_always_on_top(true) {
        log::warn!("[nexus] 通知小窗置顶失败: {}", e);
    }
    emitted || shown
}

fn show_notice(app: &AppHandle, payload: &NoticePayload) -> Result<(), String> {
    // 已有小窗：更新内容、重新置顶（不要新建第二个）
    if update_existing_notice(app, payload) {
        return Ok(());
    }

    let json = serde_json::to_string(payload)
        .map_err(|e| format!("序列化通知数据失败: {}", e))?;
    // 初始尺寸取上限，窗口创建后保持隐藏；前端量出内容尺寸再调 `notice_ready` 调整
    let (x, y) = notice_position(app, NOTICE_MAX_WIDTH, NOTICE_MAX_HEIGHT);
    let builder = WebviewWindowBuilder::new(app, NOTICE_LABEL, WebviewUrl::App("index.html".into()))
        .title("Nexus 通知")
        .inner_size(NOTICE_MAX_WIDTH, NOTICE_MAX_HEIGHT)
        .position(x, y)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        // 关掉系统阴影/描边：Tauri 给无边框窗口加阴影时，Windows 11 会顺带画一圈
        // 1px 系统边框（文档原文），和卡片自己的边框叠成“框外面还有一个框”。
        .shadow(false)
        // 不抢焦点：用户正在别的应用里输入时，通知不能把键盘抢走
        .focused(false)
        // 先隐藏，等前端渲染完调用 `notice_ready` 再显示，避免白底闪一下
        .visible(false)
        // 数据走初始化脚本：窗口文档在加载前就拿到首条通知内容
        .initialization_script(format!("window.__NEXUS_NOTICE__ = {};", json));

    match builder.build() {
        Ok(_) => Ok(()),
        Err(e) => {
            // 并发场景：另一条通知刚把窗口建好（label 唯一，后建者会失败）。
            // 再尝试更新一次；仍不可用才走"显示主窗口"的最后兜底。
            if update_existing_notice(app, payload) {
                return Ok(());
            }
            log::warn!("[nexus] 创建通知小窗失败（改为显示主窗口）: {}", e);
            crate::tray::show_main_window(app);
            Err(format!("创建通知小窗失败: {}", e))
        }
    }
}

/// 关闭通知小窗（主窗口显示时调用：用户已经回到应用，卡片没有必要再挂着）。
pub(crate) fn close_notice(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(NOTICE_LABEL) {
        if let Err(e) = win.close() {
            log::warn!("[nexus] 关闭通知小窗失败: {}", e);
        }
    }
}

/// 主窗口是否在前台可见。
///
/// 为什么不能只看 `Window::is_focused()`：键盘焦点进入 WebView2 子窗口（主内容或 AI 面板
/// 子 WebView）时，tao 会把顶层窗口的 focused 状态置为 false——即使它仍是前台窗口。
/// 实测：用户正看着 Nexus、AI 面板持有焦点时，项目启动成功通知照样弹出。Windows 下以
/// `GetForegroundWindow` 为准；其它平台沿用 Tauri 的 focused。
fn main_window_in_foreground(window: &tauri::Window) -> bool {
    if !window.is_visible().unwrap_or(false) {
        return false;
    }
    #[cfg(windows)]
    {
        let Ok(hwnd) = window.hwnd() else { return false; };
        let foreground = unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow()
        };
        // HWND 不是内存指针语义，按整数比较（clippy 的 ptr_eq 不适用于句柄）
        foreground as isize == hwnd.0 as isize
    }
    #[cfg(not(windows))]
    {
        window.is_focused().unwrap_or(false)
    }
}

/// 窗口不在前台时弹自制小窗。
///
/// 返回 `false` = 主窗口在前台可见，故意没弹（应用内 toast 已经能看到）。
pub(crate) fn notify_if_background(
    app: &AppHandle,
    level: &str,
    title: &str,
    body: &str,
) -> Result<bool, String> {
    let title = sanitize(title, 120);
    // 小窗只放摘要；完整错误仍在服务日志/应用日志里
    let body = sanitize(body, 400);
    if title.trim().is_empty() {
        return Err("通知标题不能为空".into());
    }
    // 主窗口用 get_window 找：AI 面板打开后主窗口会多一个子 WebView，
    // `get_webview_window` 会因 is_webview_window() 变 false 而返回 None，
    // 导致"前台不打扰"失效（窗口明明在前台也弹通知）。见 tray::main_window 的说明。
    if let Some(window) = crate::tray::main_window(app) {
        if main_window_in_foreground(&window) {
            return Ok(false);
        }
    }
    show_notice(
        app,
        &NoticePayload {
            title,
            body,
            level: normalize_level(level).to_string(),
        },
    )?;
    Ok(true)
}

/// 前端（主窗口）触发的通知。只允许主窗口调用；AI 子 WebView 不能借原生 UI 做提醒。
///
/// **必须是 async**：首次通知会按需创建通知小窗（第二个 WebView2）。同步命令跑在 IPC
/// 自定义协议的回调线程（主线程）上；在该回调里再同步创建 WebView2 时，controller
/// 创建会卡在同一线程（实测：HWND 建出来了、页面永不加载，`build()` 不返回）。后果是
/// **这条 IPC 永不完成，主窗口所有 invoke 一起挂起**——服务停不掉、关窗确认收不到，
/// 但 Win32 消息循环仍在跑（WM_CLOSE 还能进日志），所以看起来像“界面没死、点啥都没用”。
/// 改成 async 后命令在 tokio 线程执行，建窗请求经事件循环派发回主线程，协议回调立即返回。
#[tauri::command]
pub async fn notify_system(
    webview: tauri::Webview,
    app: AppHandle,
    title: String,
    body: String,
    level: Option<String>,
) -> Result<bool, String> {
    if webview.label() != "main" {
        log::warn!(
            "[nexus] 拒绝来自 webview「{}」的通知请求（只允许主窗口）",
            webview.label()
        );
        return Err("通知只允许主窗口调用".into());
    }
    // spawn_blocking：内部有 is_visible/is_focused 等同步查询与 WebView2 建窗，
    // 都是阻塞调用；tokio worker 上直接跑会占住 worker（逻辑不变，但不是好习惯）。
    tauri::async_runtime::spawn_blocking(move || {
        notify_if_background(&app, level.as_deref().unwrap_or("info"), &title, &body)
    })
    .await
    .map_err(|e| format!("发送通知任务失败: {}", e))?
}

/// 通知窗渲染完成后按内容尺寸调整自己并显示（避免白底闪烁）；只允许通知窗调用。
///
/// 宽高由前端 `NoticeWindow` 量出（卡片按内容取 fit-content），后端只做边界钳制：
/// 尺寸来自 webview，非法值（NaN/负数/离谱值）一律回退到边界，页面被攻陷时
/// 也不能把置顶小窗调成任意大小。更新已有通知时同一命令会重新调整尺寸并把窗口
/// 重新锚定到右下角。
#[tauri::command]
pub fn notice_ready(
    webview: tauri::Webview,
    app: AppHandle,
    width: f64,
    height: f64,
) -> Result<(), String> {
    if webview.label() != NOTICE_LABEL {
        return Err("通知就绪只允许通知窗调用".into());
    }
    let width = if width.is_finite() {
        width.clamp(NOTICE_MIN_WIDTH, NOTICE_MAX_WIDTH)
    } else {
        NOTICE_MAX_WIDTH
    };
    let height = if height.is_finite() {
        height.clamp(NOTICE_MIN_HEIGHT, NOTICE_MAX_HEIGHT)
    } else {
        NOTICE_MAX_HEIGHT
    };
    let Some(win) = app.get_webview_window(NOTICE_LABEL) else {
        return Ok(());
    };
    let (x, y) = notice_position(&app, width, height);
    win.set_size(tauri::LogicalSize::new(width, height))
        .map_err(|e| format!("调整通知小窗尺寸失败: {}", e))?;
    win.set_position(tauri::LogicalPosition::new(x, y))
        .map_err(|e| format!("调整通知小窗位置失败: {}", e))?;
    win.show().map_err(|e| format!("显示通知小窗失败: {}", e))?;
    Ok(())
}

/// 点击通知卡片：回到主窗口（`show_main_window` 会顺手关闭通知窗）；只允许通知窗调用。
#[tauri::command]
pub fn notice_clicked(webview: tauri::Webview, app: AppHandle) -> Result<(), String> {
    if webview.label() != NOTICE_LABEL {
        return Err("通知点击只允许通知窗调用".into());
    }
    crate::tray::show_main_window(&app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_keeps_newlines_and_caps_length() {
        assert_eq!(sanitize("a\u{0}b\nc", 10), "ab\nc");
        assert_eq!(sanitize("abcdef", 3), "abc");
        // 中文按字符计数，不按字节截断成半个字
        assert_eq!(sanitize("中文标题", 2), "中文");
    }

    #[test]
    fn test_normalize_level_whitelists() {
        assert_eq!(normalize_level("error"), "error");
        assert_eq!(normalize_level("warning"), "warning");
        assert_eq!(normalize_level("success"), "success");
        assert_eq!(normalize_level("info"), "info");
        assert_eq!(normalize_level("注入<script>"), "info");
    }

    /// 源码扫描护栏：`notify_system` 必须是 async。
    ///
    /// 同步命令跑在 WebView2 IPC 自定义协议的回调线程上，首次通知会在该线程再创建
    /// 一个 WebView2——实测 controller 创建卡死，且**此后全部 invoke 不再返回**
    /// （服务停不掉、关窗确认收不到）。这条失败模式没有单测能自然复现，只能用
    /// 源码扫描挡住"顺手改回同步"的回归；改签名前先读函数上方的说明。
    #[test]
    fn test_notify_system_stays_async_for_webview_creation() {
        let src = include_str!("notify.rs");
        // 运行时拼出签名，避免扫描目标本身就出现在本测试源码里（否则改成同步也照样命中）
        let needle = ["pub", "async fn notify_system"].join(" ");
        assert!(
            src.contains(&needle),
            "notify_system 必须保持 async：同步版本会在 IPC 回调线程上创建通知小窗，冻结全部 invoke"
        );
    }
}
