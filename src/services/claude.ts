import { invoke } from '@tauri-apps/api/core';
import type { ClaudeStartParams, ClaudeCheckResult } from '../types/claude';

/**
 * 启动 Claude Code 会话
 * @returns sessionId
 */
export async function claudeStart(params: ClaudeStartParams): Promise<string> {
  return await invoke<string>('claude_start', { params });
}

/**
 * 停止 Claude Code 会话
 */
export async function claudeStop(sessionId: string): Promise<void> {
  await invoke('claude_stop', { sessionId });
}

/**
 * 检查 Claude Code CLI 是否可用
 */
export async function claudeCheck(): Promise<ClaudeCheckResult> {
  return await invoke<ClaudeCheckResult>('claude_check');
}
