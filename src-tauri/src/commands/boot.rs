//! 开机启动器的安装/查询/控制命令。
//!
//! - 计划任务用 **S4U**（“无论用户是否登录都运行”+“不存储密码”）：PIN/微软账户机器
//!   也能在登录前运行，且不落密码。
//! - 任务只保存一个 action（`nexus-boot-runner.exe --manifest ...`）；具体启动哪些
//!   服务在 `core::boot` 的 manifest 里，所以同名服务、增删服务都不会改任务定义。
//! - 启动器是**独立 exe**：Tauri externalBin 先把它放到 Nexus.exe 旁边，安装任务时
//!   再提权复制到 `%ProgramData%\Nexus\boot\runner\`。任务指向稳定路径，GUI 更新不会
//!   因为 runner 正在运行而锁住 Nexus.exe。
//! - 安装/卸载要提权：用 `ShellExecuteExW runas` 拉起 `cmd.exe`/`schtasks.exe`，
//!   只传固定参数，不接受前端指定的路径或任务 XML。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::{Manager, State};

use crate::core::boot::{self, BootCommand, BootMode, BootServiceState};
use crate::core::logpipe::LogLine;
use crate::core::process::ProcessProbe;
use crate::AppState;

/// 任务名前缀；实际名字带用户名，避免同一台机器上多个 Windows 账户撞任务。
/// 不用 `Nexus\...` 文件夹形式：`schtasks /Create` 在文件夹不存在时不会自动创建。
const TASK_PREFIX: &str = "Nexus Boot Runner";

#[derive(Debug, Clone, Serialize)]
pub struct BootRunnerStatus {
    /// s4u / password
    pub mode: String,
    /// 计划任务是否已注册
    pub installed: bool,
    pub task_name: String,
    /// 回读到的登录类型（S4U / Password / 空）
    pub task_logon_type: String,
    /// 任务 action 是否指向稳定 runner 路径 + 当前 manifest
    pub task_path_ok: bool,
    /// 稳定目录里的 runner 文件路径（任务 action 指向它）
    pub runner_path: String,
    /// 稳定目录里的 runner 文件是否存在
    pub runner_installed: bool,
    /// 状态文件里的 runner 是否还活着且心跳新鲜
    pub runner_reachable: bool,
    /// runner 进程是否还在（不要求心跳）：任务卸载后残留实例的判断依据
    pub runner_alive: bool,
    /// 当前运行实例的模式（state.json 的 mode；任务模式可能已切换但实例还没重启）
    pub running_mode: String,
    pub runner_pid: Option<u32>,
    pub heartbeat_at: String,
    /// DB 里 boot_enabled=1 且 enabled=1 的服务数
    pub boot_service_count: usize,
    /// not_installed / installed_stopped / stopped / running / stale / exited
    pub state: String,
    pub message: String,
}

/// 单个开机服务的状态（设置页逐服务列表用）。
///
/// 与 `get_running` 的区别：这里**不要求启动器可达**，phase 是 state.json 的原始记录，
/// `alive` 由 pid + 启动时刻独立校验——启动器退出后仍能说清“这个服务还在不在跑”。
#[derive(Debug, Clone, Serialize)]
pub struct BootServiceStatus {
    pub service_id: String,
    pub name: String,
    pub project_id: String,
    /// s4u / password / legacy（旧版单任务）
    pub mode: String,
    /// state.json 的 phase；空串 = DB 里配置了开机启动但启动器从未记录过它
    pub phase: String,
    /// 进程是否仍活着（pid + 启动时刻校验，不依赖 runner 心跳）
    pub alive: bool,
    pub pid: Option<u32>,
    pub started_at: String,
    pub ready_at: String,
    pub exit_code: Option<i32>,
    pub error: String,
    pub log_path: String,
    /// 该来源的启动器是否存活且心跳新鲜
    pub runner_reachable: bool,
    /// 该来源的启动器进程是否还在（不要求心跳）
    pub runner_alive: bool,
    /// 当前 DB 是否仍配置为开机启动
    pub configured: bool,
    /// DB 里服务的「跟随项目启动」开关；false 时 manifest 会排除它（开机不会启动）
    pub service_enabled: bool,
    /// 服务本身是否还在 DB 里（false = 服务已删除；若进程还活着，列表只保留停止/日志入口）
    pub service_exists: bool,
    /// 服务所属项目是否还在 DB 里（false = 项目已删除）
    pub project_exists: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BootToggleResult {
    pub boot_enabled: bool,
    /// manifest 同步/启动器通知失败的原因（空 = 成功）
    pub boot_manifest_error: String,
}

// ─── 任务名 / XML ────────────────────────────────────────────

fn sanitize_user_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() { "user".to_string() } else { trimmed.to_string() }
}

fn current_user() -> (String, String) {
    let domain = std::env::var("USERDOMAIN").unwrap_or_default();
    let user = std::env::var("USERNAME").unwrap_or_default();
    (domain, user)
}

/// 从 `whoami /user /fo csv /nh` 的输出里取 SID。
///
/// 为什么任务用 SID 而不是 `DOMAIN\user`：微软账户 / AAD 账户下，`USERDOMAIN\USERNAME`
/// 未必能映射到安全标识（实测 S4U 任务会报“帐户名与安全标识间无任何映射完成”）。
/// SID 是唯一的，Task Scheduler 的 `<UserId>` 也接受它。
fn parse_whoami_sid(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let sid = line.rsplit(',').next()?.trim().trim_matches('"');
        (!sid.is_empty() && sid.starts_with("S-1-")).then(|| sid.to_string())
    })
}

fn current_user_sid() -> Option<String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new(system_dir().join("whoami.exe"))
            .args(["/user", "/fo", "csv", "/nh"])
            .env("NoDefaultCurrentDirectoryInExePath", "1")
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        parse_whoami_sid(&decode_output(&out.stdout))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn user_suffix() -> String {
    let (_, user) = current_user();
    // 带上 SID 尾号：同名的本地账户与域账户在同一台机器上不会撞任务名
    let sid_suffix = current_user_sid()
        .and_then(|sid| sid.rsplit('-').next().map(|s| s.to_string()))
        .filter(|s| !s.is_empty());
    match sid_suffix {
        Some(suffix) => format!("{}-{}", sanitize_user_name(&user), suffix),
        None => sanitize_user_name(&user),
    }
}

/// System32 的**可信**路径。
///
/// 提权助手（cmd/powershell/schtasks）的路径不能读 `SystemRoot` 环境变量：攻击者
/// 可以用改过的环境启动 Nexus，诱导用户对同目录下的假程序确认 UAC。`GetSystemDirectoryW`
/// 由系统直接给出，不经过进程环境。
#[cfg(windows)]
fn system_dir() -> PathBuf {
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
    let mut buf = vec![0u16; 261];
    loop {
        let n = unsafe { GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) };
        if n == 0 {
            break;
        }
        if (n as usize) < buf.len() {
            return PathBuf::from(String::from_utf16_lossy(&buf[..n as usize]));
        }
        buf.resize(n as usize + 1, 0);
    }
    PathBuf::from(r"C:\Windows\System32")
}

#[cfg(not(windows))]
fn system_dir() -> PathBuf {
    PathBuf::from("/usr/bin")
}

/// Windows 已知文件夹（不使用可被启动环境篡改的 `%ProgramData%`）。
#[cfg(windows)]
fn known_folder(id: &windows_sys::core::GUID) -> Option<PathBuf> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::SHGetKnownFolderPath;
    unsafe {
        let mut ptr: *mut u16 = std::ptr::null_mut();
        let hr = SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut ptr);
        if hr < 0 || ptr.is_null() {
            return None;
        }
        let mut len = 0usize;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
        CoTaskMemFree(ptr as *const _);
        Some(PathBuf::from(text))
    }
}

fn program_data_dir() -> PathBuf {
    // 状态页每 5 秒查一次，SHGetKnownFolderPath 会做一次 COM 分配；缓存即可
    static CACHE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        #[cfg(windows)]
        if let Some(p) = known_folder(&windows_sys::Win32::UI::Shell::FOLDERID_ProgramData) {
            return p;
        }
        std::env::var("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(r"C:\ProgramData"))
    }).clone()
}

/// 每个凭据模式一个任务名（同一个任务名不能同时是 S4U 和 Password）。
pub fn task_name(mode: BootMode) -> String {
    let label = match mode {
        BootMode::S4u => "S4U",
        BootMode::Password => "Password",
    };
    format!("{} {} ({})", TASK_PREFIX, label, user_suffix())
}

/// 1.10.x 之前的单任务名；安装新模式时清掉它，避免开机时两套任务同时拉起服务。
fn legacy_task_name() -> String {
    format!("{} ({})", TASK_PREFIX, user_suffix())
}

/// 稳定安装位置：`%ProgramData%\Nexus\boot\runner\nexus-boot-runner.exe`。
///
/// 放在 GUI 安装目录之外，GUI 更新（替换 Nexus.exe 及同目录文件）不会碰到正在运行的
/// runner；任务 action 也就不用随安装路径变化。runner 以当前用户 S4U 运行，读取/执行
/// 这个文件即可（ProgramData 默认对 Users 有读/执行权限）。
pub fn runner_install_path() -> PathBuf {
    program_data_dir()
        .join("Nexus")
        .join("boot")
        .join("runner")
        .join("nexus-boot-runner.exe")
}

