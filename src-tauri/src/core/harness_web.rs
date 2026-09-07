//! 内嵌 DeepSeek Harness Web GUI 的进程管理
//!
//! 启动 `dsh --profile web --no-open --port 0`（OS 分配端口，仅绑 127.0.0.1），
//! 从 stdout 解析启动行：
//!
//!   dsh web: http://127.0.0.1:<port>/?token=<token>
//!
//! 该 URL 含一次性 token：WebView 首次导航时 dsh 签发会话 cookie 并重定向到干净首页。
//! 之后可用同一 URL 重新导航（token 每次进程不同，故每次重启都需重新取 URL）。

use std::io::{BufRead, BufReader};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::AppHandle;
#[cfg(windows)]
use tauri::Manager;

use crate::core::process::build_command;

/// 一次内嵌 Harness Web GUI 会话（一个 dsh web 子进程）
pub struct HarnessWeb {
    child: Mutex<Child>,
    pid: u32,
    /// 带 token 的启动 URL（登录一次后 cookie 常驻）
    pub url: String,
    /// 启动时的工作目录（前端据此判断会话归属项目；空 = 未指定）
    pub cwd: String,
}

impl HarnessWeb {
    /// 启动 dsh web 并阻塞等待 stdout 出现 `dsh web: <url>` 行（上限 timeout）。
    ///
    /// cwd 为空 → 不设置工作目录（dsh 使用调用方目录）；调用方应尽量传入项目根。
    pub fn start(app: AppHandle, cwd: &str, timeout: Duration) -> Result<Self, String> {
        log::info!("[harness] 启动 dsh web (cwd={:?}) ...", if cwd.is_empty() { None } else { Some(cwd) });

        let mut cmd = build_command("dsh --profile web --no-open --port 0");
        if !cwd.trim().is_empty() {
            cmd.current_dir(cwd);
        }
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("无法启动 dsh web（{}）。请确认已全局安装 dsh（npm i -g @deepseek-ai/dsh）", e))?;

        // 加入共享 Job Object（KILL_ON_JOB_CLOSE）：应用退出时兜底清理进程树
        #[cfg(windows)]
        if let Some(job) = app.state::<crate::AppState>().process_mgr.job_arc() {
            job.assign_child(&mut child);
        }
        let pid = child.id();

        let stdout = child.stdout.take().ok_or_else(|| "无法获取 dsh web stdout".to_string())?;
        let stderr = child.stderr.take().ok_or_else(|| "无法获取 dsh web stderr".to_string())?;

        // stderr → 诊断日志 + 共享缓冲（启动失败时把摘要带回给调用方）
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        {
            let buf = Arc::clone(&stderr_buf);
            let pid_err = pid;
            std::thread::Builder::new()
                .name(format!("nexus-harness-{}-stderr", pid))
                .spawn(move || {
                    let reader = BufReader::new(stderr);
                    for line in reader.lines().map_while(Result::ok) {
                        log::info!("[harness#{}] {}", pid_err, line);
                        if let Ok(mut b) = buf.lock() {
                            b.push_str(&line);
                            b.push('\n');
                            if b.len() > 2000 {
                                let cut = b.len() - 2000;
                                let from = b.floor_char_boundary(cut);
                                b.drain(..from);
                            }
                        }
                    }
                })
                .map_err(|e| {
                    crate::core::process::kill_process_tree(pid);
                    format!("创建 dsh web stderr 读取线程失败: {}", e)
                })?;
        }

        // stdout → 找 `dsh web: <url>` 行（解析到即唤醒等待者）
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let pid_out = pid;
        std::thread::Builder::new()
            .name(format!("nexus-harness-{}-stdout", pid))
            .spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().map_while(Result::ok) {
                    let line = line.trim();
                    if let Some(url) = parse_web_url(line) {
                        let _ = tx.send(url);
                        return;
                    }
                    if !line.is_empty() {
                        log::debug!("[harness#{}] stdout: {}", pid_out, line);
                    }
                }
                // EOF 且没等到 URL：通知失败
                let _ = tx.send(String::new());
            })
            .map_err(|e| {
                crate::core::process::kill_process_tree(pid);
                format!("创建 dsh web stdout 读取线程失败: {}", e)
            })?;

        match rx.recv_timeout(timeout) {
            Ok(url) if !url.is_empty() => {
                log::info!("[harness#{}] dsh web 就绪: {}", pid, mask_token(&url));
                Ok(Self { child: Mutex::new(child), pid, url, cwd: cwd.trim().to_string() })
            }
            Ok(_) => {
                crate::core::process::kill_process_tree(pid);
                let detail = stderr_buf.lock().map(|b| b.trim().to_string()).unwrap_or_default();
                let detail = if detail.is_empty() { "（无 stderr 输出，可能为超时或权限问题）".to_string() } else { detail };
                Err(format!("dsh web 提前退出，未能启动：{}", detail))
            }
            Err(_) => {
                crate::core::process::kill_process_tree(pid);
                let detail = stderr_buf.lock().map(|b| b.trim().to_string()).unwrap_or_default();
                let detail = if detail.is_empty() { String::new() } else { format!("（{}）", detail) };
                Err(format!("等待 dsh web 启动地址超时（{}s）{}", timeout.as_secs(), detail))
            }
        }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// 子进程是否仍在运行（意外退出时据此清理）
    pub fn alive(&self) -> bool {
        self.child.lock().map(|mut c| matches!(c.try_wait(), Ok(None))).unwrap_or(false)
    }

    /// 停止进程（taskkill /T /F + wait，与 ProcessManager 收尾一致）
    pub fn stop(&mut self) {
        let pid = self.pid;
        log::info!("[harness#{}] 停止 dsh web...", pid);
        if let Ok(mut c) = self.child.lock() {
            let alive = matches!(c.try_wait(), Ok(None));
            if alive {
                crate::core::process::kill_process_tree(pid);
            }
            let _ = c.wait();
        }
    }
}

/// 从一行 stdout 解析 `dsh web: <url>`；不匹配返回 None
fn parse_web_url(line: &str) -> Option<String> {
    let prefix = "dsh web: ";
    let s = line.strip_prefix(prefix)?.trim();
    if s.starts_with("http://") || s.starts_with("https://") {
        Some(s.to_string())
    } else {
        None
    }
}

/// 日志脱敏：URL 只保留到 ?token= 之前
fn mask_token(url: &str) -> String {
    match url.split_once("?token=") {
        Some((base, _)) => format!("{}?token=<redacted>", base),
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_web_url_ok() {
        let line = "dsh web: http://127.0.0.1:14588/?token=abc123";
        assert_eq!(parse_web_url(line).unwrap(), "http://127.0.0.1:14588/?token=abc123");
    }

    #[test]
    fn test_parse_web_url_rejects_other_lines() {
        assert_eq!(parse_web_url("VITE ready in 123ms"), None);
        assert_eq!(parse_web_url(""), None);
        assert_eq!(parse_web_url("dsh web: hello"), None);
    }

    #[test]
    fn test_mask_token_redacts() {
        let masked = mask_token("http://127.0.0.1:1/?token=SECRET");
        assert!(!masked.contains("SECRET"));
        assert!(masked.contains("<redacted>"));
    }
}
