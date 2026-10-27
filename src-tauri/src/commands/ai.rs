//! AI 助手命令层
//!
//! 会话模型：至多一个 dsh web 会话，归属一个工作目录（当前选中项目）。
//! 打开面板/切换项目 → ai_start(cwd)；目录一致且进程存活时幂等复用。
//! 竞态防护：每次 start/stop 递增 epoch，启动完成入库前复查代数，
//! 期间被更新的请求取代则终止自己，杜绝幽灵进程。
//!
//! 前端采用 pull 模型（invoke 返回值即状态），不推送事件。

use std::sync::atomic::Ordering;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::AppState;
use crate::core::ai::{AiSession, AiTimeouts};

/// 等待 dsh web 输出启动 URL 的超时（集中定义见 core/ai.rs 的 AiTimeouts）
const START_TIMEOUT: Duration = AiTimeouts::WEB_START;

/// `ai_start` 失败时的**错误码前缀**（`<码>::<人话>`）。
///
/// 为什么用前缀而不是结构化错误：Tauri 命令的 `Result<T, E>` 里 E 的形态受前端 `catch`
/// 的既有写法约束，改动要动全部调用点的错误处理；而面板真正需要区分的只有"该给哪个按钮"
/// 这一件事——运行时没准备（给「准备运行时」）还是装了跑不起来（给「重新安装」）。
pub const ERR_RUNTIME_MISSING: &str = "ai_runtime_missing";
pub const ERR_RUNTIME_BROKEN: &str = "ai_runtime_broken";

/// 会话状态快照
#[derive(Clone, Serialize)]
pub struct AiStatus {
    pub running: bool,
    /// 子 WebView 导航目标（host 已规范为 localhost，见 core::ai）
    pub url: Option<String>,
    pub pid: Option<u32>,
    pub cwd: Option<String>,
    /// 私有运行时的状态（未运行时现场探测；把"没装"与"装了跑不动"分开）
    pub runtime: crate::core::ai::DshProbe,
}

/// 组装状态快照（不持锁调用 dsh 检测）
fn snapshot(state: &AppState) -> AiStatus {
    let (running, url, pid, cwd) = match state.ai.lock() {
        Ok(mut h) => match h.as_mut() {
            Some(s) => {
                // alive() 现场探测子进程（try_wait）：只看槽里有没有会话，
                // 会把已崩溃的 dsh 一直报成"运行中"（前端永远显示会话态）
                let alive = s.alive();
                if !alive {
                    log::warn!("[ai] 会话进程已退出（pid={}），状态将报告为未运行", s.pid);
                }
                (alive, Some(s.url.clone()), Some(s.pid), s.cwd.clone())
            }
            None => (false, None, None, None),
        },
        Err(e) => {
            log::error!("[ai] 状态锁中毒: {}", e);
            (false, None, None, None)
        }
    };
    // Job Object 句柄在这里取一次传给 `core::ai`（ARCH-19）：`core` 不再认识 AppState
    let runtime = if running {
        running_runtime()
    } else {
        crate::core::ai::probe_dsh(state.process_mgr.job_arc().as_deref())
    };
    AiStatus { running, url, pid, cwd, runtime }
}

/// 会话在跑时的运行时状态：**必然就绪**（否则压根起不来），且不去实跑探测——
/// 那是一整次进程启动，而 `snapshot` 会被前端的状态轮询反复调到。
fn running_runtime() -> crate::core::ai::DshProbe {
    let rt = crate::core::runtime::resolve();
    crate::core::ai::DshProbe {
        ready: true,
        source: rt.as_ref().map(|r| r.source),
        node_version: rt.and_then(|r| (!r.node_version.is_empty()).then_some(r.node_version)),
        version: None,
        problem: None,
    }
}

/// 查询会话状态
#[tauri::command]
pub async fn ai_status(app: AppHandle) -> Result<AiStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        Ok(snapshot(&state))
    }).await.map_err(|e| format!("查询 AI 会话状态任务失败: {}", e))?
}

