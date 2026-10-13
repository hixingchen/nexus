use std::collections::HashMap;
use std::process::{Command, Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use serde::Serialize;


// ─── 已拆分的模块（ARCH-23）─────────────────────────────────
//
// 代码**原样**搬到了各自的模块，这里只做重导出：外部调用点仍可按 `core::process::xxx`
// 的原路径使用（`pub` 的按原样公开，`pub(crate)` 的按原样 crate 内可见），一处未改。
pub use super::envparse::parse_env_vars;
pub use super::logpipe::{LogLine, ServiceLogBatchPayload};
pub use super::logtext::truncate_line;
pub use super::redact::mask_command;
pub(crate) use super::logpipe::{spawn_log_flush_thread, LogSignal};
pub(crate) use super::logtext::read_log_lines;

// 本模块自身用到的（路径随拆分变为 core::logfollow / core::logpipe）
use super::logfollow::{system_time_from_millis, LogFollow, LogProbe};
use super::logpipe::{append_system_line, spawn_log_reader, LogBuffers, LogKey};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(unix)]
use std::os::unix::process::CommandExt as UnixCommandExt;

/// 全局最大并发服务数，防止日志缓冲无限增长
const MAX_SERVICES: usize = 50;

// ─── Types ──────────────────────────────────────────────────

/// 意外退出的服务（崩溃/秒退/spawn 失败）：前端据此在卡片上显示"失败"按钮。
/// 与"主动停止"区分：主动停止（stop）会清除该状态且清空日志
#[derive(Clone, Serialize, Debug)]
pub struct FailedService {
    pub service_id: String,
    /// spawn 失败（进程未启动）时为 None
    pub exit_code: Option<i32>,
    pub timestamp: String,
}

/// 取生命周期锁（NEW-17）。锁中毒同样继续用：进程生命周期离硬件更近，
/// 让一次无关的 panic 变成"该服务永远起不来"是更坏的结局。
pub(crate) fn lock_lifecycle<'a>(lock: &'a Arc<Mutex<()>>, key: &str) -> std::sync::MutexGuard<'a, ()> {
    lock.lock().unwrap_or_else(|e| {
        log::error!("[nexus] 服务 {} 的生命周期锁已中毒，继续使用: {}", key, e);
        e.into_inner()
    })
}

/// 认领到的游离进程：服务"另开窗口"跑起来的那个进程。
///
/// 为什么要有它：`cmd /C startup.bat` 里 catalina.bat 用的是 cmd 内建命令 `start`，
/// 它给子进程**新分配一个控制台**（CreateProcess + CREATE_NEW_CONSOLE）。后果有三：
/// ① 那个进程的输出不进我们的管道（面板看不到，日志只能另找来源）；
/// ② cmd 不等待它就退出 → 面板随即显示"已退出（退出码 0）"，是**谎报**；
/// ③ 点"停止"时进程表里已经没有这个服务了，是空操作 —— Tomcat 会一直跑下去。
/// 认领（按父 pid 链走查 + 启动时刻校验，见 `super::winproc`）之后，服务继续算运行中，
/// 停止能连同它一起结束，"停止"与"运行状态"两个语义都恢复正确。
struct AdoptedChild {
    pid: u32,
    /// 映像名（`java.exe` 等），只用于日志里说清接管了谁
    name: String,
    /// 句柄：判存活与取退出码都不受 pid 复用影响（见 `winproc::OwnedProcess`）
    handle: super::winproc::OwnedProcess,
}

/// 进程清理条目：停止 / 退出时交给 `cleanup_process` 的全部信息。
///
/// 原为 6 元组；加上认领信息、且 `child` 变成 `Option` 之后，位置已经读不出含义，
/// 按项目标准（参数 >4 个用结构体，见 `ServiceSpawn`）改为具名字段。
struct CleanupEntry {
    key: String,
    pid: u32,
    /// 直接子进程的启动时刻（ms）：收游离后代时用它排除 pid 复用
    started_at: u64,
    /// 直接子进程；已被认领路径回收（`wait` 过）时为 None
    child: Option<Child>,
    /// 认领到的游离进程：停止时与直接子进程一起结束
    adopted: Vec<AdoptedChild>,
    /// 正在跟随的日志文件：停止时一并收掉（否则线程会往已清空的缓冲里写）
    follow: Option<LogFollow>,
    done_stdout: Option<Receiver<()>>,
    done_stderr: Option<Receiver<()>>,
    stop_flag: Option<Arc<AtomicBool>>,
}

// ─── ProcessManager ─────────────────────────────────────────

/// 服务进程表的一条记录。
///
/// `pub(crate)`（ARCH-23）：`core::logfollow` 里的 `impl ProcessManager`（跟随相关方法）
/// 与 `core::process` 同属一张表的读写方，字段可见性按它们用到的部分放开。
pub(crate) struct ProcessInfo {
    /// 直接子进程（cmd / sh）。认领游离进程后置 None（它已退出并被回收）
    child: Option<Child>,
    pid: u32,
    /// 直接子进程的启动时刻（ms）。认领游离进程时用它排除 pid 复用：
    /// 系统会把旧 pid 分给无关进程，"父 pid 恰好指向我们"这件事本身不足以下结论
    spawn_started_at: u64,
    /// 认领到的游离进程（见 `AdoptedChild`）
    adopted: Vec<AdoptedChild>,
    /// 工作目录：认领后用它去找服务在写的日志文件（见 `core::logfile`）
    cwd: String,
    /// 正在跟随的日志文件（见 `LogFollow`）
    pub(crate) follow: Option<LogFollow>,
    /// 认领后找日志文件的采样状态（见 `LogProbe`）；None = 还没采过样
    pub(crate) log_probe: Option<LogProbe>,
    /// 所属项目（供 get_running 返回项目级运行状态）
    project_id: String,
    /// 日志批量发射端（启动时传入）。跟随日志文件要按需另起发射线程，用它
    pub(crate) log_sink: LogSink,
    /// reader 线程结束信号（stdout/stderr）
    done_stdout: Option<Receiver<()>>,
    done_stderr: Option<Receiver<()>>,
    /// reader 线程停止标志：清理时置位，令旧线程不再写缓冲/emit。
    /// 否则 taskkill 未杀净（孙进程持管道）时，旧进程输出会串进新服务的日志，
    /// 且"停止后日志还在涨"，与已停止状态矛盾
    pub(crate) stop_flag: Arc<AtomicBool>,
}

pub struct ProcessManager {
    pub(crate) processes: Mutex<HashMap<String, ProcessInfo>>,
    pub(crate) log_buffers: LogBuffers,
    /// 意外退出的服务：key → 失败信息。日志保留供查看（正常停止/重启/全部停止时清除）
    failed: Mutex<HashMap<String, FailedService>>,
    /// 每 key 一把生命周期锁，让 start/stop/restart 对同一服务串行（NEW-17）。
    /// 键只增不删，理由见 `lifecycle_guard`。
    lifecycles: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    #[cfg(windows)]
    job: Option<Arc<super::job_object::JobObject>>,
}

/// 启动/重启服务所需的参数集合
///
/// 原为 7 个位置参数，调用处容易串位（project_id/service_id/name/command 都是 &str）。
/// 打包成结构体后语义自解释，也顺带满足"参数 ≤4 个"的项目标准。
pub struct ServiceSpawn<'a> {
    pub project_id: &'a str,
    /// 服务 id（同时是进程表 key）
    pub service_id: &'a str,
    pub name: &'a str,
    pub command: &'a str,
    pub cwd: &'a str,
    pub env_vars: &'a [(String, String)],
    pub log_sink: LogSink,
}

