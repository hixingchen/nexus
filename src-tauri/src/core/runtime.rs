//! AI 私有运行时：Nexus 自带一份 Node + dsh，**只给 AI 面板用**。
//!
//! # 为什么必须自带（这是本仓一次真实事故的收口）
//!
//! dsh 的启动链原本是：`cmd /C "dsh web …"` → PATH 上的 npm shim → shim 里写的是**裸 `node`**
//! → 再查一次 PATH → 落到 nvm 的软链（`C:\nvm4w\nodejs` → 当前版本目录）。
//! 于是"AI 用哪个 Node"由**用户 nvm 当前选中的版本**决定，而 dsh 对 Node 有硬性要求：
//! 它的入口是 `if (import.meta.main) await runCli();`，而 `import.meta.main` 只在
//! **Node ≥ 22.18.0 / ≥ 24.2.0** 存在。切到 Node 20 时这一行永远不成立——
//! **不报错、不输出、exit 0**，面板只能干等（切到 16 则报一个与真实原因无关的 SyntaxError）。
//!
//! 对策是把解释器**钉死成本模块产出的一对绝对路径**（`node.exe` + dsh 的 `bin.js`），
//! 调用方直接 spawn：全程不经 cmd、不查 PATH、不受 nvm / fnm / 用户全局包影响。
//!
//! # 为什么在 `~/.nexus/runtime` 下而不是塞进安装包
//!
//! 体重全在 dsh 上（**装完 269MB**），Node zip 只有 36MB，而 dsh 的发布节奏比 Nexus 快。
//! 打进安装包等于每次发版都把 300MB 重发一遍，且用户拿不到新版 dsh。所以按需下载。
//!
//! # 目录形状（版本化目录 + 指针 = "可替换但不覆盖"）
//!
//! ```text
//! <data_dir>/runtime/
//!   node/
//!     v24.21.0/…        ← 每版一个目录，换版本时不覆盖旧的
//!     current.txt       ← 内容是 "v24.21.0"；**只在整条链全部验证通过后才写**
//!   ai/                 ← 用当前指针那份 node 装的 dsh
//! ```
//!
//! 指针最后写 = 天然的原子性：中途失败留下的半成品永远不会被 `resolve()` 认成"已就绪"。
//! 换运行时前必须先停会话（`node.exe` 在 Windows 上被占用就删/覆盖不掉），由调用方负责。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Serialize;

use crate::core::job_object::JobObject;

/// 随 Nexus 分发的 Node 版本。
///
/// **不要凭"24 是最新 LTS"改这一行**：`import.meta.main` 在 24 线上是 **24.2.0** 才有的，
/// **24.0.0 / 24.1.x 是 `undefined`**——那两个版本会让 dsh 静默什么都不干，正是要杜绝的形态。
/// 改这里必须同时改下一条哈希，并且**在真机上跑通一次 `install`**（`ai_runtime_install`
/// 的收尾就是实跑一次 `dsh --version`，它才是真正的门禁）。
const NODE_VERSION: &str = "v24.21.0";

/// `node-v24.21.0-win-x64.zip` 的官方 SHA-256。
///
/// 出处与交叉验证（2026-10-14）：nodejs.org 的 `SHASUMS256.txt` 与 npmmirror 镜像上的同名文件
/// **两个独立来源一致**。这个 zip 解出来的 `node.exe` 会被我们直接执行，所以按仓库既有的
/// SEC-20 口径**钉死哈希**，而不是"下到什么算什么"。
const NODE_ZIP_SHA256: &str = "158f7685b44de51f6c0df1d153526cbcd3e1bc739a8dfc607721cef75de9e541";

/// 官方 zip 的字节数（进度条的分母；也可当"下没下全"的粗筛）
const NODE_ZIP_BYTES: u64 = 37_618_919;

/// 下载源，**按序尝试**。
///
/// 镜像放前面不是偏好问题：本仓的 `NodeVersionSection` 早就因为
/// "`nodejs.org/dist` 在国内机器上经常直接失败"而内置了 npmmirror。
const NODE_URLS: &[&str] = &[
    "https://npmmirror.com/mirrors/node/v24.21.0/node-v24.21.0-win-x64.zip",
    "https://nodejs.org/dist/v24.21.0/node-v24.21.0-win-x64.zip",
];

/// dsh 在 npm 上的包名
pub const DSH_PACKAGE: &str = "@deepseek-ai/dsh";

/// 安装步骤（顺序即 `RuntimeProgress::step_index` 的下标；界面据此画固定布局的步骤条）
///
/// 步骤名里**不写体积**：两处口径不同（下载 ≈260MB、装完占磁盘 ≈617MB，均本机实测 2026-10-14），
/// 写进步骤名只会让人对不上账——总下载量由按钮那句话交代，实际占用由状态行显示。
pub const STEPS: [&str; 5] = [
    "下载 Node 运行时",
    "校验文件完整性",
    "解压 Node",
    "安装 dsh（最慢的一步）",
    "验证能不能跑",
];

/// 下载 Node 的超时（秒）。36MB + 国内镜像，900 秒是给慢网留的宽裕值
const DOWNLOAD_TIMEOUT_SECS: u32 = 900;

// ─── 路径 ────────────────────────────────────────────────────

/// 私有运行时根目录：`<data_dir>/runtime`
///
/// 走 `crate::data_dir()` 而不是自己拼 `~/.nexus`：它认 `NEXUS_DATA_DIR` 覆盖，
/// 于是"真机跑一次"可以拿一个完全隔离的实例做实验，不碰用户的真实运行时。
pub fn runtime_root() -> PathBuf {
    crate::data_dir().join("runtime")
}

fn node_root() -> PathBuf {
    runtime_root().join("node")
}

/// 某个 Node 版本的目录（版本化：换版本不覆盖旧的）
pub fn node_dir(version: &str) -> PathBuf {
    node_root().join(version)
}

/// 指针文件（内容是当前生效的 Node 版本目录名）
fn node_pointer() -> PathBuf {
    node_root().join("current.txt")
}

/// 私有 dsh 的安装目录（npm 的 `--prefix`）
pub fn ai_dir() -> PathBuf {
    runtime_root().join("ai")
}

/// dsh 的入口 js 相对安装目录的位置
fn dsh_js_in(ai: &Path) -> PathBuf {
    ai.join("node_modules")
        .join("@deepseek-ai")
        .join("dsh")
        .join("lib")
        .join("bin.js")
}

// ─── 运行时句柄 ───────────────────────────────────────────────

/// 这一对路径是哪来的（界面据此决定给哪个按钮：重装 or 清除手动指定）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSource {
    /// 用户在内网/下载不通时**手动指定**的
    Manual,
    /// Nexus 自带、自己下载装好的
    Private,
}

