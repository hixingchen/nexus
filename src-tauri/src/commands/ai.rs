//! AI 助手命令层
//!
//! 会话模型：至多一个 dsh web 会话，归属一个工作目录（当前选中项目）。
//! 打开面板/切换项目 → ai_start(cwd)；目录一致且进程存活时幂等复用。
//! 竞态防护：每次 start/stop 递增 epoch，启动完成入库前复查代数，
//! 期间被更新的请求取代则终止自己，杜绝幽灵进程。
//!
//! 前端采用 pull 模型（invoke 返回值即状态），不推送事件。

use std::sync::atomic::Ordering;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::AppState;
use crate::core::ai::AiSession;

/// 等待 dsh web 输出启动 URL 的超时
const START_TIMEOUT: Duration = Duration::from_secs(60);

/// 会话状态快照
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiStatus {
    pub running: bool,
    /// 子 WebView 导航目标（host 已规范为 localhost，见 core::ai）
    pub url: Option<String>,
    pub pid: Option<u32>,
    pub cwd: Option<String>,
    /// dsh CLI 是否可用（未运行时现场检测）
    pub dsh_found: bool,
}

/// 组装状态快照（不持锁调用 dsh 检测）
fn snapshot(state: &AppState, app: &AppHandle) -> AiStatus {
    let (running, url, pid, cwd) = match state.ai.lock() {
        Ok(mut h) => match h.as_mut() {
            Some(s) => {
                // alive() 现场探测子进程（try_wait）：只看槽里有没有会话，
                // 会把已崩溃的 dsh 一直报成"运行中"（前端永远显示会话态）
                let alive = s.alive();
                if !alive {
                    log::warn!("[ai] 会话进程已退出（pid={}），状态将报告为未运行", s.pid);
                }
                (alive, Some(s.url.clone()), Some(s.pid), s.cwd.clone())
            }
            None => (false, None, None, None),
        },
        Err(e) => {
            log::error!("[ai] 状态锁中毒: {}", e);
            (false, None, None, None)
        }
    };
    let dsh_found = if running { true } else { crate::core::ai::dsh_available(app) };
    AiStatus { running, url, pid, cwd, dsh_found }
}

/// 查询会话状态
#[tauri::command]
pub async fn ai_status(app: AppHandle) -> Result<AiStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        Ok(snapshot(&state, &app))
    }).await.map_err(|e| format!("查询 AI 会话状态任务失败: {}", e))?
}

/// 启动（或按需重启）dsh web 会话。cwd：会话工作目录；name：项目显示名
/// （注册 dsh workspace 标题用，使 GUI 工作区名与 Nexus 项目列表一致）
#[tauri::command]
pub async fn ai_start(app: AppHandle, cwd: Option<String>, name: Option<String>) -> Result<AiStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let cwd_norm: Option<String> = cwd.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let name_norm: Option<String> = name.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

        // 目录不存在时提前失败：否则 dsh 在无效工作目录里启动，报错信息指向不明
        if let Some(dir) = cwd_norm.as_deref() {
            if !std::path::Path::new(dir).is_dir() {
                return Err(format!("项目目录不存在: {}", dir));
            }
        }

        // dsh 缺失：快速失败并提示安装（避免等满 60 秒超时才报错）
        if !crate::core::ai::dsh_available(&app) {
            return Err("未检测到 dsh（DeepSeek Harness CLI），请先安装：npm i -g @deepseek-ai/dsh，然后重试".into());
        }

        // 幂等：运行中且目录一致且进程存活 → 复用；进程已死则重启（崩溃自愈）
        let prev: Option<AiSession> = {
            let mut h = state.ai.lock().map_err(|e| format!("获取 AI 会话锁失败: {}", e))?;
            if let Some(s) = h.as_mut() {
                if s.cwd == cwd_norm {
                    if s.alive() {
                        return Ok(AiStatus {
                            running: true,
                            url: Some(s.url.clone()),
                            pid: Some(s.pid),
                            cwd: s.cwd.clone(),
                            dsh_found: true,
                        });
                    }
                    log::info!("[ai] 会话进程已退出 (pid={})，重启", s.pid);
                }
            }
            h.take()
        };
        if let Some(mut old) = prev {
            log::info!("[ai] 会话目录变化或进程已退出，停止旧会话 (pid={})", old.pid);
            old.stop();
        }

        // 旧进程已停（无文件竞争）→ 让 dsh GUI 启动时选中本项目：
        // 注册 workspace（title=项目名）+ bump 本项目会话活动时间（recentWorkspace
        // 按会话时间选最近 —— 对有/无历史会话的项目都强制选中当前项目）。
        // 尽力而为：失败不阻塞启动
        if let Some(dir) = cwd_norm.as_deref() {
            crate::core::ai::ensure_workspace_active(dir, name_norm.as_deref());
        }

        // 代数递增：使任何进行中的旧启动自动放弃
        let gen = state.ai_epoch.fetch_add(1, Ordering::SeqCst) + 1;

        let session = match crate::core::ai::spawn_dsh_web(&app, cwd_norm.as_deref(), START_TIMEOUT) {
            Ok(s) => s,
            Err(e) => {
                log::error!("[ai] 启动 dsh web 失败: {}", e);
                return Err(e);
            }
        };

        // 入库前复查代数：期间有 stop/新 start → 终止本次进程防幽灵
        let mut pending = Some(session);
        {
            let mut h = state.ai.lock().map_err(|e| format!("获取 AI 会话锁失败: {}", e))?;
            if state.ai_epoch.load(Ordering::SeqCst) == gen {
                *h = pending.take();
            }
        }
        if let Some(mut abandoned) = pending {
            log::warn!("[ai] 启动被更新的请求取代，终止本次 dsh (pid={})", abandoned.pid);
            abandoned.stop();
            return Err("已取消：会话被更新的请求取代".into());
        }

        let status = snapshot(&state, &app);
        log::info!("[ai] dsh web 就绪 (pid={})", status.pid.unwrap_or(0));
        Ok(status)
    }).await.map_err(|e| format!("启动 AI 会话任务失败: {}", e))?
}

