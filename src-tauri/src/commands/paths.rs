//! 文件访问白名单：状态（`PathAllowlist`）+ 一套校验规则。
//!
//! 为什么从 `commands/editor.rs` 拆出来（ARCH-15）：这套规则约 450 行，此前住在
//! 12 个 IPC 命令的家里，并被 `service`/`project`/`ai`/`search` 跨模块调用——
//! 想知道"哪些路径会被拒绝写入"必须读一个 1100 行的文件，且它同时是编辑器命令的家。
//!
//! 为什么把三个状态字段收成一个类型（ARCH-16）：它们此前是 `AppState` 上三个各自独立的
//! 互斥量，而 `AppState` 有 9 个字段、39 个命令人人可及——"哪些代码能改白名单"在类型上
//! 给不出答案，只能 grep 字段名。收成 `PathAllowlist` 后：范围由类型收敛，且它能脱离
//! `AppState` 单独构造，于是这套安全关键规则终于可以被直接测试（此前要测就得先造一个
//! 带数据库与进程管理器的完整状态）。
//!
//! 分工：本模块只管"路径能不能访问"，不碰文件内容（读写命令在 `commands/editor.rs`）。

use crate::database::Database;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 允许根解析缓存：(原始根字符串列表, 已 canonicalize 的 PathBuf 列表, 上次核对时间)。
///
/// 键为原始列表 → DB 增删服务/模板后自然失效；`Instant` 给出复用窗口（见 `ALLOWED_ROOTS_TTL`）
/// ——文件树连续展开时连"查库算原始列表"这一步也省掉。
type AllowedRootsCache = Mutex<Option<(Vec<String>, Arc<Vec<PathBuf>>, std::time::Instant)>>;

/// 文件访问白名单的**状态**：当前项目根 + 本会话确认过的目录 + 允许根解析缓存。
///
/// 三项之所以是一体的：它们只被同一套判断使用——"这个路径是否落在用户登记/确认过的范围内"。
pub struct PathAllowlist {
    /// 当前项目根（编辑器上下文的事实来源，也可能指向尚未落库的目录）
    project_root: Mutex<Option<String>>,
    /// 本会话内由原生目录选择器确认过的目录（canonicalize 后）。
    ///
    /// 为什么要有它：白名单的根来自 IPC 可写字段，若不收口，调用方（webview）
    /// 可以自己把任意目录加进白名单。经用户亲自点选的目录才允许成为项目外的新根，
    /// 见 `ensure_config_dir_allowed`。
    confirmed_dirs: Mutex<Vec<PathBuf>>,
    roots_cache: AllowedRootsCache,
}

impl PathAllowlist {
    pub fn new() -> Self {
        Self {
            project_root: Mutex::new(None),
            confirmed_dirs: Mutex::new(Vec::new()),
            roots_cache: Mutex::new(None),
        }
    }

    /// 当前项目根（未设置 / 锁中毒时返回 None）
    pub fn project_root(&self) -> Option<String> {
        match self.project_root.lock() {
            Ok(root) => root.clone(),
            Err(e) => e.into_inner().clone(),
        }
    }

    /// 设置当前项目根（安全白名单根之一：另加各服务/模板工作目录，见 `allowed_roots`）。
    /// 路径合法性由调用方先校验（命令层做 `ensure_config_dir_allowed`）。
    pub fn set_project_root(&self, path: Option<String>) -> Result<(), String> {
        let mut root = self.project_root.lock().map_err(|e| format!("获取项目根路径锁失败: {}", e))?;
        *root = path;
        Ok(())
    }

    /// 本次运行内由原生目录选择器确认过的目录（canonicalize 后存，去重）
    pub fn confirmed_dirs(&self) -> Vec<PathBuf> {
        match self.confirmed_dirs.lock() {
            Ok(g) => g.clone(),
            Err(e) => e.into_inner().clone(),
        }
    }

    /// 记录用户显式选择过的目录（只应由 `pick_directory` 调用）
    pub fn remember_confirmed_dir(&self, path: &Path) {
        let Ok(canon) = std::fs::canonicalize(path) else { return };
        let mut guard = match self.confirmed_dirs.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        if !guard.iter().any(|d| d == &canon) {
            log::info!("[nexus] 用户确认目录: {}", canon.display());
            guard.push(canon);
        }
    }

