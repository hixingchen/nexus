//! 日志行的解码与文本规整（ARCH-23：原样从 `core::process` 拆出）。
//!
//! 只做"把子进程/日志文件吐出的字节变成一行可显示的文本"：坏编码回退解码、超长行按
//! char 边界截断、非 SGR 的 ANSI 转义序列清理。缓冲与批量发射见 `logpipe`，
//! 文件跟随见 `logfollow`。

use std::io::BufRead;

/// 解码一行字节：先 UTF-8，失败按 GB18030 回退。
///
/// 不能"跳过解不出来的行"——中文 Windows 上 cmd 的报错就是 GBK，跳过它等于把唯一的原因
/// 删掉（用户看到的是"命令失败（退出码 1）。（无输出）"）。服务 stdout（`BoundedLineReader`）
/// 与日志文件跟随（`spawn_log_tailer`）共用这一条口径。
pub(crate) fn decode_lossy(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => crate::core::textenc::decode_to_string(e.as_bytes()),
    }
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

/// 单行日志字节上限（base64/JSON dump 等超长行截断）
pub(crate) const MAX_LINE_BYTES: usize = 8192;

/// 单行读取的字节上限：比日志行上限宽一些（行内多字节字符/ANSI 序列会占位），
/// 但必须存在——`BufRead::lines()` 会把**整行**先读进内存，`truncate_line` 只在读完之后截断，
/// 单行 GB 级输出（base64/JSON dump）足以打满内存。
pub(crate) const MAX_READ_LINE_BYTES: usize = MAX_LINE_BYTES * 4;

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

/// 日志文本规整：清 ANSI（仅在确有转义序列时）→ 刷新行补回 `\r` 前缀 → 截断超长行。
///
/// 抽成函数的原因：stdout/stderr 两段 reader 曾各自逐字重写这 10 行（本项目最典型的孪生重复），
/// 任一处改了另一处就容易漂移。
pub(crate) fn prepare_log_text(line: String, is_refresh: bool) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use std::process::Stdio;

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
}
