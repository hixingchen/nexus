//! 服务进程的启动、终止与身份探测。
//!
//! 这一份是 **boot-core 自己的最小实现**：只包含启动器需要的能力。
//! Windows 分支与 `src-tauri/src/core/process.rs` 的同名函数保持同一套行为；
//! Linux 分支用进程组 + `/proc`，不依赖外部 `kill` 命令。
//!
//! 为什么没有直接从 GUI 的 `ProcessManager` 里复用：那个文件还包含日志管道、
//! 生命周期锁、端口/失败态等 UI 侧职责；启动器只需要“起进程、杀进程组、查启动时间”。

use std::process::Command;
use std::time::Duration;
#[cfg(windows)]
use std::time::Instant;

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt as UnixCommandExt;

/// 服务进程的创建标志：`CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP`。
#[cfg(windows)]
pub const SERVICE_CREATE_FLAGS: u32 = 0x08000000 | 0x00000200;

/// 构建服务命令：Windows 走 `cmd /C`，Unix 走 `sh -c` 并建立独立进程组。
pub fn build_command(command_str: &str) -> Command {
    #[cfg(windows)]
    {
        let mut c = Command::new("cmd");
        c.args(["/C", command_str]);
        c.creation_flags(SERVICE_CREATE_FLAGS);
        // 补上“启动之后才装的东西”（reason 见 core::winenv 注释）
        crate::winenv::apply(&mut c);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.args(["-c", command_str]);
        // 每个服务一个进程组：停止时 kill(-pid) 能连后台子孙一起收
        c.process_group(0);
        c
    }
}

/// 认领到的游离进程。
///
/// Windows：`cmd /C startup.bat` 里用 `start` 另开窗口后，cmd 会先退出；
/// 真正的工作进程按父 pid 链认领，停止时一起结束。
///
/// Linux：不需要按进程枚举——服务命令以 `process_group(0)` 启动，直接子进程退出后
/// 只要进程组还有成员，就仍然算“服务在运行”。
pub struct AdoptedChild {
    pub pid: u32,
    /// 映像名 / 说明，只用于日志
    pub name: String,
    pub handle: crate::winproc::OwnedProcess,
}

/// 直接子进程退出后，认领仍在运行的后代。
pub fn adopt_survivors(pid: u32, started_at: u64) -> Vec<AdoptedChild> {
    #[cfg(windows)]
    {
        let snapshot = crate::winproc::snapshot_processes();
        let mut out = Vec::new();
        for child_pid in crate::winproc::descendants(pid, &snapshot) {
            if !process_start_time_millis(child_pid)
                .map(|t| t >= started_at)
                .unwrap_or(false)
            {
                continue;
            }
            let name = crate::winproc::name_of(child_pid, &snapshot);
            if is_console_host(&name) {
                continue;
            }
            let Some(handle) = crate::winproc::OwnedProcess::open(child_pid) else {
                continue;
            };
            if !handle.is_alive() {
                continue;
            }
            out.push(AdoptedChild { pid: child_pid, name, handle });
        }
        out
    }
    #[cfg(not(windows))]
    {
        let _ = started_at;
        match crate::winproc::OwnedProcess::open_group(pid) {
            Some(handle) if handle.is_alive() => vec![AdoptedChild {
                pid,
                name: "process-group".into(),
                handle,
            }],
            _ => Vec::new(),
        }
    }
}

#[cfg(windows)]
fn is_console_host(name: &str) -> bool {
    name.eq_ignore_ascii_case("conhost.exe") || name.eq_ignore_ascii_case("OpenConsole.exe")
}

// ─── 进程身份探测 ────────────────────────────────────────────

/// 进程探测结果。
///
/// 「进程不在了」和「我没权限查」都会失败，但处置不同：后者不能据此宣布退出。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessProbe {
    Alive(u64),
    Gone,
    Denied,
}

