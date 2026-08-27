use tauri::{AppHandle, Emitter, State};
use crate::AppState;
use crate::core::file_watcher::{FileChangeEvent, ServiceWatchConfig};

/// 从数据库读取单个服务的监听配置
fn load_service_watch_config(db: &crate::database::Database, service_id: &str) -> Result<Option<ServiceWatchConfig>, String> {
    db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, name, watch_paths, watch_include, watch_exclude, restart_mode FROM services WHERE id=?1"
        ).map_err(|e| format!("查询服务失败: {}", e))?;
        let mut rows = stmt.query_map([service_id], |row| {
            Ok((
                row.get::<_,String>(0)?,
                row.get::<_,String>(1)?,
                row.get::<_,String>(2)?,
                row.get::<_,String>(3)?,
                row.get::<_,String>(4)?,
                row.get::<_,i32>(5)?,
            ))
        }).map_err(|e| format!("读取服务数据失败: {}", e))?;
        match rows.next() {
            Some(r) => {
                let (id, name, wp_json, include_str, exclude_str, restart_mode) = r.map_err(|e| format!("解析服务数据失败: {}", e))?;
                let paths: Vec<String> = if wp_json.trim().is_empty() {
                    Vec::new()
                } else {
                    serde_json::from_str(&wp_json)
                        .map_err(|e| format!("服务「{}」的监听路径配置格式错误: {}", name, e))?
                };
                if paths.is_empty() { return Ok(None); }
                Ok(Some(ServiceWatchConfig {
                    id, name, paths,
                    include: include_str.lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                    exclude: exclude_str.lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                    restart_mode,
                }))
            }
            None => Ok(None),
        }
    })
}

/// 从数据库读取项目所有启用监听的服务（restart_mode>0）
fn load_project_watch_configs(db: &crate::database::Database, project_id: &str) -> Result<Vec<ServiceWatchConfig>, String> {
    db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, name, watch_paths, watch_include, watch_exclude, restart_mode FROM services WHERE project_id=?1 AND restart_mode>0"
        ).map_err(|e| format!("查询文件监听服务列表失败: {}", e))?;
        let rows = stmt.query_map([project_id], |row| {
            Ok((
                row.get::<_,String>(0)?,
                row.get::<_,String>(1)?,
                row.get::<_,String>(2)?,
                row.get::<_,String>(3)?,
                row.get::<_,String>(4)?,
                row.get::<_,i32>(5)?,
            ))
        }).map_err(|e| format!("读取文件监听服务数据失败: {}", e))?;
        let mut svcs = Vec::new();
        for r in rows {
            let (id, name, wp_json, include_str, exclude_str, restart_mode) = r.map_err(|e| format!("解析文件监听服务数据失败: {}", e))?;
            let paths: Vec<String> = if wp_json.trim().is_empty() {
                Vec::new()
            } else {
                serde_json::from_str(&wp_json)
                    .map_err(|e| format!("服务「{}」的监听路径配置格式错误: {}", name, e))?
            };
            if !paths.is_empty() {
                svcs.push(ServiceWatchConfig {
                    id, name, paths,
                    include: include_str.lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                    exclude: exclude_str.lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                    restart_mode,
                });
            }
        }
        Ok(svcs)
    })
}

/// 启动文件监听
///
/// - `service_id` 为空：启动项目级监听（所有 restart_mode>0 的服务）
/// - `service_id` 非空：追加该服务到监听（与已有监听合并）
#[tauri::command]
pub fn start_watching(app: AppHandle, state: State<AppState>, project_id: String, service_id: Option<String>) -> Result<(), String> {
    if project_id.trim().is_empty() { return Err("项目ID不能为空".into()); }
    let project_name: String = state.db.with_conn(|conn| {
        conn.query_row("SELECT name FROM projects WHERE id=?1", [&project_id],
            |row| row.get(0)
        ).map_err(|e| format!("项目不存在: {}", e))
    })?;

    let new_services = if let Some(sid) = service_id.as_deref() {
        // 单服务模式：只启动该服务的监听
        match load_service_watch_config(&state.db, sid)? {
            Some(cfg) => vec![cfg],
            None => return Ok(()), // 服务不存在或无监听路径，忽略
        }
    } else {
        // 项目模式：启动所有启用监听的服务
        load_project_watch_configs(&state.db, &project_id)?
    };

    // 合并已有监听：取现有服务 + 新服务，去重（以 service id 为准）
    let mut merged: Vec<ServiceWatchConfig> = new_services;
    if service_id.is_some() {
        // 追加模式：保留已有服务中不在新增列表里的
        if let Some(existing) = state.file_watcher.get_watched_services(&project_id) {
            let new_ids: std::collections::HashSet<String> = merged.iter().map(|s| s.id.clone()).collect();
            for svc in existing {
                if !new_ids.contains(&svc.id) {
                    merged.push(svc);
                }
            }
        }
    }

    state.file_watcher.start_watching(
        &project_id,
        &project_name,
        &merged,
        move |event: FileChangeEvent| {
            let _ = app.emit("file-changed", event);
        },
    )
}

/// 停止文件监听
///
/// - `service_id` 为空：停止整个项目的监听
/// - `service_id` 非空：仅移除该服务，剩余服务继续监听
#[tauri::command]
pub fn stop_watching(app: AppHandle, state: State<AppState>, project_id: String, service_id: Option<String>) -> Result<(), String> {
    if project_id.trim().is_empty() { return Err("项目ID不能为空".into()); }

    if service_id.is_none() {
        // 项目模式：停止整个项目监听
        return state.file_watcher.stop_watching(&project_id);
    }

    let sid = service_id.unwrap();
    // 单服务模式：从监听中移除该服务
    let remaining = state.file_watcher.remove_service_from_watching(&project_id, &sid);
    if remaining.is_empty() {
        // 没有剩余服务，停止整个项目监听
        state.file_watcher.stop_watching(&project_id)
    } else {
        // 重建监听（含剩余服务）
        let project_name: String = state.db.with_conn(|conn| {
            conn.query_row("SELECT name FROM projects WHERE id=?1", [&project_id],
                |row| row.get(0)
            ).map_err(|e| format!("项目不存在: {}", e))
        })?;
        state.file_watcher.start_watching(
            &project_id,
            &project_name,
            &remaining,
            move |event: FileChangeEvent| {
                let _ = app.emit("file-changed", event);
            },
        )
    }
}
