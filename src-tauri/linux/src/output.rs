//! 终端输出：简单等宽表格与日志尾部截取。

use std::collections::BTreeMap;

/// 打印表头 + 行；列宽按字符数计算（服务名限制 ASCII，状态可能是中文）。
pub fn table(headers: &[&str], rows: &[Vec<String>]) {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(cell.chars().count());
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
                let pad = widths[i].saturating_sub(cell.chars().count());
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

/// 取文本最后 `n` 行（保留换行）。
pub fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    let mut out = lines[start..].join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
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
    fn tail_keeps_last_lines() {
        assert_eq!(tail("a\nb\nc\n", 2), "b\nc\n");
        assert_eq!(tail("a", 5), "a\n");
        assert_eq!(tail("", 3), "");
    }
}
