//! 命令原文脱敏（ARCH-23：原样从 `core::process` 拆出）。
//!
//! 日志会落到 `~/.nexus/logs/nexus.log`，明文凭据会长期留在磁盘上——这里的规则决定
//! "哪些 token 在写日志之前必须被掩掉"。覆盖范围与取舍见 `mask_command` 的说明。

/// 命令原文脱敏：日志中的凭据掩码为 `***`。
///
/// 为什么必须做：日志会落到 `~/.nexus/logs/nexus.log`，明文凭据（形如
/// `java -Dapi.key=xxx -jar app.jar`）会长期留在磁盘上。
///
/// 覆盖四类常见写法（启发式，不追求完备）：
///   1. `NAME=VALUE`：`-Dapi.key=x`、`TOKEN=x`、`...?token=x`
///   2. **空格分隔取值**：`--token x`、`--password x`、`-u x`
///      —— 旧实现只处理含 `=` 的 token，这一类**完全漏掉**
///   3. `Bearer x` / `Basic x`（典型来源 `-H "Authorization: Bearer eyJ…"`）
///   4. URL 用户信息 `scheme://user:pass@host`，以及 `-u user:pass` 的密码段
pub fn mask_command(command: &str) -> String {
    let mut out: Vec<String> = Vec::with_capacity(16);
    // 上一个 token 是"取值型敏感开关"时，当前 token 是它的取值
    let mut pending: Option<PendingMask> = None;
    for token in command.split(' ') {
        if let Some(kind) = pending.take() {
            out.push(match kind {
                PendingMask::Whole => format!("***{}", query_tail(token)),
                PendingMask::UserInfo => mask_user_info(token),
            });
            continue;
        }
        // `Bearer <token>` / `Basic <base64>`：保留方案名，掩掉令牌
        if token.eq_ignore_ascii_case("bearer") || token.eq_ignore_ascii_case("basic") {
            pending = Some(PendingMask::Whole);
            out.push(token.to_string());
            continue;
        }
        if let Some(kind) = value_flag_kind(token) {
            pending = Some(kind);
            out.push(token.to_string());
            continue;
        }
        out.push(mask_token(token));
    }
    out.join(" ")
}

/// 待掩码的取值类型：整体掩掉，或只掩 `user:pass` 的密码段（保留用户名可读）
#[derive(Clone, Copy)]
enum PendingMask {
    Whole,
    UserInfo,
}

/// 判断一个 token 是否是"由后续 token 提供取值"的敏感开关。
///
/// 只认**纯开关名**（`-`/`--` 开头、不含 `=`、名字是短标识符），避免把路径或
/// 值本身误判成开关。`-p`/`-u` 这类短开关在真实命令里歧义大（`-p 8080` 是端口），
/// 因此只有 `user` 系列走"只掩密码段"，其余短开关不参与判断——宁可漏掉个别写法，
/// 也不要把每条 docker 命令的端口号都打成星号。
fn value_flag_kind(token: &str) -> Option<PendingMask> {
    if !token.starts_with('-') || token.contains('=') { return None; }
    let name = token.trim_start_matches('-').to_ascii_lowercase();
    if name.is_empty() || name.len() > 24 { return None; }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { return None; }
    if matches!(name.as_str(), "u" | "user" | "username") {
        return Some(PendingMask::UserInfo);
    }
    SENSITIVE_NAME_HINTS
        .iter()
        .any(|s| name.contains(s))
        .then_some(PendingMask::Whole)
}

const SENSITIVE_NAME_HINTS: &[&str] = &[
    "key", "token", "secret", "password", "passwd", "pwd", "auth", "credential",
];

/// 值里可能带查询串（`?token=t&a=1`）：只掩掉首个参数，保留 `&` 之后的其余部分
fn query_tail(value: &str) -> &str {
    match value.find('&') {
        Some(i) => &value[i..],
        None => "",
    }
}

/// `user:pass` → `user:***`（保留用户名便于排障）；无冒号则整体掩掉
fn mask_user_info(token: &str) -> String {
    match token.find(':') {
        Some(i) => format!("{}:***", &token[..i]),
        None => "***".to_string(),
    }
}

