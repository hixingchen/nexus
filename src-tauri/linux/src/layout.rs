//! 路径布局：配置文件、数据目录、manifest/state/logs/commands、systemd unit。

use std::path::{Path, PathBuf};

/// 一次 `nexusctl` 运行使用的路径集合。
#[derive(Debug, Clone)]
pub struct Layout {
    pub system: bool,
    pub config_path: PathBuf,
    pub data_dir: PathBuf,
    pub boot_dir: PathBuf,
    pub unit_path: PathBuf,
    /// system 模式下 systemd unit 的 `User=`（服务进程以谁的身份运行）。
    pub run_as: Option<String>,
}

impl Layout {
    /// `system=false`：当前用户；`system=true`：/etc/nexus + /var/lib/nexus。
    ///
    /// `config_override` / `data_override` 来自 `--config` / `--data-dir`
    /// （也由 clap 的 `NEXUS_CONFIG` / `NEXUS_DATA_DIR` 环境变量映射）。
    pub fn resolve(
        system: bool,
        config_override: Option<PathBuf>,
        data_override: Option<PathBuf>,
        run_as: Option<String>,
    ) -> Result<Self, String> {
        let (config_path, data_dir, unit_path) = if system {
            (
                config_override.unwrap_or_else(|| PathBuf::from("/etc/nexus/nexus.toml")),
                data_override.unwrap_or_else(|| PathBuf::from("/var/lib/nexus")),
                PathBuf::from("/etc/systemd/system/nexus-boot-runner.service"),
            )
        } else {
            let home = dirs::home_dir()
                .ok_or_else(|| "无法确定用户主目录（HOME）".to_string())?;
            let config_dir = dirs::config_dir().unwrap_or_else(|| home.join(".config"));
            (
                config_override.unwrap_or_else(|| config_dir.join("nexus").join("nexus.toml")),
                data_override.unwrap_or_else(|| home.join(".nexus")),
                config_dir
                    .join("systemd")
                    .join("user")
                    .join("nexus-boot-runner.service"),
            )
        };

        // `run_as` 只有安装/状态展示才需要；`project list` 之类的命令
        // 不应该因为没给 --run-as 就失败。真需要的地方（install）会给出明确错误。
        //
        // 已经有 unit 时从里面读 `User=`：升级/查状态不用每次重复 --run-as。
        let run_as = if system {
            run_as
                .or_else(|| {
                    std::env::var("SUDO_USER")
                        .ok()
                        .filter(|u| !u.trim().is_empty() && u != "root")
                })
                .or_else(|| unit_user(&unit_path))
        } else {
            None
        };

        let boot_dir = data_dir.join("boot").join("systemd");
        Ok(Self { system, config_path, data_dir, boot_dir, unit_path, run_as })
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.boot_dir.join("manifest.json")
    }

    pub fn state_path(&self) -> PathBuf {
        self.boot_dir.join("state.json")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.boot_dir.join("logs")
    }

    pub fn commands_dir(&self) -> PathBuf {
        self.boot_dir.join("commands")
    }

    pub fn results_dir(&self) -> PathBuf {
        self.boot_dir.join("results")
    }

    pub fn runner_log(&self) -> PathBuf {
        self.boot_dir.join("runner.log")
    }

    /// `nexus-boot-runner` 默认与 `nexusctl` 同目录（install.sh 就是这么放的）。
    pub fn runner_path(&self, override_path: Option<PathBuf>) -> Result<PathBuf, String> {
        if let Some(p) = override_path {
            if !p.is_file() {
                return Err(format!("--runner 指向的文件不存在: {}", p.display()));
            }
            return Ok(p);
        }
        let exe = std::env::current_exe()
            .map_err(|e| format!("获取当前程序路径失败: {}", e))?;
        let dir = exe
            .parent()
            .ok_or_else(|| format!("当前程序没有父目录: {}", exe.display()))?;
        let candidate = dir.join("nexus-boot-runner");
        if !candidate.is_file() {
            return Err(format!(
                "找不到 nexus-boot-runner（期望与 nexusctl 同目录：{}）。\
                 请用 --runner 指定，或重新运行 install.sh",
                candidate.display()
            ));
        }
        Ok(candidate)
    }

    pub fn unit_name(&self) -> &'static str {
        "nexus-boot-runner.service"
    }

    pub fn current_user(&self) -> Option<String> {
        std::env::var("USER")
            .ok()
            .or_else(|| std::env::var("LOGNAME").ok())
            .filter(|u| !u.trim().is_empty())
    }

    /// 确保目录存在；system 模式下由调用方负责 chown。
    ///
    /// 数据目录里有服务命令、环境变量与日志，统一收紧到 0700：
    /// 只有 owner（系统级是 root/`--run-as` 用户）能进，组和其他用户都不可读。
    /// system 模式的 chown 在 systemd::install 里做。
    pub fn ensure_dirs(&self) -> Result<(), String> {
        let dirs = [
            &self.data_dir,
            &self.boot_dir,
            &self.logs_dir(),
            &self.commands_dir(),
            &self.results_dir(),
        ];
        for dir in dirs {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("创建目录失败 {}: {}", dir.display(), e))?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for dir in dirs {
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                    .map_err(|e| format!("设置目录权限失败 {}: {}", dir.display(), e))?;
            }
        }
        Ok(())
    }
}

/// 从已安装的 unit 里读 `User=`（升级时免去重复 --run-as）。
fn unit_user(unit_path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(unit_path).ok()?;
    text.lines()
        .find_map(|line| line.trim().strip_prefix("User="))
        .map(str::trim)
        .filter(|user| !user.is_empty())
        .map(str::to_string)
}

/// 路径是否位于 `base` 之下（用于日志/状态的测试与防御）。
pub fn is_under(path: &Path, base: &Path) -> bool {
    path.starts_with(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_layout_does_not_require_run_as_for_read_commands() {
        // `project list --system` / `boot uninstall --system` 不需要 --run-as；
        // 只有真正安装 unit 时才需要（在 systemd::install 里报错）。
        let l = Layout::resolve(
            true,
            Some(PathBuf::from("/tmp/nexus.toml")),
            Some(PathBuf::from("/tmp/nexus-data")),
            None,
        )
        .expect("system 布局不应因为缺 run_as 失败");
        assert!(l.system);
        assert_eq!(l.manifest_path(), PathBuf::from("/tmp/nexus-data/boot/systemd/manifest.json"));
    }

    #[test]
    fn user_layout_uses_xdg_paths() {
        let l = Layout::resolve(false, None, None, None).expect("用户布局");
        assert!(!l.system);
        assert!(l.config_path.ends_with("nexus.toml"));
        assert!(l.data_dir.ends_with(".nexus"));
        assert!(l.unit_path.ends_with("nexus-boot-runner.service"));
    }

    #[test]
    fn unit_user_is_read_from_existing_unit() {
        let dir = std::env::temp_dir().join(format!("nexus-layout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let unit = dir.join("nexus-boot-runner.service");
        std::fs::write(
            &unit,
            "[Service]\nUser=deploy\nExecStart=/usr/local/bin/nexus-boot-runner\n",
        )
        .expect("写 unit");
        assert_eq!(unit_user(&unit).as_deref(), Some("deploy"));
        assert_eq!(unit_user(&dir.join("missing.service")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
