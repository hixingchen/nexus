import { useAiStore, selectAiPanelVisible } from '../stores/aiStore';

/**
 * AI 面板占用的右侧宽度（逻辑像素；0 = 未占用）。
 *
 * 背景：面板里的 dsh 页面是**原生子 WebView**，永远画在所有 DOM 之上（z-index、
 * 遮罩、portal 对它统统无效）。凡是可能落到窗口右侧的浮层——toast 通知、右键
 * 菜单/下拉浮层——都必须按「可用区 = 窗口宽 − 本值」定位，否则会落进面板矩形里
 * 被盖住且点不到。RestartConfirm 卡片是最早这么做的（右侧让位），本 hook 把口径
 * 收成一处，后来者直接复用。
 *
 * 口径与 AiPanel 的宽度事实源一致：panelOpen 或 installing 时面板撑开（安装进度
 * 期间即使 stop 收过面板也保持展开，见 AiPanel）；其余为 0。
 *
 * 不含拖拽中间态：调宽时宽度只写 DOM/手动宽度 ref，松手才落 store——拖拽期间
 * 浮层可能按旧宽度让位，松手即对齐（浮层是低频瞬态，不值得为它引入高频写）。
 *
 * 全屏弹窗打开时面板的子 WebView 会被移出屏幕（见 AiPanel 的 modalPause），但**面板
 * 那一列 DOM 仍在布局里占宽**，所以本值不减——通知/菜单继续对齐内容区，不会在弹窗
 * 期间突然跳到窗口居中再跳回来。
 */
export function useAiPanelReserve(): number {
  // 面板是否占宽 = selectAiPanelVisible（安装进度同样撑开面板），此处只多取一个宽度
  return useAiStore((s) => (selectAiPanelVisible(s) ? s.panelWidth : 0));
}
