// ── 日志面板「要不要自动关闭」的判据 ──────────────────────────────
//
// 为什么单独成文件：这条判据守的是一次真实故障——原来拿「running/failed 里暂时
// 没有它」当"用户已停止"，而自动重启与手动重启都是后端 stop → start，中间有一段
// 服务既不在 running 也不在 failed 的空窗。自动重启循环里这条判据会把面板当开关拉，
// 高频挂载/卸载最终留下"日志已经关掉、DOM 却还留在主区"的脱节态（日志冻结不再更新、
// ✕ 点了没反应、再打开的日志被挤到最下方）。
//
// 现在只认两种**确定事实**：服务已从项目配置中删除；或用户显式停止（stopSignal）。
// 纯函数、零依赖，可直接被 Node 内置测试器跑（理由同 utils/projectDetail.ts）。

/** 用户显式停止的信号（与 runningStore.stopSignal 同形；seq 单调递增） */
export interface StopSignal {
  seq: number;
  serviceIds: string[];
}

/**
 * 日志面板是否应当自动关闭（并清空该服务日志）。
 *
 * @param serviceId      正在查看的服务 id
 * @param serviceExists  该服务是否还在项目配置里
 * @param stopSignal     全局的显式停止信号
 * @param openedStopSeq  面板打开那一刻快照的 stopSignal.seq
 *
 * 为什么必须带 `openedStopSeq`：stopSignal 会一直留着上一次停过的服务 id。少了序号比较，
 * "停止服务 → 再启动 → 打开日志"会因为上一次的残留信号**立刻**把刚打开的面板关掉——
 * 这正是修复这种误关时最容易引入的新 bug。
 */
export function shouldAutoCloseLog(params: {
  serviceId: string;
  serviceExists: boolean;
  stopSignal: StopSignal;
  openedStopSeq: number;
}): boolean {
  if (!params.serviceExists) return true;
  return params.stopSignal.seq > params.openedStopSeq
    && params.stopSignal.serviceIds.includes(params.serviceId);
}
