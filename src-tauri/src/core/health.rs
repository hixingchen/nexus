//! 服务就绪检查（开机启动与本地服务共用一份定义 / 校验 / 探测）。
//!
//! 背景：这份逻辑原先只在开机启动器里（`core::boot` 的 `BootCheck` + runner 的
//! `wait_ready`）。本地服务只有“进程存活超过 1 秒 = ready”，启动慢或端口未起来
//! 只能靠日志猜。抽到这里之后，本地服务与开机启动用同一套 JSON 形状与探测方式，
//! 保存入口、manifest 生成与运行期判定不会再各写一份。
//!
//! 设计约束与 boot 版一致：只支持 四种固定形状（不执行任意正则 / 任意命令），
//! 避免把“就绪检查配置”变成第二个命令执行面。

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// 服务启动后如何算“就绪”。空串（DB 里）= `None`：进程存活即视为就绪。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HealthCheck {
    #[default]
    None,
    Delay {
        seconds: u64,
    },
    Tcp {
        host: String,
        port: u16,
        #[serde(rename = "timeoutSecs", default = "default_timeout_secs")]
        timeout_secs: u64,
    },
    LogContains {
        pattern: String,
        #[serde(rename = "timeoutSecs", default = "default_timeout_secs")]
        timeout_secs: u64,
    },
}

fn default_timeout_secs() -> u64 {
    60
}

/// 校验并归一化 DB 里的 `health_check` / `boot_check` 文本。
///
/// 空串 = 未配置；其余必须是合法 JSON 且参数在合理范围。返回规范化后的 JSON，
/// 这样保存入口、manifest 生成与运行期解析共用同一份判据。
pub fn normalize(raw: &str) -> Result<String, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Ok(String::new());
    }
    let check: HealthCheck = serde_json::from_str(text)
        .map_err(|e| format!("就绪检查不是合法 JSON: {}", e))?;
    match &check {
        HealthCheck::None => Ok(String::new()),
        HealthCheck::Delay { seconds } => {
            if *seconds > 3600 {
                return Err("就绪检查的等待时间不能超过 3600 秒".into());
            }
            serde_json::to_string(&check).map_err(|e| e.to_string())
        }
        HealthCheck::Tcp { host, port, timeout_secs } => {
            if host.trim().is_empty() {
                return Err("就绪检查的 TCP host 不能为空".into());
            }
            if *port == 0 {
                return Err("就绪检查的 TCP 端口必须在 1-65535 之间".into());
            }
            if *timeout_secs == 0 || *timeout_secs > 3600 {
                return Err("就绪检查的超时必须在 1-3600 秒之间".into());
            }
            serde_json::to_string(&check).map_err(|e| e.to_string())
        }
        HealthCheck::LogContains { pattern, timeout_secs } => {
            if pattern.trim().is_empty() {
                return Err("就绪检查的日志关键字不能为空".into());
            }
            if pattern.len() > 512 {
                return Err("就绪检查的日志关键字不能超过 512 字节".into());
            }
            if *timeout_secs == 0 || *timeout_secs > 3600 {
                return Err("就绪检查的超时必须在 1-3600 秒之间".into());
            }
            serde_json::to_string(&check).map_err(|e| e.to_string())
        }
    }
}

/// 解析已保存的检查配置；空串 = None。
pub(crate) fn parse(raw: &str) -> Result<HealthCheck, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Ok(HealthCheck::None);
    }
    serde_json::from_str(text).map_err(|e| format!("就绪检查解析失败: {}", e))
}

/// 等待上限（Delay 用自身秒数；None 没有等待）。
pub fn timeout(check: &HealthCheck) -> Option<Duration> {
    match check {
        HealthCheck::None => None,
        HealthCheck::Delay { seconds } => Some(Duration::from_secs(*seconds)),
        HealthCheck::Tcp { timeout_secs, .. } => Some(Duration::from_secs(*timeout_secs)),
        HealthCheck::LogContains { timeout_secs, .. } => Some(Duration::from_secs(*timeout_secs)),
    }
}