/// 日志批量事件的发送端（ARCH-14）。
///
/// 为什么不是 `tauri::AppHandle`：`core/` 反向依赖 IPC 框架会让 `ProcessManager::start`
/// 在**结构上**不可测——而它恰恰是唯一真的 spawn 子进程、可能留下孤儿、含 TOCTOU 清理分支
/// 的函数（24 个测试无一调用它）。换成"收一批日志，送去哪里由调用方决定"之后，
/// 测试可以塞一个 no-op 或记录用的 sink，直接驱动 start/stop 的完整生命周期。
///
/// 用 `Arc` 而不是借用：批量发射线程要求 `'static`（`thread::spawn` 的硬约束），
/// 借用进去编译不过；`Arc` 克隆进线程即可。
pub type LogSink = Arc<dyn Fn(ServiceLogBatchPayload) + Send + Sync>;

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            processes: Mutex::new(HashMap::new()),
            log_buffers: Arc::new(Mutex::new(HashMap::new())),
            failed: Mutex::new(HashMap::new()),
            lifecycles: Mutex::new(HashMap::new()),
            #[cfg(windows)]
            job: None,
        }
    }

    /// 测试用：带 Job Object 的实例。
    ///
    /// 存在的理由只有一个：`job: Some(_)` 会让服务**以挂起态创建**（CQ-27①），
    /// 而这条路径用 `new()` 的实例根本走不到——没有它，"纳管后能否唤醒"就没有任何覆盖。
    #[cfg(all(test, windows))]
    pub(crate) fn with_job(job: Arc<super::job_object::JobObject>) -> Self {
        let mut mgr = Self::new();
        mgr.job = Some(job);
        mgr
    }

    /// 取某 key 的生命周期锁（不存在则建）。
    ///
    /// 为什么必须有（NEW-17）：`start` 在"检查进程表"与"插入进程表"之间**必须释放锁**
    /// （spawn 不能持锁做）。服务 X 处于这个窗口时，`stop(X)` 查表为空 → 静默 no-op 并
    /// 返回 `Ok(())` → 用户收到"所有服务已停止"，而 X 随后被插表并**真实运行**。
    /// 加锁后 start/stop/restart 对同一 key 串行，窗口不再存在。
    ///
    /// 锁的获取顺序固定为 `lifecycles → processes`（本函数取完就释放，不与 processes 嵌套），
    /// `stop_all` 也不走 `stop`（直接 drain 表再清理），故不会出现反向持有。
    ///
    /// 表项**不回收**：删除表项会让"并发中的 start 握着旧 Arc、新来的 stop 拿到新 Arc"，
    /// 互斥直接失效。代价是每个曾启停过的服务 id 留一条 `String + Arc`——量级可忽略。
    pub(crate) fn lifecycle_lock(&self, key: &str) -> Arc<Mutex<()>> {
        let mut m = self.lifecycles.lock().unwrap_or_else(|e| {
            log::error!("[nexus] lifecycles 锁已中毒，继续使用: {}", e);
            e.into_inner()
        });
        Arc::clone(m.entry(key.to_string()).or_default())
    }

    /// 设置共享的 Job Object（确保子进程在应用退出时被终止）
    #[cfg(windows)]
    pub fn set_job(&mut self, job: Arc<super::job_object::JobObject>) {
        self.job = Some(job);
    }

    /// 返回共享 Job Object 句柄（供外部启动的子进程加入同一清理域）
    #[cfg(windows)]
    pub fn job_arc(&self) -> Option<Arc<super::job_object::JobObject>> {
        self.job.clone()
    }

    /// 启动单个服务。
    ///
    /// 全程持有该 key 的**生命周期锁**（NEW-17）——见 `lifecycle_guard` 的说明。
    pub fn start(&self, spec: ServiceSpawn<'_>) -> Result<(), String> {
        // 锁的 `Arc` 必须持有到函数结束：guard 借用的是它（不是堆上的 Mutex）
        let lock = self.lifecycle_lock(spec.service_id);
        let _guard = lock_lifecycle(&lock, spec.service_id);
        self.start_locked(spec)
    }

    fn start_locked(&self, spec: ServiceSpawn<'_>) -> Result<(), String> {
        let ServiceSpawn { project_id, service_id: key, name, command, cwd, env_vars, log_sink } = spec;
        log::info!("[nexus] 启动服务: {} ({}) cmd={:?}, cwd={:?}", key, name, mask_command(command), cwd);

        // 工作目录为空：进程可能依赖相对路径/环境变量，直接拒绝并提示配置
        // （错误不带服务名，由调用方按入口包装，避免批量启动时与"名称:"前缀重复）
        if cwd.trim().is_empty() {
            return Err("工作目录为空，无法启动".into());
        }

        // Phase 1: 检查限制，然后释放锁
        // 锁中毒用 into_inner 恢复：否则一次 panic 会让服务"既停不掉也起不来"（锁中毒后
        // start/stop 直接上抛错误），进程表项永久残留
        {
            let procs = self.processes.lock().unwrap_or_else(|e| {
                log::error!("[nexus] processes 锁已中毒，继续使用: {}", e);
                e.into_inner()
            });
            if procs.contains_key(key) {
                return Err(format!("{} 已在运行中", key));
            }
            if procs.len() >= MAX_SERVICES {
                return Err(format!("已达到最大并发服务数 ({})，请先停止其他服务", MAX_SERVICES));
            }
        }

        if let Ok(mut buffers) = self.log_buffers.lock() {
            buffers.remove(key);
        }
        // 重新启动：清除旧失败标记（新生命周期）
        if let Ok(mut failed) = self.failed.lock() {
            failed.remove(key);
        }

        // Phase 2: 在锁外执行 spawn 和线程创建
        let mut cmd = build_command(command);
        cmd.current_dir(cwd); // cwd 非空已在上方校验（原 `if !cwd.is_empty()` 是不可达分支）
        if !env_vars.is_empty() {
            cmd.envs(env_vars.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        }

        // 有 Job Object 时**挂起创建**（CQ-27①）：纳管与唤醒都在下面按顺序做。
        // `creation_flags` 是设置不是追加，所以这里必须用与 build_command 同一组常量
        #[cfg(windows)]
        if self.job.is_some() {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(SERVICE_CREATE_FLAGS | CREATE_SUSPENDED);
        }

        let mut child = cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                // spawn 失败（cwd 无效、命令不存在等）：写入日志缓冲 + 记失败状态，
                // 卡片显示"失败"按钮，点击日志面板可见原因
                let msg = format!("启动失败: {}", e);
                self.append_system_line(key, msg.clone());
                if let Ok(mut failed) = self.failed.lock() {
                    failed.insert(key.to_string(), FailedService {
                        service_id: key.to_string(),
                        exit_code: None,
                        timestamp: chrono::Utc::now().to_rfc3339(),
                    });
                }
                msg
            })?;

        #[cfg(windows)]
        if let Some(ref job) = self.job {
            // 纳管失败**必须让用户看见**（CQ-27）：它只在"Nexus 被强杀/崩溃"时才有后果
            // （该服务的进程会残留），而那正是用户最需要线索的时刻——只写进 nexus.log
            // 等于没有。这里写进服务自己的日志缓冲，日志面板里直接看得到
            if let Err(e) = job.assign_child(&child) {
                self.append_system_line(key, format!(
                    "⚠ 未能纳入 Job Object：{} —— Nexus 异常退出（崩溃/被杀）时该服务的进程可能残留，正常退出不受影响",
                    e
                ));
            }
            // 纳管之后再放行（顺序不能反：先唤醒就等于回到"spawn 之后再绑定"的老样子）。
            // 唤醒失败是**致命**的——进程挂在创建态，服务永远起不来，必须杀掉并如实报错，
            // 否则用户看到的是"启动了但一行输出都没有"这种查不下去的状态
            if let Err(e) = resume_suspended_child(child.id()) {
                kill_and_reap(&mut child, Duration::from_millis(2000));
                let msg = format!("启动失败: 进程已创建但无法唤醒（{}）", e);
                self.append_system_line(key, msg.clone());
                if let Ok(mut failed) = self.failed.lock() {
                    failed.insert(key.to_string(), FailedService {
                        service_id: key.to_string(),
                        exit_code: None,
                        timestamp: chrono::Utc::now().to_rfc3339(),
                    });
                }
                return Err(msg);
            }
        }

        let pid = child.id();
        let stdout = child.stdout.take().ok_or("无法获取 stdout")?;
        let stderr = child.stderr.take().ok_or("无法获取 stderr")?;

        // 使用 Arc<str> 作为 key，避免热路径 String clone（P2 #6）
        // done 信号：reader 线程结束时发送端 drop → recv 返回 Disconnected。
        // 清理时用 recv_timeout 等待线程退出并限时放弃，避免无限 join 阻塞主线程。
        let key1: LogKey = Arc::from(key);
        // 停止标志：cleanup 时置位，旧 reader 线程据此退出（否则 taskkill 未杀净时
        // 旧进程输出会串进新服务日志，且"已停止"的日志仍在增长）
        let stop_flag = Arc::new(AtomicBool::new(false));
        // 待发行缓冲：reader 只把行推进来，由每服务一个批量发射线程按 50ms 窗口合成一条事件。
        //
        // 为什么必须批量：每行一次 `emit` 要走完整链路——serde 序列化 → 拼 JS 字符串 →
        // EventLoopProxy 跨线程消息 → 主线程 ExecuteScript → 渲染进程一个 JS task。
        // 行速率由子进程决定且无上限（构建/进度条可达数千行/秒），逐行付这些固定成本
        // 正是"日志面板没打开也在付费"的地方。
        //
        // 为什么用独立线程而不是"攒够 N 行再发"：后者会让稀疏输出（交互式命令打印几行后
        // 静默）的最后几行一直压着，直到下次输出才显示。50ms 定时窗口既摊平突发，
        // 也不牺牲响应性。
        let pending_emit: Arc<Mutex<Vec<Arc<LogLine>>>> = Arc::new(Mutex::new(Vec::new()));
        let log_signal = Arc::new(LogSignal::new());
        // reader 存活计数：两个 reader 都退出（进程 EOF 或被停止）后发射线程收尾退出
        let readers_alive = Arc::new(std::sync::atomic::AtomicUsize::new(2));
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let stdout_thread = spawn_log_reader(
            stdout, "stdout", Arc::clone(&key1), Arc::clone(&self.log_buffers),
            Arc::clone(&pending_emit), Arc::clone(&readers_alive), Arc::clone(&log_signal),
            Arc::clone(&stop_flag), done_tx,
        )
        .map_err(|e| {
            // 线程创建失败：杀掉已 spawn 的子进程并回收句柄，避免进程泄漏
            kill_and_reap(&mut child, Duration::from_millis(2000));
            format!("创建 stdout 日志读取线程失败: {}", e)
        })?;
        let _ = stdout_thread; // 不 join；线程在进程退出（EOF）后自然结束

        let (done_tx2, done_rx2) = std::sync::mpsc::channel::<()>();
        let mut done_rx_opt = Some(done_rx);
        let stderr_thread = spawn_log_reader(
            stderr, "stderr", Arc::clone(&key1), Arc::clone(&self.log_buffers),
            Arc::clone(&pending_emit), Arc::clone(&readers_alive), Arc::clone(&log_signal),
            Arc::clone(&stop_flag), done_tx2,
        )
        .map_err(|e| {
            // stderr 线程创建失败：杀掉进程树并回收句柄（stdout 线程随后读到 EOF 自行退出）
            kill_and_reap(&mut child, Duration::from_millis(2000));
            format!("创建 stderr 日志读取线程失败: {}", e)
        })?;
        let _ = stderr_thread;

        // 批量发射线程（两个 reader 都创建成功后才起，避免失败路径留下孤儿线程）
        let flush_pending = Arc::clone(&pending_emit);
        let flush_alive = Arc::clone(&readers_alive);
        let flush_sink = Arc::clone(&log_sink);
        let flush_key = key.to_string();
        spawn_log_flush_thread(
            format!("nexus-logflush-{}", key),
            flush_pending,
            flush_alive,
            Arc::clone(&log_signal),
            move |batch| {
                flush_sink(ServiceLogBatchPayload {
                    service_key: flush_key.clone(),
                    lines: batch,
                });
            },
        )
        .map_err(|e| {
            // 发射线程起不来：日志仍写缓冲（面板打开时能取到快照），只是失去实时推送
            kill_and_reap(&mut child, Duration::from_millis(2000));
            format!("创建日志批量发射线程失败: {}", e)
        })?;

        // Phase 3: 重新获取锁，二次检查后插入
        let mut procs = self.processes.lock().unwrap_or_else(|e| {
            log::error!("[nexus] processes 锁已中毒，继续使用: {}", e);
            e.into_inner()
        });
        if procs.contains_key(key) {
            // TOCTOU 竞态：另一个线程已插入同 key，清理当前创建的资源
            log::warn!("[nexus] TOCTOU 竞态: {} 已在运行中，清理泄漏的子进程", key);
            cleanup_process(CleanupEntry {
                key: key.to_string(), pid,
                // 这个进程刚 spawn 出来、还活着，取不到时间的概率极低；取不到记 0，
                // 只会让"游离后代"的时间校验最宽松（这条路径随后就把它杀掉了）
                started_at: process_start_time_millis(pid).unwrap_or(0),
                child: Some(child), adopted: Vec::new(), follow: None,
                done_stdout: done_rx_opt.take(), done_stderr: Some(done_rx2),
                stop_flag: Some(Arc::clone(&stop_flag)),
            }, None);
            return Err(format!("{} 已在运行中", key));
        }
        // 直接子进程的启动时刻：认领游离进程时用它排除 pid 复用。
        // 取不到（进程已退出/无权限）时记 0——0 让"晚于它"的校验最宽松，
        // 而这个分支紧随其后就会被 running() 判成"已退出"，不会留下长期影响
        let spawn_started_at = process_start_time_millis(pid).unwrap_or(0);
        procs.insert(key.to_string(), ProcessInfo {
            child: Some(child), pid, spawn_started_at, adopted: Vec::new(),
            cwd: cwd.to_string(), follow: None, log_probe: None,
            project_id: project_id.to_string(),
            log_sink: Arc::clone(&log_sink),
            done_stdout: done_rx_opt,
            done_stderr: Some(done_rx2),
            stop_flag,
        });
        Ok(())
    }

    /// 停止单个服务。全程持有该 key 的生命周期锁（NEW-17）。
    pub fn stop(&self, key: &str) -> Result<(), String> {
        self.stop_with_snapshot(key, None)
    }

    /// 停止单个服务，可带一张调用方共用的进程快照（批量停止用，见 `candidate_descendants`）。
    ///
    /// 并发安全：每个 key 各自持自己的生命周期锁，进程表只在 Phase 1 短暂加锁——
    /// 因此批量停止可以并行跑（PERF-20①），互不阻塞。
    pub(crate) fn stop_with_snapshot(&self, key: &str, snapshot: Option<&[super::winproc::ProcEntry]>) -> Result<(), String> {
        let lock = self.lifecycle_lock(key);
        let _guard = lock_lifecycle(&lock, key);
        self.stop_locked(key, snapshot)
    }

    fn stop_locked(&self, key: &str, snapshot: Option<&[super::winproc::ProcEntry]>) -> Result<(), String> {
        let outcome = self.stop_locked_outcome(key, snapshot);
        // **收尸没干净就如实上报**（CQ-25）：表项此时已经被摘掉，`get_running()` 会报它
        // "未运行"，界面显示已停止，而进程仍在跑并占着端口。上层（stop_project_services /
        // delete_service）本来就是按"可能失败"写的，这条 Err 才是那些分支的第一个真实来源
        if !outcome.is_clean() {
            return Err(format!("停止未完成：{}", outcome.describe()));
        }
        Ok(())
    }

    /// 停止的**实现**：把收尸结果原样交出来，不压成错误文案。
    ///
    /// 为什么拆成两个（ARCH-31）：`restart` 需要在"旧进程没退干净"时再等一会儿，而那需要
    /// **pid**（`CleanupOutcome.survivors`）——`stop_locked` 把它拍成了一句错误字符串，pid 就
    /// 没了。两条路径共用这一段实现，停止语义因此只有一处。
    fn stop_locked_outcome(&self, key: &str, snapshot: Option<&[super::winproc::ProcEntry]>) -> CleanupOutcome {
        log::info!("[nexus] 停止服务: {}", key);

        // Phase 1: 从 map 中移除 entry，释放锁
        let entry: Option<CleanupEntry> = {
            let mut procs = self.processes.lock().unwrap_or_else(|e| {
                log::error!("[nexus] processes 锁已中毒，继续使用: {}", e);
                e.into_inner()
            });
            procs.remove(key).map(|mut info| take_cleanup_entry(key.to_string(), &mut info))
        };

        // Phase 2: 在锁外执行清理
        let outcome = if let Some(entry) = entry {
            cleanup_process(entry, snapshot)
        } else {
            log::debug!("[nexus] stop: 服务 {} 未在运行，忽略", key);
            CleanupOutcome::default()
        };
        // 主动停止 = 正常关闭：无论进程是否还在管理表（崩溃的服务已被 running() 移出），
        // 都清除失败标记并清空日志（含失败日志）——项目"全部停止"依赖此语义
        if let Ok(mut failed) = self.failed.lock() {
            failed.remove(key);
        }
        if let Ok(mut buffers) = self.log_buffers.lock() {
            let log_key: LogKey = Arc::from(key);
            buffers.remove(&*log_key);
        }
        outcome
    }

    /// 重启单个服务。
    ///
    /// **整段**（stop + start）持有同一把生命周期锁：若只让 stop/start 各自加锁，
    /// 两者之间会漏出一个窗口——并发调用方可以在这个窗口里插进自己的 start，
    /// 于是"重启"的结果取决于调度顺序。这里用 `*_locked` 变体在同一把锁内完成两段。
    pub fn restart(&self, spec: ServiceSpawn<'_>) -> Result<(), String> {
        let lock = self.lifecycle_lock(spec.service_id);
        let _guard = lock_lifecycle(&lock, spec.service_id);
        // 旧进程没确认退出**不阻塞重启**（CQ-25）：用户的诉求是"把它跑起来"，而新进程
        // 起不来时后端会如实报端口/启动错误——那比"重启被拒绝"更接近用户意图。
        //
        // 但**先给它一段宽限**（ARCH-31）：见 `settle_survivors` 的说明。原来的写法是
        // 只 `log::warn` 一句就继续启动，于是新进程与"正在退出的旧进程"抢端口——
        // dev server 的子进程持有监听套接字，taskkill 之后内核回收还要几十到几百毫秒，
        // 抢输的那一方表现为"启动即退出"。**自动重启比手动点更容易踩上这个窗口**：
        // 它每 2 秒就可能来一次，而用户手动点的时候这些资源通常已经释放了。
        let outcome = self.stop_locked_outcome(spec.service_id, None);
        self.settle_survivors(spec.service_id, outcome);
        self.start_locked(spec)
    }

    /// 重启前等"没退干净的旧进程"真的消失（最长 [`RESTART_SETTLE_TIMEOUT`]）。
    ///
    /// 等到了就什么都不说（正常路径，不打扰用户）；等不到就照旧启动，但把这件事
    /// **写进该服务的日志缓冲**——只写 `log::warn` 的话，这条唯一的原因只落在
    /// `~/.nexus/logs/nexus.log` 里，而用户的现场在日志面板上：他看到的只有卡片上
    /// "失败"两个字，没有任何线索指向"旧进程还占着端口"。
    fn settle_survivors(&self, service_id: &str, outcome: CleanupOutcome) {
        if outcome.survivors.is_empty() {
            return;
        }
        log::info!(
            "[nexus] 重启前等待旧进程退出：{} 个，最长 {:?}",
            outcome.survivors.len(),
            RESTART_SETTLE_TIMEOUT
        );
        let left = wait_until_gone(
            outcome.survivors,
            RESTART_SETTLE_TIMEOUT,
            RESTART_SETTLE_POLL,
            survivors_of,
        );
        if !left.is_empty() {
            self.append_system_line(service_id, format!(
                "上一次停止未完成：仍有 {} 个进程未退出（{:?}），本次启动可能与它们抢端口\
                 ——若随后启动失败，请稍后手动重启一次",
                left.len(),
                left
            ));
            log::warn!("[nexus] 重启前旧进程仍未退出: {:?}（已继续启动）", left);
        }
    }

    /// 追加系统标记行前，先调用者已确认需要写；锁失败时静默忽略（不影响主流程）
    pub fn append_system_line(&self, key: &str, text: String) {
        append_system_line(&self.log_buffers, key, text);
    }

    /// 取某服务的日志快照（锁内只拷 `Arc` 指针，字符串拷贝交给调用方的序列化）。
    ///
    /// 之前这里在锁内做 2000 次深拷贝：全局日志锁被持有期间，所有服务的 reader 线程
    /// 都阻塞在 `push_log_line` 的同一把锁上（已读到的行既不进缓冲也不 emit）。
    pub fn get_logs(&self, key: &str) -> Vec<Arc<LogLine>> {
        let log_key: LogKey = Arc::from(key);
        match self.log_buffers.lock() {
            Ok(b) => b.get(&*log_key)
                .map(|buf| buf.lines.iter().cloned().collect())
                .unwrap_or_default(),
            Err(e) => {
                log::error!("ProcessManager log_buffers 锁已中毒: {}", e);
                Vec::new()
            }
        }
    }

    /// 取某服务的 (工作目录, 启动时刻)；服务不在表里时 None
    fn cwd_and_start_for(&self, key: &str) -> Option<(String, u64)> {
        let procs = self.processes.lock().ok()?;
        let info = procs.get(key)?;
        Some((info.cwd.clone(), info.spawn_started_at))
    }

    /// 返回当前运行中的 (project_id, service_id) 列表。
    ///
    /// 比"直接子进程退出即服务结束"多一步**游离进程认领**：直接子进程（cmd）退出后先不
    /// 下结论，看它有没有留下"另开窗口"跑起来的后代（Tomcat 的 `startup.bat` →
    /// `catalina.bat start` → `start "Tomcat" java`，见 `AdoptedChild`）。
    /// 认领到就继续算运行中，认领不到才按退出处理。
    /// 当前运行中的服务：(project_id, service_id, 正在跟随的日志文件)
    /// 生命周期心跳：推进"崩溃判定 / 游离进程认领 / 日志文件自动发现"这三件带副作用的事。
    ///
    /// 为什么要独立成一个函数（ARCH-22）：这三件事原先**只**写在 `running()` 里，而
    /// `running()` 的唯一周期性调用者在前端（`runningStore` 每 3 秒 `get_running`）。
    /// 窗口最小化时 WebView2 对隐藏窗口的 `setInterval` 有节流（后台可降到 ≥1 分钟一次），
    /// 于是"崩掉的服务一直显示运行中""另开窗口跑掉的服务永不被认领""日志文件发现停摆"
    /// 三件事**同时**停滞，而症状是"时好时坏"（任何一次 start/stop 又会顺手纠正）。
    ///
    /// 现在由 `lib.rs` 的后端线程每 2 秒调一次：界面轮询频率从**功能正确性**的参数
    /// 降级为**显示新鲜度**的参数。`running()` 仍会先 tick 再读表（保持原语义：
    /// 谁调用它都不会看到比上一拍更旧的状态）。
    pub fn tick(&self) {
        /// 本轮判定为"进程刚结束"的服务
        struct JustEnded {
            key: String,
            pid: u32,
            started_at: u64,
            exit_code: Option<i32>,
            /// true = 直接子进程退出（要走查游离后代）；false = 认领的进程全退了
            check_survivors: bool,
        }

        // Phase 1（锁内，只做判定）：child 存活的跳过；child 已退出的记为候选；
        // 认领模式的看认领到的进程是否已全部退出。
        let mut ended: Vec<JustEnded> = Vec::new();
        // 需要去找日志文件的服务：(key, cwd, 服务启动时刻)
        let mut probe: Vec<(String, String, u64)> = Vec::new();
        {
            let mut procs = self.processes.lock().unwrap_or_else(|e| {
                log::error!("ProcessManager processes 锁已中毒，继续使用: {}", e);
                e.into_inner()
            });
            for (k, info) in procs.iter_mut() {
                match info.child.as_mut() {
                    Some(child) => match child.try_wait() {
                        Ok(Some(status)) => ended.push(JustEnded {
                            key: k.clone(),
                            pid: info.pid,
                            started_at: info.spawn_started_at,
                            exit_code: status.code(),
                            check_survivors: true,
                        }),
                        Ok(None) => {} // 运行中
                        Err(e) => {
                            // 无法检查状态：保守按"仍存活"处理（原实现判为已死 → 摘表 + 标失败，
                            // 而进程可能仍在运行且此后不再受管理）
                            log::warn!("[nexus] 查询进程状态失败 ({}): {}（按存活处理）", k, e);
                        }
                    },
                    None => {
                        // 认领模式：认领到的进程全部退出，才算这个服务结束。
                        // 退出码取第一个认领进程的——多个游离进程时"服务退出码"本就没有单一
                        // 含义，给一个可读的值即可（拿不到才落到 "?"）
                        if info.adopted.iter().all(|a| !a.handle.is_alive()) {
                            ended.push(JustEnded {
                                key: k.clone(),
                                pid: info.pid,
                                started_at: info.spawn_started_at,
                                exit_code: info.adopted.iter().find_map(|a| a.handle.exit_code()),
                                check_survivors: false,
                            });
                        } else if info.follow.is_none() {
                            // 认领意味着 stdout 拿不到东西了：找找它在写哪个日志文件。
                            // **不设尝试上限**：服务"开写第一行日志"可能要好几秒（Tomcat 要等 JVM
                            // 起来才落盘，实测有过 10 秒以上），设上限就会把这类服务永远排除在外。
                            // 代价只是每轮读几次目录，而服务一停这条路径就没了
                            probe.push((k.clone(), info.cwd.clone(), info.spawn_started_at));
                        }
                    }
                }
            }
        }

        // Phase 1.5（锁外）：走查游离后代。系统调用（进程快照 / OpenProcess）放在锁外，
        // 与"锁内只做 try_wait + 摘表"的既有约定一致。
        // 用 HashMap 而不是 Vec：`AdoptedChild` 持有进程句柄、**不可 Clone**，
        // 取出即消费（`remove`）才不会重复持有。
        let mut survived: HashMap<String, Vec<AdoptedChild>> = ended
            .iter()
            .filter(|e| e.check_survivors)
            .filter_map(|e| {
                let list = adopt_survivors(e.pid, e.started_at);
                if list.is_empty() { None } else { Some((e.key.clone(), list)) }
            })
            .collect();

        // Phase 2（锁内）：落实。认领成功的摘掉 child（已退出，只回收句柄）并存入 adopted；
        // 其余摘表，交给 Phase 2.5 收尸。
        let mut exited: Vec<(String, Option<i32>)> = Vec::new();
        let mut dead: Vec<CleanupEntry> = Vec::new();
        let mut adopted_logs: Vec<(String, String)> = Vec::new();
        {
            let mut procs = self.processes.lock().unwrap_or_else(|e| {
                log::error!("ProcessManager processes 锁已中毒，继续使用: {}", e);
                e.into_inner()
            });
            for e in ended {
                let Some(list) = survived.remove(&e.key) else {
                    // 没有游离后代：按退出处理
                    exited.push((e.key.clone(), e.exit_code));
                    if let Some(mut info) = procs.remove(&e.key) {
                        dead.push(take_cleanup_entry(e.key, &mut info));
                    }
                    continue;
                };
                // 认领成功。期间可能已被 stop 摘走 —— 那就什么都不做，
                // 句柄随 list 在这里 drop（进程已被 stop 结束）
                let Some(info) = procs.get_mut(&e.key) else { continue };
                if let Some(mut child) = info.child.take() {
                    let _ = child.wait(); // 已退出：只回收句柄，不 taskkill
                }
                let who = list.iter().map(|a| format!("{} (pid {})", a.name, a.pid))
                    .collect::<Vec<_>>().join("、");
                info.adopted = list;
                adopted_logs.push((e.key, who));
            }
        }

        // Phase 2.5（锁外）：写日志标记与失败状态（原实现在持锁期间做）
        for (key, code) in &exited {
            let code_str = code.map(|c| c.to_string()).unwrap_or_else(|| "?".into());
            self.append_system_line(key, format!("进程已退出（退出码: {}）", code_str));
            if let Ok(mut failed) = self.failed.lock() {
                failed.insert(key.clone(), FailedService {
                    service_id: key.clone(),
                    exit_code: *code,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                });
            }
        }
        // 认领成功必须**说出来**：否则面板上只有那行"进程已退出（退出码: 0）"，
        // 用户以为服务没了，而它其实还在跑 —— 这正是本次要修掉的谎报
        for (key, who) in &adopted_logs {
            self.append_system_line(key, format!(
                "直接子进程已退出，但它另开窗口启动的进程仍在运行（{}）；已接管：「停止」会一并结束它们",
                who
            ));
            // 刚认领的服务也立刻找一次日志文件（不用等下一轮轮询）
            if let Some((cwd, started_at)) = self.cwd_and_start_for(key) {
                probe.push((key.clone(), cwd, started_at));
            }
        }
        // 找日志文件（锁外：要读目录、开文件）
        for (key, cwd, started_at) in probe {
            self.probe_log_file(&key, &cwd, system_time_from_millis(started_at));
        }

        // Phase 3（锁外）：收尸已退出进程（已退出的进程不再 taskkill）。
        // 日志缓冲保留（Phase 2.5 已追加退出标记行）：崩溃/秒退的报错是诊断关键。
        // 这一支是"进程已经退出"才走到这里的，理论上收尸必然干净；不干净时留痕
        // （通常意味着 pid 刚被复用，或还有没杀掉的游离后代）
        for entry in dead {
            let outcome = cleanup_process(entry, None);
            if !outcome.is_clean() {
                log::warn!("[nexus] 收尸未完全干净: {}", outcome.describe());
            }
        }
    }

    /// 读当前运行表（只读，无副作用）：先 tick 一拍，再取快照——调用方拿到的状态
    /// 不会比"刚刚推进过的"更旧（与拆分前的 `running()` 语义一致）
    pub fn running(&self) -> Vec<(String, String, Option<String>)> {
        self.tick();
        self.snapshot_running()
    }

    /// 只读的运行表快照（不推进任何生命周期）。后台线程 tick 完之后取它来判定/展示
    fn snapshot_running(&self) -> Vec<(String, String, Option<String>)> {
        // Phase 4: 重新获取锁，一次取齐 (project_id, service_id, 跟随的日志文件)。
        //
        // `followed_log` 一并在这里取走（PERF-33）：它就在同一张表里，而原实现是
        // 调用方拿着这串 id **每个服务再各取一次锁**（18 服务 = 一轮 18 次加锁，
        // 只为读一个字段）。表是同一张、锁是同一把，没有分两次取的理由。
        self.processes.lock()
            .map(|procs| procs.iter().map(|(k, v)| {
                (v.project_id.clone(), k.clone(), v.follow.as_ref().map(|f| f.path.clone()))
            }).collect())
            .unwrap_or_default()
    }

    pub fn stop_all(&self) {
        let entries: Vec<CleanupEntry> = {
            let mut procs = match self.processes.lock() {
                Ok(guard) => guard,
                Err(e) => {
                    log::error!("ProcessManager processes 锁已中毒: {}", e);
                    e.into_inner()
                }
            };
            let count = procs.len();
            if count > 0 {
                log::info!("[nexus] stop_all: 正在终止 {} 个进程...", count);
            }
            procs.drain().map(|(key, mut info)| {
                log::debug!("[nexus]   清理 {} (pid={})", key, info.pid);
                take_cleanup_entry(key, &mut info)
            }).collect()
        };

        for entry in entries {
            let (key, pid) = (entry.key.clone(), entry.pid);
            let outcome = cleanup_process(entry, None);
            // 退出路径没有"失败上报"的接收方（进程马上要结束），但仍要留痕：
            // 残留进程会由 Job Object 在应用退出时兜底，而日志是唯一能解释
            // "为什么退出后 taskmgr 里还有 java.exe"的地方
            if !outcome.is_clean() {
                log::warn!("[nexus]   清理 {} (pid={}) 未完全成功: {}", key, pid, outcome.describe());
            } else {
                log::debug!("[nexus]   已清理 {} (pid={})", key, pid);
            }
        }

        if let Ok(mut buffers) = self.log_buffers.lock() {
            buffers.clear();
        }
        // 全部停止 = 主动关闭：清除所有失败标记（正常日志与失败日志都已清空）
        if let Ok(mut failed) = self.failed.lock() {
            failed.clear();
        }
        log::info!("[nexus] stop_all: 已完成");
    }

    /// 返回意外退出的服务列表（前端显示失败状态）
    pub fn failed(&self) -> Vec<FailedService> {
        match self.failed.lock() {
            Ok(f) => f.values().cloned().collect(),
            Err(e) => {
                log::error!("ProcessManager failed 锁已中毒: {}", e);
                Vec::new()
            }
        }
    }

    /// 测试用：取某服务当前认领到的游离进程 (pid, 映像名)。
    ///
    /// 真实写入路径是 `running()` 轮询里的认领（需要真起一个"另开窗口"的进程才能走到），
    /// 这里只把结果读出来供断言——不暴露任何内部状态给生产代码。
    #[cfg(test)]
    pub(crate) fn adopted_pids_for_test(&self, key: &str) -> Vec<(u32, String)> {
        self.processes
            .lock()
            .map(|procs| {
                procs.get(key)
                    .map(|info| info.adopted.iter().map(|a| (a.pid, a.name.clone())).collect())
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    }

    /// 测试用：注入一条"意外退出"记录。
    ///
    /// 真实写入发生在 reader 线程发现子进程意外退出时，需要真起一个进程才能触发；
    /// 这里只验证失败表本身的语义（读取、以及 stop 后清除），不重复测进程 spawn。
    #[cfg(test)]
    pub(crate) fn record_failure_for_test(&self, key: &str, exit_code: Option<i32>) {
        if let Ok(mut failed) = self.failed.lock() {
            failed.insert(key.to_string(), FailedService {
                service_id: key.to_string(),
                exit_code,
                timestamp: "test".into(),
            });
        }
    }

}

impl Drop for ProcessManager {
    fn drop(&mut self) {
        self.stop_all();
    }
}

// ─── Utilities ──────────────────────────────────────────────

/// 带超时等待子进程退出；超时返回 false（进程仍存活）
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return true,
        }
    }
}

