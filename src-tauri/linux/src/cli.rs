//! `nexusctl` 子命令实现。

use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Parser, Subcommand};

use boot_core::boot::{
    runner_reachable, service_alive_in, service_log_path_in, BootRunnerState,
};
use boot_core::textenc::decode_to_string;

use crate::client;
use crate::config::{describe_ready, parse_env_arg, parse_ready, Config, Project, Service};
use crate::layout::Layout;
use crate::manifest;
use crate::output;
use crate::systemd;

#[derive(Parser)]
#[command(name = "nexusctl", version, about = "Nexus Linux 服务端管理 CLI")]
pub struct Cli {
    /// 安装/管理系统级服务（/etc/nexus、/var/lib/nexus，需要 root）
    #[arg(long, global = true)]
    system: bool,
    /// system 模式下服务进程的运行用户（默认 SUDO_USER）
    #[arg(long, global = true)]
    run_as: Option<String>,
    /// 配置文件路径（默认 ~/.config/nexus/nexus.toml 或 /etc/nexus/nexus.toml）
    #[arg(long, global = true, env = "NEXUS_CONFIG")]
    config: Option<PathBuf>,
    /// 数据目录（默认 ~/.nexus 或 /var/lib/nexus）
    #[arg(long, global = true, env = "NEXUS_DATA_DIR")]
    data_dir: Option<PathBuf>,
    /// 覆盖 nexus-boot-runner 路径（默认与 nexusctl 同目录）
    #[arg(long, global = true)]
    runner: Option<PathBuf>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 项目管理
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    /// 服务管理
    Service {
        #[command(subcommand)]
        cmd: ServiceCmd,
    },
    /// 开机启动器 / manifest / systemd
    Boot {
        #[command(subcommand)]
        cmd: BootCmd,
    },
    /// 查看当前使用的是哪套配置/数据/unit
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// 打印当前模式使用的配置、数据、unit 路径
    Path,
    /// 打印配置文件原文
    Show,
    /// 校验配置文件
    Validate,
}

