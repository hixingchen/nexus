//! 服务开机启动：manifest 生成、就绪检查定义、runner 状态读取。
//!
//! 设计边界（见 CLAUDE.md 的「服务开机启动」方案）：
//! - **一个 runner 一个运行身份**；服务通过 `boot_enabled` 进入 manifest。
//! - 服务身份永远用 `id`，`name` 只用于展示——同名服务不冲突。
//! - 启动顺序用独立的 `boot_order`，不复用展示排序 `sort_index`。
//! - manifest / 日志 / 状态都放在 Nexus 自己的数据目录下（`<data_dir>/boot`），
//!   runner 拿到的是绝对路径，不依赖登录后才有的用户环境。
//! - `enabled=0` 的服务**不进入 manifest**（开机启动被抑制）。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// manifest 结构版本；runner 与 Nexus 都按它判断兼容性
pub const MANIFEST_VERSION: u32 = 1;
/// runner 心跳超过这个时间就认为它已失联（前端 3 秒轮询，留足余量）
pub const HEARTBEAT_STALE_MS: i64 = 10_000;
/// 启动器状态文件的版本
pub const STATE_VERSION: u32 = 1;

// ─── 凭据模式 ────────────────────────────────────────────────

/// 开机启动凭据模式。一个模式 = 一个计划任务 + 一个 runner + 一套
/// manifest/state/commands/logs；服务按 `boot_credential_mode` 只进入其中一套。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BootMode {
    S4u,
    Password,
}

impl BootMode {
    pub const ALL: [BootMode; 2] = [BootMode::S4u, BootMode::Password];

    pub fn as_str(self) -> &'static str {
        match self {
            BootMode::S4u => "s4u",
            BootMode::Password => "password",
        }
    }

    pub fn display(self) -> &'static str {
        match self {
            BootMode::S4u => "S4U",
            BootMode::Password => "保存密码",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "s4u" => Ok(BootMode::S4u),
            "password" => Ok(BootMode::Password),
            other => Err(format!("未知的开机启动模式: {}", other)),
        }
    }
}

/// 每个模式独立的目录：`<data_dir>/boot/s4u/...` / `<data_dir>/boot/password/...`。
pub fn boot_root(mode: BootMode) -> PathBuf {
    crate::data_dir().join("boot").join(mode.as_str())
}

pub fn manifest_path(mode: BootMode) -> PathBuf {
    boot_root(mode).join("manifest.json")
}

pub fn state_path(mode: BootMode) -> PathBuf {
    boot_root(mode).join("state.json")
}

pub fn logs_dir(mode: BootMode) -> PathBuf {
    boot_root(mode).join("logs")
}

pub fn commands_dir(mode: BootMode) -> PathBuf {
    boot_root(mode).join("commands")
}

pub fn results_dir(mode: BootMode) -> PathBuf {
    boot_root(mode).join("results")
}

/// 服务 id → 安全文件名。
///
/// id 由后端生成 UUID，但 manifest/日志路径直接拼 id；DB 被篡改或导入异常数据时
/// 不能让 `..\` / 路径分隔符把日志写出日志目录。只保留 UUID 会用到的字符。
fn safe_file_stem(id: &str) -> String {
    let stem: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if stem.is_empty() { "unknown".to_string() } else { stem }
}

/// 服务日志路径：**永远按 id**，同名服务不会写进同一个文件。
pub fn service_log_path(mode: BootMode, service_id: &str) -> PathBuf {
    logs_dir(mode).join(format!("{}.log", safe_file_stem(service_id)))
}

/// 旧版单任务的日志路径（升级后未迁移时读取）。
pub fn legacy_service_log_path(service_id: &str) -> PathBuf {
    legacy_boot_dir().join("logs").join(format!("{}.log", safe_file_stem(service_id)))
}

// ─── 旧版单任务路径（迁移用）─────────────────────────────────
//
// 1.10.x 之前只有一个任务，路径是 `<data_dir>/boot/...`。混合模式后旧的
// task/runner 必须被清理，否则它会在开机时和新任务同时拉起同一批服务。

pub fn legacy_boot_dir() -> PathBuf {
    crate::data_dir().join("boot")
}

pub fn legacy_state_path() -> PathBuf {
    legacy_boot_dir().join("state.json")
}

