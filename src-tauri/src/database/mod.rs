use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::Mutex;
use crate::models::Service;

/// 数据库放在用户目录 ~/.nexus/ 下，避免 Tauri dev watcher 检测到项目目录中的文件变化导致无限重启
fn db_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let nexus_dir = home.join(".nexus");
    std::fs::create_dir_all(&nexus_dir).ok();
    nexus_dir.join("nexus.db")
}

pub struct Database {
    pub conn: Mutex<Connection>,
}

impl Database {
    /// 创建数据库实例，失败时返回错误而非 panic
    pub fn try_new() -> Result<Self, String> {
        let conn = Connection::open(db_path()).map_err(|e| format!("无法打开数据库: {}", e))?;
        init_schema(&conn)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T, String>) -> Result<T, String> {
        let conn = self.conn.lock().map_err(|e| format!("数据库连接锁获取失败: {}", e))?;
        f(&conn)
    }
}

/// 当前 schema 版本号，每次改表结构时递增。
/// 版本不匹配时自动重建数据库（开发阶段策略，生产环境应做迁移）。
const SCHEMA_VERSION: i32 = 8;

fn init_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;")
        .map_err(|e| format!("初始化数据库 PRAGMA 失败: {}", e))?;

    // 检查 schema 版本，不匹配则重建
    let current: i32 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='schema_version'",
        [], |r| r.get(0)
    ).unwrap_or(0);

    let version: i32 = if current > 0 {
        conn.query_row("SELECT version FROM schema_version", [], |r| r.get(0)).unwrap_or(0)
    } else {
        0
    };

    if version < SCHEMA_VERSION {
        if version > 0 {
            log::warn!("schema 版本不兼容 (db={}, code={}), 重建数据库", version, SCHEMA_VERSION);
        }
        // 删旧表重建
        conn.execute_batch("
            DROP TABLE IF EXISTS services;
            DROP TABLE IF EXISTS projects;
            DROP TABLE IF EXISTS project_services;
            DROP TABLE IF EXISTS schema_version;
        ").map_err(|e| format!("重建数据库表失败: {}", e))?;
    }

    conn.execute_batch("
        CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS layout (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS projects (
            id TEXT PRIMARY KEY, name TEXT NOT NULL,
            path TEXT NOT NULL DEFAULT '',
            pinned INTEGER NOT NULL DEFAULT 0,
            sort_index INTEGER NOT NULL DEFAULT 0,
            terminal_init_command TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE TABLE IF NOT EXISTS services (
            id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            name TEXT NOT NULL, command TEXT NOT NULL DEFAULT '',
            cwd TEXT NOT NULL DEFAULT '',
            watch_paths TEXT NOT NULL DEFAULT '[]',
            watch_include TEXT NOT NULL DEFAULT '*',
            watch_exclude TEXT NOT NULL DEFAULT 'node_modules\n.git\ndist\ntarget\n__pycache__\n.next\nbuild\ncoverage\n*.log',
            env_vars TEXT NOT NULL DEFAULT '{}',
            restart_mode INTEGER NOT NULL DEFAULT 0,
            enabled INTEGER NOT NULL DEFAULT 1,
            show_file_tree INTEGER NOT NULL DEFAULT 1,
            sort_index INTEGER NOT NULL DEFAULT 0,
            tool_commands TEXT NOT NULL DEFAULT '[]',
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
    ").map_err(|e| format!("创建数据库表失败: {}", e))?;

    // 使用参数化查询写入 schema 版本
    conn.execute(
        "INSERT OR REPLACE INTO schema_version (rowid, version) VALUES (1, ?1)",
        [SCHEMA_VERSION],
    ).map_err(|e| format!("写入 schema 版本失败: {}", e))?;

    Ok(())
}

/// 查询指定项目下的所有服务（共享函数，消除重复 SQL）
pub fn query_services_by_project(conn: &Connection, project_id: &str) -> Result<Vec<Service>, String> {
    let mut stmt = conn.prepare(
        "SELECT id, project_id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, sort_index, tool_commands
         FROM services WHERE project_id=?1 ORDER BY sort_index"
    ).map_err(|e| format!("查询服务列表失败: {}", e))?;
    let rows = stmt.query_map([project_id], |row| {
        Ok(Service {
            id: row.get(0)?, project_id: row.get(1)?, name: row.get(2)?,
            command: row.get(3)?, cwd: row.get(4)?, watch_paths: row.get(5)?,
            watch_include: row.get(6)?, watch_exclude: row.get(7)?,
            env_vars: row.get(8)?, restart_mode: row.get(9)?,
            enabled: row.get::<_,i32>(10)? != 0,
            show_file_tree: row.get::<_,i32>(11)? != 0,
            sort_index: row.get(12)?,
            tool_commands: row.get(13)?,
        })
    }).map_err(|e| format!("读取服务数据失败: {}", e))?;
    let mut services = Vec::new();
    for r in rows { services.push(r.map_err(|e| format!("解析服务数据失败: {}", e))?); }
    Ok(services)
}