#[derive(Subcommand)]
enum ProjectCmd {
    /// 列出项目
    List {
        #[arg(long)]
        json: bool,
    },
    /// 查看项目详情（路径、服务列表与状态）
    Show {
        /// 项目名
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// 添加项目（路径必须已存在）
    Add {
        /// 项目目录
        path: PathBuf,
        /// 项目名（默认取目录名；只允许字母/数字/-/_）
        #[arg(long)]
        name: Option<String>,
    },
    /// 删除项目（必须没有服务）
    Remove { name: String },
    /// 启动项目下全部服务（按 order 顺序）
    Start { name: String },
    /// 停止项目下全部服务（逆序）
    Stop { name: String },
    /// 重启项目下全部服务（先逆序停，再正序启）
    Restart { name: String },
}

#[derive(Subcommand)]
enum ServiceCmd {
    /// 列出服务与运行状态
    List {
        /// 只看某个项目下的服务
        #[arg(short = 'p', long)]
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// 查看某个服务的详情（启动命令、目录、环境变量、状态）
    Show {
        /// 项目名
        project: String,
        /// 服务名
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// 查看服务日志尾部
    Logs {
        /// 项目名
        project: String,
        /// 服务名
        name: String,
        #[arg(long, default_value_t = 200)]
        lines: usize,
    },
    /// 添加服务
    Add {
        /// 所属项目名（需先用 project add 创建）
        #[arg(short = 'p', long)]
        project: String,
        /// 服务名（用于 start/stop/logs；可与项目名不同）
        #[arg(short = 'n', long)]
        name: String,
        /// 启动命令（由 sh -c 在工作目录执行）
        #[arg(short = 'c', long)]
        command: String,
        /// 工作目录（默认项目路径）
        #[arg(short = 'w', long)]
        workdir: Option<String>,
        /// 环境变量，可重复：--env K=V
        #[arg(long = "env", value_name = "K=V")]
        env: Vec<String>,
        /// 开机自动启动
        #[arg(long)]
        boot: bool,
        /// 开机启动顺序（小的先启动）
        #[arg(long)]
        order: Option<i32>,
        /// 就绪检查：none / wait:5 / tcp:127.0.0.1:3000 / log:关键字 / JSON
        #[arg(long)]
        ready: Option<String>,
        /// 未就绪时阻断后续开机服务
        #[arg(long)]
        required: bool,
        /// 关闭“稳定运行后崩溃自动重启”
        #[arg(long)]
        no_crash_restart: bool,
    },
    /// 修改服务信息
    Set {
        /// 项目名
        project: String,
        /// 服务名
        name: String,
        #[arg(short = 'c', long)]
        command: Option<String>,
        /// 工作目录；传空串表示回退项目路径
        #[arg(short = 'w', long)]
        workdir: Option<String>,
        #[arg(long = "env", value_name = "K=V")]
        env: Vec<String>,
        /// 先清空已有 env，再应用 --env
        #[arg(long)]
        clear_env: bool,
        #[arg(long)]
        order: Option<i32>,
        /// 就绪检查；传空串表示清除
        #[arg(long)]
        ready: Option<String>,
        #[arg(long)]
        boot: bool,
        #[arg(long)]
        no_boot: bool,
        #[arg(long)]
        required: bool,
        #[arg(long)]
        no_required: bool,
        #[arg(long)]
        crash_restart: bool,
        #[arg(long)]
        no_crash_restart: bool,
    },
    /// 删除服务（先停止）
    Remove { project: String, name: String },
    Start { project: String, name: String },
    Stop { project: String, name: String },
    Restart { project: String, name: String },
    /// 设为开机自动启动（下次启动生效；现在启动用 start）
    EnableBoot { project: String, name: String },
    /// 取消开机自动启动（不会停止正在运行的实例）
    DisableBoot { project: String, name: String },
}

#[derive(Subcommand)]
enum BootCmd {
    /// 生成 manifest、写 systemd unit 并 enable --now
    Install,
    /// 停用并删除 systemd unit（保留配置与日志）
    Uninstall,
    /// 只打印将写入的 systemd unit（不安装；用于检查/调试）
    Unit,
    /// 停止 runner 及其管理的全部服务（unit 保留；下次开机/手动启动会再拉起）
    Shutdown,
    /// 启动已安装的 runner（不重新安装）
    Start,
    /// 停止 runner 与其管理的服务（unit 保留，仍保持开机启用）
    Stop,
    /// 重启 runner（会重启它管理的服务）
    Restart,
    /// 启用开机自启（不启动 runner）
    Enable,
    /// 禁用开机自启（不停止正在运行的 runner）
    Disable,
    /// 查看 runner 自身日志尾部
    RunnerLog {
        #[arg(long, default_value_t = 200)]
        lines: usize,
    },
    /// 重新生成 manifest 并通知 runner reload
    Sync,
    /// 启动器、unit、linger 与服务状态
    Status {
        #[arg(long)]
        json: bool,
    },
    /// 查看某个服务的日志尾部
    Logs {
        project: String,
        name: String,
        #[arg(long, default_value_t = 200)]
        lines: usize,
    },
}

pub fn run(cli: Cli) -> Result<(), String> {
    if cli.run_as.is_some() && !cli.system {
        return Err("--run-as 只在 --system 模式下有效".into());
    }
    if !cli.system && std::env::var("SUDO_USER").is_ok() {
        return Err(
            "user 模式不要用 sudo：请以目标用户身份运行。\
             服务器级安装请改用 `nexusctl --system --run-as <用户> ...`"
                .into(),
        );
    }
    let layout = Layout::resolve(cli.system, cli.config, cli.data_dir, cli.run_as)?;
    match cli.command {
        Cmd::Project { cmd } => project_cmd(&layout, cmd),
        Cmd::Service { cmd } => service_cmd(&layout, cmd),
        Cmd::Boot { cmd } => boot_cmd(&layout, cli.runner, cmd),
        Cmd::Config { cmd } => config_cmd(&layout, cmd),
    }
}

// ─── 项目 ──────────────────────────────────────────────────

fn project_cmd(layout: &Layout, cmd: ProjectCmd) -> Result<(), String> {
    match cmd {
        ProjectCmd::List { json } => {
            let cfg = Config::load(&layout.config_path)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "projects": cfg.projects.iter().map(|p| serde_json::json!({
                            "name": p.name,
                            "path": p.path,
                            "services": cfg.services.iter().filter(|s| s.project == p.name).count(),
                        })).collect::<Vec<_>>()
                    }))
                    .map_err(|e| e.to_string())?
                );
                return Ok(());
            }
            if cfg.projects.is_empty() {
                println!("（没有项目）");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = cfg
                .projects
                .iter()
                .map(|p| {
                    vec![
                        p.name.clone(),
                        p.path.clone(),
                        cfg.services
                            .iter()
                            .filter(|s| s.project == p.name)
                            .count()
                            .to_string(),
                    ]
                })
                .collect();
            output::table(&["NAME", "PATH", "SERVICES"], &rows);
            Ok(())
        }
        ProjectCmd::Show { name, json } => show_project(layout, &name, json),
        ProjectCmd::Add { path, name } => {
            let canonical = canonical_dir(&path, "项目路径")?;
            let name = name.unwrap_or_else(|| {
                canonical
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
            let mut cfg = Config::load_or_default(&layout.config_path)?;
            if cfg.find_project(&name).is_some() {
                return Err(format!("项目已存在: {}", name));
            }
            cfg.projects.push(Project {
                name: name.clone(),
                path: canonical.to_string_lossy().into_owned(),
            });
            cfg.save(&layout.config_path)?;
            println!("已添加项目 {}（{}）", name, canonical.display());
            println!("下一步：nexusctl service add -p {} -n <服务名> -c <命令>", name);
            Ok(())
        }
        ProjectCmd::Remove { name } => {
            let mut cfg = Config::load(&layout.config_path)?;
            if cfg.services.iter().any(|s| s.project == name) {
                return Err(format!("项目「{}」下还有服务，先删除服务", name));
            }
            let before = cfg.projects.len();
            cfg.projects.retain(|p| p.name != name);
            if cfg.projects.len() == before {
                return Err(format!("项目不存在: {}", name));
            }
            cfg.save(&layout.config_path)?;
            println!("已删除项目 {}", name);
            Ok(())
        }
        ProjectCmd::Start { name } => project_action(layout, &name, ProjectAction::Start),
        ProjectCmd::Stop { name } => project_action(layout, &name, ProjectAction::Stop),
        ProjectCmd::Restart { name } => project_action(layout, &name, ProjectAction::Restart),
    }
}

fn show_project(layout: &Layout, name: &str, json: bool) -> Result<(), String> {
    let cfg = Config::load(&layout.config_path)?;
    let project = cfg
        .find_project(name)
        .ok_or_else(|| format!("项目不存在: {}", name))?;
    let state = client::state(layout);
    let rows = service_rows(&cfg, state.as_ref(), Some(name));
    if json {
        let services = rows
            .iter()
            .map(|(s, row)| service_json(&cfg, s, row))
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "project": {
                    "name": project.name.clone(),
                    "path": project.path.clone(),
                    "services": services,
                }
            }))
            .map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    println!("项目: {}", project.name);
    println!("路径: {}", project.path);
    println!("服务: {}", rows.len());
    if rows.is_empty() {
        println!("（该项目下没有服务）");
        return Ok(());
    }
    let table_rows: Vec<Vec<String>> = rows.into_iter().map(|(_, row)| row).collect();
    output::table(
        &["PROJECT", "SERVICE", "BOOT", "ORDER", "STATUS", "PID", "READY"],
        &table_rows,
    );
    Ok(())
}

