//! 开机自启（Windows：当前用户的 Run 键）。
//!
//! 为什么不引 `tauri-plugin-autostart` / `auto-launch`：本仓已经为了读持久化环境变量
//! 引入了 `windows-sys` 的 `Win32_System_Registry`，写一个 Run 值只需要这几个已链接的
//! API；再加一个插件等于多一份依赖、权限面与跨平台分支，而这个功能在本产品里只有 Windows。
//!
//! **默认关闭**：安装过程不写任何启动项；只有用户在「设置 > 通用」里打开时才写。
//! 值数据是 `"<current_exe>"`，**路径只能来自后端**——命令只收一个 bool，
//! 被攻陷的 webview 没法把任意程序塞进开机启动项（与 SEC-17 的"不接受前端文案"同一思路）。

/// Run 键里本应用的值名（任务管理器「启动」页看到的名字）
#[cfg(windows)]
const VALUE_NAME: &str = "Nexus";
#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// 开机自启是否**对当前这个可执行文件**生效。
///
/// 值存在但指向别的路径（换过安装位置、调试版注册过）也返回 false：界面上这个开关的
/// 含义是"登录时会启动**我现在运行的这份**"，而不是"注册表里有没有叫 Nexus 的值"。
#[tauri::command]
pub fn get_autostart_enabled() -> Result<bool, String> {
    #[cfg(windows)]
    {
        let exe = std::env::current_exe()
            .map_err(|e| format!("获取当前程序路径失败: {}", e))?;
        match win::read_value(RUN_KEY, VALUE_NAME)? {
            None => Ok(false),
            Some(value) => Ok(win::value_matches_exe(&value, &exe)),
        }
    }
    #[cfg(not(windows))]
    {
        Err("开机自启仅 Windows 支持".into())
    }
}

