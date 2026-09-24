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

use crate::core::winenv::env_or_persistent;

/// 找到的 nvm-windows 安装
struct NvmInstall {
    /// `nvm.exe` 的绝对路径
    exe: PathBuf,
    /// nvm 的安装目录（`NVM_HOME`）——nvm.exe 靠这个环境变量找自己的 `settings.txt`
    home: PathBuf,
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

/// 索引里的一个可安装版本
#[derive(Serialize)]
pub struct NodeVersionInfo {
    /// 形如 `v22.23.2`——原样带 `v`，与已安装目录名、`nvm install` 的入参是同一种写法
    pub version: String,
    /// LTS 代号（如 `Jod`）；不是 LTS 时为空串
    pub lts: String,
}

/// 可安装的版本索引（整份 `index.json`，界面在本地筛）
///
/// 为什么一次把 866 个版本全给前端：索引总共 331 KB，而"搜 22 这条线"要的正是**整条线**
/// （22.x 有 35 个）——`nvm list available` 那种每列十来条的摘要在这种用法下根本不够。
#[derive(Serialize)]
pub struct NodeVersionIndex {
    /// 新 → 旧
    pub versions: Vec<NodeVersionInfo>,
    /// 实际取索引的地址：界面上要能说清"从哪取的"（取不到时那句话里也有它）
    pub index_url: String,
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
/// - `nvm.exe` 在哪 → `NVM_HOME`（进程环境变量 → 注册表回退，见 `core::winenv`）
/// - **版本存哪** → 读它自己的 `settings.txt`（`nvm root` 可以改过，环境变量回答不了）
/// - 当前用哪个 → `NVM_SYMLINK` 软链指向谁（同样有注册表回退）
fn find_nvm() -> Result<NvmInstall, String> {
    let Some(home) = env_or_persistent("NVM_HOME") else {
        return Err("没找到 NVM_HOME（进程环境变量与注册表里都没有）——本机可能没装 nvm-windows".into());
    };
    let home = PathBuf::from(home.trim());
    let exe = home.join("nvm.exe");
    if !exe.is_file() {
        return Err(format!("{} 不存在，nvm-windows 的安装可能不完整", exe.display()));
    }

    let root = settings_value(&home, "root")?
        .ok_or_else(|| format!("{} 里没有 root 配置", home.join("settings.txt").display()))?;

    Ok(NvmInstall { exe, home, root: PathBuf::from(root) })
}

/// 读 nvm `settings.txt` 里的一项（形如 `root: C:\Users\me\AppData\Roaming\nvm`）。
///
/// `Ok(None)` = 文件读到了、但没有这一项；`Err` = 文件读不到。
/// 值一律先洗一遍：这个文件是外部可编辑的，而 NUL 会让 `Command::spawn` 报一句没头没尾的
/// "nul byte found in provided data"（见 `winenv::sanitize_for_spawn`）。
fn settings_value(home: &Path, key: &str) -> Result<Option<String>, String> {
    let path = home.join("settings.txt");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("读不到 {}: {}", path.display(), e))?;
    let prefix = format!("{}:", key);
    Ok(text
        .lines()
        .find_map(|l| l.trim().strip_prefix(prefix.as_str()))
        .map(|v| crate::core::winenv::sanitize_for_spawn(v, &format!("settings.txt 的 {}", key)))
        .filter(|v| !v.is_empty()))
}

/// nvm 没配镜像时它自己用的地址（`web.go` 里 `nodeBaseAddress` 的初值）。
///
/// 结尾的斜杠是有意的：nvm 拼地址是**字符串相加**（`GetFullNodeUrl` = 基址 + `index.json`）。
const DEFAULT_NODE_MIRROR: &str = "https://nodejs.org/dist/";

/// 版本索引的地址：`<node_mirror>index.json`。
///
/// **为什么读 settings.txt 而不是写死 nodejs.org**：`nvm install` 认不认某个版本，是拿它
/// 自己的 `node_mirror + "index.json"` 对出来的（1.2.2 的 `node.GetAvailable()` 就是这么
/// 取列表的）。我们读同一份配置、拼同一个地址，才不会出现"列表里有、一点却装不上"。
///
/// 拼法**逐字对齐 nvm 的 `web.SetMirrors`**（那里是唯一权威的拼法）：
/// - 值为空或 `none` → 回到官方默认
/// - 开头不是 `http` → 前面补 `http://`
/// - 结尾没 `/` → 补一个
///
/// 少补任何一处，我们拉到的地址就与 nvm 实际去的不是同一个（对它真失败，对我们只是查不到）。
fn node_index_url(home: &Path) -> Result<String, String> {
    // **不吞读失败**（CQ-45）：原实现 `.ok().flatten()` 把"读不到 settings.txt"也当成
    // "没配镜像"，静默退回官方源——正是上面那段注释极力避免的"两处地址不一样"：
    // 我们列的是 nodejs.org 的版本，`nvm install` 却去镜像取，症状是"搜得到、装不上"。
    // 同文件的 `root` 用的是 `?`，这里与它对齐。
    let configured = settings_value(home, "node_mirror")?;
    let mut base = match configured.as_deref() {
        None | Some("none") => DEFAULT_NODE_MIRROR.to_string(),
        Some(v) => v.to_string(),
    };
    // 不能用 Go 那种 `base[0..4]` 切片：用户手写个短值会 panic，写中文则切在字符中间也 panic
    if !base.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("http")) {
        base = format!("http://{}", base);
    }
    if !base.ends_with('/') {
        base.push('/');
    }
    Ok(format!("{}index.json", base))
}

