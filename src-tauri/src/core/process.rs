use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use serde::{Deserialize, Serialize};

use super::logfile;

/// 日志缓冲区 key 类型，使用 Arc<str> 避免热路径 String clone
type LogKey = Arc<str>;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(unix)]
use std::os::unix::process::CommandExt as UnixCommandExt;

// ─── Types ──────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct LogLine {
    /// 服务内单调递增的行号（同一次运行内，刷新帧替换队尾时取新号）。
    ///
    /// 为什么需要它：`get_service_logs` 快照与实时事件是两条独立路径，快照可能覆盖
    /// "已追加但尚未被前端合并"的实时行（跟随视图里那几行永久消失）；有了序号，
    /// 前端可以按"只应用 seq 大于已合并最大值的行"做幂等合并，两条路径任意交错都收敛。
    pub seq: u64,
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
pub struct ServiceLogBatchPayload {
    pub service_key: String,
    pub lines: Vec<Arc<LogLine>>,
}

/// 输出批量发射窗口（ms）。与服务日志面板前端的 50ms 渲染合帧同量级：
/// 够小以保证交互式输出不卡顿，够大以摊平构建/进度条的突发输出。
///
/// `pub(crate)`：工具命令输出（`commands/process.rs`）也走同一口径——两条通道的
/// 突发形态相同（构建/装依赖），窗口取不同值只会让前端要照顾两种节奏。
pub(crate) const LOG_FLUSH_MS: u64 = 50;

/// 每服务日志缓冲行数上限（IDEA Console 式：只保留最新 2000 行）
const MAX_LOG_LINES: usize = 2000;
/// 每服务日志缓冲**字节**上限（与前端 `stores/logStore.ts` 的 `MAX_BYTES` 同值同口径）。
/// 行数上限挡不住"2000 行 × 8KB 长行 = 16MB/服务"这种形状，两条上限量纲不同、都必须有。
const MAX_LOG_BYTES: usize = 2 * 1024 * 1024;
/// 单行日志字节上限（base64/JSON dump 等超长行截断）
const MAX_LINE_BYTES: usize = 8192;
/// 全局最大并发服务数，防止日志缓冲无限增长
const MAX_SERVICES: usize = 50;

/// 批量发射循环的一步：**先读存活数，再取缓冲**，返回（本批要发的行，是否收尾退出）。
///
/// 顺序即不变式——反过来写会漏行：读取线程是"先 push 再递减存活计数"，于是
/// "取缓冲"若发生在"读存活数"之前，就可能出现：取到空批 →（此时 reader 推入最后一行）
/// → 读到存活数已归零 → 退出，那一行既不在已发出的批里，循环也再不会转一圈。
/// 现在的顺序下，存活数归零一旦被读到（这一刻所有 push 都已发生），紧随其后的取缓冲
/// 必然取得到；因此"存活归零 + 本批为空"才是真的没有下一行了。
///
/// 独立成函数是为了让这个顺序能被测试钉住——它此前在两处各写了一遍（服务日志与工具命令
/// 输出），两份都只有肉眼复核。
fn flush_step<T>(pending: &Mutex<Vec<T>>, readers_alive: &std::sync::atomic::AtomicUsize) -> (Vec<T>, bool) {
    let alive_now = readers_alive.load(Ordering::SeqCst);
    let batch = {
        let mut guard = match pending.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        // mem::take：锁内只做移动，序列化/发送发生在锁外
        std::mem::take(&mut *guard)
    };
    let done = alive_now == 0 && batch.is_empty();
    (batch, done)
}

/// 批量发射线程（服务日志与工具命令输出共用）：
/// 每 `LOG_FLUSH_MS` 把待发行缓冲整批交给 `emit`，读取线程全部退出且缓冲已空时收尾。
///
/// 为什么必须独立于读取线程：读取线程会阻塞在 `lines()` 上（命令打印一行后静默 10 秒
/// 很常见），由它在读取线程侧"攒够再发"会让那一行直到下一行到来才显示。
pub(crate) fn spawn_log_flush_thread<T, F>(
    name: String,
    pending: Arc<Mutex<Vec<T>>>,
    readers_alive: Arc<std::sync::atomic::AtomicUsize>,
    mut emit: F,
) -> std::io::Result<std::thread::JoinHandle<()>>
where
    T: Send + 'static,
    F: FnMut(Vec<T>) + Send + 'static,
{
    std::thread::Builder::new().name(name).spawn(move || loop {
        std::thread::sleep(Duration::from_millis(LOG_FLUSH_MS));
        let (batch, done) = flush_step(&pending, &readers_alive);
        if !batch.is_empty() {
            emit(batch);
        }
        if done {
            break;
        }
    })
}

/// 解码一行字节：先 UTF-8，失败按 GB18030 回退。
///
/// 不能"跳过解不出来的行"——中文 Windows 上 cmd 的报错就是 GBK，跳过它等于把唯一的原因
/// 删掉（用户看到的是"命令失败（退出码 1）。（无输出）"）。服务 stdout（`BoundedLineReader`）
/// 与日志文件跟随（`spawn_log_tailer`）共用这一条口径。
fn decode_lossy(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => encoding_rs::GB18030.decode(e.as_bytes()).0.into_owned(),
    }
}

/// 文件的身份（Windows：卷序列号 + 文件索引；Unix：设备号 + inode）。
///
/// 跟随器用它判断"路径上现在这个文件还是不是我打开的那个"——这是识别轮转的**正确**判据。
/// 只比大小会在真实情况下失手（实测踩到过）：新文件一上来就比在旧文件里读到的位置大
/// （小日志轮转很容易撞上），以及复制截断（copytruncate）把同一个文件清零。
/// 平台拿不到身份时返回 None，调用方退回按大小判断（宁可少识别一次轮转，也不能误判）。
///
/// Windows 侧自己调 Win32（`std::os::windows::fs::MetadataExt::file_index` 至今仍是
/// unstable 的 `windows_by_handle`）：
#[cfg(windows)]
fn file_identity(file: &std::fs::File) -> Option<(u64, u64)> {
    use std::os::windows::io::AsRawHandle;
    type Handle = isize;
    type Bool = i32;
    type Dword = u32;
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    // BY_HANDLE_FILE_INFORMATION：全 DWORD/两 DWORD 的 FILETIME，共 52 字节，无填充
    #[repr(C)]
    #[derive(Default)]
    struct ByHandleFileInformation {
        attributes: Dword,
        creation: FileTime,
        last_access: FileTime,
        last_write: FileTime,
        volume_serial: Dword,
        size_high: Dword,
        size_low: Dword,
        links: Dword,
        index_high: Dword,
        index_low: Dword,
    }
    extern "system" {
        fn GetFileInformationByHandle(handle: Handle, info: *mut ByHandleFileInformation) -> Bool;
    }
    unsafe {
        let mut info: ByHandleFileInformation = std::mem::zeroed();
        if GetFileInformationByHandle(file.as_raw_handle() as Handle, &mut info) == 0 {
            return None;
        }
        let index = ((info.index_high as u64) << 32) | info.index_low as u64;
        Some((info.volume_serial as u64, index))
    }
}

#[cfg(unix)]
fn file_identity(file: &std::fs::File) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata().ok()?;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(any(windows, unix)))]
fn file_identity(_file: &std::fs::File) -> Option<(u64, u64)> {
    None
}

/// 把"距 UNIX 纪元的毫秒"换算成 [`SystemTime`]（给"文件修改时间是否晚于服务启动"用）
fn system_time_from_millis(ms: u64) -> SystemTime {
    let now = SystemTime::now();
    let now_ms = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(ms);
    now.checked_sub(Duration::from_millis(now_ms.saturating_sub(ms))).unwrap_or(now)
}

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
/// 由服务日志 reader 与 `core::ai` 的命令输出捕获共用（两边都需要"有界 + 坏编码不致命"）。
///
/// 错误处理语义：
/// - 非 UTF-8（如 Windows GBK 中文）→ **按 GB18030 解码**，不丢行。
///   早先的做法是"跳过该行"，理由是不让坏编码终止整个 reader——但代价是中文 Windows 上
///   cmd 与系统工具的报错（GBK）被整条吃掉，用户看到的是"命令失败（退出码 1）。（无输出）"，
///   唯一的原因没了（用户报过）。回退解码既保住了原因，也不会终止读取
/// - 其他 IO 错误（管道损坏、句柄关闭）→ 停止读取，避免空转
/// - 超长行 → 返回前 `MAX_READ_LINE_BYTES` 字节，剩余部分读到行尾后丢弃
pub(crate) fn read_log_lines<R: BufRead>(reader: R) -> BoundedLineReader<R> {
    BoundedLineReader {
        reader,
        buf: Vec::with_capacity(1024),
        draining: false,
        max_line_bytes: MAX_READ_LINE_BYTES,
        done: false,
    }
}

/// `read_log_lines` 的迭代器实现（与函数同为 crate 可见：`core::ai` 的命令输出捕获复用）
pub(crate) struct BoundedLineReader<R: BufRead> {
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
            // 3. 解码（与编辑器读文件、`node.rs` 解码命令输出同一口径）
            let mut s = decode_lossy(std::mem::take(&mut self.buf));
            if s.ends_with('\n') {
                s.pop();
                if s.ends_with('\r') {
                    s.pop();
                }
            }
            return Some(s);
        }
    }
}

/// 清理非 SGR 的 ANSI 转义序列
///
/// webpack 进度等 CLI 用 `\x1b[s`/`\x1b[u`（保存/恢复光标）、`\x1b[2K`（清行）、
/// `\x1b[?25l`（隐藏光标）做单行刷新——这些序列渲染时显示为 `s`/`u` 等垃圾字符。
/// 颜色序列（`\x1b[...m`，SGR）保留，供前端着色。
///
/// 调用方先判 `contains('\x1b')` 再调（见 `prepare_log_text`）：绝大多数行没有转义序列，
/// 本函数每次都会 `String::with_capacity + push_str` 整行拷贝一遍，无条件调用等于每行白拷一次。
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

/// 日志缓冲：`HashMap<服务 key, VecDeque<行>>`。
///
/// 元素是 `Arc<LogLine>` 而不是 `LogLine`：`get_service_logs` 要在**全局日志锁内**做快照，
/// 直接克隆结构体等于在锁内做 2000 次字符串深拷贝（单行最长 8KB → 最坏十几 MB），
/// 期间所有服务的 reader 线程都阻塞在同一把锁上。改成 Arc 后锁内只拷指针，
/// 字符串拷贝推迟到锁外（序列化时自然发生）。
type LogBuffers = Arc<Mutex<HashMap<LogKey, LogBuffer>>>;

/// 每条服务的日志缓冲：行队列 + **累计字节数**（PERF-17）。
///
/// 为什么行数上限单独不够：单行上限 8KB（`MAX_LINE_BYTES`），`MAX_LOG_LINES=2000` 行
/// = **单服务最坏 16MB**；`MAX_SERVICES=50` → 最坏约 800MB。而且崩溃/秒退的服务会
/// **保留**缓冲供诊断（进程摘表但缓冲不清），这些内存一直挂到用户手动停止/删除它。
/// 前端那份副本另有 2MB 字节上限（`stores/logStore.ts` 的 `MAX_BYTES`），但那只约束
/// 渲染进程，管不到后端这份。
///
/// 字节数**增量维护**：裁剪时遍历整个队列求和会把 O(1) 的追加变成 O(n)，而这些调用
/// 在 reader 线程热路径上、且持有全局日志锁（所有服务共用一把）——前端同一处也是这么做的。
#[derive(Default)]
struct LogBuffer {
    lines: VecDeque<Arc<LogLine>>,
    /// 队列内所有 `text` 的字节数之和（UTF-8 字节；前端按 UTF-16 单元计，量级同）
    bytes: usize,
}

impl LogBuffer {
    /// 追加一行并按两条上限裁剪
    fn push(&mut self, line: Arc<LogLine>) {
        self.bytes += line.text.len();
        self.lines.push_back(line);
        self.trim();
    }

    /// 用新行替换队尾（`\r` 刷新帧：同一视觉行只换内容，见调用点的语义说明）
    fn replace_tail(&mut self, line: Arc<LogLine>) {
        if let Some(old) = self.lines.pop_back() {
            self.bytes = self.bytes.saturating_sub(old.text.len());
        }
        self.push(line);
    }

    /// 从头部丢弃直到满足两条上限。
    /// `lines.len() > 1` 保证**至少留最新一行**——否则单行超过字节上限时会把刚写进来的行也丢掉
    fn trim(&mut self) {
        while self.lines.len() > MAX_LOG_LINES
            || (self.bytes > MAX_LOG_BYTES && self.lines.len() > 1)
        {
            match self.lines.pop_front() {
                Some(old) => self.bytes = self.bytes.saturating_sub(old.text.len()),
                None => break,
            }
        }
    }
}

/// 日志文本规整：清 ANSI（仅在确有转义序列时）→ 刷新行补回 `\r` 前缀 → 截断超长行。
///
/// 抽成函数的原因：stdout/stderr 两段 reader 曾各自逐字重写这 10 行（本项目最典型的孪生重复），
/// 任一处改了另一处就容易漂移。
fn prepare_log_text(line: String, is_refresh: bool) -> String {
    // 无转义序列时**复用原 String**，避免每行白拷一遍（绝大多数行属于这种）
    let cleaned = if line.contains('\x1b') { clean_ansi(&line) } else { line };
    // 刷新行统一以 \r 前缀标记（clean 后原前缀消失，\r 保留供前端合并判断）
    let text = if is_refresh {
        format!("\r{}", cleaned.trim_start_matches('\r'))
    } else {
        cleaned
    };
    truncate_line(text, MAX_LINE_BYTES)
}

/// 日志行序号源：全局单调，保证"同一服务内递增"（跨服务不要求可比）
static LOG_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// 写入某服务的日志缓冲：刷新帧原地替换队尾，普通行追加并按行数上限滑窗。
///
/// 刷新帧只在**队尾同属一条流**时才合并：stdout 的进度条不得覆盖 stderr 队尾的错误行。
/// 返回写进缓冲的那一行（含 seq），调用方据此发实时事件——事件与快照因此可以按 seq 对齐。
fn push_log_line(buffers: &LogBuffers, key: &LogKey, stream: &str, text: String, timestamp: String, is_refresh: bool) -> Arc<LogLine> {
    let seq = LOG_SEQ.fetch_add(1, Ordering::Relaxed);
    let line = Arc::new(LogLine { seq, timestamp, stream: stream.to_string(), text });
    let Ok(mut b) = buffers.lock() else { return line };
    let e = b.entry(Arc::clone(key)).or_default();
    if is_refresh {
        let mergeable = e.lines.back().map(|l| l.stream == stream).unwrap_or(false);
        if mergeable {
            // 刷新帧取新 seq（替换同一视觉行）：前端"只应用更大 seq"的合并规则才不会把它当成旧行丢掉
            e.replace_tail(Arc::clone(&line));
            return line;
        }
    }
    e.push(Arc::clone(&line));
    line
}

