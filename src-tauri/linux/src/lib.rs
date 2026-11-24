//! Nexus Linux 服务端：无 GUI、无 Tauri。
//!
//! - `nexusctl`：项目/服务配置、manifest 生成、systemd 安装/状态/日志/命令；
//! - `nexus-boot-runner`：由 systemd 拉起的常驻启动器（复用 `boot-core::runner`）。
//!
//! 配置是唯一真相（TOML），manifest/state/logs 都是生成物/运行时产物。

pub mod cli;
pub mod client;
pub mod config;
pub mod layout;
pub mod manifest;
pub mod output;
pub mod systemd;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