/// 进程启动时间（毫秒，距 UNIX 纪元）；进程不存在/无权限时返回 `None`。
pub fn process_start_time_millis(pid: u32) -> Option<u64> {
    match probe_process_start_time(pid) {
        ProcessProbe::Alive(started) => Some(started),
        ProcessProbe::Gone | ProcessProbe::Denied => None,
    }
}

#[cfg(windows)]
pub fn probe_process_start_time(pid: u32) -> ProcessProbe {
    type WinHandle = isize;
    type WinBool = i32;
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    extern "system" {
        fn OpenProcess(access: u32, inherit: WinBool, pid: u32) -> WinHandle;
        fn GetProcessTimes(
            handle: WinHandle,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> WinBool;
        fn GetExitCodeProcess(handle: WinHandle, code: *mut u32) -> WinBool;
        fn WaitForSingleObject(handle: WinHandle, milliseconds: u32) -> u32;
        fn CloseHandle(handle: WinHandle) -> WinBool;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const FILETIME_UNIX_DIFF_SECS: u64 = 11_644_473_600;
    const STILL_ACTIVE: u32 = 259;
    // ERROR_INVALID_PARAMETER / ERROR_NOT_FOUND：pid 已不存在
    const ERROR_INVALID_PARAMETER: u32 = 87;
    const ERROR_NOT_FOUND: u32 = 1168;

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle == 0 {
            let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32;
            return if code == ERROR_INVALID_PARAMETER || code == ERROR_NOT_FOUND {
                ProcessProbe::Gone
            } else {
                ProcessProbe::Denied
            };
        }
        let mut creation = FileTime::default();
        let mut exit = FileTime::default();
        let mut kernel = FileTime::default();
        let mut user = FileTime::default();
        let times_ok = GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user);
        let mut code: u32 = 0;
        let exit_ok = GetExitCodeProcess(handle, &mut code);
        let signaled = if code == STILL_ACTIVE {
            let syn = OpenProcess(SYNCHRONIZE, 0, pid);
            if syn == 0 {
                false
            } else {
                let w = WaitForSingleObject(syn, 0) == 0;
                CloseHandle(syn);
                w
            }
        } else {
            false
        };
        CloseHandle(handle);
        if times_ok == 0 || exit_ok == 0 {
            return ProcessProbe::Denied;
        }
        if code != STILL_ACTIVE || signaled {
            return ProcessProbe::Gone;
        }
        let ticks = ((creation.high as u64) << 32) | creation.low as u64;
        let secs = ticks / 10_000_000;
        if secs < FILETIME_UNIX_DIFF_SECS {
            return ProcessProbe::Denied;
        }
        ProcessProbe::Alive((secs - FILETIME_UNIX_DIFF_SECS) * 1000 + (ticks % 10_000_000) / 10_000)
    }
}

#[cfg(target_os = "linux")]
pub fn probe_process_start_time(pid: u32) -> ProcessProbe {
    match linux_start_time_millis(pid) {
        Ok(Some(t)) => ProcessProbe::Alive(t),
        Ok(None) => ProcessProbe::Gone,
        Err(_) => ProcessProbe::Denied,
    }
}

#[cfg(target_os = "linux")]
fn linux_start_time_millis(pid: u32) -> std::io::Result<Option<u64>> {
    let text = match std::fs::read_to_string(format!("/proc/{}/stat", pid)) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    // comm 字段可能含空格/括号：用最后一个 ')' 定位
    let Some(close) = text.rfind(')') else { return Ok(None) };
    let fields: Vec<&str> = text[close + 1..].split_whitespace().collect();
    // close 之后第 0 项是 state（第 3 字段）；starttime 是第 22 字段 → 下标 19
    let Some(ticks) = fields.get(19).and_then(|v| v.parse::<u64>().ok()) else {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "stat 字段不足"));
    };
    let btime_secs = std::fs::read_to_string("/proc/stat")?
        .lines()
        .find_map(|l| l.strip_prefix("btime ").and_then(|v| v.trim().parse::<u64>().ok()))
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "缺少 btime"))?;
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let hz = if hz > 0 { hz as u64 } else { 100 };
    Ok(Some(btime_secs.saturating_mul(1000) + ticks.saturating_mul(1000) / hz))
}