/// 开启 / 关闭开机自启。开启时写入**当前 exe 的绝对路径**（前端不参与路径）。
#[tauri::command]
pub fn set_autostart_enabled(enabled: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        if enabled {
            let exe = std::env::current_exe()
                .map_err(|e| format!("获取当前程序路径失败: {}", e))?;
            if !exe.is_file() {
                return Err(format!(
                    "当前程序路径不存在，无法设置开机自启：{}",
                    exe.display()
                ));
            }
            let value = win::command_line_for(&exe);
            win::write_value(RUN_KEY, VALUE_NAME, &value)
        } else {
            // 幂等：本来就没开也当成功（用户连点两下不该看到错误）
            win::delete_value(RUN_KEY, VALUE_NAME)
        }
    }
    #[cfg(not(windows))]
    {
        let _ = enabled;
        Err("开机自启仅 Windows 支持".into())
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::OsString;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ,
    };

    /// 打开的子键句柄，离开作用域自动关闭（早退路径也不泄漏句柄）
    struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn last_error(action: &str, subject: &str, code: u32) -> String {
        format!("{} {} 失败（Win32 错误 {}）", action, subject, code)
    }

    /// 打开 HKCU 下的子键。Run 键是系统预置的；打开失败就如实报错，不偷偷创建。
    fn open(subkey: &str, access: u32) -> Result<Key, String> {
        let sub = wide(subkey);
        let mut hkey: HKEY = std::ptr::null_mut();
        let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, access, &mut hkey) };
        if rc != ERROR_SUCCESS {
            return Err(last_error("打开注册表键", subkey, rc));
        }
        Ok(Key(hkey))
    }

    /// 读一个字符串值；值不存在返回 `None`（"没开"与"读失败"必须分开）。
    pub fn read_value(subkey: &str, value_name: &str) -> Result<Option<String>, String> {
        let key = open(subkey, KEY_QUERY_VALUE)?;
        let name = wide(value_name);

        // 先问长度，再分配（路径长度不固定；写死一个上限会在长路径上截断）
        let mut ty = 0u32;
        let mut size = 0u32;
        let rc = unsafe {
            RegQueryValueExW(
                key.0,
                name.as_ptr(),
                std::ptr::null(),
                &mut ty,
                std::ptr::null_mut(),
                &mut size,
            )
        };
        if rc == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if rc != ERROR_SUCCESS {
            return Err(last_error("读取注册表值", value_name, rc));
        }
        if ty != REG_SZ {
            return Err(format!(
                "注册表值 {} 的类型不是字符串（type={}）",
                value_name, ty
            ));
        }
        if size == 0 {
            return Ok(Some(String::new()));
        }

        let mut buf = vec![0u8; size as usize];
        let mut size2 = size;
        let rc = unsafe {
            RegQueryValueExW(
                key.0,
                name.as_ptr(),
                std::ptr::null(),
                &mut ty,
                buf.as_mut_ptr(),
                &mut size2,
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(last_error("读取注册表值", value_name, rc));
        }
        let units: Vec<u16> = buf[..size2 as usize]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        let s = String::from_utf16_lossy(&units);
        Ok(Some(s.trim_end_matches('\0').to_string()))
    }

    /// 写一个字符串值（REG_SZ，含结尾 NUL）。
    pub fn write_value(subkey: &str, value_name: &str, value: &str) -> Result<(), String> {
        let key = open(subkey, KEY_SET_VALUE)?;
        let name = wide(value_name);
        let data: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = data.len() * 2;
        let rc = unsafe {
            RegSetValueExW(
                key.0,
                name.as_ptr(),
                0,
                REG_SZ,
                data.as_ptr() as *const u8,
                bytes as u32,
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(last_error("写入注册表值", value_name, rc));
        }
        Ok(())
    }

    /// 删除一个值；本来就不存在算成功（幂等）。
    pub fn delete_value(subkey: &str, value_name: &str) -> Result<(), String> {
        let key = open(subkey, KEY_SET_VALUE)?;
        let name = wide(value_name);
        let rc = unsafe { RegDeleteValueW(key.0, name.as_ptr()) };
        if rc == ERROR_SUCCESS || rc == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(last_error("删除注册表值", value_name, rc))
        }
    }

    /// Run 值的数据：`"C:\path\Nexus.exe"`。
    ///
    /// 引号是必须的：路径带空格时，不带引号的 Run 值会被 Windows 拆成
    /// `C:\Program` + `Files\...`，登录时启动失败且没有任何提示。
    pub fn command_line_for(exe: &Path) -> String {
        let mut s = OsString::from("\"");
        s.push(exe.as_os_str());
        s.push("\"");
        s.to_string_lossy().to_string()
    }

    /// 存的值是否指向当前 exe（去引号、去空白、ASCII 大小写不敏感）。
    pub fn value_matches_exe(value: &str, exe: &Path) -> bool {
        let v = value.trim().trim_matches('"').trim();
        !v.is_empty() && v.eq_ignore_ascii_case(&exe.to_string_lossy())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 值比较：引号、大小写、空白都不能让它误判；指向别的程序必须判 false。
    #[test]
    fn test_autostart_value_matches_current_exe() {
        #[cfg(windows)]
        {
            let exe = std::path::Path::new(r"C:\Program Files\Nexus\Nexus.exe");
            assert!(win::value_matches_exe(
                r#""C:\Program Files\Nexus\Nexus.exe""#,
                exe
            ));
            assert!(win::value_matches_exe(
                r#"  c:\program files\nexus\nexus.exe  "#,
                exe
            ));
            assert!(!win::value_matches_exe(r#""C:\Other\Nexus.exe""#, exe));
            assert!(!win::value_matches_exe("", exe));
            assert_eq!(
                win::command_line_for(exe),
                r#""C:\Program Files\Nexus\Nexus.exe""#,
                "带空格的路径必须整体加引号"
            );
        }
    }

    /// **真机跑一次**（铁律 19）：在真实的 Run 键下用一次性值名走
    /// 写 → 读 → 比较 → 删，验证 FFI 的参数与 UTF-16 编码。
    ///
    /// 用一次性值名而不是 `Nexus`：不碰用户真实的开机自启设置。值的路径指向不存在的
    /// 文件，即使清理失败，登录时也只会启动失败，不会拉起别的程序。
    ///
    /// ```text
    /// cargo test --lib -- --ignored autostart_registry --nocapture
    /// ```
    #[test]
    #[ignore]
    fn test_autostart_registry_roundtrip() {
        #[cfg(windows)]
        {
            struct Cleanup(String);
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    let _ = win::delete_value(RUN_KEY, &self.0);
                }
            }

            let name = format!("NexusSelfTest-{}", std::process::id());
            let _guard = Cleanup(name.clone());
            let value = r#""C:\nonexistent-nexus-selftest\nexus.exe""#;

            win::write_value(RUN_KEY, &name, value).expect("写入测试值");
            let got = win::read_value(RUN_KEY, &name).expect("读取测试值");
            assert_eq!(got.as_deref(), Some(value));

            win::delete_value(RUN_KEY, &name).expect("删除测试值");
            assert_eq!(win::read_value(RUN_KEY, &name).expect("删除后读取"), None);
        }
    }
}
