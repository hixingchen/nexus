# Nexus 项目审查标准

> 当你需要审查项目时，发送：`按照 docs/REVIEW_STANDARD.md 审查项目`

---

## 一、审查范围

每次审查前确认范围：

```
全量审查：整个项目
增量审查：指定文件或目录
改动审查：git diff 的内容
```

---

## 二、审查维度

### 2.1 安全性（P0）

#### Rust 后端

| 检查项 | 合格标准 | 检查方法 |
|--------|----------|----------|
| 路径穿越 | `read_file` 必须校验路径在工作区内（通过 `set_project_root` + `is_path_allowed`） | 检查 `read_file` 命令 |
| 命令注入 | `Command::new` 参数不得直接拼接用户输入 | 检查所有进程执行代码 |
| unwrap 禁止 | 不得使用 `.unwrap()`，除非有注释证明不会 panic | `grep -rn "\.unwrap()" src-tauri/` |
| panic 限制 | `panic!` 仅允许在核心组件初始化失败时使用（需有明确错误信息）；`todo!`/`unimplemented!` 仅允许在开发阶段 | `grep -rn "panic!\|todo!\|unimplemented!" src-tauri/` |
| SQL 注入 | 数据库查询必须使用参数化语句 | 检查所有 SQL 语句 |
| 敏感信息 | 密钥/token 不得硬编码 | 检查所有常量和配置 |

#### React 前端

| 检查项 | 合格标准 | 检查方法 |
|--------|----------|----------|
| XSS 防护 | 用户输入不得直接插入 dangerouslySetInnerHTML | 检查 JSX 渲染 |
| 敏感信息 | API key 不得硬编码在前端代码中 | 检查常量定义 |

---

### 2.2 错误处理（P0）

#### Rust 后端

| 检查项 | 合格标准 |
|--------|----------|
| Result 处理 | 所有 `Result` 必须处理，不得忽略 |
| 错误上下文 | 错误信息必须包含操作上下文（什么操作失败、输入是什么） |
| 错误传播 | 使用 `?` 或 `.map_err()`，不得 `.unwrap()` |
| 外部调用 | 网络/文件/数据库调用必须有错误处理 |

#### React 前端

| 检查项 | 合格标准 |
|--------|----------|
| Tauri invoke | 所有 `invoke` 调用必须有 `try-catch` |
| 用户提示 | 错误发生时必须给用户友好提示（toast） |
| 空 catch 禁止 | catch 块必须处理错误或记录日志，不得留空 |

---

### 2.3 代码质量（P1）

#### 函数设计

| 检查项 | 合格标准 |
|--------|----------|
| 函数长度 | 函数体不超过 30 行 |
| 参数数量 | 参数不超过 4 个，超过则传对象 |
| 嵌套深度 | 嵌套不超过 3 层，超过则提前 return |
| 单一职责 | 一个函数只做一件事 |

#### 命名规范

| 类型 | Rust | TypeScript |
|------|------|------------|
| 变量/函数 | snake_case | camelCase |
| 结构体/类型 | PascalCase | PascalCase |
| 常量 | SCREAMING_SNAKE | SCREAMING_SNAKE |
| 布尔值 | is_/has_/can_ | is/has/can |

#### 注释规范

```
✅ 好的注释：解释"为什么"
// 用 setTimeout 0 确保 DOM 更新后再聚焦
setTimeout(() => ref.current.focus(), 0);

❌ 坏的注释：解释"是什么"
// 设置名字
user.name = "张三";
```

#### 代码卫生

| 检查项 | 合格标准 | 检查方法 |
|--------|----------|----------|
| 死代码 | 不得有未使用的函数、变量、导入 | cargo clippy / eslint |
| 重复代码 | 同一逻辑出现 3 次以上必须抽取 | 人工审查 |
| 魔法数字 | 不得有未解释的数字常量，必须定义为命名常量 | 人工审查 |

---

### 2.4 React 专项（P1）

| 检查项 | 合格标准 |
|--------|----------|
| useEffect 清理 | 有订阅/定时器/监听的 useEffect 必须返回清理函数 |
| 依赖数组 | useEffect/useMemo/useCallback 依赖数组必须完整 |
| 组件大小 | 单个组件文件不超过 400 行（超过需拆分） |
| 重渲染 | 避免不必要的重渲染（合理使用 memo/useMemo/useCallback） |
| any 禁止 | 不得使用 `any` 类型，除非有注释说明原因 |
| console.log | 生产代码不得保留 console.log |

---

### 2.5 Rust 专项（P1）

| 检查项 | 合格标准 |
|--------|----------|
| async/await | IO 密集操作使用 async，避免阻塞调用 |
| 资源释放 | 文件句柄、进程句柄必须及时关闭 |
| 共享状态 | 使用 `Arc<Mutex<T>>` 共享可变状态 |
| 克隆优化 | 避免不必要的 `.clone()`，但为了代码清晰性可以接受 |

