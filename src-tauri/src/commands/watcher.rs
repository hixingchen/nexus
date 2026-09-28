use tauri::{AppHandle, Emitter, Manager};
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

/// 项目显示名（监听线程把它填进事件里，前端据此显示"XX 有文件变更"）
///
/// 抽成一处（CQ-20）：同一条 `SELECT name FROM projects` 原先是 3 份拷贝。
fn project_name(db: &crate::database::Database, project_id: &str) -> Result<String, String> {
    db.with_conn(|conn| {
        conn.query_row("SELECT name FROM projects WHERE id=?1", [project_id],
            |row| row.get("name")
        ).map_err(|e| format!("项目不存在: {}", e))
    })
}

/// 把文件变更事件转发给前端。
///
/// 为什么收成一处（CQ-20）：三条调用路径（启动监听 / 移除单服务 / 刷新服务配置）原先
/// 各写一份 `let _ = app.emit(...)`，**全部丢弃发送结果**——前端若尚未注册监听（或已卸载），
/// 事件静默消失，用户看到的是"改了文件却没有重启提示"，日志里一条线索都没有。
/// 这三条路径恰恰就是那个现场。
fn emit_file_changed(app: &AppHandle, event: FileChangeEvent) {
    if let Err(e) = app.emit("file-changed", event) {
        log::warn!("[nexus] 文件变更事件发送失败（前端可能尚未注册监听）: {}", e);
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
                row.get::<_, String>("id")?,
                row.get::<_, String>("name")?,
                row.get::<_, String>("watch_paths")?,
                row.get::<_, String>("watch_include")?,
                row.get::<_, String>("watch_exclude")?,
                row.get::<_, i32>("restart_mode")?,
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
                Ok(Some(ServiceWatchConfig::new(
                    id, name, paths,
                    include_str.lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                    exclude_str.lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                    restart_mode,
                )))
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
                row.get::<_, String>("id")?,
                row.get::<_, String>("name")?,
                row.get::<_, String>("watch_paths")?,
                row.get::<_, String>("watch_include")?,
                row.get::<_, String>("watch_exclude")?,
                row.get::<_, i32>("restart_mode")?,
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
                svcs.push(ServiceWatchConfig::new(
                    id, name, paths,
                    include_str.lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                    exclude_str.lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                    restart_mode,
                ));
            }
        }
        Ok(svcs)
    })
}

/// 启动文件监听
///
/// - `service_id` 为空：启动项目级监听（所有 restart_mode>0 的服务）
/// - `service_id` 非空：追加该服务到监听（与已有监听合并）
///
/// 异步 + spawn_blocking：内部 `start_watching` 会先 `stop_watching`（**join 监听线程**，
/// 最长约 200ms）再对每个路径调 `notify::watch`（Windows 递归监听要枚举目录树）。
/// 同步执行时这段工作内联在 IPC 请求路径上。
#[tauri::command]
pub async fn start_watching(app: AppHandle, project_id: String, service_id: Option<String>) -> Result<(), String> {
    if project_id.trim().is_empty() { return Err("项目ID不能为空".into()); }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let project_name = project_name(&state.db, &project_id)?;

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

        // 事件回调需要拥有 AppHandle：`state` 借用了 app，不能把 app 本身移进闭包
        let emit_app = app.clone();
        state.file_watcher.start_watching(
            &project_id,
            &project_name,
            &merged,
            move |event: FileChangeEvent| emit_file_changed(&emit_app, event),
        )
    }).await.map_err(|e| format!("启动文件监听任务失败: {}", e))?
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
    // 只读地算出剩余列表，**不要**先改 map 里的 services（NEW-18）：
    // 下面的 start_watching 可能失败（剩余服务的监听路径全失效 = 磁盘上目录已不存在），
    // 而失败时运行中的线程仍持有旧的 svc_map。若先把 map 改掉，就会出现
    // "map 说 [T]、线程实际监听 [S,T]" 的永久分叉——改 S 的目录照样 emit，
    // 弹出"需要重启 S"的卡片，而 S 已被删除 → 点重启必然报"服务不存在"。
    // 提交点统一在 start_watching 末尾，失败时 map 保持原样（与旧线程一致）。
    let Some(existing) = state.file_watcher.get_watched_services(project_id) else {
        // 该项目本就没有在监听：无需重建，也无需 stop（幂等）
        return Ok(());
    };
    let remaining: Vec<ServiceWatchConfig> = existing.into_iter().filter(|s| s.id != service_id).collect();
    if remaining.is_empty() {
        return state.file_watcher.stop_watching(project_id);
    }
    let project_name = project_name(&state.db, project_id)?;
    state.file_watcher.start_watching(
        project_id,
        &project_name,
        &remaining,
        move |event: FileChangeEvent| emit_file_changed(&app, event),
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
    let project_name = project_name(&state.db, project_id)?;
    state.file_watcher.start_watching(
        project_id,
        &project_name,
        &updated,
        move |event: FileChangeEvent| emit_file_changed(&app, event),
    )
}