/// 启动（或按需重启）dsh web 会话。cwd：会话工作目录；name：项目显示名
/// （注册 dsh workspace 标题用，使 GUI 工作区名与 Nexus 项目列表一致）
#[tauri::command]
pub async fn ai_start(app: AppHandle, cwd: Option<String>, name: Option<String>) -> Result<AiStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let cwd_norm: Option<String> = cwd.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let name_norm: Option<String> = name.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

        // 目录不存在时提前失败：否则 dsh 在无效工作目录里启动，报错信息指向不明
        if let Some(dir) = cwd_norm.as_deref() {
            if !std::path::Path::new(dir).is_dir() {
                return Err(format!("项目目录不存在: {}", dir));
            }
            // 会话工作目录 = dsh agent 的工作目录（它有文件与命令工具），必须限定在已登记
            // 项目目录内：否则一次 invoke('ai_start',{cwd:'C:\\'}) 就能在任意目录起一个
            // 能读写文件、能执行命令的会话，绕过文件白名单（见 commands/paths.rs 的"配置路径收口"）
            state.paths.ensure_project_dir_allowed(&state.db, dir, "AI 会话工作目录")?;
        }

        // Job Object 句柄取一次，供本函数里所有 `core::ai` 调用共用（ARCH-19）
        let job = state.process_mgr.job_arc();

        // 运行时没准备好 / 装好了但跑不起来：**快速失败并说清是哪一种**。
        //
        // 原实现是 `if !dsh_available() { "未检测到 dsh，请先安装 npm i -g …" }`，两个洞：
        // ① 判据只看退出码，"什么都不干但 exit 0"被认成可用，于是放行到 spawn 才失败；
        // ② "没装"和"装了跑不动"给同一句文案，用户照着重装一遍仍然不行（事故里就是这样）。
        let Some(runtime) = crate::core::runtime::resolve() else {
            // 只给一句话的摘要："要下载什么、为什么、之后会怎样"由界面的错误卡负责展开
            // （它在自己的位置上画按钮与说明，后端重复一遍只会让卡片里同一句话说三遍）
            return Err(format!("{}::还没准备过，点下面的按钮下载一次即可。", ERR_RUNTIME_MISSING));
        };
        if let Err(e) = runtime.probe(job.as_deref()) {
            log::error!("[ai] 运行时实测不通过: {}", e);
            return Err(format!("{}::{}", ERR_RUNTIME_BROKEN, e));
        }

        // 幂等：运行中且目录一致且进程存活 → 复用；进程已死则重启（崩溃自愈）
        let prev: Option<AiSession> = {
            let mut h = state.ai.lock().map_err(|e| format!("获取 AI 会话锁失败: {}", e))?;
            if let Some(s) = h.as_mut() {
                if s.cwd == cwd_norm {
                    if s.alive() {
                        return Ok(AiStatus {
                            running: true,
                            url: Some(s.url.clone()),
                            pid: Some(s.pid),
                            cwd: s.cwd.clone(),
                            runtime: running_runtime(),
                        });
                    }
                    log::info!("[ai] 会话进程已退出 (pid={})，重启", s.pid);
                }
            }
            h.take()
        };
        // 本次是不是"同一个项目、上一个进程已死"的重启。
        //
        // **只有这一种情况允许跳过"会话文件已经够新鲜"的重写**：上一次 bump 的就是这个
        // 项目的会话，它必然仍是 dsh 眼里最近活动的 workspace。
        //
        // 切换项目**绝不能**用这条跳过——那时判据会比成"我自己够不够新鲜"，而不是
        // "我是不是所有项目里最新的"，于是 5 分钟内来回切项目时，赢的是**最近一次被 bump 过**
        // 的那个项目，而不是你最后点的那个（用户报的就是这个：点项目 → 点 AI 图标 →
        // 面板里的工作区是另一个项目）。「上一个会话都没有」（应用刚启动）同理：必须 bump。
        let restarting_same_project = match &prev {
            Some(s) => s.cwd == cwd_norm,
            None => false,
        };

        if let Some(mut old) = prev {
            log::info!("[ai] 会话目录变化或进程已退出，停止旧会话 (pid={})", old.pid);
            old.stop();
        }

        // 旧进程已停（无文件竞争）→ 让 dsh GUI 启动时选中本项目：
        // 注册 workspace（title=项目名）+ bump 本项目会话活动时间（recentWorkspace
        // 按会话时间选最近 —— 对有/无历史会话的项目都强制选中当前项目）。
        // 尽力而为：失败不阻塞启动
        if let Some(dir) = cwd_norm.as_deref() {
            crate::core::ai::ensure_workspace_active(dir, name_norm.as_deref(), restarting_same_project);
        }

        // 代数递增：使任何进行中的旧启动自动放弃
        let gen = state.ai_epoch.fetch_add(1, Ordering::SeqCst) + 1;

        let session = match crate::core::ai::spawn_dsh_web(&runtime, job.as_deref(), cwd_norm.as_deref(), START_TIMEOUT) {
            Ok(s) => s,
            Err(e) => {
                log::error!("[ai] 启动 dsh web 失败: {}", e);
                return Err(e);
            }
        };

        // 入库前复查代数：期间有 stop/新 start → 终止本次进程防幽灵
        let mut pending = Some(session);
        // 被本次入库顶掉的旧会话（NEW-19）：必须在**锁外** stop
        let mut displaced: Option<AiSession> = None;
        {
            let mut h = state.ai.lock().map_err(|e| format!("获取 AI 会话锁失败: {}", e))?;
            if state.ai_epoch.load(Ordering::SeqCst) == gen {
                // 槽里可能已有**并发启动的会话**：B 在本函数开头 h.take() 时槽还是空的
                // （A 尚未入库），而 B 卡在 ensure_workspace_active / spawn（慢盘可达秒级）
                // 期间 A 走完启动并落了槽。此时 B 的 gen 更大、复查通过，直接 `*h = …`
                // 会把 A 静默顶掉——而 `AiSession` 没有 Drop、其 `Child` 也不随 drop 终止，
                // A 的 dsh web 就此成为既不进表也无人停的幽灵（端口/内存长期被占，
                // 只在应用退出时由 Job Object 兜底）。
                displaced = h.take();
                *h = pending.take();
            }
        }
        if let Some(mut old) = displaced {
            log::warn!("[ai] 会话槽被覆盖，终止被取代的 dsh (pid={})", old.pid);
            old.stop();
        }
        if let Some(mut abandoned) = pending {
            log::warn!("[ai] 启动被更新的请求取代，终止本次 dsh (pid={})", abandoned.pid);
            abandoned.stop();
            return Err("已取消：会话被更新的请求取代".into());
        }

        let status = snapshot(&state);
        log::info!("[ai] dsh web 就绪 (pid={})", status.pid.unwrap_or(0));
        Ok(status)
    }).await.map_err(|e| format!("启动 AI 会话任务失败: {}", e))?
}

