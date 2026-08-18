//! 文件剪贴板操作：与系统资源管理器互通复制粘贴（Windows）
//!
//! 复制：把文件/文件夹路径写入系统剪贴板（CF_HDROP），在资源管理器中 Ctrl+V 即可粘贴。
//! 粘贴：读取系统剪贴板中的文件列表，复制到目标目录（同名自动追加 " (2)" 序号）。
//! 其他平台暂不支持（macOS/系统剪贴板文件格式无对应实现）。

use std::path::{Path, PathBuf};
use tauri::State;

use crate::commands::editor::is_path_allowed;
use crate::AppState;

/// 复制文件/文件夹到系统剪贴板（在资源管理器中 Ctrl+V 可粘贴）
#[tauri::command]
pub fn copy_files_to_clipboard(paths: Vec<String>) -> Result<(), String> {
    #[cfg(windows)]
    {
        use clipboard_win::Clipboard;

        // 过滤不存在的路径（可能已被删除），避免写入无效剪贴板条目
        let valid: Vec<String> = paths.into_iter().filter(|p| Path::new(p).exists()).collect();
        if valid.is_empty() {
            return Err("没有可复制的文件".into());
        }
        // new_attempts(10)：剪贴板被其他程序占用时最多重试 10 次
        let _clip = Clipboard::new_attempts(10).map_err(|e| format!("打开剪贴板失败: {}", e))?;
        clipboard_win::raw::set_file_list(&valid).map_err(|e| format!("写入剪贴板失败: {}", e))?;
        log::info!("[nexus] 复制 {} 个项目到系统剪贴板", valid.len());
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = paths;
        Err("当前平台暂不支持复制文件到系统剪贴板".into())
    }
}

/// 从系统剪贴板读取文件列表并复制到目标目录，返回新建的路径列表（供前端刷新树）
#[tauri::command]
pub async fn paste_files(state: State<'_, AppState>, target_dir: String) -> Result<Vec<String>, String> {
    // 在 await 前释放锁，确保 future 是 Send
    {
        let root = state.project_root.lock().map_err(|e| format!("获取项目根路径锁失败: {}", e))?;
        if !is_path_allowed(&target_dir, &root) {
            return Err("访问被拒绝".into());
        }
    }
    let target = PathBuf::from(&target_dir);
    if !target.is_dir() {
        return Err("目标不是目录".into());
    }

    #[cfg(windows)]
    {
        // 剪贴板读取 + 递归复制都在阻塞线程池执行（大目录复制耗时，不占异步 worker）
        tokio::task::spawn_blocking(move || paste_files_windows(&target))
            .await
            .map_err(|e| format!("粘贴任务执行失败: {}", e))?
    }
    #[cfg(not(windows))]
    {
        let _ = target;
        Err("当前平台暂不支持从系统剪贴板粘贴文件".into())
    }
}

#[cfg(windows)]
fn paste_files_windows(target: &Path) -> Result<Vec<String>, String> {
    use clipboard_win::formats::FileList;

    let sources: Vec<String> = clipboard_win::get_clipboard(FileList)
        .map_err(|e| format!("读取系统剪贴板失败: {}", e))?;
    if sources.is_empty() {
        return Err("剪贴板中没有文件".into());
    }
    let target_canon = std::fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());

    let mut created: Vec<String> = Vec::new();
    for src_str in &sources {
        let src = PathBuf::from(src_str);
        if !src.exists() {
            log::warn!("[nexus] 剪贴板中的源文件不存在，跳过: {}", src_str);
            continue;
        }
        // 源在目标目录内（或等于目标）→ 跳过，防止复制到自身
        let src_canon = std::fs::canonicalize(&src).unwrap_or_else(|_| src.clone());
        if src_canon.starts_with(&target_canon) {
            log::warn!("[nexus] 源位于目标目录内，跳过: {}", src_str);
            continue;
        }
        let name = match src.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue, // 非 UTF-8 文件名（极罕见），跳过
        };
        let dst = unique_dest_path(target, &name);
        copy_recursive(&src, &dst).map_err(|e| format!("复制 {} 失败: {}", name, e))?;
        created.push(dst.to_string_lossy().replace('\\', "/"));
    }
    if created.is_empty() {
        return Err("没有可粘贴的项目".into());
    }
    log::info!("[nexus] 粘贴 {} 个项目到 {}", created.len(), target.display());
    Ok(created)
}

/// 递归复制文件/目录（同步，仅在阻塞线程池中调用）
fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(src, dst)?;
    }
    Ok(())
}

/// 目标路径去重：已存在同名时自动追加 " (2)"、" (3)"…（与资源管理器同文件惯例一致）。
/// 扩展名拆分为后缀（"a.txt" → stem "a" + ext ".txt"）；".gitignore" 这类以点开头的整体作为主名。
fn unique_dest_path(target: &Path, name: &str) -> PathBuf {
    let candidate = target.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{}", e)),
        _ => (name, String::new()),
    };
    let mut i = 2;
    loop {
        let candidate = target.join(format!("{} ({}){}", stem, i, ext));
        if !candidate.exists() {
            return candidate;
        }
        i += 1;
    }
}

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unique_dest_path_no_conflict() {
        let dir = std::env::temp_dir();
        let name = format!("nexus_ut_{}", std::process::id());
        let dst = unique_dest_path(&dir, &name);
        assert_eq!(dst, dir.join(&name)); // 无同名 → 原样
    }

    #[test]
    fn test_unique_dest_path_with_extension() {
        let dir = std::env::temp_dir();
        // 创建真实冲突文件，验证序号后缀
        let name = format!("nexus_ut_ext_{}.txt", std::process::id());
        let existing = dir.join(&name);
        std::fs::write(&existing, "x").unwrap();
        let dst = unique_dest_path(&dir, &name);
        std::fs::remove_file(&existing).unwrap();
        // 主名带后缀、扩展名保留
        assert_eq!(dst.file_name().unwrap().to_string_lossy(), format!("{} (2).txt", name.trim_end_matches(".txt")));
        assert_eq!(dst, dir.join(format!("{} (2).txt", name.trim_end_matches(".txt"))));
    }

    #[test]
    fn test_unique_dest_path_dotfile_not_split() {
        // ".gitignore" 不带点开头拆分 → 保持整体
        let dir = std::env::temp_dir();
        let name = ".gitignore";
        let dst = unique_dest_path(&dir, name);
        assert_eq!(dst, dir.join(".gitignore"));
    }
}
