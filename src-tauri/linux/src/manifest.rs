//! 配置 → manifest.json。
//!
//! Linux 的 manifest 包含**全部服务**，`boot_enabled` 区分“开机自动拉起”与
//! “只纳入 runner 管理”。这样 `nexusctl service start` 也能管理非开机服务。

use std::collections::BTreeMap;
use std::path::Path;

use boot_core::boot::{
    service_log_path_in, write_atomic, BootManifest, BootService, MANIFEST_VERSION,
};

use crate::config::{parse_ready, Config};

/// 环境快照白名单：只取“找得到工具/用户目录”的变量。
///
/// **不 dump 整个环境**：可能含 token/密钥，而 manifest 会落盘。
/// 代理变量也刻意不收（`HTTP_PROXY` 常带用户名密码）；需要时写到配置的 `[env]`。
const ENV_WHITELIST: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "TERM",
    "JAVA_HOME",
    "NVM_DIR",
    "NVM_BIN",
    "NODE_PATH",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "GOROOT",
    "GOPATH",
    "M2_HOME",
    "GRADLE_HOME",
    "XDG_DATA_DIRS",
    "XDG_CONFIG_DIRS",
    "LD_LIBRARY_PATH",
];

/// 当前环境快照 + 配置里的全局 env（后者覆盖快照）。
///
/// 系统级以 `--run-as <用户>` 运行时，CLI 本身可能是 root；不能把 root 的
/// `HOME/USER/LOGNAME` 带进服务环境。这里用目标用户覆盖，服务的 `env` 仍可再覆盖。
pub fn env_snapshot(config: &Config, run_as: Option<&str>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for key in ENV_WHITELIST {
        if let Ok(v) = std::env::var(key) {
            if !v.trim().is_empty() {
                out.insert((*key).to_string(), v);
            }
        }
    }
    if let Some(user) = run_as {
        out.insert("USER".into(), user.to_string());
        out.insert("LOGNAME".into(), user.to_string());
        if let Some(home) = crate::systemd::user_home(user) {
            out.insert("HOME".into(), home);
        }
    }
    for (k, v) in &config.env {
        out.insert(k.clone(), v.clone());
    }
    out
}

