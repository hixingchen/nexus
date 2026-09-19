//! Windows 进程走查：识别服务"另开窗口"跑掉的游离子进程。
//!
//! 为什么不用 Job Object 做这件事（别改成那样）：Job 是**全局共享**的
//! （`core::job_object::shared` / `ProcessManager::job_arc`），除服务进程外还装着
//! 工具命令（`commands/process.rs`）、AI 面板命令与 `dsh web`（`core/ai.rs`）、
//! 反编译 JVM（`core/decompiler.rs`）的进程。拿"Job 集合 − 进程表"当游离进程，
//! 会把**正在跑的反编译 JVM 或构建命令**算成某个服务另开窗口跑掉的进程，
//! 于是"停止服务"顺手把它们杀掉。
//!
//! 这里改为沿**父 pid 链**走查：从我们那条 cmd 的 pid 往下找。归属无歧义——
//! 系统里只有它的后代才会把 `th32ParentProcessID` 指向它。

type Handle = isize;
type Bool = i32;
type Dword = u32;

const TH32CS_SNAPPROCESS: Dword = 0x0000_0002;
const INVALID_HANDLE_VALUE: Handle = -1;
const MAX_PATH: usize = 260;

/// 只申请查询权限：认领后用它判存活 / 取退出码。**不含**终止权限——
/// 杀进程仍统一走 `taskkill /T`（借助系统工具就能连整棵树一起收，不必自己实现遍历）。
const PROCESS_QUERY_LIMITED_INFORMATION: Dword = 0x1000;
/// `GetExitCodeProcess` 的"仍在运行"哨兵值（Win32 文档：值 259）
const STILL_ACTIVE: Dword = 259;

#[repr(C)]
struct ProcessEntry32 {
    size: Dword,
    cnt_usage: Dword,
    pid: Dword,
    default_heap_id: usize, // ULONG_PTR
    module_id: Dword,
    threads: Dword,
    ppid: Dword,
    pri_class_base: i32,
    flags: Dword,
    exe_file: [u16; MAX_PATH],
}

extern "system" {
    fn CreateToolhelp32Snapshot(flags: Dword, pid: Dword) -> Handle;
    fn Process32FirstW(snapshot: Handle, entry: *mut ProcessEntry32) -> Bool;
    fn Process32NextW(snapshot: Handle, entry: *mut ProcessEntry32) -> Bool;
    fn OpenProcess(access: Dword, inherit: Bool, pid: Dword) -> Handle;
    fn GetExitCodeProcess(handle: Handle, code: *mut Dword) -> Bool;
    fn CloseHandle(handle: Handle) -> Bool;
}

/// 进程快照里的一项
pub(crate) struct ProcEntry {
    pub pid: u32,
    pub ppid: u32,
    /// 映像名（如 `java.exe`），只用于日志里说清"接管了谁"
    pub name: String,
}

/// 取当前所有进程的快照。
///
/// 失败（快照被系统拒绝等）时返回空表：调用方按"没有游离进程"处理，
/// 即退回既有行为（服务照常判定退出），不会因为查不了就卡住生命周期。
pub(crate) fn snapshot_processes() -> Vec<ProcEntry> {
    let mut out = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            log::warn!("[nexus] CreateToolhelp32Snapshot 失败，本次不做游离进程走查");
            return out;
        }
        let mut entry: ProcessEntry32 = std::mem::zeroed();
        entry.size = std::mem::size_of::<ProcessEntry32>() as Dword;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                // NUL 截断后转字符串；坏码位不致命（只是日志里显示得难看）
                let end = entry.exe_file.iter().position(|&c| c == 0).unwrap_or(MAX_PATH);
                out.push(ProcEntry {
                    pid: entry.pid,
                    ppid: entry.ppid,
                    name: String::from_utf16_lossy(&entry.exe_file[..end]),
                });
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
    }
    out
}

/// `root` 的整棵后代（不含 root 自己）。
///
/// 广度优先、带 visited 集合：快照里 pid 唯一，但父链可能因 pid 复用出现自环，
/// 不去重的话循环不会终止。
pub(crate) fn descendants(root: u32, snapshot: &[ProcEntry]) -> Vec<u32> {
    let mut found: Vec<u32> = Vec::new();
    let mut visited: std::collections::HashSet<u32> = std::collections::HashSet::new();
    visited.insert(root);
    let mut frontier = vec![root];
    while let Some(pid) = frontier.pop() {
        for e in snapshot.iter().filter(|e| e.ppid == pid) {
            if visited.insert(e.pid) {
                found.push(e.pid);
                frontier.push(e.pid);
            }
        }
    }
    found
}