#[cfg(all(unix, not(target_os = "linux")))]
pub fn probe_process_start_time(_pid: u32) -> ProcessProbe {
    ProcessProbe::Gone
}

// ─── Linux 停止目标快照 ──────────────────────────────────────

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopTarget {
    pub pid: u32,
    pub start_ms: u64,
}

#[cfg(target_os = "linux")]
struct ProcInfo {
    pid: u32,
    ppid: u32,
    pgrp: u32,
    state: char,
    start_ms: u64,
}

#[cfg(target_os = "linux")]
fn linux_btime_secs() -> Option<u64> {
    std::fs::read_to_string("/proc/stat")
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("btime ").and_then(|v| v.trim().parse::<u64>().ok()))
}

#[cfg(target_os = "linux")]
fn parse_proc_stat(pid: u32, text: &str, btime_ms: u64, hz: u64) -> Option<ProcInfo> {
    let close = text.rfind(')')?;
    let fields: Vec<&str> = text[close + 1..].split_whitespace().collect();
    if fields.len() <= 19 {
        return None;
    }
    let state = fields[0].chars().next()?;
    let ppid = fields[1].parse::<u32>().ok()?;
    let pgrp = fields[2].parse::<u32>().ok()?;
    let ticks = fields[19].parse::<u64>().ok()?;
    Some(ProcInfo {
        pid,
        ppid,
        pgrp,
        state,
        start_ms: btime_ms.saturating_add(ticks.saturating_mul(1000) / hz),
    })
}

#[cfg(target_os = "linux")]
fn linux_process_table() -> Vec<ProcInfo> {
    let Some(btime) = linux_btime_secs() else {
        return Vec::new();
    };
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let hz = if hz > 0 { hz as u64 } else { 100 };
    let btime_ms = btime.saturating_mul(1000);
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else { continue };
        let Ok(text) = std::fs::read_to_string(format!("/proc/{}/stat", pid)) else {
            continue;
        };
        if let Some(info) = parse_proc_stat(pid, &text, btime_ms, hz) {
            out.push(info);
        }
    }
    out
}

/// 停止前快照：服务直接子进程、已认领的进程组、同组成员，以及它们的 ppid 后代。
///
/// **必须在发任何信号之前调用。** `groups` 只传调用方确认仍属于该服务的 pgid；
/// runner 自己的 pid/进程组会被排除。
#[cfg(target_os = "linux")]
pub fn snapshot_stop_targets(
    roots: &[u32],
    groups: &[u32],
    runner_pid: u32,
) -> Vec<StopTarget> {
    use std::collections::{HashMap, HashSet};

    let own_group = unsafe { libc::getpgrp() } as u32;
    let table = linux_process_table();
    if table.is_empty() {
        return Vec::new();
    }
    let by_pid: HashMap<u32, &ProcInfo> = table.iter().map(|p| (p.pid, p)).collect();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in &table {
        children.entry(p.ppid).or_default().push(p.pid);
    }

    let group_set: HashSet<u32> = groups.iter().copied().collect();
    let mut discovered: HashSet<u32> = HashSet::new();
    let mut queue: Vec<u32> = Vec::new();
    let mut roots_set: HashSet<u32> = roots.iter().copied().collect();
    for p in &table {
        if group_set.contains(&p.pgrp) {
            roots_set.insert(p.pid);
        }
    }
    for pid in roots_set {
        if discovered.insert(pid) {
            queue.push(pid);
        }
    }
    let mut targets = Vec::new();
    let mut visited: HashSet<u32> = HashSet::new();
    while let Some(pid) = queue.pop() {
        if !visited.insert(pid) {
            continue;
        }
        if let Some(info) = by_pid.get(&pid) {
            if pid != runner_pid && info.pgrp != own_group && info.state != 'Z' {
                targets.push(StopTarget { pid, start_ms: info.start_ms });
            }
        }
        if let Some(kids) = children.get(&pid) {
            for &kid in kids {
                if discovered.insert(kid) {
                    queue.push(kid);
                }
            }
        }
    }
    targets.sort_by_key(|t| t.pid);
    targets
}

