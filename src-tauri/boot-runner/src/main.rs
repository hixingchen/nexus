//! 独立开机启动器入口。
//!
//! 与 `Nexus.exe --boot-runner` 分支调用同一套 `nexus_lib::boot_runner` 逻辑；
//! 独立 exe 不初始化 Tauri/WebView、不注册单实例插件，也不包含界面。
//!
//! 任务 action 形如：
//! `"C:\ProgramData\Nexus\boot\runner\nexus-boot-runner.exe" --manifest "C:\Users\...\.nexus\boot\manifest.json"`
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    nexus_lib::boot_runner::run();
}
