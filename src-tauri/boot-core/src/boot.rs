//! 开机启动的共享数据模型与文件协议。
//!
//! Windows 桌面版与 Linux 服务端共用：
//! - manifest / state / command 的 JSON 形状；
//! - 原子写、日志文件名、状态存活判定；
//! - runner 命令队列（commands/ → results/）。
//!
//! **不包含** DB 查询（Windows GUI 的 `core::boot::sync_manifest_to` 负责）与
//! 凭据模式（S4U / 密码是 Windows 概念，Linux 用 systemd）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// manifest 结构版本；runner 与两端管理器都按它判断兼容性
pub const MANIFEST_VERSION: u32 = 1;
/// runner 心跳超过这个时间就认为它已失联（前端 3 秒轮询，留足余量）
pub const HEARTBEAT_STALE_MS: i64 = 10_000;
/// 启动器状态文件的版本
pub const STATE_VERSION: u32 = 1;

pub use crate::health::{normalize as normalize_check, parse as parse_check, HealthCheck as BootCheck};

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
    /// 稳定运行后崩溃是否自动重启（服务设置里的开关，默认开）。
    #[serde(default = "default_true")]
    pub crash_restart: bool,
    /// 是否在 runner 启动时**自动拉起**。
    ///
    /// Windows 的 manifest 只收 boot_enabled=1 的服务，因此旧 manifest 没有这个字段；
    /// 默认 true 保持原行为。Linux 端把全部服务放进 manifest（需要命令行按需启停），
    /// 用这个标志区分“开机自动启动”与“仅纳入管理”。
    #[serde(default = "default_true")]
    pub boot_enabled: bool,
}

fn default_true() -> bool {
    true
}

// ─── 路径 / 原子写 ──────────────────────────────────────────

/// 服务 id → 安全文件名。只保留字母数字、`-`、`_`。
pub fn safe_file_stem(id: &str) -> String {
    let stem: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if stem.is_empty() { "unknown".to_string() } else { stem }
}

/// 服务日志路径：**永远按 id**，同名服务不会写进同一个文件。
pub fn service_log_path_in(logs_dir: &Path, service_id: &str) -> PathBuf {
    logs_dir.join(format!("{}.log", safe_file_stem(service_id)))
}

/// 原子写：先写同目录临时文件，再 rename 覆盖。
///
/// 临时名必须唯一：manifest / state / 命令结果都可能被并发写；
/// rename 失败时**不能删目标再试**（另一路可能刚把目标换好）。
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| format!("路径没有父目录: {}", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败 {}: {}", parent.display(), e))?;
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("write"),
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&tmp, bytes).map_err(|e| format!("写临时文件失败 {}: {}", tmp.display(), e))?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("替换文件失败 {}: {}", path.display(), e));
    }
    Ok(())
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
    /// starting / ready / failed / exited / stopped / skipped / restarting / pending
    pub phase: String,
    pub pid: Option<u32>,
    pub process_started_at: Option<u64>,
    /// 启动器最近一次**亲自观测**到的存活状态。
    #[serde(default)]
    pub process_alive: Option<bool>,
    pub started_at: Option<String>,
    pub ready_at: Option<String>,
    pub exit_code: Option<i32>,
    pub error: String,
    pub log_path: String,
    pub name: String,
    pub project_id: String,
}

pub fn read_state_at(path: &Path) -> Option<BootRunnerState> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// runner 进程本身是否还活着（**不看心跳**）。
pub fn runner_process_alive(state: &BootRunnerState) -> bool {
    if state.runner_pid == 0 {
        return false;
    }
    crate::process_exec::process_start_time_millis(state.runner_pid)
        .map(|t| t == state.runner_started_at_ms)
        .unwrap_or(false)
}

/// state 里记录的服务进程是否还活着（pid + 启动时刻校验）。
pub fn service_alive(s: &BootServiceState) -> bool {
    let Some(pid) = s.pid else { return false };
    let Some(expected) = s.process_started_at else { return false };
    crate::process_exec::process_start_time_millis(pid)
        .map(|t| t == expected)
        .unwrap_or(false)
}

/// 服务是否存活：runner 心跳新鲜时采信它亲自观测的结论，否则回退本地 pid 校验。
pub fn service_alive_in(runner: &BootRunnerState, s: &BootServiceState) -> bool {
    if runner_heartbeat_fresh(runner) {
        if let Some(alive) = s.process_alive {
            return alive;
        }
    }
    service_alive(s)
}

/// runner 的心跳是否新鲜（只证明“最近 10 秒内它写过 state”，不查 pid）。
pub fn runner_heartbeat_fresh(state: &BootRunnerState) -> bool {
    if state.runner_pid == 0 {
        return false;
    }
    chrono::DateTime::parse_from_rfc3339(&state.heartbeat_at)
        .map(|t| {
            chrono::Utc::now()
                .signed_duration_since(t.with_timezone(&chrono::Utc))
                .num_milliseconds()
                <= HEARTBEAT_STALE_MS
        })
        .unwrap_or(false)
}

