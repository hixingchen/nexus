//! .class 文件字节码查看器（javap -c 风格 + 注解类 Java 源码风格渲染）
//!
//! 格式依据 JVM 规范（JVMS）第 4 章：常量池 → 字段/方法 → Code 属性反汇编。
//! 只做解析与反汇编，不做反编译（控制流还原是反编译器的事，超出范围）。
//! 类型引用收集为 import 列表、短名展示（同名冲突自动回退全限定名，同 javac），
//! 注解类（无方法体）渲染为接近源码的形式：
//! ```text
//! // class version 52.0 (Java 8)
//! package com.example.aop;
//!
//! import com.example.LogType;
//! import java.lang.annotation.Target;
//!
//! @Target({ElementType.METHOD})
//! public @interface ApiLog {
//!   LogType type() default LogType.ACCESS;
//! }
//! ```
//! 普通类的方法体仍为字节码（`invokespecial #7 // Method ...`）。

use std::collections::BTreeSet;
use std::fmt::Write as _;

/* ---- 二进制读取器 ---- */

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    fn u1(&mut self) -> Result<u8, String> {
        let b = *self.data.get(self.pos).ok_or_else(|| format!("class 文件截断 @0x{:X}", self.pos))?;
        self.pos += 1;
        Ok(b)
    }

    fn u2(&mut self) -> Result<u16, String> {
        Ok(u16::from_be_bytes([self.u1()?, self.u1()?]))
    }

    fn u4(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes([self.u1()?, self.u1()?, self.u1()?, self.u1()?]))
    }

    fn i1(&mut self) -> Result<i8, String> {
        Ok(self.u1()? as i8)
    }

    fn i2(&mut self) -> Result<i16, String> {
        Ok(self.u2()? as i16)
    }

    fn i4(&mut self) -> Result<i32, String> {
        Ok(self.u4()? as i32)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).ok_or_else(|| format!("class 文件截断 @0x{:X}", self.pos))?;
        let slice = self.data.get(self.pos..end).ok_or_else(|| format!("class 文件截断 @0x{:X}", self.pos))?;
        self.pos = end;
        Ok(slice)
    }
}

/* ---- 常量池 ---- */

#[derive(Debug, Clone)]
enum CpEntry {
    Utf8(String),
    Integer(i32),
    Float(f32),
    Long(i64),
    Double(f64),
    Class(u16),
    String(u16),
    Fieldref(u16, u16),
    Methodref(u16, u16),
    InterfaceMethodref(u16, u16),
    NameAndType(u16, u16),
    MethodHandle(u8, u16),
    MethodType(u16),
    Dynamic(u16, u16),
    InvokeDynamic(u16, u16),
    Module(u16),
    Package(u16),
}

const TAG_UTF8: u8 = 1;
const TAG_INTEGER: u8 = 3;
const TAG_FLOAT: u8 = 4;
const TAG_LONG: u8 = 5;
const TAG_DOUBLE: u8 = 6;
const TAG_CLASS: u8 = 7;
const TAG_STRING: u8 = 8;
const TAG_FIELDREF: u8 = 9;
const TAG_METHODREF: u8 = 10;
const TAG_INTERFACE_METHODREF: u8 = 11;
const TAG_NAME_AND_TYPE: u8 = 12;
const TAG_METHOD_HANDLE: u8 = 15;
const TAG_METHOD_TYPE: u8 = 16;
const TAG_DYNAMIC: u8 = 17;
const TAG_INVOKE_DYNAMIC: u8 = 18;
const TAG_MODULE: u8 = 19;
const TAG_PACKAGE: u8 = 20;

/// 解析常量池（Long/Double 占两个槽位，第二个槽位保持 None）
fn parse_constant_pool(r: &mut Reader) -> Result<Vec<Option<CpEntry>>, String> {
    let count = r.u2()? as usize;
    let mut pool: Vec<Option<CpEntry>> = vec![None; count];
    let mut i = 1;
    while i < count {
        let tag = r.u1()?;
        let entry = match tag {
            TAG_UTF8 => {
                let len = r.u2()? as usize;
                let bytes = r.take(len)?;
                // 常量池 Utf8 为 Modified UTF-8；用宽松解码避免个别字符导致整体失败
                CpEntry::Utf8(String::from_utf8_lossy(bytes).into_owned())
            }
            TAG_INTEGER => CpEntry::Integer(r.i4()?),
            TAG_FLOAT => CpEntry::Float(f32::from_bits(r.u4()?)),
            TAG_LONG => CpEntry::Long((((r.u4()? as u64) << 32) | r.u4()? as u64) as i64),
            TAG_DOUBLE => CpEntry::Double(f64::from_bits(((r.u4()? as u64) << 32) | r.u4()? as u64)),
            TAG_CLASS => CpEntry::Class(r.u2()?),
            TAG_STRING => CpEntry::String(r.u2()?),
            TAG_FIELDREF => CpEntry::Fieldref(r.u2()?, r.u2()?),
            TAG_METHODREF => CpEntry::Methodref(r.u2()?, r.u2()?),
            TAG_INTERFACE_METHODREF => CpEntry::InterfaceMethodref(r.u2()?, r.u2()?),
            TAG_NAME_AND_TYPE => CpEntry::NameAndType(r.u2()?, r.u2()?),
            TAG_METHOD_HANDLE => CpEntry::MethodHandle(r.u1()?, r.u2()?),
            TAG_METHOD_TYPE => CpEntry::MethodType(r.u2()?),
            TAG_DYNAMIC => CpEntry::Dynamic(r.u2()?, r.u2()?),
            TAG_INVOKE_DYNAMIC => CpEntry::InvokeDynamic(r.u2()?, r.u2()?),
            TAG_MODULE => CpEntry::Module(r.u2()?),
            TAG_PACKAGE => CpEntry::Package(r.u2()?),
            other => return Err(format!("不支持的常量池 tag: {} @0x{:X}", other, r.pos - 1)),
        };
        let takes_two = matches!(entry, CpEntry::Long(_) | CpEntry::Double(_));
        pool[i] = Some(entry);
        i += 1;
        if takes_two {
            i += 1;
        }
    }
    Ok(pool)
}

fn cp_utf8(pool: &[Option<CpEntry>], idx: u16) -> String {
    match pool.get(idx as usize).and_then(|e| e.as_ref()) {
        Some(CpEntry::Utf8(s)) => s.clone(),
        _ => String::from("<unknown>"),
    }
}

/// 类/数组类型名：Class 条目需再解一层引用；数组描述符（如 [Ljava/lang/String;）本身是 Utf8
fn cp_class_name(pool: &[Option<CpEntry>], idx: u16) -> String {
    match pool.get(idx as usize).and_then(|e| e.as_ref()) {
        Some(CpEntry::Class(name_idx)) => cp_utf8(pool, *name_idx),
        _ => cp_utf8(pool, idx),
    }
}

/// 方法/字段引用注释：java/lang/Object."<init>":()V
fn cp_member_ref(pool: &[Option<CpEntry>], class_idx: u16, nt_idx: u16) -> String {
    let (name_idx, desc_idx) = match pool.get(nt_idx as usize).and_then(|e| e.as_ref()) {
        Some(CpEntry::NameAndType(n, d)) => (*n, *d),
        _ => return String::from("<unknown>"),
    };
    format!(
        "{}.\"{}\":{}",
        cp_class_name(pool, class_idx),
        cp_utf8(pool, name_idx),
        cp_utf8(pool, desc_idx)
    )
}

/// NameAndType → name:desc
fn cp_name_and_type(pool: &[Option<CpEntry>], nt_idx: u16) -> String {
    match pool.get(nt_idx as usize).and_then(|x| x.as_ref()) {
        Some(CpEntry::NameAndType(n, d)) => format!("{}:{}", cp_utf8(pool, *n), cp_utf8(pool, *d)),
        _ => String::from("<unknown>"),
    }
}

