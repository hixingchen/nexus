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
use crate::commands::editor::{FileEntry, HexPage, JarEntryContent, ReadFileResponse};
use crate::commands::fileops::PasteFilesResult;
use crate::commands::process::{
    ProcessStatus, RunningService, ToolCommandLogBatchPayload, ToolCommandLogLine,
    ToolCommandResult,
};
use crate::commands::search::{SearchParams, SearchResponse, SearchResultItem};
use crate::commands::node::{
    AvailableNodeVersions, NodeRuntimeStatus, NvmCommandResult, NvmInstallResult,
};
use crate::commands::service::{AddServiceParams, ImportTemplatesResult};
use crate::core::file_watcher::{FileChange, FileChangeEvent};
use crate::core::jarfile::JarEntryInfo;
use crate::core::process::{FailedService, LogLine, ServiceLogBatchPayload};
use crate::models::{
    OpenTool, Project, ProjectDetail, Service, ServiceOpenToolBinding, ServiceTemplate, ToolCommand,
};

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
    let resp = SearchResponse { results: vec![], truncated: false, skipped: None };
    // skipped 带 skip_serializing_if：没有原因时它**不出现**在载荷里（前端按可选字段读）
    assert_eq!(keys_of(&resp), sorted(&["results", "truncated"]));
    let resp_skipped = SearchResponse { results: vec![], truncated: false, skipped: Some("x".into()) };
    assert_eq!(keys_of(&resp_skipped), sorted(&["results", "truncated", "skipped"]));

    // 模板导入结果（响应 DTO：snake_case；前端的 ImportTemplatesResult 原样对应）
    // Node 运行时（左上角「Node 版本」面板）
    let node_rt = NodeRuntimeStatus {
        available: true, root: String::new(), installed: vec![], current: String::new(), reason: String::new(),
    };
    assert_eq!(keys_of(&node_rt), sorted(&["available", "root", "installed", "current", "reason"]));
    let avail = AvailableNodeVersions {
        current: vec![], lts: vec![], old_stable: vec![], old_unstable: vec![],
    };
    assert_eq!(keys_of(&avail), sorted(&["current", "lts", "old_stable", "old_unstable"]));
    let nvm_res = NvmCommandResult { ok: true, output: String::new() };
    assert_eq!(keys_of(&nvm_res), sorted(&["ok", "output"]));
    let nvm_inst = NvmInstallResult { ok: true, code: Some(0), path: String::new() };
    assert_eq!(keys_of(&nvm_inst), sorted(&["ok", "code", "path"]));

    let imp = ImportTemplatesResult {
        imported: 0, duplicated: 0, missing_tools: vec![], missing_dirs: vec![],
    };
    assert_eq!(keys_of(&imp), sorted(&["imported", "duplicated", "missing_tools", "missing_dirs"]));

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

    // ── ARCH-11：以下是原先未覆盖的 13 个 DTO，补全后"改名静默失效"才没有盲区 ──

    // 文件树条目（FileTree 靠 is_dir 决定能否展开——改名会让**所有目录变成文件**）
    let fe = FileEntry {
        name: "a".into(),
        path: "p".into(),
        is_dir: true,
        size: 0,
        extension: None,
    };
    assert_eq!(keys_of(&fe), sorted(&["name", "path", "is_dir", "size", "extension"]));

    // jar 条目内容（编辑器按 kind 分流文本/反编译/二进制）
    let jc = JarEntryContent { content: String::new(), kind: "binary".into(), size: 0 };
    assert_eq!(keys_of(&jc), sorted(&["content", "kind", "size"]));

    // 项目与项目详情
    let proj = Project {
        id: "p".into(),
        name: "n".into(),
        path: "d".into(),
        pinned: false,
        sort_index: 0,
    };
    assert_eq!(keys_of(&proj), sorted(&["id", "name", "path", "pinned", "sort_index"]));
    let detail = ProjectDetail { project: proj, services: vec![] };
    assert_eq!(keys_of(&detail), sorted(&["project", "services"]));

    // 工具命令与打开工具/绑定/服务模板
    //
    // `ToolCommand` 尤其要钉死：它**序列化后写进 services.tool_commands 的 TEXT 列**
    // （见 service.rs 的写入校验）——改名不只是界面读不到，而是历史行里的超时配置
    // 静默回落到默认 60 秒。
    let tc = ToolCommand { id: "t".into(), name: "n".into(), command: "c".into(), timeout_secs: None };
    assert_eq!(keys_of(&tc), sorted(&["id", "name", "command", "timeout_secs"]));
    let ot = OpenTool {
        id: "o".into(),
        name: "n".into(),
        command: "c".into(),
        executable: "e".into(),
        args: "a".into(),
    };
    assert_eq!(keys_of(&ot), sorted(&["id", "name", "command", "executable", "args"]));
    let bind = ServiceOpenToolBinding { service_id: "s".into(), tool_id: "t".into() };
    assert_eq!(keys_of(&bind), sorted(&["service_id", "tool_id"]));
    let tpl = ServiceTemplate {
        id: "t".into(),
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
        tool_commands: "[]".into(),
        open_tool_id: "".into(),
        created_at: "t".into(),
    };
    assert_eq!(
        keys_of(&tpl),
        sorted(&[
            "id", "name", "command", "cwd", "watch_paths", "watch_include", "watch_exclude",
            "env_vars", "restart_mode", "enabled", "show_file_tree", "tool_commands",
            "open_tool_id", "created_at",
        ])
    );

    // 运行状态总览（前端按 exit_code 去重 + 失败态渲染；followed_log 供右键菜单
    // 显示「取消跟随日志（文件名）」）
    let run = RunningService { service_id: "s".into(), project_id: "p".into(), followed_log: None };
    assert_eq!(keys_of(&run), sorted(&["service_id", "project_id", "followed_log"]));
    let failed = FailedService { service_id: "s".into(), exit_code: Some(1), timestamp: "t".into() };
    assert_eq!(keys_of(&failed), sorted(&["service_id", "exit_code", "timestamp"]));
    let ps = ProcessStatus { running: vec![], failed: vec![] };
    assert_eq!(keys_of(&ps), sorted(&["running", "failed"]));

    // 工具命令结果与实时输出（PERF-11 起输出改成批量事件）
    let tcr = ToolCommandResult { success: true, output: String::new(), exit_code: Some(0) };
    assert_eq!(keys_of(&tcr), sorted(&["success", "output", "exit_code"]));
    let line = ToolCommandLogLine { stream: "stdout".into(), data: "x".into() };
    assert_eq!(keys_of(&line), sorted(&["stream", "data"]));
    let tcb = ToolCommandLogBatchPayload { run_id: "r".into(), lines: vec![] };
    assert_eq!(keys_of(&tcb), sorted(&["run_id", "lines"]));
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

    // ARCH-12：`SearchParams` 的这三个字段都带 `#[serde(default)]`——这里丢了
    // `rename_all = "camelCase"` 不会报 `missing field`，而是**静默取默认值**：
    // 用户勾了"区分大小写"、把上限调到 5000，全部被无声忽略。
    // 所以断言必须"读到了与默认值不同的值"（true vs 默认 false、5000 vs 默认 1000），
    // 只断言字段存在是不够的——那正是回落路径也会满足的条件。
    let json = serde_json::json!({
        "root": "C:/p",
        "query": "needle",
        "extensions": ["ts", "tsx"],
        "caseSensitive": true,
        "maxResults": 5000,
    });
    let sp: SearchParams = serde_json::from_value(json).expect("应能按驼峰反序列化");
    assert_eq!(sp.root, "C:/p");
    assert_eq!(sp.query, "needle");
    assert_eq!(sp.extensions, vec!["ts".to_string(), "tsx".to_string()], "extensions 被静默忽略");
    assert_eq!(sp.max_results, 5000, "maxResults 被静默回落到默认 1000");
    assert!(sp.case_sensitive, "caseSensitive 被静默回落到默认 false");

    // 反面：若真按 snake_case 传，则必然**读不到**（证明上面的断言确实在测驼峰映射）
    let snake = serde_json::json!({
        "root": "C:/p",
        "query": "needle",
        "case_sensitive": true,
        "max_results": 5000,
    });
    let sp2: SearchParams = serde_json::from_value(snake).expect("root/query 命中即可反序列化");
    assert_eq!(sp2.max_results, 1000, "snake_case 不该被读到（读到说明 rename_all 失效）");
    assert!(!sp2.case_sensitive, "snake_case 不该被读到");
}

