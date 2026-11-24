//! TOML 配置：项目 / 服务 / 开机启动项。
//!
//! 配置是 Linux 服务端的唯一真相；manifest 是生成物。
//! CLI 修改后会重写配置（**会丢失手写注释**），直接编辑文件则完全支持。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use boot_core::boot::{safe_file_stem, BootCheck};

pub const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u32,
    /// 全局环境变量：叠加在系统环境快照之上、服务自己的 `env` 之下。
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub projects: Vec<Project>,
    #[serde(default)]
    pub services: Vec<Service>,
}

fn default_version() -> u32 {
    CONFIG_VERSION
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            env: BTreeMap::new(),
            projects: Vec::new(),
            services: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Service {
    pub project: String,
    pub name: String,
    pub command: String,
    /// 为空时使用项目路径。
    #[serde(default)]
    pub workdir: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// 是否在 runner 启动时自动拉起。
    #[serde(default)]
    pub boot: bool,
    #[serde(default)]
    pub order: i32,
    /// 就绪检查：空 / `none` / `wait:5` / `tcp:127.0.0.1:3000` / `log:ready` / JSON。
    #[serde(default)]
    pub ready: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default = "default_true")]
    pub crash_restart: bool,
}

impl Service {
    pub fn id(&self) -> String {
        service_id(&self.project, &self.name)
    }
}

pub fn service_id(project: &str, name: &str) -> String {
    format!("{}.{}", project, name)
}

impl Config {
    /// 读取配置；文件不存在时返回错误（读命令不应该悄悄用空配置）。
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("读取配置失败 {}: {}", path.display(), e))?;
        let cfg: Config = toml::from_str(&text)
            .map_err(|e| format!("解析配置失败 {}: {}", path.display(), e))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// 写命令用：文件不存在 = 空配置。
    pub fn load_or_default(path: &Path) -> Result<Self, String> {
        if path.exists() {
            Self::load(path)
        } else {
            Ok(Self::default())
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        let text = toml::to_string_pretty(self)
            .map_err(|e| format!("序列化配置失败: {}", e))?;
        boot_core::boot::write_atomic(path, text.as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("设置配置权限失败 {}: {}", path.display(), e))?;
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != CONFIG_VERSION {
            return Err(format!(
                "配置版本不支持：{}（当前支持 {}）",
                self.version, CONFIG_VERSION
            ));
        }
        validate_env(&self.env, "全局 env")?;

        let mut project_names = BTreeSet::new();
        for p in &self.projects {
            validate_name(&p.name, "项目名")?;
            if !project_names.insert(p.name.clone()) {
                return Err(format!("项目名重复: {}", p.name));
            }
            if !Path::new(&p.path).is_absolute() {
                return Err(format!("项目「{}」的路径必须是绝对路径: {}", p.name, p.path));
            }
        }

        let mut ids = BTreeSet::new();
        let mut stems = BTreeSet::new();
        for s in &self.services {
            validate_name(&s.project, "服务所属项目名")?;
            validate_name(&s.name, "服务名")?;
            if !project_names.contains(&s.project) {
                return Err(format!(
                    "服务「{}」引用了不存在的项目「{}」",
                    s.name, s.project
                ));
            }
            if s.command.trim().is_empty() {
                return Err(format!("服务「{}.{}」的命令不能为空", s.project, s.name));
            }
            if !s.workdir.trim().is_empty() && !Path::new(&s.workdir).is_absolute() {
                return Err(format!(
                    "服务「{}.{}」的工作目录必须是绝对路径: {}",
                    s.project, s.name, s.workdir
                ));
            }
            validate_env(&s.env, &format!("服务「{}.{}」的 env", s.project, s.name))?;
            parse_ready(&s.ready).map_err(|e| {
                format!("服务「{}.{}」的就绪检查无效: {}", s.project, s.name, e)
            })?;

            let id = s.id();
            if !ids.insert(id.clone()) {
                return Err(format!("服务 id 重复: {}", id));
            }
            let stem = safe_file_stem(&id);
            if !stems.insert(stem.clone()) {
                return Err(format!(
                    "服务「{}」的日志文件名与其它服务冲突（{}）。请调整项目名/服务名",
                    id, stem
                ));
            }
        }
        Ok(())
    }

    pub fn find_project(&self, name: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.name == name)
    }

    pub fn find_service(&self, project: &str, name: &str) -> Option<&Service> {
        self.services
            .iter()
            .find(|s| s.project == project && s.name == name)
    }

    pub fn find_service_mut(&mut self, project: &str, name: &str) -> Option<&mut Service> {
        self.services
            .iter_mut()
            .find(|s| s.project == project && s.name == name)
    }

    /// 服务的工作目录：未配置时回退到项目路径。
    pub fn workdir_of(&self, s: &Service) -> String {
        if s.workdir.trim().is_empty() {
            self.find_project(&s.project)
                .map(|p| p.path.clone())
                .unwrap_or_default()
        } else {
            s.workdir.clone()
        }
    }
}

