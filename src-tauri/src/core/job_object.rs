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

    fn CloseHandle(hObject: HANDLE) -> BOOL;
}

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

    /// 将进程加入 Job Object（通过原始句柄）
    fn assign_raw(&self, raw_handle: isize, pid: Option<u32>) {
        unsafe {
            let ret = AssignProcessToJobObject(
                self.handle,
                raw_handle as HANDLE,
            );
            if ret == 0 {
                let err = std::io::Error::last_os_error();
                log::warn!(
                    "[nexus] ⚠ AssignProcessToJobObject 失败 (pid={:?}): {}",
                    pid, err
                );
            } else {
                log::debug!("[nexus] 进程 pid={:?} 已加入 Job Object", pid);
            }
        }
    }

    /// 将 std::process::Child 加入 Job Object
    pub fn assign_child(&self, child: &std::process::Child) {
        use std::os::windows::io::AsRawHandle;
        self.assign_raw(child.as_raw_handle() as isize, Some(child.id()));
    }

    /// 通过进程 ID 将进程加入 Job Object（最小权限）
    pub fn assign_by_pid(&self, pid: u32) {
        extern "system" {
            fn OpenProcess(dwDesiredAccess: DWORD, bInheritHandle: BOOL, dwProcessId: DWORD) -> HANDLE;
        }
        const PROCESS_SET_QUOTA: DWORD = 0x0100;
        const PROCESS_TERMINATE: DWORD = 0x0001;
        const MINIMAL_ACCESS: DWORD = PROCESS_SET_QUOTA | PROCESS_TERMINATE;
        unsafe {
            let handle = OpenProcess(MINIMAL_ACCESS, 0, pid);
            if handle == 0 {
                log::warn!("[nexus] ⚠ OpenProcess 失败 (pid={}): {}", pid, std::io::Error::last_os_error());
                return;
            }
            self.assign_raw(handle, Some(pid));
            CloseHandle(handle);
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

// JobObject 包含的 HANDLE 是 isize，自动满足 Send + Sync。
// 通过 Arc<JobObject> 在 TerminalManager 和 ProcessManager 间共享。