/// 从 `struct X` / `pub struct X` / `pub(crate) struct X` 这类行里取出类型名。
/// 严格要求行首就是声明（剥掉可见性修饰后必须直接是 `struct `），
/// 这样注释、`impl X {`、`let x = Foo { … }` 都不会被误判。
fn struct_name(line: &str) -> Option<&str> {
    let mut rest = line;
    for kw in ["pub(crate) ", "pub(super) ", "pub "] {
        if let Some(r) = rest.strip_prefix(kw) {
            rest = r;
            break;
        }
    }
    let rest = rest.strip_prefix("struct ")?;
    let name = rest.split(|c: char| !c.is_alphanumeric() && c != '_').next()?;
    (!name.is_empty()).then_some(name)
}

/// 扫 `src/` 下所有「`#[derive(…Serialize…)` 紧跟的结构体」的名字（跳过测试代码段）。
///
/// 启发式，且**故意宽松**：漏报（例如 derive 跨多行）只是退回今天的行为，
/// 不会造成损害；误报的代价是在豁免清单里加一行。它的目的是提醒，不是证明。
fn scan_serializable_structs() -> Vec<String> {
    fn walk(dir: &std::path::Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
                continue;
            }
            if p.extension().and_then(|s| s.to_str()) != Some("rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            let mut saw_serialize = false;
            // 测试段之后的类型不是 IPC 载荷（守卫只关心生产代码）。
            // 约定：文件里的测试一律在 `#[cfg(test)]` 之后
            let mut in_tests = false;
            for line in text.lines() {
                let t = line.trim();
                if t.starts_with("#[cfg(test)]") {
                    in_tests = true;
                }
                if in_tests {
                    continue;
                }
                if t.starts_with("#[") {
                    if t.contains("derive(") {
                        saw_serialize = t.contains("Serialize");
                    }
                    continue; // 其余属性（#[serde(…)] 等）不影响判定
                }
                if let Some(name) = struct_name(t) {
                    if saw_serialize {
                        out.push(name.to_string());
                    }
                }
                saw_serialize = false;
            }
        }
    }
    let mut out = Vec::new();
    walk(std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src")), &mut out);
    out
}

