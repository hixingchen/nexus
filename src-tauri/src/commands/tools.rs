use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{Manager, State};
use crate::AppState;
use crate::models::{OpenTool, ServiceOpenToolBinding};

// 外部打开工具（服务右键「用 XX 打开」）
//
// 新模型：executable（程序路径）+ args（参数模板）。`{path}` 占位符以
// **独立参数**注入（不经过 shell 字符串拼接）——路径含空格/特殊字符天然安全。
// command 字段为历史遗留（整串 shell 命令），仅 executable 为空的历史行使用。
//
// 安全边界：executable/args 来自用户自己的配置，与「工具命令」同一信任级别。

// ─── CRUD ───────────────────────────────────────────────────

/// 工具列表（rowid 序 = 添加顺序）
#[tauri::command]
pub fn list_open_tools(state: State<AppState>) -> Result<Vec<OpenTool>, String> {
    state.db.with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT id, name, command, executable, args FROM open_tools ORDER BY rowid")
            .map_err(|e| format!("查询打开工具失败: {}", e))?;
        let rows = stmt.query_map([], |row| {
            Ok(OpenTool {
                id: row.get("id")?, name: row.get("name")?, command: row.get("command")?,
                executable: row.get("executable")?, args: row.get("args")?,
            })
        }).map_err(|e| format!("查询打开工具失败: {}", e))?;
        let mut out = Vec::new();
        for r in rows { out.push(r.map_err(|e| format!("解析打开工具失败: {}", e))?); }
        Ok(out)
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveOpenToolParams {
    pub id: Option<String>,
    pub name: String,
    pub executable: String,
    pub args: String,
}

/// 新增（id 空）或更新（id 非空）工具
///
/// 保存即升级：command 旧字段清空（历史行编辑保存一次即完成格式升级）
#[tauri::command]
pub fn save_open_tool(state: State<AppState>, params: SaveOpenToolParams) -> Result<OpenTool, String> {
    let name = params.name.trim().to_string();
    let executable = params.executable.trim().to_string();
    let args = params.args.trim().to_string();
    if name.is_empty() { return Err("工具名称不能为空".into()); }
    if executable.is_empty() {
        return Err("可执行文件不能为空（如：C:\\Program Files\\JetBrains\\...\\idea64.exe）".into());
    }

    if let Some(id) = params.id.filter(|s| !s.trim().is_empty()) {
        let affected = state.db.with_conn(|conn| {
            conn.execute("UPDATE open_tools SET name=?1, command='', executable=?2, args=?3 WHERE id=?4",
                rusqlite::params![name, executable, args, id])
                .map_err(|e| format!("更新打开工具失败: {}", e))
        })?;
        if affected == 0 { return Err("工具不存在或已被删除".into()); }
        return Ok(OpenTool { id, name, command: String::new(), executable, args });
    }

    let id = uuid::Uuid::new_v4().to_string();
    state.db.with_conn(|conn| {
        conn.execute("INSERT INTO open_tools (id, name, command, executable, args) VALUES (?1,?2,'',?3,?4)",
            rusqlite::params![id, name, executable, args])
            .map_err(|e| format!("添加打开工具失败: {}", e))
    })?;
    Ok(OpenTool { id, name, command: String::new(), executable, args })
}

/// 某个打开工具当前的引用情况。
///
/// 为什么要它（UX-4）：删除工具会**级联解除所有服务的绑定**，而删除入口是一个 11px 的
/// 垃圾桶图标——不先告诉用户"这一下会影响哪些服务的「用 XX 打开」"，误点的代价就是
/// 跨项目的一次静默配置丢失（界面只回一句"已删除工具「X」"）。
#[derive(Serialize)]
pub struct OpenToolUsage {
    /// 绑定它的服务（`项目名 / 服务名`，跨项目）
    pub services: Vec<String>,
    /// 默认打开工具指向它的**模板**名（删除时同样会清空）
    pub templates: Vec<String>,
}

/// 查一个工具被谁引用（只读，供删除前的确认框使用）
#[tauri::command]
pub fn get_open_tool_usage(state: State<AppState>, id: String) -> Result<OpenToolUsage, String> {
    if id.trim().is_empty() { return Err("工具ID不能为空".into()); }
    open_tool_usage(&state.db, id.trim())
}

/// 上面那条命令的 DB 部分（抽出来以便测试）
pub(crate) fn open_tool_usage(db: &crate::database::Database, id: &str) -> Result<OpenToolUsage, String> {
    db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT p.name, s.name FROM service_open_tools b \
             JOIN services s ON s.id = b.service_id \
             JOIN projects p ON p.id = s.project_id \
             WHERE b.tool_id = ?1 ORDER BY p.name, s.name"
        ).map_err(|e| format!("查询工具绑定失败: {}", e))?;
        let services: Vec<String> = stmt
            .query_map([&id], |row| Ok(format!("{} / {}", row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
            .map_err(|e| format!("查询工具绑定失败: {}", e))?
            .collect::<Result<_, _>>()
            .map_err(|e| format!("查询工具绑定失败: {}", e))?;
        drop(stmt);
        let mut stmt = conn.prepare("SELECT name FROM service_templates WHERE open_tool_id=?1 ORDER BY name")
            .map_err(|e| format!("查询模板引用失败: {}", e))?;
        let templates: Vec<String> = stmt
            .query_map([&id], |row| row.get::<_, String>(0))
            .map_err(|e| format!("查询模板引用失败: {}", e))?
            .collect::<Result<_, _>>()
            .map_err(|e| format!("查询模板引用失败: {}", e))?;
        Ok(OpenToolUsage { services, templates })
    })
}

/// 删除工具（service_open_tools 级联清理绑定；模板上的引用一并清空）
#[tauri::command]
pub fn delete_open_tool(state: State<AppState>, id: String) -> Result<(), String> {
    if id.trim().is_empty() { return Err("工具ID不能为空".into()); }
    state.db.with_conn(|conn| {
        conn.execute("DELETE FROM open_tools WHERE id=?1", [&id])
            .map_err(|e| format!("删除打开工具失败: {}", e))?;
        conn.execute("UPDATE service_templates SET open_tool_id='' WHERE open_tool_id=?1", [&id])
            .map_err(|e| format!("清理模板工具引用失败: {}", e))?;
        Ok(())
    })
}

// ─── 服务绑定 ───────────────────────────────────────────────

/// 绑定/解绑服务与打开工具（tool_id 空/缺失 → 解绑）
#[tauri::command]
pub fn set_service_open_tool(
    state: State<AppState>,
    service_id: String,
    tool_id: Option<String>,
) -> Result<(), String> {
    if service_id.trim().is_empty() { return Err("服务ID不能为空".into()); }
    state.db.with_conn(|conn| {
        let tid = tool_id.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        match tid {
            Some(tid) => {
                conn.execute("INSERT OR REPLACE INTO service_open_tools (service_id, tool_id) VALUES (?1,?2)",
                    rusqlite::params![service_id, tid])
                    .map_err(|e| format!("绑定打开工具失败: {}", e))?;
            }
            None => {
                conn.execute("DELETE FROM service_open_tools WHERE service_id=?1", [&service_id])
                    .map_err(|e| format!("解除打开工具绑定失败: {}", e))?;
            }
        }
        Ok(())
    })
}

/// 项目下所有服务的工具绑定（仅已绑定的；前端与工具库合并渲染名称）
#[tauri::command]
pub fn list_service_open_tool_bindings(state: State<AppState>, project_id: String) -> Result<Vec<ServiceOpenToolBinding>, String> {
    if project_id.trim().is_empty() { return Err("项目ID不能为空".into()); }
    list_bindings_for_project(&state.db, &project_id)
}

/// 上面那条命令的 DB 部分（抽出来以便测试）。
///
/// **`AS service_id` 这个别名不是装饰**：SQLite 对 `s.id` 给出的结果列名是短名 `id`，
/// 于是按列名取值的 `row.get("service_id")` 会返回 `InvalidColumnName`。原实现漏了别名，
/// 后果是"项目下任何一条服务绑定了打开工具，这个命令就整体失败"——前端拿不到绑定，
/// 右键菜单里「用 XX 打开」全空，而错误只在控制台里（直到本轮把这类失败升级成可见提示
/// 才暴露）。**改这条 SQL 时别把别名删掉**，下方回归测试会立刻红。
pub(crate) fn list_bindings_for_project(
    db: &crate::database::Database,
    project_id: &str,
) -> Result<Vec<ServiceOpenToolBinding>, String> {
    db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT s.id AS service_id, b.tool_id FROM service_open_tools b JOIN services s ON s.id = b.service_id WHERE s.project_id=?1"
        ).map_err(|e| format!("查询服务工具绑定失败: {}", e))?;
        let rows = stmt.query_map([project_id], |row| {
            Ok(ServiceOpenToolBinding { service_id: row.get("service_id")?, tool_id: row.get("tool_id")? })
        }).map_err(|e| format!("查询服务工具绑定失败: {}", e))?;
        let mut out = Vec::new();
        for r in rows { out.push(r.map_err(|e| format!("解析服务工具绑定失败: {}", e))?); }
        Ok(out)
    })
}

