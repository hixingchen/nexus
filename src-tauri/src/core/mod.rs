pub mod ai;
pub mod classfile;
pub mod decompiler;
pub mod file_watcher;
pub mod jarfile;
pub mod logfile;
pub mod process;
pub mod winenv;
#[cfg(windows)]
pub mod job_object;

/// 游离子进程走查（见 `winproc` 的模块说明）。
///
/// 非 Windows 上是**空桩**：没有"另开窗口"这种语义（`start` 是 cmd 内建命令，
/// unix 下 `sh -c` 的孙进程默认由进程组管理），认领永远找不到东西，
/// 于是 `process.rs` 可以只写一条代码路径。
#[cfg(windows)]
pub mod winproc;

#[cfg(not(windows))]
pub mod winproc {
    pub(crate) struct ProcEntry {
        pub pid: u32,
        pub ppid: u32,
        pub name: String,
    }

    pub(crate) fn snapshot_processes() -> Vec<ProcEntry> {
        Vec::new()
    }

    pub(crate) fn descendants(_root: u32, _snapshot: &[ProcEntry]) -> Vec<u32> {
        Vec::new()
    }

    pub(crate) fn name_of(_pid: u32, _snapshot: &[ProcEntry]) -> String {
        "?".to_string()
    }

    pub(crate) struct OwnedProcess;

    impl OwnedProcess {
        pub(crate) fn open(_pid: u32) -> Option<Self> {
            None
        }
        pub(crate) fn is_alive(&self) -> bool {
            false
        }
        pub(crate) fn exit_code(&self) -> Option<i32> {
            None
        }
    }
}