/// 安装包里的 runner（Tauri externalBin 放在 Nexus.exe 旁边）。
fn bundled_runner_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("获取当前程序路径失败: {}", e))?;
    let dir = exe.parent().ok_or_else(|| "当前程序没有父目录".to_string())?;
    let path = dir.join("nexus-boot-runner.exe");
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if !path.is_file() || size == 0 {
        return Err(format!(
            "当前安装包没有包含有效的开机启动器（{}）。请重新安装 Nexus，或开发时先运行 `npm run build:runner:debug`",
            path.display()
        ));
    }
    Ok(path)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn task_xml(
    exe: &std::path::Path,
    manifest: &std::path::Path,
    mode: &str,
) -> Result<String, String> {
    let (logon_type, description) = match mode {
        "s4u" => ("S4U", "Nexus 服务开机启动器（登录前运行，不保存密码）"),
        "password" => ("Password", "Nexus 服务开机启动器（登录前运行，凭据保存在 Windows 计划任务）"),
        other => return Err(format!("未知的开机启动模式: {}", other)),
    };
    let (domain, user) = current_user();
    // 优先用 SID：微软账户/AAD 下 `DOMAIN\user` 可能无法映射到安全标识
    let user_id = current_user_sid().unwrap_or_else(|| {
        if domain.is_empty() {
            user
        } else {
            format!("{}\\{}", domain, user)
        }
    });
    if user_id.trim().is_empty() {
        return Err("无法确定当前 Windows 用户，不能创建开机启动任务".into());
    }
    let exe = exe.to_string_lossy().to_string();
    let manifest = manifest.to_string_lossy().to_string();
    let working_dir = exe.rsplit_once('\\').map(|(d, _)| d).unwrap_or("").to_string();
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Author>Nexus</Author>
    <Description>{description_escaped}</Description>
  </RegistrationInfo>
  <Triggers>
    <BootTrigger>
      <Enabled>true</Enabled>
      <Delay>PT20S</Delay>
    </BootTrigger>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user_id}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user_id}</UserId>
      <LogonType>{logon_type}</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Enabled>true</Enabled>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>3</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe_escaped}</Command>
      <Arguments>--mode {mode_escaped} --manifest "{manifest_escaped}"</Arguments>
      <WorkingDirectory>{work_escaped}</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"#,
        user_id = xml_escape(&user_id),
        logon_type = logon_type,
        mode_escaped = xml_escape(mode),
        description_escaped = xml_escape(description),
        exe_escaped = xml_escape(&exe),
        manifest_escaped = xml_escape(&manifest),
        work_escaped = xml_escape(&working_dir),
    ))
}

// ─── schtasks 调用 ───────────────────────────────────────────

fn schtasks_path() -> String {
    system_dir().join("schtasks.exe").to_string_lossy().to_string()
}

fn decode_output(bytes: &[u8]) -> String {
    // schtasks /XML 在 Windows 上常以 UTF-16LE（带 BOM）输出；普通查询则可能是
    // 控制台代码页。这里两种都兜住，避免把 XML 解析成一串乱码。
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        String::from_utf16_lossy(&units)
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        crate::core::textenc::decode_to_string(bytes)
    }
}

fn run_schtasks(args: &[&str]) -> Result<(bool, String), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new(schtasks_path())
            .args(args)
            // 内部命令收口（SEC-14）：不把当前目录当可执行搜索路径
            .env("NoDefaultCurrentDirectoryInExePath", "1")
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .output()
            .map_err(|e| format!("运行 schtasks 失败: {}", e))?;
        let mut text = decode_output(&out.stdout);
        if !out.stderr.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&decode_output(&out.stderr));
        }
        Ok((out.status.success(), text))
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        Err("开机启动仅 Windows 支持".into())
    }
}

/// 查询任务 XML；任务不存在时返回 `Ok(None)`。
fn query_task_xml(name: &str) -> Result<Option<String>, String> {
    let (ok, text) = run_schtasks(&["/Query", "/TN", name, "/XML"])?;
    if ok { Ok(Some(text)) } else { Ok(None) }
}

fn tag_text(xml: &str, tag: &str) -> String {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let Some(start) = xml.find(&open) else { return String::new() };
    let rest = &xml[start + open.len()..];
    let Some(end) = rest.find(&close) else { return String::new() };
    rest[..end].trim().to_string()
}

fn quote_arg(s: &str) -> String {
    if s.contains(' ') || s.contains('\t') {
        format!("\"{}\"", s)
    } else {
        s.to_string()
    }
}

/// 提权运行一个**固定**的内部命令（`schtasks.exe` / `cmd.exe` 的复制动作）。
#[cfg(windows)]
fn elevated_run(file_path: &str, params: &str) -> Result<(), String> {
    use std::ffi::c_void;

    type Handle = isize;
    #[repr(C)]
    struct ShellExecuteInfoW {
        cb_size: u32,
        f_mask: u32,
        hwnd: Handle,
        lp_verb: *const u16,
        lp_file: *const u16,
        lp_parameters: *const u16,
        lp_directory: *const u16,
        n_show: i32,
        h_inst_app: Handle,
        lp_id_list: *mut c_void,
        lp_class: *const u16,
        hkey_class: Handle,
        dw_hot_key: u32,
        h_icon: Handle,
        h_process: Handle,
    }
    const SEE_MASK_NOCLOSEPROCESS: u32 = 0x0000_0040;
    const SW_HIDE: i32 = 0;
    const INFINITE: u32 = 0xFFFF_FFFF;
    const COINIT_APARTMENTTHREADED: u32 = 0x2;

    extern "system" {
        fn ShellExecuteExW(info: *mut ShellExecuteInfoW) -> i32;
        fn WaitForSingleObject(h: Handle, ms: u32) -> u32;
        fn GetExitCodeProcess(h: Handle, code: *mut u32) -> i32;
        fn CloseHandle(h: Handle) -> i32;
    }
    #[link(name = "ole32")]
    extern "system" {
        fn CoInitializeEx(pv: *mut c_void, coinit: u32) -> i32;
        fn CoUninitialize();
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    // ShellExecuteEx 要求调用线程已初始化 COM；RPC_E_CHANGED_MODE 表示已以别的模式
    // 初始化过，仍可继续，但不能再 CoUninitialize。
    let hr = unsafe { CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED) };
    let should_uninit = hr == 0 || hr == 1; // S_OK / S_FALSE

    let file = wide(file_path);
    let verb = wide("runas");
    let params_w = wide(params);
    let mut info: ShellExecuteInfoW = unsafe { std::mem::zeroed() };
    info.cb_size = std::mem::size_of::<ShellExecuteInfoW>() as u32;
    info.f_mask = SEE_MASK_NOCLOSEPROCESS;
    info.lp_verb = verb.as_ptr();
    info.lp_file = file.as_ptr();
    info.lp_parameters = params_w.as_ptr();
    info.n_show = SW_HIDE;

    let ok = unsafe { ShellExecuteExW(&mut info) };
    if ok == 0 {
        let err = std::io::Error::last_os_error();
        if should_uninit { unsafe { CoUninitialize(); } }
        if err.raw_os_error() == Some(1223) {
            return Err("已取消管理员授权".into());
        }
        return Err(format!("请求管理员权限失败: {}", err));
    }
    if info.h_process != 0 {
        unsafe {
            WaitForSingleObject(info.h_process, INFINITE);
            let mut code: u32 = 0;
            let got = GetExitCodeProcess(info.h_process, &mut code);
            CloseHandle(info.h_process);
            if should_uninit { CoUninitialize(); }
            if got == 0 {
                return Err("等待提权命令退出失败".into());
            }
            if code != 0 {
                return Err(format!("提权命令执行失败（退出码 {}）", code));
            }
        }
    } else if should_uninit {
        unsafe { CoUninitialize(); }
    }
    Ok(())
}

#[cfg(not(windows))]
fn elevated_run(_file_path: &str, _params: &str) -> Result<(), String> {
    Err("开机启动仅 Windows 支持".into())
}

/// 提权创建/删除计划任务（固定动作，只传固定参数）。
fn elevated_schtasks(params: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        elevated_run(&schtasks_path(), params)
    }
    #[cfg(not(windows))]
    {
        let _ = params;
        Err("开机启动仅 Windows 支持".into())
    }
}

/// PowerShell 单引号字符串字面量（内部的 `'` 用 `''` 转义）。
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn sha256_file(path: &std::path::Path) -> Option<[u8; 32]> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Some(hasher.finalize().into())
}