    /// 库中登记的「项目目录 ∪ 服务/模板工作目录」（原始字符串，去空）+ 当前项目根。
    fn raw_allowed_roots(&self, db: &Database) -> Vec<String> {
        let mut roots: Vec<String> = Vec::new();
        // 当前项目根：编辑器上下文的事实来源（也可能指向尚未落库的目录，故独立保留一项）
        if let Some(r) = self.project_root().filter(|r| !r.trim().is_empty()) {
            roots.push(r);
        }
        // 项目/服务/模板目录：配置可能指向项目外，属合法访问范围
        match db_allowed_roots(db) {
            Ok(mut v) => roots.append(&mut v),
            Err(e) => log::warn!("[nexus] 收集允许访问目录失败（仅按当前项目根校验）: {}", e),
        }
        // 真去重（原实现用 `Vec::dedup`，但它只删**相邻**重复项，而这里并未排序 → 实际是 no-op）
        let mut seen = std::collections::HashSet::new();
        roots.retain(|r| seen.insert(r.clone()));
        roots
    }

    /// 已 canonicalize 的允许根（带缓存）。
    ///
    /// 性能背景：每个取路径的命令都会走到这里。最初每次都对**每一个根**做 `canonicalize`；
    /// 上一版改为"按原始列表做键"，但**算键本身就要查库**（三表 UNION + 全局 DB 锁）——
    /// 文件树连续展开时，每个目录节点都在为同一个答案重复查库。现在再加一层时间窗：
    /// 窗口内直接用上次结果，既不做 canonicalize 也不查库。
    pub fn allowed_roots(&self, db: &Database) -> Arc<Vec<PathBuf>> {
        // 快路径：窗口内直接复用（不查库、不解析路径）
        {
            let guard = match self.roots_cache.lock() {
                Ok(g) => g,
                Err(e) => e.into_inner(),
            };
            if let Some((_, cached, checked_at)) = guard.as_ref() {
                if checked_at.elapsed() < ALLOWED_ROOTS_TTL {
                    return Arc::clone(cached);
                }
            }
        }
        let raw = self.raw_allowed_roots(db);
        let mut guard = match self.roots_cache.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        // 窗口已过：原始列表没变就只刷新时间戳（省掉重新 canonicalize）
        if let Some((key, cached, _)) = guard.as_ref() {
            if key == &raw {
                let arc = Arc::clone(cached);
                *guard = Some((raw, Arc::clone(&arc), std::time::Instant::now()));
                return arc;
            }
        }
        // 无法解析的根直接丢弃（不存在/无权限）——与单根时代的语义一致：那类根恒不命中
        let resolved: Vec<PathBuf> = raw.iter().filter_map(|r| std::fs::canonicalize(r).ok()).collect();
        let arc = Arc::new(resolved);
        *guard = Some((raw, Arc::clone(&arc), std::time::Instant::now()));
        arc
    }

    /// 读路径校验：当前项目根 + 所有已登记项目目录 + 各服务/模板工作目录
    /// （没有任何已知目录时一律拒绝，见 `raw_allowed_roots`）。
    /// 供所有取路径的命令统一调用——此前 `list_directory`/`open_terminal`/`open_in_explorer`
    /// 等命令完全绕过白名单，导致"同类危险操作两套口径"。
    pub fn check_path_allowed(&self, db: &Database, path: &str) -> Result<(), String> {
        let roots = self.allowed_roots(db);
        if canonical_path_within(path, &roots) {
            Ok(())
        } else {
            Err("访问被拒绝".into())
        }
    }

