//! 文件剪贴板操作：与系统资源管理器互通复制粘贴（Windows）
//!
//! 复制：把文件/文件夹路径写入系统剪贴板（CF_HDROP），在资源管理器中 Ctrl+V 即可粘贴。
//! 粘贴：读取系统剪贴板中的文件列表，复制到目标目录（同名自动追加 " (2)" 序号）。
//! 其他平台暂不支持（macOS/系统剪贴板文件格式无对应实现）。

use std::path::{Path, PathBuf};
use tauri::State;

use crate::commands::editor::check_path_allowed;
use crate::AppState;

/// 单次复制的目录层级上限（联接点/符号链接会让目录树成环，纯深度兜底）
const MAX_COPY_DEPTH: usize = 64;
/// 单次粘贴复制的条目总数上限（防巨量文件耗尽时间与磁盘）
const MAX_COPY_ENTRIES: usize = 200_000;

/// 复制文件/文件夹到系统剪贴板（在资源管理器中 Ctrl+V 可粘贴）
#[tauri::command]
pub fn copy_files_to_clipboard(state: State<AppState>, paths: Vec<String>) -> Result<(), String> {
    // 白名单校验：写入系统剪贴板的路径允许被粘贴到任意位置，必须与读路径同口径
    for p in &paths {
        check_path_allowed(&state, p)?;
    }
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
    // 多根白名单校验（项目根 + 各服务/模板工作目录），与其余路径类命令同口径；
    // 校验内部不跨 await 持锁，future 仍是 Send
    check_path_allowed(&state, &target_dir)?;
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
    let mut entries = 0usize;
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
        copy_recursive(&src, &dst, &mut entries).map_err(|e| format!("复制 {} 失败: {}", name, e))?;
        created.push(dst.to_string_lossy().replace('\\', "/"));
    }
    if created.is_empty() {
        return Err("没有可粘贴的项目".into());
    }
    log::info!("[nexus] 粘贴 {} 个项目到 {}", created.len(), target.display());
    Ok(created)
}

/// 递归复制文件/目录（同步，仅在阻塞线程池中调用）
///
/// `entries` 由调用方在单次粘贴内共享，跨多个源累计，防止"每个源各自不超限、总量爆掉"。
fn copy_recursive(src: &Path, dst: &Path, entries: &mut usize) -> std::io::Result<()> {
    copy_recursive_at(src, dst, entries, 0)
}

/// 带层级的递归实现。
///
/// 不跟随符号链接/联接点：Windows 目录联接（junction）会让目录树成环，
/// 原实现用 `src.is_dir()`（跟随链接）会无限递归并把磁盘写满。
fn copy_recursive_at(src: &Path, dst: &Path, entries: &mut usize, depth: usize) -> std::io::Result<()> {
    if depth > MAX_COPY_DEPTH {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "目录层级过深（疑似符号链接环）"));
    }
    let meta = std::fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        log::warn!("[nexus] 跳过符号链接/联接点（不跟随复制）: {}", src.display());
        return Ok(());
    }
    *entries += 1;
    if *entries > MAX_COPY_ENTRIES {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "复制的条目数超过上限"));
    }
    if meta.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_recursive_at(&entry.path(), &dst.join(entry.file_name()), entries, depth + 1)?;
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

    /// 普通目录树照常复制（回归：加了 depth/entries 参数后基本功能不变）
    #[test]
    fn test_copy_recursive_copies_tree() {
        let base = std::env::temp_dir().join(format!("nexus_ut_copy_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let src = base.join("src");
        std::fs::create_dir_all(src.join("a/b")).unwrap();
        std::fs::write(src.join("a/b/f.txt"), "x").unwrap();
        let mut entries = 0usize;
        copy_recursive(&src, &base.join("dst"), &mut entries).unwrap();
        assert_eq!(std::fs::read_to_string(base.join("dst/a/b/f.txt")).unwrap(), "x");
        assert_eq!(entries, 4); // src、a、b 三个目录 + 1 个文件
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 目录层级超限时报错而不是无限递归（符号链接环的兜底防线）
    #[test]
    fn test_copy_recursive_depth_limit() {
        // 单字符目录名：66 层嵌套仍在 Windows 260 字符路径上限内
        let base = std::env::temp_dir().join(format!("nxut_depth_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let src = base.join("s");
        let mut deep = src.clone();
        for _ in 0..(MAX_COPY_DEPTH + 2) {
            deep.push("d");
        }
        std::fs::create_dir_all(&deep).unwrap();
        let mut entries = 0usize;
        let err = copy_recursive(&src, &base.join("o"), &mut entries).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 目录联接（junction）/符号链接被跳过且不跟随：不创建目标、不报错。
    /// 这是本修复的核心场景——联接会让目录树成环，原实现（is_dir 跟随链接）无限递归写满磁盘。
    /// Windows 用 `mklink /J` 建联接（无需管理员，符号链接需要）；建不出来则跳过断言。
    #[test]
    fn test_copy_recursive_skips_link() {
        let base = std::env::temp_dir().join(format!("nxut_link_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let real = base.join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("f.txt"), "x").unwrap();
        let src = base.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let link = src.join("link");
        #[cfg(windows)]
        let created = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&real)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        #[cfg(not(windows))]
        let created = std::os::unix::fs::symlink(&real, &link).is_ok();
        if created {
            let mut entries = 0usize;
            copy_recursive(&src, &base.join("dst"), &mut entries).unwrap();
            assert!(!base.join("dst/link").exists(), "目录联接/符号链接不应被跟随复制");
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
