use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::State;
use crate::AppState;
use crate::commands::editor::check_path_allowed;

/// 单条搜索结果
#[derive(Debug, Serialize)]
pub struct SearchResultItem {
    pub path: String,
    pub name: String,
    pub line: usize,
    pub snippet: String,
}

/// 搜索响应
#[derive(Debug, Serialize)]
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

    // 代数递增：新搜索一旦发起，正在跑的旧搜索会尽早自行退出（原实现里
    // "改一次关键词"就会多一个跑满 2 万文件的阻塞任务排队，互相叠着占线程）
    let my_epoch = state.search_epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    let epoch_handle = std::sync::Arc::clone(&state.search_epoch);

    // 同步遍历 + 读文件放 spawn_blocking，避免阻塞主线程
    tokio::task::spawn_blocking(move || {
        search_in_dir(&root, &q, params.case_sensitive, &exts, max_results, Some((epoch_handle, my_epoch)))
    })
    .await
    .map_err(|e| format!("搜索失败: {}", e))?
}

/// 同步递归搜索（在 spawn_blocking 中执行）
///
/// `cancel`：(共享代数, 本次代数)。每次扫描若干文件检查一次，若代数已被更新的搜索取代
/// 就提前返回（结果由前端按请求序号丢弃，这里只是别让旧任务继续占满 IO/CPU）。
fn search_in_dir(
    root: &str,
    q: &str,
    case_sensitive: bool,
    exts: &[String],
    max_results: usize,
    cancel: Option<(std::sync::Arc<std::sync::atomic::AtomicU64>, u64)>,
) -> Result<SearchResponse, String> {
    /// 每扫描多少个文件检查一次取消（检查本身是原子读，频率不用太高）
    const CANCEL_CHECK_EVERY: usize = 256;
    let superseded = |scanned: usize| -> bool {
        match &cancel {
            Some((handle, my)) => {
                scanned.is_multiple_of(CANCEL_CHECK_EVERY)
                    && handle.load(std::sync::atomic::Ordering::Relaxed) != *my
            }
            None => false,
        }
    };
    // 单文件搜索（目录树文件节点右键）：搜文件自身
    let root_path = PathBuf::from(root);
    if root_path.is_file() {
        let mut results = Vec::new();
        let truncated = false;
        if let Some(bytes) = read_searchable(&root_path, &root_path, exts) {
            if let Some(hits) = search_bytes(&root_path, &bytes, q, case_sensitive, max_results) {
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
                // 被更新的搜索取代：提前退出（结果前端本来就会丢弃）
                if superseded(scanned) { truncated = true; break; }
                // 一次 open：候选判定（大小/扩展名/NUL 探测）与逐行搜索共用同一份字节，
                // 原实现先 stat + open 读 8KB 探测、再 open 整读一遍——同一文件两次打开、
                // 前 8KB 读两次（上限 2 万文件时最坏多出 2 万次 open）
                if let Some(bytes) = read_searchable(&root_path, &path, exts) {
                    if let Some(hits) = search_bytes(&path, &bytes, q, case_sensitive, max_results - results.len()) {
                        results.extend(hits);
                    }
                }
            }
        }
        for d in subdirs.into_iter().rev() { stack.push(d); }
        if scanned > MAX_SCAN_FILES { truncated = true; break; }
    }

    results.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(SearchResponse { results, truncated })
}

