use std::collections::HashMap;
use tauri::State;
use crate::AppState;

/// 保存布局键值对（批量）
#[tauri::command]
pub fn save_layout(state: State<AppState>, items: HashMap<String, String>) -> Result<(), String> {
    state.db.with_conn(|conn| {
        let mut stmt = conn.prepare("INSERT OR REPLACE INTO layout (key, value) VALUES (?1, ?2)")
            .map_err(|e| format!("准备布局保存语句失败: {}", e))?;
        for (k, v) in &items {
            stmt.execute(rusqlite::params![k, v]).map_err(|e| format!("保存布局项 '{}' 失败: {}", k, e))?;
        }
        Ok(())
    })
}

/// 加载布局（返回所有键值对）
#[tauri::command]
pub fn load_layout(state: State<AppState>) -> Result<HashMap<String, String>, String> {
    state.db.with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT key, value FROM layout")
            .map_err(|e| format!("查询布局数据失败: {}", e))?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }).map_err(|e| format!("读取布局数据失败: {}", e))?;
        let mut map = HashMap::new();
        for r in rows {
            let (k, v) = r.map_err(|e| format!("解析布局数据失败: {}", e))?;
            map.insert(k, v);
        }
        Ok(map)
    })
}