/// 停止当前会话（若有）：递增代数（在途启动自杀）→ 取走 → 锁外终止进程树。
/// ai_stop 与 ai_upgrade_dsh 共用（升级前必须停会话释放 Windows 文件锁）
fn stop_current_session(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    state.ai_epoch.fetch_add(1, Ordering::SeqCst);
    let old = {
        let mut h = state.ai.lock().map_err(|e| format!("获取 AI 会话锁失败: {}", e))?;
        h.take()
    };
    if let Some(mut s) = old {
        log::info!("[ai] 停止会话 (pid={})", s.pid);
        s.stop();
    }
    Ok(())
}

/// 停止会话
#[tauri::command]
pub async fn ai_stop(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || stop_current_session(&app))
        .await
        .map_err(|e| format!("停止 AI 会话任务失败: {}", e))?
}

/// dsh 版本信息（面板「检查更新」）
#[derive(Clone, Serialize)]
pub struct DshVersionInfo {
    /// dsh CLI 是否已安装（未安装时 current 为 None）
    pub dsh_found: bool,
    /// 本地安装版本（dsh --version 解析）
    pub current: Option<String>,
    /// npm registry 最新稳定版
    pub latest: Option<String>,
    /// latest > current（两者都可得时才为 true）
    pub outdated: bool,
}

/// 检查 dsh 更新：本地版本 + npm 最新版（联网，失败返回错误供前端提示）
#[tauri::command]
pub async fn ai_check_update(app: AppHandle) -> Result<DshVersionInfo, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let job = app.state::<AppState>().process_mgr.job_arc();
        let Some(rt) = crate::core::runtime::resolve() else {
            // 运行时都没准备：无需联网查，UI 直接给「准备运行时」入口
            return Ok(DshVersionInfo { dsh_found: false, current: None, latest: None, outdated: false });
        };
        let current = crate::core::ai::query_dsh_version(&rt, job.as_deref());
        let latest = crate::core::ai::query_dsh_latest(&rt, job.as_deref())?;
        let outdated = current.as_ref()
            .map(|c| crate::core::ai::version_cmp(c, &latest) == Some(std::cmp::Ordering::Less))
            .unwrap_or(false);
        Ok(DshVersionInfo { dsh_found: true, current, latest: Some(latest), outdated })
    })
    .await
    .map_err(|e| format!("检查 dsh 更新任务失败: {}", e))?
}