/// 给人看的检查说明（服务卡片 tooltip / 健康弹层 / 超时报错共用）。
pub fn describe(check: &HealthCheck) -> String {
    match check {
        HealthCheck::None => "进程存活即可".into(),
        HealthCheck::Delay { seconds } => format!("等待 {} 秒", seconds),
        HealthCheck::Tcp { host, port, .. } => format!("TCP {}:{}", host, port),
        HealthCheck::LogContains { pattern, .. } => {
            // 关键字可能很长：弹层里只展示前 60 个字符，完整值仍在 DB 里
            let short: String = pattern.chars().take(60).collect();
            if short.len() < pattern.len() { format!("日志包含「{}…」", short) } else { format!("日志包含「{}」", short) }
        }
    }
}

/// 计算运行阶段（纯函数，便于单测）。
///
/// `grace_ready` 是未配置检查时的"进程存活超过宽限期"；`tcp_ok` / `log_ok`
/// 只在对应检查分支里被调用（懒探测，不会为 TCP 检查去扫日志）。
pub fn evaluate_phase(
    check: &HealthCheck,
    elapsed_ms: u64,
    grace_ready: bool,
    tcp_ok: impl FnOnce() -> bool,
    log_ok: impl FnOnce() -> bool,
) -> &'static str {
    match check {
        HealthCheck::None => if grace_ready { "ready" } else { "starting" },
        HealthCheck::Delay { seconds } => {
            if elapsed_ms >= seconds.saturating_mul(1000) { "ready" } else { "starting" }
        }
        HealthCheck::Tcp { timeout_secs, .. } => {
            if tcp_ok() {
                "ready"
            } else if elapsed_ms >= timeout_secs.saturating_mul(1000) {
                "unready"
            } else {
                "starting"
            }
        }
        HealthCheck::LogContains { timeout_secs, .. } => {
            if log_ok() {
                "ready"
            } else if elapsed_ms >= timeout_secs.saturating_mul(1000) {
                "unready"
            } else {
                "starting"
            }
        }
    }
}

/// TCP 可连接性探测（可指定连接超时，避免运行期轮询被慢地址拖住）。
pub fn tcp_ready_with_timeout(host: &str, port: u16, timeout: Duration) -> bool {
    use std::net::ToSocketAddrs;
    let addrs = match (host, port).to_socket_addrs() {
        Ok(a) => a,
        Err(_) => return false,
    };
    for addr in addrs {
        if std::net::TcpStream::connect_timeout(&addr, timeout).is_ok() {
            return true;
        }
    }
    false
}

/// 默认 2 秒连接超时（开机启动器的节奏：250ms 轮询 + 2s 连接超时）。
pub fn tcp_ready(host: &str, port: u16) -> bool {
    tcp_ready_with_timeout(host, port, Duration::from_secs(2))
}