/// 一对**绝对路径**——这就是"钉解释器"的全部内容。
#[derive(Clone, Debug, Serialize)]
pub struct AiRuntime {
    /// `node.exe` 的绝对路径
    pub node_exe: String,
    /// dsh 入口 `bin.js` 的绝对路径
    pub dsh_js: String,
    /// 自带运行时那份 Node 的版本（目录名，如 `v24.21.0`）；
    /// **手动指定时为空串**——那是用户自己的 Node，Nexus 不去猜它的版本号
    pub node_version: String,
    /// 这一对路径的来源
    pub source: RuntimeSource,
}

/// 起一个**绝对路径**的程序，并压住它可能弹出的控制台窗口。
///
/// 为什么必须有这一位（1.9.0 的回归）：打包版 Nexus 是 GUI 子系统、**自己没有控制台**
/// （见 `main.rs` 的 `windows_subsystem = "windows"`），而 `node.exe` 是控制台程序——
/// "父进程无控制台 + 没给 `CREATE_NO_WINDOW`"时 `CreateProcess` 会给它**新建一个控制台窗口**。
/// 现象：打开 AI 面板时闪出一个标题为 `…\runtime\node\<版本>\node.exe` 的黑窗（探活那一次），
/// 会话起来后那个窗口一直留在桌面上；用户顺手关掉它 = 连 AI 会话一起关掉。
/// 本机实测（2026-10-14，GUI 子系统父进程 + 这份私有 `node.exe`）：不带标志会新建一个
/// `CASCADIA_HOSTING_WINDOW_CLASS` 窗口（标题正是 node.exe 全路径），带上之后一个都不建。
///
/// 这条链原本是 `cmd /C "dsh web …"`，窗口由 `process::SERVICE_CREATE_FLAGS` 顺带压住；
/// 改成"绝对路径直起"之后标志丢了——`cmd` 那条路还在带，本模块的四个 spawn 点却一个都没带。
fn direct_command(exe: &Path) -> Command {
    let mut c = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // 控制台程序**不带窗口**运行：控制台句柄不设、窗口不建，管道读写不受影响
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

impl AiRuntime {
    /// 起一个 dsh 命令：`<node.exe> <bin.js> <args…>`
    ///
    /// **不经 cmd、不查 PATH**——这正是整个模块存在的理由。调用方拿到 Command 后
    /// 自己设 cwd / stdio，再交给 `core::process::run_captured_command` 或 `.spawn()`。
    pub fn command(&self, args: &[&str]) -> Command {
        let mut c = direct_command(Path::new(&self.node_exe));
        c.arg(&self.dsh_js).args(args);
        c
    }

    /// 实跑一次 `<node> <bin.js> --version`，**要求输出里真有版本号**才算通过。
    ///
    /// 为什么判据是"有版本号"而不是"退出码为 0"：dsh 在 `import.meta.main` 缺失的 Node 上
    /// **退出码就是 0**（入口整段被跳过）。只判退出码会把"什么都没干"认成"可用"，
    /// 接着 `dsh web` 同样零输出，用户只能干等超时——这个洞是事故的直接成因。
    pub fn probe(&self, job: Option<&JobObject>) -> Result<String, String> {
        let mut cmd = self.command(&["--version"]);
        let label = format!("{} {} --version", self.node_exe, self.dsh_js);
        let out = crate::core::process::run_captured_command(job, &mut cmd, &label, crate::core::ai::AiTimeouts::VERSION_PROBE)?;
        crate::core::ai::extract_version(&out).ok_or_else(|| {
            format!(
                "命令成功退出但**没有任何版本号输出**（这正是 Node 版本不兼容时的形态）：\n{}",
                crate::core::process::tail_of(&out)
            )
        })
    }

    /// 跑一条 npm 子命令（**这份运行时自己的 node + 它旁边那份 npm**）。
    ///
    /// 私有化之后目标机器上可能根本没有 Node，PATH 上没有 npm 可用；就算有，那也是
    /// **别的** Node 的 npm。凡是查/装 dsh 都得走这里。
    pub fn npm_command(&self, args: &[&str]) -> Result<Command, String> {
        let node = Path::new(&self.node_exe);
        let npm = npm_cli_beside(node).ok_or_else(|| {
            format!(
                "这份 Node 旁边找不到 npm（{}）——{}",
                node.display(),
                if self.source == RuntimeSource::Manual {
                    "你指定的这份 Node 可能是精简版（只带 node.exe）。dsh 的升级需要 npm，请改用完整安装，或自行升级 dsh"
                } else {
                    "运行时目录可能被改动过，建议「重新安装」"
                }
            )
        })?;
        let mut c = direct_command(Path::new(&self.node_exe));
        c.arg(npm).args(args);
        Ok(c)
    }
}

/// 把私有那份 dsh 装到最新版（或首次安装），**装完实跑一次确认**。
///
/// 为什么不能只看 npm 的退出码：npm 说"成功"不等于装出来的东西能跑，而私有化之后
/// 这份 dsh 是**唯一**路径——装坏了就是 AI 没了。所以收尾必须真起一次 `--version`，
/// 拿不到版本号就按失败处理（原实现 `unwrap_or_default()` 会给界面一个空串，
/// 前端照样弹"安装完成"）。
pub fn install_or_upgrade_dsh(
    rt: &AiRuntime,
    job: Option<&JobObject>,
) -> Result<String, RuntimeFailure> {
    let node_exe = Path::new(&rt.node_exe);
    let npm = npm_cli_beside(Path::new(&rt.node_exe))
        .ok_or_else(|| RuntimeFailure::new(3, "这份 Node 旁边找不到 npm，无法升级 dsh", String::new()))?;
    install_dsh(job, node_exe, &npm).map_err(|e| RuntimeFailure::new(3, "npm 安装 dsh 失败", e))?;
    rt.probe(job)
        .map_err(|e| RuntimeFailure::new(3, "装完了但 dsh 跑不起来，本次升级未生效", e))
}

/// 读指针 + 检查两处文件都在 → 得到一对可直接使用的绝对路径。
///
/// 这一步**不实跑**（快、不占进程），"能不能跑"由 `AiRuntime::probe` 回答。
/// 调用方按需组合：日常启动走 `resolve()`，状态检查/安装收尾走 `probe()`。
///
/// **手动指定优先，且不做静默回落**：用户明确填了路径就用它，坏了就如实报错并给出
/// "清除手动指定"的出口。悄悄退回自带那份看起来"更稳"，实际会让用户困惑于
/// "我明明指定了为什么不生效"——与本轮事故同一类的沉默失败。
pub fn resolve() -> Option<AiRuntime> {
    resolve_rooted(&crate::data_dir())
}

/// `resolve` 的本体。**数据目录作参数**（而不是内部读 `data_dir()`）是为了让用例能指向
/// 临时目录——本仓在这上面踩过：靠 `set_var` 隔离会与同进程并行跑的其它用例互相踩。
fn resolve_rooted(data: &Path) -> Option<AiRuntime> {
    manual_runtime(data).or_else(|| resolve_in(&data.join("runtime")))
}

/// 手动指定的那一对（未配置 / 文件已不在 → None）
fn manual_runtime(data: &Path) -> Option<AiRuntime> {
    let m = read_manual(data)?;
    let node_exe = PathBuf::from(&m.node_exe);
    let dsh_js = PathBuf::from(&m.dsh_js);
    if !node_exe.is_file() || !dsh_js.is_file() {
        return None;
    }
    Some(AiRuntime {
        node_exe: node_exe.display().to_string(),
        dsh_js: dsh_js.display().to_string(),
        node_version: String::new(),
        source: RuntimeSource::Manual,
    })
}

// ─── 手动指定（A 方案：内网 / 下载不通时的逃生口） ──────────────

/// 手动指定的落盘位置：`<data_dir>/ai-runtime.json`。
///
/// **放在 runtime 目录之外**（而不是 `runtime/manual.json`）：卸载运行时要删掉整个
/// `runtime/`，而"我指定过哪条路径"是用户偏好，不该跟着一起没。
fn manual_config_at(data: &Path) -> PathBuf {
    data.join("ai-runtime.json")
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ManualConfig {
    /// `node.exe` 的绝对路径
    node_exe: String,
    /// dsh 入口 `bin.js` 的绝对路径（保存时已归一化，见 `normalize_dsh_entry`）
    dsh_js: String,
}

fn read_manual(data: &Path) -> Option<ManualConfig> {
    let raw = std::fs::read_to_string(manual_config_at(data)).ok()?;
    let cfg: ManualConfig = serde_json::from_str(&raw).ok()?;
    (!cfg.node_exe.trim().is_empty() && !cfg.dsh_js.trim().is_empty()).then_some(cfg)
}

/// 把用户填的东西归一化成 `bin.js` 的绝对路径。
///
/// **三种写法都认**：用户没法预期 dsh 内部的目录形状，让他自己去翻
/// `node_modules/@deepseek-ai/dsh/lib/bin.js` 是不合理的。认的形态：
/// ① 直接指到 `…/lib/bin.js`；② 指到 dsh 包目录（`…/@deepseek-ai/dsh`）；
/// ③ 指到装了 dsh 的项目目录（`…/node_modules` 的上一级）。
fn normalize_dsh_entry(input: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(input.trim());
    if !p.exists() {
        return Err(format!("路径不存在：{}", p.display()));
    }
    if p.is_file() {
        return if p.file_name().and_then(|s| s.to_str()) == Some("bin.js") {
            Ok(p)
        } else {
            Err(format!(
                "这是个文件但不是 dsh 的入口：{}\n请填 dsh 包目录，或直接指到 …\\dsh\\lib\\bin.js",
                p.display()
            ))
        };
    }
    let candidates = [
        p.join("lib").join("bin.js"),                                   // …/@deepseek-ai/dsh
        p.join("node_modules").join("node_modules").join("dsh").join("lib").join("bin.js"),
        p.join("node_modules").join("lib").join("bin.js"),
        p.join("node_modules").join(DSH_PACKAGE).join("lib").join("bin.js"), // …/（装了 dsh 的项目）
        p.join("bin.js"),
    ];
    candidates
        .into_iter()
        .find(|c| c.is_file())
        .ok_or_else(|| {
            format!(
                "这个目录里找不到 dsh 的入口（lib\\bin.js）：{}\n\
                 请填 dsh 包目录（…\\node_modules\\@deepseek-ai\\dsh），或直接指到 …\\lib\\bin.js",
                p.display()
            )
        })
}

/// 保存手动指定。**当场实测一次**才落盘——设一个跑不起来的路径等于把 AI 设死，
/// 而"保存成功了却不能用"正是最难查的那种失败。
pub fn set_manual(
    node_exe: &str,
    dsh_path: &str,
    job: Option<&JobObject>,
) -> Result<AiRuntime, String> {
    set_manual_at(&crate::data_dir(), node_exe, dsh_path, job)
}

fn set_manual_at(
    data: &Path,
    node_exe: &str,
    dsh_path: &str,
    job: Option<&JobObject>,
) -> Result<AiRuntime, String> {
    let node = PathBuf::from(node_exe.trim());
    if !node.is_file() {
        return Err(format!("node.exe 不存在：{}", node.display()));
    }
    if node.file_name().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase())
        != Some("node.exe".to_string())
    {
        return Err(format!(
            "这不是 node.exe：{}\n请填 node.exe 的完整路径（例如 C:\\…\\node.exe）",
            node.display()
        ));
    }
    let dsh_js = normalize_dsh_entry(dsh_path)?;

    let rt = AiRuntime {
        node_exe: node.display().to_string(),
        dsh_js: dsh_js.display().to_string(),
        node_version: String::new(),
        source: RuntimeSource::Manual,
    };
    // 实测：`dsh --version` 必须吐出真正的版本号（与自带运行时同一条判据——
    // 用户手里那个 Node 完全可能就是不兼容的那个版本，而那正是他换手动路径的原因）
    rt.probe(job)?;

    let cfg = ManualConfig { node_exe: rt.node_exe.clone(), dsh_js: rt.dsh_js.clone() };
    let text = serde_json::to_string_pretty(&cfg).map_err(|e| format!("序列化失败: {}", e))?;
    let path = manual_config_at(data);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录 {} 失败: {}", dir.display(), e))?;
    }
    std::fs::write(&path, text).map_err(|e| format!("写入 {} 失败: {}", path.display(), e))?;
    log::info!("[nexus] 已保存手动指定的运行时：{} + {}", rt.node_exe, rt.dsh_js);
    Ok(rt)
}