/// 两个文件内容是否一致（长度 + SHA-256）。
///
/// 安装判“需不需要复制”、安装后校验“是否真的复制成功”都用它——只比大小会把
/// 同长度的损坏/替换文件当成一致。
fn files_identical(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (sha256_file(a), sha256_file(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// 安装包侧 runner 的哈希。
///
/// 设置页每 5 秒查一次状态，没必要每次重读 4MB 的 sidecar；它在进程生命周期内
/// 不会变（应用更新会重启进程）。
fn bundled_runner_hash() -> Option<[u8; 32]> {
    static CACHE: std::sync::OnceLock<Option<[u8; 32]>> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| bundled_runner_path().ok().and_then(|p| sha256_file(&p)))
}

/// runner 目录的安装期 ACL 加固：只留 SYSTEM / Administrators 可写，Users 只读+执行。
///
/// 为什么必须做：`C:\ProgramData` 默认给 Users `(CI)(WD,AD)`，新建的子目录会继承
/// ——同一台机器上的其他本地用户可以在 runner 目录里种 DLL，等本用户下次开机时被
/// 登录前启动器加载。这里用 SID（不依赖系统语言）收口。
/// 注意**不要**加 `/T`：`(OI)(CI)` 授权应用到已有文件时会被 OI/CI 语义丢弃，
/// 反而把文件 ACL 清空（实测 `icacls <file>` 无任何条目），任务将无法执行 runner。
/// 这里只改目录；新复制的文件在 ACL 之后落地，会自然继承 Users:(RX)。
const RUNNER_DIR_ACL: &str = "/inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' '*S-1-5-32-545:(OI)(CI)RX'";

/// 原子替换 runner 的 PowerShell 片段。
///
/// 不能直接 `Copy-Item -Force`：另一个模式的 runner 可能正从同一个 exe 运行，
/// Windows 会拒绝覆盖正在执行的镜像（用户遇到的“退出码 1”正是这条）。
/// 但 Windows **允许重命名**正在运行的 exe，所以先把旧文件改名、再把新文件就位。
fn runner_replace_block(src: &std::path::Path) -> String {
    format!(
        "Remove-Item -LiteralPath $new -Force -ErrorAction SilentlyContinue\n\
         Copy-Item -LiteralPath {src} -Destination $new -Force\n\
         if (Test-Path -LiteralPath $dst) {{ Move-Item -LiteralPath $dst -Destination $old -Force }}\n\
         Move-Item -LiteralPath $new -Destination $dst -Force\n\
         Remove-Item -LiteralPath $old -Force -ErrorAction SilentlyContinue\n\
         Get-ChildItem -LiteralPath $dir -Filter 'nexus-boot-runner.exe.old.*' -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue\n",
        src = ps_quote(&src.to_string_lossy()),
    )
}

struct InstallScript<'a> {
    mode: BootMode,
    src: &'a std::path::Path,
    dst: &'a std::path::Path,
    task: &'a str,
    legacy_task: &'a str,
    /// 任务 XML **内容**（直接嵌进脚本）。
    ///
    /// 不再写进用户可写的 `task.xml` 再让提权进程去读：那样同用户进程可以在
    /// UAC 前后把 XML 换成 `<RunLevel>HighestAvailable</RunLevel>` 的任意命令，
    /// 用户点一次“修复”就注册了一个提权任务。
    xml_text: &'a str,
    need_copy: bool,
    error_path: &'a std::path::Path,
}

/// 安装/修复的提权脚本（S4U 与保存密码共用）。
///
/// 为什么用 PowerShell `-EncodedCommand` 而不是 `cmd /c`：cmd 会对路径做 `%VAR%`
/// 展开，且失败时拿不到任何输出。编码脚本里路径走单引号字面量，异常写入错误文件，
/// 调用方能把真正的失败原因带回界面。
fn install_script(spec: &InstallScript) -> String {
    let (domain, user) = current_user();
    let username = if domain.is_empty() { user } else { format!("{}\\{}", domain, user) };
    let copy = if spec.need_copy { runner_replace_block(spec.src) } else { String::new() };
    let register = match spec.mode {
        BootMode::S4u => format!(
            "Register-ScheduledTask -TaskName {task} -Xml $xmlText -Force | Out-Null\n",
            task = ps_quote(spec.task),
        ),
        BootMode::Password => format!(
            "$cred = Get-Credential -UserName {user} -Message 'Nexus 服务开机启动需要 Windows 密码（只保存在 Windows 计划任务中）'\n\
             if ($null -eq $cred) {{ exit 2 }}\n\
             Register-ScheduledTask -TaskName {task} -Xml $xmlText -User $cred.UserName -Password $cred.GetNetworkCredential().Password -Force | Out-Null\n",
            user = ps_quote(&username),
            task = ps_quote(spec.task),
        ),
    };
    format!(
        "$ErrorActionPreference = 'Stop'\n\
         $dst = {dst}\n\
         $dir = Split-Path -Parent $dst\n\
         $boot = Split-Path -Parent $dir\n\
         $nexus = Split-Path -Parent $boot\n\
         $xmlText = {xml}\n\
         $new = \"$($dst).new.\" + [guid]::NewGuid().ToString('N')\n\
         $old = \"$($dst).old.\" + [guid]::NewGuid().ToString('N')\n\
         try {{\n\
           foreach ($p in @($nexus, $boot, $dir)) {{\n\
             if (Test-Path -LiteralPath $p) {{\n\
               $item = Get-Item -LiteralPath $p -Force\n\
               if (-not $item.PSIsContainer) {{ throw \"启动器路径不是目录：$p\" }}\n\
               if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {{ throw \"启动器路径是联接点/符号链接，拒绝写入：$p\" }}\n\
             }} else {{\n\
               New-Item -ItemType Directory -Path $p | Out-Null\n\
             }}\n\
             $aclOut = & icacls $p {acl} 2>&1\n\
             if ($LASTEXITCODE -ne 0) {{ throw \"设置目录权限失败（$p，icacls 退出码 $($LASTEXITCODE)）：$($aclOut -join ' ')\" }}\n\
           }}\n\
           {copy}\
           Unregister-ScheduledTask -TaskName {legacy} -Confirm:$false -ErrorAction SilentlyContinue | Out-Null\n\
           Stop-ScheduledTask -TaskName {task} -ErrorAction SilentlyContinue | Out-Null\n\
           {register}\
         }} catch {{\n\
           $_.Exception.Message | Out-File -FilePath {error} -Encoding utf8\n\
           exit 1\n\
         }}\n",
        dst = ps_quote(&spec.dst.to_string_lossy()),
        xml = ps_quote(spec.xml_text),
        copy = copy,
        acl = RUNNER_DIR_ACL,
        legacy = ps_quote(spec.legacy_task),
        task = ps_quote(spec.task),
        register = register,
        error = ps_quote(&spec.error_path.to_string_lossy()),
    )
}

fn encode_powershell(script: &str) -> String {
    use base64::Engine as _;
    let utf16: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    base64::engine::general_purpose::STANDARD.encode(utf16)
}

/// 执行安装脚本；失败时把 PowerShell 写出的真实异常带回界面。
fn run_install_elevated(spec: &InstallScript) -> Result<(), String> {
    if let Some(parent) = spec.error_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::remove_file(spec.error_path);
    let script = install_script(spec);
    let encoded = encode_powershell(&script);
    let powershell = system_dir().join("WindowsPowerShell").join("v1.0").join("powershell.exe");
    let params = format!("-NoProfile -ExecutionPolicy Bypass -EncodedCommand {}", encoded);
    let result = elevated_run(&powershell.to_string_lossy(), &params);
    // 取消密码输入（exit 2）不会写错误文件
    // Windows PowerShell 5.1 的 Out-File -Encoding utf8 带 BOM，读回来先剥掉
    let detail = std::fs::read_to_string(spec.error_path)
        .ok()
        .map(|s| s.trim_start_matches('\u{feff}').trim().to_string())
        .filter(|s| !s.is_empty());
    let _ = std::fs::remove_file(spec.error_path);
    match result {
        Ok(()) => Ok(()),
        Err(e) if e.contains("退出码 2") => Err("已取消密码输入，开机启动任务未变更".into()),
        Err(e) => match detail {
            Some(d) => Err(format!("{}；命令输出：{}", e, d)),
            None => Err(e),
        },
    }
}

fn delete_params(name: &str) -> String {
    format!("/Delete /TN {} /F", quote_arg(name))
}

// ─── 状态 ────────────────────────────────────────────────────

fn boot_service_count(state: &AppState, mode: BootMode) -> usize {
    state.db.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM services WHERE boot_enabled=1 AND enabled=1 AND boot_credential_mode=?1",
            [mode.as_str()],
            |r| r.get::<_, i64>(0),
        ).map(|n| n as usize).map_err(|e| format!("统计开机启动服务失败: {}", e))
    }).unwrap_or(0)
}

/// 把“是否安装 / 是否可达 / 状态文件”归成界面状态名 + 提示。
///
/// 单独成函数是为了能单测“停止后不再显示运行中”的映射（用户报过的跳变）：
/// - 干净停止（runner 退出时写 pid=0）→ `stopped`；
/// - 进程已消失（崩溃/被强杀）→ `exited`；
/// - 进程还在但心跳过期（卡住）→ `stale`。
fn classify_runner_state(
    installed: bool,
    reachable: bool,
    runner_state: Option<&crate::core::boot::BootRunnerState>,
    mode_label: &str,
) -> (&'static str, String) {
    if !installed {
        return ("not_installed", format!("尚未安装 {} 开机启动器；安装需要一次管理员授权", mode_label));
    }
    if reachable {
        return ("running", format!("{} 启动器运行中，开机服务由它管理", mode_label));
    }
    match runner_state {
        Some(s) if s.runner_pid == 0 => {
            ("stopped", format!("{} 启动器已停止；点「测试启动」可立即拉起", mode_label))
        }
        Some(s) if !boot::runner_process_alive(s) => {
            ("exited", format!("{} 启动器已退出（上次运行状态可能已过期）", mode_label))
        }
        Some(_) => {
            ("stale", format!("{} 启动器进程仍在，但心跳已过期（可能卡住）", mode_label))
        }
        None => ("installed_stopped", format!("{} 任务已安装，启动器当前未运行", mode_label)),
    }
}

