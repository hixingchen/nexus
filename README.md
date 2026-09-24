# Nexus

开发环境管理平台：把多个项目的多个服务（前端/后端/中间件…）集中在一处启停、看日志、改配置、翻代码，基于 **Tauri 2 + React + TypeScript** 构建，**以 Windows 为主平台**（macOS/Linux 有对应的命令分支，但进程树清理、文件剪贴板互通等能力为 Windows 实现；这些分支可编译但未参与打包、未经测试，见 `src-tauri/src/commands/editor.rs` 顶部的平台分支约定）。

## 功能特性

### 项目与服务
- 项目列表管理：置顶、搜索、拖拽排序、复制项目（含服务配置与"打开工具"绑定）
- 多服务配置：命令、工作目录、环境变量、监听路径、监听包含/排除、启停顺序
- 一键启动/停止项目全部服务；单个服务的启动/停止/重启与状态指示
- 意外退出（崩溃/秒退/启动失败）标记为"失败"，日志保留可直接查因
- 用 `start` 另开窗口的服务（Tomcat 的 `startup.bat`、ActiveMQ 的 wrapper 等）会被**接管**：
  状态不再误报"已退出（退出码 0）"，点停止能把它一并结束
- 服务模板库：把服务配置另存为模板，跨项目复用
- 打开工具库：为服务绑定外部工具（IDEA 等），右键"用 XX 打开"
- 工具命令：为服务配置常用命令（构建/清理…），流式输出、可超时、可手动停止、可排序（顺序即右键菜单顺序）

### 日志
- 实时日志面板（虚拟滚动，每服务保留最新 2000 行 / 2MB）
- 暂停查看、搜索高亮与命中跳转、级别与 URL/IP/时间戳着色
- 运行状态 3 秒轮询，崩溃服务保留日志供诊断
- 上面那类"另开窗口"的服务：自动跟随它自己写的日志文件（按"确实在写入"挑选文件，
  回看最近 64KB 后持续追加），右键服务可取消跟随

### 文件与代码
- 文件树浏览、懒加载、jar 虚拟子树展开、系统剪贴板复制/粘贴（目录行「粘贴到此处」、文件行「粘贴到同级」；
  重名**弹框问一次**：保留两者（落成 " (2)"）/ 覆盖（原项移入回收站）/ 跳过，没有重名就直接粘、不多一步；
  报出实际落地名与被替换的原项，被跳过的符号链接/联接点单独说明、不混进"已粘贴 N 个项目"）
- 文件树右键删除：文件与目录都可删，**移入系统回收站**（无法回收时会被永久删除，如超过回收站配额的大文件）；指向被删对象的编辑器标签会一并关闭
- 代码查看与编辑（CodeMirror 6，20+ 语言），UTF-8/GB18030 自动识别，换行风格与编码按原样写回（编辑保存不产生 git 误报变更）
- 内建查看器：图片预览、十六进制分页视图、jar 浏览（含嵌套 fat jar）
- `.class` 文件：CFR 反编译为 Java 源码（无 JRE 时回退字节码视图）
- 跨文件内容搜索（按服务工作目录，支持扩展名筛选）

### 文件监听
- 监听路径变化触发服务重启：关闭 / 确认重启 / 自动重启（带 2 秒冷却去抖）三档
- 项目级总开关 + 单服务级控制；排除 `node_modules`/`.git`/`dist` 等目录

### AI 助手
- 以原生子 WebView 内嵌 **DeepSeek Harness（dsh）** Web GUI，按项目托管会话（工作目录 = 当前项目）
- 会话生命周期自管：幂等复用、目录变更重启、停止/升级、就绪探测
- 支持检查/安装/升级 dsh

### 界面
- 深色主题、自定义标题栏、可拖拽面板布局、布局状态持久化
- 全局搜索弹窗、文件/目录二次定位、快捷键（Ctrl+S 保存、Ctrl+F 查找、Ctrl+Z 撤销）

