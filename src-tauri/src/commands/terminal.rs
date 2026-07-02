use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};

use crate::AppState;

/// 终端读取结果（用于事件驱动的 Read Loop）
enum TerminalReadResult {
    Data(String),
    TitleChanged(String),
    Exit(Option<u32>),
}

/// 终端输出事件
#[derive(Clone, Serialize)]
pub struct TerminalOutputPayload {
    pub session_id: Arc<str>,
    pub data: String,
}

/// 终端退出事件
#[derive(Clone, Serialize)]
pub struct TerminalExitPayload {
    pub session_id: Arc<str>,
    pub exit_code: Option<u32>,
}

/// 终端标题变化事件
#[derive(Clone, Serialize)]
pub struct TerminalTitleChangedPayload {
    pub session_id: Arc<str>,
    pub title: String,
}

/// 终端创建参数
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTerminalParams {
    pub session_id: String,
    pub working_dir: Option<String>,
    pub shell: Option<String>,
    pub env_vars: Option<HashMap<String, String>>,
    pub cols: Option<u16>,
    pub rows: Option<u16>,
    pub init_command: Option<String>,
}

/// 允许的 shell 白名单
const ALLOWED_SHELLS: &[&str] = &[
    "cmd.exe", "powershell.exe", "pwsh.exe", "pwsh",
    "bash", "zsh", "sh", "fish",
];

/// 最大并发终端会话数
const MAX_TERMINAL_SESSIONS: usize = 8;

