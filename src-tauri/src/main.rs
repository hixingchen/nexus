// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // 控制台 + 文件双写：默认 info 级别（启动/启停/清理等关键日志可见），
    // 可用 RUST_LOG 覆盖（如 RUST_LOG=debug）。文件日志是打包版唯一可见的诊断通道
    nexus_lib::logger::init();
    nexus_lib::run();
}
