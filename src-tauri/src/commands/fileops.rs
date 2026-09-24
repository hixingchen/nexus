//! 文件操作：与系统资源管理器互通——复制/粘贴（剪贴板）与删除（回收站）
//!
//! 复制：把文件/文件夹路径写入系统剪贴板（CF_HDROP），在资源管理器中 Ctrl+V 即可粘贴。
//! 粘贴：读取系统剪贴板中的文件列表，复制到目标目录。同名时**弹框问一次**（覆盖 / 保留两者 /
//! 跳过，见 `PasteResponse` 的两阶段说明）；没有同名就照常直接粘（默认仍追加 " (2)" 序号）。
//! 删除：移入系统回收站（文件树右键「删除」）。
//! 剪贴板部分其他平台暂不支持（macOS/系统剪贴板文件格式无对应实现）。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tauri::State;

use crate::commands::paths::PathAllowlist;
use crate::database::Database;
use crate::AppState;

/// 单次复制的目录层级上限（联接点/符号链接会让目录树成环，纯深度兜底）
const MAX_COPY_DEPTH: usize = 64;
/// 单次粘贴复制的条目总数上限（防巨量文件耗尽时间与磁盘）
const MAX_COPY_ENTRIES: usize = 200_000;
/// 同名去重的最多尝试次数（超过即判定无法落位，报为该源的失败）
const MAX_DEST_ATTEMPTS: usize = 1000;

/// 粘贴结果：`created` 为已落盘路径，`failed` 为逐个源的失败原因，`skipped` 为被跳过的链接。
///
/// 为什么不能只返回 `Vec<String>` 或直接 `Err`：多源粘贴时部分成功是常态
/// （某个源被占用/权限不足）。原实现一旦中途失败就整体返回 `Err`，
/// 此时**前面的源已经在磁盘上了**，调用方却拿不到这个清单，于是按"全部失败"
/// 刷新界面，用户看到文件明明在、界面却说没成功。
///
/// 为什么 `skipped` 不并进另外两个：符号链接/联接点既不进 `created`（那是在撒谎——
/// 复制出来的目录少了一整棵子树），也不该进 `failed`（源的其他内容**都成功过来了**，
/// 报"失败"会让用户以为整个源没复制）。原实现只写一行 `log::warn!` 就返回 Ok，
/// 界面照样说"已粘贴 N 个项目"。
#[derive(serde::Serialize)]
pub struct PasteFilesResult {
    pub created: Vec<String>,
    pub failed: Vec<String>,
    pub skipped: Vec<String>,
    /// 被覆盖掉的原项（已移入回收站）。用户必须知道"哪个旧东西没了、去哪了"——
    /// 覆盖是本功能里唯一会动**已有数据**的路径，只报"已粘贴 N 个"会把这件事整个吞掉
    pub replaced: Vec<String>,
    /// 用户在同名弹框里选「跳过」的那些源（复制本身没失败，是**用户决定不复制**）
    pub skipped_by_user: Vec<String>,
}

/// 同名冲突的处理策略（弹框里的三选一，由前端回传）
#[derive(serde::Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum ConflictPolicy {
    /// 保留两者：落成 "a (2).txt"（本功能之前唯一的做法）
    Rename,
    /// 覆盖：同名项先移入回收站，再落到原名
    Overwrite,
    /// 跳过：同名的不复制，其余照常
    Skip,
}

/// 目标目录里已有的一个同名项（探测阶段的结果，**只读、不落盘**）
#[derive(serde::Serialize)]
pub struct PasteConflict {
    /// 撞名的名字（目标里已存在的那一项就叫这个名）
    pub name: String,
    /// 已存在的那一项是目录：替换时整棵进回收站，代价比换掉一个文件大得多
    pub existing_is_dir: bool,
}

/// 粘贴响应。**两阶段**：先探测（一个字节不落盘），有同名冲突就把决策权交回用户，
/// 拿到策略后再执行一次。
///
/// 为什么不在一次调用里弹框：源路径只能由后端从**系统剪贴板**读。若让前端把源清单当参数
/// 传回来直接用作复制输入，被攻陷的 webview 就能把任意路径（`…\.ssh\id_rsa`）复制进项目目录。
/// 所以第二阶段的源清单**只用于和当前剪贴板比对**，不参与复制；真正复制的源来自剪贴板
/// （不校验源，理由见 `paste_into` 的说明）。
#[derive(serde::Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum PasteResponse {
    /// 有同名冲突，尚未复制任何东西；前端弹框后带策略再调一次
    Conflict {
        conflicts: Vec<PasteConflict>,
        /// 本轮剪贴板里的源清单，原样回传做一致性校验
        sources: Vec<String>,
    },
    Done(PasteFilesResult),
}

/// 把一串路径写进系统剪贴板（CF_HDROP）。
///
/// 抽成独立函数是为了能**脱离 AppState** 测"清不清旧格式"这件只在真实剪贴板上才发生的
/// 事（白名单校验留在命令里，与剪贴板语义无关）。
///
/// **`empty()` 这一步不能省**，它是本函数存在的理由：
/// - `Clipboard::new_attempts` 只做 `OpenClipboard`，**不清空**；
/// - `raw::set_file_list` 内部用的是 `NoClear`（见 clipboard-win 的 `set_file_list_inner`），**也不清**。
///
/// 于是上一个持有者留下的格式全部原样留着。Windows 剪贴板是**多格式并存**的，粘贴方自己
/// 挑用哪个——资源管理器复制文件时放的是 `Shell IDList Array`、`Preferred DropEffect` 这些
/// shell 自己的原生格式，而粘贴时它认的是那些格式。结果就是用户报告的那一幕：
/// **在 Nexus 里复制了 A，切到资源管理器粘贴，粘出来的却是复制 A 之前剪贴板上的那份。**
/// 实测见 `test_write_files_to_clipboard_clears_previous`（不清空的话，连一个文本格式都原样留着）。
#[cfg(windows)]
fn write_files_to_clipboard(paths: &[String]) -> Result<(), String> {
    use clipboard_win::Clipboard;

    // new_attempts(10)：剪贴板被其他程序占用时最多重试 10 次
    let _clip = Clipboard::new_attempts(10).map_err(|e| format!("打开剪贴板失败: {}", e))?;
    clipboard_win::raw::empty().map_err(|e| format!("清空剪贴板失败: {}", e))?;
    clipboard_win::raw::set_file_list(paths).map_err(|e| format!("写入剪贴板失败: {}", e))?;
    Ok(())
}

/// 复制文件/文件夹到系统剪贴板（在资源管理器中 Ctrl+V 可粘贴）
#[tauri::command]
pub fn copy_files_to_clipboard(state: State<AppState>, paths: Vec<String>) -> Result<(), String> {
    // 白名单校验：写入系统剪贴板的路径允许被粘贴到任意位置，必须与读路径同口径
    for p in &paths {
        state.paths.check_path_allowed(&state.db, p)?;
    }
    #[cfg(windows)]
    {
        // 过滤不存在的路径（可能已被删除），避免写入无效剪贴板条目
        let valid: Vec<String> = paths.into_iter().filter(|p| Path::new(p).exists()).collect();
        if valid.is_empty() {
            return Err("没有可复制的文件".into());
        }
        write_files_to_clipboard(&valid)?;
        log::info!("[nexus] 复制 {} 个项目到系统剪贴板", valid.len());
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = paths;
        Err("当前平台暂不支持复制文件到系统剪贴板".into())
    }
}

