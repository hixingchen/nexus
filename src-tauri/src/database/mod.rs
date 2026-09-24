use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::Mutex;
use crate::models::Service;

/// 数据库放在数据目录下（默认 `~/.nexus/`，可用 `NEXUS_DATA_DIR` 覆盖，见 `crate::data_dir`），
/// 避免 Tauri dev watcher 检测到项目目录中的文件变化导致无限重启
fn db_path() -> Result<PathBuf, String> {
    let nexus_dir = crate::data_dir();
    // 目录建不出来（无写权限/磁盘只读）时打开数据库必然失败，
    // 这里直接上报真实原因，而不是让后续 open 报一句笼统的 "unable to open database file"
    std::fs::create_dir_all(&nexus_dir)
        .map_err(|e| format!("无法创建数据目录 {}: {}", nexus_dir.display(), e))?;
    Ok(nexus_dir.join("nexus.db"))
}

pub struct Database {
    pub conn: Mutex<Connection>,
    /// **此刻**持连接锁的线程标识（0 = 没人持锁）。仅供 `lock_conn` 的同线程重入检测使用。
    ///
    /// 由 `ConnGuard` 随守卫写入与清零；"最近一次取到锁的是谁"那种语义会误判（CQ-26）。
    conn_owner: std::sync::atomic::AtomicU64,
}

/// 连接守卫：让 `conn_owner` 的含义收敛为"**此刻**谁持锁"。
///
/// 为什么必须在释放前清零、而不是只在取锁时写一次（CQ-26）：写一次表达的是"最近一次取到锁
/// 的是谁"，于是 ① 另一个线程经 WouldBlock 分支拿到锁后 owner 仍是上一个持有者，后者下次
/// `try_lock` 失败会被误判成"同线程重入"而 panic（它根本没持锁，也不存在死锁）；② 线程 B
/// 经 WouldBlock 持锁期间若真的重入，owner 不等于 B → **不 panic，直接永久阻塞**——
/// 检测器唯一的用途恰好漏掉。清零与解锁的顺序也不能反：先解锁再清零会留下"锁已空闲、
/// owner 还写着上一个线程"的窗口，那正是本类型要消灭的误判。
struct ConnGuard<'a> {
    guard: std::sync::MutexGuard<'a, Connection>,
    owner: &'a std::sync::atomic::AtomicU64,
    /// 自己的线程标识：Drop 时只在"owner 确实还是我"时清零（理论上恒真，
    /// 但万一有别的写入点，宁可留着上一个值也不要抹掉别人的）
    key: u64,
}

impl Drop for ConnGuard<'_> {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        let _ = self.owner.compare_exchange(self.key, 0, Ordering::Relaxed, Ordering::Relaxed);
        // 到这里仍持锁（guard 字段在 Drop::drop 之后才被丢弃）→ 清零与解锁是原子的
    }
}

impl std::ops::Deref for ConnGuard<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection { &self.guard }
}

impl std::ops::DerefMut for ConnGuard<'_> {
    fn deref_mut(&mut self) -> &mut Connection { &mut self.guard }
}

/// 当前线程的稳定标识：`ThreadId` 不能原子存取，故用 thread_local 懒分配一个递增序号
fn thread_key() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    thread_local! {
        static KEY: u64 = NEXT.fetch_add(1, Ordering::Relaxed);
    }
    KEY.with(|k| *k)
}

impl Database {
    /// 创建数据库实例，失败时返回错误而非 panic
    pub fn try_new() -> Result<Self, String> {
        let conn = Connection::open(db_path()?).map_err(|e| format!("无法打开数据库: {}", e))?;
        init_schema(&conn)?;
        Ok(Self { conn: Mutex::new(conn), conn_owner: std::sync::atomic::AtomicU64::new(0) })
    }

    /// 用已有连接构造（仅测试）：生产路径一律走 `try_new`。
    /// 存在的意义是让测试不必写出字段列表——否则每加一个内部字段都要改所有测试构造点。
    #[cfg(test)]
    pub(crate) fn from_connection(conn: Connection) -> Self {
        Self { conn: Mutex::new(conn), conn_owner: std::sync::atomic::AtomicU64::new(0) }
    }

