//! Windows Job Object 管理
//!
//! 用于确保子进程在父进程退出时自动终止。

#![allow(clippy::upper_case_acronyms)]

type HANDLE = isize;
type BOOL = i32;
type DWORD = u32;

const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: DWORD = 0x2000;

extern "system" {
    fn CreateJobObjectW(
        lpJobAttributes: *const std::ffi::c_void,
        lpName: *const u16,
    ) -> HANDLE;

    fn SetInformationJobObject(
        hJob: HANDLE,
        JobObjectInformationClass: DWORD,
        lpJobObjectInformation: *const std::ffi::c_void,
        cbJobObjectInformationLength: DWORD,
    ) -> BOOL;

    fn AssignProcessToJobObject(
        hJob: HANDLE,
        hProcess: HANDLE,
    ) -> BOOL;

    fn OpenProcess(
        dwDesiredAccess: DWORD,
        bInheritHandle: BOOL,
        dwProcessId: DWORD,
    ) -> HANDLE;

    fn CloseHandle(hObject: HANDLE) -> BOOL;
}

/// AssignProcessToJobObject 要求句柄带 PROCESS_SET_QUOTA 与 PROCESS_TERMINATE
const PROCESS_SET_QUOTA: DWORD = 0x0100;
const PROCESS_TERMINATE: DWORD = 0x0001;

pub struct JobObject {
    handle: HANDLE,
}

impl JobObject {
    pub fn new() -> Result<Self, String> {
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle == 0 {
                return Err(format!("CreateJobObject 失败: {}", std::io::Error::last_os_error()));
            }

            // JOBOBJECT_BASIC_LIMIT_INFORMATION (x64, 64 bytes):
            //   PerProcessUserTimeLimit  [0..8]    LARGE_INTEGER
            //   PerJobUserTimeLimit      [8..16]   LARGE_INTEGER
            //   LimitFlags               [16..20]  DWORD  <-- 我们需要设置的字段
            //   MinimumWorkingSetSize    [24..32]  SIZE_T (含 4 字节对齐填充)
            //   MaximumWorkingSetSize    [32..40]  SIZE_T
            //   ActiveProcessLimit       [40..44]  DWORD
            //   (padding)                [44..48]
            //   Affinity                 [48..56]  ULONG_PTR
            //   PriorityClass            [56..60]  DWORD
            //   SchedulingClass          [60..64]  DWORD
            // IO_COUNTERS (48 bytes): 6 × ULONGLONG
            // 后续 4 个 SIZE_T 各 8 字节
            // 总计: 64 + 48 + 32 = 144 字节
            #[repr(C)]
            struct ExtendedLimitInfo {
                basic_limit: [u8; 64],
                io_counters: [u8; 48],
                process_memory_limit: usize,
                job_memory_limit: usize,
                peak_process_memory_used: usize,
                peak_job_memory_used: usize,
            }

            let mut info: ExtendedLimitInfo = std::mem::zeroed();
            // LimitFlags 在 BasicLimitInformation 的偏移 16 处 (PerProcessUserTimeLimit 8 + PerJobUserTimeLimit 8)
            info.basic_limit[16..20].copy_from_slice(&JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE.to_le_bytes());

            const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS: DWORD = 9;

            let ret = SetInformationJobObject(
                handle,
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
                &info as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<ExtendedLimitInfo>() as DWORD,
            );

            if ret == 0 {
                let err = std::io::Error::last_os_error();
                CloseHandle(handle);
                return Err(format!("SetInformationJobObject 失败: {}", err));
            }

            log::info!("[nexus] Windows Job Object 已创建 (KILL_ON_JOB_CLOSE)");
            Ok(JobObject { handle })
        }
    }

    /// 将进程加入 Job Object（通过原始句柄）。
    ///
    /// **返回失败原因而不是只写一行日志**（CQ-27）：纳管失败此前完全静默——服务照常启动、
    /// 之后一切"看起来正常"，只有 Nexus 被强杀时才会发现子进程没跟着退出。这正是铁律 21
    /// （"配置写了 ≠ 生效了"）的形状：没有任何"它真的被纳管了"的观测证据。
    /// 调用方据此决定把原因写进**服务日志缓冲**（用户能看见）还是只记日志。
    fn assign_raw(&self, raw_handle: isize, pid: Option<u32>) -> Result<(), String> {
        unsafe {
            let ret = AssignProcessToJobObject(
                self.handle,
                raw_handle as HANDLE,
            );
            if ret == 0 {
                let err = std::io::Error::last_os_error();
                let msg = format!("AssignProcessToJobObject 失败 (pid={:?}): {}", pid, err);
                log::warn!("[nexus] ⚠ {}", msg);
                Err(msg)
            } else {
                log::debug!("[nexus] 进程 pid={:?} 已加入 Job Object", pid);
                Ok(())
            }
        }
    }

    /// 将 std::process::Child 加入 Job Object
    pub fn assign_child(&self, child: &std::process::Child) -> Result<(), String> {
        use std::os::windows::io::AsRawHandle;
        self.assign_raw(child.as_raw_handle() as isize, Some(child.id()))
    }

    /// 按 pid 加入 Job Object（不持有 `Child` 的场景：tokio 子进程只在 await 期间存在）。
    ///
    /// 为什么需要：`tokio::process::Child` 拿不到 std 的 `Child`，若反编译的 JVM 只在
    /// `kill_on_drop` 保护下运行，Nexus 被强杀（任务管理器结束进程/崩溃）时它不会随之退出，
    /// 会留下一个占着几百 MB 的 java 进程。加入共享 Job 后由 KILL_ON_JOB_CLOSE 兜底。
    ///
    /// 句柄是我们自己打开的，赋值完必须关闭（关闭本进程的句柄不会把目标移出 Job）。
    pub fn assign_pid(&self, pid: u32) -> Result<(), String> {
        unsafe {
            let handle = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if handle == 0 {
                let msg = format!(
                    "OpenProcess 失败 (pid={}): {}，该进程未纳入 Job Object",
                    pid,
                    std::io::Error::last_os_error()
                );
                log::warn!("[nexus] ⚠ {}", msg);
                return Err(msg);
            }
            let r = self.assign_raw(handle as isize, Some(pid));
            CloseHandle(handle);
            r
        }
    }
}

impl Drop for JobObject {
    fn drop(&mut self) {
        if self.handle != 0 {
            unsafe { CloseHandle(self.handle); }
            log::info!("[nexus] Job Object 已关闭 → KILL_ON_JOB_CLOSE 触发");
        }
    }
}

/// 进程级共享 Job 句柄。
///
/// 主路径是把 Job 传给 `ProcessManager`（服务进程用它），但**不经过 ProcessManager 的子进程**
/// 也需要纳管（当前是反编译用的 JVM）——那些调用点拿不到 `AppState`，用这个全局出口共享同一
/// Job，避免为每个子系统各建一个 Job（每个 Job 都会在 Drop 时杀掉自己名下的进程）。
static SHARED_JOB: std::sync::OnceLock<std::sync::Arc<JobObject>> = std::sync::OnceLock::new();

/// 登记共享 Job（应用启动时调用一次；重复调用忽略）
pub fn set_shared(job: std::sync::Arc<JobObject>) {
    let _ = SHARED_JOB.set(job);
}

/// 取共享 Job（未启用 Job 时返回 None，调用方按"无兜底"降级）
pub fn shared() -> Option<&'static std::sync::Arc<JobObject>> {
    SHARED_JOB.get()
}

// JobObject 包含的 HANDLE 是 isize，自动满足 Send + Sync。
// 通过 Arc<JobObject> 在 ProcessManager 间共享。
