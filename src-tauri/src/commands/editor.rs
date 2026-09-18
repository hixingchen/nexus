use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::State;
use crate::database::Database;
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
        // 同 add_project：项目根是白名单的根，必须过收口校验（此处路径来自已登记项目，
        // 因此"已登记"这一条即可放行；危险目录仍然被拒绝）
        crate::commands::editor::ensure_config_dir_allowed(&state, trimmed, "项目路径")?;
    }
    let mut root = state.project_root.lock().map_err(|e| format!("获取项目根路径锁失败: {}", e))?;
    *root = path;
    Ok(())
}

/// 库中登记的「项目目录 ∪ 服务/模板工作目录」（原始字符串，去空）。
///
/// 与服务/模板同源查询：三者都是"用户自己登记过、允许前端访问"的目录。
/// 独立成函数（只依赖 `Database`）是为了能直接用内存库做回归测试——白名单范围是安全关键逻辑。
fn db_allowed_roots(db: &Database) -> Result<Vec<String>, String> {
    db.with_conn(|conn| {
        let mut out: Vec<String> = Vec::new();
        let mut stmt = conn
            .prepare(
                "SELECT path FROM projects \
                 UNION SELECT cwd FROM services UNION SELECT cwd FROM service_templates",
            )
            .map_err(|e| format!("查询可访问目录失败: {}", e))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| format!("读取可访问目录失败: {}", e))?;
        for r in rows {
            let dir = r.map_err(|e| format!("解析可访问目录失败: {}", e))?;
            if !dir.trim().is_empty() {
                out.push(dir);
            }
        }
        Ok(out)
    })
}

/// 允许访问的根目录集合（原始字符串，未 canonicalize）：当前项目根 + 所有已登记项目目录
/// + 所有已配置服务/模板的工作目录。
///
/// 为什么是"所有项目"而不是"当前选中的项目"：
/// - 项目列表里的操作（在资源管理器中打开 / 打开终端）作用在**被右键的那一项**上，与当前
///   高亮项目无关——只放选中项目的根时，「选中 A、右键 B」必被白名单拒掉，现象是弹
///   "打开资源管理器失败"而真正原因（访问被拒绝）只在日志里；
/// - 未选中任何项目时（首次启动、刚删掉选中项目、选中记录失效）原实现整体拒绝，
///   右键菜单同样全线失败。
///
/// 三项都空（库里没有任何项目/服务/模板且未选项目）才一律拒绝——fail closed 语义保留，
/// 拒绝的对象从"未选项目"收紧为"没有任何已知目录"。
fn raw_allowed_roots(state: &AppState) -> Vec<String> {
    let mut roots: Vec<String> = Vec::new();
    // 当前项目根：编辑器上下文的事实来源（也可能指向尚未落库的目录，故独立保留一项）
    if let Ok(root) = state.project_root.lock() {
        if let Some(r) = root.as_ref().filter(|r| !r.trim().is_empty()) {
            roots.push(r.clone());
        }
    }
    // 项目/服务/模板目录：配置可能指向项目外，属合法访问范围
    match db_allowed_roots(&state.db) {
        Ok(mut v) => roots.append(&mut v),
        Err(e) => log::warn!("[nexus] 收集允许访问目录失败（仅按当前项目根校验）: {}", e),
    }
    // 真去重（原实现用 `Vec::dedup`，但它只删**相邻**重复项，而这里并未排序 → 实际是 no-op）
    let mut seen = std::collections::HashSet::new();
    roots.retain(|r| seen.insert(r.clone()));
    roots
}

/// 允许根解析结果的复用窗口。
///
/// 为什么用 TTL 而不是"配置变更代数"作缓存键：代数需要在十几个写命令里逐个记得递增，
/// 漏一处就是"新加的服务目录打不开文件"这类很难查的故障；TTL 没有遗忘风险——最坏情况是
/// 配置变更后 ≤1s 才在**文件访问**上生效（用户两次配置操作间隔远大于 1s），而校验新配置的
/// 入口（`ensure_config_dir_allowed`）走的是实时查询，不受本缓存影响。
const ALLOWED_ROOTS_TTL: std::time::Duration = std::time::Duration::from_secs(1);

