//! .class 反编译（捆绑 CFR 0.152，MIT 协议，见 resources/cfr-LICENSE.txt）
//!
//! 运行时用本机 java 执行（JVM 是 Java 开发环境的必然依赖）；
//! 无 JRE / 超时等失败由前端回退到字节码视图（read_class_file）。
//! 输出为接近 IDEA 的 Java 源码（方法体、变量名、控制流均还原）。

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

/// 内嵌的 CFR jar（首次使用时写入临时目录，由 JVM 执行）
const CFR_JAR: &[u8] = include_bytes!("../../resources/cfr-0.152.jar");

/// 临时 jar 路径（应用生命周期内只写一次）
static CFR_JAR_PATH: OnceLock<PathBuf> = OnceLock::new();

/// 临时 class 文件序号（并发反编译不互相覆盖）
static CLASS_SEQ: AtomicU32 = AtomicU32::new(0);

/// 反编译超时（CFR 正常 <1s，超时基本是极端类）
const DECOMPILE_TIMEOUT: Duration = Duration::from_secs(15);

/// 确保 CFR jar 已写入临时目录，返回其路径
async fn ensure_cfr_jar() -> Result<&'static PathBuf, String> {
    let path = CFR_JAR_PATH.get_or_init(|| std::env::temp_dir().join("nexus-cfr-0.152.jar"));
    if !tokio::fs::try_exists(path).await.unwrap_or(false) {
        tokio::fs::write(path, CFR_JAR).await.map_err(|e| format!("写入 CFR jar 失败: {}", e))?;
    }
    Ok(path)
}

/// 反编译 class 文件字节码为 Java 源码
pub async fn decompile_class_bytes(bytes: &[u8]) -> Result<String, String> {
    let jar = ensure_cfr_jar().await?;

    // CFR 只能读文件路径，写临时 class 文件
    let seq = CLASS_SEQ.fetch_add(1, Ordering::Relaxed);
    let class_path = std::env::temp_dir().join(format!("nexus-decompile-{}.class", seq));
    tokio::fs::write(&class_path, bytes).await.map_err(|e| format!("写入临时 class 失败: {}", e))?;

    let result = tokio::time::timeout(
        DECOMPILE_TIMEOUT,
        tokio::process::Command::new("java")
            .arg("-jar")
            .arg(jar)
            .arg("--silent").arg("true")
            .arg("--showversion").arg("false")
            .arg(&class_path)
            .stdin(Stdio::null())
            .output(),
    )
    .await
    .map_err(|_| "反编译超时（>15s）".to_string());

    // 无论成败都清理临时文件
    let _ = tokio::fs::remove_file(&class_path).await;

    match result {
        Ok(Ok(output)) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            Ok(strip_cfr_banner(&text).to_string())
        }
        Ok(Ok(output)) => Err(format!(
            "CFR 退出码 {}: {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr)
        )),
        Ok(Err(e)) => Err(format!("启动 java 失败（需要 JRE）: {}", e)),
        Err(e) => Err(e),
    }
}

/// 去掉 CFR 开头的 `/* Decompiled with CFR ... Could not load ... */` 头注释块。
/// `/**` 开头的是类 javadoc，不能剥（strip_prefix("/*") 会误匹配）
fn strip_cfr_banner(s: &str) -> &str {
    let t = s.trim_start();
    if t.starts_with("/*") && !t.starts_with("/**") {
        if let Some(end) = t[2..].find("*/") {
            return t[end + 4..].trim_start();
        }
    }
    t
}

/* ---- Tests ---- */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_cfr_banner() {
        let input = "/*\n * Decompiled with CFR.\n * \n * Could not load the following classes:\n *  javax.servlet.http.HttpServletRequest\n */\npackage com.example;\n\npublic class A {}\n";
        let out = strip_cfr_banner(input);
        assert!(out.starts_with("package com.example;"), "banner 未剥离:\n{}", out);
        assert!(!out.contains("Could not load"), "banner 残留:\n{}", out);
    }

    #[test]
    fn test_strip_no_banner_untouched() {
        let input = "/** 类 javadoc */\npublic class A {}\n";
        let out = strip_cfr_banner(input);
        assert!(out.starts_with("/** 类 javadoc */"), "无 banner 内容不应被改:\n{}", out);
    }
}