// ─── 打开 ───────────────────────────────────────────────────

/// 用服务绑定的工具打开其工作目录
#[tauri::command]
pub fn open_service_with_tool(state: State<AppState>, service_id: String) -> Result<(), String> {
    if service_id.trim().is_empty() { return Err("服务ID不能为空".into()); }
    let (name, cwd, tool_id) = state.db.with_conn(|conn| {
        conn.query_row(
            "SELECT s.name, s.cwd, b.tool_id FROM services s LEFT JOIN service_open_tools b ON b.service_id = s.id WHERE s.id=?1",
            [&service_id],
            |r| Ok((r.get::<_, String>("name")?, r.get::<_, String>("cwd")?, r.get::<_, Option<String>>("tool_id")?)),
        )
        .map_err(|e| format!("服务不存在: {}", e))
    })?;
    if cwd.trim().is_empty() {
        return Err(format!("服务「{}」未配置工作目录，无法用工具打开", name));
    }
    let tid = tool_id.ok_or_else(|| format!("服务「{}」未绑定打开工具，请在服务设置的「打开方式」中选择", name))?;
    let (tool_name, legacy_command, executable, args) = state.db.with_conn(|conn| {
        conn.query_row("SELECT name, command, executable, args FROM open_tools WHERE id=?1", [&tid],
            |r| Ok((
                r.get::<_, String>("name")?, r.get::<_, String>("command")?,
                r.get::<_, String>("executable")?, r.get::<_, String>("args")?,
            )))
            .map_err(|e| format!("打开工具不存在或已被删除: {}", e))
    })?;

    if !executable.trim().is_empty() {
        // 新模型：executable + args 参数化启动
        spawn_tool_program(&executable, &args, &cwd)
            .map_err(|e| format!("启动工具「{}」失败: {}", tool_name, e))?;
    } else {
        // 历史行兼容：整串 shell 命令（{path} 占位替换）
        log::info!("[nexus] 工具「{}」为旧版命令格式，建议重新编辑保存升级", tool_name);
        spawn_detached(&render_legacy_command(&legacy_command, &cwd)?)
            .map_err(|e| format!("启动工具「{}」失败: {}", tool_name, e))?;
    }
    log::info!("[nexus] 用工具打开服务: {} ({}) -> {}", name, tool_name, cwd);
    Ok(())
}

