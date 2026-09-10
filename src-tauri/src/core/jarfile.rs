//! jar 包浏览（jar = ZIP）：列条目 + 按名读取，支持嵌套 jar（Spring Boot fat jar）
//!
//! 嵌套 jar 的实现：ZIP 解析器接受 `Cursor<Vec<u8>>`，把内层 jar 条目读成字节
//! 再开一层解析即可，支持有限深度嵌套。条目读取防 zip 炸弹：按**实际解压出的字节数**
//! 设限（`f.size()` 是归档自述字段，不可信，见 `read_entry_capped`）。

use std::io::{Cursor, Read};

/// 单个 jar 文件大小上限（fat jar 可达数十 MB；需整文件读入内存解析目录）
pub const MAX_JAR_SIZE: u64 = 100 * 1024 * 1024;
/// 单条目解压上限（与 read_file 的 50MB 查看上限一致，防 zip 炸弹）
pub const MAX_ENTRY_SIZE: u64 = 50 * 1024 * 1024;
/// 嵌套 jar 层数上限（Spring Boot fat jar 实际最多 2~3 层；不设限时单次调用
/// 可叠加 N × MAX_ENTRY_SIZE 的解压与拷贝）
const MAX_NESTED_DEPTH: usize = 4;

/// 有上限地读取一个 zip 条目。
///
/// **为什么不能只信 `f.size()`**：它取自归档自述的 `uncompressed_size` 字段，
/// 攻击者可控；而 `zip` crate 的解压读取链（`Decompressor` → `Crc32Reader`）**不会**
/// 按该字段截断输出——被 `take()` 限制的只有**压缩后**的输入。因此一个
/// "声明 1 KB、实际膨胀到数 GB"的条目能通过声明值检查并撑爆内存。
/// 这里改为对**真实读出的字节数**设限（多读 1 字节以区分"刚好等于上限"与"超限"）。
fn read_entry_capped<R: Read>(reader: &mut R, declared: u64, what: &str) -> Result<Vec<u8>, String> {
    // 不用 declared 预分配：声明值不可信，伪造的 50MB 声明会造成无谓内存尖峰
    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    let read = reader
        .take(MAX_ENTRY_SIZE + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("读取{}失败: {}", what, e))?;
    if read as u64 > MAX_ENTRY_SIZE {
        return Err(format!(
            "{}过大（实际超过 {} MB 上限；归档声明 {:.1} MB，疑似 zip 炸弹）",
            what,
            MAX_ENTRY_SIZE / (1024 * 1024),
            declared as f64 / (1024.0 * 1024.0)
        ));
    }
    Ok(buf)
}

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
    // 按名排序（大小写不敏感）：sort_by_key 每个条目只转换一次，避免比较时反复 to_lowercase
    out.sort_by_key(|e| e.name.to_lowercase());
    Ok(out)
}

/// 读取 jar 条目字节；nested 为嵌套 jar 的条目路径链（外层 → 内层）
pub fn read_entry(jar_bytes: &[u8], nested: &[String], name: &str) -> Result<Vec<u8>, String> {
    if nested.len() > MAX_NESTED_DEPTH {
        return Err(format!("嵌套 jar 层数过多（{} 层，上限 {}）", nested.len(), MAX_NESTED_DEPTH));
    }
    let mut current: Vec<u8> = jar_bytes.to_vec();
    // 逐层进入嵌套 jar（Spring Boot BOOT-INF/lib/*.jar）。
    // 内层作用域保证 archive 借用先结束，才能把新字节赋回 current
    for n in nested {
        let buf = {
            let mut archive = open_archive(&current)?;
            let mut f = archive.by_name(n).map_err(|e| format!("找不到嵌套条目 {}: {}", n, e))?;
            let declared = f.size();
            read_entry_capped(&mut f, declared, &format!("嵌套条目 {}", n))?
        };
        current = buf;
    }
    let mut archive = open_archive(&current)?;
    let mut f = archive.by_name(name).map_err(|e| format!("找不到条目 {}: {}", name, e))?;
    let declared = f.size();
    read_entry_capped(&mut f, declared, &format!("条目 {}", name))
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

    /// 把归档里所有本地头 / 中央目录记录声明的 `uncompressed_size` 改成给定值，
    /// 用于伪造"声明很小、实际很大"的 zip 炸弹样本。
    fn forge_declared_uncompressed_size(bytes: &mut [u8], declared: u32) {
        let mut i = 0usize;
        while i + 4 <= bytes.len() {
            let sig = u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
            // 本地文件头 0x04034b50：uncompressed_size 在签名 +22
            // 中央目录头 0x02014b50：uncompressed_size 在签名 +24
            let field = match sig {
                0x0403_4b50 => Some(22),
                0x0201_4b50 => Some(24),
                _ => None,
            };
            if let Some(off) = field {
                let p = i + off;
                bytes[p..p + 4].copy_from_slice(&declared.to_le_bytes());
                i = p + 4;
            } else {
                i += 1;
            }
        }
    }

    /// 回归：zip 炸弹防护必须按**实际解压字节数**判定，而不是归档自述的
    /// `uncompressed_size`。该字段可被伪造成 1KB，而 `zip` crate 的解压链不会
    /// 按它截断输出——只信声明值的实现会放行并撑爆内存。
    #[test]
    fn test_read_entry_rejects_lying_declared_size() {
        // 实际 51MB（全零，压缩后极小），谎报 1KB
        let actual = MAX_ENTRY_SIZE as usize + 1024 * 1024;
        let data = vec![0u8; actual];
        let mut bytes = make_jar(&[("bomb.bin", data.as_slice())]);
        forge_declared_uncompressed_size(&mut bytes, 1024);
        // 前置确认：伪造生效（声明值确实变小了）
        assert!(bytes.len() < 1024 * 1024, "样本应被压缩得很小，实际 {} 字节", bytes.len());

        let err = read_entry(&bytes, &[], "bomb.bin").unwrap_err();
        assert!(
            err.contains("zip 炸弹") || err.contains("过大"),
            "应按实际字节数拒绝伪造声明的条目，实际错误: {}",
            err
        );
    }

    /// 正常条目仍可读取（确认上面的上限没有误伤正常路径）
    #[test]
    fn test_read_entry_accepts_normal_entry_after_cap() {
        let bytes = make_jar(&[("ok.txt", b"hello".as_slice())]);
        assert_eq!(read_entry(&bytes, &[], "ok.txt").unwrap(), b"hello");
    }

    /// 嵌套深度上限：超过上限直接拒绝，不进入解压
    #[test]
    fn test_nested_depth_capped() {
        let bytes = make_jar(&[("a.txt", b"a".as_slice())]);
        let deep: Vec<String> = (0..MAX_NESTED_DEPTH + 1).map(|i| format!("l{}.jar", i)).collect();
        let err = read_entry(&bytes, &deep, "x.txt").unwrap_err();
        assert!(err.contains("嵌套 jar 层数过多"), "应拒绝过深嵌套，实际错误: {}", err);
    }
}
