fn main() {
    // Tauri 的 externalBin 在**编译期**就要求文件存在，而 runner 本身也要用 cargo 构建——
    // 直接 `cargo test` / `cargo build`（没先跑 npm run build:runner）会在这里卡住。
    // 先放一个空占位文件满足存在性检查；真正的 runner 由 scripts/build-boot-runner.mjs
    // 在打包前构建并覆盖。安装命令会校验复制前后大小，空占位不会被当成可用的启动器。
    let target = std::env::var("TARGET")
        .or_else(|_| std::env::var("TAURI_ENV_TARGET_TRIPLE"))
        .unwrap_or_else(|_| "x86_64-pc-windows-msvc".to_string());
    let suffix = if target.contains("windows") { ".exe" } else { "" };
    let path = std::path::PathBuf::from("binaries")
        .join(format!("nexus-boot-runner-{}{}", target, suffix));
    if !path.exists() {
        let _ = std::fs::create_dir_all("binaries");
        let _ = std::fs::write(&path, []);
    }
    tauri_build::build();
}