/// 分离启动外部命令（cmd /C）
///
/// 与 open_terminal 同模式：不等待退出、不进进程管理器、不挂 Job Object——
/// 服务进程需随 Nexus 退出而终止，外部工具（IDEA 等）绝不能被 Nexus 退出连坐。
fn spawn_detached(rendered: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW：压住 cmd 自身闪窗（GUI 工具正常显示自己的窗口）
        std::process::Command::new("cmd")
            .args(["/C", rendered])
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|e| e.to_string())
            .map(|_| ())
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("sh").arg("-c").arg(rendered)
            .spawn()
            .map_err(|e| e.to_string())
            .map(|_| ())
    }
}

/// 新模型启动：executable + 参数数组直启（不经过 shell）
///
/// - `.exe` / 程序路径：`Command::new` 直启，参数逐个直传——路径含空格天然安全
/// - `.cmd` / `.bat`（Windows cmd shim）：必须经 `cmd /C`，此时才做字符串化
///   （逐个参数包引号，仅此场景有引号负担，且 shim 场景少见）
///
/// 把一个参数安全地拼进 `cmd /C` 整串命令行（仅 .cmd/.bat shim 与历史行命令需要）。
///
/// 为什么不是"删掉引号"：原实现 `s.replace('"', "")` 会**静默改写**参数
/// （`C:\a"b` → `C:\ab`），用户看到的现象是"用错路径打开了文件"，且无从察觉；
/// 无法安全表达的参数必须报错，而不是猜。
///
/// 拒绝规则（都来自 cmd 的实际语义）：
/// - `"`：Windows 路径本就非法字符；出现在参数里只能靠转义，而 `cmd /C` 下
///   转义规则与 `CommandLineToArgvW` 叠加，无法可靠推断，直接拒绝；
/// - `%` / `!`：环境变量展开与延迟展开，**在双引号内同样生效**（`"%PATH%"` 会展开），
///   而命令行上没有可靠的转义写法（`%%` 只在批处理文件里折叠为 `%`）；
/// - 换行：会把单条命令截成两条（注入）。
///
/// 尾随反斜杠按 `CommandLineToArgvW` 规则翻倍：`"C:\dir\"` 中的 `\"` 会被解析成字面引号，
/// 参数变成 `C:\dir"`，路径尾部的反斜杠因此丢失。
fn quote_for_cmd(s: &str) -> Result<String, String> {
    if s.contains('"') {
        return Err(format!("参数含双引号，无法安全传给 cmd：{}", s));
    }
    if s.contains('%') || s.contains('!') {
        return Err(format!("参数含 cmd 展开字符（% 或 !），已拒绝执行：{}", s));
    }
    if s.contains('\n') || s.contains('\r') {
        return Err("参数含换行，已拒绝执行".into());
    }
    let trailing = s.len() - s.trim_end_matches('\\').len();
    let mut out = String::with_capacity(s.len() + 2 + trailing);
    out.push('"');
    out.push_str(s);
    for _ in 0..trailing {
        out.push('\\'); // N 个尾随反斜杠 → 2N 个才是字面量
    }
    out.push('"');
    Ok(out)
}

