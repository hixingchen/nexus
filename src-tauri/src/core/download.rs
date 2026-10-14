//! 用**系统自带的 curl** 下载文件（不引 HTTP 客户端）。
//!
//! **为什么是 curl 而不是 reqwest 之类**：Rust 侧至今没有 HTTP 依赖，而需要下载的东西
//! 只有三处——nvm 安装器（GitHub release 直链）、Node 版本索引（nvm 镜像上的 `index.json`）、
//! AI 私有运行时的 Node zip。前两处的地址前端也够不着（CSP 只放开了 api.github.com），
//! 引一个客户端只为下这三个文件不划算。Windows 10 1803 起自带 `curl.exe`。
//!
//! 本模块是从 `commands/node.rs` 提取的（PDF-1 规则三：run_curl 第 3 次被用到）。
//! 提取时行为**逐字保持不变**，包括 curl 的参数与错误文案——那几条是被事故打磨出来的。

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// Windows 自带的 curl（Win10 1803 起内置）。
///
/// 用绝对路径而不是裸名：一是不依赖 PATH（Nexus 启动时的 PATH 未必和用户终端一样），
/// 二是不受 Windows "先搜当前目录"的影响。
pub fn system_curl() -> PathBuf {
    let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    Path::new(&sysroot).join("System32").join("curl.exe")
}

/// curl 调用的公共部分：拼参数、起进程、把非零退出码翻成带地址的错误。
/// `dest` 为 `None` = 不指定 `-o`，curl 把正文写到 stdout。
fn curl_command(url: &str, dest: Option<&Path>, max_time: u32) -> Result<Command, String> {
    let curl = system_curl();
    if !curl.is_file() {
        return Err(format!(
            "找不到 {}（Windows 自带的下载组件）——请手动下载：{}",
            curl.display(),
            url
        ));
    }
    if let Some(dir) = dest.and_then(Path::parent) {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录 {} 失败: {}", dir.display(), e))?;
    }

    let mut cmd = Command::new(&curl);
    // 显式设置收口（spawn_guard 的 GUARD-1）：这里是绝对路径调用，本来就不搜当前目录，
    // 写上是为了让"新增的进程调用"在守卫里是一次明确决定，而不是漏写
    cmd.env("NoDefaultCurrentDirectoryInExePath", "1").args([
        "-L",                 // release 直链会 302，镜像也会 302
        "--fail",             // 4xx/5xx 直接算失败，别把错误页当成正经文件存下来
        "--retry", "3",       // 国内直连偶尔断流，重试几次比让用户重来便宜
        "--retry-delay", "2",
        "--connect-timeout", "20",
        "--max-time",
    ]);
    cmd.arg(max_time.to_string())
        .args(["--silent", "--show-error"]);
    if let Some(d) = dest {
        cmd.arg("-o").arg(d);
    }
    cmd.arg(url);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    Ok(cmd)
}

/// 运行 curl 并把结果原样返回（成功时 stdout/stderr 都在 `Output` 里）。
pub fn run_curl_raw(
    url: &str,
    dest: Option<&Path>,
    max_time: u32,
) -> Result<Output, String> {
    let out = curl_command(url, dest, max_time)?
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("启动下载失败: {}", e))?;
    if !out.status.success() {
        let detail = crate::core::textenc::decode_to_string(&out.stderr);
        return Err(format!(
            "下载失败（curl 退出码 {:?}）：{}\n地址：{}",
            out.status.code(),
            detail.trim(),
            url
        ));
    }
    Ok(out)
}

/// 把一个 URL 下到本地文件（`max_time` 是整体超时，单位秒）。
///
/// 失败时把 URL 一起写进错误里：这几条地址有的来自常量、有的来自用户的 `settings.txt`，
/// 出问题时第一件要知道的事就是"到底去哪个地址取的"。
pub fn run_curl(url: &str, dest: &Path, max_time: u32) -> Result<(), String> {
    run_curl_raw(url, Some(dest), max_time).map(|_| ())
}

/// 同上，但结果**直接从 stdout 取**，不落任何文件（CQ-43）。
///
/// 为什么要有这一支：Node 版本索引原先下到固定的 `%TEMP%\nexus\node-index.json` 再读回来，
/// 而两条并发调用（关掉面板再打开就会触发）会同时写它——curl 截断重写时另一路读到半截，
/// 报出来是"版本索引不是预期的 JSON"这种假错误。索引本来就只是一次性解析，不必落盘。
pub fn run_curl_stdout(url: &str, max_time: u32) -> Result<Vec<u8>, String> {
    run_curl_raw(url, None, max_time).map(|out| out.stdout)
}

/// 下载到文件，过程中回调**已落盘的字节数**（用来给界面报进度）。
///
/// 为什么要边下边报：AI 私有运行时首次要下 36MB 的 Node + 269MB 的 dsh，而**没有进度的
/// 转圈和"卡死"是分不出来的**——用户这次整场困惑的底色就是"不知道它在干嘛"。curl 的
/// `--progress-bar` 写在 stderr 上、格式不稳定，不如直接轮询目标文件大小：它天然反映
/// "已经存下来多少"，重定向、断点、镜像 302 都不影响这个口径。
///
/// 轮询间隔 300ms：对肉眼足够连续，又不会为了几个字节把 CPU 烧起来。
pub fn download_with_progress(
    url: &str,
    dest: &Path,
    max_time: u32,
    on_progress: &dyn Fn(u64),
) -> Result<(), String> {
    let mut child = curl_command(url, Some(dest), max_time)?
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        // stderr 必须收走：curl 出错时把原因写在这里，而它正是我们要回给界面的"原话"
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动下载失败: {}", e))?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(max_time as u64 + 30);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                on_progress(std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0));
                if std::time::Instant::now() >= deadline {
                    // curl 自己也有 --max-time 兜底，这里只是防"它没退干净"
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("下载超时（超过 {} 秒）：{}", max_time, url));
                }
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
            Err(e) => return Err(format!("等待下载结束失败: {}", e)),
        }
    };

    let mut detail = String::new();
    if let Some(mut err) = child.stderr.take() {
        use std::io::Read;
        let _ = err.read_to_string(&mut detail);
    }
    if !status.success() {
        return Err(format!(
            "下载失败（curl 退出码 {:?}）：{}\n地址：{}",
            status.code(),
            detail.trim(),
            url
        ));
    }
    // 收尾再报一次：最后一次轮询可能停在"还差最后几 MB"的那一刻
    on_progress(std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0));
    Ok(())
}
