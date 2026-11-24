//! 文本解码的唯一口径（CQ-36）：UTF-8 严格解码，失败回退 GB18030。
//!
//! 为什么必须只有一处：这段逻辑原先在四个地方各写了一遍（编辑器读文件 / 内容搜索 /
//! 服务日志行 / nvm 输出），而且四处的注释在互相指认"与编辑器读文件同一套口径"。
//! 口径一旦要改（再加一种回退编码、或改"有损"的判定），必须记得改四处，而漏一处
//! **不会被任何编译或测试发现**——漏在 `node.rs` 是 nvm 的报错变成一串问号，
//! 漏在 `search.rs` 是老项目的 GBK 文件在搜索结果里静默缺失。
//!
//! GB18030 是 GBK 的超集，覆盖中文 Windows 上老项目的文件与 cmd / nvm 的 GBK 输出。

/// 解码结果的三态。编辑器据此决定"能不能按原编码写回"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextEncoding {
    Utf8,
    Gb18030,
    /// 既不是合法 UTF-8、也不是合法 GB18030 的字节（cp1252 / Shift-JIS / Big5 等）。
    ///
    /// GB18030 解码器对这些字节产出替换字符 U+FFFD，而替换字符**无法还原成原字节**：
    /// 按 gb18030 写回会把原文永久固化成一串问号/方块。丢掉这个标志就会出现
    /// "打开老文件 → 一个字符都没改 → Ctrl+S"，静默损坏文件（P0-6 同一条链上的坑）。
    Lossy,
}

impl TextEncoding {
    /// 落库/落盘用的标识（前端按它决定写回编码）
    pub fn id(self) -> &'static str {
        match self {
            TextEncoding::Utf8 => "utf8",
            TextEncoding::Gb18030 => "gb18030",
            TextEncoding::Lossy => "lossy",
        }
    }
}

/// 解码字节为文本（能零拷贝就零拷贝），并报出用的是哪种编码（含"有损"）。
///
/// `decode` 与 `decode_to_string` 都基于它——UTF-8 是绝大多数文件的实际情况，
/// 对它们不必整份复制（PERF-29：搜索一次最多扫 2 万个 2MB 的文件，整份 `to_string()`
/// 会在**未命中**时也付 200MB 级的分配与拷贝）。
pub fn decode_cow(bytes: &[u8]) -> (std::borrow::Cow<'_, str>, TextEncoding) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (std::borrow::Cow::Borrowed(s), TextEncoding::Utf8),
        Err(_) => {
            let (text, _enc, had_errors) = encoding_rs::GB18030.decode(bytes);
            let kind = if had_errors { TextEncoding::Lossy } else { TextEncoding::Gb18030 };
            (std::borrow::Cow::Owned(text.into_owned()), kind)
        }
    }
}

/// 解码字节为文本，并报出用的是哪种编码（含"有损"）
pub fn decode(bytes: &[u8]) -> (String, TextEncoding) {
    let (text, kind) = decode_cow(bytes);
    (text.into_owned(), kind)
}

/// 只关心文本、不关心编码的调用方（服务日志行 / nvm 输出 / 内容搜索）
pub fn decode_to_string(bytes: &[u8]) -> String {
    decode_cow(bytes).0.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_is_decoded_as_utf8() {
        let (text, enc) = decode("中文 abc".as_bytes());
        assert_eq!(text, "中文 abc");
        assert_eq!(enc, TextEncoding::Utf8);
        assert_eq!(enc.id(), "utf8");
    }

    #[test]
    fn gbk_bytes_fall_back_to_gb18030() {
        // "中文" 的 GBK 编码
        let gbk = [0xD6u8, 0xD0, 0xCE, 0xC4];
        let (text, enc) = decode(&gbk);
        assert_eq!(text, "中文");
        assert_eq!(enc, TextEncoding::Gb18030, "合法 GB18030 不该标成 lossy");
    }

    #[test]
    fn neither_utf8_nor_gb18030_is_lossy() {
        // 0xE9 出现在**末尾**（Latin-1 的 'é'）：GB18030 里它是双字节前导却没有后继字节
        // → 解出替换字符。注：后面接一个 ASCII 字节（如 `a, 0xE9, b`）反而是合法双字节，
        // 那样就落到 gb18030 分支了（写这条断言时先踩过一次）
        let bytes = [b'c', b'a', b'f', 0xE9];
        let (text, enc) = decode(&bytes);
        assert_eq!(enc, TextEncoding::Lossy, "两种编码都不合法必须标 lossy（否则写回会毁原文）");
        assert!(text.contains('\u{FFFD}'), "lossy 文本里应能观察到替换字符: {:?}", text);
    }

    #[test]
    fn empty_input_is_empty_utf8() {
        let (text, enc) = decode(&[]);
        assert_eq!(text, "");
        assert_eq!(enc, TextEncoding::Utf8);
    }
}