/// 起一个输出流的读取线程：逐行读 → 识别刷新帧 → 写日志缓冲 → 入待发行队列。
///
/// 为什么收成一处（CQ-20）：stdout/stderr 两条流原本是**两份逐行同构**的实现，连注释
/// 都是复制品。抽出来的理由不是少写几行，而是任何一次只改一边都会让两条流行为不一致——
/// 刷新帧前缀集、停止标志的检查时机、写缓冲与入待发行的先后顺序，任一处漏改的症状都是
/// "某个流偶尔渲染不对"，在界面上表现为随机的差异，极难定位。
///
/// 每个参数都是线程要独占的一份资源（`Arc` 克隆或 move 进来的管道），
/// 合并成结构体只是把这份清单换个地方列，收益为零。
#[allow(clippy::too_many_arguments)]
fn spawn_log_reader(
    pipe: impl std::io::Read + Send + 'static,
    stream: &'static str,
    key: LogKey,
    log_buffers: LogBuffers,
    pending: Arc<Mutex<Vec<Arc<LogLine>>>>,
    readers_alive: Arc<std::sync::atomic::AtomicUsize>,
    stop_flag: Arc<AtomicBool>,
    done_tx: std::sync::mpsc::Sender<()>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name(format!("nexus-log-{}-{}", key, stream))
        .spawn(move || {
            let reader = BufReader::new(pipe);
            for line in read_log_lines(reader) {
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
                let now = chrono::Utc::now().to_rfc3339();
                // 刷新行识别：\r、\x1b[s（保存光标）、\x1b[2K/\x1b[K（清行）开头的行
                // 都是单行刷新（webpack 进度条等），应合并而非逐帧追加
                let is_refresh = line.starts_with('\r')
                    || line.starts_with("\x1b[s")
                    || line.starts_with("\x1b[2K")
                    || line.starts_with("\x1b[K");
                let line = prepare_log_text(line, is_refresh);
                // 先写缓冲再入待发行：快照必然包含所有已发送事件（前端再按 seq 幂等合并）
                let logged = push_log_line(&log_buffers, &key, stream, line, now, is_refresh);
                if let Ok(mut p) = pending.lock() {
                    p.push(logged);
                }
            }
            // 退出前递减存活计数：发射线程据此收尾（含最后一批）
            readers_alive.fetch_sub(1, Ordering::SeqCst);
            drop(done_tx);
        })
}

/// 起一个日志文件跟随线程：把文件里**新增**的内容按行接进服务日志（`stream = "file"`）。
///
/// 用途：服务把真正的进程另开窗口跑（见 `AdoptedChild`）时 stdout 指望不上，
/// 跟它写的日志文件是唯一能拿到内容的现实路径。
///
/// 每条行为都对应一个真实的坑：
/// - **从末尾开始**：跑了很久的服务日志可能几百 MB，从头发一遍既灌满缓冲又猛刷界面，
///   而用户要的是"从现在起发生什么"；
/// - **轮转按大小识别**：Tomcat 每天换个新名字、ActiveMQ 是"改名 + 新建"
///   （`activemq.log` → `activemq.log.1`）——手里的句柄会一直指向被改名的旧文件，
///   所以判据是"路径上现在这个文件比读到的位置短"，短了就重开、从头读；
/// - **残行有界**：最后一行可能只写了一半，要攒着等换行；但必须限长，
///   否则一个不写换行的程序能让这个缓冲无限涨（超长即丢弃并标记，同 `read_log_lines`）；
/// - **打开句柄带 `FILE_SHARE_DELETE`**（Rust 默认的共享模式）：不带会**锁住文件**，
///   让服务自己的轮转失败——"监听日志"把被监听方搞坏是最难查的那种 bug；
/// - **停止标志**：服务停止或用户取消跟随后立刻退出。否则 `stop_locked` 把日志缓冲
///   `remove` 之后，跟随线程又把它写回来，那块内存就一直留在表里了。
///
/// `file` 与 `offset` 由**调用线程**备好（打开 + 定位到末尾），不在这里做：
/// 跟随的时间边界必须是"调用 follow_log 的那一刻"。放到线程体里做的话，
/// "跟随已记下、线程还没被调度"这段窗口内服务写的行会被当成历史丢掉（实测漏过一行）。
///
/// 参数多：每一项都是线程要独占的资源（文件句柄、待发行缓冲、两个停止标志、存活计数……），
/// 与 `spawn_log_reader` 同理——合并成结构体只是把这份清单换个地方列，收益为零。
#[allow(clippy::too_many_arguments)]
fn spawn_log_tailer(
    mut file: std::fs::File,
    mut offset: u64,
    path: PathBuf,
    key: LogKey,
    log_buffers: LogBuffers,
    pending: Arc<Mutex<Vec<Arc<LogLine>>>>,
    alive: Arc<std::sync::atomic::AtomicUsize>,
    follow_stop: Arc<AtomicBool>,
    service_stop: Arc<AtomicBool>,
    done_tx: std::sync::mpsc::Sender<()>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name(format!("nexus-logtail-{}", key))
        .spawn(move || {
            let mut partial: Vec<u8> = Vec::new();
            // 从文件中间开始读时第一段是半行：丢到第一个换行为止（开始处 offset > 0 即回看）
            let mut skip_partial = offset > 0;
            // 上一段因超长被丢弃：先把该行剩余字节丢到行尾，别再重复输出
            let mut overflowing = false;
            // 与进程 stdout 走同一条落库路径：上限、序号、刷新帧合并全部继承
            let emit = |text: String| {
                let line = push_log_line(&log_buffers, &key, "file", text, chrono::Utc::now().to_rfc3339(), false);
                if let Ok(mut p) = pending.lock() {
                    p.push(line);
                }
            };
            let stopped = || follow_stop.load(Ordering::Relaxed) || service_stop.load(Ordering::Relaxed);
            loop {
                if stopped() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(LOG_FOLLOW_POLL_MS));
                if stopped() {
                    break;
                }
                // 1) 轮转 / 清空 / 落后
                //
                // 文件暂时打不开（轮转的空档）：这轮不读——继续读手里那个已被改名的旧文件，
                // 只会把"旧文件末尾"当成新内容；下一轮再看新文件有没有就位
                let Ok(current) = std::fs::File::open(&path) else { continue };
                // 身份不同 = 换了文件（改名 + 新建 / 复制截断 / 换目录都覆盖）；
                // 同一文件但变短 = 就地清空。两种都换成新句柄、从头跟。
                let same_file = match (file_identity(&file), file_identity(&current)) {
                    (Some(a), Some(b)) => a == b,
                    _ => true, // 拿不到身份信息：按同一文件处理，靠大小兜底
                };
                let len = current.metadata().map(|m| m.len()).unwrap_or(offset);
                if !same_file || len < offset {
                    file = current;
                    offset = 0;
                    partial.clear();
                    overflowing = false;
                    emit("── 日志文件已轮转或被清空，从新文件开头继续 ──".to_string());
                }
                if len.saturating_sub(offset) > LOG_FOLLOW_MAX_LAG {
                    // 追不上了：跳到接近末尾，并说清跳过了多少（不静默丢内容）
                    let target = len - LOG_FOLLOW_MAX_LAG / 2;
                    if file.seek(SeekFrom::Start(target)).is_ok() {
                        let skipped = target.saturating_sub(offset);
                        offset = target;
                        partial.clear();
                        overflowing = false;
                        emit(format!("── 日志写入快于读取，已跳过 {} 字节 ──", skipped));
                    }
                }
                // 2) 读到 EOF（文件不长时一次就完）
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    let n = match file.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => {
                            log::warn!("[nexus] 读取日志文件失败 {}: {}", path.display(), e);
                            break;
                        }
                    };
                    offset += n as u64;
                    partial.extend_from_slice(&buf[..n]);
                    // 3) 切出完整行（残行留在 partial 里等下一轮）
                    while let Some(pos) = partial.iter().position(|&b| b == b'\n') {
                        let mut line: Vec<u8> = partial.drain(..=pos).collect();
                        line.pop(); // '\n'
                        if line.last() == Some(&b'\r') {
                            line.pop();
                        }
                        if skip_partial {
                            skip_partial = false; // 回看起点的半行：丢掉
                            continue;
                        }
                        if overflowing {
                            overflowing = false; // 该行前半段已标记过，剩余部分丢弃
                            continue;
                        }
                        emit(prepare_log_text(decode_lossy(line), false));
                    }
                    // 残行超长：输出截断版并进入"丢到行尾"状态
                    if partial.len() > MAX_READ_LINE_BYTES {
                        let text = prepare_log_text(decode_lossy(std::mem::take(&mut partial)), false);
                        emit(format!("{}… [已截断]", text));
                        overflowing = true;
                    }
                }
            }
            // 退出前递减存活计数：发射线程据此收尾（含最后一批）
            alive.fetch_sub(1, Ordering::SeqCst);
            drop(done_tx);
        })
}

/// 取生命周期锁（NEW-17）。锁中毒同样继续用：进程生命周期离硬件更近，
/// 让一次无关的 panic 变成"该服务永远起不来"是更坏的结局。
fn lock_lifecycle<'a>(lock: &'a Arc<Mutex<()>>, key: &str) -> std::sync::MutexGuard<'a, ()> {
    lock.lock().unwrap_or_else(|e| {
        log::error!("[nexus] 服务 {} 的生命周期锁已中毒，继续使用: {}", key, e);
        e.into_inner()
    })
}

// ─── 类型别名 ───────────────────────────────────────────────

/// 跟随文件的轮询间隔。比日志发射窗口（50ms）大得多：查一次文件大小、读一段增量
/// 没必要那么勤，而每次轮询最多读到 EOF 为止。
const LOG_FOLLOW_POLL_MS: u64 = 250;

/// 跟随起点的回看字节数：从"文件末尾往前这么多"开始读。
///
/// 为什么不直接从末尾读起：服务刚启动那几行恰恰是最想看的（Tomcat 的启动日志、
/// broker 起没起来），而从末尾读起会让"刚跟上时文件已经写完了"表现为**面板空白** ——
/// 实测就是这么收到的反馈。回看量有界：既不给跑了几个月的日志灌历史
/// （缓冲上限本身也会裁），也保证一跟上面板就有内容。
const LOG_FOLLOW_SEED_BYTES: u64 = 64 * 1024;

/// 允许落后的字节上限。超过就跳到接近末尾并**明说**跳过了多少：
/// 没有这条，"服务每秒写几 MB、我们每 250ms 读一段"会永远追不上，
/// 面板显示的是几分钟前的内容却看不出异常。4MB / 250ms ≈ 16MB/s 的消化能力。
const LOG_FOLLOW_MAX_LAG: u64 = 4 * 1024 * 1024;

/// 认领后找日志文件的采样状态（见 `ProcessManager::probe_log_file`）
///
/// 为什么要跨轮保存：判据是"文件在不在长"，那至少要两次采样。只采一次就只能退回
/// "谁的修改时间最新"——那正是实测选错文件（选中只写两行的次要日志）、面板永远空白的原因。
struct LogProbe {
    /// 首次采样：候选与当时的字节数
    first: Vec<logfile::LogCandidate>,
}

/// 正在跟随的日志文件
///
/// 为什么要有它：`cmd /C startup.bat` 这类服务把真正的进程另开窗口跑（见 `AdoptedChild`），
/// stdout 不进我们的管道——那种情况下日志文件是**唯一**的内容来源。
struct LogFollow {
    /// 被跟随的文件路径（写进日志、供前端显示"在跟哪个文件"）
    path: String,
    /// 本跟随自己的停止标志。与服务的 `stop_flag` 是两个：取消跟随不该停服务，
    /// 停服务要连跟随一起停（见 `spawn_log_tailer` 里的两个判断）
    stop: Arc<AtomicBool>,
    /// 跟随线程结束信号：停止时用它限时等待（同 reader 线程的 done_*）
    done: Option<Receiver<()>>,
}

/// 认领到的游离进程：服务"另开窗口"跑起来的那个进程。
///
/// 为什么要有它：`cmd /C startup.bat` 里 catalina.bat 用的是 cmd 内建命令 `start`，
/// 它给子进程**新分配一个控制台**（CreateProcess + CREATE_NEW_CONSOLE）。后果有三：
/// ① 那个进程的输出不进我们的管道（面板看不到，日志只能另找来源）；
/// ② cmd 不等待它就退出 → 面板随即显示"已退出（退出码 0）"，是**谎报**；
/// ③ 点"停止"时进程表里已经没有这个服务了，是空操作 —— Tomcat 会一直跑下去。
/// 认领（按父 pid 链走查 + 启动时刻校验，见 `super::winproc`）之后，服务继续算运行中，
/// 停止能连同它一起结束，"停止"与"运行状态"两个语义都恢复正确。
struct AdoptedChild {
    pid: u32,
    /// 映像名（`java.exe` 等），只用于日志里说清接管了谁
    name: String,
    /// 句柄：判存活与取退出码都不受 pid 复用影响（见 `winproc::OwnedProcess`）
    handle: super::winproc::OwnedProcess,
}

/// 进程清理条目：停止 / 退出时交给 `cleanup_process` 的全部信息。
///
/// 原为 6 元组；加上认领信息、且 `child` 变成 `Option` 之后，位置已经读不出含义，
/// 按项目标准（参数 >4 个用结构体，见 `ServiceSpawn`）改为具名字段。
struct CleanupEntry {
    key: String,
    pid: u32,
    /// 直接子进程的启动时刻（ms）：收游离后代时用它排除 pid 复用
    started_at: u64,
    /// 直接子进程；已被认领路径回收（`wait` 过）时为 None
    child: Option<Child>,
    /// 认领到的游离进程：停止时与直接子进程一起结束
    adopted: Vec<AdoptedChild>,
    /// 正在跟随的日志文件：停止时一并收掉（否则线程会往已清空的缓冲里写）
    follow: Option<LogFollow>,
    done_stdout: Option<Receiver<()>>,
    done_stderr: Option<Receiver<()>>,
    stop_flag: Option<Arc<AtomicBool>>,
}

// ─── ProcessManager ─────────────────────────────────────────

