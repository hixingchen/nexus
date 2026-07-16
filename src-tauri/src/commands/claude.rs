use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::AppState;

/// Claude Code 会话消息类型
#[derive(Clone, Serialize)]
#[serde(tag = "kind")]
pub enum ClaudeMessage {
    /// 文本内容（流式追加）
    #[serde(rename = "text")]
    Text { delta: String },
    /// 思考过程
    #[serde(rename = "thinking")]
    Thinking { delta: String },
    /// 工具调用开始
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// 工具结果
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
    /// 会话初始化信息
    #[serde(rename = "init")]
    Init {
        session_id: String,
        model: String,
        tools: Vec<String>,
    },
    /// 最终结果
    #[serde(rename = "result")]
    Result {
        text: String,
        cost_usd: f64,
        duration_ms: u64,
        session_id: String,
    },
    /// 错误
    #[serde(rename = "error")]
    Error { message: String },
}

/// 发送消息的参数
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeStartParams {
    /// 用户输入的 prompt
    pub prompt: String,
    /// 工作目录
    pub working_dir: Option<String>,
    /// 模型选择（opus/sonnet/haiku/fable 等）
    pub model: Option<String>,
    /// 权限模式（auto/manual/acceptEdits/bypassPermissions）
    pub permission_mode: Option<String>,
    /// 可用工具白名单
    pub allowed_tools: Option<Vec<String>>,
    /// 系统提示词
    pub system_prompt: Option<String>,
    /// 已有的会话 ID（用于续接对话，通过 --resume 实现上下文保持）
    pub session_id: Option<String>,
}

/// 找到 claude 可执行文件路径
fn find_claude_bin() -> Result<String, String> {
    if let Ok(path) = which::which("claude") {
        return Ok(path.to_string_lossy().to_string());
    }

    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let candidate = format!(r"{}\npm\claude.cmd", appdata);
            if std::path::Path::new(&candidate).exists() {
                return Ok(candidate);
            }
        }
    }

    Err("找不到 claude 命令，请确认已安装 Claude Code CLI（npm install -g @anthropic-ai/claude-code）".into())
}

