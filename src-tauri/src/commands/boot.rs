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

use crate::core::boot::{self, BootCommand};
use crate::core::logpipe::LogLine;
use crate::AppState;

/// 任务名前缀；实际名字带用户名，避免同一台机器上多个 Windows 账户撞任务。
/// 不用 `Nexus\...` 文件夹形式：`schtasks /Create` 在文件夹不存在时不会自动创建。
const TASK_PREFIX: &str = "Nexus Boot Runner";

#[derive(Debug, Clone, Serialize)]
pub struct BootRunnerStatus {
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
    /// 当前运行实例的模式（state.json 的 mode；任务模式可能已切换但实例还没重启）
    pub running_mode: String,
    pub runner_pid: Option<u32>,
    pub heartbeat_at: String,
    /// DB 里 boot_enabled=1 且 enabled=1 的服务数
    pub boot_service_count: usize,
    /// not_installed / installed_stopped / running / unreachable
    pub state: String,
    pub message: String,
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
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
        let out = std::process::Command::new(format!(r"{}\System32\whoami.exe", root))
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

pub fn task_name() -> String {
    let (_, user) = current_user();
    // 带上 SID 尾号：同名的本地账户与域账户在同一台机器上不会撞任务名
    let sid_suffix = current_user_sid()
        .and_then(|sid| sid.rsplit('-').next().map(|s| s.to_string()))
        .filter(|s| !s.is_empty());
    match sid_suffix {
        Some(suffix) => format!("{} ({}-{})", TASK_PREFIX, sanitize_user_name(&user), suffix),
        None => format!("{} ({})", TASK_PREFIX, sanitize_user_name(&user)),
    }
}

/// 稳定安装位置：`%ProgramData%\Nexus\boot\runner\nexus-boot-runner.exe`。
///
/// 放在 GUI 安装目录之外，GUI 更新（替换 Nexus.exe 及同目录文件）不会碰到正在运行的
/// runner；任务 action 也就不用随安装路径变化。runner 以当前用户 S4U 运行，读取/执行
/// 这个文件即可（ProgramData 默认对 Users 有读/执行权限）。
pub fn runner_install_path() -> PathBuf {
    let program_data = std::env::var("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(r"C:\ProgramData"));
    program_data.join("Nexus").join("boot").join("runner").join("nexus-boot-runner.exe")
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
    std::env::var("SystemRoot")
        .map(|root| format!(r"{}\System32\schtasks.exe", root))
        .unwrap_or_else(|_| r"C:\Windows\System32\schtasks.exe".to_string())
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

/// 一次提权完成“复制 runner + 注册任务”：只弹一次 UAC。
///
/// 复制和 `schtasks /Create` 放在同一条 `cmd /c` 里，用 `&&` 串起来（任一步失败就不
/// 继续后面的步骤）。路径全部无条件加引号，防止路径里的 `&` 被 cmd 当成命令分隔符。
fn install_params(
    src: &std::path::Path,
    dst: &std::path::Path,
    task: &str,
    xml: &std::path::Path,
) -> Result<String, String> {
    let dir = dst.parent().ok_or_else(|| "runner 目标路径没有父目录".to_string())?;
    let q = |p: &std::path::Path| format!("\"{}\"", p.to_string_lossy().replace('"', ""));
    // 注意不能用 `if not exist ... mkdir ... && copy ...`：cmd 会把 `&&` 绑在 if 的
    // 命令上，目录已存在（修复/重装）时整个 copy+schtasks 都会被跳过。
    // `mkdir ... 2>nul & copy ... && schtasks ...`：mkdir 失败不影响后续，copy 失败则
    // 阻止注册任务。
    Ok(format!(
        "/c mkdir {} 2>nul & copy /Y {} {} && {} /Create /TN {} /XML {} /F",
        q(dir), q(src), q(dst), q(&std::path::PathBuf::from(schtasks_path())),
        q(&std::path::PathBuf::from(task)), q(xml),
    ))
}

/// PowerShell 单引号字符串字面量（内部的 `'` 用 `''` 转义）。
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// 保存密码模式的安装脚本。
///
/// 密码**不在**脚本/命令行/日志/数据库中：脚本只负责复制 runner 与调用
/// `Get-Credential`（Windows 原生凭据对话框）和 `Register-ScheduledTask`，
/// 密码由 Task Scheduler 保存在 LSA secrets 中。
fn password_install_script(
    src: &std::path::Path,
    dst: &std::path::Path,
    task: &str,
    xml: &std::path::Path,
    error_path: &std::path::Path,
) -> Result<String, String> {
    let dir = dst.parent().ok_or_else(|| "runner 目标路径没有父目录".to_string())?;
    let (domain, user) = current_user();
    let username = if domain.is_empty() { user } else { format!("{}\\{}", domain, user) };
    Ok(format!(
        "$ErrorActionPreference = 'Stop'\n\
         try {{\n\
           New-Item -ItemType Directory -Force -Path {} | Out-Null\n\
           Copy-Item -LiteralPath {} -Destination {} -Force\n\
           $cred = Get-Credential -UserName {} -Message 'Nexus 服务开机启动需要 Windows 密码（只保存在 Windows 计划任务中）'\n\
           if ($null -eq $cred) {{ exit 2 }}\n\
           $xml = Get-Content -Raw -LiteralPath {}\n\
           Register-ScheduledTask -TaskName {} -Xml $xml -User $cred.UserName -Password $cred.GetNetworkCredential().Password -Force | Out-Null\n\
         }} catch {{\n\
           $_.Exception.Message | Out-File -FilePath {} -Encoding utf8\n\
           exit 1\n\
         }}\n",
        ps_quote(&dir.to_string_lossy()),
        ps_quote(&src.to_string_lossy()),
        ps_quote(&dst.to_string_lossy()),
        ps_quote(&username),
        ps_quote(&xml.to_string_lossy()),
        ps_quote(task),
        ps_quote(&error_path.to_string_lossy()),
    ))
}

fn encode_powershell(script: &str) -> String {
    use base64::Engine as _;
    let utf16: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    base64::engine::general_purpose::STANDARD.encode(utf16)
}

/// 一次 UAC + 一个原生凭据对话框完成“复制 runner + 注册保存密码任务”。
fn install_password_elevated(
    src: &std::path::Path,
    dst: &std::path::Path,
    task: &str,
    xml: &std::path::Path,
) -> Result<(), String> {
    let error_path = boot::boot_dir().join("password-install-error.txt");
    let _ = std::fs::remove_file(&error_path);
    let script = password_install_script(src, dst, task, xml, &error_path)?;
    let encoded = encode_powershell(&script);
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    let powershell = format!(r"{}\System32\WindowsPowerShell\v1.0\powershell.exe", root);
    let params = format!("-NoProfile -ExecutionPolicy Bypass -EncodedCommand {}", encoded);
    elevated_run(&powershell, &params).map_err(|e| {
        if e.contains("退出码 2") {
            return "已取消密码输入，开机启动任务未变更".to_string();
        }
        // PowerShell 的 stderr 拿不到（ShellExecuteEx 没有管道），让它把异常写进文件
        let detail = std::fs::read_to_string(&error_path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let _ = std::fs::remove_file(&error_path);
        match detail {
            Some(d) => format!("注册保存密码的开机启动任务失败：{}", d),
            None => format!(
                "注册保存密码的开机启动任务失败（如启动器正在运行，请先停止它）：{}",
                e
            ),
        }
    })
}

fn write_task_xml(
    exe: &std::path::Path,
    manifest: &std::path::Path,
    mode: &str,
) -> Result<PathBuf, String> {
    let xml = task_xml(exe, manifest, mode)?;
    let path = boot::boot_dir().join("task.xml");
    // schtasks 对任务 XML 的默认期望是 UTF-16（与其他 Windows 调度文件一致）。
    // 写成 UTF-16LE + BOM，避免“UTF-8 内容 + UTF-16 声明”或者中文描述被误读。
    let mut bytes = vec![0xFF, 0xFE];
    for unit in xml.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    boot::write_atomic(&path, &bytes)?;
    Ok(path)
}

fn delete_params(name: &str) -> String {
    format!("/Delete /TN {} /F", quote_arg(name))
}

// ─── 状态 ────────────────────────────────────────────────────

fn boot_service_count(state: &AppState) -> usize {
    state.db.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM services WHERE boot_enabled=1 AND enabled=1",
            [],
            |r| r.get::<_, i64>(0),
        ).map(|n| n as usize).map_err(|e| format!("统计开机启动服务失败: {}", e))
    }).unwrap_or(0)
}

fn build_status(state: &AppState) -> Result<BootRunnerStatus, String> {
    let name = task_name();
    let xml = query_task_xml(&name)?;
    let installed = xml.is_some();
    let logon_type = xml.as_deref().map(|x| tag_text(x, "LogonType")).unwrap_or_default();
    let runner_path = runner_install_path();
    let runner_installed = runner_path.is_file();
    // 安装包里的 runner 与 ProgramData 里的版本是否一致（用大小做轻量版本判断）：
    // GUI 更新后 sidecar 会变，但 ProgramData 的稳定 runner 不会自动更新；提示修复。
    let bundled_len = bundled_runner_path()
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .unwrap_or(0);
    let installed_len = std::fs::metadata(&runner_path).map(|m| m.len()).unwrap_or(0);
    let runner_current = bundled_len > 0 && installed_len == bundled_len;
    let manifest = boot::manifest_path();
    let task_path_ok = match xml.as_deref() {
        Some(x) => {
            x.contains(&runner_path.to_string_lossy().to_string())
                && x.contains(&manifest.to_string_lossy().to_string())
                && runner_installed
                && runner_current
        }
        None => false,
    };
    let runner_state = boot::read_state();
    let reachable = runner_state.as_ref().map(boot::runner_reachable).unwrap_or(false);
    let (runner_pid, heartbeat_at, running_mode) = runner_state
        .as_ref()
        .map(|s| (Some(s.runner_pid), s.heartbeat_at.clone(), s.mode.clone()))
        .unwrap_or((None, String::new(), String::new()));
    let count = boot_service_count(state);

    let (state_name, message) = if !installed {
        ("not_installed", "尚未安装开机启动器；安装需要一次管理员授权".to_string())
    } else if reachable {
        ("running", "启动器运行中，开机服务由它管理".to_string())
    } else if runner_state.is_some() {
        ("unreachable", "任务已安装，但启动器没有响应（状态可能是旧的）".to_string())
    } else {
        ("installed_stopped", "任务已安装，启动器当前未运行".to_string())
    };
    let mut message = message;
    if installed && !runner_installed {
        message.push_str("；启动器文件不存在，请点「修复 / 重新安装」");
    } else if installed && !runner_current {
        message.push_str("；启动器版本与当前 Nexus 安装包不一致，请点「修复 / 重新安装」");
    } else if installed && !task_path_ok {
        message.push_str("；任务指向的启动器路径或 manifest 与当前配置不一致，请重新安装");
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
        installed,
        task_name: name,
        task_logon_type: logon_type,
        task_path_ok,
        runner_path: runner_path.to_string_lossy().to_string(),
        runner_installed,
        runner_reachable: reachable,
        running_mode,
        runner_pid,
        heartbeat_at,
        boot_service_count: count,
        state: state_name.to_string(),
        message,
    })
}

