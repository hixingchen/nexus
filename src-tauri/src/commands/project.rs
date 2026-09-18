use tauri::{Manager, State};
use crate::AppState;
use crate::models::{Project, ProjectDetail};
use crate::database::query_services_by_project;

/// 获取所有项目（不含服务详情，列表用）
#[tauri::command]
pub fn get_projects(state: State<AppState>) -> Result<Vec<Project>, String> {
    state.db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, name, path, pinned, sort_index FROM projects ORDER BY pinned DESC, sort_index"
        ).map_err(|e| format!("查询项目列表失败: {}", e))?;
        let rows = stmt.query_map([], |row| {
            Ok(Project {
                id: row.get("id")?, name: row.get("name")?, path: row.get("path")?,
                pinned: row.get::<_, i32>("pinned")? != 0,
                sort_index: row.get("sort_index")?,
            })
        }).map_err(|e| format!("读取项目数据失败: {}", e))?;
        let mut projects = Vec::new();
        for r in rows { projects.push(r.map_err(|e| format!("解析项目数据失败: {}", e))?); }
        Ok(projects)
    })
}

/// 获取项目详情（含所有服务）
#[tauri::command]
pub fn get_project_detail(state: State<AppState>, project_id: String) -> Result<ProjectDetail, String> {
    if project_id.trim().is_empty() { return Err("项目ID不能为空".into()); }
    state.db.with_conn(|conn| {
        let project = conn.query_row(
            "SELECT id, name, path, pinned, sort_index FROM projects WHERE id=?1",
            [&project_id],
            |row| Ok(Project {
                id: row.get("id")?, name: row.get("name")?, path: row.get("path")?,
                pinned: row.get::<_, i32>("pinned")? != 0,
                sort_index: row.get("sort_index")?,
            })
        ).map_err(|e| format!("项目不存在: {}", e))?;

        let services = query_services_by_project(conn, &project_id)?;
        Ok(ProjectDetail { project, services })
    })
}

/// 复制项目时名称重试上限
const MAX_DUPLICATE_NAME_ATTEMPTS: u32 = 1000;

/// 创建项目
#[tauri::command]
pub fn add_project(state: State<AppState>, name: String, path: String) -> Result<Project, String> {
    if name.trim().is_empty() { return Err("项目名称不能为空".into()); }
    let path = path.trim().replace('\\', "/");
    if path.is_empty() { return Err("项目路径不能为空".into()); }
    if !std::path::Path::new(&path).is_dir() {
        return Err("项目路径不存在或不是目录".into());
    }
    // 项目路径会直接成为文件访问白名单的根：新路径必须是用户经原生选择器确认过的目录
    // （见 commands/paths.rs 的"配置路径收口"），驱动器根/系统目录等一律拒绝
    state.paths.ensure_config_dir_allowed(&state.db, &path, "项目路径")?;
    write_project_add(&state.db, &name, &path)
}

/// `add_project` 的 DB 部分（抽成自由函数以便测试：命令壳需要 `State<AppState>`，
/// 而"新项目排在哪一位"这条语义值得被钉住——与 `write_project_duplicate` 同一范式）
fn write_project_add(db: &crate::database::Database, name: &str, path: &str) -> Result<Project, String> {
    db.with_conn(|conn| {
        // 查重失败（DB 错误）不能当作"不存在"：那会放进一个重名项目
        let exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM projects WHERE name=?1",
            [name.trim()], |r| r.get(0)
        ).map_err(|e| format!("查询项目名是否重复失败: {}", e))?;
        if exists { return Err(format!("项目「{}」已存在", name.trim())); }

        let id = uuid::Uuid::new_v4().to_string();
        // 新项目插到**最前**（sort_index 取最小值 - 1）而不是追加到末尾。
        //
        // 为什么：项目列表**没有拖拽排序**（只有收藏），所以 sort_index 实际上就是"添加顺序"，
        // 而最新添加的正是刚要用那个——追加到末尾等于把它放在离用户最远的地方。
        // 服务列表可以拖动排序，所以那边保持"追加到末尾"（顺序属于用户，不该被新增打乱）。
        // 走负数没有副作用（INTEGER），收藏仍然优先（排序是 `pinned DESC, sort_index`）。
        let min_sort: i32 = conn.query_row(
            "SELECT COALESCE(MIN(sort_index), 1) FROM projects", [], |r| r.get(0)
        ).map_err(|e| format!("查询项目排序失败: {}", e))?;
        conn.execute(
            "INSERT INTO projects (id, name, path, pinned, sort_index) VALUES (?1,?2,?3,0,?4)",
            rusqlite::params![id, name.trim(), path, min_sort - 1],
        ).map_err(|e| format!("创建项目失败: {}", e))?;
        Ok(Project {
            id, name: name.trim().to_string(), path: path.to_string(),
            pinned: false, sort_index: min_sort - 1,
        })
    })
}

