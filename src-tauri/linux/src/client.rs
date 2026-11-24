//! 与常驻 runner 的文件命令协议（与 Windows GUI 同一套 commands/ → results/）。

use std::time::Duration;

use boot_core::boot::{
    new_command_id, read_state_at, runner_reachable, send_command_to, BootCommand,
    BootCommandResult, BootRunnerState,
};

use crate::layout::Layout;

pub fn state(layout: &Layout) -> Option<BootRunnerState> {
    read_state_at(&layout.state_path())
}

/// 要求 runner 可达；不可达时给出 systemd 操作提示。
pub fn require_runner(layout: &Layout) -> Result<BootRunnerState, String> {
    let Some(st) = state(layout) else {
        let how = if layout.system {
            format!(
                "sudo nexusctl boot install（要降权可加 --run-as <用户>），或 systemctl start {}",
                layout.unit_name()
            )
        } else {
            format!(
                "nexusctl --user boot install，或 systemctl --user start {}",
                layout.unit_name()
            )
        };
        return Err(format!(
            "启动器未运行（没有 {}）。先执行：{}",
            layout.state_path().display(),
            how
        ));
    };
    if !runner_reachable(&st) {
        let how = if layout.system {
            format!("systemctl status {}", layout.unit_name())
        } else {
            format!("systemctl --user status {}", layout.unit_name())
        };
        return Err(format!(
            "启动器心跳过期或进程已退出（{}）。检查：{}",
            layout.state_path().display(),
            how
        ));
    }
    Ok(st)
}

pub fn send(
    layout: &Layout,
    op: &str,
    service_id: Option<&str>,
    timeout: Duration,
) -> Result<BootCommandResult, String> {
    require_runner(layout)?;
    let command = BootCommand {
        id: new_command_id(),
        op: op.to_string(),
        service_id: service_id.map(|s| s.to_string()),
    };
    send_command_to(
        &layout.commands_dir(),
        &layout.results_dir(),
        command,
        timeout,
    )
}

/// 配置变更后通知 runner 重读 manifest；runner 没运行不算错误。
pub fn reload_if_running(layout: &Layout) -> Result<bool, String> {
    let Some(st) = state(layout) else { return Ok(false) };
    if !runner_reachable(&st) {
        return Ok(false);
    }
    let result = send(layout, "reload", None, Duration::from_secs(10))?;
    if result.ok {
        Ok(true)
    } else {
        Err(result.error)
    }
}
