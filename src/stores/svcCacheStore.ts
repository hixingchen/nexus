import { create } from 'zustand';
import type { Service } from '../services/service';

/**
 * 项目服务列表缓存（左侧项目列表展开态显示用）。
 *
 * 共享原因：详情页每次 load 拿到最新 services 时同步写入缓存，
 * 左侧保持展开的项目行/服务行与右侧编辑/增删即时一致（不再需要折叠重拉）。
 */

interface SvcCacheState {
  cache: Record<string, Service[]>;
  /** 详情 load 成功后写入（覆盖旧缓存） */
  setCache: (projectId: string, services: Service[]) => void;
  /** 清除某项目缓存（折叠项目时调用，释放内存） */
  invalidate: (projectId: string) => void;
}

export const useSvcCacheStore = create<SvcCacheState>((set) => ({
  cache: {},

  setCache: (projectId, services) =>
    set(st => ({ cache: { ...st.cache, [projectId]: services } })),

  invalidate: (projectId) =>
    set(st => {
      const next = { ...st.cache };
      delete next[projectId];
      return { cache: next };
    }),
}));
