//! 进程走查：Windows 识别“另开窗口”跑掉的游离子进程；Linux 用进程组。
//!
//! Windows 实现保持原样（沿父 pid 链走查）。Linux 不需要按窗口认领：
//! `build_command` 为每个服务建立了独立进程组，`OwnedProcess::open_group`
//! 直接判断整个组是否还有人——这恰好覆盖 shell 把子进程放到后台后自己退出的情况。

#[cfg(windows)]
#[path = "winproc_windows.rs"]
mod imp;

#[cfg(unix)]
mod imp {
    //! Linux/Unix：进程组 + /proc。
    //!
    //! 不实现完整进程快照：开机启动器在 Linux 上用进程组判定存活，
    //! 不需要枚举全系统进程。GUI 只在 Windows 打包，这里的空快照不会影响它。

    /// 进程快照里的一项（Unix 不使用；保留类型让调用方签名一致）
    pub struct ProcEntry {
        pub pid: u32,
        pub ppid: u32,
        pub name: String,
    }

    pub fn snapshot_processes() -> Vec<ProcEntry> {
        Vec::new()
    }

    pub fn descendants(_root: u32, _snapshot: &[ProcEntry]) -> Vec<u32> {
        Vec::new()
    }

    pub fn name_of(_pid: u32, _snapshot: &[ProcEntry]) -> String {
        "?".to_string()
    }

    pub fn process_image_path(pid: u32) -> Option<String> {
        if pid == 0 {
            return None;
        }
        std::fs::read_link(format!("/proc/{}/exe", pid))
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    }

    /// 进程组是否还有成员（`kill(-pgid, 0)`）。
    ///
    /// `EPERM` 也算活着：组里存在但不属于当前用户。
    pub fn group_alive(pgid: u32) -> bool {
        if pgid == 0 || pgid > i32::MAX as u32 {
            return false;
        }
        let rc = unsafe { libc::kill(-(pgid as i32), 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    /// 进程组/进程句柄。Linux 上没有 Windows 那种“退出码仍可读”的句柄，
    /// 但存活判断足够：停止时统一按进程组发信号。
    pub struct OwnedProcess {
        pid: u32,
        group: bool,
    }

    impl OwnedProcess {
        pub fn open(pid: u32) -> Option<Self> {
            if pid != 0 && std::path::Path::new(&format!("/proc/{}", pid)).exists() {
                Some(Self { pid, group: false })
            } else {
                None
            }
        }

        /// 以进程组为目标打开（pgid == 服务直接子进程的 pid）。
        pub fn open_group(pgid: u32) -> Option<Self> {
            group_alive(pgid).then_some(Self { pid: pgid, group: true })
        }

        pub fn is_alive(&self) -> bool {
            if self.group {
                group_alive(self.pid)
            } else {
                std::path::Path::new(&format!("/proc/{}", self.pid)).exists()
            }
        }

        /// Linux 上拿不到非子进程的退出码；返回 None 表示“未知”，
        /// 调用方按“未观测到退出码”处理。
        pub fn exit_code(&self) -> Option<i32> {
            None
        }
    }
}

pub use imp::*;
