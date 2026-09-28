/**
 * 项目详情的"归属"规则（纯函数，便于直接测）。
 *
 * 守的是一处真实症状（UX-18）：切项目时 `detail` 必须在**读取处**就与当前项目对齐——
 * 原来靠 effect 里补一次 `setDetail(null)` 追，那一帧里上一个项目的详情仍会被当成
 * 新项目的渲染出去（服务列显示上一个项目的服务，紧接着又是「暂无服务」与整片
 * 「加载中…」，一帧一个样，看起来就是"切项目时界面一闪一闪"）。
 */

/** 带项目 id 的详情槽位：详情必须和它所属的项目一起存（见 ProjectDetail 的用法） */
export interface DetailSlot<T> {
  projectId: string;
  detail: T;
}

/**
 * 当前该显示哪个项目的详情。
 *
 * 优先级：**同源的 state**（最新，含刚 load 回来的）→ **缓存里那一份**（切回看过的
 * 项目时首帧就有内容）→ null（真·加载中）。不同源的 state 一律不用：它就是"上一个
 * 项目的详情"，拿来渲染新项目等于把别人的数据当成自己的。
 */
export function selectProjectDetail<T>(
  slot: DetailSlot<T> | null,
  projectId: string,
  cache: ReadonlyMap<string, T>,
): T | null {
  if (slot && slot.projectId === projectId) return slot.detail;
  return cache.get(projectId) ?? null;
}

/**
 * 服务列表是否"还没到"——空列表这时**不能**当成"这个项目没有服务"。
 *
 * 判据不是"列表为空"：本项目详情尚未落地（state 里没有它）且共享缓存里也没有服务，
 * 才是"不知道有没有服务"。若只看空列表，切项目那一帧就会显示「暂无服务」+
 * 「添加服务」按钮，说的是件没根据的话（用户看到的是"我的服务不见了"）。
 */
export function servicesPending<T>(
  slot: DetailSlot<T> | null,
  projectId: string,
  services: readonly unknown[],
): boolean {
  return slot?.projectId !== projectId && services.length === 0;
}
