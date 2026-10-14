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
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::core::job_object::JobObject;
use crate::core::runtime::AiRuntime;

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

/// 解压输出上限（256MB）。dsh 会话文件是流式追加的 JSONL，正常远小于此；
/// 设限是为了让畸形/恶意输入（单个 zstd 帧可以极小的体积解出几十 GB）失败退出，
/// 而不是把内存吃光——分配失败在 Rust 里是 abort，Tauri 拦不住，整个应用会直接消失。
const MAX_ZSTD_DECODE_BYTES: u64 = 256 * 1024 * 1024;

/// 解压 zstd 数据（输出受 [`MAX_ZSTD_DECODE_BYTES`] 约束）
fn zstd_decode(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let dec = zstd::stream::Decoder::new(std::io::Cursor::new(data))
        .map_err(|e| format!("zstd 解压初始化失败: {}", e))?;
    // take(上限 + 1)：多读 1 字节用于区分"刚好等于上限"与"超过上限"
    let mut limited = std::io::Read::take(dec, MAX_ZSTD_DECODE_BYTES + 1);
    std::io::Read::read_to_end(&mut limited, &mut out)
        .map_err(|e| format!("zstd 解压失败: {}", e))?;
    if out.len() as u64 > MAX_ZSTD_DECODE_BYTES {
        return Err(format!(
            "会话内容解压后超过 {} MB 上限，已跳过（文件可能损坏或被构造）",
            MAX_ZSTD_DECODE_BYTES / 1024 / 1024
        ));
    }
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

/// 探测 header 时只读文件前若干字节。
///
/// 会话文件首帧就是 header 一行（几百字节），而原实现为了拿 `cwd` 把**每个**会话文件
/// 整读一遍（会话文件可以很大）——每次打开 AI 面板都要付出"全部会话字节"的读盘量。
/// 64KB 足够覆盖首帧及其后的第二个魔数（`first_frame_bounds` 需要它来界定首帧边界）。
const HEADER_PROBE_BYTES: u64 = 64 * 1024;

/// 读文件前若干字节（不足则返回全部）；失败返回 None
fn read_prefix(file: &Path, max: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let f = std::fs::File::open(file).ok()?;
    let mut buf = Vec::with_capacity(max.min(HEADER_PROBE_BYTES) as usize);
    f.take(max).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// 只读前缀解析会话 header（拿 cwd / createdAt 用，不做整读）
fn probe_session_header(file: &Path) -> Option<serde_json::Value> {
    decode_session_header(&read_prefix(file, HEADER_PROBE_BYTES)?)
}

/// dsh 会话分组目录名（cwd 派生）：`D:\work\GitProject\nexus` → `--D-work-GitProject-nexus--`。
///
/// 规则由真实目录反推（本机 7 个样本全部吻合）：去掉 `:`，`\` 换成 `-`，非 ASCII 按
/// UTF-16 码元转义为 `~XXXX`（大写十六进制，如 `福清` → `~798F~6E05`），最后前后各加 `--`。
/// 派生规则万一与 dsh 版本不一致，调用方有"整体回退扫描"兜底（见 find_project_sessions）。
fn group_name_for(dir: &str) -> String {
    let normalized = dir.replace('/', "\\");
    let mut out = String::with_capacity(normalized.len() + 16);
    out.push_str("--");
    for ch in normalized.chars() {
        match ch {
            ':' => {}
            '\\' => out.push('-'),
            c if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("~{:04X}", unit));
                }
            }
        }
    }
    out.push_str("--");
    out
}

/// 会话文件名（dsh 会把格式版本写进名字：`session.jsonl.zstd` / `session.v3.jsonl.zstd` /
/// `session.v4.jsonl.zstd` …）。
///
/// **为什么不能写死一个名字**（用户报过的症状）：写死 `session.jsonl.zstd` 的后果不是
/// "少 bump 一次"，而是整套"工作区跟随项目"**静默失效**——本机 137 个会话里 74 个是带
/// 版本号的（v3 68 个 + v4 6 个），一个都认不到，于是 dsh 永远打开上一次真正用过的那个项目，
/// 而 Nexus 这边看起来一切正常（注册表写进去了、进程也用对的 cwd 起来了）。
/// 判据放宽到"`session.` 开头、`.jsonl.zstd` 结尾"，下一个版本号出现时不必再改一次。
///
/// 安全性由 `bump_session_activity` 的 header 校验兜底：文件名认错了不会写坏数据，
/// 最多是"这次没 bump"。
fn is_session_file_name(name: &str) -> bool {
    name.starts_with("session.") && name.ends_with(".jsonl.zstd")
}

/// 扫描 dsh 会话目录，返回 cwd 匹配指定目录的会话文件（cwd 归一比较）。
///
/// 性能路径（原实现每次都整读**每个**会话文件）：① 先按 cwd 派生的分组目录名筛选，
/// 命中即只扫该目录；② 未命中则退回全量扫描，但每个文件只读前 64KB 解析 header。
/// 两条路径都不会因"派生规则不一致"而漏掉会话（后者只是慢一点，结果一样）。
fn find_project_sessions(storage: &Path, dir_norm: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let sessions_root = storage.join("sessions");
    let Ok(top) = std::fs::read_dir(&sessions_root) else { return out };
    let groups: Vec<std::path::PathBuf> = top.flatten().map(|g| g.path()).collect();
    let expected = group_name_for(dir_norm);
    let matched: Vec<&std::path::PathBuf> =
        groups.iter().filter(|g| g.file_name().and_then(|n| n.to_str()) == Some(expected.as_str())).collect();
    let scan: Vec<&std::path::PathBuf> = if matched.is_empty() {
        log::debug!("[ai] 会话分组名未命中（{}），回退全量扫描", expected);
        groups.iter().collect()
    } else {
        matched
    };
    for group in scan {
        let Ok(ses) = std::fs::read_dir(group) else { continue };
        for s in ses.flatten() {
            // 一个会话目录里就是那一个会话文件——但名字带版本号，所以要按规则认（见上）
            let Ok(files) = std::fs::read_dir(s.path()) else { continue };
            for f in files.flatten() {
                let name = f.file_name();
                let Some(name) = name.to_str() else { continue };
                if !is_session_file_name(name) { continue; }
                let file = f.path();
                let Some(header) = probe_session_header(&file) else { continue };
                let cwd_matches = header.get("cwd").and_then(|c| c.as_str()).map(norm_path) == Some(dir_norm.to_string());
                if cwd_matches { out.push(file); }
            }
        }
    }
    out
}

