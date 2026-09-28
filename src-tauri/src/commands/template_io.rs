//! 服务模板的**导入导出文件格式**（ARCH-24：从 `commands/service.rs` 拆出来）。
//!
//! 为什么值得单独一个模块：这一块与服务的增删改查不是一回事——它定义的是**磁盘上的文件格式**
//! （`TEMPLATE_EXPORT_VERSION` + camelCase 序列化，要能手工看、手工改），而 IPC 的响应 DTO
//! 一律 snake_case（见 `contract.rs` 顶部的规则）。两种命名约定住在一个文件里时，把某一边
//! 改错不会有任何编译错误，只会在用户导入别人机器上的文件时才暴露。
//!
//! 与 `service.rs` 的耦合实际为零：这里只碰 `service_templates` / `open_tools` 两张表与
//! `PathAllowlist`，**不调用任何服务 CRUD**——所以拆出来不需要新暴露任何 `pub(crate)`。

use tauri::State;
use serde::{Deserialize, Serialize};
use crate::AppState;
use crate::commands::paths::PathAllowlist;

/// 导出文件里的单个模板。与 ServiceTemplate 两点不同：
/// - **没有 id**：导入方自己生成——跨机复制会撞 id，同机重复导入也会与源模板冲突
/// - **openToolName 而不是 openToolId**：工具 id 是本机工具库的主键，换台机器指向别的
///   工具或压根不存在（静默指错）。名字是两台机器之间唯一有意义的标识
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportedTemplate {
    pub name: String,
    pub command: String,
    pub cwd: String,
    #[serde(default)]
    pub watch_paths: String,
    #[serde(default)]
    pub watch_include: String,
    #[serde(default)]
    pub watch_exclude: String,
    #[serde(default)]
    pub env_vars: String,
    #[serde(default)]
    pub restart_mode: i32,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub show_file_tree: bool,
    #[serde(default)]
    pub tool_commands: String,
    #[serde(default)]
    pub open_tool_name: String,
}

/// 导出文件的顶层结构（带 version：将来加字段时能认出旧文件）
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateExportFile {
    pub version: u32,
    pub templates: Vec<ExportedTemplate>,
}

/// 当前导出格式版本
const TEMPLATE_EXPORT_VERSION: u32 = 1;

/// 导入结果——要让界面能如实说明"到底发生了什么"，而不是一句"导入成功"。
///
/// 响应 DTO 一律 snake_case（见 contract.rs 顶部的规则），前端类型原样对应；
/// 上面的导出文件结构是**文件格式**不是 IPC 载荷，所以用 camelCase 便于阅读与手工编辑
#[derive(Serialize)]
pub struct ImportTemplatesResult {
    pub imported: usize,
    /// 与库里已有模板同名的数量（**只统计、不改名**）。
    ///
    /// 为什么不像最初那样自动加「(2)」后缀：库里本来就允许同名（`service_templates.name`
    /// 上没有唯一约束），而同一个功能里的另一个入口——「另存为模板」——从不加后缀。
    /// 加后缀是导入这边**自己加的**约束，还让用户导入完得挨个改回来。
    pub duplicated: usize,
    /// 本机工具库里没有、因而没绑上的工具名（已去重）
    pub missing_tools: Vec<String>,
    /// 工作目录在本机不存在的模板名。导入**不会**因此失败（模板只是配置，
    /// 真正用到目录是在"从模板添加服务"那一步，那里有收口），但用户需要知道
    /// "导入完为什么不能直接用"
    pub missing_dirs: Vec<String>,
}

