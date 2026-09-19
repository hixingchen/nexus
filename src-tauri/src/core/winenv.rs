//! 持久化的 Windows 环境（注册表）——给"启动那一刻的环境块已经过期"兜底。
//!
//! **为什么需要这一层**：Windows 只在**进程启动时**复制一份环境块。Nexus 是个常驻应用，
//! 用户完全可能在它运行期间装 nvm / 装 Node / 改 PATH——这些改动写进了注册表，
//! 但本进程那份副本不会更新。于是出现一类很反直觉的失败：
//!
//! - 面板读不到 `NVM_HOME` → "没检测到 nvm-windows"（其实刚装好）
//! - `nvm.exe` 找不到自己的 `settings.txt`（它按 `os.Getenv("NVM_HOME")` 找家）
//! - `npm install -g dsh` 报"找不到 npm"（npm 在 `NVM_SYMLINK` 软链目录里，
//!   而该目录是装完 nvm 才进 PATH 的）
//!
//! 三种症状同一个根因，共同点是"用户刚做完某件事，界面却要重启应用才认"。
//! 对策是在**拉起子进程时**补上注册表里的值，而不是让用户重启。

use std::process::Command;

/// 系统环境（设置里"系统变量"那一栏）；用户环境就在 `Environment` 下
#[cfg(windows)]
const SYSTEM_ENV: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";

/// 读注册表里某个环境变量。
///
/// `RegGetValueW` 会顺带展开 `REG_EXPAND_SZ` 里的 `%VAR%`（PATH 大量使用这种写法），
/// 所以不需要自己拼串——这也是没用 `winreg` 的原因之一（那个 crate 在依赖图里只是
/// tauri 的**构建期**依赖，拿它当运行时依赖等于白塞一个包）。
#[cfg(windows)]
fn reg_value(root: windows_sys::Win32::System::Registry::HKEY, subkey: &str, name: &str) -> Option<String> {
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{RegGetValueW, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ};

    let sub: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    let val: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ; // 两种字符串类型都要
    let mut size: u32 = 0;
    // 两段式：先空 buffer 问长度，再按长度读（值里可能有任意长度的路径）
    let rc = unsafe {
        RegGetValueW(
            root, sub.as_ptr(), val.as_ptr(), flags,
            std::ptr::null_mut(), std::ptr::null_mut(), &mut size,
        )
    };
    if rc != ERROR_SUCCESS || size < 2 {
        return None; // 没有这个值（或空串）
    }
    let mut buf: Vec<u16> = vec![0; size as usize / 2 + 1];
    let rc = unsafe {
        RegGetValueW(
            root, sub.as_ptr(), val.as_ptr(), flags,
            std::ptr::null_mut(), buf.as_mut_ptr() as *mut c_void, &mut size,
        )
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    // size 回来的是**字节数**，含结尾的 NUL
    let len = (size as usize / 2).saturating_sub(1).min(buf.len());
    let value = clean(&String::from_utf16_lossy(&buf[..len]), name);
    (!value.is_empty()).then_some(value)
}

/// 洗一遍从外部读来的字符串：去掉 NUL、掐掉首尾空白。
///
/// 为什么要去 NUL：`Command::spawn` 会把程序名/参数/环境变量值转成 C 字符串，
/// 只要里面有 NUL 就直接失败，报错原文是 **"nul byte found in provided data"**——
/// 用户看到这句话完全无从下手（哪个值？哪个字段？），而这行字又是我们自己传下去的。
/// 注册表是外部数据，多一个 NUL 不奇怪（写坏的值、别的工具留下的），去掉它比让整条命令
/// 失败合理。去掉时记一条日志：静默修数据是排障的天敌。
pub fn sanitize_for_spawn(s: &str, what: &str) -> String {
    clean(s, what)
}

fn clean(s: &str, what: &str) -> String {
    let had_nul = s.contains('\0');
    let out = s.replace('\0', "");
    if had_nul {
        log::warn!("[nexus] {} 里含 NUL 字节，已剔除（原值 {:?}）", what, s);
    }
    out.trim().to_string()
}

/// 读持久化的环境变量：进程环境块里没有，就去注册表找。
///
/// 用户环境在前：安装器写的是 HKCU（见 nvm 安装包的 `setup.iss` 的 `[Registry]` 段）。
#[cfg(windows)]
pub fn persistent_env(name: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    reg_value(HKEY_CURRENT_USER, "Environment", name)
        .or_else(|| reg_value(HKEY_LOCAL_MACHINE, SYSTEM_ENV, name))
}

#[cfg(not(windows))]
pub fn persistent_env(_name: &str) -> Option<String> {
    None
}

/// 先看进程环境块，没有再问注册表（理由见 `persistent_env`）。
pub fn env_or_persistent(name: &str) -> Option<String> {
    if let Ok(v) = std::env::var(name) {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Some(v);
        }
    }
    persistent_env(name)
}