    /// 取连接锁。**同一线程重入会被识别出来并 panic，而不是静默死锁。**
    ///
    /// 为什么值得专门检测：`std::sync::Mutex` 不可重入，而本项目的调用链里存在
    /// "在 `with_conn*` 闭包内调用另一个会查库的函数"这一模式（路径/白名单校验函数最容易踩）。
    /// 一旦发生就是**永久死锁**——闭包不返回 → 守卫不释放 → 之后所有查库命令一起卡死、
    /// 界面冻结、只能杀进程；而 clippy、单测、类型检查**全都发现不了**。
    /// `add_service_from_template` 就是这样踩中的（事务闭包内回调 `registered_dirs`）。
    ///
    /// 这里把它变成一条带修复指引的 panic：开发期一眼可见，而不是用户侧的静默冻死。
    fn lock_conn(&self) -> ConnGuard<'_> {
        use std::sync::atomic::Ordering;
        use std::sync::TryLockError;
        let key = thread_key();
        let guard = match self.conn.try_lock() {
            Ok(guard) => guard,
            // 锁中毒（持锁线程 panic）继续使用内部连接：与 core/process.rs::stop_all 同策略。
            // 此前直接返回错误 → 一次 panic 会让整个数据库功能永久不可用
            Err(TryLockError::Poisoned(poisoned)) => {
                log::error!("数据库连接锁已中毒，继续使用: {}", poisoned);
                poisoned.into_inner()
            }
            Err(TryLockError::WouldBlock) => {
                if self.conn_owner.load(Ordering::Relaxed) == key {
                    panic!(
                        "检测到同一线程重入数据库连接锁 —— 这必然死锁。\
                         调用链中有人在 with_conn/with_conn_mut 的闭包内又发起了查库\
                         （常见于路径/白名单校验函数），请把那次查库移到取锁之前。"
                    );
                }
                // 其他线程持有时按原语义阻塞等待
                self.conn.lock().unwrap_or_else(|e| {
                    log::error!("数据库连接锁已中毒，继续使用: {}", e);
                    e.into_inner()
                })
            }
        };
        // 取到锁之后**才**记 owner，由守卫在释放前清零——三条分支共用这一个写点，
        // 正是 CQ-26 缺失的那一步（原先 WouldBlock 分支拿到守卫却不写 owner）
        self.conn_owner.store(key, Ordering::Relaxed);
        ConnGuard { guard, owner: &self.conn_owner, key }
    }

    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T, String>) -> Result<T, String> {
        f(&self.lock_conn())
    }

    /// 可变连接（事务等需要 &mut 的场景）
    pub fn with_conn_mut<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T, String>) -> Result<T, String> {
        f(&mut self.lock_conn())
    }
}

/// 当前 schema 版本号（仅记录用途，**不是迁移开关**）。
///
/// 历史教训：版本号不匹配曾触发删表重建——用户升级即清空全部项目/服务配置。
/// 表结构变更一律走下方 MIGRATION_COLUMNS 增量 ALTER（幂等加列），
/// 任何旧版本数据库都能原地升级，禁止再引入 DROP 任何用户表。
const SCHEMA_VERSION: i32 = 9;