fn validate_name(name: &str, what: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 {
        return Err(format!("{} 长度必须在 1-64 之间: {:?}", what, name));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "{} 只允许字母、数字、`-`、`_`: {:?}（中文/空格请用项目显示名以外的命名）",
            what, name
        ));
    }
    Ok(())
}

fn validate_env(env: &BTreeMap<String, String>, what: &str) -> Result<(), String> {
    for (k, v) in env {
        if k.is_empty() || k.contains('=') || k.contains('\0') {
            return Err(format!("{} 的变量名非法: {:?}", what, k));
        }
        if v.contains('\0') {
            return Err(format!("{} 的 {} 值里含 NUL", what, k));
        }
    }
    Ok(())
}

/// 把 CLI / TOML 里的就绪检查写法解析成共享类型。
pub fn parse_ready(spec: &str) -> Result<BootCheck, String> {
    let s = spec.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("none") {
        return Ok(BootCheck::None);
    }
    if s.starts_with('{') {
        let normalized = boot_core::health::normalize(s)?;
        return boot_core::health::parse(&normalized);
    }
    let (kind, rest) = s
        .split_once(':')
        .ok_or_else(|| format!("就绪检查格式应为 kind:value（收到 {:?}）", spec))?;
    match kind.to_ascii_lowercase().as_str() {
        "wait" | "delay" => {
            let seconds: u64 = rest
                .trim()
                .parse()
                .map_err(|_| format!("等待秒数不是整数: {:?}", rest))?;
            if seconds == 0 || seconds > 3600 {
                return Err("等待秒数必须在 1-3600 之间".into());
            }
            Ok(BootCheck::Delay { seconds })
        }
        "tcp" => {
            let (host, port) = rest
                .trim()
                .rsplit_once(':')
                .ok_or_else(|| format!("TCP 就绪检查格式应为 tcp:host:port（收到 {:?}）", rest))?;
            let host = host.trim().trim_start_matches('[').trim_end_matches(']');
            if host.is_empty() {
                return Err("TCP host 不能为空".into());
            }
            let port: u16 = port
                .trim()
                .parse()
                .map_err(|_| format!("TCP 端口无效: {:?}", port))?;
            if port == 0 {
                return Err("TCP 端口必须在 1-65535 之间".into());
            }
            Ok(BootCheck::Tcp { host: host.to_string(), port, timeout_secs: 60 })
        }
        "log" => {
            let pattern = rest.trim();
            if pattern.is_empty() {
                return Err("日志关键字不能为空".into());
            }
            if pattern.len() > 512 {
                return Err("日志关键字不能超过 512 字节".into());
            }
            Ok(BootCheck::LogContains { pattern: pattern.to_string(), timeout_secs: 60 })
        }
        other => Err(format!(
            "未知的就绪检查类型 {:?}（支持 none / wait:N / tcp:host:port / log:关键字 / JSON）",
            other
        )),
    }
}

/// 给人看的就绪检查说明。
pub fn describe_ready(spec: &str) -> String {
    match parse_ready(spec) {
        Ok(check) => boot_core::health::describe(&check),
        Err(_) => spec.to_string(),
    }
}

