use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader};
use std::process::{Command, Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
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

/// 意外退出的服务（崩溃/秒退/spawn 失败）：前端据此在卡片上显示"失败"按钮。
/// 与"主动停止"区分：主动停止（stop）会清除该状态且清空日志
#[derive(Clone, Serialize, Debug)]
pub struct FailedService {
    pub service_id: String,
    /// spawn 失败（进程未启动）时为 None
    pub exit_code: Option<i32>,
    pub timestamp: String,
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

/// 单行读取的字节上限：比日志行上限宽一些（行内多字节字符/ANSI 序列会占位），
/// 但必须存在——`BufRead::lines()` 会把**整行**先读进内存，`truncate_line` 只在读完之后截断，
/// 单行 GB 级输出（base64/JSON dump）足以打满内存。
const MAX_READ_LINE_BYTES: usize = MAX_LINE_BYTES * 4;

/// 逐行读取子进程输出（带上限，超长行的剩余部分丢弃而不累积）
///
/// 错误处理语义：
/// - `InvalidData`（非 UTF-8，如 Windows GBK 中文）→ 跳过该行继续读取
///   （`map_while(Result::ok)` 会在此终止整个 reader 线程，日志永久丢失）
/// - 其他 IO 错误（管道损坏、句柄关闭）→ 停止读取，避免空转
/// - 超长行 → 返回前 `MAX_READ_LINE_BYTES` 字节，剩余部分读到行尾后丢弃
fn read_log_lines<R: BufRead>(reader: R) -> BoundedLineReader<R> {
    BoundedLineReader {
        reader,
        buf: Vec::with_capacity(1024),
        draining: false,
        max_line_bytes: MAX_READ_LINE_BYTES,
        done: false,
    }
}

/// `read_log_lines` 的迭代器实现
struct BoundedLineReader<R: BufRead> {
    reader: R,
    buf: Vec<u8>,
    /// 上一行因超长被截断：先把该行剩余字节丢弃到行尾
    draining: bool,
    max_line_bytes: usize,
    done: bool,
}

/// 从 `BufRead` 读一行到 `buf`，最多 `max` 字节（超出即截断，由调用方置 draining）。
/// 返回 `Ok(true)` = 读到内容（EOF 时 buf 里剩余的半行也算），`Ok(false)` = EOF 且无内容。
///
/// 用 `fill_buf`/`consume` 而不是 `read`/`read_until`：只消费真正复制走的字节，
/// 多读到的部分留在 BufReader 内部缓冲里给下一次用，既限住内存又不会丢行。
fn read_line_bounded<R: BufRead>(reader: &mut R, buf: &mut Vec<u8>, max: usize) -> std::io::Result<bool> {
    buf.clear();
    loop {
        if buf.len() >= max {
            return Ok(true);
        }
        let consumed = {
            let chunk = match reader.fill_buf() {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if chunk.is_empty() {
                return Ok(!buf.is_empty()); // EOF
            }
            let room = max - buf.len();
            let take = match chunk.iter().position(|&b| b == b'\n') {
                Some(i) => std::cmp::min(i + 1, room),
                None => std::cmp::min(chunk.len(), room),
            };
            buf.extend_from_slice(&chunk[..take]);
            take
        };
        reader.consume(consumed);
        if buf.last() == Some(&b'\n') {
            return Ok(true);
        }
    }
}

impl<R: BufRead> Iterator for BoundedLineReader<R> {
    type Item = String;

    fn next(&mut self) -> Option<String> {
        if self.done {
            return None;
        }
        loop {
            // 1. 丢弃上一行超长部分的剩余字节（按内部缓冲块消费，不累积内存）
            if self.draining {
                loop {
                    let chunk = match self.reader.fill_buf() {
                        Ok(b) => b,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => {
                            self.done = true;
                            return None;
                        }
                    };
                    if chunk.is_empty() {
                        self.done = true;
                        return None; // EOF
                    }
                    let newline_at = chunk.iter().position(|&x| x == b'\n');
                    let chunk_len = chunk.len();
                    match newline_at {
                        Some(i) => {
                            self.reader.consume(i + 1);
                            break;
                        }
                        None => self.reader.consume(chunk_len),
                    }
                }
                self.draining = false;
                continue;
            }
            // 2. 读一行（带上限）
            match read_line_bounded(&mut self.reader, &mut self.buf, self.max_line_bytes) {
                Ok(true) => {}
                Ok(false) => {
                    self.done = true;
                    return None;
                }
                Err(_) => {
                    self.done = true;
                    return None;
                }
            }
            // 未以换行结束 = 该行被截断，剩余部分需要在下一轮丢弃
            self.draining = self.buf.last() != Some(&b'\n');
            // 3. 解码；非 UTF-8 行跳过（继续读下一行）
            match String::from_utf8(std::mem::take(&mut self.buf)) {
                Ok(mut s) => {
                    if s.ends_with('\n') {
                        s.pop();
                        if s.ends_with('\r') {
                            s.pop();
                        }
                    }
                    return Some(s);
                }
                Err(e) => {
                    self.buf = e.into_bytes();
                    continue;
                }
            }
        }
    }
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

/// 进程清理条目：(key, pid, child, done_stdout, done_stderr, stop_flag)
/// done_* 为 reader 线程结束信号：线程退出时发送端 drop，recv 返回 Disconnected
type ProcessCleanupEntry = (
    String,
    u32,
    Child,
    Option<Receiver<()>>,
    Option<Receiver<()>>,
    Option<Arc<AtomicBool>>,
);

// ─── ProcessManager ─────────────────────────────────────────

struct ProcessInfo {
    child: Child,
    pid: u32,
    /// 所属项目（供 get_running 返回项目级运行状态）
    project_id: String,
    /// reader 线程结束信号（stdout/stderr）
    done_stdout: Option<Receiver<()>>,
    done_stderr: Option<Receiver<()>>,
    /// reader 线程停止标志：清理时置位，令旧线程不再写缓冲/emit。
    /// 否则 taskkill 未杀净（孙进程持管道）时，旧进程输出会串进新服务的日志，
    /// 且"停止后日志还在涨"，与已停止状态矛盾
    stop_flag: Arc<AtomicBool>,
}

pub struct ProcessManager {
    processes: Mutex<HashMap<String, ProcessInfo>>,
    log_buffers: Arc<Mutex<HashMap<LogKey, VecDeque<LogLine>>>>,
    /// 意外退出的服务：key → 失败信息。日志保留供查看（正常停止/重启/全部停止时清除）
    failed: Mutex<HashMap<String, FailedService>>,
    #[cfg(windows)]
    job: Option<Arc<super::job_object::JobObject>>,
}

/// 启动/重启服务所需的参数集合
///
/// 原为 7 个位置参数，调用处容易串位（project_id/service_id/name/command 都是 &str）。
/// 打包成结构体后语义自解释，也顺带满足"参数 ≤4 个"的项目标准。
pub struct ServiceSpawn<'a> {
    pub project_id: &'a str,
    /// 服务 id（同时是进程表 key）
    pub service_id: &'a str,
    pub name: &'a str,
    pub command: &'a str,
    pub cwd: &'a str,
    pub env_vars: &'a [(String, String)],
    pub app_handle: &'a tauri::AppHandle,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            processes: Mutex::new(HashMap::new()),
            log_buffers: Arc::new(Mutex::new(HashMap::new())),
            failed: Mutex::new(HashMap::new()),
            #[cfg(windows)]
            job: None,
        }
    }

    /// 设置共享的 Job Object（确保子进程在应用退出时被终止）
    #[cfg(windows)]
    pub fn set_job(&mut self, job: Arc<super::job_object::JobObject>) {
        self.job = Some(job);
    }

    /// 返回共享 Job Object 句柄（供外部启动的子进程加入同一清理域）
    #[cfg(windows)]
    pub fn job_arc(&self) -> Option<Arc<super::job_object::JobObject>> {
        self.job.clone()
    }

    pub fn start(&self, spec: ServiceSpawn<'_>) -> Result<(), String> {
        let ServiceSpawn { project_id, service_id: key, name, command, cwd, env_vars, app_handle } = spec;
        log::info!("[nexus] 启动服务: {} ({}) cmd={:?}, cwd={:?}", key, name, mask_command(command), cwd);

        // 工作目录为空：进程可能依赖相对路径/环境变量，直接拒绝并提示配置
        // （错误不带服务名，由调用方按入口包装，避免批量启动时与"名称:"前缀重复）
        if cwd.trim().is_empty() {
            return Err("工作目录为空，无法启动".into());
        }

        // Phase 1: 检查限制，然后释放锁
        // 锁中毒用 into_inner 恢复：否则一次 panic 会让服务"既停不掉也起不来"（锁中毒后
        // start/stop 直接上抛错误），进程表项永久残留
        {
            let procs = self.processes.lock().unwrap_or_else(|e| {
                log::error!("[nexus] processes 锁已中毒，继续使用: {}", e);
                e.into_inner()
            });
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
        // 重新启动：清除旧失败标记（新生命周期）
        if let Ok(mut failed) = self.failed.lock() {
            failed.remove(key);
        }

        // Phase 2: 在锁外执行 spawn 和线程创建
        let mut cmd = build_command(command);
        cmd.current_dir(cwd); // cwd 非空已在上方校验（原 `if !cwd.is_empty()` 是不可达分支）
        if !env_vars.is_empty() {
            cmd.envs(env_vars.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        }

        let mut child = cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                // spawn 失败（cwd 无效、命令不存在等）：写入日志缓冲 + 记失败状态，
                // 卡片显示"失败"按钮，点击日志面板可见原因
                let msg = format!("启动失败: {}", e);
                self.append_system_line(key, msg.clone());
                if let Ok(mut failed) = self.failed.lock() {
                    failed.insert(key.to_string(), FailedService {
                        service_id: key.to_string(),
                        exit_code: None,
                        timestamp: chrono::Utc::now().to_rfc3339(),
                    });
                }
                msg
            })?;

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
        // 停止标志：cleanup 时置位，旧 reader 线程据此退出（否则 taskkill 未杀净时
        // 旧进程输出会串进新服务日志，且"已停止"的日志仍在增长）
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_stdout = Arc::clone(&stop_flag);
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let stdout_thread = std::thread::Builder::new()
            .name(format!("nexus-log-{}-stdout", key))
            .spawn(move || {
                let reader = BufReader::new(stdout);
                // emit 需要 String，提到循环外避免每行重复分配
                let key_string = key1.to_string();
                for line in read_log_lines(reader) {
                    if stop_flag_stdout.load(Ordering::Relaxed) {
                        break;
                    }
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
                            // 仅当队尾同属 stdout 才原地替换：stdout 进度条不得覆盖 stderr 队尾的错误行
                            let mergeable = e.back().map(|l| l.stream == "stdout").unwrap_or(false);
                            if mergeable {
                                if let Some(last) = e.back_mut() {
                                    last.text = text.clone();
                                    last.timestamp = now.clone();
                                }
                            } else {
                                while e.len() >= MAX_LOG_LINES { e.pop_front(); }
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
                // 线程创建失败：杀掉已 spawn 的子进程并回收句柄，避免进程泄漏
                kill_and_reap(&mut child, Duration::from_millis(2000));
                format!("创建 stdout 日志读取线程失败: {}", e)
            })?;
        let _ = stdout_thread; // 不 join；线程在进程退出（EOF）后自然结束

        let log_buffers = Arc::clone(&self.log_buffers);
        let app_clone = app_handle.clone();
        let key2 = Arc::clone(&key1_clone);
        let stop_flag_stderr = Arc::clone(&stop_flag);
        let (done_tx2, done_rx2) = std::sync::mpsc::channel::<()>();
        let mut done_rx_opt = Some(done_rx);
        let stderr_thread = std::thread::Builder::new()
            .name(format!("nexus-log-{}-stderr", key))
            .spawn(move || {
                let reader = BufReader::new(stderr);
                let key_string = key2.to_string();
                for line in read_log_lines(reader) {
                    if stop_flag_stderr.load(Ordering::Relaxed) {
                        break;
                    }
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
                            // 仅当队尾同属 stderr 才原地替换（理由同 stdout 段）
                            let mergeable = e.back().map(|l| l.stream == "stderr").unwrap_or(false);
                            if mergeable {
                                if let Some(last) = e.back_mut() {
                                    last.text = text.clone();
                                    last.timestamp = now.clone();
                                }
                            } else {
                                while e.len() >= MAX_LOG_LINES { e.pop_front(); }
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
                // stderr 线程创建失败：杀掉进程树并回收句柄（stdout 线程随后读到 EOF 自行退出）
                kill_and_reap(&mut child, Duration::from_millis(2000));
                format!("创建 stderr 日志读取线程失败: {}", e)
            })?;
        let _ = stderr_thread;

        // Phase 3: 重新获取锁，二次检查后插入
        let mut procs = self.processes.lock().unwrap_or_else(|e| {
            log::error!("[nexus] processes 锁已中毒，继续使用: {}", e);
            e.into_inner()
        });
        if procs.contains_key(key) {
            // TOCTOU 竞态：另一个线程已插入同 key，清理当前创建的资源
            log::warn!("[nexus] TOCTOU 竞态: {} 已在运行中，清理泄漏的子进程", key);
            cleanup_process(pid, child, done_rx_opt.take(), Some(done_rx2), Some(Arc::clone(&stop_flag)));
            return Err(format!("{} 已在运行中", key));
        }
        procs.insert(key.to_string(), ProcessInfo {
            child, pid,
            project_id: project_id.to_string(),
            done_stdout: done_rx_opt,
            done_stderr: Some(done_rx2),
            stop_flag,
        });
        Ok(())
    }

    pub fn stop(&self, key: &str) -> Result<(), String> {
        log::info!("[nexus] 停止服务: {}", key);

        // Phase 1: 从 map 中移除 entry，释放锁
        let entry: Option<ProcessCleanupEntry> = {
            let mut procs = self.processes.lock().unwrap_or_else(|e| {
                log::error!("[nexus] processes 锁已中毒，继续使用: {}", e);
                e.into_inner()
            });
            procs.remove(key).map(|mut info| {
                (key.to_string(), info.pid, info.child,
                 info.done_stdout.take(), info.done_stderr.take(), Some(info.stop_flag))
            })
        };

        // Phase 2: 在锁外执行清理
        if let Some((_key, pid, child, done_stdout, done_stderr, stop_flag)) = entry {
            cleanup_process(pid, child, done_stdout, done_stderr, stop_flag);
        } else {
            log::debug!("[nexus] stop: 服务 {} 未在运行，忽略", key);
        }
        // 主动停止 = 正常关闭：无论进程是否还在管理表（崩溃的服务已被 running() 移出），
        // 都清除失败标记并清空日志（含失败日志）——项目"全部停止"依赖此语义
        if let Ok(mut failed) = self.failed.lock() {
            failed.remove(key);
        }
        if let Ok(mut buffers) = self.log_buffers.lock() {
            let log_key: LogKey = Arc::from(key);
            buffers.remove(&*log_key);
        }
        Ok(())
    }

    pub fn restart(&self, spec: ServiceSpawn<'_>) -> Result<(), String> {
        self.stop(spec.service_id)?;
        self.start(spec)
    }

    /// 追加系统标记行前，先调用者已确认需要写；锁失败时静默忽略（不影响主流程）
    pub fn append_system_line(&self, key: &str, text: String) {
        append_system_line(&self.log_buffers, key, text);
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
        // Phase 1: 收集已退出进程并从 map 中移除，释放锁。
        // 锁内只做 try_wait + 摘表：原实现持 processes 锁期间还去拿 log_buffers/failed
        // 并做字符串格式化（三把锁嵌套 + 非必要持锁，任何一处改成反向顺序即死锁）
        let mut exited: Vec<(String, Option<i32>)> = Vec::new();
        let dead: Vec<ProcessCleanupEntry> = {
            let mut procs = self.processes.lock().unwrap_or_else(|e| {
                log::error!("ProcessManager processes 锁已中毒，继续使用: {}", e);
                e.into_inner()
            });
            let mut dead_keys = Vec::new();
            for (k, info) in procs.iter_mut() {
                match info.child.try_wait() {
                    Ok(Some(status)) => {
                        exited.push((k.clone(), status.code()));
                        dead_keys.push(k.clone());
                    }
                    Ok(None) => {} // 运行中
                    Err(e) => {
                        // 无法检查状态：保守按"仍存活"处理（原实现判为已死 → 摘表 + 标失败，
                        // 而进程可能仍在运行且此后不再受管理）
                        log::warn!("[nexus] 查询进程状态失败 ({}): {}（按存活处理）", k, e);
                    }
                }
            }
            dead_keys.into_iter().filter_map(|key| {
                procs.remove(&key).map(|mut info| {
                    (key, info.pid, info.child,
                     info.done_stdout.take(), info.done_stderr.take(), Some(info.stop_flag))
                })
            }).collect()
        };
        // Phase 1.5: 锁外写日志标记与失败状态（原实现在持锁期间做）
        for (key, code) in &exited {
            let code_str = code.map(|c| c.to_string()).unwrap_or_else(|| "?".into());
            self.append_system_line(key, format!("进程已退出（退出码: {}）", code_str));
            if let Ok(mut failed) = self.failed.lock() {
                failed.insert(key.clone(), FailedService {
                    service_id: key.clone(),
                    exit_code: *code,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                });
            }
        }
        // Phase 2: 在锁外收尸已退出进程（已退出的进程不再 taskkill）。
        // 日志缓冲保留（Phase 1 已追加退出标记行）：崩溃/秒退的报错是诊断关键
        for (_key, pid, child, done_stdout, done_stderr, stop_flag) in dead {
            cleanup_process(pid, child, done_stdout, done_stderr, stop_flag);
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
                 info.done_stdout.take(), info.done_stderr.take(), Some(info.stop_flag))
            }).collect()
        };

        for (key, pid, child, done_stdout, done_stderr, stop_flag) in entries {
            cleanup_process(pid, child, done_stdout, done_stderr, stop_flag);
            log::debug!("[nexus]   已清理 {} (pid={})", key, pid);
        }

        if let Ok(mut buffers) = self.log_buffers.lock() {
            buffers.clear();
        }
        // 全部停止 = 主动关闭：清除所有失败标记（正常日志与失败日志都已清空）
        if let Ok(mut failed) = self.failed.lock() {
            failed.clear();
        }
        log::info!("[nexus] stop_all: 已完成");
    }

    /// 返回意外退出的服务列表（前端显示失败状态）
    pub fn failed(&self) -> Vec<FailedService> {
        match self.failed.lock() {
            Ok(f) => f.values().cloned().collect(),
            Err(e) => {
                log::error!("ProcessManager failed 锁已中毒: {}", e);
                Vec::new()
            }
        }
    }

}

impl Drop for ProcessManager {
    fn drop(&mut self) {
        self.stop_all();
    }
}

// ─── Utilities ──────────────────────────────────────────────

/// 向日志缓冲追加一条系统标记行（stream="system"）。
///
/// 语义：进程生命周期事件（启动失败/退出码/停止）写入日志流，用户可从日志面板查因。
/// 缓冲为空时自动创建；锁失败或超限时静默处理（系统行丢失不影响主流程）。
fn append_system_line(buffers: &Arc<Mutex<HashMap<LogKey, VecDeque<LogLine>>>>, key: &str, text: String) {
    if let Ok(mut b) = buffers.lock() {
        let e = b.entry(Arc::from(key)).or_default();
        while e.len() >= MAX_LOG_LINES { e.pop_front(); }
        e.push_back(LogLine { timestamp: chrono::Utc::now().to_rfc3339(), stream: "system".into(), text });
    }
}

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

/// 终止进程树并**回收**子进程句柄（kill → 限时等待 → wait）。
///
/// 为什么统一成一个函数：kill 之后**必须** wait——不 wait 会残留进程句柄（僵尸项）；
/// 而 tokio 的 `kill_on_drop` 只是发送终止信号，**不会**回收。
/// 原实现散落多处 `kill_process_tree` 后直接 return，句柄就留在那里了。
///
/// 超时不 panic、不无限等：仍存活的进程交由 Job Object（KILL_ON_JOB_CLOSE）在
/// 应用退出时兜底，这里只保证句柄一定被 `wait` 回收一次。
pub(crate) fn kill_and_reap(child: &mut Child, timeout: Duration) {
    let pid = child.id();
    kill_process_tree(pid);
    if wait_with_timeout(child, timeout) {
        let _ = child.wait();
    } else {
        log::warn!("[nexus] 进程 (pid={}) 未能按时退出，已交由 Job Object 兜底", pid);
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
    stop_flag: Option<Arc<AtomicBool>>,
) {
    // 先置停止标志：reader 线程立刻停止写缓冲与 emit。
    // 否则进程树未杀净（孙进程持管道）时旧线程会继续往同一个 key 写日志——
    // 表现为"已停止的服务日志还在涨"，重启后新旧进程输出还会串台
    if let Some(flag) = &stop_flag {
        flag.store(true, Ordering::Relaxed);
    }
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

/// 命令原文脱敏：日志中的凭据掩码为 `***`。
///
/// 为什么必须做：日志会落到 `~/.nexus/logs/nexus.log`，明文凭据（形如
/// `java -Dapi.key=xxx -jar app.jar`）会长期留在磁盘上。
///
/// 覆盖四类常见写法（启发式，不追求完备）：
///   1. `NAME=VALUE`：`-Dapi.key=x`、`TOKEN=x`、`...?token=x`
///   2. **空格分隔取值**：`--token x`、`--password x`、`-u x`
///      —— 旧实现只处理含 `=` 的 token，这一类**完全漏掉**
///   3. `Bearer x` / `Basic x`（典型来源 `-H "Authorization: Bearer eyJ…"`）
///   4. URL 用户信息 `scheme://user:pass@host`，以及 `-u user:pass` 的密码段
pub fn mask_command(command: &str) -> String {
    let mut out: Vec<String> = Vec::with_capacity(16);
    // 上一个 token 是"取值型敏感开关"时，当前 token 是它的取值
    let mut pending: Option<PendingMask> = None;
    for token in command.split(' ') {
        if let Some(kind) = pending.take() {
            out.push(match kind {
                PendingMask::Whole => format!("***{}", query_tail(token)),
                PendingMask::UserInfo => mask_user_info(token),
            });
            continue;
        }
        // `Bearer <token>` / `Basic <base64>`：保留方案名，掩掉令牌
        if token.eq_ignore_ascii_case("bearer") || token.eq_ignore_ascii_case("basic") {
            pending = Some(PendingMask::Whole);
            out.push(token.to_string());
            continue;
        }
        if let Some(kind) = value_flag_kind(token) {
            pending = Some(kind);
            out.push(token.to_string());
            continue;
        }
        out.push(mask_token(token));
    }
    out.join(" ")
}

/// 待掩码的取值类型：整体掩掉，或只掩 `user:pass` 的密码段（保留用户名可读）
#[derive(Clone, Copy)]
enum PendingMask {
    Whole,
    UserInfo,
}

/// 判断一个 token 是否是"由后续 token 提供取值"的敏感开关。
///
/// 只认**纯开关名**（`-`/`--` 开头、不含 `=`、名字是短标识符），避免把路径或
/// 值本身误判成开关。`-p`/`-u` 这类短开关在真实命令里歧义大（`-p 8080` 是端口），
/// 因此只有 `user` 系列走"只掩密码段"，其余短开关不参与判断——宁可漏掉个别写法，
/// 也不要把每条 docker 命令的端口号都打成星号。
fn value_flag_kind(token: &str) -> Option<PendingMask> {
    if !token.starts_with('-') || token.contains('=') { return None; }
    let name = token.trim_start_matches('-').to_ascii_lowercase();
    if name.is_empty() || name.len() > 24 { return None; }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { return None; }
    if matches!(name.as_str(), "u" | "user" | "username") {
        return Some(PendingMask::UserInfo);
    }
    SENSITIVE_NAME_HINTS
        .iter()
        .any(|s| name.contains(s))
        .then_some(PendingMask::Whole)
}

const SENSITIVE_NAME_HINTS: &[&str] = &[
    "key", "token", "secret", "password", "passwd", "pwd", "auth", "credential",
];

/// 值里可能带查询串（`?token=t&a=1`）：只掩掉首个参数，保留 `&` 之后的其余部分
fn query_tail(value: &str) -> &str {
    match value.find('&') {
        Some(i) => &value[i..],
        None => "",
    }
}

/// `user:pass` → `user:***`（保留用户名便于排障）；无冒号则整体掩掉
fn mask_user_info(token: &str) -> String {
    match token.find(':') {
        Some(i) => format!("{}:***", &token[..i]),
        None => "***".to_string(),
    }
}

/// `scheme://user:pass@host/...` → `scheme://user:***@host/...`；无用户信息时返回 None
fn mask_url_userinfo(token: &str) -> Option<String> {
    let scheme_end = token.find("://")?;
    let after = &token[scheme_end + 3..];
    let at = after.find('@')?;
    let userinfo = &after[..at];
    let colon = userinfo.find(':')?;
    Some(format!(
        "{}://{}:***@{}",
        &token[..scheme_end],
        &userinfo[..colon],
        &after[at + 1..]
    ))
}

fn mask_token(token: &str) -> String {
    // URL 用户信息优先：一个 token 里可能同时含 `://` 与 `=`（查询串）
    if let Some(masked) = mask_url_userinfo(token) {
        return masked;
    }
    let Some(eq) = token.find('=') else { return token.to_string() };
    let (name_part, rest) = token.split_at(eq);
    let value = &rest[1..];
    if value.is_empty() {
        return token.to_string();
    }
    let name = name_part
        .trim_start_matches('-')
        .trim_start_matches('/')
        .to_ascii_lowercase();
    // `-Dapi.key` 这种单字母前缀：剥掉 `d` 后再判断
    let name = if name_part.starts_with("-D") { &name[1.min(name.len())..] } else { name.as_str() };
    if !SENSITIVE_NAME_HINTS.iter().any(|s| name.contains(s)) {
        return token.to_string();
    }
    format!("{}=***{}", name_part, query_tail(value))
}

/// 解析环境变量配置（KEY=VALUE 每行 dotenv 格式，兼容 JSON 对象格式）
///
/// 为什么返回 Result：原实现用 `filter_map` 静默丢弃非法行——`PORT 8080`（漏写等号）、
/// JSON 里的数字值都会无声消失，用户看到"服务起来了"但变量根本没生效。现在非法输入
/// 一律报错并指出具体行；成对引号会被剥掉（`NODE_ENV="production"` 不应把引号传给子进程）；
/// `export KEY=VALUE` 写法也支持（否则变量名会变成 "export KEY"）。
pub fn parse_env_vars(raw: &str) -> Result<Vec<(String, String)>, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    // JSON 对象格式 {"KEY":"VALUE"}（数字/布尔值转字符串保留，不再静默丢弃）
    if raw.starts_with('{') {
        let map = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(raw)
            .map_err(|e| format!("环境变量 JSON 格式非法: {}", e))?;
        let mut out = Vec::with_capacity(map.len());
        for (k, v) in map {
            let value = match v {
                serde_json::Value::String(s) => s,
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Null => String::new(),
                other => return Err(format!("环境变量 {} 的值类型不支持: {}", k, other)),
            };
            out.push((k, value));
        }
        return Ok(out);
    }
    // KEY=VALUE 每行格式
    let mut out = Vec::new();
    for (idx, raw_line) in raw.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").map(str::trim_start).unwrap_or(line);
        let (k, v) = line
            .split_once('=')
            .ok_or_else(|| format!("环境变量第 {} 行缺少 '='（应为 KEY=VALUE）: {}", idx + 1, line))?;
        let k = k.trim();
        if k.is_empty() {
            return Err(format!("环境变量第 {} 行变量名为空: {}", idx + 1, line));
        }
        out.push((k.to_string(), strip_matching_quotes(v.trim())));
    }
    Ok(out)
}

/// 剥掉成对的首尾引号（单/双引号），非成对时原样返回
fn strip_matching_quotes(v: &str) -> String {
    let b = v.as_bytes();
    if b.len() >= 2 {
        let (first, last) = (b[0], b[b.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return v[1..v.len() - 1].to_string();
        }
    }
    v.to_string()
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

    // ── parse_env_vars（回归：非法行曾静默丢弃）──────────────

    #[test]
    fn test_parse_env_vars_dotenv_basic() {
        let v = parse_env_vars("A=1\nB = two\n# 注释\n\n").unwrap();
        assert_eq!(v, vec![("A".to_string(), "1".to_string()), ("B".to_string(), "two".to_string())]);
    }

    #[test]
    fn test_parse_env_vars_strips_quotes_and_export() {
        // 成对引号不应传给子进程；export 前缀不应变成变量名的一部分
        let v = parse_env_vars("NODE_ENV=\"production\"\nexport MODE='dev'\n").unwrap();
        assert_eq!(
            v,
            vec![
                ("NODE_ENV".to_string(), "production".to_string()),
                ("MODE".to_string(), "dev".to_string())
            ]
        );
    }

    #[test]
    fn test_parse_env_vars_rejects_missing_equals() {
        // 漏写等号的行必须报错而不是无声消失（原实现 filter_map 丢弃）
        let err = parse_env_vars("PORT 8080").unwrap_err();
        assert!(err.contains("缺少 '='"), "错误信息应指出缺少等号: {}", err);
        assert!(err.contains("第 1 行"), "错误信息应带行号: {}", err);
    }

    #[test]
    fn test_parse_env_vars_json_keeps_non_string_values() {
        // JSON 里的数字/布尔值转为字符串保留（原实现 filter_map 整条丢弃）
        let v = parse_env_vars(r#"{"PORT": 3000, "DEBUG": true, "NAME": "x"}"#).unwrap();
        assert!(v.contains(&("PORT".to_string(), "3000".to_string())));
        assert!(v.contains(&("DEBUG".to_string(), "true".to_string())));
        assert!(v.contains(&("NAME".to_string(), "x".to_string())));
    }

    #[test]
    fn test_parse_env_vars_empty_input() {
        assert!(parse_env_vars("").unwrap().is_empty());
        assert!(parse_env_vars("   \n  ").unwrap().is_empty());
    }

    // ── read_log_lines（超长行/坏编码）──────────────────────

    #[test]
    fn test_read_log_lines_overlong_line_truncated_then_recovers() {
        // 超长行只取前 MAX_READ_LINE_BYTES，其余丢弃；后续行必须正常读出
        let mut data = vec![b'a'; MAX_READ_LINE_BYTES + 500];
        data.push(b'\n');
        data.extend_from_slice(b"after\n");
        let lines: Vec<String> = read_log_lines(std::io::Cursor::new(data)).collect();
        assert_eq!(lines.len(), 2, "应为截断行 + 后续行: {:?}", lines.iter().map(|l| l.len()).collect::<Vec<_>>());
        assert_eq!(lines[0].len(), MAX_READ_LINE_BYTES);
        assert_eq!(lines[1], "after");
    }

    #[test]
    fn test_read_log_lines_skips_non_utf8_line() {
        // 非 UTF-8 行跳过而非终止整个 reader（GBK 中文日志不丢后续内容）
        let mut data = vec![0xff, 0xfe, b'\n'];
        data.extend_from_slice(b"ok\n");
        let lines: Vec<String> = read_log_lines(std::io::Cursor::new(data)).collect();
        assert_eq!(lines, vec!["ok".to_string()]);
    }

    #[test]
    fn test_read_log_lines_strips_crlf() {
        let lines: Vec<String> = read_log_lines(std::io::Cursor::new(b"a\r\nb\nc".to_vec())).collect();
        assert_eq!(lines, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    }

    // ── mask_command（日志脱敏）────────────────────────────

    #[test]
    fn test_mask_command_masks_credentials() {
        // -D 形式的 JVM 参数
        assert_eq!(
            mask_command("java -Dapi.key=abc123 -jar app.jar"),
            "java -Dapi.key=*** -jar app.jar"
        );
        // 长选项形式；非敏感参数不受影响
        assert_eq!(
            mask_command("app --token=xyz --port=8080"),
            "app --token=*** --port=8080"
        );
        // dotenv 形式
        assert_eq!(
            mask_command("NODE_ENV=production DB_PASSWORD=sekret"),
            "NODE_ENV=production DB_PASSWORD=***"
        );
        // URL 查询串：只掩掉该参数，保留后续参数
        assert_eq!(
            mask_command("curl http://x/?token=t&a=1"),
            "curl http://x/?token=***&a=1"
        );
        // 无等号 / 空值原样保留
        assert_eq!(mask_command("npm run build"), "npm run build");
        assert_eq!(mask_command("app --token="), "app --token=");
    }

    /// 回归：空格分隔取值的写法（旧实现只处理含 `=` 的 token，这一类完全漏掉）
    #[test]
    fn test_mask_command_masks_space_separated_flag_values() {
        assert_eq!(mask_command("app --token abc123"), "app --token ***");
        assert_eq!(mask_command("app --password Secret value"), "app --password *** value");
        assert_eq!(mask_command("app --api-key K1"), "app --api-key ***");
        assert_eq!(mask_command("app --secret S1 --port 8080"), "app --secret *** --port 8080");
        // 非敏感短开关不能被误掩（`-p 8080` 是端口，掩掉会让日志失去排障价值）
        assert_eq!(mask_command("docker run -p 8080:80 img"), "docker run -p 8080:80 img");
    }

    /// 回归：`Bearer`/`Basic` 令牌与 URL 用户信息
    #[test]
    fn test_mask_command_masks_bearer_and_url_userinfo() {
        assert_eq!(
            mask_command("curl -H Authorization: Bearer eyJhbGciOi"),
            "curl -H Authorization: Bearer ***"
        );
        assert_eq!(mask_command("mysql://user:pw@host/db"), "mysql://user:***@host/db");
        // `-u user:pass` 只掩密码段，保留用户名（便于排障）
        assert_eq!(mask_command("git clone -u alice:s3cr3t repo"), "git clone -u alice:*** repo");
        // 无密码段的 URL 不动
        assert_eq!(mask_command("http://host/path"), "http://host/path");
    }
}