/// 常量池条目的人类可读展示（用于指令操作数注释）
fn cp_display(pool: &[Option<CpEntry>], idx: u16) -> String {
    let Some(Some(e)) = pool.get(idx as usize) else {
        return String::from("<invalid>");
    };
    match e {
        CpEntry::Utf8(s) => format!("\"{}\"", s.replace('"', "\\\"")),
        CpEntry::Integer(v) => format!("int {}", v),
        CpEntry::Float(v) => format!("float {}", v),
        CpEntry::Long(v) => format!("long {}l", v),
        CpEntry::Double(v) => format!("double {}d", v),
        CpEntry::Class(i) => format!("class {}", cp_class_name(pool, *i)),
        CpEntry::String(i) => format!("String {}", cp_utf8(pool, *i)),
        CpEntry::Fieldref(c, n) => format!("Field {}", cp_member_ref(pool, *c, *n)),
        CpEntry::Methodref(c, n) => format!("Method {}", cp_member_ref(pool, *c, *n)),
        CpEntry::InterfaceMethodref(c, n) => format!("InterfaceMethod {}", cp_member_ref(pool, *c, *n)),
        CpEntry::NameAndType(n, d) => format!("NameAndType {}.{}", cp_utf8(pool, *n), cp_utf8(pool, *d)),
        CpEntry::MethodHandle(k, i) => format!("MethodHandle kind:{} #{}", k, i),
        CpEntry::MethodType(i) => format!("MethodType #{}", i),
        // #0 为 BootstrapMethods 属性索引（javap -v 同款展示）
        CpEntry::Dynamic(b, n) => format!("Dynamic #{}:{}", b, cp_name_and_type(pool, *n)),
        CpEntry::InvokeDynamic(b, n) => format!("InvokeDynamic #{}:{}", b, cp_name_and_type(pool, *n)),
        CpEntry::Module(i) => format!("module {}", cp_utf8(pool, *i)),
        CpEntry::Package(i) => format!("package {}", cp_utf8(pool, *i)),
    }
}

/* ---- 类型展示注册表（import 生成） ---- */

/// 收集引用的内部类名 → 生成 import 列表并决定短名/全限定名。
/// java.lang.* 隐式导入不需要 import；同名冲突时全部回退全限定名（同 javac 行为）
#[derive(Default)]
struct TypeRegistry {
    /// 引用的内部类名（去重、排序）
    used: BTreeSet<String>,
    /// 内部名 → 展示名（resolve 后填充）
    display: std::collections::HashMap<String, String>,
    /// 排序后的 import 列表（点分全名，不含 java.lang）
    imports: Vec<String>,
}

impl TypeRegistry {
    fn new() -> Self {
        Self::default()
    }

    fn register(&mut self, internal: &str) {
        if !internal.contains('/') {
            return; // 默认包类或无包名，无需 import
        }
        self.used.insert(internal.to_string());
    }

    /// 计算短名与 import；同名冲突（不同包同短名）回退全限定名
    fn resolve(&mut self) {
        let mut by_short: std::collections::HashMap<String, Vec<&String>> = std::collections::HashMap::new();
        for internal in &self.used {
            let short = internal.rsplit('/').next().unwrap_or(internal);
            by_short.entry(short.to_string()).or_default().push(internal);
        }
        let mut imports = vec![];
        for internal in &self.used {
            let dotted = internal.replace('/', ".");
            let short = internal.rsplit('/').next().unwrap_or(internal);
            let conflicted = by_short.get(short).map_or(false, |v| v.len() > 1);
            if conflicted {
                self.display.insert(internal.clone(), dotted.clone());
            } else {
                self.display.insert(internal.clone(), short.to_string());
                // java.lang 隐式导入；java.lang.annotation 是独立包，仍需显式 import
                let implicit = dotted.starts_with("java.lang.") && !dotted.starts_with("java.lang.annotation.");
                if !implicit {
                    imports.push(dotted);
                }
            }
        }
        imports.sort();
        self.imports = imports;
    }

    fn display(&self, internal: &str) -> String {
        self.display.get(internal).cloned().unwrap_or_else(|| internal.replace('/', "."))
    }
}

/// 描述符中的引用类型注册（扫描所有 L...; 片段；基本类型忽略）
fn register_desc(reg: &mut TypeRegistry, desc: &str) {
    let b = desc.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'L' {
            if let Some(end_rel) = desc[i..].find(';') {
                reg.register(&desc[i + 1..i + end_rel]);
                i += end_rel + 1;
                continue;
            }
        }
        i += 1;
    }
}

/// 类型描述符 → 展示名（数组递归、基本类型映射）
fn display_type(reg: &TypeRegistry, desc: &str) -> String {
    if let Some(rest) = desc.strip_prefix('[') {
        return format!("{}[]", display_type(reg, rest));
    }
    if let Some(inner) = desc.strip_prefix('L').and_then(|s| s.strip_suffix(';')) {
        return reg.display(inner);
    }
    match desc {
        "B" => "byte".into(),
        "C" => "char".into(),
        "D" => "double".into(),
        "F" => "float".into(),
        "I" => "int".into(),
        "J" => "long".into(),
        "S" => "short".into(),
        "Z" => "boolean".into(),
        "V" => "void".into(),
        other => other.to_string(),
    }
}

/// 解析一个类型描述符片段（s[i] 起），返回 (描述符片段, 消耗字节数)
fn parse_type_desc(s: &str, i: usize) -> Option<(String, usize)> {
    let b = s.as_bytes();
    match b[i] {
        b'L' => {
            let end = s[i..].find(';')? + i;
            Some((s[i..=end].to_string(), end + 1 - i))
        }
        b'[' => {
            let (t, n) = parse_type_desc(s, i + 1)?;
            Some((format!("[{}", t), n + 1))
        }
        _ => Some((s[i..i + 1].to_string(), 1)),
    }
}

/// 方法描述符 → (返回类型展示名, 参数类型展示名列表)
fn method_desc_parts(reg: &TypeRegistry, desc: &str) -> Option<(String, Vec<String>)> {
    let (params, rest) = desc.split_once(')')?;
    let params_str = params.strip_prefix('(')?;
    let mut ps = vec![];
    let mut i = 0;
    while i < params_str.len() {
        let (t, n) = parse_type_desc(params_str, i)?;
        ps.push(display_type(reg, &t));
        i += n;
    }
    Some((display_type(reg, rest), ps))
}

/* ---- 访问标志 ---- */

const ACC_PUBLIC: u16 = 0x0001;
const ACC_PRIVATE: u16 = 0x0002;
const ACC_PROTECTED: u16 = 0x0004;
const ACC_STATIC: u16 = 0x0008;
const ACC_FINAL: u16 = 0x0010;
const ACC_SUPER: u16 = 0x0020;
const ACC_SYNCHRONIZED: u16 = 0x0020;
const ACC_VOLATILE: u16 = 0x0040;
const ACC_BRIDGE: u16 = 0x0040;
const ACC_TRANSIENT: u16 = 0x0080;
const ACC_VARARGS: u16 = 0x0080;
const ACC_NATIVE: u16 = 0x0100;
const ACC_INTERFACE: u16 = 0x0200;
const ACC_ABSTRACT: u16 = 0x0400;
const ACC_STRICT: u16 = 0x0800;
const ACC_SYNTHETIC: u16 = 0x1000;
const ACC_ANNOTATION: u16 = 0x2000;
const ACC_ENUM: u16 = 0x4000;
const ACC_MODULE: u16 = 0x8000;

