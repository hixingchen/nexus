# Nexus

跨平台开发环境管理平台，基于 Tauri 2 + React + TypeScript 构建。

## 功能特性

### 项目管理
- 项目列表管理，支持置顶、搜索
- 多服务配置，每个项目可配置多个服务
- 一键启动/停止所有服务

### 服务管理
- 服务启停控制
- 服务日志实时查看
- 文件监听，支持自动重启
- 工具命令（右键执行常用命令）

### 终端
- 集成终端，自动跟随项目
- 终端初始命令配置
- 右键复制/粘贴

### 文件管理
- 文件树浏览
- 代码查看器
- 在资源管理器中打开

### 界面
- 深色主题
- 自定义标题栏
- 可调整面板布局
- 布局状态持久化

## 技术栈

| 层 | 技术 |
|---|---|
| 前端 | React 18, TypeScript, Tailwind CSS |
| 状态管理 | Zustand |
| 终端 | xterm.js |
| 桌面框架 | Tauri 2 |
| 后端 | Rust |
| 构建工具 | Vite |
| 包管理 | pnpm |

## 开发环境

### 环境要求
- Node.js >= 18
- pnpm >= 8
- Rust >= 1.70

### 安装
```bash
pnpm install
```

### 开发
```bash
pnpm dev
```

### 构建
```bash
pnpm build
pnpm build:app
```

## 项目结构

```
nexus/
├── src/                          # 前端源码
│   ├── components/
│   │   ├── layout/               # 布局组件
│   │   ├── editor/               # 编辑器组件
│   │   ├── terminal/             # 终端组件
│   │   ├── file-tree/            # 文件树组件
│   │   └── ui/                   # 通用组件
│   ├── stores/                   # 状态管理
│   ├── services/                 # API 服务
│   ├── hooks/                    # 自定义 Hooks
│   └── types/                    # 类型定义
├── src-tauri/                    # Rust 后端
│   ├── src/
│   │   ├── commands/             # Tauri 命令
│   │   ├── core/                 # 核心功能
│   │   ├── database/             # 数据库
│   │   └── models.rs             # 数据模型
│   └── Cargo.toml
├── tailwind.config.js
├── vite.config.ts
└── package.json
```

## 许可证

MIT