/// 升级/安装 dsh 到最新版。先停当前会话（Windows 文件锁，npm 无法覆盖运行中文件）。
///
/// 走**私有运行时自己的 npm**、装进**私有目录**，并**装完实跑一次确认**
/// （原因见 `core::runtime::install_or_upgrade_dsh`）。
#[tauri::command]
pub async fn ai_upgrade_dsh(app: AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let job = app.state::<AppState>().process_mgr.job_arc();
        let Some(rt) = crate::core::runtime::resolve() else {
            return Err(format!(
                "{}::AI 运行时还没准备好，请先在「设置 > AI 运行时」里准备",
                ERR_RUNTIME_MISSING
            ));
        };
        // 先停会话再动 dsh 的文件（Windows 文件锁）
        stop_current_session(&app)?;
        crate::core::runtime::install_or_upgrade_dsh(&rt, job.as_deref())
            .map_err(|f| format!("{}::{}", ERR_RUNTIME_BROKEN, f.to_message()))
    })
    .await
    .map_err(|e| format!("升级 dsh 任务失败: {}", e))?
}

// ─── 私有运行时（设置面板 + 错误卡的入口） ─────────────────────

/// 运行时状态（设置面板展示 + 错误卡判断该给哪个按钮）
#[derive(Clone, Serialize)]
pub struct AiRuntimeStatus {
    /// 目录与指针齐全
    pub ready: bool,
    /// 这一对路径来自哪里（`manual` = 用户手动指定）
    pub source: Option<crate::core::runtime::RuntimeSource>,
    /// 自带 Node 的版本（如 `v24.21.0`；手动指定时为空）
    pub node_version: Option<String>,
    /// 实测到的 dsh 版本（跑得起来时才有）
    pub dsh_version: Option<String>,
    /// 装了但跑不起来时的原因（人话 + 原话）
    pub problem: Option<String>,
    /// 占用磁盘字节数（"卸载能腾出多少"）
    pub size_bytes: u64,
    /// 当前生效的那对路径（出问题时能告诉用户它在哪）
    pub path: String,
    /// 自动装的那份的安装目录（与 `path` 不同：手动指定时 path 指向别处，而
    /// "卸载"删的始终是这一个）
    pub install_dir: String,
    /// 是否设了手动指定（界面据此显示「清除」入口）
    pub has_manual: bool,
    /// 安装步骤名（界面据此画步骤条，不必自己维护一份字符串）
    pub steps: Vec<String>,
}

fn runtime_status(job: Option<&crate::core::job_object::JobObject>) -> AiRuntimeStatus {
    let probe = crate::core::ai::probe_dsh(job);
    // 展示用的路径：手动指定时给用户看他指定的那对，否则给自动装的那份
    let path = crate::core::runtime::resolve()
        .map(|r| format!("{} + {}", r.node_exe, r.dsh_js))
        .unwrap_or_else(|| crate::core::runtime::runtime_root().display().to_string());
    AiRuntimeStatus {
        ready: probe.ready,
        source: probe.source,
        node_version: probe.node_version,
        dsh_version: probe.version,
        problem: probe.problem,
        size_bytes: crate::core::runtime::size_on_disk(),
        path,
        install_dir: crate::core::runtime::runtime_root().display().to_string(),
        has_manual: crate::core::runtime::has_manual(),
        steps: crate::core::runtime::STEPS.iter().map(|s| s.to_string()).collect(),
    }
}

/// 查询运行时状态（**不实跑**以外的开销很小；设置面板进入时调一次）
#[tauri::command]
pub async fn ai_runtime_status(app: AppHandle) -> Result<AiRuntimeStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let job = app.state::<AppState>().process_mgr.job_arc();
        Ok(runtime_status(job.as_deref()))
    })
    .await
    .map_err(|e| format!("查询 AI 运行时状态失败: {}", e))?
}

