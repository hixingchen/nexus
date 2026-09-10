use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::State;
use crate::AppState;

/// 在系统终端中打开路径（Windows: 新开 cmd 窗口并定位到目录）
///
/// 与文件读写同口径校验路径：此前该命令不校验，可在任意目录弹出终端
#[tauri::command]
pub fn open_terminal(state: State<AppState>, path: String) -> Result<(), String> {
    check_path_allowed(&state, &path)?;
    if !std::path::Path::new(&path).is_dir() {
        return Err("路径不是目录".into());
    }
    log::info!("[nexus] 打开终端: {}", path);

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NEW_CONSOLE(0x10)：强制新控制台窗口，避免继承父进程（pnpm dev 等）的终端
        // current_dir 直接设置工作目录（不经 shell，路径含空格/特殊字符都安全），
        // cmd /K 启动后即停留在该目录，避免 start /D 或 cd /d 的引号嵌套错乱
        std::process::Command::new("cmd")
            .current_dir(&path)
            .creation_flags(0x00000010)
            .arg("/K")
            .spawn()
            .map_err(|e| format!("打开终端失败: {}", e))?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-a").arg("Terminal").arg(&path)
            .spawn()
            .map_err(|e| format!("打开终端失败: {}", e))?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("x-terminal-emulator")
            .arg("--working-directory").arg(&path)
            .spawn()
            .map_err(|e| format!("打开终端失败: {}", e))?;
    }
    Ok(())
}

/// 在系统资源管理器中打开路径
///
/// 与文件读写同口径校验路径：此前该命令不校验，可打开任意路径（含把任意文件路径写进系统剪贴板场景）
#[tauri::command]
pub fn open_in_explorer(state: State<AppState>, path: String) -> Result<(), String> {
    check_path_allowed(&state, &path)?;
    let path_buf = PathBuf::from(&path);

    log::info!("[nexus] 打开资源管理器: {}", path);

    #[cfg(target_os = "windows")]
    {
        // Windows: 直接调用 explorer.exe（不经 cmd，避免 %VAR% 展开和 & | ^ 元字符注入）。
        // 注意：文件场景必须传完整文件路径——/select,<目录> 会让 explorer 打开该目录的父级（少一层）
        let win_path = path.replace('/', "\\");
        let is_dir = path_buf.is_dir();
        let mut cmd = std::process::Command::new("explorer.exe");
        if is_dir {
            cmd.arg(&win_path);
        } else {
            // 打开所在目录并选中该文件（/select, 必须与路径合并为单参数，否则部分环境只打开目录）
            cmd.arg(format!("/select,{}", win_path));
        }
        cmd.spawn()
            .map_err(|e| format!("打开资源管理器失败: {}", e))?;
    }
    #[cfg(target_os = "macos")]
    {
        // mac 没有"选中文件"的等价能力，文件退化为打开所在目录
        let target = if path_buf.is_dir() {
            path.clone()
        } else {
            path_buf.parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| path.clone())
        };
        std::process::Command::new("open")
            .arg(&target)
            .spawn()
            .map_err(|e| format!("打开 Finder 失败: {}", e))?;
    }
    #[cfg(target_os = "linux")]
    {
        // linux 同理，退化为打开所在目录
        let target = if path_buf.is_dir() {
            path.clone()
        } else {
            path_buf.parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| path.clone())
        };
        std::process::Command::new("xdg-open")
            .arg(&target)
            .spawn()
            .map_err(|e| format!("打开文件管理器失败: {}", e))?;
    }
    Ok(())
}

/// 文件信息
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub extension: Option<String>,
}