/// 清除手动指定（回到用自带运行时）
pub fn clear_manual() -> Result<(), String> {
    let p = manual_config_at(&crate::data_dir());
    if !p.exists() {
        return Ok(());
    }
    std::fs::remove_file(&p).map_err(|e| format!("删除 {} 失败: {}", p.display(), e))
}

/// 是否设了手动指定（界面据此显示"清除"入口）
pub fn has_manual() -> bool {
    read_manual(&crate::data_dir()).is_some()
}


/// `resolve` 的本体，根目录作参数——让用例能指向临时目录，
/// 而不是依赖"这台机器上装没装运行时"（那种用例在别人机器上会静默变成空转）。
fn resolve_in(root: &Path) -> Option<AiRuntime> {
    let version = std::fs::read_to_string(root.join("node").join("current.txt"))
        .ok()?
        .trim()
        .to_string();
    if version.is_empty() {
        return None;
    }
    let node_dir = root.join("node").join(&version);
    let node_exe = node_dir.join("node.exe");
    if !node_exe.is_file() {
        return None;
    }
    let dsh_js = dsh_js_in(&root.join("ai"));
    if !dsh_js.is_file() {
        return None;
    }
    Some(AiRuntime {
        node_exe: node_exe.display().to_string(),
        dsh_js: dsh_js.display().to_string(),
        node_version: version,
        source: RuntimeSource::Private,
    })
}

