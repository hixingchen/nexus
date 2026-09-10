//! AI 助手会话进程（dsh web GUI）
//!
//! 内嵌宿主必须是「原生子 WebView」（前端 AiPanel 创建，顶层导航），**不能用
//! iframe**：打包后父页 origin 是 tauri.localhost，与 dsh 的 localhost 跨站——
//! ① 父页 CSP（default-src 'self'，无 frame-src）直接拦截帧（报「已阻止此内容」）；
//! ② dsh 会话 cookie 为 SameSite=Strict，跨站 iframe 内不收不发 → 握手后 401。
//! 子 WebView 顶层导航不受父页 CSP 约束，Strict cookie 正常携带（dev/打包一致）。
//! URL host 规范为 localhost 仅是顺带对齐（dsh 绑 127.0.0.1，localhost 可达），
//! 对顶层导航无功能影响，保留以免 127.0.0.1 双栈怪癖。

use std::cmp::Ordering;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::Manager;

use crate::core::process::build_command;

// ─── dsh workspace 注册（GUI 工作区跟随项目目录的尽力机制） ─────────

/// dsh 存储目录：DSH_HOME 环境变量优先，默认 %USERPROFILE%\.dsh
pub fn dsh_storage_dir() -> Option<PathBuf> {
    if let Ok(h) = std::env::var("DSH_HOME") {
        if !h.trim().is_empty() {
            return Some(PathBuf::from(h));
        }
    }
    std::env::var("USERPROFILE").ok().map(|u| PathBuf::from(u).join(".dsh"))
}

/// 归一化路径用于比较（Windows 大小写不敏感 + 分隔符差异）
fn norm_path(p: &str) -> String {
    p.trim_end_matches(['/', '\\']).replace('\\', "/").to_lowercase()
}

/// ISO-8601 UTC 毫秒时间（与 dsh workspace.json 的 createdAt/updatedAt 格式一致）
fn iso_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/**
 * 把目录注册进 dsh workspace 注册表，使 dsh GUI 启动时自动打开该项目：
 * GUI 初始工作区 =「会话最近活动」的 workspace（无会话的 workspace 以
 * createdAt 参与比较）→ 新注册（无会话、createdAt=now）必被选中。
 * 已有会话的 workspace 无法保证（不改 dsh 内部会话文件），保持不变。
 *
 * 在 dsh 进程停止后、新会话 spawn 前调用（避免与运行中 host 的文件写竞争）。
 * 失败静默（文件不存在/解析失败/写失败都只是日志）——注册是尽力而为，
 * 绝不阻塞 AI 会话启动。
 */
fn ensure_workspace_registered_at(storage: &Path, dir: &str, title: Option<&str>) -> Result<(), String> {
    let file = storage.join("storages").join("workspace.json");
    let raw = match std::fs::read_to_string(&file) {
        Ok(r) => r,
        // dsh 尚未初始化（无注册表）：交给 dsh 自己创建，不抢跑
        Err(_) => return Ok(()),
    };
    let mut doc: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("workspace.json 解析失败: {}", e))?;

    let now = iso_now();
    let dir_norm = norm_path(dir);
    // 目标条目：找 path 匹配（忽略大小写/分隔符）
    let mut found: Option<&mut serde_json::Value> = None;
    if let Some(tables) = doc.get_mut("tables") {
        if let Some(workspaces) = tables.get_mut("workspaces").and_then(|w| w.as_object_mut()) {
            found = workspaces.values_mut()
                .find(|w| w.get("path").and_then(|p| p.as_str()).map(norm_path) == Some(dir_norm.clone()));
        }
    }
    match found {
        Some(ws) => {
            // title 是 dsh 纯显示字段（GUI 工作区列表名）：始终同步为项目名，
            // 与「有会话不抢时间」互不冲突——改名后 GUI 跟着变，且无副作用
            if let Some(t) = title {
                if ws.get("title").and_then(|x| x.as_str()) != Some(t) {
                    ws["title"] = serde_json::Value::String(t.to_string());
                }
            }
            let has_sessions = ws.get("sessionIds")
                .and_then(|s| s.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false);
            if !has_sessions {
                // 注册过但从未有会话：刷新时间使其重新成为「最近」
                ws["createdAt"] = serde_json::Value::String(now.clone());
                ws["updatedAt"] = serde_json::Value::String(now.clone());
                log::info!("[ai] workspace 已存在（无会话），刷新时间: {}", dir);
            } else {
                // 有会话历史：会话时间由 dsh 管理，不动（尽力而为边界）
                log::debug!("[ai] workspace 已有会话历史，仅同步标题: {}", dir);
            }
        }
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            let display = title
                .map(|t| t.to_string())
                .or_else(|| Path::new(dir).file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_else(|| dir.to_string());
            // path 用 Windows 原样反斜杠：dsh host 的 canonical 形式就是
            // Node fs.realpath 输出（Windows 上为反斜杠），会话 attach 校验
            // 用 realpath(header.cwd) !== record.path 字符串比较——
            // 写正斜杠会导致「会话创建成功但 attach 失败」（GUI 点不开工作区）
            #[cfg(windows)]
            let canonical = dir.replace('/', "\\");
            #[cfg(not(windows))]
            let canonical = dir.to_string();
            let ws = serde_json::json!({
                "path": canonical,
                "title": display,
                "sessionIds": [],
                "createdAt": now,
                "updatedAt": now,
            });
            // 插入注册表 + 头部（UI 列表置顶更接近「当前」）
            if let Some(tables) = doc.get_mut("tables") {
                if let Some(workspaces) = tables.get_mut("workspaces").and_then(|w| w.as_object_mut()) {
                    workspaces.insert(id.clone(), ws);
                    if let Some(global) = doc.get_mut("global") {
                        if let Some(ids) = global.get_mut("workspaceIds").and_then(|i| i.as_array_mut()) {
                            ids.insert(0, serde_json::Value::String(id.clone()));
                        }
                    }
                }
            }
            log::info!("[ai] 已注册 workspace: {}（title={}）", dir, display);
        }
    }
    atomic_write(&file, &serde_json::to_string_pretty(&doc).map_err(|e| format!("序列化失败: {}", e))?)?;
    Ok(())
}