/// 当前生效的版本 = `NVM_SYMLINK` 软链指向哪个目录。
///
/// 读不到就返回空串（软链没建、nvm 没初始化过都算"未设置"），不报错——
/// 那是正常状态，不是故障。
fn current_version() -> String {
    let Some(link) = env_or_persistent("NVM_SYMLINK") else { return String::new() };
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

/// 解析 `index.json`。
///
/// 它是个数组，每一项长这样（字段只用到两个，其余忽略）：
/// ```json
/// {"version":"v22.23.2","date":"2026-08-19","npm":"10.9.4",…,"lts":"Jod"}
/// ```
/// `lts` 有两种类型：不是 LTS 时是布尔 `false`、是时候是代号字符串（如 `"Jod"`）——
/// 按 `Value` 收下再取字符串，比给 `Option<String>` 写自定义反序列化省事。
fn parse_index(bytes: &[u8]) -> Result<Vec<NodeVersionInfo>, String> {
    #[derive(serde::Deserialize)]
    struct Entry {
        #[serde(default)]
        version: String,
        #[serde(default)]
        lts: serde_json::Value,
    }

    let entries: Vec<Entry> = serde_json::from_slice(bytes).map_err(|e| {
        // 镜像出问题时常见的是"HTTP 200 + 一页 HTML"：只说"解析失败"没法排障，把开头带上
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(120)]).replace(['\r', '\n'], " ");
        format!("版本索引不是预期的 JSON（{}）：{}", e, head)
    })?;

    let mut out: Vec<NodeVersionInfo> = entries
        .into_iter()
        .filter(|e| e.version.starts_with('v'))
        .map(|e| NodeVersionInfo {
            lts: e.lts.as_str().unwrap_or_default().to_string(),
            version: e.version,
        })
        .collect();
    // 索引本身是新版在前；这里自己再排一次，是不把顺序押在镜像上
    // （复用「已安装」那份版本比较：同为语义化版本，新版本在前所以参数反过来传）
    out.sort_by(|a, b| crate::core::ai::version_cmp(&b.version, &a.version).unwrap_or(std::cmp::Ordering::Equal));
    Ok(out)
}

// ─── 安装 nvm-windows ───────────────────────────────────────

/// 官方安装器地址，钉在 **1.2.2**（v1 的最后一代）。
///
/// 为什么不装当前的 v2.0.0：本面板是按 v1 的接口写的——版本目录来自 `settings.txt`、
/// 当前版本看 `NVM_SYMLINK` 软链、可安装列表解析 `nvm list available` 的表格；而 v2 把
/// 配置搬进了注册表（`InstallRoot` / `ActiveVersion`），CLI 也重写过（仓库都换了）。
/// 没适配就装 v2，用户拿到的是"装上了但面板读不到"，比不装更让人困惑。
/// 哪天适配完 v2，改这一个常量即可。
const NVM_SETUP_URL: &str =
    "https://github.com/nvm-windows/nvm/releases/download/1.2.2/nvm-setup.exe";

/// 安装器大小下限。本机实测 5,612,296 字节；下到一半断线会留下一个短文件，
/// 拿它去执行，用户看到的是"这个应用无法在你的电脑上运行"这类没头没尾的报错。
const NVM_SETUP_MIN_BYTES: u64 = 1_000_000;

/// 安装结果（界面据此说清"是装好了还是被取消了"）
#[derive(Serialize)]
pub struct NvmInstallResult {
    /// 安装器退出码为 0（走完了向导）；false 多半是用户点了取消
    pub ok: bool,
    pub code: Option<i32>,
    /// 安装器在本机的位置（出问题时界面能告诉用户它在哪）
    pub path: String,
}

/// Windows 自带的 curl（Win10 1803 起内置）。
///
/// 用绝对路径而不是裸名，与 `run_nvm` 同一个理由：不依赖 PATH，也不受"先搜当前目录"影响。
fn system_curl() -> PathBuf {
    let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    Path::new(&sysroot).join("System32").join("curl.exe")
}

