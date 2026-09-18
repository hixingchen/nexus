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
];

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
         ① 是我们自己拉起的内部命令 → 加 `.env(\"NoDefaultCurrentDirectoryInExePath\", \"1\")`\
         （或改用 `core::process::build_internal_command`）；\n\
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
