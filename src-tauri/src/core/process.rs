use std::collections::{HashMap, VecDeque};
use std::io::BufReader;
use std::process::{Command, Child, Stdio};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
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
    /// 行产生时间（RFC3339）。之前前端用接收时间且 50ms 批量内同批相同，精度不足
    pub timestamp: String,
}

/// 每服务日志缓冲行数上限（IDEA Console 式：只保留最新 2000 行）
const MAX_LOG_LINES: usize = 2000;
/// 单行日志字节上限（base64/JSON dump 等超长行截断）
const MAX_LINE_BYTES: usize = 8192;
/// 全局最大并发服务数，防止日志缓冲无限增长
const MAX_SERVICES: usize = 50;

/// 按 char 边界截断超长日志行，防止单行超大输出撑爆缓冲和 IPC payload
pub fn truncate_line(mut line: String, max: usize) -> String {
    if line.len() > max {
        let idx = line.floor_char_boundary(max);
        line.truncate(idx);
        line.push_str("… [已截断]");
    }
    line
}

/// 逐行读取子进程输出
///
/// 错误处理语义：
/// - `InvalidData`（非 UTF-8，如 Windows GBK 中文）→ 跳过该行继续读取
///   （`map_while(Result::ok)` 会在此终止整个 reader 线程，日志永久丢失）
/// - 其他 IO 错误（管道损坏、句柄关闭）→ 停止读取，避免 `filter_map` 在
///   持续失败时无限循环空转
fn read_log_lines<R: std::io::BufRead>(reader: R) -> impl Iterator<Item = String> {
    reader.lines().scan((), |_, line| {
        match line {
            Ok(l) => Some(Some(l)),
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => Some(None), // 跳过坏行
            Err(_) => None, // IO 错误：终止
        }
    }).flatten()
}

/// 清理非 SGR 的 ANSI 转义序列
///
/// webpack 进度等 CLI 用 `\x1b[s`/`\x1b[u`（保存/恢复光标）、`\x1b[2K`（清行）、
/// `\x1b[?25l`（隐藏光标）做单行刷新——这些序列渲染时显示为 `s`/`u` 等垃圾字符。
/// 颜色序列（`\x1b[...m`，SGR）保留，供前端着色。
fn clean_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    loop {
        match rest.find('\x1b') {
            None => { out.push_str(rest); break; }
            Some(pos) => {
                out.push_str(&rest[..pos]);
                let tail = &rest[pos..];
                let bytes = tail.as_bytes();
                if bytes.len() >= 2 && bytes[1] == b'[' {
                    // 扫描 CSI 序列：\x1b[ 参数(数字/;/?) 终结符(字母)
                    let mut j = 2;
                    while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';' || bytes[j] == b'?') {
                        j += 1;
                    }
                    if j < bytes.len() && bytes[j].is_ascii_alphabetic() {
                        if bytes[j] == b'm' {
                            out.push_str(&tail[..=j]); // SGR 颜色序列：保留
                        }
                        rest = &tail[j + 1..];
                    } else {
                        out.push_str(&tail[..j]); // 不完整序列按文本保留
                        rest = &tail[j..];
                    }
                } else {
                    out.push('\x1b');
                    rest = &tail[1..];
                }
            }
        }
    }
    out
}

// ─── 类型别名 ───────────────────────────────────────────────

/// 进程清理条目：(key, pid, child, done_stdout, done_stderr)
/// done_* 为 reader 线程结束信号：线程退出时发送端 drop，recv 返回 Disconnected
type ProcessCleanupEntry = (String, u32, Child, Option<Receiver<()>>, Option<Receiver<()>>);

// ─── ProcessManager ─────────────────────────────────────────

struct ProcessInfo {
    child: Child,
    pid: u32,
    /// 所属项目（供 get_running 返回项目级运行状态）
    project_id: String,
    /// reader 线程结束信号（stdout/stderr）
    done_stdout: Option<Receiver<()>>,
    done_stderr: Option<Receiver<()>>,
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

