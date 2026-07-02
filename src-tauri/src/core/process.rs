use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader};
use std::process::{Command, Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use serde::{Deserialize, Serialize};
use tauri::Emitter;

/// 日志缓冲区 key 类型，使用 Arc<str> 避免热路径 String clone
type LogKey = Arc<str>;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(unix)]
use std::os::unix::process::CommandExt as UnixCommandExt;

// ─── Types ──────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct LogLine {
    pub timestamp: String,
    pub stream: String,
    pub text: String,
}

#[derive(Clone, Serialize)]
pub struct ServiceLogPayload {
    pub service_key: String,
    pub stream: String,
    pub data: String,
}

const MAX_LOG_LINES: usize = 5000;
/// 全局最大并发服务数，防止日志缓冲无限增长
const MAX_SERVICES: usize = 50;

// ─── 类型别名 ───────────────────────────────────────────────

/// 进程清理条目：(key, pid, child, stdout_handle, stderr_handle)
type ProcessCleanupEntry = (String, u32, Child, Option<std::thread::JoinHandle<()>>, Option<std::thread::JoinHandle<()>>);

// ─── ProcessManager ─────────────────────────────────────────

struct ProcessInfo {
    child: Child,
    pid: u32,
    stdout_handle: Option<std::thread::JoinHandle<()>>,
    stderr_handle: Option<std::thread::JoinHandle<()>>,
}

