use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::Mutex;
use crate::models::Service;

/// 数据库放在用户目录 ~/.nexus/ 下，避免 Tauri dev watcher 检测到项目目录中的文件变化导致无限重启
fn db_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let nexus_dir = home.join(".nexus");
    // 目录建不出来（无写权限/磁盘只读）时打开数据库必然失败，
    // 这里直接上报真实原因，而不是让后续 open 报一句笼统的 "unable to open database file"
    std::fs::create_dir_all(&nexus_dir)
        .map_err(|e| format!("无法创建数据目录 {}: {}", nexus_dir.display(), e))?;
    Ok(nexus_dir.join("nexus.db"))
}

pub struct Database {
    pub conn: Mutex<Connection>,
}

impl Database {
    /// 创建数据库实例，失败时返回错误而非 panic
    pub fn try_new() -> Result<Self, String> {
        let conn = Connection::open(db_path()?).map_err(|e| format!("无法打开数据库: {}", e))?;
        init_schema(&conn)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T, String>) -> Result<T, String> {
        // 锁中毒（持锁线程 panic）继续使用内部连接：与 core/process.rs::stop_all 同策略。
        // 此前直接返回错误 → 一次 panic 会让整个数据库功能永久不可用
        let conn = self.conn.lock().unwrap_or_else(|e| {
            log::error!("数据库连接锁已中毒，继续使用: {}", e);
            e.into_inner()
        });
        f(&conn)
    }

    /// 可变连接（事务等需要 &mut 的场景）
    pub fn with_conn_mut<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T, String>) -> Result<T, String> {
        let mut conn = self.conn.lock().unwrap_or_else(|e| {
            log::error!("数据库连接锁已中毒，继续使用: {}", e);
            e.into_inner()
        });
        f(&mut conn)
    }
}

/// 当前 schema 版本号（仅记录用途，**不是迁移开关**）。
///
/// 历史教训：版本号不匹配曾触发删表重建——用户升级即清空全部项目/服务配置。
/// 表结构变更一律走下方 MIGRATION_COLUMNS 增量 ALTER（幂等加列），
/// 任何旧版本数据库都能原地升级，禁止再引入 DROP 任何用户表。
const SCHEMA_VERSION: i32 = 9;