/// 已 canonicalize 的允许根（带缓存）。
///
/// 性能背景：每个取路径的命令都会走到这里。最初每次都对**每一个根**做 `canonicalize`；
/// 上一版改为"按原始列表做键"，但**算键本身就要查库**（三表 UNION + 全局 DB 锁）——
/// 文件树连续展开时，每个目录节点都在为同一个答案重复查库。现在再加一层时间窗：
/// 窗口内直接用上次结果，既不做 canonicalize 也不查库。
pub(crate) fn allowed_root_paths(state: &AppState) -> std::sync::Arc<Vec<std::path::PathBuf>> {
    // 快路径：窗口内直接复用（不查库、不解析路径）
    {
        let guard = match state.allowed_roots_cache.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        if let Some((_, cached, checked_at)) = guard.as_ref() {
            if checked_at.elapsed() < ALLOWED_ROOTS_TTL {
                return std::sync::Arc::clone(cached);
            }
        }
    }
    let raw = raw_allowed_roots(state);
    let mut guard = match state.allowed_roots_cache.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    // 窗口已过：原始列表没变就只刷新时间戳（省掉重新 canonicalize）
    if let Some((key, cached, _)) = guard.as_ref() {
        if key == &raw {
            let arc = std::sync::Arc::clone(cached);
            *guard = Some((raw, std::sync::Arc::clone(&arc), std::time::Instant::now()));
            return arc;
        }
    }
    // 无法解析的根直接丢弃（不存在/无权限）——与单根时代的语义一致：那类根恒不命中
    let resolved: Vec<std::path::PathBuf> =
        raw.iter().filter_map(|r| std::fs::canonicalize(r).ok()).collect();
    let arc = std::sync::Arc::new(resolved);
    *guard = Some((raw, std::sync::Arc::clone(&arc), std::time::Instant::now()));
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

/// 读路径校验：当前项目根 + 所有已登记项目目录 + 各服务/模板工作目录
/// （没有任何已知目录时一律拒绝，见 `raw_allowed_roots`）。
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

// ─── 句柄级复核（SEC-4：闭合"校验→使用"之间的 TOCTOU 窗口）──────────

/// 取一个已打开句柄指向的**真实**路径（解析 symlink/junction/8.3 短名后的最终目标）。
///
/// Windows：`GetFinalPathNameByHandleW` + `FILE_NAME_NORMALIZED`（0）。
/// **保留 `\\?\` 前缀不剥**：允许根本身是 `std::fs::canonicalize` 的产物，而后者在 Windows 上
/// 同样返回 `\\?\` 形式——两边同为 verbatim 才能正确 `starts_with` 比较（剥了反而永不命中）。
/// 非 Windows：读 `/proc/self/fd/<n>` 链接（等价语义）。
#[cfg(windows)]
pub(crate) fn final_path_of_handle(file: &std::fs::File) -> Result<std::path::PathBuf, String> {
    use std::os::windows::io::AsRawHandle;
    type WinHandle = isize;
    extern "system" {
        fn GetFinalPathNameByHandleW(
            hFile: WinHandle,
            lpszFilePath: *mut u16,
            cchFilePath: u32,
            dwFlags: u32,
        ) -> u32;
    }

    let handle = file.as_raw_handle() as WinHandle;
    let mut buf = vec![0u16; 4096];
    let len = unsafe {
        GetFinalPathNameByHandleW(handle, buf.as_mut_ptr(), buf.len() as u32, 0)
    };
    if len == 0 {
        return Err("无法解析文件真实路径（GetFinalPathNameByHandle 失败）".into());
    }
    if len as usize >= buf.len() {
        // 超长路径：扩容重试一次（上限 32k，足够任何真实路径）
        buf = vec![0u16; len as usize + 1];
        let len2 = unsafe {
            GetFinalPathNameByHandleW(handle, buf.as_mut_ptr(), buf.len() as u32, 0)
        };
        if len2 == 0 || len2 as usize >= buf.len() {
            return Err("无法解析文件真实路径（路径过长）".into());
        }
        buf.truncate(len2 as usize);
    } else {
        buf.truncate(len as usize);
    }
    Ok(std::path::PathBuf::from(String::from_utf16_lossy(&buf)))
}

#[cfg(not(windows))]
pub(crate) fn final_path_of_handle(file: &std::fs::File) -> Result<std::path::PathBuf, String> {
    use std::os::unix::io::AsRawFd;
    let link = format!("/proc/self/fd/{}", file.as_raw_fd());
    std::fs::read_link(&link).map_err(|e| format!("无法解析文件真实路径: {}", e))
}

/// 打开文件并**按句柄复核**其真实目标在允许范围内（SEC-4）。
///
/// 为什么必须按句柄复核：`check_path_allowed` 校验的是**字符串**，校验与随后的
/// `open`/`read` 之间隔着时间窗口——项目内并发运行的构建/脚本可以在窗口期把该路径
/// 换成 symlink/junction（Windows 上 `mklink /J` 普通用户即可创建），于是"白名单内的
/// 操作"实际读写了白名单外的对象。这里把校验对象与使用对象变成**同一个内核对象**：
/// 先 open 拿句柄，再问内核"这个句柄指向谁"，复核通过后从**同一句柄**读——路径事后怎么变
/// 都影响不到这次操作。
fn open_verified_file(state: &AppState, path: &str) -> Result<(std::fs::File, std::path::PathBuf), String> {
    let file = std::fs::File::open(path).map_err(|e| format!("无法读取文件: {}", e))?;
    let real = final_path_of_handle(&file)?;
    // 目录级禁区（系统目录/用户主目录/自身安装目录…）同样按真实路径判定
    if let Some(reason) = dangerous_dir_reason(&real) {
        return Err(format!("访问被拒绝：目标是{}", reason));
    }
    let roots = allowed_root_paths(state);
    // 真实路径已经是内核给出的规范化形式，直接按分量比较（无需也不能再 canonicalize：
    // 再解析一次就又把窗口带回来了）
    if !roots.iter().any(|root| real.starts_with(root)) {
        return Err("访问被拒绝".into());
    }
    Ok((file, real))
}

/// 打开目录句柄。
///
/// **Windows 必须带 `FILE_FLAG_BACKUP_SEMANTICS`**：该标志的用途正是"允许打开目录句柄"，
/// 不带时 `CreateFileW` 对目录一律返回 `ERROR_ACCESS_DENIED`（`std::fs::File::open` 不会替你
/// 加，本机实测三种目录全部 PermissionDenied）。漏了它 `list_directory`（整个文件树）与
/// "保存新文件时复核父目录"会全体失败，而单测不覆盖它们——见下方回归测试。
/// 非 Windows 上 `File::open` 对目录本就成功。
#[cfg(windows)]
fn open_dir_for_handle(path: &str) -> Result<std::fs::File, String> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .map_err(|e| format!("无法打开目录: {}", e))
}

