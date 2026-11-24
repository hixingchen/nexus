//! 终端输出：简单等宽表格、日志尾部截取与 `--follow`。

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{IsTerminal, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Duration;

use boot_core::textenc::decode_to_string;

/// 终端语义色。ANSI 用零宽转义序列，表格计算列宽时要跳过。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Green,
    Red,
    Yellow,
    Cyan,
    Dim,
}

impl Color {
    fn code(self) -> &'static str {
        match self {
            Color::Green => "32",
            Color::Red => "31",
            Color::Yellow => "33",
            Color::Cyan => "36",
            Color::Dim => "2",
        }
    }
}

fn color_enabled() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if std::env::var("TERM").map(|t| t == "dumb").unwrap_or(false) {
        return false;
    }
    std::io::stdout().is_terminal()
}

/// 给终端文本上色；非 TTY / NO_COLOR / TERM=dumb 时原样返回。
pub fn paint(text: &str, color: Color) -> String {
    if !color_enabled() {
        return text.to_string();
    }
    format!("\x1b[{}m{}\x1b[0m", color.code(), text)
}

/// 状态机 → 颜色：成功绿、失败红、进行中黄、停止灰。
pub fn phase_color(phase: &str) -> Option<Color> {
    match phase {
        "ready" => Some(Color::Green),
        "failed" => Some(Color::Red),
        "starting" | "restarting" | "pending" => Some(Color::Yellow),
        "stopped" | "exited" | "skipped" => Some(Color::Dim),
        _ => None,
    }
}

/// 终端显示宽度：跳过 ANSI CSI 序列，避免颜色码把表格撑歪。
fn visible_width(s: &str) -> usize {
    let mut width = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        width += 1;
    }
    width
}

/// 一次最多读日志末尾这么多字节：日志再大也不会整份进内存。
pub const MAX_TAIL_BYTES: u64 = 8 * 1024 * 1024;
/// `--follow` 每次读取的上限。
const FOLLOW_CHUNK: usize = 64 * 1024;
const FOLLOW_POLL: Duration = Duration::from_millis(500);

/// 打印表头 + 行；列宽按字符数计算（服务名限制 ASCII，状态可能是中文）。
pub fn table(headers: &[&str], rows: &[Vec<String>]) {
    let mut widths: Vec<usize> = headers.iter().map(|h| visible_width(h)).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(visible_width(cell));
            }
        }
    }
    let render = |cells: &[String]| {
        let mut line = String::new();
        for (i, cell) in cells.iter().enumerate() {
            if i > 0 {
                line.push_str("  ");
            }
            line.push_str(cell);
            if i + 1 < cells.len() {
                let pad = widths[i].saturating_sub(visible_width(cell));
                line.push_str(&" ".repeat(pad));
            }
        }
        line
    };
    let head = headers.iter().map(|h| h.to_string()).collect::<Vec<_>>();
    println!("{}", render(&head));
    for row in rows {
        println!("{}", render(row));
    }
}

/// 读取日志末尾的原始字节（最多 [`MAX_TAIL_BYTES`]），行尾统一补 `\n`。
///
/// 不整份读文件：服务日志可能被用户程序写大，整份 `read_to_end` 会让 CLI
/// 在低内存机器上直接失败。文件在元数据之后继续增长也不会读超过上限。
pub fn read_tail_bytes(path: &Path, lines: usize) -> std::io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(MAX_TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let want = usize::try_from(len - start)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "文件过大"))?;
    let mut bytes = Vec::with_capacity(want);
    file.take(want as u64).read_to_end(&mut bytes)?;
    // 从中间截断时第一行可能是半行；丢掉它，避免把半截日志当成完整行。
    let bytes = if start > 0 {
        match bytes.iter().position(|b| *b == b'\n') {
            Some(i) => &bytes[i + 1..],
            None => &bytes[..],
        }
    } else {
        &bytes[..]
    };
    let body = match bytes.last() {
        Some(b'\n') => &bytes[..bytes.len() - 1],
        _ => bytes,
    };
    if lines == 0 || body.is_empty() {
        return Ok(Vec::new());
    }
    let mut seen = 0usize;
    let mut line_start = 0usize;
    for i in (0..body.len()).rev() {
        if body[i] == b'\n' {
            seen += 1;
            if seen == lines {
                line_start = i + 1;
                break;
            }
        }
    }
    let mut out = body[line_start..].to_vec();
    out.push(b'\n');
    Ok(out)
}

/// 读取并解码日志末尾。`--follow` 用原始字节，避免把 GBK 日志先转成 UTF-8
/// 之后又与后续原始字节混在一起。
pub fn read_tail(path: &Path, lines: usize) -> std::io::Result<String> {
    let bytes = read_tail_bytes(path, lines)?;
    if bytes.is_empty() {
        return Ok(String::new());
    }
    Ok(decode_to_string(&bytes))
}

