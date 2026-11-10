//! 监听端口枚举（只读）。
//!
//! 用途：服务卡片的「实际监听端口」与「打开地址」。不依赖进程命令行解析，
//! 直接用 Windows 的 TCP 表拿到 `(pid, 本地端口)`，由调用方按服务的进程树汇总。
//!
//! 只查 **LISTEN** 状态的 TCP 端口：客户端连接（本机调试器连出去的）不算服务在监听。
//! UDP 第一版不做（见 `docs/feature-analysis-health-filetree-logs.md`）。

use std::collections::{BTreeSet, HashMap};
use serde::Serialize;

/// 占用某个端口的进程（给「端口冲突」提示用）。
#[derive(Debug, Clone, Serialize)]
pub struct PortOccupant {
    pub pid: u32,
    pub name: String,
    /// 可执行文件完整路径；无权限/已退出时为 None（界面据此决定能否「打开目录」）
    pub path: Option<String>,
    /// 是否是 Nexus 自己：界面禁止把 Nexus 自己结束掉
    pub is_self: bool,
}

/// 一个端口冲突：端口 + 当前占用它的进程。
#[derive(Debug, Clone, Serialize)]
pub struct PortConflict {
    pub port: u16,
    pub occupants: Vec<PortOccupant>,
}

/// 查某个端口当前被哪些进程监听。
///
/// 与 `listening_ports_by_pid` 互为反表；进程名/路径通过一次进程快照补齐。
/// 空 = 没有进程监听该端口（可安全启动）。
/// 反查表的可复用版本：项目批量启动时一轮只取一次端口表与进程快照。
pub(crate) fn occupants_of_with(
    port: u16,
    by_pid: &HashMap<u32, Vec<u16>>,
    snapshot: &[super::winproc::ProcEntry],
) -> Vec<PortOccupant> {
    let mut pids: Vec<u32> = by_pid
        .iter()
        .filter(|(_, ports)| ports.contains(&port))
        .map(|(pid, _)| *pid)
        .collect();
    pids.sort_unstable();
    pids.dedup();
    let self_pid = std::process::id();
    pids.into_iter()
        .map(|pid| PortOccupant {
            pid,
            name: super::winproc::name_of(pid, snapshot),
            path: super::winproc::process_image_path(pid),
            is_self: pid == self_pid,
        })
        .collect()
}

/// 端口被占用时返回冲突详情；空闲返回 None。
pub(crate) fn conflict_of(port: u16) -> Option<PortConflict> {
    conflict_of_with(port, &listening_ports_by_pid(), &super::winproc::snapshot_processes())
}

/// 反查表的可复用版本（见 `occupants_of_with`）。
pub(crate) fn conflict_of_with(
    port: u16,
    by_pid: &HashMap<u32, Vec<u16>>,
    snapshot: &[super::winproc::ProcEntry],
) -> Option<PortConflict> {
    let occupants = occupants_of_with(port, by_pid, snapshot);
    if occupants.is_empty() {
        None
    } else {
        Some(PortConflict { port, occupants })
    }
}

/// 从日志尾部识别「端口被占用」并提取端口号（纯函数，便于单测）。
///
/// 覆盖常见服务的报错形态：Node（`EADDRINUSE ... :::3000`）、Spring Boot
/// （`Port 8080 was already in use`）、Go（`listen tcp 127.0.0.1:8080: bind: ...`）、
/// Java/Tomcat（`Address already in use`）、Windows/Python（`10048`）、中文（`端口 8080 已被占用`）。
///
/// 只从**确实像绑定失败**的行里取端口：Java 的 `Address already in use: bind` 不带端口，
/// 这时返回 None（调用方不能凭空猜一个端口出来把别的进程标成占用）。
pub(crate) fn port_from_bind_error(lines: &[&str]) -> Option<u16> {
    for line in lines.iter().rev() {
        if !is_bind_error_line(line) {
            continue;
        }
        if let Some(port) = extract_port(line) {
            return Some(port);
        }
    }
    None
}

/// 这些行里是否出现过绑定失败（即使没有端口号，也能据此回落到"上次监听端口"）。
pub(crate) fn has_bind_error(lines: &[&str]) -> bool {
    lines.iter().any(|line| is_bind_error_line(line))
}

fn is_bind_error_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("eaddrinuse")
        || lower.contains("address already in use")
        || lower.contains("only one usage of each socket address")
        || lower.contains("10048")
        || lower.contains("was already in use")
        || (lower.contains("端口") && (line.contains("占用") || lower.contains("in use")))
        || (lower.contains("bind") && lower.contains("in use"))
}

