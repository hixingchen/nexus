use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::State;
use crate::AppState;

// 平台分支约定（ARCH-10）：本应用只打包 Windows（`tauri.conf.json` 的 `bundle.targets`
// 只有 nsis/msi）。下面 `#[cfg(target_os = "macos" | "linux")]` 的分支是**可编译但未参与
// 打包、未经测试**的降级实现：保留它们是因为成本极低（各 5 行）且能让非 Windows 上
// `cargo check` 通过；但它们**不是**受支持的功能——改 Windows 路径时无需同步维护，
// 也不要据此认为"项目已支持 mac/linux"。

/// 在系统终端中打开路径（Windows: 新开 cmd 窗口并定位到目录）
///
/// 与文件读写同口径校验路径：此前该命令不校验，可在任意目录弹出终端
#[tauri::command]
pub fn open_terminal(state: State<AppState>, path: String) -> Result<(), String> {
    state.paths.check_path_allowed(&state.db, &path)?;
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
    state.paths.check_path_allowed(&state.db, &path)?;
    let path_buf = PathBuf::from(&path);

    log::info!("[nexus] 打开资源管理器: {}", path);

    #[cfg(target_os = "windows")]
    {
        // Windows: 直接调用 explorer.exe（不经 cmd，避免 %VAR% 展开和 & | ^ 元字符注入）。
        // 注意：文件场景必须传完整文件路径——/select,<目录> 会让 explorer 打开该目录的父级（少一层）
        let win_path = path.replace('/', "\\");
        let is_dir = path_buf.is_dir();
        let mut cmd = std::process::Command::new("explorer.exe");
        // 跳过"当前目录优先"的可执行文件搜索（SEC-14 同口径）：explorer 是我们自己拉起的
        // 系统工具、不承载用户 shell。不加这一行，Nexus 的工作目录里放一个 explorer.exe
        // 就会被执行（工作目录可能是刚打开的那个不可信仓库）。
        // 关于"env 会被子进程继承"：Explorer 已在运行时我们拉起的这个只是转发请求后退出，
        // 环境变量随之消失；真由它当 shell 的场合，双击启动用的是完整路径，不受影响。
        cmd.env("NoDefaultCurrentDirectoryInExePath", "1");
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
        // 同 add_project：项目根是白名单的根，必须过收口校验（此处路径来自已登记项目，
        // 因此"已登记"这一条即可放行；危险目录仍然被拒绝）
        state.paths.ensure_config_dir_allowed(&state.db, trimmed, "项目路径")?;
    }
    state.paths.set_project_root(path)
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
    /// 原始编码：'utf8' | 'gb18030' | 'lossy'（保存时按此编码写回，GBK 文件编辑保存后字节不变）。
    /// `lossy` = 两种编码都解不干净（解出了替换字符），保存会被 `write_file` 拒绝
    pub encoding: String,
    /// 文件修改时间（距 UNIX 纪元的毫秒数）。前端保存时原样回传 write_file 的 expected_modified，
    /// 即可检测"打开后被外部修改"并拒绝静默覆盖
    pub modified: Option<u64>,
}