    /// 写路径校验：目标已存在时按读路径规则；不存在时校验**父目录**（支持新建文件）。
    /// 文件名必须为单一段（不含分隔符与 `.`/`..`），避免经由父目录校验绕过范围限制。
    pub fn check_write_path_allowed(&self, db: &Database, path: &str) -> Result<(), String> {
        let p = Path::new(path);
        if p.exists() {
            return self.check_path_allowed(db, path);
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
                self.check_path_allowed(db, &parent.to_string_lossy())
            }
            _ => Err("访问被拒绝".into()),
        }
    }

    /// 打开文件并**按句柄复核**其真实目标在允许范围内（SEC-4）。
    ///
    /// 为什么必须按句柄复核：`check_path_allowed` 校验的是**字符串**，校验与随后的
    /// `open`/`read` 之间隔着时间窗口——项目内并发运行的构建/脚本可以在窗口期把该路径
    /// 换成 symlink/junction（Windows 上 `mklink /J` 普通用户即可创建），于是"白名单内的
    /// 操作"实际读写了白名单外的对象。这里把校验对象与使用对象变成**同一个内核对象**：
    /// 先 open 拿句柄，再问内核"这个句柄指向谁"，复核通过后从**同一句柄**读——路径事后怎么变
    /// 都影响不到这次操作。
    pub fn open_verified_file(&self, db: &Database, path: &str) -> Result<(std::fs::File, PathBuf), String> {
        let file = std::fs::File::open(path).map_err(|e| format!("无法读取文件: {}", e))?;
        let real = final_path_of_handle(&file)?;
        // 目录级禁区（系统目录/用户主目录/自身安装目录…）同样按真实路径判定
        if let Some(reason) = dangerous_dir_reason(&real) {
            return Err(format!("访问被拒绝：目标是{}", reason));
        }
        let roots = self.allowed_roots(db);
        // 真实路径已经是内核给出的规范化形式，直接按分量比较（无需也不能再 canonicalize：
        // 再解析一次就又把窗口带回来了）
        if !roots.iter().any(|root| real.starts_with(root)) {
            return Err("访问被拒绝".into());
        }
        Ok((file, real))
    }

    /// 打开目录句柄并复核真实目标（`list_directory` 用：列目录按句柄走）
    pub fn open_verified_dir(&self, db: &Database, path: &str) -> Result<(std::fs::File, PathBuf), String> {
        let file = open_dir_for_handle(path)?;
        let real = final_path_of_handle(&file)?;
        if let Some(reason) = dangerous_dir_reason(&real) {
            return Err(format!("访问被拒绝：目标是{}", reason));
        }
        let roots = self.allowed_roots(db);
        if !roots.iter().any(|root| real.starts_with(root)) {
            return Err("访问被拒绝".into());
        }
        Ok((file, real))
    }

    /// 校验一个"会进入白名单的目录"（项目 path / 服务 cwd / 模板 cwd）
    pub fn ensure_config_dir_allowed(&self, db: &Database, raw: &str, what: &str) -> Result<(), String> {
        db.with_conn(|conn| self.ensure_config_dir_allowed_with_conn(conn, raw, what))
    }

    /// `ensure_config_dir_allowed` 的**已持有连接**版本。
    ///
    /// 为什么需要它（P0-5）：模板导入跑在 `with_conn_mut` 的事务里，那里再经 `Database`
    /// 取锁是同一线程重入（本项目对重入是检测到就 panic，而检测器自身还会误报/漏报，
    /// 见 CQ-26）。校验用的连接与写库用的是同一个，因此也不存在"校验后被换掉"的窗口。
    pub(crate) fn ensure_config_dir_allowed_with_conn(
        &self,
        conn: &rusqlite::Connection,
        raw: &str,
        what: &str,
    ) -> Result<(), String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(()); // 未配置：由各命令自行决定是否允许为空
        }
        let p = Path::new(trimmed);
        if !p.is_dir() {
            return Err(format!("{}不存在或不是目录: {}", what, trimmed));
        }
        if let Some(reason) = dangerous_dir_reason(p) {
            return Err(format!("{}不能是{}（{}）。请选择项目目录或自己的代码目录", what, reason, trimmed));
        }
        let canon = std::fs::canonicalize(p).map_err(|e| format!("{}路径解析失败: {}", what, e))?;
        let mut bases = registered_dirs_from_conn(conn);
        bases.extend(self.confirmed_dirs());
        if bases.iter().any(|b| canon.starts_with(b)) {
            return Ok(());
        }
        Err(format!(
            "{}必须在已登记的项目目录内；项目外的目录请用「选择目录」按钮显式选择一次（当前: {}）",
            what, trimmed
        ))
    }

    /// 校验服务监听路径（JSON 数组）：逐项按配置目录收口
    pub fn ensure_watch_paths_allowed(&self, db: &Database, raw_json: &str) -> Result<(), String> {
        let trimmed = raw_json.trim();
        if trimmed.is_empty() || trimmed == "[]" {
            return Ok(());
        }
        let parsed: Vec<String> = serde_json::from_str(trimmed)
            .map_err(|e| format!("监听路径不是合法 JSON 数组: {}", e))?;
        for dir in parsed {
            self.ensure_config_dir_allowed(db, &dir, "监听路径")?;
        }
        Ok(())
    }

    /// 校验"会话工作目录"（AI 面板）：必须落在已登记项目目录之内
    pub fn ensure_project_dir_allowed(&self, db: &Database, raw: &str, what: &str) -> Result<(), String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        let p = Path::new(trimmed);
        if !p.is_dir() {
            return Err(format!("{}不存在或不是目录: {}", what, trimmed));
        }
        if let Some(reason) = dangerous_dir_reason(p) {
            return Err(format!("{}不能是{}（{}）", what, reason, trimmed));
        }
        let canon = std::fs::canonicalize(p).map_err(|e| format!("{}路径解析失败: {}", what, e))?;
        if registered_dirs(db).iter().any(|b| canon.starts_with(b)) {
            return Ok(());
        }
        Err(format!("{}必须是已登记的项目目录（当前: {}", what, trimmed))
    }
}

