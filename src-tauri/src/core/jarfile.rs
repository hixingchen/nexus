//! jar 包浏览（jar = ZIP）：列条目 + 按名读取，支持嵌套 jar（Spring Boot fat jar）
//!
//! 嵌套 jar 的实现：ZIP 解析器接受 `Cursor<Vec<u8>>`，把内层 jar 条目读成字节
//! 再开一层解析即可，任意深度递归。条目读取防 zip 炸弹：声明大小超过上限直接拒绝。

use std::io::{Cursor, Read};

/// 单个 jar 文件大小上限（fat jar 可达数十 MB；需整文件读入内存解析目录）
pub const MAX_JAR_SIZE: u64 = 100 * 1024 * 1024;
/// 单条目解压上限（与 read_file 的 50MB 查看上限一致，防 zip 炸弹）
pub const MAX_ENTRY_SIZE: u64 = 50 * 1024 * 1024;

/// jar 条目信息
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JarEntryInfo {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub compressed_size: u64,
}

fn open_archive(bytes: &[u8]) -> Result<zip::ZipArchive<Cursor<&[u8]>>, String> {
    zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("解析 jar 失败（不是合法的 ZIP）: {}", e))
}

/// 列 jar 条目（目录条目跳过——目录结构由文件条目路径隐含，前端按路径展示）
pub fn list_entries(jar_bytes: &[u8]) -> Result<Vec<JarEntryInfo>, String> {
    let mut archive = open_archive(jar_bytes)?;
    let mut out = vec![];
    for i in 0..archive.len() {
        let f = archive.by_index(i).map_err(|e| format!("读取条目失败: {}", e))?;
        if f.is_dir() {
            continue;
        }
        out.push(JarEntryInfo {
            name: f.name().to_string(),
            is_dir: false,
            size: f.size(),
            compressed_size: f.compressed_size(),
        });
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(out)
}

/// 读取 jar 条目字节；nested 为嵌套 jar 的条目路径链（外层 → 内层）
pub fn read_entry(jar_bytes: &[u8], nested: &[String], name: &str) -> Result<Vec<u8>, String> {
    let mut current: Vec<u8> = jar_bytes.to_vec();
    // 逐层进入嵌套 jar（Spring Boot BOOT-INF/lib/*.jar）。
    // 内层作用域保证 archive 借用先结束，才能把新字节赋回 current
    for n in nested {
        let buf = {
            let mut archive = open_archive(&current)?;
            let mut f = archive.by_name(n).map_err(|e| format!("找不到嵌套条目 {}: {}", n, e))?;
            if f.size() > MAX_ENTRY_SIZE {
                return Err(format!("嵌套 jar 过大（{:.1} MB）", f.size() as f64 / (1024.0 * 1024.0)));
            }
            let mut buf = Vec::with_capacity(f.size() as usize);
            f.read_to_end(&mut buf).map_err(|e| format!("读取嵌套条目失败: {}", e))?;
            buf
        };
        current = buf;
    }
    let mut archive = open_archive(&current)?;
    let mut f = archive.by_name(name).map_err(|e| format!("找不到条目 {}: {}", name, e))?;
    if f.size() > MAX_ENTRY_SIZE {
        return Err(format!("条目过大（{:.1} MB），超过 50 MB 查看上限", f.size() as f64 / (1024.0 * 1024.0)));
    }
    let mut buf = Vec::with_capacity(f.size() as usize);
    f.read_to_end(&mut buf).map_err(|e| format!("读取条目失败: {}", e))?;
    Ok(buf)
}

/// 取嵌套链末端的 jar 字节（nested 为空时即外层 jar 自身）
pub fn innermost_archive(jar_bytes: &[u8], nested: &[String]) -> Result<Vec<u8>, String> {
    if let Some((last, prefix)) = nested.split_last() {
        read_entry(jar_bytes, prefix, last)
    } else {
        Ok(jar_bytes.to_vec())
    }
}

/* ---- Tests ---- */

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn make_jar(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            for (name, data) in entries {
                w.start_file(*name, SimpleFileOptions::default()).unwrap();
                w.write_all(data).unwrap();
            }
            w.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn test_list_and_read_jar() {
        let bytes = make_jar(&[
            ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n".as_slice()),
            ("com/x/A.class", &[0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 0]),
        ]);
        let entries = list_entries(&bytes).unwrap();
        assert_eq!(entries.len(), 2, "条目数错误");
        assert!(entries.iter().any(|e| e.name == "META-INF/MANIFEST.MF"));
        assert!(entries.iter().any(|e| e.name == "com/x/A.class"));
        let content = read_entry(&bytes, &[], "META-INF/MANIFEST.MF").unwrap();
        assert_eq!(content, b"Manifest-Version: 1.0\n");
    }

    #[test]
    fn test_nested_jar() {
        let inner = make_jar(&[("hello.txt", b"hi".as_slice())]);
        let outer = make_jar(&[("BOOT-INF/lib/inner.jar", inner.as_slice())]);
        // 穿过嵌套层读取条目
        let content = read_entry(&outer, &["BOOT-INF/lib/inner.jar".to_string()], "hello.txt").unwrap();
        assert_eq!(content, b"hi");
        // 列嵌套层条目
        let innermost = innermost_archive(&outer, &["BOOT-INF/lib/inner.jar".to_string()]).unwrap();
        let entries = list_entries(&innermost).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "hello.txt");
    }

    #[test]
    fn test_read_missing_entry() {
        let bytes = make_jar(&[("a.txt", b"a".as_slice())]);
        assert!(read_entry(&bytes, &[], "nope.txt").is_err());
    }

    #[test]
    fn test_reject_not_zip() {
        assert!(list_entries(b"not a zip at all").is_err());
    }
}
