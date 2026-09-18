/**
 * 通知出口的**端口**：让 `utils/` 与 `stores/` 能在不 import React 组件的前提下弹提示。
 *
 * 为什么需要它（架构审计 ARCH-17）：弹提示的实现在 `components/ui/Toast.tsx`，而
 * `utils/error.ts`、`utils/clipboard.ts`、`stores/editor.ts`、`stores/serviceActions.ts`
 * 都要在出错时提示一句——于是它们各自 `import { showNotification } from '../components/ui/Toast'`，
 * 形成 **低层反向依赖高层**。代价不是今天出错，而是状态层无法离开 DOM 环境测试
 * （zustand 本来可以脱离 React 跑，只要它不 import 组件）。
 *
 * 依赖方向由此反转：低层只认这个端口（本文件不含任何 import），实现由
 * `components/ui/Toast.tsx` 在模块加载时注册进来。谁注册、用什么渲染，是 UI 层的事。
 */

/** 与 `Toast.tsx` 的 `ToastOptions` 同形——但这里不能引用它（那正是要断开的依赖） */
export interface NotifyOptions {
  variant?: 'success' | 'error' | 'warning' | 'info';
  title: string;
  description?: string;
  duration?: number;
  /**
   * 可选的行动按钮（如保存冲突提示里的「重新加载」）。
   *
   * 为什么要有它：错误提示最有用的时候，是用户**就在现场**的这几秒。让用户读完提示
   * 再去别处找菜单做同一件事，等于把"应用知道该怎么办"降级成"用户自己去想怎么办"。
   * 点击后 Toast 自行关闭（由渲染层负责），`run` 只管做事。
   */
  action?: { label: string; run: () => void };
}

type Notifier = (opts: NotifyOptions) => void;

let notifier: Notifier | null = null;

/** 由 UI 层调用（`Toast.tsx` 注册自己的 `showNotification`）。传 null 可注销（测试用） */
export function setNotifier(n: Notifier | null): void {
  notifier = n;
}

/**
 * 弹一条通知。
 *
 * 未注册时只留控制台痕迹、不抛错：通知是"顺带告知"，让它把主流程带崩是更坏的结果。
 * 实际不会发生——`MainLayout` 挂 `<Toaster>` 时必然 import 了 Toast。
 */
export function notify(opts: NotifyOptions): void {
  if (!notifier) {
    console.warn(`通知出口尚未注册（UI 层未加载），本条提示被丢弃: ${opts.title}`, opts.description ?? '');
    return;
  }
  notifier(opts);
}