/// 设置当前项目根路径（安全白名单根之一：另加各服务/模板工作目录，见 allowed_roots）
#[tauri::command]
pub fn set_project_root(state: State<AppState>, path: Option<String>) -> Result<(), String> {
    // 传入路径必须存在且为目录：否则 canonicalize 恒失败 → 所有文件命令被拒（现象与"打不开文件"一致但无解释）。
    // 这里直接给出可诊断的错误，前端已 catch 并记录日志。
    if let Some(root) = path.as_deref() {
        let trimmed = root.trim();
        if trimmed.is_empty() {
            return Err("项目路径为空".into());
        }
        let p = std::path::Path::new(trimmed);
        if !p.is_dir() {
            return Err(format!("项目路径不存在或不是目录: {}", trimmed));
        }
    }
    let mut root = state.project_root.lock().map_err(|e| format!("获取项目根路径锁失败: {}", e))?;
    *root = path;
    Ok(())
}

/// 允许访问的根目录集合（原始字符串，未 canonicalize）：项目根 + 所有已配置服务/模板的工作目录。
///
/// 为什么是多根：`Service.cwd` 允许配置在项目目录之外（模型如此），单根白名单会让这些
/// 服务的文件既打不开也搜不到。多根同时收紧了"任意路径"（原 `list_directory` 等命令无校验）。
/// 返回空集合表示"未选择项目"，此时一律拒绝——保持原有的安全语义。
fn raw_allowed_roots(state: &AppState) -> Vec<String> {
    let mut roots: Vec<String> = Vec::new();
    let has_project_root = match state.project_root.lock() {
        Ok(root) => match root.as_ref() {
            Some(r) if !r.trim().is_empty() => {
                roots.push(r.clone());
                true
            }
            _ => false,
        },
        Err(_) => false,
    };
    if !has_project_root {
        return Vec::new();
    }
    // 服务/模板工作目录：配置可能指向项目外，属合法访问范围
    let dirs = state.db.with_conn(|conn| {
        let mut out: Vec<String> = Vec::new();
        let mut stmt = conn
            .prepare("SELECT cwd FROM services UNION SELECT cwd FROM service_templates")
            .map_err(|e| format!("查询可访问目录失败: {}", e))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| format!("读取可访问目录失败: {}", e))?;
        for r in rows {
            let cwd = r.map_err(|e| format!("解析可访问目录失败: {}", e))?;
            if !cwd.trim().is_empty() {
                out.push(cwd);
            }
        }
        Ok::<Vec<String>, String>(out)
    });
    match dirs {
        Ok(mut v) => roots.append(&mut v),
        Err(e) => log::warn!("[nexus] 收集允许访问目录失败（仅按项目根校验）: {}", e),
    }
    // 真去重（原实现用 `Vec::dedup`，但它只删**相邻**重复项，而这里并未排序 → 实际是 no-op）
    let mut seen = std::collections::HashSet::new();
    roots.retain(|r| seen.insert(r.clone()));
    roots
}

/// 已 canonicalize 的允许根（带缓存）。
///
/// 性能背景：每个取路径的命令都会走到这里，而原实现每次都对**每一个根**做一次
/// `canonicalize`（外加一次对请求路径的 canonicalize）。文件树展开 + 连续打开文件
/// 会把"根数 × 操作数"次文件系统路径解析叠加起来，是这条热路径上的主要开销。
/// 现在：请求路径只解析一次，根只在原始列表变化时重新解析。
pub(crate) fn allowed_root_paths(state: &AppState) -> std::sync::Arc<Vec<std::path::PathBuf>> {
    let raw = raw_allowed_roots(state);
    let mut guard = match state.allowed_roots_cache.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    if let Some((key, cached)) = guard.as_ref() {
        if key == &raw {
            return std::sync::Arc::clone(cached);
        }
    }
    // 无法解析的根直接丢弃（不存在/无权限）——与单根时代的语义一致：那类根恒不命中
    let resolved: Vec<std::path::PathBuf> =
        raw.iter().filter_map(|r| std::fs::canonicalize(r).ok()).collect();
    let arc = std::sync::Arc::new(resolved);
    *guard = Some((raw, std::sync::Arc::clone(&arc)));
    arc
}