/// **紧挨着这个 node.exe 的那份 npm**（官方布局：`<node 目录>/node_modules/npm/bin/npm-cli.js`）。
///
/// 按 node.exe 的位置找而不是按版本号拼路径：自带运行时与**用户手动指定的那份**
/// 布局相同，一条逻辑覆盖两种来源；而版本号对手动那份根本没有意义。
pub fn npm_cli_beside(node_exe: &Path) -> Option<PathBuf> {
    let dir = node_exe.parent()?;
    let p = dir.join("node_modules").join("npm").join("bin").join("npm-cli.js");
    p.is_file().then_some(p)
}

// ─── 进度与失败 ───────────────────────────────────────────────

/// 安装进度（经 Tauri 事件推给界面）
#[derive(Clone, Debug, Serialize)]
pub struct RuntimeProgress {
    /// 当前步骤下标（`STEPS` 的下标）
    pub step_index: usize,
    /// 这一步里的子状态（人话）
    pub detail: String,
    /// 已下载字节（仅"下载"步有意义）
    pub downloaded: u64,
    /// 预期字节（0 = 未知）
    pub total: u64,
}

/// 安装失败：**按"卡在哪一步"结构化**，而不是一个字符串。
///
/// 为什么：字符串没法让界面把"卡在哪、原话是什么、能做什么"分开呈现，而这三样正是
/// 用户排障需要的（本轮事故里界面只会说"未检测到 dsh"和"进程提前退出"，两句都指不到原因）。
#[derive(Clone, Debug, Serialize)]
pub struct RuntimeFailure {
    /// 步骤下标（对应 [`STEPS`]）；`None` = 与某个具体步骤无关（如"已经在装了"），
    /// 界面据此决定要不要高亮步骤条上的某一格
    pub step_index: Option<usize>,
    /// 步骤名
    pub step: String,
    /// 一句话人话摘要
    pub summary: String,
    /// 原始输出（curl / node / npm 的原话）
    pub raw: String,
}

impl RuntimeFailure {
    fn new(step_index: usize, summary: impl Into<String>, raw: impl Into<String>) -> Self {
        Self {
            step_index: Some(step_index),
            step: STEPS.get(step_index).unwrap_or(&"未知步骤").to_string(),
            summary: summary.into(),
            raw: raw.into(),
        }
    }

    fn custom(step: &str, summary: impl Into<String>, raw: impl Into<String>) -> Self {
        Self {
            step_index: None,
            step: step.to_string(),
            summary: summary.into(),
            raw: raw.into(),
        }
    }

    /// 已经有一次安装在进行（两个安装并发写同一个目录会互相踩）
    pub fn busy() -> Self {
        Self::custom("准备安装", "已经在安装了，请等这一次跑完再试", String::new())
    }

    /// 任务本身异常结束（如工作线程 panic）
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::custom("安装任务", "安装任务异常结束", msg.into())
    }

    /// 拼成一段既给人看、也给排障看的文本：**先说卡在哪、再说人话、最后贴原话**。
    /// 顺序有讲究——用户读前两行就够，排障的人要往下看原话。
    pub fn to_message(&self) -> String {
        let mut s = format!("【{}】{}", self.step, self.summary);
        if !self.raw.trim().is_empty() {
            s.push_str("\n\n");
            s.push_str(self.raw.trim());
        }
        s
    }
}

/// 安装中标志：防并发（两个安装同时写同一个目录会互相踩）
static INSTALLING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 拿安装许可；拿不到（已有安装在进行）返回 `None`
pub struct InstallSlot;

impl Drop for InstallSlot {
    fn drop(&mut self) {
        INSTALLING.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

pub fn try_acquire_install() -> Option<InstallSlot> {
    INSTALLING
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::Relaxed,
        )
        .ok()
        .map(|_| InstallSlot)
}

// ─── 安装 ────────────────────────────────────────────────────

/// 完整安装：下载 → 校验 → 解压 → 装 dsh → 验证 → 写指针。
///
/// `on_progress` 每个步骤至少调一次（含"开始了这一步"与"这一步完成了"），
/// 下载步另按字节数持续回调。失败一律返回 [`RuntimeFailure`]，**绝不写指针**。
pub fn install(
    job: Option<&JobObject>,
    on_progress: &dyn Fn(RuntimeProgress),
) -> Result<AiRuntime, RuntimeFailure> {
    let root = runtime_root();
    std::fs::create_dir_all(root.join("node")).map_err(|e| {
        RuntimeFailure::new(0, format!("建目录 {} 失败", root.display()), e.to_string())
    })?;

    // ── 1. 下载 ──────────────────────────────────────────────
    let zip_path = node_root().join(format!("node-{}-win-x64.zip", NODE_VERSION));
    if !zip_is_valid(&zip_path) {
        let _ = std::fs::remove_file(&zip_path); // 半截文件留着会让"已有一份合格的"判断永远失灵
        download_zip(&zip_path, on_progress)?;
    }
    on_progress(RuntimeProgress {
        step_index: 0,
        detail: format!("已下载 {}", human_mb(NODE_ZIP_BYTES)),
        downloaded: NODE_ZIP_BYTES,
        total: NODE_ZIP_BYTES,
    });

    // ── 2. 校验 ──────────────────────────────────────────────
    on_progress(RuntimeProgress { step_index: 1, detail: "正在核对 SHA-256…".into(), downloaded: 0, total: 0 });
    if !zip_is_valid(&zip_path) {
        let got = file_sha256(&zip_path).unwrap_or_else(|e| format!("(算不出: {})", e));
        let _ = std::fs::remove_file(&zip_path);
        return Err(RuntimeFailure::new(
            1,
            "下载到的文件与官方版本不符，已删除，请重试",
            format!("期望 SHA-256：{}\n实际 SHA-256：{}", NODE_ZIP_SHA256, got),
        ));
    }

    // ── 3. 解压 ──────────────────────────────────────────────
    let dest = node_dir(NODE_VERSION);
    // 先解到临时目录再改名：中途失败不会留下一个"看着像但缺文件"的版本目录
    let staging = node_root().join(format!("{}.partial", NODE_VERSION));
    let _ = std::fs::remove_dir_all(&staging);
    on_progress(RuntimeProgress { step_index: 2, detail: format!("正在解压到 {}", dest.display()), downloaded: 0, total: 0 });
    extract_node_zip(&zip_path, &staging).map_err(|e| RuntimeFailure::new(2, "解压失败", e))?;
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&staging, &dest).map_err(|e| {
        RuntimeFailure::new(2, format!("解压结果落位失败：{}", dest.display()), e.to_string())
    })?;
    // 解压出来的 node.exe 先自己跑一次：解压/落位出问题时在这里就断，不要等到装完 269MB 才发现
    let node_exe = dest.join("node.exe");
    if !node_exe.is_file() {
        return Err(RuntimeFailure::new(
            2,
            format!("解压后没找到 {}（zip 结构与预期不符）", node_exe.display()),
            String::new(),
        ));
    }
    let node_ver = run_direct(job, &node_exe, &["--version"], None, crate::core::ai::AiTimeouts::VERSION_PROBE)
        .map_err(|e| {
            RuntimeFailure::new(2, format!("{} 跑不起来", node_exe.display()), e)
        })?;
    on_progress(RuntimeProgress { step_index: 2, detail: format!("Node {}", node_ver.trim()), downloaded: 0, total: 0 });