---

### 2.6 Tauri 命令（P0）

| 检查项 | 合格标准 |
|--------|----------|
| 输入校验 | 所有 `#[tauri::command]` 参数必须校验 |
| 返回值 | 必须返回 `Result<T, String>` |
| 错误信息 | 错误信息对用户友好，不暴露内部实现 |
| 权限控制 | 敏感操作（删除、执行）需确认 |

---

### 2.7 状态管理（P1）

| 检查项 | 合格标准 |
|--------|----------|
| Store 职责 | Store 只管理状态，异步操作在 service 层 |
| Store 独立 | Store 之间不直接依赖 |
| 类型定义 | Store 的 state 和 action 必须有类型定义 |

---

### 2.8 性能（P2）

| 检查项 | 合格标准 |
|--------|----------|
| 大列表 | 超过 100 条的列表使用虚拟滚动 |
| 异步操作 | 超过 100ms 的操作必须异步 |
| 事件监听 | 组件卸载时必须清理事件监听 |
| 节流防抖 | 高频事件（输入、滚动）使用节流或防抖 |

---

### 2.9 运行时稳定性（P0）

| 检查项 | 合格标准 | 检查方法 |
|--------|----------|----------|
| 重复初始化 | 全局初始化（logger、数据库、静态变量）只能调用一次 | 检查 main.rs 和 lib.rs |
| 初始化顺序 | 依赖关系正确的初始化顺序 | 检查模块加载顺序 |
| panic 处理 | 核心组件初始化失败应有明确错误信息 | 检查 expect/unwrap 使用 |
| 启动检查 | 应用启动时验证关键依赖（数据库、文件系统） | 检查 run() 函数 |

---

### 2.10 并发与线程安全（P0）

| 检查项 | 合格标准 | 检查方法 |
|--------|----------|----------|
| 死锁风险 | 避免嵌套锁，锁顺序一致 | 检查 Mutex 使用 |
| 锁粒度 | 锁持有时间尽量短，不包含 IO 操作 | 检查 lock() 到 drop 的范围 |
| 竞态条件 | 共享状态的读写必须原子化 | 检查 Arc<Mutex<T>> 使用 |
| 线程泄漏 | 线程必须有退出机制，不能无限阻塞 | 检查 JoinHandle 处理 |
| Send/Sync | 跨线程使用的类型必须实现 Send/Sync | 检查 unsafe impl |

---

### 2.11 资源管理（P1）

| 检查项 | 合格标准 | 检查方法 |
|--------|----------|----------|
| 文件句柄 | 文件操作后必须关闭，使用 RAII | 检查 File 的生命周期 |
| 进程清理 | 子进程退出时必须 kill + wait（不 wait 会导致句柄残留） | 检查 ProcessManager 和 TerminalSession |
| PTY 清理 | 终端关闭时必须：①kill 子进程 ②wait 确认退出 ③drop reader/writer ④drop master | 检查 TerminalSession::close() |
| 清理锁竞争 | close_all() 对 tokio::Mutex 使用 try_lock 时必须有重试机制，否则可能跳过清理 | 检查 close_all() 实现 |
| Job Object | Windows 上所有子进程必须加入 Job Object（确保应用退出时自动终止） | 检查 job_object.rs 和 assign 调用 |
| 终端会话清理 | CloseRequested 和 RunEvent::Exit 都必须清理终端会话 | 检查 lib.rs 的事件处理 |
| 内存泄漏 | 避免循环引用，Weak 引用打破循环 | 检查 Arc 使用 |
| 临时数据 | 大数据用完及时释放，不要长期持有 | 检查缓冲区管理 |
| 连接关闭 | 数据库/网络连接用完必须关闭 | 检查连接池管理 |

---

### 2.12 跨文件一致性（P1）

