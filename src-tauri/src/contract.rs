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
use crate::commands::fileops::{ConflictPolicy, PasteConflict, PasteFilesResult, PasteResponse};
use crate::commands::process::{
    ProcessStatus, RunningService, ToolCommandLogBatchPayload, ToolCommandLogLine,
    ToolCommandResult,
};
use crate::commands::search::{SearchParams, SearchResponse, SearchResultItem};
use crate::commands::node::{
    NodeRuntimeStatus, NodeVersionIndex, NodeVersionInfo, NvmCommandResult, NvmInstallResult,
};
use crate::commands::service::{
    AddServiceParams, CreateServiceTemplateParams, ImportTemplatesResult,
    UpdateServiceParams, UpdateServiceTemplateParams, UpdateServiceResult,
};
use crate::commands::tools::{OpenToolUsage, SaveOpenToolParams};
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
    // 可安装版本索引（面板上的搜索框筛的就是它；前端 NodeVersionIndex 原样对应）
    let info = NodeVersionInfo { version: String::new(), lts: String::new() };
    assert_eq!(keys_of(&info), sorted(&["version", "lts"]));
    let index = NodeVersionIndex { versions: vec![], index_url: String::new() };
    assert_eq!(keys_of(&index), sorted(&["versions", "index_url"]));
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

    // 粘贴结果（created/failed 决定界面提示哪一半成功；skipped 是"复制成功但内容不全"
    // 的那一类——被跳过的符号链接/联接点，界面必须单独说，不能并进任一边；
    // replaced 是覆盖时被移入回收站的原项，skipped_by_user 是用户在同名弹框里选「跳过」的）
    let paste = PasteFilesResult {
        created: vec![], failed: vec![], skipped: vec![], replaced: vec![], skipped_by_user: vec![],
    };
    assert_eq!(
        keys_of(&paste),
        sorted(&["created", "failed", "skipped", "replaced", "skipped_by_user"])
    );

    // 粘贴的两阶段响应：`status` 是判别式，前端靠它分流"弹框"还是"报结果"。
    // 改名（或丢掉 tag）不会编译报错，只会让界面把两种结局混成一种
    let done = PasteResponse::Done(paste);
    assert_eq!(
        keys_of(&done),
        sorted(&["status", "created", "failed", "skipped", "replaced", "skipped_by_user"])
    );
    let conflict = PasteResponse::Conflict {
        conflicts: vec![PasteConflict { name: "a.txt".into(), existing_is_dir: false }],
        sources: vec![],
    };
    assert_eq!(keys_of(&conflict), sorted(&["status", "conflicts", "sources"]));

    // 同名冲突项（弹框据此列出"目标里已有哪些同名项目"并说明替换代价）
    let pc = PasteConflict { name: "a.txt".into(), existing_is_dir: true };
    assert_eq!(keys_of(&pc), sorted(&["name", "existing_is_dir"]));

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

    // 删除工具前的引用清单（UX-4：删除入口是个 11px 图标，而它会级联解除所有服务的绑定）
    let usage = OpenToolUsage { services: vec![], templates: vec![] };
    assert_eq!(keys_of(&usage), sorted(&["services", "templates"]));

    // 更新服务的结果：保存本身成功，附带的"文件监听是否热刷新成功"（CQ-35 —— 保存成功
    // 而监听没生效时界面必须能说出来，所以它不能是一个只表示成败的 `void`）
    let upd = UpdateServiceResult { watch_refreshed: true, watch_error: String::new() };
    assert_eq!(keys_of(&upd), sorted(&["watch_refreshed", "watch_error"]));

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

    // ARCH-21：请求侧此前**一个字段断言都没有**（守卫的判据看不见只 derive Deserialize 的
    // 结构体）。这四个是服务/模板/工具三类编辑弹窗提交的载荷——字段名写错等于"某个设置
    // 改了不生效"：`rename_all` 丢了会直接报缺字段（响亮），但**字段名与前端对不上**时
    // 前端传的值会被静默忽略，界面照说"已保存"。
    let upd: UpdateServiceParams = serde_json::from_value(serde_json::json!({
        "id": "s1", "name": "svc", "command": "npm run dev", "cwd": "C:/p",
        "watchPaths": "[\"C:/p\"]", "watchInclude": "*.ts", "watchExclude": "dist",
        "envVars": "K=V", "restartMode": 2, "enabled": false, "showFileTree": true,
        "toolCommands": "[{\"id\":\"c1\"}]",
    })).expect("UpdateServiceParams 应能按驼峰反序列化");
    assert_eq!(upd.id, "s1");
    assert_eq!(upd.watch_paths, "[\"C:/p\"]", "watchPaths 被静默忽略");
    assert_eq!(upd.watch_include, "*.ts", "watchInclude 被静默忽略");
    assert_eq!(upd.restart_mode, 2, "restartMode 被静默忽略");
    assert!(!upd.enabled && upd.show_file_tree, "两个展示开关被静默忽略");
    assert_eq!(upd.tool_commands, "[{\"id\":\"c1\"}]", "toolCommands 被静默忽略");

    let tpl: UpdateServiceTemplateParams = serde_json::from_value(serde_json::json!({
        "id": "t1", "name": "tpl", "command": "c", "cwd": "C:/p",
        "watchPaths": "[]", "watchInclude": "*", "watchExclude": "", "envVars": "",
        "restartMode": 1, "enabled": true, "showFileTree": false,
        "toolCommands": "[]", "openToolId": "tool1",
    })).expect("UpdateServiceTemplateParams 应能按驼峰反序列化");
    assert_eq!(tpl.open_tool_id, "tool1", "openToolId 被静默忽略（模板的默认打开工具会丢）");

    let created: CreateServiceTemplateParams = serde_json::from_value(serde_json::json!({
        "name": "tpl", "command": "c", "cwd": "C:/p",
        "watchPaths": "[]", "watchInclude": "*", "watchExclude": "", "envVars": "",
        "restartMode": 1, "enabled": true, "showFileTree": false,
        "toolCommands": "[]", "openToolId": "",
    })).expect("CreateServiceTemplateParams 应能按驼峰反序列化");
    assert_eq!(created.name, "tpl");
    assert_eq!(created.restart_mode, 1, "restartMode 被静默忽略");

    // 工具：`id` 为 null 是"新建"语义（见 `save_open_tool`），必须能显式收到 null
    let tool: SaveOpenToolParams = serde_json::from_value(serde_json::json!({
        "id": null, "name": "IDEA", "executable": "C:/idea64.exe", "args": "{path}",
    })).expect("SaveOpenToolParams 应能按驼峰反序列化");
    assert!(tool.id.is_none(), "id: null 应读成「新建」");
    assert_eq!(tool.executable, "C:/idea64.exe", "executable 被静默忽略");

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

