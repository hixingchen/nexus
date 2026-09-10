import { useEffect, useRef, useState } from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';
import { readHexPage, readJarEntryBytes } from '../../services/editor';
import { parseJarVirtualPath } from '../../stores/editor';

const BYTES_PER_ROW = 16;
/** 每页行数（256 行 = 4096 字节，与后端 IPC 单次传输上限匹配） */
const ROWS_PER_PAGE = 256;
const PAGE_BYTES = BYTES_PER_ROW * ROWS_PER_PAGE;
/** 可视区外保留的页数（前后各 8 页，防滚动抖动时反复读盘） */
const KEEP_PAGE_MARGIN = 8;
const ROW_HEIGHT = 20;

/** 字节 → 可见 ASCII 字符（不可见用 ·） */
const toAscii = (b: number) => (b >= 32 && b < 127 ? String.fromCharCode(b) : '·');
const toHex = (b: number) => b.toString(16).padStart(2, '0');

/** hex 视图：虚拟滚动 + 按页加载，大文件不整体进内存 */
export function HexViewer({ path }: { path: string }) {
  const parentRef = useRef<HTMLDivElement>(null);
  const [totalSize, setTotalSize] = useState<number | null>(null);
  const [pages, setPages] = useState<Map<number, number[]>>(new Map());
  /** jar 内条目内存模式：整块字节数组（无后端分页）。
   *  用 Uint8Array 而不是 `Array.from` 得到的 number[]：装箱数组约 8 字节/元素，
   *  一个 50MB 条目会额外吃掉 400MB+；渲染时只按行（16 字节）转换。 */
  const [memoryBytes, setMemoryBytes] = useState<Uint8Array | null>(null);
  const [err, setErr] = useState<string | null>(null);
  /** 在途页请求去重 */
  const inflight = useRef<Set<number>>(new Set());
  /** 当前路径的实时值：在途分页请求返回时据此判断是否已切走 */
  const pathRef = useRef(path);
  pathRef.current = path;

  // 首屏加载：jar:// 条目一次性读入内存；磁盘文件分页加载页 0 并获得大小
  useEffect(() => {
    let alive = true;
    setTotalSize(null);
    setPages(new Map());
    setMemoryBytes(null);
    setErr(null);
    // 清在途标记：否则新文件的第 0 页会被旧文件的在途请求"占位"而永远不加载。
    // 旧请求的响应由上面的 reqPath 判断丢弃，这里只负责解锁去重集合。
    inflight.current.clear();
    if (path.startsWith('jar://')) {
      const { jarPath, nested, name } = parseJarVirtualPath(path);
      readJarEntryBytes(jarPath, nested, name)
        .then(({ bytes, size }) => {
          if (!alive) return;
          setTotalSize(size);
          // 直接持有 Uint8Array（readJarEntryBytes 已返回）；不转 number[]
          setMemoryBytes(bytes);
        })
        .catch(e => {
          if (alive) setErr(String(e));
        });
      return () => { alive = false; };
    }
    inflight.current.add(0);
    readHexPage(path, 0, ROWS_PER_PAGE)
      .then(res => {
        if (!alive) return;
        setTotalSize(res.totalSize);
        setPages(prev => new Map(prev).set(0, res.bytes));
      })
      .catch(e => {
        if (alive) setErr(String(e));
      })
      .finally(() => {
        inflight.current.delete(0);
      });
    return () => { alive = false; };
  }, [path]);

  // Number.isFinite 兜底：totalSize 异常（如旧版本后端）时显示空列表而不是整页崩溃
  const totalRows = totalSize === null || !Number.isFinite(totalSize) ? 0 : Math.ceil(totalSize / BYTES_PER_ROW);
  const virtualizer = useVirtualizer({
    count: totalRows,
    getScrollElement: () => parentRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 20,
  });
  const items = virtualizer.getVirtualItems();

  // 可见行 → 所需页：缺失且不在途的才请求（内存模式无分页，跳过）
  useEffect(() => {
    if (totalSize === null || items.length === 0 || memoryBytes || path.startsWith('jar://')) return;
    // 捕获本次 effect 对应的路径：在途请求返回时若 path 已变（用户切了文件），
    // 结果必须丢弃——否则上一个文件的字节会被写进新文件的页表，
    // 显示出来就是"另一个文件的内容"（只读查看器，无数据损失但内容是错的）。
    const reqPath = path;
    const minPage = Math.floor((items[0].index * BYTES_PER_ROW) / PAGE_BYTES);
    const maxPage = Math.floor(
      ((items[items.length - 1].index + 1) * BYTES_PER_ROW - 1) / PAGE_BYTES
    );
    for (let p = minPage; p <= maxPage; p++) {
      if (pages.has(p) || inflight.current.has(p)) continue;
      inflight.current.add(p);
      readHexPage(path, p * PAGE_BYTES, ROWS_PER_PAGE)
        .then(res => {
          if (reqPath !== pathRef.current) return; // 已切走：丢弃过期响应
          setPages(prev => new Map(prev).set(p, res.bytes));
        })
        .catch((e) => console.error('读取 hex 分页失败:', reqPath, p, e)) // 仅控制台留痕：该区域显示占位，滚动重试
        .finally(() => {
          inflight.current.delete(p);
        });
    }
    // 页级 LRU：保留可视区前后若干页，淘汰远处页——大文件从头顶滚到底
    // 不再让全部页常驻内存（此前 Map 只增不减）
    setPages(prev => {
      if (prev.size <= KEEP_PAGE_MARGIN * 2 + (maxPage - minPage + 1)) return prev;
      const keepMin = minPage - KEEP_PAGE_MARGIN;
      const keepMax = maxPage + KEEP_PAGE_MARGIN;
      const next = new Map(prev);
      for (const k of next.keys()) {
        if (k < keepMin || k > keepMax) next.delete(k);
      }
      return next;
    });
  }, [items, pages, totalSize, path]);

  const renderRow = (index: number) => {
    const byteOffset = index * BYTES_PER_ROW;
    let bytes: number[] | null;
    if (memoryBytes) {
      // 内存模式：直接从整块数组切片；每行只转 16 个元素（整块转装箱数组代价过高）
      const slice = memoryBytes.slice(byteOffset, byteOffset + BYTES_PER_ROW);
      bytes = slice.length > 0 ? Array.from(slice) : null;
    } else {
      const page = Math.floor(byteOffset / PAGE_BYTES);
      const data = pages.get(page);
      const inPage = (byteOffset % PAGE_BYTES) / BYTES_PER_ROW;
      bytes = data ? data.slice(inPage * BYTES_PER_ROW, (inPage + 1) * BYTES_PER_ROW) : null;
    }

    return (
      <div
        key={index}
        className="absolute left-0 top-0 w-full flex items-center gap-3 px-3 font-mono text-[12px] leading-none"
        style={{ height: ROW_HEIGHT, transform: `translateY(${index * ROW_HEIGHT}px)` }}
      >
        <span className="text-nexus-muted select-none">{byteOffset.toString(16).padStart(8, '0')}</span>
        {bytes ? (
          <>
            <span className="text-nexus-text whitespace-pre">
              {bytes.map((b, i) => (
                <span key={i} className={i === 7 ? 'mr-3' : ''}>{toHex(b)} </span>
              ))}
              {' '.repeat((BYTES_PER_ROW - bytes.length) * 3)}
            </span>
            <span className="text-nexus-text-muted select-none">
              {bytes.map(toAscii).join('')}
            </span>
          </>
        ) : (
          <span className="text-nexus-muted">…</span>
        )}
      </div>
    );
  };

  return (
    <div className="h-full flex flex-col bg-nexus-editor">
      {/* 表头：偏移 + 16 列字节 + ASCII */}
      <div className="flex items-center gap-3 px-3 h-[26px] border-b border-nexus-border/40 font-mono text-[11px] text-nexus-muted select-none flex-shrink-0">
        <span className="w-[70px]">offset</span>
        <span className="whitespace-pre">
          {Array.from({ length: 16 }, (_, i) => `${toHex(i)} `).join('')}
        </span>
        <span>文本</span>
      </div>
      {err ? (
        <div className="flex-1 flex items-center justify-center text-[12px] text-nexus-error">
          {err}
        </div>
      ) : (
        <div ref={parentRef} className="flex-1 overflow-auto">
          <div style={{ height: virtualizer.getTotalSize(), position: 'relative' }}>
            {items.map(vi => renderRow(vi.index))}
          </div>
        </div>
      )}
    </div>
  );
}