/// 读取当前 runner 状态（不查询计划任务；给热路径用）
pub fn runner_reachable_now() -> bool {
    boot::read_state().map(|s| boot::runner_reachable(&s)).unwrap_or(false)
}

/// runner 当前拥有的服务 id（仅 starting/ready）
pub fn boot_running_ids() -> HashSet<String> {
    let Some(state) = boot::read_state() else { return HashSet::new() };
    if !boot::runner_reachable(&state) { return HashSet::new(); }
    state.services.iter()
        .filter(|(_, s)| matches!(s.phase.as_str(), "starting" | "ready"))
        .map(|(id, _)| id.clone())
        .collect()
}

/// runner 报告失败/异常退出的服务（供 get_running 合并）
pub fn boot_failed_services() -> Vec<crate::core::process::FailedService> {
    let Some(state) = boot::read_state() else { return Vec::new() };
    if !boot::runner_reachable(&state) { return Vec::new(); }
    state.services.iter().filter_map(|(id, s)| {
        let failed = s.phase == "failed"
            || (s.phase == "exited" && s.exit_code.unwrap_or(0) != 0);
        if !failed { return None; }
        Some(crate::core::process::FailedService {
            service_id: id.clone(),
            exit_code: s.exit_code,
            timestamp: s.ready_at.clone().or_else(|| s.started_at.clone()).unwrap_or_default(),
            source: "boot".into(),
        })
    }).collect()
}

