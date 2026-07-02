import { invoke } from '@tauri-apps/api/core';

/// 创建终端会话
export async function createTerminal(
  sessionId: string,
  options?: {
    workingDir?: string;
    shell?: string;
    envVars?: Record<string, string>;
    cols?: number;
    rows?: number;
    initCommand?: string;
  }
): Promise<void> {
  await invoke('create_terminal', {
    params: {
      sessionId,
      workingDir: options?.workingDir,
      shell: options?.shell,
      envVars: options?.envVars,
      cols: options?.cols,
      rows: options?.rows,
      initCommand: options?.initCommand,
    }
  });
}

/// 向终端写入数据
export async function writeTerminal(sessionId: string, data: string): Promise<void> {
  await invoke('write_terminal', { sessionId, data });
}

/// 关闭终端会话
export async function closeTerminal(sessionId: string): Promise<void> {
  await invoke('close_terminal', { sessionId });
}
