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
fn snapshot(state: &AppState) -> AiStatus {
    let (running, url, pid, cwd) = match state.ai.lock() {
        Ok(h) => match h.as_ref() {
            Some(s) => (true, Some(s.url.clone()), Some(s.pid), s.cwd.clone()),
            None => (false, None, None, None),
        },
        Err(e) => {
            log::error!("[ai] 状态锁中毒: {}", e);
            (false, None, None, None)
        }
    };
    let dsh_found = if running { true } else { crate::core::ai::dsh_available() };
    AiStatus { running, url, pid, cwd, dsh_found }
}

/// 查询会话状态
#[tauri::command]
pub async fn ai_status(app: AppHandle) -> Result<AiStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        Ok(snapshot(&state))
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

        // dsh 缺失：快速失败并提示安装（避免等满 60 秒超时才报错）
        if !crate::core::ai::dsh_available() {
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

        let status = snapshot(&state);
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
        if !crate::core::ai::dsh_available() {
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
