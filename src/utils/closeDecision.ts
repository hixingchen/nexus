/**
 * 关窗动作决策（纯函数，单测覆盖）。
 *
 * 三个入口共用同一套判断：
 * - 标题栏 ✕ / Alt+F4 / Ctrl+W：普通关闭；
 * - 托盘「退出」：先请求退出，再走 `window.close()`（`trayQuit = true`）；
 * - 开启「关闭到托盘」时：普通关闭 = 隐藏，草稿留在内存里，不打扰用户。
 *
 * 优先级：**托盘退出 > 关闭到托盘**。否则开了托盘模式后，托盘里的「退出」会变成
 * "再隐藏一次"，用户永远退不出应用。
 */
export type CloseAction =
  /** 弹未保存草稿确认 */
  | 'ask'
  /** 真正退出（后端 prepare_exit） */
  | 'exit'
  /** 隐藏到托盘，服务与草稿都留在内存里 */
  | 'hide';

export interface CloseDecisionInput {
  /** 本次关窗是否由托盘「退出」发起 */
  trayQuit: boolean;
  /** 设置里是否开启「关闭窗口时最小化到托盘」 */
  closeToTray: boolean;
  /** 未保存的文件 + 服务/模板草稿数 */
  dirtyCount: number;
}

export function decideCloseAction({ trayQuit, closeToTray, dirtyCount }: CloseDecisionInput): CloseAction {
  // 托盘退出是明确的"我要退出"：即使开着关闭到托盘，也必须走确认/退出
  if (trayQuit) return dirtyCount > 0 ? 'ask' : 'exit';
  // 关闭到托盘：隐藏不销毁 WebView，草稿不会丢，因此不需要确认
  if (closeToTray) return 'hide';
  return dirtyCount > 0 ? 'ask' : 'exit';
}
