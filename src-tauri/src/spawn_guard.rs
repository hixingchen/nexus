//! 内部命令收口的守卫测试（GUARD-1），仅测试构建参与编译。
//!
//! **守的是什么**：我们用 `Command::new("裸名")` 拉起"自己人"的命令时（`java`、`taskkill`、
//! `explorer.exe` …），Windows 的 `CreateProcess` 会**先搜当前目录**——而 Nexus 的工作目录
//! 可能正是用户刚打开的那个仓库（里面可以有攻击者预置的同名 exe）。对策是
//! `NoDefaultCurrentDirectoryInExePath=1`（见 `core::process::build_internal_command`）。
//!
//! **为什么需要一条守卫**：这个收口在 4 处各自手写，**新增一处漏写不会报错**，只是悄悄把
//! "当前目录优先"重新打开。SEC-14 那一轮就是这样漏掉了 `java`/`taskkill`（同一收口只铺了
//! 一半），直到下一轮审计才发现——而 GUARD-1 写成之后，**第一次运行就又抓到一处**：
//! `open_in_explorer` 的 `explorer.exe` 同样没收口。这条测试把"新增内部命令"变成一次
//! **显式决定**：要么收口，要么在 `EXEMPT` 里写明它为什么属于用户边界。
//!
//! 扫描是启发式的（与 `contract.rs` 的覆盖守卫同款），**故意宽松**：漏报只是退回今天的行为，
//! 误报的代价是在豁免表里加一行带理由的条目。它的目的是提醒，不是证明。
//!
//! GUARD-2（本文件末尾）守的是另一件同源的事：**起控制台程序时压住它自己的窗口**
//! （打包版 Nexus 是 GUI 子系统、没有控制台，漏了就会在用户桌面上多一个黑窗）。

/// 允许**不做**收口的调用点（键为「相对 `src/` 的路径」+「所属函数」）。
///
/// 用函数名而不是行号：行号会随编辑漂移，函数名不会。每条都必须写明理由——
/// 没有理由的豁免会让这条守卫退化成"维护一张清单"，而不是"守住一条边界"。
const EXEMPT: &[(&str, &str, &str)] = &[
    (
        "commands/editor.rs",
        "open_terminal",
        "用户边界：开出来的终端就是要给用户敲 `gradlew`/`mvnw` 这类裸名命令的，\
         而该环境变量会被子进程继承，收了它们就跑不起来",
    ),
    (
        "commands/tools.rs",
        "spawn_detached",
        "用户边界：跑的是用户自己配置的工具命令整串（与 open_terminal 同一信任级别）",
    ),
    (
        "commands/tools.rs",
        "spawn_tool_program",
        "用户边界：同上；直启分支传的是用户配置的完整可执行路径，不存在 CWD 搜索",
    ),
    (
        "core/process.rs",
        "build_command",
        "用户边界：跑的是用户自己配置的服务命令整串（服务启动的主路径）",
    ),
    (
        "commands/node.rs",
        "run_nvm",
        "绝对路径调用：nvm.exe 的位置由 NVM_HOME 拼出来（见 find_nvm），不经 PATH 解析，\
         也不受「先搜当前目录」影响——与 spawn_tool_program 的直启分支同理。\
         用绝对路径是有意的：Nexus 启动时的 PATH 未必与用户终端一致，裸名可能压根找不到 nvm",
    ),
    ("core/runtime.rs", "direct_command", ABS_PATH_RUNTIME),
];

/// `core/runtime.rs` 那处豁免的理由（该模块的四个 spawn 点都收口到这一个构造函数）。
///
/// 它比设置 `NoDefaultCurrentDirectoryInExePath` **更强一级**：那一级是"名字解析时跳过当前
/// 目录"，这里是"名字解析压根不发生"——传进去的是私有运行时解析出的 `node.exe` /
/// `npm-cli.js` 绝对路径（见 `core::runtime` 的 `resolve` 与 `npm_cli`）。
/// 而这正是整个 `core::runtime` 存在的目的：**让 AI 用哪个 Node 不再由用户的 nvm / PATH 决定**
/// （事故经过见该模块的文档注释）。
const ABS_PATH_RUNTIME: &str = "绝对路径调用：传进去的是私有运行时解析出的 node.exe / npm-cli.js 绝对路径\
     （见 core/runtime 的 resolve 与 npm_cli），不经 PATH 解析、也不受「先搜当前目录」影响。\
     比设置 NoDefaultCurrentDirectoryInExePath 更强的一级——名字解析压根不发生";

/// 一处 `Command::new(`：所在文件、行号、所属函数、以及该行之后的一小段窗口
struct Site {
    file: String,
    line: usize,
    func: String,
    /// 该处是否已显式设置收口环境变量
    hardened: bool,
    /// 该处是否位于非 Windows 的 cfg 分支内（环境变量是 Windows 概念）
    non_windows: bool,
}

