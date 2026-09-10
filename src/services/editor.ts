import { invoke } from '@tauri-apps/api/core';

/** 读取文件响应：is_binary 时前端改用内建查看器（图片预览 / hex 视图） */
export interface ReadFileResponse {
  content: string;
  is_binary: boolean;
  size: number;
  /** 原始换行风格（保存时按此写回，避免编辑器规范化换行导致 git 误报变更） */
  lineEnding: 'lf' | 'crlf' | 'cr';
  /** 原始编码（'gb18030' 时保存按原编码写回，GBK 文件编辑后字节不变） */
  encoding: 'utf8' | 'gb18030';
  /** 读取时的修改时间（距 UNIX 纪元毫秒）。保存时原样回传，用于检测外部改动 */
  modified: number | null;
}

/** read_file 的原始返回（Rust 侧无 rename_all，字段为 snake_case） */
interface RawReadFileResponse {
  content: string;
  is_binary: boolean;
  size: number;
  line_ending: 'lf' | 'crlf' | 'cr';
  encoding: 'utf8' | 'gb18030';
  modified: number | null;
}

/** 读取文件内容（Tauri 2 返回值保持 Rust snake_case 字段名，这里统一映射为 camelCase） */
export async function readFile(path: string): Promise<ReadFileResponse> {
  const res = await invoke<RawReadFileResponse>('read_file', { path });
  return {
    content: res.content,
    is_binary: res.is_binary,
    size: res.size,
    lineEnding: res.line_ending,
    encoding: res.encoding,
    modified: res.modified,
  };
}

/** 读取 .class 源码视图：优先 CFR 反编译（IDEA 级），失败回退字节码视图（无 JRE/超时等） */
export async function readClassFile(path: string): Promise<string> {
  try {
    return await invoke('decompile_class', { path });
  } catch (e) {
    console.warn('CFR 反编译失败，回退字节码视图:', e);
    return await invoke('read_class_file', { path });
  }
}

/** 读取图片为 base64（内建预览拼 data URL） */
export async function readImageData(path: string): Promise<string> {
  return await invoke('read_image_data', { path });
}

/** hex 视图分页响应：一页 rows 行 × 16 字节 */
export interface HexPage {
  offset: number;
  bytes: number[];
  totalSize: number;
}

/** 分页读取二进制内容（hex 视图按需加载） */
export async function readHexPage(path: string, offset: number, rows: number): Promise<HexPage> {
  return await invoke('read_hex_page', { path, offset, rows });
}

/** jar 条目信息 */
export interface JarEntryInfo {
  name: string;
  isDir: boolean;
  size: number;
  compressedSize: number;
}

/** jar 条目读取结果：kind = text（已解码）/ class（已反编译）/ binary（base64） */
export interface JarEntryContent {
  content: string;
  kind: 'text' | 'class' | 'binary';
  size: number;
}

/** 列 jar 条目（nested 为嵌套 jar 条目链，支持 Spring Boot fat jar） */
export async function listJar(path: string, nested: string[]): Promise<JarEntryInfo[]> {
  return await invoke('list_jar', { path, nested });
}

/** 读取 jar 条目内容 */
export async function readJarEntry(path: string, nested: string[], name: string): Promise<JarEntryContent> {
  return await invoke('read_jar_entry', { path, nested, name });
}

/** 读取 jar 二进制条目为字节数组（HexViewer 内存模式） */
export async function readJarEntryBytes(path: string, nested: string[], name: string): Promise<{ bytes: Uint8Array; size: number }> {
  const res = await readJarEntry(path, nested, name);
  if (res.kind !== 'binary') throw new Error('条目不是二进制');
  const bin = atob(res.content);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return { bytes, size: res.size };
}

/**
 * 写入文件内容（encoding 为原编码，GBK 文件按原编码写回保证字节不变）。
 *
 * expectedModified：打开文件时记录的 mtime。传入后后端会在磁盘 mtime 不一致时
 * **拒绝写入**并返回"文件已被外部修改"——用于避免静默覆盖 IDE/git 改过的版本。
 * 返回写入后的新 mtime（后端读不到时为 null），调用方须更新其基线，
 * 否则第二次保存会被自己上一次的写入判成外部冲突。
 */
export async function writeFile(
  path: string,
  content: string,
  encoding?: 'utf8' | 'gb18030',
  expectedModified?: number | null,
): Promise<number | null> {
  return await invoke<number | null>('write_file', {
    path,
    content,
    encoding,
    expectedModified: expectedModified ?? null,
  });
}

/** 单条文件搜索结果 */
export interface SearchResultItem {
  path: string;
  name: string;
  line: number;
  snippet: string;
}

/** 文件搜索响应 */
export interface SearchResponse {
  results: SearchResultItem[];
  truncated: boolean;
}

/** 在目录中按内容搜索文件（子串匹配） */
export async function searchFiles(params: {
  root: string;
  query: string;
  extensions?: string[];
  caseSensitive?: boolean;
  maxResults?: number;
}): Promise<SearchResponse> {
  return await invoke('search_files', {
    params: {
      root: params.root,
      query: params.query,
      extensions: params.extensions ?? [],
      caseSensitive: params.caseSensitive ?? false,
      maxResults: params.maxResults ?? 200,
    },
  });
}