pub fn legacy_commands_dir() -> PathBuf {
    legacy_boot_dir().join("commands")
}

pub fn legacy_results_dir() -> PathBuf {
    legacy_boot_dir().join("results")
}

// ─── 就绪检查 ────────────────────────────────────────────────

/// 服务启动后如何算“就绪”。
///
/// 空串（DB 里）= `None`，表示进程存活即继续下一个服务。v1 只支持这四种：
/// 不做任意正则 / 任意命令，避免把“开机启动配置”变成第二个命令执行面。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BootCheck {
    #[default]
    None,
    Delay {
        seconds: u64,
    },
    Tcp {
        host: String,
        port: u16,
        #[serde(rename = "timeoutSecs", default = "default_timeout_secs")]
        timeout_secs: u64,
    },
    LogContains {
        pattern: String,
        #[serde(rename = "timeoutSecs", default = "default_timeout_secs")]
        timeout_secs: u64,
    },
}

fn default_timeout_secs() -> u64 {
    60
}

/// 校验并归一化 DB 里的 `boot_check` 文本。
///
/// 空串 = 未配置；其余必须是合法 JSON 且参数在合理范围。返回的字符串是**规范化后的
/// JSON**，这样保存入口与 manifest 生成共用同一份判据。
pub fn normalize_check(raw: &str) -> Result<String, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Ok(String::new());
    }
    let check: BootCheck = serde_json::from_str(text)
        .map_err(|e| format!("开机就绪检查不是合法 JSON: {}", e))?;
    match &check {
        BootCheck::None => Ok(String::new()),
        BootCheck::Delay { seconds } => {
            if *seconds > 3600 {
                return Err("开机就绪检查的等待时间不能超过 3600 秒".into());
            }
            serde_json::to_string(&check).map_err(|e| e.to_string())
        }
        BootCheck::Tcp { host, port, timeout_secs } => {
            if host.trim().is_empty() {
                return Err("开机就绪检查的 TCP host 不能为空".into());
            }
            if *port == 0 {
                return Err("开机就绪检查的 TCP 端口必须在 1-65535 之间".into());
            }
            if *timeout_secs == 0 || *timeout_secs > 3600 {
                return Err("开机就绪检查的超时必须在 1-3600 秒之间".into());
            }
            serde_json::to_string(&check).map_err(|e| e.to_string())
        }
        BootCheck::LogContains { pattern, timeout_secs } => {
            if pattern.trim().is_empty() {
                return Err("开机就绪检查的日志关键字不能为空".into());
            }
            if pattern.len() > 512 {
                return Err("开机就绪检查的日志关键字不能超过 512 字节".into());
            }
            if *timeout_secs == 0 || *timeout_secs > 3600 {
                return Err("开机就绪检查的超时必须在 1-3600 秒之间".into());
            }
            serde_json::to_string(&check).map_err(|e| e.to_string())
        }
    }
}

fn parse_check(raw: &str) -> Result<BootCheck, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Ok(BootCheck::None);
    }
    serde_json::from_str(text).map_err(|e| format!("开机就绪检查解析失败: {}", e))
}

// ─── Manifest ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootManifest {
    pub version: u32,
    pub generated_at: String,
    /// 开启开机启动时抓取的环境快照（只含路径类白名单变量，不含密钥）
    pub env: BTreeMap<String, String>,
    pub services: Vec<BootService>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootService {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub command: String,
    pub cwd: String,
    /// 环境快照 + 服务自己的 env_vars（服务值覆盖快照）
    pub env: BTreeMap<String, String>,
    pub check: BootCheck,
    pub required: bool,
    pub order: i32,
    pub log_path: String,
}

/// 环境快照白名单：只取“找得到工具/用户目录”的变量。
///
/// **不能 dump 整个环境**：用户环境里可能有 token/密钥，而 manifest 会落到磁盘上。
/// 服务自己的 `env_vars` 本来就在 DB 里，属于用户显式配置，合并进 manifest 是预期行为。
const ENV_WHITELIST: &[&str] = &[
    "PATH",
    "NVM_HOME",
    "NVM_SYMLINK",
    "JAVA_HOME",
    "M2_HOME",
    "GRADLE_HOME",
    "GOROOT",
    "GOPATH",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "HOMEDRIVE",
    "HOMEPATH",
    "TEMP",
    "TMP",
    "COMSPEC",
    "SystemRoot",
    "WINDIR",
    "PATHEXT",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMDATA",
    "PSModulePath",
];

