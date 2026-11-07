//! AI（MCP）高危操作的用户确认闸门。
//!
//! 为什么必须有：`confirm: true` 是给模型写的意图声明——模型照写不误，它不是人点的。
//! 而 MCP 桥里有一批"能执行任意代码 / 能改系统级持久化"的命令（新建服务、改服务命令、
//! 保存外部打开工具、安装开机启动器、运行 nvm 安装器……）。项目内容、日志、网页里
//! 任何一段提示注入都可能让模型替用户按下这些按钮。这里把它们变成**应用内弹窗**：
//! 后端挂起工具调用，前端显示「允许一次 / 拒绝」，用户点允许才继续。
//!
//! 同一时刻只允许一笔待确认：确认弹窗是模态的，叠第二笔只会让"允许的是哪一个"变成
//! 猜测。并发的第二笔直接失败（AI 会拿到明确原因，可以重试）。
//!
//! 超时（120 秒）按"未允许"失败——绝不默认放行。前端同时会在超时后关闭弹窗并提示。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::oneshot;

use crate::AppState;

/// 事件名（前端 `McpConfirmDialog` 订阅）
const CONFIRM_EVENT: &str = "mcp-confirm-request";
/// 等待用户点按钮的上限
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(120);

/// `mcp-confirm-request` 事件载荷（前端 `McpConfirmDialog` 按这些字段渲染）。
///
/// `pub(crate)` + 公开字段：IPC 契约测试要构造它并断言字段名（snake_case）。
#[derive(Clone, Serialize)]
pub(crate) struct ConfirmRequest {
    pub(crate) id: u64,
    pub(crate) title: String,
    pub(crate) detail: String,
    pub(crate) timeout_secs: u64,
}

struct Pending {
    id: u64,
    tx: oneshot::Sender<bool>,
}

/// 待确认槽位（None = 没有弹窗在等）。`Mutex::new(None)` 是 const，可直接作 static。
static PENDING: Mutex<Option<Pending>> = Mutex::new(None);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// 取消保护：`ask` 的 future 被丢弃时（AI 会话被停止/替换、工具调用被取消），
/// 也必须释放槽位——否则进程余下的生命里所有确认都会撞上"已有另一个操作"。
/// 正常路径上槽位已被 `mcp_confirm_response` 取走，`clear` 是幂等的 no-op。
struct PendingGuard(u64);

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if clear(self.0) {
            log::warn!("[mcp] 确认请求已取消（id={}），未执行", self.0);
        }
    }
}

/// 布局键的值 → 是否必须弹窗。**默认（缺省/任何异常值）一律弹窗**，只有显式 `"0"` 关闭。
fn confirm_required_from(raw: Option<&str>) -> bool {
    !matches!(raw, Some("0"))
}

/// 读用户设置：读库失败按"要确认"处理（fail-closed）。
///
/// 这个键**只能由用户在「设置 > AI 运行时」里改**：MCP 的 `save_layout` 桥拒绝写它
/// （见 `mcp_commands`），否则 AI 可以关掉自己的闸门再执行任意命令。
fn confirm_required(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    let raw = crate::commands::layout::read_layout_value(
        &state.db,
        crate::commands::layout::KEY_MCP_CONFIRM,
    )
    .ok()
    .flatten();
    confirm_required_from(raw.as_deref())
}