/// `scheme://user:pass@host/...` → `scheme://user:***@host/...`；无用户信息时返回 None
fn mask_url_userinfo(token: &str) -> Option<String> {
    let scheme_end = token.find("://")?;
    let after = &token[scheme_end + 3..];
    let at = after.find('@')?;
    let userinfo = &after[..at];
    let colon = userinfo.find(':')?;
    Some(format!(
        "{}://{}:***@{}",
        &token[..scheme_end],
        &userinfo[..colon],
        &after[at + 1..]
    ))
}

fn mask_token(token: &str) -> String {
    // URL 用户信息优先：一个 token 里可能同时含 `://` 与 `=`（查询串）
    if let Some(masked) = mask_url_userinfo(token) {
        return masked;
    }
    let Some(eq) = token.find('=') else { return token.to_string() };
    let (name_part, rest) = token.split_at(eq);
    let value = &rest[1..];
    if value.is_empty() {
        return token.to_string();
    }
    let name = name_part
        .trim_start_matches('-')
        .trim_start_matches('/')
        .to_ascii_lowercase();
    // `-Dapi.key` 这种单字母前缀：剥掉 `d` 后再判断
    let name = if name_part.starts_with("-D") { &name[1.min(name.len())..] } else { name.as_str() };
    if !SENSITIVE_NAME_HINTS.iter().any(|s| name.contains(s)) {
        return token.to_string();
    }
    format!("{}=***{}", name_part, query_tail(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── mask_command（日志脱敏）────────────────────────────

    #[test]
    fn test_mask_command_masks_credentials() {
        // -D 形式的 JVM 参数
        assert_eq!(
            mask_command("java -Dapi.key=abc123 -jar app.jar"),
            "java -Dapi.key=*** -jar app.jar"
        );
        // 长选项形式；非敏感参数不受影响
        assert_eq!(
            mask_command("app --token=xyz --port=8080"),
            "app --token=*** --port=8080"
        );
        // dotenv 形式
        assert_eq!(
            mask_command("NODE_ENV=production DB_PASSWORD=sekret"),
            "NODE_ENV=production DB_PASSWORD=***"
        );
        // URL 查询串：只掩掉该参数，保留后续参数
        assert_eq!(
            mask_command("curl http://x/?token=t&a=1"),
            "curl http://x/?token=***&a=1"
        );
        // 无等号 / 空值原样保留
        assert_eq!(mask_command("npm run build"), "npm run build");
        assert_eq!(mask_command("app --token="), "app --token=");
    }

    /// 回归：空格分隔取值的写法（旧实现只处理含 `=` 的 token，这一类完全漏掉）
    #[test]
    fn test_mask_command_masks_space_separated_flag_values() {
        assert_eq!(mask_command("app --token abc123"), "app --token ***");
        assert_eq!(mask_command("app --password Secret value"), "app --password *** value");
        assert_eq!(mask_command("app --api-key K1"), "app --api-key ***");
        assert_eq!(mask_command("app --secret S1 --port 8080"), "app --secret *** --port 8080");
        // 非敏感短开关不能被误掩（`-p 8080` 是端口，掩掉会让日志失去排障价值）
        assert_eq!(mask_command("docker run -p 8080:80 img"), "docker run -p 8080:80 img");
    }

    /// 回归：`Bearer`/`Basic` 令牌与 URL 用户信息
    #[test]
    fn test_mask_command_masks_bearer_and_url_userinfo() {
        assert_eq!(
            mask_command("curl -H Authorization: Bearer eyJhbGciOi"),
            "curl -H Authorization: Bearer ***"
        );
        assert_eq!(mask_command("mysql://user:pw@host/db"), "mysql://user:***@host/db");
        // `-u user:pass` 只掩密码段，保留用户名（便于排障）
        assert_eq!(mask_command("git clone -u alice:s3cr3t repo"), "git clone -u alice:*** repo");
        // 无密码段的 URL 不动
        assert_eq!(mask_command("http://host/path"), "http://host/path");
    }
}