/// runner 当前拥有的全部服务 id（含 failed/starting/ready/pending）。
/// 「全部停止」用它把属于 runner 的服务交给 runner 停，而不是在本地进程表里空转。
pub fn boot_owned_ids() -> HashSet<String> {
    let Some(state) = boot::read_state() else { return HashSet::new() };
    if !boot::runner_reachable(&state) { return HashSet::new(); }
    state.services.keys().cloned().collect()
}

/// 服务是否由 runner 管理（在状态文件里出现）。
pub fn is_boot_managed(service_id: &str) -> bool {
    boot::read_state()
        .map(|s| s.services.contains_key(service_id))
        .unwrap_or(false)
}

/// 通知 runner 重新读 manifest；runner 未运行时静默跳过。
pub fn reload_if_running() {
    if !runner_reachable_now() { return; }
    let cmd = BootCommand {
        id: boot::new_command_id(),
        op: "reload".into(),
        service_id: None,
    };
    if let Err(e) = boot::send_command(cmd, Duration::from_secs(3)) {
        log::warn!("[nexus] 通知开机启动器 reload 失败: {}", e);
    }
}

/// 向 runner 发一条服务命令（start/stop/restart）。返回 runner 的真实结果。
pub fn send_service_command(op: &str, service_id: &str) -> Result<(), String> {
    if !runner_reachable_now() {
        return Err("开机启动器未运行".into());
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
    let result = boot::send_command(cmd, timeout)?;
    if result.ok { Ok(()) } else if result.error.is_empty() {
        Err(format!("开机启动器执行 {} 失败", op))
    } else {
        Err(result.error)
    }
}

/// 读开机服务日志尾部（给日志面板的快照/轮询用）。
/// 返回 None = 这个服务没有 boot 日志文件。
pub fn read_boot_log_tail(service_id: &str, max_bytes: usize) -> Option<Vec<Arc<LogLine>>> {
    let path = boot::service_log_path(service_id);
    let bytes = std::fs::read(&path).ok()?;
    let start = bytes.len().saturating_sub(max_bytes);
    let slice = &bytes[start..];
    // 与编辑器/日志跟随同一套解码口径：GBK 输出不能变成一串替换字符
    let decoded = crate::core::textenc::decode_to_string(slice);
    let text = if start > 0 {
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
pub async fn get_boot_runner_status(app: tauri::AppHandle) -> Result<BootRunnerStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        build_status(&state)
    }).await.map_err(|e| format!("读取开机启动器状态失败: {}", e))?
}

