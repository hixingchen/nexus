//! .class 反编译（捆绑 CFR 0.152，MIT 协议，见 resources/cfr-LICENSE.txt）
//!
//! 运行时用本机 java 执行（JVM 是 Java 开发环境的必然依赖）；
//! 无 JRE / 超时等失败由前端回退到字节码视图（read_class_file）。
//! 输出为接近 IDEA 的 Java 源码（方法体、变量名、控制流均还原）。

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

/// 内嵌的 CFR jar（首次使用时写入临时目录，由 JVM 执行）
const CFR_JAR: &[u8] = include_bytes!("../../resources/cfr-0.152.jar");

/// 临时 jar 路径（应用生命周期内只写一次）
static CFR_JAR_PATH: OnceLock<PathBuf> = OnceLock::new();

/// 反编译超时（CFR 正常 <1s，超时基本是极端类）
const DECOMPILE_TIMEOUT: Duration = Duration::from_secs(15);

/// Windows：压住 java 控制台窗口（打包版父进程无控制台，缺此标志会闪窗并产生 conhost.exe）
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// CFR 相关文件的存放目录：应用私有目录。
///
/// 不放 `%TEMP%`：那里同用户的任意进程都能预置同名文件，而我们随后会用 `java -jar` 执行它
/// （本次修复前还叠加了 check-then-write 竞态）。home 不可用时回退系统临时目录（功能优先）。
fn cfr_dir() -> PathBuf {
    dirs::home_dir()
        .map(|home| home.join(".nexus").join("bin"))
        .unwrap_or_else(std::env::temp_dir)
}

/// 确保 CFR jar 已写入私有目录，返回其路径
///
/// 原子部署：先写 `<name>.jar.tmp` 再 rename。原实现是 `try_exists` → `write`，
/// 两个并发反编译（同时打开两个 .class）会在存在性检查与写入之间交错，另一个 `java -jar`
/// 可能读到半个 jar（表现为偶发 "Invalid or corrupt jarfile"）。rename 是原子替换，
/// 读方只会看到旧的完整文件或新的完整文件。
async fn ensure_cfr_jar() -> Result<&'static PathBuf, String> {
    let path = CFR_JAR_PATH.get_or_init(|| cfr_dir().join("cfr-0.152.jar"));
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| format!("创建 CFR 目录失败 {}: {}", dir.display(), e))?;
    }
    if tokio::fs::try_exists(path).await.unwrap_or(false) {
        return Ok(path);
    }
    let tmp = path.with_extension("jar.tmp");
    tokio::fs::write(&tmp, CFR_JAR).await.map_err(|e| format!("写入 CFR jar 失败: {}", e))?;
    if let Err(e) = tokio::fs::rename(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        // 并发部署：另一个任务已抢先完成 rename → 目标存在即视为成功
        if !tokio::fs::try_exists(path).await.unwrap_or(false) {
            return Err(format!("部署 CFR jar 失败: {}", e));
        }
    }
    Ok(path)
}

/// 反编译 class 文件字节码为 Java 源码
pub async fn decompile_class_bytes(bytes: &[u8]) -> Result<String, String> {
    let jar = ensure_cfr_jar().await?;

    // CFR 只能读文件路径，写临时 class 文件（同样放在私有目录；文件名随机，
    // 原实现用固定递增序号，多实例/并发下会互相覆盖且可被同用户进程预置）
    let class_path = cfr_dir().join(format!("decompile-{}.class", uuid::Uuid::new_v4()));
    tokio::fs::write(&class_path, bytes).await.map_err(|e| format!("写入临时 class 失败: {}", e))?;

    let mut cmd = tokio::process::Command::new("java");
    cmd.arg("-jar")
        .arg(jar)
        .arg("--silent").arg("true")
        .arg("--showversion").arg("false")
        .arg(&class_path)
        .stdin(Stdio::null())
        // kill_on_drop：timeout 丢弃 future 时 Child 被 drop → 自动终止 java 进程，
        // 避免反复打开卡死的 class 堆积僵尸 JVM
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let output = tokio::time::timeout(DECOMPILE_TIMEOUT, cmd.output())
        .await
        .map_err(|_| "反编译超时（>15s），已终止 java 进程".to_string());

    // 无论成败都清理临时文件
    let _ = tokio::fs::remove_file(&class_path).await;

    match output {
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
