use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::State;
use crate::AppState;

/// 在系统终端中打开路径（Windows: 新开 cmd 窗口并定位到目录）
#[tauri::command]
pub fn open_terminal(path: String) -> Result<(), String> {
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
#[tauri::command]
pub fn open_in_explorer(path: String) -> Result<(), String> {
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

/// 设置当前项目根路径（安全白名单）
#[tauri::command]
pub fn set_project_root(state: State<AppState>, path: Option<String>) -> Result<(), String> {
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

/// 读取文件大小上限（防止超大文件读入内存导致内存暴涨和 IPC 卡死）
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

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
    let meta = tokio::fs::metadata(&path).await.map_err(|e| format!("无法读取文件: {}", e))?;
    if meta.len() > MAX_FILE_SIZE {
        return Err(format!("文件过大（{:.1} MB），超过 10 MB 上限", meta.len() as f64 / (1024.0 * 1024.0)));
    }
    tokio::fs::read_to_string(&path).await.map_err(|e| format!("无法读取文件: {}", e))
}

/// 写入文件内容（与 read_file 同款安全校验：项目白名单 + 大小上限）
#[tauri::command]
pub async fn write_file(state: State<'_, AppState>, path: String, content: String) -> Result<(), String> {
    // 在 await 前释放锁，确保 future 是 Send
    {
        let root = state.project_root.lock().map_err(|e| format!("获取项目根路径锁失败: {}", e))?;
        if !is_path_allowed(&path, &root) {
            return Err("访问被拒绝".into());
        }
    }
    if (content.len() as u64) > MAX_FILE_SIZE {
        return Err(format!("文件过大（{:.1} MB），超过 10 MB 上限", content.len() as f64 / (1024.0 * 1024.0)));
    }
    tokio::fs::write(&path, content).await.map_err(|e| format!("无法写入文件: {}", e))
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
