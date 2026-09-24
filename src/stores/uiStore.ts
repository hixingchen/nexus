import { create } from 'zustand';

interface UiState {
  /** 当前打开的全局弹窗数量（可叠加；>0 表示有全屏遮罩弹窗打开） */
  modalCount: number;
  pushModal: () => void;
  popModal: () => void;
  /**
   * 弹窗栈（后进先出）：每个 Modal 挂载时压入一个自增 id，卸载时移除。
   *
   * 为什么需要"谁在最上面"这个信息：Modal 的 Escape 监听挂在 `document` 上，
   * 而弹窗是可以叠加的（工具库 → 删除确认）。没有栈时按一次 Esc 会触发**所有**
   * 监听器——两个弹窗一起关掉，而用户只想退出最上面那一层（UX-6）。
   * 栈顶之外一律不响应。
   */
  modalStack: number[];
  pushModalId: (id: number) => void;
  popModalId: (id: number) => void;
}

/**
 * 全局 UI 状态：跨组件协调。
 * 典型使用：弹窗打开时 AiPanel 把原生子 WebView 移出屏幕——
 * 原生层不受 DOM 遮罩（z-index）约束，不移走则弹窗期间 dsh 页面仍可点击。
 */
export const useUiStore = create<UiState>((set) => ({
  modalCount: 0,
  pushModal: () => set((s) => ({ modalCount: s.modalCount + 1 })),
  popModal: () => set((s) => ({ modalCount: Math.max(0, s.modalCount - 1) })),
  modalStack: [],
  pushModalId: (id) => set((s) => ({ modalStack: [...s.modalStack, id] })),
  popModalId: (id) => set((s) => ({ modalStack: s.modalStack.filter(x => x !== id) })),
}));

let nextModalId = 0;
/** 取一个单调递增的弹窗 id（不回收：回收会让"卸载时的移除"撞上后来者） */
export function newModalId(): number {
  nextModalId += 1;
  return nextModalId;
}