/// 展开 `%VAR%`：先问进程环境，再问注册表。
///
/// **为什么必须自己展开**：注册表 PATH 大量使用 `%NVM_SYMLINK%`、`%SystemRoot%` 这类引用
/// （nvm 的安装器就是往 PATH 里写 `%NVM_HOME%;%NVM_SYMLINK%`），而 `RegGetValueW` 是拿
/// **本进程的环境**去展开的——用户刚装完 nvm 时本进程恰恰没有 `NVM_SYMLINK`。实测：把变量从
/// 进程环境里摘掉后读 HKLM 的 Path，取回来的仍是**字面量** `%NVM_SYMLINK%`。
/// cmd 不会在 PATH 里再展开它，于是"PATH 里明明有这一项，却找不到 npm"——用户报的
/// "装完 node 必须重启 Nexus 才能装 dsh"就是这个。
///
/// 认不出来的变量原样留着：交给下游同样认不出，但至少不是我们把它变成了空串。
fn expand_vars(s: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                out.push_str(&rest[..start]);
                match lookup(name) {
                    Some(v) => out.push_str(&v),
                    None => out.push_str(&rest[start..start + end + 2]),
                }
                rest = &after[end + 1..];
            }
            None => break, // 落单的 %，整段原样带出
        }
    }
    out.push_str(rest);
    out
}

/// 合并 PATH（纯函数，便于测试）。
///
/// 规则：**进程现有的项按原顺序在前，注册表里有而进程里没有的补在后面**。
/// 这样只增不减——既不丢掉启动它的终端临时加过的路径，又能让"启动之后才装的东西"
/// （nvm 的 `NVM_SYMLINK` 软链目录就是典型）被子进程找到。
///
/// 顺序上不重排：用户重开一个终端时 Windows 给的也是"系统 PATH + 用户 PATH"，
/// 而注册表里的新项本来就排在后面，追加在后面与重开一个进程的结果一致。
///
/// 没有任何新项时返回 `None`（调用方据此跳过设置，避免无谓地改写子进程环境）。
fn merge_path(process_path: &str, registry_paths: &[String]) -> Option<String> {
    // 比较用：Windows 路径不区分大小写，且末尾的反斜杠不影响同一个目录
    fn key(s: &str) -> String {
        s.trim().trim_matches('"').trim_end_matches('\\').to_lowercase()
    }

    // 进程那份同样洗一遍：它也要原样传给子进程，含 NUL 一样会让 spawn 直接失败
    let existing: Vec<String> = process_path
        .split(';')
        .map(|s| clean(s, "PATH（进程）"))
        .filter(|s| !s.is_empty())
        .collect();
    let seen: std::collections::HashSet<String> = existing.iter().map(|s| key(s)).collect();

    let mut added: Vec<String> = Vec::new();
    let mut seen_new: std::collections::HashSet<String> = std::collections::HashSet::new();
    for p in registry_paths {
        for entry in p.split(';') {
            let entry = clean(entry, "PATH（注册表）");
            if entry.is_empty() {
                continue;
            }
            let k = key(&entry);
            if seen.contains(&k) || !seen_new.insert(k) {
                continue;
            }
            added.push(entry);
        }
    }
    if added.is_empty() {
        return None;
    }
    let mut out = existing;
    out.extend(added);
    Some(out.join(";"))
}