/// 导出模板到 JSON 文件。`ids` 为空 = 导出全部；否则只导出指定模板
///
/// **写入路径要过收口**（SEC-19，与 `write_file` 同一套）：`path` 来自 IPC，
/// 原实现直接 `std::fs::write` ——任意路径覆盖写，且写入内容里有攻击者可控的字符串。
/// 用户挑位置这一步走 `pick_save_file`（Rust 侧弹框，选中的目录当场进 `confirmed_dirs`），
/// 于是"导出到桌面/文档"照常可用，而凭空指定 `C:/Windows/...` 会被拒。
#[tauri::command]
pub fn export_service_templates(
    state: State<AppState>,
    path: String,
    ids: Vec<String>,
) -> Result<usize, String> {
    let target = path.trim();
    if target.is_empty() { return Err("导出路径不能为空".into()); }

    // 与写文件同口径的两道闸：① 路径级校验（目标不存在时按父目录校验，支持新建文件）；
    // ② 句柄级复核（SEC-4）——校验与写入之间，项目内的并发构建可以把末段换成目录联接
    state.paths.check_write_path_allowed(&state.db, target)?;
    let resolved: std::path::PathBuf = {
        let exists = std::path::Path::new(target).exists();
        match state.paths.open_verified_file(&state.db, target) {
            Ok((_f, real)) => real,
            Err(e) => {
                if exists {
                    return Err(e); // 存在但复核不通过：拒绝，不当作"新建"
                }
                let p = std::path::Path::new(target);
                let parent = p.parent().ok_or_else(|| "访问被拒绝".to_string())?;
                let name = p.file_name().ok_or_else(|| "访问被拒绝".to_string())?;
                let (_d, real_parent) = state
                    .paths
                    .open_verified_dir(&state.db, &parent.to_string_lossy())?;
                real_parent.join(name)
            }
        }
    };

    let picked = state.db.with_conn(|conn| collect_export_templates(conn, &ids))?;
    if picked.is_empty() { return Err("没有可导出的模板".into()); }
    let count = picked.len();

    let payload = TemplateExportFile { version: TEMPLATE_EXPORT_VERSION, templates: picked };
    let json = serde_json::to_string_pretty(&payload).map_err(|e| format!("生成导出内容失败: {}", e))?;
    std::fs::write(&resolved, json).map_err(|e| format!("写入文件失败: {}", e))?;
    Ok(count)
}

/// 收集要导出的模板（ids 为空 = 全部）。抽出来是为了可单测——
/// Tauri 命令签名带 `State`，测试构造不出来
fn collect_export_templates(
    conn: &rusqlite::Connection,
    ids: &[String],
) -> Result<Vec<ExportedTemplate>, String> {
    // LEFT JOIN 取工具名：没绑工具时 tool_name 为空
    let mut stmt = conn.prepare(
        "SELECT t.name, t.command, t.cwd, t.watch_paths, t.watch_include, t.watch_exclude,
                t.env_vars, t.restart_mode, t.enabled, t.show_file_tree, t.tool_commands,
                t.id, COALESCE(o.name, '') AS tool_name
         FROM service_templates t
         LEFT JOIN open_tools o ON o.id = t.open_tool_id
         ORDER BY t.sort_index, t.name"
    ).map_err(|e| format!("查询模板失败: {}", e))?;
    let rows = stmt.query_map([], |row| Ok((
        row.get::<_, String>("id")?,
        ExportedTemplate {
            name: row.get("name")?,
            command: row.get("command")?,
            cwd: row.get("cwd")?,
            watch_paths: row.get("watch_paths")?,
            watch_include: row.get("watch_include")?,
            watch_exclude: row.get("watch_exclude")?,
            env_vars: row.get("env_vars")?,
            restart_mode: row.get("restart_mode")?,
            enabled: row.get("enabled")?,
            show_file_tree: row.get("show_file_tree")?,
            tool_commands: row.get("tool_commands")?,
            open_tool_name: row.get("tool_name")?,
        },
    ))).map_err(|e| format!("查询模板失败: {}", e))?;
    let all = rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("查询模板失败: {}", e))?;

    Ok(if ids.is_empty() {
        all.into_iter().map(|(_, t)| t).collect()
    } else {
        all.into_iter().filter(|(id, _)| ids.contains(id)).map(|(_, t)| t).collect()
    })
}

