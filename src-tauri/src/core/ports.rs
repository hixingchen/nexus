//! 监听端口枚举（只读）。
//!
//! 用途：服务卡片的「实际监听端口」与「打开地址」。不依赖进程命令行解析，
//! 直接用 Windows 的 TCP 表拿到 `(pid, 本地端口)`，由调用方按服务的进程树汇总。
//!
//! 只查 **LISTEN** 状态的 TCP 端口：客户端连接（本机调试器连出去的）不算服务在监听。
//! UDP 第一版不做（见 `docs/feature-analysis-health-filetree-logs.md`）。

use std::collections::{BTreeSet, HashMap};

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
}
