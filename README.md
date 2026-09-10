# Nexus

开发环境管理平台：把多个项目的多个服务（前端/后端/中间件…）集中在一处启停、看日志、改配置、翻代码，基于 **Tauri 2 + React + TypeScript** 构建，**以 Windows 为主平台**（macOS/Linux 有对应的命令分支，但进程树清理、文件剪贴板互通等能力为 Windows 实现）。

## 功能特性

### 项目与服务
- 项目列表管理：置顶、搜索、拖拽排序、复制项目（含服务配置与"打开工具"绑定）
- 多服务配置：命令、工作目录、环境变量、监听路径、监听包含/排除、启停顺序
- 一键启动/停止项目全部服务；单个服务的启动/停止/重启与状态指示
- 意外退出（崩溃/秒退/启动失败）标记为"失败"，日志保留可直接查因
- 服务模板库：把服务配置另存为模板，跨项目复用
- 打开工具库：为服务绑定外部工具（IDEA 等），右键"用 XX 打开"
- 工具命令：为服务配置常用命令（构建/清理…），流式输出、可超时、可手动停止

### 日志
- 实时日志面板（虚拟滚动，每服务保留最新 2000 行 / 2MB）
- 暂停查看、搜索高亮与命中跳转、级别与 URL/IP/时间戳着色
- 运行状态 3 秒轮询，崩溃服务保留日志供诊断

### 文件与代码
- 文件树浏览、懒加载、jar 虚拟子树展开、系统剪贴板复制/粘贴
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

```bash
pnpm exec tsc --noEmit                       # 前端类型检查（应 0 错误）
cd src-tauri && cargo test --lib             # 后端单元测试
cd src-tauri && cargo clippy --all-targets   # 后端静态分析
cd src-tauri && cargo check --all-targets    # 后端编译检查
```

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
│   ├── stores/                   # Zustand 状态（编辑器/日志/运行态/AI/工具/布局）
│   ├── services/                 # Tauri invoke 封装
│   ├── hooks/                    # 业务编排 hooks
│   └── utils/                    # 日志格式化、配色、键盘等纯函数
├── src-tauri/                    # Rust 后端
│   ├── src/
│   │   ├── commands/             # Tauri 命令（IPC 边界）
│   │   ├── core/                 # 进程、监听、AI 会话、class/jar 解析、反编译、Job Object
│   │   ├── database/             # rusqlite + schema 增量迁移
│   │   └── logger.rs             # 控制台 + 文件双写日志
│   └── Cargo.toml
├── docs/
│   └── AUDIT_REPORT.md           # 待处理问题清单（代码审计遗留项）
├── tailwind.config.js
├── vite.config.ts
└── package.json
```

## 许可证

MIT
