//! Node 运行时管理（nvm-windows）。
//!
//! **定位**：这是一个**独立工具**，不参与服务配置——"某个服务用哪个 Node"是另一件事
//! （那个要做的话是"启动时给该进程注入 PATH"，与这里无关，两者不共用状态）。
//!
//! **为什么整件事都包 `nvm.exe`，而不是自己下载解压**：
//! - 用户已经在 nvm 里配好了镜像（npmmirror）、arch 这些，nvm 全都认；自己实现得把那份
//!   配置再抄一遍，将来换个源两边就对不上了
//! - 自己实现要加 HTTP 客户端 + zip 解压两个依赖，只为做一件 nvm 已经做好的事
//!
//! 唯一自己动手的是**列已装版本**：扫目录比解析 `nvm list` 的输出稳，也不用起进程。

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::Serialize;
use tauri::command;

/// 找到的 nvm-windows 安装
struct NvmInstall {
    /// `nvm.exe` 的绝对路径
    exe: PathBuf,
    /// 版本存放目录（settings.txt 里的 `root`）
    root: PathBuf,
}

/// 本机 Node 运行时状态（左上角「Node 版本」面板的全部数据源）
#[derive(Serialize)]
pub struct NodeRuntimeStatus {
    /// 没检测到 nvm-windows 时为 false——界面据此显示引导，而不是当成"一个版本都没装"
    pub available: bool,
    /// 版本存放目录
    pub root: String,
    /// 已安装的版本（新 → 旧），形如 `v22.22.1`
    pub installed: Vec<String>,
    /// 当前生效的版本（读 `NVM_SYMLINK` 软链指向哪）；空 = 未设置
    pub current: String,
    /// 出错时的原文（如 settings.txt 里没有 root）——让界面能说出"为什么不可用"
    pub reason: String,
}

/// 可安装的版本（`nvm list available` 的四列）
#[derive(Serialize)]
pub struct AvailableNodeVersions {
    pub current: Vec<String>,
    pub lts: Vec<String>,
    pub old_stable: Vec<String>,
    pub old_unstable: Vec<String>,
}

/// 一条 nvm 命令的执行结果
///
/// 成败看**退出码**，不看输出文本——nvm 的输出是给人看的，措辞会随版本变；
/// 原文照样带回去，失败时界面直接贴出来，比我们转述准确。
#[derive(Serialize)]
pub struct NvmCommandResult {
    pub ok: bool,
    /// 合并后的 stdout + stderr 原文
    pub output: String,
}