/// 从 JSON 文件导入模板。
///
/// **cwd 收口**（P0-5）：导入的 cwd 若在本机**存在**，必须与 create/update 走同一道闸；
/// 本机不存在的照旧留着（导入的是别人机器上的配置，收口会让正常导入失败），并记进
/// `missing_dirs` 提醒用户——那类路径不会成为白名单根，`allowed_roots` 对 canonicalize
/// 失败的根一律丢弃。
///
/// 原实现写着"不做目录收口……未收口的模板不会被真的用起来"，那句推理不成立：
/// `service_templates.cwd` 本身就是文件白名单的根来源（`paths.rs` 的三表 UNION），
/// 模板**不需要被用起来**就已经扩大了可读写范围。四个模板写入口里只有导入这条漏了。
#[tauri::command]
pub fn import_service_templates(
    state: State<AppState>,
    path: String,
) -> Result<ImportTemplatesResult, String> {
    let source = path.trim();
    if source.is_empty() { return Err("导入路径不能为空".into()); }
    let raw = std::fs::read_to_string(source).map_err(|e| format!("读取文件失败: {}", e))?;
    let file: TemplateExportFile = serde_json::from_str(&raw)
        .map_err(|e| format!("不是有效的模板导出文件: {}", e))?;
    if file.version != TEMPLATE_EXPORT_VERSION {
        return Err(format!("导出文件版本 {} 不受支持（当前支持 {}）", file.version, TEMPLATE_EXPORT_VERSION));
    }

    // `paths` 传进去而不是在事务里取：收口要用 confirmed_dirs（会话内确认过的目录）
    state.db.with_conn_mut(|conn| import_templates_into(conn, &file, &state.paths))
}

