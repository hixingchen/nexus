import { create } from 'zustand';

/**
 * 「这次关窗请求来自托盘退出」的一次性标记。
 *
 * 为什么需要：开启「关闭到托盘」后，窗口关闭按钮只隐藏窗口；而托盘菜单的「退出」
 * 也必须走同一套关窗守卫（未保存草稿确认），不能被误判成"隐藏"。托盘退出先置这个
 * 标记再 `window.close()`，`CloseGuard` 读到后跳过"隐藏"分支，走正常的确认/退出流程。
 *
 * 用一次性标记而不是持久设置：它只描述**当前这一次**关闭请求。
 */
interface ExitIntentState {
  trayQuit: boolean;
  /** 托盘「退出」：标记下一次关窗请求是真正的退出 */
  requestTrayQuit: () => void;
  /** 读取并消费标记（同一次关闭请求只消费一次） */
  consumeTrayQuit: () => boolean;
}

export const useExitIntentStore = create<ExitIntentState>((set, get) => ({
  trayQuit: false,
  requestTrayQuit: () => set({ trayQuit: true }),
  consumeTrayQuit: () => {
    const value = get().trayQuit;
    if (value) set({ trayQuit: false });
    return value;
  },
}));