fn spawn_tool_program(executable: &str, args_template: &str, path: &str) -> Result<(), String> {
    let mut args = split_args(args_template);
    // {path} 作为独立参数注入（token 内替换也安全——argv 单元素可含空格）；
    // 模板没写 {path} 时把路径作为最后一个参数追加
    if args.iter().any(|a| a.contains("{path}")) {
        for a in &mut args {
            if a.contains("{path}") { *a = a.replace("{path}", path); }
        }
    } else {
        args.push(path.to_string());
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let lower = executable.to_lowercase();
        if lower.ends_with(".cmd") || lower.ends_with(".bat") {
            // cmd /C 只能吃整串命令，引号必须自己拼——注入面见 quote_for_cmd
            let mut parts = vec![quote_for_cmd(executable)?];
            for a in &args {
                parts.push(quote_for_cmd(a)?);
            }
            std::process::Command::new("cmd")
                .args(["/C", &parts.join(" ")])
                .creation_flags(0x08000000)
                .spawn()
                .map_err(|e| e.to_string())
                .map(|_| ())
        } else {
            std::process::Command::new(executable)
                .args(&args)
                .creation_flags(0x08000000)
                .spawn()
                .map_err(|e| e.to_string())
                .map(|_| ())
        }
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new(executable)
            .args(&args)
            .spawn()
            .map_err(|e| e.to_string())
            .map(|_| ())
    }
}

/// 渲染历史行命令：`{path}` 占位替换并包双引号（安全规则见 quote_for_cmd）。
///
/// 历史行最终经 `cmd /C`/`sh -c` 整串执行——cwd 含空格时断参、含 `&`/`|` 等
/// 元字符时会被 shell 解释执行。包引号后空格与大多数元字符只作为路径一部分。
/// 模板自带引号（`"{path}"`）时先整体替换，避免二次包引号出现 `""C:\x""`。
/// 注意信任边界：command 本身仍是用户配置的 shell 命令串（与工具命令同级），
/// 本函数只保证**注入的路径**不成为新命令。
fn render_legacy_command(template: &str, path: &str) -> Result<String, String> {
    let quoted = quote_for_cmd(path)?;
    Ok(template
        .replace("\"{path}\"", &quoted)
        .replace("{path}", &quoted))
}

/// 迷你参数分词：空白分隔 + 双引号分组
///
/// 有意的限制：不支持转义/单引号/cmd 元字符——参数以数组直传进程，解析只负责
/// 把模板拆成多个参数（如 `--reuse-window {path}` → 两个参数）。
fn split_args(template: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut started = false;
    for c in template.chars() {
        match c {
            '"' => { in_quote = !in_quote; started = true; }
            c if c.is_whitespace() && !in_quote => {
                if started { out.push(std::mem::take(&mut cur)); started = false; }
            }
            c => { cur.push(c); started = true; }
        }
    }
    if started { out.push(cur); }
    out
}

// ─── 本机工具识别 ───────────────────────────────────────────

#[derive(Serialize)]
pub struct DetectedToolsResult {
    pub added: Vec<OpenTool>,
    /// 探测到但已存在（同名或同路径）而跳过的数量
    pub skipped: usize,
}

/// 路径比较口径：大小写不敏感、斜杠统一、忽略尾部反斜杠。
fn normalize_tool_path(path: &str) -> String {
    path.trim()
        .trim_end_matches(['/', '\\'])
        .replace('/', "\\")
        .to_lowercase()
}

struct ToolCandidate {
    name: &'static str,
    path: PathBuf,
}

/// 一键识别本机已安装的编辑器/IDE/终端，把工具库里还没有的加进去。
///
/// 只做注册表 App Paths / PATH / 已知安装目录的**存在性探测**，不执行任何被检测程序；
/// 已有同名或同路径工具的跳过，不产生重复条目。
///
/// 扫描会启动若干 `where.exe` 并读注册表，约几百毫秒到一两秒——放进
/// `spawn_blocking`，不占 Tauri 的主线程（否则弹窗会卡住不响应）。
#[tauri::command]
pub async fn detect_and_add_open_tools(app: tauri::AppHandle) -> Result<DetectedToolsResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        detect_and_add_open_tools_inner(&state)
    }).await.map_err(|e| format!("识别本机工具任务失败: {}", e))?
}

