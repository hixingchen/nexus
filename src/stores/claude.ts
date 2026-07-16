import { create } from 'zustand';
import type { ClaudeChatMessage, ClaudeSessionStatus, ClaudeMessageEvent } from '../types/claude';
import { claudeStart, claudeCheck } from '../services/claude';
import type { ClaudeStartParams } from '../types/claude';
import { listen } from '@tauri-apps/api/event';

interface ClaudeStore {
  /** Claude Code 分配的真实 session_id（用于 --resume 续接对话） */
  sessionId: string | null;
  /** 聊天消息列表 */
  messages: ClaudeChatMessage[];
  /** 会话状态 */
  status: ClaudeSessionStatus;
  /** 当前使用的模型 */
  model: string | null;
  /** 可用工具列表 */
  tools: string[];
  /** CLI 是否可用 */
  cliAvailable: boolean | null;
  /** CLI 版本 */
  cliVersion: string | null;

  // 操作
  sendMessage: (params: ClaudeStartParams) => Promise<void>;
  clearMessages: () => void;
  reset: () => void;
  checkCli: () => Promise<void>;
  setStatus: (status: ClaudeSessionStatus) => void;
}

let unlistenFn: (() => void) | null = null;

function generateId(): string {
  return `msg-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
}

export const useClaudeStore = create<ClaudeStore>((set, get) => ({
  sessionId: null,
  messages: [],
  status: 'idle',
  model: null,
  tools: [],
  cliAvailable: null,
  cliVersion: null,

  checkCli: async () => {
    try {
      const result = await claudeCheck();
      set({ cliAvailable: result.available, cliVersion: result.version });
    } catch {
      set({ cliAvailable: false, cliVersion: null });
    }
  },

  sendMessage: async (params) => {
    const { status, sessionId } = get();
    if (status === 'running') return;

    // 添加用户消息
    const userMsg: ClaudeChatMessage = {
      id: generateId(),
      role: 'user',
      content: params.prompt,
      timestamp: Date.now(),
    };

    set((state) => ({
      messages: [...state.messages, userMsg],
      status: 'running',
    }));

    try {
      // 注册事件监听
      if (unlistenFn) {
        unlistenFn();
        unlistenFn = null;
      }

      unlistenFn = await listen<ClaudeMessageEvent>('claude-message', (event) => {
        const payload = event.payload;

        switch (payload.kind) {
          case 'init': {
            // Claude Code 返回真实的 session_id，后续消息用 --resume 续接
            const realSessionId = payload.session_id || null;
            set({
              sessionId: realSessionId,
              model: payload.model || null,
              tools: payload.tools || [],
            });
            break;
          }

          case 'text':
            if (payload.delta) {
              const delta = payload.delta;
              set((state) => {
                const msgs = [...state.messages];
                const lastMsg = msgs[msgs.length - 1];

                if (lastMsg && lastMsg.role === 'assistant' && !lastMsg.toolName) {
                  lastMsg.content += delta;
                } else {
                  msgs.push({
                    id: generateId(),
                    role: 'assistant',
                    content: delta,
                    timestamp: Date.now(),
                  });
                }
                return { messages: msgs };
              });
            }
            break;

          case 'thinking':
            if (payload.delta) {
              const delta = payload.delta;
              set((state) => {
                const msgs = [...state.messages];
                const lastMsg = msgs[msgs.length - 1];

                if (lastMsg && lastMsg.role === 'assistant') {
                  lastMsg.thinking = (lastMsg.thinking || '') + delta;
                } else {
                  msgs.push({
                    id: generateId(),
                    role: 'assistant',
                    content: '',
                    thinking: delta,
                    timestamp: Date.now(),
                  });
                }
                return { messages: msgs };
              });
            }
            break;

          case 'tool_use':
            if (payload.name) {
              set((state) => ({
                messages: [...state.messages, {
                  id: generateId(),
                  role: 'tool' as const,
                  content: '',
                  toolName: payload.name,
                  toolInput: payload.input,
                  timestamp: Date.now(),
                }],
              }));
            }
            break;

          case 'tool_result':
            if (payload.tool_use_id) {
              set((state) => {
                const msgs = [...state.messages];
                for (let i = msgs.length - 1; i >= 0; i--) {
                  if (msgs[i].role === 'tool' && msgs[i].toolName) {
                    msgs[i] = {
                      ...msgs[i],
                      toolResult: payload.content || '',
                      toolIsError: payload.is_error || false,
                    };
                    break;
                  }
                }
                return { messages: msgs };
              });
            }
            break;

          case 'result':
            set((state) => {
              const msgs = [...state.messages];
              if (payload.text) {
                const lastMsg = msgs[msgs.length - 1];
                const isDuplicate = lastMsg
                  && lastMsg.role === 'assistant'
                  && lastMsg.content === payload.text;

                if (!isDuplicate) {
                  msgs.push({
                    id: generateId(),
                    role: 'assistant',
                    content: payload.text,
                    costUsd: payload.cost_usd,
                    durationMs: payload.duration_ms,
                    timestamp: Date.now(),
                  });
                } else if (lastMsg) {
                  lastMsg.costUsd = payload.cost_usd;
                  lastMsg.durationMs = payload.duration_ms;
                }
              }
              return {
                messages: msgs,
                status: 'idle' as const,
                // result 中的 session_id 也是真实 ID，确保同步
                sessionId: payload.session_id || state.sessionId,
              };
            });
            break;

          case 'error':
            set((state) => ({
              messages: [...state.messages, {
                id: generateId(),
                role: 'system' as const,
                content: payload.message || '未知错误',
                timestamp: Date.now(),
              }],
              status: 'error' as const,
            }));
            break;
        }
      });

      // 关键：如果有 sessionId 则传给后端，后端会用 --resume 续接
      const startParams: ClaudeStartParams = {
        ...params,
        sessionId: sessionId || undefined,
      };

      await claudeStart(startParams);
    } catch (error) {
      set((state) => ({
        messages: [...state.messages, {
          id: generateId(),
          role: 'system',
          content: `启动失败: ${error}`,
          timestamp: Date.now(),
        }],
        status: 'error',
      }));
    }
  },

  clearMessages: () => {
    // 清空消息但保留 sessionId，新对话继续在同一会话中
    set({ messages: [] });
  },

  reset: () => {
    // 完全重置，丢弃会话
    if (unlistenFn) {
      unlistenFn();
      unlistenFn = null;
    }
    set({
      sessionId: null,
      messages: [],
      status: 'idle',
      model: null,
      tools: [],
    });
  },

  setStatus: (status) => set({ status }),
}));