/// 请求路径是否位于任一允许根之下。
///
/// 任意一端无法 canonicalize（请求路径不存在、根失效）即判否——fail closed。
/// `PathBuf::starts_with` 按**路径分量**比较，因此 `/root` 不会误命中 `/root-evil`；
/// `..`、UNC、`\\?\`、8.3 短名、结尾点/空格、ADS 都在 canonicalize 阶段被解析掉。
fn canonical_path_within(requested: &str, roots: &[std::path::PathBuf]) -> bool {
    if roots.is_empty() {
        return false;
    }
    match std::fs::canonicalize(requested) {
        Ok(canon) => roots.iter().any(|root| canon.starts_with(root)),
        Err(_) => false,
    }
}

/// 读路径校验：项目根 + 各服务/模板工作目录（未选择项目时一律拒绝）。
/// 供所有取路径的命令统一调用——此前 `list_directory`/`open_terminal`/`open_in_explorer`
/// 等命令完全绕过白名单，导致"同类危险操作两套口径"。
pub(crate) fn check_path_allowed(state: &AppState, path: &str) -> Result<(), String> {
    let roots = allowed_root_paths(state);
    if canonical_path_within(path, &roots) {
        Ok(())
    } else {
        Err("访问被拒绝".into())
    }
}

/// 写路径校验：目标已存在时按读路径规则；不存在时校验**父目录**（支持新建文件）。
/// 文件名必须为单一段（不含分隔符与 `.`/`..`），避免经由父目录校验绕过范围限制。
pub(crate) fn check_write_path_allowed(state: &AppState, path: &str) -> Result<(), String> {
    let p = std::path::Path::new(path);
    if p.exists() {
        return check_path_allowed(state, path);
    }
    let name_ok = p
        .file_name()
        .map(|n| {
            let s = n.to_string_lossy();
            !s.is_empty() && s != "." && s != ".." && !s.contains('/') && !s.contains('\\')
        })
        .unwrap_or(false);
    if !name_ok {
        return Err("访问被拒绝".into());
    }
    match p.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            check_path_allowed(state, &parent.to_string_lossy())
        }
        _ => Err("访问被拒绝".into()),
    }
}

/// 读取文件大小上限：只读查看放宽到 50MB（大 SQL/日志/bundle 偶尔要看）
const MAX_READ_SIZE: u64 = 50 * 1024 * 1024;
/// 写入大小上限保持 10MB（编辑大文件内存压力大，且几乎无编辑需求）
const MAX_WRITE_SIZE: u64 = 10 * 1024 * 1024;

/// 读取文件响应：is_binary 标记二进制文件（前端转交系统默认程序打开）
#[derive(Debug, Serialize)]
pub struct ReadFileResponse {
    pub content: String,
    pub is_binary: bool,
    pub size: u64,
    /// 原始换行风格（保存时按此写回，避免编辑器规范化换行导致 git 误报变更）
    pub line_ending: String,
    /// 原始编码：'utf8' | 'gb18030'（保存时按此编码写回，GBK 文件编辑保存后字节不变）
    pub encoding: String,
    /// 文件修改时间（距 UNIX 纪元的毫秒数）。前端保存时原样回传 write_file 的 expected_modified，
    /// 即可检测"打开后被外部修改"并拒绝静默覆盖
    pub modified: Option<u64>,
}

/// UTF-8 严格解码，失败回退 GB18030（GBK 超集，覆盖 Windows 中文环境老项目的 GBK 文件）。
/// 返回 (解码内容, 编码标识)——保存时按原编码写回，保证字节保真
fn decode_text(bytes: &[u8]) -> (String, &'static str) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), "utf8"),
        Err(_) => (encoding_rs::GB18030.decode(bytes).0.into_owned(), "gb18030"),
    }
}