/// 更新项目
#[tauri::command]
pub fn update_project(state: State<AppState>, id: String, name: String, path: String) -> Result<(), String> {
    if id.trim().is_empty() { return Err("项目ID不能为空".into()); }
    if name.trim().is_empty() { return Err("项目名称不能为空".into()); }
    let path = path.trim().replace('\\', "/");
    if path.is_empty() { return Err("项目路径不能为空".into()); }
    if !std::path::Path::new(&path).is_dir() {
        return Err("项目路径不存在或不是目录".into());
    }
    // 项目路径会直接成为文件访问白名单的根：新路径必须是用户经原生选择器确认过的目录
    // （见 commands/paths.rs 的"配置路径收口"），驱动器根/系统目录等一律拒绝
    state.paths.ensure_config_dir_allowed(&state.db, &path, "项目路径")?;
    state.db.with_conn(|conn| {
        // 同名查重失败必须上报，不能静默按"无重名"继续写
        let exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM projects WHERE name=?1 AND id!=?2",
            rusqlite::params![name.trim(), id], |r| r.get(0)
        ).map_err(|e| format!("查询项目名是否重复失败: {}", e))?;
        if exists { return Err(format!("项目「{}」已存在", name.trim())); }

        let affected = conn.execute(
            "UPDATE projects SET name=?1, path=?2 WHERE id=?3",
            rusqlite::params![name.trim(), path, id],
        ).map_err(|e| format!("更新项目失败: {}", e))?;
        if affected == 0 { return Err("项目不存在".into()); }
        Ok(())
    })
}

/// 删除项目（先停进程，再删数据；进程清理含长等待 → 异步执行避免阻塞 IPC）
#[tauri::command]
pub async fn delete_project(app: tauri::AppHandle, id: String) -> Result<(), String> {
    if id.trim().is_empty() { return Err("项目ID不能为空".into()); }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        // 1. 查询服务 id（在 DB 锁外）
        let ids: Vec<String> = state.db.with_conn(|conn| {
            Ok(query_services_by_project(conn, &id)?
                .into_iter().map(|s| s.id).collect())
        })?;

        // 2. 停止所有运行中的服务进程。
        // 失败必须留痕：原先 `let _ = stop(sid)` 丢弃错误，用户看到"删除成功"而进程仍在跑
        // （占端口/CPU），且项目记录已删、UI 里再没有入口去停它。与 stop_project_services
        // （commands/process.rs 的"逐个停止并收集失败"）同一收口。
        // 仍继续删除：用户要的是删掉项目，不该因为一个停止失败而拒绝。残留进程由应用级
        // Job Object（KILL_ON_JOB_CLOSE）在 Nexus 退出时兜底回收。
        for sid in &ids {
            if let Err(e) = state.process_mgr.stop(sid) {
                log::warn!("删除项目 {} 时停止服务 {} 失败（进程可能残留至应用退出）：{}", id, sid, e);
            }
        }

        // 3. 停止文件监听（避免已删除项目的 watcher 线程泄漏并持续发送事件）
        let _ = state.file_watcher.stop_watching(&id);

        // 4. 删除数据库记录（级联删除服务）
        state.db.with_conn(|conn| {
            conn.execute("DELETE FROM projects WHERE id=?1", [&id]).map_err(|e| format!("删除项目失败: {}", e))?;
            Ok(())
        })
    }).await.map_err(|e| format!("删除项目任务失败: {}", e))?
}

/// 复制项目（含所有服务配置）
#[tauri::command]
pub fn duplicate_project(state: State<AppState>, id: String) -> Result<Project, String> {
    if id.trim().is_empty() { return Err("项目ID不能为空".into()); }
    write_project_duplicate(&state.db, &id)
}