/// 同名冲突策略的**线上取值**：前端弹框三选一回传的就是这三个字符串。
///
/// 只测反序列化——**这才是运行时真正走的方向**（后端从不把策略发回前端）。
/// 这条路上没有类型检查帮忙：前端写的是字符串常量（见 `utils/pasteResult.ts` 的
/// `ConflictPolicy`），改一边不会编译报错，只会在用户点下按钮时抛一句 `unknown variant`，
/// 且**三个选项一起坏**：粘贴从此完全不可用。
#[test]
fn test_conflict_policy_wire_values() {
    use serde_json::json;
    assert_eq!(serde_json::from_value::<ConflictPolicy>(json!("rename")).unwrap(), ConflictPolicy::Rename);
    assert_eq!(serde_json::from_value::<ConflictPolicy>(json!("overwrite")).unwrap(), ConflictPolicy::Overwrite);
    assert_eq!(serde_json::from_value::<ConflictPolicy>(json!("skip")).unwrap(), ConflictPolicy::Skip);
    // 反面：取值拼错/改名必须**报错**，不能静默落回某个默认策略——
    // 落回 rename 还算轻（多一个 " (2)"），落回 overwrite 就是用户没选也覆盖
    assert!(
        serde_json::from_value::<ConflictPolicy>(json!("replace")).is_err(),
        "未知取值必须报错，而不是落回默认策略"
    );
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

/// derive 列表里有没有这一项。**按项比对而不是子串包含**：
/// `"Deserialize".contains("Serialize")` 是 false（大小写不同），而反向的子串匹配又会把
/// `serde::Serialize` 这类限定路径漏掉——两个方向都错，所以取 `::` 后的最后一段逐项比。
fn derives(list: &str, name: &str) -> bool {
    list.split(',')
        .any(|d| d.trim().rsplit("::").next() == Some(name))
}

/// 扫描结果：两个方向分开列。
///
/// 为什么要分（ARCH-21）：原扫描只认 `Serialize`，而请求 DTO 只 derive `Deserialize`——
/// 于是"新增一个请求 DTO"从来不会让守卫变红（判据见 `scan_ipc_structs` 内的说明）。
struct IpcStructs {
    /// 出前端的载荷（响应 DTO / 事件 payload）：字段名一律 snake_case
    serialize: Vec<String>,
    /// 前端传进来的参数（请求 DTO）：字段名一律 camelCase
    deserialize: Vec<String>,
}

/// 扫 `src/` 下「`#[derive(…Serialize…)` / `#[derive(…Deserialize…)` 紧跟的结构体」的名字
/// （跳过测试代码段）。启发式，且**故意宽松**：漏报（例如 derive 跨多行）只是退回今天的行为，
/// 不会造成损害；误报的代价是在豁免清单里加一行。它的目的是提醒，不是证明。
fn scan_ipc_structs() -> IpcStructs {
    fn walk(dir: &std::path::Path, out: &mut IpcStructs) {
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
            // **按 derive 里的类型名判断**（ARCH-21）：原判据是
            // `t.contains("Serialize")`，而 `"#[derive(Deserialize)]".contains("Serialize")`
            // 恰好是 **false**（Deserialize 里的 s 是小写）——于是所有只 derive Deserialize 的
            // **请求 DTO** 对这条守卫完全不可见，守卫给出了虚假保证："仓库里多了一个 DTO"
            // 会变成一次显式决定，实际只管得住响应侧。这里分开记两个方向。
            let (mut saw_serialize, mut saw_deserialize) = (false, false);
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
                    if let Some(rest) = t.split("derive(").nth(1) {
                        let list = rest.split(")").next().unwrap_or("");
                        saw_serialize = derives(list, "Serialize");
                        saw_deserialize = derives(list, "Deserialize");
                    }
                    continue; // 其余属性（#[serde(…)] 等）不影响判定
                }
                if let Some(name) = struct_name(t) {
                    if saw_serialize {
                        out.serialize.push(name.to_string());
                    }
                    if saw_deserialize {
                        out.deserialize.push(name.to_string());
                    }
                }
                saw_serialize = false;
                saw_deserialize = false;
            }
        }
    }
    let mut out = IpcStructs { serialize: Vec::new(), deserialize: Vec::new() };
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
///
/// 响应侧覆盖清单（与 `test_response_dtos_are_snake_case` 的断言一一对应）。
///
/// 提到模块级是为了让**请求侧**那条守卫也能看见它（ARCH-21）：同一批结构体（`Service`/
/// `Project`/…）两个方向都 derive，请求侧清单里不必重列一遍。
const COVERED_RESPONSE: &[&str] = &[
        "AiStatus", "DshVersionInfo",
        "FileEntry", "ReadFileResponse", "HexPage", "JarEntryContent",
        "PasteFilesResult", "PasteConflict",
        "ToolCommandResult", "ToolCommandLogLine", "ToolCommandLogBatchPayload",
        "RunningService", "ProcessStatus",
        "SearchResultItem", "SearchResponse",
        "FileChangeEvent", "FileChange", "JarEntryInfo",
        "LogLine", "FailedService", "ServiceLogBatchPayload",
        "Project", "ToolCommand", "OpenTool", "ServiceOpenToolBinding",
        "Service", "ServiceTemplate", "ProjectDetail",
        "ImportTemplatesResult", "UpdateServiceResult", "OpenToolUsage",
        "NodeRuntimeStatus", "NodeVersionInfo", "NodeVersionIndex",
        "NvmCommandResult", "NvmInstallResult",
        // 豁免：这两个是**模板导出文件的格式**，只在 export/import 命令内部读写磁盘，
        // 不经 invoke 出前端，因此没有 keys_of 断言。它们的字段用 camelCase 是为了
        // 文件给人看、可手工编辑——与"响应 DTO 一律 snake_case"不冲突（那条规则只管 IPC 载荷）
        "ExportedTemplate", "TemplateExportFile",
];