/// 子进程用的 PATH：进程现有的 + 注册表（用户 + 系统）里新增的。
pub fn merged_path() -> Option<String> {
    let process_path = std::env::var("PATH").unwrap_or_default();
    let mut registry: Vec<String> = Vec::new();
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
        // 系统在前、用户在后：与 Windows 给新进程拼 PATH 的顺序一致
        //
        // 每个值都要自己展开一遍 `%VAR%`（理由见 `expand_vars`）：注册表里存的是
        // `%NVM_SYMLINK%` 这种引用，本进程缺变量时 RegGetValue 展开不了，原样塞给子进程
        // 等于没有这一项——cmd 不会在 PATH 里再展开它
        if let Some(p) = reg_value(HKEY_LOCAL_MACHINE, SYSTEM_ENV, "Path") {
            registry.push(expand_vars(&p, &env_or_persistent));
        }
        if let Some(p) = reg_value(HKEY_CURRENT_USER, "Environment", "Path") {
            registry.push(expand_vars(&p, &env_or_persistent));
        }
    }
    merge_path(&process_path, &registry)
}

/// 给**应用自己要跑的**子进程补上持久化环境：PATH（含新装的工具）+ nvm 的两个变量。
///
/// 服务命令、`npm install -g dsh`、`nvm.exe` 都走这里。用户的终端不走——那里应当尽量
/// 保留"启动 Nexus 时的那份环境"，语义不同（见 `commands::editor::open_terminal`）。
pub fn apply(cmd: &mut Command) {
    #[cfg(windows)]
    {
        if let Some(path) = merged_path() {
            cmd.env("PATH", path);
        }
        if let Some(home) = env_or_persistent("NVM_HOME") {
            cmd.env("NVM_HOME", home);
        }
        if let Some(link) = env_or_persistent("NVM_SYMLINK") {
            cmd.env("NVM_SYMLINK", link);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merge_path_appends_only_new_entries() {
        let got = merge_path(r"C:\a;C:\b", &[r"C:\b;C:\c".to_string()]).expect("应当有新项");
        assert_eq!(got, r"C:\a;C:\b;C:\c", "进程里已有的保持原序，只把新项补在后面");
    }

    #[test]
    fn test_merge_path_is_case_and_slash_insensitive() {
        // Windows 路径不区分大小写、末尾反斜杠同义——不这样比会把同一个目录重复塞进去，
        // 每次启动服务都让 PATH 长一截
        let got = merge_path(r"C:\Windows\System32", &[r"c:\windows\system32\;C:\Other".to_string()])
            .expect("应当只补 C:\\Other");
        assert_eq!(got, r"C:\Windows\System32;C:\Other");
    }

    #[test]
    fn test_merge_path_none_when_nothing_new() {
        assert!(merge_path(r"C:\a;C:\b", &[r"C:\a".to_string(), r"C:\b".to_string()]).is_none());
        assert!(merge_path(r"C:\a", &[]).is_none());
        // 空项与全空白不算新项（注册表里 PATH 末尾常见一个分号）
        assert!(merge_path(r"C:\a", &[";  ;".to_string()]).is_none());
    }

    #[test]
    fn test_merge_path_ignores_empty_process_path() {
        let got = merge_path("", &[r"C:\a;C:\b".to_string()]).expect("应当补上");
        assert_eq!(got, r"C:\a;C:\b");
    }

    /// NUL 会让 `Command::spawn` 直接失败，报错是没头没尾的
    /// "nul byte found in provided data"（用户就是撞的这条）。外部数据进 spawn 前必须洗。
    #[test]
    fn test_sanitize_for_spawn_strips_nul() {
        assert_eq!(sanitize_for_spawn("C:\\nvm\0\\nvm.exe", "测试"), "C:\\nvm\\nvm.exe");
        assert_eq!(sanitize_for_spawn("  C:\\nvm  ", "测试"), "C:\\nvm");
        assert_eq!(sanitize_for_spawn("\0", "测试"), "");
    }

    #[test]
    fn test_merge_path_cleans_nul_in_entries() {
        // 注册表 PATH 里混进 NUL 时，整条命令行会直接起不来——去掉它而不是让 spawn 失败
        let got = merge_path(r"C:\a", &["C:\\b\0ad;C:\\c".to_string()]).expect("应当补上");
        assert_eq!(got, r"C:\a;C:\bad;C:\c");
    }

    #[test]
    fn test_expand_vars() {
        let lookup = |name: &str| match name {
            "NVM_SYMLINK" => Some(r"C:\nvm4w\nodejs".to_string()),
            "SystemRoot" => Some(r"C:\Windows".to_string()),
            _ => None,
        };
        assert_eq!(
            expand_vars(r"%SystemRoot%\system32;%NVM_SYMLINK%", &lookup),
            r"C:\Windows\system32;C:\nvm4w\nodejs"
        );
        // 认不出来的原样留着——不能变成空串（那等于悄悄删掉 PATH 里的一项）
        assert_eq!(expand_vars(r"C:\a;%NO_SUCH_VAR%\b", &lookup), r"C:\a;%NO_SUCH_VAR%\b");
        // 落单的 %、空名字、空串，都不该 panic 也不该吞字符
        assert_eq!(expand_vars("a%", &lookup), "a%");
        assert_eq!(expand_vars("%%", &lookup), "%%");
        assert_eq!(expand_vars("", &lookup), "");
    }

    /// 用户报的那个：装完 node（没重启 Nexus）后 `npm` 找不到。
    ///
    /// 复现：把 `NVM_HOME` / `NVM_SYMLINK` 从进程环境里摘掉——注册表 PATH 里那两项写作
    /// `%NVM_SYMLINK%` 这种**引用**，本进程缺变量时 `RegGetValueW` 展开不了（实测取回来的是
    /// 字面量），而 cmd 不会在 PATH 里再展开它。合并结果必须落到**真实目录**上。
    ///
    /// ```text
    /// cargo test --lib -- --ignored merged_path_expands --nocapture
    /// ```
    #[test]
    #[ignore]
    fn test_merged_path_expands_symlink_token() {
        let symlink = env_or_persistent("NVM_SYMLINK").expect("本机应当有 NVM_SYMLINK");
        let norm = |s: &str| s.trim().trim_matches('"').trim_end_matches('\\').to_lowercase();
        std::env::remove_var("NVM_SYMLINK");
        std::env::remove_var("NVM_HOME");
        // 进程 PATH 里已有的那一项也去掉，逼它只能从注册表拿
        let stale: Vec<String> = std::env::var("PATH")
            .unwrap_or_default()
            .split(';')
            .filter(|p| norm(p) != norm(&symlink))
            .map(|s| s.to_string())
            .collect();
        std::env::set_var("PATH", stale.join(";"));

        let merged = merged_path().expect("注册表里应当有可补的项");
        let has_real = merged.to_lowercase().contains(&symlink.to_lowercase());
        println!("合并后 PATH 含真实软链目录 {}: {}", symlink, has_real);
        // 注意：**进程自己那份 PATH 里可能有没展开的字面量**（本机 Git-Bash 启的进程就有
        // `%NVM_HOME%:%NVM_SYMLINK%` 这种段），我们对"进程已有的项"是按原样保留的，不重写。
        // 关键是有没有把**真实目录**补进去——cmd 只认真实目录，字面量在它那里等于不存在。
        assert!(has_real, "必须展开成真实目录 {}，否则 npm/node 都找不到", symlink);
    }

    /// 注册表兜底：进程环境块过期时也要能拿到 `NVM_HOME` / `NVM_SYMLINK`。
    ///
    /// 手动跑（注册表内容因机器而异，只能对着真机验）：
    /// ```text
    /// cargo test --lib --manifest-path src-tauri/Cargo.toml -- --ignored persistent_env
    /// ```
    #[test]
    #[ignore]
    fn test_persistent_env_matches_process_env() {
        for name in ["NVM_HOME", "NVM_SYMLINK"] {
            let from_process = std::env::var(name).ok().filter(|v| !v.trim().is_empty());
            let from_registry = persistent_env(name);
            println!("{}: 进程={:?} 注册表={:?}", name, from_process, from_registry);
            if let Some(expected) = from_process {
                // 注册表那份是 REG_EXPAND_SZ（RegGetValue 会展开 %VAR%），
                // 所以它应当与进程环境里的最终值一致
                assert_eq!(
                    from_registry.as_deref(),
                    Some(expected.as_str()),
                    "注册表里的 {} 应当与进程环境里的一致", name,
                );
            }
        }
        assert!(persistent_env("NVM_HOME").is_some(), "本机装了 nvm，注册表里就该有 NVM_HOME");
    }

    /// 用户报的那个场景：**装完 Node 之后 `npm install -g dsh` 找不到 npm，重启 Nexus 才行**。
    ///
    /// 复现方式：把 `NVM_SYMLINK` 软链目录从本进程 PATH 里摘掉（= "Nexus 启动时 node 还没装"），
    /// 再走真实路径跑一条 `npm --version`——`build_command` 是服务命令与 dsh 安装共用的入口。
    /// 它会从注册表把软链目录补回来，所以这条命令必须成功。
    ///
    /// 手动跑（会改本测试进程的 PATH，影响同进程其它测试，所以单独跑）：
    /// ```text
    /// cargo test --lib --manifest-path src-tauri/Cargo.toml -- --ignored child_finds_npm
    /// ```
    #[test]
    #[ignore]
    fn test_child_finds_npm_with_stale_path() {
        let symlink = env_or_persistent("NVM_SYMLINK").expect("本机应当有 NVM_SYMLINK");
        let norm = |s: &str| s.trim().trim_matches('"').trim_end_matches('\\').to_lowercase();
        let stale: Vec<String> = std::env::var("PATH")
            .unwrap_or_default()
            .split(';')
            .filter(|p| norm(p) != norm(&symlink))
            .map(|s| s.to_string())
            .collect();
        std::env::set_var("PATH", stale.join(";"));
        // 连两个 nvm 变量也摘掉 = 用户那台机器"刚装完 nvm、Nexus 还没重启"的完整状态：
        // 注册表 PATH 里那两项写作 %NVM_SYMLINK%，本进程展开不了，只能靠我们自己展开
        std::env::remove_var("NVM_HOME");
        std::env::remove_var("NVM_SYMLINK");
        assert!(
            !std::env::var("PATH").unwrap().to_lowercase().contains(&symlink.to_lowercase()),
            "前提：软链目录已从 PATH 里摘掉",
        );

        // 先看子进程实际拿到的 PATH：这一条不受"本机还装着别的 node"影响，
        // 在本机有第二个 node 时也能证明机制成立
        let out = crate::core::process::build_command("echo %PATH%")
            .output()
            .expect("执行 echo %PATH%");
        let child_path = String::from_utf8_lossy(&out.stdout).to_lowercase();
        println!("子进程 PATH 是否含软链目录: {}", child_path.contains(&symlink.to_lowercase()));
        assert!(
            child_path.contains(&symlink.to_lowercase()),
            "子进程的 PATH 里应当补回了 {}（它就是 npm/node 所在目录）", symlink,
        );

        let out = crate::core::process::build_command("npm --version")
            .output()
            .expect("执行 npm --version");
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        println!("npm --version → ok={} output={:?}", out.status.success(), text);
        assert!(out.status.success(), "PATH 过期时也要能找到 npm");
    }
}
