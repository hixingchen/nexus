//! 日志初始化：控制台（开发期）+ 文件（打包版唯一可见的诊断通道）
//!
//! 背景：release 构建用 `windows_subsystem = "windows"`，进程没有控制台，而 `env_logger`
//! 只写 stderr —— 打包版所有 `log::*` 输出（Job Object 降级、锁中毒、清理失败、数据库迁移、
//! 监听 0 路径、命令 spawn 失败……）**无处可看**，出问题后没有任何证据。
//! 这里把日志同时落到 `~/.nexus/logs/nexus.log`：超过 5MB 轮转为 `nexus.log.1`
//! （保留上一份，够定位一次崩溃），控制台输出保持原有格式与 RUST_LOG 过滤语义。

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 单文件上限：超过后轮转为 `nexus.log.1`
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

struct FileSink {
    file: File,
    written: u64,
    path: PathBuf,
    /// 轮转是否已经失败过（失败后不再重试，见 `rotate`）
    rotate_blocked: bool,
}

impl FileSink {
    fn open() -> Option<Self> {
        let dir = crate::data_dir().join("logs");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("[nexus] 创建日志目录失败（文件日志已禁用）: {}", e);
            return None;
        }
        let path = dir.join("nexus.log");
        let file = match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[nexus] 打开日志文件失败（文件日志已禁用）: {}", e);
                return None;
            }
        };
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Some(Self { file, written, path, rotate_blocked: false })
    }

    fn write_line(&mut self, line_with_newline: &str) {
        if self.written + line_with_newline.len() as u64 > MAX_LOG_BYTES {
            self.rotate();
        }
        // 一次 write_all（调用方已把换行拼进字符串）：原实现分两次写正文与换行，
        // 每条日志记录多一次系统调用——诊断日志常有逐行输出（RUST_LOG=debug 时更甚）
        if self.file.write_all(line_with_newline.as_bytes()).is_ok() {
            self.written += line_with_newline.len() as u64;
        }
    }

    /// 轮转：当前文件改名为 `nexus.log.1`（覆盖旧的那份），再重开空文件。
    ///
    /// **rename 失败就不能把 `written` 清零**（CQ-32）：原实现无条件归零，而那时文件其实
    /// 是继续追加的——计数归零等于把这个 5MB 上限按失败次数成倍放大（`nexus.log.1` 被以
    /// 拒绝共享的方式打开、或目录 ACL 只读都会走到这里），而打包版唯一的诊断通道就是这个
    /// 文件。失败还得说话：重开失败会 eprintln，rename 失败此前完全静默，两条路不一致。
    fn rotate(&mut self) {
        if self.rotate_blocked {
            return; // 已失败过：不再每写一行就重试一次（否则 stderr 会被刷屏）
        }
        let rotated = self.path.with_file_name("nexus.log.1");
        if let Err(e) = std::fs::rename(&self.path, &rotated) {
            self.rotate_blocked = true;
            eprintln!("[nexus] 日志轮转失败（本次运行内不再重试，日志将继续增长）: {}", e);
            return;
        }
        match OpenOptions::new().create(true).append(true).open(&self.path) {
            Ok(f) => {
                self.file = f;
                self.written = 0;
            }
            Err(e) => eprintln!("[nexus] 日志轮转失败: {}", e),
        }
    }
}

/// 控制台 + 文件的双写 logger：级别过滤沿用 env_logger（受 RUST_LOG 控制）
struct CompositeLogger {
    console: env_logger::Logger,
    file: Option<Mutex<FileSink>>,
}

impl log::Log for CompositeLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.console.enabled(metadata)
    }

    fn log(&self, record: &log::Record) {
        if !self.console.enabled(record.metadata()) {
            return;
        }
        self.console.log(record);
        let Some(sink) = &self.file else { return };
        // 文件行自带时间戳（控制台格式由 env_logger 负责，文件需要可检索的时间与级别）；
        // 换行在这里拼进同一份字符串，写盘时只需一次系统调用
        let ts = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f");
        let line = format!("{} {:<5} {} {}\n", ts, record.level(), record.target(), record.args());
        if let Ok(mut s) = sink.lock() {
            s.write_line(&line);
        }
    }

    fn flush(&self) {
        self.console.flush();
    }
}