fn class_flags(flags: u16) -> Vec<&'static str> {
    let mut out = vec![];
    if flags & ACC_PUBLIC != 0 { out.push("ACC_PUBLIC"); }
    if flags & ACC_FINAL != 0 { out.push("ACC_FINAL"); }
    if flags & ACC_SUPER != 0 { out.push("ACC_SUPER"); }
    if flags & ACC_INTERFACE != 0 { out.push("ACC_INTERFACE"); }
    if flags & ACC_ABSTRACT != 0 { out.push("ACC_ABSTRACT"); }
    if flags & ACC_SYNTHETIC != 0 { out.push("ACC_SYNTHETIC"); }
    if flags & ACC_ANNOTATION != 0 { out.push("ACC_ANNOTATION"); }
    if flags & ACC_ENUM != 0 { out.push("ACC_ENUM"); }
    if flags & ACC_MODULE != 0 { out.push("ACC_MODULE"); }
    out
}

fn field_flags(flags: u16) -> Vec<&'static str> {
    let mut out = vec![];
    if flags & ACC_PUBLIC != 0 { out.push("ACC_PUBLIC"); }
    if flags & ACC_PRIVATE != 0 { out.push("ACC_PRIVATE"); }
    if flags & ACC_PROTECTED != 0 { out.push("ACC_PROTECTED"); }
    if flags & ACC_STATIC != 0 { out.push("ACC_STATIC"); }
    if flags & ACC_FINAL != 0 { out.push("ACC_FINAL"); }
    if flags & ACC_VOLATILE != 0 { out.push("ACC_VOLATILE"); }
    if flags & ACC_TRANSIENT != 0 { out.push("ACC_TRANSIENT"); }
    if flags & ACC_SYNTHETIC != 0 { out.push("ACC_SYNTHETIC"); }
    if flags & ACC_ENUM != 0 { out.push("ACC_ENUM"); }
    out
}

fn method_flags(flags: u16) -> Vec<&'static str> {
    let mut out = vec![];
    if flags & ACC_PUBLIC != 0 { out.push("ACC_PUBLIC"); }
    if flags & ACC_PRIVATE != 0 { out.push("ACC_PRIVATE"); }
    if flags & ACC_PROTECTED != 0 { out.push("ACC_PROTECTED"); }
    if flags & ACC_STATIC != 0 { out.push("ACC_STATIC"); }
    if flags & ACC_FINAL != 0 { out.push("ACC_FINAL"); }
    if flags & ACC_SYNCHRONIZED != 0 { out.push("ACC_SYNCHRONIZED"); }
    if flags & ACC_BRIDGE != 0 { out.push("ACC_BRIDGE"); }
    if flags & ACC_VARARGS != 0 { out.push("ACC_VARARGS"); }
    if flags & ACC_NATIVE != 0 { out.push("ACC_NATIVE"); }
    if flags & ACC_ABSTRACT != 0 { out.push("ACC_ABSTRACT"); }
    if flags & ACC_STRICT != 0 { out.push("ACC_STRICT"); }
    if flags & ACC_SYNTHETIC != 0 { out.push("ACC_SYNTHETIC"); }
    out
}

/// class 文件 major version → Java 版本
fn java_version(major: u16) -> String {
    match major {
        45 => "1.1".into(),
        46 => "1.2".into(),
        47 => "1.3".into(),
        48 => "1.4".into(),
        49 => "5".into(),
        50 => "6".into(),
        51 => "7".into(),
        52 => "8".into(),
        53 => "9".into(),
        54 => "10".into(),
        55 => "11".into(),
        56 => "12".into(),
        57 => "13".into(),
        58 => "14".into(),
        59 => "15".into(),
        60 => "16".into(),
        61 => "17".into(),
        62 => "18".into(),
        63 => "19".into(),
        64 => "20".into(),
        65 => "21".into(),
        66 => "22".into(),
        67 => "23".into(),
        68 => "24".into(),
        69 => "25".into(),
        v => format!("未知版本号 {}", v),
    }
}

/* ---- 注解结构（element_value AST，JVMS 4.7.16.1） ---- */

/// 常量池字面量 → Java 字面量（字段 ConstantValue 渲染）
fn literal_at(pool: &[Option<CpEntry>], idx: u16) -> String {
    match pool.get(idx as usize).and_then(|e| e.as_ref()) {
        Some(CpEntry::Integer(v)) => v.to_string(),
        Some(CpEntry::Float(v)) => format!("{}f", v),
        Some(CpEntry::Long(v)) => format!("{}L", v),
        Some(CpEntry::Double(v)) => format!("{}", v),
        Some(CpEntry::String(i)) => format!("\"{}\"", cp_utf8(pool, *i).replace('"', "\\\"")),
        Some(CpEntry::Utf8(s)) => format!("\"{}\"", s.replace('"', "\\\"")),
        _ => String::from("<invalid>"),
    }
}

enum ElementValue {
    /// B/C/I/S 的常量池整数值（渲染为数字）
    Int(i32),
    /// Z 布尔值
    Bool(bool),
    Double(f64),
    Float(f32),
    Long(i64),
    Str(String),
    /// 枚举常量（type 为内部类名）
    Enum { ty: String, cnst: String },
    /// 类字面量（ty 为内部类名）
    Class { ty: String },
    Ann(Annotation),
    Array(Vec<ElementValue>),
}

struct Annotation {
    /// 注解类型内部类名
    ty: String,
    pairs: Vec<(String, ElementValue)>,
}

/// 解析 annotation 元素值；同时把引用类型登记进注册表
fn parse_element_value(r: &mut Reader, pool: &[Option<CpEntry>], reg: &mut TypeRegistry) -> Result<ElementValue, String> {
    let tag = r.u1()?;
    Ok(match tag {
        b'B' | b'C' | b'I' | b'S' | b'Z' => {
            let idx = r.u2()?;
            match pool.get(idx as usize).and_then(|e| e.as_ref()) {
                Some(CpEntry::Integer(v)) if tag == b'Z' => ElementValue::Bool(*v != 0),
                Some(CpEntry::Integer(v)) => ElementValue::Int(*v),
                _ => return Err(format!("element_value 期望 Integer 常量（tag 0x{:X}）", tag)),
            }
        }
        b'D' => {
            let idx = r.u2()?;
            match pool.get(idx as usize).and_then(|e| e.as_ref()) {
                Some(CpEntry::Double(v)) => ElementValue::Double(*v),
                _ => return Err("element_value 期望 Double 常量".into()),
            }
        }
        b'F' => {
            let idx = r.u2()?;
            match pool.get(idx as usize).and_then(|e| e.as_ref()) {
                Some(CpEntry::Float(v)) => ElementValue::Float(*v),
                _ => return Err("element_value 期望 Float 常量".into()),
            }
        }
        b'J' => {
            let idx = r.u2()?;
            match pool.get(idx as usize).and_then(|e| e.as_ref()) {
                Some(CpEntry::Long(v)) => ElementValue::Long(*v),
                _ => return Err("element_value 期望 Long 常量".into()),
            }
        }
        b's' => {
            let idx = r.u2()?;
            ElementValue::Str(cp_utf8(pool, idx))
        }
        b'e' => {
            // 枚举常量：type_name_index 为字段描述符，const_name_index 为常量名
            let ty = cp_utf8(pool, r.u2()?);
            let ty = ty.strip_prefix('L').and_then(|s| s.strip_suffix(';')).unwrap_or(&ty).to_string();
            reg.register(&ty);
            ElementValue::Enum { ty, cnst: cp_utf8(pool, r.u2()?) }
        }
        b'c' => {
            // 类字面量：return descriptor
            let desc = cp_utf8(pool, r.u2()?);
            let ty = desc.strip_prefix('L').and_then(|s| s.strip_suffix(';')).unwrap_or(&desc).to_string();
            reg.register(&ty);
            ElementValue::Class { ty }
        }
        b'@' => ElementValue::Ann(parse_annotation(r, pool, reg)?),
        b'[' => {
            let n = r.u2()? as usize;
            let mut vals = vec![];
            for _ in 0..n {
                vals.push(parse_element_value(r, pool, reg)?);
            }
            ElementValue::Array(vals)
        }
        other => return Err(format!("未知 element_value tag: 0x{:X}", other)),
    })
}

