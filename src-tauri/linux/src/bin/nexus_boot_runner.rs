//! systemd 拉起的常驻启动器：`nexus-boot-runner --manifest <path> --mode user|system`。
//!
//! 主循环与 Windows 启动器完全共用（`boot-core::runner`）。

fn main() {
    boot_core::runner::run();
}