#[tauri::command]
pub async fn install_boot_runner(
    app: tauri::AppHandle,
    mode: String,
) -> Result<BootRunnerStatus, String> {
    if mode != "s4u" && mode != "password" {
        return Err(format!("未知的开机启动模式: {}", mode));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        // 先落 manifest，再建任务：任务首次启动时就要能读到完整配置
        boot::sync_manifest(&state.db)?;
        // 独立 runner：复制到 ProgramData 稳定目录 + 注册任务，**一次 UAC** 完成。
        let bundled = bundled_runner_path()?;
        let runner_path = runner_install_path();
        let xml_path = write_task_xml(&runner_path, &boot::manifest_path(), &mode)?;
        let name = task_name();
        let result = if mode == "password" {
            install_password_elevated(&bundled, &runner_path, &name, &xml_path)
        } else {
            let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
            let cmd = format!(r"{}\System32\cmd.exe", root);
            let params = install_params(&bundled, &runner_path, &name, &xml_path)?;
            elevated_run(&cmd, &params).map_err(|e| format!(
                "安装失败（复制启动器或注册任务未完成；如启动器正在运行，请先在设置中停止）：{}",
                e
            ))
        };
        let _ = std::fs::remove_file(&xml_path);
        result?;
        // 回读 + 文件校验：schtasks 返回 0 不等于任务真的按我们要求注册了（铁律 21）
        let src_len = std::fs::metadata(&bundled).map(|m| m.len()).unwrap_or(0);
        let dst_len = std::fs::metadata(&runner_path).map(|m| m.len()).unwrap_or(0);
        if dst_len == 0 || src_len != dst_len {
            return Err(format!(
                "启动器复制校验失败（源 {} 字节，目标 {} 字节）：{}",
                src_len, dst_len, runner_path.display()
            ));
        }
        let expected = if mode == "password" { "Password" } else { "S4U" };
        let xml = query_task_xml(&name)?
            .ok_or_else(|| "任务创建后回读失败（任务不存在）".to_string())?;
        if tag_text(&xml, "LogonType") != expected {
            return Err(format!(
                "任务已创建，但回读到的 LogonType 不是 {}（实际: {}）",
                expected,
                tag_text(&xml, "LogonType")
            ));
        }
        // 已有一个 runner 在跑时，让它们重新读一次刚写好的 manifest（不重启服务）
        reload_if_running();
        build_status(&state)
    }).await.map_err(|e| format!("安装开机启动器任务失败: {}", e))?
}

