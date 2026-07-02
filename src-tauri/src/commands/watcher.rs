use tauri::{AppHandle, Emitter, State};
use crate::AppState;
use crate::core::file_watcher::{FileChangeEvent, ServiceWatchConfig};

/// 启动项目文件监听
#[tauri::command]
pub fn start_watching(app: AppHandle, state: State<AppState>, project_id: String) -> Result<(), String> {
    if project_id.trim().is_empty() { return Err("项目ID不能为空".into()); }
    let project_name: String = state.db.with_conn(|conn| {
        conn.query_row("SELECT name FROM projects WHERE id=?1", [&project_id],
            |row| row.get(0)
        ).map_err(|e| format!("项目不存在: {}", e))
    })?;

    let services = state.db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, name, watch_paths, watch_include, watch_exclude FROM services WHERE project_id=?1 AND restart_mode>0"
        ).map_err(|e| format!("查询文件监听服务列表失败: {}", e))?;
        let rows = stmt.query_map([&project_id], |row| {
            Ok((
                row.get::<_,String>(0)?,  // id
                row.get::<_,String>(1)?,  // name
                row.get::<_,String>(2)?,  // watch_paths
                row.get::<_,String>(3)?,  // watch_include
                row.get::<_,String>(4)?,  // watch_exclude
            ))
        }).map_err(|e| format!("读取文件监听服务数据失败: {}", e))?;
        let mut svcs: Vec<ServiceWatchConfig> = Vec::new();
        for r in rows {
            let (id, name, wp_json, include_str, exclude_str) = r.map_err(|e| format!("解析文件监听服务数据失败: {}", e))?;
            let paths: Vec<String> = serde_json::from_str(&wp_json).unwrap_or_default();
            if !paths.is_empty() {
                svcs.push(ServiceWatchConfig {
                    id,
                    name,
                    paths,
                    include: include_str.lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                    exclude: exclude_str.lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                });
            }
        }
        Ok(svcs)
    })?;

    state.file_watcher.start_watching(
        &project_id,
        &project_name,
        &services,
        move |event: FileChangeEvent| {
            let _ = app.emit("file-changed", event);
        },
    )
}

/// 停止项目文件监听
#[tauri::command]
pub fn stop_watching(state: State<AppState>, project_id: String) -> Result<(), String> {
    if project_id.trim().is_empty() { return Err("项目ID不能为空".into()); }
    state.file_watcher.stop_watching(&project_id)
}