pub struct ProcessManager {
    processes: Mutex<HashMap<String, ProcessInfo>>,
    log_buffers: Arc<Mutex<HashMap<LogKey, VecDeque<LogLine>>>>,
    #[cfg(windows)]
    job: Option<Arc<super::job_object::JobObject>>,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            processes: Mutex::new(HashMap::new()),
            log_buffers: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(windows)]
            job: None,
        }
    }

    /// 设置共享的 Job Object（确保子进程在应用退出时被终止）
    #[cfg(windows)]
    pub fn set_job(&mut self, job: Arc<super::job_object::JobObject>) {
        self.job = Some(job);
    }

    pub fn start(&self, key: &str, command: &str, cwd: &str, app_handle: &tauri::AppHandle) -> Result<(), String> {
        log::info!("[nexus] 启动服务: {} (cmd={:?}, cwd={:?})", key, command, cwd);

        // Phase 1: 检查限制，然后释放锁
        {
            let procs = self.processes.lock().map_err(|e| format!("进程管理器锁获取失败: {}", e))?;
            if procs.contains_key(key) {
                return Err(format!("{} 已在运行中", key));
            }
            if procs.len() >= MAX_SERVICES {
                return Err(format!("已达到最大并发服务数 ({})，请先停止其他服务", MAX_SERVICES));
            }
        }

        if let Ok(mut buffers) = self.log_buffers.lock() {
            buffers.remove(key);
        }

        // Phase 2: 在锁外执行 spawn 和线程创建
        let mut cmd = build_command(command);
        if !cwd.is_empty() {
            cmd.current_dir(cwd);
        }

        let mut child = cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("启动失败: {}", e))?;

        #[cfg(windows)]
        if let Some(ref job) = self.job {
            job.assign_child(&child);
        }

        let pid = child.id();
        let stdout = child.stdout.take().ok_or("无法获取 stdout")?;
        let stderr = child.stderr.take().ok_or("无法获取 stderr")?;

        // 使用 Arc<str> 作为 key，避免热路径 String clone（P2 #6）
        let log_buffers = Arc::clone(&self.log_buffers);
        let app_clone = app_handle.clone();
        let key1: LogKey = Arc::from(key);
        let key1_clone = Arc::clone(&key1);
        let stdout_handle = std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                let now = chrono::Utc::now().to_rfc3339();
                // emit 需要 String，这里 clone 无法避免
                let _ = app_clone.emit("service-log", ServiceLogPayload { service_key: key1.to_string(), stream: "stdout".into(), data: line.clone() });
                if let Ok(mut b) = log_buffers.lock() {
                    let e = b.entry(Arc::clone(&key1)).or_default();
                    while e.len() >= MAX_LOG_LINES { e.pop_front(); }
                    e.push_back(LogLine { timestamp: now, stream: "stdout".into(), text: line });
                }
            }
        });

        let log_buffers = Arc::clone(&self.log_buffers);
        let app_clone = app_handle.clone();
        let key2 = Arc::clone(&key1_clone);
        let stderr_handle = std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                let now = chrono::Utc::now().to_rfc3339();
                let _ = app_clone.emit("service-log", ServiceLogPayload { service_key: key2.to_string(), stream: "stderr".into(), data: line.clone() });
                if let Ok(mut b) = log_buffers.lock() {
                    let e = b.entry(Arc::clone(&key2)).or_default();
                    while e.len() >= MAX_LOG_LINES { e.pop_front(); }
                    e.push_back(LogLine { timestamp: now, stream: "stderr".into(), text: line });
                }
            }
        });

        // Phase 3: 重新获取锁，二次检查后插入
        let mut procs = self.processes.lock().map_err(|e| format!("进程管理器锁获取失败: {}", e))?;
        if procs.contains_key(key) {
            // TOCTOU 竞态：另一个线程已插入同 key，清理当前创建的资源
            log::warn!("[nexus] TOCTOU 竞态: {} 已在运行中，清理泄漏的子进程", key);
            kill_process_tree(pid);
            let _ = child.wait();
            let _ = stdout_handle.join();
            let _ = stderr_handle.join();
            return Err(format!("{} 已在运行中", key));
        }
        procs.insert(key.to_string(), ProcessInfo {
            child, pid,
            stdout_handle: Some(stdout_handle), stderr_handle: Some(stderr_handle),
        });
        Ok(())
    }

    pub fn stop(&self, key: &str) -> Result<(), String> {
        log::info!("[nexus] 停止服务: {}", key);

        // Phase 1: 从 map 中移除 entry，释放锁
        let entry: Option<ProcessCleanupEntry> = {
            let mut procs = self.processes.lock().map_err(|e| format!("锁错误: {}", e))?;
            procs.remove(key).map(|mut info| {
                (key.to_string(), info.pid, info.child, info.stdout_handle.take(), info.stderr_handle.take())
            })
        };

        // Phase 2: 在锁外执行清理
        if let Some((_key, pid, mut child, stdout_h, stderr_h)) = entry {
            kill_process_tree(pid);
            if let Some(h) = stdout_h { let _ = h.join(); }
            if let Some(h) = stderr_h { let _ = h.join(); }
            let _ = child.wait();
        }
        if let Ok(mut buffers) = self.log_buffers.lock() {
            let log_key: LogKey = Arc::from(key);
            buffers.remove(&*log_key);
        }
        Ok(())
    }

    pub fn restart(&self, key: &str, command: &str, cwd: &str, app_handle: &tauri::AppHandle) -> Result<(), String> {
        self.stop(key)?;
        // 轮询等待进程完全退出（最多 500ms）
        for _ in 0..10 {
            if !self.running().contains(&key.to_string()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.start(key, command, cwd, app_handle)
    }

    pub fn get_logs(&self, key: &str) -> Vec<LogLine> {
        let log_key: LogKey = Arc::from(key);
        self.log_buffers.lock().ok()
            .and_then(|b| b.get(&*log_key).cloned())
            .map(|deque| deque.into_iter().collect())
            .unwrap_or_default()
    }

    pub fn running(&self) -> Vec<String> {
        // Phase 1: 收集已退出进程并从 map 中移除，释放锁
        let dead: Vec<ProcessCleanupEntry> = {
            let mut procs = match self.processes.lock() {
                Ok(guard) => guard,
                Err(_) => return Vec::new(),
            };
            let mut dead_keys = Vec::new();
            for (k, info) in procs.iter_mut() {
                if !matches!(info.child.try_wait(), Ok(None)) {
                    dead_keys.push(k.clone());
                }
            }
            dead_keys.into_iter().filter_map(|key| {
                procs.remove(&key).map(|mut info| {
                    (key, info.pid, info.child, info.stdout_handle.take(), info.stderr_handle.take())
                })
            }).collect()
        };
        // Phase 2: 在锁外 join 已退出进程的线程
        for (_key, _pid, mut child, stdout_h, stderr_h) in dead {
            let _ = child.wait();
            if let Some(h) = stdout_h { let _ = h.join(); }
            if let Some(h) = stderr_h { let _ = h.join(); }
        }
        // Phase 3: 重新获取锁返回当前运行中的 key
        self.processes.lock()
            .map(|procs| procs.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub fn stop_all(&self) {
        let entries: Vec<ProcessCleanupEntry> = {
            let mut procs = match self.processes.lock() {
                Ok(guard) => guard,
                Err(e) => {
                    log::error!("ProcessManager processes 锁已中毒: {}", e);
                    e.into_inner()
                }
            };
            let count = procs.len();
            if count > 0 {
                log::info!("[nexus] stop_all: 正在终止 {} 个进程...", count);
            }
            procs.drain().map(|(key, mut info)| {
                log::debug!("[nexus]   taskkill /T /F /PID {} ({})", info.pid, key);
                (key, info.pid, info.child, info.stdout_handle.take(), info.stderr_handle.take())
            }).collect()
        };

        for (_key, pid, mut child, stdout_h, stderr_h) in entries {
            kill_process_tree(pid);
            if let Some(h) = stdout_h { let _ = h.join(); }
            if let Some(h) = stderr_h { let _ = h.join(); }
            let _ = child.wait();
        }

        if let Ok(mut buffers) = self.log_buffers.lock() {
            buffers.clear();
        }
        log::info!("[nexus] stop_all: 已完成");
    }

    pub fn stop_by_prefix(&self, prefix: &str) {
        let entries: Vec<ProcessCleanupEntry> = {
            let mut procs = match self.processes.lock() {
                Ok(guard) => guard,
                Err(e) => {
                    log::error!("ProcessManager processes 锁已中毒: {}", e);
                    e.into_inner()
                }
            };
            let keys: Vec<String> = procs.keys()
                .filter(|k| k.starts_with(prefix))
                .cloned()
                .collect();
            keys.into_iter().filter_map(|key| {
                procs.remove(&key).map(|mut info| {
                    (key, info.pid, info.child, info.stdout_handle.take(), info.stderr_handle.take())
                })
            }).collect()
        };

        for (_key, pid, mut child, stdout_h, stderr_h) in entries {
            kill_process_tree(pid);
            if let Some(h) = stdout_h { let _ = h.join(); }
            if let Some(h) = stderr_h { let _ = h.join(); }
            let _ = child.wait();
        }

        // 清理对应前缀的 log_buffers
        if let Ok(mut buffers) = self.log_buffers.lock() {
            buffers.retain(|k, _| !k.as_ref().starts_with(prefix));
        }
    }
}

impl Drop for ProcessManager {
    fn drop(&mut self) {
        self.stop_all();
    }
}

// ─── Utilities ──────────────────────────────────────────────

fn kill_process_tree(pid: u32) {
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        match Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null()).stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
        {
            Ok(mut child) => {
                if let Err(e) = child.wait() {
                    log::error!("[nexus] taskkill wait 失败 (pid={}): {}", pid, e);
                }
            }
            Err(e) => {
                log::error!("[nexus] taskkill spawn 失败 (pid={}): {}", pid, e);
            }
        }
    }
    #[cfg(unix)]
    {
        match Command::new("kill").args(["-TERM", &format!("-{}", pid)])
            .stdout(Stdio::null()).stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => { let _ = child.wait(); }
            Err(e) => { log::error!("[nexus] kill -TERM 失败 (pid={}): {}", pid, e); }
        }
        std::thread::sleep(Duration::from_millis(300));
        match Command::new("kill").args(["-KILL", &format!("-{}", pid)])
            .stdout(Stdio::null()).stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => { let _ = child.wait(); }
            Err(e) => { log::error!("[nexus] kill -KILL 失败 (pid={}): {}", pid, e); }
        }
    }
}

/// 构建子进程命令
///
/// 安全设计：`command_str` 来自用户在项目中配置的服务命令。
/// 信任边界：用户只能管理自己的项目，命令执行在其配置的工作目录中。
/// Windows 上统一通过 cmd /C 执行，确保 npm/pnpm 等脚本能正确解析。
pub fn build_command(command_str: &str) -> Command {
    #[cfg(windows)]
    {
        const FLAGS: u32 = 0x08000000 | 0x00000200;
        let mut c = Command::new("cmd");
        c.args(["/C", command_str]);
        c.creation_flags(FLAGS);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.args(["-c", command_str]);
        c.process_group(0);
        c
    }
}

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kill_process_tree_invalid_pid() {
        // 测试无效 PID 不会 panic
        kill_process_tree(999999999);
    }
}