/// 扫描生产代码里的所有 `Command::new(`。
///
/// 跳过测试代码：测试里起的进程不随应用分发。判定方式是"文件末尾的 `#[cfg(test)] mod tests`"
/// （不能简单用"第一个 `#[cfg(test)]` 之后就都是测试"——`core/process.rs` 在文件中部也有
/// 一个 `#[cfg(test)]` 属性，它挂在测试辅助函数上）。
fn scan_sites() -> Vec<Site> {
    fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<Site>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, root, out);
                continue;
            }
            if p.extension().and_then(|s| s.to_str()) != Some("rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            // 跳过本文件：它整个是 `#[cfg(test)]` 模块（不进生产构建），而它的注释与错误
            // 文案里就写着 `Command::new(` 这个模式，扫自己只会扫出一堆假阳性
            if p.file_name().and_then(|s| s.to_str()) == Some("spawn_guard.rs") {
                continue;
            }
            let lines: Vec<&str> = text.lines().collect();
            let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
            let prod_end = test_module_start(&lines).unwrap_or(lines.len());

            for (i, line) in lines.iter().enumerate().take(prod_end) {
                if !line.contains("Command::new(") {
                    continue;
                }
                let window = lines[i..(i + 12).min(lines.len())].join("\n");
                out.push(Site {
                    file: rel.clone(),
                    line: i + 1,
                    func: enclosing_fn(&lines, i),
                    hardened: window.contains("NoDefaultCurrentDirectoryInExePath"),
                    non_windows: in_non_windows_block(&lines, i),
                });
            }
        }
    }

    let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

/// 文件末尾测试模块的起始行（`#[cfg(test)]` 紧跟 `mod tests`）。找不到返回 None。
fn test_module_start(lines: &[&str]) -> Option<usize> {
    let mut found = None;
    for i in 0..lines.len() {
        if lines[i].trim() != "#[cfg(test)]" {
            continue;
        }
        let mut j = i + 1;
        while j < lines.len() && lines[j].trim().is_empty() {
            j += 1;
        }
        if j < lines.len() && lines[j].trim_start().starts_with("mod tests") {
            found = Some(i); // 取最后一次（测试模块在文件末尾）
        }
    }
    found
}

/// 向上找最近的 `fn` 名字（`?` 表示没找到——不静默通过，交给断言去报）。
/// 启发式：只认含 `fn ` 且不以 `//` 开头的行，取 `fn ` 之后连续标识符。
fn enclosing_fn(lines: &[&str], at: usize) -> String {
    for line in lines[..at].iter().rev() {
        let t = line.trim_start();
        if t.starts_with("//") || !t.contains("fn ") {
            continue;
        }
        let Some(rest) = t.split("fn ").nth(1) else { continue };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            return name;
        }
    }
    "?".to_string()
}

/// 该行是否落在 `#[cfg(unix)]` / `#[cfg(target_os = "macos"|"linux")]` 之类的非 Windows 块里
/// （向上找最近的 cfg 属性，遇到函数声明就停——块作用域不会跨函数）
fn in_non_windows_block(lines: &[&str], at: usize) -> bool {
    for line in lines[..at].iter().rev() {
        let t = line.trim();
        if t.starts_with("#[cfg(") {
            return t.contains("unix") || t.contains("macos") || t.contains("linux");
        }
        if t.starts_with("fn ") || t.starts_with("pub fn ") || t.starts_with("pub(crate) fn ") {
            return false;
        }
    }
    false
}

/// GUARD-1：生产代码里每一处 `Command::new(` 都必须**收口 / 非 Windows / 明文豁免**。
#[test]
fn test_internal_commands_are_hardened_or_exempt() {
    let sites = scan_sites();
    assert!(
        sites.len() >= 12,
        "只扫到 {} 处 Command::new(，解析器很可能失效了（而非仓库里真这么少）",
        sites.len()
    );

    let mut unhandled = Vec::new();
    for s in &sites {
        if s.hardened || s.non_windows {
            continue;
        }
        if EXEMPT.iter().any(|(f, func, _)| *f == s.file && *func == s.func) {
            continue;
        }
        unhandled.push(format!("{}:{}  fn={}", s.file, s.line, s.func));
    }

    assert!(
        unhandled.is_empty(),
        "以下位置用裸名拉起了进程，但既没有设 `NoDefaultCurrentDirectoryInExePath`，\
         也不在 EXEMPT 里：\n  {}\n\
         处置二选一：\n\
         ① 是我们自己拉起的内部命令 → **首选绝对路径**（`core::runtime` 就是这么钉住 dsh 解释器的）；\
         确实只能按名字起（命令串 / cmd 内建），再加 `.env(\"NoDefaultCurrentDirectoryInExePath\", \"1\")`；\n\
         ② 属于用户边界（用户要在这个进程里敲裸名命令）→ 加进 `spawn_guard.rs` 的 EXEMPT 并写明理由。",
        unhandled.join("\n  ")
    );
}

