//! systemd unit 生成与安装/卸载/状态。
//!
//! 一个 unit 管 runner；服务之间的启动顺序、就绪阻断、逐服务重启由 runner 负责。
//! `KillMode=control-group` 让停止 unit 时连 setsid 逃逸的子孙一起收掉。

use std::path::Path;
#[cfg(unix)]
use std::process::Command;

use crate::layout::Layout;

#[derive(Debug, Clone, Default)]
pub struct Status {
    /// None = 查不了（systemctl 不存在 / 无 user bus）
    pub active: Option<bool>,
    /// user 模式：linger 是否开启（None = 查不了）
    pub linger: Option<bool>,
    /// 实际使用的用户名（system 模式是 run_as）
    pub user: Option<String>,
}

/// 生成 unit 文本。`runner` 是 nexus-boot-runner 的绝对路径。
pub fn render_unit(layout: &Layout, runner: &Path) -> Result<String, String> {
    let runner = runner
        .to_str()
        .ok_or_else(|| format!("runner 路径不是 UTF-8: {}", runner.display()))?;
    let manifest = layout
        .manifest_path()
        .to_str()
        .ok_or_else(|| format!("manifest 路径不是 UTF-8: {}", layout.manifest_path().display()))?
        .to_string();
    let mode = if layout.system { "system" } else { "user" };

    let (after, wanted, user_line) = if layout.system {
        let user = layout
            .run_as
            .as_deref()
            .ok_or_else(|| "system 模式缺少 run_as".to_string())?;
        (
            "After=network-online.target\nWants=network-online.target",
            "WantedBy=multi-user.target",
            format!("User={}\n", user),
        )
    } else {
        (
            "After=network.target",
            "WantedBy=default.target",
            String::new(),
        )
    };

    Ok(format!(
        "# 由 nexusctl 生成；修改后重新执行 `nexusctl boot install` 会覆盖。\n\
         [Unit]\n\
         Description=Nexus boot service runner\n\
         {after}\n\
         \n\
         [Service]\n\
         Type=simple\n\
         {user_line}\
         ExecStart={runner} --manifest {manifest} --mode {mode}\n\
         Restart=on-failure\n\
         RestartSec=3\n\
         KillMode=control-group\n\
         StandardOutput=journal\n\
         StandardError=journal\n\
         \n\
         [Install]\n\
         {wanted}\n",
        runner = unit_quote(runner)?,
        manifest = unit_quote(&manifest)?,
    ))
}

/// systemd 的 ExecStart 引号规则：双引号内转义 `\` `"`，并处理 `%`/`$` 展开。
pub fn unit_quote(s: &str) -> Result<String, String> {
    if s.contains('\n') || s.contains('\r') || s.contains('\0') {
        return Err(format!("unit 路径含换行/NUL，拒绝写入: {:?}", s));
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            '$' => out.push_str("$$"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    Ok(out)
}

#[derive(Debug, Clone, Default)]
pub struct InstallReport {
    pub linger: Option<bool>,
    pub user: Option<String>,
    pub warnings: Vec<String>,
    /// 安装前 unit 已在运行，因此执行了 restart 以加载新二进制/新 unit。
    pub restarted: bool,
}

#[cfg(unix)]
pub fn install(layout: &Layout, runner: &Path) -> Result<InstallReport, String> {
    if layout.system {
        if unsafe { libc::geteuid() } != 0 {
            return Err("system 模式需要 root（请用 sudo nexusctl --system boot install ...）".into());
        }
    } else if std::env::var("SUDO_USER").is_ok() {
        return Err(
            "user 模式不要用 sudo：systemctl --user 会指向 root 的用户管理器。\
             服务器级安装请改用 `nexusctl --system --run-as <用户> boot install`"
                .into(),
        );
    }
    if layout.system && layout.run_as.is_none() {
        return Err(
            "system 模式需要 --run-as <用户>（或用 sudo 运行以自动采用 SUDO_USER）".into(),
        );
    }

    let unit = render_unit(layout, runner)?;
    if let Some(parent) = layout.unit_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建 unit 目录失败 {}: {}", parent.display(), e))?;
    }
    boot_core::boot::write_atomic(&layout.unit_path, unit.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&layout.unit_path, std::fs::Permissions::from_mode(0o644))
            .map_err(|e| format!("设置 unit 权限失败: {}", e))?;
    }

    if layout.system {
        let user = layout
            .run_as
            .as_deref()
            .ok_or_else(|| "system 模式缺少 run_as".to_string())?;
        chown_tree(&layout.data_dir, user)?;
    }

    // 升级场景：unit 已在跑时 `enable --now` 不会重载新二进制，必须 restart。
    // 首次安装时为 false，只 start 一次。
    let was_active = systemctl_active(layout).unwrap_or(false);
    run_systemctl(layout, &["daemon-reload"])?;
    run_systemctl(layout, &["enable", "--now", layout.unit_name()])?;
    if was_active {
        run_systemctl(layout, &["restart", layout.unit_name()])?;
    }

    let user = if layout.system {
        layout.run_as.clone()
    } else {
        layout.current_user()
    };
    let linger = if layout.system {
        None
    } else {
        user.as_deref().and_then(linger_enabled)
    };
    let mut warnings = Vec::new();
    if matches!(linger, Some(false)) {
        warnings.push(format!(
            "linger 未开启：无人登录时不会启动。执行 `sudo loginctl enable-linger {}`",
            user.as_deref().unwrap_or("<user>")
        ));
    }
    Ok(InstallReport { linger, user, warnings, restarted: was_active })
}