/// 建表 + 迁移（`pub(crate)`：命令层的 SQL 路径测试要自己搭一个内存库，见 commands/service.rs 的 tests）
pub(crate) fn init_schema(conn: &Connection) -> Result<(), String> {
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

/// services 表的共享列清单：SELECT（读侧）与两处 INSERT（写侧）**共用这一份**。
///
/// 读侧为什么必须按列名：位置化 `row.get(N)` 一旦与 SELECT 列表顺序不一致，**不会报错**——
/// 相邻的同类型列会被静默互换（类型正确、编译干净、测试全绿，界面显示错数据）。
///
/// 写侧为什么也要共用：`services` 原先有 4 份手工列清单（3 处 INSERT + 1 处 UPDATE）。
/// 给表加一列时漏掉 `duplicate_project` 的那一份**不是报错**——新列有 DEFAULT，
/// 复制出来的服务"新字段全是默认值"，配置与非复制版本悄悄不一致，没有任何测试会失败。
/// 共用常量后这份清单只剩一处，配套的绑定方式见 `named_placeholders`。
pub const SERVICE_COLUMNS: &str =
    "id, project_id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, \
     restart_mode, enabled, show_file_tree, sort_index, tool_commands";

/// `UPDATE services` 允许改动的列（`id`/`project_id`/`sort_index` 不由更新命令改）。
pub const SERVICE_UPDATABLE_COLUMNS: &str =
    "name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, \
     enabled, show_file_tree, tool_commands";

/// `service_templates` 的列清单——**唯一来源**（ARCH-20）。
///
/// 为什么必须有（照 `SERVICE_COLUMNS` 的形状）：这 14 列此前在 `commands/service.rs` 里
/// 手写了**三处生产 INSERT**（服务「另存为模板」/ 模板库「新建」/ 导入），加一列要三处同改，
/// 而**漏一处不是编译错误**——症状是"同一个字段在不同入口有时写进去、有时没有"。
/// 读侧的 SELECT 也各写各的（`collect_export_templates` / `get_service_templates` 等）。
pub const SERVICE_TEMPLATE_COLUMNS: &str =
    "id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, \
     restart_mode, enabled, show_file_tree, sort_index, tool_commands, open_tool_id";

/// 由列清单生成 `VALUES` 用的命名占位符（`:列名`，顺序与列清单一致）。
///
/// 为什么用命名参数而不是 `?1..?N`：位置化绑定下，"往列清单中间插一列"会让后面的值
/// 整体错位——占位符个数仍然对得上，于是**静默写错列**。命名参数下漏给一个值会直接
/// 报 `InvalidParameterName`（响亮失败），且列清单的增删与值的书写顺序解耦。
pub fn named_placeholders(columns: &str) -> String {
    columns.split(',').map(|c| format!(":{}", c.trim())).collect::<Vec<_>>().join(", ")
}

/// 由列清单生成 `UPDATE` 的 `SET` 片段（`列名=:列名, …`）。
pub fn named_assignments(columns: &str) -> String {
    columns.split(',').map(|c| { let c = c.trim(); format!("{c}=:{c}") }).collect::<Vec<_>>().join(", ")
}

/// 查询指定项目下的所有服务（共享函数，消除重复 SQL）
pub fn query_services_by_project(conn: &Connection, project_id: &str) -> Result<Vec<Service>, String> {
    let mut stmt = conn.prepare(
        &format!("SELECT {} FROM services WHERE project_id=?1 ORDER BY sort_index", SERVICE_COLUMNS)
    ).map_err(|e| format!("查询服务列表失败: {}", e))?;
    let rows = stmt.query_map([project_id], |row| {
        Ok(Service {
            id: row.get("id")?,
            project_id: row.get("project_id")?,
            name: row.get("name")?,
            command: row.get("command")?,
            cwd: row.get("cwd")?,
            watch_paths: row.get("watch_paths")?,
            watch_include: row.get("watch_include")?,
            watch_exclude: row.get("watch_exclude")?,
            env_vars: row.get("env_vars")?,
            restart_mode: row.get("restart_mode")?,
            enabled: row.get::<_, i32>("enabled")? != 0,
            show_file_tree: row.get::<_, i32>("show_file_tree")? != 0,
            sort_index: row.get("sort_index")?,
            tool_commands: row.get("tool_commands")?,
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

    fn db_wrapper() -> Database {
        Database::from_connection(in_memory())
    }

    /// CQ-26：`conn_owner` 必须表达"**此刻**谁持锁"，而不是"最近一次取到锁的是谁"。
    ///
    /// 两半都要对：持锁期间是自己的线程标识，**释放时必须清零**——旧实现只在取锁时写一次、
    /// 释放时不清零，于是锁空闲时 owner 还写着上一个线程。
    #[test]
    fn test_conn_owner_tracks_current_holder_only() {
        let db = db_wrapper();
        let me = thread_key();
        assert_eq!(db.conn_owner.load(std::sync::atomic::Ordering::Relaxed), 0, "未持锁时 owner 必须是 0");

        db.with_conn(|_| {
            assert_eq!(
                db.conn_owner.load(std::sync::atomic::Ordering::Relaxed),
                me,
                "持锁期间 owner 必须是当前线程（否则重入检测形同虚设）",
            );
            Ok::<(), String>(())
        }).expect("查库");

        assert_eq!(
            db.conn_owner.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "释放后必须清零：否则锁空闲时 owner 仍指向旧持有者，它下次查库会被误判成重入而 panic",
        );
    }

    /// CQ-26：**经 WouldBlock 分支**拿到锁的线程，也必须成为 owner。
    ///
    /// 旧实现在这条分支里拿到守卫却不更新 `conn_owner`，于是：
    /// ① 交班后原持有者再来查库 → 看到 owner == 自己 → **误报重入 panic**（它并没有持锁）；
    /// ② 新持有者真重入时 owner 不等于它 → 不 panic，**直接永久阻塞**——检测器唯一的用途漏掉。
    ///
    /// 这条用例尽力让子线程走 WouldBlock 分支（主线程持锁 → 子线程就位 → 主线程才放锁）；
    /// 即便某次调度让子线程走了立即可得的路径，"owner 必须是子线程"这个断言依然成立。
    #[test]
    fn test_blocked_acquire_records_new_owner() {
        let db = std::sync::Arc::new(db_wrapper());
        let me = thread_key();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let (owner_tx, owner_rx) = std::sync::mpsc::channel::<u64>();

        let child_db = db.clone();
        let child = std::thread::spawn(move || {
            let _ = ready_tx.send(());
            child_db
                .with_conn(|_| {
                    // 主线程此刻已经放锁（它在下面等这条消息），所以这里读到的 owner 就是"交接后"的值
                    let _ = owner_tx.send(child_db.conn_owner.load(std::sync::atomic::Ordering::Relaxed));
                    Ok::<(), String>(())
                })
                .expect("子线程查库");
        });

        // 主线程持锁期间等子线程就位；就位后还要给它一点时间去撞 try_lock（见上）
        let released = db.with_conn(|_| {
            ready_rx.recv_timeout(std::time::Duration::from_secs(5)).expect("子线程就位");
            std::thread::sleep(std::time::Duration::from_millis(150));
            Ok::<(), String>(())
        });
        released.expect("主线程查库");

        let owner_seen = owner_rx.recv_timeout(std::time::Duration::from_secs(5)).expect("子线程拿到锁");
        assert_ne!(owner_seen, me, "锁已交班，owner 不能还是主线程（旧实现在这条分支上不写 owner）");
        assert_ne!(owner_seen, 0, "子线程持锁期间 owner 必须是它自己");
        child.join().expect("子线程不应 panic——误报重入会让它在这里炸掉");
    }

    /// 同线程重入连接锁必须是**响亮失败**而不是永久死锁。
    ///
    /// 死锁的后果是整个应用静默冻死（闭包不返回 → 守卫不释放 → 后续所有查库命令一起卡死），
    /// 且 clippy / 单测 / 类型检查全抓不到。`add_service_from_template` 正是这样踩中的
    /// （事务闭包内回调 `registered_dirs`）。这条测试把该防护钉住：若哪天有人把重入检测删掉，
    /// 这个测试会**挂住**（而不是失败）——故用独立线程 + 超时感知，见下。
    #[test]
    fn test_reentrant_with_conn_panics_instead_of_deadlocking() {
        let db = db_wrapper();
        // 在子线程里做重入：真出现死锁时只冻住子线程，主线程能超时返回、测试不至于永久挂起
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done2 = done.clone();
        let handle = std::thread::spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = db.with_conn(|_| db.with_conn(|_| Ok::<(), String>(())));
            }));
            assert!(r.is_err(), "同线程重入数据库连接锁应当 panic，而不是继续执行");
            done2.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        for _ in 0..200 {
            if done.load(std::sync::atomic::Ordering::SeqCst) { break; }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(
            done.load(std::sync::atomic::Ordering::SeqCst),
            "重入检测失效：子线程既没 panic 也没返回（很可能是死锁）",
        );
        handle.join().expect("子线程不应 panic 出测试范围");
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

    /// 服务行映射逐字段回归：列名取值把我们从一个"换序即静默错列"的结构里挪了出来，
    /// 但列名本身仍可能与 SELECT 不一致（写错名字会在运行时才报"no such column"）。
    /// 这里用真实 schema 建一行、逐字段断言，作为映射契约的守门测试。
    #[test]
    fn test_query_services_by_project_maps_every_column() {
        let conn = in_memory();
        init_schema(&conn).unwrap();
        conn.execute("INSERT INTO projects (id, name, path) VALUES ('p1', 'P', 'C:/p')", []).unwrap();
        conn.execute(
            "INSERT INTO services (id, project_id, name, command, cwd, watch_paths, watch_include, watch_exclude, env_vars, restart_mode, enabled, show_file_tree, sort_index, tool_commands)
             VALUES ('s1', 'p1', 'svc', 'npm run dev', 'C:/p/svc', '[\"C:/p/svc\"]', '*.ts', 'node_modules', 'A=1', 2, 1, 1, 7, '[{\"id\":\"c1\"}]')",
            [],
        ).unwrap();

        let svcs = query_services_by_project(&conn, "p1").unwrap();
        assert_eq!(svcs.len(), 1);
        let s = &svcs[0];
        assert_eq!(s.id, "s1");
        assert_eq!(s.project_id, "p1");
        assert_eq!(s.name, "svc");
        assert_eq!(s.command, "npm run dev");
        assert_eq!(s.cwd, "C:/p/svc");
        assert_eq!(s.watch_paths, "[\"C:/p/svc\"]");
        assert_eq!(s.watch_include, "*.ts");
        assert_eq!(s.watch_exclude, "node_modules");
        assert_eq!(s.env_vars, "A=1");
        assert_eq!(s.restart_mode, 2);
        assert!(s.enabled);
        assert!(s.show_file_tree);
        assert_eq!(s.sort_index, 7);
        assert_eq!(s.tool_commands, "[{\"id\":\"c1\"}]");
    }

    /// 共享列清单必须覆盖 `services` 的**全部**列，且 UPDATE 清单必须是它的子集。
    ///
    /// 为什么值得一条测试：`SERVICE_COLUMNS` 现在是读侧 SELECT 与写侧两处 INSERT 的唯一来源，
    /// 而"给表加列"（CREATE TABLE + MIGRATION_COLUMNS 各改一行）与"更新这份清单"是两处独立编辑。
    /// 漏更新的后果**不报错**：INSERT 少写一列 → 该列取 DEFAULT，复制出来的服务配置悄悄走样，
    /// 编译、clippy、其余测试全绿。这条测试把两处编辑绑在一起，漏了立刻红。
    #[test]
    fn test_service_column_lists_cover_schema() {
        let conn = in_memory();
        init_schema(&conn).unwrap();
        let mut stmt = conn.prepare("PRAGMA table_info(services)").unwrap();
        let actual: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();

        let listed: Vec<String> = SERVICE_COLUMNS.split(',').map(|c| c.trim().to_string()).collect();
        // created_at 不进清单：它是"这行什么时候建的"，写侧一律交给 DB 默认值
        let missing: Vec<&String> = actual
            .iter()
            .filter(|c| !listed.contains(c) && c.as_str() != "created_at")
            .collect();
        assert!(
            missing.is_empty(),
            "services 新增了列 {:?}：请同步 SERVICE_COLUMNS（读侧 SELECT 与两处 INSERT 共用它），\
             漏掉它不会报错——新列会静默取默认值",
            missing
        );

        let updatable: Vec<&str> = SERVICE_UPDATABLE_COLUMNS.split(',').map(|c| c.trim()).collect();
        for c in &updatable {
            assert!(
                listed.iter().any(|l| l == c),
                "SERVICE_UPDATABLE_COLUMNS 里的 {:?} 不在 SERVICE_COLUMNS 中",
                c
            );
        }
        let not_updatable: Vec<&str> = listed
            .iter()
            .map(|s| s.as_str())
            .filter(|c| !updatable.contains(c))
            .collect();
        assert_eq!(
            not_updatable,
            vec!["id", "project_id", "sort_index"],
            "UPDATE 清单之外的列变了：如果是新增列，请明确它该不该被 update_service 改写\
             （不该改就加进本断言的期望值，该改就加进 SERVICE_UPDATABLE_COLUMNS）"
        );
    }

    /// `SERVICE_TEMPLATE_COLUMNS` 必须覆盖 `service_templates` 的**全部**列（ARCH-20）。
    ///
    /// 与上面 `services` 那条同源：列清单现在是三处模板写入口（另存为模板 / 模板库新建 /
    /// 导入）的唯一来源，而"给表加列"与"更新清单"是两处独立编辑——漏了不报错，
    /// 症状是"同一个字段在不同入口有时写进去、有时没有"。这条把两处编辑绑在一起。
    #[test]
    fn test_service_template_column_list_covers_schema() {
        let conn = in_memory();
        init_schema(&conn).unwrap();
        let mut stmt = conn.prepare("PRAGMA table_info(service_templates)").unwrap();
        let actual: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();

        let listed: Vec<String> = SERVICE_TEMPLATE_COLUMNS.split(',').map(|c| c.trim().to_string()).collect();
        // created_at 不进清单：写侧一律交给 DB 默认值（与 services 同口径）
        let missing: Vec<&String> = actual
            .iter()
            .filter(|c| !listed.contains(c) && c.as_str() != "created_at")
            .collect();
        assert!(
            missing.is_empty(),
            "service_templates 新增了列 {:?}：请同步 SERVICE_TEMPLATE_COLUMNS\
             （三处模板写入口共用它），漏掉它不会报错——新列会静默取默认值",
            missing
        );
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
