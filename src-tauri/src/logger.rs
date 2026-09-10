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
}

impl FileSink {
    fn open() -> Option<Self> {
        let dir = dirs::home_dir()?.join(".nexus").join("logs");
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
        Some(Self { file, written, path })
    }

    fn write_line(&mut self, line: &str) {
        if self.written + line.len() as u64 + 1 > MAX_LOG_BYTES {
            self.rotate();
        }
        if self.file.write_all(line.as_bytes()).is_ok() && self.file.write_all(b"\n").is_ok() {
            self.written += line.len() as u64 + 1;
        }
    }

    /// 轮转：当前文件改名为 `nexus.log.1`（覆盖旧的那份），再重开空文件
    fn rotate(&mut self) {
        let rotated = self.path.with_file_name("nexus.log.1");
        let _ = std::fs::rename(&self.path, &rotated);
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
        // 文件行自带时间戳（控制台格式由 env_logger 负责，文件需要可检索的时间与级别）
        let ts = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f");
        let line = format!("{} {:<5} {} {}", ts, record.level(), record.target(), record.args());
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

    #[test]
    fn test_rotate_moves_file_and_reopens() {
        let dir = std::env::temp_dir().join(format!("nexus-logger-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nexus.log");
        std::fs::write(&path, "old\n").unwrap();
        let mut sink = FileSink {
            file: OpenOptions::new().create(true).append(true).open(&path).unwrap(),
            written: 4,
            path: path.clone(),
        };
        sink.rotate();
        assert!(dir.join("nexus.log.1").exists(), "旧文件应被轮转为 nexus.log.1");
        assert!(path.exists(), "轮转后应重开新的 nexus.log");
        assert_eq!(sink.written, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_write_line_appends_newline() {
        let dir = std::env::temp_dir().join(format!("nexus-logger-w-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nexus.log");
        let mut sink = FileSink {
            file: OpenOptions::new().create(true).append(true).open(&path).unwrap(),
            written: 0,
            path: path.clone(),
        };
        sink.write_line("hello");
        sink.write_line("world");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\nworld\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