/// 解析单个 annotation 结构
fn parse_annotation(r: &mut Reader, pool: &[Option<CpEntry>], reg: &mut TypeRegistry) -> Result<Annotation, String> {
    let ty_desc = cp_utf8(pool, r.u2()?);
    let ty = ty_desc.strip_prefix('L').and_then(|s| s.strip_suffix(';')).unwrap_or(&ty_desc).to_string();
    reg.register(&ty);
    let n = r.u2()?;
    let mut pairs = vec![];
    for _ in 0..n {
        let name = cp_utf8(pool, r.u2()?);
        pairs.push((name, parse_element_value(r, pool, reg)?));
    }
    Ok(Annotation { ty, pairs })
}

/// Runtime(In)VisibleAnnotations 属性 → 注解列表
fn parse_annotations(data: &[u8], pool: &[Option<CpEntry>], reg: &mut TypeRegistry) -> Result<Vec<Annotation>, String> {
    let mut r = Reader::new(data);
    let n = r.u2()?;
    let mut out = vec![];
    for _ in 0..n {
        out.push(parse_annotation(&mut r, pool, reg)?);
    }
    Ok(out)
}

/// 注解 → @Type(...)；value 成员的 "value=" 前缀省略（对齐 IDE）
fn render_annotation(reg: &TypeRegistry, a: &Annotation) -> String {
    let pairs = a
        .pairs
        .iter()
        .map(|(name, v)| {
            let val = render_element_value(reg, v);
            if name == "value" { val } else { format!("{}={}", name, val) }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("@{}({})", reg.display(&a.ty), pairs)
}

fn render_element_value(reg: &TypeRegistry, v: &ElementValue) -> String {
    match v {
        ElementValue::Int(i) => i.to_string(),
        ElementValue::Bool(b) => if *b { "true".into() } else { "false".into() },
        ElementValue::Double(d) => format!("{}", d),
        ElementValue::Float(f) => format!("{}f", f),
        ElementValue::Long(l) => format!("{}L", l),
        ElementValue::Str(s) => format!("\"{}\"", s.replace('"', "\\\"")),
        ElementValue::Enum { ty, cnst } => format!("{}.{}", reg.display(ty), cnst),
        ElementValue::Class { ty } => format!("{}.class", reg.display(ty)),
        ElementValue::Ann(a) => render_annotation(reg, a),
        ElementValue::Array(vs) => format!("{{{}}}", vs.iter().map(|v| render_element_value(reg, v)).collect::<Vec<_>>().join(", ")),
    }
}

/* ---- 指令反汇编 ---- */

/// 操作数格式（决定后续字节如何解释与展示）
#[derive(Clone, Copy, PartialEq, Debug)]
enum Operand {
    None,
    U1,    // 单字节索引（iload/ldc 等）
    I1,    // 单字节有符号立即数（bipush）
    U2,    // 常量池/局部变量索引（getstatic/ldc_w 等）
    I2,    // 双字节有符号偏移（ifeq/goto 等）
    I4,    // 四字节有符号偏移（goto_w/jsr_w）
    U1I1,  // iinc
    I2U1,  // invokeinterface：index u2, count u1, 0
    U2Z1,  // invokedynamic：index u2, 0, 0
    U2U1,  // multianewarray：index u2, dims u1
    Array, // newarray：atype u1
    TableSwitch,
    LookupSwitch,
    Wide,
}

/// 指令表：opcode → (助记符, 操作数格式)。依据 JVMS 第 6.5 章。
fn opcode_info(op: u8) -> (&'static str, Operand) {
    use Operand::*;
    match op {
        0x00 => ("nop", None),
        0x01 => ("aconst_null", None),
        0x02 => ("iconst_m1", None),
        0x03 => ("iconst_0", None),
        0x04 => ("iconst_1", None),
        0x05 => ("iconst_2", None),
        0x06 => ("iconst_3", None),
        0x07 => ("iconst_4", None),
        0x08 => ("iconst_5", None),
        0x09 => ("lconst_0", None),
        0x0a => ("lconst_1", None),
        0x0b => ("fconst_0", None),
        0x0c => ("fconst_1", None),
        0x0d => ("fconst_2", None),
        0x0e => ("dconst_0", None),
        0x0f => ("dconst_1", None),
        0x10 => ("bipush", I1),
        0x11 => ("sipush", I2),
        0x12 => ("ldc", U1),
        0x13 => ("ldc_w", U2),
        0x14 => ("ldc2_w", U2),
        0x15 => ("iload", U1),
        0x16 => ("lload", U1),
        0x17 => ("fload", U1),
        0x18 => ("dload", U1),
        0x19 => ("aload", U1),
        0x1a => ("iload_0", None),
        0x1b => ("iload_1", None),
        0x1c => ("iload_2", None),
        0x1d => ("iload_3", None),
        0x1e => ("lload_0", None),
        0x1f => ("lload_1", None),
        0x20 => ("lload_2", None),
        0x21 => ("lload_3", None),
        0x22 => ("fload_0", None),
        0x23 => ("fload_1", None),
        0x24 => ("fload_2", None),
        0x25 => ("fload_3", None),
        0x26 => ("dload_0", None),
        0x27 => ("dload_1", None),
        0x28 => ("dload_2", None),
        0x29 => ("dload_3", None),
        0x2a => ("aload_0", None),
        0x2b => ("aload_1", None),
        0x2c => ("aload_2", None),
        0x2d => ("aload_3", None),
        0x2e => ("iaload", None),
        0x2f => ("laload", None),
        0x30 => ("faload", None),
        0x31 => ("daload", None),
        0x32 => ("aaload", None),
        0x33 => ("baload", None),
        0x34 => ("caload", None),
        0x35 => ("saload", None),
        0x36 => ("istore", U1),
        0x37 => ("lstore", U1),
        0x38 => ("fstore", U1),
        0x39 => ("dstore", U1),
        0x3a => ("astore", U1),
        0x3b => ("istore_0", None),
        0x3c => ("istore_1", None),
        0x3d => ("istore_2", None),
        0x3e => ("istore_3", None),
        0x3f => ("lstore_0", None),
        0x40 => ("lstore_1", None),
        0x41 => ("lstore_2", None),
        0x42 => ("lstore_3", None),
        0x43 => ("fstore_0", None),
        0x44 => ("fstore_1", None),
        0x45 => ("fstore_2", None),
        0x46 => ("fstore_3", None),
        0x47 => ("dstore_0", None),
        0x48 => ("dstore_1", None),
        0x49 => ("dstore_2", None),
        0x4a => ("dstore_3", None),
        0x4b => ("astore_0", None),
        0x4c => ("astore_1", None),
        0x4d => ("astore_2", None),
        0x4e => ("astore_3", None),
        0x4f => ("iastore", None),
        0x50 => ("lastore", None),
        0x51 => ("fastore", None),
        0x52 => ("dastore", None),
        0x53 => ("aastore", None),
        0x54 => ("bastore", None),
        0x55 => ("castore", None),
        0x56 => ("sastore", None),
        0x57 => ("pop", None),
        0x58 => ("pop2", None),
        0x59 => ("dup", None),
        0x5a => ("dup_x1", None),
        0x5b => ("dup_x2", None),
        0x5c => ("dup2", None),
        0x5d => ("dup2_x1", None),
        0x5e => ("dup2_x2", None),
        0x5f => ("swap", None),
        0x60 => ("iadd", None),
        0x61 => ("ladd", None),
        0x62 => ("fadd", None),
        0x63 => ("dadd", None),
        0x64 => ("isub", None),
        0x65 => ("lsub", None),
        0x66 => ("fsub", None),
        0x67 => ("dsub", None),
        0x68 => ("imul", None),
        0x69 => ("lmul", None),
        0x6a => ("fmul", None),
        0x6b => ("dmul", None),
        0x6c => ("idiv", None),
        0x6d => ("ldiv", None),
        0x6e => ("fdiv", None),
        0x6f => ("ddiv", None),
        0x70 => ("irem", None),
        0x71 => ("lrem", None),
        0x72 => ("frem", None),
        0x73 => ("drem", None),
        0x74 => ("ineg", None),
        0x75 => ("lneg", None),
        0x76 => ("fneg", None),
        0x77 => ("dneg", None),
        0x78 => ("ishl", None),
        0x79 => ("lshl", None),
        0x7a => ("ishr", None),
        0x7b => ("lshr", None),
        0x7c => ("iushr", None),
        0x7d => ("lushr", None),
        0x7e => ("iand", None),
        0x7f => ("land", None),
        0x80 => ("ior", None),
        0x81 => ("lor", None),
        0x82 => ("ixor", None),
        0x83 => ("lxor", None),
        0x84 => ("iinc", U1I1),
        0x85 => ("i2l", None),
        0x86 => ("i2f", None),
        0x87 => ("i2d", None),
        0x88 => ("l2i", None),
        0x89 => ("l2f", None),
        0x8a => ("l2d", None),
        0x8b => ("f2i", None),
        0x8c => ("f2l", None),
        0x8d => ("f2d", None),
        0x8e => ("d2i", None),
        0x8f => ("d2l", None),
        0x90 => ("d2f", None),
        0x91 => ("i2b", None),
        0x92 => ("i2c", None),
        0x93 => ("i2s", None),
        0x94 => ("lcmp", None),
        0x95 => ("fcmpl", None),
        0x96 => ("fcmpg", None),
        0x97 => ("dcmpl", None),
        0x98 => ("dcmpg", None),
        0x99 => ("ifeq", I2),
        0x9a => ("ifne", I2),
        0x9b => ("iflt", I2),
        0x9c => ("ifge", I2),
        0x9d => ("ifgt", I2),
        0x9e => ("ifle", I2),
        0x9f => ("if_icmpeq", I2),
        0xa0 => ("if_icmpne", I2),
        0xa1 => ("if_icmplt", I2),
        0xa2 => ("if_icmpge", I2),
        0xa3 => ("if_icmpgt", I2),
        0xa4 => ("if_icmple", I2),
        0xa5 => ("if_acmpeq", I2),
        0xa6 => ("if_acmpne", I2),
        0xa7 => ("goto", I2),
        0xa8 => ("jsr", I2),
        0xa9 => ("ret", U1),
        0xaa => ("tableswitch", TableSwitch),
        0xab => ("lookupswitch", LookupSwitch),
        0xac => ("ireturn", None),
        0xad => ("lreturn", None),
        0xae => ("freturn", None),
        0xaf => ("dreturn", None),
        0xb0 => ("areturn", None),
        0xb1 => ("return", None),
        0xb2 => ("getstatic", U2),
        0xb3 => ("putstatic", U2),
        0xb4 => ("getfield", U2),
        0xb5 => ("putfield", U2),
        0xb6 => ("invokevirtual", U2),
        0xb7 => ("invokespecial", U2),
        0xb8 => ("invokestatic", U2),
        0xb9 => ("invokeinterface", I2U1),
        0xba => ("invokedynamic", U2Z1),
        0xbb => ("new", U2),
        0xbc => ("newarray", Array),
        0xbd => ("anewarray", U2),
        0xbe => ("arraylength", None),
        0xbf => ("athrow", None),
        0xc0 => ("checkcast", U2),
        0xc1 => ("instanceof", U2),
        0xc2 => ("monitorenter", None),
        0xc3 => ("monitorexit", None),
        0xc4 => ("wide", Wide),
        0xc5 => ("multianewarray", U2U1),
        0xc6 => ("ifnull", I2),
        0xc7 => ("ifnonnull", I2),
        0xc8 => ("goto_w", I4),
        0xc9 => ("jsr_w", I4),
        0xca => ("breakpoint", None),
        0xfe => ("impdep1", None),
        0xff => ("impdep2", None),
        _ => ("<unknown>", None),
    }
}

fn newarray_type_name(atype: u8) -> &'static str {
    match atype {
        4 => "boolean",
        5 => "char",
        6 => "float",
        7 => "double",
        8 => "byte",
        9 => "short",
        10 => "int",
        11 => "long",
        _ => "<unknown>",
    }
}

fn push_line(lines: &mut Vec<String>, pc: i64, line: String) {
    lines.push(format!("{:>8}: {}", pc, line));
}

/// 反汇编一段 Code 字节码（pc 即 Reader 位置，读取器推进即指令结束）
fn disassemble_code(code: &[u8], pool: &[Option<CpEntry>], lines: &mut Vec<String>) -> Result<(), String> {
    let mut r = Reader::new(code);
    while r.pos < code.len() {
        let pc = r.pos as i64;
        let op = r.u1()?;
        let (mnemonic, operand) = opcode_info(op);

        match operand {
            Operand::TableSwitch => {
                // 0~3 字节填充对齐到 4 字节边界（相对 code 起始）
                let padding = (4 - (pc as usize + 1) % 4) % 4;
                r.take(padding)?;
                let default = r.i4()?;
                let low = r.i4()?;
                let high = r.i4()?;
                if high < low {
                    return Err("tableswitch 范围非法 (high < low)".into());
                }
                let n = (high - low + 1) as usize;
                let total = r.pos as i64 + (n as i64) * 4; // 指令结束位置（偏移表尚未读取）
                push_line(lines, pc, format!("tableswitch {{ // {} to {}", low, high));
                for k in 0..n {
                    let target = r.i4()?;
                    lines.push(format!("{:>14}: {}", low + k as i32, total + target as i64));
                }
                lines.push(format!("{:>14}: {}", "default", total + default as i64));
                continue;
            }
            Operand::LookupSwitch => {
                let padding = (4 - (pc as usize + 1) % 4) % 4;
                r.take(padding)?;
                let default = r.i4()?;
                let npairs = r.i4()?;
                if npairs < 0 {
                    return Err("lookupswitch 键值对数非法".into());
                }
                let n = npairs as usize;
                let total = r.pos as i64 + (n as i64) * 8; // 指令结束位置（键值对表尚未读取）
                push_line(lines, pc, format!("lookupswitch {{ // {} keys", n));
                for _ in 0..n {
                    let key = r.i4()?;
                    let target = r.i4()?;
                    lines.push(format!("{:>14}: {}", key, total + target as i64));
                }
                lines.push(format!("{:>14}: {}", "default", total + default as i64));
                continue;
            }
            Operand::Wide => {
                let next = r.u1()?;
                let (next_mn, _) = opcode_info(next);
                if next_mn == "iinc" {
                    let idx = r.u2()?;
                    let c = r.i2()?;
                    push_line(lines, pc, format!("wide {} {}, {}", next_mn, idx, c));
                } else {
                    let idx = r.u2()?;
                    push_line(lines, pc, format!("wide {} {}", next_mn, idx));
                }
                continue;
            }
            _ => {}
        }

        let mut line = mnemonic.to_string();
        match operand {
            Operand::None => {}
            Operand::U1 => {
                let idx = r.u1()?;
                line.push_str(&format!(" {}", idx));
                if mnemonic == "ldc" {
                    line.push_str(&format!("    // {}", cp_display(pool, idx as u16)));
                }
            }
            Operand::I1 => {
                line.push_str(&format!(" {}", r.i1()?));
            }
            Operand::U2 => {
                let idx = r.u2()?;
                line.push_str(&format!(" #{}", idx));
                if matches!(mnemonic, "getstatic" | "putstatic" | "getfield" | "putfield" | "invokevirtual" | "invokespecial" | "invokestatic" | "new" | "anewarray" | "checkcast" | "instanceof" | "ldc_w" | "ldc2_w") {
                    line.push_str(&format!("    // {}", cp_display(pool, idx)));
                }
            }
            Operand::I2 => {
                let off = r.i2()?;
                let target = pc + 3 + off as i64; // 指令总长 3
                line.push_str(&format!(" {}", target));
            }
            Operand::I4 => {
                let off = r.i4()?;
                let target = pc + 5 + off as i64; // 指令总长 5
                line.push_str(&format!(" {}", target));
            }
            Operand::U1I1 => {
                let idx = r.u1()?;
                let c = r.i1()?;
                line.push_str(&format!(" {}, {}", idx, c));
            }
            Operand::I2U1 => {
                let idx = r.u2()?;
                let count = r.u1()?;
                let _zero = r.u1()?;
                line.push_str(&format!(" #{}, {}", idx, count));
                match pool.get(idx as usize).and_then(|e| e.as_ref()) {
                    Some(CpEntry::Methodref(c, n)) | Some(CpEntry::InterfaceMethodref(c, n)) => {
                        line.push_str(&format!("    // InterfaceMethod {}", cp_member_ref(pool, *c, *n)));
                    }
                    _ => {}
                }
            }
            Operand::U2Z1 => {
                let idx = r.u2()?;
                let _z1 = r.u1()?;
                let _z2 = r.u1()?;
                line.push_str(&format!(" #{}, 0", idx));
                if let Some(CpEntry::InvokeDynamic(_, n)) = pool.get(idx as usize).and_then(|e| e.as_ref()) {
                    line.push_str(&format!("    // InvokeDynamic {}", cp_display(pool, *n)));
                }
            }
            Operand::U2U1 => {
                let idx = r.u2()?;
                let dims = r.u1()?;
                line.push_str(&format!(" #{}, {}", idx, dims));
                line.push_str(&format!("    // class {}", cp_class_name(pool, idx)));
            }
            Operand::Array => {
                let atype = r.u1()?;
                line.push_str(&format!(" {}", newarray_type_name(atype)));
            }
            _ => unreachable!("switch/wide 已在上方处理"),
        }
        push_line(lines, pc, line);
    }
    Ok(())
}

/* ---- 属性解析 ---- */

/// Code 属性数据（不含属性名与长度头）
fn parse_code_data(
    data: &[u8],
    pool: &[Option<CpEntry>],
) -> Result<(Vec<u8>, Vec<(u16, u16)>, u16, u16), String> {
    let mut inner = Reader::new(data);
    let max_stack = inner.u2()?;
    let max_locals = inner.u2()?;
    let code_len = inner.u4()? as usize;
    let code = inner.take(code_len)?.to_vec();
    let exc_count = inner.u2()?;
    for _ in 0..exc_count {
        inner.u2()?; // start_pc
        inner.u2()?; // end_pc
        inner.u2()?; // handler_pc
        inner.u2()?; // catch_type
    }
    // Code 的子属性（LineNumberTable 等）
    let sub_count = inner.u2()?;
    let mut lines = vec![];
    for _ in 0..sub_count {
        let sub_name_idx = inner.u2()?;
        let sub_len = inner.u4()? as usize;
        if cp_utf8(pool, sub_name_idx) == "LineNumberTable" {
            let mut lr = Reader::new(inner.take(sub_len)?);
            let n = lr.u2()?;
            for _ in 0..n {
                lines.push((lr.u2()?, lr.u2()?));
            }
        } else {
            inner.take(sub_len)?;
        }
    }
    Ok((code, lines, max_stack, max_locals))
}

/// 方法属性解析结果
struct MethodInfo {
    code: Option<(Vec<u8>, Vec<(u16, u16)>, u16, u16)>,
    default_value: Option<ElementValue>,
    annotations: Vec<Annotation>,
}

/// 消费方法的全部属性（漏掉任何一个都会让后续解析错位）
fn parse_method_attrs(
    r: &mut Reader,
    pool: &[Option<CpEntry>],
    reg: &mut TypeRegistry,
) -> Result<MethodInfo, String> {
    let mut info = MethodInfo { code: None, default_value: None, annotations: vec![] };
    let attr_count = r.u2()?;
    for _ in 0..attr_count {
        let name_idx = r.u2()?;
        let len = r.u4()? as usize;
        let data = r.take(len)?;
        match cp_utf8(pool, name_idx).as_str() {
            "Code" => {
                info.code = Some(parse_code_data(data, pool)?);
            }
            "AnnotationDefault" => {
                let mut er = Reader::new(data);
                info.default_value = Some(parse_element_value(&mut er, pool, reg)?);
            }
            "RuntimeVisibleAnnotations" | "RuntimeInvisibleAnnotations" => {
                info.annotations.extend(parse_annotations(data, pool, reg)?);
            }
            _ => {}
        }
    }
    Ok(info)
}

/* ---- 主入口 ---- */

/// 反汇编 class 文件为可读文本
pub fn disassemble_class(bytes: &[u8]) -> Result<String, String> {
    let mut r = Reader::new(bytes);
    if r.u4()? != 0xCAFEBABE {
        return Err("不是合法的 class 文件（magic 校验失败）".into());
    }
    let minor = r.u2()?;
    let major = r.u2()?;
    let pool = parse_constant_pool(&mut r)?;

    let access = r.u2()?;
    let this_idx = r.u2()?;
    let super_idx = r.u2()?;
    let iface_count = r.u2()?;
    let ifaces: Vec<u16> = (0..iface_count).map(|_| r.u2()).collect::<Result<_, _>>()?;

    let this_full = cp_class_name(&pool, this_idx);
    let this_short = this_full.rsplit('/').next().unwrap_or(&this_full).to_string();
    let is_annotation = access & ACC_ANNOTATION != 0;

    let mut reg = TypeRegistry::new();
    // 超类/接口登记（注解类的 implements 恒为 Annotation 且不显示，无需登记）
    if super_idx != 0 && !is_annotation {
        reg.register(&cp_class_name(&pool, super_idx));
    }
    for &i in &ifaces {
        if !is_annotation {
            reg.register(&cp_class_name(&pool, i));
        }
    }

    let field_count = r.u2()?;
    let fields: Vec<(u16, u16, u16, Option<u16>, Vec<Annotation>)> = (0..field_count)
        .map(|_| {
            let f_access = r.u2()?;
            let name_idx = r.u2()?;
            let desc_idx = r.u2()?;
            register_desc(&mut reg, &cp_utf8(&pool, desc_idx));
            let attr_count = r.u2()?;
            let mut const_val = None;
            let mut annos = vec![];
            for _ in 0..attr_count {
                let a_name = r.u2()?;
                let a_len = r.u4()? as usize;
                let a_data = r.take(a_len)?;
                match cp_utf8(&pool, a_name).as_str() {
                    "ConstantValue" => {
                        let mut vr = Reader::new(a_data);
                        const_val = Some(vr.u2()?);
                    }
                    "RuntimeVisibleAnnotations" | "RuntimeInvisibleAnnotations" => {
                        annos.extend(parse_annotations(a_data, &pool, &mut reg)?);
                    }
                    _ => {}
                }
            }
            Ok((f_access, name_idx, desc_idx, const_val, annos))
        })
        .collect::<Result<_, String>>()?;

    let method_count = r.u2()?;
    let methods: Vec<(u16, u16, u16, MethodInfo)> = (0..method_count)
        .map(|_| {
            let m_access = r.u2()?;
            let name_idx = r.u2()?;
            let desc_idx = r.u2()?;
            register_desc(&mut reg, &cp_utf8(&pool, desc_idx));
            let info = parse_method_attrs(&mut r, &pool, &mut reg)?;
            Ok((m_access, name_idx, desc_idx, info))
        })
        .collect::<Result<_, String>>()?;

    // 类级属性（注解等）
    let class_attr_count = r.u2()?;
    let mut class_annos = vec![];
    for _ in 0..class_attr_count {
        let a_name = r.u2()?;
        let a_len = r.u4()? as usize;
        let a_data = r.take(a_len)?;
        match cp_utf8(&pool, a_name).as_str() {
            "RuntimeVisibleAnnotations" | "RuntimeInvisibleAnnotations" => {
                class_annos.extend(parse_annotations(a_data, &pool, &mut reg)?);
            }
            _ => {}
        }
    }

    reg.resolve();

    // ── 输出 ──
    let mut out = String::new();
    writeln!(out, "// class version {}.{} (Java {})", major, minor, java_version(major)).unwrap();
    writeln!(out, "// flags (0x{:04X}) {}", access, class_flags(access).join(", ")).unwrap();
    if let Some(pkg) = this_full.rsplit_once('/').map(|(p, _)| p.replace('/', ".")) {
        writeln!(out, "package {};", pkg).unwrap();
        writeln!(out).unwrap();
    }
    for imp in &reg.imports {
        writeln!(out, "import {};", imp).unwrap();
    }
    if !reg.imports.is_empty() {
        writeln!(out).unwrap();
    }

    for a in &class_annos {
        writeln!(out, "{}", render_annotation(&reg, a)).unwrap();
    }

    if is_annotation {
        // 注解类：extends/implements 恒为 Object/Annotation，无信息量，省略（对齐 IDE）
        writeln!(out, "public @interface {} {{", this_short).unwrap();
    } else {
        let mut decl = format!("public {}", if access & ACC_INTERFACE != 0 { "interface" } else { "class" });
        if access & ACC_ABSTRACT != 0 && access & ACC_INTERFACE == 0 {
            decl.push_str(" abstract");
        }
        if access & ACC_FINAL != 0 {
            decl.push_str(" final");
        }
        decl.push_str(&format!(" {}", this_short));
        if super_idx != 0 && cp_class_name(&pool, super_idx) != "java/lang/Object" {
            decl.push_str(&format!(" extends {}", reg.display(&cp_class_name(&pool, super_idx))));
        }
        if !ifaces.is_empty() {
            let names: Vec<String> = ifaces.iter().map(|&i| reg.display(&cp_class_name(&pool, i))).collect();
            decl.push_str(&format!(" implements {}", names.join(", ")));
        }
        writeln!(out, "{} {{", decl).unwrap();
    }

    for (f_access, name_idx, desc_idx, const_val, annos) in &fields {
        for a in annos {
            writeln!(out, "  {}", render_annotation(&reg, a)).unwrap();
        }
        let flags_str = field_flags(*f_access).join(" ").to_lowercase().replace("acc_", "");
        let prefix = if flags_str.is_empty() { String::new() } else { format!("{} ", flags_str) };
        let t = display_type(&reg, &cp_utf8(&pool, *desc_idx));
        let init = const_val.map(|idx| format!(" = {}", literal_at(&pool, idx))).unwrap_or_default();
        writeln!(out, "  {}{} {}{};", prefix, t, cp_utf8(&pool, *name_idx), init).unwrap();
    }

    for (m_access, name_idx, desc_idx, info) in &methods {
        let raw_name = cp_utf8(&pool, *name_idx);
        // 构造器显示为类名（javap/IDE 同款）且无返回类型；<clinit> 保持原样
        let is_ctor = raw_name == "<init>";
        let display_name = if is_ctor { this_short.clone() } else { raw_name };
        let desc = cp_utf8(&pool, *desc_idx);
        let (ret, params) = method_desc_parts(&reg, &desc).unwrap_or((desc.clone(), vec![]));

        if is_annotation {
            // 注解成员：Java 风格一行（public abstract 是默认修饰，省略对齐 IDE）
            for a in &info.annotations {
                writeln!(out, "  {}", render_annotation(&reg, a)).unwrap();
            }
            let default_str = info.default_value.as_ref().map(|d| format!(" default {}", render_element_value(&reg, d))).unwrap_or_default();
            writeln!(out, "  {} {}({}){};", ret, display_name, params.join(", "), default_str).unwrap();
            continue;
        }

        writeln!(out).unwrap();
        for a in &info.annotations {
            writeln!(out, "  {}", render_annotation(&reg, a)).unwrap();
        }
        let flags_str = method_flags(*m_access).join(" ").to_lowercase().replace("acc_", "");
        let sig = format!("{}({})", display_name, params.join(", "));
        let ret_str = if is_ctor { String::new() } else { format!("{} ", ret) };
        if flags_str.is_empty() {
            writeln!(out, "  {}{};", ret_str, sig).unwrap();
        } else {
            writeln!(out, "  {} {}{};", flags_str, ret_str, sig).unwrap();
        }
        writeln!(out, "    descriptor: {}", desc).unwrap();
        writeln!(out, "    flags: (0x{:04X}) {}", m_access, method_flags(*m_access).join(", ")).unwrap();

        if let Some((code, line_table, max_stack, max_locals)) = &info.code {
            writeln!(out, "    Code:").unwrap();
            writeln!(out, "      stack={}, locals={}", max_stack, max_locals).unwrap();
            let mut lines = vec![];
            disassemble_code(code, &pool, &mut lines)?;
            for l in &lines {
                writeln!(out, "      {}", l).unwrap();
            }
            if !line_table.is_empty() {
                writeln!(out, "      LineNumberTable:").unwrap();
                for (pc, line) in line_table {
                    writeln!(out, "        line {}: {}", line, pc).unwrap();
                }
            }
        }
    }

    writeln!(out, "}}").unwrap();
    Ok(out)
}

/* ---- Tests ---- */

#[cfg(test)]
mod tests {
    use super::*;

    fn u1(v: u8, b: &mut Vec<u8>) { b.push(v); }
    fn u2(v: u16, b: &mut Vec<u8>) { b.extend_from_slice(&v.to_be_bytes()); }
    fn u4(v: u32, b: &mut Vec<u8>) { b.extend_from_slice(&v.to_be_bytes()); }
    fn utf8(s: &str, b: &mut Vec<u8>) {
        u1(TAG_UTF8, b);
        u2(s.len() as u16, b);
        b.extend_from_slice(s.as_bytes());
    }
    fn attr(name_idx: u16, data: &[u8], b: &mut Vec<u8>) {
        u2(name_idx, b);
        u4(data.len() as u32, b);
        b.extend_from_slice(data);
    }

    /// 手工构造一个最小 class 文件（Java 8，Test 类，含 <init> 与行号表）
    fn minimal_class() -> Vec<u8> {
        let mut b = vec![];
        b.extend_from_slice(&[0xCA, 0xFE, 0xBA, 0xBE]);
        u2(0, &mut b); // minor
        u2(52, &mut b); // major = Java 8
        u2(12, &mut b); // constant_pool_count
        utf8("Test", &mut b); // #1
        utf8("java/lang/Object", &mut b); // #2
        u1(TAG_CLASS, &mut b); u2(2, &mut b); // #3 Class #2
        utf8("<init>", &mut b); // #4
        utf8("()V", &mut b); // #5
        utf8("Code", &mut b); // #6
        u1(TAG_METHODREF, &mut b); u2(3, &mut b); u2(8, &mut b); // #7 Methodref #3.#8
        u1(TAG_NAME_AND_TYPE, &mut b); u2(4, &mut b); u2(5, &mut b); // #8 NameAndType #4.#5
        utf8("LineNumberTable", &mut b); // #9
        utf8("Test.java", &mut b); // #10
        utf8("SourceFile", &mut b); // #11
        u2(0x0021, &mut b); // ACC_PUBLIC | ACC_SUPER
        u2(1, &mut b); // this_class
        u2(3, &mut b); // super_class
        u2(0, &mut b); // interfaces_count
        u2(0, &mut b); // fields_count
        u2(1, &mut b); // methods_count
        // method: public <init>()V
        u2(0x0001, &mut b);
        u2(4, &mut b); u2(5, &mut b);
        u2(1, &mut b); // attributes_count
        // Code 属性
        let mut code_attr = vec![];
        u2(1, &mut code_attr); // max_stack
        u2(1, &mut code_attr); // max_locals
        let code = [0x2a, 0xb7, 0x00, 0x07, 0xb1]; // aload_0, invokespecial #7, return
        u4(code.len() as u32, &mut code_attr);
        code_attr.extend_from_slice(&code);
        u2(0, &mut code_attr); // exception_table_length
        u2(1, &mut code_attr); // Code attributes_count
        let mut lnt = vec![];
        u2(1, &mut lnt);
        u2(0, &mut lnt); // start_pc
        u2(3, &mut lnt); // line 3
        attr(9, &lnt, &mut code_attr);
        attr(6, &code_attr, &mut b);
        // class attributes: SourceFile
        u2(1, &mut b);
        let mut sf = vec![];
        u2(10, &mut sf);
        attr(11, &sf, &mut b);
        b
    }

    /// 手工构造一个注解类（ApiLog：@Target 类注解 + 枚举默认值成员）
    fn annotation_class() -> Vec<u8> {
        let mut b = vec![];
        b.extend_from_slice(&[0xCA, 0xFE, 0xBA, 0xBE]);
        u2(0, &mut b);
        u2(52, &mut b);
        u2(17, &mut b); // constant_pool_count
        utf8("ApiLog", &mut b); // #1
        utf8("java/lang/Object", &mut b); // #2
        u1(TAG_CLASS, &mut b); u2(2, &mut b); // #3 Class #2
        utf8("type", &mut b); // #4
        utf8("()Lcom/x/LogType;", &mut b); // #5
        utf8("AnnotationDefault", &mut b); // #6
        utf8("Lcom/x/LogType;", &mut b); // #7
        utf8("ACCESS", &mut b); // #8
        utf8("Ljava/lang/annotation/Target;", &mut b); // #9
        utf8("value", &mut b); // #10
        utf8("Ljava/lang/annotation/ElementType;", &mut b); // #11
        utf8("METHOD", &mut b); // #12
        utf8("java/lang/annotation/Annotation", &mut b); // #13
        u1(TAG_CLASS, &mut b); u2(13, &mut b); // #14 Class #13
        utf8("RuntimeVisibleAnnotations", &mut b); // #15
        utf8("Ljava/lang/annotation/Retention;", &mut b); // #16
        u2(0x2601, &mut b); // ACC_PUBLIC | ACC_INTERFACE | ACC_ABSTRACT | ACC_ANNOTATION
        u2(1, &mut b); // this_class
        u2(3, &mut b); // super_class
        u2(1, &mut b); // interfaces_count
        u2(14, &mut b); // java/lang/annotation/Annotation
        u2(0, &mut b); // fields_count
        u2(1, &mut b); // methods_count
        // method: public abstract type() Lcom/x/LogType; + AnnotationDefault
        u2(0x0401, &mut b);
        u2(4, &mut b); u2(5, &mut b);
        u2(1, &mut b); // attributes_count
        let mut def = vec![];
        u1(b'e', &mut def); // enum const
        u2(7, &mut def); // Lcom/x/LogType;
        u2(8, &mut def); // ACCESS
        attr(6, &def, &mut b);
        // class attrs: RuntimeVisibleAnnotations = @Target({ElementType.METHOD})
        u2(1, &mut b);
        let mut annos = vec![];
        u2(1, &mut annos); // annotations count
        u2(9, &mut annos); // Ljava/lang/annotation/Target;
        u2(1, &mut annos); // pairs count
        u2(10, &mut annos); // "value"
        u1(b'[', &mut annos); // array
        u2(1, &mut annos); // 1 element
        u1(b'e', &mut annos); // enum const
        u2(11, &mut annos); // Ljava/lang/annotation/ElementType;
        u2(12, &mut annos); // METHOD
        attr(15, &annos, &mut b);
        b
    }

    #[test]
    fn test_disassemble_minimal_class() {
        let out = disassemble_class(&minimal_class()).unwrap();
        assert!(out.contains("public class Test {"), "类头错误:\n{}", out);
        assert!(out.contains("public Test();"), "构造器显示错误:\n{}", out);
        assert!(out.contains("0: aload_0"), "aload_0 偏移错误:\n{}", out);
        assert!(out.contains("1: invokespecial #7"), "invokespecial 偏移错误:\n{}", out);
        assert!(out.contains("4: return"), "return 偏移错误:\n{}", out);
        assert!(out.contains("java/lang/Object.\"<init>\":()V"), "成员引用注释错误:\n{}", out);
        assert!(out.contains("LineNumberTable"), "缺少行号表:\n{}", out);
        assert!(out.contains("line 3: 0"), "行号映射错误:\n{}", out);
    }

    #[test]
    fn test_disassemble_annotation_class() {
        let out = disassemble_class(&annotation_class()).unwrap();
        assert!(out.contains("import com.x.LogType;"), "import 缺失:\n{}", out);
        assert!(out.contains("import java.lang.annotation.ElementType;"), "annotation import 缺失:\n{}", out);
        assert!(out.contains("@Target({ElementType.METHOD})"), "类注解错误:\n{}", out);
        assert!(out.contains("public @interface ApiLog {"), "@interface 错误:\n{}", out);
        assert!(out.contains("LogType type() default LogType.ACCESS;"), "注解成员错误:\n{}", out);
    }

    #[test]
    fn test_display_type() {
        let mut reg = TypeRegistry::new();
        reg.register("com/x/Foo");
        reg.register("java/lang/String");
        reg.register("java/lang/annotation/Target");
        reg.resolve();
        assert_eq!(display_type(&reg, "I"), "int");
        assert_eq!(display_type(&reg, "Ljava/lang/String;"), "String");
        assert_eq!(display_type(&reg, "[Ljava/lang/String;"), "String[]");
        assert_eq!(display_type(&reg, "Lcom/x/Foo;"), "Foo");
        assert_eq!(display_type(&reg, "[[I"), "int[][]");
    }

    #[test]
    fn test_registry_collision_keeps_qualified() {
        // 同名冲突：全部回退全限定名，且不生成 import（同 javac）
        let mut reg = TypeRegistry::new();
        reg.register("a/Foo");
        reg.register("b/Foo");
        reg.resolve();
        assert_eq!(reg.display("a/Foo"), "a.Foo");
        assert_eq!(reg.display("b/Foo"), "b.Foo");
        assert!(reg.imports.is_empty());
    }

    #[test]
    fn test_method_desc_parts() {
        let mut reg = TypeRegistry::new();
        reg.register("com/x/R");
        reg.register("java/lang/String");
        reg.resolve();
        let (ret, params) = method_desc_parts(&reg, "(ILjava/lang/String;[B)Lcom/x/R;").unwrap();
        assert_eq!(ret, "R");
        assert_eq!(params, vec!["int", "String", "byte[]"]);
    }

    #[test]
    fn test_disassemble_rejects_bad_magic() {
        let mut bad = minimal_class();
        bad[0] = 0x00;
        assert!(disassemble_class(&bad).is_err());
    }

    #[test]
    fn test_opcode_table_known_ranges() {
        assert_eq!(opcode_info(0x00).0, "nop");
        assert_eq!(opcode_info(0xaa).1, Operand::TableSwitch);
        assert_eq!(opcode_info(0xb9).1, Operand::I2U1);
        assert_eq!(opcode_info(0xba).1, Operand::U2Z1);
        assert_eq!(opcode_info(0xc4).1, Operand::Wide);
        assert_eq!(opcode_info(0xc9).0, "jsr_w");
        assert_eq!(opcode_info(0xff).0, "impdep2");
    }

}