/// 初始化全局 logger（幂等：重复调用直接返回）
pub fn init() {
    // max_level 放到 Trace，让真正的过滤交给 env_logger::Logger::enabled（RUST_LOG 语义不变）；
    // 被过滤掉的记录不会被格式化，开销可忽略
    let console = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).build();
    let file = FileSink::open().map(Mutex::new);
    let file_log_enabled = file.is_some();
    if log::set_boxed_logger(Box::new(CompositeLogger { console, file })).is_ok() {
        log::set_max_level(log::LevelFilter::Trace);
    }
    log::info!("[nexus] 日志已就绪（文件日志: {}）", if file_log_enabled { "开" } else { "关" });
}

/* ---- 前端日志回传（NEW-28） ---- */

/// 前端消息落盘前的字符上限（超出截断）
const FRONTEND_LOG_MAX_CHARS: usize = 2000;
/// 同一 key 在一个窗口内最多放行的条数
const FRONTEND_LOG_MAX_PER_WINDOW: u32 = 5;
/// 限流窗口
const FRONTEND_LOG_WINDOW: Duration = Duration::from_secs(60);
/// 限流表的键数上限（键完全由 IPC 输入决定，必须封顶）
const FRONTEND_LOG_MAX_KEYS: usize = 256;
/// 截断标记（算在长度上限之外，见 `sanitize_frontend_message` 的说明）
const TRUNCATED_MARK: &str = "…（已截断）";

/// 前端传来的级别 → `log::Level`（**白名单**，未知一律 Info）。
///
/// 为什么不认识就报错：这条通道是用来**报告故障**的，为一个拼错的级别把整条日志丢掉，
/// 正好丢掉最需要的那条（前端只发 error/warn，其余级别是给 `RUST_LOG=debug` 用的）。
fn normalize_level(level: &str) -> log::Level {
    match level {
        "error" => log::Level::Error,
        "warn" => log::Level::Warn,
        "debug" => log::Level::Debug,
        "trace" => log::Level::Trace,
        _ => log::Level::Info,
    }
}

/// 单行化 + 截断：返回值保证**不含换行**，正文不超过 `FRONTEND_LOG_MAX_CHARS` 字符
/// （转义把 1 个字符变成 2 个时可能多出 1 个，截断标记另算）。
///
/// 换行是**伪造日志行**的唯一手段：文件 sink 按行追加，注入 `\n` 就能凭空造出一条
/// 时间戳与级别俱全的假记录——而诊断日志的全部价值就在"这行是程序写的"。
/// 换成可见的 `\n` 字面量（而不是删掉）是为了保住"这里原本断过行"这个信息。
///
/// 按**字符**而不是字节截断：`String::truncate` 会切在 UTF-8 中间并 panic，而这段输入
/// 完全来自 webview。上限之外额外追加一个固定标记（它本身不算进上限）。
fn sanitize_frontend_message(raw: &str) -> String {
    let mut out = String::with_capacity(FRONTEND_LOG_MAX_CHARS);
    // 计的是**替换后**的宽度：转义会把 1 个字符变成 2 个
    let mut width = 0usize;
    let mut overflow = false;
    for c in raw.chars() {
        if width >= FRONTEND_LOG_MAX_CHARS {
            overflow = true;
            break;
        }
        match c {
            '\n' => {
                out.push_str("\\n");
                width += 2;
            }
            '\r' => {
                out.push_str("\\r");
                width += 2;
            }
            // 其余控制字符（\t / DEL / ESC…）：换成空格。ESC 能把终端里的日志染色或清屏，
            // 而日志的读者正是要拿它来还原现场的人
            c if c.is_control() => {
                out.push(' ');
                width += 1;
            }
            c => {
                out.push(c);
                width += 1;
            }
        }
    }
    if overflow {
        out.push_str(TRUNCATED_MARK);
    }
    out
}