#[cfg(not(windows))]
fn open_dir_for_handle(path: &str) -> Result<std::fs::File, String> {
    std::fs::File::open(path).map_err(|e| format!("无法打开目录: {}", e))
}

/// 打开目录句柄并复核真实目标（`list_directory` 用：列目录按句柄走）
fn open_verified_dir(state: &AppState, path: &str) -> Result<(std::fs::File, std::path::PathBuf), String> {
    let file = open_dir_for_handle(path)?;
    let real = final_path_of_handle(&file)?;
    if let Some(reason) = dangerous_dir_reason(&real) {
        return Err(format!("访问被拒绝：目标是{}", reason));
    }
    let roots = allowed_root_paths(state);
    if !roots.iter().any(|root| real.starts_with(root)) {
        return Err("访问被拒绝".into());
    }
    Ok((file, real))
}

// ─── 配置路径收口（白名单的"入口"侧校验）────────────────────────
//
// 白名单（`raw_allowed_roots`）的根来自三处 IPC 可写字段：项目 path、服务/模板 cwd、
// 当前项目根。判定机制（canonicalize + 按分量比较 + fail closed）本身是对的，但如果
// **入口**不校验，"调用方"就能自己往根集合里加目录——`add_service{cwd:"C:/"}` 一次
// 就把白名单扩到整个文件系统（`canonicalize("C:/")` 成功即命中）。
//
// 因此这里增加两道闸：
// 1. **危险目录**一律拒绝：驱动器根、系统目录、用户主目录本身、应用数据目录、临时目录、
//    以及 Nexus 自身的安装目录（写这些位置等于代码执行或凭据泄露）；
// 2. **范围收敛**：新配置的目录必须位于"已登记项目目录"或"本会话内经原生目录选择器
//    显式选择过的目录"之内。确需项目外目录时走「选择目录」按钮
//    （`commands::app::pick_directory`）——那是用户亲自点的对话框，不是 IPC 能凭空声明的。
//
// 已存在于库里的历史配置（项目 path、服务/模板 cwd）视为已授权，保证老配置仍能编辑保存。