    // ── 4. 装 dsh ────────────────────────────────────────────
    on_progress(RuntimeProgress { step_index: 3, detail: format!("正在安装 {}（首次较慢）", DSH_PACKAGE), downloaded: 0, total: 0 });
    let npm = npm_cli_beside(&node_exe).ok_or_else(|| {
        RuntimeFailure::new(
            3,
            "解压出来的 Node 里没有 npm（官方 zip 本应自带）",
            format!("找不到 {}", node_dir(NODE_VERSION).join("node_modules/npm/bin/npm-cli.js").display()),
        )
    })?;
    install_dsh(job, &node_exe, &npm).map_err(|e| RuntimeFailure::new(3, "npm 安装 dsh 失败", e))?;

    // ── 5. 验证 ──────────────────────────────────────────────
    on_progress(RuntimeProgress { step_index: 4, detail: "正在确认真能跑起来…".into(), downloaded: 0, total: 0 });
    let runtime = AiRuntime {
        node_exe: node_exe.display().to_string(),
        dsh_js: dsh_js_in(&ai_dir()).display().to_string(),
        node_version: NODE_VERSION.to_string(),
        source: RuntimeSource::Private,
    };
    let dsh_ver = runtime
        .probe(job)
        .map_err(|e| RuntimeFailure::new(4, "装完了但 dsh 跑不起来，运行时未启用", e))?;

    // 全链通过才写指针（失败时 resolve() 必须看不见这次尝试）
    std::fs::write(node_pointer(), NODE_VERSION).map_err(|e| {
        RuntimeFailure::new(4, "写指针失败", format!("{}：{}", node_pointer().display(), e))
    })?;
    // zip 留着没意义（~36MB），且下次"已有一份合格的"判断会白读一遍哈希
    let _ = std::fs::remove_file(&zip_path);

    log::info!("[nexus] AI 私有运行时就绪：Node {} / dsh {}", node_ver.trim(), dsh_ver);
    Ok(runtime)
}

/// 按序尝试各下载源，边下边报字节数
fn download_zip(dest: &Path, on_progress: &dyn Fn(RuntimeProgress)) -> Result<(), RuntimeFailure> {
    let mut last: Option<String> = None;
    for url in NODE_URLS {
        let cb = |got: u64| {
            on_progress(RuntimeProgress {
                step_index: 0,
                detail: format!("{} / {}", human_mb(got), human_mb(NODE_ZIP_BYTES)),
                downloaded: got,
                total: NODE_ZIP_BYTES,
            });
        };
        match crate::core::download::download_with_progress(url, dest, DOWNLOAD_TIMEOUT_SECS, &cb) {
            Ok(()) => return Ok(()),
            Err(e) => {
                log::warn!("[nexus] 运行时下载源失败，换下一个：{}", e);
                last = Some(e);
                let _ = std::fs::remove_file(dest);
            }
        }
    }
    Err(RuntimeFailure::new(
        0,
        "所有下载源都失败了（可能是网络或镜像问题）",
        last.unwrap_or_else(|| "没有可用的下载源".into()),
    ))
}

/// zip 是不是官方那份：**大小粗筛 + SHA-256**（先粗筛是为了不把几百 MB 的垃圾整个读进来算哈希）
fn zip_is_valid(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if !meta.is_file() || meta.len() != NODE_ZIP_BYTES {
        return false;
    }
    match file_sha256(path) {
        Ok(got) => got.eq_ignore_ascii_case(NODE_ZIP_SHA256),
        Err(e) => {
            log::warn!("[nexus] 运行时 zip 哈希计算失败（按不合格处理）: {}", e);
            false
        }
    }
}

/// 文件内容的 SHA-256（小写十六进制）。
///
/// 与 `commands::node` 里那份同形，但**不复用**：那份的期望值、大小下限、MZ 预筛都是
/// 安装器专用的，抽公共壳只会让两边互相将就。这里只有"算一个文件的哈希"这一件事。
fn file_sha256(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(path).map_err(|e| format!("打开 {} 失败: {}", path.display(), e))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut f, &mut hasher).map_err(|e| format!("读取 {} 失败: {}", path.display(), e))?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// 解压 Node 官方 zip，**剥掉顶层目录**（`node-v24.21.0-win-x64/`）落到 `dest`。
///
/// 顶层目录名从条目里**探测**而不是拼常量：官方 zip 的命名随平台/版本变过，
/// 探测失败时报"zip 结构与预期不符"，比"解压出一个空目录、直到找不到 node.exe 才报错"好定位。
fn extract_node_zip(zip_path: &Path, dest: &Path) -> Result<(), String> {
    let file = std::fs::File::open(zip_path)
        .map_err(|e| format!("打开 {} 失败: {}", zip_path.display(), e))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("{} 不是有效的 zip: {}", zip_path.display(), e))?;

    let mut top: Option<String> = None;
    let mut count = 0usize;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("读取 zip 第 {} 个条目失败: {}", i, e))?;
        // `enclosed_name` 挡路径穿越（zip-slip）：带 `..` 的条目直接跳过
        let Some(rel) = entry.enclosed_name() else { continue };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let mut parts = rel.splitn(2, '/');
        let first = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("");
        if first.is_empty() {
            continue;
        }
        if top.is_none() {
            top = Some(first.to_string());
        }
        if Some(first) != top.as_deref() || rest.is_empty() {
            continue; // 顶层目录本身，或与顶层不一致的杂项
        }

        let out_path = dest.join(rest);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)
                .map_err(|e| format!("建目录 {} 失败: {}", out_path.display(), e))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("建目录 {} 失败: {}", parent.display(), e))?;
        }
        let mut out = std::fs::File::create(&out_path)
            .map_err(|e| format!("写 {} 失败: {}", out_path.display(), e))?;
        std::io::copy(&mut entry, &mut out)
            .map_err(|e| format!("解压 {} 失败: {}", out_path.display(), e))?;
        count += 1;
    }
    if count == 0 {
        return Err(format!(
            "{} 里没有解出任何文件（顶层目录探测为 {:?}）",
            zip_path.display(),
            top
        ));
    }
    Ok(())
}