/// 日志尾部是否包含关键字（只看尾部 1MB；UTF-8 → GB18030 回退，中文 Windows 常见 GBK）。
///
/// 为什么必须带起点：服务日志文件是追加写的，上一次运行的 "ready" 关键字还留在文件里；
/// 不排除历史内容的话，服务刚启动就会被判定"已就绪"（实测踩到）。
/// `start_offset` 超过当前文件长度 = 文件被截断/轮转，按从头扫描处理。
pub fn log_contains_since(path: &str, pattern: &str, start_offset: u64) -> bool {
    let Ok(bytes) = std::fs::read(Path::new(path)) else { return false };
    let start = if start_offset > bytes.len() as u64 {
        0
    } else {
        (start_offset as usize).max(bytes.len().saturating_sub(1024 * 1024))
    };
    let text = crate::core::textenc::decode_to_string(&bytes[start..]);
    text.contains(pattern)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timeout_and_describe() {
        assert!(timeout(&HealthCheck::None).is_none());
        assert_eq!(timeout(&HealthCheck::Delay { seconds: 5 }), Some(Duration::from_secs(5)));
        assert_eq!(
            timeout(&HealthCheck::Tcp { host: "127.0.0.1".into(), port: 3306, timeout_secs: 60 }),
            Some(Duration::from_secs(60))
        );
        assert_eq!(describe(&HealthCheck::None), "进程存活即可");
        assert_eq!(describe(&HealthCheck::Delay { seconds: 5 }), "等待 5 秒");
        assert_eq!(
            describe(&HealthCheck::Tcp { host: "127.0.0.1".into(), port: 3306, timeout_secs: 60 }),
            "TCP 127.0.0.1:3306"
        );
        assert!(describe(&HealthCheck::LogContains {
            pattern: "ready for connections".into(), timeout_secs: 60,
        }).contains("ready for connections"));
    }

    #[test]
    fn test_parse_roundtrip_with_normalize() {
        let raw = r#"{"type":"tcp","host":"127.0.0.1","port":3306,"timeoutSecs":30}"#;
        let normalized = normalize(raw).expect("合法检查");
        let check = parse(&normalized).expect("规范化后仍可解析");
        assert_eq!(check, HealthCheck::Tcp { host: "127.0.0.1".into(), port: 3306, timeout_secs: 30 });
        assert_eq!(parse("").unwrap(), HealthCheck::None);
    }

    #[test]
    fn test_evaluate_phase_rules() {
        // 未配置：走原有 1 秒宽限期
        assert_eq!(evaluate_phase(&HealthCheck::None, 100, false, || panic!(), || panic!()), "starting");
        assert_eq!(evaluate_phase(&HealthCheck::None, 1_100, true, || panic!(), || panic!()), "ready");

        // 固定等待：到点前 starting，到点 ready
        let delay = HealthCheck::Delay { seconds: 5 };
        assert_eq!(evaluate_phase(&delay, 4_999, false, || panic!(), || panic!()), "starting");
        assert_eq!(evaluate_phase(&delay, 5_000, false, || panic!(), || panic!()), "ready");

        // TCP：连上 ready；超时前 starting；超时后 unready
        let tcp = HealthCheck::Tcp { host: "127.0.0.1".into(), port: 3306, timeout_secs: 10 };
        assert_eq!(evaluate_phase(&tcp, 100, false, || true, || panic!()), "ready");
        assert_eq!(evaluate_phase(&tcp, 9_999, false, || false, || panic!()), "starting");
        assert_eq!(evaluate_phase(&tcp, 10_000, false, || false, || panic!()), "unready");

        // 日志关键字：出现即 ready；超时后 unready
        let log = HealthCheck::LogContains { pattern: "ready".into(), timeout_secs: 10 };
        assert_eq!(evaluate_phase(&log, 100, false, || panic!(), || true), "ready");
        assert_eq!(evaluate_phase(&log, 100, false, || panic!(), || false), "starting");
        assert_eq!(evaluate_phase(&log, 10_000, false, || panic!(), || false), "unready");
    }

    /// 日志检查只看**本次运行**新增的内容：旧文件里的关键字不能让它"秒就绪"。
    #[test]
    fn test_log_contains_since_ignores_old_content() {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!("nexus-health-log-{}.log", std::process::id()));
        std::fs::write(&path, "previous run: ready for connections\n").expect("写旧日志");
        let offset = std::fs::metadata(&path).expect("meta").len();
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).expect("打开日志");
        writeln!(f, "still starting").expect("追加");
        f.flush().expect("flush");
        let p = path.to_string_lossy().to_string();
        assert!(!log_contains_since(&p, "ready for connections", offset), "旧内容不能命中");
        assert!(log_contains_since(&p, "still starting", offset));
        writeln!(f, "ready for connections now").expect("追加");
        f.flush().expect("flush");
        assert!(log_contains_since(&p, "ready for connections now", offset));
        let _ = std::fs::remove_file(&path);
    }

    /// 真实监听：TCP 检查必须能连上；关掉后不再就绪。
    #[test]
    fn test_tcp_ready_with_timeout_real_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定临时端口");
        let port = listener.local_addr().expect("本地地址").port();
        assert!(tcp_ready_with_timeout("127.0.0.1", port, Duration::from_millis(500)));
        drop(listener);
        let mut freed = false;
        for _ in 0..20 {
            if !tcp_ready_with_timeout("127.0.0.1", port, Duration::from_millis(100)) {
                freed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(freed, "关闭监听后 TCP 检查应转为不就绪");
    }
}
