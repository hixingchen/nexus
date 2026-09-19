//! 找出"服务正在写的日志文件"。
//!
//! 背景：Windows 上有些服务用 cmd 内建命令 `start` 另开一个控制台窗口跑
//! （Tomcat 的 `startup.bat`、ActiveMQ 的 wrapper 等），那个进程的 stdout 不进我们的管道，
//! 面板里只剩一行"进程已退出（退出码: 0）"。这类服务通常会把日志写进文件——
//! 跟随文件是唯一能拿到内容的现实路径（要抓窗口里的输出只能去读控制台缓冲区，代价大得多）。
//!
//! 这个模块只做**纯路径/文件系统判断**（可独立测试）；跟随线程在 `process.rs` 里，
//! 因为它必须复用那边的日志缓冲上限与批量发射机制。

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// 每个目录最多看多少个条目：目录里塞了几万个文件时不至于卡住启动路径
const MAX_ENTRIES_PER_DIR: usize = 500;

/// 日志文件的扩展名白名单（小写比较）
const LOG_EXTENSIONS: &[&str] = &["log", "out", "txt"];

/// 候选日志文件：服务目录附近、**在服务启动之后被写过**的日志类文件
#[derive(Clone)]
pub(crate) struct LogCandidate {
    pub path: PathBuf,
    pub size: u64,
    /// 修改时间：增长量相同时用它决定谁更可能在被写
    pub modified: SystemTime,
}

/// 列出候选日志文件（搜索范围见 `candidate_dirs`），顺序即目录优先级。
///
/// `since` = 服务启动时刻：早于它被改过的文件不可能是这个服务写的——这条判据比
/// 扩展名更能挡住"目录里随便一个旧日志"。
///
/// **注意这里只负责"列出来"，不负责"选哪个"**：选谁是 `pick_growing` 的事，
/// 判据是"谁在长"，不是"谁的修改时间最新"。后者实测选错过：
/// Tomcat 启动瞬间会一次性创建 catalina/localhost/host-manager 几个日志文件，
/// 按"最新"选中的是只写了两行就再也不动的 localhost.log，面板因此永远是空的。
pub(crate) fn candidate_logs(cwd: &Path, since: SystemTime) -> Vec<LogCandidate> {
    let mut out = Vec::new();
    for dir in candidate_dirs(cwd) {
        // 读不到的目录直接跳过：目录不存在、权限不足都属正常（不是所有服务都有 logs/）
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten().take(MAX_ENTRIES_PER_DIR) {
            let path = entry.path();
            if !is_log_like(&path) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let Ok(modified) = meta.modified() else { continue };
            if modified < since {
                continue; // 早于服务启动：不是它写的
            }
            out.push(LogCandidate { path, size: meta.len(), modified });
        }
    }
    out
}

/// 从两次采样里选出"确实在增长"的文件。
///
/// 判据是**增长量**（`now.size - first.size`），因为"正在被写入"才是我们要的语义：
/// - 只为启动时创建、之后不再写的文件（空文件、只写了两行的次要日志）增长量为 0，被排除；
/// - 一个都没长（服务启动慢、日志还没开写）→ 返回 None，调用方下一轮再采样
///   （最多 `MAX_LOG_PROBE_ATTEMPTS` 轮，见 process.rs）。
///
/// 增长量相同时取目录优先级更靠前的（候选顺序即优先级：cwd 本体 > cwd/logs > cwd/data > 上级），
/// 再相同取修改时间更新的。
pub(crate) fn pick_growing(first: &[LogCandidate], now: &[LogCandidate]) -> Option<PathBuf> {
    // (增长量, 候选序号, 修改时间, 路径)
    let mut best: Option<(u64, usize, SystemTime, PathBuf)> = None;
    for (idx, cur) in now.iter().enumerate() {
        let growth = match first.iter().find(|c| c.path == cur.path) {
            Some(prev) => cur.size.saturating_sub(prev.size),
            // 两次采样之间才出现的文件：按"从 0 长到现在"算
            None => cur.size,
        };
        if growth == 0 {
            continue; // 没长：不是它在写
        }
        let better = match &best {
            None => true,
            // 增长量优先；相同则目录优先级更靠前（序号更小）的胜；再相同取修改时间更新的
            Some((g, i, t, _)) => {
                growth > *g || (growth == *g && (idx < *i || (idx == *i && cur.modified > *t)))
            }
        };
        if better {
            best = Some((growth, idx, cur.modified, cur.path.clone()));
        }
    }
    best.map(|(_, _, _, path)| path)
}

/// 候选目录：服务目录本身 + 它的 `logs/`、`data/`，再向上两级**只看**这两类子目录。
///
/// 两条边界都是有理由的：
/// - 向上两级：实测的真实摆法是 `apache-activemq-5.15.13/bin/win64/activemq.bat`
///   ——日志在根目录的 `data/`，离 cwd 隔着两级。再往上是瞎猜（会扫到用户 HOME）；
/// - 上级目录**本身不扫**：cwd 常常在项目里（`C:\code\myapp\bin`），扫上一级就等于
///   把兄弟项目的日志一起纳入候选。上级只可能是"应用根目录"，它的日志必然在
///   `logs/` 或 `data/` 下——这两个子目录已经覆盖了真实摆法。
fn candidate_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![cwd.to_path_buf()];
    for sub in ["logs", "data"] {
        dirs.push(cwd.join(sub));
    }
    let mut base = cwd.parent();
    for _ in 0..2 {
        let Some(dir) = base else { break };
        for sub in ["logs", "data"] {
            dirs.push(dir.join(sub));
        }
        base = dir.parent();
    }
    dirs
}