fn detect_and_add_open_tools_inner(state: &AppState) -> Result<DetectedToolsResult, String> {
    let existing: Vec<(String, String)> = state.db.with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT name, executable FROM open_tools")
            .map_err(|e| format!("查询打开工具失败: {}", e))?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        }).map_err(|e| format!("查询打开工具失败: {}", e))?;
        let mut out = Vec::new();
        for row in rows { out.push(row.map_err(|e| format!("解析打开工具失败: {}", e))?); }
        Ok(out)
    })?;
    let mut names: HashSet<String> = existing.iter()
        .map(|(name, _)| name.trim().to_lowercase())
        .collect();
    let mut paths: HashSet<String> = existing.iter()
        .filter(|(_, executable)| !executable.trim().is_empty())
        .map(|(_, executable)| normalize_tool_path(executable))
        .collect();

    let mut inserts: Vec<(String, &'static str, String)> = Vec::new();
    let mut skipped = 0usize;
    for candidate in detect_tool_candidates() {
        if !candidate.path.is_file() {
            continue;
        }
        let executable = candidate.path.to_string_lossy().replace('/', "\\");
        let name_key = candidate.name.to_lowercase();
        let path_key = normalize_tool_path(&executable);
        if names.contains(&name_key) || paths.contains(&path_key) {
            skipped += 1;
            continue;
        }
        names.insert(name_key);
        paths.insert(path_key);
        inserts.push((uuid::Uuid::new_v4().to_string(), candidate.name, executable));
    }

    if !inserts.is_empty() {
        state.db.with_conn_mut(|conn| {
            let tx = conn.transaction().map_err(|e| format!("开启事务失败: {}", e))?;
            for (id, name, executable) in &inserts {
                // Windows Terminal 的目录参数必须用 -d；其他工具接受“路径作为最后一个参数”
                let args = if *name == "Windows Terminal" { "-d {path}" } else { "" };
                tx.execute(
                    "INSERT INTO open_tools (id, name, command, executable, args) VALUES (?1,?2,'',?3,?4)",
                    rusqlite::params![id, name, executable, args],
                ).map_err(|e| format!("添加识别到的工具「{}」失败: {}", name, e))?;
            }
            tx.commit().map_err(|e| format!("提交工具识别结果失败: {}", e))
        })?;
    }

    let added = inserts.into_iter().map(|(id, name, executable)| OpenTool {
        id,
        name: name.to_string(),
        command: String::new(),
        executable,
        args: if name == "Windows Terminal" { "-d {path}".to_string() } else { String::new() },
    }).collect();
    Ok(DetectedToolsResult { added, skipped })
}

#[cfg(windows)]
fn env_dir(name: &str) -> Option<PathBuf> {
    let value = std::env::var_os(name)?;
    let path = PathBuf::from(value);
    path.is_dir().then_some(path)
}

#[cfg(windows)]
fn program_files_dir() -> Option<PathBuf> {
    ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .find_map(|name| env_dir(name))
}

#[cfg(windows)]
fn local_app_data_dir() -> Option<PathBuf> {
    env_dir("LOCALAPPDATA")
}

#[cfg(windows)]
fn system_root_dir() -> Option<PathBuf> {
    env_dir("SystemRoot").or_else(|| env_dir("windir"))
}