    pub fn start(&self, project_id: &str, key: &str, command: &str, cwd: &str, env_vars: &[(String, String)], app_handle: &tauri::AppHandle) -> Result<(), String> {
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
        if !env_vars.is_empty() {
            cmd.envs(env_vars.iter().map(|(k, v)| (k.as_str(), v.as_str())));
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
        // done 信号：reader 线程结束时发送端 drop → recv 返回 Disconnected。
        // 清理时用 recv_timeout 等待线程退出并限时放弃，避免无限 join 阻塞主线程。
        let log_buffers = Arc::clone(&self.log_buffers);
        let app_clone = app_handle.clone();
        let key1: LogKey = Arc::from(key);
        let key1_clone = Arc::clone(&key1);
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let stdout_thread = std::thread::Builder::new()
            .name(format!("nexus-log-{}-stdout", key))
            .spawn(move || {
                let reader = BufReader::new(stdout);
                // emit 需要 String，提到循环外避免每行重复分配
                let key_string = key1.to_string();
                for line in read_log_lines(reader) {
                    let now = chrono::Utc::now().to_rfc3339();
                    // 刷新行识别：\r、\x1b[s（保存光标）、\x1b[2K/\x1b[K（清行）开头的行
                    // 都是单行刷新（webpack 进度条等），应合并而非逐帧追加
                    let is_refresh = line.starts_with('\r')
                        || line.starts_with("\x1b[s")
                        || line.starts_with("\x1b[2K")
                        || line.starts_with("\x1b[K");
                    let cleaned = clean_ansi(&line);
                    // 刷新行统一以 \r 前缀标记（clean 后原前缀消失，\r 保留供前端合并判断）
                    let line = truncate_line(
                        if is_refresh { format!("\r{}", cleaned.trim_start_matches('\r')) } else { cleaned },
                        MAX_LINE_BYTES,
                    );
                    // 先写缓冲再 emit：get_service_logs 快照必然包含所有已发送事件（前端覆盖合并的前提）
                    if let Ok(mut b) = log_buffers.lock() {
                        let e = b.entry(Arc::clone(&key1)).or_default();
                        if is_refresh {
                            let text = line.trim_start_matches('\r').to_string();
                            if let Some(last) = e.back_mut() {
                                last.text = text.clone();
                                last.timestamp = now.clone();
                            } else {
                                e.push_back(LogLine { timestamp: now.clone(), stream: "stdout".into(), text });
                            }
                        } else {
                            while e.len() >= MAX_LOG_LINES { e.pop_front(); }
                            e.push_back(LogLine { timestamp: now.clone(), stream: "stdout".into(), text: line.clone() });
                        }
                    }
                    // emit 保留 \r 前缀：前端据此合并实时流（与缓冲语义一致）
                    let _ = app_clone.emit("service-log", ServiceLogPayload { service_key: key_string.clone(), stream: "stdout".into(), data: line, timestamp: now });
                }
                drop(done_tx);
            })
            .map_err(|e| {
                // 线程创建失败：杀掉已 spawn 的子进程，避免进程泄漏
                kill_process_tree(pid);
                format!("创建 stdout 日志读取线程失败: {}", e)
            })?;
        let _ = stdout_thread; // 不 join；线程在进程退出（EOF）后自然结束

        let log_buffers = Arc::clone(&self.log_buffers);
        let app_clone = app_handle.clone();
        let key2 = Arc::clone(&key1_clone);
        let (done_tx2, done_rx2) = std::sync::mpsc::channel::<()>();
        let mut done_rx_opt = Some(done_rx);
        let stderr_thread = std::thread::Builder::new()
            .name(format!("nexus-log-{}-stderr", key))
            .spawn(move || {
                let reader = BufReader::new(stderr);
                let key_string = key2.to_string();
                for line in read_log_lines(reader) {
                    let now = chrono::Utc::now().to_rfc3339();
                    // 刷新行识别：\r、\x1b[s、\x1b[2K/\x1b[K 开头的行是单行刷新，合并而非逐帧追加
                    let is_refresh = line.starts_with('\r')
                        || line.starts_with("\x1b[s")
                        || line.starts_with("\x1b[2K")
                        || line.starts_with("\x1b[K");
                    let cleaned = clean_ansi(&line);
                    let line = truncate_line(
                        if is_refresh { format!("\r{}", cleaned.trim_start_matches('\r')) } else { cleaned },
                        MAX_LINE_BYTES,
                    );
                    // 先写缓冲再 emit：快照必然包含所有已发送事件
                    if let Ok(mut b) = log_buffers.lock() {
                        let e = b.entry(Arc::clone(&key2)).or_default();
                        if is_refresh {
                            let text = line.trim_start_matches('\r').to_string();
                            if let Some(last) = e.back_mut() {
                                last.text = text.clone();
                                last.timestamp = now.clone();
                            } else {
                                e.push_back(LogLine { timestamp: now.clone(), stream: "stderr".into(), text });
                            }
                        } else {
                            while e.len() >= MAX_LOG_LINES { e.pop_front(); }
                            e.push_back(LogLine { timestamp: now.clone(), stream: "stderr".into(), text: line.clone() });
                        }
                    }
                    // emit 保留 \r 前缀：前端据此合并实时流（与缓冲语义一致）
                    let _ = app_clone.emit("service-log", ServiceLogPayload { service_key: key_string.clone(), stream: "stderr".into(), data: line, timestamp: now });
                }
                drop(done_tx2);
            })
            .map_err(|e| {
                // stderr 线程创建失败：杀掉进程树（stdout 线程随后读到 EOF 自行退出）
                kill_process_tree(pid);
                format!("创建 stderr 日志读取线程失败: {}", e)
            })?;
        let _ = stderr_thread;

        // Phase 3: 重新获取锁，二次检查后插入
        let mut procs = self.processes.lock().map_err(|e| format!("进程管理器锁获取失败: {}", e))?;
        if procs.contains_key(key) {
            // TOCTOU 竞态：另一个线程已插入同 key，清理当前创建的资源
            log::warn!("[nexus] TOCTOU 竞态: {} 已在运行中，清理泄漏的子进程", key);
            cleanup_process(pid, child, done_rx_opt.take(), Some(done_rx2));
            return Err(format!("{} 已在运行中", key));
        }
        procs.insert(key.to_string(), ProcessInfo {
            child, pid,
            project_id: project_id.to_string(),
            done_stdout: done_rx_opt,
            done_stderr: Some(done_rx2),
        });
        Ok(())
    }

    pub fn stop(&self, key: &str) -> Result<(), String> {
        log::info!("[nexus] 停止服务: {}", key);

        // Phase 1: 从 map 中移除 entry，释放锁
        let entry: Option<ProcessCleanupEntry> = {
            let mut procs = self.processes.lock().map_err(|e| format!("锁错误: {}", e))?;
            procs.remove(key).map(|mut info| {
                (key.to_string(), info.pid, info.child,
                 info.done_stdout.take(), info.done_stderr.take())
            })
        };

        // Phase 2: 在锁外执行清理
        if let Some((_key, pid, child, done_stdout, done_stderr)) = entry {
            cleanup_process(pid, child, done_stdout, done_stderr);
        } else {
            log::debug!("[nexus] stop: 服务 {} 未在运行，忽略", key);
        }
        if let Ok(mut buffers) = self.log_buffers.lock() {
            let log_key: LogKey = Arc::from(key);
            buffers.remove(&*log_key);
        }
        Ok(())
    }

    pub fn restart(&self, project_id: &str, key: &str, command: &str, cwd: &str, env_vars: &[(String, String)], app_handle: &tauri::AppHandle) -> Result<(), String> {
        self.stop(key)?;
        self.start(project_id, key, command, cwd, env_vars, app_handle)
    }

    pub fn get_logs(&self, key: &str) -> Vec<LogLine> {
        let log_key: LogKey = Arc::from(key);
        match self.log_buffers.lock() {
            Ok(b) => b.get(&*log_key)
                .map(|deque| deque.iter().cloned().collect())
                .unwrap_or_default(),
            Err(e) => {
                log::error!("ProcessManager log_buffers 锁已中毒: {}", e);
                Vec::new()
            }
        }
    }

    /// 返回当前运行中的 (project_id, service_id) 列表
    pub fn running(&self) -> Vec<(String, String)> {
        // Phase 1: 收集已退出进程并从 map 中移除，释放锁
        let dead: Vec<ProcessCleanupEntry> = {
            let mut procs = match self.processes.lock() {
                Ok(guard) => guard,
                Err(e) => {
                    log::error!("ProcessManager processes 锁已中毒: {}", e);
                    return Vec::new();
                }
            };
            let mut dead_keys = Vec::new();
            for (k, info) in procs.iter_mut() {
                if !matches!(info.child.try_wait(), Ok(None)) {
                    dead_keys.push(k.clone());
                }
            }
            dead_keys.into_iter().filter_map(|key| {
                procs.remove(&key).map(|mut info| {
                    (key, info.pid, info.child,
                     info.done_stdout.take(), info.done_stderr.take())
                })
            }).collect()
        };
        // Phase 2: 在锁外收尸已退出进程（已退出的进程不再 taskkill），并同步清理日志缓冲
        for (key, pid, child, done_stdout, done_stderr) in dead {
            if let Ok(mut buffers) = self.log_buffers.lock() {
                buffers.remove(&*key);
            }
            cleanup_process(pid, child, done_stdout, done_stderr);
        }
        // Phase 3: 重新获取锁返回当前运行中的 (project_id, service_id)
        self.processes.lock()
            .map(|procs| procs.iter().map(|(k, v)| (v.project_id.clone(), k.clone())).collect())
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
                log::debug!("[nexus]   清理 {} (pid={})", key, info.pid);
                (key, info.pid, info.child,
                 info.done_stdout.take(), info.done_stderr.take())
            }).collect()
        };

        for (key, pid, child, done_stdout, done_stderr) in entries {
            cleanup_process(pid, child, done_stdout, done_stderr);
            log::debug!("[nexus]   已清理 {} (pid={})", key, pid);
        }

        if let Ok(mut buffers) = self.log_buffers.lock() {
            buffers.clear();
        }
        log::info!("[nexus] stop_all: 已完成");
    }

}

