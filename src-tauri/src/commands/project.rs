use tauri::State;
use crate::AppState;
use crate::models::{Project, ProjectDetail};
use crate::database::query_services_by_project;

/// 获取所有项目（不含服务详情，列表用）
#[tauri::command]
pub fn get_projects(state: State<AppState>) -> Result<Vec<Project>, String> {
    state.db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, name, path, pinned, sort_index, terminal_init_command FROM projects ORDER BY pinned DESC, sort_index"
        ).map_err(|e| format!("查询项目列表失败: {}", e))?;
        let rows = stmt.query_map([], |row| {
            Ok(Project {
                id: row.get(0)?, name: row.get(1)?, path: row.get(2)?,
                pinned: row.get::<_,i32>(3)? != 0,
                sort_index: row.get(4)?,
                terminal_init_command: row.get(5)?,
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
            "SELECT id, name, path, pinned, sort_index, terminal_init_command FROM projects WHERE id=?1",
            [&project_id],
            |row| Ok(Project {
                id: row.get(0)?, name: row.get(1)?, path: row.get(2)?,
                pinned: row.get::<_,i32>(3)? != 0,
                sort_index: row.get(4)?,
                terminal_init_command: row.get(5)?,
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
pub fn add_project(state: State<AppState>, name: String, path: String, terminal_init_command: String) -> Result<Project, String> {
    if name.trim().is_empty() { return Err("项目名称不能为空".into()); }
    let path = path.trim().replace('\\', "/");
    if path.is_empty() { return Err("项目路径不能为空".into()); }
    if !std::path::Path::new(&path).is_dir() {
        return Err("项目路径不存在或不是目录".into());
    }
    state.db.with_conn(|conn| {
        let exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM projects WHERE name=?1",
            [name.trim()], |r| r.get(0)
        ).unwrap_or(false);
        if exists { return Err(format!("项目「{}」已存在", name.trim())); }

        let id = uuid::Uuid::new_v4().to_string();
        let max_sort: i32 = conn.query_row(
            "SELECT COALESCE(MAX(sort_index), -1) FROM projects", [], |r| r.get(0)
        ).unwrap_or(-1);
        conn.execute(
            "INSERT INTO projects (id, name, path, pinned, sort_index, terminal_init_command) VALUES (?1,?2,?3,0,?4,?5)",
            rusqlite::params![id, name.trim(), path, max_sort + 1, terminal_init_command],
        ).map_err(|e| format!("创建项目失败: {}", e))?;
        Ok(Project {
            id, name: name.trim().to_string(), path: path.to_string(),
            pinned: false, sort_index: max_sort + 1,
            terminal_init_command,
        })
    })
}

/// 更新项目
#[tauri::command]
pub fn update_project(state: State<AppState>, id: String, name: String, path: String, terminal_init_command: String) -> Result<(), String> {
    if id.trim().is_empty() { return Err("项目ID不能为空".into()); }
    if name.trim().is_empty() { return Err("项目名称不能为空".into()); }
    let path = path.trim().replace('\\', "/");
    if path.is_empty() { return Err("项目路径不能为空".into()); }
    if !std::path::Path::new(&path).is_dir() {
        return Err("项目路径不存在或不是目录".into());
    }
    state.db.with_conn(|conn| {
        let exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM projects WHERE name=?1 AND id!=?2",
            rusqlite::params![name.trim(), id], |r| r.get(0)
        ).unwrap_or(false);
        if exists { return Err(format!("项目「{}」已存在", name.trim())); }

        let affected = conn.execute(
            "UPDATE projects SET name=?1, path=?2, terminal_init_command=?3 WHERE id=?4",
            rusqlite::params![name.trim(), path, terminal_init_command, id],
        ).map_err(|e| format!("更新项目失败: {}", e))?;
        if affected == 0 { return Err("项目不存在".into()); }
        Ok(())
    })
}

/// 删除项目（先停进程，再删数据）
#[tauri::command]
pub fn delete_project(state: State<AppState>, id: String) -> Result<(), String> {
    if id.trim().is_empty() { return Err("项目ID不能为空".into()); }
    // 1. 查询服务名称（在 DB 锁外）
    let names: Vec<String> = state.db.with_conn(|conn| {
        Ok(query_services_by_project(conn, &id)?
            .into_iter().map(|s| s.name).collect())
    })?;

    // 2. 停止所有运行中的服务进程
    for name in &names {
        let _ = state.process_mgr.stop(&format!("{}:{}", id, name));
    }

    // 3. 删除数据库记录（级联删除服务）
    state.db.with_conn(|conn| {
        conn.execute("DELETE FROM projects WHERE id=?1", [&id]).map_err(|e| format!("删除项目失败: {}", e))?;
        Ok(())
    })
}

/// 复制项目（含所有服务配置）
#[tauri::command]
pub fn duplicate_project(state: State<AppState>, id: String) -> Result<Project, String> {
    if id.trim().is_empty() { return Err("项目ID不能为空".into()); }
    state.db.with_conn(|conn| {
        let src = conn.query_row(
            "SELECT id, name, path, pinned, sort_index, terminal_init_command FROM projects WHERE id=?1",
            [&id],
            |row| Ok(Project {
                id: row.get(0)?, name: row.get(1)?, path: row.get(2)?,
                pinned: row.get::<_,i32>(3)? != 0,
                sort_index: row.get(4)?,
                terminal_init_command: row.get(5)?,
            })
        ).map_err(|e| format!("项目不存在: {}", e))?;

        let services = query_services_by_project(conn, &id)?;

        let mut new_name = format!("{}_copy", src.name.trim());
        let mut n: u32 = 2;
        while n <= MAX_DUPLICATE_NAME_ATTEMPTS {
            let exists: bool = conn.query_row(
                "SELECT COUNT(*) > 0 FROM projects WHERE name=?1",
                [&new_name], |r| r.get(0)
            ).unwrap_or(false);
            if !exists { break; }
            new_name = format!("{}_copy{}", src.name.trim(), n);
            n += 1;
        }
        if n > MAX_DUPLICATE_NAME_ATTEMPTS {
            return Err("无法生成唯一项目名".into());
        }

        let new_id = uuid::Uuid::new_v4().to_string();
        let max_sort: i32 = conn.query_row(
            "SELECT COALESCE(MAX(sort_index), -1) FROM projects", [], |r| r.get(0)
        ).unwrap_or(-1);
        conn.execute(
            "INSERT INTO projects (id, name, path, pinned, sort_index, terminal_init_command) VALUES (?1,?2,?3,0,?4,?5)",
            rusqlite::params![new_id, new_name, src.path, max_sort + 1, src.terminal_init_command],
        ).map_err(|e| format!("创建复制项目失败: {}", e))?;

        for svc in &services {
            let svc_id = uuid::Uuid::new_v4().to_string();
            let en = if svc.enabled { 1 } else { 0 };
            let sft = if svc.show_file_tree { 1 } else { 0 };
            conn.execute(
                "INSERT INTO services (id, project_id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, sort_index)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                rusqlite::params![svc_id, new_id, svc.name, svc.command, svc.cwd, svc.watch_paths, svc.watch_include, svc.watch_exclude, svc.env_vars, svc.restart_mode, en, sft, svc.sort_index],
            ).map_err(|e| format!("复制服务配置失败: {}", e))?;
        }

        Ok(Project {
            id: new_id, name: new_name, path: src.path,
            pinned: false, sort_index: max_sort + 1,
            terminal_init_command: src.terminal_init_command,
        })
    })
}

/// 切换项目置顶状态
#[tauri::command]
pub fn toggle_pin_project(state: State<AppState>, id: String) -> Result<bool, String> {
    if id.trim().is_empty() { return Err("项目ID不能为空".into()); }
    state.db.with_conn(|conn| {
        let current: bool = conn.query_row(
            "SELECT pinned FROM projects WHERE id=?1", [&id], |r| r.get::<_,i32>(0)
        ).map_err(|e| format!("项目不存在: {}", e))? != 0;
        let new_val = if current { 0 } else { 1 };
        conn.execute("UPDATE projects SET pinned=?1 WHERE id=?2", rusqlite::params![new_val, id])
            .map_err(|e| format!("更新项目置顶状态失败: {}", e))?;
        Ok(!current)
    })
}
