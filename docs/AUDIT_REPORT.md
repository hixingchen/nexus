# Nexus 待处理问题清单

> **本文件只列仍需处理的项**（问题 + 新功能候选 + 处理顺序 + 待验证事项）。
> 审计背景、验证记录、架构现状分析与已修复清单不再单独留档，本文件不重复它们。
>
> **来源**：第三轮全量六维审计（基线 `b4006e1`）· 第四轮纯代码修复已落地（22 项关闭）
> **当前门禁**：`cargo test --lib` 111 passed · `cargo clippy` 0 警告 · `tsc --noEmit` 0 错误 · `vite build` exit 0
>
> **阅读约定**
> - 编号是**稳定标识，关闭后不回收**：正文出现跳号（如 SEC-2 之后直接是 SEC-4）即表示该项已修复。
>   （保留编号不复用，是为了让外部记录/提交信息里引用的编号长期有效。）
> - 每条标注 `[新增]`（第三轮首次发现）/ `[承前]`（更早轮次提出、复核仍开放）。
> - 结论一律带 `文件:行号`。无法从代码断言的项进 [附录 A](#附录-a-未验证与需实测)，不混入正文。
> - 标记 **【已实证】** = 代码/依赖源码可证明机制；**【需实测】** = 机制成立但量级待测量。
> - 权威清单见 [附录 B 待处理问题编号索引](#附录-b-待处理问题编号索引)（共 77 条：P0 2 · P1 30 · P2 45）。

---

## 一、总览

### 1.1 当前门禁状态

| 检查 | 命令 | 当前 |
|---|---|---|
| 前端类型检查 | `pnpm exec tsc --noEmit` | 0 错误 |
| 前端生产构建 | `pnpm exec vite build` | exit 0，2 条警告（[CQ-8](#cq-8-构建警告未被处理-新增)） |
| 后端单测 | `cargo test --lib` | 111 passed / 0 failed |
| 后端静态分析 | `cargo clippy --all-targets` | 0 错误 / 0 警告 |
| 命令注册一致性 | 对比 `commands/*.rs` 与 `lib.rs` | 59 声明 / 59 注册，无漂移 |
| 事件名一致性 | 对比 `emit` 与 `listen` | 3 类事件全部匹配 |
| 前端测试 | — | 0 个（[CQ-6](#cq-6-前端零测试-承前)） |

> **工程质量已达标**：clippy 零警告、单测全绿、`tsc` 零错误、命令注册无漂移、事件名前后端一致、构建 exit 0。
> 因此下面的问题集中在**界面确认交互、可达性、架构重构与工程化**——不是"代码乱"。
> 逐项修复记录不再单独留档；本文件只列仍需处理的项。

### 1.2 问题总览

> 计数按条目计；**权威清单见 [附录 B](#附录-b-待处理问题编号索引)**。
> 编号是稳定标识、关闭后不回收：正文出现跳号（如 SEC-2 之后直接是 SEC-4）即表示该项已修复，
> 编号不回收，以便后续与历史记录对照。

| 级别 | 未处理 | 内容 |
|---|---|---|
| **P0** | **2** | 数据丢失，均为"技术方案已定、缺界面确认交互" |
| **P1** | **30** | 安全边界、静默失败、破坏性操作确认、主路径阻塞、可达性、架构缺口 |
| **P2** | **45** | 重构、性能实测、工程化、一致性、响应式布局 |

### 1.3 优先处理的三件事

1. **[P0-2](#p0-2-关窗退出不拦未保存标签) / [P0-3](#p0-3-服务编辑面板切换即丢弃未保存编辑6-条触发路径) 两项数据丢失仍需界面决策**
   ——关窗拦截未保存标签、编辑面板 dirty 守卫。后端与状态层都已就绪，缺的只是"弹什么、文案怎么写"。
   **这是当前唯一的 P0 类别，也是仅剩的数据安全缺口。**
2. **[SEC-4](#sec-4-文件访问白名单可通过-ipc-自我放宽-新增已实证) 白名单边界可被 IPC 自我放宽**
   ——变换机制（canonicalize + 按分量比较）是正确的，但根集合由 IPC 可写字段（`add_service` 的 `cwd`、
   `set_project_root`）构成。收口需要"额外根只能由原生目录选择器确认后加入"，属策略变更而非纯代码修复。
3. **[ARCH-1](#55-架构级缺口按风险排序) `row_to_service` 映射器**
   ——加一个服务字段要改 29 处，其中 4 处是**位置化** `row.get(N)`：在 `SELECT` 中间插一列会让其后每个索引
   **静默读到错误的列**（类型正确、编译干净、测试全绿）。它是本项目最贵的结构性成本，也是后续任何服务新字段的前置。

---

## 二、安全检查

### 2.1 本轮新增发现

#### SEC-2 zip 校验用的是"不可信自述字段"这一模式值得单独记住

`SEC-1` 的教训不是"忘了校验"，而是**校验了攻击者可控的声明字段**——审计时极易被判为"已覆盖"。同类需复查的点：`core/ai.rs:224`（`find_project_sessions` 全量 `std::fs::read` 每个 dsh 会话文件，无大小上限，每次 `ai_start` 同步执行）、`:237`/`:251`（`bump_session_activity` 同时持 2× 文件大小）、`:659`（`raw_get` 对 localhost HTTP 响应 `read_to_end` 无上限）。

#### SEC-4 文件访问白名单可通过 IPC 自我放宽 `[新增]`【已实证】

**机制正确、边界不成立**——这是本轮最需要纠正的认知。

- `set_project_root`（`commands/editor.rs:118-134`）只校验 `p.is_dir()` → `C:/Windows` 被接受为项目根。
- `add_service`（`commands/service.rs:68`）`let cwd = params.cwd.replace('\\', "/");` → **无任何校验**（连 `is_dir` 都没有），随后 `:91-95` 原样入库；`update_service`（`:115`）、`update_service_template`（`:399`）、`add_service_from_template`（`:316-319`）同样。
- `allowed_roots`（`commands/editor.rs:176-191`）把这些字段**当作白名单根**：`SELECT cwd FROM services UNION SELECT cwd FROM service_templates`。
- 于是"项目根 + 各服务/模板 cwd"这一多根集合可被调用方自行扩大。

**影响**：白名单是 webview 与文件系统之间唯一的containment 边界，而 webview 能给自己授权任意根。串联链路：`add_service{cwd:"C:/Users/<me>"}` → `write_file` 写入 `~/.nexus/bin/cfr-0.152.jar` → 打开任意 `.class` → `java -jar` 执行（该环节的 CFR jar 现已做完整性校验）。`watch_paths` 同样可指向任意目录，再通过 `file-changed` 事件泄漏任意目录的文件路径（`commands/watcher.rs:151-158`）。

**修复**：把"额外根"放入 IPC **不可写**的集合（只能由原生目录选择器确认后加入）；在保存时校验 `cwd`/`watch_paths`（必须位于项目根或用户显式确认过的目录内）；拒绝驱动器根/系统目录作为项目根。

#### SEC-6 子 WebView 仅校验初始 URL，无导航守卫、端口未固定 `[新增]`

`commands/ai.rs:246-253` 的校验本身写得很严谨（`tauri::Url` 解析 + `host_str()` ∈ {localhost, 127.0.0.1, ::1} + scheme http/https，可挡 `localhost.evil.com` 与 `@` 用户信息段），但 `:261-271` 创建 WebView 时只有 `disable_drag_drop_handler()` 与 `initialization_script(...)`——**全 crate 无 `on_navigation` / `on_web_resource_request`**。
**影响**：加载后，环回页面（或其内的链接/重定向/JS 导航）可把面板导航到任意 URL——应用框架内的界面伪装/钓鱼；也能指向**任意其他环回端口**（未固定会话端口），例如另一个本地开发服务器。
**修复**：`.on_navigation(|url| host 为环回 && scheme 为 http/https)` + 固定会话端口。

### 2.2 承前轮仍开放

| 编号 | 问题 | 证据 | 收口要点 |
|---|---|---|---|
| **SEC-8** `[承前]` | 服务环境变量**明文**存库并在编辑面板明文展示 | 存：`database/mod.rs:96`；展示：`ServiceEditPanel.tsx:36` 起的 `envVars` textarea | 加"显示/遮蔽"切换；**不要**建凭据库（见 [7.7](#87-不建议做明确反对)） |
| **SEC-9** `[承前]` | TOCTOU：`canonicalize` 校验的是**字符串**，操作随后按路径重新打开 | `commands/editor.rs:137-153` 校验后，同一个原始字符串在 `:296`(读)、`:373-382`(写)、`:532-540`(列目录) 再次使用 | 先打开再校验句柄真实路径（`GetFinalPathNameByHandleW`），或拒绝重解析点 |

## 三、正确性与数据安全

> 本节放"输出是错的"或"数据会丢"的问题——优先级高于风格与性能。

### 3.1 P0：数据丢失（2 条，均需界面确认交互）

#### P0-2 关窗/退出不拦未保存标签

- `TitleBar.tsx:48`：关闭按钮直接 `appWindow.close()`。
- 全库**无** `onCloseRequested`、无 `beforeunload`（grep 确认）。
- `lib.rs:211-217` 的 `CloseRequested` 只做资源清理，**没有 `prevent_close`**。
- 草稿只活在内存里（`stores/editor.ts:150-155` 的模块级 `Map`），无任何落盘/崩溃恢复。

**语义不一致**：标签级关闭**是有确认的**（`EditorTabs.tsx:306-323` 的"未保存的更改"弹窗），关窗/Alt+F4/系统关闭没有——同一应用两条路径两种语义，用户会误以为草稿安全。
**同一缺口还有一个次要风险**：WebView2 的浏览器加速键仍然生效——代码专门吞掉了 Ctrl+F 以阻止原生查找框（`CodeViewer.tsx:888-900`），但没有任何东西拦截 Ctrl+R/F5/F12，一次习惯性的刷新就会丢掉全部草稿与未保存表单（**加速键默认行为未在运行时验证**，故记为证据支持的风险而非确定结论，见 [附录 C](#附录-a-未验证与需实测)）。
**收口**：注册 `getCurrentWindow().onCloseRequested`，未保存标签或运行中服务 > 0 时 `preventDefault` 并弹确认（文案给数量："未保存文件 N 个 / 将停止 M 个服务"）；标题栏 × 与系统关闭走同一路径。可借这次改动一并解决 [PERF-4](#perf-4-关窗清理仍在主线程同步执行-承前) 的阻塞。

#### P0-3 服务编辑面板切换即丢弃未保存编辑（6 条触发路径）

- **13 个表单 state 全部从 prop 初始化，无 dirty 追踪**：`ServiceEditPanel.tsx:30-71`。
- **组件内唯一的 `useEffect`（`:75-93`）只处理工具选择器的外部点击关闭**，没有 prop 同步、没有 dirty 守卫。
- **挂载点带 key → 切换编辑对象即卸载重挂载**：`ProjectDetail.tsx:253` `key={editingService.id}`、`:264` `key={tpl-${editingTemplate.id}}`。
- **切项目也会丢**：`useProjectDetail.ts:66` 在 `load` 变化时 `setEditingService(null)`。

**六条静默丢失路径**：① 点另一张服务卡（`ProjectDetail.tsx:191-194`）；② 再点同一张卡（toggle 收起，`:193`）；③ 点"日志"（`:308-312`）；④ 收起服务列（`:285`）；⑤ 服务↔模板编辑互切（`:191-199`）；⑥ 切项目。
**收口**：面板自持 dirty（各字段与原值比较），所有外部切换入口统一走 `requestClose()` 并在 dirty 时确认；或把草稿提升到 `useProjectDetail` 的按 id Map 以便切回恢复。

### 3.2 阻塞与资源（机制已实证，量级部分需实测）

#### NEW-9 Job Object 覆盖仍缺一处 `[新增]`【部分已修复】

**已修复**：`dsh --version` 现在走 `run_captured`（自带 10s 超时 + Job 绑定）。

**仍缺**：**反编译 JVM**（`core/decompiler.rs:72-83`）只有 `kill_on_drop(true)`——而 tokio 在 drop 时**不会** reap，应用被强杀/崩溃时该 JVM 可残留，违反项目自己写下的"所有子进程必须加入 Job Object"规则。
**为什么本轮没做**：`decompile_class_bytes` 需要把 `tokio::process::Child` 的原始句柄穿过 `#[cfg(windows)]` 的类型边界（`JobObject` 是平台门控类型），收益仅限"应用异常退出时残留一个 JVM"。已判定不值得为该 P2 引入整套 cfg 管线；若要做，建议顺带把 `assign_child` 泛化为接受原始句柄。

> **有意为之、不要"修"的**：`open_terminal` 的 cmd（`editor.rs:23`）、`explorer.exe`（`:63`）、分离启动的外部工具（`tools.rs:177-248`，`:175-176` 有说明）、短命的 `taskkill`（`core/process.rs:777`）。

#### PERF-4 关窗清理仍在主线程同步执行 `[承前]`

`lib.rs:210-218`：`WindowEvent::CloseRequested` 里直接调 `cleanup_resources`（`:31-50`）→ `stop_all()` → **串行**对每个进程 `cleanup_process`（最坏 2s + 2×1s）。N 个服务最坏 ≈ N×4s，期间 Windows 会显示"未响应"。典型路径约 100ms/服务，故属最坏情形。
**修复**：与 [P0-2](#p0-2-关窗退出不拦未保存标签) 同一处时序，一起做——先 `prevent_close` →（有未保存则确认）→ 隐藏窗口 → worker 线程清理（有总时限）→ 退出。

#### NEW-14 jar 读取每次调用都把整包复制至少两遍 `[新增]`

`core/jarfile.rs:49-50` `read_entry` 开头即 `let mut current: Vec<u8> = jar_bytes.to_vec();`（即使 `nested` 为空也整包复制）；`:77-83` `innermost_archive` 在扁平情形又 `to_vec()`。`commands/editor.rs:478-480` `list_jar` = 全量读（≤100 MB）→ 整包复制 → 新建 ZipArchive；`:501-502` `read_jar_entry` 再来一遍。每层嵌套再加一个内层解压缓冲（`:53-65`，每层 ≤50 MB），二进制条目再 base64（+33%）。
**影响**：列一次条目峰值 ≈ 2× jar 大小（60 MB fat jar ≈ 120 MB），下钻嵌套时 ≈3×；而且**每次点开一个条目都从磁盘重读重解析整个 jar**。
**修复**：按 (path, mtime, size) 缓存一个 `Vec<u8>` + 已解析条目表；解析直接用读缓冲、免掉首次 `to_vec()`；条目字节不再 base64。

#### NEW-15 服务日志热路径：每行一次时间戳、多次分配、一次 IPC 事件、Rust 侧无批量 `[新增]`

`core/process.rs:405-445`(stdout) / `:466-504`(stderr) —— 每行都做：`chrono::Utc::now().to_rfc3339()`（分配+格式化）、`clean_ansi`（新 `String`）、`truncate_line`、`log_buffers.lock()`（**所有服务共享的全局锁**）、`line.clone()`、然后**逐行** `app_clone.emit("service-log", ...)`。
前端已批量（`MainLayout.tsx:40-52` 的 50ms 刷新）保护了 React，但 **IPC 边界成本是逐行的**。
**修复**：reader 线程内累积 ~50ms 批量 emit（与前端批量的思路一致）；时间戳按批复用；用 `Arc<str>` 避免双重 clone。

### 3.3 跨项目切换后无法保存文件 `[新增]`

`MainLayout.tsx:118-120` 切换项目时改写**单一**项目根（`commands/editor.rs:117-134`），而白名单是"当前项目根 + DB 里所有服务/模板 cwd"（`:160-198`）。切换后：服务目录内的文件仍可保存（cwd 保留了），但**项目根下的文件**（README、package.json、.gitignore、docs/）不再可写，而标签仍打开着、仍带 dirty 点。
**症状**：打开仓库根目录的 README → 切到另一个项目对比 → 编辑 → Ctrl+S → **"保存文件失败 / 访问被拒绝"**，与权限故障无法区分。
**修复**：把"仍有打开标签的项目根"也纳入允许根；并让错误信息说明真实原因（见 [UX-12](#ux-12-错误文案不可行动-新增)）。

---

## 四、代码质量

### 4.1 量化基线（本轮实测）

| 维度 | 数值 |
|---|---|
| 前端 | **55 文件 / 10,517 行**（`src/**/*.{ts,tsx}`） |
| 后端 | **24 文件 / 8,686 行**（`src-tauri/src/**/*.rs`，含测试） |
| Tauri 命令 | 59 声明 / 59 注册（无漂移）；**26 async / 33 sync** |
| 后端测试 / 前端测试 | **98** / **0** |
| `console.log` / `any` / `@ts-ignore` | **0 / 0 / 0**（标准硬指标达成） |
| `as` 断言 / 非空 `!` / `eslint-disable` | 17 / 5 / 7（**7 处全部无效**，见 [CQ-3](#cq-3-7-处-eslint-disable-是无效注释-承前)） |
| `invoke()` 总数 / 其中绕过 `services/` | 68 / **16（6 个文件）** |
| 组件 >400 行 | 5 个 |
| 顶层声明 >30 行 | 57 个 |
| 后端函数 >30 行 / 嵌套 >3 层 | 37 / 59 |
| 数据库 | 7 表 / 47 列 / 3 显式索引 |
| 事件通道 | 3 类（`service-log`/`tool-command-log`/`file-changed`），前后端一致 |

> **测量方法警告**：本环境下用 PowerShell `Get-Content`/`Select-String` 读取这些 UTF-8 文件会按 ANSI(GBK) 解码并**静默丢行**（例如 `SearchResultPanel.tsx` 实际 319 行，`Get-Content` 报 307）。本轮第一版统计因此偏低约 9%（前端 9,614→10,517、后端 7,790→8,686）。上表数字用 `[System.IO.File]::ReadAllLines(path, UTF8)` 重算。**后续审计请勿用 `Get-Content` 统计行数。**

### 4.2 本轮发现

#### CQ-1 文件体量超标（含生产/测试拆分）`[承前]`

标准为"组件 ≤400 行 / 模块 ≤500 行"。剔除 `#[cfg(test)]` 之后的测试行，以反映真实维护负担：

| 文件 | 总行 | 生产行 | 测试行 | 判定 |
|---|---|---|---|---|
| `src/components/editor/CodeViewer.tsx` | 984 | 984 | 0 | 超标 ×2.5 |
| `src/components/layout/ProjectDetail.tsx` | 813 | 813 | 0 | 超标 ×2.0 |
| `src/components/ai/AiPanel.tsx` | 733 | 733 | 0 | 超标 ×1.8 |
| `src/components/layout/ServiceEditPanel.tsx` | 645 | 645 | 0 | 超标 ×1.6 |
| `src/components/file-tree/FileTree.tsx` | 614 | 614 | 0 | 超标 ×1.5 |
| `src/stores/editor.ts` | 563 | 563 | 0 | 模块超标 |
| `src-tauri/src/core/classfile.rs` | 1,575 | **1,329** | 246 | 超标 ×2.7 |
| `src-tauri/src/core/process.rs` | 1,097 | **~940** | ~155 | 超标 ×1.9 |
| `src-tauri/src/core/ai.rs` | 952 | **~730** | ~224 | 超标 ×1.5 |
| `src-tauri/src/commands/editor.rs` | 643 | **576** | 67 | 略超 |
| `src-tauri/src/core/file_watcher.rs` | 559 | **381** | 178 | 生产部分**达标**，不必动 |

**最大函数**（实际跨度）：`opcode_info`（`classfile.rs:657-867`，**211 行**）、`ProcessManager::start`（`process.rs:291-492`，**202**）、`run()`（`lib.rs:93-249`，**157**）、`run_tool_command`（`commands/process.rs:224-375`，**152**）、`ServiceEditPanel`（`ServiceEditPanel.tsx:29-548`，**520**）、`AiPanel`（`AiPanel.tsx:117-608`，**492**）、`init_schema`（`database/mod.rs:56-163`，**108**）。

> `file_watcher.rs` 是"测试占比高所以总行数大"的典型——不算问题。剔除测试后，前 5 个前端组件与 `classfile.rs` 是真的大。

#### CQ-2 `ProjectDetail.tsx` 内部重复 `[新增]`

813 行里塞了 **7 个组件**（`ProjectDetail:43`、`EmptyState:385`、`ServicePanel:420`、`CollapsedView:468`、`ExpandedView:568`、`ServiceSection:626`、`TemplateSection:760`），并有明显复制粘贴：

- AI 面板可见性三行逻辑重复两遍：`:481/:483/:484`（`CollapsedView`）与 `:635/:636/:637`（`ServiceSection`）—— `aiRunning`/`aiPanelVisible`/`toggleAi` 逐字相同。
- 同一句注释重复三遍：`：404`、`:551`、`:609`。
- **两套 dnd-kit 拖拽实现**：`:640-747`（服务）与 `:763-...`（模板），结构高度相似。
- **prop 链过深**：`ServicePanel`(15 props) → `ExpandedView`(15) → `ServiceSection`(**15，其中 13 个是纯透传**) → `ServiceTreeEntry`。这是"服务列状态没有 store"的机械代价。

**建议拆分**：`ProjectDetailShell`（壳+视图路由+互斥）+ `ServiceColumn`（直接读 `runningStore`/`svcCacheStore`，不再透传 13 个 prop）+ `TemplateLibraryColumn` + `EditingPanelHost`；把工具命令流状态上移 store（与 [7.x](#83-日志与搜索) 的日志导出/过滤同批）。

#### CQ-3 7 处 `eslint-disable` 是无效注释 `[承前]`

`AiPanel.tsx:205,286,313`、`FileTree.tsx:190,201`、`LogViewer.tsx:128,142` 共 7 处 `// eslint-disable-next-line react-hooks/exhaustive-deps`。但全项目**没有 eslint 配置**（无 `.eslintrc*`/`eslint.config.*`，`node_modules` 里无 eslint），项目文档里写的 `pnpm eslint src/ --ext .ts,.tsx` **命令不可执行**。
**影响**：① 项目审查流程里"自动化那一半"是虚构的；② 这 7 处豁免从来没被机器校验过，而它们实际是"跳过依赖数组的四种写法"——依赖遗漏正是本项目历史上出过 bug 的区域。其中 `FileTree.tsx:195-202` 是唯一**依赖省略方式很脆弱**的一处（effect 体内读 `reveal`，却只依赖 `revealSeq`；正确性依赖 `requestReveal` 总是同时递增两者，`stores/editor.ts:324-326`）。
**修复**：接 eslint + `eslint-plugin-react-hooks`（`react-hooks/exhaustive-deps: 'warn'`），然后重新审这 7 处。

#### CQ-4 类型断言中的真实隐患 `[新增]`

17 处 `as` 里多数无害（10 处 `e.target as Node`——`Node.contains(null)` 返回 `false`，语义正确）。需处理的：

| 位置 | 问题 |
|---|---|
| `ServiceTreeEntry.tsx:54` | `JSON.parse(service.tool_commands \|\| '[]') as ToolCommand[]` —— **DB→JSON→UI 是真实的不受信数据跨越边界**。`{"a":1}` 或 `[1,2]` 会产出 `id`/`name`/`command` 全为 `undefined` 的项，右键菜单渲染无名条目，运行它会把 `commandId: undefined` 送进 `run_tool_command` |
| `ServiceEditPanel.tsx:47` | `JSON.parse(...)` 无断言、直接流进 `useState<ToolCommand[]>` —— 隐式 `any` 泄漏（grep `any` 找不到它） |
| `ProjectList.tsx:74` | `{ stopPropagation: () => {} } as React.MouseEvent` —— **伪造 DOM 事件**满足签名（`:110-117` 只用了 `stopPropagation`）。任何未来使用其他成员（如 `currentTarget`）都会在无编译期帮助的调用点抛 `TypeError` |
| `CodeViewer.tsx:530` | `(base.language as LRLanguage).configure(...)` —— `LanguageSupport.language` 类型是 `Language`，`.configure()` 只存在于 `LRLanguage`。当前只因 `lang-vue@0.1.3` 恰好基于 `LRLanguage` 才成立，**升级即 `TypeError`** |
| `TitleBar.tsx:20` | `(e.target as HTMLElement).closest(...)` —— `target` 可能是文本节点/`document`，此时 `.closest` 为 `undefined` |
| `CodeViewer.tsx:474`、`LogViewer.tsx:317` | 冗余断言，可直接删 |

**正确范式就在同项目**：`ServiceEditPanel.tsx:132` `JSON.parse(trimmed) as unknown` + `Array.isArray` 收窄。

#### CQ-5 重复代码（每项 ≥3 次，含确切位置）`[新增]`

| # | 模式 | 次数 | 位置 | 建议 |
|---|---|---|---|---|
| D1 | "外部 mousedown 关闭" effect（各约 8 行） | **9** | `FileTree.tsx:280,527`、`ProjectList.tsx:36`、`ProjectListItem.tsx:43`、`EditorTabs.tsx:69`、`ServiceTreeEntry.tsx:248`、`TemplateTreeEntry.tsx:139`、`TitleBar.tsx:62`、`ServiceEditPanel.tsx:75`（含 scroll/Escape 变体） | `hooks/useClickOutside(ref, onClose, {escape?, captureScroll?})`，省约 70 行 |
| D2 | `open_in_explorer`/`open_terminal` 的 invoke+错误处理 | **8** | `FileTree.tsx:205`、`ProjectList.tsx:84,93`、`ProjectListItem.tsx:216`、`ServiceTreeEntry.tsx:196,205`、`TemplateTreeEntry.tsx:51,61` | `services/system.ts` 两个封装（同时解决 16 处越层调用的一半） |
| D3 | 右键菜单项 markup（按钮类 22 处 + 图标盒 13 处） | **35** | `FileTree.tsx:340,361,378,392,412,425,438`、`ProjectList.tsx:214,226,238,251,266`、`ProjectListItem.tsx:199,215,233,252`、`ServiceTreeEntry.tsx:280,295,304,321,336`、`TemplateTreeEntry.tsx:171,180,194` | `<ContextMenu>/<ContextMenuItem>` 组件；5 个菜单各从 60~80 行降到约 15 行 |
| D4 | 确认弹窗 JSX（18 行/处） | **6** | `ProjectDetail.tsx:328,349`、`ProjectModals.tsx:218,266`、`EditorTabs.tsx:306,326` | `<ConfirmDialog>`，省约 90 行 |
| D5 | 输入框 class 串 | 3 处逐字/近似相同 + 14 个同类输入 | `ServiceEditPanel.tsx:207` 与 `:587` **逐字相同**；`ToolsManagerModal.tsx:87` 少个 `mt-1` | `<TextField>/<TextArea>` 或 `ui/styles.ts` |
| D6 | 扩展名解析 | **6** | `stores/editor.ts:78`、`FileTree.tsx:14,65`、`CodeViewer.tsx:500,675`、`ImageViewer.tsx:54`、`EditorTabs.tsx:143`（**忘了 `.toLowerCase()`** → `.PNG` 标签无图标） | `utils/path.ts: getExtension()`，顺带修掉那个 bug |
| D7 | `console.error('<操作>失败:', e)` + `showNotification` 成对 | **约 20** | `FileTree.tsx:122,164,209,249,262`、`ProjectList.tsx:87,96`、`ProjectListItem.tsx:221`、`ServiceTreeEntry.tsx:102,200,208`、`TemplateTreeEntry.tsx:55,64`、`ToolsManagerModal.tsx:58,81`、`ServiceEditPanel.tsx:115,160,174`、`AddServiceFormContent.tsx:37`、`RestartConfirm.tsx:43,78`、`ProjectDetail.tsx:136` | `utils/errors.ts: reportError(op, e, {silent?})`，省约 40 行，并机械修掉 [UX-12](#ux-12-错误文案不可行动-新增)/[UX-14](#ux-14-控制台日志缺操作上下文-新增) |
| D8 | 复制路径/文件名的 handler | **4** | `FileTree.tsx:216,227`、`ProjectListItem.tsx:232,251`（四处的失败提示已在第四轮补齐，见历史留档；此处保留的是**重复实现**本身） | `utils/clipboard.ts: copyText(text, label)` |
| D9 | 后端 `emit("file-changed")` 闭包 / `SELECT name FROM projects` / stdout-stderr reader 孪生 | 3 / 3 / 2+1 | `commands/watcher.rs:155,183,223`；`:116,174,214`；`core/process.rs:399-452` vs `:461-511` vs `commands/process.rs:310-341` | 抽 `emit_file_changed`、`load_project_name`、`spawn_log_reader` |
| D10 | 白名单检查的注释+调用被复制 | **10** | `commands/editor.rs:296,419,439,455,464,477,500,533`（8 处是逐字相同的行内副本） | 统一到一个 `guard_path!` 宏或直接接受（属可读性取舍） |

#### CQ-6 前端零测试 `[承前]`

后端 111 个测试覆盖了 classfile 边界、日志行读取、环境变量解析、掩码、路径白名单、行尾检测、复制深度、dsh URL 解析等——质量高。前端**一个都没有**，而历史上出过 bug 的地方恰好都在前端状态机：内容晚到不同步、dirty 基线污染、EOL 归一化（这些缺陷已在更早轮次修掉，但没有回归测试）。第二轮修掉的这 13 个前端缺陷**没有任何回归保护**。

**更值得注意的后端空白**：`cargo test --lib` 里**没有一个测试触及锁、线程、子进程、真实 DB 文件或监听循环**。`ProcessManager::start/stop/restart/running/stop_all`（`core/process.rs:318-680`，项目里风险最高的代码）**零测试**——只有 `kill_process_tree(999999999)`。原因是 `AppHandle` 被硬编码进 `ServiceSpawn`（`:292`），使这些函数无法脱离 Tauri 运行。

**优先补的测试**（按性价比）：① `stores/editor.ts` 的 dirty/基线状态机；② `logFormatter.ts` 的转义与占位符恢复；③ `logStore.ts` 的 2000 行/2MB 裁剪与 `\r` 合并；④ `check_write_path_allowed`（**写路径零测试**）；⑤ `allowed_roots`（空根=拒绝、服务 cwd 加入、DB 错误回退）；⑥ `ProcessManager` 生命周期（对 `cmd /C timeout` 之类真实子进程）。

> 含分支指令的字节码 fixture 已在第四轮补上（`class_with_code` + 2 个测试）——这也是"测试数量≠行为空间覆盖"的具体标本。

#### CQ-7 工程化缺口（本轮可直接落地）`[承前]`

| 缺什么 | 现状 |
|---|---|
| CI | 无 `.github/`、无任何流水线 |
| eslint / prettier / rustfmt / editorconfig | 全缺 |
| `cargo audit` / `pnpm audit` | 未接入（本机未装 `cargo audit`） |

> **可直接落地**：`cargo clippy --all-targets` 实测 **0 警告**，故 `cargo clippy --all-targets -- -D warnings` 现在就能当门禁。加上 `cargo test --lib`（**111 绿**）与 `tsc --noEmit`（0 错），三条命令即最小 CI。

#### CQ-8 构建警告未被处理 `[新增]`

`pnpm exec vite build` 成功（exit 0），但报两条警告：

1. **单 chunk > 500 kB** —— 实测 **1,600.25 kB**（gzip 538.75 kB）。见 [PERF-2](#perf-2-无代码分割单-chunk-约-16-mb-新增已实证)。
2. **`@tauri-apps/plugin-dialog` 被动态与静态同时导入** —— 动态导入不会分包、完全无效：动态在 `ProjectModals.tsx:27,122`，静态在 `AddServiceFormContent.tsx:3`、`ServiceEditPanel.tsx:4`、`ToolsManagerModal.tsx:2`。

#### CQ-9 死代码 / 永不触发的逻辑 `[新增]`

- **`ai_status` 全链路无人调用**：命令已注册（`lib.rs:202`），service 层已封装（`services/aiService.ts:16`），但 `status()` 在整个 `src/` **零调用点**。后端有能力做真实存活探测（`core/ai.rs:288` 的 `alive()`），前端从不问。**接线成本很小，建议提前做**（原 [8.4 AI 助手](#84-ai-助手) 的首条）。
- **由此"AI 崩溃自愈"是死逻辑**：`AiPanel.tsx:216-221` 的守卫要求 `!runningHere && !url`，而 `running`/`url` 只在 `aiStore.start()` 的返回里被写——进程被杀后没有任何东西把这两字段改回假，条件**永不成立**。注释声称覆盖的"dsh 崩溃/被杀后无项目切换动作的场景"恰好是它检测不到的场景。
- **8 个类型导出从未被 import**：`aiService.ts:4`、`editor.ts:4,51,71,111`、`service.ts:234,273`、`types/editor.ts:2`。
- **死 prop**：`LogViewer.tsx:10 maxHeight`（`ProjectDetail.tsx:220-225` 从不传）、`ErrorBoundary.tsx:5 fallback`（唯一使用点不传）、`useContextMenuPosition.ts:19 gap`（5 个调用点都不传）。
- **未使用的依赖**：`@lezer/highlight`（`package.json:42`，全项目零 import，只作 `@codemirror/language` 的传递依赖）；`tauri-plugin-clipboard-manager`（Rust 侧初始化于 `lib.rs:140` 但从未调用，剪贴板走 `clipboard-win`）；`tauri` 的 `unstable` feature（`Cargo.toml:17`，未见使用）。清理需 `pnpm remove` + 改 `Cargo.toml`。
- `src-tauri/test_escape.pdb`：遗留构建产物（未跟踪、已被 gitignore）。

> 注：`activeEditorView` / `sameStatus` 的多余 `export` 已在第四轮修掉，不再列入。

## 五、架构合理性（待处理缺口）

> 说明：原先的架构现状分析（分层依赖图、状态归属、契约同步）已随审计手稿移除；
> 下面只留**需要动手的两节**，缺依据处已在条目内补一行证据。

### 5.4 扩展成本（架构摩擦量化）

| 需求 | 需要改的地方 | 评价 |
|---|---|---|
| **新增一个后端命令** | `commands/x.rs` 写函数 → `lib.rs:149-209` 注册一行 → `services/*.ts` 加封装 → 组件调用 | **4 处。这是本项目最健康的一个轴——命令新增确实便宜。**（应用自定义命令不受 ACL 门控，无需改 `capabilities`） |
| **给服务加一个字段** | `数据库 CREATE` + `MIGRATION_COLUMNS`（必配成对，`database/mod.rs:150-151` 有注释）+ `query_services_by_project` 的 SELECT + 位置化 `row.get(N)` + `models.rs` 两个结构体 + `commands/service.rs` 的 `UpdateServiceParams`/`AddServiceParams`/INSERT/UPDATE/返回字面量 + 模板路径的 SELECT/INSERT/返回 + `project.rs` 的 `duplicate_project` INSERT + 前端 2 个接口 + 3 个 payload + `ServiceEditPanel` 表单 + `AddServiceFormContent` + `ServiceTreeEntry` 展示 | **约 29 处（21 个 Rust 点 + 8 个 TS 点）** |
| 新增一个面板 | 组件文件 + `MainLayout.tsx:170-184` 挂载点 +（有几何时）新的 layout 键 | 约 3 处 + 布局持久化样板（`MainLayout.tsx:83-113`、`ProjectDetail.tsx:79-94`、`aiStore.ts:117-136` 是**三份独立实现**的"load/save 我的 layout 键 + 500ms 防抖"） |

**最关键的一点**：上述 29 处里，**4 处是位置化的 `row.get(N)` 索引**（`database/mod.rs:205-214`、`commands/service.rs:96-103,270-275,333-336`）。**在 `SELECT` 中间插入一列会让其后每个 `row.get(N)` 静默读到错误的列**——类型正确、编译干净、测试全绿、无声的数据损坏。47 列的表上，这是下一个服务字段特性面前的**上膛的枪**。
**修复顺序很重要**：先抽一个 `row_to_service(row)` 映射器 + 共享 `SELECT` 常量，再加字段——那时工作量从 29 处降到约 6 处。**反过来做就要先付 29 处的税，而且下一个字段再付一次。**

### 5.5 架构级缺口（按风险排序）

| 编号 | 缺口 | 现状 | 建议 |
|---|---|---|---|
| **ARCH-1** `[新增]` | **位置化 SQL 行映射**（见 5.4） | 加字段 29 处，其中 4 处索引会静默错位 | 抽 `row_to_service` + 共享 SELECT 常量。**这是阻止未来静默损坏的第一优先重构** |
| **ARCH-2** `[新增]` | **单个 SQLite 连接 + 全局 Mutex** | `database/mod.rs:18` `conn: Mutex<Connection>`，`with_conn`（`:29-37`）闭包全程持锁 → 所有 DB 访问串行 | 单用户下**不是**瓶颈；但历史上路径校验就是"每次文件操作都查库"（[PERF-1](#cq-1-文件体量超标含生产测试拆分承前) 已加缓存），这类用法会让它变成热锁。修调用方式，别换存储 |
| **ARCH-3** `[承前]` | 运行态纯内存，重启失忆 | 7 表无运行态表 | 加 `service_runs(service_id, started_at, ended_at, exit_code, trigger)`——一并解决崩溃历史、运行时长、启动耗时、重启恢复。**必须配保留策略**（运行历史是高容量数据） |
| **ARCH-4** `[承前]` | 事件通道仅 3 类，状态靠 3 秒轮询 | 无 `service-lifecycle`（started/stopped/failed） | 补生命周期事件（从既有 `running()` 路径 emit 很便宜），轮询降为兜底。这是 [UX-21](#ux-21-崩溃退出码与时间不可见-承前) 与系统通知的数据来源 |
| **ARCH-5** `[承前]` | 契约无同步机制 | Rust↔TS 类型手工双重声明，无 codegen、无契约测试（`read_file` 的 `modified` 就曾被漏掉） | 推广 `read_file` 的 raw+映射范式，或引入 `ts-rs` |
| **ARCH-6** `[承前]` | AI 单会话占用制（三重锁死） | `Mutex<Option<AiSession>>`（`lib.rs:22`）+ **单个全局** `ai_epoch`（`:24`，非按会话）+ 前端 `occupyAiOn` 互斥（`aiStore.ts:55-60`）；`AiSession` 无会话 id 字段 | **不建议**做多会话（见 [7.7](#87-不建议做明确反对)）；短期在 UI 显式提示"切项目会中断 AI"，并给"在浏览器新标签打开"（`openUrl` 已用于 `AiPanel.tsx:415`）——用 5% 成本拿到大部分价值 |
| **ARCH-7** `[承前]` | 服务可信度不足 | `spawn` 成功即算"已启动"；无就绪探测、无依赖顺序、无崩溃自动重启。启动顺序是 `ORDER BY sort_index`（`commands/process.rs:140`）且**不等就绪**（`:153-166`） | 见 [7.2](#82-服务与进程可信度) |
| **ARCH-8** `[新增]` | 硬编码假设会挡住可预见特性 | 单窗口（`tauri.conf.json:13-27` + `lib.rs:130`/`commands/ai.rs:233` 硬编码 `"main"`）；Windows-only 且**部分无 cfg 回退**（`core/ai.rs:31` 读 `USERPROFILE` + 字面 `.dsh`，macOS/Linux 下除非设 `DSH_HOME` 否则返回 `None`，**而 `dirs::home_dir()` 已是依赖**（`database/mod.rs:8` 在用）→ 这是非受迫的可移植性 bug）；20 个硬编码超时（均为命名常量，但无一可从配置读取） | 至少修 `core/ai.rs:31` 的 `USERPROFILE` 硬编码；超时可后续按需开放 |

---

## 六、性能优化

> 项目规则是"先测量再优化"。本节把结论分成 **【已实证】**（代码/依赖源码即可证明机制）与 **【需实测】**（机制成立、量级待测）。

### 6.1 已实证

#### PERF-6 每次按键的全量文档处理（3 次物化 + 全量正则 + 整树重渲染）`[承前，本轮大幅加深]`【已实证】

| # | 位置 | 每按键成本 |
|---|---|---|
| 1 | `CodeViewer.tsx:766-768` `if (update.docChanged) onChange(update.state.doc.toString())` | **全 rope 扁平化**一次 |
| 2 | `stores/editor.ts:281` `set({ fileContent: content })` + `:284` `normalizeEOL(content)` + `:285` 与基线**全量比较** | 又一次全量拷贝 + 一次全量正则 + 一次全量 memcmp |
| 3 | `CodeViewer.tsx:873-884` 的 effect 依赖 `[content, ...]` → 每次按键都跑，内部 `const doc = view.state.doc.toString();` 后**才**执行便宜的 `lastEmittedRef` 守卫 | **第三次**全量字符串 + 一次完整比较（**守卫顺序反了**：便宜判断在贵操作之后） |
| 4 | `stores/editor.ts:284` 的 `set` 触发 `useProjectDetail.ts:37` 的 `fileContent` 订阅 → `ProjectDetail.tsx:209-380` 整棵树重渲染（该子树**无任何 memo**） | 每按键重渲染：标签栏 + CodeMirror + N 个服务卡（各跑 `useSortable`）+ M 个模板卡 + 2 套 dnd-kit context + 日志面板 + 搜索面板。折叠态下服务行还会渲染两遍（`:431-463`） |

**规模**：5 MB 文件 ≈ 每按键 15 MB 分配/拷贝；10 MB（可编辑上限，`editor.ts:67`）≈ 30 MB/按键。
**修复**：① 把 `if (content === lastEmittedRef.current) return;` 提到 effect 首行；② dirty 判定改为"长度 + 哈希"或只在保存/blur/切标签时与基线比较；③ `fileContent` 改用 ref + 150~250ms 防抖发布；④ 把 `fileContent` 订阅从 `useProjectDetail`/`ProjectDetail` 下移到 `CodeViewer` 自身，并 memo 化右栏。

#### PERF-7 CSS/HTML/Vue 的颜色色块装饰：每次文档变更做全文档正则 `[新增]`【已实证】

`CodeViewer.tsx:261-272` 的 `StateField.update` 在 `tr.docChanged` 时调 `buildSwatchDeco(tr.state.doc, ...)`，而它开头就是 `const text = doc.toString();`（`:242-258`），然后跑 `COLOR_RE`（`:212`）——该正则的第三个分支 `\b[a-zA-Z]{2,20}(?=[\s;})]|$)` 会匹配**任何 2~20 个字母的单词**。
**触发**：在 `.html/.vue/.scss/.css` 里敲任意字符——HTML 文本节点与属性值里几乎每个词都是候选，每个匹配还要 `trim()` + `toLowerCase()` + 查表（`:204-209`）。
**成本**：全量 `toString()` + 高命中密度的全文档正则，**叠加在 PERF-6 的三次全量之上**——这是 CSS/HTML/Vue 场景下最贵的一次按键。
**修复**：不要每次 `docChanged` 重建；防抖约 200ms 或只重算变更行/范围；用 `syntaxTree(state)` 把扫描限制在字符串/值节点内。

#### PERF-8 工具命令输出：每行一次整树重渲染 + 每次重渲染重新 join `[新增]`【已实证】

`ProjectDetail.tsx:120-143` 的 `tool-command-log` 监听器每次都 `setToolCommandState(prev => { const logs = [...prev.logs, event.payload.data]; ... })`。该 state 住在 `ProjectDetail`，所以**每一行输出都重渲染整个窗格**（无论结果弹窗是否打开——`ToolCommandResultDialog.tsx:67` 关闭时返回 `null`，但 state churn 照旧）。`:127` 还渲染 `{logs.join('\n')}`，即每次重渲染把最多 2000 个字符串重新拼一次进一个 `<pre>`。
**对比（同项目已有正确范式）**：服务日志在 `MainLayout.tsx:40-52` 做 50ms 批量并在 `LogViewer` 虚拟化。
**成本**：5000 行输出的命令 → 5000 次整窗格重渲染 + 5000 次数组拷贝 + 至多 5000×O(2000) 的 join。实际约 O(行数 × 保留行数)。
**修复**：把运行态移出 `ProjectDetail`（放到叶子组件或小 store）；按 50ms 批量；弹窗主体用 memo 化的拼接结果或像 `LogViewer` 一样虚拟化。

#### PERF-9 选区变更扫描：O(文档行数) 且每行分配一次字符串 `[新增]`【已实证机制】

`CodeViewer.tsx:614-640`：`for (let n = 1; n <= doc.lines && lines.size < 500; n++) { if (doc.line(n).text.includes(text)) ... }` —— `doc.line(n)` 是树查找，`.text` 每行物化一个字符串。
**触发**：在 5 MB 阈值以下、选中一个**罕见** token 时拖动选区（每次 `pointermove` 都改变选中文本，`lastMarkText` 早退失效；常见 token 会因 500 命中提前结束）。
**成本**：每次拖动步进最多 `doc.lines` 次迭代 + 字符串分配，且发生在 CodeMirror 事务内（阻塞编辑器）。4 MB / 15 万行文件 ≈ 每次 pointermove 15 万次迭代。
**修复**：用已安装的 `highlightSelectionMatches` 装饰推导标记行，或用防抖/rAF 扫描并在文档未变时复用上次结果。

#### PERF-2 无代码分割，单 chunk 约 1.6 MB `[新增]`【已实证】

- 实测：`dist/assets/index-*.js` = **1,600.25 kB**（gzip 538.75 kB），CSS 31.32 kB，**整个应用一个 chunk**；`vite build` 自己报了 >500 kB 警告。
- `vite.config.ts` 全文 22 行，**无 `build`/`rollupOptions` 段**（无 `manualChunks`）。
- 根因可定位：`CodeViewer.tsx:13-41` 静态导入 **15 个官方 `@codemirror/lang-*`** + **11 个 `@codemirror/legacy-modes`** 解析器。用户打开 Nexus 就要解析并执行**所有**语言包，而一次会话通常只用一两种。
- 次要：`@tauri-apps/plugin-dialog` 的动态导入因同模块被静态导入而失效（[CQ-8](#cq-8-构建警告未被处理-新增)）。

**修复**：`React.lazy` 编辑器/查看器让外壳先绘制；把 `langMap`（`CodeViewer.tsx:502-570`，约 60 个闭包）改为按扩展名分组 `await import()`，通过 compartment 异步 reconfigure。**先上 `rollup-plugin-visualizer` 测量归因**（CodeMirror/Lezer 与语言包各占多少）。

#### PERF-5 搜索结果列表未虚拟化 + 整店订阅 `[新增]`

`SearchResultPanel.tsx:153` 请求 `maxResults: 1000`，渲染为 `groups.map` → `g.hits.map`（`:266,293`），**无虚拟化** → 最多 1000 行真实 DOM；`:46` 还是整店订阅（该 store 只 3 个原始值，代价小，列出只为完整性）。
**公平说明**：真正的大列表**已经**虚拟化了（`LogViewer.tsx:106`、`JarViewer.tsx:48`、`HexViewer.tsx:67`）——这不是"没做"，而是"漏了搜索结果这一处"。

#### PERF-10 每分钟级的小额浪费（可选）

- `stores/logStore.ts:69` `const merged = existing ? [...existing] : []`，随后 `trimLines` 第一件事是遍历全部求 `text.length`（`:34`）——每 50ms 每活跃服务 O(2000) 指针拷贝 + O(2000) 长度读取。绝对量小，仅在剖析显示它在火焰图上时再动。
- 后端日志**无全局字节上限**：只有每服务 2000 行（`core/process.rs:49`）与单行 8192 B（`:51`）。最坏理论值 2000×8192×50 服务 ≈ 800 MB（实际约 10 MB）。**建议加一个全局预算**。
- `EditorTabs.tsx:47-50` 的 `onScroll` 每次读 `scrollWidth`/`clientWidth`/`scrollLeft` 造成强制重排（子树小，P2）。
- `read_file` 在返回前对同一份字节做**3 次全扫描**（`commands/editor.rs:277-290` 的 `detect_line_ending` 分别数 `\r\n`、`\n`、`\r`）+ 1 次 UTF-8 校验 + 1 次拷贝。可合并为单趟（`memchr`），或只对前 64 KB 做启发式、仅在保存时全量扫描。
- `classfile.rs:1060` 每方法 `to_vec()` 复制 Code 属性；`:1301-1321` 先建 `Vec<String>` 行数组再逐行 `writeln!` 进输出 → 输出文本峰值翻倍（20 万行 × 40 B ≈ 8 MB → 16 MB）。可直接写入 `out` 去掉中间数组。

### 6.2 需实测（未测，不预设结论）

| 项 | 为什么需要实测 |
|---|---|
| 其余 27 个同步命令的线程影响 | 第四轮已把 6 个命令改为 async（见历史留档）；**剩余 27 个同步命令**同样内联在 IPC 请求路径上。两次独立复核对"内联执行所在线程"结论不一致（主线程 vs WebView2 协议线程）。需在 10+ 服务、含已崩溃服务时实测响应延迟，再决定是否继续改造 |
| [PERF-6](#perf-6-每次按键的全量文档处理3-次物化-全量正则-整树重渲染承前本轮大幅加深已实证)/[PERF-7](#perf-7-csshtmlvue-的颜色色块装饰每次文档变更做全文档正则-新增已实证) 的收益 | 需 DevTools Performance 录一次大文件连续输入（小文件下可能本就不可感知） |
| [PERF-9](#perf-9-选区变更扫描o文档行数-且每行分配一次字符串-新增已实证机制) 的墙钟耗时 | 需在 10 万行文件里拖动选区采样 |
| 3 秒轮询与关窗清理耗时 | 需 10+ 服务实例；含 3 个以上服务同时崩溃的场景 |
| jar/hex 内存峰值倍数 | 用大 fat jar 观测进程内存峰值（机制已在 [NEW-14](#new-14-jar-读取每次调用都把整包复制至少两遍-新增) 给出） |
| 发布二进制优化收益 | 当前 `nexus.exe` = 19.2 MB（含 2.1 MB 内嵌 CFR jar）。去掉 `unstable`、未用插件与 `[profile.release]` 调优后再量 |
| **测量环境提醒** | `main.tsx:13` 有 `React.StrictMode` → **开发构建下每个 effect 都会双调用**（编辑器创建/销毁两次、监听器两次）。做性能测量必须用**生产构建**，不要用 `tauri dev`。另外审计期间 `dist/` 被重建过（同一份代码两次测得 1,562.7 kB / 1,600.25 kB），引用包体积数字前先确认 `dist/` 与当前源码同步 |

---

## 七、用户体验优化

### 7.1 破坏性操作：确认、忙碌态与可逆性

#### UX-1 三个"停全部/停止服务"入口，三种行为；且会连带清空日志 `[承前]`

| 入口 | 位置 | 确认 | busy | 成功提示 |
|---|---|---|---|---|
| 全部**启动**（详情页） | `useProjectDetail.ts:137-152` | — | ✅ `setLoading({__all__:true})`（`:139`） | — |
| 全部**停止**（详情页） | `useProjectDetail.ts:154-167` | ❌ | ❌ **两者皆无** | ❌ |
| 停止项目（列表页） | `useProjectList.ts:94-106` | ❌ | ✅ `setActingId(id)`（`:96`） | ✅ |

`ProjectDetail.tsx:734-738` 的按钮 `disabled` 只看 `services.length === 0`（连运行态都不看）。而这条路径还会 `clearLogs` 掉**每个**服务的日志（`useProjectDetail.ts:160-162`）+ 后端清缓冲（`core/process.rs:558-564`）。
**场景**：误点一次底栏按钮 → 停掉项目全部服务，并抹掉你正要贴进 bug 报告的那段崩溃栈；多秒停止期间重复点击会重复发请求。
**收口**：加确认（名为数量："将停止 N 个服务"）+ 执行期间禁用两个按钮（对齐 start-all 的 `__all__`）+ 成功提示；三个入口统一语义。

#### UX-2 单服务停止（悬停 ✕）同样静默清日志 `[新增]`

`ServiceTreeEntry.tsx:150-155`（无确认）→ `:68-73` `clearLogs(service.id)` + 后端 `core/process.rs:558-564`。同一行悬停还显示"日志"按钮——**用户为了干净重启而点 ■，证据在同一瞬间消失**。
**收口**：同 UX-1；至少给一个"停止并清空日志"复选框，默认**保留**日志。

#### UX-3 日志"清空"即时、无确认、不可恢复，且全应用**没有日志导出** `[新增]`

`LogViewer.tsx:338-341` → `onClear`（`:206-209`）→ `logStore.clearLogs`（`stores/logStore.ts:119-127`）。服务仍在运行时后端快照不会重取（仅在 running→stopped 时重同步，`LogViewer.tsx:76-82`），所以清掉的历史彻底消失。
**收口**：确认（或 5 秒内 Undo），并在"清空"旁提供"导出/复制全部"（`dialog:allow-save` 权限已具备）。

#### UX-4 删除全局打开工具（IDEA/VSCode 绑定）无确认 `[新增]`

`ToolsManagerModal.tsx:170-176` → `handleDelete`（`:76-85`）**直接删**，只有事后 toast（标题自己都写着"服务绑定将自动解除"）。对照：项目/服务/模板删除**都**有具名确认弹窗（`ProjectModals.tsx:219-239`、`ProjectDetail.tsx:328-346,349-367`）。
**收口**：复用同一个确认弹窗。

#### UX-5 做得好的部分（保留）

项目/服务/模板删除的确认弹窗都**点名对象**、写"此操作不可撤销"、用红色按钮；后端只删配置行、不碰用户文件（`commands/project.rs:112-133`、`commands/service.rs:200`）。**UI 里没有任何能销毁用户文件的路径**：文件粘贴从不覆盖（`commands/fileops.rs:148-167` 加 " (2)"），文件树没有删除/重命名。可改进的细节：项目删除弹窗**不说会删掉几个服务**（`ProjectModals.tsx:224-226`），也不提示会先杀掉运行中的服务（`commands/project.rs:120-123`）。

### 7.2 反馈与错误可追溯

#### UX-6 错误 toast 3 秒消失，且无错误历史 `[承前]`

实测统计（全 `src/`）：`showNotification` 调用 **77** 处，其中 `variant: 'error'` **52** 处，全项目 `duration:` 覆盖 **仅 4** 处 → **50 个错误提示用默认 3000ms**（`Toast.tsx:49` 是唯一默认值，与 variant 无关）。无通知中心、无错误历史（grep 无实现）、无 `aria-live`。
**影响**：后端错误多数是可诊断的（含路径与原因），但用户来不及读；而 52 个错误点大量属于"启动失败/保存失败/加载失败"这类需要采取行动的场景。**这是本项目最高频的体验缺陷。**
**收口（两档）**：① 低成本——按 variant 定默认时长（error ≥ 8s 或需手动关闭）+ 错误 toast 加"复制详情"；② 彻底——通知中心/错误历史面板（可复用 [ARCH-4](#55-架构级缺口按风险排序) 的生命周期事件做数据源）。

#### UX-8 打开 `.class` / jar 条目最长 15 秒零反馈 `[新增]`

标签只在读取完成后才创建（`stores/editor.ts:403-405` await `fetchTextContent` → `openLoadedTab`；jar 条目 `:455-462`），反编译超时是 15s（`core/decompiler.rs:19,85-87`），嵌套 jar 还要叠加。文件树行无忙碌态（`FileTree.tsx:171-174`）、无 toast。
**场景**：双击 fat jar 里的 `Foo.class` → 应用看起来死了 1~15 秒；用户重复点击 → **又启动一个 `java -jar`**。
**收口**：立即以"正在反编译…"占位打开标签，并在飞行期间禁用该行。

#### UX-9 文件搜索无进度、不可取消 `[新增]`

`SearchResultPanel.tsx:143-167` 单次 fire-and-forget，只有 `status='searching'` 文案（`:254-256`）；后端最多扫 20000 个文件 / 深度 24（`commands/search.rs:60-64,127-159`）且**无取消通道**；面板唯一的出口是"关闭"（`:241-244`），而它只是隐藏，扫描继续。
**收口**：显示已扫描文件数（或至少已耗时）+ 一个"取消"（丢弃响应）；加单次超时。

#### UX-10 文件变更自动重启：零反馈，且静默抹掉该服务日志 `[新增]`

`RestartConfirm.tsx:39-48`（`restart_mode == 2` 直接重启，无 toast，失败只 `console.error`）+ UX-2 的日志清理。
**场景**：你正在看某个服务的输出，一次误存改动了被监听文件 → 服务重启，你正在读的控制台被清空，**屏幕上没有任何东西解释为什么**。
**收口**：toast/内联提示"「x」因文件变更已自动重启"（**变更路径就在事件载荷里**——见下条）。

#### UX-11 重启确认卡从不说明**是哪个文件**变了 `[新增]`

载荷里带着：`FileChange.path`（`services/service.ts:273-280`），在 `RestartConfirm.tsx:31-36` 也可取；但卡片只渲染服务名与命中计数（`:103-113`，`:112` 是静态文案）。
**影响**：2 秒防抖 + `**/*` 这类监听通配下，你无法判断触发者是自己的编辑还是某个构建产物。
**收口**：渲染最后变更的路径（截断，`title` 放全路径）。

#### UX-12 错误文案不可行动 `[新增]`

原样透出后端字符串，例如 `editor.ts:514-516` 的"保存文件失败 / 访问被拒绝"、`ServiceEditPanel.tsx:160-163` 的 `description: String(e)`（整条 Rust 错误含路径）、`ProjectModals.tsx:139` 的 `title: String(e)`、`FileTree.tsx:580` 把原始错误打印进树里。后端的"访问被拒绝"不说明**哪个根是允许的、该怎么做**——而这正是 [3.5](#33-跨项目切换后无法保存文件-新增) 那个 bug 的表现。
**收口**：把已知错误映射为可行动文案（"文件不在当前项目可访问范围内 —— 请重新选择该项目"），原始文本放在"复制详情"后。**正确范式已在同项目**：`AddServiceFormContent.tsx:37`、`ServiceEditPanel.tsx:117/162/176`、`RestartConfirm.tsx:45/78`、`ProjectDetail.tsx:103/114/138`。

#### UX-13 工具命令失败后弹窗停在"等待执行…" `[新增]`

`ProjectDetail.tsx:136-139` 的 catch 置 `loading:false` 但 `result` 仍为 `null` → `ToolCommandResultDialog.tsx:147-151` 渲染"等待执行…"，真正的错误只在那条 3 秒 toast 里。另外关闭弹窗时命令仍在运行，而**界面上没有任何地方指示**（`ToolCommandResultDialog.tsx:59` 有注释说明这是有意保留进程）。
**收口**：把错误存进弹窗 state 并渲染；关闭后保留一个"命令仍在运行"的提示。

#### UX-14 控制台日志缺操作上下文 `[新增]`

`ServiceTreeEntry.tsx:78` `console.error(String(err))` 没写操作名（紧邻的 toast 反而写了，`:79`）。全项目唯一的偏差，违反"日志必含操作与上下文"的项目规范。

### 7.3 空态 / 加载态 / 失败态

| 编号 | 问题 | 证据 |
|---|---|---|
| **UX-15** `[新增]` | 项目列表**混淆了加载、失败与空**，且两个错误状态都提示"创建第一个项目" | `useProjectList.ts:12-13` 无 loading 标志；失败只 log + toast（`:36-41`）；渲染 `filtered.length === 0 → <EmptyState/>`（`ProjectList.tsx:54-56`），其无搜索分支是"暂无项目 / 创建第一个项目"（`:180-187`）。**启动瞬间与每次 IPC 失败都显示"暂无项目"**，失败时永久停留并邀请你重复创建已有项目 |
| **UX-16** `[新增]` | 项目详情加载失败与"加载中"外观相同，永久停留且无重试 | `useProjectDetail.ts:57-61`（catch 只弹 toast，`detail` 保持 null）+ `ProjectDetail.tsx:201-207`（`detail === null` 就渲染"加载中…"）。一次 `get_project_detail` 失败（项目被删/DB 锁）→ 右栏永远"加载中…"，3 秒 toast 消失后只能靠切项目重试 |
| **UX-17** `[新增]` | 日志面板无空态：清空或安静的服务是一块空白黑矩形 | `LogViewer.tsx:214-234` 在 `rows.length === 0` 时渲染空轨道；只有头部传达"未运行 / 0 行"（`:303-312`） |
| **UX-18** `[新增]` | 重新打开读取失败的文件：得到一个**空且只读**、外观与"真的是空文件"无区别的标签 | `stores/editor.ts:406-422`：读失败时弹 3 秒 toast 并以 `readonly` + `content: ''` 打开；编辑器/标签上没有任何失败标记。同类：>10MB 文件静默只读（`:404`），原因只在保存按钮的悬停 tooltip 里（`EditorTabs.tsx:226`），**而且该 tooltip 对 `.class`/图片/hex 标签是错的**（一律说"文件过大"） |
| **UX-19** `[新增]` | 文件树根节点在列表到达前闪现"空目录" | `FileTree.tsx:481-483`（`entries` 初值 `[]`，无 loading 标志）+ `:582-584`。子目录做对了（`:455-457` 用 "…" 占位） |
| **UX-20** `[新增]` | `ErrorBoundary` 让用户"查看控制台"，而打包版**没有控制台** | `ErrorBoundary.tsx:32-36`；无重试入口（重置需要改文件/标签，因为 boundary 以路径为 key，`ProjectDetail.tsx:218`）。应用**确实**在写 `~/.nexus/logs`（`lib.rs`）却从不指向那里 |

**做得好的空/失败态（保留）**：服务列表空 → 引导添加服务（`ProjectDetail.tsx:697-706`）；模板列表空 → 解释如何创建（`:787-792`）；搜索面板有真正区分的 空闲/搜索中/错误/未找到/已截断 五态（`SearchResultPanel.tsx:249-265,312-316`）；文件树行区分错误与"空目录"（`:579-584`）；AI 面板的 spinner / 安装 ETA / 错误+重试 / "先打开一个项目"（`AiPanel.tsx:559-603`）。

### 7.4 信息呈现

#### UX-21 崩溃退出码与时间不可见 `[承前]`

**数据已在前端 store 里**：后端写入完整信息（`core/process.rs:629-633` `FailedService{service_id, exit_code, timestamp}`）；前端类型已带字段（`services/service.ts:226-229`）；store 已保存且变更检测已按字段比较（`runningStore.ts:26`）。**但被压成布尔**：`useProjectDetail.ts:133-135` `isServiceFailed` 只返回 `failed.some(...)`；卡片只渲染一个"失败"按钮（`ServiceTreeEntry.tsx:159-165`），折叠轨道的 tooltip 也只有"（失败）"（`ProjectDetail.tsx:526`）。
**影响**：最有价值的一个事实（退出码 1 vs 127 vs 被信号杀死）需要打开日志面板并滚动才能看到——而退出码目前只作为**日志里的一行**存在（`core/process.rs:627`）。
**收口**：卡片 tooltip 与轨道 tooltip 显示"退出码 1 · 14:32"。**纯展示改动，数据全在手——全报告性价比最高的一条。**

#### UX-22 日志截断只在 tooltip 里交代 `[承前]`

`LogViewer.tsx:310-312` 的 `title="当前行数（只保留最新 2000 行）"`，可见文本只有"1,234 行"；2000 行/2MB 的裁剪发生在 `stores/logStore.ts:24-43` 与 Rust 侧，**被丢弃的行数从未被计数或告知**。对照：搜索结果**已经**做对了——页脚写明"已截断，仅显示前 1000 条，可加扩展名筛选缩小范围"（`SearchResultPanel.tsx:314`）。
**收口**：常驻页脚"已截断：仅保留最近 2000 行（丢弃 N 行）"，文案风格照抄搜索面板。

#### UX-23 "停止即清日志"的产品语义 `[承前]`

崩溃日志**有意保留**（`core/process.rs:636-637` 注释："日志缓冲保留……崩溃/秒退的报错是诊断关键"），而**主动停止**会清（`:561-563` `stop` 移除缓冲、`:344-346` `start` 再清、`:672-674` `stop_all` 清全部）。
**结果**：诊断崩溃有据可查，但**想看上一轮正常运行的输出永远看不到**（重启即清），且**没有任何日志导出**可兜底。
**这是产品语义决策**（两轮未动）。建议：改为"下一次启动时才清"，或提供"保留上一次运行"开关（实现上就是每服务 2 个槽位：current + previous；代价是内存翻倍）。

#### UX-24 无处可见编码 / 换行风格 / dirty / 语言 `[新增]`

`StatusBar.tsx` 只有 16 行，渲染**仅** `activeTab.path`（`:12`，右对齐、300px 截断）。而应用在 EOL/编码保真上做了大量工作（`stores/editor.ts:99-124,502-504`、`commands/editor.rs:266-290`）——用户**永远看不到**当前生效的是 CRLF 还是 LF、GB18030 还是 UTF-8，也看不到缓冲区是否 dirty（dirty 信号是一个 8px 圆点，且悬停时消失，`EditorTabs.tsx:174-179`）。
**收口**：状态栏改为 dirty 标记 + 路径 + 行:列（取自 CM 选区）+ "CRLF" + "GB18030" + 语言。编码与 EOL 已在 `fileMetaCache` 里，只需把它变成 React 可见。

### 7.5 键盘可达性与焦点

#### UX-25 重要操作只在右键菜单，而原生菜单被全局禁用 `[承前]`

`main.tsx:7` 全局 `contextmenu` `preventDefault()` 屏蔽了所有原生右键菜单；而"删除服务""搜索文件内容""用 XX 打开""复制路径"等**只在自定义右键菜单**里（`ServiceTreeEntry.tsx:250`、`FileTree.tsx:283,530`、`ProjectList.tsx:39`、`ProjectListItem.tsx:46`、`TemplateTreeEntry.tsx:141`、`EditorTabs.tsx:156`）。

#### UX-26 三个主导航面是纯鼠标 `<div onClick>`；服务卡自称 "button" 但键盘激活会开始拖拽 `[承前]`

项目卡 `ProjectListItem.tsx:55-64`（div，无 `tabIndex`）；文件树行 `FileTree.tsx:302-314`（div，无 `tabIndex`）——**文件树，应用的主导航，键盘完全进不去**（`:315` 的箭头也是 hover-only）；搜索结果行 `SearchResultPanel.tsx:271-279,293-299`（div）。
服务卡 `ServiceTreeEntry.tsx:110-121` 从 dnd-kit 拿到 `{...attributes}{...listeners}`，即 `role="button"` + `tabIndex=0` + `aria-roledescription="sortable"`，而 **KeyboardSensor 在 Space 或 Enter 上开始拖拽**——打开编辑面板却是普通 `onClick`（`:118`）。模板卡同（`TemplateTreeEntry.tsx:73-83`）。
**影响**：键盘用户无法选中项目、打开文件、跳到搜索命中、或打开服务/模板编辑；在聚焦的服务卡上按 Enter 会静默进入拖拽模式。
**收口**：行改成真正的 `<button>`（或加 `role="button" tabIndex={0}` + Enter/Space 处理），把 dnd 激活移到专用手柄；方向键树导航作为后续。

#### UX-27 不可见但可聚焦的控件 `[新增]`

`opacity-0 group-hover:opacity-100` 的行**仍在 Tab 序中**：`ServiceTreeEntry.tsx:133`（日志/重启/停止，或 失败/启动）、`TemplateTreeEntry.tsx:92`、`ServiceEditPanel.tsx:414`、`EditorTabs.tsx:181-183`（每个非活动标签的关闭按钮）。都没用 `pointer-events-none`/`invisible`，也没加 `group-focus-within:opacity-100`。
**影响**：Tab 穿过服务列表时会停在不可见的按钮上，没有可见焦点环；**用户不知道焦点在哪，按 Enter 却会做出不可见的操作（例如停止一个服务）**。
**收口**：hover 包裹层加 `focus-within:opacity-100`（或按钮加 `focus-visible:opacity-100`）。

#### UX-28 Ctrl+F 在"有文件标签打开"时被全应用吞掉 `[新增]`

`CodeViewer.tsx:889-904` 的捕获阶段 document 处理器：若焦点元素不在 `.cm-editor` 内，就 `preventDefault()+stopPropagation()`（有意为之，用于压制浏览器查找框）。但点文件树行或项目卡后焦点留在 `<body>`（那些行不可聚焦），于是 **Ctrl+F 什么都不做**——看起来就是坏的。CodeMirror 的 `Mod-f`（文件内查找）只在编辑器自身有焦点时生效（`:654-665`），而**全局"在文件中搜索"没有任何快捷键**（唯二入口是两个右键菜单，`ProjectListItem.tsx:200-203`、`FileTree.tsx:238-241`）。
**收口**：按上下文路由 Ctrl+F（编辑器聚焦 → 文件内查找；否则打开文件搜索面板），并给文件搜索一个真正的快捷键（Ctrl+Shift+F）。

#### UX-29 Ctrl+S 只在编辑器有焦点时生效 `[新增]`

`Mod-s` 只绑在 CodeMirror keymap 内（`CodeViewer.tsx:770-773`），而 Ctrl+Z **特意**做了文档级转发，注释写明原因："切回文件后焦点常在标签栏/文件树上，编辑器 keymap 收不到按键"（`:906-923`）。**同一个理由完全适用于保存，但保存没做。**
相关：标签栏保存按钮在**任一**标签 dirty 时即可用（`EditorTabs.tsx:227`），而 tooltip 说"保存当前文件（Ctrl+S）"（`:226`）——当前标签干净、后台标签 dirty 时它看起来可用却什么也不保存。
**收口**：给 `Mod-s` 加同样的全局转发（焦点在 input/textarea 时跳过）；按钮的 disabled 基于**当前标签**。

#### UX-30 Modal 无焦点陷阱/初始焦点/焦点归还，且 Escape 会一次关掉所有叠层 `[承前]`

`Modal.tsx` **已有**无障碍属性（`:38-40` `role="dialog"`/`aria-modal`/`aria-label`，`:59` 关闭按钮 `aria-label`）——这部分前轮做到了。**仍缺**：

- Escape 监听挂在 **document** 上（`:18-20`），每个打开的 modal 各加一个 → 多弹窗叠加时一次 Escape 触发所有监听器 → **全关**（`ToolCommandResultDialog.tsx:60-65` 同样）。
- 无 focus trap、无初始焦点、关闭后不归还焦点。12 个弹窗里只有 2 个设了初始焦点（`ProjectModals.tsx:22,117`，还是 `setTimeout(…, 80)`，不确定）。
- Tab 会走出弹窗进入被遮挡的界面。

#### UX-31 上下文菜单是键盘死胡同 `[新增]`

菜单没有 `role="menu"`、没有方向键导航、没有自动聚焦（`ProjectList.tsx:206-277`、`FileTree.tsx:330-451`、`ServiceTreeEntry.tsx:270-346`、`ProjectListItem.tsx:190-273`），而且**完全没有任何 Escape 处理**——只在外部 `mousedown` 时关闭（如 `ProjectList.tsx:36-45`），对键盘用户来说不可能。Windows 菜单键/Shift+F10 只在行可聚焦处有效（即 [UX-26](#ux-26-三个主导航面是纯鼠标-div-onclick服务卡自称-button-但键盘激活会开始拖拽-承前) 不可聚焦的地方无效）。
**收口**：通过 `contextmenu` 打开后聚焦首项，处理 ↑↓/Enter/Escape，关闭时把焦点还给触发元素，并对键盘触发的菜单夹取 `clientX/clientY`（共享 hook 已做夹取：`hooks/useContextMenuPosition.ts:23-31`，但**6 个菜单只有 4 个在用**）。

#### UX-32 分隔条纯鼠标：3~6 px 拖拽目标、无 `role="separator"`、无方向键 `[新增]`

`ResizablePanel.tsx:125-130`（3 px 手柄，仅 `onMouseDown`）、AI 分隔条 `AiPanel.tsx:484-491`（6 px）。AI 面板的宽度**只能靠拖拽**改变（无预设/重置命令）。

#### 关于焦点环的澄清

**没有全局移除 outline**（`index.css:5-12` 只做 margin/padding/border-box 重置），每处 `focus:outline-none` 都配了替代（输入的 `focus:border-nexus-accent`、开关的 `focus:ring-2`、日志查找框的 `focus-within:border`）。所以键盘焦点**是可见的**，只是用 Chromium 默认蓝环、在深色主题上不显眼，且没有应用级 `:focus-visible` 样式。真正的例外只有一个：[UX-27](#ux-27-不可见但可聚焦的控件-新增) 的不可见悬停按钮。

**已核实可用的快捷键清单**（供后续维护对照）：编辑器聚焦时 Ctrl+S 保存 · Ctrl+F 文件内查找 · F3/Shift+F3 · Ctrl+G/Ctrl+Shift+G · Ctrl+Z/Ctrl+Y/Ctrl+Shift+Z · Ctrl+/ 注释 · Ctrl+Shift+[ / ] 折叠 · Tab/Shift+Tab 缩进；全局（编辑器未聚焦）Ctrl+Z/Ctrl+Shift+Z 转发到活动编辑器（**可用**）；日志视图 Ctrl+F 聚焦查找框、Escape 失焦、Enter/Shift+Enter 上下一个；文件内查找面板 Enter/Shift+Enter/Escape；弹窗与工具命令弹窗 Escape 关闭；服务编辑抽屉 Escape 关闭工具选择器、Enter/Space 切换开关；各表单 Enter 提交（带输入法守卫）；搜索面板 Enter 执行；标签中键关闭、右键菜单；可排序列表 Space/Enter 开始拖拽、方向键移动、Space/Enter/Tab 放下、Escape 取消。
**冲突**：① Ctrl+F 有三个归属，但编辑器与日志视图在 DOM 上互斥（`ProjectDetail.tsx:219-226`），真正的缺陷是全应用吞键而非重复触发；② Escape 每弹窗一个监听器 → 一键关掉所有叠加弹窗；③ **Ctrl+R/F5/F12/Ctrl+W/Ctrl+Tab 无任何处理**——浏览器级加速键活着且无人看管（见 [P0-2](#p0-2-关窗退出不拦未保存标签)）。

### 7.6 响应式布局（最低 800×600）

#### UX-33 在文档规定的最小宽度下，服务编辑抽屉有约 183 px 在屏幕外 `[新增]`

窗口最小 800（`tauri.conf.json:18-19`）− 左栏 260（`MainLayout.tsx:20`）− 3 px 分隔条（`ResizablePanel.tsx:125-130`）= 详情区 537 px。服务列是固定 360 px 的**绝对定位**面板（`ProjectDetail.tsx:427-429`），位于 `overflow-hidden` 容器内（`:212`）；编辑抽屉是另一个固定 360 px、`right: 360px`（`ServiceEditPanel.tsx:212-213` + `ProjectDetail.tsx:258`）→ 其左边缘落在 537−360−360 = **−183 px**，被裁掉：字段标签（"名称/启动命令/工作目录"）、每个输入框的左半部分、以及"保存配置/另存为模板"的左半边**都在屏幕外且不可达**（容器不能滚动）。AI 面板打开时详情区缩到约 217 px，360 px 的服务列会丢掉左侧 143 px——状态点、分区标题、服务名开头全部不可见。
**收口**：抽屉宽度改 `min(360px, 100% − servicePanelWidth)`；低于阈值时自动折叠服务列；**绝不在 `overflow-hidden` 容器里放无最小宽度保护的固定宽绝对定位面板**。

#### UX-34 AI 面板宽度不受窗口宽度约束，主界面可被压到 0 `[新增]`

只有拖拽处理器夹取到 `innerWidth − 420`（`AiPanel.tsx:317-320`）；启动时恢复的持久化宽度只夹到 [320, 900]（`stores/aiStore.ts:122-125`），每次 `setPanelWidth` 同样（`:203-207`）。而 dock 是 `flex-shrink-0`（`AiPanel.tsx:474`），主列是 `flex-1 min-w-0`（`MainLayout.tsx:175`）。
**场景**：先在宽窗口把 AI 面板拖到 700 px，再把窗口缩到 800 px（或在更小的显示器上重开）→ 主列只剩 100 px，项目列表/编辑器/服务列基本不可见，而 AI 面板仍占 700 px。
**收口**：在 resize 时夹取持久化宽度（`window.onresize`，或从实时 `innerWidth` 夹取），禁止 `panelWidth > innerWidth − 420`。

#### UX-35 垂直分割只在启动时按 `window.innerHeight` 算一次，resize 后不重算 `[新增]`

`ProjectDetail.tsx:71-73`（初始化器 + `topPanelMaxHeight`），**全 `src/` 无 resize 监听**（grep 确认），DB 恢复的值也未夹取（`:82`）。
**场景**：在 1440p 上最大化（上栏高度存为约 600 px），关闭，在 768 px 高的屏上打开 → 模板库被压到约 0，且分隔条也修不好（`maxWidth` 同样是陈旧值），而上栏在 `overflow-hidden` 容器里仍占 600 px。
**收口**：两者都由 `useWindowSize()` 推导并在每次 resize 时夹取。

#### UX-36 搜索面板的保存/拖拽高度只在拖拽时夹取 `[新增]`

`SearchResultPanel.tsx:60` + `PANEL_MAX_HEIGHT()` 在拖拽时求值（`:81`），resize 时不再求 → 在高窗口里设成 400 px 的面板，在 600 px 高的窗口里会溢出、头部可能被挤出屏幕。P2。

> **横向滚动本身处理得当**：编辑器靠 `.cm-scroller { overflow:auto }`（不换行，长行滚动而非回流）；标签条有显式 ←/→ 滚动按钮（`EditorTabs.tsx:196-221`）；搜索头部会换行；状态栏截断路径。**真正的溢出 bug 只有 UX-33/UX-34**（固定宽绝对定位面板 + `overflow-hidden`）。

### 7.7 一致性

| 编号 | 问题 | 证据 |
|---|---|---|
| **UX-37** `[新增]` | 同类操作两套确认范式：删除工具**直接删**，而删除服务/模板/项目**都**确认 | `ToolsManagerModal.tsx:170-176` → `:76-85` vs [UX-4](#ux-4-删除全局打开工具ideavscode-绑定无确认-新增) |
| **UX-38** `[新增]` | 两套"停止"语义：列表页停止有 toast（"「x」已停止 / 所有服务已停止"，`useProjectList.ts:101`），详情页脚栏的"全部停止"什么都没有（`useProjectDetail.ts:154-167`）；单服务停止也静默（`ServiceTreeEntry.tsx:60-84` 只在出错时 toast） | 见 [UX-1](#ux-1-三个停全部停止服务入口三种行为且会连带清空日志-承前) |
| **UX-39** `[新增]` | 同一文件里两套"关闭确认"流程：单标签确认**点名文件**（`EditorTabs.tsx:309`），批量确认只给数量（"其中 N 个文件有未保存的更改"，`:90`）→ "关闭其他/右侧/所有"无法在确认前复核 | `EditorTabs.tsx:79-124,306-341` |
| **UX-40** `[承前]` | 6 个右键菜单、4 种实现：只有 `ProjectList`/`ProjectListItem`/`FileTree`/`EditorTabs` 用了位置 hook（`hooks/useContextMenuPosition.ts`），而 `ServiceTreeEntry.tsx:258-268` 与 `TemplateTreeEntry.tsx:150-159` 用硬编码高度估算（36 px/项…）**正是该 hook 注释警告过的失败模式**；（那个 200 vs 180 的 20 px 夹取错误已随 CQ-10 修掉，重复实现本身还在） | 与 [CQ-5](#cq-5-重复代码每项-3-次含确切位置新增) D3/D8 同源 |
| **UX-41** `[新增]` | 同一"启动/停止"概念的按钮语义不一致：全部启动在运行时禁用并显示 `__all__`（`ProjectDetail.tsx:731`），全部停止从不禁用；项目卡启动有 spinner（`ProjectListItem.tsx:115-120`），详情页全部启动没有 | 见 [UX-1](#ux-1-三个停全部停止服务入口三种行为且会连带清空日志-承前) |
| **UX-42** `[新增]` | 术语漂移："失败"（服务卡 `ServiceTreeEntry.tsx:165`）/"未运行"（日志头 `LogViewer.tsx:308`）/"（失败）""（未运行）"（轨道 tooltip `ProjectDetail.tsx:526`）描述同一停止态；"服务模板库" vs "模板" vs 按钮"☆ 另存为模板"；折叠轨道混用两套 tooltip 约定（`:491/504` vs `:669`） | — |
| **UX-43** `[承前]` | 新服务 `show_file_tree` 默认值与 DB 默认**不一致** | `commands/service.rs:100` 硬编码 `show_file_tree: false`；DB 默认是 `1`（`database/mod.rs:99`），模板路径则沿用用户值（`service.rs:260-262`）→ "手动新建"与"从模板新建"的界面初始状态不同。改成一致会改变新服务默认展示，**需产品确认** |

---

## 八、新功能推荐

> 判据：优先推荐**后端/数据结构已就绪、只差界面接线**的；每条给出价值、成本、依据（已就绪 / 缺什么）、以及风险。最后列出**明确不建议做**的。

### 8.1 编辑器与文件

| 功能 | 价值 | 成本 | 依据 | 风险 |
|---|---|---|---|---|
| **保存冲突确认（覆盖/重载/看差异）** | **高** | **小** | 接线已在第四轮完成（冲突时拒绝写入 + 明确提示）；**剩下的"覆盖/重载/看差异"三选一对话框仍需界面决策** | 无。**先做这个**，唯一的数据丢失 P0 类 |
| 编辑器内查找**替换**（Ctrl+H） | 高 | 小 | 搜索面板有 Aa/.*/全词但**只有一个输入框**（`CodeViewer.tsx:392,411-413`），`createSearchPanel`（`:425-434`）无替换字段；`@codemirror/search` 已导入且提供 `replaceNext`/`replaceAll` | 在当前"每按键全量"设计下，跨 50 MB 文件替换全部会卡（[PERF-6](#perf-6-每次按键的全量文档处理3-次物化-全量正则-整树重渲染承前本轮大幅加深已实证)）。v1 先限定在当前选区 |
| **保存全部** | 高 | 小 | 只有单文件 `saveActiveFile`（`editor.ts:482`）；`EditorTabs.tsx:227` 用 `dirtyIds.length === 0` 判定已证明全应用可见 dirty 状态 | 无。必须复用既有的 `fileContent === null` 守卫（拒绝保存读取失败的标签，那是历史数据丢失 bug） |
| **树/标签显示"已修改未保存"** | 中 | 小 | `dirtyIds` 已在 store（`editor.ts:11`）但**只有一个消费者**（`EditorTabs.tsx:14`）；`src/components/file-tree/` 里 grep `dirty` **零命中** | 结构性障碍：`dirtyIds` 按**标签 id** 索引，树按**路径**（`FileTree.tsx:483,485`）→ 需要一个 id↔path 反查（`types/editor.ts:5-13` 两者都有，一个 `useMemo` 即可） |
| 跳到指定行（Ctrl+G） | 中 | 小 | **冲突**：`Mod-g` 现绑 `findNext`（`CodeViewer.tsx:667`）。`EditorView.scrollIntoView` 已用于 reveal（`:964`） | 需重新分配按键；`Mod-g`→findNext 是部分人的肌肉记忆 |
| 状态栏（行:列 / 编码 / EOL / 语言 / dirty） | 中 | 小 | `StatusBar.tsx` 只有 16 行、只渲染路径（`:12`）；编码与 EOL 已在 `fileMetaCache`（`editor.ts:100,115`）但**模块私有且对 React 不可见** | 需要把 `fileMetaCache` 暴露为响应式（小 store 字段），不是新子系统 |
| 自动换行开关 | 中 | 小 | 扩展数组里**没有** `lineWrapping()`（`CodeViewer.tsx:596-780`）；日志视图**是**换行的（`LogViewer.tsx:215`） | 若照搬到日志视图会与实测行高的虚拟化冲突，限定在编辑器 |
| **文件树新建/重命名/删除（删除走回收站）** | 高 | 中 | `write_file` **已支持新建路径**（父目录校验，`editor.rs:224-241`，前轮专门解的阻断）。**无** create-dir/rename/delete 命令（`fileops.rs` 只有剪贴板两项） | 重命名/删除是破坏性的，需确认弹窗范式；回收站（而非硬删）才是正确默认——用 `trash` crate 比自己调 `SHFileOperation` 便宜 |
| jar 条目导出 / 反编译另存 `.java` | 中 | 小~中 | 后端无 extract 命令 | — |
| HexViewer 偏移跳转与字节搜索 / ImageViewer 缩放 | 中 | 中 | 两个查看器当前是纯只读呈现 | — |

### 8.2 服务与进程可信度

> 统一观察：**`start_service` 在 `spawn()` 成功的那一刻就返回 `Ok(())`**（`commands/process.rs:104-107` → `core/process.rs:318-533`），没有任何东西验证进程是否**有用**。

| 功能 | 价值 | 成本 | 依据 | 风险 / 注意 |
|---|---|---|---|---|
| **崩溃退出码/时间上卡片** | 高 | **小** | 数据已到前端 store（`service.ts:226-229`、`runningStore.ts:26`），纯展示——[UX-21](#ux-21-崩溃退出码与时间不可见-承前) | 无。**本节最便宜的一条** |
| **就绪探测 / 健康检查** | **高** | 中 | 全无（`start()` 在建完 reader 线程后即返回，`process.rs:525-532`）。但**同项目有可用先例**：`wait_plugins_ready`（`core/ai.rs:675-727`）就是这件事——TCP 连接 + HTTP GET + 50→400ms 退避 + 8s 截止，`raw_get`（`:646-668`）是现成的最小 HTTP 客户端 | 需每服务的探测定义（TCP 端口/HTTP 路径/stdout 正则）。**存活性 ≠ 就绪性**：探测失败不能杀掉"慢但正常"的服务 → 只能是**建议性状态**，不能当门禁 |
| 崩溃自动重启 + 退避 | 高 | 中 | **语义冲突**：`restart_mode` 已存在（`models.rs:55`）但含义是**文件监听**重启，只被 watcher 消费（`commands/watcher.rs:32,74`）。崩溃检测已可用（`running()` 轮询 `try_wait()` 并记 `FailedService`，`process.rs:604-635`） | 复用 `restart_mode` 会让用户困惑，需新字段。**危险**：cwd 配错导致进程秒退的服务会变成无限重启循环打满 CPU → 必须**强制**指数退避 + 最大次数上限 + "已放弃"状态 |
| **运行历史持久化（跨重启）** | **高** | 中 | 全无（`processes`/`failed` 是 `Mutex<HashMap>`，`:272,275`，启动时重建为空 `:296-304`）。而 `FailedService` 已带 `exit_code` + `timestamp`（`:32-37`）——**记录的形状已经设计好了** | schema 很便宜（`service_runs`）。**必须配保留/清理策略**，运行历史是高容量数据；否则表无限增长 |
| 依赖与启动顺序 `depends_on` | 高 | 大 | 全无（grep `depends_on` 在两个 crate 里都是 0 命中）；顺序是 `ORDER BY sort_index`（`commands/process.rs:140`）且**不等就绪**（`:153-166`） | 需要真正的图工作：环检测、部分失败策略、依赖编辑器 UI。**只有先有就绪探测才值得做**——不等就绪的顺序只是装饰。**建议排在就绪探测之后** |
| **配置导入/导出（服务/模板 JSON）+ 跨项目复制服务** | 高 | 中 | 后端在 DB→DB 方向**已经把活干完了**：`duplicate_project` 在事务里复制项目+服务+工具绑定（`commands/project.rs:138-204`，含 `INSERT…SELECT`）；模板路径也已验证。只需"序列化到文件"与"文件→插入"。`dialog:allow-save` **权限已授予** | **凭据风险**：`env_vars` 是明文存储/导出（[SEC-8](#22-承前轮仍开放)），导出的 JSON 会泄漏凭据 → 必须提供**脱敏导出**并在 UI 说明。另需版本字段，否则旧导出在 schema 变更后失效 |
| 端口字段 + 冲突检测 + 一键打开 | 中 | 中 | 无 port 列（`database/mod.rs:89-103`）。但这正是 5.4 的 **29 处改动链**——**最贵的一个字段**。原料有：服务日志里会打 URL，`logFormatter.ts:98` 已识别 `IP:port` | 成本由 schema/重复问题主导，**不是**特性本身。**排序建议：先抽 `row_to_service` + 共享 SELECT 常量，此举把 29 处降到约 6 处**；反过来做要先付 29 处的税，下个字段再付一次 |
| 优雅停止超时（可配） | 中 | 小~中 | 硬编码 2000ms（`core/process.rs:759`）+ 1000ms reader 等待（`:763`）；`kill_process_tree` 走 `taskkill /T /F`（`:777-781`）——Windows 上是**立即硬杀**。而 unix 分支**确实**做 TERM→KILL + 300ms 间隔（`:793-809`） | **要诚实**：Windows 对 `cmd /C` 拉起的进程树没有干净的 SIGTERM；"优雅"意味着 `GenerateConsoleCtrlEvent`/`WM_CLOSE`，很脆。**作为每服务的"超时"旋钮发布，别叫"优雅关闭"** |
| 启动前预检 cwd/命令 | 中 | **小** | `start()` 只校验 `cwd` 非空（`process.rs:324-326`），cwd 错要等 spawn 失败（`:363-376`）才知道——而那已在用户点击之后。`Path::is_dir()`（`commands/ai.rs:79` 在用）现成 | 保存时不校验是**有意为之**（项目目录可能还不存在）→ 做成服务卡上的**非阻断警告**，绝不在保存时拒绝 |
| 批量多选启停/删除 | 中 | 中 | 只有"全部启动/全部停止"（`ProjectDetail.tsx:729-738`），`start_project_services` 是项目级全或无（`commands/process.rs:136`），无 id 列表。`reorder_services` 展示了有序 id 的范式（`commands/service.rs:161`） | 需新命令 + 选择 UI + "部分失败"上报路径。单用户场景下"全部"已覆盖多数需求，优先级低 |

### 8.3 日志与搜索

| 功能 | 价值 | 成本 | 依据 | 风险 / 注意 |
|---|---|---|---|---|
| **日志里的 `file:line` 可点击跳源码** | **高** | **小** | 两端都已就绪：`logFormatter.ts:134` **已经**用正则识别并着色 `([a-zA-Z\/\\]+\.\w+:\d+)`；`stores/editor.ts:473` 的 `locateFile(path, name, line, query)` **已经**接收 line 参数（`SearchResultPanel.tsx:297` 在用）。实测 `LogViewer.tsx` **无此点击处理** | `logFormatter` 返回 **HTML 字符串**给 `dangerouslySetInnerHTML`（`LogViewer.tsx:250`）→ 可点击化需要事件委托读 `data-path`/`data-line`，或改为结构化返回。另：日志里的路径常是相对的，且可能落在白名单外（`check_path_allowed` 会拒）→ 必须优雅失败，不能每次点击弹一个错误 toast |
| **保留"上一次运行"日志** | **高** | 小 | 只被语义挡住，不是能力：`stop` 清 Rust 缓冲（`:561-564`）且 `start` 再清（`:344-346`），上一轮输出**无处可存**；`logStore` 也镜像了这次擦除（`LogViewer.tsx:63-64` 拿到空快照后丢弃本地行） | 这是 [UX-23](#ux-23-停止即清日志的产品语义-承前) 的产品决策（"停止即清"是有意的）。改动即"每服务 2 槽位（current + previous）+ UI 入口"，内存翻倍（2×2000 行 × 2MB 上限）。值得做，但**先定语义** |
| **日志导出** | 中 | **小** | `dialog:allow-save` **权限已授予**；`paste_files`（`fileops.rs:49`）是 `spawn_blocking` IO 的现成范式；前端已持有全部行（`logStore.logs`） | 建议做成 **Rust 侧命令**从权威缓冲写出 + 原生保存对话框，这样导出不会被前端的 2000 行/2MB 上限截断 |
| 日志级别/关键字过滤 | 中 | 小 | 搜索只过滤文本（`LogViewer.tsx:95-99`）；`LOG_LEVEL_COLORS`（`logFormatter.ts:10-17`）已枚举 20 个级别 token，但**只用于着色**。`stream` 已在每行上（`logService.ts:9`）→ stdout/stderr/system 过滤是一个一行的谓词 | 过滤会让 `rows` 换标识 → 必须 `virtualizer.measure()`（范式已在 `LogViewer.tsx:122,208`），否则行高会在过滤间复用。便宜但容易做错 |
| 日志截断提示 | 中 | 小 | 文案照抄 `SearchResultPanel.tsx:314` | 无 |
| 全项目搜索 + `Ctrl+Shift+F`、正则/全词/排除 glob | 中 | 中 | 后端**仅子串**（`search.rs:204-217` 的 `find`/`eq_ignore_ascii_case`）；参数无 regex/排除项，排除目录是**硬编码** `DEFAULT_EXCLUDE_DIRS`（`:45-48`）；当前根是**服务 cwd** 而非项目根（`SearchResultPanel.tsx:148-153`） | **ReDoS 面**：对 2 万文件开放用户正则需要线性时间引擎（`regex` crate，本项目未用）+ 每文件超时。项目根可作真正"全项目"范围，但白名单根是"项目 + 各服务 cwd"（`editor.rs:160-198`） |
| 搜索结果键盘导航 + 重开保留上次查询 | 中 | 小 | 结果行只有 `onClick`（`SearchResultPanel.tsx:293`）；查询在组件 state（`:47-48`）且每次打开重置（`:122-134`） | 无。`isSubmitEnter`（`utils/keyboard.ts`）已处理方向键/Enter 导航所需的输入法守卫 |
| 搜索结果虚拟化 | 中 | 小 | 最多 1000 行未虚拟化（[PERF-5](#perf-5-搜索结果列表未虚拟化-整店订阅-新增)） | `@tanstack/react-virtual` 已在依赖内 |

### 8.4 AI 助手

| 功能 | 价值 | 成本 | 依据 | 风险 / 注意 |
|---|---|---|---|---|
| **会话存活探测接线（让崩溃自愈真正生效）** | **高** | **小** | 后端探测**已正确**（`snapshot()` 调 `alive()` 并记"会话进程已退出"，`commands/ai.rs:42-45` → `core/ai.rs:288-290`）；前端 `aiService.status`（`aiService.ts:16`）**零调用**；自愈 effect 存在但被 `!url` 挡住（`AiPanel.tsx:216-221`） | **本节最佳性价比**。可在既有的 3 秒 tick（`runningStore.ts:9`）里轮询，或发生命周期事件。风险：对**持续性**故障的 dsh 会形成重启循环 → 加上限并把 `lastError` 显出来，别空转 |
| **切项目不打断 AI 会话（UX 修法，非多会话）** | 中高 | 小 | 切项目只置 `panelOpen:false`（`aiStore.ts:138-155`），旧 dsh 会一直跑到下次以不同 cwd 启动（`commands/ai.rs:92-111`）——而那次启动会**停掉**它。这就是为什么有强制的 `sessionCwd === currentCwd` 展示门（`AiPanel.tsx:127`）与整套 `aiOnCache`/`aiPanelCache` 记忆机制（`aiStore.ts:23-46`） | 诚实的修法：明确警告"切换项目将停止当前 AI 会话"，并提供"在新标签页打开"（`openUrl` 已用于 `AiPanel.tsx:415`）让运行中的会话在浏览器里survive。**约 20 行，拿到多会话方案约 5% 成本的大部分价值** |
| **修 AI 会话的 cwd 字符串相等陷阱** | 中 | **小** | `commands/ai.rs:93` 比较 `s.cwd == cwd_norm`，而 `cwd_norm` 只做 trim（`:74`）——**没有大小写/分隔符归一化**，尽管同 crate 已有专为此写的 `norm_path`（`core/ai.rs:35`，只用于 dsh 文件扫描）。`D:\p` vs `d:\p\` 会强制整轮 stop + 最长 60 秒重启（`commands/ai.rs:20,124`） | 无。**做它**——3 行修复，去掉一个 Windows 上（盘符大小写与结尾分隔符随来源变化）用户会反复撞上的 60 秒卡顿 |
| 会话崩溃主动通知 | 中 | 小 | 无 notification 插件；已授予的最接近原语是 `core:window:allow-request-user-attention`（任务栏闪烁） | `tauri-plugin-notification` 需要依赖 + Windows 上非打包二进制要 AppUserModelID 才真能弹 toast。**先试任务栏闪烁 + 既有 toast 系统**，再考虑加插件 |
| 把"当前打开文件/选中日志"作为上下文送入会话 | 中 | 中 | **无任何机制**——这是 AI 方向最重要的否定结论。`raw_get`（`core/ai.rs:646-668`）是唯一 HTTP 客户端且**只支持 GET**（请求行在 `:650-653` 构造，无 body、无 method 参数），只被就绪探测调用；stdin 是 `Stdio::null()`（`:503`）。全仓 grep `--prompt`/`--print`/`postMessage` **零命中**；`Webview::eval` 从未被调用 | 积木其实很好：应用已经用 HTTP/1.1 跟 `127.0.0.1` 说话并复用认证 cookie（端口与 token 已解析于 `core/ai.rs:617-633`、随 `AiSession.url:281` 携带），`initialization_script`（`commands/ai.rs:265`）也证明了 JS 环境可控。但它依赖 dsh 的**私有未公开 HTTP API**，而应用自己把 dsh 自动升级到 `@latest`（`core/ai.rs:463`）——**任何一次 dsh 发版都可能无声打破它**。必须特性探测 + 降级到"复制日志到剪贴板"，而不是假装可用 |

### 8.5 平台与常驻

| 功能 | 价值 | 成本 | 依据 | 风险 / 注意 |
|---|---|---|---|---|
| **关闭窗口最小化到托盘、不杀服务** | **高** | 中 | `lib.rs:210-218` 在 `CloseRequested` **无条件** `cleanup_resources`（停全部服务 + AI 会话 + 监听）。`tauri.conf.json` 无 `trayIcon` 键，`Cargo.toml` 无托盘插件。而 `cleanup_resources` **明确是幂等的**（`lib.rs:29`）→ 拦截关闭、延后清理在设计上安全 | 需要 `tray-icon` feature + 菜单 + 一个真实决策："关闭"到底是什么意思（经典陷阱：用户点 × 期望退出，应用却继续跑）。必须在 `RunEvent::Exit`（`lib.rs:228-248`）保留清理，让托盘最小化后真正退出仍能清干净。对"开发环境管理器"这个定位是核心诉求 |
| 重启应用恢复上次运行的服务 | 中 | 中 | 完全依赖 [运行历史](#82-服务与进程可信度)；今天进程表启动时重建为空（`process.rs:296-304`）。5 个已持久化的 layout 键证明 `layout` 表适合小 KV，但**不是**运行记录的家 | **有惊吓用户的风险**：启动即拉起服务（尤其占端口的）很侵入。默认关闭、按项目显式开启，且**绝不自动启动上次失败的服务**；启动前先确认 |
| 全局命令面板（Ctrl+K） | 中 | 中 | **重要区分**：应用内 Ctrl+K 面板**不需要任何新插件**（只有"在其他应用聚焦时也生效"才需要 `tauri-plugin-global-shortcut`，而那种行为对命令面板并不可取） | 真正的工作量在于动作清单散落在 4 套右键菜单实现里（[UX-40](#77-一致性)）且没有注册表。做面板意味着先抽一个命令注册表——**那才是成本，且它是重构不是特性**。值得做，正是因为它强迫这次抽取 |
| 系统通知（服务崩溃/启动完成） | 中 | 小 | 失败已被检测（`running()` → `FailedService`，`process.rs:625-635`），但只通过 3 秒轮询暴露——**没有 `service-lifecycle` 事件**（只有 `service-log`/`tool-command-log`/`file-changed`，[ARCH-4](#55-架构级缺口按风险排序)） | 从既有 reader/`running()` 路径 emit `service-lifecycle` 很便宜，还能让 3 秒轮询降为兜底（减少 IPC）。**事件是有价值的那一半，可以独立于通知先做** |
| 进程资源占用（CPU/内存） | 中 | 中 | `RunningService` 只有 `service_id`/`project_id`（`commands/process.rs:85-88`）；无 `sysinfo` 依赖；底层 `Child`/`pid` **确实保留着**（`process.rs:258`） | **不建议做**：对 N 个进程持续采样是一个带自身生命周期 bug 的新轮询子系统，而开发者很少据此行动。真需要就做**按需单次采样**（一个"资源占用"按钮 + `GetProcessMemoryInfo`），不要实时曲线 |
| 崩溃 / 致命错误落盘与"下次启动提示" | 中 | 小~中 | 部分已有：`ErrorBoundary` 只包住查看器区域（`ProjectDetail.tsx:218`）；启动致命错误会落盘 + 退出码 1（`lib.rs:81-90`）。但**其他任何地方的 Rust panic**（线程 spawn、`classfile.rs`、持锁线程 panic）在打包版里什么都不产生 | `std::panic::set_hook` + 写崩溃文件 + 下次启动弹提示。**很便宜且关掉了"双击后什么都没发生"这一类报告。不做远程上报** |
| 自动更新 | 低~中 | 大 | 无 `tauri-plugin-updater`；`bundle.targets` 是 `nsis`/`msi`；CSP 是 `connect-src 'self' ipc: http://ipc.localhost` → 前端 `fetch()` 访问更新服务器**被阻止**（更新器必须跑在 Rust）；无签名密钥、无发布端点、无 CI | **不建议做**：完整成本 = 更新插件 + minisign 密钥对 + 托管清单 + 签名产物 + 一条不存在的发布流水线（本项目**没有任何 CI**）。单用户工具用 NSIS/MSI 侧载完全够。注意应用**确实**会自升级 **dsh**（`core/ai.rs:460-467`）——那才是真正的边界，别混淆两者 |

### 8.6 诊断可支持性

| 功能 | 价值 | 成本 | 依据 | 风险 / 注意 |
|---|---|---|---|---|
| **本地诊断包（无遥测）** | **中高** | **小~中** | 所需材料都已存在且已隔离：`~/.nexus/logs/nexus.log` + `.1`（`logger.rs:15,53`）、`startup-error.log`（`lib.rs:86`）、DB（`database/mod.rs:14`）、应用/OS/WebView2 版本、以及 8.2 的 JSON 导出形状。**`zstd` 已是依赖**（`core/ai.rs:165-183`）→ 打包不需要新 crate | **必须脱敏，这就是全部风险**：`env_vars` 是明文凭据，服务 `command` 只在**日志输出**里被遮蔽（`process.rs:819-849`）而非 DB 内，DB 还含全路径。**打包 DB 会泄漏凭据** → 只装日志 + 版本 + **脱敏**配置摘要（名字、env 键的哈希、计数），并让用户分享前可自查 |

### 8.7 不建议做（明确反对）

| 提议 | 为什么不做 |
|---|---|
| **多会话 AI** | `Mutex<Option<AiSession>>` + 单个全局 epoch + 约 10 个硬编码布局常量 + N 个子 WebView 是一轮大架构变更，其收益（"一个在跑时再开一个"）大部分由 8.4 的"切项目不打断 + 浏览器新标签打开"以 5% 成本拿到 |
| **自动更新** | 需要签名基础设施、托管清单与一条不存在的发布流水线。单用户本地工具用安装包侧载足够 |
| **CPU/内存实时曲线** | 为开发者很少据以行动的信息新增一个持续采样子系统。按需单次采样即可 |
| **`nexus.log` 改 JSON** | 为**不存在的**机器消费方优化，同时劣化唯一真实的消费方（用编辑器打开文件的人）。现有文本已带时间戳/级别/轮转 |
| **就绪探测之前先做 `depends_on`** | 没有就绪信号的顺序只是装饰。先落 8.2 的就绪探测，再决定依赖是否还需要 |
| **任何远程遥测/崩溃上报** | 本地、单用户、可离线的工具。可自查的本地诊断包（8.6）拿到诊断价值而不付隐私与基础设施成本 |
| **env var 的凭据保险库** | 过度设计（铁律 2）。UI 遮蔽 + 导出脱敏保证 + 引用 `.env` 文件，是"已经明文存在 SQLite 里"的工具的合理上限 |
| **先做端口字段再做映射器重构** | 特性合理但**顺序错了**：先做要先付 29 处改动的税，下个字段再付一次。先抽 `row_to_service` + 共享 SELECT 常量 |
| **引入 Redux / 状态机库替换 Zustand** | 现有 Zustand 用法没有出现它解决不了的问题；问题在订阅粒度（[PERF-6](#perf-6-每次按键的全量文档处理3-次物化-全量正则-整树重渲染承前本轮大幅加深已实证)），不是库能力 |
| **把 SQLite 换成数据库服务/连接池** | 单用户本地应用；`Mutex<Connection>` 只在"每次路径校验都查库"这个坏用法下成为热点（白名单根缓存已加，PERF-1）。修调用方式，别换存储 |
| **引入 `{code,message,details}` 响应封套** | 项目规范本来就写明"团队未达成一致不引入"；当前 `Result<T, String>` + 中文错误文案对本应用足够。**但**错误字符串当前被当作契约使用（契约同步问题见 [ARCH-5](#55-架构级缺口按风险排序)），应改的是**类型化错误**，不是封套 |
| **给前端全量补 UI 快照测试** | 收益低于 [CQ-6](#cq-6-前端零测试-承前) 点名的 3 个纯逻辑模块；项目规范本就允许跳过纯展示层 |

---

## 九、建议路线图

> **第四轮已完成**：原批 0（保存冲突接线 / 反汇编修正 / cwd 归一化）与批 1（同步命令改异步 / zip 上限 / 掩码补全）
> 全部落地，批 2 完成了其中的静默失败与保存重入部分，批 5 完成了白名单缓存与两处内存问题。
> 下表只列**剩余工作**。

| 批次 | 内容 | 成本 | 说明 |
|---|---|---|---|
| **批 A · 收口两项 P0** | [P0-2](#p0-2-关窗退出不拦未保存标签) 关窗拦截未保存标签（顺带把 [PERF-4](#perf-4-关窗清理仍在主线程同步执行-承前) 的清理移出同步路径）· [P0-3](#p0-3-服务编辑面板切换即丢弃未保存编辑6-条触发路径) 编辑面板 dirty 守卫 | 小~中 | 技术方案与状态层都已就绪，只差"弹什么、文案怎么写"。**这是当前唯一的数据安全缺口** |
| **批 B · 安全边界收口（含两个纯代码小项）** | [SEC-6](#sec-6-子-webview-仅校验初始-url无导航守卫端口未固定-新增) 子 WebView `.on_navigation` + 固定端口 · [SEC-2](#sec-2-zip-校验用的是不可信自述字段这一模式值得单独记住) `core/ai.rs` 全量读设限 · [SEC-4](#sec-4-文件访问白名单可通过-ipc-自我放宽-新增已实证) 额外根改为"仅原生选择器确认后加入" · [SEC-9](#22-承前轮仍开放) 校验句柄真实路径 · [SEC-8](#22-承前轮仍开放) env 值显示/遮蔽切换 | 中 | SEC-6/SEC-2 是纯代码小改动，可先单独做掉；SEC-4 属策略变更，需定"哪些目录算合法的额外根" |
| **批 C · 剩余确认交互与反馈** | [UX-1](#ux-1-三个停全部停止服务入口三种行为且会连带清空日志-承前) 停全部确认 + busy · [UX-3](#ux-3-日志清空即时无确认不可恢复且全应用没有日志导出-新增) 日志清空确认/导出 · [UX-4](#ux-4-删除全局打开工具ideavscode-绑定无确认-新增) 删除工具确认 · [UX-6](#ux-6-错误-toast-3-秒消失且无错误历史-承前) 错误时长/可追溯 · [UX-12](#ux-12-错误文案不可行动-新增) 错误文案可行动 · [UX-8](#ux-8-打开-class-jar-条目最长-15-秒零反馈-新增) 反编译占位标签 · [UX-13](#ux-13-工具命令失败后弹窗停在等待执行-新增) 失败不留"等待执行" | 中 | 与批 A 同属"信任度"主题，建议连续做完 |
| **批 D · 界面体验与响应式** | [UX-15](#73-空态-加载态-失败态)~[UX-20](#73-空态-加载态-失败态) 空/加载/失败三态 · [UX-21](#ux-21-崩溃退出码与时间不可见-承前) 退出码上卡片（数据已在前端 store，纯展示）· [UX-24](#ux-24-无处可见编码-换行风格-dirty-语言-新增) 状态栏 · [UX-25](#ux-25-重要操作只在右键菜单而原生菜单被全局禁用-承前)~[UX-32](#ux-32-分隔条纯鼠标36-px-拖拽目标无-roleseparator无方向键-新增) 可达性 · [UX-33](#ux-33-在文档规定的最小宽度下服务编辑抽屉有约-183-px-在屏幕外-新增)~[UX-36](#ux-36-搜索面板的保存拖拽高度只在拖拽时夹取-新增) 响应式 · [UX-37~43](#77-一致性) 一致性 | 中 | 可拆两次；`UX-21` 与 `UX-15/16` 成本最低、感知最强 |
| **批 E · 结构重构（解锁后续一切）** | [ARCH-1](#55-架构级缺口按风险排序) `row_to_service` + 共享 SELECT 常量 · `services/system.ts` 收掉 16 处越层 `invoke` · `utils/errors.ts` · `<ConfirmDialog>` / `useClickOutside` / `<ContextMenu>`（顺带消掉 [CQ-5](#cq-5-重复代码每项-3-次含确切位置新增) 的重复与 20 px 错位） | 中 | **所有"加字段/加动作很痛"的根因都在此**。`row_to_service` 必须**先于**任何服务新字段 |
| **批 F · 性能收口** | [PERF-6](#perf-6-每次按键的全量文档处理3-次物化-全量正则-整树重渲染承前本轮大幅加深已实证) 订阅下移 + 防抖 · [PERF-7](#perf-7-csshtmlvue-的颜色色块装饰每次文档变更做全文档正则-新增已实证) swatch 防抖 · [PERF-8](#perf-8-工具命令输出每行一次整树重渲染-每次重渲染重新-join-新增已实证) 工具命令批量 + 状态外移 · [PERF-5](#perf-5-搜索结果列表未虚拟化-整店订阅-新增) 搜索结果虚拟化 · [NEW-14](#new-14-jar-读取每次调用都把整包复制至少两遍-新增) jar 免整包复制 · [NEW-15](#new-15-服务日志热路径每行一次时间戳多次分配一次-ipc-事件rust-侧无批量-新增) 日志批量 emit · [PERF-2](#perf-2-无代码分割单-chunk-约-16-mb-新增已实证) 语言包懒加载 | 中 | 除 [NEW-14](#new-14-jar-读取每次调用都把整包复制至少两遍-新增)/[NEW-15](#new-15-服务日志热路径每行一次时间戳多次分配一次-ipc-事件rust-侧无批量-新增) 外都建议**先测再改**（生产构建，注意 StrictMode） |
| **批 G · 服务可信度与常驻** | 就绪探测（抄 `wait_plugins_ready`）· 崩溃自动重启（强制退避 + 上限）· `service_runs` 运行历史 · `service-lifecycle` 事件 + 通知 · 托盘常驻 · 配置导入/导出（含脱敏）· [CQ-9](#cq-9-死代码-永不触发的逻辑-新增) `ai_status` 接线（纯接线，可提前做） | 中大 | 需数据模型迁移，单独排一轮并准备回滚；`service_runs` 必须带保留策略 |
| **批 H · 工程化** | CI 三件套（`clippy -D warnings` + `cargo test` + `tsc --noEmit` 已可直接用）· eslint + `eslint-plugin-react-hooks`（[CQ-3](#cq-3-7-处-eslint-disable-是无效注释-承前) 的 7 处 disable 在等它）· prettier/rustfmt/editorconfig · vitest 补纯逻辑模块与"含分支的字节码 fixture" · `cargo audit`/`pnpm audit` · 清理未用依赖 | 小~中 | **唯一能防住已修问题复发的手段**。第四轮新增的 13 个回归测试已经证明这条路有效 |
| **持续** | 拆 `ProjectDetail`/`CodeViewer`/`classfile.rs`/`process.rs`/`core/ai.rs`（[CQ-1](#cq-1-文件体量超标含生产测试拆分承前)）· 把 `check_path_allowed` 移出 `commands/editor.rs` · `core/ai.rs` 的 `job_arc` 改参数（解锁可测性）· [NEW-9](#new-9-job-object-覆盖仍缺一处-新增部分已修复) 反编译 JVM 入 Job Object | 小~中 | "改到哪清到哪"，但 `classfile.rs` 拆分要**先抽 `ClassModel`**，否则每一步都在跟借用检查器搏斗 |

## 十、审查结论

- [x] **待修复**：2 条 P0（数据丢失，均需界面确认交互）、30 条 P1、45 条 P2
- [ ] ~~通过：无 P0 问题~~（**未通过**：仍有 P0）

**判定依据**

1. **工程质量已达标**：`cargo clippy` 0 警告、111 单测全绿、59/59 命令注册无漂移、事件名前后端一致、`tsc` 0 错误、构建 exit 0。第一、二、四轮累计关闭 **82 项**，其中第四轮在纯代码范围内关闭 22 项，并新增 13 个回归测试——**其中 3 条回归测试做过反证验证**（把修复退回原逻辑，确认测试确实失败）。
2. **P0 从 3 条降到 2 条，且性质发生变化**：原 P0-1（保存静默覆盖外部改动）已闭环；剩下两条的共同点是**技术方案与状态层都已就绪，缺的只是界面决策**（弹什么、文案怎么写）。**这是当前唯一的数据安全缺口。**
3. **两个"护栏给了假保证"的案例已经收口，但教训要留下**：
   - [SEC-1]（zip 炸弹）校验的是攻击者可控的声明字段——比"没防护"更危险，因为审计时容易被判为已覆盖。现已改为按实际字节数设限并实证。
   - [SEC-4](#sec-4-文件访问白名单可通过-ipc-自我放宽-新增已实证)（白名单根可被 IPC 放宽）仍然开放：它说明**"机制正确"不等于"边界成立"**——变换逻辑（canonicalize + 按分量比较）是对的，但根集合来自 IPC 可写字段。**上一版审计（含本报告初稿）在这两处的乐观判断已被推翻并修正**。。
4. **"测试数量 ≠ 测试覆盖"有了一个具体标本**：`classfile.rs` 有 11 个单测、是测试最密的生产文件之一，但 fixture 里**没有一个分支指令**，于是"所有分支目标都偏移 +3/+5"活了很久（原 NEW-1，已修复）。第四轮补的 fixture 才第一次覆盖这条路径。
5. **最大的系统性风险仍是工程化**：`cargo test` 里**没有一个测试触及锁/线程/子进程/真实 DB/监听循环**（`ProcessManager` 约 400 行零测试，原因是被 `AppHandle` 绑定）；前端**零测试**，而第二、四轮修掉的缺陷大多落在那套状态机上；项目既无 CI 也无 eslint（项目文档里的 `pnpm eslint` 命令至今不可执行，却有 7 处 `eslint-disable` 注释在"等"它）。**同类问题再次引入只是时间问题。**
6. **方法论教训（供后续审计复用）**：
   - 本环境下 `Get-Content`/`Select-String` 读 UTF-8 会**静默丢行**，统计代码规模必须用 `ReadAllLines(..., UTF8)`（[4.1](#41-量化基线本轮实测)）。
   - 依赖第三方库行为的结论要**读锁定版本源码**（`zip-2.4.2`、`tauri-plugin-shell-2.3.5`、`tauri-macros-2.6.3`），不能凭"看起来有校验"下结论。
   - 对"改了同类命令一半"的批量重构，应维护**同族命令清单**逐项核对（旧 PERF-3 就是这么漏掉的）。
   - 修复性能/权限类改动时，**缓存要选能自失效的键**（白名单根缓存以"原始根列表"为键，DB 变更自动失效），避免在多处写入点手动清缓存——漏一处就是"新加的服务目录打不开文件"。

**一句话建议**：先做**批 A**（两项 P0 的确认交互，数小时量级）与**批 B 里两个纯代码小项**（子 WebView 导航守卫、`core/ai.rs` 全量读设限），随后用**批 H**（CI 三件套 + eslint）把第四轮拿到的成果锁住——`clippy -D warnings` + `cargo test` + `tsc --noEmit` 现在就能直接当门禁。

---

## 附录 A 未验证与需实测

| 项 | 原因 | 建议验证方式 |
|---|---|---|
| **capabilities 收窄后的运行时冒烟** | 权限从 37 收到 12，`cargo build` 只能验证"标识符合法"，**无法验证运行时功能未被误删** | 打包版点一遍：标题栏拖动/最小化/最大化/关闭、目录选择对话框（新建项目/服务编辑/工具库）、AI 面板显隐与尺寸跟随、系统剪贴板文件复制粘贴。若报 "not allowed"，补回对应权限即可（删除项均已核为零调用点） |
| 其余 27 个同步命令的线程影响 | 第四轮把 6 个命令改为 async；**剩余 27 个同步命令**同样内联在 IPC 请求路径上。两次独立复核对"内联执行所在线程"结论不一致（主线程 vs WebView2 协议线程），因此"是否冻结界面"仍未定论 | 10+ 服务（含已崩溃服务）下实测"启动全部"的响应延迟与界面帧率；或对 27 个命令逐个评估阻塞量后决定是否继续改造 |
| WebView2 加速键行为 | Ctrl+R/F5/F12/Ctrl+W 未被拦截（[P0-2](#p0-2-关窗退出不拦未保存标签)），但未在运行时确认打包版里这些键的实际效果 | 打包版里逐个按键，确认是否需要拦截 |
| 恶意项目能否影响 `parse_web_url` 的选择 | `core/ai.rs:479-484` 取首个 http(s) 子串；极端情况下可能指向另一个本地端口（面板无 IPC，故最坏是 UI 重定向） | 用带恶意输出的仓库实跑 dsh |
| 前端 XSS 面（`dangerouslySetInnerHTML` 之外） | 只按常见 sink 做了 grep；React 层面的注入会把 [SEC-4](#sec-4-文件访问白名单可通过-ipc-自我放宽-新增已实证) 从"需先攻陷 webview"升级为直接可达 | 专门的 XSS 审计；React DevTools 检查渲染路径 |
| 非 Windows 分支 | 本机只装了 `x86_64-pc-windows-msvc`，`#[cfg(not(windows))]` 被 cfg 掉；`core/ai.rs:31` 的 `USERPROFILE` 硬编码在 macOS/Linux 下会静默禁用 workspace 注册（[ARCH-8](#55-架构级缺口按风险排序)） | 在 macOS/Linux 实跑构建与用例 |
| [PERF-6](#perf-6-每次按键的全量文档处理3-次物化-全量正则-整树重渲染承前本轮大幅加深已实证)/[PERF-7](#perf-7-csshtmlvue-的颜色色块装饰每次文档变更做全文档正则-新增已实证)/[PERF-9](#perf-9-选区变更扫描o文档行数-且每行分配一次字符串-新增已实证机制) 的收益 | 机制可证，量级未测（小文件下可能不可感知） | DevTools Performance 录大文件连续输入与选区拖动。**必须用生产构建**（StrictMode 会双调用 effect） |
| 3 秒轮询与关窗清理耗时 | 需多服务实例 | 起 10+ 服务，量 `get_running` 单 tick 与 `CloseRequested` 全程；含 3 个以上服务同时崩溃 |
| jar/hex 内存峰值倍数 | 机制已在 [NEW-14](#new-14-jar-读取每次调用都把整包复制至少两遍-新增) 给出（NEW-12/NEW-13 已修），未实测 | 用大 fat jar 观测进程内存峰值 |
| 包体积归因与发布优化收益 | 1.6 MB 单 chunk 中 CodeMirror/Lezer 与语言包各占多少未知；`unstable`/未用插件/`[profile.release]` 的收益未测 | `rollup-plugin-visualizer` + 调优后再量 `nexus.exe` |
| `dist/` 新鲜度 | 本轮审计期间 `dist/` 被重建过（同一代码两次测得 1,562.7 kB / 1,600.25 kB），引用包体积前需确认与源码同步 | 引用前先 `pnpm build` |
| 依赖漏洞 | 本机未装 `cargo audit`；`pnpm audit` 未跑 | `cargo install cargo-audit && cargo audit`；`pnpm audit` |

---

## 附录 B 待处理问题编号索引

> 只列**当前仍未修复**的项（共 77 条：P0 2 · P1 30 · P2 45）。已修复项的编号在本表中**不存在**——
> 这是刻意的：编号是稳定标识，不回收不复用，看到跳号即表示已关闭（编号不回收以保留追溯能力）。
> 级别为该项首次提出时的判定。
>
> 注：为控制篇幅，正文以表格形式存在于同一小节内的项（UX-17~20、UX-37~43）在下面**合并成一行**，
> 因此行数少于条目数（P1 为 29 行 / 30 条，P2 为 36 行 / 45 条）。

**P0（2）**

| 编号 | 主题 | 位置 |
|---|---|---|
| [P0-2](#p0-2-关窗退出不拦未保存标签) | 关窗不拦未保存标签 | 三 · 3.1 |
| [P0-3](#p0-3-服务编辑面板切换即丢弃未保存编辑6-条触发路径) | 编辑面板切走丢编辑（6 条触发路径） | 三 · 3.1 |

**P1（30）**

| 编号 | 主题 | 位置 |
|---|---|---|
| [SEC-4](#sec-4-文件访问白名单可通过-ipc-自我放宽-新增已实证) | 白名单根可被 IPC 自我放宽 | 二 · 2.1 |
| [SEC-8](#22-承前轮仍开放) | 环境变量明文存库与展示 | 二 · 2.2 |
| 3.3 | [切项目后项目根下的文件无法保存](#33-跨项目切换后无法保存文件-新增) | 三 · 3.3 |
| [NEW-15](#new-15-服务日志热路径每行一次时间戳多次分配一次-ipc-事件rust-侧无批量-新增) | 日志逐行 emit，Rust 侧无批量 | 三 · 3.2 |
| [PERF-4](#perf-4-关窗清理仍在主线程同步执行-承前) | 关窗清理串行阻塞 | 三 · 3.2 |
| [PERF-8](#perf-8-工具命令输出每行一次整树重渲染-每次重渲染重新-join-新增已实证) | 工具命令每行整树重渲染 | 六 · 6.1 |
| [CQ-3](#cq-3-7-处-eslint-disable-是无效注释-承前) | 7 处 `eslint-disable` 是无效注释 | 四 · 4.2 |
| [CQ-6](#cq-6-前端零测试-承前) | 前端零测试；后端测试不触并发路径 | 四 · 4.2 |
| [CQ-9](#cq-9-死代码-永不触发的逻辑-新增) | `ai_status` 零调用 + 自愈死逻辑 + 未用依赖 | 四 · 4.2 |
| [ARCH-1](#55-架构级缺口按风险排序) | 位置化 SQL 行映射（29 处改动链 / 静默读错列） | 五 · 5.5 |
| [ARCH-3](#55-架构级缺口按风险排序) | 运行态纯内存，重启失忆 | 五 · 5.5 |
| [ARCH-4](#55-架构级缺口按风险排序) | 事件通道仅 3 类 | 五 · 5.5 |
| [ARCH-5](#55-架构级缺口按风险排序) | 契约无同步机制 | 五 · 5.5 |
| [ARCH-7](#55-架构级缺口按风险排序) | 服务可信度不足（无就绪探测/依赖顺序/自动重启） | 五 · 5.5 |
| [UX-1](#ux-1-三个停全部停止服务入口三种行为且会连带清空日志-承前) | "停全部"无确认/busy，且连带清日志 | 七 · 7.1 |
| [UX-2](#ux-2-单服务停止悬停-同样静默清日志-新增) | 单服务停止静默清日志 | 七 · 7.1 |
| [UX-3](#ux-3-日志清空即时无确认不可恢复且全应用没有日志导出-新增) | 日志清空不可恢复 + 无导出 | 七 · 7.1 |
| [UX-6](#ux-6-错误-toast-3-秒消失且无错误历史-承前) | 错误 toast 3 秒消失、无历史 | 七 · 7.2 |
| [UX-8](#ux-8-打开-class-jar-条目最长-15-秒零反馈-新增) | 反编译 15 秒零反馈 | 七 · 7.2 |
| [UX-12](#ux-12-错误文案不可行动-新增) | 错误文案不可行动 | 七 · 7.2 |
| UX-15 / [UX-16](#73-空态-加载态-失败态) | 项目列表混淆加载/失败/空；详情失败像"加载中" | 七 · 7.3 |
| [UX-21](#ux-21-崩溃退出码与时间不可见-承前) | 崩溃退出码与时间不可见 | 七 · 7.4 |
| [UX-25](#ux-25-重要操作只在右键菜单而原生菜单被全局禁用-承前) | 重要操作只在右键菜单 | 七 · 7.5 |
| [UX-26](#ux-26-三个主导航面是纯鼠标-div-onclick服务卡自称-button-但键盘激活会开始拖拽-承前) | 主导航面鼠标专有 | 七 · 7.5 |
| [UX-27](#ux-27-不可见但可聚焦的控件-新增) | 不可见但可聚焦的控件 | 七 · 7.5 |
| [UX-28](#ux-28-ctrlf-在有文件标签打开时被全应用吞掉-新增) | Ctrl+F 被全应用吞掉 | 七 · 7.5 |
| [UX-30](#ux-30-modal-无焦点陷阱初始焦点焦点归还且-escape-会一次关掉所有叠层-承前) | Modal 无焦点陷阱/不归还焦点 | 七 · 7.5 |
| [UX-33](#ux-33-在文档规定的最小宽度下服务编辑抽屉有约-183-px-在屏幕外-新增) | 800×600 下编辑抽屉约 183 px 出屏 | 七 · 7.6 |
| [UX-34](#ux-34-ai-面板宽度不受窗口宽度约束主界面可被压到-0-新增) | AI 面板宽度不受窗口约束 | 七 · 7.6 |

**P2（45）**

| 编号 | 主题 | 位置 |
|---|---|---|
| [SEC-2](#sec-2-zip-校验用的是不可信自述字段这一模式值得单独记住) | "不可信自述字段"模式（`core/ai.rs` 的全量读未设限） | 二 · 2.1 |
| [SEC-6](#sec-6-子-webview-仅校验初始-url无导航守卫端口未固定-新增) | 子 WebView 无导航守卫、端口未固定 | 二 · 2.1 |
| [SEC-9](#22-承前轮仍开放) | 路径校验 TOCTOU | 二 · 2.2 |
| [NEW-9](#new-9-job-object-覆盖仍缺一处-新增部分已修复) | 反编译 JVM 未入 Job Object（部分已修复） | 三 · 3.2 |
| [NEW-14](#new-14-jar-读取每次调用都把整包复制至少两遍-新增) | jar 读取每次整包复制 ≥2 遍 | 三 · 3.2 |
| [PERF-2](#perf-2-无代码分割单-chunk-约-16-mb-新增已实证) | 无代码分割，单 chunk 约 1.6 MB | 六 · 6.1 |
| [PERF-5](#perf-5-搜索结果列表未虚拟化-整店订阅-新增) | 搜索结果未虚拟化 | 六 · 6.1 |
| [PERF-6](#perf-6-每次按键的全量文档处理3-次物化-全量正则-整树重渲染承前本轮大幅加深已实证) | 每按键全量处理 + 整树重渲染（守卫顺序部分已修复） | 六 · 6.1 |
| [PERF-7](#perf-7-csshtmlvue-的颜色色块装饰每次文档变更做全文档正则-新增已实证) | swatch 每次文档变更做全文档正则 | 六 · 6.1 |
| [PERF-9](#perf-9-选区变更扫描o文档行数-且每行分配一次字符串-新增已实证机制) | 选区扫描 O(文档行数) | 六 · 6.1 |
| [PERF-10](#perf-10-每分钟级的小额浪费可选) | 每分钟级的小额浪费合集 | 六 · 6.1 |
| [CQ-1](#cq-1-文件体量超标含生产测试拆分承前) | 文件体量超标（6 个前端 + 3 个 Rust） | 四 · 4.2 |
| [CQ-2](#cq-2-projectdetailtsx-内部重复-新增) | `ProjectDetail` 内部重复（7 组件 / 15 props 透传链） | 四 · 4.2 |
| [CQ-4](#cq-4-类型断言中的真实隐患-新增) | 类型断言剩余隐患（伪造 MouseEvent / `LRLanguage` / `.closest`） | 四 · 4.2 |
| [CQ-5](#cq-5-重复代码每项-3-次含确切位置新增) | 重复代码 10 类（click-outside ×9、菜单项 ×35…） | 四 · 4.2 |
| [CQ-7](#cq-7-工程化缺口本轮可直接落地承前) | 工程化缺口（CI / eslint / prettier / rustfmt） | 四 · 4.2 |
| [CQ-8](#cq-8-构建警告未被处理-新增) | 构建警告未处理（动态+静态双导入等） | 四 · 4.2 |
| [ARCH-2](#55-架构级缺口按风险排序) | 单连接全局锁 | 五 · 5.5 |
| [ARCH-6](#55-架构级缺口按风险排序) | AI 单会话占用制 | 五 · 5.5 |
| [ARCH-8](#55-架构级缺口按风险排序) | 单窗口 / 平台硬编码（`USERPROFILE` 无 cfg 回退） | 五 · 5.5 |
| [UX-4](#ux-4-删除全局打开工具ideavscode-绑定无确认-新增) | 删除工具无确认 | 七 · 7.1 |
| [UX-9](#ux-9-文件搜索无进度不可取消-新增) | 搜索无进度、不可取消 | 七 · 7.2 |
| [UX-10](#ux-10-文件变更自动重启零反馈且静默抹掉该服务日志-新增) | 自动重启零反馈 | 七 · 7.2 |
| [UX-11](#ux-11-重启确认卡从不说明是哪个文件变了-新增) | 重启卡不说变更文件 | 七 · 7.2 |
| [UX-13](#ux-13-工具命令失败后弹窗停在等待执行-新增) | 命令失败停在"等待执行…" | 七 · 7.2 |
| [UX-14](#ux-14-控制台日志缺操作上下文-新增) | console 缺操作上下文 | 七 · 7.2 |
| UX-17 ~ UX-20 | [日志无空态 / 读失败标签像空文件 / 文件树根闪"空目录" / ErrorBoundary 指向不存在的控制台](#73-空态-加载态-失败态) | 七 · 7.3 |
| [UX-22](#ux-22-日志截断只在-tooltip-里交代-承前) | 日志截断未交代 | 七 · 7.4 |
| [UX-23](#ux-23-停止即清日志的产品语义-承前) | "停止即清日志"的语义 | 七 · 7.4 |
| [UX-24](#ux-24-无处可见编码-换行风格-dirty-语言-新增) | 状态栏缺编码/EOL/dirty/语言 | 七 · 7.4 |
| [UX-29](#ux-29-ctrls-只在编辑器有焦点时生效-新增) | Ctrl+S 不全局 | 七 · 7.5 |
| [UX-31](#ux-31-上下文菜单是键盘死胡同-新增) | 菜单键盘死胡同 | 七 · 7.5 |
| [UX-32](#ux-32-分隔条纯鼠标36-px-拖拽目标无-roleseparator无方向键-新增) | 分隔条纯鼠标 | 七 · 7.5 |
| [UX-35](#ux-35-垂直分割只在启动时按-windowinnerheight-算一次resize-后不重算-新增) | 垂直分割 resize 不重算 | 七 · 7.6 |
| [UX-36](#ux-36-搜索面板的保存拖拽高度只在拖拽时夹取-新增) | 搜索面板高度不夹取 | 七 · 7.6 |
| UX-37 ~ UX-43 | [一致性：确认范式 / 停止语义 / 术语漂移 / `show_file_tree` 默认值（7 条）](#77-一致性) | 七 · 7.7 |

---