/// createdAt 距今小于该阈值的会话视为"已经够新鲜"，跳过整文件重写。
///
/// 重写一次会话文件 = 整读 + 解压首帧 + 重新压缩 + 写临时文件 + rename（按原大小整体写盘），
/// 同一个项目反复起停时不必每次都写一遍。
///
/// ⚠️ **这条跳过只能用在"同一个项目的重启"上**（见 `ensure_workspace_active` 的
/// `restarting_same_project`）。它的判据是"**我自己**够不够新鲜"，而 dsh 选工作区比的是
/// "**谁在所有项目里最新**"——两者不等价。曾经把它用在切换项目上，症状是：
/// 5 分钟内从 A 切到 B 再切回 A 时，A 因为"够新鲜"被跳过，而 B 的时间戳更新，
/// 于是 dsh 打开的是 B 而不是你在 Nexus 里点的 A。
const BUMP_SKIP_WINDOW_MS: i64 = 5 * 60 * 1000;

/// bump 会话文件首帧 createdAt = 现在。
/// 帧替换语义：只解/改/压第一个帧（header 行），其余帧原样拼接 —— dsh 格式要求
/// 首帧恰好为 header 一行，结构不可整体重写（整文件重写会导致 host 拒读）。
///
/// 已核对的三种文件名（`session.jsonl.zstd` / `.v3.` / `.v4.`）**首帧结构完全一致**：
/// 一个 zstd 帧、解出恰好一行 header JSON（含 `type:"session"` / `createdAt` / `cwd`），
/// 后续帧才是事件——所以这里的整帧替换对它们都安全（实测：v3 首帧 227 字节 1 行、
/// v4 首帧 207 字节 1 行）。
fn bump_session_activity(file: &Path) -> Result<(), String> {
    let raw = std::fs::read(file).map_err(|e| format!("读会话文件失败: {}", e))?;
    let (s, e) = first_frame_bounds(&raw).ok_or("会话文件帧结构异常")?;
    let plain = zstd_decode(&raw[s..e])?;
    let text = String::from_utf8_lossy(&plain);
    let mut head: serde_json::Value = text.lines().next()
        .ok_or("首帧无内容")?
        .parse()
        .map_err(|err: serde_json::Error| format!("header 解析失败: {}", err))?;
    // **只改"确实是会话 header"的首帧**：dsh 的格式还会再变，而这条路径写的是用户数据。
    // 认不出来时宁可这次不 bump（工作区跟随失效，用户看得见、可恢复），也不能往一个
    // 不认识的结构里塞字段——那是在写坏用户的会话历史，不可恢复。
    if head.get("type").and_then(|t| t.as_str()) != Some("session") {
        return Err("首帧不是会话 header（格式不认识），跳过改写".into());
    }
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
///
/// `restarting_same_project`：本次是不是"同一个项目、上一个进程已死"的重启。
/// **只有这种情况**才能跳过"已经够新鲜"的会话重写——上一次 bump 的就是它，它必然仍是
/// dsh 眼里最近活动的那个 workspace。切换项目、以及应用刚启动（还没有任何会话）都必须
/// 老老实实 bump：dsh GUI 选的是"所有项目里最近活动的"，只把自己刷新到"够新鲜"并不够
/// （详见 `BUMP_SKIP_WINDOW_MS` 的说明）。
pub fn ensure_workspace_active(dir: &str, title: Option<&str>, restarting_same_project: bool) {
    let Some(storage) = dsh_storage_dir() else { return };
    ensure_workspace_active_at(&storage, dir, title, restarting_same_project);
}

/// [`ensure_workspace_active`] 的实现：storage 由调用方给。
///
/// 拆出来的唯一理由是**可测**（与 `ensure_workspace_registered_at` 同一套做法）：
/// `dsh_storage_dir()` 读环境变量，而测试里改环境变量会影响同进程的其它用例——
/// 偏偏"切换项目必须 bump、同一项目重启才可以跳过"是这个模块里最该被钉住的判据
/// （它判错的症状就是用户报的：AI 面板打开的工作区和项目卡片对不上）。
pub(crate) fn ensure_workspace_active_at(
    storage: &Path,
    dir: &str,
    title: Option<&str>,
    restarting_same_project: bool,
) {
    // 1. 注册/刷新 workspace 记录（含 path 缺失时重建）
    if let Err(e) = ensure_workspace_registered_at(storage, dir, title) {
        log::warn!("[ai] workspace 注册失败（忽略）: {}", e);
    }
    // 2. bump 本项目会话的活动时间 → recentWorkspace 必选中本项目
    let dir_norm = norm_path(dir);
    for file in find_project_sessions(storage, &dir_norm) {
        if restarting_same_project {
            // 同一个项目的重启：上一次 bump 的就是它，跳过"已经够新鲜"的那些
            // （bump 一次 = 整读 + 解压 + 重压 + 整份写盘，见常量的说明）
            let created_at = probe_session_header(&file)
                .and_then(|h| h.get("createdAt").and_then(|c| c.as_i64()));
            if let Some(ts) = created_at {
                if epoch_ms() - ts < BUMP_SKIP_WINDOW_MS {
                    log::debug!("[ai] 会话活动时间已新鲜，跳过重写: {}", file.display());
                    continue;
                }
            }
        }
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

/// dsh 相关操作的超时集中表（ARCH-10）。
///
/// 为什么收进一处：此前这些数字散落在 6 个函数里（`Duration::from_secs(10)` 出现两次、
/// 60s 启动超时写在调用方），改一个值要在多个文件中找；而它们全都属于同一类决策
/// （"等 dsh/npm 多久算卡住"）。集中后既便于按机器/网络调优，也便于在文档里一句话说明。
pub struct AiTimeouts;

impl AiTimeouts {
    /// `dsh --version`（探活/取版本）：正常 <1s，10s 足够宽裕。
    /// 私有运行时的 `probe`（`<node.exe> <bin.js> --version`）也走这个值。
    pub const VERSION_PROBE: Duration = Duration::from_secs(10);
    /// `npm view`（联网查最新版）：registry 慢时常见数秒
    pub const NPM_QUERY: Duration = Duration::from_secs(30);
    /// `npm install`（下载安装/升级 dsh）：唯一允许分钟级的操作。
    ///
    /// 30 分钟而不是 5 分钟：**首次要下 269MB**，300 秒在慢网下不够；而"装到一半超时被杀"
    /// 会留下一个只能整个重来的半成品目录（比多等一会儿贵得多）。升级通常只下增量，远快于此。
    pub const NPM_INSTALL: Duration = Duration::from_secs(1800);
    /// `dsh web` 启动并打印 URL：冷启动要起 Node + 加载插件，60s 是保守上限
    pub const WEB_START: Duration = Duration::from_secs(60);
    /// 插件 bundle 就绪轮询窗口（URL 已打印但模块服务尚未注册）
    pub const PLUGINS_READY: Duration = Duration::from_secs(8);
    /// 环回 HTTP 读超时
    pub const LOOPBACK_READ: Duration = Duration::from_secs(3);
    /// 杀进程树后等它真正退出的宽限期（统一口径，避免各处写不同的毫秒数）
    pub const REAP: Duration = Duration::from_millis(2000);
}

/// 私有运行时探活结果（界面据此把"卡在哪"说清楚）
#[derive(Clone, Serialize)]
pub struct DshProbe {
    /// 运行时已就绪（node.exe / dsh 的 bin.js / 指针齐全）
    pub ready: bool,
    /// 这一对路径来自哪里（`manual` = 用户手动指定；`None` = 未就绪）。
    /// 界面据此决定给哪个按钮：重装 or 清除手动指定
    pub source: Option<crate::core::runtime::RuntimeSource>,
    /// 自带 Node 的版本（目录名，如 `v24.21.0`；手动指定 / 未就绪时 None）
    pub node_version: Option<String>,
    /// 实测到的 dsh 版本号（跑得起来时才有）
    pub version: Option<String>,
    /// 跑不起来时的原因（人话摘要 + 原话）
    pub problem: Option<String>,
}

/// 私有运行时探活：**就绪**（目录与指针齐全）+ **实测能跑**（输出里真有版本号）。
///
/// 两点为什么都必要：
/// - 判据是"输出里有版本号"而不是"退出码为 0"。dsh 的入口是
///   `if (import.meta.main) await runCli();`，在缺这个字段的 Node（20、24.0、24.1）上
///   那一行永远不成立，**进程干干净净退出、零输出、退出码 0**——只判退出码会把"什么都
///   没干"认成"可用"。这就是原实现（`dsh_available` 只看 `is_ok()`）的洞。
/// - 返回结构体而不是 bool：把"没装"和"装了但跑不动"分开，否则界面只能笼统说
///   "未检测到 dsh，请先安装"，用户照着重装一遍还是不行。
pub fn probe_dsh(job: Option<&JobObject>) -> DshProbe {
    let Some(rt) = crate::core::runtime::resolve() else {
        return DshProbe { ready: false, source: None, node_version: None, version: None, problem: None };
    };
    let source = Some(rt.source);
    let node_version = (!rt.node_version.is_empty()).then(|| rt.node_version.clone());
    match rt.probe(job) {
        Ok(v) => DshProbe { ready: true, source, node_version, version: Some(v), problem: None },
        Err(e) => DshProbe { ready: true, source, node_version, version: None, problem: Some(e) },
    }
}

// ─── dsh 版本检查 / 升级（面板「检查更新」） ─────────────────────

/// 运行命令并捕获输出（stdout+stderr 合并，保留尾部若干行；失败信息带输出尾部便于诊断）
///
/// 超时未退出 → 杀进程树并报错：npm 在 registry 不可达/网络异常时可能长期挂起，
/// 「检查/升级」按钮不能无限等待。子进程加入共享 Job Object（应用退出兜底清理）。
/// 进程退出与 reader 线程刷盘有微小竞态，退出后短睡再读（诊断文本，无需精确）
///
/// 为什么收 `job` 而不是 `AppHandle`（ARCH-19）：`core` 反向依赖 `commands::AppState`
/// 会让这组"真的 spawn 子进程、真的超时杀树"的函数在**结构上**不可测——`AppState` 只有
/// IPC 框架能造出来。改成显式依赖后，测试传 `None`（无 Job）就能直接驱动完整路径，
/// 与 `core/process.rs` 的 `LogSink`、`core/file_watcher.rs` 的 `on_change` 同一手法。
/// 句柄由命令层取一次传进来（`ProcessManager::job_arc`）。
/// **仅测试**：跑一条**命令串**（经 `cmd /C`）并收输出。
///
/// 生产路径已全部改走 [`crate::core::process::run_captured_command`] 的绝对路径形态
/// （见 `core::runtime`）。保留这一条是因为 `test_run_captured_timeout_kills_process_tree`
/// 要用 `cmd` 内建命令驱动"超时杀进程树"那条路。`#[cfg(test)]` 如实反映
/// "生产不再用命令串起内部命令"——这正是本轮要根除的形态。
#[cfg(test)]
fn run_captured(job: Option<&JobObject>, command: &str, timeout: Duration) -> Result<String, String> {
    // 本体在 `core::process`（ARCH-24）：私有运行时那条路要起**绝对路径的程序**、
    // 不经 cmd 不查 PATH，两条路必须共用同一套超时/杀树/输出捕获语义，否则口径会各自漂移
    crate::core::process::run_captured_command(
        job,
        &mut crate::core::process::build_internal_command(command),
        command,
        timeout,
    )
}

/// 解析版本号：主段数字逐段比较前统一补足 3 段 + 是否有预发布后缀。
/// "v1.2.3-beta.1" → ([1,2,3], true)。解析失败 → None
fn parse_version(raw: &str) -> Option<(Vec<u64>, Vec<PreId>)> {
    let s = raw.trim().trim_start_matches('v').trim_start_matches('V');
    let (core, pre_raw) = match s.split_once('-') {
        Some((c, p)) => (c, p.trim()),
        None => (s, ""),
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
    // 预发布段：`rc.2` → [Alpha("rc"), Num(2)]。解析不出数字的段当字母段（宽松，
    // 不为一个奇怪的后缀丢掉整个版本号）
    let pre = pre_raw
        .split('.')
        .filter(|p| !p.is_empty())
        .map(|p| p.parse::<u64>().map(PreId::Num).unwrap_or_else(|_| PreId::Alpha(p.to_ascii_lowercase())))
        .collect();
    Some((segs, pre))
}

/// 版本比较（数值主段；主段相等时无预发布后缀 > 有后缀）
pub fn version_cmp(a: &str, b: &str) -> Option<Ordering> {
    let (sa, pa) = parse_version(a)?;
    let (sb, pb) = parse_version(b)?;
    match sa.cmp(&sb) {
        Ordering::Equal => Some(cmp_pre(&pa, &pb)),
        o => Some(o),
    }
}

/// 预发布段里的一个标识符：数字段按**数值**比、字母段按字典序比，且**数字 < 字母**
/// （语义化版本规则）。
#[derive(Debug, PartialEq, Eq)]
enum PreId {
    Num(u64),
    Alpha(String),
}

/// 按语义化版本规则比较预发布段。
///
/// 为什么不能只看"有没有预发布后缀"：dsh 发布的全是预发布版（`0.1.5-rc.1`、`0.1.6-alpha.1`…），
/// 只判有无的话 `0.1.5-rc.1` 与 `0.1.5-rc.2` 会判成**相等**——同一个主段内的升级永远检测不到，
/// 用户明明装的是旧 rc，界面却说"已是最新版本"（升级按钮根本不出现）。
fn cmp_pre(a: &[PreId], b: &[PreId]) -> Ordering {
    match (a.is_empty(), b.is_empty()) {
        (true, true) => return Ordering::Equal,
        // 无预发布 > 有预发布：1.0.0 > 1.0.0-rc.1
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        _ => {}
    }
    for (x, y) in a.iter().zip(b.iter()) {
        let o = match (x, y) {
            (PreId::Num(n), PreId::Num(m)) => n.cmp(m),
            (PreId::Num(_), PreId::Alpha(_)) => Ordering::Less,
            (PreId::Alpha(_), PreId::Num(_)) => Ordering::Greater,
            (PreId::Alpha(s), PreId::Alpha(t)) => s.cmp(t),
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    // 公共前缀全同：段数少的更小（1.0.0-rc < 1.0.0-rc.1）
    a.len().cmp(&b.len())
}

/// 从命令输出中提取第一个形如 x.y.z（可带 v 前缀/预发布后缀）的版本号。
///
/// `pub(crate)`：私有运行时的探测判据也用它——"什么叫一个版本号"必须只有一处定义，
/// 否则两条路的判据迟早漂移，而这次的洞（exit 0 但零输出被判成可用）正出在判据上。
pub(crate) fn extract_version(text: &str) -> Option<String> {
    text.lines()
        .flat_map(|l| l.split(|c: char| c.is_whitespace() || c == ','))
        .map(|tok| tok.trim_matches(['"', '\'']))
        .find(|tok| parse_version(tok).is_some())
        .map(|tok| tok.to_string())
}

/// 私有 dsh 的版本（**实测**；跑不起来 → None）
pub fn query_dsh_version(rt: &AiRuntime, job: Option<&JobObject>) -> Option<String> {
    rt.probe(job).ok()
}

/// npm registry 上 @deepseek-ai/dsh 的最新稳定版（联网，上限 30 秒）
///
/// 走**私有 npm**：私有化之后目标机器上可能根本没有 Node，PATH 上没有 npm 可用；
/// 就算有，那也是别的 Node 的 npm。镜像配置（用户的 `~/.npmrc`）依然生效。
pub fn query_dsh_latest(rt: &AiRuntime, job: Option<&JobObject>) -> Result<String, String> {
    let mut cmd = rt.npm_command(&["view", crate::core::runtime::DSH_PACKAGE, "version"])?;
    let label = format!("npm view {} version（私有运行时）", crate::core::runtime::DSH_PACKAGE);
    let out = crate::core::process::run_captured_command(job, &mut cmd, &label, AiTimeouts::NPM_QUERY)?;
    extract_version(&out).ok_or_else(|| format!("无法从 npm 响应解析版本号：{}", crate::core::process::tail_of(&out)))
}

// 升级/安装 dsh 的入口搬到了 `core::runtime::install_or_upgrade_dsh`：
// 它必须用**私有 node + 私有 npm + `--prefix` 私有目录**（`-g` 会按用户级 `~/.npmrc`
// 里的 `prefix=` 装进用户那份，于是界面报"升级成功"而实际跑的还是老版本、且不报错），
// 并且装完要**实跑一次**确认——这两条都属于运行时自己的知识，不该散在 ai.rs 里。

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
/// job：共享 Job Object（应用异常退出时兜底清理），由命令层传入；测试传 None
///
/// `runtime` 提供的是**一对绝对路径**：`<私有node.exe> <私有bin.js> web …`。
/// 这是整件事的收口——原先 `cmd /C "dsh web …"` 要经两次 PATH 解析（dsh 的 shim →
/// shim 里的裸 `node`），于是"AI 用哪个 Node"由用户 nvm 当前选中的版本决定。
pub fn spawn_dsh_web(
    runtime: &AiRuntime,
    job: Option<&JobObject>,
    cwd: Option<&str>,
    timeout: Duration,
) -> Result<AiSession, String> {
    spawn_web_command(
        job,
        runtime.command(&["web", "--no-open", "--port", "0"]),
        cwd,
        timeout,
    )
}

/// `spawn_dsh_web` 的实体。
///
/// 命令为何是参数（ARCH-19）：这条函数里有 spawn + 双流读取 + URL 解析 + 三条 kill 清理
/// 路径，却长期零覆盖——它们此前只能靠"机器上真的装了 dsh"才能走到。命令变成参数后，
/// 测试用一条 `cmd /C …` 就能驱动全部路径（生产只有一个入口，即上面的 `spawn_dsh_web`）。
///
/// 参数从"命令串"改成 `Command`：生产那条路**必须能指定绝对路径的可执行文件**
/// （`Command::new` 收的必须是绝对路径），而命令串只能经 `cmd /C` 再查 PATH。
/// 测试那条路自己用 `build_internal_command` 拼 `cmd /C`，能力一点没减。
fn spawn_web_command(
    job: Option<&JobObject>,
    mut cmd: Command,
    cwd: Option<&str>,
    timeout: Duration,
) -> Result<AiSession, String> {
    log::info!("[ai] 启动 dsh web (cwd={:?})", cwd);

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
    if let Some(job) = job {
        // 纳管失败只影响"应用被强杀"这条路径（CQ-27）；assign_child 内部已给出原因，这里留痕
        if let Err(e) = job.assign_child(&child) {
            log::warn!("[nexus] dsh 进程未纳入 Job Object（Nexus 异常退出时可能残留）: {}", e);
        }
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
                    // 凭据红线（SEC-23）：这一行既进日志（落盘 ~/.nexus/logs/nexus.log）
                    // 也进 stderr 尾部摘要（启动失败时原样回给界面），而 dsh 的启动横幅
                    // 里可能带 `?token=…` 的会话 URL——两处都先过一遍脱敏（与命令脱敏同一
                    // 套口径，SENSITIVE_NAME_HINTS 覆盖 token/secret/password…）
                    let line = crate::core::process::mask_command(&line);
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
                crate::core::process::kill_and_reap(&mut child, AiTimeouts::REAP);
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
                crate::core::process::kill_and_reap(&mut child, AiTimeouts::REAP);
                format!("创建 dsh web stdout 读取线程失败: {}", e)
            })?;
    }

    // 等待 URL（上限 timeout）。失败清理：杀树 + stderr 尾部诊断
    let url = match rx.recv_timeout(timeout) {
        Ok(u) if !u.is_empty() => u,
        Ok(_) => {
            crate::core::process::kill_and_reap(&mut child, AiTimeouts::REAP);
            return Err(format!("dsh web 启动失败：进程提前退出。{}", stderr_summary(&stderr_tail)));
        }
        Err(_) => {
            crate::core::process::kill_and_reap(&mut child, AiTimeouts::REAP);
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

/// `raw_get` 的响应上限（2 MB）。
///
/// 为什么要有：这里请求的是 dsh 自己的环回服务，正常响应只有几百字节（303 头 + 一小段 HTML）。
/// 但环回端口上的对端不一定是 dsh——端口被别的进程占用时，`read_to_end` 会一直读到对方
/// 关闭连接为止，把任意大小的数据读进内存（且有 3s 读超时也拦不住持续输出的流）。
/// 超限即按"响应不可信"处理，直接放弃这次探测（调用方本就有超时兜底）。
///
/// **只适用于小响应**（握手 + 那一小段 HTML）。要读 body 的请求别用这条口径：
/// 插件 bundle 是 JS 资源、有好几 MB，用 2MB 上限去量它只会**每次都判不可信**
/// （曾经就是这样——见 `raw_get_status`）。
const RAW_GET_MAX_BYTES: u64 = 2 * 1024 * 1024;

/// 只取状态行的裸 GET（`Connection: close`）——给"只需知道对方是不是 200"的探测用。
///
/// 为什么不复用 `raw_get`：它会把整个 body 读进内存并受 `RAW_GET_MAX_BYTES` 限制，
/// 而**插件 bundle 是好几 MB 的 JS**——于是那条探测每次都撞上限、必然判为不可信，
/// 8 秒轮询窗口被白烧光（日志实测：每次会话启动都比 dsh 就绪晚 **8.1 秒**，
/// 之后才由前端刷新兜底）。这里只读到状态行，body 一个字都不读（调用方本来也不用）。
fn raw_get_status(port: u16, path: &str, cookie: Option<&str>) -> Option<u16> {
    use std::io::{BufRead, BufReader, Read, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(AiTimeouts::LOOPBACK_READ)).ok()?;
    let mut req = format!(
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n",
        path, port
    );
    if let Some(c) = cookie {
        req.push_str(&format!("Cookie: {}\r\n", c));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).ok()?;
    // take(8KB)：状态行 + 头只有几百字节，这个上限只为挡住"端口上其实是别的进程"时
    // 那条没有换行的长行（read_line 会一直长下去）。body 不在读取范围内。
    let mut reader = BufReader::new(Read::take(&mut stream, 8 * 1024));
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// 一次裸 HTTP GET（Connection: close），返回 (状态行, headers, body)
fn raw_get(port: u16, path: &str, cookie: Option<&str>) -> Option<(u16, String, String)> {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(AiTimeouts::LOOPBACK_READ)).ok()?;
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
    // take(上限)：超限时返回"已读到的部分"而不报错，这里用读满上限来判定不可信
    stream.take(RAW_GET_MAX_BYTES).read_to_end(&mut buf).ok()?;
    if buf.len() as u64 >= RAW_GET_MAX_BYTES {
        log::warn!("[ai] 插件就绪探测：响应超过 {} 字节上限，按不可信处理并放弃", RAW_GET_MAX_BYTES);
        return None;
    }
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
    let deadline = started + AiTimeouts::PLUGINS_READY;

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

    // 3. 轮询 bundle 直到 200 / 超时（退避：首测常在几百毫秒内就绪，固定 200ms 会白等）。
    //    用 `raw_get_status` 而不是 `raw_get`：bundle 是好几 MB 的 JS，把 body 读进来
    //    既慢又会撞 2MB 上限判成"不可信"——那样这条探测永远不可能成功（见该函数的说明）。
    let mut backoff = Duration::from_millis(50);
    loop {
        match cookie.as_ref().and_then(|c| raw_get_status(port, &bundle_path, Some(c))) {
            Some(200) => {
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

    /// 会话分组目录名的派生规则：期望值全部取自本机 `~/.dsh/sessions` 的**真实目录名**，
    /// 规则一旦与 dsh 版本不符，这里会先炸（而不是让 AI 面板静默选错工作区）。
    #[test]
    fn test_group_name_for_matches_real_dsh_dirs() {
        assert_eq!(group_name_for(r"D:\work\GitProject\nexus"), "--D-work-GitProject-nexus--");
        assert_eq!(
            group_name_for(r"D:\work\GitProject\nexus\src-tauri"),
            "--D-work-GitProject-nexus-src-tauri--"
        );
        assert_eq!(
            group_name_for(r"D:\work\SVNProject\老干局-人才库"),
            "--D-work-SVNProject-~8001~5E72~5C40-~4EBA~624D~5E93--"
        );
        assert_eq!(
            group_name_for("D:/work/SVNProject/演示-智慧运维电梯服务平台-show-smart-elevator-ops-platform"),
            "--D-work-SVNProject-~6F14~793A-~667A~6167~8FD0~7EF4~7535~68AF~670D~52A1~5E73~53F0-show-smart-elevator-ops-platform--"
        );
        assert_eq!(
            group_name_for(r"D:\work\SVNProject\福清-陆源科技化工园区-code"),
            "--D-work-SVNProject-~798F~6E05-~9646~6E90~79D1~6280~5316~5DE5~56ED~533A-code--"
        );
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

    /// 构造双帧会话文件（首帧 header + 尾随帧），模拟 dsh 真实布局。
    ///
    /// 默认用**无版本号的旧名字**；带版本号的名字（dsh 现在实际写的）见 `make_session_file_named`。
    fn make_session_file(dir: &std::path::Path, cwd: &str) -> PathBuf {
        make_session_file_named(dir, cwd, "session.jsonl.zstd")
    }

    /// 同 `make_session_file`，但文件名可指定（`session.v3.jsonl.zstd` 等）
    fn make_session_file_named(dir: &std::path::Path, cwd: &str, name: &str) -> PathBuf {
        let sid = "session-test-0001";
        let group = dir.join("sessions").join("--D-work--proj--");
        let file = group.join(sid).join(name);
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

    /// 首帧是**任意一行 JSON** 的会话文件（用来验证"认不出来就拒绝改写"）
    fn make_session_file_with_first_line(dir: &std::path::Path, first_line: &str) -> PathBuf {
        let group = dir.join("sessions").join("--D-work--proj--");
        let file = group.join("session-odd-0001").join("session.v9.jsonl.zstd");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let mut line = first_line.to_string();
        line.push('\n');
        let mut buf = zstd_encode(line.as_bytes()).unwrap();
        buf.extend_from_slice(&zstd_encode(b"").unwrap());
        std::fs::write(&file, buf).unwrap();
        file
    }

    /// dsh 从 v3 起把格式版本写进**文件名**（`session.v3.jsonl.zstd` / `session.v4.jsonl.zstd`）。
    ///
    /// 这条钉的是用户报过的症状：Nexus 曾写死 `session.jsonl.zstd`，于是带版本号的会话
    /// **一个都扫不到**（本机 137 个里 74 个），`ensure_workspace_active` 没有可 bump 的对象
    /// ——"工作区跟随项目"整套静默失效，表现为"点了项目，AI 面板打开的还是另一个项目"。
    #[test]
    fn test_find_project_sessions_matches_versioned_file_names() {
        let cwd = r"D:\work\GitProject\proj";
        for name in ["session.jsonl.zstd", "session.v3.jsonl.zstd", "session.v4.jsonl.zstd"] {
            let dir = std::env::temp_dir().join(format!("ai-name-test-{}", uuid::Uuid::new_v4()));
            make_session_file_named(&dir, cwd, name);
            let found = find_project_sessions(&dir, &norm_path(cwd));
            assert_eq!(found.len(), 1, "`{}` 必须被认到（否则工作区跟随静默失效）", name);
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    /// 会话目录里的非会话文件不该被当成会话（放宽文件名判据的代价要守住）
    #[test]
    fn test_find_project_sessions_ignores_non_session_files() {
        let cwd = r"D:\work\GitProject\proj";
        let dir = std::env::temp_dir().join(format!("ai-name-test-{}", uuid::Uuid::new_v4()));
        let session = make_session_file_named(&dir, cwd, "session.v3.jsonl.zstd");
        // 同一个会话目录里放几个别的文件
        let session_dir = session.parent().unwrap();
        std::fs::write(session_dir.join("session.jsonl"), b"{}").unwrap();
        std::fs::write(session_dir.join("notes.jsonl.zstd"), b"x").unwrap();
        let found = find_project_sessions(&dir, &norm_path(cwd));
        assert_eq!(found.len(), 1, "只该认出那一个会话文件，实际 {:?}", found);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// **首帧不是会话 header 时拒绝改写**，且一个字节都不动。
    ///
    /// 这是这条路径唯一的安全底线：它写的是用户的会话历史（不可恢复）。dsh 的格式还会再变，
    /// 认不出来时正确的动作是"这次不 bump"（工作区跟随失效，用户看得见），
    /// 而不是往一个不认识的结构里塞 `createdAt`。
    #[test]
    fn test_bump_refuses_unknown_first_frame_without_touching_file() {
        let dir = std::env::temp_dir().join(format!("ai-guard-test-{}", uuid::Uuid::new_v4()));
        // 首帧是合法 JSON，但不是会话 header（type 不是 session）
        let file = make_session_file_with_first_line(&dir, r#"{"type":"event","payload":"x"}"#);
        let before = std::fs::read(&file).unwrap();
        assert!(bump_session_activity(&file).is_err(), "认不出首帧时必须报错而不是改写");
        assert_eq!(std::fs::read(&file).unwrap(), before, "拒绝改写时必须一个字节都不动");

        // 首帧根本不是 JSON：同样拒绝
        let file2 = make_session_file_with_first_line(&dir, "not json at all");
        let before2 = std::fs::read(&file2).unwrap();
        assert!(bump_session_activity(&file2).is_err());
        assert_eq!(std::fs::read(&file2).unwrap(), before2);

        std::fs::remove_dir_all(&dir).ok();
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

    /// **切换项目必须 bump**，哪怕本项目的会话"刚刚才写过"；只有"同一项目、进程已死"的
    /// 重启才允许跳过。
    ///
    /// 这条钉的是用户报过的症状：5 分钟内从 A 切到 B 再切回 A 时，A 因为"够新鲜"被跳过，
    /// 而 B 的时间戳更新，于是 dsh GUI 打开的是 **B**——而 Nexus 里选中的是 A。
    /// 根子在判据问错了问题：跳过窗口问的是"**我自己**够不够新鲜"，而 dsh 选工作区比的是
    /// "**谁在所有项目里最新**"。这两个问题只在"没有别的项目被 bump 过"时才等价。
    #[test]
    fn test_workspace_active_bumps_on_switch_but_skips_on_same_project_restart() {
        let storage = std::env::temp_dir().join(format!("ai-active-test-{}", uuid::Uuid::new_v4()));
        let cwd = r"D:\work\GitProject\proj";
        let file = make_session_file(&storage, cwd);
        let created_at = |f: &std::path::Path| -> i64 {
            probe_session_header(f)
                .and_then(|h| h.get("createdAt").and_then(|c| c.as_i64()))
                .expect("会话 header 里有 createdAt")
        };
        // 造出来的会话是 2023 年的时间戳（远早于跳过窗口）
        assert!(epoch_ms() - created_at(&file) > BUMP_SKIP_WINDOW_MS);

        // 切换项目（restarting_same_project = false）→ 必须 bump 到"现在"
        ensure_workspace_active_at(&storage, cwd, Some("proj"), false);
        let after_switch = created_at(&file);
        assert!(
            epoch_ms() - after_switch < BUMP_SKIP_WINDOW_MS,
            "切换项目后会话时间戳必须是新鲜的，实际 {}",
            after_switch
        );

        // 同一项目的重启 → 允许跳过（现在它确实已经是最新的了）
        std::thread::sleep(std::time::Duration::from_millis(5));
        ensure_workspace_active_at(&storage, cwd, Some("proj"), true);
        assert_eq!(created_at(&file), after_switch, "同一项目重启可以跳过重写");

        // 再切一次项目（restarting_same_project = false）→ **即使刚刚才写过也要重写**：
        // 这正是原实现漏掉的那一步（它看到"够新鲜"就跳过了）
        std::thread::sleep(std::time::Duration::from_millis(5));
        ensure_workspace_active_at(&storage, cwd, Some("proj"), false);
        assert!(
            created_at(&file) > after_switch,
            "切换项目必须重新 bump，否则 dsh 会打开另一个更近活动的项目"
        );

        std::fs::remove_dir_all(&storage).ok();
    }

    // ── 插件就绪探测 ─────────────────────────────────────

    /// 回归：插件 bundle 是好几 MB 的 JS，探测**必须只看状态行**。
    ///
    /// 老实现用 `raw_get`（把 body 整读进内存 + 2MB 上限），于是每次会话启动都判
    /// "响应不可信" → 8 秒轮询窗口被白烧光（日志实测 10:40.281 就绪 → 10:48.443 超时，
    /// 每次差值 8.1s）。这条测试用"状态行 + 2.5MB body"的假服务端把该契约钉住。
    #[test]
    fn test_raw_get_status_ignores_large_body() {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind 随机端口");
        let port = listener.local_addr().expect("取端口").port();
        let writer = std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2621440\r\n\r\n");
                let chunk = vec![b'x'; 64 * 1024];
                // 读端只取状态行后就关闭连接 → 这里的写会失败并退出循环（不会挂住 join）
                for _ in 0..40 {
                    if s.write_all(&chunk).is_err() {
                        break;
                    }
                }
            }
        });
        assert_eq!(
            raw_get_status(port, "/plugins/??@deepseek-ai/dsh&rev=abc", None),
            Some(200),
            "body 再大也不该影响状态判定"
        );
        let _ = writer.join();
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

    /// 同一主段的两个预发布版必须能分出先后。
    ///
    /// 这条是**真缺陷的回归测试**：原实现只记录"有没有预发布后缀"这一个布尔，
    /// 于是 `0.1.5-rc.1` 与 `0.1.5-rc.2` 判成相等 → "检查更新"永远显示已是最新，
    /// 而 dsh 发布的全是预发布版（rc/alpha），等于同主段内的升级全部检测不到。
    #[test]
    fn test_version_cmp_prerelease_ordering() {
        use std::cmp::Ordering;
        // 实际会遇到的形状
        assert_eq!(version_cmp("0.1.5-rc.1", "0.1.5-rc.2"), Some(Ordering::Less));
        assert_eq!(version_cmp("0.1.5-rc.2", "0.1.5-rc.1"), Some(Ordering::Greater));
        assert_eq!(version_cmp("0.1.5-rc.10", "0.1.5-rc.9"), Some(Ordering::Greater), "数字段按数值比，不是字典序");
        // 字母段按字典序：alpha < beta < rc
        assert_eq!(version_cmp("0.1.5-alpha.2", "0.1.5-rc.1"), Some(Ordering::Less));
        assert_eq!(version_cmp("0.1.5-rc.1", "0.1.6-alpha.1"), Some(Ordering::Less), "主段优先于预发布段");
        // 段数不同：1.0.0-rc < 1.0.0-rc.1
        assert_eq!(version_cmp("1.0.0-rc", "1.0.0-rc.1"), Some(Ordering::Less));
        // 数字段 < 字母段（语义化版本规则）
        assert_eq!(version_cmp("1.0.0-1", "1.0.0-alpha"), Some(Ordering::Less));
        // 完全相同
        assert_eq!(version_cmp("0.1.5-rc.2", "0.1.5-rc.2"), Some(Ordering::Equal));
        // 大小写不敏感（同一版本的两种写法不该判成有新旧）
        assert_eq!(version_cmp("0.1.5-RC.2", "0.1.5-rc.2"), Some(Ordering::Equal));
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

    /// URL 解析的边界形状：解析结果直接进子 WebView 的导航目标与就绪探测，
    /// 解析错就是"面板打开一个不存在的页面"，而解析不出只是退化成"启动失败"——宁可后者。
    #[test]
    fn test_parse_web_url_edge_shapes() {
        // 只改写 127.0.0.1；其它主机（含 https）原样保留
        assert_eq!(
            parse_web_url("https://dsh.example.com:8443/?token=t"),
            Some("https://dsh.example.com:8443/?token=t".to_string())
        );
        // 前后空白（dsh 的启动行可能带缩进）
        assert_eq!(
            parse_web_url("  http://127.0.0.1:8080/?token=t  "),
            Some("http://localhost:8080/?token=t".to_string())
        );
        // scheme 大小写敏感：大写不是 dsh 的输出形状，不去猜
        assert_eq!(parse_web_url("HTTP://127.0.0.1:8080/"), None);
        // 只有 host:port 没有 scheme：不猜（猜错会导航到相对地址）
        assert_eq!(parse_web_url("127.0.0.1:8080/?token=t"), None);
        assert_eq!(parse_web_url("   "), None);
    }

    /// 端口/token 提取的边界：拿不到就是 None（调用方据此跳过就绪探测，
    /// 而不是拿一个猜出来的端口去连本机别的服务）
    #[test]
    fn test_parse_port_and_token_edge_shapes() {
        assert_eq!(parse_port("http://localhost:65535/?token=t"), Some(65535));
        assert_eq!(parse_port("http://localhost:8080/x?token=t"), Some(8080));
        // 非数字端口 / 缺端口 / 缺 scheme
        assert_eq!(parse_port("http://localhost:not-a-port/"), None);
        assert_eq!(parse_port("http://localhost/?token=a"), None);
        assert_eq!(parse_port("localhost:80/"), None);

        // token 可以不是第一个查询参数
        assert_eq!(parse_token("http://localhost:1/?a=1&token=xyz&b=2"), Some("xyz".to_string()));
        // 空值也是"取到了"（调用方按非空判断要不要探测）
        assert_eq!(parse_token("http://localhost:1/?token="), Some(String::new()));
        assert_eq!(parse_token("http://localhost:1/?a=1"), None);
        assert_eq!(parse_token("http://localhost:1/"), None);
    }

    /// 输出累积必须有上限（②）：命令狂刷输出（npm install、base64/JSON dump）时，
    /// 把每个字节都留在内存里等于把"诊断"变成内存泄漏；保留尾部是因为排障看的是最后几行。
    #[test]
    fn test_capture_reader_keeps_bounded_tail() {
        let mut payload = String::new();
        for i in 0..3000 {
            payload.push_str(&format!("line-{:05}-{}\n", i, "x".repeat(120)));
        }
        payload.push_str("SENTINEL-LAST-LINE\n");
        let total = payload.len();
        assert!(total > 300_000, "测试输入要足够大才能顶到上限（{} 字节）", total);

        let captured = Arc::new(Mutex::new(String::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        crate::core::process::spawn_capture_reader(std::io::Cursor::new(payload.into_bytes()), Arc::clone(&captured), Arc::clone(&stop));

        // 读取线程不返回 join 句柄（生产路径也不回收）：轮询等它把内存输入读完
        let deadline = Instant::now() + Duration::from_secs(20);
        while !captured.lock().map(|g| g.contains("SENTINEL-LAST-LINE")).unwrap_or(false) {
            assert!(Instant::now() < deadline, "读取线程 20 秒内没读完 {} 字节的内存输入", total);
            std::thread::sleep(Duration::from_millis(20));
        }
        let text = captured.lock().unwrap().clone();
        assert!(text.len() <= 6000, "缓冲必须有上限，实际 {} 字节", text.len());
        assert!(text.contains("SENTINEL-LAST-LINE"), "最后一行必须在（诊断靠尾部）");
        assert!(!text.contains("line-00000-"), "最旧的行应已被丢弃");
    }

    // `tail_text` 的用例随函数搬到了 `core::process`（见那里的 test_tail_of_keeps_last_lines_only）

    // ── 超时 / 杀进程树（ARCH-19：这三条路径此前零覆盖）──────────────
    //
    // 起进程一律走 `run_captured` / `spawn_web_command`：它们内部用
    // `build_internal_command`（自带 `NoDefaultCurrentDirectoryInExePath=1`，见 spawn_guard.rs）。

    /// 探针跑的时长（秒）。8 ≈ 7 秒：比任何等待窗口都长，才分得清"超时后立刻返回"与
    /// "等命令自己跑完"（并行跑整套测试时 taskkill 本身可能耗到 ~2 秒）
    #[cfg(windows)]
    const PROBE_PINGS: &str = "8";

    /// 探针进程：把 `ping.exe` 复制成一个**唯一映像名**再跑。
    ///
    /// 为什么要唯一名：`run_captured` 只回错误文本、拿不到 pid，"进程有没有被杀掉"只能从
    /// 进程表读；按 `ping.exe` 查会与本机其它 ping（含 `job_object.rs` 的测试）混在一起，
    /// 唯一名才能把结论钉死。
    ///
    /// 为什么用相对路径 `target\...`：命令串里不能出现引号——`Command` 会给含空格的参数
    /// 整体加引号，而 cmd `/C` 的引号消解规则（两个引号 + 特殊字符 → 剥首尾）会把路径弄坏。
    /// 相对路径不含空格，也就永远不需要引号（CWD 由 cargo 固定为 crate 根）。
    #[cfg(windows)]
    fn spawn_probe() -> (String, String, std::path::PathBuf) {
        let name = format!("nexus-ut-ai-probe-{}.exe", uuid::Uuid::new_v4());
        // 相对 CWD（cargo 把测试进程的 CWD 设成 crate 根）的命令行形式与磁盘位置
        let rel = format!("target\\{}", name);
        let abs = std::path::PathBuf::from(&rel);
        let sys32 = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        std::fs::copy(std::path::Path::new(&sys32).join("System32").join("PING.EXE"), &abs)
            .expect("复制探针可执行文件失败");
        (rel, name, abs)
    }

    /// 进程表里是否还有这个映像名（探针名唯一，命中即是我们自己起的那个）
    #[cfg(windows)]
    fn probe_running(name: &str) -> bool {
        crate::core::winproc::snapshot_processes()
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case(name))
    }

    /// 等探针从进程表消失（杀进程树是异步生效的：taskkill 之后系统回收需要一点时间）
    #[cfg(windows)]
    fn wait_probe_gone(name: &str, why: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while probe_running(name) {
            assert!(Instant::now() < deadline, "{}：探针进程仍在运行", why);
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// 超时路径（③）：进程迟迟不退出 → 杀进程树 + 返回**可读**错误。
    ///
    /// 探针跑约 7 秒（`ping -n 8`）、超时 300ms：若实现退回"等进程自然结束"，
    /// 耗时断言与"进程已消失"断言都会红。错误文本三件套（是哪条命令 / 为什么失败 /
    /// 已终止）缺一不可——用户看到的只有这一行。
    /// 耗时上限给到 4 秒：杀进程树是 `taskkill` 子进程 + 最多 2 秒的收尸宽限，
    /// 并行跑整套测试时实测能到 2.2 秒（不是"等命令跑完"——那是 7 秒级）。
    #[cfg(windows)]
    #[test]
    fn test_run_captured_timeout_kills_process_tree() {
        let (rel, name, abs) = spawn_probe();
        let cmd = format!("{} -n {} 127.0.0.1", rel, PROBE_PINGS);
        let started = Instant::now();
        let err = run_captured(None, &cmd, Duration::from_millis(300)).expect_err("超时必须返回错误");
        let elapsed = started.elapsed();

        assert!(err.contains("命令超时"), "错误要说清是超时: {}", err);
        assert!(err.contains(&cmd), "错误要带原命令（否则排障不知道卡在哪条）: {}", err);
        assert!(err.contains("已终止"), "错误要说清进程已被终止: {}", err);
        assert!(
            elapsed < Duration::from_secs(4),
            "超时后应立刻返回，不该等命令自然跑完（{:?}）",
            elapsed
        );
        wait_probe_gone(&name, "超时路径没有把进程树杀干净");
        let _ = std::fs::remove_file(&abs);
    }

    /// 成功路径：stdout 里的启动 URL 被解析出来（127.0.0.1 → localhost），会话字段齐备。
    ///
    /// 用 `cmd` 假冒 dsh：生产命令是写死的 `dsh web --no-open --port 0`，只有测试能替换
    /// （见 `spawn_web_command` 的说明）。cwd 传 `target`、探针用 `.\` 开头的相对路径——
    /// 顺便证明 `current_dir` 确实传给了子进程（裸文件名不行：收口用的
    /// `NoDefaultCurrentDirectoryInExePath=1` 会让 cmd 跳过当前目录，见 spawn_guard.rs）。
    #[cfg(windows)]
    #[test]
    fn test_spawn_web_command_parses_url_and_reports_session() {
        let (_, name, abs) = spawn_probe();
        // 绑定再释放拿到一个"没人监听"的端口：就绪探测会立刻连接失败并放弃（不会白等 8 秒）
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("取空闲端口")
            .local_addr()
            .expect("读端口")
            .port();
        // 先打印 URL（stdout 解析），再用探针拖住进程（约 7 秒）验证 alive/stop
        let cmd = format!(
            "echo http://127.0.0.1:{}/?token=ut-token & .\\{} -n {} 127.0.0.1 > NUL",
            port, name, PROBE_PINGS
        );
        let mut session = spawn_web_command(None, crate::core::process::build_internal_command(&cmd), Some("target"), Duration::from_secs(10))
            .expect("应解析到启动 URL");

        assert_eq!(session.url, format!("http://localhost:{}/?token=ut-token", port));
        assert!(session.pid > 0, "会话必须带 pid（状态面板要显示）");
        assert_eq!(session.cwd.as_deref(), Some("target"), "cwd 应原样带回（前端据此判断归属项目）");
        assert!(session.alive(), "URL 打印后进程仍在跑（探针 ~7 秒），不该被误判为已退出");

        session.stop();
        assert!(!session.alive(), "stop 之后进程应已退出");
        let _ = std::fs::remove_file(&abs);
    }

    /// 失败路径（进程提前退出）：不能只说"启动失败"——stderr 的原话必须带回来，
    /// 否则用户和我们都只能猜（"dsh 没装"与"端口被占"在界面上长得一样）。
    #[cfg(windows)]
    #[test]
    fn test_spawn_web_command_reports_stderr_when_exited_without_url() {
        let (rel, _, abs) = spawn_probe();
        // 先写 stderr，再用探针拖 1 秒才退出：stderr 尾部是异步线程写的，
        // 进程立刻退出会让"读到没有"变成掷骰子（生产里同样存在这个窗口）
        let cmd = format!("echo dsh-not-installed 1>&2 & {} -n 2 127.0.0.1 > NUL", rel);
        let err = match spawn_web_command(None, crate::core::process::build_internal_command(&cmd), None, Duration::from_secs(10)) {
            Err(e) => e,
            Ok(s) => panic!("没打印 URL 就必须失败（实际拿到 url={} pid={}）", s.url, s.pid),
        };
        assert!(err.contains("进程提前退出"), "错误要说清是哪种失败: {}", err);
        assert!(
            err.contains("dsh-not-installed"),
            "stderr 的原话要带回来（否则用户只能猜）: {}",
            err
        );
        let _ = std::fs::remove_file(&abs);
    }

    /// 超时路径（URL 一直不出现）：同样要杀进程树——否则每次"打开面板"卡住都会留一个
    /// 跑着的假 dsh，端口与内存都收不回来。
    #[cfg(windows)]
    #[test]
    fn test_spawn_web_command_timeout_kills_process_tree() {
        let (rel, name, abs) = spawn_probe();
        let cmd = format!("{} -n {} 127.0.0.1 > NUL", rel, PROBE_PINGS);
        let started = Instant::now();
        let err = match spawn_web_command(None, crate::core::process::build_internal_command(&cmd), None, Duration::from_millis(300)) {
            Err(e) => e,
            Ok(s) => panic!("超时必须失败（实际拿到 url={} pid={}）", s.url, s.pid),
        };
        assert!(err.contains("等待 dsh web 启动超时"), "错误要说清是启动超时: {}", err);
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "超时应立刻返回，而不是等 60 秒的启动窗口（{:?}）",
            started.elapsed()
        );
        wait_probe_gone(&name, "启动超时路径没有把进程树杀干净");
        let _ = std::fs::remove_file(&abs);
    }
}
