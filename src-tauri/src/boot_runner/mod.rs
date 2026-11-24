//! 登录前启动器入口（Windows GUI 的 `Nexus.exe --boot-runner` 分支）。
//!
//! 启动器主循环已移到 `boot-core::runner`，与 Linux 服务端共用一份实现；
//! 独立的 `nexus-boot-runner.exe` 也直接依赖 `boot-core`，不再链接 Tauri。
//!
//! 所有权：runner 是开机启动服务的**唯一拥有者**。GUI 的 `ProcessManager` 永远不会
//! 收养这些进程，因此关闭 Nexus 不会停掉它们；停止/重启必须经文件命令队列发到这里。

pub fn run() {
    boot_core::runner::run();
}
