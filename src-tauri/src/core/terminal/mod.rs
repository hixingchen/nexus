pub mod session;
pub mod config;

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use self::session::TerminalSession;
use self::config::TerminalConfig;

#[cfg(windows)]
use crate::core::job_object::JobObject;

/// 终端管理器
pub struct TerminalManager {
    sessions: HashMap<String, Arc<Mutex<TerminalSession>>>,
    config: TerminalConfig,
    #[cfg(windows)]
    job: Option<Arc<JobObject>>,
}

impl TerminalManager {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            config: TerminalConfig::default(),
            #[cfg(windows)]
            job: None,
        }
    }

    /// 设置 Job Object（用于确保子进程在应用退出时被终止）
    #[cfg(windows)]
    pub fn set_job(&mut self, job: Arc<JobObject>) {
        self.job = Some(job);
    }

    /// 创建新的终端会话
    pub fn create_session(
        &mut self,
        session_id: String,
        working_dir: Option<String>,
        shell: Option<String>,
        env_vars: HashMap<String, String>,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<()> {
        use self::session::CreateSessionConfig;

        let shell = shell.unwrap_or_else(|| session::get_default_shell());

        let cfg = CreateSessionConfig {
            session_id: session_id.clone(),
            working_dir,
            shell,
            env_vars,
            cols,
            rows,
            term_type: self.config.term_type.clone(),
            #[cfg(windows)]
            job: self.job.clone(),
        };

        let session = TerminalSession::new(cfg)?;

        self.sessions.insert(session_id, Arc::new(Mutex::new(session)));
        Ok(())
    }

    /// 获取终端会话
    pub fn get_session(&self, session_id: &str) -> Option<Arc<Mutex<TerminalSession>>> {
        self.sessions.get(session_id).cloned()
    }

    /// 移除终端会话（返回 Arc，由调用方负责 close）
    pub fn remove_session(&mut self, session_id: &str) -> Option<Arc<Mutex<TerminalSession>>> {
        self.sessions.remove(session_id)
    }

    /// 获取当前会话数量
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// 关闭所有终端会话（应用退出时调用）
    /// 使用 try_lock 循环重试，因为 reader 线程可能短暂持有锁
    pub async fn close_all(&mut self) {
        for (_, session) in self.sessions.drain() {
            // 最多重试 50 次（共 500ms），等待 reader 线程释放锁
            for _ in 0..50 {
                if let Ok(mut sess) = session.try_lock() {
                    let _ = sess.close();
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
    }

}
