// ── 源码 / 预览两种模式的滚动位置共享记忆 ──────────────────────────
//
// 为什么需要：两种模式此前各记各的——源码存 CodeMirror 的文档位置快照，预览存滚动比例，
// 切换模式时互不知情。于是"源码停在第 100 行 → 切到预览"会落到预览自己上次的位置
// （没看过就是第一行），反过来也一样。
//
// 这里只共享**滚动比例**：预览拿不到源码行号（marked 的 token 不带源码位置），源码行号
// 也无法精确映射到渲染后各块的实际高度——比例是两种模式都能理解的唯一口径，属于
// "大致停在文档同一处"的近似。**源码 ↔ 源码仍由 CodeMirror 快照精确还原**，不走这里。
//
// `writer` 记录最后一次是谁写的：源码挂载时若最后是预览在滚动，就按比例落位；否则
// 用自己那份精确快照。

/** 写入者：源码编辑器 / Markdown 预览 */
export type ScrollMode = 'source' | 'preview';

export interface ScrollMemory {
  /** 0~1：scrollTop / (scrollHeight - clientHeight) */
  ratio: number;
  /** 最后一次写入来自哪种模式 */
  writer: ScrollMode;
}

/** 每个文件的最近滚动位置（按插入序淘汰最旧的，防长期使用后无限增长） */
const byPath = new Map<string, ScrollMemory>();
/** 上限与 CodeViewer 的 stateCache 同一量级 */
const MAX_ENTRIES = 64;

/** 记录一次滚动（两种模式共用；同一文件以最后一次写入为准） */
export function rememberScroll(path: string, writer: ScrollMode, ratio: number): void {
  if (!Number.isFinite(ratio)) return;
  // 浏览器回弹（滚动越界）可能给出略小于 0 / 略大于 1 的值，夹回区间
  const clamped = Math.min(1, Math.max(0, ratio));
  // 先删再设：让它排到末尾，下面按插入序淘汰最旧的
  byPath.delete(path);
  byPath.set(path, { ratio: clamped, writer });
  while (byPath.size > MAX_ENTRIES) {
    const oldest = byPath.keys().next().value;
    if (oldest === undefined) break;
    byPath.delete(oldest);
  }
}

/** 读取某文件最近一次的滚动记忆（无记录 = 本次会话还没在任何模式里看过它） */
export function recallScroll(path: string): ScrollMemory | undefined {
  const memory = byPath.get(path);
  if (memory === undefined) return undefined;
  // 命中后挪到末尾：淘汰的是最久没被读写的，而不是最久没被写的
  byPath.delete(path);
  byPath.set(path, memory);
  return memory;
}
