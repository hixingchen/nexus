use tauri::{AppHandle, Emitter, State};
use crate::AppState;
use crate::core::file_watcher::{FileChangeEvent, ServiceWatchConfig};

/// 记录「配置了但磁盘上不存在」的监听路径。
///
/// 核心层只在**全部**路径无效时告警；部分无效会被静默丢弃（该服务实际少监听了目录，
/// 表现为"改了某个目录下的文件没反应"），故在这一层把无效清单显式打出来。
fn warn_invalid_paths(service_name: &str, paths: &[String]) {
    let invalid: Vec<&str> = paths
        .iter()
        .map(|p| p.as_str())
        .filter(|p| !std::path::Path::new(p).exists())
        .collect();
    if !invalid.is_empty() {
        log::warn!(
            "[nexus] 服务「{}」有 {} 个监听路径不存在，将被忽略: {:?}",
            service_name, invalid.len(), invalid
        );
    }
}

/// 从数据库读取单个服务的监听配置
///
/// restart_mode=0（关闭监听）的服务不返回——与项目级加载（restart_mode>0）语义一致：
/// 任何入口都不得让"关闭监听"的服务实际监听文件。
/// 同时限定 project_id：服务 id 全局唯一，但监听是项目维度资源，
/// 跨项目绑定会让 stop/refresh 落在错误的项目监听上。
fn load_service_watch_config(db: &crate::database::Database, project_id: &str, service_id: &str) -> Result<Option<ServiceWatchConfig>, String> {
    db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, name, watch_paths, watch_include, watch_exclude, restart_mode FROM services WHERE id=?1 AND project_id=?2 AND restart_mode>0"
        ).map_err(|e| format!("查询服务失败: {}", e))?;
        let mut rows = stmt.query_map(rusqlite::params![service_id, project_id], |row| {
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
                warn_invalid_paths(&name, &paths);
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

/// 从数据库读取项目所有启用监听的服务
///
/// 条件 restart_mode>0 AND enabled=1：监听集合 = 项目启动实际会拉起的服务中开了
/// 监听模式的子集——不跟随项目启动（enabled=0）的服务若被手动启动，由单服务入口另行监听。
fn load_project_watch_configs(db: &crate::database::Database, project_id: &str) -> Result<Vec<ServiceWatchConfig>, String> {
    db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, name, watch_paths, watch_include, watch_exclude, restart_mode FROM services WHERE project_id=?1 AND restart_mode>0 AND enabled=1"
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
                warn_invalid_paths(&name, &paths);
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
        match load_service_watch_config(&state.db, &project_id, sid)? {
            Some(cfg) => vec![cfg],
            None => {
                // 返回 Ok 但没有任何路径进入监听：显式记录，避免"点了监听什么都没发生"
                log::warn!("[nexus] 服务 {} 未进入文件监听：不属于项目 {}、未开启监听模式或未配置监听路径", sid, project_id);
                return Ok(());
            }
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

/// 从项目监听中移除单服务；无剩余服务时停止整个项目监听。
/// 重建时按剩余服务的当前配置重建 notify watcher（否则旧路径仍被监听产生孤儿事件）。
/// 停止按钮（stop_watching）与删除服务（delete_service）共用。
pub(crate) fn remove_service_watch(
    app: AppHandle,
    state: &AppState,
    project_id: &str,
    service_id: &str,
) -> Result<(), String> {
    let remaining = state.file_watcher.remove_service_from_watching(project_id, service_id);
    if remaining.is_empty() {
        return state.file_watcher.stop_watching(project_id);
    }
    let project_name: String = state.db.with_conn(|conn| {
        conn.query_row("SELECT name FROM projects WHERE id=?1", [project_id],
            |row| row.get(0)
        ).map_err(|e| format!("项目不存在: {}", e))
    })?;
    state.file_watcher.start_watching(
        project_id,
        &project_name,
        &remaining,
        move |event: FileChangeEvent| {
            let _ = app.emit("file-changed", event);
        },
    )
}

/// 服务配置保存后刷新其监听（配置热更新）
///
/// 仅当该服务当前正在被监听时生效：用数据库新配置替换旧条目并重建 watcher；
/// 新配置 restart_mode=0 或无监听路径 → 从监听中摘除该服务。
/// 服务不在监听中 → no-op（下次启动时自然读新配置）。
pub(crate) fn refresh_service_watch(
    app: AppHandle,
    state: &AppState,
    project_id: &str,
    service_id: &str,
) -> Result<(), String> {
    let Some(existing) = state.file_watcher.get_watched_services(project_id) else {
        return Ok(());
    };
    if !existing.iter().any(|s| s.id == service_id) {
        return Ok(());
    }
    let mut updated = existing;
    updated.retain(|s| s.id != service_id);
    if let Some(cfg) = load_service_watch_config(&state.db, project_id, service_id)? {
        updated.push(cfg);
    }
    if updated.is_empty() {
        return state.file_watcher.stop_watching(project_id);
    }
    let project_name: String = state.db.with_conn(|conn| {
        conn.query_row("SELECT name FROM projects WHERE id=?1", [project_id],
            |row| row.get(0)
        ).map_err(|e| format!("项目不存在: {}", e))
    })?;
    state.file_watcher.start_watching(
        project_id,
        &project_name,
        &updated,
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

    match service_id {
        None => state.file_watcher.stop_watching(&project_id),
        Some(sid) => remove_service_watch(app, &state, &project_id, &sid),
    }
}
