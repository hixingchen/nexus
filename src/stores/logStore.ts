import { create } from 'zustand';
import type { LogStream, ServiceLogLine } from '../services/logService';

interface LogStore {
  /** 跟随数据源：始终最新 2000 行（滑动窗口），后台持续维护 */
  logs: Record<string, ServiceLogLine[]>;
  /** 暂停视图：暂停时的快照（最多 2000 行，满后冻结——新日志不再进入；\r 刷新帧仍可替换最后一行） */
  pausedLogs: Record<string, ServiceLogLine[]>;
  /** 版本号，每次 bulkAppend 递增。LogViewer 用它检测新数据（解决 2000 行上限后行数不变的问题） */
  version: Record<string, number>;
  /** 累计新增行数（只增不减，\r 刷新帧不计）。暂停时显示"新增 N 行"的依据 */
  totalAdded: Record<string, number>;
  bulkAppend: (items: Array<{ serviceKey: string; stream: LogStream; data: string; timestamp?: string; seq?: number }>) => void;
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

/**
 * 每服务"已合并的最大行号"（模块级：只是去重窗口，不需要触发渲染）。
 *
 * 为什么需要：快照（get_service_logs）与实时事件是两条独立路径，快照可能覆盖
 * "刚追加、尚未合并"的实时行——跟随视图里那几行会永久消失。有了 seq 就能做幂等合并：
 * 实时事件只应用 seq 大于本值的行；快照应用时保留本地 seq 更大的尾部。
 */
const lastSeq = new Map<string, number>();

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

/**
 * 每服务已驻留字节数（UTF-16 单元）。
 *
 * 为什么单独维护：`bulkAppend` 每 50ms 对每个活跃服务跑一次，`trimLines` 里那次
 * "遍历全部行求 text.length" 是纯粹的重复劳动——行数/字节的增减我们都清楚，可以增量维护。
 * 只在首次（缓存缺失）与快照（setLogs）时全量重算一次。
 */
const byteTotals = new Map<string, number>();

/** 全量重算某服务的字节数（首次/快照用） */
function sumBytes(lines: ServiceLogLine[]): number {
  let total = 0;
  for (const l of lines) total += l.text.length;
  return total;
}

/**
 * 按字节上限裁剪（已知总字节时的版本）：从头部丢行直到不超上限，返回新数组与剩余字节数
 */
function trimByBytes(lines: ServiceLogLine[], total: number): { lines: ServiceLogLine[]; bytes: number } {
  if (total <= MAX_BYTES) return { lines, bytes: total };
  let drop = 0;
  let removed = 0;
  while (drop < lines.length && total - removed > MAX_BYTES) {
    removed += lines[drop].text.length;
    drop++;
  }
  return { lines: lines.slice(drop), bytes: total - removed };
}

export const useLogStore = create<LogStore>((set) => ({
  logs: {},
  pausedLogs: {},
  version: {},
  totalAdded: {},

  bulkAppend: (items) => {
    if (items.length === 0) return;
    set((state) => {
      const logs = { ...state.logs };
      const pausedLogs = { ...state.pausedLogs };
      const version = { ...state.version };
      const totalAdded = { ...state.totalAdded };
      // 按服务分组后一次拷贝 + 一次裁剪，避免每行 O(n) 复制（O(n²)）
      const grouped = new Map<string, ServiceLogLine[]>();
      for (const { serviceKey, stream, data, timestamp, seq } of items) {
        // 幂等合并：seq 不大于已合并最大值的行直接丢弃。
        // 命中场景 = 快照已包含该行（快照先到、事件后到），否则会重复显示同一行。
        const s = seq ?? 0;
        if (s > 0 && s <= (lastSeq.get(serviceKey) ?? 0)) continue;
        if (s > 0) lastSeq.set(serviceKey, s);
        let arr = grouped.get(serviceKey);
        if (!arr) { arr = []; grouped.set(serviceKey, arr); }
        // 使用后端打点的行产生时间（旧事件缺字段时回退到接收时间）
        arr.push({ seq: s, timestamp: timestamp ?? new Date().toISOString(), stream, text: data });
      }
      for (const [serviceKey, newLines] of grouped) {
        // ── 跟随数据源：始终最新 2000 行（滑动窗口）──
        const existing = logs[serviceKey];
        const merged = existing ? [...existing] : [];
        // 已驻留字节数：命中缓存则直接用，缺失（首次/清空后）才全量重算一次
        let bytes = byteTotals.get(serviceKey) ?? sumBytes(merged);
        let added = 0;
        for (const l of newLines) {
          // \r 开头 = 单行刷新（webpack 进度条等）：替换最后一条而非追加，避免每帧一行。
          // 必须同时校验 stream（与后端 push_log_line 同口径）：队尾若是 system 行（"已启动"/退出码）
          // 或 stderr 行，stdout 的进度帧不能把它覆盖掉——那会改写系统行的语义与颜色。
          const tail = merged[merged.length - 1];
          if (l.text.startsWith('\r') && tail && tail.stream === l.stream) {
            bytes += l.text.length - tail.text.length; // 原地替换：只结算差值
            merged[merged.length - 1] = { ...tail, seq: l.seq, text: l.text.slice(1), timestamp: l.timestamp };
          } else {
            merged.push(l);
            bytes += l.text.length;
            added++;
          }
        }
        // 行数上限：从头部丢弃并扣减字节（不重新遍历求和）
        if (merged.length > MAX_LINES) {
          const drop = merged.length - MAX_LINES;
          for (let i = 0; i < drop; i++) bytes -= merged[i].text.length;
          merged.splice(0, drop);
        }
        // 字节上限：同理按已知总量裁剪
        const trimmed = trimByBytes(merged, bytes);
        byteTotals.set(serviceKey, trimmed.bytes);
        logs[serviceKey] = trimmed.lines;

        // ── 暂停视图：未满 2000 行继续接收；满后冻结（新日志不再进入）；
        //    \r 刷新帧仍替换最后一行（不增加行数，进度条在暂停视图里也更新）──
        const paused = pausedLogs[serviceKey];
        if (paused !== undefined) {
          const pMerged = [...paused];
          let pChanged = false;
          for (const l of newLines) {
            const pTail = pMerged[pMerged.length - 1];
            if (l.text.startsWith('\r') && pTail && pTail.stream === l.stream) {
              pMerged[pMerged.length - 1] = { ...pTail, seq: l.seq, text: l.text.slice(1), timestamp: l.timestamp };
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
    // 快照 ≠ 覆盖：快照到达前已合并的实时行（seq 比快照里任何一行都大）必须保留，
    // 否则"打开日志面板瞬间已经刷出来的几行"会永久消失（原实现直接整表覆盖）。
    const snapMax = lines.reduce((m, l) => Math.max(m, l.seq ?? 0), 0);
    set((state) => {
      const local = state.logs[serviceKey] ?? [];
      const newer = local.filter(l => (l.seq ?? 0) > snapMax);
      // 只更新跟随数据源（不影响暂停视图）；同样走上限裁剪：后端快照也不能突破行数/字节上限。
      // 快照是低频路径（打开面板/服务停止），这里全量重算字节数并写回缓存即可
      const merged = trimLines([...lines, ...newer]);
      byteTotals.set(serviceKey, sumBytes(merged));
      return {
        logs: { ...state.logs, [serviceKey]: merged },
        version: { ...state.version, [serviceKey]: (state.version[serviceKey] ?? 0) + 1 },
        // 快照是初始状态，不是"新"日志——重置累计器
        totalAdded: { ...state.totalAdded, [serviceKey]: 0 },
      };
    });
    // 去重窗口取两者较大值：快照里的行不应再被后续事件重复插入
    lastSeq.set(serviceKey, Math.max(lastSeq.get(serviceKey) ?? 0, snapMax));
  },

  clearLogs: (serviceKey) => {
    lastSeq.delete(serviceKey); // 清空后重新计数：新会话的行号从 1 重新开始
    byteTotals.delete(serviceKey);
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
      // 同步丢弃已不再跟踪的服务的去重窗口与字节计数（否则 Map 随服务数无限增长）
      for (const key of [...lastSeq.keys()]) {
        if (!activeKeys.has(key)) lastSeq.delete(key);
      }
      for (const key of [...byteTotals.keys()]) {
        if (!activeKeys.has(key)) byteTotals.delete(key);
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
