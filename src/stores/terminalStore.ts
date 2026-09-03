import { create } from 'zustand';

/**
 * 内嵌终端（Claude CLI）状态，两层关闭语义：
 *
 * - session：会话宿主（cwd/name）。非空 = claude 会话存在（面板保持挂载，
 *   即使隐藏也不卸载 → 不杀进程）。仅「右上角 ×」清除（真关闭）。
 * - visible：面板显隐。图标按钮只翻转它——隐藏 = 收起面板但会话继续跑。
 *
 * 状态全局化的原因：右侧服务面板展开态工具栏与收起态折叠条都要一键
 * 开关/显隐终端，按钮组件直接订阅 store，避免逐层 props 钻孔。
 */

export interface TerminalSession {
  cwd: string;
  name: string;
}

interface TerminalStore {
  /** 会话宿主（null = 无会话，面板卸载） */
  session: TerminalSession | null;
  /** 面板是否显示（session 存在时隐藏 = 保活收起） */
  visible: boolean;
  /** 打开/切换会话（不同目录 = 替换旧会话并 kill；同目录已在跑 = 幂等） */
  openTerminal: (cwd: string, name: string) => void;
  /** 真关闭：清除会话（面板卸载 → 后端杀 claude） */
  closeTerminal: () => void;
  /** 图标切换：同目录会话存在 → 翻转显隐；无会话/不同目录 → 打开 */
  toggleTerminal: (cwd: string, name: string) => void;
}

export const useTerminalStore = create<TerminalStore>((set, get) => ({
  session: null,
  visible: false,

  openTerminal: (cwd, name) => {
    const s = get().session;
    if (s && s.cwd === cwd) {
      // 同目录会话已在（含隐藏中）：展开显示
      set({ visible: true });
    } else {
      set({ session: { cwd, name }, visible: true });
    }
  },

  closeTerminal: () => {
    set({ session: null, visible: false });
  },

  toggleTerminal: (cwd, name) => {
    const s = get().session;
    if (s && s.cwd === cwd) {
      set(st => ({ visible: !st.visible }));
    } else {
      set({ session: { cwd, name }, visible: true });
    }
  },
}));