/// 用**私有 node 跑私有 npm**，把 dsh 装进私有目录。
///
/// **必须是 `--prefix` 而不是 `-g`**：`-g` 的落地位置由用户级 `~/.npmrc` 里的 `prefix=` 决定
/// （本机是 `node_global`），于是新版会被装进**用户那份**、私有目录里还是旧的——
/// 界面报"升级成功"、实际跑的还是老版本，且**不报任何错**。
/// 命令行 `--prefix` 的优先级高于 npmrc，显式压过它。
fn install_dsh(job: Option<&JobObject>, node_exe: &Path, npm_cli: &Path) -> Result<(), String> {
    let prefix = ai_dir();
    std::fs::create_dir_all(&prefix).map_err(|e| format!("建目录 {} 失败: {}", prefix.display(), e))?;
    let mut cmd = direct_command(node_exe);
    cmd.arg(npm_cli)
        .arg("install")
        .arg("--prefix")
        .arg(&prefix)
        .arg(format!("{}@latest", DSH_PACKAGE))
        .arg("--no-fund")
        .arg("--no-audit")
        .current_dir(&prefix);
    let label = format!("{} {} install --prefix {} {}@latest", node_exe.display(), npm_cli.display(), prefix.display(), DSH_PACKAGE);
    crate::core::process::run_captured_command(job, &mut cmd, &label, crate::core::ai::AiTimeouts::NPM_INSTALL).map(|_| ())
}

/// 直接起一个**绝对路径的可执行文件**并收输出（超时/杀树/Job 纳管走 `core::process` 那一套）
fn run_direct(
    job: Option<&JobObject>,
    exe: &Path,
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
) -> Result<String, String> {
    let mut cmd = direct_command(exe);
    cmd.args(args);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let label = format!("{} {}", exe.display(), args.join(" "));
    crate::core::process::run_captured_command(job, &mut cmd, &label, timeout)
}

// ─── 卸载与状态 ───────────────────────────────────────────────

/// 删掉整个私有运行时目录。
///
/// **只删 `runtime_root()`**——`~/.dsh` 一个字节都不碰（那里是登录凭据、会话历史、
/// 浏览器 profile；删了等于让用户重新登录并丢掉全部会话）。
///
/// 调用方**必须先停掉 AI 会话**：正在跑的 `dsh web` 就是私有 `node.exe`，
/// 在 Windows 上被占用就删不掉（会得到 EBUSY）。
pub fn uninstall() -> Result<(), String> {
    let root = runtime_root();
    if !root.exists() {
        return Ok(());
    }
    std::fs::remove_dir_all(&root).map_err(|e| {
        format!(
            "删除 {} 失败：{}（多半是还有进程占用着里面的 node.exe，请先停止 AI 会话再试）",
            root.display(),
            e
        )
    })?;
    // 复查：`remove_dir_all` 在 Windows 上可能"部分成功"（个别文件被占用时），
    // 不复查就会一边报成功、一边让下次安装撞上残留
    if root.exists() {
        let left = count_files(&root);
        return Err(format!(
            "{} 没能完全删除，还剩 {} 个文件（可能被杀软或残留进程占用）。可以稍后重试，或手动删除该目录。",
            root.display(),
            left
        ));
    }
    log::info!("[nexus] AI 私有运行时已删除：{}", root.display());
    Ok(())
}

/// 运行时占用的磁盘字节数（设置面板展示用）
pub fn size_on_disk() -> u64 {
    dir_size(&runtime_root())
}

fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let mut total = 0;
    for e in entries.flatten() {
        let Ok(meta) = e.metadata() else { continue };
        if meta.is_dir() {
            total += dir_size(&e.path());
        } else {
            total += meta.len();
        }
    }
    total
}

fn count_files(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let mut n = 0;
    for e in entries.flatten() {
        let Ok(meta) = e.metadata() else { continue };
        if meta.is_dir() {
            n += count_files(&e.path());
        } else {
            n += 1;
        }
    }
    n
}

