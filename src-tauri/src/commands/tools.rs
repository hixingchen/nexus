use serde::Deserialize;
use tauri::State;
use crate::AppState;
use crate::models::{OpenTool, ServiceOpenToolBinding};

/// 外部打开工具（服务右键「用 XX 打开」）
///
/// 新模型：executable（程序路径）+ args（参数模板）。`{path}` 占位符以
/// **独立参数**注入（不经过 shell 字符串拼接）——路径含空格/特殊字符天然安全。
/// command 字段为历史遗留（整串 shell 命令），仅 executable 为空的历史行使用。
///
/// 安全边界：executable/args 来自用户自己的配置，与「工具命令」同一信任级别。

// ─── CRUD ───────────────────────────────────────────────────

/// 工具列表（rowid 序 = 添加顺序）
#[tauri::command]
pub fn list_open_tools(state: State<AppState>) -> Result<Vec<OpenTool>, String> {
    state.db.with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT id, name, command, executable, args FROM open_tools ORDER BY rowid")
            .map_err(|e| format!("查询打开工具失败: {}", e))?;
        let rows = stmt.query_map([], |row| {
            Ok(OpenTool {
                id: row.get(0)?, name: row.get(1)?, command: row.get(2)?,
                executable: row.get(3)?, args: row.get(4)?,
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
    state.db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT s.id, b.tool_id FROM service_open_tools b JOIN services s ON s.id = b.service_id WHERE s.project_id=?1"
        ).map_err(|e| format!("查询服务工具绑定失败: {}", e))?;
        let rows = stmt.query_map([&project_id], |row| {
            Ok(ServiceOpenToolBinding { service_id: row.get(0)?, tool_id: row.get(1)? })
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
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?)),
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
                r.get::<_, String>(0)?, r.get::<_, String>(1)?,
                r.get::<_, String>(2)?, r.get::<_, String>(3)?,
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
        spawn_detached(&render_legacy_command(&legacy_command, &cwd))
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
            let mut parts = vec![format!("\"{}\"", executable)];
            parts.extend(args.iter().map(|a| format!("\"{}\"", a)));
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

/// 渲染历史行命令：`{path}` 占位替换并包双引号。
///
/// 历史行最终经 `cmd /C`/`sh -c` 整串执行——cwd 含空格时断参、含 `&`/`|` 等
/// 元字符时会被 shell 解释执行。包引号后空格与大多数元字符只作为路径一部分。
/// 模板自带引号（`"{path}"`）时先整体替换，避免二次包引号出现 `""C:\x""`。
/// 注意信任边界：command 本身仍是用户配置的 shell 命令串（与工具命令同级），
/// 本函数只保证**注入的路径**不成为新命令。
fn render_legacy_command(template: &str, path: &str) -> String {
    let quoted = format!("\"{}\"", path.replace('"', ""));
    template
        .replace("\"{path}\"", &quoted)
        .replace("{path}", &quoted)
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── render_legacy_command ──────────────────────────────

    #[test]
    fn test_render_legacy_bare_placeholder_gets_quoted() {
        // 裸 {path} 必须包引号：路径含空格/& 时不被 shell 拆成多 token
        assert_eq!(
            render_legacy_command("idea64 {path}", r"C:\My & calc Project"),
            "idea64 \"C:\\My & calc Project\""
        );
    }

    #[test]
    fn test_render_legacy_pre_quoted_not_double_quoted() {
        // 模板自带引号：整体替换，不出现 ""C:\x"" 二次包裹
        assert_eq!(
            render_legacy_command("idea64 \"{path}\"", r"C:\My Project"),
            "idea64 \"C:\\My Project\""
        );
    }

    #[test]
    fn test_render_legacy_embedded_placeholder() {
        // 占位符嵌在 token 中同样补引号（防元字符逃逸，token 完整性由用户模板负责）
        assert_eq!(
            render_legacy_command("prog dir={path}", r"C:\a&b"),
            "prog dir=\"C:\\a&b\""
        );
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
}