/// 扩展名是不是日志类（`catalina.2026-09-19.log`、`nohup.out`、`access_log.txt` 都算）
fn is_log_like(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| LOG_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// 建一个隔离的临时目录（名字带 pid，避免并行测试互相踩）
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nexus_logfile_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("应当能建临时目录");
        dir
    }

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("应当能建父目录");
        }
        std::fs::write(path, content).expect("应当能写文件");
    }

    /// 候选范围：根目录下的 logs/ 会被列进来（Tomcat 的摆法）
    #[test]
    fn test_candidates_include_logs_dir() {
        let root = temp_dir("tomcat");
        write(&root.join("logs").join("catalina.2026-09-19.log"), "boot");
        write(&root.join("conf").join("server.xml"), "<Server/>");
        let got = candidate_logs(&root, SystemTime::now() - Duration::from_secs(60));
        assert_eq!(got.len(), 1, "只应列出日志类文件：{:?}", got.iter().map(|c| &c.path).collect::<Vec<_>>());
        assert!(got[0].path.ends_with("catalina.2026-09-19.log"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// cwd 在 bin 子目录时，往上找两级内的 data/（ActiveMQ `bin/win64` 的摆法）
    #[test]
    fn test_candidates_include_parent_data_dir() {
        let root = temp_dir("activemq");
        let cwd = root.join("bin").join("win64");
        std::fs::create_dir_all(&cwd).expect("建 bin 目录");
        write(&root.join("data").join("activemq.log"), "broker up");
        let got = candidate_logs(&cwd, SystemTime::now() - Duration::from_secs(60));
        assert_eq!(got.len(), 1);
        assert!(got[0].path.ends_with("activemq.log"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 早于服务启动的文件不进候选——"目录里恰好有个旧日志"是最容易误跟的情况
    #[test]
    fn test_ignores_files_older_than_service_start() {
        let root = temp_dir("stale");
        write(&root.join("logs").join("stale.log"), "很久以前");
        // 启动时刻取"现在"，那个文件必然早于它
        let got = candidate_logs(&root, SystemTime::now());
        assert!(got.is_empty(), "早于服务启动的日志不该进候选");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 非日志扩展名不进候选（目录里常混着 .jar/.xml/.properties）
    #[test]
    fn test_ignores_non_log_extensions() {
        let root = temp_dir("ext");
        write(&root.join("conf").join("server.xml"), "<Server/>");
        write(&root.join("lib").join("app.jar"), "PK");
        let got = candidate_logs(&root, SystemTime::now() - Duration::from_secs(60));
        assert!(got.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 目录不存在/为空时安静返回空（不能因为"没有 logs 目录"就报错）
    #[test]
    fn test_missing_dirs_return_empty() {
        let root = temp_dir("empty");
        assert!(candidate_logs(&root, SystemTime::now() - Duration::from_secs(60)).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 上级目录**本身**不进候选：否则 cwd 在项目里时会把兄弟项目的日志认成自己的
    #[test]
    fn test_parent_dir_itself_is_not_scanned() {
        let root = temp_dir("sibling");
        let cwd = root.join("myapp").join("bin");
        std::fs::create_dir_all(&cwd).expect("建 cwd");
        write(&root.join("sibling.log"), "别的项目");
        let got = candidate_logs(&cwd, SystemTime::now() - Duration::from_secs(60));
        assert!(got.is_empty(), "上级目录本体不该进候选");
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── pick_growing：选"在长的那个" ────────────────────────────
    //
    // 这组钉住的是实测踩到的 bug：Tomcat 启动时一次性创建好几个日志文件，
    // 按"修改时间最新"会选中只写了两行就再不动的 localhost.log，面板永远是空的。

    fn cand(path: &str, size: u64) -> LogCandidate {
        LogCandidate { path: PathBuf::from(path), size, modified: SystemTime::now() }
    }

    /// 写了两行就停的文件（哪怕它是最新被碰过的）不该赢过一直在写的主日志
    #[test]
    fn test_pick_growing_prefers_file_that_grows() {
        let first = vec![cand("logs/catalina.log", 100), cand("logs/localhost.log", 200)];
        // catalina 长了 5KB；localhost 一动不动
        let now = vec![cand("logs/catalina.log", 5100), cand("logs/localhost.log", 200)];
        assert_eq!(pick_growing(&first, &now), Some(PathBuf::from("logs/catalina.log")));
    }

    /// 谁都没长 → None（调用方下一轮再采样，而不是随便挑一个）
    #[test]
    fn test_pick_growing_returns_none_when_nothing_grew() {
        let first = vec![cand("logs/a.log", 10), cand("logs/b.log", 20)];
        assert_eq!(pick_growing(&first, &first.clone()), None);
    }

    /// 两次采样之间才出现的文件按"从 0 长到现在"算
    #[test]
    fn test_pick_growing_counts_new_file() {
        let first = vec![cand("logs/a.log", 10)];
        let now = vec![cand("logs/a.log", 10), cand("data/b.log", 4096)];
        assert_eq!(pick_growing(&first, &now), Some(PathBuf::from("data/b.log")));
    }

    /// 增长量相同时取目录优先级靠前的（候选顺序即目录优先级）
    #[test]
    fn test_pick_growing_tie_breaks_by_directory_priority() {
        let first = vec![cand("logs/close.log", 0), cand("data/far.log", 0)];
        let now = vec![cand("logs/close.log", 100), cand("data/far.log", 100)];
        assert_eq!(pick_growing(&first, &now), Some(PathBuf::from("logs/close.log")));
    }
}