/// 剥离 Windows canonicalize() 产生的 `\\?\` 前缀，得到普通路径字符串
#[cfg(windows)]
fn strip_unc_prefix(p: &std::path::Path) -> String {
    let s = p.to_string_lossy();
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

/// 验证 shell 是否在白名单内，并返回规范化后的绝对路径
///
/// 安全规则：
///   - 无路径的 shell 名 → 在 PATH 中查找，验证找到的是合法 shell
///   - 带路径的 shell    → 必须存在，文件名必须在白名单内，且位于系统目录下
fn validate_shell(shell: &str) -> Result<std::path::PathBuf, String> {
    let path = std::path::Path::new(shell);
    let has_parent = path.parent().map(|p| !p.as_os_str().is_empty()).unwrap_or(false);

    if has_parent {
        // 带路径的 shell — 规范化后验证
        let canonical = path.canonicalize()
            .map_err(|_| format!("Shell 路径不存在: {}", shell))?;

        let shell_name = canonical.file_name()
            .map(|f| f.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        if !ALLOWED_SHELLS.iter().any(|s| s == &shell_name) {
            return Err(format!("不允许的 shell: {}，允许的 shell: {:?}", shell, ALLOWED_SHELLS));
        }

        // 验证位于系统目录下，防止执行任意路径下的同名恶意程序
        #[cfg(windows)]
        {
            let canonical_str = strip_unc_prefix(&canonical).to_lowercase();
            let win_dir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
            let is_system = canonical_str.starts_with(&win_dir.to_lowercase())
                || canonical_str.contains(r"\windowsapps\");  // Store 版 PowerShell
            if !is_system {
                return Err(format!("Shell 必须位于系统目录下，当前路径: {}", canonical.display()));
            }
        }
        #[cfg(not(windows))]
        {
            let canonical_str = canonical.to_string_lossy().to_lowercase();
            let is_system = canonical_str.starts_with("/usr/bin/")
                || canonical_str.starts_with("/usr/local/bin/")
                || canonical_str.starts_with("/bin/")
                || canonical_str.starts_with("/snap/bin/");
            if !is_system {
                return Err(format!("Shell 必须位于系统目录下，当前路径: {}", canonical.display()));
            }
        }

        return Ok(canonical);
    }

    // 无路径的 shell 名 — 在 PATH 中查找
    let found = which::which(shell)
        .map_err(|_| format!("找不到 shell: {}，请确认已安装", shell))?;

    let shell_name = found.file_name()
        .map(|f| f.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    if !ALLOWED_SHELLS.iter().any(|s| s == &shell_name) {
        return Err(format!("不允许的 shell: {}，允许的 shell: {:?}", shell, ALLOWED_SHELLS));
    }

    Ok(found)
}

/// 验证工作目录合法性
///
/// 安全规则：
///   - 目录必须存在
///   - 不能是系统关键目录（Windows/System32、/etc 等）
///   - 如果提供了项目根目录列表，必须在项目范围内
fn validate_working_dir(dir: &str, project_roots: &[String]) -> Result<std::path::PathBuf, String> {
    let path = std::path::Path::new(dir).canonicalize()
        .map_err(|_| format!("工作目录不存在: {}", dir))?;

    // 禁止在系统关键目录中启动终端
    #[cfg(windows)]
    {
        let canonical_str = strip_unc_prefix(&path).to_lowercase();
        let win_dir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        if canonical_str.starts_with(&win_dir.to_lowercase()) {
            return Err("不允许在 Windows 系统目录中启动终端".into());
        }
    }
    #[cfg(not(windows))]
    {
        let canonical_str = path.to_string_lossy();
        let restricted = ["/etc", "/root", "/var", "/proc", "/sys", "/dev"];
        if restricted.iter().any(|r| canonical_str.starts_with(r)) {
            return Err(format!("不允许在系统目录中启动终端: {}", dir));
        }
    }

    // 如果提供了项目根目录列表，限制在项目范围内
    // 注意：project_roots 来自数据库，也可能带 \\?\ 前缀，统一用 canonicalize 比较
    if !project_roots.is_empty() {
        let allowed = project_roots.iter().any(|root| {
            if let Ok(root_path) = std::path::Path::new(root).canonicalize() {
                path.starts_with(&root_path)
            } else {
                false
            }
        });
        if !allowed {
            return Err(format!("工作目录不在允许的项目范围内: {}", dir));
        }
    }

    Ok(path)
}

/// 危险环境变量 key（禁止用户注入）
const BLOCKED_ENV_KEYS: &[&str] = &[
    // 路径注入
    "PATH", "PATHEXT",
    "LD_PRELOAD", "LD_LIBRARY_PATH",
    "DYLD_INSERT_LIBRARIES", "DYLD_LIBRARY_PATH",
    // Shell/解释器注入
    "PYTHONPATH", "NODE_OPTIONS", "RUSTFLAGS",
    "SHELL", "COMSPEC",
    // 用户目录劫持
    "HOME", "USERPROFILE", "HOMEDRIVE", "HOMEPATH",
    "APPDATA", "LOCALAPPDATA", "XDG_CONFIG_HOME", "XDG_DATA_HOME",
    // Shell 初始化文件
    "IFS", "CDPATH", "ENV", "BASH_ENV",
    // Windows 系统目录
    "SYSTEMROOT", "SYSTEMDRIVE", "WINDIR",
];

/// 创建终端会话
#[tauri::command]
pub async fn create_terminal(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    params: CreateTerminalParams,
) -> Result<(), String> {
    if params.session_id.trim().is_empty() {
        return Err("会话ID不能为空".into());
    }

    // 并发会话数限制
    {
        let manager = state.terminal_mgr.lock().await;
        if manager.session_count() >= MAX_TERMINAL_SESSIONS {
            return Err(format!("终端会话数已达上限 ({})，请先关闭不用的终端", MAX_TERMINAL_SESSIONS));
        }
    }

    // 验证 shell 白名单 + 路径合法性
    if let Some(ref shell) = params.shell {
        validate_shell(shell)?;
    }

    // 验证工作目录（查询项目路径列表，用于限制终端工作目录范围）
    if let Some(ref dir) = params.working_dir {
        let project_roots: Vec<String> = state.db.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT path FROM projects")
                .map_err(|e| format!("查询项目列表失败: {}", e))?;
            let paths = stmt.query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| format!("读取项目路径失败: {}", e))?
                .filter_map(|r| r.ok())
                .collect();
            Ok(paths)
        })?;
        validate_working_dir(dir, &project_roots)?;
    }

    // 过滤危险环境变量
    let env_vars: HashMap<String, String> = params.env_vars
        .unwrap_or_default()
        .into_iter()
        .filter(|(k, _)| !BLOCKED_ENV_KEYS.contains(&k.to_uppercase().as_str()))
        .collect();

    let mut manager = state.terminal_mgr.lock().await;
    manager
        .create_session(
            params.session_id.clone(),
            params.working_dir,
            params.shell,
            env_vars,
            params.cols.unwrap_or(80),
            params.rows.unwrap_or(24),
        )
        .map_err(|e| format!("创建终端失败: {}", e))?;

    let session = manager
        .get_session(&params.session_id)
        .ok_or_else(|| format!("终端会话不存在: {}", params.session_id))?;

    // 从 session 中取出 PTY reader，避免 reader 线程持有 session 锁
    let pty_reader = {
        let mut sess = session.lock().await;
        sess.take_reader()
    };
    // 释放 manager 锁，后续通过 session Arc 访问
    drop(manager);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<TerminalReadResult>();

    // 1. 在独立线程中阻塞读取 PTY 输出（不持有 session 锁）
    if let Some(mut reader) = pty_reader {
        let session_for_title = session.clone();
        let _read_thread = tokio::task::spawn_blocking(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        // PTY 读取到 EOF，进程已退出
                        // 尝试获取 session 锁读取退出码
                        let code = if let Ok(mut sess) = session_for_title.try_lock() {
                            sess.try_wait_child()
                        } else {
                            None
                        };
                        let _ = tx.send(TerminalReadResult::Exit(code));
                        break;
                    }
                    Ok(n) => {
                        let data = String::from_utf8_lossy(&buf[..n]).to_string();
                        // 检测标题变更（需要短暂获取 session 锁）
                        if let Ok(mut sess) = session_for_title.try_lock() {
                            if let Some(title) = sess.take_title_changed() {
                                let _ = tx.send(TerminalReadResult::TitleChanged(title));
                            }
                        }
                        let _ = tx.send(TerminalReadResult::Data(data));
                    }
                    Err(ref e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::TimedOut =>
                    {
                        continue;
                    }
                    Err(_) => {
                        let _ = tx.send(TerminalReadResult::Exit(None));
                        break;
                    }
                }
            }
        });
    } else {
        // 没有 reader（不应该发生），直接报告退出
        let _ = tx.send(TerminalReadResult::Exit(None));
    }

    // 2. 异步任务接收读取结果并发射 Tauri 事件
    // 使用 Arc<str> 避免循环中反复 clone String（P2 #4）
    let session_id: Arc<str> = Arc::from(params.session_id.as_str());

    // 初始命令执行逻辑
    let init_command = params.init_command.filter(|s| !s.trim().is_empty());
    let init_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let session_for_init = if init_command.is_some() { Some(session.clone()) } else { None };

    let event_handle = tokio::spawn(async move {
        while let Some(result) = rx.recv().await {
            match result {
                TerminalReadResult::Data(data) => {
                    // 检测提示符并执行初始命令
                    if init_command.is_some() {
                        if !init_done.load(std::sync::atomic::Ordering::Relaxed) {
                            let trimmed = data.trim_end();
                            let is_prompt = trimmed.ends_with('>')
                                || trimmed.ends_with("$")
                                || trimmed.ends_with("#")
                                || trimmed.ends_with("❯")
                                || trimmed.ends_with("%");

                            if is_prompt {
                                log::info!("[nexus] 检测到提示符，准备执行初始命令: {:?}", data);
                                init_done.store(true, std::sync::atomic::Ordering::Relaxed);
                                if let (Some(session), Some(cmd)) = (&session_for_init, &init_command) {
                                    match session.try_lock() {
                                        Ok(mut sess) => {
                                            #[cfg(windows)]
                                            let full_cmd = cmd.clone() + "\r\n";
                                            #[cfg(not(windows))]
                                            let full_cmd = cmd.clone() + "\n";
                                            match sess.write(&full_cmd) {
                                                Ok(_) => log::info!("[nexus] 初始命令已执行"),
                                                Err(e) => log::error!("[nexus] 执行初始命令失败: {}", e),
                                            }
                                        }
                                        Err(e) => log::error!("[nexus] 获取 session 锁失败: {}", e),
                                    }
                                }
                            }
                        }
                    }

                    let _ = app_handle.emit(
                        "terminal-output",
                        TerminalOutputPayload {
                            session_id: Arc::clone(&session_id),
                            data,
                        },
                    );
                }
                TerminalReadResult::TitleChanged(title) => {
                    let _ = app_handle.emit(
                        "terminal-title-changed",
                        TerminalTitleChangedPayload {
                            session_id: Arc::clone(&session_id),
                            title,
                        },
                    );
                }
                TerminalReadResult::Exit(code) => {
                    let _ = app_handle.emit(
                        "terminal-exit",
                        TerminalExitPayload {
                            session_id,
                            exit_code: code,
                        },
                    );
                    break;
                }
            }
        }
    });

    // 存储 AbortHandle 以便关闭时中止事件任务
    if let Ok(mut tasks) = state.event_tasks.lock() {
        tasks.insert(params.session_id.clone(), event_handle.abort_handle());
    } else {
        log::error!("[nexus] event_tasks 锁中毒，无法存储 AbortHandle");
    }

    Ok(())
}