/// 检测文件原始换行风格：'lf' | 'crlf' | 'cr'（混合取占比最大的）
fn detect_line_ending(bytes: &[u8]) -> &'static str {
    let crlf = bytes.windows(2).filter(|w| w == b"\r\n").count();
    let lf = bytes.iter().filter(|&&b| b == b'\n').count() - crlf;
    let cr = bytes.iter().filter(|&&b| b == b'\r').count() - crlf;
    if crlf == 0 && lf == 0 && cr == 0 {
        "lf" // 无换行（空文件/单行）
    } else if crlf >= lf && crlf >= cr {
        "crlf"
    } else if lf >= cr {
        "lf"
    } else {
        "cr"
    }
}

/// 读取文件内容
#[tauri::command]
pub async fn read_file(state: State<'_, AppState>, path: String) -> Result<ReadFileResponse, String> {
    // 多根白名单校验（项目根 + 各服务/模板工作目录），内部不跨 await 持锁
    check_path_allowed(&state, &path)?;
    let meta = tokio::fs::metadata(&path).await.map_err(|e| format!("无法读取文件: {}", e))?;
    if meta.len() > MAX_READ_SIZE {
        return Err(format!("文件过大（{:.1} MB），超过 50 MB 查看上限", meta.len() as f64 / (1024.0 * 1024.0)));
    }
    let bytes = tokio::fs::read(&path).await.map_err(|e| format!("无法读取文件: {}", e))?;
    // metadata 与 read 之间存在 TOCTOU（文件在两者之间变大）：读入后按实际长度复核一次
    if bytes.len() as u64 > MAX_READ_SIZE {
        return Err(format!("文件过大（{:.1} MB），超过 50 MB 查看上限", bytes.len() as f64 / (1024.0 * 1024.0)));
    }
    let modified = meta.modified().ok().map(file_modified_millis);

    // 二进制嗅探：前 8KB 含 NUL 字节即视为二进制（与 git 同策略），
    // 前端收到 is_binary 后改用系统默认程序打开，不进入编辑器
    let is_binary = bytes.iter().take(8192).any(|&b| b == 0);
    if is_binary {
        return Ok(ReadFileResponse {
            content: String::new(),
            is_binary: true,
            size: meta.len(),
            line_ending: "lf".into(),
            encoding: "utf8".into(),
            modified,
        });
    }

    // 先 UTF-8 严格解码，失败回退 GB18030（GBK 超集，覆盖 Windows 中文环境老项目的 GBK 文件）
    let (content, encoding) = decode_text(&bytes);
    let line_ending = detect_line_ending(&bytes);

    Ok(ReadFileResponse {
        content,
        is_binary: false,
        size: meta.len(),
        line_ending: line_ending.into(),
        encoding: encoding.into(),
        modified,
    })
}

