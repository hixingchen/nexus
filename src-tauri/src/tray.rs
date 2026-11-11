//! 系统托盘：显示主窗口 / 启动当前项目 / 停止当前项目 / 退出。
//!
//! 设计边界：
//! - **托盘只发事件，不直接启停服务**：当前项目是前端状态，动作/成功/失败提示也都在
//!   前端（`stores/serviceActions.ts`）。后端直接调启动命令会绕开这些语义，并让
//!   "当前项目"出现第二份真相。
//! - **退出必须走前端关窗流程**：托盘「退出」只负责把窗口显示出来并 emit
//!   `tray-quit`；前端 `CloseGuard` 会走未保存草稿确认，确认后才 `prepare_exit`
//!   （停服务 / AI / 监听）。直接 `app.exit()` 会绕过草稿确认与资源清理。
//! - 托盘图标没有前端也可用（左键显示窗口），但启动/停止/退出依赖前端事件。

use tauri::menu::{MenuBuilder, MenuItem, PredefinedMenuItem};
use tauri::tray::{
    MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
};
use tauri::{AppHandle, Emitter, Manager, Wry};

/// 菜单项 id（同时用于区分全局菜单事件）
const ID_SHOW: &str = "tray-show";
const ID_START: &str = "tray-start-project";
const ID_STOP: &str = "tray-stop-project";
const ID_QUIT: &str = "tray-quit";

/// 托盘句柄与动态菜单项。
///
/// `_tray` 必须持有：`TrayIcon` 是引用计数的，最后一个实例 drop 时图标会被移除。
pub struct TrayState {
    _tray: TrayIcon<Wry>,
    start_item: MenuItem<Wry>,
    stop_item: MenuItem<Wry>,
}

/// 把主窗口显示出来并聚焦。托盘图标点击、通知点击、单实例唤醒都走这里。
pub fn show_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        log::warn!("[nexus] 托盘: 找不到主窗口");
        return;
    };
    if let Err(e) = window.show() {
        log::warn!("[nexus] 托盘: 显示主窗口失败: {}", e);
    }
    if let Err(e) = window.unminimize() {
        log::warn!("[nexus] 托盘: 取消最小化失败: {}", e);
    }
    if let Err(e) = window.set_focus() {
        log::warn!("[nexus] 托盘: 聚焦主窗口失败: {}", e);
    }
}

/// 菜单项文案：有当前项目时把名字带上，便于在托盘里确认操作对象。
///
/// 名字过长时截断（菜单会撑得很宽）；只截展示，不影响实际项目。
/// `&` 在原生菜单里是助记符前缀：必须转义成 `&&`，否则项目名里的 `&` 会吞掉
/// 后一个字符并给菜单项挂上下划线，看起来像"名字被改坏了"。
fn project_label(prefix: &str, name: Option<&str>) -> String {
    match name {
        Some(raw) => {
            let count = raw.chars().count();
            let short: String = raw.chars().take(24).collect();
            let safe = short.replace('&', "&&");
            if count > 24 {
                format!("{}「{}…」", prefix, safe)
            } else {
                format!("{}「{}」", prefix, safe)
            }
        }
        None => prefix.to_string(),
    }
}

/// 托盘/菜单里显示的项目名：去掉控制字符（换行会破坏菜单排版）、限长。
/// 空串视为"没有项目"。
fn sanitize_project_name(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() { None } else { Some(cleaned) }
}