/// 停止当前会话（若有）：递增代数（在途启动自杀）→ 取走 → 锁外终止进程树。
/// ai_stop 与 ai_upgrade_dsh 共用（升级前必须停会话释放 Windows 文件锁）
fn stop_current_session(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    state.ai_epoch.fetch_add(1, Ordering::SeqCst);
    let old = {
        let mut h = state.ai.lock().map_err(|e| format!("获取 AI 会话锁失败: {}", e))?;
        h.take()
    };
    if let Some(mut s) = old {
        log::info!("[ai] 停止会话 (pid={})", s.pid);
        s.stop();
    }
    Ok(())
}

/// 停止会话
#[tauri::command]
pub async fn ai_stop(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || stop_current_session(&app))
        .await
        .map_err(|e| format!("停止 AI 会话任务失败: {}", e))?
}

/// dsh 版本信息（面板「检查更新」）
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DshVersionInfo {
    /// dsh CLI 是否已安装（未安装时 current 为 None）
    pub dsh_found: bool,
    /// 本地安装版本（dsh --version 解析）
    pub current: Option<String>,
    /// npm registry 最新稳定版
    pub latest: Option<String>,
    /// latest > current（两者都可得时才为 true）
    pub outdated: bool,
}

/// 检查 dsh 更新：本地版本 + npm 最新版（联网，失败返回错误供前端提示）
#[tauri::command]
pub async fn ai_check_update(app: AppHandle) -> Result<DshVersionInfo, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if !crate::core::ai::dsh_available(&app) {
            // 未安装：无需联网查，UI 直接给「安装」入口
            return Ok(DshVersionInfo { dsh_found: false, current: None, latest: None, outdated: false });
        }
        let current = crate::core::ai::query_dsh_version(&app);
        let latest = crate::core::ai::query_dsh_latest(&app)?;
        let outdated = current.as_ref()
            .map(|c| crate::core::ai::version_cmp(c, &latest) == Some(std::cmp::Ordering::Less))
            .unwrap_or(false);
        Ok(DshVersionInfo { dsh_found: true, current, latest: Some(latest), outdated })
    })
    .await
    .map_err(|e| format!("检查 dsh 更新任务失败: {}", e))?
}

/// 升级/安装 dsh 到最新版。先停当前会话（Windows 文件锁，npm 无法覆盖运行中文件）。
/// 返回安装后的 dsh 版本（读取失败返回空串，前端降级文案）
#[tauri::command]
pub async fn ai_upgrade_dsh(app: AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        stop_current_session(&app)?;
        crate::core::ai::upgrade_dsh(&app)?;
        Ok(crate::core::ai::query_dsh_version(&app).unwrap_or_default())
    })
    .await
    .map_err(|e| format!("升级 dsh 任务失败: {}", e))?
}