/// 限流判定结果
#[derive(Debug, PartialEq, Eq)]
enum Gate {
    /// 放行，照常写日志
    Pass,
    /// 丢弃；这是本窗口内第一次丢弃
    DropFirst,
    /// 丢弃（本窗口内已经说明过）
    Drop,
}

/// 前端日志的去重 + 限流闸：key = 级别 + 消息，窗口 60s，同一 key 内最多 5 条。
///
/// 为什么要（NEW-28）：前端的错误天生是**重复**的——轮询失败、渲染循环里抛异常、
/// 高频路径调 `reportError`。没有这道闸，一次故障能在几秒内把 5MB 的日志写满，
/// 顺便把之前有用的历史轮转掉——那正好废掉这条通道的全部价值。
/// 丢弃时**必须留一句**（`DropFirst`）：否则用户会以为"这个错误只发生了一次"，
/// 而"静默少记"与"没发生"在日志里长得一模一样。
///
/// 为什么整道闸放在后端而不是前端：这是 IPC 边界，输入不可信（铁律 10）。长度、换行、
/// 频率三件事都由**收下这份输入的一侧**保证，而不是靠"前端记得别刷屏"。
#[derive(Default)]
struct FrontendLogGate {
    /// key → (窗口起点, 本窗口已放行条数, 是否已就"开始丢弃"留过痕)
    windows: HashMap<String, (Instant, u32, bool)>,
}

impl FrontendLogGate {
    /// `now` 由参数传入而不是内部取：窗口过期是这条闸最主要的边界，单测必须能把它
    /// 推着走——否则只能靠 `sleep`（慢，且在 CI 上不可靠）
    fn check_at(&mut self, key: &str, now: Instant) -> Gate {
        if self.windows.len() >= FRONTEND_LOG_MAX_KEYS && !self.windows.contains_key(key) {
            // 键来自不可信输入，必须封顶（否则就是一条内存泄漏路径）。先清过期项，
            // 仍然满则整体清空：代价只是限流计数重置（同类消息可能多放几条），
            // 比"表随输入无限增长"好
            self.windows
                .retain(|_, (start, _, _)| now.duration_since(*start) < FRONTEND_LOG_WINDOW);
            if self.windows.len() >= FRONTEND_LOG_MAX_KEYS {
                self.windows.clear();
            }
        }
        match self.windows.get_mut(key) {
            Some(entry) if now.duration_since(entry.0) < FRONTEND_LOG_WINDOW => {
                if entry.1 < FRONTEND_LOG_MAX_PER_WINDOW {
                    entry.1 += 1;
                    Gate::Pass
                } else if entry.2 {
                    Gate::Drop
                } else {
                    entry.2 = true;
                    Gate::DropFirst
                }
            }
            Some(entry) => {
                *entry = (now, 1, false);
                Gate::Pass
            }
            None => {
                self.windows.insert(key.to_string(), (now, 1, false));
                Gate::Pass
            }
        }
    }
}

/// 全局限流表（只存计数与时刻，中毒后原样取回数据继续用：
/// 这条路径上"少记一条日志"没有任何理由让 IPC 线程 panic）
fn frontend_log_gate() -> &'static Mutex<FrontendLogGate> {
    static GATE: OnceLock<Mutex<FrontendLogGate>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(FrontendLogGate::default()))
}