/// 原子写（临时文件 + rename，避免半写文件被 dsh 读到）
fn atomic_write(file: &Path, content: &str) -> Result<(), String> {
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, content).map_err(|e| format!("写临时文件失败: {}", e))?;
    std::fs::rename(&tmp, file).map_err(|e| format!("替换 workspace.json 失败: {}", e))
}

/// 原子写二进制内容（同目录临时文件 + rename）。
/// 用于会话文件（`session.jsonl.zstd`）这类用户数据：直接整份 `fs::write` 在写入中途
/// 崩溃/断电/磁盘满时会留下截断文件，历史会话就此损坏。
fn atomic_write_bytes(file: &Path, content: &[u8]) -> Result<(), String> {
    let tmp = file.with_extension("tmp");
    std::fs::write(&tmp, content).map_err(|e| format!("写临时文件失败: {}", e))?;
    if let Err(e) = std::fs::rename(&tmp, file) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("替换会话文件失败 {}: {}", file.display(), e));
    }
    Ok(())
}

// ─── 会话活动时间 bump（让 dsh GUI 初始选中本项目） ────────────
// dsh GUI 初始工作区 = 「会话最近活动」的 workspace（recentWorkspace），会话
// updatedAt = max(header.createdAt, metadata.lastPromptAt)。把目标项目某会话的
// header.createdAt 顶到当前时刻 → GUI 必选中该项目（实测验证：有/无历史会话均生效）。

/// zstd 魔数（帧头 4 字节）
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// 解压 zstd 数据
fn zstd_decode(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut dec = zstd::stream::Decoder::new(std::io::Cursor::new(data))
        .map_err(|e| format!("zstd 解压初始化失败: {}", e))?;
    std::io::Read::read_to_end(&mut dec, &mut out).map_err(|e| format!("zstd 解压失败: {}", e))?;
    Ok(out)
}

/// zstd 压缩（level 3，无 checksum —— 兼容性已实测：dsh 读取不依赖 checksum）
fn zstd_encode(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut enc = zstd::stream::Encoder::new(&mut out, 3)
            .map_err(|e| format!("zstd 压缩初始化失败: {}", e))?;
        std::io::Write::write_all(&mut enc, data).map_err(|e| format!("zstd 压缩失败: {}", e))?;
        enc.finish().map_err(|e| format!("zstd 压缩收尾失败: {}", e))?;
    }
    Ok(out)
}

/// 查找 zstd 魔数位置
fn find_magic(data: &[u8], from: usize) -> Option<usize> {
    if data.len() < from + 4 { return None; }
    (from..=data.len() - 4).find(|&i| data[i..i + 4] == ZSTD_MAGIC)
}

/// 定位第一个 zstd 帧区间（dsh 会话文件 = 多帧拼接；首帧 = header 一行）
fn first_frame_bounds(data: &[u8]) -> Option<(usize, usize)> {
    let first = find_magic(data, 0)?;
    let second = find_magic(data, first + 4);
    Some((first, second.unwrap_or(data.len())))
}

/// 解压会话文件首帧并解析首行 JSON（header 记录）
fn decode_session_header(raw: &[u8]) -> Option<serde_json::Value> {
    let (s, e) = first_frame_bounds(raw)?;
    let dec = zstd_decode(&raw[s..e]).ok()?;
    let first_line = dec.split(|&b| b == b'\n').next()?;
    serde_json::from_slice(first_line).ok()
}

/// 当前 epoch 毫秒（与 dsh header.createdAt 同格式）
fn epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 扫描 dsh 会话目录，返回 cwd 匹配指定目录的会话文件（cwd 归一比较）
fn find_project_sessions(storage: &Path, dir_norm: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let sessions_root = storage.join("sessions");
    let Ok(top) = std::fs::read_dir(&sessions_root) else { return out };
    for group in top.flatten() {
        let Ok(ses) = std::fs::read_dir(group.path()) else { continue };
        for s in ses.flatten() {
            let file = s.path().join("session.jsonl.zstd");
            if !file.exists() { continue; }
            let Ok(raw) = std::fs::read(&file) else { continue };
            let Some(header) = decode_session_header(&raw) else { continue };
            let cwd_matches = header.get("cwd").and_then(|c| c.as_str()).map(norm_path) == Some(dir_norm.to_string());
            if cwd_matches { out.push(file); }
        }
    }
    out
}