/// 库中登记的「项目目录 ∪ 服务/模板工作目录」（原始字符串，去空）。
///
/// 与服务/模板同源查询：三者都是"用户自己登记过、允许前端访问"的目录。
/// 独立成函数（只依赖 `Database`）是为了能直接用内存库做回归测试——白名单范围是安全关键逻辑。
fn db_allowed_roots(db: &Database) -> Result<Vec<String>, String> {
    db.with_conn(db_allowed_roots_from_conn)
}

/// `db_allowed_roots` 的**连接版**：给"已经持有连接（事务内）"的调用方用。
///
/// 为什么必须有它：`Database::conn` 是不可重入的互斥量，事务闭包里再经 `Database`
/// 取锁是同一线程重入——本项目对重入是检测到就 panic，而检测器自身有误报/漏报
/// （CQ-26）。模板导入（P0-5）必须在写库的同一个事务里做校验，因此只能走这条。
fn db_allowed_roots_from_conn(conn: &rusqlite::Connection) -> Result<Vec<String>, String> {
    {
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
    }
}

/// 库中已登记的目录（项目 path + 服务/模板 cwd）：历史配置视为已授权
fn registered_dirs(db: &Database) -> Vec<PathBuf> {
    to_registered_dirs(db_allowed_roots(db))
}

/// `registered_dirs` 的连接版（理由同 `db_allowed_roots_from_conn`）
pub(crate) fn registered_dirs_from_conn(conn: &rusqlite::Connection) -> Vec<PathBuf> {
    to_registered_dirs(db_allowed_roots_from_conn(conn))
}

/// 原始根字符串 → 已 canonicalize 的目录（解析失败的根直接丢弃，与 `allowed_roots` 同口径）
fn to_registered_dirs(raw: Result<Vec<String>, String>) -> Vec<PathBuf> {
    match raw {
        Ok(list) => list.iter().filter_map(|d| std::fs::canonicalize(d).ok()).collect(),
        Err(e) => {
            log::warn!("[nexus] 读取已登记目录失败（本次按无历史授权处理）: {}", e);
            Vec::new()
        }
    }
}

/// 允许根解析结果的复用窗口。
///
/// 为什么用 TTL 而不是"配置变更代数"作缓存键：代数需要在十几个写命令里逐个记得递增，
/// 漏一处就是"新加的服务目录打不开文件"这类很难查的故障；TTL 没有遗忘风险——最坏情况是
/// 配置变更后 ≤1s 才在**文件访问**上生效（用户两次配置操作间隔远大于 1s），而校验新配置的
/// 入口（`ensure_config_dir_allowed`）走的是实时查询，不受本缓存影响。
const ALLOWED_ROOTS_TTL: std::time::Duration = std::time::Duration::from_secs(1);

