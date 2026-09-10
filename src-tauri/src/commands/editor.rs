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

/// 检查路径是否在允许范围内（search 等模块复用）
pub(crate) fn is_path_allowed(requested: &str, allowed_root: &Option<String>) -> bool {
    if let Some(root) = allowed_root {
        // 使用 canonicalize 处理符号链接、.. 等
        let canon_req = match std::fs::canonicalize(requested) {
            Ok(p) => p,
            Err(_) => return false, // 路径不存在或无法解析，拒绝访问
        };
        let canon_root = match std::fs::canonicalize(root) {
            Ok(p) => p,
            Err(_) => return false, // 根路径无效，拒绝访问
        };
        canon_req.starts_with(&canon_root)
    } else {
        // 未选中项目 → 拒绝访问（防止路径穿越）
        false
    }
}

/// 允许访问的根目录集合：项目根 + 所有已配置服务/模板的工作目录。
///
/// 为什么是多根：`Service.cwd` 允许配置在项目目录之外（模型如此），单根白名单会让这些
/// 服务的文件既打不开也搜不到。多根同时收紧了"任意路径"（原 `list_directory` 等命令无校验）。
/// 返回空集合表示"未选择项目"，此时一律拒绝——保持原有的安全语义。
pub(crate) fn allowed_roots(state: &AppState) -> Vec<String> {
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
    roots.dedup();
    roots
}

/// 多根白名单校验：命中任一允许根即通过
pub(crate) fn is_path_allowed_any(requested: &str, roots: &[String]) -> bool {
    roots
        .iter()
        .any(|root| is_path_allowed(requested, &Some(root.clone())))
}

/// 读路径校验：项目根 + 各服务/模板工作目录（未选择项目时一律拒绝）。
/// 供所有取路径的命令统一调用——此前 `list_directory`/`open_terminal`/`open_in_explorer`
/// 等命令完全绕过白名单，导致"同类危险操作两套口径"。
pub(crate) fn check_path_allowed(state: &AppState, path: &str) -> Result<(), String> {
    let roots = allowed_roots(state);
    if roots.is_empty() {
        return Err("访问被拒绝".into());
    }
    if is_path_allowed_any(path, &roots) {
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
    /// 即可检测"打开后被外部修改"并拒绝静默覆盖（前端尚未接入，字段先就绪）
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
/// expected_modified：调用方若传入打开文件时记录的修改时间（毫秒），写入前会比对磁盘现值，
/// 不一致则拒绝写入并返回"文件已被外部修改"——供前端接入"覆盖/放弃/查看差异"确认（当前前端尚未传）。
#[tauri::command]
pub async fn write_file(
    state: State<'_, AppState>,
    path: String,
    content: String,
    encoding: Option<String>,
    expected_modified: Option<u64>,
) -> Result<(), String> {
    // 目标不存在时按父目录校验（支持新建文件），校验内部不跨 await 持锁
    check_write_path_allowed(&state, &path)?;
    if (content.len() as u64) > MAX_WRITE_SIZE {
        return Err(format!("文件过大（{:.1} MB），超过 10 MB 上限", content.len() as f64 / (1024.0 * 1024.0)));
    }
    if let Some(expected) = expected_modified {
        match tokio::fs::metadata(&path).await.ok().and_then(|m| m.modified().ok()) {
            Some(actual) if file_modified_millis(actual) != expected => {
                return Err("文件已被外部修改，请重新加载后再保存（已取消本次写入）".into());
            }
            // 目标不存在（新建）：无冲突可言
            None => {}
            _ => {}
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
    let p = std::path::Path::new(&path);
    let tmp = p.with_extension(format!("{}.tmp", p.extension().and_then(|e| e.to_str()).unwrap_or("")));
    tokio::fs::write(&tmp, &data).await.map_err(|e| format!("无法写入文件: {}", e))?;
    match tokio::fs::rename(&tmp, p).await {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            Err(format!("无法写入文件: {}", e))
        }
    }
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

    #[test]
    fn test_is_path_allowed_no_root() {
        // 未设置项目根路径 → 拒绝所有访问
        assert!(!is_path_allowed("/any/path", &None));
    }

    #[test]
    fn test_is_path_allowed_nonexistent_path() {
        // 请求的路径不存在 → 拒绝
        assert!(!is_path_allowed("/nonexistent/path/that/does/not/exist", &Some("/tmp".to_string())));
    }

    #[test]
    fn test_is_path_allowed_nonexistent_root() {
        // 根路径不存在 → 拒绝
        assert!(!is_path_allowed("/tmp", &Some("/nonexistent/root".to_string())));
    }

    #[test]
    fn test_is_path_allowed_within_root() {
        // 使用当前目录作为根路径（确保路径存在）
        let cwd = std::env::current_dir().unwrap();
        let root = Some(cwd.to_string_lossy().to_string());
        // 当前目录本身应该被允许
        assert!(is_path_allowed(&cwd.to_string_lossy(), &root));
    }

    #[test]
    fn test_is_path_allowed_outside_root() {
        let cwd = std::env::current_dir().unwrap();
        let root = Some(cwd.to_string_lossy().to_string());
        // 系统根目录不在当前目录下
        #[cfg(windows)]
        assert!(!is_path_allowed("C:\\Windows", &root));
        #[cfg(not(windows))]
        assert!(!is_path_allowed("/etc", &root));
    }

    #[test]
    fn test_is_path_allowed_any_multi_root() {
        let cwd = std::env::current_dir().unwrap();
        let root = cwd.to_string_lossy().to_string();
        let other = std::env::temp_dir().to_string_lossy().to_string();
        let roots = vec![root.clone(), other.clone()];
        // 命中任一允许根即通过（服务 cwd 可以配在项目目录之外）
        assert!(is_path_allowed_any(&root, &roots));
        assert!(is_path_allowed_any(&other, &roots));
        // 未选择项目（空集合）→ 一律拒绝
        assert!(!is_path_allowed_any(&root, &[]));
        #[cfg(windows)]
        assert!(!is_path_allowed_any("C:\\Windows", &roots));
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