/// 文件像不像个能跑的安装器：够大 + `MZ` 头。
/// curl 的 `--fail` 挡了 4xx/5xx，但"下到一半断线"仍会留下一个短文件——不验就会把它执行起来。
/// 官方 nvm-windows 1.2.2 安装器的 SHA-256（SEC-20）。
///
/// 为什么必须钉死它：落地路径是 `%TEMP%\nexus\nvm-setup-1.2.2.exe`，而**同用户的任意进程**
/// （例如克隆来的仓库里的构建脚本——本项目明确假设项目目录不可信）都能往那里预置一个
/// ≥1MB、以 `MZ` 开头的文件。原实现的"完整性判据"只有"≥1MB + MZ 头"，且**已有合格文件就不
/// 重新下载**——用户点「运行安装向导」时，Nexus 会跳过下载、直接以**管理员权限**执行它
/// （nvm 1.2.2 是 `PrivilegesRequired=admin`），而 UAC 框上显示的正是 `nvm-setup-1.2.2.exe`，
/// 用户没有可分辨的线索。
///
/// 这个值的出处与交叉验证（2026-09-24，本机实测）：官方 release 直链下载的 exe 与
/// **官方公布了 MD5 的** `nvm-setup.zip`（`8a663b9af5836ea1abb2e93b7fbfbaad`，本机复算一致）
/// 解出的 `nvm-setup.exe` 字节完全相同——即这个哈希有发布方自己的校验和背书，不是"下到什么算什么"。
const NVM_SETUP_SHA256: &str = "2d5ad523aa6182205da77c0eb8210638aaa8792f4e6a4bc12e1ac854c5455a68";

/// 文件内容的 SHA-256（小写十六进制）。读不动文件时返回 Err（调用方一律按"不合格"处理）。
fn file_sha256(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(path).map_err(|e| format!("打开 {} 失败: {}", path.display(), e))?;
    let mut hasher = Sha256::new();
    // 流式读：安装器 5.6MB，一次性读进内存没必要（这里也只是省一次分配）
    std::io::copy(&mut f, &mut hasher).map_err(|e| format!("读取 {} 失败: {}", path.display(), e))?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// 缓存里的这份文件是不是**可信的**官方安装器。
///
/// 判据是 SHA-256，而不是"够大 + 以 MZ 开头"（SEC-20）——后两者只是**便宜的预筛**：
/// 先按大小与魔数挡掉明显不是 PE 的文件，免得把几百 MB 的垃圾整个读进来算哈希。
fn valid_installer(path: &Path) -> bool {
    valid_installer_with(path, NVM_SETUP_SHA256)
}

/// 判据本体（期望哈希作参数：让测试能构造"哈希对得上"的正例，而不必真的放一份安装包进仓库）
fn valid_installer_with(path: &Path, expected_sha256: &str) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if !meta.is_file() || meta.len() < NVM_SETUP_MIN_BYTES {
        return false;
    }
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut magic = [0u8; 2];
    if !(std::io::Read::read_exact(&mut f, &mut magic).is_ok() && &magic == b"MZ") {
        return false;
    }
    match file_sha256(path) {
        Ok(got) => got.eq_ignore_ascii_case(expected_sha256),
        // 算不出哈希（读权限/IO 错误）→ 不合格，宁可让用户重下
        Err(e) => {
            log::warn!("[nexus] 安装器哈希计算失败（按不合格处理）: {}", e);
            false
        }
    }
}

/// 用系统自带的 curl 把一个 URL 下到本地文件（`max_time` 是整体超时，单位秒）。
///
/// 两处下载共用：nvm 安装器（GitHub release 直链）与版本索引（镜像上的 `index.json`）。
/// 用 curl 而不是引 HTTP 客户端：Rust 侧至今没有 HTTP 依赖，而且**前端也够不着这两个地址**
/// （CSP 只放开了 api.github.com）——引一个客户端只为下两个文件不划算。
///
/// 失败时把 URL 一起写进错误里：这两条地址一个来自常量、一个来自用户的 `settings.txt`，
/// 出问题时第一件要知道的事就是"到底去哪个地址取的"。
fn run_curl(url: &str, dest: &Path, max_time: u32) -> Result<(), String> {
    run_curl_raw(url, Some(dest), max_time).map(|_| ())
}

/// 同上，但结果**直接从 stdout 取**，不落任何文件（CQ-43）。
///
/// 为什么要有这一支：版本索引原先下到固定的 `%TEMP%\nexus\node-index.json` 再读回来，
/// 而两条并发调用（关掉面板再打开就会触发）会同时写它——curl 截断重写时另一路读到半截，
/// 报出来是"版本索引不是预期的 JSON"这种假错误。索引本来就只是一次性解析，不必落盘。
fn run_curl_stdout(url: &str, max_time: u32) -> Result<Vec<u8>, String> {
    run_curl_raw(url, None, max_time).map(|out| out.stdout)
}