/// 状态是否来自一个仍然存活、且心跳新鲜的 runner。
pub fn runner_reachable(state: &BootRunnerState) -> bool {
    if !runner_heartbeat_fresh(state) {
        return false;
    }
    !matches!(
        crate::process_exec::probe_process_start_time(state.runner_pid),
        crate::process_exec::ProcessProbe::Gone
    )
}

// ─── 控制命令（文件队列）───────────────────────────────────

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

fn command_file(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{}.json", id))
}

/// 向 runner 发送一条命令并等待结果（最长 `timeout`）。
///
/// 调用方必须在**阻塞线程**里调用（runner 的轮询是 250ms 级）。
pub fn send_command_to(
    commands_dir: &Path,
    results_dir: &Path,
    command: BootCommand,
    timeout: Duration,
) -> Result<BootCommandResult, String> {
    std::fs::create_dir_all(commands_dir).map_err(|e| format!("创建命令目录失败: {}", e))?;
    std::fs::create_dir_all(results_dir).map_err(|e| format!("创建结果目录失败: {}", e))?;
    let cmd_path = command_file(commands_dir, &command.id);
    let res_path = command_file(results_dir, &command.id);
    let _ = std::fs::remove_file(&res_path);
    let cmd_bytes = serde_json::to_vec(&command).map_err(|e| format!("序列化命令失败: {}", e))?;
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

    /// 并发写同一路径不能互相截断/删目标：manifest 与 state 都会被不同命令并发写。
    #[test]
    fn test_write_atomic_concurrent_writers_never_corrupt() {
        let dir = std::env::temp_dir().join(format!("nexus-boot-atomic-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let path = dir.join("state.json");
        let payloads = [br#"{"a":1}"#.to_vec(), br#"{"b":2}"#.to_vec()];
        let mut handles = Vec::new();
        for i in 0..8 {
            let p = path.clone();
            let data = payloads[i % payloads.len()].clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..50 {
                    write_atomic(&p, &data).expect("原子写");
                }
            }));
        }
        for h in handles {
            h.join().expect("写线程不应 panic");
        }
        let bytes = std::fs::read(&path).expect("读最终文件");
        assert!(
            payloads.iter().any(|p| p.as_slice() == bytes.as_slice()),
            "最终文件必须是某一次完整写入，实际 {:?}",
            String::from_utf8_lossy(&bytes)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 跨登录会话：GUI 查不了 S4U 进程的 pid，但 runner 心跳新鲜时它上报的
    /// `process_alive` 才是权威结论。心跳过期后必须回退到本地 pid 校验。
    #[test]
    fn test_service_alive_prefers_fresh_runner_report() {
        let now = chrono::Utc::now().to_rfc3339();
        let own_pid = std::process::id();
        let own_started = crate::process_exec::process_start_time_millis(own_pid);
        let mut state = BootRunnerState {
            runner_pid: 999_999,
            heartbeat_at: now,
            ..Default::default()
        };
        let mut svc = BootServiceState {
            pid: Some(999_998),
            process_started_at: Some(1),
            process_alive: Some(true),
            ..Default::default()
        };
        state.services.insert("s".into(), svc.clone());

        assert!(runner_heartbeat_fresh(&state));
        assert!(!runner_reachable(&state));
        assert!(service_alive_in(&state, &svc));
        assert!(!service_alive(&svc));

        let live = BootRunnerState {
            runner_pid: own_pid,
            heartbeat_at: state.heartbeat_at.clone(),
            ..Default::default()
        };
        assert!(runner_reachable(&live));

        svc.pid = Some(own_pid);
        svc.process_started_at = own_started;
        svc.process_alive = Some(false);
        assert!(!service_alive_in(&state, &svc));

        state.heartbeat_at = (chrono::Utc::now() - chrono::Duration::seconds(60)).to_rfc3339();
        assert!(!runner_heartbeat_fresh(&state));
        assert!(!runner_reachable(&state));
        assert!(service_alive_in(&state, &svc));
    }

    #[test]
    fn test_service_log_path_cannot_escape_logs_dir() {
        let logs = Path::new("/tmp/nexus-logs");
        let p = service_log_path_in(logs, r"../../evil");
        assert!(p.starts_with(logs));
        assert!(!p.to_string_lossy().contains(".."));
        assert_eq!(p.file_name().unwrap().to_string_lossy(), "______evil.log");
    }

    /// 旧 manifest 没有 boot_enabled 字段：默认 true，Windows 行为不变。
    #[test]
    fn test_boot_service_boot_enabled_defaults_true() {
        let raw = r#"{
            "id":"s1","project_id":"p1","name":"svc","command":"echo x","cwd":"",
            "env":{},"check":{"type":"none"},"required":false,"order":0,"log_path":"/tmp/s1.log"
        }"#;
        let svc: BootService = serde_json::from_str(raw).expect("旧 manifest 应可解析");
        assert!(svc.boot_enabled);
        assert!(svc.crash_restart);
    }
}