fn config_cmd(layout: &Layout, cmd: ConfigCmd) -> Result<(), String> {
    match cmd {
        ConfigCmd::Path => {
            println!("mode:   {}", if layout.system { "system" } else { "user" });
            println!("config: {}", layout.config_path.display());
            println!("data:   {}", layout.data_dir.display());
            println!("unit:   {}", layout.unit_path.display());
            Ok(())
        }
        ConfigCmd::Show => {
            let text = std::fs::read_to_string(&layout.config_path).map_err(|e| {
                format!("读取配置失败 {}: {}", layout.config_path.display(), e)
            })?;
            print!("{}", text);
            if !text.ends_with('\n') {
                println!();
            }
            Ok(())
        }
        ConfigCmd::Validate => {
            let cfg = Config::load(&layout.config_path)?;
            println!(
                "配置有效：{}（{} 个项目，{} 个服务）",
                layout.config_path.display(),
                cfg.projects.len(),
                cfg.services.len()
            );
            Ok(())
        }
    }
}

#[derive(Clone, Copy)]
enum ProjectAction {
    Start,
    Stop,
    Restart,
}

impl ProjectAction {
    fn verb(self) -> &'static str {
        match self {
            ProjectAction::Start => "启动",
            ProjectAction::Stop => "停止",
            ProjectAction::Restart => "重启",
        }
    }
}

fn project_services<'a>(cfg: &'a Config, project: &str) -> Vec<&'a Service> {
    let mut services: Vec<&Service> = cfg
        .services
        .iter()
        .filter(|s| s.project == project)
        .collect();
    services.sort_by(|a, b| {
        a.order
            .cmp(&b.order)
            .then_with(|| a.name.cmp(&b.name))
    });
    services
}

fn project_action(layout: &Layout, project: &str, action: ProjectAction) -> Result<(), String> {
    let cfg = Config::load(&layout.config_path)?;
    if cfg.find_project(project).is_none() {
        return Err(format!("项目不存在: {}", project));
    }
    let services = project_services(&cfg, project);
    if services.is_empty() {
        return Err(format!("项目「{}」下没有服务", project));
    }
    // 先确认 runner 在跑：它才是服务的唯一拥有者；没装/没启动时给出明确提示
    let state = client::require_runner(layout)?;
    let mut failures = 0usize;
    match action {
        ProjectAction::Start => {
            for s in &services {
                let id = s.id();
                if state
                    .services
                    .get(&id)
                    .map(|st| service_alive_in(&state, st))
                    .unwrap_or(false)
                {
                    println!("跳过 {}（已在运行）", id);
                    continue;
                }
                send_project_op(layout, "start", s, Duration::from_secs(180), &mut failures);
            }
        }
        ProjectAction::Stop => {
            for s in services.iter().rev() {
                send_project_op(layout, "stop", s, Duration::from_secs(30), &mut failures);
            }
        }
        ProjectAction::Restart => {
            for s in services.iter().rev() {
                send_project_op(layout, "stop", s, Duration::from_secs(30), &mut failures);
            }
            for s in &services {
                send_project_op(layout, "start", s, Duration::from_secs(180), &mut failures);
            }
        }
    }
    if failures > 0 {
        Err(format!(
            "项目 {} {}完成，但有 {} 个服务操作失败",
            project,
            action.verb(),
            failures
        ))
    } else {
        println!(
            "项目 {} 已{}（{} 个服务）",
            project,
            action.verb(),
            services.len()
        );
        Ok(())
    }
}

fn send_project_op(
    layout: &Layout,
    op: &str,
    svc: &Service,
    timeout: Duration,
    failures: &mut usize,
) {
    let id = svc.id();
    let verb = if op == "start" { "启动" } else { "停止" };
    match client::send(layout, op, Some(&id), timeout) {
        Ok(r) if r.ok => println!("{} {} ... ok", verb, id),
        Ok(r) => {
            println!("{} {} ... 失败: {}", verb, id, r.error);
            *failures += 1;
        }
        Err(e) => {
            println!("{} {} ... 失败: {}", verb, id, e);
            *failures += 1;
        }
    }
}

