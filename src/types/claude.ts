/// Claude Code 消息类型
export type ClaudeMessageKind = 'text' | 'thinking' | 'tool_use' | 'tool_result' | 'init' | 'result' | 'error';

/// 前端展示用的消息条目
export interface ClaudeChatMessage {
  id: string;
  role: 'user' | 'assistant' | 'system' | 'tool';
  content: string;
  thinking?: string;
  toolName?: string;
  toolInput?: unknown;
  toolResult?: string;
  toolIsError?: boolean;
  costUsd?: number;
  durationMs?: number;
  timestamp: number;
}

/// Claude Code 会话状态
export type ClaudeSessionStatus = 'idle' | 'running' | 'error';

/// Claude Code 后端事件 payload
export interface ClaudeMessageEvent {
  kind: ClaudeMessageKind;
  delta?: string;
  id?: string;
  name?: string;
  input?: unknown;
  tool_use_id?: string;
  content?: string;
  is_error?: boolean;
  text?: string;
  cost_usd?: number;
  duration_ms?: number;
  session_id?: string;
  model?: string;
  tools?: string[];
  message?: string;
}

/// Claude 启动参数
export interface ClaudeStartParams {
  prompt: string;
  workingDir?: string;
  model?: string;
  permissionMode?: string;
  allowedTools?: string[];
  systemPrompt?: string;
  sessionId?: string;
}

/// Claude CLI 检查结果
export interface ClaudeCheckResult {
  available: boolean;
  path: string;
  version: string;
}
