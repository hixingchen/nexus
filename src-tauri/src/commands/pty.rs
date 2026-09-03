use portable_pty::{CommandBuilder, PtyPair, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, State};
use crate::AppState;

/// PTY 会话状态：子进程、读写句柄
pub struct PtySession {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    _pty: PtyPair,
}

/// 全局 PTY 状态（AppState 持有，单例：同一时间只跑一个 claude 会话）
pub struct PtyState {
    session: Mutex<Option<PtySession>>,
}

impl PtyState {
    pub fn new() -> Self {
        Self {
            session: Mutex::new(None),
        }
    }

    /// 清理 PTY 会话：杀死整个进程树（幂等，空会话 no-op）
    ///
    /// 进程链是 cmd /C → claude(.cmd) → node：只 kill 顶层 cmd 会留下孙进程
    /// （未挂 job object，应用退出也不收），故用 taskkill /T /F 与服务进程一致
    pub fn cleanup(&self) {
        if let Ok(mut guard) = self.session.lock() {
            if let Some(mut session) = guard.take() {
                #[cfg(windows)]
                {
                    if let Some(pid) = session.child.process_id() {
                        crate::core::process::kill_process_tree(pid);
                    }
                }
                #[cfg(not(windows))]
                {
                    let _ = session.child.kill();
                }
                let _ = session.child.wait();
                log::info!("[nexus] PTY 会话已清理");
            }
        }
    }
}

/// 生成 PTY 并运行 claude CLI
///
/// 流程：创建伪终端 → spawn claude → 后台线程读输出 → emit 事件到前端。
/// 再次调用会先清理旧会话（同一时间只支持一个内嵌终端）
#[tauri::command]
pub fn pty_spawn(
    app: AppHandle,
    state: State<'_, AppState>,
    cwd: String,
    claude_path: Option<String>,
) -> Result<(), String> {
    // PtyState 是 AppState 的字段（与 cleanup_resources 同源），非顶层独立注册状态
    let pty_state = &state.pty;

    // cwd 必须真实存在，否则 claude 在无效目录启动会立即失败且难排查
    let cwd_path = std::path::Path::new(&cwd);
    if !cwd_path.is_dir() {
        return Err(format!("工作目录不存在: {}", cwd));
    }

    // 清理已有会话（切换服务/重复启动时旧 claude 一并终止）
    pty_state.cleanup();

    let pty_system = native_pty_system();
    let pty_pair = pty_system
        .openpty(PtySize {
            rows: 30,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("创建 PTY 失败: {}", e))?;

    let claude_cmd = claude_path.unwrap_or_else(|| "claude".to_string());

    // Windows：npm 全局安装的 claude 是 .cmd shim（非原生 exe），CreateProcess 无法直启
    // （os error 193 不是有效的 Win32 应用程序）→ 统一经 cmd /C 启动（原生 exe 同样兼容）。
    // 非 Windows：直接执行
    let mut cmd = {
        #[cfg(windows)]
        {
            let mut c = CommandBuilder::new("cmd");
            c.arg("/C");
            c.arg(&claude_cmd);
            c
        }
        #[cfg(not(windows))]
        {
            CommandBuilder::new(&claude_cmd)
        }
    };
    cmd.cwd(&cwd);

    let child = pty_pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!(
            "启动 claude 失败: {}（若 claude 为 npm 安装的 .cmd shim，已自动经 cmd 启动仍失败，请确认安装或用原生 claude.exe 路径）",
            e
        ))?;

    // 句柄获取失败路径必须杀掉已启动的 child（否则进程无人持有、无 job 覆盖，
    // 成为永久孤儿）
    let mut child = child;
    let (writer, reader) = match (pty_pair.master.take_writer(), pty_pair.master.try_clone_reader()) {
        (Ok(w), Ok(r)) => (w, r),
        (Err(e), _) | (_, Err(e)) => {
            #[cfg(windows)]
            {
                if let Some(pid) = child.process_id() {
                    crate::core::process::kill_process_tree(pid);
                }
            }
            #[cfg(not(windows))]
            {
                let _ = child.kill();
            }
            let _ = child.wait();
            return Err(format!("获取 PTY 读写句柄失败: {}", e));
        }
    };

    // 存储会话
    {
        let mut guard = pty_state
            .session
            .lock()
            .map_err(|e| format!("锁定 PTY 状态失败: {}", e))?;
        *guard = Some(PtySession {
            child,
            writer,
            _pty: pty_pair,
        });
    }

    // 后台线程：读取 PTY 输出并 emit 到前端
    let app_handle = app.clone();
    std::thread::spawn(move || {
        read_pty_output(reader, app_handle);
    });

    Ok(())
}

/// 后台线程：持续读取 PTY 输出，通过事件推送到前端
fn read_pty_output(mut reader: Box<dyn Read + Send>, app: AppHandle) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                let _ = app.emit("pty-output", serde_json::json!({ "eof": true }));
                break;
            }
            Ok(n) => {
                let text = String::from_utf8_lossy(&buf[..n]);
                let payload = serde_json::json!({
                    "data": text,
                    "eof": false,
                });
                if app.emit("pty-output", payload).is_err() {
                    break;
                }
            }
            Err(_) => {
                let _ = app.emit("pty-output", serde_json::json!({ "eof": true }));
                break;
            }
        }
    }
}

/// 向 PTY 写入用户输入
#[tauri::command]
pub fn pty_write(state: State<'_, AppState>, data: String) -> Result<(), String> {
    let pty_state = &state.pty;

    let mut guard = pty_state
        .session
        .lock()
        .map_err(|e| format!("锁定 PTY 状态失败: {}", e))?;

    if let Some(ref mut session) = *guard {
        session
            .writer
            .write_all(data.as_bytes())
            .map_err(|e| format!("写入 PTY 失败: {}", e))?;
        session
            .writer
            .flush()
            .map_err(|e| format!("刷新 PTY 缓冲区失败: {}", e))?;
        Ok(())
    } else {
        Err("PTY 会话未建立".to_string())
    }
}

/// 调整 PTY 尺寸（窗口大小变化时调用）
#[tauri::command]
pub fn pty_resize(state: State<'_, AppState>, rows: u16, cols: u16) -> Result<(), String> {
    let pty_state = &state.pty;

    let guard = pty_state
        .session
        .lock()
        .map_err(|e| format!("锁定 PTY 状态失败: {}", e))?;

    if let Some(ref session) = *guard {
        session
            ._pty
            .master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("调整 PTY 尺寸失败: {}", e))?;
    }
    Ok(())
}

/// 关闭 PTY 会话（杀死子进程）
#[tauri::command]
pub fn pty_kill(state: State<'_, AppState>) -> Result<(), String> {
    state.pty.cleanup();
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_placeholder() {
        // PTY 命令需要真实终端环境，集成测试在实际运行时验证
    }
}
