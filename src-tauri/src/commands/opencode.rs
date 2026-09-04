//! OpenCode 助手（内嵌官方 Web UI）
//!
//! 思路：把 `opencode serve`（本地 HTTP 服务，自带 Web UI）作为受管子进程跑，
//! 前端用 iframe 嵌入其页面 —— 不需要 PTY/终端模拟，也不需要重写聊天 UI。
//! 与旧 Claude 终端（xterm + portable-pty）本质不同：这里是 HTTP + 进程生命周期管理。
//!
//! 单例设计：同一时间只跑一个会话（与旧 PTY 状态一致），切项目 = 重启服务。
//!
//! 二进制来源（build_launcher 依次探测）：
//!   1. 自定义路径（设置里指定）
//!   2. PATH 原生 opencode（npm 全局安装等，可直接 CreateProcess）
//!   3. 自管二进制（应用数据目录，由 opencode_download_latest 安装，可点「更新」）
//!   4. npx 兜底（cmd /C npx -y opencode-ai@latest，需 PATH 有 node）
//! 全部不可用 → NEED_BOOTSTRAP 错误（前端据此显示「下载并启动」按钮，纯 curl 安装，
//! 不依赖 node/npm/PATH）。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::core::process::kill_process_tree;

/// 自管二进制固定位置（应用数据目录，更新时原地替换）
fn managed_exe_path(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("nexus"))
        .join("tools")
        .join("opencode")
        .join("opencode.exe")
}

/// 一次会话：serve 子进程（端口与自管标记已在启动成功时通过回执返回给前端）
pub struct OpenCodeSession {
    child: Child,
}

impl OpenCodeSession {
    /// 终止会话：taskkill /T /F 杀整棵进程树（cmd → npx → node → opencode.exe）→ 带超时等待退出
    fn kill(&mut self) {
        kill_process_tree(self.child.id());
        // 进程树被杀后管道必然 EOF，wait 不会无限阻塞（沿用全局审查学到的 wait 纪律）
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
                _ => {
                    log::warn!("[nexus] opencode 进程 {} 等待退出超时", self.child.id());
                    break;
                }
            }
        }
    }
}

/// 全局 OpenCode 状态（AppState 持有，单例）
pub struct OpenCodeState {
    session: Mutex<Option<OpenCodeSession>>,
}

impl OpenCodeState {
    pub fn new() -> Self {
        Self { session: Mutex::new(None) }
    }

    /// 清理会话（幂等：空会话 no-op）。CloseRequested / Exit / 切换会话共用
    pub fn cleanup(&self) {
        let mut guard = self.session.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(mut s) = guard.take() {
            s.kill();
            log::info!("[nexus] OpenCode 会话已清理");
        }
    }
}

/// start 成功后的回执（前端据此拼 iframe URL；managed = 用的是自管二进制 → 可显示「更新」）
#[derive(Serialize)]
pub struct OpenCodeInfo {
    pub port: u16,
    pub managed: bool,
}

/// 启动方式
enum LaunchKind {
    /// 原生 exe 直启（PATH 找到 / 自定义路径 / 自管二进制）
    Direct(String),
    /// .cmd shim 或 npx 走 cmd /C 包装（启动慢、依赖 PATH，能直启就不走这里）
    Cmd(String),
}

struct Launcher {
    kind: LaunchKind,
    managed: bool,
}

/// 探测某命令在 PATH 是否可用（spawn 一次 --version 立即退出）
fn probe_in_path(cmd: &str) -> bool {
    let mut c = Command::new(cmd);
    c.arg("--version").stdout(Stdio::null()).stderr(Stdio::null());
    c.spawn().map(|mut c| { let _ = c.wait(); }).is_ok()
}

/// 探测 PATH 中是否有原生 opencode.exe（npm 全局安装 bin 即原生 exe）
fn path_has_opencode() -> bool {
    let mut c = Command::new("opencode");
    c.arg("--version").stdout(Stdio::null()).stderr(Stdio::null());
    match c.spawn() {
        Ok(mut c) => { let _ = c.wait(); true }
        Err(_) => false,
    }
}

