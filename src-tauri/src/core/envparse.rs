//! 服务环境变量配置的解析（ARCH-23：原样从 `core::process` 拆出）。
//!
//! 支持 dotenv（`KEY=VALUE` 每行）与 JSON 对象两种写法；非法输入一律报错并指出行号，
//! 不静默丢弃（理由见 `parse_env_vars` 的说明）。

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
