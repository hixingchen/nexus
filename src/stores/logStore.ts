import { create } from 'zustand';
import type { ServiceLogLine } from '../services/logService';

interface LogStore {
  /** 跟随数据源：始终最新 2000 行（滑动窗口），后台持续维护 */
  logs: Record<string, ServiceLogLine[]>;
  /** 暂停视图：暂停时的快照（最多 2000 行，满后冻结——新日志不再进入；\r 刷新帧仍可替换最后一行） */
  pausedLogs: Record<string, ServiceLogLine[]>;
  /** 版本号，每次 bulkAppend 递增。LogViewer 用它检测新数据（解决 2000 行上限后行数不变的问题） */
  version: Record<string, number>;
  /** 累计新增行数（只增不减，\r 刷新帧不计）。暂停时显示"新增 N 行"的依据 */
  totalAdded: Record<string, number>;
  appendLog: (serviceKey: string, stream: 'stdout' | 'stderr', data: string) => void;
  bulkAppend: (items: Array<{ serviceKey: string; stream: 'stdout' | 'stderr'; data: string; timestamp?: string }>) => void;
  /** 同步设置跟随数据源（由组件调用 service 后传入；不影响暂停视图） */
  setLogs: (serviceKey: string, lines: ServiceLogLine[]) => void;
  clearLogs: (serviceKey: string) => void;
  pruneInactive: (activeKeys: Set<string>) => void;
  /** 暂停：快照当前跟随数据源作为暂停视图（之后最多涨到 2000 行，满后冻结） */
  pauseLogs: (serviceKey: string) => void;
  /** 恢复：丢弃暂停视图（LogViewer 切回跟随数据源，其始终是最新 2000 行） */
  resumeLogs: (serviceKey: string) => void;
}

/** 每服务日志行数上限（跟随数据源与暂停视图一致：最多 2000 行） */
const MAX_LINES = 2000;
/** 每服务日志内存上限（UTF-16 单元数，约 2MB）。防止单行超长输出（base64/JSON dump）导致内存爆炸 */
const MAX_BYTES = 2 * 1024 * 1024;

/** 按行数上限 + 字节上限裁剪日志（大行从头部删除，保留最新） */
function trimLines(lines: ServiceLogLine[], maxLines: number = MAX_LINES): ServiceLogLine[] {
  let trimmed = lines;
  if (trimmed.length > maxLines) trimmed = trimmed.slice(-maxLines);
  let total = 0;
  for (const l of trimmed) total += l.text.length;
  if (total <= MAX_BYTES) return trimmed;
  let drop = 0;
  let removed = 0;
  while (drop < trimmed.length && total - removed > MAX_BYTES) {
    removed += trimmed[drop].text.length;
    drop++;
  }
  return trimmed.slice(drop);
}