#[cfg(not(unix))]
pub fn install(_layout: &Layout, _runner: &Path) -> Result<InstallReport, String> {
    Err("systemd 安装仅 Linux 支持".into())
}

#[cfg(unix)]
pub fn uninstall(layout: &Layout) -> Result<Vec<String>, String> {
    if layout.system && unsafe { libc::geteuid() } != 0 {
        return Err("system 模式需要 root".into());
    }
    let mut warnings = Vec::new();
    // 先停再 disable；unit 没加载时 systemctl 会报错，不让它阻断清理
    if let Err(e) = run_systemctl(layout, &["disable", "--now", layout.unit_name()]) {
        warnings.push(format!("systemctl disable 失败（继续清理）: {}", e));
    }
    if layout.unit_path.exists() {
        std::fs::remove_file(&layout.unit_path)
            .map_err(|e| format!("删除 unit 失败 {}: {}", layout.unit_path.display(), e))?;
    }
    if let Err(e) = run_systemctl(layout, &["daemon-reload"]) {
        warnings.push(format!("daemon-reload 失败: {}", e));
    }
    Ok(warnings)
}

#[cfg(not(unix))]
pub fn uninstall(_layout: &Layout) -> Result<Vec<String>, String> {
    Err("systemd 卸载仅 Linux 支持".into())
}

#[cfg(unix)]
pub fn status(layout: &Layout) -> Status {
    let active = systemctl_active(layout);
    let user = if layout.system {
        layout.run_as.clone()
    } else {
        layout.current_user()
    };
    let linger = if layout.system {
        None
    } else {
        user.as_deref().and_then(linger_enabled)
    };
    Status { active, linger, user }
}

#[cfg(not(unix))]
pub fn status(_layout: &Layout) -> Status {
    Status::default()
}

/// 解析 `systemctl is-active` 的 stdout。
///
/// 对 inactive/failed，systemctl 返回**非零退出码**（3）但 stdout 仍是可读的状态；
/// 不能只看退出码，否则"未运行"会被误判成"查不了"。
#[cfg(unix)]
fn is_active_state(stdout: &str) -> Option<bool> {
    let state = stdout.trim();
    if state.is_empty() {
        None
    } else {
        Some(matches!(state, "active" | "reloading"))
    }
}

#[cfg(unix)]
fn systemctl_active(layout: &Layout) -> Option<bool> {
    let out = systemctl_command(layout)
        .args(["is-active", layout.unit_name()])
        .output()
        .ok()?;
    is_active_state(&String::from_utf8_lossy(&out.stdout))
}

