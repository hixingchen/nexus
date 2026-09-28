import { create } from 'zustand';
import type { Service } from '../services/service';

/**
 * 项目服务列表的**唯一来源**（ARCH-7 ③）。
 *
 * 共享原因：详情页每次 load 拿到最新 services 时同步写入缓存，
 * 左侧保持展开的项目行/服务行与右侧编辑/增删即时一致（不再需要折叠重拉）。
 *
 * 引用计数（`retain` / `release`）：折叠左侧项目行会 `invalidate` 以释放内存，
 * 但详情页可能正开着同一个项目——直接删掉会让右侧服务列表整片消失。
 * 因此"正在被使用"的项目由使用方登记，`invalidate` 只在无人持有时真正删除。
 */

interface SvcCacheState {
  cache: Record<string, Service[]>;
  /** 各项目的持有者数量（详情页挂载即持有；左侧折叠只在 0 时删除缓存） */
  refs: Record<string, number>;
  /** 折叠时被请求过删除、但因仍被持有而推迟的项目（持有者释放后真正删除） */
  pendingEvict: Record<string, true>;
  /**
   * 详情 load 成功后写入（覆盖旧缓存）。
   *
   * 写入即撤销本项目此前"待删除"的请求——见实现处的说明（UX-19）。
   */
  setCache: (projectId: string, services: Service[]) => void;
  /** 声明"我正在用这个项目的服务列表"（在 effect 里成对调用 retain/release） */
  retain: (projectId: string) => void;
  release: (projectId: string) => void;
  /** 请求清除某项目缓存（折叠项目时调用；仍被持有时推迟到释放之后） */
  invalidate: (projectId: string) => void;
}

export const useSvcCacheStore = create<SvcCacheState>((set, get) => ({
  cache: {},
  refs: {},
  pendingEvict: {},

  /**
   * 为什么写入时要清掉 `pendingEvict`（UX-19）：那个标记是**折叠项目行**时留下的
   * "待删请求"，而列表重新展开同一个项目（或详情页 load）会再写一次缓存——数据既然
   * 刚回来，旧的删除请求就已经过期了。
   *
   * 不清它会出现这个 bug（实测复现）：折叠 → 标记待删（详情页还持有，删不了）→ 双击
   * 重新展开（缓存写入，标记却留着）→ 切到别的项目（详情页释放引用）→ `release` 按
   * 那条陈旧请求把**刚装回来**的服务列表删掉。表现是"展开着的项目卡片显示『暂无服务
   * 目录树』"，而折叠它的动作根本没发生过。
   */
  setCache: (projectId, services) =>
    set(st => {
      const cache = { ...st.cache, [projectId]: services };
      if (!st.pendingEvict[projectId]) return { cache };
      const pendingEvict = { ...st.pendingEvict };
      delete pendingEvict[projectId];
      return { cache, pendingEvict };
    }),

  retain: (projectId) =>
    set(st => ({ refs: { ...st.refs, [projectId]: (st.refs[projectId] ?? 0) + 1 } })),

  release: (projectId) => {
    const next = Math.max(0, (get().refs[projectId] ?? 1) - 1);
    set(st => {
      const refs = { ...st.refs, [projectId]: next };
      // 释放后无人持有，且期间被请求过删除 → 真正删除（推迟的清理）
      if (next === 0 && st.pendingEvict[projectId]) {
        const cache = { ...st.cache };
        delete cache[projectId];
        const pendingEvict = { ...st.pendingEvict };
        delete pendingEvict[projectId];
        return { refs, cache, pendingEvict };
      }
      return { refs };
    });
  },

  invalidate: (projectId) => {
    // 仍被持有（详情页开着同一个项目）：只记下"想删"，等释放时再删——
    // 直接删会让右侧服务列表在左侧折叠时整片消失
    if ((get().refs[projectId] ?? 0) > 0) {
      set(st => ({ pendingEvict: { ...st.pendingEvict, [projectId]: true } }));
      return;
    }
    set(st => {
      const cache = { ...st.cache };
      delete cache[projectId];
      return { cache };
    });
  },
}));