/// 注册表 App Paths（HKCU/HKLM、含 WOW6432Node）：GUI 程序最可靠的发现方式。
#[cfg(windows)]
fn app_path(exe: &str) -> Option<PathBuf> {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    let key = format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{}", exe);
    let wow = format!(r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\App Paths\{}", exe);
    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        for sub in [&key, &wow] {
            if let Some(value) = crate::core::winenv::reg_value(root, sub, "") {
                let path = PathBuf::from(value);
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    None
}

/// `where.exe` 查 PATH 上的命令（`code`、`cursor`、`wt` 等）。
#[cfg(windows)]
fn where_exe(name: &str) -> Option<PathBuf> {
    use std::os::windows::process::CommandExt;
    let root = system_root_dir().unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let where_path = root.join("System32").join("where.exe");
    let out = std::process::Command::new(where_path)
        .arg(name)
        .env("NoDefaultCurrentDirectoryInExePath", "1")
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| !l.trim().is_empty())?.trim();
    let path = PathBuf::from(line);
    path.is_file().then_some(path)
}

/// 在 JetBrains 安装根下按产品名前缀找 `bin/<exe>`。
///
/// 兼容两种布局：
/// - Program Files：`<product>\bin\idea64.exe`
/// - Toolbox：`<product>\ch-0\<version>\bin\idea64.exe`
#[cfg(windows)]
fn find_product_exe(root: &Path, prefix: &str, exe: &str) -> Option<PathBuf> {
    let prefix = prefix.to_lowercase();
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten().take(100) {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if !name.starts_with(&prefix) {
            continue;
        }
        let base = entry.path();
        let direct = base.join("bin").join(exe);
        if direct.is_file() {
            return Some(direct);
        }
        let Ok(children) = std::fs::read_dir(&base) else { continue };
        for child in children.flatten().take(20) {
            let nested = child.path().join("bin").join(exe);
            if nested.is_file() {
                return Some(nested);
            }
            let Ok(versions) = std::fs::read_dir(child.path()) else { continue };
            for version in versions.flatten().take(20) {
                let path = version.path().join("bin").join(exe);
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    None
}

#[cfg(windows)]
fn jetbrains_ide(prefix: &str, exe: &str) -> Option<PathBuf> {
    // JetBrains 的 exe 基本不在 PATH 上；不逐个跑 `where`（每个进程约几十毫秒，
    // 9 个产品会把一次识别拖到数秒）。App Paths + 安装目录已经覆盖实际场景。
    app_path(exe)
        .or_else(|| {
            program_files_dir()
                .map(|dir| dir.join("JetBrains"))
                .and_then(|root| find_product_exe(&root, prefix, exe))
        })
        .or_else(|| {
            local_app_data_dir()
                .map(|dir| dir.join("JetBrains").join("Toolbox").join("apps"))
                .and_then(|root| find_product_exe(&root, prefix, exe))
        })
}

#[cfg(windows)]
fn detect_tool_candidates() -> Vec<ToolCandidate> {
    let pf = program_files_dir();
    let local = local_app_data_dir();
    let system = system_root_dir();
    let local_programs = local.as_ref().map(|dir| dir.join("Programs"));
    let mut out: Vec<ToolCandidate> = Vec::new();
    let mut add = |name: &'static str, path: Option<PathBuf>| {
        if let Some(path) = path {
            out.push(ToolCandidate { name, path });
        }
    };

    add("VS Code",
        app_path("Code.exe")
            .or_else(|| local_programs.as_ref().map(|dir| dir.join("Microsoft VS Code").join("Code.exe")).filter(|p| p.is_file()))
            .or_else(|| where_exe("code")));
    add("VS Code Insiders",
        app_path("Code - Insiders.exe")
            .or_else(|| local_programs.as_ref().map(|dir| dir.join("Microsoft VS Code Insiders").join("Code - Insiders.exe")).filter(|p| p.is_file()))
            .or_else(|| where_exe("code-insiders")));
    add("Cursor",
        app_path("Cursor.exe")
            .or_else(|| local_programs.as_ref().map(|dir| dir.join("cursor").join("Cursor.exe")).filter(|p| p.is_file()))
            .or_else(|| local_programs.as_ref().map(|dir| dir.join("Cursor").join("Cursor.exe")).filter(|p| p.is_file()))
            .or_else(|| where_exe("cursor")));
    add("Sublime Text",
        app_path("sublime_text.exe")
            .or_else(|| pf.as_ref().map(|dir| dir.join("Sublime Text").join("sublime_text.exe")).filter(|p| p.is_file()))
            .or_else(|| pf.as_ref().map(|dir| dir.join("Sublime Text 3").join("sublime_text.exe")).filter(|p| p.is_file()))
            .or_else(|| where_exe("subl")));
    add("IntelliJ IDEA", jetbrains_ide("intellij idea", "idea64.exe"));
    add("PyCharm", jetbrains_ide("pycharm", "pycharm64.exe"));
    add("WebStorm", jetbrains_ide("webstorm", "webstorm64.exe"));
    add("GoLand", jetbrains_ide("goland", "goland64.exe"));
    add("CLion", jetbrains_ide("clion", "clion64.exe"));
    add("PhpStorm", jetbrains_ide("phpstorm", "phpstorm64.exe"));
    add("RubyMine", jetbrains_ide("rubymine", "rubymine64.exe"));
    add("Rider", jetbrains_ide("rider", "rider64.exe"));
    add("DataGrip", jetbrains_ide("datagrip", "datagrip64.exe"));
    add("Android Studio",
        app_path("studio64.exe")
            .or_else(|| pf.as_ref().map(|dir| dir.join("Android").join("Android Studio").join("bin").join("studio64.exe")).filter(|p| p.is_file()))
            .or_else(|| where_exe("studio64")));
    add("HBuilderX",
        app_path("HBuilderX.exe")
            .or_else(|| pf.as_ref().map(|dir| dir.join("HBuilderX").join("HBuilderX.exe")).filter(|p| p.is_file()))
            .or_else(|| local_programs.as_ref().map(|dir| dir.join("HBuilderX").join("HBuilderX.exe")).filter(|p| p.is_file())));
    add("Windows Terminal",
        where_exe("wt")
            .or_else(|| local.as_ref().map(|dir| dir.join("Microsoft").join("WindowsApps").join("wt.exe")).filter(|p| p.is_file())));
    add("资源管理器", system.as_ref().map(|dir| dir.join("explorer.exe")).filter(|p| p.is_file()));

    out
}

#[cfg(not(windows))]
fn detect_tool_candidates() -> Vec<ToolCandidate> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── render_legacy_command ──────────────────────────────

    #[test]
    fn test_render_legacy_bare_placeholder_gets_quoted() {
        // 裸 {path} 必须包引号：路径含空格/& 时不被 shell 拆成多 token
        assert_eq!(
            render_legacy_command("idea64 {path}", r"C:\My & calc Project").unwrap(),
            "idea64 \"C:\\My & calc Project\""
        );
    }

    #[test]
    fn test_render_legacy_pre_quoted_not_double_quoted() {
        // 模板自带引号：整体替换，不出现 ""C:\x"" 二次包裹
        assert_eq!(
            render_legacy_command("idea64 \"{path}\"", r"C:\My Project").unwrap(),
            "idea64 \"C:\\My Project\""
        );
    }

    #[test]
    fn test_render_legacy_embedded_placeholder() {
        // 占位符嵌在 token 中同样补引号（防元字符逃逸，token 完整性由用户模板负责）
        assert_eq!(
            render_legacy_command("prog dir={path}", r"C:\a&b").unwrap(),
            "prog dir=\"C:\\a&b\""
        );
    }

    #[test]
    fn test_quote_for_cmd_rejects_expansion_and_quotes() {
        // SEC-13：无法安全表达的参数必须报错，而不是静默删字符改写路径
        assert!(quote_for_cmd(r"C:\a%TEMP%b").is_err(), "% 会被 cmd 展开");
        assert!(quote_for_cmd(r"C:\a!b").is_err(), "! 在延迟展开下会被吃");
        assert!(quote_for_cmd("C:\\a\"b").is_err(), "双引号无法可靠转义");
        assert!(quote_for_cmd("C:\\a\nb").is_err(), "换行会截断命令");
        // 原实现会把引号删掉：路径被静默改写（C:\a"b → C:\ab）
        assert_ne!(
            quote_for_cmd("C:\\a\"b").unwrap_or_default(),
            "\"C:\\ab\""
        );
    }

    #[test]
    fn test_quote_for_cmd_doubles_trailing_backslash() {
        // CommandLineToArgvW：尾随反斜杠必须翻倍，否则 \" 被解析成字面引号
        assert_eq!(quote_for_cmd(r"C:\dir\").unwrap(), r#""C:\dir\\""#);
        assert_eq!(quote_for_cmd(r"C:\dir").unwrap(), r#""C:\dir""#);
    }

    // ── split_args ──────────────────────────────────────────

    #[test]
    fn test_split_args_empty() {
        assert_eq!(split_args(""), Vec::<String>::new());
    }

    #[test]
    fn test_split_args_simple() {
        assert_eq!(split_args("--reuse-window --new-window"), vec!["--reuse-window", "--new-window"]);
    }

    #[test]
    fn test_split_args_quoted_space() {
        // 引号内的空格不被拆分
        assert_eq!(split_args("--name \"my project\""), vec!["--name", "my project"]);
    }

    #[test]
    fn test_split_args_quotes_removed() {
        assert_eq!(split_args("\"{path}\""), vec!["{path}"]);
    }

    // ── {path} 注入 ─────────────────────────────────────────

    fn inject(path: &str, template: &str) -> Vec<String> {
        let mut args = split_args(template);
        if args.iter().any(|a| a.contains("{path}")) {
            for a in &mut args {
                if a.contains("{path}") { *a = a.replace("{path}", path); }
            }
        } else {
            args.push(path.to_string());
        }
        args
    }

    #[test]
    fn test_inject_no_template_appends_path() {
        assert_eq!(inject("C:/my dir/svc", ""), vec!["C:/my dir/svc"]);
    }

    #[test]
    fn test_inject_quoted_placeholder_becomes_single_arg() {
        // 模板 {path} 独立 token：路径含空格仍是单参数（argv 语义）
        assert_eq!(inject("C:/my dir/svc", "{path}"), vec!["C:/my dir/svc"]);
        assert_eq!(inject("C:/my dir/svc", "\"{path}\""), vec!["C:/my dir/svc"]);
    }

    #[test]
    fn test_inject_with_flags() {
        assert_eq!(
            inject("C:/svc", "--reuse-window {path}"),
            vec!["--reuse-window", "C:/svc"]
        );
    }

    #[test]
    fn test_inject_with_embedded_placeholder() {
        // 占位符嵌在 token 内也安全（argv 单元素可含空格）
        assert_eq!(inject("C:/my dir", "dir={path}"), vec!["dir=C:/my dir"]);
    }

    /// 回归：绑定查询**必须真的能读到行**。
    ///
    /// 这条测试守的是 `list_bindings_for_project` 的列名契约：原实现写的是
    /// `SELECT s.id, …` 而映射用 `row.get("service_id")`，SQLite 给出的结果列名是短名
    /// `id` → 每一行都解不出来 → 命令整体失败（"解析服务工具绑定失败: Invalid column name"）。
    /// 只建表、不插行的测试**测不出来**（没有行就不会执行映射闭包），所以这里必须插一条绑定。
    #[test]
    fn test_list_bindings_maps_service_id_column() {
        use crate::database::{init_schema, Database};
        let conn = rusqlite::Connection::open_in_memory().expect("内存库");
        init_schema(&conn).expect("建表");
        conn.execute_batch(
            "INSERT INTO projects (id, name, path) VALUES ('p1', 'P', 'C:/p');
             INSERT INTO services (id, project_id, name) VALUES ('s1', 'p1', 'svc');
             INSERT INTO open_tools (id, name, executable) VALUES ('t1', 'IDEA', 'C:/idea64.exe');
             INSERT INTO service_open_tools (service_id, tool_id) VALUES ('s1', 't1');",
        ).expect("填测试数据");
        let db = Database::from_connection(conn);

        let rows = list_bindings_for_project(&db, "p1").expect("绑定查询不应失败");
        assert_eq!(rows.len(), 1, "有一行绑定就必须读到一行");
        assert_eq!(rows[0].service_id, "s1", "service_id 必须映射到服务的 id");
        assert_eq!(rows[0].tool_id, "t1");

        // 没有绑定的项目返回空表而不是报错（前端据此判断"没绑定"）
        let empty = list_bindings_for_project(&db, "p1-none").expect("空项目不应失败");
        assert!(empty.is_empty());
    }

    /// UX-4：删除工具前的引用清单必须**跨项目**给出，且带上项目名。
    ///
    /// 删除工具是级联的（`service_open_tools` 的 `ON DELETE CASCADE` + 模板引用清空），
    /// 而入口只是一个 11px 的垃圾桶图标。这条守住"确认框里能说清会影响哪些服务"——
    /// 漏了项目名，用户在有多个同名服务的项目里根本认不出是哪一条。
    #[test]
    fn test_open_tool_usage_lists_services_across_projects_and_templates() {
        use crate::database::{init_schema, Database};
        let conn = rusqlite::Connection::open_in_memory().expect("内存库");
        init_schema(&conn).expect("建表");
        conn.execute_batch(
            "INSERT INTO projects (id, name, path) VALUES ('p1', '甲项目', 'C:/a'), ('p2', '乙项目', 'C:/b');
             INSERT INTO services (id, project_id, name) VALUES ('s1', 'p1', 'web'), ('s2', 'p2', 'web'), ('s3', 'p1', '未绑定');
             INSERT INTO open_tools (id, name, executable) VALUES ('t1', 'IDEA', 'C:/idea64.exe'), ('t2', 'VSCode', 'C:/code.exe');
             INSERT INTO service_open_tools (service_id, tool_id) VALUES ('s1', 't1'), ('s2', 't1'), ('s3', 't2');
             INSERT INTO service_templates (id, name, command, open_tool_id) VALUES ('tpl1', '常用服务', 'npm start', 't1');",
        ).expect("填测试数据");
        let db = Database::from_connection(conn);

        let usage = open_tool_usage(&db, "t1").expect("查询引用不应失败");
        // ORDER BY 按字节序：乙(U+4E59) 在 甲(U+7532) 之前。顺序不重要，**跨项目都在**才重要
        assert_eq!(usage.services, vec!["乙项目 / web".to_string(), "甲项目 / web".to_string()],
            "两个项目下的服务都要列出来，且带项目名（同名服务否则分不清）");
        assert_eq!(usage.templates, vec!["常用服务".to_string()], "模板上的引用同样会在删除时被清空");

        // 没有引用时是空表而不是报错
        let none = open_tool_usage(&db, "t-none").expect("没有引用不应失败");
        assert!(none.services.is_empty() && none.templates.is_empty());
    }

    #[test]
    fn test_normalize_tool_path_is_case_and_slash_insensitive() {
        assert_eq!(
            normalize_tool_path("C:/Program Files/Code.exe"),
            normalize_tool_path(r"c:\program files\code.exe"),
        );
        assert_eq!(normalize_tool_path(r"C:\Tools\"), r"c:\tools");
    }

    /// 探测结果不能有重名/重路径；只做结构性检查，不假设本机装了哪些工具。
    #[cfg(windows)]
    #[test]
    fn test_detected_tool_candidates_are_unique_and_exist() {
        let mut names = HashSet::new();
        let mut paths = HashSet::new();
        for candidate in detect_tool_candidates() {
            assert!(candidate.path.is_file(), "候选路径不存在: {}", candidate.path.display());
            assert!(names.insert(candidate.name), "重复工具名: {}", candidate.name);
            assert!(
                paths.insert(normalize_tool_path(&candidate.path.to_string_lossy())),
                "重复工具路径: {}",
                candidate.path.display()
            );
        }
    }
}