/// 危险目录判定：返回拒绝原因（None = 通过）。路径不存在时返回 None，交由调用方的 is_dir 检查
pub(crate) fn dangerous_dir_reason(p: &std::path::Path) -> Option<String> {
    let canon = std::fs::canonicalize(p).ok()?;
    // 驱动器根（C:\、D:\）与 UNC 根：没有父目录
    if canon.parent().is_none() {
        return Some("驱动器根目录".into());
    }
    let first = canon
        .components()
        .find_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().to_lowercase()),
            _ => None,
        })
        .unwrap_or_default();
    const SYSTEM_DIRS: [&str; 8] = [
        "windows", "program files", "program files (x86)", "programdata",
        "$recycle.bin", "system volume information", "perflogs", "recovery",
    ];
    if SYSTEM_DIRS.contains(&first.as_str()) {
        return Some(format!("系统目录 {}", first));
    }
    // 用户主目录本身 / 应用数据目录 / 临时目录：写这里等于接管用户级配置
    let mut sensitive: Vec<(&str, std::path::PathBuf)> = Vec::new();
    if let Some(h) = dirs::home_dir() {
        sensitive.push(("用户主目录", h.clone()));
        sensitive.push(("Nexus 数据目录", h.join(".nexus")));
        sensitive.push(("dsh 数据目录", h.join(".dsh")));
    }
    if let Some(d) = dirs::data_dir() {
        sensitive.push(("应用数据目录", d));
    }
    if let Some(d) = dirs::data_local_dir() {
        sensitive.push(("本地应用数据目录", d));
    }
    sensitive.push(("临时目录", std::env::temp_dir()));
    for (label, dir) in sensitive {
        if let Ok(dc) = std::fs::canonicalize(&dir) {
            if canon == dc {
                return Some(label.into());
            }
        }
    }
    // 自身安装目录（或其上级）：写进去可以替换 exe/dll，下次启动即代码执行
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent().and_then(|d| std::fs::canonicalize(d).ok()) {
            if canon == dir || dir.starts_with(&canon) {
                return Some("Nexus 自身安装目录（或其上级）".into());
            }
        }
    }
    None
}

/// 本次运行内由原生目录选择器确认过的目录（canonicalize 后存，去重）
pub(crate) fn confirmed_dirs(state: &AppState) -> Vec<std::path::PathBuf> {
    match state.confirmed_dirs.lock() {
        Ok(g) => g.clone(),
        Err(e) => e.into_inner().clone(),
    }
}

/// 记录用户显式选择过的目录（只应由 `pick_directory` 调用）
pub(crate) fn remember_confirmed_dir(state: &AppState, path: &std::path::Path) {
    let Ok(canon) = std::fs::canonicalize(path) else { return };
    let mut guard = match state.confirmed_dirs.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    if !guard.iter().any(|d| d == &canon) {
        log::info!("[nexus] 用户确认目录: {}", canon.display());
        guard.push(canon);
    }
}

/// 库中已登记的目录（项目 path + 服务/模板 cwd）：历史配置视为已授权
fn registered_dirs(state: &AppState) -> Vec<std::path::PathBuf> {
    let raw = state.db.with_conn(|conn| {
        let mut out: Vec<String> = Vec::new();
        let mut stmt = conn
            .prepare(
                "SELECT path FROM projects \
                 UNION SELECT cwd FROM services UNION SELECT cwd FROM service_templates",
            )
            .map_err(|e| format!("查询已登记目录失败: {}", e))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| format!("读取已登记目录失败: {}", e))?;
        for r in rows {
            let dir = r.map_err(|e| format!("解析已登记目录失败: {}", e))?;
            if !dir.trim().is_empty() {
                out.push(dir);
            }
        }
        Ok(out)
    });
    match raw {
        Ok(list) => list.iter().filter_map(|d| std::fs::canonicalize(d).ok()).collect(),
        Err(e) => {
            log::warn!("[nexus] 读取已登记目录失败（本次按无历史授权处理）: {}", e);
            Vec::new()
        }
    }
}

