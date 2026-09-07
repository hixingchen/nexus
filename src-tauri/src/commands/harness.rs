//! 内嵌 DeepSeek Harness Web GUI 命令
//!
//! harness_start 不在命令线程里阻塞等待 dsh web 就绪（冷启动可达数秒），
//! 而是派后台线程启动；前端通过 harness_status 轮询或订阅事件获知 URL。
//! 事件：
//!   harness-running {running, url?}
//!   harness-error   {error}
//!   harness-dsh-installed {ok, error?}

use std::io::{BufRead, BufReader};
use std::process::Stdio;
use std::time::Duration;

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::core::harness_web::HarnessWeb;
use crate::AppState;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessStatus {
    pub running: bool,
    pub url: Option<String>,
    pub pid: Option<u32>,
    /// dsh CLI 是否存在于 PATH（未安装时前端展示安装引导）
    pub dsh_found: bool,
    /// 会话启动时的工作目录（前端据此判断会话归属项目；None = 未指定）
    pub cwd: Option<String>,
}

/// 检查 `dsh` 是否在 PATH 上（where dsh；结果缓存 1.5s——
/// 前端启动/安装期间会高频轮询 status，不必每次 spawn 子进程）
fn dsh_available() -> bool {
    use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
    static LAST_CHECK_MS: AtomicI64 = AtomicI64::new(0);
    static FOUND: AtomicBool = AtomicBool::new(false);
    const CACHE_MS: i64 = 1500;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    if now_ms - LAST_CHECK_MS.load(Ordering::Relaxed) < CACHE_MS {
        return FOUND.load(Ordering::Relaxed);
    }

    // 并发 miss 时可能重复检测，幂等无害
    let found = {
        let mut child = match crate::core::process::build_command("where dsh")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return false,
        };
        match child.wait() {
            Ok(status) => status.success(),
            Err(_) => false,
        }
    };
    LAST_CHECK_MS.store(now_ms, Ordering::Relaxed);
    FOUND.store(found, Ordering::Relaxed);
    found
}

/// 启动（后台）内嵌 dsh web；已在运行时为幂等 no-op。
/// cwd：当前项目根（可选；为空时 dsh 使用调用方目录）
#[tauri::command]
pub fn harness_start(app: AppHandle, state: State<AppState>, cwd: Option<String>) -> Result<(), String> {
    if !dsh_available() {
        return Err("未检测到 dsh（DeepSeek Harness 运行环境未安装）".into());
    }
    // 确认需要启动后记录 epoch 快照：启动期间若被 harness_stop（epoch 递增），
    // boot 线程完成后据此丢弃新实例，避免「已停止」后幽灵进程复活
    let epoch = state
        .harness_epoch
        .load(std::sync::atomic::Ordering::Relaxed);
    // 后台启动：不阻塞调用线程
    let worker_app = app.clone();
    let worker_cwd = cwd.unwrap_or_default();
    std::thread::Builder::new()
        .name("nexus-harness-boot".into())
        .spawn(move || {
            match HarnessWeb::start(worker_app.clone(), &worker_cwd, Duration::from_secs(60)) {
                Ok(mut hw) => {
                    let url = hw.url.clone();
                    {
                        let s = worker_app.state::<AppState>();
                        let mut guard = s.harness_web.lock().unwrap_or_else(|e| e.into_inner());
                        if s.harness_epoch.load(std::sync::atomic::Ordering::Relaxed) != epoch {
                            // 启动窗口内被并发 stop：停掉刚起的新实例，不塞回槽位
                            drop(guard);
                            hw.stop();
                            return;
                        }
                        match guard.as_mut() {
                            // 并发双启动（无 stop）：保留本轮新实例
                            Some(old) if old.alive() => {
                                old.stop();
                                *guard = Some(hw);
                            }
                            // 槽位为空（首次启动）或旧实例已死（崩溃后重启）：直接接管
                            _ => *guard = Some(hw),
                        }
                        let _ = worker_app.emit("harness-running", json!({ "running": true, "url": url }));
                    }
                }
                Err(e) => {
                    log::error!("[harness] 启动失败: {}", e);
                    let _ = worker_app.emit("harness-error", json!({ "error": e }));
                }
            }
        })
        .map_err(|e| format!("创建 dsh web 启动线程失败: {}", e))?;
    Ok(())
}

