//! 服务开机启动（Windows GUI 侧）：数据模型来自 `boot-core`，这里保留
//! DB → manifest 的生成、Windows 两种凭据模式的路径，以及旧版路径迁移。
//!
//! 设计边界（见 CLAUDE.md 的「服务开机启动」方案）：
//! - **一个 runner 一个运行身份**；服务通过 `boot_enabled` 进入 manifest。
//! - 服务身份永远用 `id`，`name` 只用于展示——同名服务不冲突。
//! - 启动顺序用独立的 `boot_order`，不复用展示排序 `sort_index`。
//! - manifest / 日志 / 状态都放在 Nexus 自己的数据目录下（`<data_dir>/boot`），
//!   runner 拿到的是绝对路径，不依赖登录后才有的用户环境。
//! - manifest 只看 `boot_enabled`；`enabled`（跟随项目启动）是独立范围，不影响开机启动。
//!
//! 共享部分（类型、health、原子写、状态判定、命令队列）在 `boot-core`；
//! Linux 服务端直接使用同一份模型，只把“凭据模式”换成 systemd。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

// 这些是 GUI/MCP 与测试使用的模块表面；`core` 是私有模块，部分符号只在测试或
// 平台分支里出现，统一允许未用告警（不是死代码，删掉会破坏调用方）。
#[allow(unused_imports)]
pub use boot_core::boot::{
    new_command_id, read_state_at, runner_heartbeat_fresh, runner_process_alive, runner_reachable,
    safe_file_stem, send_command_to, service_alive, service_alive_in, service_log_path_in,
    write_atomic, BootCheck, BootCommand, BootCommandResult, BootManifest, BootRunnerState,
    BootService, BootServiceState, HEARTBEAT_STALE_MS, MANIFEST_VERSION, STATE_VERSION,
};
pub use boot_core::health::normalize as normalize_check;
pub(crate) use boot_core::health::parse as parse_check;

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

/// 服务日志路径：**永远按 id**，同名服务不会写进同一个文件。
pub fn service_log_path(mode: BootMode, service_id: &str) -> PathBuf {
    service_log_path_in(&logs_dir(mode), service_id)
}

/// 旧版单任务的日志路径（升级后未迁移时读取）。
pub fn legacy_service_log_path(service_id: &str) -> PathBuf {
    service_log_path_in(&legacy_boot_dir().join("logs"), service_id)
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

// ─── 环境快照 ────────────────────────────────────────────────

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

// ─── DB → Manifest ──────────────────────────────────────────

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
            "SELECT id, project_id, name, command, cwd, env_vars, boot_check, boot_required, boot_order, crash_restart \
             FROM services WHERE boot_enabled=1 AND boot_credential_mode=?1 \
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
                row.get::<_, i32>("crash_restart")? != 0,
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
    for (id, project_id, name, command, cwd, env_vars, boot_check, required, order, crash_restart) in rows {
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
            crash_restart,
            stop_timeout_secs: boot_core::boot::DEFAULT_STOP_TIMEOUT_SECS,
            // Windows 的 manifest 只收 boot_enabled=1 的服务，全部自动启动。
            boot_enabled: true,
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

// ─── 读取 / 命令 ────────────────────────────────────────────

pub fn read_state(mode: BootMode) -> Option<BootRunnerState> {
    read_state_at(&state_path(mode))
}

/// 旧版单任务的状态（迁移时用来停止旧 runner）。
pub fn read_legacy_state() -> Option<BootRunnerState> {
    read_state_at(&legacy_state_path())
}

/// 向指定模式的 runner 发送一条命令并等待结果（最长 `timeout`）。
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

    /// manifest 只看 `boot_enabled=1` 且模式匹配的服务（**不再看 enabled**：
    /// 「跟随项目启动」与「开机启动」是两个独立范围），顺序按 boot_order，
    /// 服务 env 覆盖快照；两个模式的 manifest 互不混入。
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
        insert("s3", 0, 1, 0, "", "", "s4u"); // enabled=0：仍应进入 manifest（两个开关独立）
        insert("s4", 1, 0, 0, "", "", "s4u"); // boot_enabled=0：必须被过滤
        insert("s5", 1, 1, 0, "", "", "password"); // 密码模式：只进 password manifest

        let db = crate::database::Database::from_connection(conn);
        let dir = std::env::temp_dir().join(format!("nexus-boot-manifest-{}", uuid::Uuid::new_v4()));
        let s4u_dir = dir.join("s4u");
        let pwd_dir = dir.join("password");
        let manifest = sync_manifest_to(&db, &s4u_dir, BootMode::S4u).expect("生成 S4U manifest");
        let pwd = sync_manifest_to(&db, &pwd_dir, BootMode::Password).expect("生成密码 manifest");

        assert_eq!(manifest.services.len(), 3);
        assert_eq!(manifest.services[0].id, "s3", "boot_order 小的先出现（enabled=0 不排除）");
        assert_eq!(manifest.services[1].id, "s1");
        assert_eq!(manifest.services[2].id, "s2");
        assert!(manifest.services.iter().all(|s| s.id != "s5"), "密码服务不能进 S4U manifest");
        assert_eq!(manifest.services[1].env.get("A").map(String::as_str), Some("1"));
        assert_eq!(manifest.services[1].env.get("B"), None);
        assert_eq!(
            manifest.services[1].check,
            BootCheck::Tcp { host: "127.0.0.1".into(), port: 3306, timeout_secs: 60 }
        );
        assert!(manifest.services[1].log_path.contains("s4u"));
        assert!(manifest.services[1].log_path.ends_with("s1.log"));

        assert_eq!(pwd.services.len(), 1);
        assert_eq!(pwd.services[0].id, "s5");
        assert!(pwd.services[0].log_path.contains("password"));

        // 写盘的是同一份结构（runner 读到的不是内存里的副本）
        let written: BootManifest = serde_json::from_slice(
            &std::fs::read(s4u_dir.join("manifest.json")).expect("读 manifest"),
        ).expect("解析 manifest");
        assert_eq!(written.services.len(), 3);
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