/// 停止文件监听
///
/// - `service_id` 为空：停止整个项目的监听
/// - `service_id` 非空：仅移除该服务，剩余服务继续监听
///
/// 异步 + spawn_blocking：`stop_watching` 要 join 监听线程，`remove_service_watch`
/// 还可能重建 notify watcher（同上，Windows 下是目录树枚举 + 系统调用）。
#[tauri::command]
pub async fn stop_watching(app: AppHandle, project_id: String, service_id: Option<String>) -> Result<(), String> {
    if project_id.trim().is_empty() { return Err("项目ID不能为空".into()); }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        match service_id {
            None => state.file_watcher.stop_watching(&project_id),
            // 同理：state 借用了 app，这里传一份克隆
            Some(sid) => remove_service_watch(app.clone(), &state, &project_id, &sid),
        }
    }).await.map_err(|e| format!("停止文件监听任务失败: {}", e))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::{init_schema, Database};

    /// 内存库 + 两个项目（ARCH-32：`load_service_watch_config` 的 4 条业务规则此前零覆盖）
    fn test_db() -> Database {
        let conn = rusqlite::Connection::open_in_memory().expect("内存库");
        init_schema(&conn).expect("建表");
        conn.execute(
            "INSERT INTO projects (id, name, path, sort_index, created_at) VALUES ('p1','甲项目','C:/p1',0,'t')",
            [],
        ).expect("插入 p1");
        conn.execute(
            "INSERT INTO projects (id, name, path, sort_index, created_at) VALUES ('p2','乙项目','C:/p2',1,'t')",
            [],
        ).expect("插入 p2");
        Database::from_connection(conn)
    }

    fn insert_service(db: &Database, id: &str, project: &str, name: &str, watch_paths: &str, restart_mode: i32) {
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO services (id, project_id, name, command, cwd, watch_paths, restart_mode)                  VALUES (?1, ?2, ?3, 'npm run dev', 'C:/s', ?4, ?5)",
                rusqlite::params![id, project, name, watch_paths, restart_mode],
            ).map_err(|e| e.to_string())
        }).expect("插入服务");
    }

    #[test]
    fn test_watch_config_skips_restart_mode_zero() {
        let db = test_db();
        // restart_mode=0 = 关闭监听：任何入口都不得让它实际监听文件
        insert_service(&db, "s-off", "p1", "关监听", r#"["./src"]"#, 0);
        let got = load_service_watch_config(&db, "p1", "s-off").expect("查询应成功");
        assert!(got.is_none(), "restart_mode=0 的服务不该返回监听配置");
    }

    #[test]
    fn test_watch_config_is_scoped_to_project() {
        let db = test_db();
        insert_service(&db, "s1", "p1", "甲服务", r#"["./src"]"#, 2);
        let ok = load_service_watch_config(&db, "p1", "s1").expect("查询应成功");
        assert!(ok.is_some(), "本项目内应能读到");
        let crossed = load_service_watch_config(&db, "p2", "s1").expect("查询应成功");
        assert!(crossed.is_none(), "跨项目绑定必须落空（id 全局唯一，但监听是项目维度资源）");
    }

    #[test]
    fn test_watch_config_reports_malformed_paths_json() {
        let db = test_db();
        insert_service(&db, "s-bad", "p1", "坏配置", "{不是数组}", 1);
        let err = load_service_watch_config(&db, "p1", "s-bad").expect_err("JSON 非法必须报错");
        assert!(err.contains("坏配置"), "错误里要点出是哪个服务的配置有问题: {}", err);
        assert!(err.contains("监听路径"), "要说清坏在哪一项: {}", err);
    }

    #[test]
    fn test_watch_config_empty_paths_means_no_watching() {
        let db = test_db();
        insert_service(&db, "s-empty", "p1", "空路径", "[]", 2);
        assert!(load_service_watch_config(&db, "p1", "s-empty").expect("查询应成功").is_none(),
            "没配监听路径 = 不监听（而不是监听整个工作目录）");
        insert_service(&db, "s-blank", "p1", "空串", "   ", 2);
        assert!(load_service_watch_config(&db, "p1", "s-blank").expect("查询应成功").is_none(),
            "空白串按没配处理");
    }

    #[test]
    fn test_watch_config_parses_paths_and_rules() {
        let db = test_db();
        insert_service(&db, "s1", "p1", "甲服务", r#"["C:/s/src", "C:/s/web"]"#, 2);
        let got = load_service_watch_config(&db, "p1", "s1").expect("查询应成功").expect("应有配置");
        assert_eq!(got.paths, vec!["C:/s/src".to_string(), "C:/s/web".to_string()]);
        assert_eq!(got.restart_mode, 2);
        assert!(!got.include.is_empty(), "include 未配时应回落到建表默认值 '*'");
    }

    #[test]
    fn test_project_name_reports_missing_project() {
        let db = test_db();
        assert_eq!(project_name(&db, "p1").expect("应能读到"), "甲项目");
        let err = project_name(&db, "nope").expect_err("项目不存在必须报错");
        assert!(err.contains("项目不存在"), "错误文案要说清原因: {}", err);
    }
}