/// 写入文件内容（与 read_file 同款安全校验：多根白名单 + 大小上限）。
/// encoding 指定原编码（'gb18030'）时按原编码编码写回——GBK 文件编辑保存后字节不变，
/// git 不会误报变更（前端按 read_file 返回的 encoding 传入）
///
/// expected_modified：调用方传入打开文件时记录的修改时间（毫秒），写入前比对磁盘现值，
/// 不一致则拒绝写入并返回"文件已被外部修改"（前端据此提示重新加载）。
///
/// 返回值：写入后的新修改时间（毫秒）。**必须回传**——否则前端缓存的仍是打开时的
/// 旧 mtime，连续两次保存的第二次会被自己的上次写入判成"外部修改"（假冲突）。
#[tauri::command]
pub async fn write_file(
    state: State<'_, AppState>,
    path: String,
    content: String,
    encoding: Option<String>,
    expected_modified: Option<u64>,
) -> Result<Option<u64>, String> {
    // 目标不存在时按父目录校验（支持新建文件），校验内部不跨 await 持锁
    check_write_path_allowed(&state, &path)?;
    if (content.len() as u64) > MAX_WRITE_SIZE {
        return Err(format!("文件过大（{:.1} MB），超过 10 MB 上限", content.len() as f64 / (1024.0 * 1024.0)));
    }
    if let Some(expected) = expected_modified {
        match tokio::fs::metadata(&path).await {
            Ok(m) => match m.modified() {
                Ok(actual) if file_modified_millis(actual) != expected => {
                    return Err("文件已被外部修改，请重新加载后再保存（已取消本次写入）".into());
                }
                // modified() 失败：拿不到可比对的时间戳，保守拒绝而不是放行
                Err(e) => return Err(format!("无法读取文件修改时间，已取消写入: {}", e)),
                _ => {}
            },
            // 目标不存在（新建）：无冲突可言。其余 IO 错误保守拒绝——
            // 原实现把 stat 失败与"文件不存在"都压成 None 并照写，会在文件被锁定/
            // 权限不足时静默跳过冲突检测。
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("无法检查文件状态，已取消写入: {}", e)),
        }
    }
    let data: Vec<u8> = match encoding.as_deref() {
        Some("gb18030") => encoding_rs::GB18030.encode(&content).0.into_owned(),
        _ => content.into_bytes(),
    };
    // 原子写：先写同目录临时文件再 rename。直接覆写会在写入中途崩溃/磁盘满时
    // 留下截断的坏文件（大文件整份丢失）。Windows 上 std/tokio 的 rename 会覆盖已存在的目标
    // （MOVEFILE_REPLACE_EXISTING），因此失败分支只清理临时文件——
    // 早期实现先 remove_file(目标) 再 rename，二次失败会直接删掉用户原文件。
    //
    // 临时名带随机后缀：原实现用固定的 `<name>.tmp`，两个并发保存（例如双击 Ctrl+S）
    // 会在同一个临时文件上互相覆盖，必有一次 rename 输掉；预置同名符号链接还会被跟随。
    // 反编译路径早已用 UUID 解决同类问题（core/decompiler.rs），这里对齐。
    let p = std::path::Path::new(&path);
    let tmp = p.with_file_name(format!(
        ".{}.{}.tmp",
        p.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        uuid::Uuid::new_v4()
    ));
    tokio::fs::write(&tmp, &data).await.map_err(|e| format!("无法写入文件: {}", e))?;
    if let Err(e) = tokio::fs::rename(&tmp, p).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(format!("无法写入文件: {}", e));
    }
    // 回读新 mtime 供前端更新其冲突检测基线（读失败不视为写入失败）
    Ok(tokio::fs::metadata(p).await.ok().and_then(|m| m.modified().ok()).map(file_modified_millis))
}