export const useLogStore = create<LogStore>((set) => ({
  logs: {},
  pausedLogs: {},
  version: {},
  totalAdded: {},

  appendLog: (serviceKey, stream, data) => {
    const now = new Date().toISOString();
    const line: ServiceLogLine = { timestamp: now, stream, text: data };
    set((state) => {
      const existing = state.logs[serviceKey] ?? [];
      const updated = trimLines(existing.concat([line]));
      // 暂停视图：未满 2000 行继续接收；满后冻结（\r 刷新帧仍替换最后一行）
      let pausedUpdated = state.pausedLogs[serviceKey];
      if (pausedUpdated !== undefined && pausedUpdated.length < MAX_LINES) {
        pausedUpdated = pausedUpdated.concat([line]);
        pausedUpdated = trimLines(pausedUpdated);
      }
      return {
        logs: { ...state.logs, [serviceKey]: updated },
        pausedLogs: pausedUpdated !== undefined ? { ...state.pausedLogs, [serviceKey]: pausedUpdated } : state.pausedLogs,
        version: { ...state.version, [serviceKey]: (state.version[serviceKey] ?? 0) + 1 },
        totalAdded: { ...state.totalAdded, [serviceKey]: (state.totalAdded[serviceKey] ?? 0) + 1 },
      };
    });
  },

  bulkAppend: (items) => {
    if (items.length === 0) return;
    set((state) => {
      const logs = { ...state.logs };
      const pausedLogs = { ...state.pausedLogs };
      const version = { ...state.version };
      const totalAdded = { ...state.totalAdded };
      // 按服务分组后一次拷贝 + 一次裁剪，避免每行 O(n) 复制（O(n²)）
      const grouped = new Map<string, ServiceLogLine[]>();
      for (const { serviceKey, stream, data, timestamp } of items) {
        let arr = grouped.get(serviceKey);
        if (!arr) { arr = []; grouped.set(serviceKey, arr); }
        // 使用后端打点的行产生时间（旧事件缺字段时回退到接收时间）
        arr.push({ timestamp: timestamp ?? new Date().toISOString(), stream, text: data });
      }
      for (const [serviceKey, newLines] of grouped) {
        // ── 跟随数据源：始终最新 2000 行（滑动窗口）──
        const existing = logs[serviceKey];
        const merged = existing ? [...existing] : [];
        let added = 0;
        for (const l of newLines) {
          // \r 开头 = 单行刷新（webpack 进度条等）：替换最后一条而非追加，避免每帧一行
          if (l.text.startsWith('\r') && merged.length > 0) {
            merged[merged.length - 1] = { ...merged[merged.length - 1], text: l.text.slice(1), timestamp: l.timestamp };
          } else {
            merged.push(l);
            added++;
          }
        }
        logs[serviceKey] = trimLines(merged);

        // ── 暂停视图：未满 2000 行继续接收；满后冻结（新日志不再进入）；
        //    \r 刷新帧仍替换最后一行（不增加行数，进度条在暂停视图里也更新）──
        const paused = pausedLogs[serviceKey];
        if (paused !== undefined) {
          const pMerged = [...paused];
          let pChanged = false;
          for (const l of newLines) {
            if (l.text.startsWith('\r') && pMerged.length > 0) {
              pMerged[pMerged.length - 1] = { ...pMerged[pMerged.length - 1], text: l.text.slice(1), timestamp: l.timestamp };
              pChanged = true;
            } else if (pMerged.length < MAX_LINES) {
              pMerged.push(l);
              pChanged = true;
            }
            // 满 2000 后普通行冻结（不接收）
          }
          if (pChanged) pausedLogs[serviceKey] = trimLines(pMerged);
        }

        version[serviceKey] = (version[serviceKey] ?? 0) + 1;
        // 累计新增行数（\r 刷新帧不计入）：暂停时显示"新增 N 行"的依据
        totalAdded[serviceKey] = (totalAdded[serviceKey] ?? 0) + added;
      }
      return { logs, pausedLogs, version, totalAdded };
    });
  },

  setLogs: (serviceKey, lines) => {
    set((state) => ({
      // 只更新跟随数据源（不影响暂停视图）
      logs: { ...state.logs, [serviceKey]: lines },
      version: { ...state.version, [serviceKey]: (state.version[serviceKey] ?? 0) + 1 },
      // 快照是初始状态，不是"新"日志——重置累计器
      totalAdded: { ...state.totalAdded, [serviceKey]: 0 },
    }));
  },

  clearLogs: (serviceKey) => {
    set((state) => {
      const { [serviceKey]: _, ...rest } = state.logs;
      const { [serviceKey]: _p, ...restPaused } = state.pausedLogs;
      const { [serviceKey]: __, ...restVer } = state.version;
      const { [serviceKey]: ___, ...restAdded } = state.totalAdded;
      return { logs: rest, pausedLogs: restPaused, version: restVer, totalAdded: restAdded };
    });
  },

  /** 清理不在 activeKeys 中的所有日志（服务停止后调用） */
  pruneInactive: (activeKeys: Set<string>) => {
    set((state) => {
      const logs: Record<string, ServiceLogLine[]> = {};
      const pausedLogs: Record<string, ServiceLogLine[]> = {};
      const version: Record<string, number> = {};
      const totalAdded: Record<string, number> = {};
      for (const key of activeKeys) {
        if (state.logs[key]) logs[key] = state.logs[key];
        if (state.pausedLogs[key]) pausedLogs[key] = state.pausedLogs[key];
        if (state.version[key] !== undefined) version[key] = state.version[key];
        if (state.totalAdded[key] !== undefined) totalAdded[key] = state.totalAdded[key];
      }
      return { logs, pausedLogs, version, totalAdded };
    });
  },

  pauseLogs: (serviceKey) => set((state) => ({
    // 快照跟随数据源作为暂停视图
    pausedLogs: { ...state.pausedLogs, [serviceKey]: state.logs[serviceKey] ? [...state.logs[serviceKey]] : [] },
  })),

  resumeLogs: (serviceKey) => set((state) => {
    const { [serviceKey]: _, ...rest } = state.pausedLogs;
    return { pausedLogs: rest };
  }),
}));
