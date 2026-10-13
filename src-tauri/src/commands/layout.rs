use std::collections::HashMap;
use rusqlite::Connection;
use tauri::State;
use crate::AppState;

/// 保存布局键值对（批量）
#[tauri::command]
pub fn save_layout(state: State<AppState>, items: HashMap<String, String>) -> Result<(), String> {
    state.db.with_conn_mut(|conn| {
        // 事务：避免逐条自动提交（多次 fsync），中途失败时整体回滚。
        // 用 rusqlite 事务而非手写 BEGIN/COMMIT：出错时由 Drop 自动 ROLLBACK
        // （手写版在 COMMIT 失败时会把连接留在未提交事务里，后续语句全部报错）
        let tx = conn.transaction().map_err(|e| format!("开启事务失败: {}", e))?;
        {
            let mut stmt = tx.prepare("INSERT OR REPLACE INTO layout (key, value) VALUES (?1, ?2)")
                .map_err(|e| format!("准备布局保存语句失败: {}", e))?;
            for (k, v) in &items {
                stmt.execute(rusqlite::params![k, v]).map_err(|e| format!("保存布局项 '{}' 失败: {}", k, e))?;
            }
        }
        tx.commit().map_err(|e| format!("提交事务失败: {}", e))
    })
}

/// 加载布局（返回所有键值对）
#[tauri::command]
pub fn load_layout(state: State<AppState>) -> Result<HashMap<String, String>, String> {
    state.db.with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT key, value FROM layout")
            .map_err(|e| format!("查询布局数据失败: {}", e))?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>("key")?, row.get::<_, String>("value")?))
        }).map_err(|e| format!("读取布局数据失败: {}", e))?;
        let mut map = HashMap::new();
        for r in rows {
            let (k, v) = r.map_err(|e| format!("解析布局数据失败: {}", e))?;
            map.insert(k, v);
        }
        Ok(map)
    })
}

/// 布局表中**重置时要保留**的键。它们的共同点：**不是"面板几何"**，清掉会让人莫名其妙。
///
/// - `selected_project_id`：用户点「恢复默认布局」是想让面板回到初始尺寸，不是想丢掉正在看的
///   项目——一起清掉的话，下次启动就停在"选择一个项目开始"。
/// - `editor_font_size`：文本字号（代码视图 + Markdown 预览）属于设置里的「编辑器」分类，
///   在"恢复默认布局"里被顺手清掉，用户会觉得"我就想调个面板宽度，字怎么变小了"。
///
/// 这几个字面量与前端 `stores/layoutStore.ts` 的 `LAYOUT_KEYS` 是同一把键。`contract.rs` 管的是
/// IPC 载荷的字段名，管不到"存进库里的键"，所以这一对只能靠人同步——新增布局键时，
/// 先问一句"它算不算几何"。
const KEEP_ON_RESET: &[&str] = &["selected_project_id", "editor_font_size"];

/// 恢复默认布局的**纯逻辑**（单独成函数才测得到，`State<AppState>` 在单测里造不出来）：
/// 清掉除 [`KEEP_ON_RESET`] 以外的全部布局键，返回删除的行数。
pub(crate) fn clear_layout(conn: &Connection) -> Result<usize, String> {
    // 占位符按清单长度现生成，而不是写死 `?1`/`?2`：清单加一项却忘了改 SQL，结果是
    // **那一项没被保留下来**（清库照常成功，没有报错，只在用户下次看设置时才发现被重置了）。
    // 拼进 SQL 的只有 `?N` 这种编译期常量，值仍然走参数绑定。
    let placeholders = (1..=KEEP_ON_RESET.len())
        .map(|i| format!("?{}", i))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("DELETE FROM layout WHERE key NOT IN ({})", placeholders);
    conn.execute(&sql, rusqlite::params_from_iter(KEEP_ON_RESET.iter()))
        .map_err(|e| format!("清除布局数据失败: {}", e))
}