/// 安装 / 重新安装 AI 运行时（下载 Node + 装 dsh，首次约 300MB）。
///
/// **先停 AI 会话**：正在跑的 dsh 就是私有 `node.exe`，不先停就删不掉/覆盖不了旧目录
/// （Windows 文件锁）。进度经 `ai-runtime-progress` 事件推送。
#[tauri::command]
pub async fn ai_runtime_install(
    app: AppHandle,
) -> Result<AiRuntimeStatus, crate::core::runtime::RuntimeFailure> {
    tauri::async_runtime::spawn_blocking(move || {
        let Some(_slot) = crate::core::runtime::try_acquire_install() else {
            return Err(crate::core::runtime::RuntimeFailure::busy());
        };
        stop_current_session(&app)
            .map_err(|e| crate::core::runtime::RuntimeFailure::internal(format!("先停会话失败: {}", e)))?;
        let job = app.state::<AppState>().process_mgr.job_arc();

        let emitter = app.clone();
        let result = crate::core::runtime::install(job.as_deref(), &|p| {
            // 进度推送失败不影响安装本身（前端没监听就没人收），但必须留痕
            if let Err(e) = emitter.emit("ai-runtime-progress", p) {
                log::warn!("[ai] 推运行时进度失败: {}", e);
            }
        });
        let rt = result?;
        log::info!("[ai] 私有运行时就绪: node={} dsh={}", rt.node_version, rt.dsh_js);
        Ok(runtime_status(job.as_deref()))
    })
    .await
    .unwrap_or_else(|e| Err(crate::core::runtime::RuntimeFailure::internal(e.to_string())))
}

/// 卸载 AI 运行时（删掉 `~/.nexus/runtime`，腾出约 300MB）。
///
/// **只删运行时自己的目录**——`~/.dsh` 一个字节都不碰（那里是登录凭据、会话历史、
/// 浏览器 profile，删了等于让用户重新登录并丢掉全部会话）。
#[tauri::command]
pub async fn ai_runtime_uninstall(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        // 正在跑的 dsh 就是私有 node.exe：不先停就是 EBUSY（删不掉）
        stop_current_session(&app)?;
        crate::core::runtime::uninstall()
    })
    .await
    .map_err(|e| format!("卸载 AI 运行时任务失败: {}", e))?
}

/// **手动指定**运行时（A 方案）：填 `node.exe` 与 dsh 的路径，绕开下载。
///
/// 给两类机器用：① 内网/下载不通，拿不到那 300MB；② 机器上本来就有可用的 Node + dsh。
/// 保存前会**当场实测一次**（`dsh --version` 要吐出真版本号）——设一个跑不起来的路径
/// 等于把 AI 设死，而"保存成功了却不能用"是最难查的那种失败。
#[tauri::command]
pub async fn ai_runtime_set_manual(
    app: AppHandle,
    node_exe: String,
    dsh_path: String,
) -> Result<AiRuntimeStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let job = app.state::<AppState>().process_mgr.job_arc();
        // 换解释器要先停会话：旧会话的 node.exe 还占着，而且留着它会让"换没换成功"看不出来
        stop_current_session(&app)?;
        crate::core::runtime::set_manual(&node_exe, &dsh_path, job.as_deref())?;
        Ok(runtime_status(job.as_deref()))
    })
    .await
    .map_err(|e| format!("保存手动指定的运行时失败: {}", e))?
}

/// 清除手动指定，回到用 Nexus 自带的那份
#[tauri::command]
pub async fn ai_runtime_clear_manual(app: AppHandle) -> Result<AiRuntimeStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let job = app.state::<AppState>().process_mgr.job_arc();
        stop_current_session(&app)?;
        crate::core::runtime::clear_manual()?;
        Ok(runtime_status(job.as_deref()))
    })
    .await
    .map_err(|e| format!("清除手动指定的运行时失败: {}", e))?
}