fn build_status(state: &AppState, mode: BootMode) -> Result<BootRunnerStatus, String> {
    let name = task_name(mode);
    let xml = query_task_xml(&name)?;
    let installed = xml.is_some();
    let logon_type = xml.as_deref().map(|x| tag_text(x, "LogonType")).unwrap_or_default();
    let runner_path = runner_install_path();
    let runner_installed = runner_path.is_file();
    // 安装包里的 runner 与 ProgramData 里的版本是否一致（SHA-256）：
    // GUI 更新后 sidecar 会变，但 ProgramData 的稳定 runner 不会自动更新；提示修复。
    // sidecar 哈希缓存（每 5 秒轮询不必重读 4MB），只重算已安装文件。
    let runner_current = match (bundled_runner_hash(), sha256_file(&runner_path)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    };
    let manifest = boot::manifest_path(mode);
    let task_path_ok = match xml.as_deref() {
        Some(x) => {
            x.contains(&runner_path.to_string_lossy().to_string())
                && x.contains(&manifest.to_string_lossy().to_string())
                && runner_installed
                && runner_current
        }
        None => false,
    };
    let runner_state = boot::read_state(mode);
    // state.json 缺失/被删时，锁文件仍能指出那个占着 runner 文件的进程
    let lock_pid = runner_lock_pid(&boot::boot_root(mode));
    let reachable = runner_state.as_ref().map(boot::runner_reachable).unwrap_or(false);
    let state_alive = runner_state.as_ref().map(boot::runner_process_alive).unwrap_or(false);
    // 心跳新鲜也算“进程还在”：GUI 与 S4U runner 可能不在同一登录会话，
    // OpenProcess 查不到它，但 runner 自己写的心跳能证明它还活着（跨会话场景）。
    let runner_alive = state_alive || lock_pid.is_some() || reachable;
    let (runner_pid, heartbeat_at, running_mode) = runner_state
        .as_ref()
        .map(|s| (
            (s.runner_pid != 0).then_some(s.runner_pid).or(lock_pid),
            s.heartbeat_at.clone(),
            s.mode.clone(),
        ))
        .unwrap_or((lock_pid, String::new(), String::new()));
    let count = boot_service_count(state, mode);
    let mode_label = mode.display();

    let (mut state_name, mut message) = classify_runner_state(installed, reachable, runner_state.as_ref(), mode_label);
    // 状态文件没写成功/被删，但锁文件证明进程还在：不能显示成“已停止/未运行”
    if (!state_alive && lock_pid.is_some()) && state_name != "running" {
        state_name = "stale";
        message = format!("{} 启动器进程仍在（状态文件缺失或未更新）", mode_label);
    }
    if installed && !runner_installed {
        message.push_str("；启动器文件不存在，请点「修复 / 重新安装」");
    } else if installed && !runner_current {
        message.push_str("；启动器版本与当前 Nexus 安装包不一致，请点「修复 / 重新安装」");
    } else if installed && !task_path_ok {
        message.push_str("；任务指向的启动器路径或 manifest 与当前配置不一致，请重新安装");
    }
    // 任务已卸载但进程还活着：它就是那个“占着 runner 文件、让重新安装失败”的残留实例
    if !installed && runner_alive {
        message.push_str(&format!(
            "；计划任务已卸载，但启动器进程仍在（PID {}），会占用 runner 文件；可点「结束残留启动器」",
            runner_pid.unwrap_or(0)
        ));
    }
    if installed && !logon_type.is_empty() && logon_type != "S4U" && logon_type != "Password" {
        message.push_str(&format!("；当前任务的登录类型是 {}（只支持 S4U / Password）", logon_type));
    }
    // 任务模式与当前运行实例模式不一致：切换在下次启动 runner 时生效
    if reachable && !running_mode.is_empty() && !logon_type.is_empty() {
        let task_mode = if logon_type.eq_ignore_ascii_case("password") { "password" } else { "s4u" };
        if task_mode != running_mode {
            message.push_str(&format!(
                "；任务已切换为 {}，当前运行实例仍是 {}，停止/重启启动器后生效",
                task_mode, running_mode
            ));
        }
    }

    Ok(BootRunnerStatus {
        mode: mode.as_str().to_string(),
        installed,
        task_name: name,
        task_logon_type: logon_type,
        task_path_ok,
        runner_path: runner_path.to_string_lossy().to_string(),
        runner_installed,
        runner_reachable: reachable,
        runner_alive,
        running_mode,
        runner_pid,
        heartbeat_at,
        boot_service_count: count,
        state: state_name.to_string(),
        message,
    })
}

/// 读取指定模式 runner 的状态（不查询计划任务；给热路径用）
pub fn runner_reachable_now(mode: BootMode) -> bool {
    boot::read_state(mode).map(|s| boot::runner_reachable(&s)).unwrap_or(false)
}

/// runner 进程是否还在（不要求心跳；任务卸载后的残留实例也算）。
///
/// state.json 可能被删/写失败，锁文件是第二判据（同样做 pid + 启动时刻校验）。
pub fn runner_alive_now(mode: BootMode) -> bool {
    let (state_alive, heartbeat_fresh) = boot::read_state(mode)
        .map(|s| {
            (
                boot::runner_process_alive(&s),
                boot::runner_heartbeat_fresh(&s),
            )
        })
        .unwrap_or((false, false));
    state_alive || heartbeat_fresh || runner_lock_pid(&boot::boot_root(mode)).is_some()
}

/// 所有已知状态源：新版本两个模式 + 旧版单任务（升级遗留）。
///
/// 旧版只用于**展示与 PID 兜底**，不参与新命令队列的路由；`label` 直接给界面用。
fn state_sources() -> Vec<(&'static str, crate::core::boot::BootRunnerState)> {
    let mut out = Vec::new();
    for mode in BootMode::ALL {
        if let Some(state) = boot::read_state(mode) {
            out.push((mode.as_str(), state));
        }
    }
    if let Some(state) = boot::read_legacy_state() {
        out.push(("legacy", state));
    }
    out
}

/// 当前拥有服务的 id（starting/ready **且进程确实还活着**）。
///
/// 不再要求 runner 心跳新鲜：runner 卡住/被强杀时，它拉起的子进程可能仍在跑，
/// 这里必须把它们算进来，否则批量启动会再起一份同名进程。
pub fn boot_running_ids() -> HashSet<String> {
    let mut out = HashSet::new();
    for (_, state) in state_sources() {
        out.extend(
            state.services.iter()
                .filter(|(_, s)| {
                    matches!(s.phase.as_str(), "starting" | "ready")
                        && boot::service_alive_in(&state, s)
                })
                .map(|(id, _)| id.clone()),
        );
    }
    out
}

/// 仍在运行的开机服务 `(service_id, project_id)`，不要求 runner 可达。
///
/// `get_running` 用它把 runner 拉起的服务并进运行列表：进程活着就该显示运行中，
/// 哪怕管理它的启动器已经退出/卡住。
pub fn boot_running_services() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (_, state) in state_sources() {
        for (id, s) in &state.services {
            if matches!(s.phase.as_str(), "starting" | "ready")
                && boot::service_alive_in(&state, s)
                && seen.insert(id.clone())
            {
                out.push((id.clone(), s.project_id.clone()));
            }
        }
    }
    out
}

/// 报告失败/异常退出的服务（供 get_running 合并）。
///
/// 仍要求 runner 可达：状态文件可能在 runner 死后停很久，把陈旧失败永久标红没有意义。
pub fn boot_failed_services() -> Vec<crate::core::process::FailedService> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (_, state) in state_sources() {
        if !boot::runner_reachable(&state) { continue; }
        for (id, s) in &state.services {
            let failed = s.phase == "failed"
                || (s.phase == "exited" && s.exit_code.unwrap_or(0) != 0);
            if !failed || !seen.insert(id.clone()) { continue; }
            out.push(crate::core::process::FailedService {
                service_id: id.clone(),
                exit_code: s.exit_code,
                timestamp: s.ready_at.clone().or_else(|| s.started_at.clone()).unwrap_or_default(),
                source: "boot".into(),
            });
        }
    }
    out
}

/// 服务当前由哪个模式的 runner 拥有（以状态文件为准；仅 runner 可达时可用于发命令）。
///
/// 模式切换后**两个 state 文件里可能都残留同一条记录**（旧模式里还在跑、新模式里是
/// pending）。必须优先认“进程仍活着”的那个，否则停止/重启会发给错误的启动器。
pub fn boot_owned_mode(service_id: &str) -> Option<BootMode> {
    let mut fallback = None;
    for mode in BootMode::ALL {
        let Some(state) = boot::read_state(mode) else { continue };
        if !boot::runner_reachable(&state) { continue; }
        let Some(s) = state.services.get(service_id) else { continue };
        if boot::service_alive_in(&state, s) {
            return Some(mode);
        }
        if fallback.is_none() {
            fallback = Some(mode);
        }
    }
    fallback
}

/// 仍活着的开机服务（**不看 runner 是否可达**）。
///
/// 返回 `(来源, 状态)`，来源是 `s4u` / `password` / `legacy`。PID 与启动时刻双重校验，
/// 不依赖心跳——这是“服务在不在跑”的唯一可靠判据，也是阻止本地重复启动的依据。
pub fn live_boot_service(service_id: &str) -> Option<(&'static str, BootServiceState)> {
    for (label, state) in state_sources() {
        if let Some(s) = state.services.get(service_id) {
            if boot::service_alive_in(&state, s) {
                return Some((label, s.clone()));
            }
        }
    }
    None
}

/// 停止一个由开机启动器拥有的服务，返回是否由开机启动器侧处理。
///
/// - runner 可达 → 走命令队列；
/// - runner 不可达但 state 记录的子进程仍活着 → 直接结束进程树（PID + 启动时刻校验防复用）。
///
/// 删除服务/项目、批量停止都走这里，避免“启动器失联后界面说已停止、进程其实还在”。
pub fn stop_boot_service(service_id: &str) -> Result<bool, String> {
    if let Some(mode) = boot_owned_mode(service_id) {
        send_service_command(mode, "stop", service_id)?;
        return Ok(true);
    }
    if let Some((_, s)) = live_boot_service(service_id) {
        if let Some(pid) = s.pid {
            crate::core::process::kill_process_tree(pid)
                .map_err(|e| format!("结束残留的开机启动进程失败（PID {}）：{}", pid, e))?;
            return Ok(true);
        }
    }
    Ok(false)
}

/// 通知指定模式的 runner 重新读 manifest；未运行时静默跳过。
pub fn reload_if_running(mode: BootMode) {
    if !runner_reachable_now(mode) { return; }
    let cmd = BootCommand {
        id: boot::new_command_id(),
        op: "reload".into(),
        service_id: None,
    };
    if let Err(e) = boot::send_command(mode, cmd, Duration::from_secs(3)) {
        log::warn!("[nexus] 通知 {} 开机启动器 reload 失败: {}", mode.as_str(), e);
    }
}

/// 两个 runner 都重新读 manifest。
pub fn reload_all_if_running() {
    for mode in BootMode::ALL {
        reload_if_running(mode);
    }
}

/// 向指定模式的 runner 发一条服务命令（start/stop/restart）。返回 runner 的真实结果。
pub fn send_service_command(
    mode: BootMode,
    op: &str,
    service_id: &str,
) -> Result<(), String> {
    if !runner_reachable_now(mode) {
        return Err(format!("{} 开机启动器未运行", mode.display()));
    }
    let cmd = BootCommand {
        id: boot::new_command_id(),
        op: op.into(),
        service_id: Some(service_id.to_string()),
    };
    // start/restart 要等就绪检查（MySQL 可能几十秒），不能沿用停止动作的短超时；
    // 否则 runner 还在正常启动，GUI 已经报“没有响应”。
    let timeout = if matches!(op, "start" | "restart") {
        Duration::from_secs(180)
    } else {
        Duration::from_secs(15)
    };
    let result = boot::send_command(mode, cmd, timeout)?;
    if result.ok { Ok(()) } else if result.error.is_empty() {
        Err(format!("开机启动器执行 {} 失败", op))
    } else {
        Err(result.error)
    }
}