/// 从一行日志里取端口：优先 `port 8080` / `端口 8080`，其次常见主机写法，最后是
/// 前面不是数字的 `:8080`（避免把 `12:34:56` 的时间戳当成端口）。
fn extract_port(line: &str) -> Option<u16> {
    let lower = line.to_ascii_lowercase();
    // 1) 显式关键字：Port 8080 / 端口 8080
    for kw in ["port", "端口"] {
        let mut from = 0;
        while let Some(idx) = lower[from..].find(kw) {
            let start = from + idx;
            // "support" 之类不应命中：关键字前必须是边界
            let boundary = start == 0 || !lower.as_bytes()[start - 1].is_ascii_alphanumeric();
            let after = &line[start + kw.len()..];
            let after = after.trim_start_matches(|c: char| c.is_whitespace() || c == ':' || c == '=' || c == '：');
            let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
            if boundary && !digits.is_empty() {
                if let Ok(p) = digits.parse::<u16>() {
                    if p > 0 { return Some(p); }
                }
            }
            from = start + kw.len();
        }
    }
    // 2) 常见主机前缀：localhost:8080 / 127.0.0.1:8080 / 0.0.0.0:8080 /
    //    [::1]:8080 / [::]:8080 / :::8080（Node 的 "address already in use :::3000"）
    for prefix in ["localhost:", "127.0.0.1:", "0.0.0.0:", "[::1]:", "[::]:", ":::"] {
        let mut from = 0;
        while let Some(idx) = lower[from..].find(prefix) {
            let after = &line[from + idx + prefix.len()..];
            let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(p) = digits.parse::<u16>() {
                if p > 0 { return Some(p); }
            }
            from += idx + prefix.len();
        }
    }
    // 3) 通用 `:8080`：冒号前不能是数字（排除 12:34:56）；
    //    允许前面是字母/右括号/冒号（如 `tcp :8080`、`[::1]:8080`）
    let chars: Vec<char> = line.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c != ':' || (i > 0 && chars[i - 1].is_ascii_digit()) {
            continue;
        }
        let mut digits = String::new();
        let mut j = i + 1;
        while j < chars.len() && chars[j].is_ascii_digit() && digits.len() < 5 {
            digits.push(chars[j]);
            j += 1;
        }
        if digits.len() >= 2 {
            if let Ok(p) = digits.parse::<u16>() {
                if p > 0 { return Some(p); }
            }
        }
    }
    None
}

/// 汇总一组进程的监听端口：去重、升序。
///
/// 抽成纯函数便于测试，也保证「一个服务多个端口」与「多个进程监听同一端口」不会产生重复项。
pub(crate) fn service_ports(pids: &[u32], by_pid: &HashMap<u32, Vec<u16>>) -> Vec<u16> {
    let mut out = BTreeSet::new();
    for pid in pids {
        if let Some(ports) = by_pid.get(pid) {
            out.extend(ports.iter().copied());
        }
    }
    out.into_iter().collect()
}

#[cfg(target_os = "windows")]
pub(crate) fn listening_ports_by_pid() -> HashMap<u32, Vec<u16>> {
    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6TABLE_OWNER_PID, MIB_TCP6ROW_OWNER_PID,
        MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
    };
    use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};

    /// 取一次某地址族的监听表。缓冲区用 `Vec<u32>` 分配：表项按 4 字节对齐，
    /// 用 `Vec<u8>` 再强转结构体指针属于未对齐访问。
    unsafe fn query_table(family: u32) -> Option<Vec<u32>> {
        let mut size: u32 = 0;
        let ret = GetExtendedTcpTable(
            std::ptr::null_mut(),
            &mut size,
            0,
            family,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        );
        if ret != ERROR_INSUFFICIENT_BUFFER || size == 0 {
            log::warn!("[nexus] 查询 TCP 监听表大小失败（family={}，错误 {}）", family, ret);
            return None;
        }
        let words = (size as usize).div_ceil(4);
        let mut buf = vec![0u32; words];
        let ret = GetExtendedTcpTable(
            buf.as_mut_ptr() as *mut core::ffi::c_void,
            &mut size,
            0,
            family,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        );
        if ret != NO_ERROR {
            log::warn!("[nexus] 读取 TCP 监听表失败（family={}，错误 {}）", family, ret);
            return None;
        }
        Some(buf)
    }

    unsafe fn add_ipv4(out: &mut HashMap<u32, Vec<u16>>, buf: &[u32]) {
        let table = &*(buf.as_ptr() as *const MIB_TCPTABLE_OWNER_PID);
        let count = table.dwNumEntries as usize;
        if count == 0 {
            return;
        }
        let rows: &[MIB_TCPROW_OWNER_PID] = std::slice::from_raw_parts(table.table.as_ptr(), count);
        for row in rows {
            // 端口在 DWORD 的低 16 位，网络字节序
            out.entry(row.dwOwningPid)
                .or_default()
                .push(u16::from_be(row.dwLocalPort as u16));
        }
    }

    unsafe fn add_ipv6(out: &mut HashMap<u32, Vec<u16>>, buf: &[u32]) {
        let table = &*(buf.as_ptr() as *const MIB_TCP6TABLE_OWNER_PID);
        let count = table.dwNumEntries as usize;
        if count == 0 {
            return;
        }
        let rows: &[MIB_TCP6ROW_OWNER_PID] = std::slice::from_raw_parts(table.table.as_ptr(), count);
        for row in rows {
            out.entry(row.dwOwningPid)
                .or_default()
                .push(u16::from_be(row.dwLocalPort as u16));
        }
    }

    let mut out: HashMap<u32, Vec<u16>> = HashMap::new();
    unsafe {
        if let Some(buf) = query_table(AF_INET as u32) {
            add_ipv4(&mut out, &buf);
        }
        if let Some(buf) = query_table(AF_INET6 as u32) {
            add_ipv6(&mut out, &buf);
        }
    }
    for ports in out.values_mut() {
        ports.sort_unstable();
        ports.dedup();
    }
    out
}