fn capture_env_snapshot() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    // PATH 必须**总是**写进快照：`merged_path()` 在“进程 PATH 已包含注册表全部项”
    // 时返回 None，而 S4U runner 自己的 PATH 未必等同登录用户的 PATH——漏掉这一项
    // 会让 npm/node 在登录前找不到（只剩 runner 的最小 PATH）。
    let process_path = std::env::var("PATH").unwrap_or_default();
    let path = crate::core::winenv::merged_path().unwrap_or(process_path);
    if !path.trim().is_empty() {
        out.insert("PATH".into(), path);
    }
    for key in ENV_WHITELIST {
        if *key == "PATH" {
            continue;
        }
        let value = crate::core::winenv::env_or_persistent(key)
            .or_else(|| std::env::var(key).ok())
            .filter(|v| !v.trim().is_empty());
        if let Some(v) = value {
            out.insert((*key).to_string(), v);
        }
    }
    out
}

/// 生成**两个模式**的 manifest（会写盘）。每个服务只进入其
/// `boot_credential_mode` 对应的那一个。
pub fn sync_all_manifests(
    db: &crate::database::Database,
) -> Result<Vec<(BootMode, BootManifest)>, String> {
    let mut out = Vec::with_capacity(BootMode::ALL.len());
    for mode in BootMode::ALL {
        out.push((mode, sync_manifest_for(db, mode)?));
    }
    Ok(out)
}

/// 生成指定模式的 manifest（会写盘）。
pub fn sync_manifest_for(
    db: &crate::database::Database,
    mode: BootMode,
) -> Result<BootManifest, String> {
    sync_manifest_to(db, &boot_root(mode), mode)
}

/// `sync_manifest_for` 的可注入目录版本：测试用临时目录，避免写用户的真实 `~/.nexus`。
pub fn sync_manifest_to(
    db: &crate::database::Database,
    boot_dir: &std::path::Path,
    mode: BootMode,
) -> Result<BootManifest, String> {
    let rows = db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, project_id, name, command, cwd, env_vars, boot_check, boot_required, boot_order \
             FROM services WHERE boot_enabled=1 AND enabled=1 AND boot_credential_mode=?1 \
             ORDER BY boot_order, sort_index, name",
        ).map_err(|e| format!("查询开机启动服务失败: {}", e))?;
        let rows = stmt.query_map([mode.as_str()], |row| {
            Ok((
                row.get::<_, String>("id")?,
                row.get::<_, String>("project_id")?,
                row.get::<_, String>("name")?,
                row.get::<_, String>("command")?,
                row.get::<_, String>("cwd")?,
                row.get::<_, String>("env_vars")?,
                row.get::<_, String>("boot_check")?,
                row.get::<_, i32>("boot_required")? != 0,
                row.get::<_, i32>("boot_order")?,
            ))
        }).map_err(|e| format!("读取开机启动服务失败: {}", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| format!("解析开机启动服务失败: {}", e))?);
        }
        Ok(out)
    })?;

    let snapshot = capture_env_snapshot();
    let logs_dir = boot_dir.join("logs");
    std::fs::create_dir_all(&logs_dir).map_err(|e| format!("创建开机启动日志目录失败: {}", e))?;

    let mut services = Vec::with_capacity(rows.len());
    for (id, project_id, name, command, cwd, env_vars, boot_check, required, order) in rows {
        let check = parse_check(&boot_check)
            .map_err(|e| format!("服务「{}」的{}", name, e))?;
        let mut env = snapshot.clone();
        for (k, v) in crate::core::process::parse_env_vars(&env_vars)? {
            env.insert(k, v);
        }
        services.push(BootService {
            log_path: logs_dir.join(format!("{}.log", safe_file_stem(&id))).to_string_lossy().to_string(),
            id,
            project_id,
            name,
            command,
            cwd,
            env,
            check,
            required,
            order,
        });
    }

    let manifest = BootManifest {
        version: MANIFEST_VERSION,
        generated_at: chrono::Utc::now().to_rfc3339(),
        env: snapshot,
        services,
    };
    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| format!("序列化 manifest 失败: {}", e))?;
    write_atomic(&boot_dir.join("manifest.json"), &bytes)?;
    Ok(manifest)
}