/// 候选文件读取 + 可搜索判定（一次 open 完成）。
///
/// 判定顺序：扩展名筛选（不读盘）→ 打开并**按句柄复核真实目标仍在搜索根内**（SEC-4：
/// 目录遍历与实际读取之间，项目内并发脚本可以把某个路径换成指向外面的 junction，
/// 若不复核就会把白名单外文件的内容作为命中片段返回给界面）→ 整读（受大小上限约束）
/// → 头部 NUL 探测。返回 `None` 表示跳过该文件（过大/非目标类型/二进制/读失败/越界）。
fn read_searchable(root: &Path, path: &Path, exts: &[String]) -> Option<Vec<u8>> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let ext = Path::new(name).extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    // 先按名字过滤：命中排除项就不必打开文件
    if !exts.is_empty() && !exts.iter().any(|e| e == &ext) { return None; }
    if BINARY_EXTENSIONS.contains(&ext.as_str()) { return None; }

    use std::io::Read;
    // 一次 open，后续读取都在这条已复核的句柄上
    let file = std::fs::File::open(path).ok()?;
    let real = crate::commands::editor::final_path_of_handle(&file).ok()?;
    // 复核基准同样取"内核解析后的根"，两边同为 verbatim 形式才能正确比较
    let real_root = std::fs::canonicalize(root).ok()?;
    if !real.starts_with(&real_root) { return None; }

    let metadata = file.metadata().ok()?;
    if metadata.len() == 0 || metadata.len() > MAX_SEARCH_FILE_SIZE { return None; }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    // 上限 +1 字节：读满即说明文件比 metadata 报的更大，下面按实际长度复核
    file.take(MAX_SEARCH_FILE_SIZE + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_SEARCH_FILE_SIZE { return None; }
    // 二进制检测：头部含 NUL 视为二进制（原先单独读 8KB 探测，现在用已读入的字节）
    let probe = &bytes[..bytes.len().min(BINARY_PROBE_SIZE)];
    if probe.contains(&0) { return None; }
    Some(bytes)
}

/// 在单文件的**已读入字节**中逐行搜索，返回命中行
fn search_bytes(
    path: &Path,
    bytes: &[u8],
    q: &str,
    case_sensitive: bool,
    limit: usize,
) -> Option<Vec<SearchResultItem>> {
    // UTF-8 严格解码失败 → 回退 GB18030（GBK 超集）：与编辑器读取（decode_text）对齐，
    // 否则 Windows 中文老项目的 GBK 文件在搜索中静默缺失
    let content = match std::str::from_utf8(bytes) {
        Ok(c) => c.to_string(),
        Err(_) => encoding_rs::GB18030.decode(bytes).0.into_owned(),
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
            find_ci_char_idx(line, q)
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

/// 在 `line` 中做 Unicode 大小写不敏感查找，返回命中起点在**原行**里的字符下标。
///
/// 为什么不能"在小写副本上 find，再把位置用回原行"：Unicode 小写化会改变字符数
/// （`İ`(1 字符) → `i̇`(2 字符)、`ẞ` → `ß` 等），小写副本里的字符下标与原始行不再一一对应，
/// 片段会整体错位（原实现正是如此）。这里逐字符小写化并记录每个小写字符的来源下标，
/// 从而把命中位置准确地映射回原行的字符网格。
fn find_ci_char_idx(line: &str, query_lower: &str) -> Option<usize> {
    let mut lower = String::with_capacity(line.len());
    // origin[i] = 小写串第 i 个字符来自原行的第几个字符
    let mut origin: Vec<usize> = Vec::with_capacity(line.len());
    for (oi, ch) in line.chars().enumerate() {
        for lc in ch.to_lowercase() {
            lower.push(lc);
            origin.push(oi);
        }
    }
    let byte_pos = lower.find(query_lower)?;
    let ch_pos = lower[..byte_pos].chars().count();
    origin.get(ch_pos).copied()
}

/* ---- Tests ---- */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_ci_char_idx_ascii() {
        // ASCII 行：下标与字节一致
        assert_eq!(find_ci_char_idx("hello WORLD", "world"), Some(6));
        assert_eq!(find_ci_char_idx("hello", "zzz"), None);
    }

    #[test]
    fn test_find_ci_char_idx_unicode_prefix_shifts() {
        // 回归：`İ` 小写化后变成 2 个字符（i + U+0307 组合点），于是小写副本里
        // "target" 的字符下标是 4，而它在**原行**里只是第 3 个字符。
        // 原实现直接把小写副本的下标用到原行 → 片段从 "arget" 开始（整体错位 1）。
        let line = "İ  target";
        let idx = find_ci_char_idx(line, "target").expect("应命中");
        assert_eq!(idx, 3, "命中位置应映射回原行的第 3 个字符");
        let got: String = line.chars().skip(idx).take(6).collect();
        assert_eq!(got, "target", "按映射后的下标切片应取到原文");
    }

    #[test]
    fn test_find_ci_char_idx_after_cjk_prefix() {
        // 非膨胀的 Unicode 前缀（CJK 逐字符小写化不变）：下标照常等于原行字符序号
        let line = "中文 target 结尾";
        let idx = find_ci_char_idx(line, "target").expect("应命中");
        assert_eq!(idx, 3);
        let got: String = line.chars().skip(idx).take(6).collect();
        assert_eq!(got, "target");
    }
}