/// 创建 AI 面板的内嵌子 WebView（带文档创建时注入的焦点记忆/恢复脚本）。
///
/// 与 JS 侧 `new Webview(...)`（plugin:webview|create_webview，unstable）等价，
/// 额外用 initialization_script 安装页面内焦点恢复器（scripts/ai_focus_restore.js）。
/// 为什么必须 Rust 侧：@tauri-apps/api 的 Webview 类没有 eval/initializationScript
/// 入口，注入只能走 Rust。
///
/// 参数较多是 IPC 契约决定的：这些字段由前端 `invoke` 逐个传入，合并成结构体会同时改变
/// 前端调用形状（属界面契约变更）。逐项豁免并说明理由，而不是全局关闭该 lint。
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn create_ai_panel_webview(
    app: AppHandle,
    window_label: String,
    label: String,
    url: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    // 当前项目目录：据此给 dsh 预置"上次会话"记忆（见 core::ai::latest_session_id）。
    // None = 不预置（未选项目/项目还没有会话）。
    project_cwd: Option<String>,
    // true = 恢复上次会话；false（默认）= 每次打开都新建会话（见 build_panel_init_script）
    resume_session: bool,
) -> Result<(), String> {
    let window = app
        .get_window(&window_label)
        .ok_or_else(|| format!("窗口不存在: {}", window_label))?;
    // URL 白名单：本命令在内嵌浏览器里加载任意 URL——不校验等于把"能渲染任意网页的
    // 窗口"开放给调用方（钓鱼/本地服务探测）。用 tauri::Url 解析而非字符串前缀匹配：
    // 前缀匹配挡不住 `http://localhost.evil.com`、`@` 用户信息段等写法。
    //
    // 回显一律走 `url_hint`（SEC-27）：这个 URL 带 dsh 的一次性 token（`?token=…`），
    // 而错误串会被渲染到 AI 面板上——用户截图或贴 issue 时就顺手把它带出去了
    let parsed: tauri::Url = url.parse().map_err(|e| format!("URL 解析失败 ({}): {}", url_hint(&url), e))?;
    let host_ok = matches!(parsed.host_str(), Some("localhost") | Some("127.0.0.1") | Some("::1"));
    if !host_ok || !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!("AI 面板 URL 不允许: {}", url_hint(&url)));
    }
    let url = tauri::WebviewUrl::External(parsed);
    // 几何值来自前端 invoke 参数：非有限值（NaN/±inf）与负坐标会让 WebView 创建失败
    // 或落在屏幕外；尺寸下限 1px（0 尺寸同样创建失败）
    let x = if x.is_finite() { x.max(0.0) } else { 0.0 };
    let y = if y.is_finite() { y.max(0.0) } else { 0.0 };
    let width = if width.is_finite() { width.max(1.0) } else { 1.0 };
    let height = if height.is_finite() { height.max(1.0) } else { 1.0 };

    // 首次在新 origin 上打开：预置 dsh 的"当前会话"记忆。
    //
    // 稳定端口（core::ai::stable_port_for_project）保证同一项目每次同一个 origin，
    // 但**升级后的第一次启动**这个 origin 还是空的——dsh 会新建空白会话，而用户想看
    // 的是已有的上次会话。写 localStorage 必须赶在 dsh 页面脚本之前，所以放进
    // initialization_script；只在键不存在时写，之后仍由 dsh 自己维护。
    // 「新建会话」模式不需要扫描会话目录（也就不用为一次打开付扫盘成本）
    let latest_session = match project_cwd.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        Some(cwd) if resume_session => {
            let cwd = cwd.to_string();
            tauri::async_runtime::spawn_blocking(move || crate::core::ai::latest_session_id(&cwd))
                .await
                .ok()
                .flatten()
        }
        _ => None,
    };
    let init_script = crate::core::ai::build_panel_init_script(
        include_str!("../scripts/ai_focus_restore.js"),
        latest_session.as_deref(),
        resume_session,
    );

    let builder = tauri::webview::WebviewBuilder::new(label, url)
        // HTML5 drag and drop（与 JS 创建时 dragDropEnabled: false 同义）
        .disable_drag_drop_handler()
        // 导航守卫：面板只允许停留在环回 http(s) 上。
        // 只校验创建前的 URL 是不够的——加载后页面里的链接、重定向与 JS 导航可以把面板
        // 带到任意 URL：应用框架内的界面伪装，或指向本机其它环回端口（含 Nexus 自己的 GUI）。
        // 保留 about:blank（部分引擎初始化时会先导航到它，拦掉会让面板白屏）。
        .on_navigation(|url| {
            if url.scheme() == "about" {
                return true;
            }
            let host_ok = matches!(url.host_str(), Some("localhost") | Some("127.0.0.1") | Some("::1"));
            let scheme_ok = matches!(url.scheme(), "http" | "https");
            if !(host_ok && scheme_ok) {
                log::warn!("[ai] 面板导航已拦截（非环回 http/https）: {}", url);
            }
            host_ok && scheme_ok
        })
        // 文档创建时注入焦点记忆/恢复脚本 + 首次"上次会话"预置（每次导航重新执行，幂等）
        .initialization_script(&init_script)
        // 屏蔽 dsh 页面自带的右键菜单：主界面的全局禁止（src/main.tsx）作用于主文档，
        // 覆盖不到本子 WebView。用 for_all_frames：iframe 内的右键弹的是该帧自己的
        // 菜单，父文档的 contextmenu 监听收不到（Windows 上 wry 本就注入所有帧，
        // 这里只是把各平台语义对齐）
        .initialization_script_for_all_frames(include_str!("../scripts/ai_disable_context_menu.js"));
    window
        .add_child(
            builder,
            tauri::LogicalPosition::new(x, y),
            tauri::LogicalSize::new(width, height),
        )
        .map_err(|e| format!("创建 AI 面板 WebView 失败: {}", e))?;
    Ok(())
}