/// bump 会话文件首帧 createdAt = 现在。
/// 帧替换语义：只解/改/压第一个帧（header 行），其余帧原样拼接 —— dsh 格式要求
/// 首帧恰好为 header 一行，结构不可整体重写（整文件重写会导致 host 拒读）
fn bump_session_activity(file: &Path) -> Result<(), String> {
    let raw = std::fs::read(file).map_err(|e| format!("读会话文件失败: {}", e))?;
    let (s, e) = first_frame_bounds(&raw).ok_or("会话文件帧结构异常")?;
    let plain = zstd_decode(&raw[s..e])?;
    let text = String::from_utf8_lossy(&plain);
    let mut head: serde_json::Value = text.lines().next()
        .ok_or("首帧无内容")?
        .parse()
        .map_err(|err: serde_json::Error| format!("header 解析失败: {}", err))?;
    if let Some(obj) = head.as_object_mut() {
        obj.insert("createdAt".into(), serde_json::Value::from(epoch_ms()));
    }
    let mut out = serde_json::to_string(&head).map_err(|e| format!("header 序列化失败: {}", e))?;
    out.push('\n');
    let new_frame = zstd_encode(out.as_bytes())?;
    let mut buf = Vec::with_capacity(new_frame.len() + (raw.len() - e));
    buf.extend_from_slice(&new_frame);
    buf.extend_from_slice(&raw[e..]);
    // 原子写：这是用户的会话历史文件，整份 std::fs::write 在写入中途崩溃/断电/磁盘满时
    // 会留下截断文件。与 workspace.json 同样走"临时文件 + rename"
    atomic_write_bytes(file, &buf)
}

/// 让 dsh GUI 打开时选中目标项目：注册 workspace（沿用注册逻辑）+
/// bump 该项目会话活动时间。host 停止后、spawn 前调用。尽力而为，失败不阻塞启动
pub fn ensure_workspace_active(dir: &str, title: Option<&str>) {
    let Some(storage) = dsh_storage_dir() else { return };
    // 1. 注册/刷新 workspace 记录（含 path 缺失时重建）
    if let Err(e) = ensure_workspace_registered_at(&storage, dir, title) {
        log::warn!("[ai] workspace 注册失败（忽略）: {}", e);
    }
    // 2. bump 本项目会话的活动时间 → recentWorkspace 必选中本项目
    let dir_norm = norm_path(dir);
    for file in find_project_sessions(&storage, &dir_norm) {
        if let Err(e) = bump_session_activity(&file) {
            log::warn!("[ai] 会话活动时间 bump 失败（忽略）: {}", e);
        }
    }
}

/// 一次 dsh web 会话（一个子进程）
pub struct AiSession {
    child: Child,
    pub pid: u32,
    /// 已规范为 localhost 的会话 URL（子 WebView 导航目标）
    pub url: String,
    /// 会话工作目录（前端据此判断归属项目）
    pub cwd: Option<String>,
}

impl AiSession {
    /// 进程是否仍存活（启动幂等复用前检查：崩溃后的内存残留会话需重启）
    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// 终止进程树并限时等待退出（超时由 Job Object 在应用退出时兜底）
    pub fn stop(&mut self) {
        // kill_and_reap 内部保证 kill 后一定 wait 一次（回收句柄）
        crate::core::process::kill_and_reap(&mut self.child, Duration::from_millis(2000));
    }
}

/// dsh CLI 是否可用（`dsh --version` 能否成功退出）
///
/// 走 `run_captured` 而不是手写 `spawn + wait`：后者**没有超时**（CLI 挂起会
/// 永久占用一个阻塞线程，AI 面板再也拿不到状态），也**没有把子进程加入 Job Object**
/// （应用异常退出后该进程可残留）。`run_captured` 两者都具备。
pub fn dsh_available(app: &tauri::AppHandle) -> bool {
    run_captured(app, "dsh --version", Duration::from_secs(10)).is_ok()
}

// ─── dsh 版本检查 / 升级（面板「检查更新」） ─────────────────────