impl Drop for ProcessManager {
    fn drop(&mut self) {
        self.stop_all();
    }
}

// ─── Utilities ──────────────────────────────────────────────

/// 带超时等待子进程退出；超时返回 false（进程仍存活）
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return true,
        }
    }
}

/// 清理单个进程条目（在锁外调用）
///
/// 流程：try_wait 确认进程仍存活才 taskkill（已退出时 PID 可能已被系统复用，
/// 直接 taskkill 会误杀无辜进程）→ 带超时等待退出 → 等待 reader 线程结束
/// （进程死后管道写端关闭，reader 读到 EOF 自然退出）→ 超时则放弃等待，
/// 交由 Job Object（KILL_ON_JOB_CLOSE）在应用退出时兜底。
///
/// 注意：等待 reader 线程用 recv_timeout 而非 join——进程树未全灭时
/// （taskkill 失败、或孙进程仍持有管道写端）EOF 永不发生，join 会无限阻塞
/// 同步命令的主线程（stop/get_running 全部卡死、窗口关不掉）。
fn cleanup_process(
    pid: u32,
    mut child: Child,
    done_stdout: Option<Receiver<()>>,
    done_stderr: Option<Receiver<()>>,
) {
    let alive = matches!(child.try_wait(), Ok(None));
    if alive {
        kill_process_tree(pid);
    }
    let exited = wait_with_timeout(&mut child, Duration::from_millis(2000));
    // 等待 reader 线程结束：正常路径（进程已死）立即返回；异常路径最多等 1 秒后放弃，
    // 线程在进程树最终退出后自然结束（不 join 不会泄漏——线程自行退出即释放资源）
    for rx in [done_stdout, done_stderr].into_iter().flatten() {
        let _ = rx.recv_timeout(Duration::from_millis(1000));
    }
    if exited {
        let _ = child.wait();
    } else {
        log::warn!("[nexus] 进程 (pid={}) 未能按时退出，已释放句柄，由 Job Object 在应用退出时兜底", pid);
    }
}