// ─── 服务 ──────────────────────────────────────────────────

fn service_cmd(layout: &Layout, cmd: ServiceCmd) -> Result<(), String> {
    match cmd {
        ServiceCmd::List { project, json } => list_services(layout, json, project.as_deref()),
        ServiceCmd::Show { project, name, json } => {
            show_service(layout, &project, &name, json)
        }
        ServiceCmd::Logs {
            project,
            name,
            lines,
        } => show_service_log(layout, &project, &name, lines),
        ServiceCmd::Add {
            project,
            name,
            command,
            workdir,
            env,
            boot,
            order,
            ready,
            required,
            no_crash_restart,
        } => {
            let mut cfg = Config::load_or_default(&layout.config_path)?;
            if cfg.find_project(&project).is_none() {
                return Err(format!("项目不存在: {}（先 nexusctl project add）", project));
            }
            if cfg.find_service(&project, &name).is_some() {
                return Err(format!("服务已存在: {}.{}", project, name));
            }
            let workdir = match workdir {
                Some(w) if !w.trim().is_empty() => {
                    canonical_dir(Path::new(&w), "工作目录")?.to_string_lossy().into_owned()
                }
                _ => String::new(),
            };
            let ready = ready.unwrap_or_default();
            parse_ready(&ready)?;
            let env = parse_envs(&env)?;
            cfg.services.push(Service {
                project,
                name,
                command,
                workdir,
                env,
                boot,
                order: order.unwrap_or(0),
                ready,
                required,
                crash_restart: !no_crash_restart,
            });
            let reloaded = save_and_sync(layout, &cfg)?;
            println!(
                "已添加服务 {}.{}；manifest 已更新{}",
                cfg.services.last().map(|s| s.project.as_str()).unwrap_or(""),
                cfg.services.last().map(|s| s.name.as_str()).unwrap_or(""),
                if reloaded { "（runner 已 reload）" } else { "（runner 未运行）" }
            );
            println!("启动：nexusctl service start <project> <name>；开机启动：--boot");
            Ok(())
        }
        ServiceCmd::Set {
            project,
            name,
            command,
            workdir,
            env,
            clear_env,
            order,
            ready,
            boot,
            no_boot,
            required,
            no_required,
            crash_restart,
            no_crash_restart,
        } => {
            if boot && no_boot {
                return Err("--boot 与 --no-boot 不能同时使用".into());
            }
            if required && no_required {
                return Err("--required 与 --no-required 不能同时使用".into());
            }
            if crash_restart && no_crash_restart {
                return Err("--crash-restart 与 --no-crash-restart 不能同时使用".into());
            }
            let workdir = match workdir {
                Some(w) if !w.trim().is_empty() => {
                    Some(canonical_dir(Path::new(&w), "工作目录")?.to_string_lossy().into_owned())
                }
                Some(_) => Some(String::new()),
                None => None,
            };
            if let Some(r) = &ready {
                parse_ready(r)?;
            }
            let new_env = parse_envs(&env)?;
            let mut cfg = Config::load(&layout.config_path)?;
            let svc = cfg
                .find_service_mut(&project, &name)
                .ok_or_else(|| format!("服务不存在: {}.{}", project, name))?;
            if let Some(c) = command {
                svc.command = c;
            }
            if let Some(w) = workdir {
                svc.workdir = w;
            }
            if let Some(o) = order {
                svc.order = o;
            }
            if let Some(r) = ready {
                svc.ready = r;
            }
            if boot {
                svc.boot = true;
            }
            if no_boot {
                svc.boot = false;
            }
            if required {
                svc.required = true;
            }
            if no_required {
                svc.required = false;
            }
            if crash_restart {
                svc.crash_restart = true;
            }
            if no_crash_restart {
                svc.crash_restart = false;
            }
            if clear_env {
                svc.env.clear();
            }
            for (k, v) in new_env {
                svc.env.insert(k, v);
            }
            let reloaded = save_and_sync(layout, &cfg)?;
            println!(
                "已更新服务 {}.{}；manifest 已更新{}",
                project,
                name,
                if reloaded { "（runner 已 reload）" } else { "（runner 未运行）" }
            );
            Ok(())
        }
        ServiceCmd::Remove { project, name } => {
            let mut cfg = Config::load(&layout.config_path)?;
            let id = format!("{}.{}", project, name);
            if let Some(st) = client::state(layout) {
                if let Some(s) = st.services.get(&id) {
                    if service_alive_in(&st, s) {
                        return Err(format!("服务 {} 正在运行，先 stop 再删除", id));
                    }
                }
            }
            let before = cfg.services.len();
            cfg.services.retain(|s| !(s.project == project && s.name == name));
            if cfg.services.len() == before {
                return Err(format!("服务不存在: {}", id));
            }
            let reloaded = save_and_sync(layout, &cfg)?;
            println!(
                "已删除服务 {}；manifest 已更新{}",
                id,
                if reloaded { "（runner 已 reload）" } else { "（runner 未运行）" }
            );
            Ok(())
        }
        ServiceCmd::Start { project, name } => {
            ensure_service(layout, &project, &name)?;
            let id = format!("{}.{}", project, name);
            send_op(layout, "start", &id, Duration::from_secs(180))?;
            println!("已启动 {}", id);
            Ok(())
        }
        ServiceCmd::Stop { project, name } => {
            ensure_service(layout, &project, &name)?;
            let id = format!("{}.{}", project, name);
            send_op(layout, "stop", &id, Duration::from_secs(30))?;
            println!("已停止 {}", id);
            Ok(())
        }
        ServiceCmd::Restart { project, name } => {
            ensure_service(layout, &project, &name)?;
            let id = format!("{}.{}", project, name);
            send_op(layout, "restart", &id, Duration::from_secs(180))?;
            println!("已重启 {}", id);
            Ok(())
        }
        ServiceCmd::EnableBoot { project, name } => {
            set_boot(layout, &project, &name, true)?;
            println!(
                "已设置 {}.{} 开机自动启动（下次启动生效；现在启动请 service start）",
                project, name
            );
            Ok(())
        }
        ServiceCmd::DisableBoot { project, name } => {
            set_boot(layout, &project, &name, false)?;
            println!(
                "已取消 {}.{} 开机自动启动（正在运行的实例不会被停止）",
                project, name
            );
            Ok(())
        }
    }
}

