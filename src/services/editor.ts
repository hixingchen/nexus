import { invoke } from '@tauri-apps/api/core';

/** 读取文件内容 */
export async function readFile(path: string): Promise<string> {
  return await invoke('read_file', { path });
}

/** 写入文件内容 */
export async function writeFile(path: string, content: string): Promise<void> {
  return await invoke('write_file', { path, content });
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