/// Alt+Tab 切回后把系统键盘焦点交给 AI 面板子 WebView。
///
/// 为什么不用 tauri/wry 的 set_focus：其 Windows 底层是
/// CoreWebView2Controller::MoveFocus(Programmatic)——语义是「页面内 Tab 焦点循环
/// 移动」而非「交键盘焦点给控件」，实测破坏输入。正确做法：定位 WebView2 控件
/// 窗口（窗口类 "Chrome_WidgetWin_0"，探测确认与面板槽位矩形一致、属于应用进程
/// 主线程，SetFocus 合法）后调用 Win32 SetFocus。控件获得焦点后页面触发
/// window focus 事件 → 注入脚本自动恢复输入框（双保险）。
///
/// 非 Windows 平台无对应实现：命令仍在注册表中（保持跨平台可编译），调用返回错误。
#[tauri::command]
pub async fn ai_panel_focus(app: AppHandle, window_label: String, label: String) -> Result<(), String> {
    // 命令本体不带 cfg：lib.rs 的 generate_handler! 无条件注册本命令，
    // 函数被 cfg 掉会让非 Windows 目标整体编译失败。平台分支放在函数体内。
    #[cfg(windows)]
    {
        let window = app
            .get_window(&window_label)
            .ok_or_else(|| format!("窗口不存在: {}", window_label))?;
        let webview = app.get_webview(&label).ok_or_else(|| format!("AI 面板 WebView 不存在: {}", label))?;
        let w2 = window.clone();
        let wv2 = webview.clone();
        // run_on_main_thread：EnumChildWindows/SetFocus 的窗口属于 UI 线程，
        // SetFocus 要求调用线程与目标窗口同线程（或附加输入队列）——必须在主线程执行
        window
            .run_on_main_thread(move || {
                let res = do_panel_focus(&w2, &wv2);
                match &res {
                    Ok(()) => log::info!("[ai] 系统焦点已交给 dsh 面板"),
                    Err(e) => log::warn!("[ai] 恢复 dsh 系统焦点失败: {}", e),
                }
            })
            .map_err(|e| format!("主线程调度失败: {}", e))
    }
    #[cfg(not(windows))]
    {
        // 其他平台没有 WebView2 控件窗口，也就没有 SetFocus 的等价做法
        let _ = (app, window_label, label);
        Err("系统焦点恢复仅 Windows 支持".into())
    }
}

#[cfg(windows)]
fn do_panel_focus(window: &tauri::Window, webview: &tauri::Webview) -> Result<(), String> {
    unsafe {
        // 1. 主窗口句柄 + 客户区屏幕原点（webview 的 position 相对客户区）
        let handle = match window.hwnd() {
            Ok(h) => h,
            Err(e) => return Err(format!("获取主窗口句柄失败: {}", e)),
        };
        let hwnd = handle.0 as isize;
        let mut pt = Point { x: 0, y: 0 };
        ClientToScreen(hwnd, &mut pt);

        let wpos = webview
            .position()
            .map_err(|e| format!("获取 WebView 位置失败: {}", e))?;
        let wsize = webview
            .size()
            .map_err(|e| format!("获取 WebView 尺寸失败: {}", e))?;
        let target = (
            pt.x + wpos.x,
            pt.y + wpos.y,
            pt.x + wpos.x + wsize.width as i32,
            pt.y + wpos.y + wsize.height as i32,
        );

        // 2. 枚举主窗口子窗口，按矩形精确匹配 WebView2 控件窗口（±2px 容差）
        let mut ctx = EnumCtx { target, found: 0 };
        EnumChildWindows(hwnd, Some(enum_proc), &mut ctx as *mut _ as isize);
        if ctx.found == 0 {
            return Err(format!("未找到 AI 面板 WebView 控件窗口 (target={:?})", target));
        }

        // 3. 键盘焦点交给控件（同线程队列，合法且是标准做法）
        let _prev = SetFocus(ctx.found);
        log::debug!("[ai] SetFocus hwnd={} (prev focus={:?})", ctx.found, _prev);
        Ok(())
    }
}