/// 运行命令并捕获输出（stdout+stderr 合并，保留尾部若干行；失败信息带输出尾部便于诊断）
///
/// 超时未退出 → 杀进程树并报错：npm 在 registry 不可达/网络异常时可能长期挂起，
/// 「检查/升级」按钮不能无限等待。子进程加入共享 Job Object（应用退出兜底清理）。
/// 进程退出与 reader 线程刷盘有微小竞态，退出后短睡再读（诊断文本，无需精确）
fn run_captured(app: &tauri::AppHandle, command: &str, timeout: Duration) -> Result<String, String> {
    log::info!("[ai] 执行命令: {}", crate::core::process::mask_command(command));
    let mut cmd = build_command(command);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("启动命令失败: {}", e))?;
    #[cfg(windows)]
    if let Some(job) = app.state::<crate::AppState>().process_mgr.job_arc() {
        job.assign_child(&child);
    }
    let stdout = child.stdout.take().ok_or_else(|| "无法读取命令输出".to_string())?;
    let stderr = child.stderr.take().ok_or_else(|| "无法读取命令错误输出".to_string())?;
    let captured = Arc::new(Mutex::new(String::new()));
    spawn_capture_reader(stdout, Arc::clone(&captured));
    spawn_capture_reader(stderr, Arc::clone(&captured));

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                std::thread::sleep(Duration::from_millis(150)); // 等 reader 刷完剩余行
                let text = captured.lock().map(|g| g.clone()).unwrap_or_default();
                if status.success() {
                    return Ok(text);
                }
                return Err(format!("命令失败（退出码 {:?}）。输出：{}", status.code(), tail_text(&text)));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    crate::core::process::kill_and_reap(&mut child, Duration::from_millis(2000));
                    return Err(format!("命令超时（超过 {} 秒），已终止：{}", timeout.as_secs(), command));
                }
                std::thread::sleep(Duration::from_millis(80));
            }
            Err(e) => return Err(format!("等待命令退出失败: {}", e)),
        }
    }
}

/// 后台线程逐行累积输出（stdout/stderr 各起一个；非 UTF-8 行跳过；保留尾部）
fn spawn_capture_reader<R: std::io::Read + Send + 'static>(stream: R, captured: Arc<Mutex<String>>) {
    let spawn = std::thread::Builder::new()
        .name("nexus-cmd-capture".into())
        .spawn(move || {
            let reader = std::io::BufReader::new(stream);
            for line in reader.lines() {
                let Ok(line) = line else { continue };
                if let Ok(mut c) = captured.lock() {
                    c.push_str(line.trim_end());
                    c.push('\n');
                    if c.len() > 6000 {
                        let from = c.floor_char_boundary(c.len() - 3000);
                        c.drain(..from);
                    }
                }
            }
        });
    if let Err(e) = spawn {
        log::warn!("[ai] 命令输出读取线程创建失败（输出将不完整）: {}", e);
    }
}

/// 输出文本取尾部（最多 8 行；空时给占位说明）
fn tail_text(text: &str) -> String {
    let t = text.trim();
    if t.is_empty() {
        return "（无输出）".into();
    }
    let lines: Vec<&str> = t.lines().collect();
    if lines.len() > 8 {
        format!("…{}", lines[lines.len() - 8..].join("\n"))
    } else {
        t.to_string()
    }
}

/// 解析版本号：主段数字逐段比较前统一补足 3 段 + 是否有预发布后缀。
/// "v1.2.3-beta.1" → ([1,2,3], true)。解析失败 → None
fn parse_version(raw: &str) -> Option<(Vec<u64>, bool)> {
    let s = raw.trim().trim_start_matches('v').trim_start_matches('V');
    let (core, pre) = match s.split_once('-') {
        Some((c, p)) => (c, !p.trim().is_empty()),
        None => (s, false),
    };
    if core.is_empty() {
        return None;
    }
    let mut segs = Vec::new();
    for part in core.split('.') {
        if part.is_empty() {
            return None;
        }
        segs.push(part.parse::<u64>().ok()?);
    }
    while segs.len() < 3 {
        segs.push(0);
    }
    Some((segs, pre))
}

/// 版本比较（数值主段；主段相等时无预发布后缀 > 有后缀）
pub fn version_cmp(a: &str, b: &str) -> Option<Ordering> {
    let (sa, pa) = parse_version(a)?;
    let (sb, pb) = parse_version(b)?;
    match sa.cmp(&sb) {
        Ordering::Equal => match (pa, pb) {
            (true, false) => Some(Ordering::Less),
            (false, true) => Some(Ordering::Greater),
            _ => Some(Ordering::Equal),
        },
        o => Some(o),
    }
}

/// 从命令输出中提取第一个形如 x.y.z（可带 v 前缀/预发布后缀）的版本号
fn extract_version(text: &str) -> Option<String> {
    text.lines()
        .flat_map(|l| l.split(|c: char| c.is_whitespace() || c == ','))
        .map(|tok| tok.trim_matches(['"', '\'']))
        .find(|tok| parse_version(tok).is_some())
        .map(|tok| tok.to_string())
}

/// dsh 本地安装版本（`dsh --version` 输出首个版本号；未安装/解析失败 → None）
pub fn query_dsh_version(app: &tauri::AppHandle) -> Option<String> {
    run_captured(app, "dsh --version", Duration::from_secs(10))
        .ok()
        .and_then(|out| extract_version(&out))
}

/// npm registry 上 @deepseek-ai/dsh 的最新稳定版（联网，上限 30 秒）
pub fn query_dsh_latest(app: &tauri::AppHandle) -> Result<String, String> {
    let out = run_captured(app, "npm view @deepseek-ai/dsh version", Duration::from_secs(30))?;
    extract_version(&out).ok_or_else(|| format!("无法从 npm 响应解析版本号：{}", tail_text(&out)))
}