/// 终止进程树并**回收**子进程句柄（kill → 限时等待 → wait）。
///
/// 为什么统一成一个函数：kill 之后**必须** wait——不 wait 会残留进程句柄（僵尸项）；
/// 而 tokio 的 `kill_on_drop` 只是发送终止信号，**不会**回收。
/// 原实现散落多处 `kill_process_tree` 后直接 return，句柄就留在那里了。
///
/// 超时不 panic、不无限等：仍存活的进程交由 Job Object（KILL_ON_JOB_CLOSE）在
/// 应用退出时兜底，这里只保证句柄一定被 `wait` 回收一次。
pub(crate) fn kill_and_reap(child: &mut Child, timeout: Duration) {
    let pid = child.id();
    // 失败留痕（CQ-25）：这里没有接收方（早退/超时路径），但"没杀掉"必须能在日志里看见
    if let Err(e) = kill_process_tree(pid) {
        log::warn!("[nexus] 终止进程树失败 (pid={}): {}", pid, e);
    }
    if wait_with_timeout(child, timeout) {
        let _ = child.wait();
    } else {
        log::warn!("[nexus] 进程 (pid={}) 未能按时退出，已交由 Job Object 兜底", pid);
    }
}

/// 把服务表项拆成清理条目（`child` / `adopted` 一并带走：停止要连同它们一起结束）
fn take_cleanup_entry(key: String, info: &mut ProcessInfo) -> CleanupEntry {
    CleanupEntry {
        key,
        pid: info.pid,
        started_at: info.spawn_started_at,
        child: info.child.take(),
        adopted: std::mem::take(&mut info.adopted),
        follow: info.follow.take(),
        done_stdout: info.done_stdout.take(),
        done_stderr: info.done_stderr.take(),
        stop_flag: Some(Arc::clone(&info.stop_flag)),
    }
}