/// 从系统剪贴板读取文件列表并复制到目标目录（供前端刷新树）。
///
/// `conflict_policy` 为空 = **探测**：只回报同名冲突、不复制；不为空 = 按该策略执行
/// （此时 `expected_sources` 要带上探测那轮回传的源清单，见 `paste_files_windows`）。
#[tauri::command]
pub async fn paste_files(
    state: State<'_, AppState>,
    target_dir: String,
    conflict_policy: Option<ConflictPolicy>,
    expected_sources: Option<Vec<String>>,
) -> Result<PasteResponse, String> {
    // 多根白名单校验（项目根 + 各服务/模板工作目录），与其余路径类命令同口径；
    // 校验内部不跨 await 持锁，future 仍是 Send
    state.paths.check_path_allowed(&state.db, &target_dir)?;
    let target = PathBuf::from(&target_dir);
    if !target.is_dir() {
        return Err("目标不是目录".into());
    }

    #[cfg(windows)]
    {
        // 剪贴板读取 + 递归复制都在阻塞线程池执行（大目录复制耗时，不占异步 worker）
        tokio::task::spawn_blocking(move || {
            paste_files_windows(&target, conflict_policy, expected_sources.as_deref())
        })
        .await
        .map_err(|e| format!("粘贴任务执行失败: {}", e))?
    }
    #[cfg(not(windows))]
    {
        let _ = (target, conflict_policy, expected_sources);
        Err("当前平台暂不支持从系统剪贴板粘贴文件".into())
    }
}

#[cfg(windows)]
fn paste_files_windows(
    target: &Path,
    policy: Option<ConflictPolicy>,
    expected_sources: Option<&[String]>,
) -> Result<PasteResponse, String> {
    use clipboard_win::formats::FileList;

    let sources: Vec<String> = clipboard_win::get_clipboard(FileList)
        .map_err(|e| format!("读取系统剪贴板失败: {}", e))?;
    if sources.is_empty() {
        return Err("剪贴板中没有文件".into());
    }
    // 探测那轮回传的源清单必须与**当前**剪贴板一致：弹框开着的时候，用户完全可能去别处
    // 又复制了一次。那时再按"覆盖"执行，落下去的就不是他刚刚看过的那批文件了
    if let Some(expected) = expected_sources {
        if !same_file_set(expected, &sources) {
            return Err("剪贴板内容已改变，请重新粘贴".into());
        }
    }
    paste_into(target, policy, &sources)
}

/// 两份源清单是不是同一批文件。只比集合不比顺序——顺序变了但文件没变，用户看到的东西没变
fn same_file_set(a: &[String], b: &[String]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut x: Vec<&str> = a.iter().map(String::as_str).collect();
    let mut y: Vec<&str> = b.iter().map(String::as_str).collect();
    x.sort_unstable();
    y.sort_unstable();
    x == y
}

/// 粘贴的主体：探测同名冲突 / 按策略复制。
///
/// **源清单是参数而不是自己去读剪贴板**：抽这一层就是为了能脱离真实剪贴板测
/// （理由与 `write_files_to_clipboard` 从命令里抽出来相同），否则"覆盖真的替换了、
/// 跳过真的没动"这些结论只能靠手点验证。
///
/// **不校验源**（审计的 SEC-21 建议过，本轮试了又撤回）：剪贴板内容由**用户自己的动作**
/// 产生（被攻陷的 webview 不具备写 CF_HDROP 的能力，审计原文也写了"实际可利用性受限"），
/// 而"从下载目录/桌面复制文件再粘进项目"是正常用法——加上源白名单会把这条路堵死。
/// 那句"与 read_file 口径不一致"是有意的取舍，不是漏写。
fn paste_into(
    target: &Path,
    policy: Option<ConflictPolicy>,
    sources: &[String],
) -> Result<PasteResponse, String> {
    let target_canon = std::fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());

    // 探测轮：只读，一个字节都不落盘
    if policy.is_none() {
        let conflicts = detect_conflicts(target, &target_canon, sources);
        if !conflicts.is_empty() {
            log::info!("[nexus] 粘贴探测到 {} 个同名项，等用户定策略", conflicts.len());
            return Ok(PasteResponse::Conflict { conflicts, sources: sources.to_vec() });
        }
    }

    // 覆盖/跳过的范围在**执行时重算**，不直接信探测那轮的结论：两轮之间目标目录可能已经
    // 变了（冲突项被删掉、同批的另一个源落了地、外部进程新建了文件）。重算之后，
    // 覆盖面永远不会超出用户当时看到的那一份，多出来的同名项一律走改名
    // ——两种判错代价不对称：少覆盖 = 多一个 " (2).txt"，多覆盖 = 用户没看过的文件没了
    let conflicted: HashSet<String> = if policy.is_some() {
        detect_conflicts(target, &target_canon, sources).into_iter().map(|c| c.name).collect()
    } else {
        HashSet::new()
    };

    let mut created: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    let mut replaced: Vec<String> = Vec::new();
    let mut skipped_by_user: Vec<String> = Vec::new();
    // 本次粘贴已经用作"覆盖落点"的路径（见 plan_dest）
    let mut claimed: HashSet<PathBuf> = HashSet::new();
    let mut acc = CopyAcc::default();
    for src_str in sources {
        let src = PathBuf::from(src_str);
        if !src.exists() {
            log::warn!("[nexus] 剪贴板中的源文件不存在，跳过: {}", src_str);
            failed.push(format!("{}: 源文件不存在", src_str));
            continue;
        }
        // 拦"把目录粘进它自己的子树里"，见 is_pasting_dir_into_itself 的方向说明
        if is_pasting_dir_into_itself(&src, &target_canon) {
            log::warn!("[nexus] 不能把文件夹粘贴到它自己（或其子目录）里: {}", src_str);
            failed.push(format!("{}: 不能把文件夹粘贴到它自己（或其子目录）里", src_str));
            continue;
        }
        let name = match src.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue, // 非 UTF-8 文件名（极罕见），跳过
        };
        // 用户选了"跳过"：只跳过**真的撞名**的源，其余照常复制
        if policy == Some(ConflictPolicy::Skip) && conflicted.contains(&name) {
            skipped_by_user.push(src_str.clone());
            continue;
        }
        let overwrite = policy == Some(ConflictPolicy::Overwrite) && conflicted.contains(&name);
        // 单个源失败不中断整批：继续处理其余源，最后连同已创建清单一起上报
        let Some(plan) = plan_dest(target, &name, &src, overwrite, &mut claimed) else {
            failed.push(format!("{}: 同名文件过多，无法确定目标名", name));
            continue;
        };
        // 覆盖前先把同名项移入回收站（与「删除」同口径，见 delete_path）。
        // 回收站失败**不降级成直接删**——那正是"用户以为旧文件还在回收站里"的静默数据丢失
        if let Some(existing) = &plan.replacing {
            if let Err(e) = trash::delete(existing) {
                log::warn!("[nexus] 替换 {} 前清理失败: {}", name, e);
                failed.push(format!("{}: 替换失败，{}", name, describe_trash_error(&e)));
                continue;
            }
            replaced.push(existing.to_string_lossy().replace('\\', "/"));
        }
        match copy_recursive(&src, &plan.dst, &mut acc) {
            // 顶层源本身就是链接：一个字节都没落盘（它已经进了 acc.skipped）。
            // 不进 created——见 copy_recursive 的返回值说明（CQ-41）。
            // 这一支**不可能**有 plan.replacing：plan_dest 对链接源一律不给覆盖，
            // 否则就成了"删掉目标里那份、什么都不放"
            Ok(false) => {}
            Ok(true) => created.push(plan.dst.to_string_lossy().replace('\\', "/")),
            Err(e) => {
                log::warn!("[nexus] 粘贴 {} 失败: {}", name, e);
                // 旧文件已经进回收站了才失败：不说这句的话，用户看到的是"替换失败"，
                // 却不知道原来的文件此刻已经不在原位
                let tail = if plan.replacing.is_some() {
                    "（同名原项已移入回收站，可从回收站恢复）"
                } else {
                    ""
                };
                failed.push(format!("{}: {}{}", name, e, tail));
            }
        }
    }
    // skipped 也算"有结果"：剪贴板里只有一个联接点时，前几个都空，
    // 但用户该看到的是"跳过了它"，而不是一句没头没尾的"没有可粘贴的项目"
    if created.is_empty() && failed.is_empty() && acc.skipped.is_empty() && skipped_by_user.is_empty() {
        return Err("没有可粘贴的项目".into());
    }
    log::info!(
        "[nexus] 粘贴完成：成功 {} 个，失败 {} 个，替换 {} 个，跳过链接 {} 个，用户跳过 {} 个 → {}",
        created.len(), failed.len(), replaced.len(), acc.skipped.len(), skipped_by_user.len(), target.display()
    );
    Ok(PasteResponse::Done(PasteFilesResult {
        created,
        failed,
        skipped: acc.skipped,
        replaced,
        skipped_by_user,
    }))
}