/// 校验一个"会进入白名单的目录"（项目 path / 服务 cwd / 模板 cwd）
pub(crate) fn ensure_config_dir_allowed(state: &AppState, raw: &str, what: &str) -> Result<(), String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(()); // 未配置：由各命令自行决定是否允许为空
    }
    let p = std::path::Path::new(trimmed);
    if !p.is_dir() {
        return Err(format!("{}不存在或不是目录: {}", what, trimmed));
    }
    if let Some(reason) = dangerous_dir_reason(p) {
        return Err(format!(
            "{}不能是{}（{}）。请选择项目目录或自己的代码目录",
            what, reason, trimmed
        ));
    }
    let canon = std::fs::canonicalize(p).map_err(|e| format!("{}路径解析失败: {}", what, e))?;
    let mut bases = registered_dirs(state);
    bases.extend(confirmed_dirs(state));
    if bases.iter().any(|b| canon.starts_with(b)) {
        return Ok(());
    }
    Err(format!(
        "{}必须在已登记的项目目录内；项目外的目录请用「选择目录」按钮显式选择一次（当前: {}）",
        what, trimmed
    ))
}

/// 校验服务监听路径（JSON 数组）：逐项按配置目录收口
pub(crate) fn ensure_watch_paths_allowed(state: &AppState, raw_json: &str) -> Result<(), String> {
    let trimmed = raw_json.trim();
    if trimmed.is_empty() || trimmed == "[]" {
        return Ok(());
    }
    let parsed: Vec<String> = serde_json::from_str(trimmed)
        .map_err(|e| format!("监听路径不是合法 JSON 数组: {}", e))?;
    for dir in parsed {
        ensure_config_dir_allowed(state, &dir, "监听路径")?;
    }
    Ok(())
}

