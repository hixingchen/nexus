//! 给独立 runner 嵌入 Windows 图标与版本资源。
//!
//! Tauri 的 bundle.icon 只作用于主程序（src-tauri/build.rs）；runner 是独立 crate，
//! 不自己编译 .rsrc 的话，安装目录里的 nexus-boot-runner.exe 就只有系统默认空白图标。

fn main() {
    let icon = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../icons/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", icon.display());

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    // rc.exe 解析相对路径的行为不可靠，这里与 tauri-build 一致传绝对路径
    tauri_winres::WindowsResource::new()
        .set_icon(&icon.display().to_string())
        .compile()
        .expect("嵌入 src-tauri/icons/icon.ico 到 nexus-boot-runner.exe 失败");
}