/// 探测这次粘贴会在目标目录里撞上哪些同名项（**只读，不落盘**）。
///
/// 只判**顶层名字**：目标里已有同名项就算冲突，不递归比对内容。于是"覆盖一个目录"是
/// **整棵替换**语义，而不是资源管理器那种递归合并——后者要按文件逐个弹框，是另一个量级的功能。
///
/// 同名只报一次：剪贴板里 `D:\a\x.txt` 和 `E:\b\x.txt` 撞的是目标里的同一个 `x.txt`，
/// 弹框说"2 个同名项目"会让用户以为目标里真有两项。
fn detect_conflicts(target: &Path, target_canon: &Path, sources: &[String]) -> Vec<PasteConflict> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for src_str in sources {
        let src = PathBuf::from(src_str);
        // 源不存在、"往自己的子树里粘"、源本身就是链接：这三类本来就复制不出东西
        // （前面两类报失败、第三类进 skipped），不参与这次决策
        if !src.exists() || is_pasting_dir_into_itself(&src, target_canon) || is_link_source(&src) {
            continue;
        }
        let Some(name) = src.file_name().and_then(|n| n.to_str()) else { continue };
        let existing = target.join(name);
        // 同名的就是源自己（原地复制一份）：不算冲突，见 is_pasting_onto_itself。
        // 这一支必须排在 seen 之前——否则"先遇到一个原地复制的源"会把同名让给它的、
        // 后面那个真正撞名的源一起吞掉
        if !existing.exists() || is_pasting_onto_itself(&src, &existing) {
            continue;
        }
        if !seen.insert(name.to_string()) {
            continue;
        }
        // symlink_metadata 不跟随链接：与 copy_recursive / ensure_deletable 同口径，
        // 替换时清掉的也是链接本身
        let existing_is_dir = std::fs::symlink_metadata(&existing).map(|m| m.is_dir()).unwrap_or(false);
        out.push(PasteConflict { name: name.to_string(), existing_is_dir });
    }
    out
}

/// 同名目标**就是源本身**吗（"原地复制一份"）。
///
/// 这一支必须显式排除：把文件粘回它自己所在的目录是最常用的复制方式（见
/// `is_pasting_dir_into_itself` 的说明），它本该落成 "a (2).txt"。若把它算成同名冲突，
/// 弹框里选「覆盖」就等于**拿文件覆盖它自己**——`dst == src` 的复制没有任何正当语义。
///
/// 解析不出规范路径时按**不是**处理（返回 false）：这里两种判错也不对称，但方向相反——
/// 误判成"是自己"会让真正撞名的文件走改名（多一个 " (2).txt"），而误判成"不是自己"
/// 只是让用户多看到一个他自己刚复制的那份。
fn is_pasting_onto_itself(src: &Path, existing: &Path) -> bool {
    match (std::fs::canonicalize(src), std::fs::canonicalize(existing)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// 一个源的落点决策
struct DestPlan {
    /// 复制到哪里
    dst: PathBuf,
    /// 覆盖前要先清掉的那一项（已存在）；`None` = 无需清理
    replacing: Option<PathBuf>,
}

/// 决定一个源落到哪里。
///
/// 覆盖的三个前提缺一不可：策略是覆盖、**真的撞名**（`overwrite` 由调用方算好）、
/// 本批还没占用过这个落点。最后一条挡的是"剪贴板里两个同名源"——
/// 没有它，第二个源会把刚落地的第一个盖掉。
///
/// 其余情况一律走 `unique_dest_path` 改名去重（没撞名时它返回的就是原名，
/// 见该函数第一个分支），这也保证了原地复制不会被误当成覆盖。
fn plan_dest(
    target: &Path,
    name: &str,
    src: &Path,
    overwrite: bool,
    claimed: &mut HashSet<PathBuf>,
) -> Option<DestPlan> {
    let plain = target.join(name);
    if overwrite
        && plain.exists()
        && !claimed.contains(&plain)
        && !is_pasting_onto_itself(src, &plain)
        && !is_link_source(src)
    {
        claimed.insert(plain.clone());
        return Some(DestPlan { dst: plain.clone(), replacing: Some(plain) });
    }
    unique_dest_path(target, name).map(|dst| DestPlan { dst, replacing: None })
}

/// 这次粘贴是不是"把目录粘进它自己的子树里"（含粘进它自己）。
///
/// **只看目录**：文件没有子树，把文件粘到它自己所在的目录，就是最普通的"原地复制一份"
/// ——由 `unique_dest_path` 生成 " (2)" 名字。拦掉它等于把最常用的操作禁了。
///
/// **方向不能反**（原实现写反了）：该拦的是"目标在源**里面**"——那会一边 `read_dir`
/// 一边往里写，目录自我嵌套下去；而不是"源在目标里面"。写反的代价实测过：五个常用场景
/// 判错三个（`test_paste_into_own_subtree_*` 把这五条钉住了）。
///
/// **联接点/符号链接不参与判定**：`copy_recursive` 本就不跟随它们（只记一条 skipped），
/// 因而没有"一边读一边写"的递归可言——与它用 `symlink_metadata` 判链接是同一口径。
///
/// 解析不出规范路径时**保守拦下**：这条路上两种判错的代价不对等——多拦一次的代价是
/// "粘不动、换个地方粘"，漏拦一次的代价是往用户项目里写一棵 `A/B/A/B/…` 垃圾目录树。
fn is_pasting_dir_into_itself(src: &Path, target_canon: &Path) -> bool {
    match std::fs::symlink_metadata(src) {
        Ok(m) if m.is_dir() => {}
        _ => return false,
    }
    match std::fs::canonicalize(src) {
        Ok(c) => target_canon.starts_with(&c),
        Err(_) => true,
    }
}

/// 单次粘贴的累计器：**由调用方创建、跨多个源共享**。
///
/// `entries` 跨源累计，防止"每个源各自不超限、总量爆掉"；`skipped` 同理——它是这次粘贴
/// 的结论的一部分，不能只写进日志（见 `PasteFilesResult` 的说明）。
#[derive(Default)]
struct CopyAcc {
    entries: usize,
    /// 被跳过的符号链接/联接点（源路径）。复制本身成功，但内容不全——必须让用户看见
    skipped: Vec<String>,
}

/// 递归复制文件/目录（同步，仅在阻塞线程池中调用）。
///
/// 返回值 = **这次到底有没有落盘**。为什么不让调用方看 `dst.exists()`：那是又一趟文件系统
/// 查询，而且"顶层的源本身就是链接"这一支会**先返回成功、再什么都不创建**——调用方据此把
/// 从未存在的 `dst` 记进 `created`，同一句提示里就会既说"已粘贴「link」"又说"它被跳过了"
/// （CQ-41）。
fn copy_recursive(src: &Path, dst: &Path, acc: &mut CopyAcc) -> std::io::Result<bool> {
    copy_recursive_at(src, dst, acc, 0)
}

/// 带层级的递归实现。
///
/// 不跟随符号链接/联接点：Windows 目录联接（junction）会让目录树成环，
/// 原实现用 `src.is_dir()`（跟随链接）会无限递归并把磁盘写满。
fn copy_recursive_at(src: &Path, dst: &Path, acc: &mut CopyAcc, depth: usize) -> std::io::Result<bool> {
    if depth > MAX_COPY_DEPTH {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "目录层级过深（疑似符号链接环）"));
    }
    let meta = std::fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        log::warn!("[nexus] 跳过符号链接/联接点（不跟随复制）: {}", src.display());
        acc.skipped.push(src.to_string_lossy().replace('\\', "/"));
        return Ok(false);
    }
    acc.entries += 1;
    if acc.entries > MAX_COPY_ENTRIES {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "复制的条目数超过上限"));
    }
    if meta.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            // 子项是链接就跳过：目录本身已经落地，整棵算成功（这条 bool 只管最外层）
            copy_recursive_at(&entry.path(), &dst.join(entry.file_name()), acc, depth + 1)?;
        }
    } else {
        std::fs::copy(src, dst)?;
    }
    Ok(true)
}