/// 反向检查：EXEMPT 里的条目必须仍然对应真实存在的调用点。
///
/// 没有这一条时，重构（比如把 `spawn_detached` 改名或改用别的启动方式）之后豁免条目会变成
/// 一张过期清单——看着"都豁免了"，实际守的是不存在的代码。
#[test]
fn test_exempt_list_has_no_stale_entries() {
    let sites = scan_sites();
    let stale: Vec<&str> = EXEMPT
        .iter()
        .filter(|(f, func, _)| !sites.iter().any(|s| s.file == *f && s.func == *func))
        .map(|(_, func, _)| *func)
        .collect();
    assert!(
        stale.is_empty(),
        "EXEMPT 里这些条目在源码中已找不到对应调用点（函数改名/删除了？）：{:?}",
        stale
    );
}

// ─── GUARD-2：起控制台程序的地方必须对"窗口"表过态 ────────────────

/// GUARD-2 的扫描范围（相对 `src/`）：**故意只守这一个文件**。
///
/// 为什么不扫全仓：判定"该不该有窗口"要看调用点的性质——`open_terminal` 有意开一个终端
/// （`CREATE_NEW_CONSOLE`），`open_in_explorer` / `run_installer` 拉的是 GUI 程序
/// （explorer.exe、Inno 安装器本就不建控制台）。把它们一起扫进来只会得到一张越写越长的
/// 豁免表，最后没人看。真正会踩坑的是**我们自己起控制台程序的地方**——今天就是这里
/// （`node.exe`），而它的 spawn 点已全部收口到 `direct_command`，扫一遍就堵死了。
const WINDOW_GUARD_FILE: &str = "core/runtime.rs";

/// 把控制台窗口**压掉**的写法（三选一，都是本仓在用的写法）
const WINDOW_SUPPRESSED: [&str; 3] = ["CREATE_NO_WINDOW", "0x08000000", "SERVICE_CREATE_FLAGS"];
/// 有意**新开**窗口的写法（用户自己要看的终端）
const WINDOW_INTENDED: [&str; 2] = ["CREATE_NEW_CONSOLE", "0x00000010"];

/// GUARD-2：`core/runtime.rs` 里每一处 `Command::new(` 都必须对控制台窗口**表过态**。
///
/// 为什么需要：打包版 Nexus 是 GUI 子系统、自己没有控制台（`main.rs` 的
/// `windows_subsystem = "windows"`），而 `node.exe` 是控制台程序——"父进程无控制台 + 没给
/// `CREATE_NO_WINDOW`"时 `CreateProcess` 会给它**新建一个控制台窗口**。1.9.0 真发生过：
/// 启动链从 `cmd /C`（标志由 `SERVICE_CREATE_FLAGS` 顺带带上）改成绝对路径直起之后，
/// 这里的四个 spawn 点一个都没带标志，于是"打开 AI 面板就闪出一个标题为 `node.exe` 全路径的
/// 黑窗，会话起来后它还一直留着，关掉它连 AI 会话一起关掉"——而**全程不报任何错**。
///
/// 判据是"表过态"而不是"必须压掉"：有意开窗口的写法（`CREATE_NEW_CONSOLE`）同样通过。
/// 这条守卫要的是"作者想过这件事"，不是替他决定该不该有窗口。
#[test]
fn test_runtime_spawns_decide_about_console_window() {
    let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
    let text = std::fs::read_to_string(root.join(WINDOW_GUARD_FILE)).unwrap_or_else(|e| {
        panic!("读不到 {}（改名/移动了？这条守卫要跟着走）：{}", WINDOW_GUARD_FILE, e)
    });
    let lines: Vec<&str> = text.lines().collect();
    let prod_end = test_module_start(&lines).unwrap_or(lines.len());

    let mut spawns = 0;
    let mut undecided = Vec::new();
    for (i, line) in lines.iter().enumerate().take(prod_end) {
        if !line.contains("Command::new(") {
            continue;
        }
        spawns += 1;
        // 与 GUARD-1 同一个窗口大小：`CREATE_NO_WINDOW` 就在 `creation_flags` 那一行上
        let window = lines[i..(i + 12).min(lines.len())].join("\n");
        let decided = WINDOW_SUPPRESSED
            .iter()
            .chain(WINDOW_INTENDED.iter())
            .any(|m| window.contains(m));
        if !decided {
            undecided.push(i + 1);
        }
    }

    assert!(
        spawns >= 1,
        "{} 里一处 `Command::new(` 都没扫到——路径或解析失效，这条守卫正在空转",
        WINDOW_GUARD_FILE
    );
    assert!(
        undecided.is_empty(),
        "{} 的第 {:?} 行起进程时没有对**控制台窗口**表态。打包版 Nexus 没有控制台，\
         控制台子进程会各自新建一个黑窗（标题是 exe 全路径），关掉它还等于关掉那个服务。\n\
         处置二选一：\n\
         ① 压住 → `.creation_flags(CREATE_NO_WINDOW)`（0x08000000；本文件的 spawn 点已全部\
         收口到 `direct_command`，加在那里最省事）；\n\
         ② 确实要让用户看见窗口 → `CREATE_NEW_CONSOLE`（0x00000010）。",
        WINDOW_GUARD_FILE, undecided
    );
}
