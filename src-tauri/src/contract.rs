//! 前后端契约测试（IPC 边界的字段名规则）。
//!
//! **规则（本文件是它的可执行说明）**：
//! - **响应 DTO 一律 snake_case**：字段名与 Rust/数据库列名一致，前端类型原样对应；
//! - **请求参数结构体一律 camelCase**（`#[serde(rename_all = "camelCase")]`）：与 Tauri 的
//!   命令参数约定一致（`tauri-macros` 默认 `ArgumentCase::Camel`），前端传 `projectId`。
//!
//! 为什么要有这层测试：前后端类型是**手工双重声明**，把某个响应结构体改名（或误加/误删
//! `rename_all`）不会有任何编译或类型错误，前端只会在运行时拿到 `undefined`——
//! 这类故障在界面上表现为"某个字段莫名其妙是空的"，很难定位。这里把字段名钉住。

use crate::commands::ai::{AiStatus, DshVersionInfo};
use crate::commands::editor::{HexPage, ReadFileResponse};
use crate::commands::fileops::PasteFilesResult;
use crate::commands::search::{SearchResponse, SearchResultItem};
use crate::commands::service::AddServiceParams;
use crate::core::file_watcher::{FileChange, FileChangeEvent};
use crate::core::jarfile::JarEntryInfo;
use crate::core::process::{LogLine, ServiceLogBatchPayload};
use crate::models::Service;

/// 取序列化后的字段名集合（排序，便于断言）
fn keys_of<T: serde::Serialize>(value: &T) -> Vec<String> {
    let v = serde_json::to_value(value).expect("序列化失败");
    let mut keys: Vec<String> = v.as_object().expect("DTO 应序列化为对象").keys().cloned().collect();
    keys.sort();
    keys
}

fn sorted(items: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = items.iter().map(|s| s.to_string()).collect();
    v.sort();
    v
}

#[test]
fn test_response_dtos_are_snake_case() {
    // AI 会话状态：前端 aiStore 直接读这些字段（cwd 用于判断归属项目）
    let status = AiStatus {
        running: true,
        url: None,
        pid: None,
        cwd: None,
        dsh_found: true,
    };
    assert_eq!(keys_of(&status), sorted(&["running", "url", "pid", "cwd", "dsh_found"]));

    // dsh 版本信息（面板「检查更新」）
    let info = DshVersionInfo {
        dsh_found: true,
        current: None,
        latest: None,
        outdated: false,
    };
    assert_eq!(keys_of(&info), sorted(&["dsh_found", "current", "latest", "outdated"]));

    // jar 条目（JarViewer）
    let entry = JarEntryInfo {
        name: "a.txt".into(),
        is_dir: false,
        size: 1,
        compressed_size: 1,
    };
    assert_eq!(keys_of(&entry), sorted(&["name", "is_dir", "size", "compressed_size"]));

    // 日志行（LogViewer / logStore 按 seq 幂等合并）
    let line = LogLine {
        seq: 1,
        timestamp: "t".into(),
        stream: "stdout".into(),
        text: "x".into(),
    };
    assert_eq!(keys_of(&line), sorted(&["seq", "timestamp", "stream", "text"]));

    // 日志批量事件载荷
    let batch = ServiceLogBatchPayload {
        service_key: "k".into(),
        lines: vec![],
    };
    assert_eq!(keys_of(&batch), sorted(&["service_key", "lines"]));

    // 文件变更事件（watcher 推送 + store 消费）
    let change = FileChange {
        path: "p".into(),
        service_name: "n".into(),
        service_id: "s".into(),
        kind: "modify".into(),
        restart_mode: 0,
    };
    assert_eq!(
        keys_of(&change),
        sorted(&["path", "service_name", "service_id", "kind", "restart_mode"])
    );
    let event = FileChangeEvent {
        project_id: "p".into(),
        project_name: "n".into(),
        changes: vec![],
    };
    assert_eq!(keys_of(&event), sorted(&["project_id", "project_name", "changes"]));

    // 文本搜索
    let item = SearchResultItem {
        path: "p".into(),
        name: "n".into(),
        line: 1,
        snippet: "s".into(),
    };
    assert_eq!(keys_of(&item), sorted(&["path", "name", "line", "snippet"]));
    let resp = SearchResponse { results: vec![], truncated: false };
    assert_eq!(keys_of(&resp), sorted(&["results", "truncated"]));

    // 编辑器读文件（保存时原样回传 modified 做"外部已改"检测）与 hex 分页
    let read = ReadFileResponse {
        content: String::new(),
        is_binary: false,
        size: 0,
        line_ending: "lf".into(),
        encoding: "utf8".into(),
        modified: None,
    };
    assert_eq!(
        keys_of(&read),
        sorted(&["content", "is_binary", "size", "line_ending", "encoding", "modified"])
    );
    let hex = HexPage { offset: 0, bytes: vec![], total_size: 0 };
    assert_eq!(keys_of(&hex), sorted(&["offset", "bytes", "total_size"]));

    // 粘贴结果（created/failed 决定界面提示哪一半成功）
    let paste = PasteFilesResult { created: vec![], failed: vec![] };
    assert_eq!(keys_of(&paste), sorted(&["created", "failed"]));

    // 服务行（前端 Service 类型 14 个字段一一对应）
    let svc = Service {
        id: "s".into(),
        project_id: "p".into(),
        name: "n".into(),
        command: "c".into(),
        cwd: "d".into(),
        watch_paths: "[]".into(),
        watch_include: "*".into(),
        watch_exclude: "".into(),
        env_vars: "".into(),
        restart_mode: 0,
        enabled: true,
        show_file_tree: false,
        sort_index: 0,
        tool_commands: "[]".into(),
    };
    assert_eq!(
        keys_of(&svc),
        sorted(&[
            "id", "project_id", "name", "command", "cwd", "watch_paths", "watch_include",
            "watch_exclude", "env_vars", "restart_mode", "enabled", "show_file_tree",
            "sort_index", "tool_commands",
        ])
    );
}

#[test]
fn test_request_params_are_camel_case() {
    // 请求参数走 Tauri 的驼峰约定：前端传 projectId / watchPaths / toolCommands …
    let json = serde_json::json!({
        "projectId": "p1",
        "name": "svc",
        "command": "npm run dev",
        "cwd": "C:/p",
        "watchPaths": "[]",
        "envVars": "",
        "restartMode": 0,
        "toolCommands": "[]",
    });
    let params: AddServiceParams = serde_json::from_value(json).expect("应能按驼峰反序列化");
    assert_eq!(params.project_id, "p1");
    assert_eq!(params.restart_mode, 0);
    assert_eq!(params.tool_commands, "[]");
}