/// 源**本身**是不是符号链接/联接点。
///
/// 单独判它是因为这一支有两个超出"复制时跳过"的连带后果：
/// - **不算同名冲突**：它永远复制不出东西，问用户"要不要覆盖"没有意义；
/// - **绝不能触发覆盖**：否则「覆盖」会先把目标里那一项移进回收站，然后什么都不放进去
///   ——用户丢了一个文件，换来一片空白。
fn is_link_source(src: &Path) -> bool {
    std::fs::symlink_metadata(src).map(|m| m.file_type().is_symlink()).unwrap_or(false)
}

/// 目标路径去重：已存在同名时自动追加 " (2)"、" (3)"…（与资源管理器同文件惯例一致）。
/// 扩展名拆分为后缀（"a.txt" → stem "a" + ext ".txt"）；".gitignore" 这类以点开头的整体作为主名。
///
/// 重名探测有上限：目标目录被其它进程持续写入时，原实现的无界 `loop` 会一直自增下去。
/// 超过上限返回 None，由调用方作为该源的失败上报（而不是静默丢弃或无限循环）。
fn unique_dest_path(target: &Path, name: &str) -> Option<PathBuf> {
    let candidate = target.join(name);
    if !candidate.exists() {
        return Some(candidate);
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{}", e)),
        _ => (name, String::new()),
    };
    for i in 2..MAX_DEST_ATTEMPTS {
        let candidate = target.join(format!("{} ({}){}", stem, i, ext));
        if !candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

// ─── 删除到回收站 ────────────────────────────────────────────

/// 删除前的可删性校验。抽成独立函数是为了**脱离 AppState 与真实回收站**测这套判定。
///
/// 用 `check_write_path_allowed` 而不是 `check_path_allowed`：**目标不存在时就到了这里**
/// （树是快照，磁盘上的文件可能已被构建脚本/外部程序删掉），而它内部是 `canonicalize`，
/// 对不存在的路径必然失败——于是"文件已被删掉"会一律报成「访问被拒绝」，把用户指向权限问题。
/// 写路径校验对不存在的目标退回**父目录**校验，这正是我们要的：能给出准确文案，
/// 又不放松范围（越界路径无论存在与否都恒报「访问被拒绝」——不存在的东西本就无可删对象，
/// 这一支不会让任何越界删除变得可能）。
///
/// 两条检查的**顺序不能反**：范围在前。反过来先查存在性的话，调用方（被攻陷的 webview）
/// 就能拿它当"这个路径存不存在"的探针。
fn ensure_deletable(paths: &PathAllowlist, db: &Database, path: &str) -> Result<(), String> {
    paths.check_write_path_allowed(db, path)?;
    // symlink_metadata：不跟随链接（与 copy_recursive 同口径）。删除动作删的是链接本身、
    // 不是它的目标（`trash` 只 canonicalize 父目录），所以判存在也该按链接判
    std::fs::symlink_metadata(path)
        .map(|_| ())
        .map_err(|e| format!("无法访问（可能已被外部删除）: {}（{}）", path, e))
}

/// `trash` 的错误 `Display` 打的是 Debug 形态（`Os { code: -2147024891, description: "…" }`），
/// 这里只做**提取**、不做猜测：Windows 那句原文由系统按本地化资源给出，
/// 替它编一句"可能因为文件被占用"反而会把用户指向错误的方向（铁律 8 要的是原因，不是安慰）。
fn describe_trash_error(e: &trash::Error) -> String {
    match e {
        trash::Error::Os { code, description } => format!("{}（0x{:08X}）", description, *code as u32),
        trash::Error::CanonicalizePath { original } => format!("路径无效或已不存在: {}", original.display()),
        trash::Error::CouldNotAccess { target } => format!("无法访问: {}", target),
        trash::Error::TargetedRoot => "不能删除驱动器根目录".into(),
        trash::Error::ConvertOsString { original } => format!("路径无法转换为字符串: {}", original.to_string_lossy()),
        // RestoreCollision / RestoreTwins 只可能从 restore 出来，delete 走不到；
        // 非 Windows 上还会多出 FileSystem 变体，故必须有兜底臂
        other => format!("{:?}", other),
    }
}

/// 删除文件/目录到系统回收站（文件树右键「删除」）。
///
/// 为什么走回收站而不是 `std::fs::remove_file`：删的是用户真实的源码与产物，一次误点即
/// 不可逆；资源管理器、IDEA、VS Code 的删除默认也都进回收站。
///
/// 一个已知边界（`trash` 5.2 调用侧的固定行为，我们改不了，写在这里免得日后被当成 bug）：
/// **不保证一定进回收站**。它带 `FOF_ALLOWUNDO | FOF_NO_UI` 调 `IFileOperation`
/// （见该 crate 的 `windows.rs`），而 `ALLOWUNDO` 按 shell 的语义是"能回收才回收"：
/// 超出回收站配额的单个大文件、网络驱动器上的文件本就无法回收，会走永久删除；
/// 而 `FOF_NO_UI` 又把 `FOF_WANTNUKEWARNING` 本要弹的那句警告一并压掉了。
/// 该 crate 没暴露 flags，从调用侧关不掉——**"删到回收站"是常态而非保证**，
/// 界面文案因此不去承诺"一定能找回"。
///
/// （别在这里写"超过 MAX_PATH 的长路径会失败"：`trash` 虽然把 canonicalize 的 `\\?\`
/// 前缀剥掉再交给 `SHCreateItemFromParsingName`，看着像不支持超长路径，但本机实测
/// **391 字符的深层文件照常进回收站**（2026-09-22）——那是读源码推出来的结论，实测推翻了它。）
#[tauri::command]
pub async fn delete_path(state: State<'_, AppState>, path: String) -> Result<(), String> {
    ensure_deletable(&state.paths, &state.db, &path)?;
    // 同步阻塞调用（COM 操作），放阻塞线程池；校验已在上一步返回，闭包里不持任何锁
    tokio::task::spawn_blocking(move || {
        trash::delete(&path).map_err(|e| format!("删除失败: {}", describe_trash_error(&e)))
    })
    .await
    .map_err(|e| format!("删除任务执行失败: {}", e))?
}

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::init_schema;

    /// 测试用白名单：一个内存库 + 一个已登记的临时目录，返回 (db, paths, 根路径)。
    ///
    /// `tag` 必须逐测试不同：这些用例并行跑，而每个都会 `remove_dir_all(&root)` 收尾——
    /// 只按 pid 命名的话，先跑完的那个会把别人正在用的根删掉，
    /// 于是 `canonicalize` 失败 → 根集合变空 → 其余用例集体报「访问被拒绝」。
    fn scoped_allowlist(tag: &str) -> (Database, PathAllowlist, PathBuf) {
        let conn = rusqlite::Connection::open_in_memory().expect("内存库");
        init_schema(&conn).expect("建表");
        let db = Database::from_connection(conn);
        let root = std::env::temp_dir().join(format!("nexus_ut_del_{}_{}", tag, std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let paths = PathAllowlist::new();
        paths.set_project_root(Some(root.to_string_lossy().to_string())).unwrap();
        (db, paths, root)
    }

    #[test]
    fn test_unique_dest_path_no_conflict() {
        let dir = std::env::temp_dir();
        let name = format!("nexus_ut_{}", std::process::id());
        let dst = unique_dest_path(&dir, &name).expect("应能确定目标路径");
        assert_eq!(dst, dir.join(&name)); // 无同名 → 原样
    }

    #[test]
    fn test_unique_dest_path_with_extension() {
        let dir = std::env::temp_dir();
        // 创建真实冲突文件，验证序号后缀
        let name = format!("nexus_ut_ext_{}.txt", std::process::id());
        let existing = dir.join(&name);
        std::fs::write(&existing, "x").unwrap();
        let dst = unique_dest_path(&dir, &name).expect("应能确定目标路径");
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
        let dst = unique_dest_path(&dir, name).expect("应能确定目标路径");
        assert_eq!(dst, dir.join(".gitignore"));
    }

    /// 重名探测有上限：目标目录被占满时返回 None，而不是无限自增
    #[test]
    fn test_unique_dest_path_bounded_returns_none() {
        let dir = std::env::temp_dir().join(format!("nexus_ut_full_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let name = "a.txt";
        // 占满全部候选名（原名 + (2)...(MAX_DEST_ATTEMPTS-1)）
        for i in 1..MAX_DEST_ATTEMPTS {
            let p = if i == 1 { dir.join(name) } else { dir.join(format!("a ({}).txt", i)) };
            std::fs::write(&p, "x").unwrap();
        }
        assert!(unique_dest_path(&dir, name).is_none(), "候选名用尽应返回 None");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 每个用例一个独立根：这些用例并行跑，共用一个根时先跑完的那个会 `remove_dir_all`
    /// 掉别人正在用的目录（症状是一片用例集体报"访问被拒绝"，看着像权限问题）
    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nexus_ut_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 建一个目录联接（junction）。`mklink /J` 不需要管理员权限（符号链接才需要）；
    /// 建不出来（受限环境）返回 false，用例据此跳过
    #[cfg(windows)]
    fn make_junction(link: &Path, real: &Path) -> bool {
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(real)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    fn make_junction(link: &Path, real: &Path) -> bool {
        std::os::unix::fs::symlink(real, link).is_ok()
    }

    /// 探测只报"目标里已有的同名项"，且同名只报一次
    #[test]
    fn test_detect_conflicts_reports_existing_once_per_name() {
        let base = tmp_dir("conflict");
        let target = base.join("dst");
        std::fs::create_dir_all(target.join("assets")).unwrap();
        std::fs::write(target.join("config.json"), "old").unwrap();

        let mut srcs = Vec::new();
        for sub in ["a", "b"] {
            let dir = base.join(sub);
            std::fs::create_dir_all(&dir).unwrap();
            let f = dir.join("config.json");
            std::fs::write(&f, "new").unwrap();
            srcs.push(f.to_string_lossy().into_owned());
        }
        let target_canon = std::fs::canonicalize(&target).unwrap();

        let conflicts = detect_conflicts(&target, &target_canon, &srcs);
        assert_eq!(conflicts.len(), 1, "两个同名源撞的是目标里同一项，只该报一次");
        assert_eq!(conflicts[0].name, "config.json");
        assert!(!conflicts[0].existing_is_dir);

        // 目录同名要标出来：弹框据此说清"整棵替换"的代价
        let dir_src = base.join("c/assets");
        std::fs::create_dir_all(&dir_src).unwrap();
        let conflicts = detect_conflicts(&target, &target_canon, &[dir_src.to_string_lossy().into_owned()]);
        assert_eq!(conflicts.len(), 1);
        assert!(conflicts[0].existing_is_dir);

        // 目标里没有的源不算冲突
        let fresh = base.join("a/fresh.txt");
        std::fs::write(&fresh, "x").unwrap();
        assert!(detect_conflicts(&target, &target_canon, &[fresh.to_string_lossy().into_owned()]).is_empty());

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 原地复制（同名目标就是源自己）不算冲突——否则弹框里选「覆盖」就是拿文件覆盖它自己。
    /// 且它不能把**后面**那个真正撞名的同名源一起吞掉。
    #[test]
    fn test_detect_conflicts_ignores_self_paste() {
        let base = tmp_dir("selfpaste");
        let f = base.join("a.txt");
        std::fs::write(&f, "x").unwrap();
        let target_canon = std::fs::canonicalize(&base).unwrap();

        assert!(
            detect_conflicts(&base, &target_canon, &[f.to_string_lossy().into_owned()]).is_empty(),
            "把文件粘回它自己的目录是最常用的复制方式，不该被当成同名冲突"
        );

        // 同名、但来自别处的那个源才是真冲突
        let other = base.join("sub");
        std::fs::create_dir_all(&other).unwrap();
        let other_f = other.join("a.txt");
        std::fs::write(&other_f, "y").unwrap();
        let conflicts = detect_conflicts(
            &base,
            &target_canon,
            &[f.to_string_lossy().into_owned(), other_f.to_string_lossy().into_owned()],
        );
        assert_eq!(conflicts.len(), 1, "原地复制的那个不能把真正撞名的同名源吞掉");
        assert_eq!(conflicts[0].name, "a.txt");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 源**本身**是链接：不算同名冲突、不触发覆盖、也不该出现在 `created` 里。
    ///
    /// 守的是一条"删掉旧文件却什么都没放进去"的路径：`copy_recursive` 对链接只记一条
    /// skipped 就返回，若把它当成可覆盖的同名源，用户会丢掉目标里那一项换来一片空白。
    /// 同时守住 CQ-41 的另一半：没落盘就不该进 `created`（否则同一句提示里既说"已粘贴"、
    /// 又说"它被跳过了"）。
    #[test]
    fn test_link_source_never_conflicts_never_lands() {
        let base = tmp_dir("linksrc");
        let target = base.join("dst");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("link"), "old").unwrap(); // 目标里的同名项
        let real = base.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let link = sub.join("link");
        if !make_junction(&link, &real) {
            return; // 受限环境建不出联接，跳过
        }
        let srcs = [link.to_string_lossy().into_owned()];

        let target_canon = std::fs::canonicalize(&target).unwrap();
        assert!(
            detect_conflicts(&target, &target_canon, &srcs).is_empty(),
            "链接源永远复制不出东西，问用户要不要覆盖没有意义"
        );

        let mut claimed = HashSet::new();
        let plan = plan_dest(&target, "link", &link, true, &mut claimed).unwrap();
        assert!(plan.replacing.is_none(), "链接源触发覆盖 = 删掉目标里那份再什么都不放");

        let r = done(paste_into(&target, Some(ConflictPolicy::Overwrite), &srcs).unwrap());
        assert!(r.created.is_empty(), "没落盘就不该进 created: {:?}", r.created);
        assert_eq!(r.skipped.len(), 1, "它该被记成跳过的链接");
        assert!(r.replaced.is_empty(), "不该动目标里的同名项");
        assert_eq!(std::fs::read_to_string(target.join("link")).unwrap(), "old", "同名项必须原封不动");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 落点决策：覆盖的三个前提（策略允许 / 真的撞名 / 本批没占用过），其余一律改名去重
    #[test]
    fn test_plan_dest_overwrite_rules() {
        let base = tmp_dir("plandest");
        std::fs::write(base.join("a.txt"), "old").unwrap();
        let src_dir = base.join("sub");
        std::fs::create_dir_all(&src_dir).unwrap();
        let src = src_dir.join("a.txt");
        std::fs::write(&src, "new").unwrap();
        let mut claimed = HashSet::new();

        // ① 撞名但没开覆盖 → 改名（本功能之前的行为，必须原样保留）
        let plan = plan_dest(&base, "a.txt", &src, false, &mut claimed).unwrap();
        assert_eq!(plan.dst, base.join("a (2).txt"));
        assert!(plan.replacing.is_none());

        // ② 开了覆盖 → 落到原名，并且要清掉同名项
        let plan = plan_dest(&base, "a.txt", &src, true, &mut claimed).unwrap();
        assert_eq!(plan.dst, base.join("a.txt"));
        assert_eq!(plan.replacing, Some(base.join("a.txt")));

        // ③ 本批已经覆盖过这个落点（剪贴板里两个同名源）→ 第二个必须改名，
        //    否则它会把刚落地的第一个盖掉
        let plan = plan_dest(&base, "a.txt", &src, true, &mut claimed).unwrap();
        assert!(plan.replacing.is_none(), "同一落点在一次粘贴里只能覆盖一次");
        assert_eq!(plan.dst, base.join("a (2).txt"));

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 开着覆盖也不能覆盖源自己：`dst == src` 的复制没有任何正当语义
    #[test]
    fn test_plan_dest_never_overwrites_source_itself() {
        let base = tmp_dir("planself");
        let f = base.join("a.txt");
        std::fs::write(&f, "x").unwrap();
        let mut claimed = HashSet::new();

        let plan = plan_dest(&base, "a.txt", &f, true, &mut claimed).unwrap();
        assert!(plan.replacing.is_none(), "原地复制必须落到改名分支，与策略无关");
        assert_eq!(plan.dst, base.join("a (2).txt"));

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 取"直接执行"的那一半结果；要求决策则说明探测轮的判据失效了
    fn done(res: PasteResponse) -> PasteFilesResult {
        match res {
            PasteResponse::Done(r) => r,
            PasteResponse::Conflict { conflicts, .. } => panic!(
                "期望直接执行，却要求先决策：{:?}",
                conflicts.iter().map(|c| &c.name).collect::<Vec<_>>()
            ),
        }
    }

    /// 造一个源目录 `sub/`，里面有 `a.txt`（与目标同名，内容 "new"）和 `b.txt`（永不撞名）。
    /// 源在**独立的子目录**里，好让"同名但来自别处"这个前提真的成立
    /// ——同目录同名的源是"原地复制"，走的是另一条分支
    fn two_sources(base: &Path, tag: &str) -> (String, String) {
        let dir = base.join(format!("{}_src", tag));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.txt");
        std::fs::write(&a, "new").unwrap();
        let b = dir.join("b.txt");
        std::fs::write(&b, "b").unwrap();
        (a.to_string_lossy().into_owned(), b.to_string_lossy().into_owned())
    }

    /// 目标目录 `dst/`，里面已有一个 `a.txt`（内容 "old"）
    fn target_with_a(base: &Path) -> PathBuf {
        let target = base.join("dst");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("a.txt"), "old").unwrap();
        target
    }

    /// 有同名冲突时**必须先问**，且探测轮一个字节都不能落盘
    #[test]
    fn test_paste_into_probe_copies_nothing() {
        let base = tmp_dir("probe");
        let target = target_with_a(&base);
        let (a, b) = two_sources(&base, "s");

        let res = paste_into(&target, None, &[a, b]).unwrap();
        match res {
            PasteResponse::Conflict { conflicts, sources } => {
                assert_eq!(conflicts.len(), 1);
                assert_eq!(conflicts[0].name, "a.txt");
                assert_eq!(sources.len(), 2, "源清单要原样回传，供决策那轮做一致性校验");
            }
            PasteResponse::Done(_) => panic!("有同名冲突时必须先问，而不是直接粘"),
        }
        // 探测轮不落盘：连不撞名的 b.txt 也不能被提前复制
        assert!(!target.join("b.txt").exists(), "探测轮不该复制任何东西");
        assert_eq!(std::fs::read_to_string(target.join("a.txt")).unwrap(), "old");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 「跳过」只跳过同名项，其余照常复制；同名项原封不动
    #[test]
    fn test_paste_into_skip_leaves_existing_untouched() {
        let base = tmp_dir("skip");
        let target = target_with_a(&base);
        let (a, b) = two_sources(&base, "s");

        let r = done(paste_into(&target, Some(ConflictPolicy::Skip), &[a, b]).unwrap());
        assert!(r.failed.is_empty(), "不该有失败: {:?}", r.failed);
        assert_eq!(std::fs::read_to_string(target.join("a.txt")).unwrap(), "old", "跳过就不该动同名项");
        assert_eq!(r.skipped_by_user.len(), 1, "跳过的源要上报，否则界面会把它报成已粘贴");
        assert_eq!(r.created.len(), 1, "不撞名的那个照常复制");
        assert!(r.created[0].ends_with("/b.txt"), "实际: {}", r.created[0]);
        assert!(r.replaced.is_empty());

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 「保留两者」= 本功能之前的行为：同名项一个都不动，新来的落成 " (2)"
    #[test]
    fn test_paste_into_rename_keeps_both() {
        let base = tmp_dir("rename");
        let target = target_with_a(&base);
        let (a, b) = two_sources(&base, "s");

        let r = done(paste_into(&target, Some(ConflictPolicy::Rename), &[a, b]).unwrap());
        assert_eq!(std::fs::read_to_string(target.join("a.txt")).unwrap(), "old", "原文件必须原样留着");
        assert_eq!(std::fs::read_to_string(target.join("a (2).txt")).unwrap(), "new");
        assert!(r.replaced.is_empty(), "保留两者不该动任何已有数据");
        assert_eq!(r.created.len(), 2);

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 「覆盖」的**端到端**：新内容落到原名，老项真进回收站（而不是被静默永久删除），
    /// 且剪贴板里两个同名源时第二个不会把刚落地的第一个盖掉。
    ///
    /// 默认忽略——它会往用户回收站里塞测试文件（与 `test_trash_real_file_dir_and_junction`
    /// 同一个理由）。本机手动跑：`cargo test --lib -- --ignored paste_into_overwrite`
    #[test]
    #[ignore = "会往用户回收站里塞测试文件"]
    fn test_paste_into_overwrite_replaces_in_place() {
        /// 回收站里是否躺着一个叫这个名字的条目（"其实是被永久删了"的判别式）
        fn in_recycle_bin(name: &str) -> bool {
            trash::os_limited::list()
                .expect("回收站应可枚举")
                .iter()
                .any(|i| i.name == std::ffi::OsStr::new(name))
        }

        let base = tmp_dir("overwrite");
        let target = target_with_a(&base);
        let (a, _) = two_sources(&base, "s");

        let r = done(paste_into(&target, Some(ConflictPolicy::Overwrite), std::slice::from_ref(&a)).unwrap());
        assert!(r.failed.is_empty(), "不该有失败: {:?}", r.failed);
        assert_eq!(std::fs::read_to_string(target.join("a.txt")).unwrap(), "new", "新内容要落到原名上");
        assert_eq!(r.created.len(), 1);
        assert!(r.created[0].ends_with("/a.txt"), "落的是原名而不是 a (2).txt: {}", r.created[0]);
        assert_eq!(r.replaced.len(), 1, "被替换掉的原项必须上报，否则用户不知道老文件去哪了");
        assert!(in_recycle_bin("a.txt"), "老项应躺在回收站里，而不是被永久删除");

        // 两个同名源（来自不同目录）：第二个必须落成 " (2)"，不能把刚落地的第一个盖掉
        let (a2, _) = two_sources(&base, "s2");
        let r = done(paste_into(&target, Some(ConflictPolicy::Overwrite), &[a.clone(), a2]).unwrap());
        assert!(r.failed.is_empty(), "不该有失败: {:?}", r.failed);
        assert_eq!(r.replaced.len(), 1, "同一个落点在一次粘贴里只能被覆盖一次");
        assert!(target.join("a (2).txt").exists(), "第二个同名源该落到 a (2).txt");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 普通目录树照常复制（回归：加了 depth/entries 参数后基本功能不变）
    #[test]
    fn test_copy_recursive_copies_tree() {
        let base = std::env::temp_dir().join(format!("nexus_ut_copy_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let src = base.join("src");
        std::fs::create_dir_all(src.join("a/b")).unwrap();
        std::fs::write(src.join("a/b/f.txt"), "x").unwrap();
        let mut acc = CopyAcc::default();
        copy_recursive(&src, &base.join("dst"), &mut acc).unwrap();
        assert_eq!(std::fs::read_to_string(base.join("dst/a/b/f.txt")).unwrap(), "x");
        assert_eq!(acc.entries, 4); // src、a、b 三个目录 + 1 个文件
        assert!(acc.skipped.is_empty(), "没有链接就不该有跳过项");
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
        let mut acc = CopyAcc::default();
        let err = copy_recursive(&src, &base.join("o"), &mut acc).unwrap_err();
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
            let mut acc = CopyAcc::default();
            copy_recursive(&src, &base.join("dst"), &mut acc).unwrap();
            assert!(!base.join("dst/link").exists(), "目录联接/符号链接不应被跟随复制");
            // 跳过必须被记下来：只写日志的话，界面会照报"已粘贴 N 个项目"，
            // 而复制出来的是个少了一整棵子树的目录
            assert_eq!(acc.skipped.len(), 1, "跳过的链接要进 skipped，实际: {:?}", acc.skipped);
            assert!(acc.skipped[0].ends_with("link"), "记的应是链接自己的路径，实际: {:?}", acc.skipped);
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// **真跑一次剪贴板**——默认忽略：它会占用（覆盖）当前用户的剪贴板内容。
    /// 本机手动跑：`cargo test --lib -- --ignored write_files_to_clipboard`
    ///
    /// 守的是一处真实故障（用户报告："复制完到资源管理器粘贴，有时粘出来的不是刚复制的那个
    /// 文件"）：写剪贴板前**必须先清空**，否则上一个持有者留下的格式原样留着，
    /// 而粘贴方（资源管理器）认的是它自己那套 shell 格式。见 `write_files_to_clipboard` 的说明。
    #[test]
    #[ignore = "需要真实剪贴板，会覆盖用户当前剪贴板"]
    #[cfg(windows)]
    fn test_write_files_to_clipboard_clears_previous() {
        use clipboard_win::{formats, get_clipboard, set_clipboard};

        let base = std::env::temp_dir().join(format!("nexus_ut_clip_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&base);
        let file = base.join("nexus-复制测试.txt");
        std::fs::write(&file, "x").unwrap();
        let path = file.to_string_lossy().to_string();

        // 模拟"别的程序先在剪贴板上留了自己的格式"。这里用文本当替身：
        // 机制是"SetClipboardData 不会移除别的格式"，与具体是哪个格式无关
        set_clipboard(formats::Unicode, "MARKER-上一个持有者留下的格式").expect("放入标记格式");

        write_files_to_clipboard(std::slice::from_ref(&path)).expect("写入文件列表");

        let files: Vec<String> = get_clipboard(formats::FileList).expect("该读得到文件列表");
        assert!(
            files.iter().any(|p| p.ends_with("nexus-复制测试.txt")),
            "文件列表应是我们刚放进去的那个，实际: {:?}",
            files
        );
        assert!(
            get_clipboard::<String, _>(formats::Unicode).is_err(),
            "上一个持有者的格式必须已被清掉——留着它，资源管理器粘贴时用的就是它，\
             而不是刚复制的这份（这正是用户报的那个故障）"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    // ── 粘贴守卫：哪些该拦、哪些该放行 ────────────────────────
    //
    // 这几条是把一次实测的判据表钉住：原实现写的是 `src.starts_with(target)`（"源在目标里"），
    // 方向反了——五个最常用场景里判错三个（下面前两条该拦却放行，中间两条该放行却拦下）。
    // 判错的代价不对称：漏拦 = 往用户项目里写一棵 A/B/A/B/… 的垃圾树（实测写到目录层级上限
    // 才停、留下 65 个条目）；误拦 = 一句"粘不动"。
    #[test]
    fn test_paste_into_own_subtree_is_blocked() {
        let base = std::env::temp_dir().join(format!("nexus_ut_paste_guard_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let a = base.join("a");
        let b = a.join("b"); // a 的子目录：粘进这里会让 a 自我嵌套
        std::fs::create_dir_all(&b).unwrap();
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap();

        assert!(is_pasting_dir_into_itself(&a, &canon(&b)), "目录粘进自己的子目录 → 必须拦");
        assert!(is_pasting_dir_into_itself(&a, &canon(&a)), "目录粘进它自己 → 必须拦");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_paste_in_place_is_allowed() {
        let base = std::env::temp_dir().join(format!("nexus_ut_paste_ok_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let a = base.join("a");
        let b = a.join("b");
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("x.txt"), "x").unwrap();
        let x = a.join("x.txt");
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap();

        // 这三条都是最常用的操作，全靠后面的 unique_dest_path 生成 " (2)" 名字。
        // 原实现把这三条全拦了，于是"原地复制一份""复制到上一级"根本用不了
        assert!(!is_pasting_dir_into_itself(&b, &canon(&a)), "子目录粘到父目录 → 应放行（复制出 b (2)）");
        assert!(!is_pasting_dir_into_itself(&x, &canon(&a)), "文件粘到它自己所在的目录 → 应放行（复制出 x (2).txt）");
        assert!(!is_pasting_dir_into_itself(&x, &canon(&b)), "文件粘到别的目录 → 应放行");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 联接点/符号链接不参与判定：`copy_recursive` 本就不跟随它（只记一条 skipped），
    /// 没有"一边读一边往里写"的递归可言——拦下它反而会把"想删/想挪走一个链接"的场景憋死
    #[test]
    fn test_paste_guard_ignores_links() {
        let base = std::env::temp_dir().join(format!("nexus_ut_paste_link_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let real = base.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = base.join("link");
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
            // 链接指向 real，而目标就在 real 里面——若按"跟随链接"判就会误拦
            assert!(
                !is_pasting_dir_into_itself(&link, &std::fs::canonicalize(&real).unwrap()),
                "链接不参与子树判定（它不会被跟随复制，也就不会递归）"
            );
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    // ── delete_path 的可删性校验 ──────────────────────────────

    #[test]
    fn test_ensure_deletable_accepts_path_inside_root() {
        let (db, paths, root) = scoped_allowlist("ok");
        let f = root.join("a.txt");
        std::fs::write(&f, "x").unwrap();
        assert!(ensure_deletable(&paths, &db, &f.to_string_lossy()).is_ok());
        assert!(ensure_deletable(&paths, &db, &root.to_string_lossy()).is_ok(), "目录同样可删");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **顺序**是这个函数值钱的地方：白名单必须在存在性之前。
    /// 判定反过来的话，"越界且存在"与"越界且不存在"会得到不同答复，
    /// 调用方（被攻陷的 webview）就能拿它当"这个路径存不存在"的探针。
    #[test]
    fn test_ensure_deletable_rejects_out_of_scope_without_leaking_existence() {
        let (db, paths, root) = scoped_allowlist("scope");
        let outside = std::env::temp_dir().join(format!("nexus_ut_del_out_{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        let existing = outside.join("b.txt");
        std::fs::write(&existing, "x").unwrap();
        let missing = outside.join("never-existed.txt");

        let e1 = ensure_deletable(&paths, &db, &existing.to_string_lossy()).unwrap_err();
        let e2 = ensure_deletable(&paths, &db, &missing.to_string_lossy()).unwrap_err();
        assert_eq!(e1, "访问被拒绝");
        assert_eq!(e1, e2, "越界路径的答复不得随'是否存在'变化");

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// 范围内但不在了 → 说"可能已被外部删除"，而不是笼统的「访问被拒绝」。
    /// 树里那行还画着、磁盘上已经没了时，用户需要能分辨是这一种。
    #[test]
    fn test_ensure_deletable_missing_inside_root_says_gone() {
        let (db, paths, root) = scoped_allowlist("gone");
        let err = ensure_deletable(&paths, &db, &root.join("gone.txt").to_string_lossy()).unwrap_err();
        assert!(err.contains("可能已被外部删除"), "实际: {}", err);
        assert!(!err.contains("访问被拒绝"), "实际: {}", err);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 错误翻译只做提取：Windows 的原文与错误码都要留着，用户报障时能对上
    #[test]
    fn test_describe_trash_error_extracts_os_message() {
        let e = trash::Error::Os { code: 0x8007_0005u32 as i32, description: "Access is denied.".into() };
        let msg = describe_trash_error(&e);
        assert!(msg.contains("Access is denied."), "实际: {}", msg);
        assert!(msg.contains("0x80070005"), "错误码不能丢，实际: {}", msg);
    }

    /// **真跑一次回收站**——默认忽略：它需要真实的回收站，GitHub runner 上不保证有。
    /// 本机手动跑：`cargo test --lib -- --ignored trash_real`
    ///
    /// 它验证两条我们**依赖、但只在真实 shell 调用里才成立**的事（铁律 16 / 19）：
    /// 1. 普通文件与非空目录**确实进的是回收站**——「原位置没了」这一条单独不算数，
    ///    永久删除同样会让它消失，必须回回收站里查到这个条目才算数；
    /// 2. 删**目录联接**时删掉的是联接本身——目标目录必须原样还在。
    ///    （`trash` 只 canonicalize 父目录、链接路径原样交给 shell，这里是它的运行时证据：
    ///    文件树里联接是会显示成目录的，若它跟随链接，一次右键就会带走目标下的全部内容。）
    #[test]
    #[ignore = "需要真实回收站，本机手动跑"]
    fn test_trash_real_file_dir_and_junction() {
        let base = std::env::temp_dir().join(format!("nxut_trash_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        /// 回收站里是否躺着一个叫这个名字的条目（"其实是被永久删了"的判别式）
        fn in_recycle_bin(name: &str) -> bool {
            trash::os_limited::list()
                .expect("回收站应可枚举")
                .iter()
                .any(|i| i.name == std::ffi::OsStr::new(name))
        }

        // 1. 普通文件
        let file = base.join("plain.txt");
        std::fs::write(&file, "x").unwrap();
        trash::delete(&file).expect("普通文件应能进回收站");
        assert!(!file.exists(), "文件应从原位置消失");
        assert!(in_recycle_bin("plain.txt"), "文件必须躺在回收站里（而不是被永久删除）");

        // 2. 非空目录
        let dir = base.join("sub");
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        std::fs::write(dir.join("inner/f.txt"), "x").unwrap();
        trash::delete(&dir).expect("非空目录应能进回收站");
        assert!(!dir.exists(), "目录应从原位置消失");
        assert!(in_recycle_bin("sub"), "目录必须躺在回收站里（而不是被永久删除）");

        // 3. 目录联接：删链接，目标必须幸存
        let target = base.join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("keep.txt"), "x").unwrap();
        let link = base.join("link");
        #[cfg(windows)]
        let created = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        #[cfg(not(windows))]
        let created = std::os::unix::fs::symlink(&target, &link).is_ok();
        if created {
            trash::delete(&link).expect("目录联接应能进回收站");
            assert!(!link.exists(), "联接本身应被删掉");
            assert!(target.join("keep.txt").exists(), "联接的目标不得被连带删除");
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