/// 停掉旧版单任务 runner；它会和新任务同时拉起服务，必须在安装新模式前退出。
/// 等某个模式的 runner 退出（写 pid=0 或进程消失）。
fn wait_runner_exit(mode: BootMode, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let gone = match boot::read_state(mode) {
            Some(s) => {
                if s.runner_pid == 0 {
                    true
                } else {
                    match crate::core::process::probe_process_start_time(s.runner_pid) {
                        ProcessProbe::Gone => true,
                        ProcessProbe::Alive(_) => false,
                        // 无权限查（跨登录会话）：只要心跳还在更新就认为它还活着
                        ProcessProbe::Denied => !boot::runner_heartbeat_fresh(&s),
                    }
                }
            }
            None => true,
        };
        if gone {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// runner 的单实例锁（pid + 进程启动时刻），用于 state.json 缺失时找出残留进程。
#[derive(serde::Deserialize)]
struct RunnerLockInfo {
    pid: u32,
    started_at_ms: u64,
}

fn runner_lock_pid(dir: &std::path::Path) -> Option<u32> {
    let bytes = std::fs::read(dir.join("runner.lock")).ok()?;
    let lock: RunnerLockInfo = serde_json::from_slice(&bytes).ok()?;
    // pid + 启动时刻双重校验，避免 pid 复用把无关进程当成 runner
    crate::core::process::process_start_time_millis(lock.pid)
        .filter(|t| *t == lock.started_at_ms)
        .map(|_| lock.pid)
}

/// 结束某个模式的 runner（安装前、停止、卸载共用）。
///
/// - 可达 → 发 shutdown（退出码 0，不会被 `RestartOnFailure` 当成失败重启），等服务收尾；
/// - 不可达但进程仍在 → 先 `schtasks /End`（用户主动结束，避免失败重启），短暂等待；
/// - 仍不退 → 按 state 的 PID 结束整棵树（PID + 启动时刻已校验，防复用）。
///
/// 这条路径同时覆盖“任务已删但进程还在”的残留实例（`/End` 会失败，直接走 PID）。
fn stop_runner_now(mode: BootMode, timeout: Duration) -> Result<(), String> {
    let boot_dir = boot::boot_root(mode);
    let lock_pid = runner_lock_pid(&boot_dir);
    let Some(state) = boot::read_state(mode) else {
        // state 丢了但进程还在（用户删过数据文件/写入失败）：用锁文件兜底
        if let Some(pid) = lock_pid {
            crate::core::process::kill_process_tree(pid)
                .map_err(|e| format!("结束残留启动器进程失败（PID {}）：{}", pid, e))?;
        }
        return Ok(());
    };
    // 只有确认“进程确实不在了”才走早退；Denied 不能当退出——跨会话查不到 S4U
    // runner 时（服务器场景）早退会让停止/修复/卸载全部跳过，runner 继续在后台跑。
    if state.runner_pid == 0
        || matches!(
            crate::core::process::probe_process_start_time(state.runner_pid),
            ProcessProbe::Gone
        )
    {
        if let Some(pid) = lock_pid {
            crate::core::process::kill_process_tree(pid)
                .map_err(|e| format!("结束残留启动器进程失败（PID {}）：{}", pid, e))?;
        }
        return Ok(());
    }

    if boot::runner_reachable(&state) {
        let cmd = BootCommand { id: boot::new_command_id(), op: "shutdown".into(), service_id: None };
        // 结果文件只代表“收到 shutdown”，服务还要逐个收尾；不要用它的 ok 判断进程已退出。
        // 命令没送出去（超时/IO 错误）就没必要再等 60 秒，直接进入 PID 兜底。
        let sent = boot::send_command(mode, cmd, Duration::from_secs(120)).is_ok();
        if sent && wait_runner_exit(mode, timeout) {
            return Ok(());
        }
    } else {
        // 心跳已过期：命令队列没人处理，直接让计划任务结束这个实例。
        // 任务已卸载（残留进程）时跳过 `/End`，直接走下面的 PID 兜底。
        let name = task_name(mode);
        let has_task = query_task_xml(&name).map(|x| x.is_some()).unwrap_or(false);
        if has_task {
            let _ = run_schtasks(&["/End", "/TN", &name]);
            if wait_runner_exit(mode, Duration::from_secs(3)) {
                return Ok(());
            }
        }
    }

    // 最后兜底：直接结束进程树（Job Object 会带走它启动的服务）
    if let Some(s) = boot::read_state(mode) {
        if s.runner_pid != 0 && boot::runner_process_alive(&s) {
            crate::core::process::kill_process_tree(s.runner_pid)
                .map_err(|e| format!("结束启动器进程失败（PID {}）：{}", s.runner_pid, e))?;
        }
    }
    if wait_runner_exit(mode, Duration::from_secs(5)) {
        Ok(())
    } else {
        Err("启动器没有在预期时间内退出".into())
    }
}

/// 强杀后把 state 收敛为“已停止”。
///
/// runner 已经死了，不会有别的写入者；否则界面会把一次成功的停止显示成“已退出”。
fn mark_runner_stopped(mode: BootMode) {
    let Some(mut state) = boot::read_state(mode) else { return };
    // 只有确认 runner 进程已经结束才改写状态；Denied（跨会话查不到）时不能把
    // 仍在运行的启动器标成“已停止”（否则后续停止/修复会以为没有实例）。
    if state.runner_pid == 0
        || !matches!(
            crate::core::process::probe_process_start_time(state.runner_pid),
            ProcessProbe::Gone
        )
    {
        return;
    }
    state.runner_pid = 0;
    state.heartbeat_at = chrono::Utc::now().to_rfc3339();
    for svc in state.services.values_mut() {
        if matches!(svc.phase.as_str(), "starting" | "ready") {
            svc.phase = "stopped".into();
            svc.pid = None;
            svc.error.clear();
        }
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(&state) {
        let _ = boot::write_atomic(&boot::state_path(mode), &bytes);
    }
}

fn wait_legacy_exit(timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let gone = match boot::read_legacy_state() {
            Some(s) => s.runner_pid == 0 || !boot::runner_process_alive(&s),
            None => true,
        };
        if gone {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 停止旧版单任务 runner（升级迁移 / 安装前）。
///
/// 旧版本把 runner 装在同一个 ProgramData 路径下：不结束它就会占住文件，新版本
/// 无法复制/注册。可达走命令队列；不可达但进程还在就按 PID 兜底结束。
fn stop_legacy_runner() -> Result<(), String> {
    let Some(state) = boot::read_legacy_state() else { return Ok(()) };
    if state.runner_pid == 0 || !boot::runner_process_alive(&state) {
        return Ok(());
    }
    if boot::runner_reachable(&state) {
        let cmd = BootCommand {
            id: boot::new_command_id(),
            op: "shutdown".into(),
            service_id: None,
        };
        let _ = boot::send_legacy_command(cmd, Duration::from_secs(120));
        if wait_legacy_exit(Duration::from_secs(60)) {
            return Ok(());
        }
    }
    if let Some(s) = boot::read_legacy_state() {
        if s.runner_pid != 0 && boot::runner_process_alive(&s) {
            crate::core::process::kill_process_tree(s.runner_pid)
                .map_err(|e| format!("结束旧版启动器失败（PID {}）：{}", s.runner_pid, e))?;
        }
    }
    if wait_legacy_exit(Duration::from_secs(5)) {
        Ok(())
    } else {
        Err("旧版启动器没有在预期时间内退出".into())
    }
}

/// 日志路由用：优先取**状态文件里出现过**它的模式（不要求 runner 可达），否则用配置里的模式。
///
/// 不再用 `boot_owned_mode`：那个要求心跳新鲜，启动器一失联，日志面板就会说“没有日志”
/// ——而日志文件明明还在磁盘上。这正是用户报的“看不了开机服务的日志”。
pub fn service_log_mode(
    db: &crate::database::Database,
    service_id: &str,
) -> Option<BootMode> {
    // 模式切换后两边都可能残留记录：优先选进程仍活着的那个，否则日志会读到空文件。
    // 只读一遍 state 文件（日志面板每秒轮询一次）
    let mut fallback = None;
    for mode in BootMode::ALL {
        let Some(state) = boot::read_state(mode) else { continue };
        let Some(s) = state.services.get(service_id) else { continue };
        if boot::service_alive_in(&state, s) {
            return Some(mode);
        }
        if fallback.is_none() {
            fallback = Some(mode);
        }
    }
    if fallback.is_some() {
        return fallback;
    }
    // 旧版单任务：日志在 legacy 目录，`read_boot_log_tail` 会在新模式路径不存在时回落。
    // 这里返回哪个模式只用于定位，不影响实际读取路径。
    if boot::read_legacy_state().map(|s| s.services.contains_key(service_id)).unwrap_or(false) {
        return Some(BootMode::S4u);
    }
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT boot_enabled, boot_credential_mode FROM services WHERE id=?1",
            [service_id],
            |r| Ok((r.get::<_, i32>(0)? != 0, r.get::<_, String>(1)?)),
        ).map_err(|e| e.to_string())
    }).ok().and_then(|(enabled, mode)| {
        if enabled { BootMode::parse(&mode).ok() } else { None }
    })
}

/// 读指定模式的开机服务日志尾部（给日志面板的快照/轮询用）。
/// 返回 None = 这个服务没有 boot 日志文件。
pub fn read_boot_log_tail(
    mode: BootMode,
    service_id: &str,
    max_bytes: usize,
) -> Option<Vec<Arc<LogLine>>> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let path = boot::service_log_path(mode, service_id);
    // 旧版单任务（升级后未迁移）把日志写在 ~/.nexus/boot/logs/ 下：新模式的日志
    // 不存在时回落到那里，否则“服务在跑、日志就在磁盘上，界面却读不到”。
    let legacy_path = boot::legacy_service_log_path(service_id);
    let mut file = std::fs::File::open(&path)
        .or_else(|_| std::fs::File::open(&legacy_path))
        .ok()?;
    // 只读尾部：日志面板每秒轮询一次，整文件读（最大 5MB）会变成每秒 5MB 的分配/IO
    let len = file.metadata().ok()?.len();
    let tail_start = len.saturating_sub(max_bytes as u64);
    file.seek(SeekFrom::Start(tail_start)).ok()?;
    let mut bytes = Vec::with_capacity((len - tail_start) as usize);
    file.read_to_end(&mut bytes).ok()?;
    // 与编辑器/日志跟随同一套解码口径：GBK 输出不能变成一串替换字符
    let decoded = crate::core::textenc::decode_to_string(&bytes);
    // 从文件中间开始时，第一行可能是半截——丢掉
    let text = if tail_start > 0 {
        decoded.split_once('\n').map(|(_, rest)| rest).unwrap_or("")
    } else {
        &decoded
    };
    let now = chrono::Utc::now().to_rfc3339();
    let mut lines: Vec<Arc<LogLine>> = text
        .lines()
        .map(|line| Arc::new(LogLine {
            seq: 0,
            timestamp: now.clone(),
            stream: "file".into(),
            text: line.to_string(),
        }))
        .collect();
    // 只保留最新 2000 行；seq 在裁剪后重新编号（快照轮询按整份替换，不做跨快照合并）
    if lines.len() > 2000 {
        lines.drain(..lines.len() - 2000);
    }
    for (i, line) in lines.iter_mut().enumerate() {
        if let Some(l) = Arc::get_mut(line) {
            l.seq = (i + 1) as u64;
        }
    }
    Some(lines)
}

// ─── Tauri 命令 ─────────────────────────────────────────────

#[tauri::command]
pub async fn get_boot_runner_status(app: tauri::AppHandle) -> Result<Vec<BootRunnerStatus>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let mut out = Vec::with_capacity(BootMode::ALL.len());
        for mode in BootMode::ALL {
            out.push(build_status(&state, mode)?);
        }
        Ok(out)
    }).await.map_err(|e| format!("读取开机启动器状态失败: {}", e))?
}