/// `loginctl show-user <user> -p Linger` → `Linger=yes|no`。
#[cfg(unix)]
pub fn linger_enabled(user: &str) -> Option<bool> {
    let out = Command::new("loginctl")
        .args(["show-user", user, "-p", "Linger"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let value = text
        .lines()
        .find_map(|l| l.strip_prefix("Linger="))?
        .trim();
    Some(value.eq_ignore_ascii_case("yes"))
}

#[cfg(not(unix))]
pub fn linger_enabled(_user: &str) -> Option<bool> {
    None
}

#[cfg(unix)]
fn run_systemctl(layout: &Layout, args: &[&str]) -> Result<String, String> {
    let out = systemctl_command(layout)
        .args(args)
        .output()
        .map_err(|e| format!("执行 systemctl 失败: {}", e))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        return Err(format!(
            "systemctl {} 失败（退出码 {:?}）{}",
            args.join(" "),
            out.status.code(),
            if stderr.is_empty() { stdout } else { stderr }
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(unix)]
fn systemctl_command(layout: &Layout) -> Command {
    let mut cmd = Command::new("systemctl");
    if !layout.system {
        cmd.arg("--user");
    }
    cmd
}

/// 递归 chown（system 模式下 root 写出的 manifest/logs 必须归服务用户）。
#[cfg(unix)]
fn chown_tree(path: &Path, user: &str) -> Result<(), String> {
    let (uid, gid) = user_ids(user)?;
    fn walk(path: &Path, uid: u32, gid: u32) -> Result<(), String> {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| format!("路径含 NUL: {}", path.display()))?;
        if unsafe { libc::chown(c.as_ptr(), uid, gid) } != 0 {
            return Err(format!(
                "chown {} 失败: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        if path.is_dir() {
            for entry in std::fs::read_dir(path)
                .map_err(|e| format!("读取目录失败 {}: {}", path.display(), e))?
            {
                let entry = entry.map_err(|e| format!("读取目录项失败: {}", e))?;
                walk(&entry.path(), uid, gid)?;
            }
        }
        Ok(())
    }
    if !path.exists() {
        return Ok(());
    }
    walk(path, uid, gid)
}

#[cfg(unix)]
fn user_ids(name: &str) -> Result<(u32, u32), String> {
    use std::ffi::CString;
    let c = CString::new(name).map_err(|_| format!("用户名含 NUL: {}", name))?;
    let pw = unsafe { libc::getpwnam(c.as_ptr()) };
    if pw.is_null() {
        return Err(format!("用户不存在: {}", name));
    }
    unsafe { Ok(((*pw).pw_uid, (*pw).pw_gid)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Layout;

    fn layout(system: bool) -> Layout {
        Layout {
            system,
            config_path: "/tmp/nexus.toml".into(),
            data_dir: "/tmp/nexus".into(),
            boot_dir: "/tmp/nexus/boot/systemd".into(),
            unit_path: "/tmp/nexus-boot-runner.service".into(),
            run_as: system.then(|| "deploy".to_string()),
        }
    }

    #[test]
    fn user_unit_has_expected_exec_start() {
        let base = std::env::temp_dir().join("nexus unit runner");
        let runner = base.join("nexus-boot-runner");
        let mut l = layout(false);
        l.data_dir = base.join("data");
        l.boot_dir = l.data_dir.join("boot").join("systemd");
        let unit = render_unit(&l, &runner).unwrap();
        assert!(unit.contains("WantedBy=default.target"));
        assert!(!unit.contains("User="));
        assert!(unit.contains("KillMode=control-group"));
        let runner_q = unit_quote(runner.to_str().unwrap()).unwrap();
        let manifest_q = unit_quote(l.manifest_path().to_str().unwrap()).unwrap();
        assert!(
            unit.contains(&format!(
                "ExecStart={} --manifest {} --mode user",
                runner_q, manifest_q
            )),
            "unit 内容不符合预期:
{}",
            unit
        );
    }

    #[test]
    fn system_unit_sets_user_and_multi_user_target() {
        let unit = render_unit(&layout(true), Path::new("/usr/local/bin/nexus-boot-runner")).unwrap();
        assert!(unit.contains("User=deploy"));
        assert!(unit.contains("WantedBy=multi-user.target"));
        assert!(unit.contains("--mode system"));
    }

    #[test]
    fn unit_quote_escapes_specifiers() {
        assert_eq!(unit_quote("/tmp/a b").unwrap(), "\"/tmp/a b\"");
        assert_eq!(unit_quote("/tmp/100%/x$y").unwrap(), "\"/tmp/100%%/x$$y\"");
        assert!(unit_quote("/tmp/a\nb").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn is_active_state_understands_systemd_output() {
        assert_eq!(is_active_state("active\n"), Some(true));
        assert_eq!(is_active_state("reloading\n"), Some(true));
        assert_eq!(is_active_state("inactive\n"), Some(false));
        assert_eq!(is_active_state("failed\n"), Some(false));
        assert_eq!(is_active_state(""), None);
    }
}
