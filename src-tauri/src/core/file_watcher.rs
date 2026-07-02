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
}

/// 单个服务的监听配置
#[derive(Debug, Clone)]
pub struct ServiceWatchConfig {
    pub id: String,
    pub name: String,
    pub paths: Vec<String>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
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
            return Ok(());
        }

        // channel 用于接收文件事件和停止信号
        let (event_tx, event_rx) = mpsc::channel::<notify::Result<Event>>();
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
                match event_rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(Ok(event)) => {
                        let kind = event_kind_str(&event.kind);
                        for path in &event.paths {
                            if path.is_dir() { continue; }
                            if !should_ignore_path(path, &svc_map) {
                                let p = path.to_string_lossy().replace('\\', "/");
                                pending.insert(p, kind.to_string());
                            }
                        }
                    }
                    Ok(Err(_)) => {}
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if stop_rx.try_recv().is_ok() { break; }
                        if !pending.is_empty() && last_flush.elapsed() >= debounce {
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
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });

        self.watchers.lock().map_err(|e| format!("FileWatcher watchers 锁获取失败: {}", e))?
            .insert(project_id.to_string(), WatcherState { _watcher: watcher, stop_tx, listener_handle: Some(listener_handle) });

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

    pub fn stop_all(&self) {
        let handles: Vec<_> = {
            let mut w = match self.watchers.lock() {
                Ok(guard) => guard,
                Err(e) => {
                    log::error!("FileWatcher watchers 锁已中毒: {}", e);
                    e.into_inner()
                }
            };
            w.drain().map(|(_, mut state)| {
                let _ = state.stop_tx.send(());
                state.listener_handle.take()
            }).flatten().collect()
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

/// 检查路径是否匹配 glob 模式（支持 *）
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

/// 路径是否被所有服务的排除规则共同忽略
fn should_ignore_path(path: &Path, services: &[ServiceWatchConfig]) -> bool {
    let path_str = normalize(&path.to_string_lossy());
    // 提取路径各组件用于目录名匹配
    let components: Vec<&str> = path_str.split('/').collect();

    // 检查每个服务的排除规则
    for svc in services {
        // 先检查路径是否在服务的监听范围内
        let in_watch = svc.paths.iter().any(|wp| path_str.starts_with(&normalize_prefix(wp)));
        if !in_watch { continue; }

        // 检查排除规则
        for ex in &svc.exclude {
            let ex = ex.trim();
            if ex.is_empty() { continue; }
            // 目录名精确匹配（如 node_modules, .git）
            if !ex.contains('*') && components.contains(&ex) {
                return true;
            }
            // glob 匹配文件名
            if let Some(name) = path_str.rsplit('/').next() {
                if glob_match(ex, name) { return true; }
            }
        }

        // 检查包含规则（如果有非 * 的规则）
        let has_include = svc.include.iter().any(|i| i.trim() != "*" && !i.trim().is_empty());
        if has_include {
            if let Some(name) = path_str.rsplit('/').next() {
                let included = svc.include.iter().any(|inc| glob_match(inc.trim(), name));
                if !included { return true; } // 不在包含列表中 → 忽略
            }
        }
    }
    false
}

/// 将变更路径归属到对应服务
fn match_changes(
    services: &[ServiceWatchConfig],
    paths: &HashMap<String, String>,
) -> Vec<FileChange> {
    let mut changes = Vec::new();
    for (path_str, kind) in paths {
        let mut matched = false;
        for svc in services {
            if svc.paths.iter().any(|wp| path_str.starts_with(&normalize_prefix(wp))) {
                changes.push(FileChange {
                    path: path_str.clone(),
                    service_name: svc.name.clone(),
                    service_id: svc.id.clone(),
                    kind: kind.clone(),
                });
                matched = true;
                break;
            }
        }
        if !matched {
            changes.push(FileChange {
                path: path_str.clone(),
                service_name: "(project)".to_string(),
                service_id: String::new(),
                kind: kind.clone(),
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

    // ── should_ignore_path ─────────────────────────────────

    fn make_svc(id: &str, paths: Vec<&str>, exclude: Vec<&str>, include: Vec<&str>) -> ServiceWatchConfig {
        ServiceWatchConfig {
            id: id.to_string(),
            name: id.to_string(),
            paths: paths.into_iter().map(String::from).collect(),
            include: include.into_iter().map(String::from).collect(),
            exclude: exclude.into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn test_should_ignore_node_modules() {
        let svc = make_svc("s1", vec!["/project"], vec!["node_modules"], vec!["*"]);
        assert!(should_ignore_path(Path::new("/project/node_modules/express/index.js"), &[svc]));
    }

    #[test]
    fn test_should_not_ignore_normal_file() {
        let svc = make_svc("s1", vec!["/project"], vec!["node_modules"], vec!["*"]);
        assert!(!should_ignore_path(Path::new("/project/src/index.js"), &[svc]));
    }

    #[test]
    fn test_should_ignore_outside_watch_path() {
        let svc = make_svc("s1", vec!["/project"], vec!["node_modules"], vec!["*"]);
        // 路径不在监听范围内，不处理（返回 false）
        assert!(!should_ignore_path(Path::new("/other/node_modules/express/index.js"), &[svc]));
    }

    #[test]
    fn test_should_ignore_glob_pattern() {
        let svcs = vec![make_svc("s1", vec!["/project"], vec!["*.log"], vec!["*"])];
        assert!(should_ignore_path(Path::new("/project/debug.log"), &svcs));
        assert!(!should_ignore_path(Path::new("/project/debug.txt"), &svcs));
    }

    #[test]
    fn test_should_ignore_include_filter() {
        let svcs = vec![make_svc("s1", vec!["/project"], vec![], vec!["*.ts"])];
        // 有非 * 的 include 规则，不在 include 中的文件应被忽略
        assert!(should_ignore_path(Path::new("/project/index.js"), &svcs));
        assert!(!should_ignore_path(Path::new("/project/index.ts"), &svcs));
    }

    #[test]
    fn test_should_ignore_git_directory() {
        let svc = make_svc("s1", vec!["/project"], vec![".git"], vec!["*"]);
        assert!(should_ignore_path(Path::new("/project/.git/config"), &[svc]));
    }
}