/// 原子写：先写同目录临时文件，再 rename 覆盖。
///
/// Windows 上 `rename` 覆盖已存在文件的行为随实现/文件系统而异；失败时删掉目标再试一次。
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| format!("路径没有父目录: {}", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败 {}: {}", parent.display(), e))?;
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("write"),
        std::process::id()
    ));
    std::fs::write(&tmp, bytes).map_err(|e| format!("写临时文件失败 {}: {}", tmp.display(), e))?;
    if std::fs::rename(&tmp, path).is_ok() {
        return Ok(());
    }
    let _ = std::fs::remove_file(path);
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("替换文件失败 {}: {}", path.display(), e)
    })
}

// ─── Runner 状态 ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BootRunnerState {
    pub version: u32,
    pub runner_pid: u32,
    /// runner 进程的启动时刻（毫秒），用于排除 pid 复用
    pub runner_started_at_ms: u64,
    pub started_at: String,
    pub heartbeat_at: String,
    pub mode: String,
    pub services: BTreeMap<String, BootServiceState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BootServiceState {
    /// starting / ready / failed / exited / stopped / skipped
    pub phase: String,
    pub pid: Option<u32>,
    pub process_started_at: Option<u64>,
    pub started_at: Option<String>,
    pub ready_at: Option<String>,
    pub exit_code: Option<i32>,
    pub error: String,
    pub log_path: String,
    pub name: String,
    pub project_id: String,
}

pub fn read_state(mode: BootMode) -> Option<BootRunnerState> {
    read_state_at(&state_path(mode))
}

/// 旧版单任务的状态（迁移时用来停止旧 runner）。
pub fn read_legacy_state() -> Option<BootRunnerState> {
    read_state_at(&legacy_state_path())
}

fn read_state_at(path: &std::path::Path) -> Option<BootRunnerState> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// runner 进程本身是否还活着（**不看心跳**）。
///
/// pid + 启动时刻双重校验，排除 pid 复用。runner 卡住（心跳过期）时进程仍可能活着，
/// 界面要能区分“进程还在但没响应”和“已经退出”。
pub fn runner_process_alive(state: &BootRunnerState) -> bool {
    if state.runner_pid == 0 {
        return false;
    }
    crate::core::process::process_start_time_millis(state.runner_pid)
        .map(|t| t == state.runner_started_at_ms)
        .unwrap_or(false)
}

/// state 里记录的服务进程是否还活着。
///
/// 这是“服务在不在跑”的**独立判据**：不依赖 runner 心跳，runner 退出/卡住后
/// 仍能正确显示由它拉起、且仍然存活的子进程（否则界面会把它们全当成没启动）。
pub fn service_alive(s: &BootServiceState) -> bool {
    let Some(pid) = s.pid else { return false; };
    let Some(expected) = s.process_started_at else { return false; };
    crate::core::process::process_start_time_millis(pid)
        .map(|t| t == expected)
        .unwrap_or(false)
}

/// 状态是否来自一个仍然存活、且心跳新鲜的 runner。
///
/// 心跳新鲜还不够：runner 被强杀后状态文件会停在最后一次心跳（10 秒内），
/// 必须再用 pid 身份复核一次，避免“进程已死但状态看起来还活着”。
pub fn runner_reachable(state: &BootRunnerState) -> bool {
    if state.runner_pid == 0 {
        return false;
    }
    let fresh = chrono::DateTime::parse_from_rfc3339(&state.heartbeat_at)
        .map(|t| chrono::Utc::now().signed_duration_since(t.with_timezone(&chrono::Utc)).num_milliseconds() <= HEARTBEAT_STALE_MS)
        .unwrap_or(false);
    if !fresh {
        return false;
    }
    runner_process_alive(state)
}

