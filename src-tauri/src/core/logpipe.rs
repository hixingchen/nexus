//! 服务日志的缓冲与批量发射（ARCH-23：原样从 `core::process` 拆出）。
//!
//! 三件事：① 每服务一份有界日志缓冲（行数 + 字节双上限）；② stdout/stderr 的读取线程
//! 把行写进缓冲并推进待发行队列；③ 批量发射线程按窗口把队列整批交给 `LogSink`，
//! 并用 `LogSignal` 唤醒（空闲时不空转）。文本层面的解码/裁剪见 `logtext`。

use std::collections::{HashMap, VecDeque};
use std::io::BufReader;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use serde::{Deserialize, Serialize};

use super::logtext::{prepare_log_text, read_log_lines};

/// 日志缓冲区 key 类型，使用 Arc<str> 避免热路径 String clone
pub(crate) type LogKey = Arc<str>;

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
pub(crate) const MAX_LOG_LINES: usize = 2000;
/// 每服务日志缓冲**字节**上限（与前端 `stores/logStore.ts` 的 `MAX_BYTES` 同值同口径）。
/// 行数上限挡不住"2000 行 × 8KB 长行 = 16MB/服务"这种形状，两条上限量纲不同、都必须有。
pub(crate) const MAX_LOG_BYTES: usize = 2 * 1024 * 1024;

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

/// 一批最多发多少字节（PERF-24）。
///
/// 批量化（`LOG_FLUSH_MS`）压住的是**事件次数**（≤20/s/服务），压不住**单个事件的字节数**：
/// 50ms 窗口里 Maven/Node 构建能打出几万行，整批发出去就是一个几百 KB 的 IPC 载荷——Rust 侧
/// 序列化成 JS 串、渲染进程再 JSON.parse 一遍，全都堆在一次事件里（卡的是那一帧）。
/// 按字节切批只改变"拆成几次发"，总字节数与顺序都不变。
const MAX_LOG_BATCH_BYTES: usize = 64 * 1024;

/// 切批需要知道"这一条占多少字节"。只给批量发射的两种载荷实现（服务日志行 / 工具命令输出行）。
pub(crate) trait LogChunkItem {
    fn byte_len(&self) -> usize;
}

impl LogChunkItem for String {
    fn byte_len(&self) -> usize { self.len() }
}

impl LogChunkItem for Arc<LogLine> {
    fn byte_len(&self) -> usize { self.text.len() }
}

impl LogChunkItem for crate::commands::process::ToolCommandLogLine {
    fn byte_len(&self) -> usize { self.data.len() }
}

/// 日志发射的**唤醒通道**（PERF-25）。
///
/// 为什么需要：每个服务的发射线程原先无条件 `sleep(50ms)` 再取缓冲——18 个服务就是
/// 360 次唤醒 + 360 次加锁/秒，无论有没有日志。改成"生产者在 push 之后叫一声、发射线程
/// 睡着等"之后，空闲服务**一次都不醒**（实测口径：唤醒次数从 20/s 降到 ~0）。
///
/// 为什么还留一个兜底超时：`notify` 只是"叫一声"，万一哪条路径漏叫（将来有人加了个新的
/// 生产者忘了 notify），超时是唯一能让日志**最终**发出去的东西——丢一次唤醒的代价被限制为
/// "这一批最多晚 `LOG_FLUSH_IDLE_MS`"，而不是"日志永远停在缓冲里"。
///
/// 锁序：生产者在**释放 pending 锁之后**才 notify（见 `spawn_log_reader`），发射线程则是
/// "持 signal 锁 → 取 pending"——两个方向不会同时持有两把锁，因此不成环。
pub(crate) struct LogSignal {
    m: Mutex<()>,
    cv: std::sync::Condvar,
}

impl LogSignal {
    pub(crate) fn new() -> Self {
        Self { m: Mutex::new(()), cv: std::sync::Condvar::new() }
    }