struct ProcessInfo {
    /// 直接子进程（cmd / sh）。认领游离进程后置 None（它已退出并被回收）
    child: Option<Child>,
    pid: u32,
    /// 直接子进程的启动时刻（ms）。认领游离进程时用它排除 pid 复用：
    /// 系统会把旧 pid 分给无关进程，"父 pid 恰好指向我们"这件事本身不足以下结论
    spawn_started_at: u64,
    /// 认领到的游离进程（见 `AdoptedChild`）
    adopted: Vec<AdoptedChild>,
    /// 工作目录：认领后用它去找服务在写的日志文件（见 `core::logfile`）
    cwd: String,
    /// 正在跟随的日志文件（见 `LogFollow`）
    follow: Option<LogFollow>,
    /// 认领后找日志文件的采样状态（见 `LogProbe`）；None = 还没采过样
    log_probe: Option<LogProbe>,
    /// 所属项目（供 get_running 返回项目级运行状态）
    project_id: String,
    /// 日志批量发射端（启动时传入）。跟随日志文件要按需另起发射线程，用它
    log_sink: LogSink,
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
    log_buffers: LogBuffers,
    /// 意外退出的服务：key → 失败信息。日志保留供查看（正常停止/重启/全部停止时清除）
    failed: Mutex<HashMap<String, FailedService>>,
    /// 每 key 一把生命周期锁，让 start/stop/restart 对同一服务串行（NEW-17）。
    /// 键只增不删，理由见 `lifecycle_guard`。
    lifecycles: Mutex<HashMap<String, Arc<Mutex<()>>>>,
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
    pub log_sink: LogSink,
}