#[test]
fn test_every_serializable_struct_is_covered() {
    let found = scan_ipc_structs();
    assert!(
        found.serialize.len() >= 20,
        "扫描只找到 {} 个 Serialize 结构体，解析器很可能失效了（而非仓库里真这么少）",
        found.serialize.len()
    );

    let missing: Vec<&String> = found.serialize.iter().filter(|n| !COVERED_RESPONSE.contains(&n.as_str())).collect();
    assert!(
        missing.is_empty(),
        "以下 Serialize 结构体既没有契约断言、也没登记为豁免：{:?}\n\
         请在 `test_response_dtos_are_snake_case` 里补一条 `keys_of` 断言；\
         若它确实不是 IPC 载荷（不经 invoke/事件出前端），把它加进 COVERED_RESPONSE 并写明理由",
        missing
    );

    // 反向：清单里有、源码里已找不到的名字（更名或删除后忘了同步清单）
    let stale: Vec<&str> = COVERED_RESPONSE.iter().copied().filter(|n| !found.serialize.iter().any(|f| f == n)).collect();
    assert!(stale.is_empty(), "覆盖清单里的这些名字在源码中已不存在（更名/删除了？）：{:?}", stale);
}

/// ARCH-21：**请求侧**的同一套守卫。
///
/// 响应侧那条守卫此前把请求 DTO 整个漏掉了（判据是 `contains("Serialize")`，而
/// `Deserialize` 里那个 s 是小写），于是 `UpdateServiceParams` / `UpdateServiceTemplateParams` /
/// `CreateServiceTemplateParams` / `SaveOpenToolParams` 四个结构体**一个字段断言都没有**：
/// 把某个字段改名（或误删 `rename_all = "camelCase"`）不会红，前端传的字段被静默忽略，
/// 界面表现是"某个设置改了不生效"。这条把请求侧也变成一次显式决定。
#[test]
fn test_every_deserializable_struct_is_covered() {
    /// 只做**请求方向**的类型：有 `from_value` 断言（且断言到"值与默认不同"）
    const COVERED_REQUEST_ONLY: &[&str] = &[
        "SearchParams",                    // ARCH-12：三个 default 字段的静默回落
        "AddServiceParams",
        "UpdateServiceParams",
        "UpdateServiceTemplateParams",
        "CreateServiceTemplateParams",
        "SaveOpenToolParams",
        // 以下不是 IPC 请求载荷：
        // - `ConflictPolicy`：前端弹框回传的策略枚举（`#[serde(rename_all = "lowercase")]`，
        //   没有 rename_all = "camelCase" 这条规则的适用面），它的取值由弹框三选一固定
        // - `ToolCommand`：DB TEXT 列里的 JSON 的元素类型，前端把整串原样存取
        // - `Entry`（`commands/node.rs::parse_index` 内的局部结构）：反序列化的是**网上拉来的**
        //   nodejs.org 版本索引 JSON——外部文件格式，字段名由对方决定，我们无权改
        "ConflictPolicy", "ToolCommand", "Entry",
    ];

    let found = scan_ipc_structs();
    assert!(
        found.deserialize.len() >= 5,
        "只扫到 {} 个 Deserialize 结构体，解析器很可能失效了",
        found.deserialize.len()
    );

    // 两个方向都 derive 的类型（Service/Project/… 既出前端也进库）已由响应侧清单覆盖，
    // 这里不重复要求——它们的方向规则是"响应 snake_case"，断言在 `test_response_dtos_are_snake_case`
    let missing: Vec<&String> = found
        .deserialize
        .iter()
        .filter(|n| !COVERED_REQUEST_ONLY.contains(&n.as_str()) && !COVERED_RESPONSE.contains(&n.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "以下 Deserialize 结构体没有契约断言、也没登记为豁免：{:?}\n\
         请在 `test_request_params_are_camel_case` 里补一条 `from_value` 断言\
         （断言「读到了与默认值不同的值」，只断言字段存在不够）；\
         若它不是 IPC 请求载荷（例如库里的 JSON 列格式），把它加进 COVERED_REQUEST_ONLY 并写明理由",
        missing
    );
}