/// 发送消息到 Claude Code
///
/// 策略：
///   - 首次调用（session_id 为空）：`claude -p "prompt" --output-format stream-json --verbose`
///     → 从 init 事件中获取 session_id，后续调用需要用它来续接
///   - 续接调用（session_id 有值）：`claude -p "prompt" --resume SESSION_ID --output-format stream-json --verbose`
///     → Claude Code 自动加载历史上下文，实现多轮对话
#[tauri::command]
pub async fn claude_start(
    _state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    params: ClaudeStartParams,
) -> Result<String, String> {
    if params.prompt.trim().is_empty() {
        return Err("输入不能为空".into());
    }

    let claude_bin = find_claude_bin()?;

    // 如果有 session_id 则用 --resume 续接，否则开始新会话
    let is_resume = params.session_id.is_some();
    // 用于前端标识的临时 ID（首次调用时生成）
    let frontend_session_id = params.session_id.clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let mut cmd = Command::new(&claude_bin);
    cmd.arg("-p")
        .arg(&params.prompt)
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose");

    if is_resume {
        // 续接已有会话：--resume SESSION_ID
        cmd.arg("--resume").arg(params.session_id.as_ref().unwrap());
        log::info!("[claude] 续接会话: resume={}", params.session_id.as_ref().unwrap());
    }
    // 注意：不再使用 --no-session-persistence，让 Claude Code 持久化会话

    // 工作目录
    if let Some(ref dir) = params.working_dir {
        cmd.current_dir(dir);
    }

    // 模型
    if let Some(ref model) = params.model {
        cmd.arg("--model").arg(model);
    }

    // 权限模式
    let permission_mode = params.permission_mode.as_deref().unwrap_or("auto");
    cmd.arg("--permission-mode").arg(permission_mode);

    // 工具白名单
    if let Some(ref tools) = params.allowed_tools {
        if !tools.is_empty() {
            cmd.arg("--allowedTools").arg(tools.join(","));
        }
    }

    // 系统提示词
    if let Some(ref sp) = params.system_prompt {
        cmd.arg("--system-prompt").arg(sp);
    }

    // 设置子进程不继承 Job Object 的 KILL_ON_JOB_CLOSE
    #[cfg(windows)]
    {
        #[allow(unused_imports)]
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x00000200); // CREATE_NEW_PROCESS_GROUP
    }

    log::info!("[claude] 启动: frontend_session={}, model={:?}, permission={}, is_resume={}",
        frontend_session_id, params.model, permission_mode, is_resume);

    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        log::error!("[claude] 启动失败: {}", e);
        format!("启动 Claude Code 失败: {}", e)
    })?;

    let stdout = child.stdout.take().ok_or("无法获取 stdout")?;
    let stderr = child.stderr.take().ok_or("无法获取 stderr")?;

    let app_handle_clone = app_handle.clone();
    let frontend_sid: Arc<str> = Arc::from(frontend_session_id.as_str());

    // 读取 stdout 的 JSON 流
    let stdout_handle = {
        let app = app_handle_clone.clone();
        let _sid = frontend_sid.clone();
        tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                match serde_json::from_str::<serde_json::Value>(trimmed) {
                    Ok(json) => {
                        let msg_type = json.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        let subtype = json.get("subtype").and_then(|v| v.as_str());

                        let event = match msg_type {
                            "system" if subtype == Some("init") => {
                                // 返回真实的 session_id（Claude Code 分配的）
                                let real_session_id = json.get("session_id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let model = json.get("model")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let tools = json.get("tools")
                                    .and_then(|v| v.as_array())
                                    .map(|arr| arr.iter()
                                        .filter_map(|t| t.as_str().map(String::from))
                                        .collect())
                                    .unwrap_or_default();
                                Some(ClaudeMessage::Init {
                                    session_id: real_session_id,
                                    model,
                                    tools,
                                })
                            }
                            "system" if subtype == Some("thinking_tokens") => {
                                None
                            }
                            "assistant" => {
                                if let Some(message) = json.get("message") {
                                    if let Some(content) = message.get("content").and_then(|v| v.as_array()) {
                                        for item in content {
                                            let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                                            match item_type {
                                                "text" => {
                                                    if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                                                        let _ = app.emit("claude-message", ClaudeMessage::Text {
                                                            delta: text.to_string(),
                                                        });
                                                    }
                                                }
                                                "thinking" => {
                                                    if let Some(thinking) = item.get("thinking").and_then(|v| v.as_str()) {
                                                        let _ = app.emit("claude-message", ClaudeMessage::Thinking {
                                                            delta: thinking.to_string(),
                                                        });
                                                    }
                                                }
                                                "tool_use" => {
                                                    let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                                    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                                    let input = item.get("input").cloned().unwrap_or(serde_json::Value::Null);
                                                    let _ = app.emit("claude-message", ClaudeMessage::ToolUse {
                                                        id, name, input,
                                                    });
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                }
                                None
                            }
                            "user" => {
                                if let Some(message) = json.get("message") {
                                    if let Some(content) = message.get("content").and_then(|v| v.as_array()) {
                                        for item in content {
                                            let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                                            if item_type == "tool_result" {
                                                let tool_use_id = item.get("tool_use_id")
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or("")
                                                    .to_string();
                                                let content_str = item.get("content")
                                                    .map(|v| {
                                                        if v.is_string() {
                                                            v.as_str().unwrap_or("").to_string()
                                                        } else {
                                                            v.to_string()
                                                        }
                                                    })
                                                    .unwrap_or_default();
                                                let is_error = item.get("is_error")
                                                    .and_then(|v| v.as_bool())
                                                    .unwrap_or(false);
                                                let _ = app.emit("claude-message", ClaudeMessage::ToolResult {
                                                    tool_use_id, content: content_str, is_error,
                                                });
                                            }
                                        }
                                    }
                                }
                                None
                            }
                            "result" if subtype == Some("success") || subtype == Some("error") || subtype == Some("error_during_execution") => {
                                let is_error = subtype == Some("error") || subtype == Some("error_during_execution");
                                let text = json.get("result")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let cost = json.get("total_cost_usd")
                                    .and_then(|v| v.as_f64())
                                    .unwrap_or(0.0);
                                let duration = json.get("duration_ms")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                let result_session = json.get("session_id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();

                                if is_error {
                                    let error_msg = json.get("errors")
                                        .and_then(|v| v.as_array())
                                        .map(|arr| arr.iter()
                                            .filter_map(|e| e.as_str())
                                            .collect::<Vec<_>>()
                                            .join("; "))
                                        .filter(|s| !s.is_empty())
                                        .or_else(|| json.get("error").and_then(|v| v.as_str()).map(String::from))
                                        .unwrap_or_else(|| text.clone());
                                    Some(ClaudeMessage::Error { message: error_msg })
                                } else {
                                    Some(ClaudeMessage::Result {
                                        text, cost_usd: cost, duration_ms: duration, session_id: result_session,
                                    })
                                }
                            }
                            _ => None,
                        };

                        if let Some(event) = event {
                            let _ = app.emit("claude-message", event);
                        }
                    }
                    Err(e) => {
                        log::warn!("[claude] JSON 解析失败: {} — 原文: {}", e, trimmed);
                    }
                }
            }
        })
    };

    // 读取 stderr
    let stderr_handle = tokio::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            log::warn!("[claude stderr] {}", line);
        }
    });

    // 等待子进程退出
    let cleanup_sid = frontend_session_id.clone();
    tokio::spawn(async move {
        let _ = stdout_handle.await;
        let _ = stderr_handle.await;
        match child.wait().await {
            Ok(status) => {
                log::info!("[claude] 进程退出: session={}, status={}", cleanup_sid, status);
            }
            Err(e) => {
                log::error!("[claude] 等待进程退出失败: {}", e);
            }
        }
    });

    Ok(frontend_session_id)
}

/// 停止 Claude Code 会话
#[tauri::command]
pub async fn claude_stop(session_id: String) -> Result<(), String> {
    log::info!("[claude] 停止会话: {}", session_id);
    Ok(())
}

/// 检查 Claude Code CLI 是否可用
#[tauri::command]
pub async fn claude_check() -> Result<ClaudeCheckResult, String> {
    let bin = find_claude_bin()?;

    let output = tokio::process::Command::new(&bin)
        .arg("--version")
        .output()
        .await
        .map_err(|e| format!("执行 claude --version 失败: {}", e))?;

    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();

    Ok(ClaudeCheckResult {
        available: true,
        path: bin,
        version,
    })
}

#[derive(Serialize)]
pub struct ClaudeCheckResult {
    pub available: bool,
    pub path: String,
    pub version: String,
}
