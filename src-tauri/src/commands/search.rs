use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::State;
use crate::AppState;
use crate::commands::editor::check_path_allowed;

/// 单条搜索结果
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResultItem {
    pub path: String,
    pub name: String,
    pub line: usize,
    pub snippet: String,
}

/// 搜索响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    pub results: Vec<SearchResultItem>,
    /// 结果/扫描达到上限被截断
    pub truncated: bool,
}

/// 服务文件搜索参数
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchParams {
    /// 搜索根目录（服务 cwd）
    pub root: String,
    pub query: String,
    /// 扩展名筛选（不含点，如 ["ts","tsx","vue"]；空 = 全部文本文件）
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default = "default_max_results")]
    pub max_results: usize,
}

fn default_max_results() -> usize { 1000 }

/// 默认排除的目录（SVN 项目含 .svn）
const DEFAULT_EXCLUDE_DIRS: &[&str] = &[
    "node_modules", ".git", ".svn", ".hg", "dist", "target", "__pycache__",
    ".next", "build", "coverage", ".idea", ".vscode", ".cache", ".turbo",
];

/// 二进制/不可搜索的扩展名（小写，不含点）
const BINARY_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "ico", "bmp", "webp", "tiff",
    "zip", "tar", "gz", "7z", "rar", "xz", "bz2", "pdf",
    "exe", "dll", "so", "dylib", "wasm", "obj", "o", "a", "lib", "class", "jar", "pyc",
    "mp3", "mp4", "wav", "flac", "avi", "mkv", "mov",
    "bin", "dat", "db", "sqlite", "sqlite3", "woff", "woff2", "ttf", "otf", "eot",
];

/// 单文件搜索大小上限（防超大文件读入内存）
const MAX_SEARCH_FILE_SIZE: u64 = 2 * 1024 * 1024;
/// 遍历文件数上限（防失控）
const MAX_SCAN_FILES: usize = 20000;
/// 目录递归深度上限
const MAX_DEPTH: usize = 24;
/// 二进制检测：前 8KB 含 NUL 视为二进制
const BINARY_PROBE_SIZE: usize = 8192;
/// snippet 命中位置前后保留的字符数
const SNIPPET_MARGIN: usize = 50;

/// 在目录中按内容搜索文件（子串匹配，大小写可开关，可按扩展名筛选）
#[tauri::command]
pub async fn search_files(
    state: State<'_, AppState>,
    params: SearchParams,
) -> Result<SearchResponse, String> {
    let query = params.query.trim().to_string();
    if query.is_empty() { return Err("搜索内容不能为空".into()); }
    if query.len() > 200 { return Err("搜索内容过长".into()); }
    let root = params.root.replace('\\', "/");
    if root.is_empty() { return Err("搜索目录不能为空".into()); }
    // 多根白名单校验（项目根 + 各服务/模板工作目录），内部不跨 await 持锁
    check_path_allowed(&state, &root)?;

    let exts: Vec<String> = params.extensions.iter()
        .map(|e| e.trim().trim_start_matches('.').to_lowercase())
        .filter(|e| !e.is_empty())
        .collect();
    // 大小写不敏感时统一小写，行内匹配用同一规则
    let q = if params.case_sensitive { query } else { query.to_lowercase() };
    let max_results = params.max_results.clamp(1, 5000);

    // 同步遍历 + 读文件放 spawn_blocking，避免阻塞主线程
    tokio::task::spawn_blocking(move || {
        search_in_dir(&root, &q, params.case_sensitive, &exts, max_results)
    })
    .await
    .map_err(|e| format!("搜索失败: {}", e))?
}