/// 字节数转人话（MB，一位小数）
pub fn human_mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 解压要**剥掉顶层目录**：zip 里是 `node-vX-win-x64/node.exe`，
    /// 剥不掉的话 `dest/node.exe` 就不存在，后面整条链全崩。
    #[test]
    fn test_extract_strips_top_level_dir() {
        let tmp = std::env::temp_dir().join(format!("nexus_zip_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let zip_path = tmp.join("t.zip");

        {
            use std::io::Write;
            let f = std::fs::File::create(&zip_path).unwrap();
            let mut w = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            w.add_directory("node-v0.0.0-win-x64/", opts).unwrap();
            w.start_file("node-v0.0.0-win-x64/node.exe", opts).unwrap();
            w.write_all(b"MZ fake").unwrap();
            w.start_file("node-v0.0.0-win-x64/node_modules/npm/bin/npm-cli.js", opts).unwrap();
            w.write_all(b"// npm").unwrap();
            w.finish().unwrap();
        }

        let dest = tmp.join("out");
        extract_node_zip(&zip_path, &dest).unwrap();
        assert!(dest.join("node.exe").is_file(), "顶层目录必须被剥掉");
        assert!(
            dest.join("node_modules/npm/bin/npm-cli.js").is_file(),
            "子目录结构要保住（npm 就在里面）"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 路径穿越条目必须被跳过——解压目标是我们即将执行的 `node.exe` 的所在目录
    #[test]
    fn test_extract_skips_path_traversal() {
        let tmp = std::env::temp_dir().join(format!("nexus_zip_evil_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let zip_path = tmp.join("evil.zip");
        {
            use std::io::Write;
            let f = std::fs::File::create(&zip_path).unwrap();
            let mut w = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            w.start_file("node-v0.0.0-win-x64/../../escaped.txt", opts).unwrap();
            w.write_all(b"pwned").unwrap();
            // 混一条正常条目：光有恶意条目时整个解压会因"没解出任何文件"而失败，
            // 那样就分不清"挡住了穿越"还是"压根没解析"
            w.start_file("node-v0.0.0-win-x64/node.exe", opts).unwrap();
            w.write_all(b"MZ fake").unwrap();
            w.finish().unwrap();
        }
        let dest = tmp.join("out");
        extract_node_zip(&zip_path, &dest).unwrap();
        assert!(!tmp.join("escaped.txt").exists(), "带 .. 的条目不能落到目标目录之外");
        assert!(!dest.join("escaped.txt").exists(), "也不能被塞进目标目录里");
        assert!(dest.join("node.exe").is_file(), "正常条目要照常解出来");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `resolve()` 的三个前提缺一不可。**指向临时目录而非用户真实数据目录**——
    /// 否则这条用例在装了运行时的机器上是空转、在没装的机器上才真跑，等于没测。
    ///
    /// 为什么这是关键路径：指针**最后写**是整套安装唯一的原子性保证。中途失败留下的
    /// 半成品目录（有 node.exe、没装成 dsh）一旦被 `resolve()` 认成"已就绪"，
    /// 界面就会放行到 spawn 才失败，用户看到的又是"启动不了且不说为什么"。
    #[test]
    fn test_resolve_requires_pointer_and_both_files() {
        let tmp = std::env::temp_dir().join(format!("nexus_rt_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("node").join("v9.9.9")).unwrap();
        std::fs::create_dir_all(tmp.join("ai").join("node_modules").join("@deepseek-ai").join("dsh").join("lib")).unwrap();

        // ① 只有目录、没有指针 → 不算就绪（这正是"安装到一半"的形态）
        assert!(resolve_in(&tmp).is_none(), "没有指针文件时不能认为运行时已就绪");

        // ② 指针在、但 node.exe 没解压出来 → 不算就绪
        std::fs::write(tmp.join("node").join("current.txt"), "v9.9.9").unwrap();
        assert!(resolve_in(&tmp).is_none(), "缺 node.exe 时不能认为运行时已就绪");

        // ③ 指针与 node.exe 都在、但 dsh 没装成 → 仍然不算就绪
        std::fs::write(tmp.join("node").join("v9.9.9").join("node.exe"), b"MZ").unwrap();
        assert!(resolve_in(&tmp).is_none(), "缺 dsh 入口时不能认为运行时已就绪");

        // ④ 三样齐全 → 就绪，且给出的必须是**绝对路径对**
        let js = tmp.join("ai").join("node_modules").join("@deepseek-ai").join("dsh").join("lib").join("bin.js");
        std::fs::write(&js, b"// dsh").unwrap();
        let rt = resolve_in(&tmp).expect("三样齐全就该就绪");
        assert_eq!(rt.node_version, "v9.9.9");
        assert!(rt.node_exe.ends_with("node.exe"), "{}", rt.node_exe);
        assert_eq!(Path::new(&rt.dsh_js), js, "dsh 入口必须指向私有目录里那一份");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// **真机跑一次**（铁律 19）：整条链——下载 36MB Node → 校验 SHA-256 → 解压 →
    /// 用私有 npm 装 269MB 的 dsh → 实跑 `dsh --version`。
    ///
    /// 为什么必须是"真跑"而不是打桩：这条链上每一步都碰外部世界（curl、镜像、
    /// zip 结构、npm 的 `--prefix` 语义、dsh 在**我们自己选的 Node 版本**上到底跑不跑得起来），
    /// 桩掉任何一步都等于把风险留到用户机器上。而 `NODE_VERSION` 那个常量唯一的门禁
    /// 就是这条用例——`import.meta.main` 缺失的 Node（20、24.0、24.1）会在这里静默变成
    /// "零输出"，所以这里断言的是**输出里有版本号**，不是"命令成功退出"。
    ///
    /// 跑法（会在临时目录里落 ~305MB，跑完自动清理；不动用户的真实运行时）：
    /// ```text
    /// cargo test --lib --manifest-path src-tauri/Cargo.toml -- --ignored real_install --nocapture
    /// ```
    #[test]
    #[ignore]
    fn test_real_install_end_to_end() {
        let tmp = std::env::temp_dir().join(format!("nexus_rt_real_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        // 隔离：`runtime_root()` 走 `data_dir()`，它认这个变量
        std::env::set_var("NEXUS_DATA_DIR", &tmp);

        let seen = std::sync::Mutex::new(Vec::<String>::new());
        let rt = install(None, &|p| {
            let line = format!("[{}] {}", STEPS[p.step_index.min(STEPS.len() - 1)], p.detail);
            let mut g = seen.lock().unwrap();
            // 只留每种状态一条，避免逐字节进度刷屏
            if g.last() != Some(&line) {
                println!("{}", line);
                g.push(line);
            }
        })
        .unwrap_or_else(|f| panic!("真实安装失败：{}", f.to_message()));

        assert_eq!(rt.node_version, NODE_VERSION);
        // 收尾验证：装完必须**实测**能跑，且输出里真有版本号
        let ver = rt.probe(None).unwrap_or_else(|e| panic!("装完却跑不起来：{}", e));
        println!("实测 dsh 版本 = {}", ver);
        assert!(
            ver.chars().next().is_some_and(|c| c.is_ascii_digit()),
            "探测到的必须是真版本号（拿不到版本号 = Node 不兼容那种静默形态）：{:?}",
            ver
        );

        // 指针已写 + 三样齐全 → resolve() 认得出来
        assert!(resolve().is_some(), "装完必须能 resolve");
        assert!(size_on_disk() > 200 * 1024 * 1024, "运行时应当在 200MB 以上（实测 {}", human_mb(size_on_disk()));

        // 卸载要**干净且不碰别处**：再 resolve 必须是 None
        uninstall().unwrap();
        assert!(resolve().is_none(), "卸载后不能再 resolve 出运行时");
        assert!(!tmp.join("runtime").exists(), "卸载后目录应当消失");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 手动指定认**三种写法**：用户没法预期 dsh 内部的目录形状，让他自己去翻
    /// `node_modules/@deepseek-ai/dsh/lib/bin.js` 是不合理的。
    #[test]
    fn test_normalize_dsh_entry_accepts_three_shapes() {
        let tmp = std::env::temp_dir().join(format!("nexus_manual_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let bin = tmp.join("node_modules").join("@deepseek-ai").join("dsh").join("lib").join("bin.js");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, b"// dsh").unwrap();

        let pkg = tmp.join("node_modules").join("@deepseek-ai").join("dsh");
        // ① 直接指到 lib\bin.js
        assert_eq!(normalize_dsh_entry(&bin.display().to_string()).unwrap(), bin);
        // ② 指到 dsh 包目录
        assert_eq!(normalize_dsh_entry(&pkg.display().to_string()).unwrap(), bin);
        // ③ 指到装了 dsh 的项目目录（会走 …/node_modules/@deepseek-ai/dsh/lib/bin.js）
        assert_eq!(normalize_dsh_entry(&tmp.display().to_string()).unwrap(), bin);

        // 不存在的路径要**说清是路径不存在**，而不是笼统的"找不到入口"
        let err = normalize_dsh_entry(&tmp.join("nope").display().to_string()).unwrap_err();
        assert!(err.contains("路径不存在"), "{}", err);
        // 空目录：确实是"目录里没有入口"，两种失败要能分开
        let empty = tmp.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let err = normalize_dsh_entry(&empty.display().to_string()).unwrap_err();
        assert!(err.contains("lib\\bin.js"), "{}", err);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 手动指定**优先**，且不做静默回落：这条是有意选的（"我明明指定了为什么不生效"
    /// 比"指定坏了没法用"更难查）。坏了要能被 `manual_runtime` 判掉、由界面给清除入口。
    #[test]
    fn test_manual_takes_priority_and_drops_when_files_vanish() {
        let tmp = std::env::temp_dir().join(format!("nexus_manual2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let node = tmp.join("node.exe");
        let bin = tmp.join("dsh").join("lib").join("bin.js");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&node, b"MZ").unwrap();
        std::fs::write(&bin, b"// dsh").unwrap();

        // 直接落盘配置文件，不走 `set_manual`——后者会**实测**跑一次，而这里的假 node.exe
        // 根本执行不了（那正是它该拦的）。本用例测的是"解析优先级"，不是"验证"。
        let cfg = ManualConfig {
            node_exe: node.display().to_string(),
            dsh_js: bin.display().to_string(),
        };
        std::fs::write(manual_config_at(&tmp), serde_json::to_string(&cfg).unwrap()).unwrap();

        // 自带那份也放一份（内容无所谓）——要证明的是"手动优先于自带"，
        // 光有手动那份的话这条用例退化成"有就返回"，证明不了优先级
        let priv_root = tmp.join("runtime");
        std::fs::create_dir_all(priv_root.join("node").join("v1")).unwrap();
        std::fs::create_dir_all(priv_root.join("ai").join("node_modules").join("@deepseek-ai").join("dsh").join("lib")).unwrap();
        std::fs::write(priv_root.join("node").join("current.txt"), "v1").unwrap();
        std::fs::write(priv_root.join("node").join("v1").join("node.exe"), b"MZ").unwrap();
        std::fs::write(
            priv_root.join("ai").join("node_modules").join("@deepseek-ai").join("dsh").join("lib").join("bin.js"),
            b"// dsh",
        ).unwrap();
        assert!(resolve_in(&priv_root).is_some(), "前提：自带那份本身是就绪的");

        let rt = resolve_rooted(&tmp).expect("设了手动指定就该优先用它");
        assert_eq!(rt.source, RuntimeSource::Manual, "手动指定必须赢过自带那份");
        assert_eq!(Path::new(&rt.node_exe), node);
        assert_eq!(Path::new(&rt.dsh_js), bin);
        assert!(rt.node_version.is_empty(), "用户那份 Node 的版本不去猜");

        // 用户把那个 Node 删了 → 不能继续指向一个不存在的解释器。
        // **注意这里会回落到自带那份**：手动优先指的是"配置存在且有效时优先"，
        // 而文件已经不在了就该按没配处理（否则用户删了目录反而彻底用不了 AI）
        std::fs::remove_file(&node).unwrap();
        assert!(manual_runtime(&tmp).is_none(), "文件没了就不该再被认成有效的手动指定");
        assert_eq!(
            resolve_rooted(&tmp).map(|r| r.source),
            Some(RuntimeSource::Private),
            "手动那份没了要能回到自带那份",
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 手动指定必须答对"这不是 node.exe"——填错一个 exe 会以完全无关的方式失败
    #[test]
    fn test_set_manual_rejects_non_node_exe() {
        let tmp = std::env::temp_dir().join(format!("nexus_manual3_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let not_node = tmp.join("python.exe");
        std::fs::write(&not_node, b"MZ").unwrap();

        let err = set_manual_at(&tmp, &not_node.display().to_string(), &tmp.display().to_string(), None)
            .unwrap_err();
        assert!(err.contains("不是 node.exe"), "{}", err);
        assert!(read_manual(&tmp).is_none(), "验证没过就不能落盘");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// **真机跑一次**（铁律 19）：拿机器上**真实**的 `node.exe` 与全局 dsh 走一遍手动指定
    /// ——保存（含当场实测）→ 解析优先级 → 清除。
    ///
    /// 为什么必须真跑：这条路上的失败全是"文件能打开但跑不起来"那种（Node 版本不兼容、
    /// dsh 是畸形安装、npm 布局不同），桩掉任何一步都测不到。而它正是内网机器的**唯一**出路，
    /// 坏在这里等于那批机器彻底没有 AI。
    ///
    /// 路径经环境变量传入（不硬编码本机路径），未设置就跳过：
    /// ```text
    /// NEXUS_TEST_NODE="…\node.exe" NEXUS_TEST_DSH="…\node_modules\@deepseek-ai\dsh" \
    ///   cargo test --lib -- --ignored real_manual --nocapture
    /// ```
    /// （数据目录指向临时目录，**不碰用户真实的 `~/.nexus/ai-runtime.json`**）
    #[test]
    #[ignore]
    fn test_real_manual_runtime_roundtrip() {
        let (Ok(node), Ok(dsh)) = (std::env::var("NEXUS_TEST_NODE"), std::env::var("NEXUS_TEST_DSH"))
        else {
            println!("未设置 NEXUS_TEST_NODE / NEXUS_TEST_DSH，跳过");
            return;
        };
        let tmp = std::env::temp_dir().join(format!("nexus_manual_real_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let rt = set_manual_at(&tmp, &node, &dsh, None)
            .unwrap_or_else(|e| panic!("手动指定保存失败（当场实测没过）：{}", e));
        println!("已保存：{} + {}（来源 {:?}）", rt.node_exe, rt.dsh_js, rt.source);
        assert_eq!(rt.source, RuntimeSource::Manual);

        // 解析要优先拿手动这份
        let got = resolve_rooted(&tmp).expect("设了手动指定就该解析得出来");
        assert_eq!(got.source, RuntimeSource::Manual);
        assert_eq!(got.node_exe, rt.node_exe);

        // 再跑一次探测，把实测到的版本号打出来（人工核对这一对确实能用）
        let ver = got.probe(None).unwrap_or_else(|e| panic!("解析出来的那份跑不起来：{}", e));
        println!("手动指定的那份实测 dsh 版本 = {}", ver);

        // 清除后不再认它（**只动临时目录里那份**：`clear_manual()` 删的是用户真实数据目录
        // 里的配置，在这条用例里调它会与上面那句"不碰真实配置"自相矛盾）
        std::fs::remove_file(manual_config_at(&tmp)).unwrap();
        assert!(manual_runtime(&tmp).is_none());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_human_mb() {
        assert_eq!(human_mb(0), "0.0 MB");
        assert_eq!(human_mb(1024 * 1024), "1.0 MB");
        assert_eq!(human_mb(NODE_ZIP_BYTES), "35.9 MB");
    }

    /// 下载源顺序：镜像是第一顺位（`nodejs.org` 在国内机器上经常直接失败）
    #[test]
    fn test_mirror_is_first_source() {
        assert!(NODE_URLS[0].contains("npmmirror"), "镜像应当排在第一个");
        assert!(NODE_URLS.iter().all(|u| u.contains(NODE_VERSION)), "所有源都要指向同一个版本");
    }

    /// 版本常量与哈希、URL 必须指向同一个版本——改一处忘了另一处是这类常量的经典事故
    #[test]
    fn test_node_version_constant_is_consistent() {
        assert!(NODE_VERSION.starts_with('v'), "目录名与 URL 都带 v 前缀");
        assert!(
            NODE_URLS.iter().all(|u| u.contains(&format!("node-{}-win-x64.zip", NODE_VERSION))),
            "URL 里的文件名必须与 NODE_VERSION 一致",
        );
    }
}