fn list_services(layout: &Layout, json: bool, project: Option<&str>) -> Result<(), String> {
    let cfg = Config::load(&layout.config_path)?;
    if let Some(p) = project {
        if cfg.find_project(p).is_none() {
            return Err(format!("项目不存在: {}", p));
        }
    }
    let state = client::state(layout);
    let rows_all = service_rows(&cfg, state.as_ref(), project);
    if json {
        let services = rows_all
            .into_iter()
            .map(|(s, row)| service_json(&cfg, s, &row))
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "services": services }))
                .map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    if rows_all.is_empty() {
        println!(
            "{}",
            if project.is_some() { "（该项目下没有服务）" } else { "（没有服务）" }
        );
        return Ok(());
    }
    let rows: Vec<Vec<String>> = rows_all.into_iter().map(|(_, row)| row).collect();
    output::table(
        &["PROJECT", "SERVICE", "BOOT", "ORDER", "STATUS", "PID", "READY"],
        &rows,
    );
    Ok(())
}

fn show_service(layout: &Layout, project: &str, name: &str, json: bool) -> Result<(), String> {
    let cfg = Config::load(&layout.config_path)?;
    let svc = cfg
        .find_service(project, name)
        .ok_or_else(|| format!("服务不存在: {}.{}", project, name))?;
    let id = svc.id();
    let state = client::state(layout);
    let st = state.as_ref().and_then(|s| s.services.get(&id));
    let log_path = st
        .map(|s| s.log_path.clone())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| {
            service_log_path_in(&layout.logs_dir(), &id)
                .to_string_lossy()
                .into_owned()
        });
    let workdir = cfg.workdir_of(svc);
    let project_path = cfg
        .find_project(&svc.project)
        .map(|p| p.path.clone())
        .unwrap_or_default();

    if json {
        let status = st.map(|s| {
            serde_json::json!({
                "phase": s.phase.clone(),
                "pid": s.pid,
                "ready_at": s.ready_at.clone(),
                "error": s.error.clone(),
            })
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "project": svc.project.clone(),
                "name": svc.name.clone(),
                "id": id,
                "project_path": project_path,
                "command": svc.command.clone(),
                "workdir": workdir,
                "env": svc.env.clone(),
                "boot": svc.boot,
                "order": svc.order,
                "ready": svc.ready.clone(),
                "required": svc.required,
                "crash_restart": svc.crash_restart,
                "status": status,
                "log_path": log_path,
            }))
            .map_err(|e| e.to_string())?
        );
        return Ok(());
    }

    println!("项目:       {}", svc.project);
    println!("服务:       {}", svc.name);
    println!("ID:         {}", id);
    println!("启动命令:   {}", svc.command);
    println!("项目路径:   {}", project_path);
    println!("工作目录:   {}", workdir);
    println!("开机自启:   {}", if svc.boot { "yes" } else { "no" });
    println!("启动顺序:   {}", svc.order);
    println!(
        "就绪检查:   {}",
        if svc.ready.trim().is_empty() {
            describe_ready(&svc.ready)
        } else {
            format!("{}（{}）", describe_ready(&svc.ready), svc.ready)
        }
    );
    println!("未就绪阻断: {}", if svc.required { "yes" } else { "no" });
    println!("崩溃重启:   {}", if svc.crash_restart { "yes" } else { "no" });
    if svc.env.is_empty() {
        println!("环境变量:   （无）");
    } else {
        println!("环境变量:");
        for (k, v) in &svc.env {
            println!("  {}={}", k, v);
        }
    }
    match st {
        Some(s) => {
            println!(
                "运行状态:   {}（pid {}）",
                s.phase,
                s.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into())
            );
            if !s.error.is_empty() {
                println!("最近错误:   {}", s.error);
            }
        }
        None => println!("运行状态:   无（runner 未运行或还没有状态）"),
    }
    println!("日志:       {}", log_path);
    Ok(())
}