/// 向终端写入数据
#[tauri::command]
pub async fn write_terminal(
    state: State<'_, AppState>,
    session_id: String,
    data: String,
) -> Result<(), String> {
    if session_id.trim().is_empty() {
        return Err("会话ID不能为空".into());
    }

    // 获取 session Arc 后立即释放 manager 锁
    let session = {
        let manager = state.terminal_mgr.lock().await;
        manager
            .get_session(&session_id)
            .ok_or_else(|| format!("终端会话不存在: {}", session_id))?
    };

    let mut session = session.lock().await;
    session
        .write(&data)
        .map_err(|e| format!("写入终端失败: {}", e))
}

/// 调整终端大小
#[tauri::command]
pub async fn resize_terminal(
    state: State<'_, AppState>,
    session_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    if session_id.trim().is_empty() {
        return Err("会话ID不能为空".into());
    }

    // 获取 session Arc 后立即释放 manager 锁
    let session = {
        let manager = state.terminal_mgr.lock().await;
        manager
            .get_session(&session_id)
            .ok_or_else(|| format!("终端会话不存在: {}", session_id))?
    };

    let mut session = session.lock().await;
    session
        .resize(cols, rows)
        .map_err(|e| format!("调整终端大小失败: {}", e))
}

/// 关闭终端会话
#[tauri::command]
pub async fn close_terminal(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<(), String> {
    if session_id.trim().is_empty() {
        return Err("会话ID不能为空".into());
    }

    // 中止事件推送任务
    if let Ok(mut tasks) = state.event_tasks.lock() {
        if let Some(handle) = tasks.remove(&session_id) {
            handle.abort();
        }
    } else {
        log::error!("[nexus] event_tasks 锁中毒，无法中止事件任务");
    }

    // 从 manager 中移除 session，然后释放 manager 锁再执行阻塞 close
    let session = {
        let mut manager = state.terminal_mgr.lock().await;
        manager.remove_session(&session_id)
    };

    if let Some(session) = session {
        // child.wait() 是阻塞操作，必须在 spawn_blocking 中执行避免阻塞 tokio 运行时
        tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Handle::current();
            let mut sess = rt.block_on(session.lock());
            sess.close().map_err(|e| format!("关闭终端失败: {}", e))
        })
        .await
        .map_err(|e| format!("关闭终端任务失败: {}", e))??;
    }
    Ok(())
}