fn init_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;")
        .map_err(|e| format!("初始化数据库 PRAGMA 失败: {}", e))?;

    // 读取旧版本号（仅日志用；老库由 CREATE IF NOT EXISTS + 增量加列原地升级）
    let current: i32 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='schema_version'",
        [], |r| r.get(0)
    ).unwrap_or(0);

    let version: i32 = if current > 0 {
        conn.query_row("SELECT version FROM schema_version", [], |r| r.get(0)).unwrap_or(0)
    } else {
        0
    };

    if version > 0 && version != SCHEMA_VERSION {
        log::warn!("schema 版本不兼容 (db={}, code={})，原地增量迁移，不删除任何数据", version, SCHEMA_VERSION);
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
        -- 服务模板库：跨项目复用的服务配置（独立于项目，不随项目删除）
        CREATE TABLE IF NOT EXISTS service_templates (
            id TEXT PRIMARY KEY,
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
            -- 模板携带的默认打开工具（从模板添加服务时复制为服务绑定；工具被删时由命令清空）
            open_tool_id TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        -- 外部打开工具库（服务右键「用 XX 打开」）：executable 为可执行程序路径，
        -- args 为参数模板（{path} 参数化注入，不经过 shell——避免引号/元字符地狱）。
        -- command 为旧版整串命令（已废弃，仅兼容历史行：executable 为空时按旧格式执行）
        CREATE TABLE IF NOT EXISTS open_tools (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            command TEXT NOT NULL DEFAULT '',
            executable TEXT NOT NULL DEFAULT '',
            args TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        -- 服务 ↔ 打开工具绑定：删除任一侧记录时级联清理
        CREATE TABLE IF NOT EXISTS service_open_tools (
            service_id TEXT PRIMARY KEY REFERENCES services(id) ON DELETE CASCADE,
            tool_id TEXT NOT NULL REFERENCES open_tools(id) ON DELETE CASCADE
        );
        -- 服务按项目过滤是主查询路径（列表/运行状态/启动全部），缺索引时全表扫描
        CREATE INDEX IF NOT EXISTS idx_services_project_id ON services(project_id);
        -- 反向查询（某工具被哪些服务绑定）：删除工具前查引用、绑定列表展示都走此列
        CREATE INDEX IF NOT EXISTS idx_service_open_tools_tool_id ON service_open_tools(tool_id);
    ").map_err(|e| format!("创建数据库表失败: {}", e))?;

    // 项目名唯一索引兜底（add/update 已做代码层查重；此处防未来新写入路径漏查）。
    // 历史数据若已有重名项目则建索引失败——降级为警告，绝不阻塞启动
    if let Err(e) = conn.execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS idx_projects_name ON projects(name)") {
        log::warn!("[nexus] 创建项目名唯一索引失败（可能已有重名项目，建议手动清理）: {}", e);
    }

    // 增量迁移：老库逐列补齐（幂等——列已存在直接跳过；新库 CREATE 已带全部列）。
    // 新增表结构字段时两条缺一不可：① 上方 CREATE TABLE 加列 ② 此处追加一行
    for (table, column, decl) in MIGRATION_COLUMNS {
        ensure_column(conn, table, column, decl)?;
    }

    // 使用参数化查询写入 schema 版本
    conn.execute(
        "INSERT OR REPLACE INTO schema_version (rowid, version) VALUES (1, ?1)",
        [SCHEMA_VERSION],
    ).map_err(|e| format!("写入 schema 版本失败: {}", e))?;

    Ok(())
}

/// 历史上各版本逐步新增的列。老库缺列时 ALTER TABLE 补上（含默认值，旧行无副作用），
/// 新库创建时已含、跳过。覆盖全部历史版本 → 任意旧库首次打开即原地升级为当前结构
const MIGRATION_COLUMNS: &[(&str, &str, &str)] = &[
    ("projects", "pinned", "INTEGER NOT NULL DEFAULT 0"),
    ("projects", "sort_index", "INTEGER NOT NULL DEFAULT 0"),
    ("services", "watch_paths", "TEXT NOT NULL DEFAULT '[]'"),
    ("services", "watch_include", "TEXT NOT NULL DEFAULT '*'"),
    ("services", "watch_exclude", "TEXT NOT NULL DEFAULT 'node_modules\n.git\ndist\ntarget\n__pycache__\n.next\nbuild\ncoverage\n*.log'"),
    ("services", "env_vars", "TEXT NOT NULL DEFAULT '{}'"),
    ("services", "restart_mode", "INTEGER NOT NULL DEFAULT 0"),
    ("services", "enabled", "INTEGER NOT NULL DEFAULT 1"),
    ("services", "show_file_tree", "INTEGER NOT NULL DEFAULT 1"),
    ("services", "sort_index", "INTEGER NOT NULL DEFAULT 0"),
    ("services", "tool_commands", "TEXT NOT NULL DEFAULT '[]'"),
    ("open_tools", "executable", "TEXT NOT NULL DEFAULT ''"),
    ("open_tools", "args", "TEXT NOT NULL DEFAULT ''"),
    ("service_templates", "open_tool_id", "TEXT NOT NULL DEFAULT ''"),
];

/// 幂等加列：列已存在则跳过（pragma_table_info 表值函数）
fn ensure_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<(), String> {
    let exists: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM pragma_table_info(?1) WHERE name=?2",
        rusqlite::params![table, column],
        |r| r.get(0),
    ).unwrap_or(false);
    if exists { return Ok(()); }
    conn.execute_batch(&format!("ALTER TABLE {} ADD COLUMN {} {}", table, column, decl))
        .map_err(|e| format!("迁移数据库（加列 {}.{}）失败: {}", table, column, e))?;
    log::info!("[nexus] 数据库迁移: {}.{} 已添加", table, column);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn in_memory() -> Connection {
        Connection::open_in_memory().expect("打开内存数据库失败")
    }

    /// 所有表 + 索引都在
    #[test]
    fn test_init_schema_creates_tables_and_indexes() {
        let conn = in_memory();
        init_schema(&conn).unwrap();
        let count: i32 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('schema_version','layout','projects','services','service_templates','open_tools','service_open_tools')",
            [], |r| r.get(0),
        ).unwrap();
        assert_eq!(count, 7);
        // services.project_id 索引（服务查询主路径）
        let idx: i32 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_services_project_id'",
            [], |r| r.get(0),
        ).unwrap();
        assert_eq!(idx, 1);
    }

    /// 老库（低版本、缺列）升级：数据保留、缺列补齐、版本号更新——绝不删表
    #[test]
    fn test_init_schema_upgrades_old_db_without_data_loss() {
        let conn = in_memory();
        // 构造历史老库（version=3）：projects/services 只有早期列，schema_version 表在
        conn.execute_batch(
            "CREATE TABLE schema_version (version INTEGER NOT NULL);
             CREATE TABLE projects (
                id TEXT PRIMARY KEY, name TEXT NOT NULL,
                path TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
             );
             CREATE TABLE services (
                id TEXT PRIMARY KEY, project_id TEXT NOT NULL,
                name TEXT NOT NULL, command TEXT NOT NULL DEFAULT '',
                cwd TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
             );
             INSERT INTO schema_version (rowid, version) VALUES (1, 3);
             INSERT INTO projects (id, name, path) VALUES ('p1', '老项目', 'C:/old');
             INSERT INTO services (id, project_id, name) VALUES ('s1', 'p1', '老服务');",
        ).unwrap();

        init_schema(&conn).unwrap();

        // 数据原样保留（升级不丢用户数据是硬约束）
        let (name, path): (String, String) = conn.query_row(
            "SELECT name, path FROM projects WHERE id='p1'", [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!(name, "老项目");
        assert_eq!(path, "C:/old");
        let svc: i32 = conn.query_row(
            "SELECT COUNT(*) FROM services WHERE project_id='p1'", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(svc, 1);
        // 缺列补齐（新代码的 INSERT/SELECT 依赖这些列）
        for col in ["pinned", "sort_index"] {
            let has: i32 = conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('projects') WHERE name=?1", [col], |r| r.get(0),
            ).unwrap();
            assert_eq!(has, 1, "projects.{col} 应被补齐");
        }
        for col in ["watch_paths", "env_vars", "restart_mode", "enabled", "show_file_tree", "sort_index", "tool_commands"] {
            let has: i32 = conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('services') WHERE name=?1", [col], |r| r.get(0),
            ).unwrap();
            assert_eq!(has, 1, "services.{col} 应被补齐");
        }
        // 版本号更新到当前
        let v: i32 = conn.query_row("SELECT version FROM schema_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    /// 重复初始化幂等（每次启动都会跑 init_schema）
    #[test]
    fn test_init_schema_idempotent() {
        let conn = in_memory();
        init_schema(&conn).unwrap();
        init_schema(&conn).unwrap();
        let count: i32 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='projects'", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(count, 1);
    }
}