/// 创建 AI 面板的内嵌子 WebView（带文档创建时注入的焦点记忆/恢复脚本）。
///
/// 与 JS 侧 `new Webview(...)`（plugin:webview|create_webview，unstable）等价，
/// 额外用 initialization_script 安装页面内焦点恢复器（scripts/ai_focus_restore.js）。
/// 为什么必须 Rust 侧：@tauri-apps/api 的 Webview 类没有 eval/initializationScript
/// 入口，注入只能走 Rust。
///
/// 参数较多是 IPC 契约决定的：这些字段由前端 `invoke` 逐个传入，合并成结构体会同时改变
/// 前端调用形状（属界面契约变更）。逐项豁免并说明理由，而不是全局关闭该 lint。
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn create_ai_panel_webview(
    app: AppHandle,
    window_label: String,
    label: String,
    url: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    let window = app
        .get_window(&window_label)
        .ok_or_else(|| format!("窗口不存在: {}", window_label))?;
    // URL 白名单：本命令在内嵌浏览器里加载任意 URL——不校验等于把"能渲染任意网页的
    // 窗口"开放给调用方（钓鱼/本地服务探测）。用 tauri::Url 解析而非字符串前缀匹配：
    // 前缀匹配挡不住 `http://localhost.evil.com`、`@` 用户信息段等写法。
    let parsed: tauri::Url = url.parse().map_err(|e| format!("URL 解析失败 ({}): {}", url, e))?;
    let host_ok = matches!(parsed.host_str(), Some("localhost") | Some("127.0.0.1") | Some("::1"));
    if !host_ok || !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!("AI 面板 URL 不允许: {}", url));
    }
    let url = tauri::WebviewUrl::External(parsed);
    // 几何值来自前端 invoke 参数：非有限值（NaN/±inf）与负坐标会让 WebView 创建失败
    // 或落在屏幕外；尺寸下限 1px（0 尺寸同样创建失败）
    let x = if x.is_finite() { x.max(0.0) } else { 0.0 };
    let y = if y.is_finite() { y.max(0.0) } else { 0.0 };
    let width = if width.is_finite() { width.max(1.0) } else { 1.0 };
    let height = if height.is_finite() { height.max(1.0) } else { 1.0 };
    let builder = tauri::webview::WebviewBuilder::new(label, url)
        // HTML5 drag and drop（与 JS 创建时 dragDropEnabled: false 同义）
        .disable_drag_drop_handler()
        // 文档创建时注入焦点记忆/恢复脚本（每次导航重新执行，幂等）
        .initialization_script(include_str!("../scripts/ai_focus_restore.js"));
    window
        .add_child(
            builder,
            tauri::LogicalPosition::new(x, y),
            tauri::LogicalSize::new(width, height),
        )
        .map_err(|e| format!("创建 AI 面板 WebView 失败: {}", e))?;
    Ok(())
}

/// Alt+Tab 切回后把系统键盘焦点交给 AI 面板子 WebView。
///
/// 为什么不用 tauri/wry 的 set_focus：其 Windows 底层是
/// CoreWebView2Controller::MoveFocus(Programmatic)——语义是「页面内 Tab 焦点循环
/// 移动」而非「交键盘焦点给控件」，实测破坏输入。正确做法：定位 WebView2 控件
/// 窗口（窗口类 "Chrome_WidgetWin_0"，探测确认与面板槽位矩形一致、属于应用进程
/// 主线程，SetFocus 合法）后调用 Win32 SetFocus。控件获得焦点后页面触发
/// window focus 事件 → 注入脚本自动恢复输入框（双保险）。
///
/// 非 Windows 平台无对应实现：命令仍在注册表中（保持跨平台可编译），调用返回错误。
#[tauri::command]
pub async fn ai_panel_focus(app: AppHandle, window_label: String, label: String) -> Result<(), String> {
    // 命令本体不带 cfg：lib.rs 的 generate_handler! 无条件注册本命令，
    // 函数被 cfg 掉会让非 Windows 目标整体编译失败。平台分支放在函数体内。
    #[cfg(windows)]
    {
        let window = app
            .get_window(&window_label)
            .ok_or_else(|| format!("窗口不存在: {}", window_label))?;
        let webview = app.get_webview(&label).ok_or_else(|| format!("AI 面板 WebView 不存在: {}", label))?;
        let w2 = window.clone();
        let wv2 = webview.clone();
        // run_on_main_thread：EnumChildWindows/SetFocus 的窗口属于 UI 线程，
        // SetFocus 要求调用线程与目标窗口同线程（或附加输入队列）——必须在主线程执行
        window
            .run_on_main_thread(move || {
                let res = do_panel_focus(&w2, &wv2);
                match &res {
                    Ok(()) => log::info!("[ai] 系统焦点已交给 dsh 面板"),
                    Err(e) => log::warn!("[ai] 恢复 dsh 系统焦点失败: {}", e),
                }
            })
            .map_err(|e| format!("主线程调度失败: {}", e))
    }
    #[cfg(not(windows))]
    {
        // 其他平台没有 WebView2 控件窗口，也就没有 SetFocus 的等价做法
        let _ = (app, window_label, label);
        Err("系统焦点恢复仅 Windows 支持".into())
    }
}