/// `duplicate_project` 的 DB 部分（抽成自由函数以便测试：命令壳需要 `State<AppState>`，
/// 而这里真正要守门的是那份 14 列清单——`project.rs` 此前 `#[test]` 数为 0）
fn write_project_duplicate(db: &crate::database::Database, id: &str) -> Result<Project, String> {
    db.with_conn_mut(|conn| {
        // 事务包裹：项目 + 全部服务是原子写入——中途失败（磁盘满等）自动回滚，
        // 不会留下「项目已建、服务残缺」的半复制状态
        let tx = conn.transaction().map_err(|e| format!("开启事务失败: {}", e))?;
        let src = tx.query_row(
            "SELECT id, name, path, pinned, sort_index FROM projects WHERE id=?1",
            [&id],
            |row| Ok(Project {
                id: row.get("id")?, name: row.get("name")?, path: row.get("path")?,
                pinned: row.get::<_, i32>("pinned")? != 0,
                sort_index: row.get("sort_index")?,
            })
        ).map_err(|e| format!("项目不存在: {}", e))?;

        let services = query_services_by_project(&tx, id)?;

        let mut new_name = format!("{}_copy", src.name.trim());
        let mut n: u32 = 2;
        while n <= MAX_DUPLICATE_NAME_ATTEMPTS {
            let exists: bool = tx.query_row(
                "SELECT COUNT(*) > 0 FROM projects WHERE name=?1",
                [&new_name], |r| r.get(0)
            ).map_err(|e| format!("查询项目名是否重复失败: {}", e))?;
            if !exists { break; }
            new_name = format!("{}_copy{}", src.name.trim(), n);
            n += 1;
        }
        if n > MAX_DUPLICATE_NAME_ATTEMPTS {
            return Err("无法生成唯一项目名".into());
        }

        let new_id = uuid::Uuid::new_v4().to_string();
        // 副本同样插到最前：它是"刚建的项目"，理由见 `add_project` 的注释
        let min_sort: i32 = tx.query_row(
            "SELECT COALESCE(MIN(sort_index), 1) FROM projects", [], |r| r.get(0)
        ).map_err(|e| format!("查询项目排序失败: {}", e))?;
        tx.execute(
            "INSERT INTO projects (id, name, path, pinned, sort_index) VALUES (?1,?2,?3,0,?4)",
            rusqlite::params![new_id, new_name, src.path, min_sort - 1],
        ).map_err(|e| format!("创建复制项目失败: {}", e))?;

        for svc in &services {
            let svc_id = uuid::Uuid::new_v4().to_string();
            let en = if svc.enabled { 1 } else { 0 };
            let sft = if svc.show_file_tree { 1 } else { 0 };
            tx.execute(
                &format!(
                    "INSERT INTO services ({}) VALUES ({})",
                    crate::database::SERVICE_COLUMNS,
                    crate::database::named_placeholders(crate::database::SERVICE_COLUMNS),
                ),
                rusqlite::named_params! {
                    ":id": svc_id, ":project_id": new_id, ":name": svc.name,
                    ":command": svc.command, ":cwd": svc.cwd, ":watch_paths": svc.watch_paths,
                    ":watch_include": svc.watch_include, ":watch_exclude": svc.watch_exclude,
                    ":env_vars": svc.env_vars, ":restart_mode": svc.restart_mode,
                    ":enabled": en, ":show_file_tree": sft, ":sort_index": svc.sort_index,
                    ":tool_commands": svc.tool_commands,
                },
            ).map_err(|e| format!("复制服务配置失败: {}", e))?;
            // 打开工具绑定随服务一起复制：service_open_tools 以 service_id 为主键，
            // 用 INSERT...SELECT 直接换主键（原服务没有绑定时插入 0 行，无需先查）
            tx.execute(
                "INSERT OR REPLACE INTO service_open_tools (service_id, tool_id)
                 SELECT ?1, tool_id FROM service_open_tools WHERE service_id=?2",
                rusqlite::params![svc_id, svc.id],
            ).map_err(|e| format!("复制服务打开工具绑定失败: {}", e))?;
        }
        tx.commit().map_err(|e| format!("提交复制项目事务失败: {}", e))?;

        Ok(Project {
            id: new_id, name: new_name, path: src.path,
            pinned: false, sort_index: min_sort - 1,
        })
    })
}

/// 切换项目**收藏**状态（界面上叫「收藏」；字段名仍是 `pinned`，不动库结构）。
///
/// 取消收藏**不改变 sort_index**：它回到自己原来的位置。收藏/取消收藏是一个开关，
/// 开关带"改变顺序"的副作用最容易让人意外（而且只有取消时才发现）——顺序要变，
/// 应该是用户显式拖动，而不是点收藏的副作用。
#[tauri::command]
pub fn toggle_pin_project(state: State<AppState>, id: String) -> Result<bool, String> {
    if id.trim().is_empty() { return Err("项目ID不能为空".into()); }
    state.db.with_conn(|conn| {
        let current: bool = conn.query_row(
            "SELECT pinned FROM projects WHERE id=?1", [&id], |r| r.get::<_,i32>(0)
        ).map_err(|e| format!("项目不存在: {}", e))? != 0;
        let new_val = if current { 0 } else { 1 };
        conn.execute("UPDATE projects SET pinned=?1 WHERE id=?2", rusqlite::params![new_val, id])
            .map_err(|e| format!("更新项目收藏状态失败: {}", e))?;
        Ok(!current)
    })
}

