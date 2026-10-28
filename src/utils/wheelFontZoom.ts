/**
 * Ctrl / Cmd + 滚轮调字号的**纯事件判定**（不碰 DOM、不碰 store，可被 node 直接测）。
 *
 * 抽出来的原因（铁律 19）：真正的 hook 要挂原生 `wheel` 监听并 `preventDefault`，
 * 而「修饰键判断 / 增量累计 / 什么时候拦默认行为」这些容易写错的点，
 * 不该只靠"在浏览器里滚一下看看"。
 */

/**
 * 滚轮累计到多少（像素）才走一步字号。
 *
 * 鼠标滚轮一格在 Chromium 里约 100；触控板会拆成很多个位数的增量——不累计的话
 * 一下手势就能从 12 直接冲到 20。
 */
export const WHEEL_STEP_THRESHOLD = 40;

/** `createWheelZoomHandler` 需要的最小事件形状（测试可以给假对象） */
export interface WheelZoomEvent {
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  deltaY: number;
  preventDefault: () => void;
}

/**
 * 造一个 wheel 处理器：按住 Ctrl（macOS 也认 Cmd）且是纵向滚动时，
 * **先拦住默认行为**（WebView2 的整页缩放），累计到阈值后回调一次 `step(累计 deltaY)`。
 *
 * 拦默认行为必须发生在阈值判断**之前**：到上下限时 `step` 不会改字号，但也不能让
 * 浏览器退化成"整页放大"。
 */
export function createWheelZoomHandler(step: (deltaY: number) => void) {
  let accumulated = 0;
  return (e: WheelZoomEvent) => {
    // Alt 组合留给别的功能；没有修饰键时是普通滚动，原样放行
    if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
    // 横向滚轮（deltaY=0）不是缩放意图
    if (e.deltaY === 0) return;

    e.preventDefault();

    accumulated += e.deltaY;
    if (Math.abs(accumulated) < WHEEL_STEP_THRESHOLD) return;

    const delta = accumulated;
    accumulated = 0;
    step(delta);
  };
}
