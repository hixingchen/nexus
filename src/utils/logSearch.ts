import type { ServiceLogLine } from '../services/logService';
import { includesIgnoreCase } from './search';

/**
 * 日志搜索的增量匹配缓存。
 *
 * 日志面板每 50ms 收到一批新行，`lines` 每次都是新数组；若每次都 `filter` 全部 2000 行，
 * 搜索词不变时也会把每行的 `includesIgnoreCase` 重跑一遍。本地日志的行对象跨批次稳定，
 * 所以用 `WeakMap<行对象, 是否命中>`：只测新行，旧行直接查缓存；行对象被滑动窗口淘汰后
 * 由 GC 自然回收（不需要手动清理，也不会随历史增长）。
 *
 * boot 日志的快照会重建所有行对象，此时缓存自然失效（等价于全量重算），不会把上一条
 * 快照的命中结果错配到新行上。
 */
export interface LogSearchCache {
  term: string;
  hits: WeakMap<ServiceLogLine, boolean>;
}

export function createLogSearchCache(): LogSearchCache {
  return { term: '', hits: new WeakMap() };
}

export function filterLogLinesIncremental(
  lines: ServiceLogLine[],
  term: string,
  cache: LogSearchCache,
  matches: (text: string, term: string) => boolean = includesIgnoreCase,
): ServiceLogLine[] {
  if (cache.term !== term) {
    cache.term = term;
    cache.hits = new WeakMap();
  }
  const rows: ServiceLogLine[] = [];
  for (const line of lines) {
    let hit = cache.hits.get(line);
    if (hit === undefined) {
      hit = matches(line.text, term);
      cache.hits.set(line, hit);
    }
    if (hit) rows.push(line);
  }
  return rows;
}