/// `pid` 名下、创建时刻晚于 `started_at` 的后代 pid（含"另开窗口"跑掉的那些）。
///
/// 启动时刻这一关是**防 pid 复用**：系统会把旧 pid 分给无关进程，而新进程可能恰好
/// 由"复用了我们那条 cmd 的 pid"的进程生出来，此时父链看起来完全合法。
/// 时间不可能倒流——候选的创建时刻早于我们那条子进程，就一定不是它生出来的。
///
/// 非 Windows 上 `snapshot_processes` 返回空表，这里自然也返回空。
fn candidate_descendants(pid: u32, started_at: u64, snapshot: Option<&[super::winproc::ProcEntry]>) -> Vec<u32> {
    // 批量停止会带一张**本轮共用**的快照进来（PERF-21）：18 个服务一起停时，全系统进程表
    // 只需枚举一次，而不是每个服务各枚举一次。快照取在"杀任何东西之前"也更准确——后代与
    // 父进程的链按原始快照判定，不受我们自己的收尸动作影响。单独停止时传 None，就地取一张
    let owned;
    let snapshot = match snapshot {
        Some(s) => s,
        None => {
            owned = super::winproc::snapshot_processes();
            &owned
        }
    };
    super::winproc::descendants(pid, snapshot)
        .into_iter()
        .filter(|&child| {
            process_start_time_millis(child)
                .map(|t| t >= started_at)
                .unwrap_or(false)
        })
        .collect()
}