#[tauri::command]
pub async fn uninstall_boot_runner(app: tauri::AppHandle) -> Result<BootRunnerStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        // 先让 runner 干净退出；它退出时 Job Object 会带走服务
        if runner_reachable_now() {
            let cmd = BootCommand { id: boot::new_command_id(), op: "shutdown".into(), service_id: None };
            // 逐个停服务最坏 3s×N；11 个服务就可能 30s+，8 秒会把“正在正常收尾”误报成失败
            if let Err(e) = boot::send_command(cmd, Duration::from_secs(120)) {
                return Err(format!("停止开机启动器失败，未删除任务: {}", e));
            }
        }
        let name = task_name();
        if query_task_xml(&name)?.is_some() {
            elevated_schtasks(&delete_params(&name))?;
        }
        build_status(&state)
    }).await.map_err(|e| format!("卸载开机启动器任务失败: {}", e))?
}

#[tauri::command]
pub async fn start_boot_runner(app: tauri::AppHandle) -> Result<BootRunnerStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        boot::sync_manifest(&state.db)?;
        let name = task_name();
        if query_task_xml(&name)?.is_none() {
            return Err("开机启动器尚未安装，请先安装".into());
        }
        let (ok, text) = run_schtasks(&["/Run", "/TN", &name])?;
        if !ok {
            return Err(format!("启动开机启动器失败: {}", text.trim()));
        }
        build_status(&state)
    }).await.map_err(|e| format!("启动开机启动器失败: {}", e))?
}