/// ARCH-11③：DTO 覆盖清单守卫。
///
/// 为什么需要它（而不是靠人记得）：`contract.rs` 的价值是"字段改名 → 测试红"，
/// 但它只能守住**已经在清单里**的类型——新增一个响应 DTO 却忘了补断言时，
/// 编译干净、测试全绿，前端拿到的 `undefined` 要到运行时才暴露，而这正是
/// ARCH-11 描述的失败模式本身。这条测试把"仓库里多了一个 Serialize 结构体"
/// 变成一次**显式的决定**：要么补断言，要么在这里写明它为什么不是 IPC 载荷。
#[test]
fn test_every_serializable_struct_is_covered() {
    // 与 `test_response_dtos_are_snake_case` / `test_request_params_are_camel_case`
    // 里的断言一一对应；新增类型必须同步这里（测试会红着提醒你）
    const COVERED: &[&str] = &[
        "AiStatus", "DshVersionInfo",
        "FileEntry", "ReadFileResponse", "HexPage", "JarEntryContent",
        "PasteFilesResult",
        "ToolCommandResult", "ToolCommandLogLine", "ToolCommandLogBatchPayload",
        "RunningService", "ProcessStatus",
        "SearchResultItem", "SearchResponse",
        "FileChangeEvent", "FileChange", "JarEntryInfo",
        "LogLine", "FailedService", "ServiceLogBatchPayload",
        "Project", "ToolCommand", "OpenTool", "ServiceOpenToolBinding",
        "Service", "ServiceTemplate", "ProjectDetail",
        "ImportTemplatesResult",
        "NodeRuntimeStatus", "AvailableNodeVersions", "NvmCommandResult", "NvmInstallResult",
        // 豁免：这两个是**模板导出文件的格式**，只在 export/import 命令内部读写磁盘，
        // 不经 invoke 出前端，因此没有 keys_of 断言。它们的字段用 camelCase 是为了
        // 文件给人看、可手工编辑——与"响应 DTO 一律 snake_case"不冲突（那条规则只管 IPC 载荷）
        "ExportedTemplate", "TemplateExportFile",
    ];
    let found = scan_serializable_structs();
    assert!(
        found.len() >= 20,
        "扫描只找到 {} 个 Serialize 结构体，解析器很可能失效了（而非仓库里真这么少）",
        found.len()
    );

    let missing: Vec<&String> = found.iter().filter(|n| !COVERED.contains(&n.as_str())).collect();
    assert!(
        missing.is_empty(),
        "以下 Serialize 结构体既没有契约断言、也没登记为豁免：{:?}\n\
         请在 `test_response_dtos_are_snake_case` 里补一条 `keys_of` 断言；\
         若它确实不是 IPC 载荷（不经 invoke/事件出前端），把它加进 COVERED 并写明理由",
        missing
    );

    // 反向：清单里有、源码里已找不到的名字（更名或删除后忘了同步清单）
    let stale: Vec<&str> = COVERED.iter().copied().filter(|n| !found.iter().any(|f| f == n)).collect();
    assert!(stale.is_empty(), "覆盖清单里的这些名字在源码中已不存在（更名/删除了？）：{:?}", stale);
}