/// 内嵌 dsh web 状态（含带 token 的 URL；子进程意外退出时自动清槽）
#[tauri::command]
pub fn harness_status(state: State<AppState>) -> Result<HarnessStatus, String> {
    let found = dsh_available();
    let mut guard = state.harness_web.lock().map_err(|e| format!("状态锁获取失败: {}", e))?;
    match guard.as_mut() {
        Some(r) => {
            if !r.alive() {
                *guard = None;
                return Ok(HarnessStatus { running: false, url: None, pid: None, dsh_found: found, cwd: None });
            }
            Ok(HarnessStatus {
                running: true,
                url: Some(r.url.clone()),
                pid: Some(r.pid()),
                dsh_found: found,
                cwd: Some(r.cwd.clone()),
            })
        }
        None => Ok(HarnessStatus { running: false, url: None, pid: None, dsh_found: found, cwd: None }),
    }
}

/// 后台执行 `npm i -g @deepseek-ai/dsh`（需本机已有 node/npm）。
/// 结束事件：harness-dsh-installed { ok, error? }
#[tauri::command]
pub fn harness_install(app: AppHandle) -> Result<(), String> {
    let worker_app = app.clone();
    std::thread::Builder::new()
        .name("nexus-harness-install".into())
        .spawn(move || {
            let result = run_global_install();
            match result {
                Ok(()) => {
                    log::info!("[harness] dsh 全局安装完成");
                    let _ = worker_app.emit("harness-dsh-installed", json!({ "ok": true }));
                }
                Err(e) => {
                    log::error!("[harness] dsh 全局安装失败: {}", e);
                    let _ = worker_app.emit("harness-dsh-installed", json!({ "ok": false, "error": e }));
                }
            }
        })
        .map_err(|e| format!("创建 dsh 安装线程失败: {}", e))?;
    Ok(())
}

/// npm 全局安装 dsh（带 240s 超时与输出日志）
fn run_global_install() -> Result<(), String> {
    let mut child = crate::core::process::build_command("npm i -g @deepseek-ai/dsh")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法启动 npm（{}）。请先安装 Node.js / npm", e))?;
    let pid = child.id();

    let stdout = child.stdout.take().ok_or("无法获取 npm stdout")?;
    let stderr = child.stderr.take().ok_or("无法获取 npm stderr")?;
    let pid_out = pid;
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            log::info!("[npm#{}] {}", pid_out, line);
        }
    });
    let pid_err = pid;
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            log::warn!("[npm#{}] {}", pid_err, line);
        }
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(240);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                return Err(format!("npm 安装失败（退出码 {:?}），详见日志", status.code()));
            }
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    crate::core::process::kill_process_tree(pid);
                    return Err("npm 安装超时（240s），已终止".into());
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(e) => return Err(format!("等待 npm 失败: {}", e)),
        }
    }
}

/// 停止内嵌 dsh web（优雅 kill 进程树；对话历史保留在 DSH_HOME）
#[tauri::command]
pub fn harness_stop(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    // 先递增 epoch：作废正在后台 boot 的启动线程（它们完成时发现 epoch 已变会自弃）
    state
        .harness_epoch
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut guard = state.harness_web.lock().map_err(|e| format!("状态锁获取失败: {}", e))?;
    if let Some(r) = guard.as_mut() {
        r.stop();
    }
    *guard = None;
    drop(guard);
    let _ = app.emit("harness-running", json!({ "running": false }));
    Ok(())
}

// (dev) 该文件内容变更触发 tauri watcher 整窗重启
