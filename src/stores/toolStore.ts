import { create } from 'zustand';
import { openToolsApi, type OpenTool } from '../services/service';

/**
 * 打开工具共享 store：全局工具库 + 当前项目的服务绑定。
 * 工具是全局配置（跨项目复用），绑定按项目加载——useProjectDetail 在
 * 详情加载时调用 loadProject 刷新，服务卡片/编辑面板各自订阅取用。
 */

interface ToolStore {
  /** 全局工具库（按添加顺序） */
  openTools: OpenTool[];
  /** 当前项目的服务绑定：serviceId → toolId */
  bindings: Record<string, string>;
  /** 绑定所属项目（切换项目时清空旧绑定，避免串项目） */
  loadedProject: string | null;
  /** 加载全局工具库（幂等，已有数据时跳过重复请求） */
  ensureToolsLoaded: () => Promise<void>;
  /** 加载某项目的全部服务绑定（项目详情加载时调用） */
  loadProject: (projectId: string) => Promise<void>;
  /** 绑定/解绑服务（写后端 + 更新本地） */
  bind: (serviceId: string, toolId: string | null) => Promise<void>;
  /** 工具库增删改后刷新本地列表 */
  refreshTools: () => Promise<void>;
}

export const useToolStore = create<ToolStore>((set, get) => ({
  openTools: [],
  bindings: {},
  loadedProject: null,

  ensureToolsLoaded: async () => {
    if (get().openTools.length > 0) return;
    try {
      set({ openTools: await openToolsApi.list() });
    } catch (e) {
      console.error('加载打开工具列表失败:', e);
    }
  },

  loadProject: async (projectId) => {
    set({ loadedProject: projectId });
    await get().ensureToolsLoaded();
    try {
      const list = await openToolsApi.listBindings(projectId);
      // 响应返回前可能已切到另一项目：过期响应不得覆盖新项目的绑定
      if (get().loadedProject !== projectId) return;
      const bindings: Record<string, string> = {};
      for (const b of list) bindings[b.service_id] = b.tool_id;
      set({ bindings });
    } catch (e) {
      console.error('加载服务工具绑定失败:', e);
      if (get().loadedProject === projectId) set({ bindings: {} });
    }
  },

  bind: async (serviceId, toolId) => {
    await openToolsApi.bindService(serviceId, toolId);
    set(state => {
      const next = { ...state.bindings };
      if (toolId) next[serviceId] = toolId;
      else delete next[serviceId];
      return { bindings: next };
    });
  },

  refreshTools: async () => {
    try {
      set({ openTools: await openToolsApi.list() });
    } catch (e) {
      console.error('刷新打开工具列表失败:', e);
    }
  },
}));