/// WebView2 控件窗口类名（Windows 内部实现细节，类名多年稳定）
#[cfg(windows)]
const WEBVIEW2_CONTROL_CLASS: &str = "Chrome_WidgetWin_0";

#[cfg(windows)]
#[repr(C)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[cfg(windows)]
#[repr(C)]
struct Point {
    x: i32,
    y: i32,
}

#[cfg(windows)]
struct EnumCtx {
    target: (i32, i32, i32, i32),
    found: isize,
}

#[cfg(windows)]
unsafe extern "system" fn enum_proc(hwnd: isize, l_param: isize) -> i32 {
    let ctx = &mut *(l_param as *mut EnumCtx);
    let mut buf = [0u16; 64];
    let n = GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
    if n <= 0 {
        return 1;
    }
    let name = String::from_utf16_lossy(&buf[..n as usize]);
    if name != WEBVIEW2_CONTROL_CLASS {
        return 1;
    }
    let mut r = Rect { left: 0, top: 0, right: 0, bottom: 0 };
    if GetWindowRect(hwnd, &mut r) == 0 {
        return 1;
    }
    let (tl, tt, tr, tb) = ctx.target;
    if (r.left - tl).abs() <= 2
        && (r.top - tt).abs() <= 2
        && (r.right - tr).abs() <= 2
        && (r.bottom - tb).abs() <= 2
    {
        ctx.found = hwnd;
    }
    1
}

/// 从原始 URL 串里取出**可安全回显**的部分：`scheme://host`，路径与查询串一律丢掉。
///
/// 存在理由（SEC-27）：AI 面板的 URL 带 dsh 的一次性 token（`?token=…`），而这个命令的
/// 错误串会被渲染到面板上（`PanelMessage detail=…`）——用户截图、贴 issue 时就把凭据带
/// 出去了。定位问题只需要 scheme 与 host，`user:pass@host` 这种写法也一并切掉。
/// 连 `://` 都没有的串不回显原文：它可能整串就是凭据。
fn url_hint(raw: &str) -> String {
    let Some((scheme, rest)) = raw.trim().split_once("://") else {
        return "(无法识别为 URL)".to_string();
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or(host);
    format!("{}://{}", scheme, host)
}

#[cfg(windows)]
extern "system" {
    fn EnumChildWindows(
        hwnd_parent: isize,
        lp_enum_func: Option<unsafe extern "system" fn(isize, isize) -> i32>,
        l_param: isize,
    ) -> i32;
    fn GetClassNameW(hwnd: isize, lp_class_name: *mut u16, n_max_count: i32) -> i32;
    fn GetWindowRect(hwnd: isize, lp_rect: *mut Rect) -> i32;
    fn ClientToScreen(hwnd: isize, lp_point: *mut Point) -> i32;
    fn SetFocus(hwnd: isize) -> isize;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SEC-27：错误串里**不能**出现 token。
    ///
    /// 这条守的是"用户截图/贴 issue 时不顺带泄露凭据"——面板 URL 里 `?token=…` 是一次性的，
    /// 但一次性不等于泄露无害（它同时是 dsh 的会话凭据）。改动若把 `url_hint` 换回原串，
    /// 最后一条断言会红。
    #[test]
    fn test_url_hint_drops_token_and_credentials() {
        let real = "http://127.0.0.1:49152/?token=abc123secret";
        assert_eq!(url_hint(real), "http://127.0.0.1:49152");
        assert!(!url_hint(real).contains("abc123secret"));
        // 用户信息段（user:pass@host）同样不进提示
        assert_eq!(url_hint("https://user:pw@example.com/x?y=1"), "https://example.com");
        // 路径/片段/查询只按第一个分隔符切
        assert_eq!(url_hint("http://localhost:8080#frag"), "http://localhost:8080");
        // 解析不出 scheme：不回显原文（它可能整串就是凭据）
        assert_eq!(url_hint("token=abc123secret"), "(无法识别为 URL)");
    }
}
