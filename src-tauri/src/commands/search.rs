use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::State;
use crate::AppState;
use crate::commands::editor::is_path_allowed;

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
    // 路径白名单校验（在 await 前释放锁，确保 future 是 Send）
    {
        let project_root = state.project_root.lock().map_err(|e| format!("获取项目根路径锁失败: {}", e))?;
        if !is_path_allowed(&root, &project_root) {
            return Err("访问被拒绝".into());
        }
    }

    let exts: Vec<String> = params.extensions.iter()
        .map(|e| e.trim().trim_start_matches('.').to_lowercase())
        .filter(|e| !e.is_empty())
        .collect();
    // 大小写不敏感时统一小写，行内匹配用同一规则
    let q = if params.case_sensitive { query } else { query.to_lowercase() };
    let max_results = params.max_results.max(1).min(5000);

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
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name().to_string_lossy().to_lowercase();
                if DEFAULT_EXCLUDE_DIRS.contains(&name.as_str()) { continue; }
                subdirs.push((path, depth + 1));
            } else if path.is_file() {
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
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return None, // 非 UTF-8 或读取失败：跳过
    };
    let mut hits = Vec::new();
    let q_char_count = q.chars().count();
    for (idx, line) in content.lines().enumerate() {
        if hits.len() >= limit { break; }
        let hay = if case_sensitive { line.to_string() } else { line.to_lowercase() };
        if let Some(pos) = hay.find(q) {
            // hay 与 line 的字节偏移可能不一致（to_lowercase 会改变个别字符的字节长度，
            // 如 İ→i̇、ẞ→ß），直接用字节偏移切原行会落在字符中间 panic。
            // 解法：用字符序号映射回原行，再按字符边界切片
            let char_idx = hay[..pos].chars().count();
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