/// 终止进程树（taskkill /T /F）。工具命令超时等场景也需要，故设为 pub(crate)
pub(crate) fn kill_process_tree(pid: u32) {
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

/// 解析环境变量配置（KEY=VALUE 每行 dotenv 格式，兼容 JSON 对象格式）
pub fn parse_env_vars(raw: &str) -> Vec<(String, String)> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    // JSON 对象格式 {"KEY":"VALUE"}
    if raw.starts_with('{') {
        if let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(raw) {
            return map.into_iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
                .collect();
        }
    }
    // KEY=VALUE 每行格式
    raw.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (k, v) = line.split_once('=')?;
            let k = k.trim();
            if k.is_empty() {
                return None;
            }
            Some((k.to_string(), v.trim().to_string()))
        })
        .collect()
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

    // ── clean_ansi ──────────────────────────────────────────

    #[test]
    fn test_clean_ansi_keeps_sgr_colors() {
        // 颜色序列必须保留，供前端着色
        assert_eq!(clean_ansi("\x1b[31mred\x1b[0m"), "\x1b[31mred\x1b[0m");
    }

    #[test]
    fn test_clean_ansi_removes_cursor_save_restore() {
        // webpack 进度条的 \x1b[s / \x1b[u（保存/恢复光标）应被清除
        assert_eq!(clean_ansi("\x1b[s[webpack.Progress] 69% building\x1b[u"), "[webpack.Progress] 69% building");
    }

    #[test]
    fn test_clean_ansi_removes_clear_line_and_cursor_hide() {
        assert_eq!(clean_ansi("\x1b[2K\x1b[?25lhidden"), "hidden");
    }

    #[test]
    fn test_clean_ansi_plain_text_unchanged() {
        assert_eq!(clean_ansi("hello world"), "hello world");
        assert_eq!(clean_ansi(""), "");
    }

    #[test]
    fn test_clean_ansi_mixed_content() {
        // 颜色 + 光标序列混用：只保留颜色
        assert_eq!(
            clean_ansi("\x1b[s\x1b[32mOK\x1b[0m\x1b[u"),
            "\x1b[32mOK\x1b[0m"
        );
    }

    // ── truncate_line ───────────────────────────────────────

    #[test]
    fn test_truncate_line_short_unchanged() {
        assert_eq!(truncate_line("short".to_string(), 8192), "short");
    }

    #[test]
    fn test_truncate_line_long_truncated_at_char_boundary() {
        // 超长行截断且不破坏 UTF-8 字符边界
        let long = "中".repeat(5000);
        let out = truncate_line(long, 100);
        assert!(out.len() <= 100 + "… [已截断]".len());
        assert!(out.ends_with("… [已截断]"));
    }
}