    /// 有新内容了（或读线程退出了）：叫醒发射线程
    pub(crate) fn notify(&self) {
        let _g = self.m.lock().unwrap_or_else(|e| e.into_inner());
        self.cv.notify_all();
    }

    /// 睡着等下一次唤醒；超时兜底见结构体说明
    fn wait_timeout(&self, d: Duration) {
        let g = self.m.lock().unwrap_or_else(|e| e.into_inner());
        let _ = self.cv.wait_timeout(g, d).unwrap_or_else(|e| e.into_inner());
    }
}

/// 兜底超时：只在"唤醒丢了"时才会走到，正常路径由 notify 立即叫醒
const LOG_FLUSH_IDLE_MS: u64 = 1_000;

/// 批量发射线程（服务日志与工具命令输出共用）：
/// 每 `LOG_FLUSH_MS` 把待发行缓冲整批交给 `emit`，读取线程全部退出且缓冲已空时收尾。
///
/// 为什么必须独立于读取线程：读取线程会阻塞在 `lines()` 上（命令打印一行后静默 10 秒
/// 很常见），由它在读取线程侧"攒够再发"会让那一行直到下一行到来才显示。
pub(crate) fn spawn_log_flush_thread<T, F>(
    name: String,
    pending: Arc<Mutex<Vec<T>>>,
    readers_alive: Arc<std::sync::atomic::AtomicUsize>,
    signal: Arc<LogSignal>,
    mut emit: F,
) -> std::io::Result<std::thread::JoinHandle<()>>
where
    T: Send + LogChunkItem + 'static,
    F: FnMut(Vec<T>) + Send + 'static,
{
    std::thread::Builder::new().name(name).spawn(move || loop {
        // 先睡：等生产者叫醒（或兜底超时到）再取缓冲。
        // 顺序不能反——先取缓冲再睡的话，取完到睡着之间到达的行要等满一个超时才会被看到
        signal.wait_timeout(Duration::from_millis(LOG_FLUSH_IDLE_MS));
        let (batch, done) = flush_step(&pending, &readers_alive);
        if !batch.is_empty() {
            // 按字节切批（PERF-24）：切的是"怎么发"，不是"发多少"——总字节与顺序不变，
            // 单次事件的序列化/解析峰值被压到 64KB 级
            let mut chunk: Vec<T> = Vec::new();
            let mut bytes = 0usize;
            for item in batch {
                let size = item.byte_len();
                if !chunk.is_empty() && bytes + size > MAX_LOG_BATCH_BYTES {
                    emit(std::mem::take(&mut chunk));
                    bytes = 0;
                }
                bytes += size;
                chunk.push(item);
            }
            if !chunk.is_empty() {
                emit(chunk);
                // 合帧窗口：一次发射之后冷却 `LOG_FLUSH_MS` 再取下一批。**不能省**——
                // 只有 notify 的话，高频生产者（构建刷屏）会让"每一个 push 一次事件"，
                // 而这条路径上真值钱的是**事件次数**（≤20/s/服务）。冷却期间到达的行留在
                // 缓冲里，由下一次唤醒一起发走，界面侧延迟仍在一帧量级
                std::thread::sleep(Duration::from_millis(LOG_FLUSH_MS));
            }
        }
        if done {
            break;
        }
    })
}

