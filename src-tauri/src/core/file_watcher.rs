use notify::{Event, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct FileWatcher {
    watchers: Mutex<HashMap<String, WatcherState>>,
}

struct WatcherState {
    _watcher: notify::RecommendedWatcher,
    stop_tx: Sender<()>,
    listener_handle: Option<std::thread::JoinHandle<()>>,
    /// 当前正在监听的服务列表（用于追加/移除单服务）
    services: Vec<ServiceWatchConfig>,
}

/// 单个服务的监听配置
#[derive(Debug, Clone)]
pub struct ServiceWatchConfig {
    pub id: String,
    pub name: String,
    pub paths: Vec<String>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    /// 0=关闭监听, 1=确认重启, 2=自动重启（随事件透传给前端）
    pub restart_mode: i32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileChangeEvent {
    pub project_id: String,
    pub project_name: String,
    pub changes: Vec<FileChange>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileChange {
    pub path: String,
    pub service_name: String,
    pub service_id: String,
    pub kind: String,
    /// 0=关闭监听, 1=确认重启, 2=自动重启（前端据此决定是否直接重启）
    pub restart_mode: i32,
}

impl FileWatcher {
    pub fn new() -> Self {
        Self { watchers: Mutex::new(HashMap::new()) }
    }

    /// 启动监听指定项目
    pub fn start_watching(
        &self,
        project_id: &str,
        project_name: &str,
        services: &[ServiceWatchConfig],
        on_change: impl Fn(FileChangeEvent) + Send + 'static,
    ) -> Result<(), String> {
        let _ = self.stop_watching(project_id);

        let mut unique_paths: Vec<PathBuf> = Vec::new();
        for svc in services {
            for p in &svc.paths {
                let path = PathBuf::from(p);
                if path.exists() && !unique_paths.contains(&path) {
                    unique_paths.push(path);
                }
            }
        }

        if unique_paths.is_empty() {
            // 静默跳过会表现为"改了文件什么都没发生"，显式记录无效路径便于诊断
            let total: usize = services.iter().map(|s| s.paths.len()).sum();
            let invalid: Vec<&str> = services.iter().flat_map(|s| s.paths.iter().map(|p| p.as_str()))
                .filter(|p| !Path::new(p).exists()).collect();
            log::warn!(
                "文件监听未启动: 有效路径 0/{total}（无效路径 {:?}，可能路径不存在或全部服务未启用监听）",
                invalid
            );
            return Ok(());
        }

        // channel 用于接收文件事件和停止信号（有界：接收端慢时丢弃旧事件，避免无限堆积）
        let (event_tx, event_rx) = mpsc::sync_channel::<notify::Result<Event>>(1024);
        let (stop_tx, stop_rx) = mpsc::channel::<()>();

        let mut watcher = notify::recommended_watcher(move |res| {
            let _ = event_tx.send(res);
        }).map_err(|e| format!("创建文件监听器失败: {}", e))?;

        for path in &unique_paths {
            watcher.watch(path, RecursiveMode::Recursive)
                .map_err(|e| format!("监听路径失败 {}: {}", path.display(), e))?;
        }

        let pid = project_id.to_string();
        let pname = project_name.to_string();
        let svc_map: Vec<ServiceWatchConfig> = services.to_vec();

        let listener_handle = std::thread::spawn(move || {
            let debounce = Duration::from_millis(500);
            let mut pending: HashMap<String, String> = HashMap::new();
            let mut last_flush = Instant::now();

            loop {
                let mut should_flush = false;
                match event_rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(Ok(event)) => {
                        let kind = event_kind_str(&event.kind);
                        for path in &event.paths {
                            if path.is_dir() { continue; }
                            // 预过滤：有服务需要该路径才入队（防 node_modules 等全员排除目录洪泛）
                            if should_queue_event(path, &svc_map) {
                                let p = path.to_string_lossy().replace('\\', "/");
                                pending.insert(p, kind.to_string());
                            }
                        }
                        // 持续事件流下（recv 从不超时）也要在去抖间隔后及时 flush，
                        // 否则 pending 永不发送且无限累积
                        should_flush = !pending.is_empty() && last_flush.elapsed() >= debounce;
                    }
                    Ok(Err(_)) => {}
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if stop_rx.try_recv().is_ok() { break; }
                        should_flush = !pending.is_empty() && last_flush.elapsed() >= debounce;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
                if should_flush {
                    let changes = match_changes(&svc_map, &pending);
                    if !changes.is_empty() {
                        for c in &changes {
                            log::trace!("文件变更 [{}] {}: {}", c.kind, c.service_name, c.path);
                        }
                        on_change(FileChangeEvent {
                            project_id: pid.clone(),
                            project_name: pname.clone(),
                            changes,
                        });
                    }
                    pending.clear();
                    last_flush = Instant::now();
                }
            }
        });

        self.watchers.lock().map_err(|e| format!("FileWatcher watchers 锁获取失败: {}", e))?
            .insert(project_id.to_string(), WatcherState { _watcher: watcher, stop_tx, listener_handle: Some(listener_handle), services: services.to_vec() });

        log::info!("文件监听已启动: project={} (id={})", project_name, project_id);
        Ok(())
    }

    pub fn stop_watching(&self, project_id: &str) -> Result<(), String> {
        // 先从 map 中移除 entry 并释放锁，再 join 线程，避免死锁
        let state = {
            let mut watchers = self.watchers.lock().map_err(|e| format!("FileWatcher watchers 锁获取失败: {}", e))?;
            watchers.remove(project_id)
        };
        if let Some(mut state) = state {
            let _ = state.stop_tx.send(());
            if let Some(handle) = state.listener_handle.take() {
                let _ = handle.join();
            }
        }
        Ok(())
    }

    /// 获取项目当前正在监听的服务列表
    pub fn get_watched_services(&self, project_id: &str) -> Option<Vec<ServiceWatchConfig>> {
        let watchers = self.watchers.lock().ok()?;
        watchers.get(project_id).map(|s| s.services.clone())
    }

    /// 从项目监听中移除指定服务，返回剩余的服务列表
    pub fn remove_service_from_watching(&self, project_id: &str, service_id: &str) -> Vec<ServiceWatchConfig> {
        let mut watchers = match self.watchers.lock() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        if let Some(state) = watchers.get_mut(project_id) {
            state.services.retain(|s| s.id != service_id);
            state.services.clone()
        } else {
            Vec::new()
        }
    }

    pub fn stop_all(&self) {
        let handles: Vec<_> = {
            let mut w = match self.watchers.lock() {
                Ok(guard) => guard,
                Err(e) => {
                    log::error!("FileWatcher watchers 锁已中毒: {}", e);
                    e.into_inner()
                }
            };
            w.drain().filter_map(|(_, mut state)| {
                let _ = state.stop_tx.send(());
                state.listener_handle.take()
            }).collect()
        };
        for h in handles {
            let _ = h.join();
        }
    }

}

impl Drop for FileWatcher {
    fn drop(&mut self) {
        self.stop_all();
    }
}

// ─── helpers ───────────────────────────────────────────────

/// 规范化路径（统一 /）
fn normalize(path: &str) -> String {
    path.replace('\\', "/")
}

/// 规范化路径并确保以 / 结尾（用于前缀匹配，防止 /project 匹配 /projectile）
fn normalize_prefix(path: &str) -> String {
    let mut p = path.replace('\\', "/");
    if !p.ends_with('/') {
        p.push('/');
    }
    p
}

/// 检查路径是否匹配 glob 模式
///
/// 支持的语法子集：`*`（匹配任意）、`*.ext`（按扩展名匹配）、
/// 无通配符（精确匹配目录名或目录前缀）。`src/**/*.ts` 这类复杂
/// glob 不支持，会静默不匹配——需要完整 glob 时引入 globset crate。
fn glob_match(pattern: &str, name: &str) -> bool {
    if pattern == "*" { return true; }
    if !pattern.contains('*') {
        return name == pattern || name.starts_with(&format!("{}/", pattern));
    }
    // 简单 glob: *.ext 匹配扩展名
    if let Some(ext) = pattern.strip_prefix("*.") {
        return name.ends_with(&format!(".{}", ext));
    }
    false
}

/// 单个服务对路径的接收判定：命中该服务的排除规则、或不在其包含列表中 → 不接收
///
/// 注意：判定是 per-service 的。路径被服务 A 排除不影响同目录的服务 B 接收它，
/// 排除/包含规则不得跨服务串扰（之前全局"任一排除即丢弃"会让 B 漏事件）。
fn service_accepts(svc: &ServiceWatchConfig, path_str: &str, components: &[&str]) -> bool {
    // 检查排除规则
    for ex in &svc.exclude {
        let ex = ex.trim();
        if ex.is_empty() { continue; }
        // 目录名精确匹配（如 node_modules, .git）
        if !ex.contains('*') && components.contains(&ex) {
            return false;
        }
        // glob 匹配文件名
        if let Some(name) = path_str.rsplit('/').next() {
            if glob_match(ex, name) { return false; }
        }
    }

    // 检查包含规则（如果有非 * 的规则）
    let has_include = svc.include.iter().any(|i| i.trim() != "*" && !i.trim().is_empty());
    if has_include {
        if let Some(name) = path_str.rsplit('/').next() {
            let included = svc.include.iter().any(|inc| glob_match(inc.trim(), name));
            if !included { return false; } // 不在包含列表中 → 不接收
        }
    }
    true
}

/// 预过滤方向判定：路径是否值得进入待发送队列
///
/// true = 至少一个归属服务接收该路径（需要事件）；false = 无人需要（丢弃）。
/// 入队条件直接用本函数本身（`if should_queue_event(...)`），**不要取反**——取反会让
/// 有服务需要的文件全部被丢弃、被全员排除的目录反而入队，表现为"改文件永远不触发"。
fn should_queue_event(path: &Path, services: &[ServiceWatchConfig]) -> bool {
    path_needed_by_any(path, services)
}

/// 路径是否至少被一个归属服务接收（粗过滤：全部归属服务都拒绝或无人归属才丢弃）
fn path_needed_by_any(path: &Path, services: &[ServiceWatchConfig]) -> bool {
    let path_str = normalize(&path.to_string_lossy());
    // 提取路径各组件用于目录名匹配
    let components: Vec<&str> = path_str.split('/').collect();
    services.iter().any(|svc| {
        let in_watch = svc.paths.iter().any(|wp| path_str.starts_with(&normalize_prefix(wp)));
        in_watch && service_accepts(svc, &path_str, &components)
    })
}

/// 将变更路径归属到对应服务
///
/// 归属判定 per-service：路径须在该服务监听范围内**且**被其规则接收
/// （接收由 service_accepts 判定——服务 A 的排除不得压制服务 B 的事件）。
fn match_changes(
    services: &[ServiceWatchConfig],
    paths: &HashMap<String, String>,
) -> Vec<FileChange> {
    let mut changes = Vec::new();
    for (path_str, kind) in paths {
        let components: Vec<&str> = path_str.split('/').collect();
        let mut matched = false;
        for svc in services {
            let in_watch = svc.paths.iter().any(|wp| path_str.starts_with(&normalize_prefix(wp)));
            if !in_watch { continue; }
            if !service_accepts(svc, path_str, &components) { continue; }
            changes.push(FileChange {
                path: path_str.clone(),
                service_name: svc.name.clone(),
                service_id: svc.id.clone(),
                kind: kind.clone(),
                restart_mode: svc.restart_mode,
            });
            matched = true;
            break;
        }
        if !matched {
            changes.push(FileChange {
                path: path_str.clone(),
                service_name: "(project)".to_string(),
                service_id: String::new(),
                kind: kind.clone(),
                restart_mode: 0,
            });
        }
    }
    changes
}

/// 将 notify 事件类型映射为字符串
fn event_kind_str(kind: &notify::EventKind) -> &str {
    use notify::EventKind;
    match kind {
        EventKind::Create(_) => "create",
        EventKind::Modify(_) => "modify",
        EventKind::Remove(_) => "delete",
        EventKind::Any => "modify",
        _ => "modify",
    }
}

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── normalize ──────────────────────────────────────────

    #[test]
    fn test_normalize_forward_slash() {
        assert_eq!(normalize("/home/user/file.txt"), "/home/user/file.txt");
    }

    #[test]
    fn test_normalize_backslash() {
        assert_eq!(normalize("C:\\Users\\file.txt"), "C:/Users/file.txt");
    }

    #[test]
    fn test_normalize_mixed() {
        assert_eq!(normalize("C:\\Users/file.txt"), "C:/Users/file.txt");
    }

    // ── normalize_prefix ───────────────────────────────────

    #[test]
    fn test_normalize_prefix_adds_trailing_slash() {
        assert_eq!(normalize_prefix("/home/user"), "/home/user/");
    }

    #[test]
    fn test_normalize_prefix_already_has_slash() {
        assert_eq!(normalize_prefix("/home/user/"), "/home/user/");
    }

    #[test]
    fn test_normalize_prefix_backslash() {
        assert_eq!(normalize_prefix("C:\\Users"), "C:/Users/");
    }

    // ── glob_match ─────────────────────────────────────────

    #[test]
    fn test_glob_match_star() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*", ""));
    }

    #[test]
    fn test_glob_match_exact() {
        assert!(glob_match("node_modules", "node_modules"));
        assert!(!glob_match("node_modules", "dist"));
    }

    #[test]
    fn test_glob_match_directory_prefix() {
        assert!(glob_match("node_modules", "node_modules/express"));
        assert!(!glob_match("node_modules", "node_modules_extra"));
    }

    #[test]
    fn test_glob_match_extension() {
        assert!(glob_match("*.log", "debug.log"));
        assert!(glob_match("*.log", "error.log"));
        assert!(!glob_match("*.log", "debug.txt"));
    }

    // ── path_needed_by_any / service_accepts ────────────────

    fn make_svc(id: &str, paths: Vec<&str>, exclude: Vec<&str>, include: Vec<&str>) -> ServiceWatchConfig {
        ServiceWatchConfig {
            id: id.to_string(),
            name: id.to_string(),
            paths: paths.into_iter().map(String::from).collect(),
            include: include.into_iter().map(String::from).collect(),
            exclude: exclude.into_iter().map(String::from).collect(),
            restart_mode: 0,
        }
    }

    #[test]
    fn test_needed_ignore_node_modules() {
        let svc = make_svc("s1", vec!["/project"], vec!["node_modules"], vec!["*"]);
        assert!(!path_needed_by_any(Path::new("/project/node_modules/express/index.js"), &[svc]));
    }

    #[test]
    fn test_needed_normal_file() {
        let svc = make_svc("s1", vec!["/project"], vec!["node_modules"], vec!["*"]);
        assert!(path_needed_by_any(Path::new("/project/src/index.js"), &[svc]));
    }

    #[test]
    fn test_needed_outside_watch_path() {
        let svc = make_svc("s1", vec!["/project"], vec!["node_modules"], vec!["*"]);
        // 路径不在监听范围内：无归属服务 → 丢弃
        assert!(!path_needed_by_any(Path::new("/other/node_modules/express/index.js"), &[svc]));
    }

    #[test]
    fn test_needed_glob_pattern() {
        let svcs = vec![make_svc("s1", vec!["/project"], vec!["*.log"], vec!["*"])];
        assert!(!path_needed_by_any(Path::new("/project/debug.log"), &svcs));
        assert!(path_needed_by_any(Path::new("/project/debug.txt"), &svcs));
    }

    #[test]
    fn test_needed_include_filter() {
        let svcs = vec![make_svc("s1", vec!["/project"], vec![], vec!["*.ts"])];
        // 有非 * 的 include 规则，不在 include 中的文件应被忽略
        assert!(!path_needed_by_any(Path::new("/project/index.js"), &svcs));
        assert!(path_needed_by_any(Path::new("/project/index.ts"), &svcs));
    }

    #[test]
    fn test_needed_git_directory() {
        let svc = make_svc("s1", vec!["/project"], vec![".git"], vec!["*"]);
        assert!(!path_needed_by_any(Path::new("/project/.git/config"), &[svc]));
    }

    // ── should_queue_event（回归：入队方向，取反会让正常文件全部丢失）──

    #[test]
    fn test_queue_direction_source_file_queued() {
        // 源码文件被服务需要 → 必须入队
        let svc = make_svc("s1", vec!["/project"], vec!["node_modules"], vec!["*"]);
        assert!(should_queue_event(Path::new("/project/src/index.ts"), &[svc]));
    }

    #[test]
    fn test_queue_direction_excluded_dir_dropped() {
        // 被服务排除的目录（node_modules）→ 不入队（防洪泛）
        let svc = make_svc("s1", vec!["/project"], vec!["node_modules"], vec!["*"]);
        assert!(!should_queue_event(Path::new("/project/node_modules/pkg/index.js"), &[svc]));
    }

    // ── 跨服务规则串扰（回归：A 的排除/包含不得压制 B 的事件）──

    #[test]
    fn test_cross_service_exclude_does_not_leak() {
        // A 排除 *.log，B 全收：.log 事件应由 B 接收，而不是全局被 A 丢弃
        let svcs = vec![
            make_svc("A", vec!["/project"], vec!["*.log"], vec!["*"]),
            make_svc("B", vec!["/project"], vec![], vec!["*"]),
        ];
        assert!(path_needed_by_any(Path::new("/project/a.log"), &svcs), "B 应接收该路径");

        let mut paths = HashMap::new();
        paths.insert("/project/a.log".to_string(), "modify".to_string());
        let changes = match_changes(&svcs, &paths);
        assert_eq!(changes.len(), 1, "A 被自身规则排除，不应产生事件");
        assert_eq!(changes[0].service_id, "B");
        assert_eq!(changes[0].path, "/project/a.log");
    }

    #[test]
    fn test_cross_service_include_does_not_leak() {
        // A 只收 *.ts，B 全收：.js 事件 B 应收到，不被 A 的包含规则拦截
        let svcs = vec![
            make_svc("A", vec!["/project"], vec![], vec!["*.ts"]),
            make_svc("B", vec!["/project"], vec![], vec!["*"]),
        ];
        let mut paths = HashMap::new();
        paths.insert("/project/app.js".to_string(), "modify".to_string());
        let changes = match_changes(&svcs, &paths);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].service_id, "B");
    }

    #[test]
    fn test_match_changes_all_excluded_falls_back_to_project() {
        // 全部归属服务都拒绝该路径（预过滤已拦截，兜底场景）→ "(project)" 事件
        let svcs = vec![make_svc("A", vec!["/project"], vec!["*.log"], vec!["*"])];
        let mut paths = HashMap::new();
        paths.insert("/project/a.log".to_string(), "modify".to_string());
        let changes = match_changes(&svcs, &paths);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].service_name, "(project)");
    }
}