/// 升级/安装 dsh 到最新版（npm 全局；--no-fund/--no-audit 免噪音与审计网络请求）。
/// 调用方须先停掉运行中的 dsh 会话（Windows 文件锁，见 commands::ai）。返回安装后的版本
pub fn upgrade_dsh(app: &tauri::AppHandle) -> Result<String, String> {
    run_captured(
        app,
        "npm install -g @deepseek-ai/dsh@latest --no-fund --no-audit",
        Duration::from_secs(300),
    )?;
    Ok(query_dsh_version(app).unwrap_or_default())
}

/// 会话 URL host 规范为 localhost（无功能依赖，仅统一展示/导航目标，见模块注释）
fn to_localhost(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("http://127.0.0.1") {
        format!("http://localhost{}", rest)
    } else {
        url.to_string()
    }
}

/// 从 dsh web stdout 解析启动行中的 URL
fn parse_web_url(line: &str) -> Option<String> {
    let line = line.trim();
    let start = line.find("http://").or_else(|| line.find("https://"))?;
    let url = line[start..].trim_end();
    if url.is_empty() { None } else { Some(to_localhost(url)) }
}

/// 启动 dsh web 会话并等待 stdout 输出 URL（上限 timeout）
///
/// cwd：会话工作目录（dsh web UI 的项目根）。失败路径均先杀进程树再返回，
/// 不残留孤儿；错误携带 stderr 尾部便于诊断。
/// app：用于把子进程加入共享 Job Object（应用异常退出时兜底清理）
pub fn spawn_dsh_web(
    app: &tauri::AppHandle,
    cwd: Option<&str>,
    timeout: Duration,
) -> Result<AiSession, String> {
    log::info!("[ai] 启动 dsh web (cwd={:?})", cwd);

    let mut cmd = build_command("dsh web --no-open --port 0");
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动 dsh web 失败: {}", e))?;

    // 加入共享 Job Object：应用异常退出时兜底清理（正常退出走显式 stop）
    #[cfg(windows)]
    if let Some(job) = app.state::<crate::AppState>().process_mgr.job_arc() {
        job.assign_child(&child);
    }

    let pid = child.id();
    let stdout = child.stdout.take().ok_or_else(|| "无法获取 dsh web stdout".to_string())?;
    let stderr = child.stderr.take().ok_or_else(|| "无法获取 dsh web stderr".to_string())?;

    // stderr → 日志 + 尾部摘要（启动失败/超时时带回来诊断；非 UTF-8 行跳过）
    let stderr_tail = Arc::new(Mutex::new(String::new()));
    {
        let tail = Arc::clone(&stderr_tail);
        std::thread::Builder::new()
            .name(format!("nexus-ai-{}-stderr", pid))
            .spawn(move || {
                let reader = std::io::BufReader::new(stderr);
                for line in reader.lines() {
                    let line = match line {
                        Ok(l) => l,
                        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => continue,
                        Err(_) => break,
                    };
                    log::debug!("[ai#{}] stderr: {}", pid, line);
                    if let Ok(mut t) = tail.lock() {
                        t.push_str(&line);
                        t.push('\n');
                        if t.len() > 4000 {
                            let from = t.floor_char_boundary(t.len() - 2000);
                            t.drain(..from);
                        }
                    }
                }
            })
            .map_err(|e| {
                crate::core::process::kill_and_reap(&mut child, Duration::from_millis(2000));
                format!("创建 dsh web stderr 读取线程失败: {}", e)
            })?;
    }

    // stdout → 解析 URL 行唤醒等待者；EOF 未等到则发空串（视为启动失败）
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    {
        std::thread::Builder::new()
            .name(format!("nexus-ai-{}-stdout", pid))
            .spawn(move || {
                let reader = std::io::BufReader::new(stdout);
                let mut found = false;
                for line in reader.lines() {
                    let line = match line {
                        Ok(l) => l,
                        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => continue,
                        Err(_) => break,
                    };
                    if !found {
                        if let Some(url) = parse_web_url(&line) {
                            log::info!("[ai#{}] 解析到 dsh web URL", pid);
                            let _ = tx.send(url);
                            found = true;
                            // 找到 URL 后继续把 stdout 排空：直接 return 会 drop 读端 → 管道关闭 →
                            // dsh 后续写 stdout 收到 EPIPE（Node 可能因此退出），同时丢失后续诊断输出
                            continue;
                        }
                    }
                    if !line.trim().is_empty() {
                        log::debug!("[ai#{}] stdout: {}", pid, line.trim());
                    }
                }
                if !found {
                    log::warn!("[ai#{}] dsh web 在输出 URL 前退出", pid);
                    let _ = tx.send(String::new());
                }
            })
            .map_err(|e| {
                crate::core::process::kill_and_reap(&mut child, Duration::from_millis(2000));
                format!("创建 dsh web stdout 读取线程失败: {}", e)
            })?;
    }

    // 等待 URL（上限 timeout）。失败清理：杀树 + stderr 尾部诊断
    let url = match rx.recv_timeout(timeout) {
        Ok(u) if !u.is_empty() => u,
        Ok(_) => {
            crate::core::process::kill_and_reap(&mut child, Duration::from_millis(2000));
            return Err(format!("dsh web 启动失败：进程提前退出。{}", stderr_summary(&stderr_tail)));
        }
        Err(_) => {
            crate::core::process::kill_and_reap(&mut child, Duration::from_millis(2000));
            return Err(format!("等待 dsh web 启动超时（{} 秒）。{}", timeout.as_secs(), stderr_summary(&stderr_tail)));
        }
    };

    // 插件 bundle 就绪探测：过早导航会让 loader 永久报 failed to load
    // （冷启动竞态，stop 后立即重开最易触发）。探测失败仅告警，不阻塞启动
    if let (Some(port), Some(token)) = (parse_port(&url), parse_token(&url)) {
        wait_plugins_ready(port, &token);
    }

    Ok(AiSession {
        child,
        pid,
        url,
        cwd: cwd.map(|c| c.to_string()),
    })
}

