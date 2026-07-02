use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::State;
use crate::AppState;

/// 在系统资源管理器中打开路径
#[tauri::command]
pub fn open_in_explorer(path: String) -> Result<(), String> {
    let path_buf = PathBuf::from(&path);
    let target = if path_buf.is_dir() {
        path.clone()
    } else {
        // 文件：打开所在目录
        path_buf.parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone())
    };

    log::info!("[nexus] 打开资源管理器: {}", target);

    #[cfg(target_os = "windows")]
    {
        // Windows: 使用 cmd /c start 处理中文路径
        let win_path = target.replace('/', "\\");
        std::process::Command::new("cmd")
            .args(["/c", "start", "", &win_path])
            .spawn()
            .map_err(|e| format!("打开资源管理器失败: {}", e))?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&target)
            .spawn()
            .map_err(|e| format!("打开 Finder 失败: {}", e))?;
    }
    #[cfg(target_os = "linux")]
    {
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

/// 设置当前项目根路径（安全白名单）
#[tauri::command]
pub fn set_project_root(state: State<AppState>, path: Option<String>) -> Result<(), String> {
    let mut root = state.project_root.lock().map_err(|e| format!("获取项目根路径锁失败: {}", e))?;
    *root = path;
    Ok(())
}

/// 检查路径是否在允许范围内
fn is_path_allowed(requested: &str, allowed_root: &Option<String>) -> bool {
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

/// 读取文件内容
#[tauri::command]
pub async fn read_file(state: State<'_, AppState>, path: String) -> Result<String, String> {
    // 在 await 前释放锁，确保 future 是 Send
    {
        let root = state.project_root.lock().map_err(|e| format!("获取项目根路径锁失败: {}", e))?;
        if !is_path_allowed(&path, &root) {
            return Err("访问被拒绝".into());
        }
    }
    tokio::fs::read_to_string(&path).await.map_err(|e| format!("无法读取文件: {}", e))
}

/// 列出目录内容（允许浏览任意路径，供 FilePicker/FileTree 使用）
///
/// 安全设计：不限制读取范围，因为 FilePicker 需要浏览整个文件系统来选择项目目录。
/// 路径穿越防护由 `read_file` 的 `is_path_allowed()` 负责。
/// `list_directory` 仅返回文件元数据（名称、大小、类型），不暴露文件内容。
#[tauri::command]
pub async fn list_directory(path: String) -> Result<Vec<FileEntry>, String> {
    let dir = PathBuf::from(&path);
    if !dir.is_dir() {
        return Err("路径不是目录".to_string());
    }

    let mut entries = vec![];
    let mut read_dir = tokio::fs::read_dir(&dir).await.map_err(|e| format!("读取目录失败: {}", e))?;

    while let Some(entry) = read_dir.next_entry().await.map_err(|e| format!("读取条目失败: {}", e))? {
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
}