/// 解析 `K=V` 形式的 `--env`。
pub fn parse_env_arg(raw: &str) -> Result<(String, String), String> {
    let (k, v) = raw
        .split_once('=')
        .ok_or_else(|| format!("--env 需要 K=V 形式: {:?}", raw))?;
    if k.trim().is_empty() {
        return Err(format!("--env 的变量名不能为空: {:?}", raw));
    }
    Ok((k.trim().to_string(), v.to_string()))
}

/// 配置文件的默认路径（供提示语使用）。
pub fn default_config_path() -> PathBuf {
    dirs::config_dir()
        .map(|d| d.join("nexus").join("nexus.toml"))
        .unwrap_or_else(|| PathBuf::from("~/.config/nexus/nexus.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_path() -> String {
        if cfg!(windows) {
            "C:/srv/app".to_string()
        } else {
            "/srv/app".to_string()
        }
    }

    fn sample() -> Config {
        let mut cfg = Config::default();
        cfg.projects.push(Project {
            name: "app".into(),
            path: app_path(),
        });
        cfg.services.push(Service {
            project: "app".into(),
            name: "api".into(),
            command: "npm run start".into(),
            workdir: String::new(),
            env: BTreeMap::from([("NODE_ENV".into(), "production".into())]),
            boot: true,
            order: 10,
            ready: "tcp:127.0.0.1:3000".into(),
            required: true,
            crash_restart: true,
        });
        cfg
    }

    #[test]
    fn roundtrip_toml() {
        let cfg = sample();
        let text = toml::to_string_pretty(&cfg).expect("序列化");
        let back: Config = toml::from_str(&text).expect("解析");
        assert_eq!(back.projects[0].name, "app");
        assert_eq!(back.services[0].id(), "app.api");
        back.validate().expect("合法配置");
    }

    #[test]
    fn validate_rejects_duplicates_and_unknown_project() {
        let mut cfg = sample();
        cfg.services.push(cfg.services[0].clone());
        assert!(cfg.validate().unwrap_err().contains("id 重复"));

        let mut cfg = sample();
        cfg.services[0].project = "nope".into();
        assert!(cfg.validate().unwrap_err().contains("不存在的项目"));

        let mut cfg = sample();
        cfg.services[0].command = "  ".into();
        assert!(cfg.validate().unwrap_err().contains("命令不能为空"));
    }

    #[test]
    fn validate_rejects_relative_paths_and_bad_names() {
        let mut cfg = sample();
        cfg.projects[0].path = "relative/path".into();
        assert!(cfg.validate().unwrap_err().contains("绝对路径"));

        let mut cfg = sample();
        cfg.services[0].name = "api 服务".into();
        assert!(cfg.validate().unwrap_err().contains("只允许字母"));
    }

    #[test]
    fn parse_ready_supports_human_specs_and_json() {
        assert_eq!(parse_ready("").unwrap(), BootCheck::None);
        assert_eq!(parse_ready("none").unwrap(), BootCheck::None);
        assert_eq!(parse_ready("wait:5").unwrap(), BootCheck::Delay { seconds: 5 });
        assert_eq!(
            parse_ready("tcp:127.0.0.1:3000").unwrap(),
            BootCheck::Tcp { host: "127.0.0.1".into(), port: 3000, timeout_secs: 60 }
        );
        assert_eq!(
            parse_ready("log:ready for connections").unwrap(),
            BootCheck::LogContains { pattern: "ready for connections".into(), timeout_secs: 60 }
        );
        assert_eq!(
            parse_ready(r#"{"type":"tcp","host":"127.0.0.1","port":3306,"timeoutSecs":30}"#).unwrap(),
            BootCheck::Tcp { host: "127.0.0.1".into(), port: 3306, timeout_secs: 30 }
        );
        assert!(parse_ready("tcp:127.0.0.1:0").is_err());
        assert!(parse_ready("whatever").is_err());
    }

    #[test]
    fn workdir_falls_back_to_project_path() {
        let cfg = sample();
        assert_eq!(cfg.workdir_of(&cfg.services[0]), app_path());
    }
}