/// 快照里某个 pid 的映像名（找不到时返回 "?"）
pub(crate) fn name_of(pid: u32, snapshot: &[ProcEntry]) -> String {
    snapshot
        .iter()
        .find(|e| e.pid == pid)
        .map(|e| e.name.clone())
        .unwrap_or_else(|| "?".to_string())
}

/// 我们持有的进程句柄（认领游离进程用）。
///
/// 为什么持有句柄而不是只记 pid：
/// 1. 判存活/取退出码都不再受 pid 复用影响——句柄指向的是进程对象本身；
/// 2. 进程退出后对象不会立刻销毁，退出码读得到（只记 pid 的话，等到下次轮询时
///    进程早没了，只能报一个"?"）。
///
/// 句柄是稀缺资源：只对认领到的游离进程开，服务停止时随 `Drop` 关闭。
pub(crate) struct OwnedProcess {
    handle: Handle,
}

impl OwnedProcess {
    /// 打开句柄；进程已退出或无权限时返回 None（调用方跳过该候选）
    pub(crate) fn open(pid: u32) -> Option<Self> {
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle == 0 {
                None
            } else {
                Some(OwnedProcess { handle })
            }
        }
    }

    /// 是否仍在运行
    pub(crate) fn is_alive(&self) -> bool {
        self.exit_code().is_none()
    }

    /// 退出码；仍在运行时 None
    pub(crate) fn exit_code(&self) -> Option<i32> {
        unsafe {
            let mut code: Dword = 0;
            if GetExitCodeProcess(self.handle, &mut code) == 0 {
                return None; // 查不到：按"仍在运行"处理，比误报退出安全
            }
            if code == STILL_ACTIVE {
                None
            } else {
                Some(code as i32)
            }
        }
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pid: u32, ppid: u32, name: &str) -> ProcEntry {
        ProcEntry { pid, ppid, name: name.to_string() }
    }

    /// 快照必须包含我们自己和父进程链——这是走查的前提
    #[test]
    fn test_snapshot_contains_self() {
        let snap = snapshot_processes();
        assert!(!snap.is_empty(), "快照不该为空");
        let me = std::process::id();
        assert!(snap.iter().any(|e| e.pid == me), "快照里应当有当前进程 (pid={})", me);
    }

    /// 真起一个子进程，走查必须找得到它
    #[test]
    fn test_descendants_finds_live_child() {
        let child = std::process::Command::new("cmd")
            .args(["/C", "ping -n 6 127.0.0.1 >nul"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("应当能起 cmd");
        let child_pid = child.id();

        // ping 大约活 5 秒，这里给它一点时间进入快照
        let mut found = Vec::new();
        for _ in 0..40 {
            let snap = snapshot_processes();
            found = descendants(std::process::id(), &snap);
            if found.contains(&child_pid) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        kill_and_wait(child);
        assert!(found.contains(&child_pid), "走查应找到子进程 {}，实际 {:?}", child_pid, found);
    }

    /// 自环/重复父链不能把走查卡死（pid 复用会造出 pid == ppid 的项）
    #[test]
    fn test_descendants_survives_cyclic_parent_chain() {
        let snap = vec![
            entry(100, 100, "self-loop.exe"),
            entry(101, 100, "child.exe"),
            entry(102, 101, "grand.exe"),
            entry(103, 102, "back.exe"),
        ];
        let mut got = descendants(100, &snap);
        got.sort_unstable();
        assert_eq!(got, vec![101, 102, 103], "后代应各出现一次且不重复");
    }

    /// name_of 找不到时给 "?"，不为难调用方
    #[test]
    fn test_name_of_missing_pid() {
        assert_eq!(name_of(999_999, &[]), "?");
    }

    /// 真实进程：句柄能判存活，进程结束后退出码读得出来
    #[test]
    fn test_owned_process_alive_then_exit_code() {
        let child = std::process::Command::new("cmd")
            .args(["/C", "exit /b 7"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("应当能起 cmd");
        let pid = child.id();
        let owned = OwnedProcess::open(pid);
        assert!(owned.is_some(), "刚起的进程应当能打开句柄 (pid={})", pid);

        let mut child = child;
        let _ = child.wait();
        let owned = owned.unwrap();
        // 句柄持有期间进程对象还在：退出码读得到（只记 pid 的做法在这里只能报 "?"）
        let mut code = None;
        for _ in 0..40 {
            code = owned.exit_code();
            if code.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(code, Some(7), "应当读到退出码 7");
        assert!(!owned.is_alive(), "进程已退出，不该报告存活");
    }

    fn kill_and_wait(mut child: std::process::Child) {
        let _ = child.kill();
        let _ = child.wait();
    }
}