/// 设置页逐服务列表的保留规则。
///
/// 只保留仍配置为开机启动（且服务/项目都还在）的服务，或仍有存活进程的实例。
/// 删除服务/项目后残留的死记录、取消开机自启后已停止的记录不再展示；但删除后
/// 仍有进程在跑的必须保留，否则数据库记录已删、界面上没有停止入口。
fn keep_boot_service_item(item: &BootServiceStatus) -> bool {
    item.alive || (item.configured && item.service_exists && item.project_exists)
}

/// 逐服务状态：合并两个模式的 state.json、旧版遗留 state 与 DB 里已配置但从未启动的服务。
///
/// 列表只保留两类：**仍配置为开机启动的服务**，以及**仍有存活进程的实例**。
/// 服务/项目删除后死掉的旧记录、取消开机自启且已停止的记录会被过滤掉；删除后进程
/// 仍残留的则不隐藏（否则数据库记录已删，界面上就再也没有停止入口了）。
///
/// 存活判断优先采信 runner 心跳里上报的观测结果；启动器不可达时再回退到 pid 校验。
#[tauri::command]
pub async fn get_boot_services_status(app: tauri::AppHandle) -> Result<Vec<BootServiceStatus>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        // 判断 state 里的记录对应服务/项目是否还存在：已删除的、且进程已不在的记录
        // 不再展示；进程仍在的保留一条警示项（否则数据库记录没了，界面上没有停止入口）。
        let (service_project, project_ids): (
            std::collections::HashMap<String, String>,
            std::collections::HashSet<String>,
        ) = state.db.with_conn(|conn| {
            let mut svc_stmt = conn.prepare("SELECT id, project_id FROM services")
                .map_err(|e| format!("查询服务存在性失败: {}", e))?;
            let svc_rows = svc_stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| format!("读取服务存在性失败: {}", e))?;
            let mut service_project = std::collections::HashMap::new();
            for row in svc_rows {
                let (id, project_id) = row.map_err(|e| format!("解析服务存在性失败: {}", e))?;
                service_project.insert(id, project_id);
            }
            let mut proj_stmt = conn.prepare("SELECT id FROM projects")
                .map_err(|e| format!("查询项目存在性失败: {}", e))?;
            let proj_rows = proj_stmt.query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| format!("读取项目存在性失败: {}", e))?;
            let mut project_ids = std::collections::HashSet::new();
            for row in proj_rows {
                project_ids.insert(row.map_err(|e| format!("解析项目存在性失败: {}", e))?);
            }
            Ok((service_project, project_ids))
        })?;

        let mut out: Vec<BootServiceStatus> = Vec::new();
        let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

        for (label, rs) in state_sources() {
            let runner_reachable = boot::runner_reachable(&rs);
            let runner_alive = runner_reachable || boot::runner_process_alive(&rs);
            for (id, s) in &rs.services {
                let alive = boot::service_alive_in(&rs, s);
                let service_exists = service_project.contains_key(id);
                // 服务还在时以 DB 的归属为准；服务已删时只能用 state 里记录的 project_id
                let project_exists = service_project
                    .get(id)
                    .map(|p| project_ids.contains(p))
                    .unwrap_or_else(|| project_ids.contains(&s.project_id));
                let item = BootServiceStatus {
                    service_id: id.clone(),
                    name: s.name.clone(),
                    project_id: s.project_id.clone(),
                    mode: label.to_string(),
                    phase: s.phase.clone(),
                    alive,
                    pid: s.pid,
                    started_at: s.started_at.clone().unwrap_or_default(),
                    ready_at: s.ready_at.clone().unwrap_or_default(),
                    exit_code: s.exit_code,
                    error: s.error.clone(),
                    log_path: s.log_path.clone(),
                    runner_reachable,
                    runner_alive,
                    configured: false,
                    service_enabled: true,
                    service_exists,
                    project_exists,
                };
                match index.get(id) {
                    // 同一服务同时出现在旧/新状态里：保留进程仍活着的记录
                    Some(&i) if alive && !out[i].alive => out[i] = item,
                    Some(_) => {}
                    None => { index.insert(id.clone(), out.len()); out.push(item); }
                }
            }
        }

        let configured: Vec<(String, String, String, bool, String)> = state.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, name, project_id, enabled, boot_credential_mode \
                 FROM services WHERE boot_enabled=1"
            ).map_err(|e| format!("查询开机启动服务失败: {}", e))?;
            let rows = stmt.query_map([], |r| Ok((
                r.get::<_, String>(0)?, r.get::<_, String>(1)?,
                r.get::<_, String>(2)?, r.get::<_, i32>(3)? != 0,
                r.get::<_, String>(4)?,
            ))).map_err(|e| format!("读取开机启动服务失败: {}", e))?;
            let mut v = Vec::new();
            for row in rows { v.push(row.map_err(|e| format!("解析开机启动服务失败: {}", e))?); }
            Ok::<_, String>(v)
        })?;

        let configured_map: std::collections::HashMap<String, bool> =
            configured.iter().map(|(id, _, _, enabled, _)| (id.clone(), *enabled)).collect();
        for (id, name, project_id, enabled, mode) in &configured {
            if index.contains_key(id) { continue; }
            let boot_mode = BootMode::parse(mode).unwrap_or(BootMode::S4u);
            index.insert(id.clone(), out.len());
            out.push(BootServiceStatus {
                service_id: id.clone(),
                name: name.clone(),
                project_id: project_id.clone(),
                mode: boot_mode.as_str().to_string(),
                // 从未被启动器记录：让界面如实说“未由启动器管理”，而不是伪造一个 phase
                phase: String::new(),
                alive: false,
                pid: None,
                started_at: String::new(),
                ready_at: String::new(),
                exit_code: None,
                error: String::new(),
                log_path: boot::service_log_path(boot_mode, id).to_string_lossy().to_string(),
                runner_reachable: false,
                runner_alive: false,
                configured: true,
                service_enabled: *enabled,
                service_exists: true,
                project_exists: project_ids.contains(project_id),
            });
        }
        for item in &mut out {
            item.configured = configured_map.contains_key(&item.service_id);
            if let Some(enabled) = configured_map.get(&item.service_id) {
                item.service_enabled = *enabled;
            }
        }
        // 只保留：仍配置为开机启动（且服务/项目都在）的，或仍有存活进程的。
        // 删除服务/项目后残留的死记录、取消开机自启后已停止的记录，都不再显示。
        out.retain(keep_boot_service_item);
        // 还在跑的排前面，其余按名称；界面不必再排一次
        out.sort_by(|a, b| b.alive.cmp(&a.alive).then_with(|| a.name.cmp(&b.name)));
        Ok(out)
    }).await.map_err(|e| format!("读取开机服务状态失败: {}", e))?
}