## 第三方组件

- CFR（[MIT License](src-tauri/resources/cfr-LICENSE.txt)）：Java 字节码反编译，jar 随应用内嵌分发

## 技术栈

| 层 | 技术 |
|---|---|
| 前端 | React 18, TypeScript（strict）, Tailwind CSS, Zustand, CodeMirror 6 |
| 桌面框架 | Tauri 2 |
| 后端 | Rust（rusqlite / notify / tokio / zstd） |
| 构建 | Vite + pnpm + cargo |

**依赖版本锁定**：`.npmrc` 的 `save-exact=true` 让直接依赖精确落盘，`pnpm-lock.yaml` 入库，
CI 与发布流程一律 `pnpm install --frozen-lockfile`。`package.json` 的 `pnpm.overrides` 是四条
**传递依赖的版本地板**（`package.json` 不支持注释，理由记在这里；四条由提交 `455a603` 的审计收口引入）：

| 被钉的包 | 区间 | 由谁传递引入 | 区间为什么这样写 |
|---|---|---|---|
| `nanoid` | `>=3.3.18 <7` | 仅 postcss | 下界是引入时的值；`>=` 是开区间，不加上界会把任何新主版本自动拉进来——实测被拉到 6.0.1，而 postcss 自己声明的是 `^3.3.18`（跨两个主版本，没人验过） |
| `postcss` | `>=8.5.23 <9` | tailwindcss / vite / postcss-import / postcss-js / postcss-load-config / postcss-nested，同时也是本仓直接 devDependency | 同上；上界取当前 8.5.28 的下一个主版本 |
| `browserslist` | `>=4.28.7 <5` | autoprefixer、`@babel/helper-compilation-targets`（经 `@vitejs/plugin-react`） | 同上；上界取当前 4.29.0 的下一个主版本 |
| `baseline-browser-mapping` | `>=2.11.0 <3` | 仅 browserslist | 同上；上界取当前 2.11.25 的下一个主版本 |

> 区间只写在 `package.json` 一处（本表是它的说明，不是第二份可执行配置）。改动区间后
> `pnpm install --frozen-lockfile` 会先报 `ERR_PNPM_LOCKFILE_CONFIG_MISMATCH`——`pnpm-lock.yaml`
> 的 `overrides:` 段记录的是区间原文，需要 `pnpm install --no-frozen-lockfile` 同步一次
> （本次同步只改了区间字符串，解析版本与 integrity 未变）。

## 开发环境

### 环境要求
- Node.js >= 18、pnpm >= 8
- Rust >= 1.70（Windows 需 MSVC 工具链）
- Java（可选）：仅 `.class` 反编译需要本机 JRE；缺失时自动回退字节码视图

### 常用命令

```bash
pnpm install          # 安装依赖
pnpm dev              # 启动开发模式（tauri dev）
pnpm dev:frontend     # 仅启动前端（vite，端口 1420）
pnpm build            # 前端类型检查 + 构建（tsc && vite build）
pnpm build:app        # 打包应用（tauri build）
```

### 校验

改动后本地跑一遍即可：

```bash
pnpm exec tsc --noEmit                       # 前端类型检查（0 错误，含 vite.config.ts）
pnpm exec vite build                         # 前端产物必须能构建（0 警告是硬门禁）
pnpm test                                    # 前端测试（utils 纯逻辑 + editor store 状态机）
pnpm lint                                    # eslint（0 错误 / ≤6 条存量告警基线）
cd src-tauri && cargo test --lib             # 后端单元测试（用例数见输出）
cd src-tauri && cargo test --lib -- --ignored  # 默认被跳过的本机资源测试（见下）
cd src-tauri && cargo clippy --all-targets   # 后端静态分析（0 警告）
```