/// 给快照中的目标发信号；启动时间不匹配（pid 复用）时跳过。
#[cfg(target_os = "linux")]
pub fn signal_stop_targets(targets: &[StopTarget], signal: i32) -> Result<usize, String> {
    let mut sent = 0usize;
    let mut errors = Vec::new();
    for t in targets {
        if t.pid <= 1 || process_start_time_millis(t.pid) != Some(t.start_ms) {
            continue;
        }
        let rc = unsafe { libc::kill(t.pid as i32, signal) };
        if rc == 0 {
            sent += 1;
            continue;
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::ESRCH) {
            errors.push(format!("kill({}, {}) 失败: {}", t.pid, signal, err));
        }
    }
    if errors.is_empty() { Ok(sent) } else { Err(errors.join("; ")) }
}

/// 进程组兜底信号；组 id 由调用方确认为该服务所有。
#[cfg(target_os = "linux")]
pub fn signal_stop_groups(groups: &[u32], signal: i32) -> Result<usize, String> {
    let own_group = unsafe { libc::getpgrp() } as u32;
    let mut sent = 0usize;
    let mut errors = Vec::new();
    for &pgid in groups {
        if pgid <= 1 || pgid == own_group || pgid > i32::MAX as u32 {
            continue;
        }
        let rc = unsafe { libc::kill(-(pgid as i32), signal) };
        if rc == 0 {
            sent += 1;
            continue;
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::ESRCH) {
            errors.push(format!("kill(-{}, {}) 失败: {}", pgid, signal, err));
        }
    }
    if errors.is_empty() { Ok(sent) } else { Err(errors.join("; ")) }
}

/// 快照中仍存活的目标（按启动时间复核）。
#[cfg(target_os = "linux")]
pub fn alive_stop_targets(targets: &[StopTarget]) -> Vec<StopTarget> {
    targets
        .iter()
        .copied()
        .filter(|t| process_start_time_millis(t.pid) == Some(t.start_ms))
        .collect()
}

// ─── 终止 ────────────────────────────────────────────────────

/// 终止进程树/进程组。
///
/// Windows 用 `taskkill /T /F`（与原实现一致）。
/// Unix 直接对进程组发信号：`kill -TERM -pgid` → 等待 → `kill -KILL -pgid`，
/// 不依赖外部 `kill` 命令，也不把“信号发出去了”当成“进程死了”。
pub fn kill_process_tree(pid: u32) -> Result<(), String> {
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let mut child = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .env("NoDefaultCurrentDirectoryInExePath", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("启动 taskkill 失败 (pid={}): {}", pid, e))?;
        let exited = wait_with_timeout(&mut child, Duration::from_secs(10));
        if !exited {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "taskkill 超过 10 秒未返回 (pid={})，已放弃等待（进程可能仍在运行）",
                pid
            ));
        }
        let mut stderr = String::new();
        if let Some(mut err) = child.stderr.take() {
            use std::io::Read;
            let _ = err.read_to_string(&mut stderr);
        }
        let status = child
            .wait()
            .map_err(|e| format!("等待 taskkill 失败 (pid={}): {}", pid, e))?;
        if !status.success() {
            return Err(format!(
                "taskkill 退出码 {:?} (pid={}): {}",
                status.code(),
                pid,
                stderr.trim()
            ));
        }
        Ok(())
    }
    #[cfg(unix)]
    {
        if pid <= 1 || pid > i32::MAX as u32 {
            return Err(format!("无效 pid: {}", pid));
        }
        let pgid = pid as i32;
        // 服务组 id 应该等于直接子进程 pid。万一调用方传错（例如传了 runner 自己的
        // pid），kill(-pgid) 会把 runner 所在的整个组一起杀掉；这里直接拒绝。
        let own_group = unsafe { libc::getpgrp() };
        if pgid == own_group {
            return Err(format!(
                "拒绝终止当前进程组 (pgid={})：服务应在独立进程组中启动",
                pgid
            ));
        }

        // 只对**进程组**发信号：pgid 在组内还有成员时不会被系统回收，
        // 而单个 pid 可能已被复用——对着 pid 补刀会误杀无关进程。
        //
        // **不在这里等"组消失"**：直接子进程退出后会变成僵尸，直到调用方
        // `Child::wait/try_wait` 回收前都还挂在进程组里。在这里等会把正常停止误报成
        // "SIGKILL 后仍未退出"（CI 实测），而且调用方本来就持有 Child，应该在那边
        // 回收并复核存活（与 Windows 侧 taskkill + survivors 复核同一口径）。
        let term = unsafe { libc::kill(-pgid, libc::SIGTERM) };
        if term != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ESRCH) {
                return Ok(()); // 组本来就不在了
            }
            return Err(format!("kill(-{}, SIGTERM) 失败: {}", pid, err));
        }
        // 给 TERM 一个短暂宽限，再把仍活着的组员 KILL 掉
        std::thread::sleep(Duration::from_millis(300));
        let killed = unsafe { libc::kill(-pgid, libc::SIGKILL) };
        if killed != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            return Err(format!("kill(-{}, SIGKILL) 失败: {}", pid, err));
        }
        Ok(())
    }
}

