//! 登录前启动器（`Nexus.exe --boot-runner --manifest <path>`）。
//!
//! 这个入口**不初始化 Tauri**：不开窗口、不注册单实例插件、不加载 WebView。
//! 它只做四件事：读 manifest、按顺序拉起服务、把输出写进文件、把状态写进 state.json。
//!
//! 为什么和 GUI 同一个 exe（`--boot-runner` 分支）而不是第二个二进制：
//! v1 优先让功能先跑起来——同一 exe 不需要改安装包，任务路径也天然稳定。
//! 代价是更新时 runner 会锁住 Nexus.exe；这属于已知限制（更新前先停启动器）。
//!
//! 所有权：runner 是开机启动服务的**唯一拥有者**。GUI 的 `ProcessManager` 永远不会
//! 收养这些进程，因此关闭 Nexus 不会停掉它们；停止/重启必须经文件命令队列发到这里。

#[cfg(windows)]
mod imp {
    use std::collections::{BTreeMap, HashMap};
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Stdio};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use serde::{Deserialize, Serialize};

    use crate::core::boot::{
        self, BootCheck, BootCommand, BootCommandResult, BootManifest, BootRunnerState,
        BootService, BootServiceState, MANIFEST_VERSION, STATE_VERSION,
    };

    /// 服务日志轮转阈值（单文件）
    const LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;
    /// runner 自身日志轮转阈值
    const RUNNER_LOG_MAX_BYTES: u64 = 2 * 1024 * 1024;
    /// 心跳间隔
    const HEARTBEAT: Duration = Duration::from_secs(2);
    /// 命令轮询间隔
    const COMMAND_POLL: Duration = Duration::from_millis(250);
    /// 停止服务时等进程退出的上限
    const STOP_WAIT: Duration = Duration::from_secs(3);

    struct RunnerLog {
        file: Mutex<std::fs::File>,
    }

    impl RunnerLog {
        fn open(path: &Path) -> Result<Self, String> {
            if let Ok(meta) = std::fs::metadata(path) {
                if meta.len() > RUNNER_LOG_MAX_BYTES {
                    let rotated = path.with_extension("log.1");
                    let _ = std::fs::remove_file(&rotated);
                    let _ = std::fs::rename(path, &rotated);
                }
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("创建 runner 日志目录失败: {}", e))?;
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| format!("打开 runner 日志失败 {}: {}", path.display(), e))?;
            Ok(Self { file: Mutex::new(file) })
        }

        fn info(&self, msg: &str) {
            if let Ok(mut f) = self.file.lock() {
                let _ = writeln!(f, "{} [runner] {}", chrono::Utc::now().to_rfc3339(), msg);
            }
        }
    }

    struct RunningChild {
        /// 直接子进程。`startup.bat`/`start` 这类命令会另开窗口后立即退出，
        /// 此时置 None，服务是否仍在跑由 `adopted` 判定（与 GUI 的 ProcessManager 同一套逻辑）。
        child: Option<Child>,
        /// 直接子进程的启动时刻（ms）：认领游离后代时排除 pid 复用
        child_started_at: u64,
        /// 直接子进程退出后接管的存活后代
        adopted: Vec<crate::core::process::AdoptedChild>,
        name: String,
        log_path: String,
    }

    struct Runner {
        manifest: BootManifest,
        manifest_path: PathBuf,
        /// **manifest 所在目录**，不是 `data_dir()`。
        ///
        /// S4U 登录时用户配置单元可能没加载，`dirs::home_dir()` 未必指向真实用户目录；
        /// 而 manifest 路径是计划任务里写死的绝对路径，它才是唯一可靠的锚点。
        /// state/log/commands 全部挂在它下面，GUI 才能读到同一份数据。
        boot_dir: PathBuf,
        state: BootRunnerState,
        running: HashMap<String, RunningChild>,
        log: Arc<RunnerLog>,
        #[cfg(windows)]
        job: Option<crate::core::job_object::JobObject>,
        shutdown: bool,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct LockInfo {
        pid: u32,
        started_at_ms: u64,
    }

    pub fn run() {
        if let Err(e) = real_main() {
            eprintln!("[nexus-boot-runner] {}", e);
            std::process::exit(1);
        }
    }

    fn arg_value(name: &str) -> Option<String> {
        let args: Vec<String> = std::env::args().collect();
        let mut i = 0;
        while i < args.len() {
            if args[i] == name {
                return args.get(i + 1).cloned();
            }
            if let Some(v) = args[i].strip_prefix(&format!("{}=", name)) {
                return Some(v.to_string());
            }
            i += 1;
        }
        None
    }

    fn real_main() -> Result<(), String> {
        let Some(manifest_path) = arg_value("--manifest").map(PathBuf::from) else {
            return Err("缺少 --manifest 参数（任务 action 应由安装命令生成）".into());
        };
        let mode = arg_value("--mode").unwrap_or_else(|| "s4u".to_string());
        if mode != "s4u" && mode != "password" {
            return Err(format!("未知的 --mode: {}（只支持 s4u / password）", mode));
        }
        let boot_dir = manifest_path
            .parent()
            .ok_or_else(|| format!("manifest 没有父目录: {}", manifest_path.display()))?
            .to_path_buf();
        std::fs::create_dir_all(&boot_dir).map_err(|e| format!("创建 boot 目录失败: {}", e))?;
        let log = Arc::new(RunnerLog::open(&boot_dir.join("runner.log"))?);

        // 单实例：同一份 manifest 只允许一个 runner。
        // 用 pid + 进程启动时刻复核，避免 pid 复用把“旧锁”误判成“还活着”。
        let my_pid = std::process::id();
        let my_started = crate::core::process::process_start_time_millis(my_pid).unwrap_or(0);
        let lock_path = boot_dir.join("runner.lock");
        if let Ok(bytes) = std::fs::read(&lock_path) {
            if let Ok(old) = serde_json::from_slice::<LockInfo>(&bytes) {
                let alive = crate::core::winproc::OwnedProcess::open(old.pid)
                    .map(|p| p.is_alive()).unwrap_or(false);
                let same = crate::core::process::process_start_time_millis(old.pid)
                    .map(|t| t == old.started_at_ms).unwrap_or(false);
                if alive && same {
                    log.info(&format!("已有 runner 在运行 (pid={})，本实例退出", old.pid));
                    return Ok(());
                }
            }
        }
        boot::write_atomic(
            &lock_path,
            serde_json::to_vec(&LockInfo { pid: my_pid, started_at_ms: my_started })
                .map_err(|e| e.to_string())?.as_slice(),
        )?;

        let manifest = match read_manifest(&manifest_path) {
            Ok(m) => m,
            Err(e) => {
                log.info(&format!("读取 manifest 失败: {}", e));
                return Err(e);
            }
        };
        let log = Arc::clone(&log);
        log.info(&format!(
            "启动：pid={} manifest={} services={}",
            my_pid, manifest_path.display(), manifest.services.len()
        ));

        #[cfg(windows)]
        let job = match crate::core::job_object::JobObject::new() {
            Ok(j) => Some(j),
            Err(e) => {
                // 没有 Job 时仍可运行，但 runner 被杀后服务会残留；必须留痕
                log.info(&format!("⚠ 创建 Job Object 失败（异常退出时服务可能残留）: {}", e));
                None
            }
        };

        let mut runner = Runner {
            state: init_state(&manifest, my_pid, my_started, &mode),
            manifest,
            manifest_path,
            boot_dir: boot_dir.clone(),
            running: HashMap::new(),
            log,
            #[cfg(windows)]
            job,
            shutdown: false,
        };
        runner.write_state();

        // 没有开机服务时不要常驻：写一份空状态、释放锁后正常退出。
        // 否则任务会一直挂着一个没有用的 runner，且“停止/卸载”还得先找到它。
        if runner.manifest.services.is_empty() {
            runner.log.info("manifest 没有服务，runner 退出");
            // pid=0 = 干净退出：GUI 据此显示“已停止”，而不是把最后一次心跳当成“无响应”
            runner.state.runner_pid = 0;
            runner.write_state();
            let _ = std::fs::remove_file(boot_dir.join("runner.lock"));
            return Ok(());
        }

        // 按 boot_order 串行启动；required 失败则阻断后续服务
        let services = runner.manifest.services.clone();
        let mut blocked = false;
        for svc in &services {
            if runner.shutdown {
                break;
            }
            if blocked {
                runner.set_state(&svc.id, |s| {
                    s.phase = "skipped".into();
                    s.error = "前一个必需服务未就绪，已跳过".into();
                });
                runner.log.info(&format!(
                    "跳过服务「{}」：前一个必需服务未就绪（顺序 {}）",
                    svc.name, svc.order
                ));
                continue;
            }
            let result = runner.start_service(svc);
            if let Err(e) = result {
                let required = svc.required;
                runner.set_state(&svc.id, |s| {
                    if s.phase != "ready" {
                        s.phase = "failed".into();
                        s.error = e.clone();
                    }
                });
                runner.log.info(&format!("服务「{}」启动失败: {}", svc.name, e));
                if required {
                    blocked = true;
                }
            }
        }
        runner.write_state();

        // 主循环：命令、子进程退出、心跳
        let mut last_heartbeat = Instant::now();
        while !runner.shutdown {
            runner.process_commands();
            runner.poll_children();
            if last_heartbeat.elapsed() >= HEARTBEAT {
                runner.write_state();
                last_heartbeat = Instant::now();
            }
            std::thread::sleep(COMMAND_POLL);
        }

        // 正常退出：先停服务，再写最终状态。
        // pid=0 = 干净退出：GUI 据此显示“已停止”，而不是把最后一次心跳当成“无响应”。
        runner.stop_all();
        runner.state.runner_pid = 0;
        runner.write_state();
        let _ = std::fs::remove_file(boot_dir.join("runner.lock"));
        runner.log.info("runner 已退出");
        Ok(())
    }

    fn read_manifest(path: &Path) -> Result<BootManifest, String> {
        let bytes = std::fs::read(path)
            .map_err(|e| format!("读取 manifest 失败 {}: {}", path.display(), e))?;
        let manifest: BootManifest = serde_json::from_slice(&bytes)
            .map_err(|e| format!("解析 manifest 失败: {}", e))?;
        if manifest.version != MANIFEST_VERSION {
            return Err(format!(
                "manifest 版本不兼容（文件 {}，当前 {}）",
                manifest.version, MANIFEST_VERSION
            ));
        }
        Ok(manifest)
    }

    fn init_state(
        manifest: &BootManifest,
        pid: u32,
        started_at_ms: u64,
        mode: &str,
    ) -> BootRunnerState {
        let now = chrono::Utc::now().to_rfc3339();
        let mut services = BTreeMap::new();
        for svc in &manifest.services {
            services.insert(svc.id.clone(), BootServiceState {
                phase: "pending".into(),
                pid: None,
                process_started_at: None,
                started_at: None,
                ready_at: None,
                exit_code: None,
                error: String::new(),
                log_path: svc.log_path.clone(),
                name: svc.name.clone(),
                project_id: svc.project_id.clone(),
            });
        }
        BootRunnerState {
            version: STATE_VERSION,
            runner_pid: pid,
            runner_started_at_ms: started_at_ms,
            started_at: now.clone(),
            heartbeat_at: now,
            mode: mode.to_string(),
            services,
        }
    }

    impl Runner {
        fn write_state(&mut self) {
            self.state.heartbeat_at = chrono::Utc::now().to_rfc3339();
            let bytes = match serde_json::to_vec_pretty(&self.state) {
                Ok(b) => b,
                Err(e) => {
                    self.log.info(&format!("序列化状态失败: {}", e));
                    return;
                }
            };
            if let Err(e) = boot::write_atomic(&self.boot_dir.join("state.json"), &bytes) {
                self.log.info(&format!("写状态失败: {}", e));
            }
        }

        fn set_state(&mut self, id: &str, f: impl FnOnce(&mut BootServiceState)) {
            if let Some(s) = self.state.services.get_mut(id) {
                f(s);
            }
        }

        fn find_service(&self, id: &str) -> Option<BootService> {
            self.manifest.services.iter().find(|s| s.id == id).cloned()
        }

        fn start_service(&mut self, svc: &BootService) -> Result<(), String> {
            if self.running.contains_key(&svc.id) {
                return Err("服务已在运行中".into());
            }
            let log_path = PathBuf::from(&svc.log_path);
            let (log_file, _) = open_service_log(&log_path, svc)?;
            let started_at = chrono::Utc::now().to_rfc3339();
            self.set_state(&svc.id, |s| {
                s.phase = "starting".into();
                s.started_at = Some(started_at.clone());
                s.error.clear();
                s.exit_code = None;
            });

            let mut cmd = crate::core::process::build_command(&svc.command);
            if !svc.cwd.trim().is_empty() {
                cmd.current_dir(&svc.cwd);
            }
            if !svc.env.is_empty() {
                cmd.envs(svc.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
            }
            let stdout = log_file.try_clone().map_err(|e| format!("复制日志句柄失败: {}", e))?;
            let stderr = log_file.try_clone().map_err(|e| format!("复制日志句柄失败: {}", e))?;
            let child = cmd
                .stdin(Stdio::null())
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(stderr))
                .spawn()
                .map_err(|e| format!("启动失败: {}", e))?;

            #[cfg(windows)]
            if let Some(job) = &self.job {
                if let Err(e) = job.assign_child(&child) {
                    self.log.info(&format!(
                        "⚠ 服务「{}」未纳入 Job Object（runner 异常退出时可能残留）: {}",
                        svc.name, e
                    ));
                }
            }

            let pid = child.id();
            let process_started_at = crate::core::process::process_start_time_millis(pid).unwrap_or(0);
            self.running.insert(svc.id.clone(), RunningChild {
                child: Some(child),
                child_started_at: process_started_at,
                adopted: Vec::new(),
                name: svc.name.clone(),
                log_path: svc.log_path.clone(),
            });
            self.set_state(&svc.id, |s| {
                s.pid = Some(pid);
                s.process_started_at = Some(process_started_at);
            });
            self.write_state();
            self.log.info(&format!(
                "启动服务「{}」pid={} command={}",
                svc.name, pid, crate::core::process::mask_command(&svc.command)
            ));

            // 就绪检查：失败不杀进程（它可能只是慢），但状态记为 failed 供界面显示
            let wait_started = Instant::now();
            let ready = self.wait_ready(svc);
            match ready {
                Ok(()) => {
                    self.set_state(&svc.id, |s| {
                        s.phase = "ready".into();
                        s.ready_at = Some(chrono::Utc::now().to_rfc3339());
                    });
                    self.write_state();
                    // 记下检查耗时：排查“为什么后面的服务半天没起来”时，这行是直接证据
                    self.log.info(&format!(
                        "服务「{}」已就绪（就绪检查耗时 {:.1}s）",
                        svc.name,
                        wait_started.elapsed().as_secs_f64()
                    ));
                    Ok(())
                }
                Err(e) => {
                    self.set_state(&svc.id, |s| {
                        s.phase = "failed".into();
                        s.error = e.clone();
                    });
                    self.write_state();
                    Err(e)
                }
            }
        }

        fn wait_ready(&mut self, svc: &BootService) -> Result<(), String> {
            let timeout = match &svc.check {
                BootCheck::None => return Ok(()),
                BootCheck::Delay { seconds } => Duration::from_secs(*seconds),
                BootCheck::Tcp { timeout_secs, .. } => Duration::from_secs(*timeout_secs),
                BootCheck::LogContains { timeout_secs, .. } => Duration::from_secs(*timeout_secs),
            };
            let deadline = Instant::now() + timeout;
            // 就绪等待可能几十秒；期间必须继续写心跳，否则 GUI 会把“正在启动”误判成
            // “启动器无响应”（runner_reachable 的心跳窗口是 10 秒）。
            let mut last_beat = Instant::now();
            loop {
                if let Some(code) = self.child_exit_code(&svc.id) {
                    return Err(format!("进程在就绪前退出（退出码: {:?}）", code));
                }
                let ok = match &svc.check {
                    BootCheck::None => true,
                    BootCheck::Delay { .. } => Instant::now() >= deadline,
                    BootCheck::Tcp { host, port, .. } => tcp_ready(host, *port),
                    BootCheck::LogContains { pattern, .. } => log_contains(&svc.log_path, pattern),
                };
                if ok {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(format!("就绪检查超时（{:?}）", timeout));
                }
                if last_beat.elapsed() >= HEARTBEAT {
                    self.write_state();
                    last_beat = Instant::now();
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }

        /// 子进程（含认领的后代）都退出时返回退出码；仍在运行返回 `None`。
        ///
        /// 直接子进程退出不代表服务结束：`startup.bat`/`start` 会另开窗口并立即返回，
        /// 真正的工作进程（java.exe 等）仍在跑。这里复用 `core::process` 的认领逻辑，
        /// 接管存活后代后继续把服务算作运行中。
        fn child_exit_code(&mut self, id: &str) -> Option<Option<i32>> {
            let direct = {
                let run = self.running.get_mut(id)?;
                match run.child.as_mut() {
                    Some(child) => match child.try_wait() {
                        Ok(Some(status)) => Some(status.code()),
                        _ => return None,
                    },
                    None => None,
                }
            };
            if let Some(code) = direct {
                if self.adopt_if_survivors(id) {
                    return None;
                }
                return Some(code);
            }
            let run = self.running.get(id)?;
            if run.adopted.iter().any(|a| a.handle.is_alive()) {
                None
            } else {
                Some(run.adopted.iter().find_map(|a| a.handle.exit_code()))
            }
        }

        /// 直接子进程已退出时认领仍存活的后代；返回 true = 服务应继续算运行中。
        fn adopt_if_survivors(&mut self, id: &str) -> bool {
            let (pid, started, name) = {
                let Some(run) = self.running.get(id) else { return false };
                let Some(child) = run.child.as_ref() else { return false };
                (child.id(), run.child_started_at, run.name.clone())
            };
            let adopted = crate::core::process::adopt_survivors(pid, started);
            if adopted.is_empty() {
                return false;
            }
            let who = adopted
                .iter()
                .map(|a| format!("{} (pid {})", a.name, a.pid))
                .collect::<Vec<_>>()
                .join(", ");
            let count = adopted.len();
            if let Some(run) = self.running.get_mut(id) {
                run.child = None;
                run.adopted = adopted;
            }
            self.log.info(&format!(
                "服务「{}」直接子进程已退出，已接管 {} 个仍在运行的进程：{}；按仍在运行处理",
                name, count, who,
            ));
            self.sync_state_pid(id);
            true
        }

        /// 把 state.pid/process_started_at 指向当前仍存活的代表进程
        /// （GUI 用这两个字段判断服务是否在跑、显示 PID）。
        fn sync_state_pid(&mut self, id: &str) {
            let leader = {
                let Some(run) = self.running.get(id) else { return };
                if let Some(child) = run.child.as_ref() {
                    Some((child.id(), run.child_started_at))
                } else {
                    run.adopted
                        .iter()
                        .find(|a| a.handle.is_alive())
                        .map(|a| (a.pid, crate::core::process::process_start_time_millis(a.pid).unwrap_or(0)))
                }
            };
            let Some((pid, started_at)) = leader else { return };
            let changed = self.state.services.get(id)
                .map(|s| s.pid != Some(pid) || s.process_started_at != Some(started_at))
                .unwrap_or(true);
            if !changed {
                return;
            }
            self.set_state(id, |s| {
                s.pid = Some(pid);
                s.process_started_at = Some(started_at);
            });
            self.write_state();
        }

        fn poll_children(&mut self) {
            let mut exited: Vec<(String, Option<i32>)> = Vec::new();
            let mut exited_direct: Vec<String> = Vec::new();
            let mut poll_errors: Vec<String> = Vec::new();
            for (id, run) in self.running.iter_mut() {
                match run.child.as_mut() {
                    Some(child) => match child.try_wait() {
                        Ok(Some(_)) => exited_direct.push(id.clone()),
                        Ok(None) => {}
                        Err(e) => poll_errors.push(format!("查询服务「{}」状态失败: {}", run.name, e)),
                    },
                    // 直接子进程已退出：接管的进程全退才算服务退出
                    None => {
                        if !run.adopted.iter().any(|a| a.handle.is_alive()) {
                            let code = run.adopted.iter().find_map(|a| a.handle.exit_code());
                            exited.push((id.clone(), code));
                        }
                    }
                }
            }
            for msg in poll_errors {
                self.log.info(&msg);
            }

            for id in exited_direct {
                // 先取直接子进程的退出码（认领失败时它就是服务的退出码）
                let code = self.running.get_mut(&id)
                    .and_then(|r| r.child.as_mut())
                    .and_then(|c| c.try_wait().ok().flatten())
                    .and_then(|s| s.code());
                if !self.adopt_if_survivors(&id) {
                    exited.push((id, code));
                }
            }
            // 接管后代表进程可能换了（原 leader 退出、其余仍活）：同步 state.pid
            let adopted_ids: Vec<String> = self.running.iter()
                .filter(|(_, r)| r.child.is_none() && !r.adopted.is_empty())
                .map(|(id, _)| id.clone())
                .collect();
            for id in adopted_ids {
                self.sync_state_pid(&id);
            }

            for (id, code) in exited {
                let run = self.running.remove(&id);
                let (name, log_path) = run
                    .map(|r| (r.name, r.log_path))
                    .unwrap_or_else(|| (id.clone(), String::new()));
                let note = if code == Some(0) { "已退出" } else { "意外退出" };
                self.log.info(&format!("服务「{}」{}（退出码: {:?}）", name, note, code));
                self.set_state(&id, |s| {
                    s.phase = "exited".into();
                    s.exit_code = code;
                    s.pid = None;
                    s.error = format!("进程{}（退出码: {:?}）", note, code);
                    s.log_path = log_path;
                });
            }
        }

        fn stop_service(&mut self, id: &str) -> Result<(), String> {
            let Some(mut run) = self.running.remove(id) else {
                self.set_state(id, |s| {
                    s.phase = "stopped".into();
                    s.pid = None;
                });
                return Ok(());
            };
            let mut kill_error = String::new();
            if let Some(child) = run.child.as_mut() {
                if let Err(e) = crate::core::process::kill_process_tree(child.id()) {
                    kill_error = e;
                }
            }
            // 直接子进程已经退出时，真正在跑的是认领的后代——必须一起结束，
            // 否则 Tomcat 这类“另开窗口”的服务点了停止还在跑。
            for a in &run.adopted {
                if a.handle.is_alive() {
                    if let Err(e) = crate::core::process::kill_process_tree(a.pid) {
                        if kill_error.is_empty() {
                            kill_error = e;
                        }
                    }
                }
            }
            let deadline = Instant::now() + STOP_WAIT;
            while Instant::now() < deadline {
                let child_dead = match run.child.as_mut() {
                    Some(child) => matches!(child.try_wait(), Ok(Some(_))),
                    None => true,
                };
                let adopted_dead = run.adopted.iter().all(|a| !a.handle.is_alive());
                if child_dead && adopted_dead {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let pid_log = run.child.as_ref().map(|c| c.id())
                .or_else(|| run.adopted.first().map(|a| a.pid))
                .unwrap_or(0);
            let code = run.child.as_mut()
                .and_then(|c| c.try_wait().ok().flatten())
                .and_then(|s| s.code())
                .or_else(|| run.adopted.iter().find_map(|a| a.handle.exit_code()));
            self.set_state(id, |s| {
                s.phase = "stopped".into();
                s.pid = None;
                s.exit_code = code;
                s.error = if kill_error.is_empty() { String::new() } else { format!("停止未完成: {}", kill_error) };
            });
            self.log.info(&format!("已停止服务「{}」pid={}", run.name, pid_log));
            self.write_state();
            Ok(())
        }

        fn stop_all(&mut self) {
            let ids: Vec<String> = self.running.keys().cloned().collect();
            for id in ids {
                let _ = self.stop_service(&id);
            }
        }

        fn restart_service(&mut self, id: &str) -> Result<(), String> {
            // 服务可能是刚被打开开机启动、而 GUI 的 reload 命令还没被处理到的；
            // 先按需重读一次 manifest，避免“配置已经保存、启动器却说不在清单里”。
            if self.find_service(id).is_none() {
                self.reload()?;
            }
            let Some(svc) = self.find_service(id) else {
                return Err(format!("服务 {} 不在开机启动 manifest 中", id));
            };
            self.stop_service(id)?;
            self.start_service(&svc)
        }

        fn reload(&mut self) -> Result<(), String> {
            let manifest = read_manifest(&self.manifest_path)?;
            // 保留运行中服务的状态；新服务补一条 pending
            for svc in &manifest.services {
                self.state.services.entry(svc.id.clone()).or_insert_with(|| BootServiceState {
                    phase: "pending".into(),
                    log_path: svc.log_path.clone(),
                    name: svc.name.clone(),
                    project_id: svc.project_id.clone(),
                    ..Default::default()
                });
            }
            self.manifest = manifest;
            self.log.info("manifest 已重新加载");
            self.write_state();
            Ok(())
        }

        fn process_commands(&mut self) {
            let dir = self.boot_dir.join("commands");
            let Ok(entries) = std::fs::read_dir(&dir) else { return };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&path) else { continue };
                let command: BootCommand = match serde_json::from_slice(&bytes) {
                    Ok(c) => c,
                    Err(e) => {
                        self.log.info(&format!("忽略非法命令文件 {}: {}", path.display(), e));
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                };
                let mut ok = true;
                let mut error = String::new();
                match command.op.as_str() {
                    "status" => {}
                    "start" => {
                        if let Some(id) = command.service_id.as_deref() {
                            if self.find_service(id).is_none() {
                                if let Err(e) = self.reload() {
                                    ok = false; error = e;
                                }
                            }
                            if ok {
                                if let Some(svc) = self.find_service(id) {
                                    if let Err(e) = self.start_service(&svc) {
                                        ok = false; error = e;
                                    }
                                } else {
                                    ok = false; error = format!("服务 {} 不在 manifest 中", id);
                                }
                            }
                        } else {
                            ok = false; error = "start 缺少 serviceId".into();
                        }
                    }
                    "stop" => {
                        if let Some(id) = command.service_id.as_deref() {
                            if let Err(e) = self.stop_service(id) {
                                ok = false; error = e;
                            }
                        } else {
                            // 没有 serviceId = 停止全部（但不退出 runner）
                            self.stop_all();
                        }
                    }
                    "restart" => {
                        if let Some(id) = command.service_id.as_deref() {
                            if let Err(e) = self.restart_service(id) {
                                ok = false; error = e;
                            }
                        } else {
                            ok = false; error = "restart 缺少 serviceId".into();
                        }
                    }
                    "reload" => {
                        if let Err(e) = self.reload() {
                            ok = false; error = e;
                        }
                    }
                    "shutdown" => {
                        self.shutdown = true;
                    }
                    other => {
                        ok = false; error = format!("未知命令: {}", other);
                    }
                }
                write_command_result(&self.boot_dir.join("results"), &command.id, ok, &error);
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    fn open_service_log(path: &Path, svc: &BootService) -> Result<(std::fs::File, PathBuf), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建服务日志目录失败: {}", e))?;
        }
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.len() > LOG_MAX_BYTES {
                let rotated = path.with_extension("log.1");
                let _ = std::fs::remove_file(&rotated);
                let _ = std::fs::rename(path, &rotated);
            }
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("打开服务日志失败 {}: {}", path.display(), e))?;
        // 头部**只用 ASCII**：服务输出可能是 GBK（中文 Windows 上的 Java/旧工具），
        // 而 GUI 按整段 UTF-8 → GB18030 回退解码；混入 UTF-8 中文会让 GBK 日志
        // 解码时头部变成乱码。名称/命令在界面上本来就有，这里只留可对账的 id/时间。
        let _ = writeln!(file, "=== nexus boot runner: service start ===");
        let _ = writeln!(file, "service_id: {}", svc.id);
        let _ = writeln!(file, "started_at: {}", chrono::Utc::now().to_rfc3339());
        let _ = file.flush();
        Ok((file, path.to_path_buf()))
    }

    fn tcp_ready(host: &str, port: u16) -> bool {
        use std::net::ToSocketAddrs;
        let addrs = match (host, port).to_socket_addrs() {
            Ok(a) => a,
            Err(_) => return false,
        };
        for addr in addrs {
            if std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok() {
                return true;
            }
        }
        false
    }

    fn log_contains(path: &str, pattern: &str) -> bool {
        let Ok(bytes) = std::fs::read(path) else { return false };
        // 只看尾部 1MB：日志可能已经很大，就绪关键字通常在最近输出里。
        // 用仓库统一解码口径（UTF-8 → GB18030 回退），中文 Windows 上的服务日志
        // 常是 GBK；直接 from_utf8_lossy 会让中文关键字永远匹配不到。
        let start = bytes.len().saturating_sub(1024 * 1024);
        let text = crate::core::textenc::decode_to_string(&bytes[start..]);
        text.contains(pattern)
    }

    fn write_command_result(results_dir: &Path, id: &str, ok: bool, error: &str) {
        let result = BootCommandResult {
            id: id.to_string(),
            ok,
            error: error.to_string(),
        };
        let _ = std::fs::create_dir_all(results_dir);
        let path = results_dir.join(format!("{}.json", id));
        if let Ok(bytes) = serde_json::to_vec(&result) {
            let _ = boot::write_atomic(&path, &bytes);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::collections::BTreeMap;

        /// 回归：`startup.bat` / `start` 这类命令的**直接子进程会立即退出**，
        /// 真正的工作进程另开窗口继续跑。启动器必须接管它并继续算运行中，
        /// 不能把服务标成“已退出/失败”（用户报的 Tomcat 场景）。
        #[test]
        fn test_detached_child_is_adopted_not_marked_exited() {
            let dir = std::env::temp_dir().join(format!("nexus_runner_adopt_{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("建临时目录");
            let log_path = dir.join("logs").join("s1.log");
            let svc = BootService {
                id: "s1".into(),
                project_id: "p1".into(),
                name: "TomcatLike".into(),
                // /b 不开新窗口，但 cmd 会立即退出、ping 继续跑——与 startup.bat 同形
                command: "start /b ping -n 10 127.0.0.1".into(),
                cwd: String::new(),
                env: BTreeMap::new(),
                // 用 delay 检查走一遍 wait_ready 里的“直接子进程退出 → 认领”路径
                check: BootCheck::Delay { seconds: 1 },
                required: false,
                order: 0,
                log_path: log_path.to_string_lossy().to_string(),
            };
            let manifest = BootManifest {
                version: MANIFEST_VERSION,
                generated_at: String::new(),
                env: BTreeMap::new(),
                services: vec![svc.clone()],
            };
            let log = Arc::new(RunnerLog::open(&dir.join("runner.log")).expect("打开日志"));
            let mut runner = Runner {
                state: init_state(&manifest, std::process::id(), 0, "s4u"),
                manifest,
                manifest_path: dir.join("manifest.json"),
                boot_dir: dir.clone(),
                running: HashMap::new(),
                log,
                job: None,
                shutdown: false,
            };

            runner.start_service(&svc).expect("启动服务");
            std::thread::sleep(Duration::from_millis(1200));
            runner.poll_children();

            let adopted: Vec<u32> = {
                let run = runner.running.get("s1")
                    .expect("直接子进程退出后应接管游离进程，而不是标记退出");
                assert!(!run.adopted.is_empty(), "应认领到 ping 进程");
                assert!(run.adopted.iter().any(|a| a.handle.is_alive()), "接管的进程应仍在运行");
                run.adopted.iter().map(|a| a.pid).collect()
            };
            let state = &runner.state.services["s1"];
            assert_eq!(state.phase, "ready");
            assert!(state.pid.is_some(), "state.pid 应指向接管的代表进程");

            // 取消开机启动 = 新 manifest 里没有它。reload 只更新清单，
            // **不能**顺手停掉当前已运行的实例（停止由用户显式操作）。
            let empty = BootManifest {
                version: MANIFEST_VERSION,
                generated_at: String::new(),
                env: BTreeMap::new(),
                services: Vec::new(),
            };
            std::fs::write(&runner.manifest_path, serde_json::to_vec(&empty).expect("序列化"))
                .expect("写新 manifest");
            runner.reload().expect("重新加载 manifest");
            assert!(
                runner.running.contains_key("s1"),
                "取消开机启动不应停止当前实例"
            );
            assert_eq!(runner.state.services["s1"].phase, "ready");

            runner.stop_service("s1").expect("停止服务");
            assert!(!runner.running.contains_key("s1"));
            for pid in adopted {
                for _ in 0..40 {
                    if crate::core::process::process_start_time_millis(pid).is_none() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                assert!(
                    crate::core::process::process_start_time_millis(pid).is_none(),
                    "停止后认领的进程仍在运行（pid {}）",
                    pid
                );
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

#[cfg(windows)]
pub use imp::run;

#[cfg(not(windows))]
pub fn run() {
    eprintln!("[nexus-boot-runner] 仅 Windows 支持");
    std::process::exit(1);
}
