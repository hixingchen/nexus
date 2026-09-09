import { create } from 'zustand';

interface UiState {
  /** 当前打开的全局弹窗数量（可叠加；>0 表示有全屏遮罩弹窗打开） */
  modalCount: number;
  pushModal: () => void;
  popModal: () => void;
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
}));