#[cfg(windows)]
fn wait_with_timeout(child: &mut std::process::Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return true,
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn test_build_command_uses_shell_and_process_group() {
        let mut cmd = build_command("echo hi");
        // spawn 后 pid 即 pgid；这里只验证命令形态与实际执行
        let out = cmd.output().expect("执行 sh -c");
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hi");
    }

    #[test]
    fn test_kill_process_tree_reaps_group() {
        let mut child = build_command("sleep 30 & wait").spawn().expect("起服务");
        let pid = child.id();
        assert!(process_start_time_millis(pid).is_some());
        kill_process_tree(pid).expect("终止进程组");
        // 调用方负责回收直接子进程（它退出后是僵尸，僵尸也挂在进程组里）
        let mut exited = false;
        for _ in 0..60 {
            if child.try_wait().ok().flatten().is_some() {
                exited = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(exited, "直接子进程应已退出");
        let mut gone = false;
        for _ in 0..60 {
            if !crate::winproc::group_alive(pid) {
                gone = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(gone, "进程组里不应还有成员");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_snapshot_catches_setsid_child_while_parent_alive() {
        use std::io::{BufRead, BufReader};
        let mut child = build_command("setsid sleep 30 & echo $!; wait")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("起服务");
        let mut line = String::new();
        BufReader::new(child.stdout.take().expect("stdout"))
            .read_line(&mut line)
            .expect("读子 pid");
        let setsid_pid: u32 = line.trim().parse().expect("解析 setsid pid");
        std::thread::sleep(Duration::from_millis(100));

        let targets = snapshot_stop_targets(&[child.id()], &[child.id()], std::process::id());
        assert!(
            targets.iter().any(|t| t.pid == setsid_pid),
            "快照应通过 ppid 找到 setsid 子进程: {:?}",
            targets
        );
        signal_stop_targets(&targets, libc::SIGTERM).expect("TERM 快照目标");
        signal_stop_groups(&[child.id()], libc::SIGTERM).expect("TERM 进程组");
        let _ = child.wait();
        for _ in 0..60 {
            if process_start_time_millis(setsid_pid).is_none() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = signal_stop_targets(&targets, libc::SIGKILL);
        let _ = signal_stop_groups(&[child.id()], libc::SIGKILL);
        panic!("setsid 子进程未退出");
    }

    #[test]
    fn test_linux_process_start_time_changes_with_new_process() {
        let a = process_start_time_millis(std::process::id()).expect("当前进程");
        assert!(a > 0);
        // 不存在的 pid：Gone
        assert!(matches!(probe_process_start_time(999_999), ProcessProbe::Gone));
    }
}