/// 文件修改时间 → 距离 UNIX 纪元的毫秒数（用于保存前的外部修改冲突检测）
fn file_modified_millis(t: std::time::SystemTime) -> u64 {
    t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 受限读取：metadata 预检 + 读入后按实际长度复核（消除 TOCTOU，统一错误文案）。
/// 供图片预览 / 字节码视图 / 反编译 / jar 读取共用，避免四处重复同一段校验。
async fn read_file_limited(path: &str, max: u64, what: &str) -> Result<Vec<u8>, String> {
    let over = |len: u64| {
        format!(
            "{}过大（{:.1} MB），超过 {} MB 上限",
            what,
            len as f64 / (1024.0 * 1024.0),
            max / (1024 * 1024)
        )
    };
    let meta = tokio::fs::metadata(path).await.map_err(|e| format!("无法读取文件: {}", e))?;
    if meta.len() > max {
        return Err(over(meta.len()));
    }
    let bytes = tokio::fs::read(path).await.map_err(|e| format!("无法读取文件: {}", e))?;
    if bytes.len() as u64 > max {
        return Err(over(bytes.len() as u64));
    }
    Ok(bytes)
}

/// 图片预览大小上限（base64 放大 1/3，避免 IPC 传输爆内存）
const MAX_IMAGE_SIZE: u64 = 20 * 1024 * 1024;

/// 读取图片为 base64（内建预览，前端拼 data URL）
#[tauri::command]
pub async fn read_image_data(state: State<'_, AppState>, path: String) -> Result<String, String> {
    // 多根白名单校验（项目根 + 各服务/模板工作目录），内部不跨 await 持锁
    check_path_allowed(&state, &path)?;
    let bytes = read_file_limited(&path, MAX_IMAGE_SIZE, "图片").await?;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    Ok(STANDARD.encode(&bytes))
}

/// hex 视图分页读取响应：一页 rows 行 × 16 字节
/// Tauri 只把命令参数 camelCase→snake_case，返回值字段保持原样，需显式转换
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HexPage {
    pub offset: u64,
    pub bytes: Vec<u8>,
    pub total_size: u64,
}

/// 分页读取二进制内容（hex 视图按需加载，大文件不整体进内存/IPC）
#[tauri::command]
pub async fn read_hex_page(state: State<'_, AppState>, path: String, offset: u64, rows: u32) -> Result<HexPage, String> {
    // 多根白名单校验（项目根 + 各服务/模板工作目录），内部不跨 await 持锁
    check_path_allowed(&state, &path)?;
    let rows = rows.clamp(1, 1024);
    let mut file = tokio::fs::File::open(&path).await.map_err(|e| format!("无法读取文件: {}", e))?;
    let total_size = file.metadata().await.map_err(|e| format!("无法读取文件: {}", e))?.len();
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    file.seek(std::io::SeekFrom::Start(offset)).await.map_err(|e| format!("无法读取文件: {}", e))?;
    let mut bytes = vec![0u8; rows as usize * 16];
    let n = file.read(&mut bytes).await.map_err(|e| format!("无法读取文件: {}", e))?;
    bytes.truncate(n);
    Ok(HexPage { offset, bytes, total_size })
}

/// 读取 .class 文件为字节码视图文本（javap 风格，只读）
#[tauri::command]
pub async fn read_class_file(state: State<'_, AppState>, path: String) -> Result<String, String> {
    // 多根白名单校验（项目根 + 各服务/模板工作目录），内部不跨 await 持锁
    check_path_allowed(&state, &path)?;
    let bytes = read_file_limited(&path, MAX_READ_SIZE, "文件").await?;
    crate::core::classfile::disassemble_class(&bytes)
}

/// 反编译 .class 文件为 Java 源码（捆绑 CFR，失败由前端回退字节码视图）
#[tauri::command]
pub async fn decompile_class(state: State<'_, AppState>, path: String) -> Result<String, String> {
    // 多根白名单校验（项目根 + 各服务/模板工作目录），内部不跨 await 持锁
    check_path_allowed(&state, &path)?;
    let bytes = read_file_limited(&path, MAX_READ_SIZE, "文件").await?;
    crate::core::decompiler::decompile_class_bytes(&bytes).await
}

/// 列 jar 条目（nested 为嵌套 jar 条目链，支持 Spring Boot fat jar）
#[tauri::command]
pub async fn list_jar(
    state: State<'_, AppState>,
    path: String,
    nested: Vec<String>,
) -> Result<Vec<crate::core::jarfile::JarEntryInfo>, String> {
    // 多根白名单校验（项目根 + 各服务/模板工作目录），内部不跨 await 持锁
    check_path_allowed(&state, &path)?;
    let bytes = read_file_limited(&path, crate::core::jarfile::MAX_JAR_SIZE, "jar").await?;
    let inner = crate::core::jarfile::innermost_archive(&bytes, &nested)?;
    crate::core::jarfile::list_entries(&inner)
}

/// jar 条目读取响应：kind = text（已解码）/ class（已反编译）/ binary（base64）
#[derive(Debug, Serialize)]
pub struct JarEntryContent {
    pub content: String,
    pub kind: String,
    pub size: u64,
}

/// 读取 jar 条目内容
#[tauri::command]
pub async fn read_jar_entry(
    state: State<'_, AppState>,
    path: String,
    nested: Vec<String>,
    name: String,
) -> Result<JarEntryContent, String> {
    // 多根白名单校验（项目根 + 各服务/模板工作目录），内部不跨 await 持锁
    check_path_allowed(&state, &path)?;
    let bytes = read_file_limited(&path, crate::core::jarfile::MAX_JAR_SIZE, "jar").await?;
    let entry = crate::core::jarfile::read_entry(&bytes, &nested, &name)?;

    if name.to_ascii_lowercase().ends_with(".class") {
        // class：CFR 反编译，失败回退字节码视图（与 readClassFile 前端回退同策略）
        let content = match crate::core::decompiler::decompile_class_bytes(&entry).await {
            Ok(src) => src,
            Err(e) => {
                log::warn!("[nexus] jar 内 class 反编译失败，回退字节码视图: {}", e);
                crate::core::classfile::disassemble_class(&entry)?
            }
        };
        Ok(JarEntryContent { content, kind: "class".into(), size: entry.len() as u64 })
    } else if entry.iter().take(8192).any(|&b| b == 0) {
        // 二进制：base64（前端 hex 视图内存模式渲染）
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        Ok(JarEntryContent { content: STANDARD.encode(&entry), kind: "binary".into(), size: entry.len() as u64 })
    } else {
        Ok(JarEntryContent { content: decode_text(&entry).0, kind: "text".into(), size: entry.len() as u64 })
    }
}

/// 目录条目数上限：超大目录一次性进内存/IPC 会拖垮渲染，超限直接报错而非静默截断
const MAX_DIR_ENTRIES: usize = 20_000;

/// 列出目录内容（受多根白名单约束：项目根 + 各服务/模板工作目录）
///
/// 历史：本命令曾以"FilePicker 需要浏览整个文件系统"为由不限制读取范围，但目录选择现已改用
/// 系统原生对话框（`@tauri-apps/plugin-dialog`），该豁免失去依据，于是任意路径的目录结构与
/// 文件名可被枚举（而 `read_file` 有白名单）——同类危险操作两套口径。现已统一走白名单。
#[tauri::command]
pub async fn list_directory(state: State<'_, AppState>, path: String) -> Result<Vec<FileEntry>, String> {
    check_path_allowed(&state, &path)?;
    let dir = PathBuf::from(&path);
    if !dir.is_dir() {
        return Err("路径不是目录".to_string());
    }

    let mut entries = vec![];
    let mut read_dir = tokio::fs::read_dir(&dir).await.map_err(|e| format!("读取目录失败: {}", e))?;

    while let Some(entry) = read_dir.next_entry().await.map_err(|e| format!("读取条目失败: {}", e))? {
        if entries.len() >= MAX_DIR_ENTRIES {
            return Err(format!("目录条目过多（超过 {} 条），请改用更具体的目录", MAX_DIR_ENTRIES));
        }
        let metadata = entry.metadata().await.map_err(|e| format!("读取元数据失败: {}", e))?;
        let name = entry.file_name().to_string_lossy().to_string();

        entries.push(FileEntry {
            name,
            path: entry.path().to_string_lossy().replace('\\', "/"),
            is_dir: metadata.is_dir(),
            size: metadata.len(),
            extension: entry
                .path()
                .extension()
                .map(|e| e.to_string_lossy().to_string()),
        });
    }

    // 目录在前，文件在后，按名称排序
    entries.sort_by(|a, b| {
        if a.is_dir == b.is_dir {
            a.name.to_lowercase().cmp(&b.name.to_lowercase())
        } else if a.is_dir {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        }
    });

    Ok(entries)
}

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试辅助：把若干根字符串 canonicalize 成与生产同形的 `Vec<PathBuf>`
    fn roots_of(paths: &[&std::path::Path]) -> Vec<std::path::PathBuf> {
        paths.iter().filter_map(|p| std::fs::canonicalize(p).ok()).collect()
    }

    #[test]
    fn test_path_check_denies_without_roots() {
        // 未设置项目根路径（空根集合）→ 一律拒绝
        let cwd = std::env::current_dir().unwrap();
        assert!(!canonical_path_within(&cwd.to_string_lossy(), &[]));
    }

    #[test]
    fn test_path_check_denies_nonexistent_path() {
        // 请求的路径不存在（canonicalize 失败）→ 拒绝
        let cwd = std::env::current_dir().unwrap();
        let roots = roots_of(&[&cwd]);
        assert!(!canonical_path_within("/nonexistent/path/that/does/not/exist", &roots));
    }

    #[test]
    fn test_path_check_denies_when_root_gone() {
        // 根不存在 → canonicalize 失败 → 空根集合 → 拒绝
        let roots = roots_of(&[std::path::Path::new("/nonexistent/root/for/nexus/test")]);
        assert!(roots.is_empty());
        assert!(!canonical_path_within("/tmp", &roots));
    }

    #[test]
    fn test_path_check_allows_within_root() {
        let cwd = std::env::current_dir().unwrap();
        let roots = roots_of(&[&cwd]);
        assert!(canonical_path_within(&cwd.to_string_lossy(), &roots));
    }

    #[test]
    fn test_path_check_denies_outside_root() {
        let cwd = std::env::current_dir().unwrap();
        let roots = roots_of(&[&cwd]);
        #[cfg(windows)]
        assert!(!canonical_path_within("C:\\Windows", &roots));
        #[cfg(not(windows))]
        assert!(!canonical_path_within("/etc", &roots));
    }

    /// 多根：命中任一即通过（服务 cwd 可以配在项目目录之外）
    #[test]
    fn test_path_check_multi_root() {
        let cwd = std::env::current_dir().unwrap();
        let other = std::env::temp_dir();
        let roots = roots_of(&[&cwd, &other]);
        assert!(canonical_path_within(&cwd.to_string_lossy(), &roots));
        assert!(canonical_path_within(&other.to_string_lossy(), &roots));
        #[cfg(windows)]
        assert!(!canonical_path_within("C:\\Windows", &roots));
    }

    /// 安全关键属性：按**路径分量**比较，兄弟目录不能靠共享字符串前缀混进来
    #[test]
    fn test_path_check_sibling_prefix_not_matched() {
        let base = std::env::temp_dir().join(format!("nexus_ut_prefix_{}", std::process::id()));
        let inside = base.join("proj");
        let sibling = base.join("proj-evil");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let roots = roots_of(&[&inside]);
        assert!(canonical_path_within(&inside.to_string_lossy(), &roots), "根本身应通过");
        assert!(
            !canonical_path_within(&sibling.to_string_lossy(), &roots),
            "`proj-evil` 与 `proj` 共享字符串前缀，但按分量比较必须拒绝"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 允许根缓存：相同原始列表复用已解析结果，列表变化即失效
    #[test]
    fn test_allowed_roots_cache_key_changes_with_input() {
        // 直接验证缓存键的比较语义（AppState 需要 DB，构造成本高，故只测键相等性）
        let a = vec!["/a".to_string(), "/b".to_string()];
        let b = vec!["/a".to_string(), "/b".to_string()];
        let c = vec!["/a".to_string(), "/c".to_string()];
        assert_eq!(a, b, "相同列表应命中缓存");
        assert_ne!(a, c, "列表变化必须让缓存失效");
    }

    #[test]
    fn test_detect_line_ending() {
        assert_eq!(detect_line_ending(b""), "lf"); // 空文件/单行默认 lf
        assert_eq!(detect_line_ending(b"a\nb\n"), "lf");
        assert_eq!(detect_line_ending(b"a\r\nb\r\n"), "crlf");
        assert_eq!(detect_line_ending(b"a\rb\r"), "cr");
        // 混合：取占比最大者
        assert_eq!(detect_line_ending(b"a\r\nb\r\nc\n"), "crlf");
    }
}