/// 同步递归搜索（在 spawn_blocking 中执行）
fn search_in_dir(
    root: &str,
    q: &str,
    case_sensitive: bool,
    exts: &[String],
    max_results: usize,
) -> Result<SearchResponse, String> {
    // 单文件搜索（目录树文件节点右键）：搜文件自身
    let root_path = PathBuf::from(root);
    if root_path.is_file() {
        let mut results = Vec::new();
        let truncated = false;
        if file_is_searchable(&root_path, exts) {
            if let Some(hits) = search_file(&root_path, q, case_sensitive, max_results) {
                results.extend(hits);
            }
        }
        return Ok(SearchResponse { results, truncated });
    }

    let mut results: Vec<SearchResultItem> = Vec::new();
    let mut scanned = 0usize;
    let mut truncated = false;

    // 深度优先（栈），每层记录深度防失控
    let mut stack: Vec<(PathBuf, usize)> = vec![(PathBuf::from(root), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if results.len() >= max_results { truncated = true; break; }
        if depth > MAX_DEPTH { continue; }

        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue, // 无权限等：跳过该目录
        };
        let mut subdirs: Vec<(PathBuf, usize)> = Vec::new();
        for entry in entries.flatten() {
            // file_type() 取自目录项本身（不额外 stat），并据此跳过符号链接/junction：
            // 它们可能指向项目外（既泄漏白名单外内容，也让同一份内容被重复遍历）
            let Ok(file_type) = entry.file_type() else { continue };
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            if file_type.is_dir() {
                let name = entry.file_name().to_string_lossy().to_lowercase();
                if DEFAULT_EXCLUDE_DIRS.contains(&name.as_str()) { continue; }
                subdirs.push((path, depth + 1));
            } else if file_type.is_file() {
                scanned += 1;
                if scanned > MAX_SCAN_FILES { truncated = true; break; }
                if !file_is_searchable(&path, exts) { continue; }
                if let Some(hits) = search_file(&path, q, case_sensitive, max_results - results.len()) {
                    results.extend(hits);
                }
            }
        }
        for d in subdirs.into_iter().rev() { stack.push(d); }
        if scanned > MAX_SCAN_FILES { truncated = true; break; }
    }

    results.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(SearchResponse { results, truncated })
}

/// 文件是否可搜索：大小上限 / 扩展名筛选 / 二进制扩展名 / 内容含 NUL
fn file_is_searchable(path: &Path, exts: &[String]) -> bool {
    let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if len == 0 || len > MAX_SEARCH_FILE_SIZE { return false; }

    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let ext = Path::new(name).extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if !exts.is_empty() && !exts.iter().any(|e| e == &ext) { return false; }
    if BINARY_EXTENSIONS.contains(&ext.as_str()) { return false; }

    // 二进制检测：读前 8KB，含 NUL 字节视为二进制跳过
    let mut buf = [0u8; BINARY_PROBE_SIZE];
    let n = std::fs::File::open(path).ok()
        .and_then(|mut f| std::io::Read::read(&mut f, &mut buf).ok())
        .unwrap_or(0);
    !buf[..n].contains(&0)
}

/// 在单文件中逐行搜索，返回命中行
fn search_file(path: &Path, q: &str, case_sensitive: bool, limit: usize) -> Option<Vec<SearchResultItem>> {
    // UTF-8 严格解码失败 → 回退 GB18030（GBK 超集）：与编辑器读取（decode_text）对齐，
    // 否则 Windows 中文老项目的 GBK 文件在搜索中静默缺失
    let content = match std::fs::read(path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(c) => c,
            Err(utf8_bytes) => encoding_rs::GB18030.decode(&utf8_bytes.into_bytes()).0.into_owned(),
        },
        Err(_) => return None, // 读取失败：跳过
    };
    let mut hits = Vec::new();
    let q_char_count = q.chars().count();
    for (idx, line) in content.lines().enumerate() {
        if hits.len() >= limit { break; }
        // 命中位置统一折算成**字符序号**（后续按字符切 snippet，字节偏移在非 ASCII 行上会切错）：
        // - 大小写敏感：直接在原行匹配后把字节偏移折算成字符数
        // - 不敏感且行/查询都是 ASCII：逐字节比较，零分配（大仓搜索时每行一次 to_lowercase 是主要开销）
        // - 其余：回退到小写副本匹配（Unicode 小写化会改变字节长度，如 İ→i̇、ẞ→ß，必须再映射回原行）
        let char_idx = if case_sensitive {
            line.find(q).map(|p| line[..p].chars().count())
        } else if line.is_ascii() && q.is_ascii() {
            let lb = line.as_bytes();
            let qb = q.as_bytes();
            if qb.is_empty() || qb.len() > lb.len() {
                None
            } else {
                lb.windows(qb.len()).position(|w| w.eq_ignore_ascii_case(qb))
            }
        } else {
            let hay = line.to_lowercase();
            hay.find(q).map(|p| hay[..p].chars().count())
        };
        if let Some(char_idx) = char_idx {
            let start_char = char_idx.saturating_sub(SNIPPET_MARGIN);
            let end_char = (char_idx + q_char_count + SNIPPET_MARGIN).min(line.chars().count());
            let mut snippet: String = line.chars().skip(start_char).take(end_char - start_char).collect();
            if start_char > 0 { snippet.insert(0, '…'); }
            hits.push(SearchResultItem {
                path: path.to_string_lossy().replace('\\', "/"),
                name: path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                line: idx + 1,
                snippet,
            });
        }
    }
    if hits.is_empty() { None } else { Some(hits) }
}
