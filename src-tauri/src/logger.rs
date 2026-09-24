//! 日志初始化：控制台（开发期）+ 文件（打包版唯一可见的诊断通道）
//!
//! 背景：release 构建用 `windows_subsystem = "windows"`，进程没有控制台，而 `env_logger`
//! 只写 stderr —— 打包版所有 `log::*` 输出（Job Object 降级、锁中毒、清理失败、数据库迁移、
//! 监听 0 路径、命令 spawn 失败……）**无处可看**，出问题后没有任何证据。
//! 这里把日志同时落到 `~/.nexus/logs/nexus.log`：超过 5MB 轮转为 `nexus.log.1`
//! （保留上一份，够定位一次崩溃），控制台输出保持原有格式与 RUST_LOG 过滤语义。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

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
}
