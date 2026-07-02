import { create } from 'zustand';
import type { ServiceLogLine } from '../services/logService';

interface LogStore {
  logs: Record<string, ServiceLogLine[]>;
  /** 版本号，每次 bulkAppend 递增。LogViewer 用它检测新数据（解决 5000 行上限后行数不变的问题） */
  version: Record<string, number>;
  appendLog: (serviceKey: string, stream: 'stdout' | 'stderr', data: string) => void;
  bulkAppend: (items: Array<{ serviceKey: string; stream: 'stdout' | 'stderr'; data: string }>) => void;
  /** 同步设置日志数据（由组件调用 service 后传入） */
  setLogs: (serviceKey: string, lines: ServiceLogLine[]) => void;
  clearLogs: (serviceKey: string) => void;
  pruneInactive: (activeKeys: Set<string>) => void;
}

const MAX_LINES = 5000;

export const useLogStore = create<LogStore>((set) => ({
  logs: {},
  version: {},

  appendLog: (serviceKey, stream, data) => {
    const now = new Date().toISOString();
    const line: ServiceLogLine = { timestamp: now, stream, text: data };
    set((state) => {
      const existing = state.logs[serviceKey] ?? [];
      const updated = [...existing, line];
      if (updated.length > MAX_LINES) updated.splice(0, updated.length - MAX_LINES);
      return { logs: { ...state.logs, [serviceKey]: updated }, version: { ...state.version, [serviceKey]: (state.version[serviceKey] ?? 0) + 1 } };
    });
  },

  bulkAppend: (items) => {
    if (items.length === 0) return;
    const now = new Date().toISOString();
    set((state) => {
      const logs = { ...state.logs };
      const version = { ...state.version };
      for (const { serviceKey, stream, data } of items) {
        const existing = logs[serviceKey] ?? [];
        const line: ServiceLogLine = { timestamp: now, stream, text: data };
        const updated = [...existing, line];
        if (updated.length > MAX_LINES) updated.splice(0, updated.length - MAX_LINES);
        logs[serviceKey] = updated;
        version[serviceKey] = (version[serviceKey] ?? 0) + 1;
      }
      return { logs, version };
    });
  },

  setLogs: (serviceKey, lines) => {
    set((state) => ({ logs: { ...state.logs, [serviceKey]: lines }, version: { ...state.version, [serviceKey]: (state.version[serviceKey] ?? 0) + 1 } }));
  },

  clearLogs: (serviceKey) => {
    set((state) => {
      const { [serviceKey]: _, ...rest } = state.logs;
      const { [serviceKey]: __, ...restVer } = state.version;
      return { logs: rest, version: restVer };
    });
  },

  /** 清理不在 activeKeys 中的所有日志（服务停止后调用） */
  pruneInactive: (activeKeys: Set<string>) => {
    set((state) => {
      const logs: Record<string, ServiceLogLine[]> = {};
      const version: Record<string, number> = {};
      for (const key of activeKeys) {
        if (state.logs[key]) logs[key] = state.logs[key];
        if (state.version[key] !== undefined) version[key] = state.version[key];
      }
      return { logs, version };
    });
  },
}));
