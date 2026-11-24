//! Nexus 服务开机启动的共享核心。
//!
//! 这里放 **不依赖 Tauri** 的实现：manifest/state 类型、就绪检查、崩溃重启策略、
//! 进程启动与终止、Windows/Linux 两端的启动器主循环。
//!
//! Windows 桌面版（`nexus`）与 Linux 服务端（`nexus-linux`）都依赖本 crate：
//! 一份实现、两端复用，避免“重启/就绪规则改了一边忘了另一边”。

pub mod boot;
pub mod health;
#[cfg(windows)]
pub mod job_object;
pub mod process_exec;
pub mod redact;
pub mod restart_policy;
pub mod runner;
pub mod textenc;
pub mod winenv;
pub mod winproc;
