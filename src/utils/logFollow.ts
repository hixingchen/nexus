// ── 日志面板「是否贴底跟随」的判定 ──────────────────────────────
//
// 为什么单独成文件：这条判据是提交 4c6ab57 修的那个 bug 的核心（批量新增被误判成
// "用户在翻历史"→ 跟随静默停止，误判不自愈），而它此前以一行内联表达式的形式长在
// LogViewer 的 .tsx 里——`.tsx` 无法被 Node 的类型擦除处理（见 test/ts-hooks.mjs），
// 于是这条判定**在测试里不可达**。与 utils/pasteResult.ts、utils/serviceFailure.ts
// 同一套做法：把可判定的谓词抽成零依赖纯函数，组件只做取值 + 调用（审计 NEW-25）。
//
// 纯函数、不碰 DOM：只要三个数字就能判定。

/** 滚动容器的三个度量（等价于 DOM 的 scrollHeight / scrollTop / clientHeight） */
export interface ScrollMetrics {
  scrollHeight: number;
  scrollTop: number;
  clientHeight: number;
}

/**
 * 判定"用户在底部"的容差（px）：距底小于它视为跟随中，可以贴底。
 *
 * 24px 的依据：一行日志约 21px（ESTIMATED_ROW_H），容差略大于一行——既容得下子像素
 * 舍入与粘性滚动的零头，又不至于把"往上翻了两行"当成还在底部。
 */
export const STICK_TO_BOTTOM_EPSILON = 24;

/**
 * 视口（或其一部分）是否贴着底部。
 *
 * 判据是"距底的像素距离 ≤ 容差"，用 `<=` 而不是 `<`：程序贴底后量到的距离常常不是
 * 精确的 0（行高是估算值 + ResizeObserver 实测回填，两者会有亚像素差），严格小于会把
 * 恰好落在这个零头上的情况判成"用户在翻历史"。
 * 距离为负（部分引擎在边界回弹时会给出 scrollTop 略大于最大值的值）同样算贴底。
 */
export function isStickToBottom(metrics: ScrollMetrics, epsilon = STICK_TO_BOTTOM_EPSILON): boolean {
  return metrics.scrollHeight - metrics.scrollTop - metrics.clientHeight <= epsilon;
}