#[tauri::command]
pub async fn stop_boot_runner(app: tauri::AppHandle) -> Result<BootRunnerStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        if runner_reachable_now() {
            let cmd = BootCommand { id: boot::new_command_id(), op: "shutdown".into(), service_id: None };
            let _ = boot::send_command(cmd, Duration::from_secs(120));
        }
        build_status(&state)
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
    if let Err(e) = boot::sync_manifest(&state.db) {
        error = e;
    } else {
        reload_if_running();
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

    #[test]
    fn test_password_install_script_uses_native_credential_dialog() {
        let script = password_install_script(
            std::path::Path::new(r"C:\App Folder\nexus-boot-runner.exe"),
            std::path::Path::new(r"C:\ProgramData\Nexus\boot\runner\nexus-boot-runner.exe"),
            "Nexus Boot Runner (u-1001)",
            std::path::Path::new(r"C:\Users\u\.nexus\boot\task.xml"),
            std::path::Path::new(r"C:\Users\u\.nexus\boot\password-install-error.txt"),
        ).expect("生成密码安装脚本");
        assert!(script.contains("Get-Credential"));
        assert!(script.contains("Register-ScheduledTask"));
        assert!(script.contains("Copy-Item"));
        // 脚本里不能出现任何密码字面量/参数
        assert!(!script.contains("Password="));
    }

    /// 只做 PowerShell **语法解析**，不执行：密码脚本里有 Get-Credential/注册任务，
    /// 但语法错误会让密码模式静默失败，值得在 CI 里挡一次。
    #[cfg(windows)]
    #[test]
    fn test_password_install_script_has_valid_powershell_syntax() {
        let dir = std::env::temp_dir().join(format!("nexus_ps_parse_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let script_path = dir.join("install.ps1");
        let parse_path = dir.join("parse.ps1");
        let script = password_install_script(
            std::path::Path::new(r"C:\App Folder\nexus-boot-runner.exe"),
            std::path::Path::new(r"C:\ProgramData\Nexus\boot\runner\nexus-boot-runner.exe"),
            "Nexus Boot Runner (u-1001)",
            std::path::Path::new(r"C:\Users\u\.nexus\boot\task.xml"),
            std::path::Path::new(r"C:\Users\u\.nexus\boot\password-install-error.txt"),
        ).expect("生成密码安装脚本");
        std::fs::write(&script_path, script).expect("写脚本");
        std::fs::write(
            &parse_path,
            "$src = Get-Content -Raw -LiteralPath $args[0]\n\
             $tokens = $null\n\
             $errors = $null\n\
             [System.Management.Automation.Language.Parser]::ParseInput($src, [ref]$tokens, [ref]$errors) | Out-Null\n\
             if ($errors.Count -gt 0) { $errors | ForEach-Object { Write-Error ($_.Message) }; exit 1 }\n",
        ).expect("写解析脚本");

        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&parse_path)
            .arg(&script_path)
            .output()
            .expect("运行 PowerShell 语法检查");
        assert!(
            out.status.success(),
            "PowerShell 语法检查失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 真跑一次 `if not exist ... mkdir ... && copy ...` 链：
    /// 第一次目录不存在、第二次已存在，两次都必须成功复制（cmd 的 `if` 在条件为假时
    /// 也必须让 `&&` 继续）。
    ///
    /// Windows-only：用 `raw_arg` 模拟 `ShellExecuteEx` 传 `lpParameters` 的形态，
    /// 避免 Rust 自动加引号改变 cmd 的解析。
    #[cfg(windows)]
    #[test]
    fn test_cmd_chain_creates_dir_and_copies_twice() {
        use std::os::windows::process::CommandExt;
        let dir = std::env::temp_dir().join(format!("nexus_cmd_chain_{}", uuid::Uuid::new_v4()));
        let sub = dir.join("sub");
        let src = dir.join("a.txt");
        let dst = sub.join("b.txt");
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let q = |p: &std::path::Path| format!("\"{}\"", p.display());
        let command = format!(
            "mkdir {} 2>nul & copy /Y {} {} && echo CHAIN_OK",
            q(&sub), q(&src), q(&dst),
        );

        std::fs::write(&src, "first").expect("写源文件");
        let mut first_cmd = std::process::Command::new("cmd");
        first_cmd.raw_arg(format!("/C {}", command));
        let first = first_cmd.output().expect("跑 cmd");
        assert!(first.status.success(), "第一次链式命令失败: {:?}", String::from_utf8_lossy(&first.stderr));
        assert_eq!(std::fs::read_to_string(&dst).expect("读副本"), "first");

        std::fs::write(&src, "second").expect("改写源文件");
        let mut second_cmd = std::process::Command::new("cmd");
        second_cmd.raw_arg(format!("/C {}", command));
        let second = second_cmd.output().expect("跑 cmd");
        assert!(second.status.success(), "目录已存在时第二次链式命令失败: {:?}", String::from_utf8_lossy(&second.stderr));
        assert_eq!(std::fs::read_to_string(&dst).expect("读副本"), "second");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_install_params_quotes_paths_and_chains_copy_and_task() {
        let params = install_params(
            std::path::Path::new(r"C:\App Folder\nexus-boot-runner.exe"),
            std::path::Path::new(r"C:\ProgramData\Nexus\boot\runner\nexus-boot-runner.exe"),
            "Nexus Boot Runner (u-1001)",
            std::path::Path::new(r"C:\Users\u\.nexus\boot\task.xml"),
        ).expect("生成安装参数");
        assert!(params.contains("mkdir \"C:\\ProgramData\\Nexus\\boot\\runner\" 2>nul & copy /Y"));
        assert!(params.contains("copy /Y \"C:\\App Folder\\nexus-boot-runner.exe\""));
        assert!(params.contains("schtasks.exe\" /Create /TN \"Nexus Boot Runner (u-1001)\""));
        assert!(params.contains("/XML \"C:\\Users\\u\\.nexus\\boot\\task.xml\" /F"));
        assert!(params.contains(" && "), "复制失败时必须阻止注册任务");
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
}
