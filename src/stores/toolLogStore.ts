import { create } from 'zustand';

/**
 * 工具命令输出行（按 run_id 分桶）。
 *
 * 为什么单独一个 store 而不是放在 `ProjectDetail` 的 state 里：
 * 后端按 50ms 批量 `emit("tool-command-log-batch")`（`commands/process.rs`），
 * 原实现每行一次 `setToolCommandState` → **整个 ProjectDetail 子树**（标签栏 + 编辑器 +
 * 全部服务卡 + 模板卡 + 两套 dnd context）跟着重渲染，弹窗里还要把最多 2000 行重新
 * `join('\n')`。`npm install`/`mvn` 这类命令每秒数百行时界面基本被占满。
 *
 * 现在改成：事件回调只往模块级缓冲里推（零 React 工作），50ms 合帧一次写入 store；
 * 只有订阅了该 run_id 的弹窗组件会重渲染，`join` 也只在合帧后做一次。
 */

/** 合帧窗口（与 MainLayout 的服务日志批量一致） */
const FLUSH_MS = 50;
/** 与服务日志一致的保留上限 */
const MAX_LINES = 2000;

/** 模块级缓冲：emit 频率远高于渲染帧率，逐条 setState 是纯粹的浪费 */
const pending = new Map<string, string[]>();
let flushTimer: ReturnType<typeof setTimeout> | null = null;

interface ToolLogState {
  /** run_id → 行数组（仅在合帧时整体替换引用，保证订阅者能感知变化） */
  logs: Record<string, string[]>;
  /** 入队（回调里调，不做任何 React 工作） */
  append: (runId: string, lines: string[]) => void;
  /** 开始新一次运行：清空该 run 的行 */
  reset: (runId: string) => void;
  /** 运行结束且不再需要展示时清理（避免按 run 累积） */
  drop: (runId: string) => void;
}

/** 合帧：把缓冲里的行一次性并入 store */
function flushPending() {
  flushTimer = null;
  if (pending.size === 0) return;
  const entries = [...pending.entries()];
  pending.clear();
  useToolLogStore.setState(state => {
    const next = { ...state.logs };
    for (const [runId, lines] of entries) {
      const merged = [...(next[runId] ?? []), ...lines];
      // 超限时保留最新 MAX_LINES 行（滑动窗口，与服务日志同口径）
      if (merged.length > MAX_LINES) merged.splice(0, merged.length - MAX_LINES);
      next[runId] = merged;
    }
    return { logs: next };
  });
}

export const useToolLogStore = create<ToolLogState>((set, get) => ({
  logs: {},
  append: (runId, lines) => {
    if (lines.length === 0) return;
    const buf = pending.get(runId);
    if (buf) buf.push(...lines);
    else pending.set(runId, [...lines]);
    if (flushTimer === null) flushTimer = setTimeout(flushPending, FLUSH_MS);
  },
  reset: (runId) => {
    pending.delete(runId);
    if (!(runId in get().logs)) return;
    set(state => {
      const next = { ...state.logs };
      delete next[runId];
      return { logs: next };
    });
  },
  drop: (runId) => {
    pending.delete(runId);
    set(state => {
      if (!(runId in state.logs)) return state;
      const next = { ...state.logs };
      delete next[runId];
      return { logs: next };
    });
  },
}));

/** 空数组单例：订阅选择器返回它可避免"每次新建空数组"造成的无谓重渲染 */
export const EMPTY_LOGS: string[] = [];