/// curl 调用的公共部分：拼参数、起进程、把非零退出码翻成带地址的错误。
/// `dest` 为 `None` = 不指定 `-o`，curl 把正文写到 stdout。
fn run_curl_raw(
    url: &str,
    dest: Option<&Path>,
    max_time: u32,
) -> Result<std::process::Output, String> {
    let curl = system_curl();
    if !curl.is_file() {
        return Err(format!(
            "找不到 {}（Windows 自带的下载组件）——请手动下载：{}",
            curl.display(), url
        ));
    }
    if let Some(dir) = dest.and_then(Path::parent) {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录 {} 失败: {}", dir.display(), e))?;
    }

    let mut cmd = std::process::Command::new(&curl);
    // 显式设置收口（spawn_guard 的 GUARD-1）：这里是绝对路径调用，本来就不搜当前目录，
    // 写上是为了让"新增的进程调用"在守卫里是一次明确决定，而不是漏写
    cmd.env("NoDefaultCurrentDirectoryInExePath", "1")
        .args([
            "-L",                 // release 直链会 302 到 objects.githubusercontent.com，镜像也会 302
            "--fail",             // 4xx/5xx 直接算失败，别把错误页当成正经文件存下来
            "--retry", "3",       // 国内直连偶尔断流，重试几次比让用户重来便宜
            "--retry-delay", "2",
            "--connect-timeout", "20",
            "--max-time",
        ])
        .arg(max_time.to_string())
        .args(["--silent", "--show-error"]);
    if let Some(d) = dest {
        cmd.arg("-o").arg(d);
    }
    cmd.arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let out = cmd.output().map_err(|e| format!("启动下载失败: {}", e))?;
    if !out.status.success() {
        let detail = decode_output(&out.stderr);
        return Err(format!(
            "下载失败（curl 退出码 {:?}）：{}\n地址：{}",
            out.status.code(),
            detail.trim(),
            url
        ));
    }
    Ok(out)
}

/// 下载官方安装器（本地已有一份合格的就不再下——安装器可重复运行，没必要重下 5.6 MB）
fn download_setup(dest: &Path) -> Result<(), String> {
    // "已有一份合格的就不再下"——这里的"合格"现在**包含哈希核对**（SEC-20）：
    // 同用户进程预置的假安装器在这一步就被挡掉，不会因为"够大 + MZ 头"被放行
    if valid_installer(dest) {
        return Ok(());
    }
    // 5.6 MB：慢网也够，到点就报错而不是无限等
    run_curl(NVM_SETUP_URL, dest, 900)?;
    if !valid_installer(dest) {
        let size = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
        let got = file_sha256(dest).unwrap_or_else(|e| format!("(算不出: {})", e));
        // **删掉**：不合格的文件留着的话，下次进这个函数时第一步的"已有合格文件"判断
        // 会再把它捡回来（它确实"存在"），于是永远走不到重新下载这一步
        let _ = std::fs::remove_file(dest);
        return Err(format!(
            "下载到的文件校验不通过（{} 字节）——已删除该文件。\n期望 SHA-256：{}\n实际 SHA-256：{}\n可手动下载：{}",
            size, NVM_SETUP_SHA256, got, NVM_SETUP_URL
        ));
    }
    Ok(())
}

/// 拉版本索引并解析（联网，约 331 KB）。
///
/// **不落盘**（CQ-43）：原实现把索引下到固定路径 `%TEMP%\nexus\node-index.json` 再读回来，
/// 而两条并发调用（关掉面板再打开即可触发，`) effect 是"每次 open 变真就拉一次"、
/// 在飞请求不取消）会同时写同一个文件——curl 截断重写时另一路读到半截 JSON，
/// 报出来是"版本索引不是预期的 JSON"这种**假错误**，还留在面板上让人以为是镜像坏了。
/// curl 直接写 stdout（本来就是 `Stdio::piped()`，只是原先被丢掉了），共享临时文件与
/// 截断读一起消失。
fn fetch_index(url: &str) -> Result<Vec<NodeVersionInfo>, String> {
    let bytes = run_curl_stdout(url, 120)?;
    let versions = parse_index(&bytes)?;
    // 空索引当成失败而不是"没有可安装的版本"：镜像同步中/被拦掉时返回 `[]` 是常见形态，
    // 报出来用户知道该重试（nvm 自己也这么处理——它的 `GetAvailable` 见空直接报错退出）
    if versions.is_empty() {
        return Err(format!(
            "{} 里没有任何版本（镜像可能在同步中，稍后重试）",
            url
        ));
    }
    Ok(versions)
}