/// 前端日志的统一入口（由 `commands::app::log_frontend` 调用）：校验 → 限流 → 写同一份日志。
///
/// 打包版既没有控制台也没有 devtools（`tauri` 的 features 里不含 `devtools`），
/// README 承诺的"排查问题看 `nexus.log`"此前只覆盖 Rust 侧，而最近几轮的真实故障
/// （日志跟随静默停止、失败态渲染、右键菜单）**全发生在前端**——一条日志都没留下。
pub fn log_frontend_message(level: &str, message: &str) {
    let level = normalize_level(level);
    let text = sanitize_frontend_message(message);
    if text.trim().is_empty() {
        return; // 空消息没有诊断价值，也挡住"用空白刷屏"
    }
    let verdict = {
        let mut gate = frontend_log_gate().lock().unwrap_or_else(|e| e.into_inner());
        gate.check_at(&format!("{}|{}", level, text), Instant::now())
    };
    match verdict {
        Gate::Pass => match level {
            log::Level::Error => log::error!("[frontend] {}", text),
            log::Level::Warn => log::warn!("[frontend] {}", text),
            log::Level::Info => log::info!("[frontend] {}", text),
            log::Level::Debug => log::debug!("[frontend] {}", text),
            log::Level::Trace => log::trace!("[frontend] {}", text),
        },
        Gate::DropFirst => log::warn!(
            "[frontend] 同一消息 {} 秒内已超过 {} 条，本窗口内不再写日志（本次丢弃）: {}",
            FRONTEND_LOG_WINDOW.as_secs(),
            FRONTEND_LOG_MAX_PER_WINDOW,
            text
        ),
        Gate::Drop => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用的 sink：字段列表只写在这里，加字段时不必逐个用例改
    fn test_sink(path: &std::path::Path, written: u64) -> FileSink {
        FileSink {
            file: OpenOptions::new().create(true).append(true).open(path).unwrap(),
            written,
            path: path.to_path_buf(),
            rotate_blocked: false,
        }
    }

    #[test]
    fn test_rotate_moves_file_and_reopens() {
        let dir = std::env::temp_dir().join(format!("nexus-logger-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nexus.log");
        std::fs::write(&path, "old\n").unwrap();
        let mut sink = test_sink(&path, 4);
        sink.rotate();
        assert!(dir.join("nexus.log.1").exists(), "旧文件应被轮转为 nexus.log.1");
        assert!(path.exists(), "轮转后应重开新的 nexus.log");
        assert_eq!(sink.written, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_write_line_writes_single_call() {
        let dir = std::env::temp_dir().join(format!("nexus-logger-w-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nexus.log");
        let mut sink = test_sink(&path, 0);
        // 约定：调用方把换行拼进字符串，写入只做一次系统调用
        sink.write_line("hello\n");
        sink.write_line("world\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\nworld\n");
        assert_eq!(sink.written, 12, "字节计数应包含换行");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// CQ-32：轮转**失败**时不能把字节计数清零，也不能一声不吭。
    ///
    /// 旧实现 `let _ = std::fs::rename(...)` 之后无条件 `written = 0`，而那一刻文件其实是
    /// **继续追加**的——于是每失败一次，5MB 的上限就重新攒一遍，实际能长到多少全看失败次数，
    /// 而打包版唯一的诊断通道就是这个文件（README 承诺的上限因此不成立）。
    #[test]
    fn test_rotate_failure_keeps_byte_counter_and_marks_blocked() {
        let dir = std::env::temp_dir().join(format!("nexus-logger-fail-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nexus.log");
        std::fs::write(&path, "old\n").unwrap();
        // 占住 nexus.log.1 这个名字：用一个同名**目录**，rename 到已存在的目录必然失败
        std::fs::create_dir_all(dir.join("nexus.log.1")).unwrap();

        let mut sink = test_sink(&path, MAX_LOG_BYTES);
        sink.write_line("more\n");

        assert!(sink.rotate_blocked, "失败要标记：否则每写一行都去 rename 一次并刷屏 stderr");
        assert!(
            sink.written > MAX_LOG_BYTES,
            "笔数要继续累计（实际写入 {} 字节）——清零等于把上限成倍放大",
            sink.written,
        );
        assert!(path.exists(), "轮转失败后仍在写原文件");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// NEW-28：前端消息必须被压成**一行**——文件 sink 按行追加，注入换行就能伪造日志记录。
    #[test]
    fn test_frontend_message_cannot_forge_a_log_line() {
        let out = sanitize_frontend_message("错误: 失败\n2026-01-01T00:00:00.000 ERROR nexus_lib 假的记录\r\n尾巴");
        assert!(!out.contains('\n'), "输出里不能有换行：{}", out);
        assert!(!out.contains('\r'), "输出里不能有回车：{}", out);
        // 换行改为可见字面量（保住"这里断过行"的信息），其余控制字符（\t）变空格
        assert_eq!(out, "错误: 失败\\n2026-01-01T00:00:00.000 ERROR nexus_lib 假的记录\\r\\n尾巴");
        assert_eq!(sanitize_frontend_message("a\tb"), "a b");
    }

    /// NEW-28：几 MB 的字符串不能进日志（前端一次误传就能顶掉一整轮 5MB 的轮转）。
    #[test]
    fn test_frontend_message_is_length_capped() {
        let raw = "错".repeat(FRONTEND_LOG_MAX_CHARS * 3);
        let out = sanitize_frontend_message(&raw);
        // 上限 + 转义可能多出的 1 格 + 固定标记
        let limit = FRONTEND_LOG_MAX_CHARS + 1 + TRUNCATED_MARK.chars().count();
        assert!(out.chars().count() <= limit, "截断后仍有 {} 字符", out.chars().count());
        assert!(out.ends_with(TRUNCATED_MARK), "截断要留下痕迹");
        // 短消息原样（不带标记）
        let short = sanitize_frontend_message("就一行");
        assert_eq!(short, "就一行");
        assert!(!short.contains(TRUNCATED_MARK));
        // 恰好等于上限的输入不算超长（边界不多截一个字符）
        let exact = "x".repeat(FRONTEND_LOG_MAX_CHARS);
        assert_eq!(sanitize_frontend_message(&exact), exact);
    }

    #[test]
    fn test_frontend_level_is_whitelisted() {
        assert_eq!(normalize_level("error"), log::Level::Error);
        assert_eq!(normalize_level("warn"), log::Level::Warn);
        assert_eq!(normalize_level("debug"), log::Level::Debug);
        // 未知/伪造的级别一律降到 Info：不认识的级别不该被当成"更严重"
        assert_eq!(normalize_level("fatal"), log::Level::Info);
        assert_eq!(normalize_level(""), log::Level::Info);
        assert_eq!(normalize_level("ERROR"), log::Level::Info);
    }

    /// NEW-28：同样的消息在一个窗口内只能写 N 条，且**第一次丢弃要留痕**。
    #[test]
    fn test_frontend_gate_dedups_and_announces_first_drop() {
        let mut gate = FrontendLogGate::default();
        let t0 = Instant::now();
        for i in 0..FRONTEND_LOG_MAX_PER_WINDOW {
            assert_eq!(gate.check_at("error|boom", t0), Gate::Pass, "第 {} 条应放行", i + 1);
        }
        // 超出：第一次丢弃返回 DropFirst（调用方据此写一行"已限流"），之后静默
        assert_eq!(gate.check_at("error|boom", t0), Gate::DropFirst);
        assert_eq!(gate.check_at("error|boom", t0), Gate::Drop);
        assert_eq!(gate.check_at("error|boom", t0 + Duration::from_secs(1)), Gate::Drop);
        // 别的 key 不受影响（否则一个高频错误会把整条通道按死）
        assert_eq!(gate.check_at("warn|other", t0), Gate::Pass);
        // 窗口过期后重新计数，并且**能再次**留一次痕
        let t1 = t0 + FRONTEND_LOG_WINDOW + Duration::from_secs(1);
        assert_eq!(gate.check_at("error|boom", t1), Gate::Pass);
        for _ in 1..FRONTEND_LOG_MAX_PER_WINDOW {
            assert_eq!(gate.check_at("error|boom", t1), Gate::Pass);
        }
        assert_eq!(gate.check_at("error|boom", t1), Gate::DropFirst);
    }

    /// NEW-28：限流表不能随不可信输入无限增长。
    #[test]
    fn test_frontend_gate_table_is_bounded() {
        let mut gate = FrontendLogGate::default();
        let t0 = Instant::now();
        for i in 0..(FRONTEND_LOG_MAX_KEYS * 2) {
            gate.check_at(&format!("info|消息{}", i), t0);
        }
        assert!(
            gate.windows.len() <= FRONTEND_LOG_MAX_KEYS,
            "键数应被压在 {} 以内，实际 {}",
            FRONTEND_LOG_MAX_KEYS,
            gate.windows.len()
        );
    }
}