#[tauri::command]
pub async fn install_boot_runner(
    app: tauri::AppHandle,
    mode: String,
) -> Result<BootRunnerStatus, String> {
    let boot_mode = BootMode::parse(&mode)?;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        // 先落两个模式的 manifest；每个服务只进入自己的那一个
        boot::sync_all_manifests(&state.db)?;
        // 旧版单任务必须停掉：它和新任务会同时拉起同一批服务，还可能占着 runner 文件
        stop_legacy_runner()?;

        let bundled = bundled_runner_path()?;
        let runner_path = runner_install_path();
        let name = task_name(boot_mode);
        let legacy = legacy_task_name();
        // 修复/重装前记录它原本是否在跑：装完把运行状态恢复，避免“修一次把服务全停了”
        let was_running = runner_alive_now(boot_mode);

        // 修复/重装：先停掉该模式现有 runner。既避免任务实例仍在运行导致 /Create 失败，
        // 也让新二进制在下次启动时生效。停不掉不立刻放弃——原子替换仍能换文件，
        // 脚本里的 `/End` 也会再收一次；失败会写进日志。
        if let Err(e) = stop_runner_now(boot_mode, Duration::from_secs(60)) {
            log::warn!("[nexus] 安装前停止 {} 启动器失败: {}", boot_mode.as_str(), e);
        }
        mark_runner_stopped(boot_mode);

        let need_copy = !files_identical(&bundled, &runner_path);
        let xml_text = task_xml(&runner_path, &boot::manifest_path(boot_mode), boot_mode.as_str())?;
        let error_path = boot::boot_root(boot_mode).join("install-error.txt");
        let spec = InstallScript {
            mode: boot_mode,
            src: &bundled,
            dst: &runner_path,
            task: &name,
            legacy_task: &legacy,
            // XML 直接嵌进提权脚本：不再落一份用户可改的 task.xml
            xml_text: &xml_text,
            need_copy,
            error_path: &error_path,
        };
        let result = run_install_elevated(&spec);
        if let Err(e) = result {
            // 安装失败时尽量把原本在跑的服务恢复回来（旧任务通常还在）
            if was_running {
                let _ = run_schtasks(&["/Run", "/TN", &name]);
            }
            return Err(format!("安装失败：{}", e));
        }

        // 回读 + 文件校验：schtasks 返回 0 不等于任务真的按我们要求注册了（铁律 21）。
        // 用 SHA-256 而不是长度，避免“复制到一半/被替换成同长度文件”被当成成功。
        if !files_identical(&bundled, &runner_path) {
            let src_len = std::fs::metadata(&bundled).map(|m| m.len()).unwrap_or(0);
            let dst_len = std::fs::metadata(&runner_path).map(|m| m.len()).unwrap_or(0);
            return Err(format!(
                "启动器复制校验失败（源 {} 字节，目标 {} 字节，内容不一致）：{}",
                src_len, dst_len, runner_path.display()
            ));
        }
        let expected = match boot_mode {
            BootMode::S4u => "S4U",
            BootMode::Password => "Password",
        };
        let xml = query_task_xml(&name)?
            .ok_or_else(|| "任务创建后回读失败（任务不存在）".to_string())?;
        if tag_text(&xml, "LogonType") != expected {
            return Err(format!(
                "任务已创建，但回读到的 LogonType 不是 {}（实际: {}）",
                expected,
                tag_text(&xml, "LogonType")
            ));
        }
        // 原来在跑的（修复/更新密码/清理残留后重装）→ 装完恢复运行，
        // 避免用户以为“修一下服务全没了”。原本没在跑则保持停止状态。
        if was_running {
            let (ok, text) = run_schtasks(&["/Run", "/TN", &name])?;
            if !ok {
                log::warn!("[nexus] 修复后重新启动 {} 启动器失败: {}", boot_mode.as_str(), text.trim());
            }
        } else {
            // 已有同一模式的 runner 在跑时，让它重新读一次 manifest（不重启服务）
            reload_if_running(boot_mode);
        }
        build_status(&state, boot_mode)
    }).await.map_err(|e| format!("安装开机启动器任务失败: {}", e))?
}

#[tauri::command]
pub async fn uninstall_boot_runner(
    app: tauri::AppHandle,
    mode: String,
) -> Result<BootRunnerStatus, String> {
    let boot_mode = BootMode::parse(&mode)?;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let name = task_name(boot_mode);
        // 1) 先请求优雅退出；失败不阻塞删任务——否则用户会卡在“任务还在、进程又停不掉”
        let stop_error = stop_runner_now(boot_mode, Duration::from_secs(60)).err();
        // 2) 删除任务（也确保不会按 RestartOnFailure 再次拉起）
        if query_task_xml(&name)?.is_some() {
            elevated_schtasks(&delete_params(&name))?;
        }
        // 3) 任务已经没了，再兜底结束一次（这次不会再被计划任务重启）
        if stop_error.is_some() {
            if let Err(e) = stop_runner_now(boot_mode, Duration::from_secs(10)) {
                log::warn!("[nexus] 卸载 {} 后结束残留启动器失败: {}", boot_mode.as_str(), e);
            }
        }
        mark_runner_stopped(boot_mode);
        build_status(&state, boot_mode)
    }).await.map_err(|e| format!("卸载开机启动器任务失败: {}", e))?
}

#[tauri::command]
pub async fn start_boot_runner(
    app: tauri::AppHandle,
    mode: String,
) -> Result<BootRunnerStatus, String> {
    let boot_mode = BootMode::parse(&mode)?;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        boot::sync_all_manifests(&state.db)?;
        let name = task_name(boot_mode);
        if query_task_xml(&name)?.is_none() {
            return Err(format!("{} 开机启动器尚未安装，请先安装", mode));
        }
        let (ok, text) = run_schtasks(&["/Run", "/TN", &name])?;
        if !ok {
            return Err(format!("启动开机启动器失败: {}", text.trim()));
        }
        build_status(&state, boot_mode)
    }).await.map_err(|e| format!("启动开机启动器失败: {}", e))?
}

#[tauri::command]
pub async fn stop_boot_runner(
    app: tauri::AppHandle,
    mode: String,
) -> Result<BootRunnerStatus, String> {
    let boot_mode = BootMode::parse(&mode)?;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        // 可达走 shutdown（runner 自己收尾、写 pid=0）；不可达但进程还在时
        // 走 `/End` + PID 兜底，任务已卸载的残留实例也能在这里结束。
        stop_runner_now(boot_mode, Duration::from_secs(60))?;
        mark_runner_stopped(boot_mode);
        build_status(&state, boot_mode)
    }).await.map_err(|e| format!("停止开机启动器失败: {}", e))?
}