/// 跑安装向导并等它结束。
///
/// 安装器要求管理员（1.2.2 的 `nvm.iss` 里是 `PrivilegesRequired=admin`），所以这一步会弹
/// UAC——那一下只能由用户点，我们负责的只是把向导拉起来、等它关掉。
async fn run_installer(path: &Path) -> Result<NvmInstallResult, String> {
    let mut child = tokio::process::Command::new(path)
        .env("NoDefaultCurrentDirectoryInExePath", "1") // 同上：新增进程调用要么收口要么豁免
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| format!("启动安装向导失败: {}", e))?;
    let status = child
        .wait()
        .await
        .map_err(|e| format!("等待安装向导结束失败: {}", e))?;
    Ok(NvmInstallResult {
        ok: status.success(),
        code: status.code(),
        path: path.display().to_string(),
    })
}

/// 安装器的落地路径（`%TEMP%\nexus\nvm-setup-1.2.2.exe`）
fn setup_path() -> PathBuf {
    std::env::temp_dir().join("nexus").join("nvm-setup-1.2.2.exe")
}

/// 下载官方安装器到本机（已有一份合格的直接复用），返回它的路径。
///
/// 与下一步（`run_nvm_installer`）**分成两条命令**，是因为界面要分开说这两件事：
/// "正在下载（可能要一两分钟）" 与 "安装向导已打开、在等你操作"——合成一条命令，界面就
/// 只能在向导开着的时候继续显示"正在下载"，用户会以为卡住了。
///
/// 下载交给**系统自带的 curl.exe**：与 `run_nvm` 同一个路子（绝对路径调用），不为此引
/// HTTP 客户端；也绕开了 CSP——前端只被允许访问 api.github.com，而安装包在 github.com
/// 的 release 直链上。
#[command]
pub async fn download_nvm_installer() -> Result<String, String> {
    if !cfg!(windows) {
        // 用 cfg! 而不是 #[cfg]：这个模块在非 Windows 上也要编译得过（其余命令同理）
        return Err("nvm-windows 是 Windows 专用的，当前平台装不了".into());
    }
    let dest = setup_path();
    // curl 是阻塞的，别占着异步运行时
    let target = dest.clone();
    tokio::task::spawn_blocking(move || download_setup(&target))
        .await
        .map_err(|e| format!("下载任务失败: {}", e))??;
    Ok(dest.display().to_string())
}

/// 运行已下载的安装器并等它结束。
///
/// **跑向导而不是静默安装**：1.2.2 的向导会问 nvm 目录与软链位置，这两处用户可能有自己的
/// 安排；静默装完再发现路径不合心意，比多点两下更糟。向导要求管理员权限（`nvm.iss` 里是
/// `PrivilegesRequired=admin`），那一下只能由用户点。
///
/// 装完**不必重启应用**：环境变量读不到时会回退到注册表（见 `persistent_env`）。
#[command]
pub async fn run_nvm_installer() -> Result<NvmInstallResult, String> {
    if !cfg!(windows) {
        return Err("nvm-windows 是 Windows 专用的，当前平台装不了".into());
    }
    let path = setup_path();
    if !valid_installer(&path) {
        return Err(format!("{} 还没下载好（先下载再运行）", path.display()));
    }
    // **执行前再核对一次**（SEC-20）：下载与点「运行安装向导」之间可以隔着很久，而这一步
    // 会以管理员权限执行它。校验与执行之间没有 TOCTOU 可钻的缝——校验的就是即将执行的那份文件
    verify_before_run(&path)?;
    run_installer(&path).await
}