fn build_launcher(custom: Option<String>, app: &AppHandle) -> Result<Launcher, String> {
    // 1. 自定义路径（.cmd/.bat shim 走 cmd 包装，其余直启）
    if let Some(p) = custom {
        let p = p.trim().trim_matches('"').to_string();
        let lower = p.to_ascii_lowercase();
        let managed = p == managed_exe_path(app).to_string_lossy();
        return Ok(Launcher {
            kind: if lower.ends_with(".cmd") || lower.ends_with(".bat") {
                LaunchKind::Cmd(p)
            } else {
                LaunchKind::Direct(p)
            },
            managed,
        });
    }

    // 2. PATH 原生 opencode
    if path_has_opencode() {
        return Ok(Launcher { kind: LaunchKind::Direct("opencode".to_string()), managed: false });
    }

    // 3. 自管二进制（上次「下载并启动」装的）
    let managed = managed_exe_path(app);
    if managed.is_file() {
        return Ok(Launcher { kind: LaunchKind::Direct(managed.to_string_lossy().to_string()), managed: true });
    }

    // 4. npx 兜底（未安装任何可直启二进制时）
    if probe_in_path("npx") {
        return Ok(Launcher { kind: LaunchKind::Cmd("npx -y opencode-ai@latest".to_string()), managed: false });
    }

    // 5. 全都没有 → 简短的缺失提示（前缀供前端分流到「下载并启动」，详情文案放面板按钮旁）
    Err("NEED_BOOTSTRAP: 未检测到 OpenCode，无法启动服务。".to_string())
}

/// 启动单例 `opencode serve`：先杀旧会话 → 选空闲端口 → spawn → 轮询 /health 就绪
///
/// 前端嵌入地址固定为 http://127.0.0.1:{port}（serve 默认只绑本机回环，无需鉴权）
#[tauri::command]
pub async fn opencode_start(
    app: AppHandle,
    state: State<'_, crate::AppState>,
    cwd: String,
    opencode_path: Option<String>,
) -> Result<OpenCodeInfo, String> {
    // cwd 必须真实存在（与旧 PTY 逻辑一致：无效目录启动即失败且难排查）
    if !std::path::Path::new(&cwd).is_dir() {
        return Err(format!("工作目录不存在: {}", cwd));
    }

    // 单例：切换项目/重复启动时先终止旧会话
    state.opencode.cleanup();

    let launcher = build_launcher(opencode_path, &app)?;

    // 选空闲端口：bind 0 拿端口后立即释放（单用户桌面工具，竞争窗口可忽略）
    let port = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|e| format!("分配端口失败: {}", e))?
        .local_addr()
        .map_err(|e| format!("读取端口失败: {}", e))?
        .port();

    let stderr_tail = Arc::new(Mutex::new(String::new()));
    let mut child = match &launcher.kind {
        LaunchKind::Direct(prog) => spawn_direct(prog, &cwd, port, &stderr_tail),
        LaunchKind::Cmd(cmdline) => spawn_via_cmd(cmdline, &cwd, port, &stderr_tail),
    }?;

    // 等 /health 起来（首启经 npx 可能需下载，预算放宽到 30s）
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            let tail = stderr_tail.lock().map(|s| s.clone()).unwrap_or_default();
            kill_process_tree(child.id());
            return Err(format!(
                "opencode 进程启动后立即退出 (exit={})。{}",
                status.code().map(|c| c.to_string()).unwrap_or_else(|| "无退出码".into()),
                if tail.trim().is_empty() { "请确认 opencode 可正常启动".to_string() } else { tail }
            ));
        }
        if health_ok(port) {
            break;
        }
        if Instant::now() >= deadline {
            kill_process_tree(child.id());
            return Err("opencode 服务 30s 内未就绪（首次 npx 启动需联网下载，可重试）".to_string());
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    let mut guard = state.opencode.session.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(OpenCodeSession { child });
    log::info!("[nexus] opencode 已就绪 (port={}, cwd={}, managed={})", port, cwd, launcher.managed);
    Ok(OpenCodeInfo { port, managed: launcher.managed })
}