> **`--ignored` 那一批会动本机资源**：剪贴板、回收站（真的删文件）、目录联接点、nvm 探测，
> 所以默认不跑、也不进 CI 的测试步骤（CI 只跑 `cargo test --lib -- --ignored --list`，
> 保证它们仍能编译，不实际执行）。改动这几个模块（`commands/fileops.rs`、`commands/node.rs`、
> `core/winenv.rs`）后请在真机上手动跑一次。
>
> 用例数**以命令输出为准**，不要照抄文档里的数字（写死的数字正是它出错的原因）。
> 前端测试只覆盖 `.ts` 模块链（utils / stores / services）——JSX 无法被 Node 的类型擦除处理，
> 组件仍靠真机点检；解析钩子见 `test/ts-hooks.mjs`。
> 测试分布：Rust 侧覆盖 `core/` 与 `commands/` 的纯逻辑与边界（`spawn_guard.rs` 是源码扫描式
> 守卫，`contract.rs` 是 IPC 字段契约）；前端集中在 `utils/` 与 `editor` store。

## 行为约定（容易踩到的几条）

- **关窗有确认**：有未保存的文件或服务配置时，关闭窗口会先确认（保存全部 / 放弃 / 取消）。
- **路径白名单**：文件 API 只能访问已登记项目目录（及各服务/模板工作目录）。项目外的目录需要经「选择目录」原生对话框确认一次；AI 会话的工作目录必须是已登记项目目录。
- **AI 面板是原生子 WebView**：它永远盖在所有网页元素之上（z-index 无效），因此面板网页内的鼠标右键被禁用，弹窗/浮层会主动避开面板区域。
- **环境变量默认遮蔽**：服务编辑面板里 `KEY=VALUE` 的值默认显示为掩码，点「显示值」后可查看与编辑（保存的始终是真实值）。
- **反编译进程受管**：`.class` 反编译用的 JVM 有 256MB 堆上限，并纳入应用级 Job Object（Nexus 退出/被强杀时不残留）。

## 数据与日志位置

| 内容 | 路径 |
|---|---|
| 数据库（项目/服务/模板/工具/布局） | `~/.nexus/nexus.db`（SQLite，WAL） |
| 应用日志 | `~/.nexus/logs/nexus.log`（超过 5MB 轮转为 `nexus.log.1`） |
| 启动致命错误 | `~/.nexus/logs/startup-error.log` |

> 打包版没有控制台，**排查问题请看 `~/.nexus/logs/nexus.log`**；开发期 `RUST_LOG=debug` 可提高日志级别。

## 项目结构

```
nexus/
├── src/                          # 前端
│   ├── components/
│   │   ├── layout/               # 布局、项目列表/详情、服务列、模板列、弹窗
│   │   ├── editor/               # 代码视图、标签栏、jar/hex/图片查看器
│   │   ├── file-tree/            # 文件树（懒加载 + jar 虚拟子树）
│   │   ├── terminal/             # 服务日志面板
│   │   ├── ai/                   # 内嵌 dsh 面板
│   │   └── ui/                   # 通用件（Modal/Toast/ErrorBoundary）
│   ├── stores/                   # Zustand 状态（编辑器/日志/运行态/AI/工具/布局/服务动作）
│   ├── services/                 # Tauri invoke 封装（所有 IPC 的唯一入口）
│   ├── hooks/                    # 业务编排 hooks（含工具命令执行）
│   └── utils/                    # 日志格式化、错误归一化、查找、配色等纯函数
├── src-tauri/                    # Rust 后端
│   ├── src/
│   │   ├── commands/             # Tauri 命令（IPC 边界）
│   │   ├── core/                 # 进程、监听、AI 会话、class/jar 解析、反编译、Job Object
│   │   ├── database/             # rusqlite + schema 增量迁移
│   │   ├── contract.rs           # IPC 字段命名契约测试（响应 snake_case / 请求 camelCase）
│   │   └── logger.rs             # 控制台 + 文件双写日志
│   └── Cargo.toml
├── tailwind.config.js
├── vite.config.ts
└── package.json
```

## 许可证

MIT