/// 导入的核心逻辑（与命令分开：命令只管读文件与解析，这里拿得到连接、可单测）
fn import_templates_into(
    conn: &mut rusqlite::Connection,
    file: &TemplateExportFile,
    paths: &PathAllowlist,
) -> Result<ImportTemplatesResult, String> {
    let tx = conn.transaction().map_err(|e| format!("开启事务失败: {}", e))?;
    let mut imported = 0usize;
    let mut duplicated = 0usize;
    let mut missing_tools: Vec<String> = Vec::new();
    let mut missing_dirs: Vec<String> = Vec::new();

    // 整批一个事务：导入要么全成、要么全不成。半途失败留下"导了一半"的模板库，
    // 用户看到的是一堆残缺数据，还不如重来
    for t in &file.templates {
        let name = t.name.trim().to_string();
        if name.is_empty() { continue; } // 没名字的行没法用，跳过（计数里也不含）
        // 同名照收，名字保持原样（见 ImportTemplatesResult.duplicated 的说明）。
        // 只统计一下，好让界面能如实告诉用户"有 N 个与现有的同名"
        let taken: i64 = tx
            .query_row("SELECT COUNT(*) FROM service_templates WHERE name=?1", [&name], |r| r.get(0))
            .unwrap_or(0);
        if taken > 0 { duplicated += 1; }

        // 工具按**名字**在本机工具库里找；找不到就不绑（记下来告诉用户）
        let tool_id = if t.open_tool_name.trim().is_empty() {
            String::new()
        } else {
            let found = tx
                .query_row("SELECT id FROM open_tools WHERE name=?1 LIMIT 1", [t.open_tool_name.trim()], |r| r.get::<_, String>(0))
                .ok();
            match found {
                Some(id) => id,
                None => {
                    let n = t.open_tool_name.trim().to_string();
                    if !missing_tools.contains(&n) { missing_tools.push(n); }
                    String::new()
                }
            }
        };

        let cwd = t.cwd.replace('\\', "/");
        let cwd_trim = cwd.trim();
        if !cwd_trim.is_empty() {
            if !std::path::Path::new(cwd_trim).is_dir() {
                // 本机没这个目录：留着，只记一笔（用户随后可以自己改成自己的路径）
                missing_dirs.push(name.clone());
            } else {
                // 存在的目录 = 一个**即将进入白名单的根**，必须与 create/update 同一套收口。
                // 放在事务里校验（用同一个连接）是为了不让"整批一个事务"的语义出现缺口：
                // 拒绝时整个导入失败，库里不会留下半截模板
                paths
                    .ensure_config_dir_allowed_with_conn(&tx, cwd_trim, "工作目录")
                    .map_err(|e| format!("模板「{}」的工作目录无法导入：{}", name, e))?;
            }
        }

        let id = uuid::Uuid::new_v4().to_string();
        let en = if t.enabled { 1 } else { 0 };
        let sft = if t.show_file_tree { 1 } else { 0 };
        let tool_commands = if t.tool_commands.trim().is_empty() { "[]".to_string() } else { t.tool_commands.clone() };
        let next_sort: i32 = tx
            .query_row("SELECT COALESCE(MAX(sort_index), -1) + 1 FROM service_templates", [], |r| r.get(0))
            .unwrap_or(0);
        tx.execute(
            &format!(
                "INSERT INTO service_templates ({}) VALUES ({})",
                crate::database::SERVICE_TEMPLATE_COLUMNS,
                crate::database::named_placeholders(crate::database::SERVICE_TEMPLATE_COLUMNS),
            ),
            rusqlite::named_params! {
                ":id": id, ":name": name, ":command": t.command, ":cwd": cwd,
                ":watch_paths": t.watch_paths, ":watch_include": t.watch_include,
                ":watch_exclude": t.watch_exclude, ":env_vars": t.env_vars,
                ":restart_mode": t.restart_mode, ":enabled": en, ":show_file_tree": sft,
                ":sort_index": next_sort, ":tool_commands": tool_commands, ":open_tool_id": tool_id,
            },
        ).map_err(|e| format!("导入模板失败: {}", e))?;
        imported += 1;
    }

    tx.commit().map_err(|e| format!("提交事务失败: {}", e))?;
    Ok(ImportTemplatesResult { imported, duplicated, missing_tools, missing_dirs })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::{init_schema, Database};

    /// 内存库 + 一个项目行。
    ///
    /// 与 `service.rs` 的测试同形但各自持有：两个模块的测试互不依赖，
    /// 谁改了对方的夹具都不该影响这边。
    fn test_db() -> Database {
        let conn = rusqlite::Connection::open_in_memory().expect("内存库");
        init_schema(&conn).expect("建表");
        conn.execute(
            "INSERT INTO projects (id, name, path, sort_index, created_at) VALUES ('p1','P','C:/p',0,'t')",
            [],
        ).expect("插入项目");
        Database::from_connection(conn)
    }

    /// 测试用的空模板：只填必要的名字/命令，其余留空
    fn exported(name: &str, tool: &str) -> ExportedTemplate {
        ExportedTemplate {
            name: name.into(),
            command: "npm run dev".into(),
            cwd: String::new(),
            watch_paths: "[]".into(),
            watch_include: String::new(),
            watch_exclude: String::new(),
            env_vars: String::new(),
            restart_mode: 0,
            enabled: true,
            show_file_tree: false,
            tool_commands: "[]".into(),
            open_tool_name: tool.into(),
        }
    }

    /// 导出 → 序列化 → 读回 → 导入：配置原样搬过去，**id 不带**（跨机会撞、
    /// 同机重复导入也会与源模板冲突，所以导出格式里根本没有 id 字段）
    #[test]
    fn test_template_export_import_roundtrip() {
        let db = test_db();
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO service_templates (id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, sort_index, tool_commands, open_tool_id)
                 VALUES ('t1','前端 dev','npm run dev','','[]','','','K=V',1,1,0,0,'[]','')",
                [],
            ).map_err(|e| e.to_string())?;
            Ok(())
        }).expect("插入模板");

        let exported_list = db.with_conn(|c| collect_export_templates(c, &[])).expect("导出");
        assert_eq!(exported_list.len(), 1);
        assert_eq!(exported_list[0].name, "前端 dev");
        assert_eq!(exported_list[0].env_vars, "K=V", "配置要原样带出去");

        // 落盘再读回（导出→导入的真实路径）
        let file = TemplateExportFile { version: TEMPLATE_EXPORT_VERSION, templates: exported_list };
        let json = serde_json::to_string(&file).expect("序列化导出文件");
        let back: TemplateExportFile = serde_json::from_str(&json).expect("反序列化导出文件");

        // 导入回同一个库：名字会撞上源模板 → 照收、不改名，只在结果里报"有同名"
        let res = db.with_conn_mut(|c| import_templates_into(c, &back, &PathAllowlist::new())).expect("导入");
        assert_eq!(res.imported, 1);
        assert_eq!(res.duplicated, 1, "同名只统计、不改名");
        assert!(res.missing_tools.is_empty(), "没绑工具就不该报缺失");
        assert!(res.missing_dirs.is_empty(), "cwd 为空不算缺失目录");

        db.with_conn(|c| {
            let n: i64 = c
                .query_row("SELECT COUNT(*) FROM service_templates WHERE name='前端 dev'", [], |r| r.get(0))
                .map_err(|e| e.to_string())?;
            assert_eq!(n, 2, "两条都该叫原名");
            Ok(())
        }).expect("校验名字保持原样");
    }

    /// 同名不改名、工具按名字匹配、缺工具与缺目录都要报出来
    #[test]
    fn test_import_keeps_duplicate_names_and_matches_tool_by_name() {
        let db = test_db();
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO service_templates (id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, sort_index, tool_commands, open_tool_id)
                 VALUES ('t1','T','c','','[]','','','',0,1,0,0,'[]','')",
                [],
            ).map_err(|e| e.to_string())?;
            c.execute(
                "INSERT INTO open_tools (id, name, command, executable, args) VALUES ('tool1','IDEA','','idea64.exe','{path}')",
                [],
            ).map_err(|e| e.to_string())?;
            Ok(())
        }).expect("准备数据");

        let mut with_missing_dir = exported("带目录的", "");
        with_missing_dir.cwd = format!("C:/nexus-ut-nonexistent-{}", std::process::id());
        let file = TemplateExportFile {
            version: TEMPLATE_EXPORT_VERSION,
            templates: vec![
                exported("T", "IDEA"),          // 名字撞库里的 T；工具在本机有
                exported("U", "不存在的工具"),   // 名字不撞；工具在本机没有
                with_missing_dir,                // 目录在本机不存在
            ],
        };

        let res = db.with_conn_mut(|c| import_templates_into(c, &file, &PathAllowlist::new())).expect("导入");
        assert_eq!(res.imported, 3);
        assert_eq!(res.duplicated, 1, "只有与 T 撞名的那条被统计");
        assert_eq!(res.missing_tools, vec!["不存在的工具".to_string()]);
        assert_eq!(res.missing_dirs, vec!["带目录的".to_string()]);

        db.with_conn(|c| {
            let names: Vec<String> = c
                .prepare("SELECT name FROM service_templates ORDER BY name").map_err(|e| e.to_string())?
                .query_map([], |r| r.get(0)).map_err(|e| e.to_string())?
                .collect::<Result<_, _>>().map_err(|e| e.to_string())?;
            // 两条 T 都在、名字一模一样："T"(0x54) < "U"(0x55) < "带"(E5B8A6)
            assert_eq!(names, vec!["T".to_string(), "T".to_string(), "U".to_string(), "带目录的".to_string()]);

            // 工具按名字绑到本机的 IDEA（不是导出方的 id）
            let bound: String = c
                .query_row("SELECT open_tool_id FROM service_templates WHERE name='T' AND open_tool_id<>''", [], |r| r.get(0))
                .map_err(|e| e.to_string())?;
            assert_eq!(bound, "tool1");

            let unbound: i64 = c
                .query_row("SELECT COUNT(*) FROM service_templates WHERE name='U' AND open_tool_id=''", [], |r| r.get(0))
                .map_err(|e| e.to_string())?;
            assert_eq!(unbound, 1, "本机没有同名工具就不绑");
            Ok(())
        }).expect("校验落库结果");
    }

    /// 走**真实文件**的导出→导入：命令里做的就是"收集 → 序列化写盘"，再"读盘 → 反序列化"。
    /// 只测内存里的序列化会漏掉"落盘后读不回来"这类问题（这正是铁律 19 说的那条路径）
    #[test]
    fn test_export_import_through_real_file() {
        let db = test_db();
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO service_templates (id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, sort_index, tool_commands, open_tool_id)
                 VALUES ('t1','带出去','echo hi','','[]','','','',0,1,0,0,'[]','')",
                [],
            ).map_err(|e| e.to_string())?;
            Ok(())
        }).expect("插入模板");

        let dir = std::env::temp_dir().join(format!("nexus_ut_tpl_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let file = dir.join("templates.json");

        // 导出
        let picked = db.with_conn(|c| collect_export_templates(c, &[])).expect("收集模板");
        assert_eq!(picked.len(), 1);
        let payload = TemplateExportFile { version: TEMPLATE_EXPORT_VERSION, templates: picked };
        let json = serde_json::to_string_pretty(&payload).expect("序列化");
        std::fs::write(&file, &json).expect("写文件");
        assert!(json.contains("\n  "), "导出文件应当是缩进过的（要能手工看/改）");

        // 读回并导入（导回同一个库，必然同名 → 走改名分支）
        let raw = std::fs::read_to_string(&file).expect("读文件");
        let back: TemplateExportFile = serde_json::from_str(&raw).expect("反序列化");
        let res = db.with_conn_mut(|c| import_templates_into(c, &back, &PathAllowlist::new())).expect("导入");
        assert_eq!(res.imported, 1);
        assert_eq!(res.duplicated, 1, "导回同一个库必然同名 → 只统计不改名");
        assert!(res.missing_dirs.is_empty(), "cwd 为空不算缺失目录");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 库里是空的、文件里也没有重名时，**一个后缀都不该加**。
    ///
    /// 起因是用户反馈"我模板里一个都没有，导入却直接加后缀，还得一个一个改"。
    /// 把"什么情况下才该加后缀"钉死：只有真的撞名才加，免得日后放宽条件。
    #[test]
    fn test_import_without_conflicts_keeps_original_names() {
        let db = test_db();
        let file = TemplateExportFile {
            version: TEMPLATE_EXPORT_VERSION,
            templates: vec![exported("前端", ""), exported("后端", ""), exported("redis", "")],
        };
        let res = db.with_conn_mut(|c| import_templates_into(c, &file, &PathAllowlist::new())).expect("导入");
        assert_eq!(res.imported, 3);
        assert_eq!(res.duplicated, 0, "没有同名就没有可报的冲突");

        db.with_conn(|c| {
            let names: Vec<String> = c
                .prepare("SELECT name FROM service_templates ORDER BY name").map_err(|e| e.to_string())?
                .query_map([], |r| r.get(0)).map_err(|e| e.to_string())?
                .collect::<Result<_, _>>().map_err(|e| e.to_string())?;
            // ORDER BY name 是**字节序**："前"(E5 89 8D) < "后"(E5 90 8E)，不是拼音顺序
            assert_eq!(names, vec!["redis".to_string(), "前端".to_string(), "后端".to_string()]);
            Ok(())
        }).expect("校验名字未被改动");
    }

    /// 导出文件**内部**就有重名时：三条照收，名字**全部保持原样**。
    ///
    /// 这是"库里明明空着却被加了后缀"最常见的原因——文件里本身就有几个同名模板
    /// （同一个项目里几个都叫「前端」的服务，各另存了一次）。加后缀的话，用户导入完
    /// 还得挨个把「前端 (2)」「前端 (3)」改回来。
    #[test]
    fn test_import_keeps_names_when_file_has_duplicates() {
        let db = test_db();
        let file = TemplateExportFile {
            version: TEMPLATE_EXPORT_VERSION,
            templates: vec![exported("前端", ""), exported("前端", ""), exported("前端", "")],
        };
        let res = db.with_conn_mut(|c| import_templates_into(c, &file, &PathAllowlist::new())).expect("导入");
        assert_eq!(res.imported, 3);
        assert_eq!(res.duplicated, 2, "第 2、3 条与刚落库的第 1 条同名");

        db.with_conn(|c| {
            let names: Vec<String> = c
                .prepare("SELECT name FROM service_templates ORDER BY name").map_err(|e| e.to_string())?
                .query_map([], |r| r.get(0)).map_err(|e| e.to_string())?
                .collect::<Result<_, _>>().map_err(|e| e.to_string())?;
            assert_eq!(
                names,
                vec!["前端".to_string(), "前端".to_string(), "前端".to_string()],
                "三条都叫原名，一个后缀都不加",
            );
            Ok(())
        }).expect("校验名字");
    }

    /// 重复导入同一份文件：产生两份**同名**副本（与"把同名服务另存两次"的行为一致），
    /// 删哪份由用户自己决定
    #[test]
    fn test_import_twice_keeps_both_copies_with_same_name() {
        let db = test_db();
        let file = TemplateExportFile {
            version: TEMPLATE_EXPORT_VERSION,
            templates: vec![exported("同一个模板", "")],
        };
        db.with_conn_mut(|c| import_templates_into(c, &file, &PathAllowlist::new())).expect("第一次导入");
        let res = db.with_conn_mut(|c| import_templates_into(c, &file, &PathAllowlist::new())).expect("第二次导入");
        assert_eq!(res.imported, 1);
        assert_eq!(res.duplicated, 1, "第二次与第一次同名 → 统计但不改名");

        db.with_conn(|c| {
            let names: Vec<String> = c
                .prepare("SELECT name FROM service_templates ORDER BY name").map_err(|e| e.to_string())?
                .query_map([], |r| r.get(0)).map_err(|e| e.to_string())?
                .collect::<Result<_, _>>().map_err(|e| e.to_string())?;
            assert_eq!(names, vec!["同一个模板".to_string(), "同一个模板".to_string()]);
            Ok(())
        }).expect("校验");
    }

    /// P0-5：导入的 cwd 若在本机**存在**，必须与 create/update 同一套收口。
    ///
    /// 修复前的症状：一个 JSON（`{"version":1,"templates":[{"name":"x","command":"y","cwd":"D:/"}]}`）
    /// 就能把任意已存在目录写进 `service_templates.cwd`，而它是文件读写白名单的**根来源**
    /// （`paths.rs` 的三表 UNION）——模板根本不需要被"用起来"就已经扩大了可读写范围。
    #[test]
    fn test_import_rejects_existing_cwd_outside_allowlist() {
        let db = test_db();
        // 一个真实存在、但不在任何已登记目录之下的目录
        let outside = std::env::temp_dir().join(format!("nexus_ut_import_{}", std::process::id()));
        std::fs::create_dir_all(&outside).expect("建临时目录");

        let mut t = exported("投毒模板", "");
        t.cwd = outside.to_string_lossy().to_string();
        let file = TemplateExportFile { version: TEMPLATE_EXPORT_VERSION, templates: vec![t] };

        let err = match db.with_conn_mut(|c| import_templates_into(c, &file, &PathAllowlist::new())) {
            Err(e) => e,
            Ok(r) => panic!("白名单外的已存在目录必须拒绝，实际导入了 {} 条", r.imported),
        };
        assert!(err.contains("投毒模板"), "错误要说清是哪个模板: {}", err);

        // 整批拒绝：事务里不能留下半截数据（否则"报错了但库里多了一条"更难查）
        let n: i64 = db.with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM service_templates", [], |r| r.get(0)).unwrap())).unwrap();
        assert_eq!(n, 0, "被拒绝的导入不能留下任何模板行");

        let _ = std::fs::remove_dir_all(&outside);
    }

    /// 正面控制：**已登记目录内**的已存在 cwd 照常导入。
    /// 没有这条，上面的收口可以靠"一律拒绝"通过——那会把正常导入一起打死。
    #[test]
    fn test_import_accepts_existing_cwd_inside_registered_dir() {
        let db = test_db();
        let project_dir = std::env::temp_dir().join(format!("nexus_ut_proj_{}", std::process::id()));
        let cwd = project_dir.join("sub");
        std::fs::create_dir_all(&cwd).expect("建项目子目录");
        // 把已存在的这个目录登记为项目目录（历史配置视为已授权）
        let path = project_dir.to_string_lossy().replace('\\', "/");
        db.with_conn(|c| {
            c.execute("UPDATE projects SET path=?1 WHERE id='p1'", [&path]).map_err(|e| e.to_string())
        }).expect("登记项目目录");

        let mut t = exported("正常模板", "");
        t.cwd = cwd.to_string_lossy().replace('\\', "/");
        let file = TemplateExportFile { version: TEMPLATE_EXPORT_VERSION, templates: vec![t] };

        let res = db.with_conn_mut(|c| import_templates_into(c, &file, &PathAllowlist::new())).expect("导入应成功");
        assert_eq!(res.imported, 1);
        assert!(res.missing_dirs.is_empty(), "目录存在就不该报缺失");

        let _ = std::fs::remove_dir_all(&project_dir);
    }
}
