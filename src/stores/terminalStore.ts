import { create } from 'zustand';

/**
 * 内嵌终端请求通道：服务卡片右键「Claude 终端」→ ProjectDetail 订阅并挂载终端列。
 * 用 store 而非逐层 props 钻孔（ProjectDetail→ServicePanel→ExpandedView→ServiceSection→Entry 共 4 层）。
 * seq 递增保证"同目录已打开时再次请求"也能让订阅方感知（ProjectDetail 幂等处理）。
 */

export interface TerminalRequest {
  cwd: string;
  name: string;
  seq: number;
}

interface TerminalStore {
  request: TerminalRequest | null;
  /** 请求在项目路径下打开 claude 终端（seq 递增保证每次都触发订阅方） */
  openTerminal: (cwd: string, name: string) => void;
}

export const useTerminalStore = create<TerminalStore>((set, get) => ({
  request: null,

  openTerminal: (cwd, name) => {
    // 不去重：关闭面板后再次右键同一项目也必须能重开（ProjectDetail 端
    // 对同值 host 的 setState 会自动 bail out，重复触发无副作用）
    const prev = get().request;
    set({ request: { cwd, name, seq: (prev?.seq ?? 0) + 1 } });
  },
}));
