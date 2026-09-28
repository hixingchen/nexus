//! 日志文件跟随（ARCH-23：原样从 `core::process` 拆出）。
//!
//! 服务把真正的进程另开窗口跑时（见 `AdoptedChild`）stdout 指望不上，跟它写的日志文件
//! 是唯一的内容来源。这里装着跟随线程、轮转/清空/落后的识别，以及"认领后自动发现该跟
//! 哪个文件"的两阶段采样。写入缓冲与批量发射见 `logpipe`，文本规整见 `logtext`。

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use super::logfile;
use super::logpipe::{
    push_log_line, spawn_log_flush_thread, LogBuffers, LogKey, LogLine, LogSignal,
    ServiceLogBatchPayload,
};
use super::logtext::{decode_lossy, prepare_log_text, MAX_READ_LINE_BYTES};
use super::process::{lock_lifecycle, ProcessManager};

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
pub(crate) fn system_time_from_millis(ms: u64) -> SystemTime {
    let now = SystemTime::now();
    let now_ms = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(ms);
    now.checked_sub(Duration::from_millis(now_ms.saturating_sub(ms))).unwrap_or(now)
}

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
pub(crate) struct LogProbe {
    /// 首次采样：候选与当时的字节数
    first: Vec<logfile::LogCandidate>,
    /// 下次最早可采样的时刻（PERF-22 的退避，单调钟）
    next_at: std::time::Instant,
    /// 当前采样间隔（ms）：每轮没结果就翻倍，封顶 `PROBE_LOG_MAX_INTERVAL_MS`
    interval_ms: u64,
}

/// 找不到日志文件时的采样间隔上限（ms）：翻倍退避到这里就不再拉长，继续等下去
/// （服务可能十几秒后才落盘，放弃等待会让面板永远空白）
const PROBE_LOG_MAX_INTERVAL_MS: u64 = 60_000;
/// 首次采样后的初始等待：与前端的 3 秒轮询同量级，保证"确实在写"的文件一轮就能认出来
const PROBE_LOG_FIRST_INTERVAL_MS: u64 = 3_000;

/// 正在跟随的日志文件
///
/// 为什么要有它：`cmd /C startup.bat` 这类服务把真正的进程另开窗口跑（见 `AdoptedChild`），
/// stdout 不进我们的管道——那种情况下日志文件是**唯一**的内容来源。
pub(crate) struct LogFollow {
    /// 被跟随的文件路径（写进日志、供前端显示"在跟哪个文件"）
    pub(crate) path: String,
    /// 本跟随自己的停止标志。与服务的 `stop_flag` 是两个：取消跟随不该停服务，
    /// 停服务要连跟随一起停（见 `spawn_log_tailer` 里的两个判断）
    pub(crate) stop: Arc<AtomicBool>,
    /// 跟随线程结束信号：停止时用它限时等待（同 reader 线程的 done_*）
    pub(crate) done: Option<Receiver<()>>,
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
    signal: Arc<LogSignal>,
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
                {
                    if let Ok(mut p) = pending.lock() {
                        p.push(line);
                    }
                    // 与 stdout/stderr reader 同口径：push 之后（锁已释放）叫醒发射线程
                    signal.notify();
                }
            };
            let stopped = || follow_stop.load(Ordering::Relaxed) || service_stop.load(Ordering::Relaxed);
            // 读缓冲在轮询循环外分配一次（PERF-23）：原先每轮新建 64KB 零缓冲，
            // 每 250ms 一次 = 每个跟随中的服务每秒白白清零 256KB
            let mut read_buf = vec![0u8; 64 * 1024];
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
                // 2) 读到 EOF（文件不长时一次就完）。
                // 读缓冲提到轮询循环外面（PERF-23）：原先每轮新建一个 64KB 的零缓冲，
                // 每 250ms 一次 = 每个跟随中的服务每秒白白清零 256KB
                loop {
                    let n = match file.read(&mut read_buf) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => {
                            log::warn!("[nexus] 读取日志文件失败 {}: {}", path.display(), e);
                            break;
                        }
                    };
                    offset += n as u64;
                    partial.extend_from_slice(&read_buf[..n]);
                    // 3) 切出完整行（残行留在 partial 里等下一轮）。
                    //
                    // 用游标扫一遍、末尾一次性 drain（PERF-23）：原实现每行 `partial.drain(..=pos)`
                    // 从队首删除，是尾部 memmove——一块 64KB 里的行越多总搬移越大（≈ n²/2），
                    // 而日志洪泛时正是"一块里几万行"的场景。游标版是 O(n)
                    let mut consumed = 0usize;
                    while let Some(rel) = partial[consumed..].iter().position(|&b| b == b'\n') {
                        let end = consumed + rel; // '\n' 的下标
                        let mut line = partial[consumed..end].to_vec();
                        consumed = end + 1;
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
                    if consumed > 0 {
                        partial.drain(..consumed);
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

impl ProcessManager {
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
        // 自己的存活计数（初值 1）+ 自己的发射线程 + 自己的唤醒通道：与进程 stdout 的
        // 发射线程互不影响（stdout 那两个 reader 此时早已 EOF 退出，它们的发射线程也已收尾）
        let alive = Arc::new(std::sync::atomic::AtomicUsize::new(1));
        let signal = Arc::new(LogSignal::new());
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let log_key: LogKey = Arc::from(key);
        let flush_sink = Arc::clone(&sink);
        let flush_key = key.to_string();
        spawn_log_flush_thread(
            format!("nexus-followflush-{}", key),
            Arc::clone(&pending),
            Arc::clone(&alive),
            Arc::clone(&signal),
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
            signal,
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
    pub(crate) fn probe_log_file(&self, key: &str, cwd: &str, since: SystemTime) {
        if self.followed_log(key).is_some() {
            return;
        }
        // 退避（PERF-22）：**先看窗口再扫目录**——原实现每一轮（前端每 3 秒一次 get_running）
        // 都要把 cwd、cwd/logs、cwd/data 与上两级各 logs/data 这 7 个目录扫一遍（每个最多
        // 500 条目 + 逐项 metadata）。服务若始终不写日志文件，这一扫**永不停止**；而对
        // Tomcat 这类十几秒后才落盘的服务，退避到 60s 也照样能发现（最坏多等一个间隔）
        let now_at = std::time::Instant::now();
        {
            let procs = match self.processes.lock() {
                Ok(p) => p,
                Err(e) => e.into_inner(),
            };
            if let Some(probe) = procs.get(key).and_then(|i| i.log_probe.as_ref()) {
                if now_at < probe.next_at {
                    return;
                }
            }
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
                // 已有首采：拿它跟这一轮比，并把下次的等待拉长一倍（封顶）
                Some(probe) => {
                    probe.interval_ms = (probe.interval_ms * 2).min(PROBE_LOG_MAX_INTERVAL_MS);
                    probe.next_at = now_at + std::time::Duration::from_millis(probe.interval_ms);
                    probe.first.clone()
                }
                None => {
                    // 首采：这轮只记录，不选（要有两次采样才能看出"谁在长"）
                    info.log_probe = Some(LogProbe {
                        first: now,
                        interval_ms: PROBE_LOG_FIRST_INTERVAL_MS,
                        next_at: now_at + std::time::Duration::from_millis(PROBE_LOG_FIRST_INTERVAL_MS),
                    });
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::process::{test_support::test_sink, ServiceSpawn};
    #[cfg(windows)]
    use crate::core::process::test_support::{wait_for_adoption, write_detach_bat};

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
            mgr.running().iter().any(|(_, id, _)| id == "follow-4"),
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
}