| 检查项 | 合格标准 | 检查方法 |
|--------|----------|----------|
| 命令注册 | 所有 #[tauri::command] 必须在 invoke_handler 注册 | 对比 lib.rs 和 commands/*.rs |
| 类型一致 | 前后端共享类型定义必须一致 | 检查 types/*.ts 和 models.rs |
| 接口契约 | API 参数和返回值必须匹配 | 检查 service.ts 和 commands |
| 事件名一致 | 前端 listen 的事件名必须与后端 emit 的事件名完全匹配 | 检查 listen() 和 emit() 调用 |
| 事件载荷一致 | 前端事件 payload 类型必须与后端序列化结构一致 | 检查 event payload 定义 |
| 导入完整 | 使用的模块必须正确导入 | 检查 use/import 语句 |

---

### 2.13 构建与依赖（P2）

| 检查项 | 合格标准 | 检查方法 |
|--------|----------|----------|
| 依赖版本 | 版本锁定，避免意外升级 | 检查 Cargo.lock/package-lock.json |
| 编译警告 | 无编译警告（#![allow] 需注释原因） | cargo check / tsc --noEmit |
| 未使用代码 | 未使用的函数/变量必须清理或标记 | cargo clippy / eslint |
| feature flags | 不需要的 feature 不要启用 | 检查 Cargo.toml |

---

### 2.14 Tauri 特定（P1）

| 检查项 | 合格标准 | 检查方法 |
|--------|----------|----------|
| 插件初始化 | 插件必须在 Builder 中正确初始化 | 检查 plugin() 调用 |
| 窗口事件 | CloseRequested 必须清理资源（进程、终端、文件监听） | 检查 on_window_event |
| Exit 事件 | RunEvent::Exit 必须完整清理所有资源 | 检查 .run() 中的事件处理 |
| 清理顺序 | 先停进程 → 再关终端 → 最后停监听 | 检查清理代码顺序 |
| 等待时间 | 清理后必须等待足够时间（≥200ms）让线程退出 | 检查 sleep 时长 |
| IPC 安全 | invoke 参数必须校验 | 检查所有 command 函数 |
| 权限配置 | tauri.conf.json 权限最小化 | 检查 capabilities |
| CSP 策略 | 必须设置 Content-Security-Policy | 检查 tauri.conf.json |

---

### 2.15 Windows 进程安全（P0）

> 来源：2026-06-30 排查 conhost.exe 泄漏问题

| 检查项 | 合格标准 | 检查方法 |
|--------|----------|----------|
| CREATE_NO_WINDOW | Windows 上通过 `Command::new` 启动的后台进程必须设置 `creation_flags(0x08000000)`，否则会弹出控制台窗口 | 检查所有 `Command::new` 调用 |
| taskkill 静默 | `taskkill` 等系统命令必须带 `CREATE_NO_WINDOW`，否则每次调用产生一个 conhost.exe | 检查 kill_process_tree 等函数 |
| 进程等待 | `child.kill()` 后必须 `child.wait()`，不 wait 会导致句柄残留 | 检查所有 kill 调用 |
| PTY 伪控制台 | portable_pty 创建的 PTY 会关联 conhost.exe，close 时必须按顺序：kill → wait → drop reader/writer → drop master | 检查 TerminalSession::close() |
| Job Object 唯一性 | 避免重复创建 JobObject，通过 set_job() 传入共享实例 | 检查 JobObject::new() 调用次数 |

```bash
# 检查缺少 CREATE_NO_WINDOW 的 Command::new 调用
grep -rn "Command::new" src-tauri/src/ | grep -v "creation_flags" | grep -v "target_os"

# 检查 kill 后缺少 wait
grep -A2 "\.kill()" src-tauri/src/ | grep -v "wait"
```

---

### 2.16 日志规范（P1）

| 检查项 | 合格标准 | 检查方法 |
|--------|----------|----------|
| 日志级别 | ERROR：操作失败需关注；WARN：可恢复异常；INFO：关键业务节点；DEBUG：调试信息 | 人工审查 |
| 结构化日志 | 使用 `log::info!` 等宏，包含上下文（操作、ID、路径） | 检查日志调用 |
| 敏感信息 | 日志不得输出密码、token、密钥 | `grep -rn "password\|token\|secret" src-tauri/src/` |
| 热路径 | 高频调用路径减少日志（用 DEBUG 级别，生产环境关闭） | 检查循环和回调中的日志 |
| 前端日志 | 生产代码不得保留 `console.log`，错误用 `console.error` | `grep -rn "console\.log" src/` |

---

### 2.17 测试规范（P1）

| 检查项 | 合格标准 | 检查方法 |
|--------|----------|----------|
| 核心逻辑覆盖 | 数据转换、业务规则、边界条件必须有测试 | 检查 tests 目录 |
| 测试命名 | `test_<被测函数>_<场景>_<预期结果>` | 人工审查 |
| 测试独立 | 测试之间不依赖执行顺序，不共享可变状态 | 检查测试代码 |
| 边界值 | 必测空值（null/""/[]）、极值、格式异常 | 检查测试用例 |
| 错误路径 | 错误处理逻辑必须有对应的测试 | 检查 Result 处理的测试 |

---

### 2.18 代码卫生（P1）

| 检查项 | 合格标准 | 检查方法 |
|--------|----------|----------|
| 死代码 | 不得有未使用的函数、变量、导入、类型定义 | cargo clippy / eslint |
| 未使用导出 | 仅内部使用的函数不加 `pub`，仅文件内使用的不加 `export` | 人工审查 |
| 废弃文件 | 不被任何文件引用的整个文件必须删除 | 检查 import 引用链 |
| 未使用状态 | Store 中不得有只写不读的状态字段 | 检查组件是否消费了 store 字段 |
| 冗余 prop | 组件不得有从未传入的 prop | 检查调用方 |
| 全局抑制 | 不得使用 `#![allow(...)]` 全局抑制警告，应逐项处理 | 检查 lib.rs 和 crate 根 |

```bash
# 检查未使用的 Rust pub 函数（需结合代码审查）
cd src-tauri && cargo clippy -- -W clippy::all 2>&1 | grep "dead_code\|unused"

# 检查未使用的 TypeScript 导出
# 手动方法：对每个 export 搜索是否有外部 import
```

---

### 2.19 架构设计（P1）

| 检查项 | 合格标准 | 检查方法 |
|--------|----------|----------|
| 目录结构 | 前后端分离，职责清晰 | 检查 src/ 和 src-tauri/ 结构 |
| 模块划分 | 单一职责，高内聚低耦合 | 检查模块依赖关系 |
| 循环依赖 | 不得有循环依赖 | 检查 import/use 语句 |
| 文件大小 | 组件不超过 400 行，模块不超过 500 行 | `wc -l` 统计 |
| 未使用代码 | 不得有未使用的函数、变量、导入 | cargo clippy / eslint |

---

### 2.20 依赖安全（P2）

| 检查项 | 合格标准 | 检查方法 |
|--------|----------|----------|
| 已知漏洞 | 依赖不得有已知安全漏洞 | `pnpm audit` / `cargo audit` |
| 版本锁定 | 生产依赖必须锁定版本 | 检查 lock 文件 |
| 许可证 | 依赖许可证必须兼容 | 检查 LICENSE 文件 |

---

## 三、审查输出格式

```
## 审查报告

**审查范围**：[全量/增量/改动]
**审查日期**：YYYY-MM-DD
**文件数量**：x 个

### 问题汇总

| 级别 | 数量 | 说明 |
|------|------|------|
| P0 | x | 必须修复（安全、崩溃） |
| P1 | x | 应该修复（质量、逻辑） |
| P2 | x | 建议改进（性能、规范） |

### P0 问题（必须修复）

1. **[文件:行号]** 问题描述
   - 影响：会导致什么后果
   - 修复：具体修复方案

### P1 问题（应该修复）

1. **[文件:行号]** 问题描述
   - 修复：具体修复方案

### P2 问题（建议改进）

1. **[文件:行号]** 问题描述
   - 建议：改进方向

### 审查结论

- [ ] 通过：无 P0 问题
- [ ] 待修复：有 P0 问题需修复后重新审查
```

---

## 四、审查命令

### 4.1 AI 审查指令

```
# 全量审查
按照 docs/REVIEW_STANDARD.md 审查项目

# 审查指定目录
按照 docs/REVIEW_STANDARD.md 审查 src-tauri/src/commands/

# 审查指定文件
按照 docs/REVIEW_STANDARD.md 审查 src/commands/process.rs

# 审查最近改动
按照 docs/REVIEW_STANDARD.md 审查最近的 git 改动

# 只审查安全性
按照 docs/REVIEW_STANDARD.md 的安全性维度审查项目

# 快速检查（只报告 P0）
按照 docs/REVIEW_STANDARD.md 审查项目，只报告 P0 问题
```

### 4.2 自动化检查命令

```bash
# ===== 前端检查 =====

# TypeScript 类型检查
pnpm tsc --noEmit

# ESLint 检查
pnpm eslint src/ --ext .ts,.tsx

# 依赖漏洞检查
pnpm audit

# ===== 后端检查 =====

# Rust 编译检查
cd src-tauri && cargo check

# Clippy 静态分析
cd src-tauri && cargo clippy -- -W clippy::all

# Rust 依赖漏洞检查
cd src-tauri && cargo audit

# ===== 文件大小检查 =====

# 前端文件大小（超过 400 行需关注）
find src -name "*.tsx" -o -name "*.ts" | xargs wc -l | sort -rn | head -20

# 后端文件大小（超过 500 行需关注）
find src-tauri/src -name "*.rs" | xargs wc -l | sort -rn | head -20

# ===== 未使用代码检查 =====

# 检查未使用的导入
grep -rn "import.*from" src --include="*.tsx" --include="*.ts" | head -20

# 检查未使用的导出
grep -rn "export" src --include="*.ts" | head -20

# ===== 安全检查 =====

# 检查 unwrap 使用
grep -rn "\.unwrap()" src-tauri/src/

# 检查 panic 使用
grep -rn "panic!\|todo!\|unimplemented!" src-tauri/src/

# 检查 dangerouslySetInnerHTML
grep -rn "dangerouslySetInnerHTML" src/

# 检查硬编码密钥
grep -rn "password\|secret\|api_key\|token" src-tauri/src/ -i

# 检查 console.log
grep -rn "console\.log" src/

# 检查 any 类型
grep -rn ": any" src/
```