/// 请求用户确认。
///
/// `Ok(())` = 用户点了「允许一次」（或用户已在界面上关闭确认）；`Err(原因)` = 拒绝 /
/// 超时 / 无法弹窗——调用方必须把 Err 原样返回给 AI，**绝不能当成成功继续执行**。
pub async fn ask(app: &AppHandle, title: &str, detail: &str) -> Result<(), String> {
    if !confirm_required(app) {
        // 审计留痕：关闭确认后这些操作是**静默**执行的，日志是唯一的事后证据
        log::warn!("[mcp] 用户已关闭 AI 操作确认，直接执行: {}", title);
        return Ok(());
    }
    let (tx, rx) = oneshot::channel();
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    {
        let mut slot = PENDING.lock().map_err(|e| format!("确认锁中毒: {}", e))?;
        if slot.is_some() {
            return Err("已有另一个操作正在等待用户确认，本次操作未执行".into());
        }
        *slot = Some(Pending { id, tx });
    }
    let _guard = PendingGuard(id);

    let request = ConfirmRequest {
        id,
        title: title.to_string(),
        detail: detail.to_string(),
        timeout_secs: CONFIRM_TIMEOUT.as_secs(),
    };
    // 留痕：安全闸门的"弹了没有、用户怎么答的"必须能事后核对（不只是 AI 的返回值）
    log::info!("[mcp] 等待用户确认（id={}）: {}", id, title);
    if let Err(e) = app.emit(CONFIRM_EVENT, request) {
        // 显式清掉，避免 PendingGuard 再记一条"已取消"（这里不是取消，是没弹出去）
        clear(id);
        return Err(format!("无法向界面发送确认请求: {}", e));
    }

    match tokio::time::timeout(CONFIRM_TIMEOUT, rx).await {
        Ok(Ok(true)) => {
            log::info!("[mcp] 用户允许操作（id={}）: {}", id, title);
            Ok(())
        }
        // 拒绝，或通道被丢弃（界面/进程正在退出）
        Ok(Ok(false)) | Ok(Err(_)) => {
            log::warn!("[mcp] 用户拒绝操作（id={}）: {}", id, title);
            Err("用户拒绝了该操作，未执行".into())
        }
        Err(_) => {
            // 先清槽位：迟到的「允许」不能落到下一笔请求上
            clear(id);
            log::warn!("[mcp] 等待用户确认超时（id={}）: {}", id, title);
            Err(format!(
                "等待用户确认超时（{} 秒），操作未执行",
                CONFIRM_TIMEOUT.as_secs()
            ))
        }
    }
}

/// 只清掉"仍是我这笔"的槽位（别把超时后新进来的请求清掉）；返回是否真的清了。
fn clear(id: u64) -> bool {
    if let Ok(mut slot) = PENDING.lock() {
        if slot.as_ref().map(|p| p.id) == Some(id) {
            *slot = None;
            return true;
        }
    }
    false
}

/// 前端提交确认结果（`McpConfirmDialog` 调用）。
///
/// 只接受与当前待确认 id 匹配的答复：超时/串台时宁可不执行，也不能把上一笔的
/// 「允许」误交给下一笔操作。
#[tauri::command]
pub fn mcp_confirm_response(id: u64, approved: bool) -> Result<(), String> {
    let mut slot = PENDING.lock().map_err(|e| format!("确认锁中毒: {}", e))?;
    let Some(pending) = slot.take() else {
        // 已超时/已由别的路径收尾：这一步本身就是 no-op（操作一定没有执行），
        // 按 Ok 返回，避免用户在超时瞬间点按钮时弹一个无意义的错误
        log::info!("[mcp] 收到确认答复但已无待确认操作（id={} approved={}）", id, approved);
        return Ok(());
    };
    let expected_id = pending.id;
    if expected_id != id {
        *slot = Some(pending);
        log::warn!(
            "[mcp] 确认编号不匹配（期望 {}，收到 {}），已忽略",
            expected_id,
            id
        );
        return Err("确认编号不匹配（可能已超时）".into());
    }
    log::info!("[mcp] 收到用户确认答复（id={} approved={}）", id, approved);
    let _ = pending.tx.send(approved);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认必须确认：只有显式 "0" 才关闭；缺省/空串/异常值一律按开启（fail-closed）
    #[test]
    fn confirm_is_on_by_default_and_only_explicit_zero_turns_it_off() {
        assert!(confirm_required_from(None));
        assert!(confirm_required_from(Some("1")));
        assert!(confirm_required_from(Some("")));
        assert!(confirm_required_from(Some("false")));
        assert!(!confirm_required_from(Some("0")));
    }
}
