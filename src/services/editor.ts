import { invoke } from '@tauri-apps/api/core';

/** 读取文件内容 */
export async function readFile(path: string): Promise<string> {
  return await invoke('read_file', { path });
}