/// 找本机的 nvm-windows。
///
/// 三条信息各有各的来路，别混：
/// - `nvm.exe` 在哪 → `NVM_HOME` 环境变量
/// - **版本存哪** → 读它自己的 `settings.txt`（`nvm root` 可以改过，环境变量回答不了）
/// - 当前用哪个 → `NVM_SYMLINK` 软链指向谁
fn find_nvm() -> Result<NvmInstall, String> {
    let home = std::env::var("NVM_HOME").unwrap_or_default();
    if home.trim().is_empty() {
        return Err("没有找到 NVM_HOME 环境变量——本机可能没装 nvm-windows".into());
    }
    let exe = Path::new(home.trim()).join("nvm.exe");
    if !exe.is_file() {
        return Err(format!("{} 不存在，nvm-windows 的安装可能不完整", exe.display()));
    }

    let settings_path = Path::new(home.trim()).join("settings.txt");
    let settings = std::fs::read_to_string(&settings_path)
        .map_err(|e| format!("读不到 {}: {}", settings_path.display(), e))?;
    let root = settings
        .lines()
        .find_map(|l| l.trim().strip_prefix("root:"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{} 里没有 root 配置", settings_path.display()))?;

    Ok(NvmInstall { exe, root: PathBuf::from(root) })
}

/// 当前生效的版本 = `NVM_SYMLINK` 软链指向哪个目录。
///
/// 读不到就返回空串（软链没建、nvm 没初始化过都算"未设置"），不报错——
/// 那是正常状态，不是故障。
fn current_version() -> String {
    let Ok(link) = std::env::var("NVM_SYMLINK") else { return String::new() };
    if link.trim().is_empty() {
        return String::new();
    }
    // nvm-windows 建的是 junction；read_link 对两者都能读出目标路径
    let Ok(target) = std::fs::read_link(link.trim()) else { return String::new() };
    target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

/// 等软链落定（`nvm use` 之后调用）。
///
/// 抽成独立函数是为了**能被测试直接调用**——测试必须走命令里那段一模一样的等待，
/// 否则测的就不是它了（在测试里重抄一遍等待逻辑，等于什么都没验证）。
///
/// `before` 是切换前的当前版本：等到"非空且与它不同"就算落定。用"变了"而不是
/// "等于目标版本"是因为目标可能是 `16.20.2` 这种不带 `v` 的写法，与目录名对不上。
fn wait_for_switch(before: &str) {
    for _ in 0..20 {
        let now = current_version();
        if !now.is_empty() && now != before {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// 扫 root 下的 `v*` 目录。比解析 `nvm list` 稳，也不起进程。
fn installed_versions(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with('v') || n.starts_with('V'))
        .collect();
    // 版本比较复用 dsh 那份（同为语义化版本：主段数值、预发布段在后）——排序要
    // 新版本在前，所以参数反过来传
    out.sort_by(|a, b| crate::core::ai::version_cmp(b, a).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// 本机 Node 运行时状态。**不返回 Err**：nvm 没装是正常情况，由 `available` 表达，
/// 让面板能显示"怎么装"而不是弹一个错误。
#[command]
pub fn get_node_runtime() -> NodeRuntimeStatus {
    let unavailable = |reason: String| NodeRuntimeStatus {
        available: false,
        root: String::new(),
        installed: Vec::new(),
        current: String::new(),
        reason,
    };

    match find_nvm() {
        Ok(nvm) => NodeRuntimeStatus {
            available: true,
            root: nvm.root.to_string_lossy().to_string(),
            installed: installed_versions(&nvm.root),
            current: current_version(),
            reason: String::new(),
        },
        Err(e) => unavailable(e),
    }
}

/// 解析 `nvm list available` 的表格。
///
/// 它长这样（四列，分隔行由 `---` 组成）：
/// ```text
/// |   CURRENT    |     LTS      |  OLD STABLE  | OLD UNSTABLE |
/// |--------------|--------------|--------------|--------------|
/// |    26.9.0    |   24.21.0    |   0.12.18    |   0.11.16    |
/// ```
/// 按分隔符切、跳过表头与分隔行，比"按列宽定位"稳——列宽会随内容变。
fn parse_available(output: &str) -> AvailableNodeVersions {
    let mut out = AvailableNodeVersions {
        current: Vec::new(),
        lts: Vec::new(),
        old_stable: Vec::new(),
        old_unstable: Vec::new(),
    };
    for line in output.lines() {
        if !line.trim_start().starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line.trim().trim_matches('|').split('|').map(|c| c.trim()).collect();
        if cells.len() < 4 {
            continue;
        }
        // 表头（CURRENT/LTS/…）与分隔行（---）：认得出就跳
        if cells[0].eq_ignore_ascii_case("CURRENT") || cells[0].starts_with('-') {
            continue;
        }
        let looks_like_version = |s: &str| s.chars().next().is_some_and(|c| c.is_ascii_digit());
        if !looks_like_version(cells[0]) {
            continue;
        }
        out.current.push(cells[0].to_string());
        out.lts.push(cells[1].to_string());
        out.old_stable.push(cells[2].to_string());
        out.old_unstable.push(cells[3].to_string());
    }
    out
}

/// 把命令输出按文本解码。
///
/// 先按 UTF-8 试，失败回退 GB18030——与编辑器读文件同一套口径：中文 Windows 上
/// nvm 的提示语可能是 GBK 编码，直接 `from_utf8_lossy` 会得到一串问号。
fn decode_output(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::GB18030.decode(bytes).0.into_owned(),
    }
}

/// 跑一条 nvm 命令（绝对路径调用，收口它自己的输出）。
///
/// 用**绝对路径**而不是裸名 `nvm`：一是不依赖 PATH（Nexus 启动时的 PATH 未必和用户终端
/// 一样），二是绝对路径不受 Windows "先搜当前目录"的影响（若走裸名，得跟其他内部命令
/// 一样加 `NoDefaultCurrentDirectoryInExePath`，见 spawn_guard）。
fn run_nvm(nvm: &NvmInstall, args: &[&str]) -> Result<NvmCommandResult, String> {
    let mut cmd = std::process::Command::new(&nvm.exe);
    cmd.args(args)
        .current_dir(&nvm.root) // 在版本目录里跑：nvm 自己产生的中转文件落这儿
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let out = cmd.output().map_err(|e| format!("执行 nvm 失败: {}", e))?;
    let mut text = decode_output(&out.stdout);
    let err_text = decode_output(&out.stderr);
    if !err_text.trim().is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&err_text);
    }
    Ok(NvmCommandResult { ok: out.status.success(), output: text.trim().to_string() })
}

/// 可安装的版本列表（`nvm list available`，要联网，几秒）
#[command]
pub async fn list_available_node_versions() -> Result<AvailableNodeVersions, String> {
    let nvm = find_nvm()?;
    tokio::task::spawn_blocking(move || {
        let res = run_nvm(&nvm, &["list", "available"])?;
        if !res.ok {
            return Err(format!("nvm list available 失败：\n{}", res.output));
        }
        Ok(parse_available(&res.output))
    })
    .await
    .map_err(|e| format!("查询可安装版本失败: {}", e))?
}

/// 安装指定版本（`nvm install`）。要下几十兆，界面得给明确的等待提示。
#[command]
pub async fn install_node_version(version: String) -> Result<NvmCommandResult, String> {
    let v = version.trim().to_string();
    if v.is_empty() {
        return Err("版本号不能为空".into());
    }
    let nvm = find_nvm()?;
    tokio::task::spawn_blocking(move || run_nvm(&nvm, &["install", &v]))
        .await
        .map_err(|e| format!("安装任务失败: {}", e))?
}

/// 卸载前的守卫：当前正在使用的版本不许卸。
///
/// **这道判断必须我们自己写**（此前以为"软链指着它，nvm 会拒绝"，是错的）：
/// nvm-windows 1.2.2 的 `uninstall()` 里没有任何"在用就不许卸"的检查，实测其二进制里
/// 也不含这类文案；它反而会**主动**先 `rmdir` 掉 `NVM_SYMLINK` 软链、再 `RemoveAll`
/// 版本目录——所有终端里的 node/npm 当场消失，而退出码仍是 0，
/// 于是界面还会弹一个"已卸载"的成功提示。
///
/// 比较时统一去掉 `v` 前缀：版本目录名是 `v22.22.1`，nvm 自己认的写法是 `22.22.1`
/// （它靠跑 `node -v` 判断当前版本）。返回 `Some(原因)` = 拦住，`None` = 放行。
fn uninstall_guard(target: &str, current: &str) -> Option<String> {
    if current.trim().is_empty() {
        return None; // 软链没建（nvm 还没初始化过）：没有"当前版本"这回事，照常卸
    }
    let norm = |s: &str| s.trim().trim_start_matches(['v', 'V']).to_string();
    if norm(target) == norm(current) {
        return Some(format!(
            "{} 是当前正在使用的版本，先切换到别的版本再卸载——直接卸会先删掉 node 软链，\
             所有终端里的 node/npm 会一起不可用",
            current
        ));
    }
    None
}

/// 卸载指定版本（`nvm uninstall`）。
///
/// 当前版本由 `uninstall_guard` 拦下（**nvm 自己不拦**，理由见那个函数），
/// 其余失败照贴 nvm 的原话。
#[command]
pub async fn uninstall_node_version(version: String) -> Result<NvmCommandResult, String> {
    let v = version.trim().to_string();
    if v.is_empty() {
        return Err("版本号不能为空".into());
    }
    let nvm = find_nvm()?;
    // 守卫放在后端而不是只写在前端：前端置灰/提示只管得到这一个界面，
    // 换个调用方（以后的批量卸载、命令行）就绕过去了
    if let Some(reason) = uninstall_guard(&v, &current_version()) {
        return Err(reason);
    }
    tokio::task::spawn_blocking(move || run_nvm(&nvm, &["uninstall", &v]))
        .await
        .map_err(|e| format!("卸载任务失败: {}", e))?
}

/// 切换当前使用的版本（`nvm use`）。
///
/// ⚠️ 改的是**全局软链**：用户在**所有**终端里看到的 node 版本都会跟着变。
/// 界面必须在按钮上说清这一点。
///
/// **返回时保证软链已经落定**，理由见下面那段等待——这是本命令比"调一下 nvm"多做的事，
/// 也是唯一一件：`nvm use` 返回 ≠ 切换完成。
#[command]
pub async fn use_node_version(version: String) -> Result<NvmCommandResult, String> {
    let v = version.trim().to_string();
    if v.is_empty() {
        return Err("版本号不能为空".into());
    }
    let nvm = find_nvm()?;
    tokio::task::spawn_blocking(move || {
        let before = current_version();
        let res = run_nvm(&nvm, &["use", &v])?;
        if res.ok {
            // nvm use 是"先删软链、再建"的，**它返回时软链可能还不存在**——实测：返回后
            // 立刻读会读到空，约 1 秒后才指向新版本（Git Bash 与 Rust 侧都一样）。
            //
            // 不等的话，调用方紧接着的 get_node_runtime 会读到 current 为空 →
            // 界面上"当前"标记不动、所有行都显示「设为当前」（用户报过这个）。
            // 等待交给后端而不是前端延迟，是因为"切换什么时候算完成"是这条命令的语义，
            // 每个调用方各等一次迟早漏掉一个。
            //
            // 最多 2 秒；等不到也照样返回（让界面显示实情，而不是把请求挂住）。
            wait_for_switch(&before);
        }
        Ok(res)
    })
    .await
    .map_err(|e| format!("切换版本失败: {}", e))?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 解析真实输出（从本机 `nvm list available` 抄下来的一段）
    #[test]
    fn test_parse_available_table() {
        let sample = "\
|   CURRENT    |     LTS      |  OLD STABLE  | OLD UNSTABLE |
|--------------|--------------|--------------|--------------|
|    26.9.0    |   24.21.0    |   0.12.18    |   0.11.16    |
|    26.8.2    |   24.20.0    |   0.12.17    |   0.11.15    |
";
        let got = parse_available(sample);
        assert_eq!(got.current, vec!["26.9.0", "26.8.2"]);
        assert_eq!(got.lts, vec!["24.21.0", "24.20.0"]);
        assert_eq!(got.old_stable, vec!["0.12.18", "0.12.17"]);
        assert_eq!(got.old_unstable, vec!["0.11.16", "0.11.15"]);
    }

    /// 认不出的行要跳过而不是编出假数据：表头、分隔行、空表、列数不足
    #[test]
    fn test_parse_available_ignores_noise() {
        let sample = "\
| CURRENT | LTS | OLD STABLE | OLD UNSTABLE |
|---------|-----|------------|--------------|
|  not a version  |  x  |  y  |  z  |
| 20.11.1 | 18.20.4 | 0.12.18 | 0.11.16 |
";
        let got = parse_available(sample);
        assert_eq!(got.current, vec!["20.11.1"], "只有真正像版本号的那行该被收下");
        assert_eq!(got.lts, vec!["18.20.4"]);

        assert!(parse_available("").current.is_empty(), "空输出不该 panic");
        assert!(parse_available("nvm 无法联网").current.is_empty(), "非表格输出不该编出条目");
    }

    /// 当前版本不许卸——nvm 自己不拦（见 `uninstall_guard` 的说明），这道判断漏一次
    /// 就会静默毁掉用户全局的 node，所以把边界一个个钉住。
    #[test]
    fn test_uninstall_guard_blocks_current_only() {
        // 同一个版本的两种写法（目录名带 v、nvm 认的不带）都要拦住
        assert!(uninstall_guard("v22.22.1", "v22.22.1").is_some());
        assert!(uninstall_guard("22.22.1", "v22.22.1").is_some());
        assert!(uninstall_guard("V22.22.1", "v22.22.1").is_some());

        // 别的版本照常放行；软链没建（当前为空）也放行
        assert!(uninstall_guard("v16.20.2", "v22.22.1").is_none());
        assert!(uninstall_guard("v22.22.1", "").is_none());

        // 前缀相同但不是同一个版本：不能用 starts_with 这类近似比较
        assert!(uninstall_guard("v22.22.10", "v22.22.1").is_none());
        assert!(uninstall_guard("v22.22.1", "v22.22.10").is_none());

        // 拦截时的话要能指出下一步（"先切换"），不是只说一句"不行"
        let msg = uninstall_guard("v22.22.1", "v22.22.1").expect("同版本应当拦住");
        assert!(msg.contains("先切换"), "提示要给出下一步，实际: {}", msg);
    }

    /// 对着**本机真实的 nvm 安装**跑一遍探测链路。
    ///
    /// 标 `#[ignore]`：CI 上没有 nvm-windows，跑不了。手动跑：
    /// ```text
    /// cargo test --lib --manifest-path src-tauri/Cargo.toml -- --ignored find_nvm
    /// ```
    /// 留着它的价值：`find_nvm` 读的是环境变量 + settings.txt + 软链三处外部状态，
    /// 纯单测构造不出来——真机上跑一次才知道这条链路通不通。
    #[test]
    #[ignore]
    fn test_find_nvm_on_this_machine() {
        let nvm = find_nvm().expect("本机应当装有 nvm-windows（NVM_HOME + settings.txt）");
        assert!(nvm.exe.is_file(), "nvm.exe 应当存在: {}", nvm.exe.display());
        assert!(nvm.root.is_dir(), "root 目录应当存在: {}", nvm.root.display());

        let versions = installed_versions(&nvm.root);
        println!(
            "nvm.exe = {}\nroot    = {}\n已安装  = {:?}\n当前    = {}",
            nvm.exe.display(), nvm.root.display(), versions, current_version(),
        );
        assert!(!versions.is_empty(), "本机应当装过至少一个 Node 版本");
        assert!(
            versions.iter().all(|v| v.starts_with('v')),
            "版本目录名应当形如 v22.22.1，实际: {:?}", versions,
        );
        // 当前版本要么为空（没设置），要么确实在已装列表里——不在的话说明软链指到了别处
        let cur = current_version();
        assert!(
            cur.is_empty() || versions.contains(&cur),
            "当前版本 {} 不在已装列表 {:?} 里", cur, versions,
        );
    }

    /// 「等软链落定」这段逻辑本身：切换后**返回前**必须已经指向新版本。
    ///
    /// 标 `#[ignore]`：它**真的会改软链**（切过去、验证、再切回来），且 CI 上没有 nvm。
    /// 手动跑：
    /// ```text
    /// cargo test --lib --manifest-path src-tauri/Cargo.toml -- --ignored use_settles
    /// ```
    /// 这条直接对应一个用户反馈（"点完设为当前，面板里的标记不变"）——根因就是
    /// `nvm use` 返回时软链还不存在，界面紧接着读到空。
    #[test]
    #[ignore]
    fn test_use_settles_before_returning() {
        let nvm = find_nvm().expect("本机应当装有 nvm-windows");
        let versions = installed_versions(&nvm.root);
        assert!(versions.len() >= 2, "至少要装两个版本才切换得动，当前 {:?}", versions);

        let original = current_version();
        assert!(!original.is_empty(), "当前应当有一个生效版本");
        let target = versions
            .iter()
            .find(|v| **v != original)
            .expect("找一个与当前不同的版本")
            .clone();

        // 与命令里完全相同的两步：跑 nvm use，然后等落定
        let before = current_version();
        let res = run_nvm(&nvm, &["use", &target]).expect("执行 nvm use");
        assert!(res.ok, "nvm use 应当成功: {}", res.output);
        wait_for_switch(&before);

        // 关键断言：等待之后（也就是命令返回时）软链必须已经指向新版本
        assert_eq!(
            current_version(), target,
            "返回前软链就应当指向 {}——不然界面紧接着读会读到空", target,
        );

        // 恢复原状（无论上面断言成败，都别把用户的版本留在改动后的状态）
        let _ = run_nvm(&nvm, &["use", &original]);
        wait_for_switch(&target);
        assert_eq!(current_version(), original, "应当已切回 {}", original);
    }

    /// 已装版本按版本号排序（新在前），而不是字典序——字典序会把 v9 排在 v22 后面
    #[test]
    fn test_installed_versions_sorted_by_version() {
        let dir = std::env::temp_dir().join(format!("nexus_ut_nvm_{}", std::process::id()));
        for v in ["v22.22.1", "v9.11.2", "v16.20.2"] {
            std::fs::create_dir_all(dir.join(v)).expect("建版本目录");
        }
        std::fs::create_dir_all(dir.join("not-a-version")).expect("建干扰目录");
        std::fs::write(dir.join("v1-file"), b"x").expect("建干扰文件");

        let got = installed_versions(&dir);
        assert_eq!(got, vec!["v22.22.1", "v16.20.2", "v9.11.2"], "版本号数值序，且只收目录");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