/// 配置里的服务 + 状态 → (服务引用, 表格行)；`project` 非空时只保留该项目。
fn service_rows<'a>(
    cfg: &'a Config,
    state: Option<&BootRunnerState>,
    project: Option<&str>,
) -> Vec<(&'a Service, Vec<String>)> {
    let mut services: Vec<&Service> = cfg
        .services
        .iter()
        .filter(|s| project.is_none_or(|p| s.project == p))
        .collect();
    services.sort_by(|a, b| {
        a.order
            .cmp(&b.order)
            .then_with(|| a.project.cmp(&b.project))
            .then_with(|| a.name.cmp(&b.name))
    });
    services
        .into_iter()
        .map(|s| {
            let id = s.id();
            let st = state.and_then(|st| st.services.get(&id));
            let status = st.map(|s| s.phase.clone()).unwrap_or_else(|| "-".into());
            let pid = st
                .and_then(|s| s.pid)
                .map(|p| p.to_string())
                .unwrap_or_else(|| "-".into());
            let row = vec![
                s.project.clone(),
                s.name.clone(),
                if s.boot { "yes" } else { "no" }.to_string(),
                s.order.to_string(),
                status,
                pid,
                describe_ready(&s.ready),
            ];
            (s, row)
        })
        .collect()
}

fn service_json(cfg: &Config, s: &Service, row: &[String]) -> serde_json::Value {
    serde_json::json!({
        "project": s.project.clone(),
        "name": s.name.clone(),
        "id": s.id(),
        "boot": s.boot,
        "order": s.order,
        "ready": s.ready.clone(),
        "command": s.command.clone(),
        "workdir": cfg.workdir_of(s),
        "status": row[4].clone(),
        "pid": row[5].clone(),
    })
}

fn ensure_service(layout: &Layout, project: &str, name: &str) -> Result<(), String> {
    let cfg = Config::load(&layout.config_path)?;
    cfg.find_service(project, name)
        .map(|_| ())
        .ok_or_else(|| format!("服务不存在: {}.{}", project, name))
}

fn set_boot(layout: &Layout, project: &str, name: &str, boot: bool) -> Result<(), String> {
    let mut cfg = Config::load(&layout.config_path)?;
    let svc = cfg
        .find_service_mut(project, name)
        .ok_or_else(|| format!("服务不存在: {}.{}", project, name))?;
    svc.boot = boot;
    save_and_sync(layout, &cfg)?;
    Ok(())
}

fn send_op(layout: &Layout, op: &str, id: &str, timeout: Duration) -> Result<(), String> {
    let result = client::send(layout, op, Some(id), timeout)?;
    if result.ok {
        Ok(())
    } else {
        Err(result.error)
    }
}

// ─── Boot ──────────────────────────────────────────────────

fn boot_cmd(layout: &Layout, runner: Option<PathBuf>, cmd: BootCmd) -> Result<(), String> {
    match cmd {
        BootCmd::Install => {
            // 文件已存在时不要重写（会丢手写注释）；只有首次安装才落一份空配置。
            let config_existed = layout.config_path.exists();
            let cfg = Config::load_or_default(&layout.config_path)?;
            if !config_existed {
                cfg.save(&layout.config_path)?;
            }
            layout.ensure_dirs()?;
            manifest::generate(&cfg, &layout.boot_dir)?;
            let runner = layout.runner_path(runner)?;
            let report = systemd::install(layout, &runner)?;
            println!("已生成 manifest：{}", layout.manifest_path().display());
            println!("已安装 unit：{}", layout.unit_path.display());
            if report.restarted {
                println!("unit 原先已在运行，已 restart 以加载新二进制/manifest");
            }
            // 之前已在运行的 runner 不会因为 enable --now 重读 manifest；主动 reload 一次。
            // 刚启动时 state 可能还没写出来，返回 false 不是错误。
            match client::reload_if_running(layout) {
                Ok(true) => println!("runner 已加载最新 manifest"),
                Ok(false) => {}
                Err(e) => eprintln!("[nexusctl] 警告: 通知 runner reload 失败: {}", e),
            }
            if let Some(user) = report.user.as_deref() {
                match report.linger {
                    Some(true) => println!("linger 已开启（{}）", user),
                    Some(false) => {}
                    None => {}
                }
            }
            for w in report.warnings {
                eprintln!("[nexusctl] 警告: {}", w);
            }
            println!("查看状态：nexusctl boot status");
            Ok(())
        }
        BootCmd::Uninstall => {
            // 先让 runner 优雅退出：它会在主循环里 stop_all（杀服务进程组）并写最终状态。
            // 直接 systemctl disable --now 只会让 systemd 用 cgroup 强杀，状态文件不干净。
            if let Some(st) = client::state(layout) {
                if runner_reachable(&st) {
                    let _ = client::send(layout, "shutdown", None, Duration::from_secs(10));
                    // 给 runner 一点时间 stop_all + 写最终状态；最长等 5 秒。
                    for _ in 0..50 {
                        match client::state(layout) {
                            Some(s) if s.runner_pid != 0 && runner_reachable(&s) => {
                                std::thread::sleep(Duration::from_millis(100));
                            }
                            _ => break,
                        }
                    }
                }
            }
            let warnings = systemd::uninstall(layout)?;
            for w in warnings {
                eprintln!("[nexusctl] 警告: {}", w);
            }
            println!("已卸载 systemd unit；配置与日志保留在 {}", layout.data_dir.display());
            Ok(())
        }
        BootCmd::Unit => {
            let runner = layout.runner_path(runner)?;
            print!("{}", systemd::render_unit(layout, &runner)?);
            Ok(())
        }
        BootCmd::Shutdown => stop_runner(layout, false),
        BootCmd::Start => boot_start(layout),
        BootCmd::Stop => stop_runner(layout, true),
        BootCmd::Restart => boot_restart(layout),
        BootCmd::Enable => boot_enable(layout, true),
        BootCmd::Disable => boot_enable(layout, false),
        BootCmd::RunnerLog { lines } => {
            print_log_tail(&layout.runner_log(), lines, "runner 日志")
        }
        BootCmd::Sync => {
            let cfg = Config::load(&layout.config_path)?;
            layout.ensure_dirs()?;
            manifest::generate(&cfg, &layout.boot_dir)?;
            let reloaded = client::reload_if_running(layout)?;
            println!(
                "manifest 已更新：{}{}",
                layout.manifest_path().display(),
                if reloaded { "（runner 已 reload）" } else { "（runner 未运行）" }
            );
            Ok(())
        }
        BootCmd::Status { json } => boot_status(layout, json),
        BootCmd::Logs { project, name, lines } => {
            show_service_log(layout, &project, &name, lines)
        }
    }
}