/// 非 Windows：应用只打包 Windows，这里保留空实现让 `cargo check` 通过。
#[cfg(not(target_os = "windows"))]
pub(crate) fn listening_ports_by_pid() -> HashMap<u32, Vec<u16>> {
    HashMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_ports_unions_and_dedups() {
        let by_pid = HashMap::from([
            (10, vec![8080u16, 5005]),
            (11, vec![8080, 3000]),
        ]);
        assert_eq!(service_ports(&[10, 11], &by_pid), vec![3000, 5005, 8080]);
        assert_eq!(service_ports(&[99], &by_pid), Vec::<u16>::new());
        assert_eq!(service_ports(&[], &by_pid), Vec::<u16>::new());
    }

    /// 绑定失败日志 → 端口。守的是"失败卡片上能直接说出哪个端口冲突"：
    /// 解析不出来就只剩一句"启动失败"，用户还得自己去翻日志找端口。
    #[test]
    fn test_port_from_bind_error_common_formats() {
        assert_eq!(
            port_from_bind_error(&["Error: listen EADDRINUSE: address already in use :::3000"]),
            Some(3000)
        );
        assert_eq!(
            port_from_bind_error(&["Web server failed to start. Port 8080 was already in use."]),
            Some(8080)
        );
        assert_eq!(
            port_from_bind_error(&["listen tcp 127.0.0.1:10001: bind: Only one usage of each socket address"]),
            Some(10001)
        );
        assert_eq!(
            port_from_bind_error(&["[WinError 10048] 通常每个套接字地址(协议/网络地址/端口)只允许使用一次"]),
            None,
            "没有端口号时不要瞎猜"
        );
        assert_eq!(
            port_from_bind_error(&["java.net.BindException: Address already in use: bind"]),
            None,
            "Java 的这条不带端口"
        );
        assert_eq!(
            port_from_bind_error(&["端口 9000 已被占用"]),
            Some(9000)
        );
    }

    /// 真实监听一个临时端口：反查表必须能报出自己（PID/进程名/自身标记）。
    ///
    /// 这条走真实的 Windows TCP 表与进程快照——端口反查一旦失效，整个冲突提示就是空的，
    /// 而纯解析测试看不出来。
    #[cfg(target_os = "windows")]
    #[test]
    fn test_occupants_of_finds_current_process_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定临时端口");
        let port = listener.local_addr().expect("本地地址").port();
        let occupants = conflict_of(port).expect("端口应被占用").occupants;
        assert!(
            occupants.iter().any(|o| o.pid == std::process::id() && o.is_self),
            "端口 {} 应能查到当前测试进程，实际 {:?}",
            port,
            occupants.iter().map(|o| (o.pid, &o.name)).collect::<Vec<_>>()
        );
        assert!(!occupants[0].name.is_empty(), "进程名不能是空串");
        drop(listener);
        let mut freed = false;
        for _ in 0..20 {
            if conflict_of(port).is_none() {
                freed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(freed, "监听关闭后端口应回到空闲");
    }

    #[test]
    fn test_port_from_bind_error_ignores_timestamps_and_non_bind_lines() {
        // 时间戳不是端口
        assert_eq!(
            port_from_bind_error(&["12:34:56 服务启动失败: Address already in use"]),
            None
        );
        // 没提到绑定失败的行即使带端口也不认
        assert_eq!(
            port_from_bind_error(&["Server started on port 8080"]),
            None
        );
        // 从新到旧找：最后一条绑定失败优先
        assert_eq!(
            port_from_bind_error(&[
                "EADDRINUSE: address already in use :::3000",
                "EADDRINUSE: address already in use :::4000",
            ]),
            Some(4000)
        );
    }
}