/// 恢复默认布局：把布局表清回"从没设置过"的状态。
///
/// 为什么是**删行**而不是"把默认值写回去"：默认值（左列 260、折叠 false、AI 面板宽度…）
/// 是各组件自己的字面量（`MainLayout` 的 `useState(260)` 等），后端并不知道它们。
/// 删掉行、各消费方回落到自己的默认值——默认值因此只有**一处**定义；写回一份到库里
/// 等于把它复制成两份，早晚会出现"恢复默认布局恢复出一个旧默认值"。
///
/// 前端调用点见 `SettingsModal` 的「恢复默认布局」，那里守两条：
/// 有未保存草稿时不做（不能拿别人的改动当代价），以及**不刷新窗口**（刷新会关掉所有
/// 已打开的标签页、并拆掉 AI 会话）。
#[tauri::command]
pub fn reset_layout(state: State<AppState>) -> Result<usize, String> {
    state.db.with_conn(clear_layout)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 建一张与 `database::init_schema` 同形的 layout 表（内存库，不碰真实数据目录）
    fn conn_with_layout() -> Connection {
        let conn = Connection::open_in_memory().expect("内存库");
        conn.execute_batch("CREATE TABLE layout (key TEXT PRIMARY KEY, value TEXT NOT NULL);")
            .expect("建表");
        conn
    }

    fn put(conn: &Connection, key: &str, value: &str) {
        conn.execute("INSERT OR REPLACE INTO layout (key, value) VALUES (?1, ?2)", rusqlite::params![key, value])
            .expect("写入布局项");
    }

    /// 「恢复默认布局」清掉几何类布局键，**但留住**当前选中的项目与编辑器字号。
    ///
    /// 两个方向都有代价：漏清一个键 = 用户点了按钮而那一项没复位（看起来像没生效）；
    /// 多清一个保留键 = 项目选择或字号被顺手抹掉，而用户点的是"恢复默认**布局**"。
    #[test]
    fn test_clear_layout_keeps_non_geometry_keys() {
        let conn = conn_with_layout();
        put(&conn, "selected_project_id", "p-1");
        put(&conn, "editor_font_size", "18");
        put(&conn, "left_panel_width", "420");
        put(&conn, "left_panel_collapsed", "1");
        put(&conn, "service_panel_collapsed", "1");
        put(&conn, "right_panel_top_height", "300");
        put(&conn, "ai_panel_width", "500");

        let removed = clear_layout(&conn).expect("清库");
        assert_eq!(removed, 5, "应当清掉 5 项（7 项减去 2 个保留键）");

        let left: HashMap<String, String> = {
            let mut stmt = conn.prepare("SELECT key, value FROM layout").expect("查询");
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).expect("遍历");
            rows.map(|r| r.expect("行")).collect()
        };
        assert_eq!(left.get("selected_project_id").map(String::as_str), Some("p-1"));
        assert_eq!(
            left.get("editor_font_size").map(String::as_str),
            Some("18"),
            "编辑器字号不是面板几何，不该被「恢复默认布局」清掉"
        );
        assert_eq!(left.len(), 2, "除保留键外不该留下任何布局键：{:?}", left);

        // 幂等：没有可清的行时返回 0，不报错（用户连点两次不该看到错误提示）
        assert_eq!(clear_layout(&conn).expect("再清一次"), 0);
    }

    /// 保留清单里的每一项都必须真的存在——防的是"清单里写了个拼错的键名"。
    ///
    /// 拼错的后果很隐蔽：`key NOT IN (...)` 照常执行、清库照常成功，而那个**本该保留**的键
    /// 因为名字对不上被当成几何键清掉了。所以这里逐项断言它活得下来。
    #[test]
    fn test_every_keep_key_is_actually_preserved() {
        for key in KEEP_ON_RESET {
            let conn = conn_with_layout();
            put(&conn, key, "keep-me");
            // 再放一个几何键，确保这一轮真的执行了删除
            put(&conn, "left_panel_width", "420");
            clear_layout(&conn).expect("清库");
            let survived: Option<String> = conn
                .query_row("SELECT value FROM layout WHERE key = ?1", rusqlite::params![key], |r| r.get(0))
                .ok();
            assert_eq!(survived.as_deref(), Some("keep-me"), "保留清单里的 `{}` 没有活下来", key);
        }
    }
}
