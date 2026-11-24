pub mod ai;
pub mod auto_restart;
pub mod boot;
pub mod classfile;
pub mod decompiler;
pub mod download;
pub mod envparse;
pub mod file_watcher;
pub mod health;
pub mod jarfile;
pub mod logfile;
pub mod logfollow;
pub mod logpipe;
pub mod logtext;
pub mod ports;
pub mod process;
pub mod redact;
pub mod restart_policy;
pub mod runtime;
pub mod textenc;
pub mod winenv;
#[cfg(windows)]
pub mod job_object;

/// 游离子进程走查（实现见 `boot-core::winproc`）。
///
/// Windows 沿父 pid 链认领；Linux 用进程组判定（`build_command` 为每个服务
/// 建立了独立进程组）。GUI 只在 Windows 打包，但模块本身两端都可编译。
pub mod winproc;