/// 执行安装器前的最后一道核对（单独成函数是为了能直接测"哈希不对就别执行"）。
fn verify_before_run(path: &Path) -> Result<(), String> {
    if valid_installer(path) {
        return Ok(());
    }
    // 走到这里说明文件在下载之后被换掉了（或被手动改过）——这与"还没下载好"是两件事，
    // 文案要说清，否则用户只会一遍遍点下载而不知道有人动过那个文件
    let got = file_sha256(path).unwrap_or_else(|e| format!("(算不出: {})", e));
    let _ = std::fs::remove_file(path);
    Err(format!(
        "安装器文件与官方版本不符，已删除：{}\n期望 SHA-256：{}\n实际 SHA-256：{}\n请重新点「下载」再运行",
        path.display(), NVM_SETUP_SHA256, got
    ))
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
    // 兜底自检：真出问题时报错必须**指名道姓**。
    // 用户报过一条 "执行 nvm 失败: nul byte found in provided data"——那是 Rust 的
    // `Command::spawn` 在说"C 字符串里有 NUL"，但没说哪个值、哪个字段，用户与排障的人都
    // 只能猜。正常路径上 NUL 已经在源头被洗掉了（见 `winenv::sanitize_for_spawn`），
    // 这段是"万一还有"，让下一次出现时一眼能定位。
    let mut parts: Vec<(&str, String)> = vec![
        ("nvm.exe 路径", nvm.exe.to_string_lossy().to_string()),
        ("nvm 版本目录", nvm.root.to_string_lossy().to_string()),
    ];
    parts.extend(args.iter().map(|a| ("nvm 参数", a.to_string())));
    for (what, value) in &parts {
        if value.contains('\0') {
            return Err(format!(
                "{} 里含非法字符（NUL），nvm 无法启动：{:?}——这个值来自外部（注册表或 settings.txt），请检查后重试",
                what, value
            ));
        }
    }

    let mut cmd = std::process::Command::new(&nvm.exe);
    // 子进程要拿到**当前**的 PATH 与 nvm 变量，不能继承 Nexus 启动那一刻的副本（见 core::winenv）：
    // - nvm.exe 按 `os.Getenv("NVM_HOME")` 找自己的 `settings.txt`（1.2.2 的 `src/nvm.go:59`），
    //   缺了它就去盘根找 `\settings.txt`——实测报 "ERROR open \settings.txt"
    // - `nvm use` / `nvm install` 还依赖 PATH 上的 `node`（nvm 内部会跑 `node -v`）
    crate::core::winenv::apply(&mut cmd);
    cmd.args(args)
        .current_dir(&nvm.root) // 在版本目录里跑：nvm 自己产生的中转文件落这儿
        // apply 会按注册表给一份 NVM_HOME，这里再用"我们实际用的那个 nvm.exe 所在目录"覆盖一次：
        // 两者应当一致，但真不一致时，必须跟着这个 exe 走
        .env("NVM_HOME", &nvm.home)
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

/// 可安装的版本索引（`<node_mirror>index.json`，要联网，几秒）。
///
/// 与旧实现（`nvm list available` 的四列表格）的区别：那个每列只给十来个版本，"想装
/// 22 这条线的某个版本"根本查不到；这里给的是**整份索引**，界面按前缀筛，搜 `22` 能列出
/// 全部 35 个 22.x。
#[command]
pub async fn list_node_versions() -> Result<NodeVersionIndex, String> {
    let nvm = find_nvm()?;
    let url = node_index_url(&nvm.home)?;
    tokio::task::spawn_blocking(move || {
        let versions = fetch_index(&url)?;
        Ok(NodeVersionIndex { versions, index_url: url })
    })
    .await
    .map_err(|e| format!("查询可安装版本失败: {}", e))?
}

/// 给 nvm 配镜像（`nvm node_mirror` / `nvm npm_mirror` 会写进它自己的 `settings.txt`）。
///
/// 为什么需要这条：`nvm list available` 与 `nvm install` 都要去 `node_mirror`（默认
/// `https://nodejs.org/dist/`）取版本索引与安装包，国内机器上这一步常常直接失败——
/// 面板里"拿不到可安装列表"十有八九就是它。地址由前端传（面板给的是 npmmirror），
/// 这里只负责写入并把 nvm 的原话带回去。
///
/// 写的是**用户自己的 nvm 配置**，所以只能由用户点按钮触发，不做自动兜底。
#[command]
pub async fn set_nvm_mirrors(node_mirror: String, npm_mirror: String) -> Result<NvmCommandResult, String> {
    let node = node_mirror.trim().to_string();
    let npm = npm_mirror.trim().to_string();
    if node.is_empty() {
        return Err("node 镜像地址不能为空".into());
    }
    let nvm = find_nvm()?;
    tokio::task::spawn_blocking(move || {
        let res = run_nvm(&nvm, &["node_mirror", &node])?;
        if !res.ok {
            return Ok(res); // 第一步就没成：把 nvm 的原文带回去，别接着写 npm 镜像
        }
        if npm.is_empty() {
            return Ok(res);
        }
        run_nvm(&nvm, &["npm_mirror", &npm])
    })
    .await
    .map_err(|e| format!("配置镜像失败: {}", e))?
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

    /// SHA-256 的已知答案向量（NIST）：`abc` → ba7816bf…
    /// 手算哈希写错是**静默**的（每个文件都算出同一个错值，正例反例一起过），所以先钉住实现本身。
    #[test]
    fn test_file_sha256_known_answer() {
        let p = std::env::temp_dir().join(format!("nexus_ut_sha_{}.bin", std::process::id()));
        std::fs::write(&p, b"abc").expect("写临时文件");
        assert_eq!(
            file_sha256(&p).expect("算哈希"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        );
        let _ = std::fs::remove_file(&p);
    }

    /// **SEC-20 的正面用例**：一个"够大 + 以 MZ 开头"但内容不对的文件，不能算合格安装器。
    ///
    /// 这就是审计描述的攻击现场——同用户进程往 `%TEMP%\nexus\` 预置一个假安装器，
    /// 而"已有合格文件就不再下载"会把它直接送到管理员权限的执行点上。
    #[test]
    fn test_installer_check_rejects_lookalike_with_wrong_hash() {
        let p = std::env::temp_dir().join(format!("nexus_ut_fake_{}.exe", std::process::id()));
        // 2MB、以 MZ 开头：旧判据（≥1MB + MZ）会放行它
        let mut bytes = vec![0u8; 2 * 1024 * 1024];
        bytes[0] = b'M';
        bytes[1] = b'Z';
        std::fs::write(&p, &bytes).expect("写假安装器");

        assert!(!valid_installer(&p), "内容不对就必须拒绝，哪怕它够大、带 MZ 头");
        // 判据本体（哈希作参数）在哈希对得上时应当放行——否则上面那条可能只是"永远返回 false"
        let real_hash = file_sha256(&p).expect("算哈希");
        assert!(valid_installer_with(&p, &real_hash), "哈希一致时应当放行（判据不是恒假）");
        assert!(!valid_installer_with(&p, NVM_SETUP_SHA256), "与官方哈希不符 → 拒绝");

        // 执行前的那道闸：文件不符时**删除并报出两个哈希**，而不是照常执行
        let err = verify_before_run(&p).expect_err("不符的文件必须被拦下");
        assert!(err.contains("与官方版本不符"), "错误要说清是文件被换过: {}", err);
        assert!(!p.exists(), "拦下之后要删掉它，否则下一次判断又会捡回同一个文件");

        let _ = std::fs::remove_file(&p);
    }

    /// 解析真实索引（从本机拉下来的 `index.json` 抄下来的几项，字段原样保留）
    #[test]
    fn test_parse_index_versions_and_lts() {
        let sample = r#"[
{"version":"v22.23.2","date":"2026-08-19","npm":"10.9.4","lts":"Jod"},
{"version":"v26.9.0","date":"2026-09-16","npm":"11.19.1","lts":false},
{"version":"v20.11.1","date":"2024-02-14","npm":"10.2.4"}
]"#;
        let got = parse_index(sample.as_bytes()).expect("应当解析成功");
        assert_eq!(
            got.iter().map(|v| v.version.as_str()).collect::<Vec<_>>(),
            vec!["v26.9.0", "v22.23.2", "v20.11.1"],
            "新版在前（镜像万一乱序也由我们排好）",
        );
        // lts 是代号字符串 / 布尔 false / 整个字段缺失（老版本没有这个字段）——三种都要收下
        assert_eq!(got[1].lts, "Jod");
        assert_eq!(got[0].lts, "");
        assert_eq!(got[2].lts, "");
    }

    /// 不是索引就该报错，而不是编出一个空列表——空列表会被界面显示成"没有可安装的版本"
    #[test]
    fn test_parse_index_rejects_garbage() {
        // 镜像挂掉时最可能的形态：HTTP 200 + 一页 HTML
        let html = b"<html><head><title>502 Bad Gateway</title></head></html>";
        match parse_index(html) {
            // 解析失败的原因要带上原文开头，否则用户只看到"解析失败"，没法判断镜像返回了什么
            Err(err) => assert!(err.contains("502 Bad Gateway"), "错误里要带上原文开头，实际: {}", err),
            Ok(v) => panic!("HTML 不该被解析成版本列表，实际拿到 {} 条", v.len()),
        }

        assert!(parse_index(b"").is_err(), "空响应应当报错");
        assert!(parse_index(br#"{"versions":[]}"#).is_err(), "不是数组应当报错");
    }

    /// 索引地址的拼法必须与 nvm 的 `web.SetMirrors` 逐字一致——**不然我们列出来的版本
    /// 与 `nvm install` 认的不是同一份**（用户会看到"搜得到、装不上"）。
    #[test]
    fn test_node_index_url_matches_nvm_normalization() {
        let dir = std::env::temp_dir().join(format!("nexus_ut_nvm_url_{}", std::process::id()));
        let write = |content: &str| {
            std::fs::create_dir_all(&dir).expect("建目录");
            std::fs::write(dir.join("settings.txt"), content).expect("写 settings.txt");
        };

        // settings.txt 里没有 node_mirror：用官方默认（nvm 的 nodeBaseAddress 初值）
        write("root: C:\\nvm\r\n");
        assert_eq!(node_index_url(&dir).unwrap(), "https://nodejs.org/dist/index.json");

        // 配了镜像、结尾有斜杠：原样拼
        write("root: C:\\nvm\r\nnode_mirror: https://npmmirror.com/mirrors/node/\r\n");
        assert_eq!(node_index_url(&dir).unwrap(), "https://npmmirror.com/mirrors/node/index.json");

        // 少了结尾斜杠：补一个（nvm 也补，不补的话两边地址就不一样了）
        write("node_mirror: https://npmmirror.com/mirrors/node\r\n");
        assert_eq!(node_index_url(&dir).unwrap(), "https://npmmirror.com/mirrors/node/index.json");

        // 没写 http：nvm 会补 `http://`，我们照做
        write("node_mirror: mirrors.example.com/node/\r\n");
        assert_eq!(node_index_url(&dir).unwrap(), "http://mirrors.example.com/node/index.json");

        // `none` = 不用镜像（与空值同义，见 nvm 的 SetMirrors）
        write("node_mirror: none\r\n");
        assert_eq!(node_index_url(&dir).unwrap(), "https://nodejs.org/dist/index.json");

        // 极短/带中文的怪值不能 panic（Go 那边会因为切 `[0:4]` 直接崩）
        write("node_mirror: 镜像\r\n");
        assert_eq!(node_index_url(&dir).unwrap(), "http://镜像/index.json");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 真去拉一次索引（联网）。手动跑：
    /// ```text
    /// cargo test --lib --manifest-path src-tauri/Cargo.toml -- --ignored fetch_node_index
    /// ```
    /// 留着它的价值：curl 的路径、参数、落盘、解析、以及"镜像配的是哪个地址"这条链路，
    /// 单测构造不出来——而它一旦断了，面板上的搜索框就是空的。
    #[test]
    #[ignore]
    fn test_fetch_node_index() {
        let nvm = find_nvm().expect("本机应当装有 nvm-windows");
        let url = node_index_url(&nvm.home).expect("读 settings.txt");
        let versions = fetch_index(&url).expect("拉取版本索引应当成功");
        println!("索引 {} → {} 个版本，最新 {}", url, versions.len(), versions[0].version);
        assert!(versions.len() > 100, "完整索引应当有上百个版本，实际 {}", versions.len());
        assert!(versions.iter().all(|v| v.version.starts_with('v')), "版本号应当带 v 前缀");
        assert!(
            versions.iter().any(|v| !v.lts.is_empty()),
            "应当有 LTS 版本（代号字段没解析出来？）",
        );
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

    /// 「装完 nvm 不用重启 Nexus」的真正门槛：**子进程**能不能自己找到 nvm。
    ///
    /// 复现用户报的那个状态：把 `NVM_HOME` / `NVM_SYMLINK` 从本进程环境里摘掉
    /// （等价于"Nexus 启动时 nvm 还没装"），再跑一条 nvm 命令。面板读注册表只能让**界面**
    /// 看到 nvm；而 nvm.exe 是从环境变量找 `settings.txt` 的——摘掉之后就只剩我们显式传参
    /// 这一条路，所以这条测试就是那个场景的复现。
    ///
    /// 手动跑（会改本测试进程的环境变量，影响同进程其它测试，所以单独跑）：
    /// ```text
    /// cargo test --lib --manifest-path src-tauri/Cargo.toml -- --ignored nvm_runs_without
    /// ```
    #[test]
    #[ignore]
    fn test_nvm_runs_without_inherited_env() {
        std::env::remove_var("NVM_HOME");
        std::env::remove_var("NVM_SYMLINK");

        // 连"找 nvm"这一步也只能靠注册表兜底了
        let nvm = find_nvm().expect("环境变量摘掉后，仍应当能从注册表找到 nvm");
        let res = run_nvm(&nvm, &["root"]).expect("执行 nvm root");
        println!("nvm root → ok={} output={:?}", res.ok, res.output);
        assert!(res.ok, "nvm 应当仍能工作（它靠 NVM_HOME 找 settings.txt）: {}", res.output);
        assert!(
            res.output.contains(&nvm.root.display().to_string()),
            "nvm 报的 root 应当是 {}，实际输出: {}",
            nvm.root.display(),
            res.output,
        );
    }

    /// 下载链路真跑一次：curl 的路径、参数、落盘校验都要走一遍（只下不装）。
    ///
    /// 手动跑（会联网、往 %TEMP%\nexus\ 写 5.6 MB）：
    /// ```text
    /// cargo test --lib --manifest-path src-tauri/Cargo.toml -- --ignored download_nvm_setup
    /// ```
    #[test]
    #[ignore]
    fn test_download_nvm_setup() {
        let dest = std::env::temp_dir().join("nexus").join("nvm-setup-1.2.2.exe");
        let _ = std::fs::remove_file(&dest); // 从零下，别复用上次的
        download_setup(&dest).expect("下载官方安装器应当成功");
        let size = std::fs::metadata(&dest).expect("文件应当存在").len();
        println!("已下载 {}: {} 字节", dest.display(), size);
        assert!(valid_installer(&dest), "下载到的文件应当通过校验（大小 + MZ 头）");
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