/// 快速开/关一个服务的开机启动（服务编辑面板/右键菜单用）。
#[tauri::command]
pub fn set_service_boot_enabled(
    state: State<AppState>,
    service_id: String,
    enabled: bool,
) -> Result<BootToggleResult, String> {
    if service_id.trim().is_empty() {
        return Err("服务ID不能为空".into());
    }
    let affected = state.db.with_conn(|conn| {
        conn.execute(
            "UPDATE services SET boot_enabled=?1 WHERE id=?2",
            rusqlite::params![if enabled { 1 } else { 0 }, service_id],
        ).map_err(|e| format!("更新开机启动开关失败: {}", e))
    })?;
    if affected == 0 {
        return Err("服务不存在".into());
    }
    let mut error = String::new();
    if let Err(e) = boot::sync_all_manifests(&state.db) {
        error = e;
    } else {
        reload_all_if_running();
    }
    Ok(BootToggleResult { boot_enabled: enabled, boot_manifest_error: error })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_name_sanitizes_user() {
        let name = format!("{} ({})", TASK_PREFIX, sanitize_user_name(r#"a\b/c:d*e?f"g<h>i|j"#));
        assert!(name.starts_with("Nexus Boot Runner"));
        assert!(!name.contains('/'));
        assert!(!name.contains(':'));
    }

    #[test]
    fn test_two_modes_have_distinct_task_names_and_legacy_is_separate() {
        let s4u = task_name(BootMode::S4u);
        let pwd = task_name(BootMode::Password);
        let legacy = legacy_task_name();
        assert_ne!(s4u, pwd, "两个模式不能共用任务名");
        assert_ne!(s4u, legacy, "S4U 任务不能和旧版单任务同名");
        assert_ne!(pwd, legacy, "密码任务不能和旧版单任务同名");
        assert!(s4u.contains("S4U"));
        assert!(pwd.contains("Password"));
    }

    #[test]
    fn test_task_xml_is_s4u_and_starts_runner() {
        let runner = std::path::Path::new(r"C:\ProgramData\Nexus\boot\runner\nexus-boot-runner.exe");
        let manifest = std::path::Path::new(r"C:\Users\u\.nexus\boot\manifest.json");
        let xml = task_xml(runner, manifest, "s4u").expect("生成任务 XML");
        assert!(xml.contains("<LogonType>S4U</LogonType>"));
        assert!(xml.contains("--mode s4u"));
        assert!(xml.contains("nexus-boot-runner.exe"));
        assert!(xml.contains("--manifest"));
        assert!(!xml.contains("--boot-runner"), "独立 runner 不需要兼容参数");
        assert!(xml.contains("manifest.json"));
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(xml.contains("<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>"));
        assert!(!xml.contains("Password"));

        let pwd = task_xml(runner, manifest, "password").expect("生成密码模式 XML");
        assert!(pwd.contains("<LogonType>Password</LogonType>"));
        assert!(pwd.contains("--mode password"));
        assert!(pwd.contains("manifest.json"));
    }

    const TEST_XML: &str = "<Task><Description>测试任务</Description></Task>";

    fn install_script_for(mode: BootMode, need_copy: bool) -> String {
        let src = std::path::PathBuf::from(r"C:\App Folder\nexus-boot-runner.exe");
        let dst = std::path::PathBuf::from(r"C:\ProgramData\Nexus\boot\runner\nexus-boot-runner.exe");
        let error = std::path::PathBuf::from(r"C:\Users\u\.nexus\boot\install-error.txt");
        let spec = InstallScript {
            mode,
            src: &src,
            dst: &dst,
            task: "Nexus Boot Runner S4U (u-1001)",
            legacy_task: "Nexus Boot Runner (u-1001)",
            xml_text: TEST_XML,
            need_copy,
            error_path: &error,
        };
        install_script(&spec)
    }

    #[test]
    fn test_install_script_replaces_atomically_and_hardens_acl() {
        let script = install_script_for(BootMode::S4u, true);
        assert!(script.contains("Copy-Item -LiteralPath 'C:\\App Folder\\nexus-boot-runner.exe'"));
        // 正在运行的 exe 不能覆盖，但可以改名：必须走 .new/.old 原子替换
        assert!(script.contains("Move-Item -LiteralPath $dst -Destination $old -Force"));
        assert!(script.contains("Move-Item -LiteralPath $new -Destination $dst -Force"));
        // 目录权限收口（SID，不依赖系统语言）；必须先改目录 ACL 再复制，
        // 新文件才会继承 Users:(RX)。加 `/T` 会把已有文件的 ACL 清空（实测）。
        let acl_pos = script.find("icacls $p").expect("脚本应包含 icacls");
        let copy_pos = script.find("Copy-Item").expect("脚本应包含复制");
        assert!(acl_pos < copy_pos, "ACL 必须排在复制之前");
        assert!(script.contains("*S-1-5-32-545:(OI)(CI)RX"));
        assert!(!script.contains("RX' /T"), "不要用 /T 传播到已有文件");
        // ProgramData 下三级目录逐个检查联接点并收口，防止非管理员预建 junction
        assert!(script.contains("foreach ($p in @($nexus, $boot, $dir))"));
        assert!(script.contains("ReparsePoint"));
        // 任务 XML 直接嵌进脚本（不再落用户可改的 task.xml），走 cmdlet 注册
        assert!(script.contains(TEST_XML));
        assert!(script.contains("Register-ScheduledTask -TaskName 'Nexus Boot Runner S4U (u-1001)' -Xml $xmlText"));
        assert!(script.contains("Unregister-ScheduledTask -TaskName 'Nexus Boot Runner (u-1001)'"));
        // 不再走 cmd，也没有任何密码字面量
        assert!(!script.contains("cmd /c"));
        assert!(!script.contains("Get-Credential"));
        assert!(!script.contains("Password="));
    }

    #[test]
    fn test_install_script_skips_copy_when_unchanged() {
        let script = install_script_for(BootMode::S4u, false);
        assert!(!script.contains("Copy-Item"), "同版本 runner 不应再次复制");
        assert!(script.contains("Register-ScheduledTask"));
    }

    #[test]
    fn test_password_install_script_uses_native_credential_dialog() {
        let script = install_script_for(BootMode::Password, true);
        assert!(script.contains("Get-Credential"));
        assert!(script.contains("Register-ScheduledTask"));
        assert!(script.contains("Copy-Item"));
        // 脚本里不能出现任何密码字面量/参数
        assert!(!script.contains("Password="));
    }

    /// 只做 PowerShell **语法解析**，不执行：脚本里有 Get-Credential/注册任务，
    /// 但语法错误会让安装静默失败，值得在 CI 里挡一次（两种模式都解析）。
    #[cfg(windows)]
    #[test]
    fn test_install_scripts_have_valid_powershell_syntax() {
        let dir = std::env::temp_dir().join(format!("nexus_ps_parse_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let parse_path = dir.join("parse.ps1");
        std::fs::write(
            &parse_path,
            "$src = Get-Content -Raw -LiteralPath $args[0] -Encoding UTF8\n\
             $tokens = $null\n\
             $errors = $null\n\
             [System.Management.Automation.Language.Parser]::ParseInput($src, [ref]$tokens, [ref]$errors) | Out-Null\n\
             if ($errors.Count -gt 0) { $errors | ForEach-Object { Write-Error ($_.Message) }; exit 1 }\n",
        ).expect("写解析脚本");

        for mode in [BootMode::S4u, BootMode::Password] {
            let script_path = dir.join(format!("install-{}.ps1", mode.as_str()));
            std::fs::write(&script_path, install_script_for(mode, true)).expect("写脚本");
            let out = std::process::Command::new("powershell")
                .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
                .arg(&parse_path)
                .arg(&script_path)
                .output()
                .expect("运行 PowerShell 语法检查");
            assert!(
                out.status.success(),
                "{} 安装脚本 PowerShell 语法检查失败: {}",
                mode.as_str(),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 回归：**正在运行的 exe 无法覆盖，但可以改名**。
    ///
    /// 用户卸载后安装失败（退出码 1）的根因就是旧 runner 还占着同一个文件。
    /// 这里用一个真实运行的进程复现：先把它改名，再把新文件放到原位置。
    /// 替换片段必须能在“文件正在被运行”的情况下成功。
    #[cfg(windows)]
    #[test]
    fn test_runner_replace_works_while_old_exe_is_running() {
        use std::os::windows::process::CommandExt;
        let dir = std::env::temp_dir().join(format!("nexus_replace_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let dst = dir.join("nexus-boot-runner.exe");
        let src = dir.join("bundled.exe");
        let ping = std::path::PathBuf::from(
            std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()),
        ).join("System32").join("ping.exe");
        std::fs::copy(&ping, &dst).expect("复制 ping 作为正在运行的 exe");
        std::fs::write(&src, b"new-runner-content").expect("写新文件");

        let mut child = std::process::Command::new(&dst)
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(0x08000000)
            .spawn()
            .expect("启动占位进程");
        std::thread::sleep(std::time::Duration::from_millis(600));

        let script = format!(
            "$dst = {dst}\n$dir = Split-Path -Parent $dst\n\
             $new = \"$($dst).new.\" + [guid]::NewGuid().ToString('N')\n\
             $old = \"$($dst).old.\" + [guid]::NewGuid().ToString('N')\n\
             {replace}\n",
            dst = ps_quote(&dst.to_string_lossy()),
            replace = runner_replace_block(&src),
        );
        let script_path = dir.join("replace.ps1");
        std::fs::write(&script_path, script).expect("写替换脚本");
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&script_path)
            .output()
            .expect("运行替换脚本");
        assert!(
            out.status.success(),
            "占用中的 exe 替换失败: {} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(std::fs::read(&dst).expect("读新文件"), b"new-runner-content");
        assert!(child.try_wait().expect("查询占位进程").is_none(), "旧进程应仍在运行");

        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_tag_text_reads_logon_type() {
        let xml = "<Task><Principals><Principal><LogonType>S4U</LogonType></Principal></Principals></Task>";
        assert_eq!(tag_text(xml, "LogonType"), "S4U");
        assert_eq!(tag_text(xml, "Missing"), "");
    }

    #[test]
    fn test_decode_output_handles_utf16le_bom() {
        let text = "<Task>中文</Task>";
        let mut bytes = vec![0xFF, 0xFE];
        for u in text.encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(decode_output(&bytes), text);
    }

    #[test]
    fn test_parse_whoami_sid() {
        assert_eq!(
            parse_whoami_sid("\"XINGCHEN\\\\27810\",\"S-1-5-21-1-2-3-1001\"\r\n").as_deref(),
            Some("S-1-5-21-1-2-3-1001"),
        );
        assert_eq!(parse_whoami_sid("no sid here\r\n"), None);
        assert_eq!(parse_whoami_sid("\"u\",\"not-a-sid\"\r\n"), None);
    }

    // ── 停止/退出后的状态映射（用户报过“停止后仍显示运行中，刷新才变无响应”） ──

    fn state_with_pid(pid: u32, started_at_ms: u64) -> crate::core::boot::BootRunnerState {
        crate::core::boot::BootRunnerState {
            runner_pid: pid,
            runner_started_at_ms: started_at_ms,
            mode: "s4u".into(),
            ..Default::default()
        }
    }

    #[test]
    fn classify_clean_stop_is_stopped() {
        // runner 正常退出会写 pid=0：必须是“已停止”，而不是“运行中/已退出”
        let st = state_with_pid(0, 123);
        assert_eq!(classify_runner_state(true, false, Some(&st), "S4U").0, "stopped");
    }

    #[test]
    fn classify_dead_runner_is_exited() {
        // 不存在的 pid（或启动时刻对不上）→ 进程确实没了
        let st = state_with_pid(999_999, 1);
        assert_eq!(classify_runner_state(true, false, Some(&st), "S4U").0, "exited");
    }

    #[test]
    fn classify_alive_but_stale_runner_is_stale() {
        // 心跳过期但进程还活着（例如卡住）：与“已退出”区分开
        let pid = std::process::id();
        let started = crate::core::process::process_start_time_millis(pid).unwrap_or(0);
        let st = state_with_pid(pid, started);
        assert_eq!(classify_runner_state(true, false, Some(&st), "S4U").0, "stale");
    }

    #[test]
    fn classify_installed_and_reachable() {
        assert_eq!(classify_runner_state(false, false, None, "S4U").0, "not_installed");
        assert_eq!(classify_runner_state(true, true, None, "S4U").0, "running");
        assert_eq!(classify_runner_state(true, false, None, "S4U").0, "installed_stopped");
    }

    fn boot_item(alive: bool, configured: bool, service_exists: bool, project_exists: bool) -> BootServiceStatus {
        BootServiceStatus {
            service_id: "s".into(),
            name: "n".into(),
            project_id: "p".into(),
            mode: "s4u".into(),
            phase: if alive { "ready" } else { "stopped" }.into(),
            alive,
            pid: None,
            started_at: String::new(),
            ready_at: String::new(),
            exit_code: None,
            error: String::new(),
            log_path: String::new(),
            runner_reachable: true,
            runner_alive: true,
            configured,
            service_enabled: true,
            service_exists,
            project_exists,
        }
    }

    /// 设置页的保留规则：配置在 → 显示；进程活着 → 显示（即使服务/项目已删除）；
    /// 取消开机自启且已停止 → 隐藏；删除且进程已停 → 隐藏。
    #[test]
    fn test_keep_boot_service_item_rules() {
        // 仍配置、服务/项目都在：显示（哪怕当前没跑）
        assert!(keep_boot_service_item(&boot_item(false, true, true, true)));
        // 取消开机自启、已停止：隐藏
        assert!(!keep_boot_service_item(&boot_item(false, false, true, true)));
        // 取消开机自启、但进程还在跑：保留停止入口
        assert!(keep_boot_service_item(&boot_item(true, false, true, true)));
        // 服务已删除、进程还在：保留警告项
        assert!(keep_boot_service_item(&boot_item(true, false, false, true)));
        // 服务已删除、进程已停：隐藏
        assert!(!keep_boot_service_item(&boot_item(false, false, false, true)));
        // 项目已删除、进程还在：保留
        assert!(keep_boot_service_item(&boot_item(true, true, true, false)));
        // 项目已删除、进程已停：隐藏
        assert!(!keep_boot_service_item(&boot_item(false, true, true, false)));
    }
}
