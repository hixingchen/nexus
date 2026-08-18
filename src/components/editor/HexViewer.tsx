import { useEffect, useRef, useState } from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';
import { readHexPage, readJarEntryBytes } from '../../services/editor';
import { parseJarVirtualPath } from '../../stores/editor';

const BYTES_PER_ROW = 16;
/** 每页行数（256 行 = 4096 字节，与后端 IPC 单次传输上限匹配） */
const ROWS_PER_PAGE = 256;
const PAGE_BYTES = BYTES_PER_ROW * ROWS_PER_PAGE;
const ROW_HEIGHT = 20;

/** 字节 → 可见 ASCII 字符（不可见用 ·） */
const toAscii = (b: number) => (b >= 32 && b < 127 ? String.fromCharCode(b) : '·');
const toHex = (b: number) => b.toString(16).padStart(2, '0');

/** hex 视图：虚拟滚动 + 按页加载，大文件不整体进内存 */
export function HexViewer({ path }: { path: string }) {
  const parentRef = useRef<HTMLDivElement>(null);
  const [totalSize, setTotalSize] = useState<number | null>(null);
  const [pages, setPages] = useState<Map<number, number[]>>(new Map());
  /** jar 内条目内存模式：整块字节数组（无后端分页） */
  const [memoryBytes, setMemoryBytes] = useState<number[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  /** 在途页请求去重 */
  const inflight = useRef<Set<number>>(new Set());

  // 首屏加载：jar:// 条目一次性读入内存；磁盘文件分页加载页 0 并获得大小
  useEffect(() => {
    let alive = true;
    setTotalSize(null);
    setPages(new Map());
    setMemoryBytes(null);
    setErr(null);
    if (path.startsWith('jar://')) {
      const { jarPath, nested, name } = parseJarVirtualPath(path);
      readJarEntryBytes(jarPath, nested, name)
        .then(({ bytes, size }) => {
          if (!alive) return;
          setTotalSize(size);
          setMemoryBytes(Array.from(bytes));
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
    const minPage = Math.floor((items[0].index * BYTES_PER_ROW) / PAGE_BYTES);
    const maxPage = Math.floor(
      ((items[items.length - 1].index + 1) * BYTES_PER_ROW - 1) / PAGE_BYTES
    );
    for (let p = minPage; p <= maxPage; p++) {
      if (pages.has(p) || inflight.current.has(p)) continue;
      inflight.current.add(p);
      readHexPage(path, p * PAGE_BYTES, ROWS_PER_PAGE)
        .then(res => setPages(prev => new Map(prev).set(p, res.bytes)))
        .catch(() => {}) // 单页失败静默：该区域显示占位，滚动重试
        .finally(() => {
          inflight.current.delete(p);
        });
    }
  }, [items, pages, totalSize, path]);

  const renderRow = (index: number) => {
    const byteOffset = index * BYTES_PER_ROW;
    let bytes: number[] | null;
    if (memoryBytes) {
      // 内存模式：直接从整块数组切片
      const slice = memoryBytes.slice(byteOffset, byteOffset + BYTES_PER_ROW);
      bytes = slice.length > 0 ? slice : null;
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