/// 终止终端进程
#[tauri::command]
pub async fn kill_terminal(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<(), String> {
    if session_id.trim().is_empty() {
        return Err("会话ID不能为空".into());
    }

    // 中止事件推送任务
    if let Ok(mut tasks) = state.event_tasks.lock() {
        if let Some(handle) = tasks.remove(&session_id) {
            handle.abort();
        }
    } else {
        log::error!("[nexus] event_tasks 锁中毒，无法中止事件任务");
    }

    // 从 manager 中移除 session，然后释放 manager 锁再执行阻塞 close
    let session = {
        let mut manager = state.terminal_mgr.lock().await;
        manager.remove_session(&session_id)
    };

    if let Some(session) = session {
        // child.wait() 是阻塞操作，必须在 spawn_blocking 中执行避免阻塞 tokio 运行时
        tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Handle::current();
            let mut sess = rt.block_on(session.lock());
            sess.close().map_err(|e| format!("终止终端失败: {}", e))
        })
        .await
        .map_err(|e| format!("终止终端任务失败: {}", e))??;
    }
    Ok(())
}

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // validate_shell 白名单拒绝测试（不依赖 PATH 中是否安装了 shell）

    #[test]
    fn test_validate_shell_rejected_unknown_binary() {
        assert!(validate_shell("python").is_err());
        assert!(validate_shell("node").is_err());
        assert!(validate_shell("malicious.exe").is_err());
        assert!(validate_shell("").is_err());
    }

    #[test]
    fn test_validate_shell_rejected_non_system_path() {
        // 任意路径下的非白名单程序应被拒绝
        assert!(validate_shell("/usr/bin/python").is_err());
        assert!(validate_shell("C:\\Program Files\\evil.exe").is_err());
    }

    // validate_shell 白名单接受测试（仅在 shell 实际存在时通过）

    #[test]
    fn test_validate_shell_system_shell() {
        // Windows: cmd.exe 必须存在
        #[cfg(windows)]
        {
            let result = validate_shell("cmd.exe");
            assert!(result.is_ok(), "cmd.exe 应该在 PATH 中找到: {:?}", result.err());
        }
        // Unix: sh 必须存在
        #[cfg(not(windows))]
        {
            let result = validate_shell("sh");
            assert!(result.is_ok(), "sh 应该在 PATH 中找到: {:?}", result.err());
        }
    }

    #[test]
    fn test_validate_shell_with_system_path() {
        #[cfg(windows)]
        {
            let result = validate_shell(r"C:\Windows\System32\cmd.exe");
            assert!(result.is_ok(), "系统目录下的 cmd.exe 应该通过验证: {:?}", result.err());
        }
        #[cfg(not(windows))]
        {
            let result = validate_shell("/bin/sh");
            assert!(result.is_ok(), "/bin/sh 应该通过验证: {:?}", result.err());
        }
    }

    // validate_working_dir 测试

    #[test]
    fn test_validate_working_dir_nonexistent() {
        let result = validate_working_dir("/nonexistent/path/xyz", &[]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("不存在"));
    }

    #[test]
    fn test_validate_working_dir_current_dir() {
        let cwd = std::env::current_dir().unwrap();
        let result = validate_working_dir(&cwd.to_string_lossy(), &[]);
        assert!(result.is_ok(), "当前目录应该通过验证: {:?}", result.err());
    }

    #[test]
    fn test_validate_working_dir_project_root_restriction() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.to_string_lossy().to_string();

        // 在项目范围内 → 通过
        let result = validate_working_dir(&root, &[root.clone()]);
        assert!(result.is_ok());

        // 不在项目范围内 → 拒绝
        let result = validate_working_dir(&root, &["/some/other/project".into()]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("不在允许的项目范围内"));
    }
}