/// 控制台宿主进程名：它们不是服务在跑的东西，而是"另开窗口"时系统给新控制台配的宿主。
///
/// 为什么要排除（实测踩到）：
/// 1. cmd 用 `start` 另开窗口时会拉起 `conhost.exe`，父链上它也是后代，于是被一起认领——
///    接管名单里混进一个 `conhost.exe`，用户看到的日志像是 Nexus 管错了进程；
/// 2. 更麻烦的是它**持有被托管进程的句柄**，客户端被结束后对象不会立刻销毁，
///    于是"进程是否还在"的两种判据（按 pid 查得到 vs taskkill 说找不到）会打架；
/// 3. 杀掉真正的工作进程时，`taskkill /T` 顺带就会结束它的控制台宿主，不缺这一步。
///
/// 名字匹配是启发式，但代价只是"少接管一个控制台宿主"：真出错也不会漏掉工作进程。
/// `OpenConsole.exe` 是 Windows 11 上 Windows Terminal 作为默认终端时的同类进程。
fn is_console_host(name: &str) -> bool {
    name.eq_ignore_ascii_case("conhost.exe") || name.eq_ignore_ascii_case("OpenConsole.exe")
}

/// 认领游离进程：走查 `pid` 名下仍在运行的后代，逐个打开句柄。
///
/// 返回空 = 确实没有游离进程（调用方按服务真的退出处理）。
/// 只认得下还在跑的：已退出的进程没必要接管，它的退出也不该让"服务"继续算运行中。
fn adopt_survivors(pid: u32, started_at: u64) -> Vec<AdoptedChild> {
    let snapshot = super::winproc::snapshot_processes();
    let mut out = Vec::new();
    for child_pid in super::winproc::descendants(pid, &snapshot) {
        if !process_start_time_millis(child_pid).map(|t| t >= started_at).unwrap_or(false) {
            continue;
        }
        let name = super::winproc::name_of(child_pid, &snapshot);
        if is_console_host(&name) {
            continue;
        }
        let Some(handle) = super::winproc::OwnedProcess::open(child_pid) else {
            continue; // 已退出 / 无权限：不接管
        };
        if !handle.is_alive() {
            continue;
        }
        out.push(AdoptedChild { pid: child_pid, name, handle });
    }
    out
}

/// 清理单个进程条目（在锁外调用）
///
/// 流程：先结束**认领到的游离进程**（它们不在我们的进程树里，漏掉就等于"停止"没生效）
/// → try_wait 确认直接子进程仍存活才 taskkill（已退出时 PID 可能已被系统复用，
/// 直接 taskkill 会误杀无辜进程）→ 顺手收掉直接子进程名下跑出去的游离后代
/// （停止与下一次 `running()` 轮询之间存在窗口，这个窗口里 stop 也必须收干净）
/// → 带超时等待退出 → 等待 reader 线程结束（进程死后管道写端关闭，reader 读到 EOF
/// 自然退出）→ 超时则放弃等待，交由 Job Object（KILL_ON_JOB_CLOSE）在应用退出时兜底。
///
/// 注意：等待 reader 线程用 recv_timeout 而非 join——进程树未全灭时
/// （taskkill 失败、或孙进程仍持有管道写端）EOF 永不发生，join 会无限阻塞
/// 同步命令的主线程（stop/get_running 全部卡死、窗口关不掉）。
/// 一次收尸的结果。
///
/// 为什么要有返回值（CQ-25）：上层本来就是**按"stop 可能失败"写的**——
/// `stop_project_services` 逐个收集失败、`delete_service`/`delete_project` 都留痕，
/// 而这里此前连返回值都没有、`stop_locked` 只有一个 `Ok(())` 出口，那些 Err 分支
/// **结构性不可达**。真实后果：taskkill 失败或进程 2 秒内没死时，表项已被摘掉，
/// `get_running()` 于是报它"未运行"，界面显示已停止，而进程仍在跑并占着端口。
///
/// 判据是**复核存活**而不是 taskkill 的退出码：进程不存在时 taskkill 也返回非零，
/// 而那正是"已经收干净了"。
#[derive(Default)]
pub(crate) struct CleanupOutcome {
    /// 收尸结束时仍存活的 pid（有它 = 这次停止没生效）
    survivors: Vec<u32>,
    /// taskkill 的失败原文（诊断上下文；它本身不等于收尸失败）
    errors: Vec<String>,
}

impl CleanupOutcome {
    pub(crate) fn is_clean(&self) -> bool {
        self.survivors.is_empty()
    }

    /// 给人看的原因（错误信息里带上，界面与日志都能说清"为什么说没停掉"）
    pub(crate) fn describe(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.survivors.is_empty() {
            parts.push(format!(
                "仍有 {} 个进程未退出（{:?}）——可能正在退出中，或被安全软件拦截了终止",
                self.survivors.len(),
                self.survivors
            ));
        }
        parts.extend(self.errors.iter().cloned());
        parts.join("；")
    }
}

/// 重启前等待旧进程退出的上限（ARCH-31）。
///
/// 3 秒是折中：端口/句柄回收实测在几十到几百毫秒量级，3 秒足够覆盖；再长就等于"点了重启
/// 半天没反应"，而这条路径的用户诉求恰恰是"赶紧把它跑起来"——所以宁可超时后照旧启动
/// （并如实写出原因），也不无限等。
const RESTART_SETTLE_TIMEOUT: Duration = Duration::from_millis(3000);

/// 等待期间的轮询间隔：100ms 足够细（句柄失效不会更快），也不会空转烧 CPU
const RESTART_SETTLE_POLL: Duration = Duration::from_millis(100);

/// 轮询到给定 pid 全部消失为止，返回**仍未消失**的那些（超时也原样返回）。
///
/// 为什么抽成自由函数并接收 `alive` 闭包：真正判断存活要操作系统句柄（`survivors_of`），
/// 而"轮询—超时—返回剩余"这段决策逻辑本身值得被钉住——超时算错的两个方向都不报错：
/// 要么白等（用户以为卡住），要么在旧进程还在时就抢端口（就是这次要修的症状）。
/// 时间参数同样注入，测试因此是毫秒级的，不用真的等 3 秒。
fn wait_until_gone(
    mut survivors: Vec<u32>,
    timeout: Duration,
    poll: Duration,
    mut alive: impl FnMut(&[u32]) -> Vec<u32>,
) -> Vec<u32> {
    let deadline = Instant::now() + timeout;
    while !survivors.is_empty() && Instant::now() < deadline {
        std::thread::sleep(poll);
        survivors = alive(&survivors);
    }
    survivors
}

/// 收尸后复核一批 pid 是否真的没了。
///
/// 用 `OwnedProcess::open` 拿到句柄再判存活（而不是 `process_start_time_millis`）：
/// 句柄身份不受 pid 复用影响，这里刚杀完就复用的概率虽低，但"误报已退出"正是
/// 本项目最忌讳的错误方向。
fn survivors_of(pids: &[u32]) -> Vec<u32> {
    pids.iter()
        .copied()
        .filter(|pid| super::winproc::OwnedProcess::open(*pid).map(|h| h.is_alive()).unwrap_or(false))
        .collect()
}

fn cleanup_process(entry: CleanupEntry, snapshot: Option<&[super::winproc::ProcEntry]>) -> CleanupOutcome {
    let CleanupEntry { key: _key, pid, started_at, mut child, adopted, follow, done_stdout, done_stderr, stop_flag } = entry;
    // 先置停止标志：reader 线程立刻停止写缓冲与 emit。
    // 否则进程树未杀净（孙进程持管道）时旧线程会继续往同一个 key 写日志——
    // 表现为"已停止的服务日志还在涨"，重启后新旧进程输出还会串台
    if let Some(flag) = &stop_flag {
        flag.store(true, Ordering::Relaxed);
    }
    // 跟随线程同理（它看的是同一个 stop_flag，这里额外等它退出）：
    // 不等的话，stop 之后它还会往"已被 remove 的日志缓冲"里写——那等于把缓冲又建回来，
    // 这块内存直到下次启动同 id 的服务才会被替换
    let mut follow_done = None;
    if let Some(mut f) = follow {
        f.stop.store(true, Ordering::Relaxed);
        follow_done = f.done.take();
    }
    // 认领到的游离进程：父进程（cmd）早已退出，它们不在任何人的进程树下，
    // 必须按 pid 单独结束 —— 漏掉就是"点了停止，Tomcat 还在后台跑"
    let mut outcome = CleanupOutcome::default();
    let mut kill = |pid: u32, what: &str| {
        if let Err(e) = kill_process_tree(pid) {
            log::warn!("[nexus] 结束{}失败 (pid={}): {}", what, pid, e);
            outcome.errors.push(e);
        }
    };
    for a in &adopted {
        log::info!("[nexus] 结束接管进程 {} (pid={})", a.name, a.pid);
        kill(a.pid, "接管进程");
    }
    let alive = matches!(child.as_mut().map(|c| c.try_wait()), Some(Ok(None)));
    if alive {
        kill(pid, "子进程");
    }
    // 还活着的直接子进程名下，可能已经有"另开窗口"跑出去的后代（此刻它们尚未被认领，
    // 或刚被认领前就被 stop 撞上）。一并收掉，避免留下占端口的孤儿。
    let orphans = candidate_descendants(pid, started_at, snapshot);
    for orphan in &orphans {
        log::info!("[nexus] 结束游离孙进程 (pid={})", orphan);
        kill(*orphan, "游离孙进程");
    }
    let exited = match child.as_mut() {
        Some(c) => wait_with_timeout(c, Duration::from_millis(2000)),
        // 认领路径已经 `wait` 回收过直接子进程：没有可等的东西，也不该再 warn
        None => true,
    };
    // 等待 reader 线程结束：正常路径（进程已死）立即返回；异常路径最多等 1 秒后放弃，
    // 线程在进程树最终退出后自然结束（不 join 不会泄漏——线程自行退出即释放资源）
    for rx in [done_stdout, done_stderr, follow_done].into_iter().flatten() {
        let _ = rx.recv_timeout(Duration::from_millis(1000));
    }
    match child.as_mut() {
        Some(c) if exited => {
            let _ = c.wait();
        }
        Some(_) => {
            log::warn!("[nexus] 进程 (pid={}) 未能按时退出，已释放句柄，由 Job Object 在应用退出时兜底", pid);
        }
        None => {}
    }
    // **复核存活**：直接子进程、接管进程、游离孙进程都再问一遍（用句柄，不受 pid 复用影响）。
    // 这一步才是"stop 到底成没成"的判据——taskkill 退出码非零可能只是进程本来就不存在
    // （那正是"已经收干净了"），而一个安静的 taskkill 也可能什么都没杀成（CQ-25）
    if !exited {
        outcome.survivors.push(pid);
    }
    let mut watch: Vec<u32> = adopted.iter().map(|a| a.pid).collect();
    watch.extend(orphans.iter().copied());
    outcome.survivors.extend(survivors_of(&watch));
    // 认领到的句柄随 `adopted` 在这里 drop（CloseHandle）——进程对象随之释放，
    // 不留句柄泄漏
    outcome
}