/// 从会话 URL 提取端口
fn parse_port(url: &str) -> Option<u16> {
    let after = url.split("://").nth(1)?;
    let host_port = after.split('/').next()?;
    let port = host_port.rsplit(':').next()?;
    port.parse().ok()
}

/// 从会话 URL 提取一次性 token
fn parse_token(url: &str) -> Option<String> {
    let q = url.split('?').nth(1)?;
    for pair in q.split('&') {
        if let Some(v) = pair.strip_prefix("token=") {
            return Some(v.to_string());
        }
    }
    None
}

/// 取 stderr 尾部摘要（线程未及写入/空时为固定提示）
fn stderr_summary(tail: &Arc<Mutex<String>>) -> String {
    match tail.lock() {
        Ok(t) if !t.trim().is_empty() => format!("stderr: {}", t.trim_end()),
        _ => "（无 stderr 输出，请确认 dsh 已安装且可用：npm i -g @deepseek-ai/dsh）".into(),
    }
}

// ─── 插件 bundle 就绪探测（冷启动竞态防护） ────────────────────

/// 一次裸 HTTP GET（Connection: close），返回 (状态行, headers, body)
fn raw_get(port: u16, path: &str, cookie: Option<&str>) -> Option<(u16, String, String)> {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    let mut req = format!(
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n",
        path, port
    );
    if let Some(c) = cookie {
        req.push_str(&format!("Cookie: {}\r\n", c));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).ok()?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status: u16 = head.lines().next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Some((status, head.to_string(), body.to_string()))
}