fn show_service_log(
    layout: &Layout,
    project: &str,
    name: &str,
    lines: usize,
) -> Result<(), String> {
    let cfg = Config::load(&layout.config_path)?;
    cfg.find_service(project, name)
        .ok_or_else(|| format!("服务不存在: {}.{}", project, name))?;
    let id = format!("{}.{}", project, name);
    let state = client::state(layout);
    let path = state
        .as_ref()
        .and_then(|s| s.services.get(&id))
        .map(|s| PathBuf::from(&s.log_path))
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| service_log_path_in(&layout.logs_dir(), &id));
    print_log_tail(&path, lines, "服务日志")
}

fn print_log_tail(path: &Path, lines: usize, what: &str) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| {
        format!(
            "读取日志失败 {}: {}（{}，可能还没生成）",
            path.display(),
            e,
            what
        )
    })?;
    let text = decode_to_string(&bytes);
    let tail = output::tail(&text, lines);
    if tail.is_empty() {
        println!("（{}为空）", what);
    } else {
        print!("{}", tail);
    }
    Ok(())
}

fn stop_runner(layout: &Layout, stop_unit: bool) -> Result<(), String> {
    let mut stopped = false;
    if let Some(st) = client::state(layout) {
        if runner_reachable(&st) {
            let result = client::send(layout, "shutdown", None, Duration::from_secs(10))?;
            if !result.ok {
                return Err(result.error);
            }
            for _ in 0..50 {
                match client::state(layout) {
                    Some(s) if s.runner_pid != 0 && runner_reachable(&s) => {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    _ => break,
                }
            }
            stopped = true;
        }
    }
    if stop_unit && systemd::unit_exists(layout) {
        systemd::stop_unit(layout)?;
        stopped = true;
    }
    if stopped {
        println!("已停止 runner 与其管理的服务（unit 保留）");
    } else {
        println!("runner 未运行");
    }
    Ok(())
}

fn boot_start(layout: &Layout) -> Result<(), String> {
    if !systemd::unit_exists(layout) {
        return Err(format!(
            "unit 未安装（{}）。先执行 nexusctl boot install",
            layout.unit_path.display()
        ));
    }
    systemd::start_unit(layout)?;
    println!("已启动 runner");
    Ok(())
}

fn boot_restart(layout: &Layout) -> Result<(), String> {
    if !systemd::unit_exists(layout) {
        return Err(format!(
            "unit 未安装（{}）。先执行 nexusctl boot install",
            layout.unit_path.display()
        ));
    }
    systemd::restart_unit(layout)?;
    println!("已重启 runner（其管理的服务会随之重启）");
    Ok(())
}

fn boot_enable(layout: &Layout, enable: bool) -> Result<(), String> {
    if !systemd::unit_exists(layout) {
        return Err(format!(
            "unit 未安装（{}）。先执行 nexusctl boot install",
            layout.unit_path.display()
        ));
    }
    if enable {
        systemd::enable_unit(layout)?;
        println!("已启用开机自启（未启动 runner；用 boot start 启动）");
    } else {
        systemd::disable_unit(layout)?;
        println!("已禁用开机自启（正在运行的 runner 不受影响）");
    }
    Ok(())
}

fn boot_status(layout: &Layout, json: bool) -> Result<(), String> {
    let state = client::state(layout);
    let reachable = state.as_ref().map(runner_reachable).unwrap_or(false);
    let unit = systemd::status(layout);

    if json {
        let cfg = Config::load(&layout.config_path).ok();
        let services = cfg
            .as_ref()
            .map(|cfg| {
                service_rows(cfg, state.as_ref(), None)
                    .into_iter()
                    .map(|(s, row)| {
                        serde_json::json!({
                            "project": s.project.clone(),
                            "name": s.name.clone(),
                            "id": s.id(),
                            "status": row[4].clone(),
                            "pid": row[5].clone(),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "unit": {
                    "active": unit.active,
                    "linger": unit.linger,
                    "user": unit.user,
                    "path": layout.unit_path.display().to_string(),
                },
                "runner": state.as_ref().map(|s| serde_json::json!({
                    "pid": s.runner_pid,
                    "mode": s.mode.clone(),
                    "heartbeat_at": s.heartbeat_at.clone(),
                    "reachable": reachable,
                })),
                "services": services,
            }))
            .map_err(|e| e.to_string())?
        );
        return Ok(());
    }

    println!("unit:    {}", layout.unit_path.display());
    println!(
        "active:  {}",
        match unit.active {
            Some(true) => "active",
            Some(false) => "inactive",
            None => "unknown",
        }
    );
    if !layout.system {
        println!(
            "linger:  {}",
            match unit.linger {
                Some(true) => "on（无人登录也启动）",
                Some(false) => "off（登录后才启动；sudo loginctl enable-linger <user>）",
                None => "unknown",
            }
        );
    }
    match state.as_ref() {
        Some(s) => println!(
            "runner:  pid={} mode={} heartbeat={} {}",
            s.runner_pid,
            s.mode,
            s.heartbeat_at,
            if reachable { "reachable" } else { "unreachable" }
        ),
        None => println!("runner:  未运行（没有 state.json）"),
    }

    if let Ok(cfg) = Config::load(&layout.config_path) {
        if !cfg.services.is_empty() {
            println!();
            let rows: Vec<Vec<String>> = service_rows(&cfg, state.as_ref(), None)
                .into_iter()
                .map(|(_, row)| row)
                .collect();
            output::table(
                &["PROJECT", "SERVICE", "BOOT", "ORDER", "STATUS", "PID", "READY"],
                &rows,
            );
        }
    }
    Ok(())
}

// ─── 工具 ──────────────────────────────────────────────────

/// 保存配置 + 生成 manifest + 通知 runner。返回是否真的 reload 了。
fn save_and_sync(layout: &Layout, cfg: &Config) -> Result<bool, String> {
    layout.ensure_dirs()?;
    cfg.save(&layout.config_path)?;
    manifest::generate(cfg, &layout.boot_dir)?;
    client::reload_if_running(layout)
}

fn parse_envs(args: &[String]) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut out = std::collections::BTreeMap::new();
    for raw in args {
        let (k, v) = parse_env_arg(raw)?;
        out.insert(k, v);
    }
    Ok(out)
}

/// 目录必须是已存在的绝对目录；返回 canonical 形式。
fn canonical_dir(path: &Path, what: &str) -> Result<PathBuf, String> {
    let canonical = std::fs::canonicalize(path)
        .map_err(|e| format!("{}不可用 {}: {}", what, path.display(), e))?;
    if !canonical.is_dir() {
        return Err(format!("{}不是目录: {}", what, canonical.display()));
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_env_arg_rejects_missing_equals() {
        assert!(parse_env_arg("NO_EQUALS").is_err());
        assert_eq!(
            parse_env_arg("A=1").unwrap(),
            ("A".to_string(), "1".to_string())
        );
        assert_eq!(
            parse_env_arg("A=").unwrap(),
            ("A".to_string(), "".to_string())
        );
    }

    #[test]
    fn canonical_dir_rejects_missing() {
        let p = std::env::temp_dir().join("nexus-does-not-exist-xyz");
        assert!(canonical_dir(&p, "目录").is_err());
    }

    #[test]
    fn project_services_filters_and_sorts() {
        let mut cfg = Config::default();
        cfg.projects.push(Project {
            name: "p".into(),
            path: "/tmp/p".into(),
        });
        cfg.projects.push(Project {
            name: "q".into(),
            path: "/tmp/q".into(),
        });
        let svc = |project: &str, name: &str, order: i32| Service {
            project: project.into(),
            name: name.into(),
            command: "true".into(),
            workdir: String::new(),
            env: Default::default(),
            boot: false,
            order,
            ready: String::new(),
            required: false,
            crash_restart: true,
        };
        cfg.services.push(svc("p", "b", 20));
        cfg.services.push(svc("p", "a", 10));
        cfg.services.push(svc("q", "c", 0));

        let got = project_services(&cfg, "p");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "a");
        assert_eq!(got[1].name, "b");
        assert!(project_services(&cfg, "missing").is_empty());
    }

    #[test]
    fn service_rows_filters_by_project() {
        let mut cfg = Config::default();
        cfg.projects.push(Project {
            name: "a".into(),
            path: "/tmp/a".into(),
        });
        cfg.projects.push(Project {
            name: "b".into(),
            path: "/tmp/b".into(),
        });
        let svc = |project: &str, name: &str| Service {
            project: project.into(),
            name: name.into(),
            command: "true".into(),
            workdir: String::new(),
            env: Default::default(),
            boot: false,
            order: 0,
            ready: String::new(),
            required: false,
            crash_restart: true,
        };
        cfg.services.push(svc("a", "x"));
        cfg.services.push(svc("b", "y"));

        let only_a = service_rows(&cfg, None, Some("a"));
        assert_eq!(only_a.len(), 1);
        assert_eq!(only_a[0].0.project, "a");
        assert_eq!(service_rows(&cfg, None, None).len(), 2);
        assert!(service_rows(&cfg, None, Some("missing")).is_empty());
    }
}