pub fn init(app: &AppHandle) -> tauri::Result<()> {
    let show_item = MenuItem::with_id(app, ID_SHOW, "显示主窗口", true, None::<&str>)?;
    let start_item = MenuItem::with_id(app, ID_START, "启动当前项目", false, None::<&str>)?;
    let stop_item = MenuItem::with_id(app, ID_STOP, "停止当前项目", false, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, ID_QUIT, "退出 Nexus", true, None::<&str>)?;
    let sep_top = PredefinedMenuItem::separator(app)?;
    let sep_bottom = PredefinedMenuItem::separator(app)?;

    let menu = MenuBuilder::new(app)
        .item(&show_item)
        .item(&sep_top)
        .item(&start_item)
        .item(&stop_item)
        .item(&sep_bottom)
        .item(&quit_item)
        .build()?;

    let mut builder = TrayIconBuilder::new()
        .menu(&menu)
        .tooltip("Nexus")
        // 左键用于显示窗口；菜单走右键（Windows 的惯例）
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            ID_SHOW => show_main_window(app),
            ID_START => {
                if let Err(e) = app.emit("tray-action", "start-project") {
                    log::warn!("[nexus] 托盘启动事件发送失败（前端可能未加载）: {}", e);
                }
            }
            ID_STOP => {
                if let Err(e) = app.emit("tray-action", "stop-project") {
                    log::warn!("[nexus] 托盘停止事件发送失败（前端可能未加载）: {}", e);
                }
            }
            ID_QUIT => {
                // 先显示窗口：未保存草稿确认弹窗在窗口里，隐藏状态下用户看不到
                show_main_window(app);
                if let Err(e) = app.emit("tray-quit", ()) {
                    log::warn!("[nexus] 托盘退出事件发送失败（前端可能未加载）: {}", e);
                }
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
            | TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    } else {
        // 没有应用图标时托盘会是一个空白格；留痕便于排查（正常打包/开发都有图标）
        log::warn!("[nexus] 托盘: 当前应用没有默认图标");
    }

    let tray = builder.build(app)?;
    app.manage(TrayState { _tray: tray, start_item, stop_item });
    log::info!("[nexus] 系统托盘已创建");
    Ok(())
}

/// 前端同步「当前选中项目」：更新两个菜单项的文字与可用状态。
///
/// `None` = 没有选中项目：两项置灰（避免点了没反应）。
#[tauri::command]
pub fn set_tray_project(
    // 只允许主窗口改托盘文案：AI 子 WebView 加载的是 dsh 页面，不该能改写原生 UI。
    webview: tauri::Webview,
    app: AppHandle,
    project_name: Option<String>,
) -> Result<(), String> {
    if webview.label() != "main" {
        log::warn!(
            "[nexus] 拒绝来自 webview「{}」的托盘菜单更新（只允许主窗口）",
            webview.label()
        );
        return Err("托盘菜单只允许主窗口更新".into());
    }
    let state = app
        .try_state::<TrayState>()
        .ok_or_else(|| "系统托盘未初始化".to_string())?;
    let name = project_name.and_then(|n| sanitize_project_name(&n));

    state
        .start_item
        .set_text(project_label("启动", name.as_deref()))
        .map_err(|e| format!("更新托盘菜单失败: {}", e))?;
    state
        .stop_item
        .set_text(project_label("停止", name.as_deref()))
        .map_err(|e| format!("更新托盘菜单失败: {}", e))?;
    let enabled = name.is_some();
    state
        .start_item
        .set_enabled(enabled)
        .map_err(|e| format!("更新托盘菜单失败: {}", e))?;
    state
        .stop_item
        .set_enabled(enabled)
        .map_err(|e| format!("更新托盘菜单失败: {}", e))?;

    let tooltip = match &name {
        Some(n) => format!("Nexus · {}", n),
        None => "Nexus".to_string(),
    };
    if let Err(e) = state._tray.set_tooltip(Some(tooltip)) {
        log::warn!("[nexus] 更新托盘提示失败: {}", e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::project_label;

    #[test]
    fn test_project_label_includes_name_and_truncates_long_names() {
        assert_eq!(project_label("启动", None), "启动");
        assert_eq!(project_label("启动", Some("demo")), "启动「demo」");
        let long = "a".repeat(30);
        let label = project_label("停止", Some(&long));
        assert!(label.starts_with("停止「"));
        assert!(label.ends_with("…」"), "过长名字应截断并留痕: {}", label);
        assert!(label.chars().count() < long.chars().count() + 10);
        // & 是原生菜单的助记符前缀：必须转义成 &&，否则显示会吞字符/加下划线
        assert_eq!(project_label("启动", Some("A&B")), "启动「A&&B」");
    }

    #[test]
    fn test_sanitize_project_name_strips_controls_and_caps() {
        assert_eq!(super::sanitize_project_name("  demo  ").as_deref(), Some("demo"));
        assert_eq!(super::sanitize_project_name("a\nb\tc").as_deref(), Some("abc"));
        assert_eq!(super::sanitize_project_name("   "), None);
        let long = "项".repeat(100);
        assert_eq!(super::sanitize_project_name(&long).unwrap().chars().count(), 64);
    }
}