/// 关闭会话（关闭面板/退出应用时）
#[tauri::command]
pub fn opencode_stop(state: State<'_, crate::AppState>) -> Result<(), String> {
    state.opencode.cleanup();
    Ok(())
}

/// 下载并安装最新版自管二进制（「下载并启动」/「更新」按钮）
///
/// 流程：查 GitHub latest release → curl 下载 windows zip → zip 解出 opencode.exe →
/// 临时文件验证可运行（--version）→ 原子替换到应用数据目录。
/// force=true 时即使已存在也重新下载（更新）。
/// 下载任务可能数分钟 → async + spawn_blocking，不阻塞 IPC/UI
#[tauri::command]
pub async fn opencode_download_latest(app: AppHandle, force: Option<bool>) -> Result<DownloadResult, String> {
    let handle = app.clone();
    let force = force.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || download_latest_inner(&handle, force))
        .await
        .map_err(|e| format!("下载任务异常终止: {}", e))?
}

fn download_latest_inner(app: &AppHandle, force: bool) -> Result<DownloadResult, String> {
    let dir = managed_exe_path(app)
        .parent()
        .expect("managed exe 必在子目录")
        .to_path_buf();
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建安装目录失败: {}", e))?;

    let exe_path = managed_exe_path(app);
    if !force && exe_path.is_file() {
        // 已装且非强制更新 → 直接复用（版本从二进制读出）
        let v = read_version(&exe_path).unwrap_or_else(|| "已安装".to_string());
        return Ok(DownloadResult { path: exe_path.to_string_lossy().to_string(), version: v });
    }

    // 直链下载（releases/latest/download 走文件重定向，不占 GitHub API 限流配额）。
    // 仓库 sst/opencode 已 301 迁至 anomalyco/opencode，两个都试；
    // 资产命名 1.18 起为连字符 opencode-windows-x64.zip（旧版下划线兜底）
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
    let candidates = [
        format!("https://github.com/sst/opencode/releases/latest/download/opencode-windows-{}.zip", arch),
        format!("https://github.com/sst/opencode/releases/latest/download/opencode_windows_{}.zip", arch),
        format!("https://github.com/anomalyco/opencode/releases/latest/download/opencode-windows-{}.zip", arch),
        format!("https://github.com/anomalyco/opencode/releases/latest/download/opencode_windows_{}.zip", arch),
    ];

    let zip_path = dir.join("opencode.zip.tmp");
    let exe_tmp = dir.join("opencode.exe.tmp");
    let _ = std::fs::remove_file(&zip_path);
    let _ = std::fs::remove_file(&exe_tmp);

    // curl.exe：Windows 10+ 系统自带，无需 node/npm/PATH 环境
    if !probe_in_path("curl.exe") {
        return Err("系统缺少 curl.exe（Windows 10+ 自带），无法自动下载。可手动 npm i -g opencode-ai@latest 后重试".to_string());
    }
    let zip_path_str = zip_path.to_string_lossy().to_string();
    let mut last_code = None;
    for url in &candidates {
        let st = Command::new("curl.exe")
            .args(["-fSL", "--retry", "2", "-o", &zip_path_str, url])
            .status()
            .map_err(|e| format!("启动 curl 失败: {}", e))?;
        if st.success() {
            last_code = None; // 下载成功
            break;
        }
        last_code = st.code();
        let _ = std::fs::remove_file(&zip_path);
    }
    if last_code.is_some() {
        return Err(format!("下载 opencode 安装包失败（curl exit={:?}）。请检查网络或稍后重试", last_code));
    }

    // 解压出 exe（zip 顶层一般是 opencode.exe，遍历找第一个 .exe 更稳）
    let file = std::fs::File::open(&zip_path).map_err(|e| format!("打开下载文件失败: {}", e))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("解压 opencode 失败: {}", e))?;
    let mut found = None;
    for i in 0..archive.len() {
        let name = archive.by_index(i).map(|f| f.name().to_string()).unwrap_or_default();
        if name.to_ascii_lowercase().ends_with(".exe") {
            found = Some(i);
            break;
        }
    }
    let Some(idx) = found else {
        let _ = std::fs::remove_file(&zip_path);
        return Err("下载的压缩包中未找到 opencode.exe".to_string());
    };
    {
        let mut entry = archive.by_index(idx).map_err(|e| format!("读取压缩条目失败: {}", e))?;
        let mut out = std::fs::File::create(&exe_tmp).map_err(|e| format!("写入 opencode.exe 失败: {}", e))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| format!("解压失败: {}", e))?;
    }
    drop(archive);
    let _ = std::fs::remove_file(&zip_path);

    // 验证可运行，通过才替换（失败的下载不污染可用旧版）
    let version = read_version(&exe_tmp).ok_or_else(|| {
        let _ = std::fs::remove_file(&exe_tmp);
        "下载的 opencode 无法运行（可能架构不匹配），已保留现有版本".to_string()
    })?;
    std::fs::rename(&exe_tmp, &exe_path).map_err(|e| {
        let _ = std::fs::remove_file(&exe_tmp);
        format!("安装 opencode 失败: {}", e)
    })?;

    log::info!("[nexus] opencode {} 已安装到 {}", version, exe_path.display());
    Ok(DownloadResult { path: exe_path.to_string_lossy().to_string(), version })
}