/// 持续跟踪日志（类似 `tail -f`）。
///
/// - 先打印最后 `lines` 行；
/// - 之后只读新增字节，单次最多 64 KiB，内存占用有界；
/// - 文件被轮转/截断/删除后自动重新打开，不会一直读旧 inode；
/// - 文件尚未创建时等待它出现。
pub fn follow_file(path: &Path, lines: usize) -> Result<(), String> {
    let stdout = std::io::stdout();
    let mut read_from_start = match read_tail_bytes(path, lines) {
        Ok(bytes) => {
            if !bytes.is_empty() {
                let mut out = stdout.lock();
                out.write_all(&bytes)
                    .map_err(|e| format!("写终端失败: {}", e))?;
                out.flush().map_err(|e| format!("刷新终端失败: {}", e))?;
            }
            false
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("[nexusctl] 等待日志文件出现: {}", path.display());
            true
        }
        Err(e) => return Err(format!("读取日志失败 {}: {}", path.display(), e)),
    };

    let mut file: Option<File> = None;
    let mut last_len = 0u64;
    loop {
        if file.is_none() {
            match File::open(path) {
                Ok(mut f) => {
                    if read_from_start {
                        f.seek(SeekFrom::Start(0))
                            .map_err(|e| format!("定位日志失败 {}: {}", path.display(), e))?;
                        last_len = 0;
                        read_from_start = false;
                    } else {
                        f.seek(SeekFrom::End(0))
                            .map_err(|e| format!("定位日志失败 {}: {}", path.display(), e))?;
                        last_len = f
                            .stream_position()
                            .map_err(|e| format!("读取日志位置失败 {}: {}", path.display(), e))?;
                    }
                    file = Some(f);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    std::thread::sleep(FOLLOW_POLL);
                    continue;
                }
                Err(e) => return Err(format!("打开日志失败 {}: {}", path.display(), e)),
            }
        }

        let (changed, truncated) = {
            let f = file.as_ref().expect("刚打开");
            let current = f
                .metadata()
                .map_err(|e| format!("读取日志元数据失败 {}: {}", path.display(), e))?;
            match std::fs::metadata(path) {
                Ok(meta) => (!same_file(&current, &meta), meta.len() < last_len),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (true, false),
                Err(e) => return Err(format!("读取日志元数据失败 {}: {}", path.display(), e)),
            }
        };
        if changed || truncated {
            // 轮转或原地截断：从头读新文件，否则会一直停在旧 inode 上。
            file = None;
            last_len = 0;
            read_from_start = true;
            continue;
        }

        let mut buf = [0u8; FOLLOW_CHUNK];
        match file.as_mut().expect("已打开").read(&mut buf) {
            Ok(0) => std::thread::sleep(FOLLOW_POLL),
            Ok(n) => {
                let mut out = stdout.lock();
                out.write_all(&buf[..n])
                    .map_err(|e| format!("写终端失败: {}", e))?;
                out.flush().map_err(|e| format!("刷新终端失败: {}", e))?;
                last_len = last_len.saturating_add(n as u64);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("读取日志失败 {}: {}", path.display(), e)),
        }
    }
}

/// 两个元数据是否指向同一个文件（Unix 用 dev+ino；其它平台退化为长度判断）。
#[cfg(unix)]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_file(_a: &std::fs::Metadata, _b: &std::fs::Metadata) -> bool {
    // 非 Unix 不是发布目标；长度判断至少能发现常见截断。
    true
}

/// 把 `BTreeMap` 转成 JSON 对象（值都是字符串）。
pub fn map_to_json(map: &BTreeMap<String, String>) -> serde_json::Value {
    serde_json::Value::Object(
        map.iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_width_ignores_ansi_color() {
        assert_eq!(visible_width("ready"), 5);
        assert_eq!(visible_width("\x1b[32mready\x1b[0m"), 5);
        assert_eq!(visible_width("a\x1b[1;31mb\x1b[0mc"), 3);
    }

    fn temp_log(tag: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("nexus-output-{}-{}.log", tag, std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn read_tail_reads_only_the_end() {
        let path = temp_log("tail");
        std::fs::write(&path, "a\nb\nc\nd\n").expect("写日志");
        assert_eq!(read_tail(&path, 2).expect("读尾部"), "c\nd\n");
        assert_eq!(read_tail(&path, 99).expect("读全部"), "a\nb\nc\nd\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_tail_reports_missing_file() {
        let path = temp_log("missing");
        assert_eq!(
            read_tail(&path, 10).expect_err("文件不存在").kind(),
            std::io::ErrorKind::NotFound
        );
    }
}
