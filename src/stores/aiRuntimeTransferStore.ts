import { create } from 'zustand';
import type { RuntimeTransferProgress } from '../services/aiService';

export type AiRuntimeTransferKind = 'export' | 'import';

/**
 * AI 运行时导出 / 导入的**全局**状态。
 *
 * 为什么不能放在设置面板的 `useState`：面板可以被关掉，而传输还在后端跑
 * （`spawn_blocking` 不受前端组件卸载影响）。局部 state 会随组件卸载一起消失，
 * 重新打开面板时既看不到进度，也看不到"正在传"的标志——用户会以为没在传、再点一次。
 * 与 `toolCommandStore` 是同一类问题（见那里的说明）。
 */
interface AiRuntimeTransferState {
  /** null = 没有传输在进行 */
  kind: AiRuntimeTransferKind | null;
  progress: RuntimeTransferProgress | null;
  /** 最近一次失败的原因（就地展示；下一次 start 清掉） */
  error: string;
  start: (kind: AiRuntimeTransferKind) => void;
  setProgress: (progress: RuntimeTransferProgress) => void;
  succeed: () => void;
  fail: (message: string) => void;
  clearError: () => void;
}

export const useAiRuntimeTransferStore = create<AiRuntimeTransferState>((set) => ({
  kind: null,
  progress: null,
  error: '',
  start: (kind) => set({ kind, progress: null, error: '' }),
  // 只收当前这次传输的事件：完成 / 失败后事件队列里可能还压着上一轮的事件
  setProgress: (progress) =>
    set((s) => (s.kind === progress.kind ? { progress } : s)),
  succeed: () => set({ kind: null, progress: null, error: '' }),
  fail: (error) => set({ kind: null, progress: null, error }),
  clearError: () => set({ error: '' }),
}));