// ─── 控制命令（文件队列）─────────────────────────────────────
//
// 为什么用文件而不是命名管道：v1 要的是“能停/能重启/能关机”，文件队列同样可靠，
// 而且 runner 崩溃后命令不会卡在一个没有监听者的管道上。每条命令一个文件、每个结果
// 一个文件，双方都做原子写；命令按 id 幂等。

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootCommand {
    pub id: String,
    /// start / stop / restart / shutdown / reload / status
    pub op: String,
    #[serde(default)]
    pub service_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootCommandResult {
    pub id: String,
    pub ok: bool,
    pub error: String,
}

fn command_file(dir: &std::path::Path, id: &str) -> PathBuf {
    dir.join(format!("{}.json", id))
}

/// 向指定模式的 runner 发送一条命令并等待结果（最长 `timeout`）。
///
/// 调用方必须在**阻塞线程**里调用（runner 的轮询是 250ms 级）。
pub fn send_command(
    mode: BootMode,
    command: BootCommand,
    timeout: Duration,
) -> Result<BootCommandResult, String> {
    send_command_to(&commands_dir(mode), &results_dir(mode), command, timeout)
}

/// 向旧版单任务 runner 发送命令（迁移时停止它）。
pub fn send_legacy_command(
    command: BootCommand,
    timeout: Duration,
) -> Result<BootCommandResult, String> {
    send_command_to(&legacy_commands_dir(), &legacy_results_dir(), command, timeout)
}

fn send_command_to(
    commands_dir: &std::path::Path,
    results_dir: &std::path::Path,
    command: BootCommand,
    timeout: Duration,
) -> Result<BootCommandResult, String> {
    std::fs::create_dir_all(commands_dir).map_err(|e| format!("创建命令目录失败: {}", e))?;
    std::fs::create_dir_all(results_dir).map_err(|e| format!("创建结果目录失败: {}", e))?;
    let cmd_path = command_file(commands_dir, &command.id);
    let res_path = command_file(results_dir, &command.id);
    let _ = std::fs::remove_file(&res_path);
    let cmd_bytes = serde_json::to_vec(&command)
        .map_err(|e| format!("序列化命令失败: {}", e))?;
    write_atomic(&cmd_path, &cmd_bytes)?;

    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Ok(bytes) = std::fs::read(&res_path) {
            let result: BootCommandResult = serde_json::from_slice(&bytes)
                .map_err(|e| format!("解析命令结果失败: {}", e))?;
            let _ = std::fs::remove_file(&cmd_path);
            let _ = std::fs::remove_file(&res_path);
            return Ok(result);
        }
        if std::time::Instant::now() >= deadline {
            let _ = std::fs::remove_file(&cmd_path);
            return Err("开机启动器没有响应（命令超时）".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 生成一次命令 id：时间戳 + 进程 id + 原子计数，避免并发请求撞名。
pub fn new_command_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        chrono::Utc::now().timestamp_millis(),
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_check_accepts_supported_shapes() {
        assert_eq!(normalize_check("").unwrap(), "");
        assert_eq!(normalize_check("  ").unwrap(), "");
        assert_eq!(normalize_check(r#"{"type":"none"}"#).unwrap(), "");
        let delay = normalize_check(r#"{"type":"delay","seconds":5}"#).unwrap();
        assert!(delay.contains("\"type\":\"delay\""));
        let tcp = normalize_check(r#"{"type":"tcp","host":"127.0.0.1","port":3306,"timeoutSecs":60}"#).unwrap();
        assert!(tcp.contains("\"port\":3306"));
        let log = normalize_check(r#"{"type":"logContains","pattern":"ready for connections","timeoutSecs":120}"#).unwrap();
        assert!(log.contains("ready for connections"));
    }

    #[test]
    fn test_normalize_check_rejects_invalid() {
        assert!(normalize_check("not json").is_err());
        assert!(normalize_check(r#"{"type":"tcp","host":"","port":3306}"#).is_err());
        assert!(normalize_check(r#"{"type":"tcp","host":"127.0.0.1","port":0}"#).is_err());
        assert!(normalize_check(r#"{"type":"tcp","host":"127.0.0.1","port":3306,"timeoutSecs":0}"#).is_err());
        assert!(normalize_check(r#"{"type":"logContains","pattern":""}"#).is_err());
        assert!(normalize_check(r#"{"type":"delay","seconds":99999}"#).is_err());
        assert!(normalize_check(r#"{"type":"unknown"}"#).is_err());
    }

    /// manifest 只看 `boot_enabled=1 AND enabled=1` 且模式匹配的服务，
    /// 顺序按 boot_order，服务 env 覆盖快照；两个模式的 manifest 互不混入。
    #[test]
    fn test_sync_manifest_filters_orders_and_merges_env() {
        let conn = rusqlite::Connection::open_in_memory().expect("内存库");
        crate::database::init_schema(&conn).expect("建表");
        conn.execute(
            "INSERT INTO projects (id, name, path, sort_index, created_at) VALUES ('p1','P','C:/p',0,'t')",
            [],
        ).expect("插入项目");
        let insert = |id: &str, enabled: i32, boot: i32, order: i32, check: &str, env: &str, mode: &str| {
            conn.execute(
                "INSERT INTO services (id, project_id, name, command, cwd, env_vars, enabled, \
                 boot_enabled, boot_order, boot_check, boot_required, boot_credential_mode) \
                 VALUES (?1,'p1',?2,'echo x','C:/p',?3,?4,?5,?6,?7,1,?8)",
                rusqlite::params![id, format!("name-{}", id), env, enabled, boot, order, check, mode],
            ).expect("插入服务");
        };
        insert("s2", 1, 1, 2, "", "B=2", "s4u");
        insert("s1", 1, 1, 1, r#"{"type":"tcp","host":"127.0.0.1","port":3306,"timeoutSecs":60}"#, "A=1", "s4u");
        insert("s3", 0, 1, 0, "", "", "s4u"); // enabled=0：必须被过滤
        insert("s4", 1, 0, 0, "", "", "s4u"); // boot_enabled=0：必须被过滤
        insert("s5", 1, 1, 0, "", "", "password"); // 密码模式：只进 password manifest

        let db = crate::database::Database::from_connection(conn);
        let dir = std::env::temp_dir().join(format!("nexus-boot-manifest-{}", uuid::Uuid::new_v4()));
        let s4u_dir = dir.join("s4u");
        let pwd_dir = dir.join("password");
        let manifest = sync_manifest_to(&db, &s4u_dir, BootMode::S4u).expect("生成 S4U manifest");
        let pwd = sync_manifest_to(&db, &pwd_dir, BootMode::Password).expect("生成密码 manifest");

        assert_eq!(manifest.services.len(), 2);
        assert_eq!(manifest.services[0].id, "s1", "boot_order 小的先出现");
        assert_eq!(manifest.services[1].id, "s2");
        assert!(manifest.services.iter().all(|s| s.id != "s5"), "密码服务不能进 S4U manifest");
        assert_eq!(manifest.services[0].env.get("A").map(String::as_str), Some("1"));
        assert_eq!(manifest.services[0].env.get("B"), None);
        assert_eq!(
            manifest.services[0].check,
            BootCheck::Tcp { host: "127.0.0.1".into(), port: 3306, timeout_secs: 60 }
        );
        assert!(manifest.services[0].log_path.contains("s4u"));
        assert!(manifest.services[0].log_path.ends_with("s1.log"));

        assert_eq!(pwd.services.len(), 1);
        assert_eq!(pwd.services[0].id, "s5");
        assert!(pwd.services[0].log_path.contains("password"));

        // 写盘的是同一份结构（runner 读到的不是内存里的副本）
        let written: BootManifest = serde_json::from_slice(
            &std::fs::read(s4u_dir.join("manifest.json")).expect("读 manifest"),
        ).expect("解析 manifest");
        assert_eq!(written.services.len(), 2);
        assert_eq!(written.version, MANIFEST_VERSION);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_service_log_path_cannot_escape_logs_dir() {
        // DB 被篡改时 id 可能带路径分隔符/上跳；日志必须仍落在 logs 目录内
        let p = service_log_path(BootMode::S4u, r"..\..\evil");
        assert!(p.starts_with(logs_dir(BootMode::S4u)));
        assert!(!p.to_string_lossy().contains(".."));
        assert_eq!(p.file_name().unwrap().to_string_lossy(), "______evil.log");

        let l = legacy_service_log_path(r"C:\Windows\System32\evil");
        assert!(l.starts_with(legacy_boot_dir().join("logs")));
        assert_eq!(l.file_name().unwrap().to_string_lossy(), "C__Windows_System32_evil.log");
    }
}