/// 日志缓冲：`HashMap<服务 key, VecDeque<行>>`。
///
/// 元素是 `Arc<LogLine>` 而不是 `LogLine`：`get_service_logs` 要在**全局日志锁内**做快照，
/// 直接克隆结构体等于在锁内做 2000 次字符串深拷贝（单行最长 8KB → 最坏十几 MB），
/// 期间所有服务的 reader 线程都阻塞在同一把锁上。改成 Arc 后锁内只拷指针，
/// 字符串拷贝推迟到锁外（序列化时自然发生）。
pub(crate) type LogBuffers = Arc<Mutex<HashMap<LogKey, LogBuffer>>>;

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
pub(crate) struct LogBuffer {
    /// `pub(crate)`：`ProcessManager::get_logs` 在锁内只拷 `Arc` 指针取快照（见上面的说明）
    pub(crate) lines: VecDeque<Arc<LogLine>>,
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

/// 日志行序号源：全局单调，保证"同一服务内递增"（跨服务不要求可比）
static LOG_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// 写入某服务的日志缓冲：刷新帧原地替换队尾，普通行追加并按行数上限滑窗。
///
/// 刷新帧只在**队尾同属一条流**时才合并：stdout 的进度条不得覆盖 stderr 队尾的错误行。
/// 返回写进缓冲的那一行（含 seq），调用方据此发实时事件——事件与快照因此可以按 seq 对齐。
pub(crate) fn push_log_line(buffers: &LogBuffers, key: &LogKey, stream: &str, text: String, timestamp: String, is_refresh: bool) -> Arc<LogLine> {
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
pub(crate) fn spawn_log_reader(
    pipe: impl std::io::Read + Send + 'static,
    stream: &'static str,
    key: LogKey,
    log_buffers: LogBuffers,
    pending: Arc<Mutex<Vec<Arc<LogLine>>>>,
    readers_alive: Arc<std::sync::atomic::AtomicUsize>,
    signal: Arc<LogSignal>,
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
                {
                    if let Ok(mut p) = pending.lock() {
                        p.push(logged);
                    }
                    // push 之后、**释放 pending 锁之后**再叫醒（锁序见 LogSignal 说明）
                    signal.notify();
                }
            }
            // 退出前递减存活计数：发射线程据此收尾（含最后一批）。
            // 这里也要叫一声：否则发射线程要等兜底超时才发现"不会再有新行了"
            readers_alive.fetch_sub(1, Ordering::SeqCst);
            signal.notify();
            drop(done_tx);
        })
}

/// 向日志缓冲追加一条系统标记行（stream="system"）。
///
/// 语义：进程生命周期事件（启动失败/退出码/停止）写入日志流，用户可从日志面板查因。
/// 缓冲为空时自动创建；锁失败或超限时静默处理（系统行丢失不影响主流程）。
pub(crate) fn append_system_line(buffers: &LogBuffers, key: &str, text: String) {
    if let Ok(mut b) = buffers.lock() {
        let e = b.entry(Arc::from(key)).or_default();
        e.push(Arc::new(LogLine { seq: LOG_SEQ.fetch_add(1, Ordering::Relaxed), timestamp: chrono::Utc::now().to_rfc3339(), stream: "system".into(), text }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PERF-25：push 之后靠 `notify` **立刻**发出，不等兜底超时。
    ///
    /// 兜底超时是 1 秒，所以"200ms 内送达"这条断言只有在 notify 真的生效时才成立——
    /// 去掉 `signal.notify()` 它必然失败（这条用例就是为此写的）。
    #[test]
    fn test_flush_thread_wakes_on_notify_without_waiting_for_timeout() {
        let pending: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let alive = Arc::new(std::sync::atomic::AtomicUsize::new(1));
        let signal = Arc::new(LogSignal::new());
        let got: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let got_sink = Arc::clone(&got);
        let _handle = spawn_log_flush_thread(
            "test-notify".to_string(),
            Arc::clone(&pending),
            Arc::clone(&alive),
            Arc::clone(&signal),
            move |batch| got_sink.lock().unwrap().extend(batch),
        )
        .expect("发射线程应能创建");

        // 等它起来并进入等待（线程一起来就先睡）
        std::thread::sleep(Duration::from_millis(50));
        pending.lock().unwrap().push("notify-1".to_string());
        signal.notify();

        let deadline = std::time::Instant::now() + Duration::from_millis(200);
        while got.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            got.lock().unwrap().as_slice(),
            ["notify-1".to_string()],
            "push + notify 之后应在 200ms 内送达（兜底超时是 1s）"
        );
        alive.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        signal.notify();
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
            Arc::new(LogSignal::new()),
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