/// 请求路径是否位于任一允许根之下。
///
/// 任意一端无法 canonicalize（请求路径不存在、根失效）即判否——fail closed。
/// `PathBuf::starts_with` 按**路径分量**比较，因此 `/root` 不会误命中 `/root-evil`；
/// `..`、UNC、`\\?\`、8.3 短名、结尾点/空格、ADS 都在 canonicalize 阶段被解析掉。
fn canonical_path_within(requested: &str, roots: &[PathBuf]) -> bool {
    if roots.is_empty() {
        return false;
    }
    match std::fs::canonicalize(requested) {
        Ok(canon) => roots.iter().any(|root| canon.starts_with(root)),
        Err(_) => false,
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
pub(crate) fn final_path_of_handle(file: &std::fs::File) -> Result<PathBuf, String> {
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
    Ok(PathBuf::from(String::from_utf16_lossy(&buf)))
}

#[cfg(not(windows))]
pub(crate) fn final_path_of_handle(file: &std::fs::File) -> Result<PathBuf, String> {
    use std::os::unix::io::AsRawFd;
    let link = format!("/proc/self/fd/{}", file.as_raw_fd());
    std::fs::read_link(&link).map_err(|e| format!("无法解析文件真实路径: {}", e))
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

// ─── 配置路径收口（白名单的"入口"侧校验）────────────────────────
//
// 白名单的根来自三处 IPC 可写字段：项目 path、服务/模板 cwd、当前项目根。判定机制
// （canonicalize + 按分量比较 + fail closed）本身是对的，但如果**入口**不校验，
// "调用方"就能自己往根集合里加目录——`add_service{cwd:"C:/"}` 一次就把白名单扩到整个
// 文件系统（`canonicalize("C:/")` 成功即命中）。
//
// 因此入口侧有两道闸（见 `PathAllowlist::ensure_config_dir_allowed`）：
// 1. **危险目录**一律拒绝（`dangerous_dir_reason`）：驱动器根、系统目录、用户主目录本身、
//    应用数据目录、临时目录、以及 Nexus 自身的安装目录（写这些位置等于代码执行或凭据泄露）；
// 2. **范围收敛**：新配置的目录必须位于"已登记项目目录"或"本会话内经原生目录选择器
//    显式选择过的目录"之内。确需项目外目录时走「选择目录」按钮
//    （`commands::app::pick_directory`）——那是用户亲自点的对话框，不是 IPC 能凭空声明的。
//
// 已存在于库里的历史配置（项目 path、服务/模板 cwd）视为已授权，保证老配置仍能编辑保存。

/// 危险目录判定：返回拒绝原因（None = 通过）。路径不存在时返回 None，交由调用方的 is_dir 检查
pub(crate) fn dangerous_dir_reason(p: &Path) -> Option<String> {
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
    let mut sensitive: Vec<(&str, PathBuf)> = Vec::new();
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

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试辅助：把若干根字符串 canonicalize 成与生产同形的 `Vec<PathBuf>`
    fn roots_of(paths: &[&Path]) -> Vec<PathBuf> {
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
        let roots = roots_of(&[Path::new("/nonexistent/root/for/nexus/test")]);
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
        // 直接验证缓存键的比较语义（构造成本高的部分在别处，故只测键相等性）
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
        let db = Database::from_connection(conn);

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
        assert!(dangerous_dir_reason(Path::new("C:\\")).is_some());
        // 系统目录（第一级分量命中）
        if Path::new("C:\\Windows").is_dir() {
            assert!(dangerous_dir_reason(Path::new("C:\\Windows")).is_some());
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
        assert!(dangerous_dir_reason(Path::new("Z:\\definitely\\missing\\dir")).is_none());
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

    /// 白名单收成独立类型后的直接收益：**不需要 AppState** 就能测这套安全关键规则。
    ///
    /// 覆盖三条真实失败路径（此前只能靠读代码确认）：
    /// 1. 未登记目录 → 拒绝（否则 IPC 可以自选任意 cwd 进白名单）；
    /// 2. 已登记目录 → 通过（历史配置必须仍能编辑保存，否则老项目改不了配置）；
    /// 3. 危险目录（临时目录本身）→ 即使已登记也拒绝。
    #[test]
    fn test_path_allowlist_scope_check_without_app_state() {
        use crate::database::init_schema;
        let conn = rusqlite::Connection::open_in_memory().expect("内存库");
        init_schema(&conn).expect("建表");
        let db = Database::from_connection(conn);

        let base = std::env::temp_dir().join(format!("nexus_ut_scope_{}", std::process::id()));
        let registered = base.join("registered");
        let outside = base.join("outside");
        std::fs::create_dir_all(&registered).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        // 登记一个项目目录（走 DB → 历史配置即"已授权"）
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO projects (id, name, path) VALUES ('p1', 'P', ?1)",
                [&registered.to_string_lossy().to_string()],
            ).map_err(|e| e.to_string())?;
            Ok(())
        }).unwrap();

        let paths = PathAllowlist::new();
        assert!(paths.ensure_config_dir_allowed(&db, &registered.to_string_lossy(), "工作目录").is_ok(),
            "已登记目录必须放行——否则老项目连配置都改不了");
        assert!(paths.ensure_config_dir_allowed(&db, &outside.to_string_lossy(), "工作目录").is_err(),
            "未登记目录必须拒绝——否则 IPC 能自己把任意目录扩进白名单");

        // 用户显式确认过的目录越过后一道闸（但仍过危险目录那道闸）
        paths.remember_confirmed_dir(&outside);
        assert!(paths.ensure_config_dir_allowed(&db, &outside.to_string_lossy(), "工作目录").is_ok(),
            "经原生目录选择器确认过的目录应被接受");

        let temp = std::env::temp_dir();
        assert!(paths.ensure_config_dir_allowed(&db, &temp.to_string_lossy(), "工作目录").is_err(),
            "临时目录即使作为根也必须拒绝");

        let _ = std::fs::remove_dir_all(&base);
    }
}