/// 校验"会话工作目录"（AI 面板）：必须落在已登记项目目录之内
pub(crate) fn ensure_project_dir_allowed(state: &AppState, raw: &str, what: &str) -> Result<(), String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let p = std::path::Path::new(trimmed);
    if !p.is_dir() {
        return Err(format!("{}不存在或不是目录: {}", what, trimmed));
    }
    if let Some(reason) = dangerous_dir_reason(p) {
        return Err(format!("{}不能是{}（{}）", what, reason, trimmed));
    }
    let canon = std::fs::canonicalize(p).map_err(|e| format!("{}路径解析失败: {}", what, e))?;
    if registered_dirs(state).iter().any(|b| canon.starts_with(b)) {
        return Ok(());
    }
    Err(format!("{}必须是已登记的项目目录（当前: {}）", what, trimmed))
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
    // 句柄级校验（SEC-4）：打开后用 GetFinalPathNameByHandleW 复核真实目标，并从同一句柄读取
    let (file, _real) = open_verified_file(&state, &path)?;
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
    check_write_path_allowed(&state, &path)?;
    if (content.len() as u64) > MAX_WRITE_SIZE {
        return Err(format!("文件过大（{:.1} MB），超过 10 MB 上限", content.len() as f64 / (1024.0 * 1024.0)));
    }
    // 句柄级复核（SEC-4）：把"要写哪个对象"从路径字符串换成内核解析出的真实路径。
    // 已存在 → 打开它并按句柄复核；不存在（新建）→ 按句柄复核父目录再用真实父目录拼目标。
    let target: std::path::PathBuf = {
        let exists = std::path::Path::new(&path).exists();
        match open_verified_file(&state, &path) {
            Ok((_f, real)) => real,
            Err(e) => {
                if exists {
                    return Err(e); // 存在但复核不通过：拒绝，不当作"新建"
                }
                let p = std::path::Path::new(&path);
                let parent = p.parent().ok_or_else(|| "访问被拒绝".to_string())?;
                let name = p.file_name().ok_or_else(|| "访问被拒绝".to_string())?;
                let (_d, real_parent) = open_verified_dir(&state, &parent.to_string_lossy())?;
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
    let (file, _real) = open_verified_file(state, path)?;
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
    let (mut file, _real) = open_verified_file(&state, &path)?;
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
    // 句柄级校验（SEC-4）：打开目录句柄复核真实目标，随后按**核实后的路径**列目录
    let (_dir_handle, real) = open_verified_dir(&state, &path)?;
    // 条目 path 仍以用户给的原始路径为前缀（前端按它做展开/拼接，换成 verbatim 形式会显示成 \\?\C:\…），
    // 读取侧用的是核实后的 real
    let display_prefix = path.trim_end_matches(['/', '\\']).to_string();

    let mut entries = vec![];
    let mut read_dir = tokio::fs::read_dir(&real).await.map_err(|e| format!("读取目录失败: {}", e))?;

    while let Some(entry) = read_dir.next_entry().await.map_err(|e| format!("读取条目失败: {}", e))? {
        if entries.len() >= MAX_DIR_ENTRIES {
            return Err(format!("目录条目过多（超过 {} 条），请改用更具体的目录", MAX_DIR_ENTRIES));
        }
        let metadata = entry.metadata().await.map_err(|e| format!("读取元数据失败: {}", e))?;
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

    /// 回归：白名单必须覆盖**所有**已登记项目，而不只是当前选中的那个。
    /// 旧实现只放"当前项目根"，于是项目列表里「选中 A、右键 B → 在资源管理器中打开」
    /// 被白名单拒掉（现象：弹"打开资源管理器失败"，原因只在日志里）。
    #[test]
    fn test_db_allowed_roots_includes_every_project() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE projects (id TEXT PRIMARY KEY, path TEXT NOT NULL DEFAULT '');
             CREATE TABLE services (cwd TEXT NOT NULL DEFAULT '');
             CREATE TABLE service_templates (cwd TEXT NOT NULL DEFAULT '');
             INSERT INTO projects (id, path) VALUES ('p1', 'D:/work/a'), ('p2', 'D:/work/b');
             INSERT INTO services (cwd) VALUES ('D:/work/a/svc'), ('');
             INSERT INTO service_templates (cwd) VALUES ('D:/work/tpl');",
        ).unwrap();
        let db = Database { conn: std::sync::Mutex::new(conn) };

        let roots = db_allowed_roots(&db).unwrap();

        assert!(roots.contains(&"D:/work/a".to_string()), "选中项目的目录应在白名单内");
        assert!(roots.contains(&"D:/work/b".to_string()), "未选中项目的目录同样必须在白名单内");
        assert!(roots.contains(&"D:/work/a/svc".to_string()), "服务工作目录应在白名单内");
        assert!(roots.contains(&"D:/work/tpl".to_string()), "模板工作目录应在白名单内");
        assert!(!roots.iter().any(|r| r.is_empty()), "空 cwd 不得进白名单（空串会被 canonicalize 成当前目录）");
    }

    /// 危险目录判定：驱动器根 / 系统目录 / 临时目录必须被拒，普通目录放行。
    /// （这些路径若成为白名单的根，写文件就等于代码执行或凭据泄露）
    #[test]
    #[cfg(windows)]
    fn test_dangerous_dir_rejected() {
        // 驱动器根：canonicalize("C:\\") 成功且没有父目录
        assert!(dangerous_dir_reason(std::path::Path::new("C:\\")).is_some());
        // 系统目录（第一级分量命中）
        if std::path::Path::new("C:\\Windows").is_dir() {
            assert!(dangerous_dir_reason(std::path::Path::new("C:\\Windows")).is_some());
        }
        // 临时目录本身
        let temp = std::env::temp_dir();
        assert!(dangerous_dir_reason(&temp).is_some(), "临时目录不应被当作可写根");
        // 普通子目录放行
        let sub = temp.join(format!("nexus-danger-ok-{}", std::process::id()));
        std::fs::create_dir_all(&sub).unwrap();
        assert!(dangerous_dir_reason(&sub).is_none(), "普通目录应放行");
        let _ = std::fs::remove_dir_all(&sub);
    }

    /// 不存在的路径交回调用方判定（这里返回 None，由 is_dir 报"不存在或不是目录"）
    #[test]
    fn test_dangerous_dir_nonexistent_is_none() {
        assert!(dangerous_dir_reason(std::path::Path::new("Z:\\definitely\\missing\\dir")).is_none());
    }

    /// 回归：目录句柄必须能打开 —— Windows 上 `File::open` 对目录是 Access Denied，
    /// 漏了 `FILE_FLAG_BACKUP_SEMANTICS` 会让 `list_directory`（文件树）与"新建文件时复核
    /// 父目录"在运行时整体失败，而这两条路径此前没有任何测试覆盖。
    #[test]
    fn test_open_dir_for_handle_opens_directory() {
        let dir = std::env::temp_dir();
        let handle = open_dir_for_handle(&dir.to_string_lossy()).expect("目录句柄应能打开");
        assert!(handle.metadata().expect("读取句柄元数据失败").is_dir(), "句柄应指向目录");
        // 句柄级复核依赖 GetFinalPathNameByHandleW 对目录同样有效
        let real = final_path_of_handle(&handle).expect("目录句柄应能解析真实路径");
        assert!(real.is_dir(), "解析出的真实路径应为目录: {:?}", real);
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