/// 日志批量事件的发送端（ARCH-14）。
///
/// 为什么不是 `tauri::AppHandle`：`core/` 反向依赖 IPC 框架会让 `ProcessManager::start`
/// 在**结构上**不可测——而它恰恰是唯一真的 spawn 子进程、可能留下孤儿、含 TOCTOU 清理分支
/// 的函数（24 个测试无一调用它）。换成"收一批日志，送去哪里由调用方决定"之后，
/// 测试可以塞一个 no-op 或记录用的 sink，直接驱动 start/stop 的完整生命周期。
///
/// 用 `Arc` 而不是借用：批量发射线程要求 `'static`（`thread::spawn` 的硬约束），
/// 借用进去编译不过；`Arc` 克隆进线程即可。
pub type LogSink = Arc<dyn Fn(ServiceLogBatchPayload) + Send + Sync>;

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            processes: Mutex::new(HashMap::new()),
            log_buffers: Arc::new(Mutex::new(HashMap::new())),
            failed: Mutex::new(HashMap::new()),
            lifecycles: Mutex::new(HashMap::new()),
            #[cfg(windows)]
            job: None,
        }
    }

    /// 取某 key 的生命周期锁（不存在则建）。
    ///
    /// 为什么必须有（NEW-17）：`start` 在"检查进程表"与"插入进程表"之间**必须释放锁**
    /// （spawn 不能持锁做）。服务 X 处于这个窗口时，`stop(X)` 查表为空 → 静默 no-op 并
    /// 返回 `Ok(())` → 用户收到"所有服务已停止"，而 X 随后被插表并**真实运行**。
    /// 加锁后 start/stop/restart 对同一 key 串行，窗口不再存在。
    ///
    /// 锁的获取顺序固定为 `lifecycles → processes`（本函数取完就释放，不与 processes 嵌套），
    /// `stop_all` 也不走 `stop`（直接 drain 表再清理），故不会出现反向持有。
    ///
    /// 表项**不回收**：删除表项会让"并发中的 start 握着旧 Arc、新来的 stop 拿到新 Arc"，
    /// 互斥直接失效。代价是每个曾启停过的服务 id 留一条 `String + Arc`——量级可忽略。
    fn lifecycle_lock(&self, key: &str) -> Arc<Mutex<()>> {
        let mut m = self.lifecycles.lock().unwrap_or_else(|e| {
            log::error!("[nexus] lifecycles 锁已中毒，继续使用: {}", e);
            e.into_inner()
        });
        Arc::clone(m.entry(key.to_string()).or_default())
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

    /// 启动单个服务。
    ///
    /// 全程持有该 key 的**生命周期锁**（NEW-17）——见 `lifecycle_guard` 的说明。
    pub fn start(&self, spec: ServiceSpawn<'_>) -> Result<(), String> {
        // 锁的 `Arc` 必须持有到函数结束：guard 借用的是它（不是堆上的 Mutex）
        let lock = self.lifecycle_lock(spec.service_id);
        let _guard = lock_lifecycle(&lock, spec.service_id);
        self.start_locked(spec)
    }

    fn start_locked(&self, spec: ServiceSpawn<'_>) -> Result<(), String> {
        let ServiceSpawn { project_id, service_id: key, name, command, cwd, env_vars, log_sink } = spec;
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
            // 纳管失败**必须让用户看见**（CQ-27）：它只在"Nexus 被强杀/崩溃"时才有后果
            // （该服务的进程会残留），而那正是用户最需要线索的时刻——只写进 nexus.log
            // 等于没有。这里写进服务自己的日志缓冲，日志面板里直接看得到
            if let Err(e) = job.assign_child(&child) {
                self.append_system_line(key, format!(
                    "⚠ 未能纳入 Job Object：{} —— Nexus 异常退出（崩溃/被杀）时该服务的进程可能残留，正常退出不受影响",
                    e
                ));
            }
        }

        let pid = child.id();
        let stdout = child.stdout.take().ok_or("无法获取 stdout")?;
        let stderr = child.stderr.take().ok_or("无法获取 stderr")?;

        // 使用 Arc<str> 作为 key，避免热路径 String clone（P2 #6）
        // done 信号：reader 线程结束时发送端 drop → recv 返回 Disconnected。
        // 清理时用 recv_timeout 等待线程退出并限时放弃，避免无限 join 阻塞主线程。
        let key1: LogKey = Arc::from(key);
        // 停止标志：cleanup 时置位，旧 reader 线程据此退出（否则 taskkill 未杀净时
        // 旧进程输出会串进新服务日志，且"已停止"的日志仍在增长）
        let stop_flag = Arc::new(AtomicBool::new(false));
        // 待发行缓冲：reader 只把行推进来，由每服务一个批量发射线程按 50ms 窗口合成一条事件。
        //
        // 为什么必须批量：每行一次 `emit` 要走完整链路——serde 序列化 → 拼 JS 字符串 →
        // EventLoopProxy 跨线程消息 → 主线程 ExecuteScript → 渲染进程一个 JS task。
        // 行速率由子进程决定且无上限（构建/进度条可达数千行/秒），逐行付这些固定成本
        // 正是"日志面板没打开也在付费"的地方。
        //
        // 为什么用独立线程而不是"攒够 N 行再发"：后者会让稀疏输出（交互式命令打印几行后
        // 静默）的最后几行一直压着，直到下次输出才显示。50ms 定时窗口既摊平突发，
        // 也不牺牲响应性。
        let pending_emit: Arc<Mutex<Vec<Arc<LogLine>>>> = Arc::new(Mutex::new(Vec::new()));
        // reader 存活计数：两个 reader 都退出（进程 EOF 或被停止）后发射线程收尾退出
        let readers_alive = Arc::new(std::sync::atomic::AtomicUsize::new(2));
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let stdout_thread = spawn_log_reader(
            stdout, "stdout", Arc::clone(&key1), Arc::clone(&self.log_buffers),
            Arc::clone(&pending_emit), Arc::clone(&readers_alive), Arc::clone(&stop_flag), done_tx,
        )
        .map_err(|e| {
            // 线程创建失败：杀掉已 spawn 的子进程并回收句柄，避免进程泄漏
            kill_and_reap(&mut child, Duration::from_millis(2000));
            format!("创建 stdout 日志读取线程失败: {}", e)
        })?;
        let _ = stdout_thread; // 不 join；线程在进程退出（EOF）后自然结束

        let (done_tx2, done_rx2) = std::sync::mpsc::channel::<()>();
        let mut done_rx_opt = Some(done_rx);
        let stderr_thread = spawn_log_reader(
            stderr, "stderr", Arc::clone(&key1), Arc::clone(&self.log_buffers),
            Arc::clone(&pending_emit), Arc::clone(&readers_alive), Arc::clone(&stop_flag), done_tx2,
        )
        .map_err(|e| {
            // stderr 线程创建失败：杀掉进程树并回收句柄（stdout 线程随后读到 EOF 自行退出）
            kill_and_reap(&mut child, Duration::from_millis(2000));
            format!("创建 stderr 日志读取线程失败: {}", e)
        })?;
        let _ = stderr_thread;

        // 批量发射线程（两个 reader 都创建成功后才起，避免失败路径留下孤儿线程）
        let flush_pending = Arc::clone(&pending_emit);
        let flush_alive = Arc::clone(&readers_alive);
        let flush_sink = Arc::clone(&log_sink);
        let flush_key = key.to_string();
        spawn_log_flush_thread(
            format!("nexus-logflush-{}", key),
            flush_pending,
            flush_alive,
            move |batch| {
                flush_sink(ServiceLogBatchPayload {
                    service_key: flush_key.clone(),
                    lines: batch,
                });
            },
        )
        .map_err(|e| {
            // 发射线程起不来：日志仍写缓冲（面板打开时能取到快照），只是失去实时推送
            kill_and_reap(&mut child, Duration::from_millis(2000));
            format!("创建日志批量发射线程失败: {}", e)
        })?;

        // Phase 3: 重新获取锁，二次检查后插入
        let mut procs = self.processes.lock().unwrap_or_else(|e| {
            log::error!("[nexus] processes 锁已中毒，继续使用: {}", e);
            e.into_inner()
        });
        if procs.contains_key(key) {
            // TOCTOU 竞态：另一个线程已插入同 key，清理当前创建的资源
            log::warn!("[nexus] TOCTOU 竞态: {} 已在运行中，清理泄漏的子进程", key);
            cleanup_process(CleanupEntry {
                key: key.to_string(), pid,
                // 这个进程刚 spawn 出来、还活着，取不到时间的概率极低；取不到记 0，
                // 只会让"游离后代"的时间校验最宽松（这条路径随后就把它杀掉了）
                started_at: process_start_time_millis(pid).unwrap_or(0),
                child: Some(child), adopted: Vec::new(), follow: None,
                done_stdout: done_rx_opt.take(), done_stderr: Some(done_rx2),
                stop_flag: Some(Arc::clone(&stop_flag)),
            });
            return Err(format!("{} 已在运行中", key));
        }
        // 直接子进程的启动时刻：认领游离进程时用它排除 pid 复用。
        // 取不到（进程已退出/无权限）时记 0——0 让"晚于它"的校验最宽松，
        // 而这个分支紧随其后就会被 running() 判成"已退出"，不会留下长期影响
        let spawn_started_at = process_start_time_millis(pid).unwrap_or(0);
        procs.insert(key.to_string(), ProcessInfo {
            child: Some(child), pid, spawn_started_at, adopted: Vec::new(),
            cwd: cwd.to_string(), follow: None, log_probe: None,
            project_id: project_id.to_string(),
            log_sink: Arc::clone(&log_sink),
            done_stdout: done_rx_opt,
            done_stderr: Some(done_rx2),
            stop_flag,
        });
        Ok(())
    }

    /// 停止单个服务。全程持有该 key 的生命周期锁（NEW-17）。
    pub fn stop(&self, key: &str) -> Result<(), String> {
        let lock = self.lifecycle_lock(key);
        let _guard = lock_lifecycle(&lock, key);
        self.stop_locked(key)
    }

    fn stop_locked(&self, key: &str) -> Result<(), String> {
        log::info!("[nexus] 停止服务: {}", key);

        // Phase 1: 从 map 中移除 entry，释放锁
        let entry: Option<CleanupEntry> = {
            let mut procs = self.processes.lock().unwrap_or_else(|e| {
                log::error!("[nexus] processes 锁已中毒，继续使用: {}", e);
                e.into_inner()
            });
            procs.remove(key).map(|mut info| take_cleanup_entry(key.to_string(), &mut info))
        };

        // Phase 2: 在锁外执行清理
        let outcome = if let Some(entry) = entry {
            cleanup_process(entry)
        } else {
            log::debug!("[nexus] stop: 服务 {} 未在运行，忽略", key);
            CleanupOutcome::default()
        };
        // 主动停止 = 正常关闭：无论进程是否还在管理表（崩溃的服务已被 running() 移出），
        // 都清除失败标记并清空日志（含失败日志）——项目"全部停止"依赖此语义
        if let Ok(mut failed) = self.failed.lock() {
            failed.remove(key);
        }
        if let Ok(mut buffers) = self.log_buffers.lock() {
            let log_key: LogKey = Arc::from(key);
            buffers.remove(&*log_key);
        }
        // **收尸没干净就如实上报**（CQ-25）：表项此时已经被摘掉，`get_running()` 会报它
        // "未运行"，界面显示已停止，而进程仍在跑并占着端口。上层（stop_project_services /
        // delete_service）本来就是按"可能失败"写的，这条 Err 才是那些分支的第一个真实来源
        if !outcome.is_clean() {
            return Err(format!("停止未完成：{}", outcome.describe()));
        }
        Ok(())
    }

    /// 重启单个服务。
    ///
    /// **整段**（stop + start）持有同一把生命周期锁：若只让 stop/start 各自加锁，
    /// 两者之间会漏出一个窗口——并发调用方可以在这个窗口里插进自己的 start，
    /// 于是"重启"的结果取决于调度顺序。这里用 `*_locked` 变体在同一把锁内完成两段。
    pub fn restart(&self, spec: ServiceSpawn<'_>) -> Result<(), String> {
        let lock = self.lifecycle_lock(spec.service_id);
        let _guard = lock_lifecycle(&lock, spec.service_id);
        // 旧进程没确认退出**不阻塞重启**（CQ-25）：用户的诉求是"把它跑起来"，而新进程
        // 起不来时后端会如实报端口/启动错误——那比"重启被拒绝"更接近用户意图。
        // 但要留痕：这说明旧进程可能还占着端口
        if let Err(e) = self.stop_locked(spec.service_id) {
            log::warn!("[nexus] 重启前停止旧进程未完全成功，继续尝试启动: {}", e);
        }
        self.start_locked(spec)
    }

    /// 追加系统标记行前，先调用者已确认需要写；锁失败时静默忽略（不影响主流程）
    pub fn append_system_line(&self, key: &str, text: String) {
        append_system_line(&self.log_buffers, key, text);
    }

    /// 取某服务的日志快照（锁内只拷 `Arc` 指针，字符串拷贝交给调用方的序列化）。
    ///
    /// 之前这里在锁内做 2000 次深拷贝：全局日志锁被持有期间，所有服务的 reader 线程
    /// 都阻塞在 `push_log_line` 的同一把锁上（已读到的行既不进缓冲也不 emit）。
    pub fn get_logs(&self, key: &str) -> Vec<Arc<LogLine>> {
        let log_key: LogKey = Arc::from(key);
        match self.log_buffers.lock() {
            Ok(b) => b.get(&*log_key)
                .map(|buf| buf.lines.iter().cloned().collect())
                .unwrap_or_default(),
            Err(e) => {
                log::error!("ProcessManager log_buffers 锁已中毒: {}", e);
                Vec::new()
            }
        }
    }

    /// 跟随某个日志文件：把它的新增内容接进服务日志（`stream = "file"`）。
    ///
    /// 已跟随的会被替换（同一个服务只跟一个文件：多来源会让面板没法说清每行来自哪）。
    /// 文件打不开时**当场报错**，不留到线程里静默失败——前端要能说出为什么。
    pub fn follow_log(&self, key: &str, path: &str) -> Result<(), String> {
        // 与 start/stop 同一把生命周期锁：否则"轮询触发的自动跟随"与"用户手动跟随"
        // 可能同时给一个服务起两个跟随线程
        let lock = self.lifecycle_lock(key);
        let _guard = lock_lifecycle(&lock, key);
        self.unfollow_log_locked(key);

        let (sink, service_stop) = {
            let procs = self.processes.lock().unwrap_or_else(|e| {
                log::error!("ProcessManager processes 锁已中毒，继续使用: {}", e);
                e.into_inner()
            });
            let Some(info) = procs.get(key) else {
                return Err(format!("服务 {} 未在运行，无法跟随日志文件", key));
            };
            (Arc::clone(&info.log_sink), Arc::clone(&info.stop_flag))
        };
        // 打开 + 定位到末尾都在这里（调用线程）做完，见 `spawn_log_tailer` 的说明：
        // 跟随的时间边界就是这一刻，之后写进文件的每一行都会进面板
        let mut file = std::fs::File::open(path).map_err(|e| format!("打开日志文件失败: {}（{}）", path, e))?;
        let len = file.seek(SeekFrom::End(0)).map_err(|e| format!("定位日志文件末尾失败: {}（{}）", path, e))?;
        // 回看一段（见 LOG_FOLLOW_SEED_BYTES）：服务刚启动那几行是最想看到的内容。
        // 必须**无条件** seek：文件比回看窗口小时 offset 为 0，而此刻文件位置还在末尾
        // （上一步 seek(End) 留下的），不 seek 回去就成了"什么都不带出来"
        let offset = len.saturating_sub(LOG_FOLLOW_SEED_BYTES);
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| format!("定位日志文件跟随起点失败: {}（{}）", path, e))?;

        let follow_stop = Arc::new(AtomicBool::new(false));
        let pending: Arc<Mutex<Vec<Arc<LogLine>>>> = Arc::new(Mutex::new(Vec::new()));
        // 自己的存活计数（初值 1）+ 自己的发射线程：与进程 stdout 的发射线程互不影响
        // （stdout 那两个 reader 此时早已 EOF 退出，它们的发射线程也已经收尾）
        let alive = Arc::new(std::sync::atomic::AtomicUsize::new(1));
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let log_key: LogKey = Arc::from(key);
        let flush_sink = Arc::clone(&sink);
        let flush_key = key.to_string();
        spawn_log_flush_thread(
            format!("nexus-followflush-{}", key),
            Arc::clone(&pending),
            Arc::clone(&alive),
            move |batch| {
                flush_sink(ServiceLogBatchPayload { service_key: flush_key.clone(), lines: batch });
            },
        )
        .map_err(|e| format!("创建日志发射线程失败: {}", e))?;
        spawn_log_tailer(
            file,
            offset,
            PathBuf::from(path),
            log_key,
            Arc::clone(&self.log_buffers),
            pending,
            alive,
            Arc::clone(&follow_stop),
            service_stop,
            done_tx,
        )
        .map_err(|e| format!("创建日志跟随线程失败: {}", e))?;

        let mut procs = self.processes.lock().unwrap_or_else(|e| {
            log::error!("ProcessManager processes 锁已中毒，继续使用: {}", e);
            e.into_inner()
        });
        if let Some(info) = procs.get_mut(key) {
            info.follow = Some(LogFollow {
                path: path.to_string(),
                stop: follow_stop,
                done: Some(done_rx),
            });
        }
        Ok(())
    }

    /// 取消跟随（服务本身不受影响），返回被取消的文件路径
    pub fn unfollow_log(&self, key: &str) -> Option<String> {
        let lock = self.lifecycle_lock(key);
        let _guard = lock_lifecycle(&lock, key);
        self.unfollow_log_locked(key)
    }

    fn unfollow_log_locked(&self, key: &str) -> Option<String> {
        let mut procs = match self.processes.lock() {
            Ok(p) => p,
            Err(e) => {
                log::error!("ProcessManager processes 锁已中毒，继续使用: {}", e);
                e.into_inner()
            }
        };
        let follow = procs.get_mut(key)?.follow.take()?;
        follow.stop.store(true, Ordering::Relaxed);
        Some(follow.path)
    }

    /// 取某服务的 (工作目录, 启动时刻)；服务不在表里时 None
    fn cwd_and_start_for(&self, key: &str) -> Option<(String, u64)> {
        let procs = self.processes.lock().ok()?;
        let info = procs.get(key)?;
        Some((info.cwd.clone(), info.spawn_started_at))
    }

    /// 当前跟随的文件路径（前端显示"在跟哪个文件"用）
    pub fn followed_log(&self, key: &str) -> Option<String> {
        self.processes
            .lock()
            .ok()?
            .get(key)?
            .follow
            .as_ref()
            .map(|f| f.path.clone())
    }

    /// 认领到游离进程后：这个服务的 stdout 已经指望不上了，找找它在写哪个日志文件。
    ///
    /// **两阶段采样**，不是一次扫描：第一次只记下候选与大小，下一轮再看谁长大了，
    /// 跟"确实在写"的那个。为什么必须这样（实测踩过）：Tomcat 启动瞬间会一次性创建
    /// catalina/localhost/host-manager 几个日志文件，按"修改时间最新"选中的是只写了两行
    /// 就再也不动的 `localhost.log` —— 而跟随线程从文件末尾读起，于是面板**永远是空的**。
    /// ActiveMQ 同样：`data/` 里 `wrapper.log` 与 `activemq.log` 都在写，谁"最新"是随机的。
    ///
    /// 一个都没长（服务还没开写日志）就不表态，下一轮再采样——**一直等下去**：
    /// Tomcat 要等 JVM 起来才落盘，实测有十几秒才写第一行的情况。
    fn probe_log_file(&self, key: &str, cwd: &str, since: SystemTime) {
        if self.followed_log(key).is_some() {
            return;
        }
        let now = logfile::candidate_logs(Path::new(cwd), since);
        let first = {
            let mut procs = match self.processes.lock() {
                Ok(p) => p,
                Err(e) => {
                    log::error!("ProcessManager processes 锁已中毒，继续使用: {}", e);
                    e.into_inner()
                }
            };
            let Some(info) = procs.get_mut(key) else { return };
            match &mut info.log_probe {
                // 已有首采：拿它跟这一轮比
                Some(probe) => probe.first.clone(),
                None => {
                    // 首采：这轮只记录，不选（要有两次采样才能看出"谁在长"）
                    info.log_probe = Some(LogProbe { first: now });
                    return;
                }
            }
        };
        let picked = logfile::pick_growing(&first, &now);
        let Some(path) = picked else {
            return; // 还没长：下一轮再看
        };
        match self.follow_log(key, &path.to_string_lossy()) {
            Ok(()) => {
                log::info!("[nexus] 已跟随日志文件（在增长）: {}", path.display());
                self.append_system_line(key, format!(
                    "已跟随日志文件：{}（该服务另开窗口运行，面板从文件读取；右键服务可取消跟随）",
                    path.display()
                ));
            }
            Err(e) => {
                log::warn!("[nexus] 跟随日志文件失败 {}: {}", path.display(), e);
                self.append_system_line(key, format!("发现日志文件 {} 但跟随失败：{}", path.display(), e));
            }
        }
    }

    /// 返回当前运行中的 (project_id, service_id) 列表。
    ///
    /// 比"直接子进程退出即服务结束"多一步**游离进程认领**：直接子进程（cmd）退出后先不
    /// 下结论，看它有没有留下"另开窗口"跑起来的后代（Tomcat 的 `startup.bat` →
    /// `catalina.bat start` → `start "Tomcat" java`，见 `AdoptedChild`）。
    /// 认领到就继续算运行中，认领不到才按退出处理。
    pub fn running(&self) -> Vec<(String, String)> {
        /// 本轮判定为"进程刚结束"的服务
        struct JustEnded {
            key: String,
            pid: u32,
            started_at: u64,
            exit_code: Option<i32>,
            /// true = 直接子进程退出（要走查游离后代）；false = 认领的进程全退了
            check_survivors: bool,
        }

        // Phase 1（锁内，只做判定）：child 存活的跳过；child 已退出的记为候选；
        // 认领模式的看认领到的进程是否已全部退出。
        let mut ended: Vec<JustEnded> = Vec::new();
        // 需要去找日志文件的服务：(key, cwd, 服务启动时刻)
        let mut probe: Vec<(String, String, u64)> = Vec::new();
        {
            let mut procs = self.processes.lock().unwrap_or_else(|e| {
                log::error!("ProcessManager processes 锁已中毒，继续使用: {}", e);
                e.into_inner()
            });
            for (k, info) in procs.iter_mut() {
                match info.child.as_mut() {
                    Some(child) => match child.try_wait() {
                        Ok(Some(status)) => ended.push(JustEnded {
                            key: k.clone(),
                            pid: info.pid,
                            started_at: info.spawn_started_at,
                            exit_code: status.code(),
                            check_survivors: true,
                        }),
                        Ok(None) => {} // 运行中
                        Err(e) => {
                            // 无法检查状态：保守按"仍存活"处理（原实现判为已死 → 摘表 + 标失败，
                            // 而进程可能仍在运行且此后不再受管理）
                            log::warn!("[nexus] 查询进程状态失败 ({}): {}（按存活处理）", k, e);
                        }
                    },
                    None => {
                        // 认领模式：认领到的进程全部退出，才算这个服务结束。
                        // 退出码取第一个认领进程的——多个游离进程时"服务退出码"本就没有单一
                        // 含义，给一个可读的值即可（拿不到才落到 "?"）
                        if info.adopted.iter().all(|a| !a.handle.is_alive()) {
                            ended.push(JustEnded {
                                key: k.clone(),
                                pid: info.pid,
                                started_at: info.spawn_started_at,
                                exit_code: info.adopted.iter().find_map(|a| a.handle.exit_code()),
                                check_survivors: false,
                            });
                        } else if info.follow.is_none() {
                            // 认领意味着 stdout 拿不到东西了：找找它在写哪个日志文件。
                            // **不设尝试上限**：服务"开写第一行日志"可能要好几秒（Tomcat 要等 JVM
                            // 起来才落盘，实测有过 10 秒以上），设上限就会把这类服务永远排除在外。
                            // 代价只是每轮读几次目录，而服务一停这条路径就没了
                            probe.push((k.clone(), info.cwd.clone(), info.spawn_started_at));
                        }
                    }
                }
            }
        }

        // Phase 1.5（锁外）：走查游离后代。系统调用（进程快照 / OpenProcess）放在锁外，
        // 与"锁内只做 try_wait + 摘表"的既有约定一致。
        // 用 HashMap 而不是 Vec：`AdoptedChild` 持有进程句柄、**不可 Clone**，
        // 取出即消费（`remove`）才不会重复持有。
        let mut survived: HashMap<String, Vec<AdoptedChild>> = ended
            .iter()
            .filter(|e| e.check_survivors)
            .filter_map(|e| {
                let list = adopt_survivors(e.pid, e.started_at);
                if list.is_empty() { None } else { Some((e.key.clone(), list)) }
            })
            .collect();

        // Phase 2（锁内）：落实。认领成功的摘掉 child（已退出，只回收句柄）并存入 adopted；
        // 其余摘表，交给 Phase 2.5 收尸。
        let mut exited: Vec<(String, Option<i32>)> = Vec::new();
        let mut dead: Vec<CleanupEntry> = Vec::new();
        let mut adopted_logs: Vec<(String, String)> = Vec::new();
        {
            let mut procs = self.processes.lock().unwrap_or_else(|e| {
                log::error!("ProcessManager processes 锁已中毒，继续使用: {}", e);
                e.into_inner()
            });
            for e in ended {
                let Some(list) = survived.remove(&e.key) else {
                    // 没有游离后代：按退出处理
                    exited.push((e.key.clone(), e.exit_code));
                    if let Some(mut info) = procs.remove(&e.key) {
                        dead.push(take_cleanup_entry(e.key, &mut info));
                    }
                    continue;
                };
                // 认领成功。期间可能已被 stop 摘走 —— 那就什么都不做，
                // 句柄随 list 在这里 drop（进程已被 stop 结束）
                let Some(info) = procs.get_mut(&e.key) else { continue };
                if let Some(mut child) = info.child.take() {
                    let _ = child.wait(); // 已退出：只回收句柄，不 taskkill
                }
                let who = list.iter().map(|a| format!("{} (pid {})", a.name, a.pid))
                    .collect::<Vec<_>>().join("、");
                info.adopted = list;
                adopted_logs.push((e.key, who));
            }
        }

        // Phase 2.5（锁外）：写日志标记与失败状态（原实现在持锁期间做）
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
        // 认领成功必须**说出来**：否则面板上只有那行"进程已退出（退出码: 0）"，
        // 用户以为服务没了，而它其实还在跑 —— 这正是本次要修掉的谎报
        for (key, who) in &adopted_logs {
            self.append_system_line(key, format!(
                "直接子进程已退出，但它另开窗口启动的进程仍在运行（{}）；已接管：「停止」会一并结束它们",
                who
            ));
            // 刚认领的服务也立刻找一次日志文件（不用等下一轮轮询）
            if let Some((cwd, started_at)) = self.cwd_and_start_for(key) {
                probe.push((key.clone(), cwd, started_at));
            }
        }
        // 找日志文件（锁外：要读目录、开文件）
        for (key, cwd, started_at) in probe {
            self.probe_log_file(&key, &cwd, system_time_from_millis(started_at));
        }

        // Phase 3（锁外）：收尸已退出进程（已退出的进程不再 taskkill）。
        // 日志缓冲保留（Phase 2.5 已追加退出标记行）：崩溃/秒退的报错是诊断关键。
        // 这一支是"进程已经退出"才走到这里的，理论上收尸必然干净；不干净时留痕
        // （通常意味着 pid 刚被复用，或还有没杀掉的游离后代）
        for entry in dead {
            let outcome = cleanup_process(entry);
            if !outcome.is_clean() {
                log::warn!("[nexus] 收尸未完全干净: {}", outcome.describe());
            }
        }

        // Phase 4: 重新获取锁返回当前运行中的 (project_id, service_id)
        self.processes.lock()
            .map(|procs| procs.iter().map(|(k, v)| (v.project_id.clone(), k.clone())).collect())
            .unwrap_or_default()
    }

    pub fn stop_all(&self) {
        let entries: Vec<CleanupEntry> = {
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
                take_cleanup_entry(key, &mut info)
            }).collect()
        };

        for entry in entries {
            let (key, pid) = (entry.key.clone(), entry.pid);
            let outcome = cleanup_process(entry);
            // 退出路径没有"失败上报"的接收方（进程马上要结束），但仍要留痕：
            // 残留进程会由 Job Object 在应用退出时兜底，而日志是唯一能解释
            // "为什么退出后 taskmgr 里还有 java.exe"的地方
            if !outcome.is_clean() {
                log::warn!("[nexus]   清理 {} (pid={}) 未完全成功: {}", key, pid, outcome.describe());
            } else {
                log::debug!("[nexus]   已清理 {} (pid={})", key, pid);
            }
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

    /// 测试用：取某服务当前认领到的游离进程 (pid, 映像名)。
    ///
    /// 真实写入路径是 `running()` 轮询里的认领（需要真起一个"另开窗口"的进程才能走到），
    /// 这里只把结果读出来供断言——不暴露任何内部状态给生产代码。
    #[cfg(test)]
    pub(crate) fn adopted_pids_for_test(&self, key: &str) -> Vec<(u32, String)> {
        self.processes
            .lock()
            .map(|procs| {
                procs.get(key)
                    .map(|info| info.adopted.iter().map(|a| (a.pid, a.name.clone())).collect())
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    }

    /// 测试用：注入一条"意外退出"记录。
    ///
    /// 真实写入发生在 reader 线程发现子进程意外退出时，需要真起一个进程才能触发；
    /// 这里只验证失败表本身的语义（读取、以及 stop 后清除），不重复测进程 spawn。
    #[cfg(test)]
    pub(crate) fn record_failure_for_test(&self, key: &str, exit_code: Option<i32>) {
        if let Ok(mut failed) = self.failed.lock() {
            failed.insert(key.to_string(), FailedService {
                service_id: key.to_string(),
                exit_code,
                timestamp: "test".into(),
            });
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
fn append_system_line(buffers: &LogBuffers, key: &str, text: String) {
    if let Ok(mut b) = buffers.lock() {
        let e = b.entry(Arc::from(key)).or_default();
        e.push(Arc::new(LogLine { seq: LOG_SEQ.fetch_add(1, Ordering::Relaxed), timestamp: chrono::Utc::now().to_rfc3339(), stream: "system".into(), text }));
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
    // 失败留痕（CQ-25）：这里没有接收方（早退/超时路径），但"没杀掉"必须能在日志里看见
    if let Err(e) = kill_process_tree(pid) {
        log::warn!("[nexus] 终止进程树失败 (pid={}): {}", pid, e);
    }
    if wait_with_timeout(child, timeout) {
        let _ = child.wait();
    } else {
        log::warn!("[nexus] 进程 (pid={}) 未能按时退出，已交由 Job Object 兜底", pid);
    }
}

/// 把服务表项拆成清理条目（`child` / `adopted` 一并带走：停止要连同它们一起结束）
fn take_cleanup_entry(key: String, info: &mut ProcessInfo) -> CleanupEntry {
    CleanupEntry {
        key,
        pid: info.pid,
        started_at: info.spawn_started_at,
        child: info.child.take(),
        adopted: std::mem::take(&mut info.adopted),
        follow: info.follow.take(),
        done_stdout: info.done_stdout.take(),
        done_stderr: info.done_stderr.take(),
        stop_flag: Some(Arc::clone(&info.stop_flag)),
    }
}

/// `pid` 名下、创建时刻晚于 `started_at` 的后代 pid（含"另开窗口"跑掉的那些）。
///
/// 启动时刻这一关是**防 pid 复用**：系统会把旧 pid 分给无关进程，而新进程可能恰好
/// 由"复用了我们那条 cmd 的 pid"的进程生出来，此时父链看起来完全合法。
/// 时间不可能倒流——候选的创建时刻早于我们那条子进程，就一定不是它生出来的。
///
/// 非 Windows 上 `snapshot_processes` 返回空表，这里自然也返回空。
fn candidate_descendants(pid: u32, started_at: u64) -> Vec<u32> {
    let snapshot = super::winproc::snapshot_processes();
    super::winproc::descendants(pid, &snapshot)
        .into_iter()
        .filter(|&child| {
            process_start_time_millis(child)
                .map(|t| t >= started_at)
                .unwrap_or(false)
        })
        .collect()
}

/// 控制台宿主进程名：它们不是服务在跑的东西，而是"另开窗口"时系统给新控制台配的宿主。
///
/// 为什么要排除（实测踩到）：
/// 1. cmd 用 `start` 另开窗口时会拉起 `conhost.exe`，父链上它也是后代，于是被一起认领——
///    接管名单里混进一个 `conhost.exe`，用户看到的日志像是 Nexus 管错了进程；
/// 2. 更麻烦的是它**持有被托管进程的句柄**，客户端被结束后对象不会立刻销毁，
///    于是"进程是否还在"的两种判据（按 pid 查得到 vs taskkill 说找不到）会打架；
/// 3. 杀掉真正的工作进程时，`taskkill /T` 顺带就会结束它的控制台宿主，不缺这一步。
///
/// 名字匹配是启发式，但代价只是"少接管一个控制台宿主"：真出错也不会漏掉工作进程。
/// `OpenConsole.exe` 是 Windows 11 上 Windows Terminal 作为默认终端时的同类进程。
fn is_console_host(name: &str) -> bool {
    name.eq_ignore_ascii_case("conhost.exe") || name.eq_ignore_ascii_case("OpenConsole.exe")
}

/// 认领游离进程：走查 `pid` 名下仍在运行的后代，逐个打开句柄。
///
/// 返回空 = 确实没有游离进程（调用方按服务真的退出处理）。
/// 只认得下还在跑的：已退出的进程没必要接管，它的退出也不该让"服务"继续算运行中。
fn adopt_survivors(pid: u32, started_at: u64) -> Vec<AdoptedChild> {
    let snapshot = super::winproc::snapshot_processes();
    let mut out = Vec::new();
    for child_pid in super::winproc::descendants(pid, &snapshot) {
        if !process_start_time_millis(child_pid).map(|t| t >= started_at).unwrap_or(false) {
            continue;
        }
        let name = super::winproc::name_of(child_pid, &snapshot);
        if is_console_host(&name) {
            continue;
        }
        let Some(handle) = super::winproc::OwnedProcess::open(child_pid) else {
            continue; // 已退出 / 无权限：不接管
        };
        if !handle.is_alive() {
            continue;
        }
        out.push(AdoptedChild { pid: child_pid, name, handle });
    }
    out
}

/// 清理单个进程条目（在锁外调用）
///
/// 流程：先结束**认领到的游离进程**（它们不在我们的进程树里，漏掉就等于"停止"没生效）
/// → try_wait 确认直接子进程仍存活才 taskkill（已退出时 PID 可能已被系统复用，
/// 直接 taskkill 会误杀无辜进程）→ 顺手收掉直接子进程名下跑出去的游离后代
/// （停止与下一次 `running()` 轮询之间存在窗口，这个窗口里 stop 也必须收干净）
/// → 带超时等待退出 → 等待 reader 线程结束（进程死后管道写端关闭，reader 读到 EOF
/// 自然退出）→ 超时则放弃等待，交由 Job Object（KILL_ON_JOB_CLOSE）在应用退出时兜底。
///
/// 注意：等待 reader 线程用 recv_timeout 而非 join——进程树未全灭时
/// （taskkill 失败、或孙进程仍持有管道写端）EOF 永不发生，join 会无限阻塞
/// 同步命令的主线程（stop/get_running 全部卡死、窗口关不掉）。
/// 一次收尸的结果。
///
/// 为什么要有返回值（CQ-25）：上层本来就是**按"stop 可能失败"写的**——
/// `stop_project_services` 逐个收集失败、`delete_service`/`delete_project` 都留痕，
/// 而这里此前连返回值都没有、`stop_locked` 只有一个 `Ok(())` 出口，那些 Err 分支
/// **结构性不可达**。真实后果：taskkill 失败或进程 2 秒内没死时，表项已被摘掉，
/// `get_running()` 于是报它"未运行"，界面显示已停止，而进程仍在跑并占着端口。
///
/// 判据是**复核存活**而不是 taskkill 的退出码：进程不存在时 taskkill 也返回非零，
/// 而那正是"已经收干净了"。
#[derive(Default)]
pub(crate) struct CleanupOutcome {
    /// 收尸结束时仍存活的 pid（有它 = 这次停止没生效）
    survivors: Vec<u32>,
    /// taskkill 的失败原文（诊断上下文；它本身不等于收尸失败）
    errors: Vec<String>,
}

impl CleanupOutcome {
    pub(crate) fn is_clean(&self) -> bool {
        self.survivors.is_empty()
    }

    /// 给人看的原因（错误信息里带上，界面与日志都能说清"为什么说没停掉"）
    pub(crate) fn describe(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.survivors.is_empty() {
            parts.push(format!(
                "仍有 {} 个进程未退出（{:?}）——可能正在退出中，或被安全软件拦截了终止",
                self.survivors.len(),
                self.survivors
            ));
        }
        parts.extend(self.errors.iter().cloned());
        parts.join("；")
    }
}

/// 收尸后复核一批 pid 是否真的没了。
///
/// 用 `OwnedProcess::open` 拿到句柄再判存活（而不是 `process_start_time_millis`）：
/// 句柄身份不受 pid 复用影响，这里刚杀完就复用的概率虽低，但"误报已退出"正是
/// 本项目最忌讳的错误方向。
fn survivors_of(pids: &[u32]) -> Vec<u32> {
    pids.iter()
        .copied()
        .filter(|pid| super::winproc::OwnedProcess::open(*pid).map(|h| h.is_alive()).unwrap_or(false))
        .collect()
}

fn cleanup_process(entry: CleanupEntry) -> CleanupOutcome {
    let CleanupEntry { key: _key, pid, started_at, mut child, adopted, follow, done_stdout, done_stderr, stop_flag } = entry;
    // 先置停止标志：reader 线程立刻停止写缓冲与 emit。
    // 否则进程树未杀净（孙进程持管道）时旧线程会继续往同一个 key 写日志——
    // 表现为"已停止的服务日志还在涨"，重启后新旧进程输出还会串台
    if let Some(flag) = &stop_flag {
        flag.store(true, Ordering::Relaxed);
    }
    // 跟随线程同理（它看的是同一个 stop_flag，这里额外等它退出）：
    // 不等的话，stop 之后它还会往"已被 remove 的日志缓冲"里写——那等于把缓冲又建回来，
    // 这块内存直到下次启动同 id 的服务才会被替换
    let mut follow_done = None;
    if let Some(mut f) = follow {
        f.stop.store(true, Ordering::Relaxed);
        follow_done = f.done.take();
    }
    // 认领到的游离进程：父进程（cmd）早已退出，它们不在任何人的进程树下，
    // 必须按 pid 单独结束 —— 漏掉就是"点了停止，Tomcat 还在后台跑"
    let mut outcome = CleanupOutcome::default();
    let mut kill = |pid: u32, what: &str| {
        if let Err(e) = kill_process_tree(pid) {
            log::warn!("[nexus] 结束{}失败 (pid={}): {}", what, pid, e);
            outcome.errors.push(e);
        }
    };
    for a in &adopted {
        log::info!("[nexus] 结束接管进程 {} (pid={})", a.name, a.pid);
        kill(a.pid, "接管进程");
    }
    let alive = matches!(child.as_mut().map(|c| c.try_wait()), Some(Ok(None)));
    if alive {
        kill(pid, "子进程");
    }
    // 还活着的直接子进程名下，可能已经有"另开窗口"跑出去的后代（此刻它们尚未被认领，
    // 或刚被认领前就被 stop 撞上）。一并收掉，避免留下占端口的孤儿。
    let orphans = candidate_descendants(pid, started_at);
    for orphan in &orphans {
        log::info!("[nexus] 结束游离孙进程 (pid={})", orphan);
        kill(*orphan, "游离孙进程");
    }
    let exited = match child.as_mut() {
        Some(c) => wait_with_timeout(c, Duration::from_millis(2000)),
        // 认领路径已经 `wait` 回收过直接子进程：没有可等的东西，也不该再 warn
        None => true,
    };
    // 等待 reader 线程结束：正常路径（进程已死）立即返回；异常路径最多等 1 秒后放弃，
    // 线程在进程树最终退出后自然结束（不 join 不会泄漏——线程自行退出即释放资源）
    for rx in [done_stdout, done_stderr, follow_done].into_iter().flatten() {
        let _ = rx.recv_timeout(Duration::from_millis(1000));
    }
    match child.as_mut() {
        Some(c) if exited => {
            let _ = c.wait();
        }
        Some(_) => {
            log::warn!("[nexus] 进程 (pid={}) 未能按时退出，已释放句柄，由 Job Object 在应用退出时兜底", pid);
        }
        None => {}
    }
    // **复核存活**：直接子进程、接管进程、游离孙进程都再问一遍（用句柄，不受 pid 复用影响）。
    // 这一步才是"stop 到底成没成"的判据——taskkill 退出码非零可能只是进程本来就不存在
    // （那正是"已经收干净了"），而一个安静的 taskkill 也可能什么都没杀成（CQ-25）
    if !exited {
        outcome.survivors.push(pid);
    }
    let mut watch: Vec<u32> = adopted.iter().map(|a| a.pid).collect();
    watch.extend(orphans.iter().copied());
    outcome.survivors.extend(survivors_of(&watch));
    // 认领到的句柄随 `adopted` 在这里 drop（CloseHandle）——进程对象随之释放，
    // 不留句柄泄漏
    outcome
}

/// 进程启动时间（毫秒，距 UNIX 纪元）；进程不存在/无权限时返回 None。
///
/// 用途：pid 会被系统回收复用，"按 pid 杀进程"必须先确认这个 pid 还是当初那个进程。
/// 启动时间是 (pid, 启动时刻) 二元组的第二半——同一个 pid 两次启动的时间不可能相同，
/// 而存活中的进程启动时间不变，所以"记录时 == 现在"即可判定身份未变。
#[cfg(windows)]
pub(crate) fn process_start_time_millis(pid: u32) -> Option<u64> {
    // Win32 类型别名（按 clippy 命名习惯写，避免 upper_case_acronyms 抑制）
    type WinHandle = isize;
    type WinBool = i32;
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    extern "system" {
        fn OpenProcess(access: u32, inherit: WinBool, pid: u32) -> WinHandle;
        fn GetProcessTimes(
            handle: WinHandle,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> WinBool;
        fn CloseHandle(handle: WinHandle) -> WinBool;
    }
    // PROCESS_QUERY_LIMITED_INFORMATION：查启动时间所需的最小权限
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    // FILETIME 起点是 1601-01-01，与 UNIX 纪元差 11644473600 秒
    const FILETIME_UNIX_DIFF_SECS: u64 = 11_644_473_600;

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle == 0 {
            return None;
        }
        let mut creation = FileTime::default();
        let mut exit = FileTime::default();
        let mut kernel = FileTime::default();
        let mut user = FileTime::default();
        let ok = GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        // 100ns 单位 → 毫秒；跨度远超 u64 溢出范围（584 年），只做一次减法
        let ticks = ((creation.high as u64) << 32) | creation.low as u64;
        let secs = ticks / 10_000_000;
        if secs < FILETIME_UNIX_DIFF_SECS {
            return None;
        }
        Some((secs - FILETIME_UNIX_DIFF_SECS) * 1000 + (ticks % 10_000_000) / 10_000)
    }
}

#[cfg(not(windows))]
pub(crate) fn process_start_time_millis(_pid: u32) -> Option<u64> {
    None
}

/// 终止进程树（taskkill /T /F）。工具命令超时等场景也需要，故设为 pub(crate)
pub(crate) fn kill_process_tree(pid: u32) -> Result<(), String> {
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let child = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            // 跳过"当前目录优先"搜索（SEC-14，与 `build_internal_command` 同口径）：
            // taskkill 是我们自己拉起的系统工具、不承载用户 shell，关掉这一级没有副作用，
            // 却能避免"CWD 恰好是一个不可信仓库"时执行到那里预置的 taskkill.exe
            .env("NoDefaultCurrentDirectoryInExePath", "1")
            // stderr **留管道**：原实现是 `Stdio::null()`，于是"拒绝访问/进程不存在/被安全
            // 软件拦截"全都拿不到，只剩一句"未按时退出"（CQ-25）。失败要说得清原因
            .stdout(Stdio::null()).stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("启动 taskkill 失败 (pid={}): {}", pid, e))?;
        let out = child
            .wait_with_output()
            .map_err(|e| format!("等待 taskkill 失败 (pid={}): {}", pid, e))?;
        if !out.status.success() {
            // 注意：进程本来就不存在时 taskkill 也返回非零。**这不代表收尸失败**——
            // 判"有没有收干净"的是调用方对存活状态的复核（见 `CleanupOutcome`），
            // 这里的返回值只用于把原因带到错误信息里
            return Err(format!(
                "taskkill 退出码 {:?} (pid={}): {}",
                out.status.code(),
                pid,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    }
    #[cfg(unix)]
    {
        let mut first_err: Option<String> = None;
        match Command::new("kill").args(["-TERM", &format!("-{}", pid)])
            .stdout(Stdio::null()).stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => { let _ = child.wait(); }
            Err(e) => first_err = Some(format!("kill -TERM 失败 (pid={}): {}", pid, e)),
        }
        std::thread::sleep(Duration::from_millis(300));
        match Command::new("kill").args(["-KILL", &format!("-{}", pid)])
            .stdout(Stdio::null()).stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => { let _ = child.wait(); }
            Err(e) => {
                return Err(match first_err {
                    Some(prev) => format!("{}；kill -KILL 失败 (pid={}): {}", prev, pid, e),
                    None => format!("kill -KILL 失败 (pid={}): {}", pid, e),
                })
            }
        }
        if let Some(e) = first_err { log::warn!("[nexus] {}", e); }
        Ok(())
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
/// 信任边界（写清楚，避免高估白名单的覆盖范围）：**文件 API 的白名单不约束执行面**——
/// 服务命令本身就是用户写的 shell 串，`cwd` 与 `watch_paths` 已按 SEC-1 纳入范围校验，
/// 但"命令里可以写什么"不受限制（用户只能管理自己的项目，命令在用户自己的机器上以用户身份运行）。
/// 因此这条路径不做 NoDefaultCurrentDirectoryInExePath 收口（见 `build_internal_command`），
/// 也不该被当作沙箱：真正的隔离要靠 Job Object（进程随应用退出）+ 路径白名单（文件 API）。
/// Windows 上统一通过 cmd /C 执行，确保 npm/pnpm 等脚本能正确解析。
pub fn build_command(command_str: &str) -> Command {
    #[cfg(windows)]
    {
        const FLAGS: u32 = 0x08000000 | 0x00000200;
        let mut c = Command::new("cmd");
        c.args(["/C", command_str]);
        c.creation_flags(FLAGS);
        // 补上"启动之后才装的东西"（见 core::winenv）：Windows 只在进程启动时复制一份
        // 环境块，而用户可能是在 Nexus 运行期间装 nvm / 装 Node 的——不补的话，服务里的
        // `npm run dev`、`npm install -g dsh` 都会报"找不到 npm"，直到重启 Nexus
        crate::core::winenv::apply(&mut c);
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

/// 构建 Nexus **自己**发起的命令（`dsh` / `npm` 等内部 CLI）。
///
/// 与 `build_command` 的唯一差别：设置 `NoDefaultCurrentDirectoryInExePath=1`，
/// 让 Windows 在解析可执行文件名时**跳过当前目录**。
///
/// 攻击面（SEC-12）：`dsh web` 的工作目录是项目目录（AI 会话根），
/// 而 cmd 默认先搜当前目录——项目里放一个 `dsh.cmd` 就会被优先于 PATH 上的真 dsh 执行，
/// "克隆一个不受信任的仓库 + 打开 AI 面板"即可触发任意命令执行。
///
/// 为什么不给用户配置的服务命令也加：`gradlew`/`mvnw` 这类**项目内**启动脚本按裸名调用
/// 是常见且正当的用法，禁掉当前目录搜索会让它们失效（用户得改写成 `.\gradlew`）。
/// 用户命令本就是用户自己写的 shell 串，那条路径的信任边界见 `build_command`。
pub fn build_internal_command(command_str: &str) -> Command {
    let mut c = build_command(command_str);
    // 文档化开关（NT 4.0 起支持）：cmd 与 CreateProcess 不再把当前目录当搜索路径
    c.env("NoDefaultCurrentDirectoryInExePath", "1");
    c
}

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kill_process_tree_invalid_pid() {
        // 测试无效 PID 不会 panic
        // 不存在的 pid：taskkill 会返回非零，这条用例只要求不 panic（返回值这里不关心）
        let _ = kill_process_tree(999999999);
    }

    // ── prepare_log_text（无 ANSI 走快路径 / 刷新帧补前缀 / 超长截断）────
    #[test]
    fn test_prepare_log_text_paths() {
        // 无转义序列：原样返回（这条路径不拷贝、不进 clean_ansi）
        assert_eq!(prepare_log_text("hello world".to_string(), false), "hello world");
        // 有转义序列：清掉光标控制（\x1b[2K），保留文本
        assert_eq!(prepare_log_text("\x1b[2Kprogress 10%".to_string(), false), "progress 10%");
        // SGR 颜色序列保留（前端着色依赖它）
        assert_eq!(prepare_log_text("\x1b[32mOK\x1b[0m".to_string(), false), "\x1b[32mOK\x1b[0m");
        // 刷新帧统一补 \r 前缀（前端据此原地替换最后一行）
        assert_eq!(prepare_log_text("progress 20%".to_string(), true), "\rprogress 20%");
        // 刷新帧原带 \r 时不重复叠加
        assert_eq!(prepare_log_text("\rprogress 30%".to_string(), true), "\rprogress 30%");
        // 超长行被截断并带标记
        let long = "x".repeat(MAX_LINE_BYTES + 64);
        let out = prepare_log_text(long, false);
        assert!(out.ends_with("[已截断]"), "超长行应带截断标记");
        assert!(out.len() < MAX_LINE_BYTES + 64, "超长行应被截短");
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
    fn test_read_log_lines_decodes_gbk_instead_of_dropping() {
        // GBK 的行必须解出来而不是丢掉：中文 Windows 上 cmd 的报错就是 GBK，
        // 丢掉它，用户看到的是"命令失败（退出码 1）。（无输出）"——唯一的原因没了
        let mut data = Vec::new();
        data.extend_from_slice(&encoding_rs::GB18030.encode("'npm' 不是内部或外部命令").0);
        data.push(b'\n');
        data.extend_from_slice(b"ok\n");
        let lines: Vec<String> = read_log_lines(std::io::Cursor::new(data)).collect();
        assert_eq!(lines, vec!["'npm' 不是内部或外部命令".to_string(), "ok".to_string()]);
    }

    #[test]
    fn test_read_log_lines_survives_undecodable_bytes() {
        // 坏字节：关键行为是"这一行仍然读得出来、后续行不受影响"，不是字节级完全还原——
        // GB18030 是多字节编码，前导字节可能把后面一个 ASCII 字节一起吃进那个字
        // （实测 `a \xff \xfe b` 解成 "a�㧏"）。这是回退解码固有的代价，比整行丢掉强：
        // 丢掉等于把命令失败的原因吃了
        let mut data = vec![b'a', 0xff, 0xfe, b'b', b'\n'];
        data.extend_from_slice(b"after\n");
        let lines: Vec<String> = read_log_lines(std::io::Cursor::new(data)).collect();
        assert_eq!(lines.len(), 2, "坏字节所在行也要读出来: {:?}", lines);
        assert!(lines[0].starts_with('a'), "实际: {:?}", lines[0]);
        assert_eq!(lines[1], "after");
    }

    /// 真机走一遍：中文 Windows 上 cmd 的报错是 GBK，经整条读取链路必须读得出来
    /// （这条正是 "命令失败（退出码 1）。（无输出）" 的复现——旧实现把 GBK 行整条丢了）
    #[test]
    #[cfg(windows)]
    fn test_cmd_gbk_error_survives_read_log_lines() {
        let out = std::process::Command::new("cmd")
            .args(["/C", "nexus-no-such-command-xyz"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("执行 cmd");
        assert_eq!(out.status.code(), Some(1), "cmd 找不到命令时应当返回 1");
        let text: String = read_log_lines(std::io::BufReader::new(&out.stderr[..]))
            .collect::<Vec<_>>()
            .join("\n");
        println!("cmd stderr → {:?}", text);
        assert!(!text.trim().is_empty(), "cmd 的报错必须读得出来（GBK 也要解，不能整行丢掉）");
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

    // ── ProcessManager 生命周期（不依赖 AppHandle 的部分）──────────────
    //
    // 为什么这些能测：spawn 需要 AppHandle（要向上 emit 事件），但"没有进程时"的
    // 生命周期语义——停止不存在的服务、日志缓冲的序号/裁剪/清理、运行表快照——
    // 都只依赖内部状态，正是历史上出过错的地方（stale pid、快照覆盖实时流）。

    #[test]
    fn test_stop_unstarted_service_is_idempotent() {
        let mgr = ProcessManager::new();
        // 未启动过的服务：stop 不该报错（前端会在"以为在跑"时点停止，报错会让用户困惑）
        assert!(mgr.stop("never-started").is_ok());
        assert!(mgr.running().is_empty());
        assert!(mgr.get_logs("never-started").is_empty());
        assert!(mgr.failed().is_empty());
    }

    #[test]
    fn test_log_buffer_assigns_monotonic_seq_and_prefixes_platform_marks() {
        let mgr = ProcessManager::new();
        mgr.append_system_line("svc-1", "第一条".into());
        mgr.append_system_line("svc-1", "第二条".into());
        let lines = mgr.get_logs("svc-1");
        assert_eq!(lines.len(), 2);
        // 序号严格递增：前端按 seq 做幂等合并（快照与实时流交错时靠它收敛）
        assert!(lines[0].seq < lines[1].seq, "序号必须单调递增");
        // 系统行带平台标记前缀，且计入同一缓冲（与真实输出混排时不丢顺序）
        assert!(lines[0].text.contains("第一条"), "实际文本: {}", lines[0].text);
    }

    #[test]
    fn test_log_buffer_is_per_service_and_bounded() {
        let mgr = ProcessManager::new();
        for i in 0..(MAX_LOG_LINES + 50) {
            mgr.append_system_line("svc-2", format!("line-{}", i));
        }
        let lines = mgr.get_logs("svc-2");
        assert_eq!(lines.len(), MAX_LOG_LINES, "每条服务的缓冲有上限（IDEA Console 式只留最新）");
        // 保留的是**最新**的若干行：末尾必须是最后写入的那条
        assert!(lines.last().unwrap().text.contains(&format!("line-{}", MAX_LOG_LINES + 49)));
        // 不同服务互不影响（key 隔离）
        assert!(mgr.get_logs("svc-3").is_empty());
    }

    /// PERF-17 的回归：**字节**上限必须与行数上限同时生效。
    ///
    /// 形状取自审计：每行都顶到单行上限（8KB）时，行数上限挡不住膨胀——
    /// 2000 行 × 8KB = 单服务 16MB，50 个服务 = 约 800MB。
    /// 反证方式：把 `MAX_LOG_BYTES` 临时改成 `usize::MAX`，本测试应如期失败。
    #[test]
    fn test_log_buffer_is_byte_bounded_too() {
        let mgr = ProcessManager::new();
        let n = 400usize; // 400 × 8KB = 3.2MB > 2MB，必然触发裁剪
        for i in 0..n {
            mgr.append_system_line("svc-bytes", format!("{:0>width$}", i, width = MAX_LINE_BYTES));
        }
        let lines = mgr.get_logs("svc-bytes");
        let total: usize = lines.iter().map(|l| l.text.len()).sum();
        assert!(total <= MAX_LOG_BYTES, "总字节必须受上限约束，实际 {}", total);
        assert!(lines.len() < n, "应已按字节裁剪，实际仍有 {} 行", lines.len());
        assert!(lines.len() <= MAX_LOG_LINES, "行数上限仍须独立生效");
        assert!(
            lines.last().unwrap().text.ends_with(&format!("{:0>16}", n - 1)),
            "保留的必须是最新写入的那行"
        );
    }

    /// 测试用日志 sink：记录收到的批次。
    /// ARCH-14 把 `AppHandle` 换成这个闭包之后，`start()` 的全部路径才第一次可测。
    fn test_sink() -> (LogSink, Arc<Mutex<Vec<ServiceLogBatchPayload>>>) {
        let seen: Arc<Mutex<Vec<ServiceLogBatchPayload>>> = Arc::new(Mutex::new(Vec::new()));
        let s = Arc::clone(&seen);
        (Arc::new(move |p| {
            if let Ok(mut v) = s.lock() {
                v.push(p);
            }
        }), seen)
    }

    /// ARCH-14：`start()` 的"工作目录为空"分支此前零覆盖。
    #[test]
    fn test_start_rejects_empty_cwd() {
        let mgr = ProcessManager::new();
        let (sink, _seen) = test_sink();
        let err = mgr.start(ServiceSpawn {
            project_id: "p1", service_id: "s1", name: "n", command: "echo hi",
            cwd: "   ", env_vars: &[], log_sink: sink,
        }).unwrap_err();
        assert!(err.contains("工作目录为空"), "实际错误: {}", err);
        // 被拒绝的启动不得留下任何痕迹（失败表/日志缓冲都该是干净的）
        assert!(mgr.failed().is_empty(), "空 cwd 是配置错误，不该记成「服务启动失败」");
        assert!(mgr.get_logs("s1").is_empty(), "拒绝路径不该写日志缓冲");
    }

    /// ARCH-14 解锁的核心用例：**spawn 失败路径**（此前 24 个测试无一走到 start）。
    ///
    /// 用"不存在的 cwd"触发：`cmd /C` 包着的命令本身总能 spawn，
    /// 但 `current_dir` 指向不存在的目录时 `spawn()` 会直接失败。
    #[test]
    fn test_start_records_failure_when_spawn_fails() {
        let mgr = ProcessManager::new();
        let (sink, _seen) = test_sink();
        let missing = std::env::temp_dir()
            .join(format!("nexus_ut_missing_cwd_{}", std::process::id()))
            .to_string_lossy()
            .to_string();
        let err = mgr.start(ServiceSpawn {
            project_id: "p1", service_id: "s1", name: "n", command: "echo hi",
            cwd: &missing, env_vars: &[], log_sink: sink,
        }).unwrap_err();
        assert!(err.starts_with("启动失败"), "实际错误: {}", err);

        // 失败必须同时落两处：失败表（卡片显示"失败"按钮）+ 日志缓冲（点开可见原因）
        let failed = mgr.failed();
        assert_eq!(failed.len(), 1, "失败必须进失败表");
        assert_eq!(failed[0].service_id, "s1");
        assert!(failed[0].exit_code.is_none(), "spawn 失败没有退出码");
        assert!(
            mgr.get_logs("s1").iter().any(|l| l.text.contains("启动失败")),
            "日志缓冲应记下失败原因"
        );
        assert!(
            !mgr.running().iter().any(|(_, id)| id == "s1"),
            "失败的启动不得留在运行表"
        );
    }

    /// NEW-17 的回归：同一 key 的 stop 必须等 start 的生命周期锁释放。
    ///
    /// 直接构造 start/stop 的真实交错需要进程调度运气（会变成 flaky 测试），
    /// 而"锁是否真的把同 key 串行化"是确定性可测的——测它即可。
    #[test]
    fn test_lifecycle_lock_serializes_same_key() {
        let mgr = Arc::new(ProcessManager::new());
        let lock = mgr.lifecycle_lock("svc-x");
        let guard = lock_lifecycle(&lock, "svc-x");

        let done = Arc::new(AtomicBool::new(false));
        let mgr2 = Arc::clone(&mgr);
        let done2 = Arc::clone(&done);
        let t = std::thread::spawn(move || {
            let _ = mgr2.stop("svc-x"); // 表里没有该 key：无锁时立即返回
            done2.store(true, Ordering::SeqCst);
        });

        std::thread::sleep(Duration::from_millis(150));
        assert!(!done.load(Ordering::SeqCst), "同 key 的 stop 不该在锁被持有期间完成");

        drop(guard);
        t.join().expect("线程应能结束");
        assert!(done.load(Ordering::SeqCst), "放锁后 stop 应立即完成");
    }

    #[test]
    fn test_failed_table_records_and_drains_exit_status() {
        let mgr = ProcessManager::new();
        // 直接注入失败记录（真实路径由 reader 线程在进程意外退出时写入）
        mgr.record_failure_for_test("svc-x", Some(1));
        let failed = mgr.failed();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].service_id, "svc-x");
        assert_eq!(failed[0].exit_code, Some(1));
        // 主动停止过的服务不该留在失败表里（前端据此显示"失败"按钮）
        mgr.stop("svc-x").unwrap();
        assert!(mgr.failed().iter().all(|f| f.service_id != "svc-x" || f.exit_code != Some(1)));
    }

    /// 收尾判据：**缓冲里还有行时绝不能退出**。
    ///
    /// 这正是"实时输出最后一行丢失"的窗口：读取线程先 push 再递减存活计数，
    /// 若发射循环在取缓冲之后才读存活数，就可能"取到空批 → 读到归零 → 退出"，
    /// 把那一行永远留在缓冲里。`flush_step` 把顺序固定成"先读存活、再取缓冲"，
    /// 于是归零被读到的那一刻，所有 push 都已可见 —— 取到空批才是真的没有下一行了。
    #[test]
    fn test_flush_step_does_not_exit_with_pending_lines() {
        let alive = std::sync::atomic::AtomicUsize::new(0);
        let pending = Mutex::new(vec!["最后一行".to_string()]);

        let (batch, done) = flush_step(&pending, &alive);
        assert_eq!(batch, vec!["最后一行".to_string()], "存活归零但缓冲有行：必须发出去");
        assert!(!done, "还有行要发时不能收尾");

        // 下一轮：缓冲已空且存活归零 → 收尾
        let (batch, done) = flush_step(&pending, &alive);
        assert!(batch.is_empty());
        assert!(done, "缓冲空 + 读取线程全部退出 = 可以收尾");
    }

    /// 读取线程仍在跑时一律不收尾（哪怕这一轮没取到行）——否则稀疏输出会被截断
    #[test]
    fn test_flush_step_keeps_running_while_readers_alive() {
        let alive = std::sync::atomic::AtomicUsize::new(1);
        let pending: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let (batch, done) = flush_step(&pending, &alive);
        assert!(batch.is_empty());
        assert!(!done, "还有读取线程在跑，不能收尾");
    }

    // ── 游离进程认领（"另开窗口"的服务，如 Tomcat 的 startup.bat）──────────
    //
    // 这一组是**真起进程**的：认领走查的是系统进程表，只有真进程能验证
    // （铁律 19：触碰进程/平台的路径必须真跑一次）。

    /// 写一个替身脚本：模拟 `catalina.bat` 的 `:doStart` —— 真正干活的进程被 cmd 内建
    /// 命令 `start` 另开一个控制台跑掉（见 `core::winproc` 的模块说明），脚本自己随即结束。
    /// `hold_secs > 0` 时脚本额外存活一段时间（用于测"父进程还活着时的走查"）。
    ///
    /// 为什么用 .bat 而不是直接把 `start "标题" "…"` 当命令串传：
    /// 服务命令是经 `cmd /C <命令串>` 执行的，带引号的串要穿过 Rust 的 argv 转义再被
    /// cmd 解析一遍，两边规则不同（实测这种形态下 `start` 根本跑不起来）。
    /// 真实场景（Tomcat/ActiveMQ）也都是 .bat 文件，这样写同时更贴近实际。
    /// 内容**只用 ASCII**：中文在 GBK 代码页下会被解成乱码（见候选区那条）。
    #[cfg(windows)]
    fn write_detach_bat(tag: &str, hold_secs: u32) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("nexus_test_{}_{}.bat", tag, std::process::id()));
        let mut body = String::from("@echo off\r\n");
        body.push_str("start \"NexusTestOrphan\" \"%COMSPEC%\" /c \"ping -n 20 127.0.0.1 >nul\"\r\n");
        if hold_secs > 0 {
            body.push_str(&format!("ping -n {} 127.0.0.1 >nul\r\n", hold_secs));
        }
        std::fs::write(&path, body).expect("应当能写替身脚本");
        path
    }

    /// 轮询到认领发生（真实路径由 `running()` 驱动），返回认领到的 (pid, 映像名)
    #[cfg(windows)]
    fn wait_for_adoption(mgr: &ProcessManager, key: &str) -> Vec<(u32, String)> {
        for _ in 0..100 {
            let _ = mgr.running();
            let pids = mgr.adopted_pids_for_test(key);
            if !pids.is_empty() {
                return pids;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Vec::new()
    }

    /// 等服务被判定为失败（真实路径：下一轮 `running()`）
    #[cfg(windows)]
    fn wait_for_failure(mgr: &ProcessManager) -> Vec<FailedService> {
        for _ in 0..100 {
            let _ = mgr.running();
            let failed = mgr.failed();
            if !failed.is_empty() {
                return failed;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Vec::new()
    }

    /// 等 pid 真的不再运行。
    ///
    /// 判据用**句柄**（`GetExitCodeProcess`）而不是 `process_start_time_millis`：
    /// 后者是"身份未变"的判据，进程被结束后只要还有句柄（例如控制台宿主）没关，
    /// `OpenProcess` 照样成功——实测就是这么被骗的：taskkill 已经报"没有找到进程"，
    /// 而按 pid 查启动时间仍能查到一个值。
    #[cfg(windows)]
    fn wait_gone(pid: u32) -> bool {
        for _ in 0..40 {
            let alive = super::super::winproc::OwnedProcess::open(pid)
                .map(|h| h.is_alive())
                .unwrap_or(false);
            if !alive {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// 认领 + 状态 + 停止：一条"另开窗口"的服务走完整生命周期
    ///
    /// 这是本功能的核心判据——修的就是三件事：
    /// 1) 直接子进程退出不等于服务退出（不谎报"已退出（退出码 0）"）
    /// 2) 日志里必须写明接管了谁（否则用户以为服务没了）
    /// 3) 「停止」要能真的把这个跑掉的进程结束掉
    #[cfg(windows)]
    #[test]
    fn test_adopts_detached_child_and_stop_kills_it() {
        let mgr = ProcessManager::new();
        let (sink, _seen) = test_sink();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let script = write_detach_bat("detach", 0);
        let command = script.to_string_lossy().to_string();
        mgr.start(ServiceSpawn {
            project_id: "p1", service_id: "detach-1", name: "n",
            command: &command, cwd: &cwd, env_vars: &[], log_sink: sink,
        }).expect("启动应当成功");

        let pids = wait_for_adoption(&mgr, "detach-1");
        assert!(!pids.is_empty(), "应当认领到另开窗口跑起来的进程（替身命令没生效？）");
        // 回归：控制台宿主不算服务在跑的东西（实测混进来过，还会让"杀没杀死"的
        // 两种判据打架，见 is_console_host 的说明）
        assert!(
            pids.iter().all(|(_, name)| !is_console_host(name)),
            "认领名单里不该有控制台宿主：{:?}", pids
        );

        let running: Vec<String> = mgr.running().into_iter().map(|(_, id)| id).collect();
        assert!(running.contains(&"detach-1".to_string()), "认领后应仍算运行中，实际 {:?}", running);
        assert!(mgr.failed().is_empty(), "认领成功不该记成失败");
        assert!(
            mgr.get_logs("detach-1").iter().any(|l| l.text.contains("另开窗口启动的进程仍在运行")),
            "认领必须写一行说明进日志，实际: {:?}",
            mgr.get_logs("detach-1").iter().map(|l| l.text.clone()).collect::<Vec<_>>()
        );

        // 停止：认领到的进程必须一起结束（否则就是"点了停止，Tomcat 还在跑"）
        mgr.stop("detach-1").expect("停止应当成功");
        for (pid, name) in &pids {
            assert!(wait_gone(*pid), "停止后 pid {} ({}) 仍在运行——认领的游离进程没被结束", pid, name);
        }
        assert!(!mgr.running().iter().any(|(_, id)| id == "detach-1"), "停止后不该还在运行列表");
    }

    /// 回归：没有游离后代的秒退服务照旧判失败（认领不能把它"救活"）
    ///
    /// 系统里此刻有大量无关进程（测试进程自己、cmd、各种系统服务），
    /// 父链走查必须一个都不认——这条同时钉住了"认领不会误伤别人的进程"。
    #[cfg(windows)]
    #[test]
    fn test_quick_exit_without_descendants_still_fails() {
        let mgr = ProcessManager::new();
        let (sink, _seen) = test_sink();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        mgr.start(ServiceSpawn {
            project_id: "p1", service_id: "quick-1", name: "n",
            command: "exit /b 3", cwd: &cwd, env_vars: &[], log_sink: sink,
        }).expect("启动应当成功");

        let failed = wait_for_failure(&mgr);
        assert_eq!(failed.len(), 1, "秒退服务应记为失败，实际 {:?}", failed.iter().map(|f| &f.service_id).collect::<Vec<_>>());
        assert_eq!(failed[0].exit_code, Some(3), "退出码应原样带出");
        assert!(mgr.adopted_pids_for_test("quick-1").is_empty(), "没有游离后代时不该认领任何东西");
    }

    /// 启动时刻校验：候选的创建时刻早于我们那条子进程时，一律不认领（防 pid 复用）
    ///
    /// 直接测走查函数而不是经 `running()`：这里要的是"同一个候选，只因为时间下限不同，
    /// 一个被认领、一个被挡掉"这一对照，走管理器拿不到这么干净的对照组。
    #[cfg(windows)]
    #[test]
    fn test_adoption_rejects_candidates_created_before_child() {
        // 父进程自己活 4 秒（脚本末尾那条 ping），期间 `start` 出来的进程是它的游离后代
        let script = write_detach_bat("guard", 4);
        let mut parent = std::process::Command::new("cmd")
            .args(["/C", &script.to_string_lossy()])
            .stdout(Stdio::null()).stderr(Stdio::null())
            .spawn().expect("应当能起替身父进程");
        let parent_pid = parent.id();

        // 下限 0 = 不做时间校验：应当找到游离后代
        let mut found = Vec::new();
        for _ in 0..40 {
            found = adopt_survivors(parent_pid, 0);
            if !found.is_empty() { break; }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(!found.is_empty(), "下限为 0 时应当认出游离后代");

        // 下限拉满 = 要求候选比 u64::MAX 还晚创建：不存在，必须一个都不认
        let rejected = adopt_survivors(parent_pid, u64::MAX);
        assert!(rejected.is_empty(), "时间校验没挡住：{:?}", rejected.iter().map(|a| a.pid).collect::<Vec<_>>());

        // 收尾：认领到的句柄随 drop 关闭，进程按 pid 结束
        let pids: Vec<u32> = found.iter().map(|a| a.pid).collect();
        drop(found);
        for pid in pids { let _ = kill_process_tree(pid); }
        let _ = parent.kill();
        let _ = parent.wait();
    }

    // ── 日志文件跟随 ────────────────────────────────────────────
    //
    // 这些用例真起服务、真写文件、真跑跟随线程：跟文件这件事的坑全在
    // "文件被改名/被清空/半行/停止后还在写"这类时序上，只有真跑才碰得到。

    /// "活着但不干活"的服务命令：跟随要求服务在运行表里
    #[cfg(windows)]
    const HOLD_CMD: &str = "ping -n 30 127.0.0.1 >nul";
    #[cfg(not(windows))]
    const HOLD_CMD: &str = "sleep 30";

    /// 建一个隔离的临时日志路径（目录名带 pid，避免并行测试互相踩）
    fn temp_log(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nexus_tail_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("应当能建临时目录");
        dir.join("app.log")
    }

    /// 往文件尾追加内容（跟随线程只认追加）
    fn append(path: &Path, text: &str) {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new().append(true).create(true).open(path).expect("应当能打开日志文件");
        f.write_all(text.as_bytes()).expect("应当能写日志文件");
    }

    /// 等日志里出现某段文本（跟随是异步的，必须轮询）
    fn wait_for_text(mgr: &ProcessManager, key: &str, needle: &str) -> bool {
        for _ in 0..60 {
            if mgr.get_logs(key).iter().any(|l| l.text.contains(needle)) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    /// 起一个用于跟随测试的服务，返回 (管理器, 日志文件路径)。
    /// 日志文件会先建好（空白）：跟随要求文件存在——文件打不开时当场报错，
    /// 而不是静默等到线程里失败（前端要能说出为什么）
    fn start_hold_service(key: &str, tag: &str) -> (ProcessManager, PathBuf) {
        let mgr = ProcessManager::new();
        let (sink, _seen) = test_sink();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        mgr.start(ServiceSpawn {
            project_id: "p1", service_id: key, name: "n",
            command: HOLD_CMD, cwd: &cwd, env_vars: &[], log_sink: sink,
        }).expect("启动应当成功");
        let log = temp_log(tag);
        std::fs::write(&log, "").expect("建空日志文件");
        (mgr, log)
    }

    /// 跟随从"末尾回看一段"开始：窗口内的最近内容看得到、更早的历史不灌进面板，
    /// 之后追加的行按顺序到达
    #[test]
    fn test_follow_seeds_recent_history_in_order() {
        let (mgr, log) = start_hold_service("follow-1", "newlines");
        // 造一段明显超过回看窗口（64KB）的历史
        let mut history = String::new();
        for i in 0..8000 {
            history.push_str(&format!("早先的行-{}
", i));
        }
        std::fs::write(&log, &history).expect("写历史内容");
        append(&log, "窗口内的最近一行
");
        mgr.follow_log("follow-1", &log.to_string_lossy()).expect("跟随应当成功");

        assert!(wait_for_text(&mgr, "follow-1", "窗口内的最近一行"), "回看窗口内的内容应当出现");
        assert!(
            !mgr.get_logs("follow-1").iter().any(|l| l.text == "早先的行-0"),
            "超出回看窗口的历史不该灌进面板"
        );

        append(&log, "第一行
第二行
");
        assert!(wait_for_text(&mgr, "follow-1", "第一行"), "追加的行应当进日志");
        assert!(wait_for_text(&mgr, "follow-1", "第二行"));
        let texts: Vec<String> = mgr.get_logs("follow-1").iter().map(|l| l.text.clone()).collect();
        let i1 = texts.iter().position(|t| t.contains("第一行")).expect("应有第一行");
        let i2 = texts.iter().position(|t| t.contains("第二行")).expect("应有第二行");
        assert!(i1 < i2, "行序应与文件里一致，实际 {:?}", texts);
        assert!(mgr.followed_log("follow-1").is_some(), "followed_log 应报告正在跟随");
        mgr.stop("follow-1").expect("停止应当成功");
    }

    /// 没有换行符的半行不能提前输出（否则停半秒的内容会被拆成两条，
    /// 而真正的一行到达时又会和前半段错位）
    #[test]
    fn test_follow_holds_partial_line_until_newline() {
        let (mgr, log) = start_hold_service("follow-2", "partial");
        mgr.follow_log("follow-2", &log.to_string_lossy()).expect("跟随应当成功");

        append(&log, "未完的一行");
        std::thread::sleep(Duration::from_millis(700));
        assert!(
            !mgr.get_logs("follow-2").iter().any(|l| l.text.contains("未完的一行")),
            "没有换行符时不该输出半行"
        );

        append(&log, " 补完\n");
        assert!(wait_for_text(&mgr, "follow-2", "未完的一行 补完"), "换行到达后应当输出完整一行");
        mgr.stop("follow-2").expect("停止应当成功");
    }

    /// 轮转（改名 + 新建，ActiveMQ 的 RollingFileAppender 就是这个形状）后必须跟到新文件：
    /// 手里的句柄会一直指向被改名的旧文件，只有比大小才能发现换了文件
    #[test]
    fn test_follow_switches_to_rotated_file() {
        let (mgr, log) = start_hold_service("follow-3", "rotate");
        mgr.follow_log("follow-3", &log.to_string_lossy()).expect("跟随应当成功");

        append(&log, "轮转前\n");
        assert!(wait_for_text(&mgr, "follow-3", "轮转前"));
        std::fs::rename(&log, log.with_file_name("app.log.1")).expect("改名应当成功");
        std::fs::write(&log, "轮转后\n").expect("新建日志文件");

        assert!(wait_for_text(&mgr, "follow-3", "轮转后"), "轮转后应当跟到新文件");
        assert!(wait_for_text(&mgr, "follow-3", "已轮转"), "轮转应当在日志里说明（用户要知道读到哪去了）");
        mgr.stop("follow-3").expect("停止应当成功");
    }

    /// 取消跟随：服务照常运行，之后写进文件的内容不再进面板
    #[test]
    fn test_unfollow_stops_writing_but_service_keeps_running() {
        let (mgr, log) = start_hold_service("follow-4", "unfollow");
        mgr.follow_log("follow-4", &log.to_string_lossy()).expect("跟随应当成功");
        append(&log, "跟随中\n");
        assert!(wait_for_text(&mgr, "follow-4", "跟随中"));

        assert_eq!(mgr.unfollow_log("follow-4").as_deref(), Some(log.to_string_lossy().as_ref()));
        assert!(mgr.followed_log("follow-4").is_none(), "取消后不该再报告在跟随");
        let before = mgr.get_logs("follow-4").len();
        append(&log, "取消后\n");
        std::thread::sleep(Duration::from_millis(800));
        assert_eq!(mgr.get_logs("follow-4").len(), before, "取消跟随后文件内容不该再进日志");
        assert!(
            mgr.running().iter().any(|(_, id)| id == "follow-4"),
            "取消跟随不该影响服务本身"
        );
        mgr.stop("follow-4").expect("停止应当成功");
    }

    /// 停止服务必须让跟随线程一起退出。
    ///
    /// 这是"内存留在那儿"那条回归：`stop_locked` 会 `remove` 掉日志缓冲，
    /// 跟随线程若还在跑，下一行就会把这个 key 的缓冲**重新建出来**——那块内存
    /// 直到同名服务再次启动才会被替换，中间一直挂在表里。
    #[test]
    fn test_stop_stops_follow_and_does_not_recreate_buffer() {
        let (mgr, log) = start_hold_service("follow-5", "stopfollow");
        mgr.follow_log("follow-5", &log.to_string_lossy()).expect("跟随应当成功");
        append(&log, "停止前\n");
        assert!(wait_for_text(&mgr, "follow-5", "停止前"));

        mgr.stop("follow-5").expect("停止应当成功");
        append(&log, "停止后\n");
        std::thread::sleep(Duration::from_millis(900));
        assert!(
            mgr.get_logs("follow-5").is_empty(),
            "停止后跟随线程仍在写：日志缓冲被重新建出来了，实际 {:?}",
            mgr.get_logs("follow-5").iter().map(|l| l.text.clone()).collect::<Vec<_>>()
        );
    }

    /// 端到端（C + B 串起来）：服务另开窗口跑 → 认领 → 自动发现日志文件 → 跟上面板
    ///
    /// 这是用户真实场景的完整链路（Tomcat 的 startup.bat 就是这个形状）：
    /// 命令立刻结束、真正的进程在别处、面板本该什么都看不到。
    #[cfg(windows)]
    #[test]
    fn test_detached_service_auto_follows_its_log_file() {
        let root = std::env::temp_dir().join(format!("nexus_detach_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("建服务目录");
        let script = write_detach_bat("autofollow", 0);

        let mgr = ProcessManager::new();
        let (sink, _seen) = test_sink();
        let cwd = root.to_string_lossy().to_string();
        let command = script.to_string_lossy().to_string();
        mgr.start(ServiceSpawn {
            project_id: "p1", service_id: "detach-log", name: "n",
            command: &command, cwd: &cwd, env_vars: &[], log_sink: sink,
        }).expect("启动应当成功");

        assert!(!wait_for_adoption(&mgr, "detach-log").is_empty(), "应当认领到游离进程");
        // 服务启动后才落盘的日志（真实的 Tomcat 就是这样：起来就写 catalina.<日期>.log）
        let log = root.join("logs").join("catalina.2026-09-19.log");
        std::fs::create_dir_all(log.parent().unwrap()).expect("建 logs 目录");
        std::fs::write(&log, "启动完成\n").expect("写日志文件");

        // 轮询驱动自动发现（真实路径就是 get_running 的轮询）
        let mut followed = None;
        for _ in 0..40 {
            let _ = mgr.running();
            followed = mgr.followed_log("detach-log");
            if followed.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(followed.as_deref(), Some(log.to_string_lossy().as_ref()), "应当跟到刚写的那份日志");
        assert!(wait_for_text(&mgr, "detach-log", "已跟随日志文件"), "必须写明跟的是哪个文件");

        append(&log, "面板应当看到这一行\n");
        assert!(
            wait_for_text(&mgr, "detach-log", "面板应当看到这一行"),
            "跟随的内容应当进面板；文件大小={:?}，服务日志={:?}",
            std::fs::metadata(&log).map(|m| m.len()),
            mgr.get_logs("detach-log").iter().map(|l| l.text.clone()).collect::<Vec<_>>()
        );

        mgr.stop("detach-log").expect("停止应当成功");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 自动跟随要选"在增长"的那个文件，而不是"最新被碰过"的那个
    ///
    /// 回归（用户实测报过"面板空白"）：Tomcat 启动时一次性创建 catalina/localhost/
    /// host-manager 几个日志文件，按"修改时间最新"选中的是只写两行就再也不动的
    /// localhost.log —— 而跟随从文件末尾读起，于是面板永远是空的。
    #[cfg(windows)]
    #[test]
    fn test_auto_follow_picks_the_file_that_grows() {
        let root = std::env::temp_dir().join(format!("nexus_growing_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("logs")).expect("建 logs 目录");
        let script = write_detach_bat("growing", 0);

        let mgr = ProcessManager::new();
        let (sink, _seen) = test_sink();
        let cwd = root.to_string_lossy().to_string();
        let command = script.to_string_lossy().to_string();
        mgr.start(ServiceSpawn {
            project_id: "p1", service_id: "growing-1", name: "n",
            command: &command, cwd: &cwd, env_vars: &[], log_sink: sink,
        }).expect("启动应当成功");
        assert!(!wait_for_adoption(&mgr, "growing-1").is_empty(), "应当认领到游离进程");

        // 次要日志：刚被创建（mtime 最新）但之后不再写 —— 正是"最新"判据会选中的那个
        let main = root.join("logs").join("catalina.2026-09-19.log");
        let decoy = root.join("logs").join("localhost.2026-09-19.log");
        std::fs::write(&main, "启动完成
").expect("写主日志");
        std::fs::write(&decoy, "").expect("写次要日志");

        let mut followed = None;
        for i in 0..60 {
            // 主日志持续在写（真实服务就是这样）
            append(&main, &format!("主日志第 {} 行
", i));
            let _ = mgr.running();
            followed = mgr.followed_log("growing-1");
            if followed.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let followed = followed.expect("应当自动发现并跟随日志文件");
        assert!(
            followed.ends_with("catalina.2026-09-19.log"),
            "应当跟随在增长的主日志，实际跟了 {}",
            followed
        );
        assert!(wait_for_text(&mgr, "growing-1", "主日志第"), "跟随的内容应当进面板");

        mgr.stop("growing-1").expect("停止应当成功");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 端到端：真实发射线程 + 真实"读取线程"，最后一批必须到达且线程自行退出。
    ///
    /// 用 `alive` 从 1 → 0 的完整过程（而不是预置 0）覆盖"reader 推完最后一行后归零"这条
    /// 主路径——`PERF-11` 的真机验证欠账里，"末批不丢"是其中一条，这里把它变成可复跑的判据。
    #[test]
    fn test_flush_thread_delivers_final_batch_and_exits() {
        let pending: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let alive = Arc::new(std::sync::atomic::AtomicUsize::new(1));
        let got: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let got_sink = Arc::clone(&got);
        let handle = spawn_log_flush_thread(
            "test-flush".to_string(),
            Arc::clone(&pending),
            Arc::clone(&alive),
            move |batch| got_sink.lock().unwrap().extend(batch),
        )
        .expect("发射线程应能创建");

        // 模拟读取线程：推入最后一行后立刻归零
        {
            let pending = Arc::clone(&pending);
            let alive = Arc::clone(&alive);
            std::thread::spawn(move || {
                pending.lock().unwrap().push("line-1".to_string());
                pending.lock().unwrap().push("line-2".to_string());
                alive.fetch_sub(1, Ordering::SeqCst);
            });
        }

        handle.join().expect("读取线程归零后发射线程应自行退出");
        assert_eq!(
            *got.lock().unwrap(),
            vec!["line-1".to_string(), "line-2".to_string()],
            "最后一批必须完整送达（顺序即读取顺序）"
        );
    }
}


