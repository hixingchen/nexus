//! 共享实现：`boot-core::health`。
//!
//! 本文件只做 re-export，让 GUI 内部的 `crate::core::health::...` 路径保持不变。
//! 真实代码在 `src-tauri/boot-core/src/health.rs`（Windows 与 Linux 共用）。

pub use boot_core::health::*;