#[derive(Serialize)]
pub struct DownloadResult {
    pub path: String,
    pub version: String,
}

/// 运行 exe --version 取首行（验证可用 + 拿版本号）
fn read_version(exe: &std::path::Path) -> Option<String> {
    let mut c = Command::new(exe);
    c.arg("--version").stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x08000000);
    }
    let out = c.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Some(text.lines().next().unwrap_or("").trim().to_string()).filter(|s| !s.is_empty())
}

/// spawn：原生 exe 直接 CreateProcess
fn spawn_direct(
    prog: &str,
    cwd: &str,
    port: u16,
    stderr_tail: &Arc<Mutex<String>>,
) -> Result<Child, String> {
    let mut c = Command::new(prog);
    c.args(["serve", "--port", &port.to_string(), "--hostname", "127.0.0.1"])
        .current_dir(cwd)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x08000000); // CREATE_NO_WINDOW：不弹控制台窗口
    }
    let mut child = c.spawn().map_err(|e| format!("启动 opencode 失败: {}", e))?;
    drain_stderr(child.stderr.take(), stderr_tail);
    Ok(child)
}

/// spawn：.cmd shim / npx 走 cmd /C 包装（CreateProcess 无法直启批处理）
fn spawn_via_cmd(
    cmdline: &str,
    cwd: &str,
    port: u16,
    stderr_tail: &Arc<Mutex<String>>,
) -> Result<Child, String> {
    // 进程链 cmd → npx(node) → opencode.exe：结束时会用 taskkill /T 整树清理
    let full = format!("{} serve --port {} --hostname 127.0.0.1", cmdline, port);
    let mut c = Command::new("cmd");
    c.args(["/C", &full])
        .current_dir(cwd)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = c
        .spawn()
        .map_err(|e| format!("通过 cmd 启动 opencode 失败: {}", e))?;
    drain_stderr(child.stderr.take(), stderr_tail);
    Ok(child)
}

/// stderr 收集线程：读进共享缓冲（保留尾 4KB），EOF（进程树被杀）自动退出
fn drain_stderr(pipe: Option<std::process::ChildStderr>, tail: &Arc<Mutex<String>>) {
    if let Some(pipe) = pipe {
        let tail = tail.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 1024];
            let mut reader = pipe;
            let mut collected = String::new();
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                collected.push_str(&String::from_utf8_lossy(&buf[..n]));
                if collected.len() > 4096 {
                    collected.drain(..collected.len() - 4096);
                }
            }
            if let Ok(mut t) = tail.lock() {
                *t = collected;
            }
        });
    }
}

/// 极简 GET /health（无 HTTP 依赖）：就绪返回 true
fn health_ok(port: u16) -> bool {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(600)));
    let req = format!("GET /health HTTP/1.0\r\nHost: 127.0.0.1:{}\r\n\r\n", port);
    if stream.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut resp = [0u8; 64];
    if stream.read(&mut resp).is_err() {
        return false;
    }
    String::from_utf8_lossy(&resp).starts_with("HTTP/1.1 200")
}