#[cfg(windows)]
fn do_panel_focus(window: &tauri::Window, webview: &tauri::Webview) -> Result<(), String> {
    unsafe {
        // 1. 主窗口句柄 + 客户区屏幕原点（webview 的 position 相对客户区）
        let handle = match window.hwnd() {
            Ok(h) => h,
            Err(e) => return Err(format!("获取主窗口句柄失败: {}", e)),
        };
        let hwnd = handle.0 as isize;
        let mut pt = Point { x: 0, y: 0 };
        ClientToScreen(hwnd, &mut pt);

        let wpos = webview
            .position()
            .map_err(|e| format!("获取 WebView 位置失败: {}", e))?;
        let wsize = webview
            .size()
            .map_err(|e| format!("获取 WebView 尺寸失败: {}", e))?;
        let target = (
            pt.x + wpos.x,
            pt.y + wpos.y,
            pt.x + wpos.x + wsize.width as i32,
            pt.y + wpos.y + wsize.height as i32,
        );

        // 2. 枚举主窗口子窗口，按矩形精确匹配 WebView2 控件窗口（±2px 容差）
        let mut ctx = EnumCtx { target, found: 0 };
        EnumChildWindows(hwnd, Some(enum_proc), &mut ctx as *mut _ as isize);
        if ctx.found == 0 {
            return Err(format!("未找到 AI 面板 WebView 控件窗口 (target={:?})", target));
        }

        // 3. 键盘焦点交给控件（同线程队列，合法且是标准做法）
        let _prev = SetFocus(ctx.found);
        log::debug!("[ai] SetFocus hwnd={} (prev focus={:?})", ctx.found, _prev);
        Ok(())
    }
}

/// WebView2 控件窗口类名（Windows 内部实现细节，类名多年稳定）
#[cfg(windows)]
const WEBVIEW2_CONTROL_CLASS: &str = "Chrome_WidgetWin_0";

#[cfg(windows)]
#[repr(C)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[cfg(windows)]
#[repr(C)]
struct Point {
    x: i32,
    y: i32,
}

#[cfg(windows)]
struct EnumCtx {
    target: (i32, i32, i32, i32),
    found: isize,
}

#[cfg(windows)]
unsafe extern "system" fn enum_proc(hwnd: isize, l_param: isize) -> i32 {
    let ctx = &mut *(l_param as *mut EnumCtx);
    let mut buf = [0u16; 64];
    let n = GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
    if n <= 0 {
        return 1;
    }
    let name = String::from_utf16_lossy(&buf[..n as usize]);
    if name != WEBVIEW2_CONTROL_CLASS {
        return 1;
    }
    let mut r = Rect { left: 0, top: 0, right: 0, bottom: 0 };
    if GetWindowRect(hwnd, &mut r) == 0 {
        return 1;
    }
    let (tl, tt, tr, tb) = ctx.target;
    if (r.left - tl).abs() <= 2
        && (r.top - tt).abs() <= 2
        && (r.right - tr).abs() <= 2
        && (r.bottom - tb).abs() <= 2
    {
        ctx.found = hwnd;
    }
    1
}

#[cfg(windows)]
extern "system" {
    fn EnumChildWindows(
        hwnd_parent: isize,
        lp_enum_func: Option<unsafe extern "system" fn(isize, isize) -> i32>,
        l_param: isize,
    ) -> i32;
    fn GetClassNameW(hwnd: isize, lp_class_name: *mut u16, n_max_count: i32) -> i32;
    fn GetWindowRect(hwnd: isize, lp_rect: *mut Rect) -> i32;
    fn ClientToScreen(hwnd: isize, lp_point: *mut Point) -> i32;
    fn SetFocus(hwnd: isize) -> isize;
}