/// UTF-8 严格解码，失败回退 GB18030（GBK 超集，覆盖 Windows 中文环境老项目的 GBK 文件）。
/// 返回 (解码内容, 编码标识)——保存时按原编码写回，保证字节保真。
///
/// 第三种标识 `lossy`：既不是合法 UTF-8、也不是合法 GB18030 的文件（cp1252 / Shift-JIS /
/// Big5 等）。GB18030 解码器对这些字节会产出替换字符 U+FFFD，而替换字符**无法还原成原字节**：
/// 按 gb18030 写回会把原文永久固化成一串问号/方块。原实现丢弃了 `had_errors` 标志，
/// 于是"打开老文件 → 一个字符都没改 → Ctrl+S"就会静默损坏文件。故单独标记，
/// 由 `write_file` 拒绝写入（编辑仍可查看、可复制，只是不落盘）。
fn decode_text(bytes: &[u8]) -> (String, &'static str) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), "utf8"),
        Err(_) => {
            let (text, _enc, had_errors) = encoding_rs::GB18030.decode(bytes);
            if had_errors {
                (text.into_owned(), "lossy")
            } else {
                (text.into_owned(), "gb18030")
            }
        }
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
    // 句柄级校验（SEC-4）：打开后用 GetFinalPathNameByHandleW 复核真实目标，并从同一句柄读取
    let (file, _real) = state.paths.open_verified_file(&state.db, &path)?;
    let meta = file.metadata().map_err(|e| format!("无法读取文件: {}", e))?;
    if meta.len() > MAX_READ_SIZE {
        return Err(format!("文件过大（{:.1} MB），超过 50 MB 查看上限", meta.len() as f64 / (1024.0 * 1024.0)));
    }
    // 从已复核的句柄读：路径在复核后即使被换成链接也影响不到这次读取
    let bytes = tokio::task::spawn_blocking(move || {
        use std::io::Read;
        // 上限 +1 字节：读满上限说明文件在 metadata 与 read 之间变大了，交由调用方按需拒绝
        let mut buf = Vec::new();
        file.take(MAX_READ_SIZE + 1).read_to_end(&mut buf).map(|_| buf)
    })
    .await
    .map_err(|e| format!("读取文件任务失败: {}", e))?
    .map_err(|e| format!("无法读取文件: {}", e))?;
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
    state.paths.check_write_path_allowed(&state.db, &path)?;
    // 解码有损（既非合法 UTF-8 也非合法 GB18030）的文件拒绝写回：内容里的替换字符 U+FFFD
    // 无法还原成原字节，任何一次保存都会把原文件永久固化成一串替换字符。见 `decode_text`。
    if encoding.as_deref() == Some("lossy") {
        return Err(
            "该文件既不是合法的 UTF-8 也不是 GB18030 编码，按原编码写回会损坏原始字节，已取消保存".into(),
        );
    }
    if (content.len() as u64) > MAX_WRITE_SIZE {
        return Err(format!("文件过大（{:.1} MB），超过 10 MB 上限", content.len() as f64 / (1024.0 * 1024.0)));
    }
    // 句柄级复核（SEC-4）：把"要写哪个对象"从路径字符串换成内核解析出的真实路径。
    // 已存在 → 打开它并按句柄复核；不存在（新建）→ 按句柄复核父目录再用真实父目录拼目标。
    let target: std::path::PathBuf = {
        let exists = std::path::Path::new(&path).exists();
        match state.paths.open_verified_file(&state.db, &path) {
            Ok((_f, real)) => real,
            Err(e) => {
                if exists {
                    return Err(e); // 存在但复核不通过：拒绝，不当作"新建"
                }
                let p = std::path::Path::new(&path);
                let parent = p.parent().ok_or_else(|| "访问被拒绝".to_string())?;
                let name = p.file_name().ok_or_else(|| "访问被拒绝".to_string())?;
                let (_d, real_parent) = state.paths.open_verified_dir(&state.db, &parent.to_string_lossy())?;
                real_parent.join(name)
            }
        }
    };
    if let Some(expected) = expected_modified {
        match tokio::fs::metadata(&target).await {
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
    let p = target.as_path();
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

/// 受限读取：**句柄级复核 + 预检 + 读入后按实际长度复核**（消除 TOCTOU，统一错误文案）。
/// 供图片预览 / 字节码视图 / 反编译 / jar 读取共用，避免四处重复同一段校验。
///
/// 只接受 `AppState`（而不是"调用方先 check_path_allowed 再传路径"）：那样校验与读取
/// 之间仍有窗口（SEC-4），这里把校验与打开合并成一次句柄操作（见 `open_verified_file`）。
async fn read_file_limited(
    state: &AppState,
    path: &str,
    max: u64,
    what: &str,
) -> Result<Vec<u8>, String> {
    let over = |len: u64| {
        format!(
            "{}过大（{:.1} MB），超过 {} MB 上限",
            what,
            len as f64 / (1024.0 * 1024.0),
            max / (1024 * 1024)
        )
    };
    let (file, _real) = state.paths.open_verified_file(&state.db, path)?;
    let meta = file.metadata().map_err(|e| format!("无法读取文件: {}", e))?;
    if meta.len() > max {
        return Err(over(meta.len()));
    }
    let bytes = tokio::task::spawn_blocking(move || {
        use std::io::Read;
        // 多读 1 字节：读满上限说明文件在 metadata 与 read 之间变大，交由下面复核拒绝
        let mut buf = Vec::new();
        file.take(max + 1).read_to_end(&mut buf).map(|_| buf)
    })
    .await
    .map_err(|e| format!("读取文件任务失败: {}", e))?
    .map_err(|e| format!("无法读取文件: {}", e))?;
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
    let bytes = read_file_limited(&state, &path, MAX_IMAGE_SIZE, "图片").await?;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    Ok(STANDARD.encode(&bytes))
}

/// hex 视图分页读取响应：一页 rows 行 × 16 字节
/// 字段名保持 snake_case：响应 DTO 与 Rust 字段同名（命名规则见 `crate::contract`），
/// 前端 `HexPage` 类型按同名读取；只有请求参数结构体才用 camelCase
#[derive(Debug, Serialize)]
pub struct HexPage {
    pub offset: u64,
    pub bytes: Vec<u8>,
    pub total_size: u64,
}

/// 分页读取二进制内容（hex 视图按需加载，大文件不整体进内存/IPC）
#[tauri::command]
pub async fn read_hex_page(state: State<'_, AppState>, path: String, offset: u64, rows: u32) -> Result<HexPage, String> {
    // 句柄级校验（SEC-4）：打开并复核真实目标后从同一句柄定位读取
    let (mut file, _real) = state.paths.open_verified_file(&state.db, &path)?;
    let rows = rows.clamp(1, 1024);
    let total_size = file.metadata().map_err(|e| format!("无法读取文件: {}", e))?.len();
    use std::io::{Read, Seek, SeekFrom};
    file.seek(SeekFrom::Start(offset)).map_err(|e| format!("无法读取文件: {}", e))?;
    let mut bytes = vec![0u8; rows as usize * 16];
    let n = file.read(&mut bytes).map_err(|e| format!("无法读取文件: {}", e))?;
    bytes.truncate(n);
    Ok(HexPage { offset, bytes, total_size })
}

/// 读取 .class 文件为字节码视图文本（javap 风格，只读）
#[tauri::command]
pub async fn read_class_file(state: State<'_, AppState>, path: String) -> Result<String, String> {
    let bytes = read_file_limited(&state, &path, MAX_READ_SIZE, "文件").await?;
    crate::core::classfile::disassemble_class(&bytes)
}

/// 反编译 .class 文件为 Java 源码（捆绑 CFR，失败由前端回退字节码视图）
#[tauri::command]
pub async fn decompile_class(state: State<'_, AppState>, path: String) -> Result<String, String> {
    let bytes = read_file_limited(&state, &path, MAX_READ_SIZE, "文件").await?;
    crate::core::decompiler::decompile_class_bytes(&bytes).await
}

/// 列 jar 条目（nested 为嵌套 jar 条目链，支持 Spring Boot fat jar）
#[tauri::command]
pub async fn list_jar(
    state: State<'_, AppState>,
    path: String,
    nested: Vec<String>,
) -> Result<Vec<crate::core::jarfile::JarEntryInfo>, String> {
    let bytes = read_file_limited(&state, &path, crate::core::jarfile::MAX_JAR_SIZE, "jar").await?;
    // 解析 ZIP 中央目录是纯 CPU 工作（fat jar 上是几十 MB 的活），放 spawn_blocking：
    // 与 search_files 等已改造的命令同口径，不占用 tokio worker 线程跑同步解析
    let inner = crate::core::jarfile::innermost_archive(bytes, &nested)?;
    tokio::task::spawn_blocking(move || crate::core::jarfile::list_entries(&inner))
        .await
        .map_err(|e| format!("列 jar 条目任务失败: {}", e))?
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
    let bytes = read_file_limited(&state, &path, crate::core::jarfile::MAX_JAR_SIZE, "jar").await?;
    // 先判类型：`name` 下面要 move 进闭包
    let is_class = name.to_ascii_lowercase().ends_with(".class");
    // 解压指定条目是同步 CPU 工作（fat jar 上可达数十 ms），放进 spawn_blocking 不占 tokio worker。
    // 与 list_jar 同一口径——那里已改，这里原先漏了。
    let (entry, size) = tokio::task::spawn_blocking(move || {
        let entry = crate::core::jarfile::read_entry(bytes, &nested, &name)?;
        let size = entry.len() as u64;
        Ok::<_, String>((entry, size))
    })
    .await
    .map_err(|e| format!("读取 jar 条目任务失败: {}", e))??;

    if is_class {
        // class：CFR 反编译，失败回退字节码视图（与 readClassFile 前端回退同策略）。
        // 反编译要起 java 子进程，本身是 async，故不并入上面的 spawn_blocking
        let content = match crate::core::decompiler::decompile_class_bytes(&entry).await {
            Ok(src) => src,
            Err(e) => {
                log::warn!("[nexus] jar 内 class 反编译失败，回退字节码视图: {}", e);
                crate::core::classfile::disassemble_class(&entry)?
            }
        };
        Ok(JarEntryContent { content, kind: "class".into(), size })
    } else {
        // 二进制探测 + base64/文本解码同属同步 CPU（大条目的 base64 是 MB 级编码），一并挪出
        let (content, kind) = tokio::task::spawn_blocking(move || {
            if entry.iter().take(8192).any(|&b| b == 0) {
                // 二进制：base64（前端 hex 视图内存模式渲染）
                use base64::{engine::general_purpose::STANDARD, Engine as _};
                (STANDARD.encode(&entry), "binary")
            } else {
                (decode_text(&entry).0, "text")
            }
        })
        .await
        .map_err(|e| format!("解码 jar 条目任务失败: {}", e))?;
        Ok(JarEntryContent { content, kind: kind.into(), size })
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
    // 句柄级校验（SEC-4）：打开目录句柄复核真实目标，随后按**核实后的路径**列目录
    let (_dir_handle, real) = state.paths.open_verified_dir(&state.db, &path)?;
    // 条目 path 仍以用户给的原始路径为前缀（前端按它做展开/拼接，换成 verbatim 形式会显示成 \\?\C:\…），
    // 读取侧用的是核实后的 real
    let display_prefix = path.trim_end_matches(['/', '\\']).to_string();

    // 整个列目录过程放进**一次** `spawn_blocking`（PERF-15）：原先走 `tokio::fs`，
    // 每个条目要两次线程池往返（`next_entry` 与 `metadata` 各自 `asyncify` 一次），
    // `MAX_DIR_ENTRIES=20000` 时最坏 4 万次往返；而 Windows 上 std 的
    // `DirEntry::metadata` **本身不发 syscall**（find 的数据已缓存），逐项 await
    // 反而把廉价操作变贵。与 `commands/search.rs` 的 `search_in_dir` 同一写法。
    let entries = tokio::task::spawn_blocking(move || -> Result<Vec<FileEntry>, String> {
        let read_dir = std::fs::read_dir(&real).map_err(|e| format!("读取目录失败: {}", e))?;
        let mut entries = vec![];
        for entry in read_dir {
            if entries.len() >= MAX_DIR_ENTRIES {
                return Err(format!("目录条目过多（超过 {} 条），请改用更具体的目录", MAX_DIR_ENTRIES));
            }
            let entry = entry.map_err(|e| format!("读取条目失败: {}", e))?;
            let metadata = entry.metadata().map_err(|e| format!("读取元数据失败: {}", e))?;
            let name = entry.file_name().to_string_lossy().to_string();

            entries.push(FileEntry {
                name: name.clone(),
                // 用原始前缀拼（而不是 entry.path()，那是 verbatim 形式）
                path: format!("{}/{}", display_prefix, name),
                is_dir: metadata.is_dir(),
                size: metadata.len(),
                extension: std::path::Path::new(&name)
                    .extension()
                    .map(|e| e.to_string_lossy().to_string()),
            });
        }
        Ok(entries)
    })
    .await
    .map_err(|e| format!("列目录任务失败: {}", e))??;

    // 目录在前，文件在后，按名称排序。
    // 排序键**预计算**一次：`sort_by` 的比较器会被调用 O(n log n) 次，在比较器里
    // `to_lowercase()` 会分配约 2 倍比较次数的临时字符串（n=20000 时近 30 万次分配）。
    // 同仓库 core/jarfile.rs 已用同一写法。
    let mut keyed: Vec<(bool, String, FileEntry)> = entries
        .into_iter()
        .map(|e| (!e.is_dir, e.name.to_lowercase(), e))
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    Ok(keyed.into_iter().map(|(_, _, e)| e).collect())
}

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    /// 编码判定三态：合法 UTF-8 / 合法 GB18030 / 两者都不是（lossy）。
    ///
    /// lossy 的后果是"打开老文件 → 什么都没改 → Ctrl+S"就把原字节固化成一串替换字符，
    /// 故必须与普通 gb18030 区分开（`write_file` 见到 lossy 直接拒绝写）。
    #[test]
    fn test_decode_text_marks_lossy_when_neither_utf8_nor_gb18030() {
        // 合法 UTF-8 中文
        let (text, enc) = decode_text("中文 abc".as_bytes());
        assert_eq!(enc, "utf8");
        assert_eq!(text, "中文 abc");

        // 合法 GBK：'中' = D6 D0，'文' = CE C4。
        // 这串字节不是合法 UTF-8（0xD6 是两字节前导，但后继 0xD0 超出 0x80–0xBF），
        // 所以下面的 `enc == "gb18030"` 同时就在检查这个前提——若它其实合法，
        // decode_text 会判成 utf8 而断言失败。
        let gbk = [0xD6u8, 0xD0, 0xCE, 0xC4];
        let (text, enc) = decode_text(&gbk);
        assert_eq!(enc, "gb18030");
        assert_eq!(text, "中文");

        // 0xE9 单独出现（Latin-1 的 'é'）：GB18030 里是非法的两字节前导 → 解出替换字符
        let latin1 = [b'c', b'a', b'f', 0xE9];
        let (text, enc) = decode_text(&latin1);
        assert_eq!(enc, "lossy", "既非 UTF-8 也非 GB18030 的字节必须标为 lossy");
        assert!(text.contains('\u{FFFD}'), "lossy 文本里应能观察到替换字符: {:?}", text);
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
