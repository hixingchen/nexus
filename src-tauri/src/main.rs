// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // 默认 info 级别：启动/启停/清理等关键日志可见；可用 RUST_LOG 覆盖（如 RUST_LOG=debug）
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    nexus_lib::run();
}