/// 进程启动时间（毫秒，距 UNIX 纪元）；进程不存在/无权限时返回 None。
///
/// 用途：pid 会被系统回收复用，"按 pid 杀进程"必须先确认这个 pid 还是当初那个进程。
/// 启动时间是 (pid, 启动时刻) 二元组的第二半——同一个 pid 两次启动的时间不可能相同，
/// 而存活中的进程启动时间不变，所以"记录时 == 现在"即可判定身份未变。
#[cfg(windows)]
pub(crate) fn process_start_time_millis(pid: u32) -> Option<u64> {
    // Win32 类型别名（按 clippy 命名习惯写，避免 upper_case_acronyms 抑制）
    type WinHandle = isize;
    type WinBool = i32;
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    extern "system" {
        fn OpenProcess(access: u32, inherit: WinBool, pid: u32) -> WinHandle;
        fn GetProcessTimes(
            handle: WinHandle,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> WinBool;
        fn CloseHandle(handle: WinHandle) -> WinBool;
    }
    // PROCESS_QUERY_LIMITED_INFORMATION：查启动时间所需的最小权限
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    // FILETIME 起点是 1601-01-01，与 UNIX 纪元差 11644473600 秒
    const FILETIME_UNIX_DIFF_SECS: u64 = 11_644_473_600;

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle == 0 {
            return None;
        }
        let mut creation = FileTime::default();
        let mut exit = FileTime::default();
        let mut kernel = FileTime::default();
        let mut user = FileTime::default();
        let ok = GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        // 100ns 单位 → 毫秒；跨度远超 u64 溢出范围（584 年），只做一次减法
        let ticks = ((creation.high as u64) << 32) | creation.low as u64;
        let secs = ticks / 10_000_000;
        if secs < FILETIME_UNIX_DIFF_SECS {
            return None;
        }
        Some((secs - FILETIME_UNIX_DIFF_SECS) * 1000 + (ticks % 10_000_000) / 10_000)
    }
}

#[cfg(not(windows))]
pub(crate) fn process_start_time_millis(_pid: u32) -> Option<u64> {
    None
}

/// 服务进程的创建标志：`CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP`。
///
/// 抽成常量是因为"挂起创建"那条路径（CQ-27①）必须传**完全相同**的一组标志——
/// `CommandExt::creation_flags` 是设置而不是追加，少写一位就会静默改变进程的创建方式。
#[cfg(windows)]
pub(crate) const SERVICE_CREATE_FLAGS: u32 = 0x08000000 | 0x00000200;
/// 先挂起创建、纳入 Job Object 之后再唤醒（见 `resume_suspended_child`）
#[cfg(windows)]
const CREATE_SUSPENDED: u32 = 0x00000004;

/// 唤醒一个以 `CREATE_SUSPENDED` 创建的进程（CQ-27①）。
///
/// 为什么需要它：`AssignProcessToJobObject` 只对**调用那一刻**已存在的进程生效，而
/// `cmd /C startup.bat` 这类服务毫秒级就能 `start` 出真身——那之后才被拉起的进程不在 Job 里，
/// Nexus 被强杀时残留。改成"挂起创建 → 纳管 → 唤醒"即可把这个窗口收到零。
///
/// 为什么按 pid 反查线程：`std::process::Child` 不暴露主线程句柄。刚创建的挂起进程只有
/// 一个线程，枚举它并 `ResumeThread` 即可。**任何一步失败都必须返回 Err**——挂起的进程
/// 不报错、不出声，只是一行输出都没有（调用方据此杀进程并如实报"启动失败"）。
#[cfg(windows)]
fn resume_suspended_child(pid: u32) -> Result<(), String> {
    const TH32CS_SNAPTHREAD: u32 = 0x0000_0004;
    const INVALID_HANDLE_VALUE: isize = -1;
    const THREAD_SUSPEND_RESUME: u32 = 0x0002;
    #[repr(C)]
    struct ThreadEntry32 {
        dw_size: u32,
        cnt_usage: u32,
        th32_thread_id: u32,
        th32_owner_process_id: u32,
        tp_base_pri: i32,
        tp_delta_pri: i32,
        dw_flags: u32,
    }
    // 手写 FFI：与本仓库 job_object.rs / final_path_of_handle 同一做法，不为两个函数引
    // windows-sys 的整块 Threading/ToolHelp feature（依赖变更要过审计与 CI 同款核对）
    extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> isize;
        fn Thread32First(snapshot: isize, entry: *mut ThreadEntry32) -> i32;
        fn Thread32Next(snapshot: isize, entry: *mut ThreadEntry32) -> i32;
        fn OpenThread(access: u32, inherit: i32, thread_id: u32) -> isize;
        fn ResumeThread(handle: isize) -> u32;
        fn CloseHandle(handle: isize) -> i32;
    }
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err("枚举线程失败（快照创建失败）".into());
        }
        let mut entry: ThreadEntry32 = std::mem::zeroed();
        entry.dw_size = std::mem::size_of::<ThreadEntry32>() as u32;
        let mut resumed = false;
        let mut ok = Thread32First(snapshot, &mut entry) != 0;
        while ok {
            if entry.th32_owner_process_id == pid {
                let handle = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32_thread_id);
                if handle != 0 {
                    // 返回先前的挂起计数；u32::MAX = 失败
                    let prev = ResumeThread(handle);
                    CloseHandle(handle);
                    if prev == u32::MAX {
                        CloseHandle(snapshot);
                        return Err("唤醒失败（ResumeThread 返回失败）".into());
                    }
                    resumed = true;
                }
            }
            ok = Thread32Next(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        if !resumed {
            return Err("没找到该进程的线程，未能唤醒".into());
        }
    }
    Ok(())
}

/// 终止进程树（taskkill /T /F）。工具命令超时等场景也需要，故设为 pub(crate)
pub(crate) fn kill_process_tree(pid: u32) -> Result<(), String> {
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let child = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            // 跳过"当前目录优先"搜索（SEC-14，与 `build_internal_command` 同口径）：
            // taskkill 是我们自己拉起的系统工具、不承载用户 shell，关掉这一级没有副作用，
            // 却能避免"CWD 恰好是一个不可信仓库"时执行到那里预置的 taskkill.exe
            .env("NoDefaultCurrentDirectoryInExePath", "1")
            // stderr **留管道**：原实现是 `Stdio::null()`，于是"拒绝访问/进程不存在/被安全
            // 软件拦截"全都拿不到，只剩一句"未按时退出"（CQ-25）。失败要说得清原因
            .stdout(Stdio::null()).stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("启动 taskkill 失败 (pid={}): {}", pid, e))?;
        let out = child
            .wait_with_output()
            .map_err(|e| format!("等待 taskkill 失败 (pid={}): {}", pid, e))?;
        if !out.status.success() {
            // 注意：进程本来就不存在时 taskkill 也返回非零。**这不代表收尸失败**——
            // 判"有没有收干净"的是调用方对存活状态的复核（见 `CleanupOutcome`），
            // 这里的返回值只用于把原因带到错误信息里
            return Err(format!(
                "taskkill 退出码 {:?} (pid={}): {}",
                out.status.code(),
                pid,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    }
    #[cfg(unix)]
    {
        let mut first_err: Option<String> = None;
        match Command::new("kill").args(["-TERM", &format!("-{}", pid)])
            .stdout(Stdio::null()).stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => { let _ = child.wait(); }
            Err(e) => first_err = Some(format!("kill -TERM 失败 (pid={}): {}", pid, e)),
        }
        std::thread::sleep(Duration::from_millis(300));
        match Command::new("kill").args(["-KILL", &format!("-{}", pid)])
            .stdout(Stdio::null()).stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => { let _ = child.wait(); }
            Err(e) => {
                return Err(match first_err {
                    Some(prev) => format!("{}；kill -KILL 失败 (pid={}): {}", prev, pid, e),
                    None => format!("kill -KILL 失败 (pid={}): {}", pid, e),
                })
            }
        }
        if let Some(e) = first_err { log::warn!("[nexus] {}", e); }
        Ok(())
    }
}

/// 构建子进程命令
///
/// 安全设计：`command_str` 来自用户在项目中配置的服务命令。
/// 信任边界（写清楚，避免高估白名单的覆盖范围）：**文件 API 的白名单不约束执行面**——
/// 服务命令本身就是用户写的 shell 串，`cwd` 与 `watch_paths` 已按 SEC-1 纳入范围校验，
/// 但"命令里可以写什么"不受限制（用户只能管理自己的项目，命令在用户自己的机器上以用户身份运行）。
/// 因此这条路径不做 NoDefaultCurrentDirectoryInExePath 收口（见 `build_internal_command`），
/// 也不该被当作沙箱：真正的隔离要靠 Job Object（进程随应用退出）+ 路径白名单（文件 API）。
/// Windows 上统一通过 cmd /C 执行，确保 npm/pnpm 等脚本能正确解析。
pub fn build_command(command_str: &str) -> Command {
    #[cfg(windows)]
    {
        let mut c = Command::new("cmd");
        c.args(["/C", command_str]);
        c.creation_flags(SERVICE_CREATE_FLAGS);
        // 补上"启动之后才装的东西"（见 core::winenv）：Windows 只在进程启动时复制一份
        // 环境块，而用户可能是在 Nexus 运行期间装 nvm / 装 Node 的——不补的话，服务里的
        // `npm run dev`、`npm install -g dsh` 都会报"找不到 npm"，直到重启 Nexus
        crate::core::winenv::apply(&mut c);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.args(["-c", command_str]);
        c.process_group(0);
        c
    }
}

/// 构建 Nexus **自己**发起的命令（`dsh` / `npm` 等内部 CLI）。
///
/// 与 `build_command` 的唯一差别：设置 `NoDefaultCurrentDirectoryInExePath=1`，
/// 让 Windows 在解析可执行文件名时**跳过当前目录**。
///
/// 攻击面（SEC-12）：`dsh web` 的工作目录是项目目录（AI 会话根），
/// 而 cmd 默认先搜当前目录——项目里放一个 `dsh.cmd` 就会被优先于 PATH 上的真 dsh 执行，
/// "克隆一个不受信任的仓库 + 打开 AI 面板"即可触发任意命令执行。
///
/// 为什么不给用户配置的服务命令也加：`gradlew`/`mvnw` 这类**项目内**启动脚本按裸名调用
/// 是常见且正当的用法，禁掉当前目录搜索会让它们失效（用户得改写成 `.\gradlew`）。
/// 用户命令本就是用户自己写的 shell 串，那条路径的信任边界见 `build_command`。
pub fn build_internal_command(command_str: &str) -> Command {
    let mut c = build_command(command_str);
    // 文档化开关（NT 4.0 起支持）：cmd 与 CreateProcess 不再把当前目录当搜索路径
    c.env("NoDefaultCurrentDirectoryInExePath", "1");
    c
}