/// 生成并写盘 manifest；返回生成的结构（runner 读的是同一份文件）。
pub fn generate(
    config: &Config,
    boot_dir: &Path,
    run_as: Option<&str>,
) -> Result<BootManifest, String> {
    config.validate()?;
    let snapshot = env_snapshot(config, run_as);
    let logs_dir = boot_dir.join("logs");
    std::fs::create_dir_all(&logs_dir)
        .map_err(|e| format!("创建日志目录失败 {}: {}", logs_dir.display(), e))?;

    let mut services: Vec<_> = config.services.iter().collect();
    services.sort_by(|a, b| {
        a.order
            .cmp(&b.order)
            .then_with(|| a.project.cmp(&b.project))
            .then_with(|| a.name.cmp(&b.name))
    });

    let mut out = Vec::with_capacity(services.len());
    for s in services {
        let id = s.id();
        let check = parse_ready(&s.ready)?;
        let mut env = snapshot.clone();
        for (k, v) in &s.env {
            env.insert(k.clone(), v.clone());
        }
        out.push(BootService {
            log_path: service_log_path_in(&logs_dir, &id)
                .to_string_lossy()
                .into_owned(),
            id,
            project_id: s.project.clone(),
            name: s.name.clone(),
            command: s.command.clone(),
            cwd: config.workdir_of(s),
            env,
            check,
            required: s.required,
            order: s.order,
            crash_restart: s.crash_restart,
            stop_timeout_secs: config.stop_timeout_of(s),
            boot_enabled: s.boot,
        });
    }

    let manifest = BootManifest {
        version: MANIFEST_VERSION,
        generated_at: chrono::Utc::now().to_rfc3339(),
        env: snapshot,
        services: out,
    };
    let bytes =
        serde_json::to_vec_pretty(&manifest).map_err(|e| format!("序列化 manifest 失败: {}", e))?;
    write_atomic(&boot_dir.join("manifest.json"), &bytes)?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Project, Service};

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nexus-linux-{}-{}-{}",
            tag,
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    fn app_path() -> String {
        if cfg!(windows) {
            "C:/srv/app".to_string()
        } else {
            "/srv/app".to_string()
        }
    }

    fn worker_path() -> String {
        std::path::Path::new(&app_path())
            .join("worker")
            .to_string_lossy()
            .into_owned()
    }

    fn sample_config() -> Config {
        let mut cfg = Config::default();
        cfg.env.insert("GLOBAL".into(), "1".into());
        cfg.projects.push(Project {
            name: "app".into(),
            path: app_path(),
        });
        cfg.services.push(Service {
            project: "app".into(),
            name: "api".into(),
            command: "npm start".into(),
            workdir: String::new(),
            env: BTreeMap::from([("NODE_ENV".into(), "production".into())]),
            boot: true,
            order: 10,
            ready: "tcp:127.0.0.1:3000".into(),
            required: true,
            crash_restart: true,
            stop_timeout_secs: None,
        });
        cfg.services.push(Service {
            project: "app".into(),
            name: "worker".into(),
            command: "node worker.js".into(),
            workdir: worker_path(),
            env: BTreeMap::new(),
            boot: false,
            order: 20,
            ready: String::new(),
            required: false,
            crash_restart: false,
            stop_timeout_secs: None,
        });
        cfg
    }

    #[test]
    fn generates_all_services_with_boot_flag_and_env_merge() {
        let dir = temp_dir("manifest");
        let cfg = sample_config();
        let m = generate(&cfg, &dir, None).expect("生成 manifest");
        assert_eq!(m.services.len(), 2, "非开机服务也要在 manifest 里");
        assert_eq!(m.services[0].id, "app.api");
        assert!(m.services[0].boot_enabled);
        assert!(!m.services[1].boot_enabled);
        assert_eq!(m.services[0].cwd, app_path(), "未配置 workdir 时回退项目路径");
        assert_eq!(m.services[1].cwd, worker_path());
        assert_eq!(m.services[0].env.get("GLOBAL").map(String::as_str), Some("1"));
        assert_eq!(m.services[0].env.get("NODE_ENV").map(String::as_str), Some("production"));
        // 服务自己的 env 不能泄漏到全局快照
        assert_eq!(m.env.get("NODE_ENV"), None);
        assert!(m.services[0].log_path.starts_with(&dir.join("logs").to_string_lossy().to_string()));
        assert!(m.services[0].log_path.ends_with("app_api.log"));

        // 写盘的是 runner 读的那份
        let written: BootManifest = serde_json::from_slice(
            &std::fs::read(dir.join("manifest.json")).expect("读 manifest"),
        )
        .expect("解析 manifest");
        assert_eq!(written.services.len(), 2);
        assert_eq!(written.version, MANIFEST_VERSION);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_as_overrides_identity_env_but_config_wins() {
        let mut cfg = Config::default();
        cfg.env.insert("HOME".into(), "/custom/home".into());
        let env = env_snapshot(&cfg, Some("deploy"));
        assert_eq!(env.get("USER").map(String::as_str), Some("deploy"));
        assert_eq!(env.get("LOGNAME").map(String::as_str), Some("deploy"));
        assert_eq!(env.get("HOME").map(String::as_str), Some("/custom/home"));
    }

    #[test]
    fn manifest_uses_effective_stop_timeout() {
        let dir = temp_dir("stop-timeout");
        let mut cfg = sample_config();
        cfg.settings.stop_timeout_secs = 30;
        cfg.services[0].stop_timeout_secs = Some(0);
        let m = generate(&cfg, &dir, None).expect("生成");
        assert_eq!(m.services[0].stop_timeout_secs, 0);
        assert_eq!(m.services[1].stop_timeout_secs, 30);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_workdir_and_sorting() {
        let dir = temp_dir("sort");
        let mut cfg = sample_config();
        cfg.services[0].order = 99;
        let m = generate(&cfg, &dir, None).expect("生成");
        assert_eq!(m.services[0].id, "app.worker", "order 小的先出现");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