/// 探测 dsh 插件 bundle 服务是否就绪：dsh 打印 URL 时 HTTP 已监听，但插件
/// 模块服务注册在其后 —— 过早导航会让页面 loader 报
/// 「bundle script ... failed to load」（永久错误，需整页刷新）。
/// 流程：token 握手拿会话 cookie → GET / 解析 loader 的 bundle URL/rev →
/// 轮询该 URL 直到 200。超时只告警不失败（让前端重试路径兜底）。
fn wait_plugins_ready(port: u16, token: &str) {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(8);

    // 1. 握手：GET /?token= → 303 + Set-Cookie
    let handshake = raw_get(port, &format!("/?token={}", token), None);
    let Some((status, head, _)) = handshake else {
        log::warn!("[ai] 插件就绪探测：握手失败（HTTP 不可达），跳过");
        return;
    };
    if status != 303 {
        log::debug!("[ai] 插件就绪探测：握手返回 {}（非 303，仍尝试）", status);
    }
    let cookie = head.lines()
        .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
        .map(|l| l["set-cookie:".len()..].trim().split(';').next().unwrap_or("").to_string());

    // 2. 解析 loader bundle URL（HTML 中第一个 href="/plugins/??@deepseek-ai/...&rev="）
    let bundle_path = cookie.as_ref().and_then(|c| {
        let html = raw_get(port, "/", Some(c));
        html.as_ref().and_then(|(_, _, body)| {
            let start = body.find(r#"href="/plugins/??@deepseek-ai/"#)? + r#"href=""#.len();
            let rest = &body[start..];
            let end = rest.find('"')?;
            Some(rest[..end].replace("&amp;", "&").to_string())
        })
    });
    let Some(bundle_path) = bundle_path else {
        log::warn!("[ai] 插件就绪探测：无法解析 bundle URL，跳过");
        return;
    };

    // 3. 轮询 bundle 直到 200 / 超时（退避：首测常在几百毫秒内就绪，固定 200ms 会白等）
    let mut backoff = Duration::from_millis(50);
    loop {
        match cookie.as_ref().and_then(|c| raw_get(port, &bundle_path, Some(c))) {
            Some((200, _, _)) => {
                // 用 start 时刻计时：deadline 是未来时刻，deadline.elapsed() 恒为 0
                log::info!("[ai] 插件 bundle 就绪（{}ms）", started.elapsed().as_millis());
                return;
            }
            _ if Instant::now() >= deadline => {
                log::warn!("[ai] 插件 bundle 就绪探测超时（8s），仍返回（前端有刷新兜底）");
                return;
            }
            _ => {
                // 退避轮询：首测常在几百毫秒内就绪，固定 200ms 会白等
                std::thread::sleep(backoff);
                backoff = std::cmp::min(backoff * 2, Duration::from_millis(400));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个最小假 workspace.json（对齐 dsh 结构）
    fn fixture(dir: &std::path::Path) -> String {
        let doc = serde_json::json!({
            "unit": { "name": "workspace", "version": 2 },
            "global": {
                "initialized": true,
                "workspaceIds": ["id-old"],
                "archivedSessionIds": []
            },
            "tables": {
                "workspaces": {
                    "id-old": {
                        "path": "D:\\work\\old-project",
                        "title": "old-project",
                        "sessionIds": [],
                        "createdAt": "2026-09-01T00:00:00.000Z",
                        "updatedAt": "2026-09-01T00:00:00.000Z"
                    }
                }
            }
        });
        std::fs::create_dir_all(dir.join("storages")).unwrap();
        std::fs::write(dir.join("storages").join("workspace.json"), serde_json::to_string_pretty(&doc).unwrap()).unwrap();
        dir.join("storages").join("workspace.json").to_string_lossy().into_owned()
    }

    #[test]
    fn test_register_new_workspace_prepends_and_wins_recent() {
        let dir = std::env::temp_dir().join(format!("ai-ws-test-{}", uuid::Uuid::new_v4()));
        fixture(&dir);
        ensure_workspace_registered_at(&dir, r"D:\work\GitProject\myapp", Some("我的应用")).unwrap();
        let doc: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("storages").join("workspace.json")).unwrap()
        ).unwrap();
        let ws = &doc["tables"]["workspaces"];
        let new = ws.as_object().unwrap().iter().find(|(id, _)| *id != "id-old").unwrap();
        // path 保持 Windows 反斜杠（host canonical 形式，attach 校验字符串比较依赖它）
        #[cfg(windows)]
        assert_eq!(new.1["path"], r"D:\work\GitProject\myapp");
        #[cfg(not(windows))]
        assert_eq!(new.1["path"], "D:/work/GitProject/myapp");
        // 传入的项目名优先于目录名 —— GUI 工作区名与 Nexus 项目列表一致
        assert_eq!(new.1["title"], "我的应用");
        assert!(new.1["sessionIds"].as_array().unwrap().is_empty());
        // 新注册置顶 → GUI 列表优先 + createdAt 最新参与「最近」比较
        assert_eq!(doc["global"]["workspaceIds"][0], new.0.clone());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_register_existing_empty_session_workspace_refreshes_time() {
        let dir = std::env::temp_dir().join(format!("ai-ws-test-{}", uuid::Uuid::new_v4()));
        fixture(&dir);
        ensure_workspace_registered_at(&dir, r"D:\WORK\OLD-PROJECT", None).unwrap(); // 大小写不同
        let doc: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("storages").join("workspace.json")).unwrap()
        ).unwrap();
        let ws = &doc["tables"]["workspaces"]["id-old"];
        assert_eq!(ws["path"], r"D:\work\old-project"); // 原 path 未被覆盖
        assert_ne!(ws["createdAt"], "2026-09-01T00:00:00.000Z"); // 时间已刷新
        // 不产生重复条目
        assert_eq!(doc["tables"]["workspaces"].as_object().unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_register_missing_file_is_noop() {
        let dir = std::env::temp_dir().join(format!("ai-ws-test-{}", uuid::Uuid::new_v4()));
        assert!(ensure_workspace_registered_at(&dir, "D:\\x", None).is_ok()); // 不报错、不创建文件
        assert!(!dir.join("storages").exists());
    }

    #[test]
    fn test_parse_web_url_localhost_normalized() {
        let line = "dsh web: http://127.0.0.1:2708/?token=abc123";
        let url = parse_web_url(line).unwrap();
        assert!(url.starts_with("http://localhost:2708/?token="));
    }

    #[test]
    fn test_parse_web_url_already_localhost() {
        assert_eq!(
            parse_web_url("http://localhost:1234/?token=abc"),
            Some("http://localhost:1234/?token=abc".to_string())
        );
    }

    #[test]
    fn test_parse_web_url_no_url() {
        assert_eq!(parse_web_url("some log line"), None);
        assert_eq!(parse_web_url(""), None);
    }

    #[test]
    fn test_to_localhost_keeps_token_intact() {
        let url = to_localhost("http://127.0.0.1:9/?token=token_has_127.0.0.1_substr");
        assert_eq!(url, "http://localhost:9/?token=token_has_127.0.0.1_substr");
    }

    /// 构造双帧会话文件（首帧 header + 尾随帧），模拟 dsh 真实布局
    fn make_session_file(dir: &std::path::Path, cwd: &str) -> PathBuf {
        let sid = "session-test-0001";
        let group = dir.join("sessions").join("--D-work--proj--");
        let file = group.join(sid).join("session.jsonl.zstd");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let header = serde_json::json!({
            "type": "session", "version": 0, "id": sid,
            "createdAt": 1_700_000_000_000_i64,
            "cwd": cwd, "delegationDepth": 0, "agentPreset": "codecraft"
        });
        let mut h = serde_json::to_string(&header).unwrap();
        h.push('\n');
        let frame1 = zstd_encode(h.as_bytes()).unwrap();
        let frame2 = zstd_encode(b"").unwrap(); // 尾随帧（原样保留验证）
        let mut buf = frame1;
        buf.extend_from_slice(&frame2);
        std::fs::write(&file, buf).unwrap();
        file
    }

    #[test]
    fn test_bump_session_updates_created_at_keeps_frames() {
        let dir = std::env::temp_dir().join(format!("ai-bump-test-{}", uuid::Uuid::new_v4()));
        let file = make_session_file(&dir, r"D:\work\GitProject\proj");
        let raw_before = std::fs::read(&file).unwrap();
        let frames_before = find_magic(&raw_before, 4).unwrap(); // 第二帧偏移存在

        bump_session_activity(&file).unwrap();

        let raw_after = std::fs::read(&file).unwrap();
        // 首帧长度会随 createdAt 数值变化而变（偏移必然移动），
        // 关键是尾随帧内容原样保留
        let second_after = find_magic(&raw_after, 4).unwrap();
        assert_eq!(
            &raw_after[second_after..],
            &raw_before[frames_before..],
            "尾随帧应原样保留"
        );
        // createdAt 已更新为当前时间
        let header = decode_session_header(&raw_after).unwrap();
        let created = header.get("createdAt").and_then(|c| c.as_i64()).unwrap();
        assert!(created > 1_700_000_000_000_i64, "createdAt 应被 bump 到当前");
        // 文件仍可整体解压（host 兼容性前提）
        assert!(zstd_decode(&raw_after).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_find_project_sessions_matches_normalized_cwd() {
        let dir = std::env::temp_dir().join(format!("ai-scan-test-{}", uuid::Uuid::new_v4()));
        make_session_file(&dir, r"D:\work\GitProject\proj");
        let found = find_project_sessions(&dir, &norm_path(r"D:\work\GitProject\proj"));
        assert_eq!(found.len(), 1);
        // 大小写/分隔符差异也能匹配
        let found2 = find_project_sessions(&dir, &norm_path("D:/WORK/GitProject/PROJ"));
        assert_eq!(found2.len(), 1);
        // 不匹配的目录返回空
        let found3 = find_project_sessions(&dir, &norm_path("D:/other/proj"));
        assert!(found3.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    // ── 版本解析 / 比较 ──────────────────────────────────

    #[test]
    fn test_version_cmp_basic() {
        use std::cmp::Ordering;
        assert_eq!(version_cmp("0.4.2", "0.5.0"), Some(Ordering::Less));
        assert_eq!(version_cmp("1.2.3", "1.2.3"), Some(Ordering::Equal));
        assert_eq!(version_cmp("2.0.0", "1.99.99"), Some(Ordering::Greater));
    }

    #[test]
    fn test_version_cmp_edge_shapes() {
        use std::cmp::Ordering;
        // 段数不足补 0；v 前缀忽略
        assert_eq!(version_cmp("v0.4", "0.4.0"), Some(Ordering::Equal));
        assert_eq!(version_cmp("1.2", "1.2.1"), Some(Ordering::Less));
        // 预发布：同主段时旧于正式版
        assert_eq!(version_cmp("1.2.3-beta.1", "1.2.3"), Some(Ordering::Less));
        assert_eq!(version_cmp("1.2.3", "1.2.4-beta"), Some(Ordering::Less));
        // 不同主段时预发布不影响数值比较
        assert_eq!(version_cmp("1.2.3-beta", "0.9.0"), Some(Ordering::Greater));
        // 解析失败
        assert_eq!(version_cmp("abc", "1.0.0"), None);
    }

    #[test]
    fn test_extract_version_from_output() {
        // 纯版本号 / 带 v 前缀 / 多 token 行只取第一个合法版本 / 无版本
        assert_eq!(extract_version("0.4.2"), Some("0.4.2".to_string()));
        assert_eq!(extract_version("v0.4.2\n"), Some("v0.4.2".to_string()));
        assert_eq!(extract_version("dsh 1.2.3 (DeepSeek Harness)\n"), Some("1.2.3".to_string()));
        assert_eq!(extract_version(""), None);
        assert_eq!(extract_version("nothing here"), None);
    }

    // ── 会话 URL 解析（就绪探测依赖它，此前无测试）────────────

    #[test]
    fn test_parse_web_url_and_localhost_normalization() {
        // 127.0.0.1 规范为 localhost（仅展示/导航统一，token 保留）
        assert_eq!(
            parse_web_url("dsh web listening on http://127.0.0.1:51234/?token=abc"),
            Some("http://localhost:51234/?token=abc".to_string())
        );
        assert_eq!(parse_web_url("no url here"), None);
        assert_eq!(parse_web_url(""), None);
    }

    #[test]
    fn test_parse_port_and_token() {
        let url = "http://localhost:51234/?token=xyz-123";
        assert_eq!(parse_port(url), Some(51234));
        assert_eq!(parse_token(url), Some("xyz-123".to_string()));
        // 缺端口 / 缺 token
        assert_eq!(parse_port("http://localhost/?token=a"), None);
        assert_eq!(parse_token("http://localhost:51234/"), None);
    }
}
