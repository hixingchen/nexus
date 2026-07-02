use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Arc;

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};

#[cfg(windows)]
use crate::core::job_object::JobObject;

/// 终端会话状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum SessionState {
    /// 运行中
    Running,
    /// 已退出
    Exited(Option<u32>),
}

/// 终端会话
pub struct TerminalSession {
    id: String,
    title_changed: Option<String>,
    state: SessionState,
    master: Option<Box<dyn MasterPty + Send>>,
    child: Box<dyn Child + Send + Sync>,
    reader: Option<Box<dyn Read + Send>>,
    writer: Option<Box<dyn Write + Send>>,
}

/// 终端配置参数
pub struct CreateSessionConfig {
    pub session_id: String,
    pub working_dir: Option<String>,
    pub shell: String,
    pub env_vars: HashMap<String, String>,
    pub cols: u16,
    pub rows: u16,
    pub term_type: String,
    #[cfg(windows)]
    pub job: Option<Arc<JobObject>>,
}

impl TerminalSession {
    /// 创建新的终端会话
    pub fn new(cfg: CreateSessionConfig) -> Result<Self> {
        let pty_system = native_pty_system();

        let pty = pty_system
            .openpty(PtySize {
                rows: cfg.rows,
                cols: cfg.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("无法创建 PTY")?;

        let mut cmd = CommandBuilder::new(&cfg.shell);

        // 设置工作目录
        if let Some(dir) = &cfg.working_dir {
            cmd.cwd(dir);
        }

        // 注入环境变量
        for (key, value) in &cfg.env_vars {
            cmd.env(key, value);
        }

        // 注入 Nexus 特定的环境变量
        cmd.env("NEXUS_TERM", "true");
        cmd.env("TERM_PROGRAM", "nexus");
        cmd.env("TERM", &cfg.term_type);
        cmd.env("COLORTERM", "truecolor");

        // Windows 特殊处理
        #[cfg(target_os = "windows")]
        {
            cmd.env("CHCP", "65001");
        }

        let child = pty
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("无法启动 shell: {}", cfg.shell))?;

        // 将子进程加入 Job Object（确保应用退出时子进程被终止）
        #[cfg(windows)]
        if let Some(ref job) = cfg.job {
            if let Some(pid) = child.process_id() {
                job.assign_by_pid(pid);
            }
        }

        let reader = pty
            .master
            .try_clone_reader()
            .context("无法获取终端读取器")?;

        let writer = pty
            .master
            .take_writer()
            .context("无法获取终端写入器")?;

        Ok(TerminalSession {
            id: cfg.session_id,
            title_changed: None,
            state: SessionState::Running,
            master: Some(pty.master),
            child,
            reader: Some(reader),
            writer: Some(writer),
        })
    }

    /// 向终端写入数据
    pub fn write(&mut self, data: &str) -> Result<()> {
        if let Some(ref mut w) = self.writer {
            w.write_all(data.as_bytes())?;
            w.flush()?;
        }
        Ok(())
    }

    /// 调整终端大小
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        if let Some(ref mut master) = self.master {
            master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })?;
        }
        Ok(())
    }

    /// 关闭终端会话
    /// 1. 标记为 Exited 状态
    /// 2. 杀死子进程并等待其退出（确保 reader 线程收到 EOF）
    /// 3. 关闭 PTY reader/writer，强制解除 reader 线程的阻塞读取
    /// 4. 关闭 PTY master（释放伪控制台句柄 → conhost.exe 退出）
    pub fn close(&mut self) -> Result<()> {
        self.state = SessionState::Exited(None);
        if let Err(e) = self.child.kill() {
            log::warn!("[nexus] kill 子进程失败 ({}): {}", self.id, e);
        }
        // 带超时等待子进程退出（最多 2 秒），避免无限阻塞
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        log::warn!("[nexus] 子进程退出超时 ({}), 强制继续清理", self.id);
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(e) => {
                    log::warn!("[nexus] try_wait 失败 ({}): {}", self.id, e);
                    break;
                }
            }
        }
        // 强制关闭读写端，确保 spawn_blocking 中的 reader 线程能立即退出
        drop(self.reader.take());
        drop(self.writer.take());
        // 关闭 master 释放伪控制台句柄（Windows 上这会导致 conhost.exe 退出）
        drop(self.master.take());
        Ok(())
    }

    /// 取出并清除待发送的标题变更
    pub fn take_title_changed(&mut self) -> Option<String> {
        self.title_changed.take()
    }

    /// 取出 PTY reader（所有权转移给调用方，session 不再持有 reader）
    pub fn take_reader(&mut self) -> Option<Box<dyn Read + Send>> {
        self.reader.take()
    }

    /// 非阻塞检查子进程退出状态（不阻塞，立即返回）
    pub fn try_wait_child(&mut self) -> Option<u32> {
        if let Ok(Some(status)) = self.child.try_wait() {
            let code = status.exit_code();
            self.state = SessionState::Exited(Some(code));
            Some(code)
        } else {
            None
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if matches!(self.state, SessionState::Running) {
            let _ = self.close();
        }
    }
}

/// 获取系统默认 shell
pub fn get_default_shell() -> String {
    #[cfg(target_os = "windows")]
    {
        // 优先使用 PowerShell，其次 cmd
        std::env::var("SHELL").unwrap_or_else(|_| {
            if which::which("powershell.exe").is_ok() {
                "powershell.exe".to_string()
            } else if which::which("pwsh.exe").is_ok() {
                "pwsh.exe".to_string()
            } else {
                "cmd.exe".to_string()
            }
        })
    }

    #[cfg(not(target_os = "windows"))]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
    }
}