/// 跨模块共用的测试夹具（ARCH-23）：`core::process` 与 `core::logfollow` 的测试都要用。
///
/// 为什么不放进各自的 `mod tests`：它是 `LogSink` 的测试替身与"另开窗口"的替身脚本，
/// 两边的用例都要真起进程 / 真收批次，各留一份就会各自漂移。
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// 测试用日志 sink：记录收到的批次。
    /// ARCH-14 把 `AppHandle` 换成这个闭包之后，`start()` 的全部路径才第一次可测。
    pub(crate) fn test_sink() -> (LogSink, Arc<Mutex<Vec<ServiceLogBatchPayload>>>) {
        let seen: Arc<Mutex<Vec<ServiceLogBatchPayload>>> = Arc::new(Mutex::new(Vec::new()));
        let s = Arc::clone(&seen);
        (Arc::new(move |p| {
            if let Ok(mut v) = s.lock() {
                v.push(p);
            }
        }), seen)
    }

    /// 写一个替身脚本：模拟 `catalina.bat` 的 `:doStart` —— 真正干活的进程被 cmd 内建
    /// 命令 `start` 另开一个控制台跑掉（见 `core::winproc` 的模块说明），脚本自己随即结束。
    /// `hold_secs > 0` 时脚本额外存活一段时间（用于测"父进程还活着时的走查"）。
    ///
    /// 为什么用 .bat 而不是直接把 `start "标题" "…"` 当命令串传：
    /// 服务命令是经 `cmd /C <命令串>` 执行的，带引号的串要穿过 Rust 的 argv 转义再被
    /// cmd 解析一遍，两边规则不同（实测这种形态下 `start` 根本跑不起来）。
    /// 真实场景（Tomcat/ActiveMQ）也都是 .bat 文件，这样写同时更贴近实际。
    /// 内容**只用 ASCII**：中文在 GBK 代码页下会被解成乱码（见候选区那条）。
    #[cfg(windows)]
    pub(crate) fn write_detach_bat(tag: &str, hold_secs: u32) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("nexus_test_{}_{}.bat", tag, std::process::id()));
        let mut body = String::from("@echo off\r\n");
        body.push_str("start \"NexusTestOrphan\" \"%COMSPEC%\" /c \"ping -n 20 127.0.0.1 >nul\"\r\n");
        if hold_secs > 0 {
            body.push_str(&format!("ping -n {} 127.0.0.1 >nul\r\n", hold_secs));
        }
        std::fs::write(&path, body).expect("应当能写替身脚本");
        path
    }

    /// 轮询到认领发生（真实路径由 `running()` 驱动），返回认领到的 (pid, 映像名)
    #[cfg(windows)]
    pub(crate) fn wait_for_adoption(mgr: &ProcessManager, key: &str) -> Vec<(u32, String)> {
        for _ in 0..100 {
            let _ = mgr.running();
            let pids = mgr.adopted_pids_for_test(key);
            if !pids.is_empty() {
                return pids;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Vec::new()
    }
}

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    // 常量随日志缓冲/文本层拆到了 core::logpipe / core::logtext（这里只被断言引用）
    use crate::core::logpipe::{MAX_LOG_BYTES, MAX_LOG_LINES};
    use crate::core::logtext::MAX_LINE_BYTES;
    use super::test_support::test_sink;
    #[cfg(windows)]
    use super::test_support::{wait_for_adoption, write_detach_bat};

    #[test]
    fn test_kill_process_tree_invalid_pid() {
        // 测试无效 PID 不会 panic
        // 不存在的 pid：taskkill 会返回非零，这条用例只要求不 panic（返回值这里不关心）
        let _ = kill_process_tree(999999999);
    }

    // ── ProcessManager 生命周期（不依赖 AppHandle 的部分）──────────────
    //
    // 为什么这些能测：spawn 需要 AppHandle（要向上 emit 事件），但"没有进程时"的
    // 生命周期语义——停止不存在的服务、日志缓冲的序号/裁剪/清理、运行表快照——
    // 都只依赖内部状态，正是历史上出过错的地方（stale pid、快照覆盖实时流）。

    /// CQ-27①：有 Job Object 时服务以**挂起态**创建，纳管后再唤醒。
    ///
    /// 判据必须是"真的跑起来了"：唤醒失败的表现是进程挂在创建态——**不报错、也没有任何输出**，
    /// 所以这里启动一条真命令并断言它把输出写进了日志缓冲（只断言 start() 返回 Ok 是骗人的）。
    #[cfg(windows)]
    #[test]
    fn test_suspended_creation_with_job_still_produces_output() {
        let job = match super::super::job_object::JobObject::new() {
            Ok(j) => Arc::new(j),
            Err(e) => {
                eprintln!("跳过：本机创建 Job Object 失败（{}）", e);
                return;
            }
        };
        let mgr = ProcessManager::with_job(job);
        let key = "suspend-1";
        mgr.start(ServiceSpawn {
            project_id: "p1", service_id: key, name: "挂起创建",
            command: "echo suspended-ok", cwd: ".", env_vars: &[],
            log_sink: Arc::new(|_| {}),
        }).expect("启动应当成功（挂起→纳管→唤醒）");

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if mgr.get_logs(key).iter().any(|l| l.text.contains("suspended-ok")) { break; }
            if std::time::Instant::now() > deadline {
                let texts: Vec<String> = mgr.get_logs(key).iter().map(|l| l.text.clone()).collect();
                panic!("挂起创建的进程没有被唤醒（一直没有输出），日志: {:?}", texts);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = mgr.stop(key);
    }

    #[test]
    fn test_stop_unstarted_service_is_idempotent() {
        let mgr = ProcessManager::new();
        // 未启动过的服务：stop 不该报错（前端会在"以为在跑"时点停止，报错会让用户困惑）
        assert!(mgr.stop("never-started").is_ok());
        assert!(mgr.running().is_empty());
        assert!(mgr.get_logs("never-started").is_empty());
        assert!(mgr.failed().is_empty());
    }

    #[test]
    fn test_log_buffer_assigns_monotonic_seq_and_prefixes_platform_marks() {
        let mgr = ProcessManager::new();
        mgr.append_system_line("svc-1", "第一条".into());
        mgr.append_system_line("svc-1", "第二条".into());
        let lines = mgr.get_logs("svc-1");
        assert_eq!(lines.len(), 2);
        // 序号严格递增：前端按 seq 做幂等合并（快照与实时流交错时靠它收敛）
        assert!(lines[0].seq < lines[1].seq, "序号必须单调递增");
        // 系统行带平台标记前缀，且计入同一缓冲（与真实输出混排时不丢顺序）
        assert!(lines[0].text.contains("第一条"), "实际文本: {}", lines[0].text);
    }

    #[test]
    fn test_log_buffer_is_per_service_and_bounded() {
        let mgr = ProcessManager::new();
        for i in 0..(MAX_LOG_LINES + 50) {
            mgr.append_system_line("svc-2", format!("line-{}", i));
        }
        let lines = mgr.get_logs("svc-2");
        assert_eq!(lines.len(), MAX_LOG_LINES, "每条服务的缓冲有上限（IDEA Console 式只留最新）");
        // 保留的是**最新**的若干行：末尾必须是最后写入的那条
        assert!(lines.last().unwrap().text.contains(&format!("line-{}", MAX_LOG_LINES + 49)));
        // 不同服务互不影响（key 隔离）
        assert!(mgr.get_logs("svc-3").is_empty());
    }

    /// PERF-17 的回归：**字节**上限必须与行数上限同时生效。
    ///
    /// 形状取自审计：每行都顶到单行上限（8KB）时，行数上限挡不住膨胀——
    /// 2000 行 × 8KB = 单服务 16MB，50 个服务 = 约 800MB。
    /// 反证方式：把 `MAX_LOG_BYTES` 临时改成 `usize::MAX`，本测试应如期失败。
    #[test]
    fn test_log_buffer_is_byte_bounded_too() {
        let mgr = ProcessManager::new();
        let n = 400usize; // 400 × 8KB = 3.2MB > 2MB，必然触发裁剪
        for i in 0..n {
            mgr.append_system_line("svc-bytes", format!("{:0>width$}", i, width = MAX_LINE_BYTES));
        }
        let lines = mgr.get_logs("svc-bytes");
        let total: usize = lines.iter().map(|l| l.text.len()).sum();
        assert!(total <= MAX_LOG_BYTES, "总字节必须受上限约束，实际 {}", total);
        assert!(lines.len() < n, "应已按字节裁剪，实际仍有 {} 行", lines.len());
        assert!(lines.len() <= MAX_LOG_LINES, "行数上限仍须独立生效");
        assert!(
            lines.last().unwrap().text.ends_with(&format!("{:0>16}", n - 1)),
            "保留的必须是最新写入的那行"
        );
    }

    /// ARCH-14：`start()` 的"工作目录为空"分支此前零覆盖。
    #[test]
    fn test_start_rejects_empty_cwd() {
        let mgr = ProcessManager::new();
        let (sink, _seen) = test_sink();
        let err = mgr.start(ServiceSpawn {
            project_id: "p1", service_id: "s1", name: "n", command: "echo hi",
            cwd: "   ", env_vars: &[], log_sink: sink,
        }).unwrap_err();
        assert!(err.contains("工作目录为空"), "实际错误: {}", err);
        // 被拒绝的启动不得留下任何痕迹（失败表/日志缓冲都该是干净的）
        assert!(mgr.failed().is_empty(), "空 cwd 是配置错误，不该记成「服务启动失败」");
        assert!(mgr.get_logs("s1").is_empty(), "拒绝路径不该写日志缓冲");
    }

    /// ARCH-14 解锁的核心用例：**spawn 失败路径**（此前 24 个测试无一走到 start）。
    ///
    /// 用"不存在的 cwd"触发：`cmd /C` 包着的命令本身总能 spawn，
    /// 但 `current_dir` 指向不存在的目录时 `spawn()` 会直接失败。
    #[test]
    fn test_start_records_failure_when_spawn_fails() {
        let mgr = ProcessManager::new();
        let (sink, _seen) = test_sink();
        let missing = std::env::temp_dir()
            .join(format!("nexus_ut_missing_cwd_{}", std::process::id()))
            .to_string_lossy()
            .to_string();
        let err = mgr.start(ServiceSpawn {
            project_id: "p1", service_id: "s1", name: "n", command: "echo hi",
            cwd: &missing, env_vars: &[], log_sink: sink,
        }).unwrap_err();
        assert!(err.starts_with("启动失败"), "实际错误: {}", err);

        // 失败必须同时落两处：失败表（卡片显示"失败"按钮）+ 日志缓冲（点开可见原因）
        let failed = mgr.failed();
        assert_eq!(failed.len(), 1, "失败必须进失败表");
        assert_eq!(failed[0].service_id, "s1");
        assert!(failed[0].exit_code.is_none(), "spawn 失败没有退出码");
        assert!(
            mgr.get_logs("s1").iter().any(|l| l.text.contains("启动失败")),
            "日志缓冲应记下失败原因"
        );
        assert!(
            !mgr.running().iter().any(|(_, id, _)| id == "s1"),
            "失败的启动不得留在运行表"
        );
    }

    /// NEW-17 的回归：同一 key 的 stop 必须等 start 的生命周期锁释放。
    ///
    /// 直接构造 start/stop 的真实交错需要进程调度运气（会变成 flaky 测试），
    /// 而"锁是否真的把同 key 串行化"是确定性可测的——测它即可。
    #[test]
    fn test_lifecycle_lock_serializes_same_key() {
        let mgr = Arc::new(ProcessManager::new());
        let lock = mgr.lifecycle_lock("svc-x");
        let guard = lock_lifecycle(&lock, "svc-x");

        let done = Arc::new(AtomicBool::new(false));
        let mgr2 = Arc::clone(&mgr);
        let done2 = Arc::clone(&done);
        let t = std::thread::spawn(move || {
            let _ = mgr2.stop("svc-x"); // 表里没有该 key：无锁时立即返回
            done2.store(true, Ordering::SeqCst);
        });

        std::thread::sleep(Duration::from_millis(150));
        assert!(!done.load(Ordering::SeqCst), "同 key 的 stop 不该在锁被持有期间完成");

        drop(guard);
        t.join().expect("线程应能结束");
        assert!(done.load(Ordering::SeqCst), "放锁后 stop 应立即完成");
    }

    #[test]
    fn test_failed_table_records_and_drains_exit_status() {
        let mgr = ProcessManager::new();
        // 直接注入失败记录（真实路径由 reader 线程在进程意外退出时写入）
        mgr.record_failure_for_test("svc-x", Some(1));
        let failed = mgr.failed();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].service_id, "svc-x");
        assert_eq!(failed[0].exit_code, Some(1));
        // 主动停止过的服务不该留在失败表里（前端据此显示"失败"按钮）
        mgr.stop("svc-x").unwrap();
        assert!(mgr.failed().iter().all(|f| f.service_id != "svc-x" || f.exit_code != Some(1)));
    }

    // ── 游离进程认领（"另开窗口"的服务，如 Tomcat 的 startup.bat）──────────
    //
    // 这一组是**真起进程**的：认领走查的是系统进程表，只有真进程能验证
    // （铁律 19：触碰进程/平台的路径必须真跑一次）。

    /// 等服务被判定为失败（真实路径：下一轮 `running()`）
    #[cfg(windows)]
    fn wait_for_failure(mgr: &ProcessManager) -> Vec<FailedService> {
        for _ in 0..100 {
            let _ = mgr.running();
            let failed = mgr.failed();
            if !failed.is_empty() {
                return failed;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Vec::new()
    }

    /// 等 pid 真的不再运行。
    ///
    /// 判据用**句柄**（`GetExitCodeProcess`）而不是 `process_start_time_millis`：
    /// 后者是"身份未变"的判据，进程被结束后只要还有句柄（例如控制台宿主）没关，
    /// `OpenProcess` 照样成功——实测就是这么被骗的：taskkill 已经报"没有找到进程"，
    /// 而按 pid 查启动时间仍能查到一个值。
    #[cfg(windows)]
    fn wait_gone(pid: u32) -> bool {
        for _ in 0..40 {
            let alive = super::super::winproc::OwnedProcess::open(pid)
                .map(|h| h.is_alive())
                .unwrap_or(false);
            if !alive {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// 认领 + 状态 + 停止：一条"另开窗口"的服务走完整生命周期
    ///
    /// 这是本功能的核心判据——修的就是三件事：
    /// 1) 直接子进程退出不等于服务退出（不谎报"已退出（退出码 0）"）
    /// 2) 日志里必须写明接管了谁（否则用户以为服务没了）
    /// 3) 「停止」要能真的把这个跑掉的进程结束掉
    #[cfg(windows)]
    #[test]
    fn test_adopts_detached_child_and_stop_kills_it() {
        let mgr = ProcessManager::new();
        let (sink, _seen) = test_sink();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let script = write_detach_bat("detach", 0);
        let command = script.to_string_lossy().to_string();
        mgr.start(ServiceSpawn {
            project_id: "p1", service_id: "detach-1", name: "n",
            command: &command, cwd: &cwd, env_vars: &[], log_sink: sink,
        }).expect("启动应当成功");

        let pids = wait_for_adoption(&mgr, "detach-1");
        assert!(!pids.is_empty(), "应当认领到另开窗口跑起来的进程（替身命令没生效？）");
        // 回归：控制台宿主不算服务在跑的东西（实测混进来过，还会让"杀没杀死"的
        // 两种判据打架，见 is_console_host 的说明）
        assert!(
            pids.iter().all(|(_, name)| !is_console_host(name)),
            "认领名单里不该有控制台宿主：{:?}", pids
        );

        let running: Vec<String> = mgr.running().into_iter().map(|(_, id, _)| id).collect();
        assert!(running.contains(&"detach-1".to_string()), "认领后应仍算运行中，实际 {:?}", running);
        assert!(mgr.failed().is_empty(), "认领成功不该记成失败");
        assert!(
            mgr.get_logs("detach-1").iter().any(|l| l.text.contains("另开窗口启动的进程仍在运行")),
            "认领必须写一行说明进日志，实际: {:?}",
            mgr.get_logs("detach-1").iter().map(|l| l.text.clone()).collect::<Vec<_>>()
        );

        // 停止：认领到的进程必须一起结束（否则就是"点了停止，Tomcat 还在跑"）
        mgr.stop("detach-1").expect("停止应当成功");
        for (pid, name) in &pids {
            assert!(wait_gone(*pid), "停止后 pid {} ({}) 仍在运行——认领的游离进程没被结束", pid, name);
        }
        assert!(!mgr.running().iter().any(|(_, id, _)| id == "detach-1"), "停止后不该还在运行列表");
    }

    /// 回归：没有游离后代的秒退服务照旧判失败（认领不能把它"救活"）
    ///
    /// 系统里此刻有大量无关进程（测试进程自己、cmd、各种系统服务），
    /// 父链走查必须一个都不认——这条同时钉住了"认领不会误伤别人的进程"。
    #[cfg(windows)]
    #[test]
    fn test_quick_exit_without_descendants_still_fails() {
        let mgr = ProcessManager::new();
        let (sink, _seen) = test_sink();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        mgr.start(ServiceSpawn {
            project_id: "p1", service_id: "quick-1", name: "n",
            command: "exit /b 3", cwd: &cwd, env_vars: &[], log_sink: sink,
        }).expect("启动应当成功");

        let failed = wait_for_failure(&mgr);
        assert_eq!(failed.len(), 1, "秒退服务应记为失败，实际 {:?}", failed.iter().map(|f| &f.service_id).collect::<Vec<_>>());
        assert_eq!(failed[0].exit_code, Some(3), "退出码应原样带出");
        assert!(mgr.adopted_pids_for_test("quick-1").is_empty(), "没有游离后代时不该认领任何东西");
    }

    /// 启动时刻校验：候选的创建时刻早于我们那条子进程时，一律不认领（防 pid 复用）
    ///
    /// 直接测走查函数而不是经 `running()`：这里要的是"同一个候选，只因为时间下限不同，
    /// 一个被认领、一个被挡掉"这一对照，走管理器拿不到这么干净的对照组。
    #[cfg(windows)]
    #[test]
    fn test_adoption_rejects_candidates_created_before_child() {
        // 父进程自己活 4 秒（脚本末尾那条 ping），期间 `start` 出来的进程是它的游离后代
        let script = write_detach_bat("guard", 4);
        let mut parent = std::process::Command::new("cmd")
            .args(["/C", &script.to_string_lossy()])
            .stdout(Stdio::null()).stderr(Stdio::null())
            .spawn().expect("应当能起替身父进程");
        let parent_pid = parent.id();

        // 下限 0 = 不做时间校验：应当找到游离后代
        let mut found = Vec::new();
        for _ in 0..40 {
            found = adopt_survivors(parent_pid, 0);
            if !found.is_empty() { break; }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(!found.is_empty(), "下限为 0 时应当认出游离后代");

        // 下限拉满 = 要求候选比 u64::MAX 还晚创建：不存在，必须一个都不认
        let rejected = adopt_survivors(parent_pid, u64::MAX);
        assert!(rejected.is_empty(), "时间校验没挡住：{:?}", rejected.iter().map(|a| a.pid).collect::<Vec<_>>());

        // 收尾：认领到的句柄随 drop 关闭，进程按 pid 结束
        let pids: Vec<u32> = found.iter().map(|a| a.pid).collect();
        drop(found);
        for pid in pids { let _ = kill_process_tree(pid); }
        let _ = parent.kill();
        let _ = parent.wait();
    }

    /// `wait_until_gone`：复查到"全没了"就提前返回，不把超时耗满。
    ///
    /// 这条钉的是"白等"那一侧：实现若写成"先睡满超时再查一次"，用户点重启会白卡 3 秒。
    #[test]
    fn test_wait_until_gone_returns_early_when_all_dead() {
        let mut calls = 0;
        let started = Instant::now();
        let left = wait_until_gone(
            vec![11, 22],
            // 超时给得远大于实际需要：实现若睡满超时，耗时会明显偏离
            Duration::from_millis(1000),
            Duration::from_millis(1),
            |pids| {
                calls += 1;
                if calls >= 2 { Vec::new() } else { pids.to_vec() }
            },
        );
        assert!(left.is_empty(), "全部退出后应当返回空，实际 {:?}", left);
        assert_eq!(calls, 2, "应当在复查到空的那一刻就停，而不是继续轮询");
        assert!(started.elapsed() < Duration::from_millis(500), "不该把超时耗满");
    }

    /// 超时：仍活着的 pid 原样交回去（调用方据此写出"可能与它们抢端口"）。
    ///
    /// 这条钉的是另一侧：不返回 survivors 的话，上层就写不出原因，用户只看到"失败"。
    #[test]
    fn test_wait_until_gone_returns_survivors_on_timeout() {
        let started = Instant::now();
        let mut calls = 0;
        let left = wait_until_gone(
            vec![7],
            Duration::from_millis(20),
            Duration::from_millis(1),
            |pids| {
                calls += 1;   // 永远不死
                pids.to_vec()
            },
        );
        assert_eq!(left, vec![7], "超时后应当把仍存活的 pid 交回去");
        assert!(calls > 1, "超时前应当轮询过多次，实际 {}", calls);
        assert!(started.elapsed() < Duration::from_secs(5), "等待时间失控（疑似死循环）");
    }

    /// 空输入：一次存活查询都不该发生（列表为空时立刻返回，也不该白睡一拍）。
    #[test]
    fn test_wait_until_gone_empty_input_does_not_poll() {
        let mut calls = 0;
        let left = wait_until_gone(
            Vec::new(),
            Duration::from_millis(50),
            Duration::from_millis(1),
            |pids| {
                calls += 1;
                pids.to_vec()
            },
        );
        assert!(left.is_empty());
        assert_eq!(calls, 0, "空列表不该触发任何一次存活查询");
    }
}