/* ---- Tests ---- */

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::{init_schema, Database};
    use std::collections::BTreeMap;

    /// 某表在 schema 里的列名（**运行时枚举**，不写死）
    fn columns_of(conn: &rusqlite::Connection, table: &str) -> Vec<String> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table)).expect("读列信息");
        let names: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .expect("读列信息")
            .map(|r| r.expect("列信息行"))
            .collect();
        names
    }

    /// 取一行为「列名 → 值的文本形式」（列名来自 `SELECT *` 的运行时枚举）
    ///
    /// 为什么不逐字段写断言：那样测试自己就成了**又一份手工列清单**——给 `services`
    /// 加一列时它同样会漏，而"这一列没被复制"正是本测试要发现的东西。
    fn row_map(conn: &rusqlite::Connection, sql: &str, id: &str) -> BTreeMap<String, String> {
        let mut stmt = conn.prepare(sql).expect("prepare");
        let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let mut rows = stmt.query([id]).expect("query");
        let row = rows.next().expect("query 应成功").expect("应恰好有一行");
        cols.iter()
            .enumerate()
            .map(|(i, c)| {
                let s = match row.get_ref(i).expect("取值") {
                    rusqlite::types::ValueRef::Null => "<null>".to_string(),
                    rusqlite::types::ValueRef::Integer(n) => n.to_string(),
                    rusqlite::types::ValueRef::Real(f) => f.to_string(),
                    rusqlite::types::ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                    rusqlite::types::ValueRef::Blob(b) => format!("<blob {}B>", b.len()),
                };
                (c.clone(), s)
            })
            .collect()
    }

    /// 插入一个「**每一列都是非默认值**」的服务
    ///
    /// 列清单取自 `PRAGMA table_info`：给 `services` 加一列时本函数会 panic 提醒补值。
    /// 少了这道提醒，新列会在 fixture 里静默取 DEFAULT，于是"duplicate_project 没复制它"
    /// 也就跟着测不出来了——那正是这个测试存在的理由。
    fn insert_full_service(conn: &rusqlite::Connection, project_id: &str, id: &str) {
        // 刻意留空、由 DB 默认值填充的列：created_at 的语义是"这行是什么时候建的"，
        // 副本取复制时刻才是对的（见断言处的同名豁免）
        const ALLOW_DEFAULT: &[&str] = &["created_at"];
        let vals: BTreeMap<&str, String> = BTreeMap::from([
            ("id", id.to_string()),
            ("project_id", project_id.to_string()),
            ("name", "构建服务".to_string()),
            ("command", "npm run build".to_string()),
            ("cwd", "C:/p/sub".to_string()),
            ("watch_paths", r#"["C:/p/sub/src"]"#.to_string()),
            ("watch_include", "*.ts".to_string()),
            ("watch_exclude", "target".to_string()),
            ("env_vars", "NODE_ENV=production".to_string()),
            ("restart_mode", "2".to_string()),
            ("enabled", "0".to_string()),
            ("show_file_tree", "0".to_string()),
            ("sort_index", "7".to_string()),
            ("tool_commands", r#"[{"id":"t1","name":"build","command":"npm run build","timeout_secs":1800}]"#.to_string()),
        ]);

        let missing: Vec<String> = columns_of(conn, "services")
            .into_iter()
            .filter(|c| !vals.contains_key(c.as_str()) && !ALLOW_DEFAULT.contains(&c.as_str()))
            .collect();
        assert!(
            missing.is_empty(),
            "services 新增了列 {:?}：请在本 fixture 里给它一个非默认值，\
             否则「该列没被复制」在这条测试里测不出来",
            missing
        );

        let cols: Vec<String> = columns_of(conn, "services")
            .into_iter()
            .filter(|c| vals.contains_key(c.as_str()))
            .collect();
        let sql = format!(
            "INSERT INTO services ({}) VALUES ({})",
            cols.join(", "),
            (1..=cols.len()).map(|i| format!("?{}", i)).collect::<Vec<_>>().join(", ")
        );
        let params: Vec<&dyn rusqlite::ToSql> =
            cols.iter().map(|c| vals.get(c.as_str()).unwrap() as &dyn rusqlite::ToSql).collect();
        conn.execute(&sql, params.as_slice()).expect("插入服务");
    }

    /// 建库 + 一个项目 + 一个满配置服务 + 一条打开工具绑定
    fn seed() -> Database {
        let conn = rusqlite::Connection::open_in_memory().expect("内存库");
        init_schema(&conn).expect("建表");
        conn.execute(
            "INSERT INTO projects (id, name, path, pinned, sort_index) VALUES ('p1','P','C:/p',1,0)",
            [],
        ).expect("插入项目");
        insert_full_service(&conn, "p1", "s1");
        conn.execute(
            "INSERT INTO open_tools (id, name) VALUES ('tool1','T')", [],
        ).expect("插入打开工具");
        conn.execute(
            "INSERT INTO service_open_tools (service_id, tool_id) VALUES ('s1','tool1')", [],
        ).expect("插入绑定");
        Database::from_connection(conn)
    }

    /// ARCH-13①：`duplicate_project` 逐字段回归——「列清单没漏」的守门人。
    ///
    /// 背景：`services` 的列清单在仓库里有 4 份手工副本（3 处 INSERT/UPDATE + 这里的
    /// 复制）。给表加字段时漏掉 `duplicate_project` 那份**不会报错**：新列有 DEFAULT，
    /// 复制会成功写出一批"新字段全是默认值"的服务，配置悄悄与非复制版本不一致，
    /// 且没有任何测试会失败。
    #[test]
    fn test_duplicate_project_copies_every_service_column() {
        let db = seed();
        let copy = write_project_duplicate(&db, "p1").expect("复制应成功");

        assert_eq!(copy.name, "P_copy", "副本名应带 _copy 后缀");
        assert_eq!(copy.path, "C:/p", "路径原样保留");
        assert!(!copy.pinned, "副本不继承置顶（与 add_project 的语义一致）");

        db.with_conn(|conn| {
            let src = row_map(conn, "SELECT * FROM services WHERE project_id=?1", "p1");
            let dup = row_map(conn, "SELECT * FROM services WHERE project_id=?1", &copy.id);

            // 这三列**本来就该不同**：id/project_id 是新生成的；created_at 取复制时刻
            // （副本的"创建时间"就是现在，继承源时间反而错）。
            // 其余每一列都必须逐字一致——不一致即说明那列没进 duplicate_project 的列清单。
            let mut compared = 0;
            for (col, want) in &src {
                if matches!(col.as_str(), "id" | "project_id" | "created_at") { continue; }
                assert_eq!(
                    dup.get(col).map(String::as_str),
                    Some(want.as_str()),
                    "列「{}」没被复制（副本拿到了默认值）",
                    col
                );
                compared += 1;
            }
            assert!(compared >= 12, "比对到的列太少（{}），fixture 或 SQL 可能退化了", compared);

            // 打开工具绑定随服务一起复制
            let bound: String = conn
                .query_row(
                    "SELECT tool_id FROM service_open_tools WHERE service_id=(SELECT id FROM services WHERE project_id=?1)",
                    [&copy.id],
                    |r| r.get(0),
                )
                .expect("副本服务应保留打开工具绑定");
            assert_eq!(bound, "tool1");
            Ok(())
        })
        .unwrap();
    }

    /// 新项目要排在**最前**（`sort_index = MIN - 1`），而不是追加到末尾。
    ///
    /// 为什么是这个方向：项目列表**没有拖拽排序**，所以 sort_index 实际就是"添加顺序"，
    /// 而最新添加的正是刚要用那个——追加到末尾等于把它放在离用户最远的地方。
    /// 复制出来的项目同理（它也是"刚建的项目"）。
    #[test]
    fn test_new_project_goes_first_and_copy_too() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO projects (id, name, path, sort_index) VALUES
                ('p1','旧一','C:/a',0), ('p2','旧二','C:/b',1);",
        ).unwrap();
        let db = Database::from_connection(conn);

        // 走真实的插入路径（命令壳只多一层 IPC 编解码）
        let created = write_project_add(&db, "新项目", "C:/new").expect("新建项目");
        assert_eq!(created.sort_index, -1, "新项目应取 MIN-1");

        let ordered: Vec<String> = db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT name FROM projects ORDER BY pinned DESC, sort_index",
            ).unwrap();
            let v = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap()
                .map(|r| r.unwrap()).collect();
            Ok(v)
        }).unwrap();
        assert_eq!(ordered, vec!["新项目", "旧一", "旧二"], "新项目应排在最前");

        // 复制出来的项目也一样在最前
        let copy = write_project_duplicate(&db, "p1").expect("复制项目");
        assert_eq!(copy.sort_index, -2, "副本也应取 MIN-1");
    }
}
