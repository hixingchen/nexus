import { useEffect, useMemo, useRef, useState } from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';
import { listJar, type JarEntryInfo } from '../../services/editor';
import { openJarEntry } from '../../stores/editor';

const ROW_HEIGHT = 24;

const formatSize = (n: number) => {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
};

/** jar 包浏览：条目列表（虚拟滚动 + 过滤），点击 .class/文本 → 虚拟标签，点击嵌套 .jar → 进入 */
export function JarViewer({ path }: { path: string }) {
  const parentRef = useRef<HTMLDivElement>(null);
  /** 嵌套 jar 条目链（Spring Boot BOOT-INF/lib/*.jar 一层层进入） */
  const [nested, setNested] = useState<string[]>([]);
  const [entries, setEntries] = useState<JarEntryInfo[]>([]);
  const [filter, setFilter] = useState('');
  const [err, setErr] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let alive = true;
    setLoading(true);
    setErr(null);
    listJar(path, nested)
      .then(list => {
        if (!alive) return;
        setEntries(list);
        setLoading(false);
      })
      .catch(e => {
        if (!alive) return;
        setErr(String(e));
        setLoading(false);
      });
    return () => { alive = false; };
  }, [path, nested]);

  const filtered = useMemo(() => {
    if (!filter) return entries;
    const q = filter.toLowerCase();
    return entries.filter(e => e.name.toLowerCase().includes(q));
  }, [entries, filter]);

  const virtualizer = useVirtualizer({
    count: filtered.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 20,
  });
  const items = virtualizer.getVirtualItems();

  const isNestedJar = (name: string) => name.toLowerCase().endsWith('.jar');

  return (
    <div className="h-full flex flex-col bg-nexus-editor">
      {/* 头部：返回（嵌套层时）+ jar 路径面包屑 */}
      <div className="flex items-center gap-2 px-3 h-[34px] border-b border-nexus-border/40 flex-shrink-0">
        {nested.length > 0 && (
          <button
            className="flex-shrink-0 px-1.5 py-0.5 text-[11px] text-nexus-text-muted border border-nexus-border rounded hover:text-nexus-text hover:bg-nexus-hover transition-colors"
            onClick={() => setNested(nested.slice(0, -1))}
            title="返回上一层"
          >
            ← 返回
          </button>
        )}
        <span className="text-[12px] text-nexus-text-muted truncate" title={path}>
          {path}
          {nested.map(n => `!/${n}`).join('')}
        </span>
        <span className="text-[11px] text-nexus-muted flex-shrink-0 ml-auto">{entries.length} 个条目</span>
      </div>

      {/* 过滤框（fat jar 数万条目，靠过滤定位） */}
      <div className="px-3 py-1.5 border-b border-nexus-border/40 flex-shrink-0">
        <input
          className="w-full px-2 py-1 text-[12px] bg-nexus-bg border border-nexus-border rounded text-nexus-text placeholder:text-nexus-muted focus:outline-none focus:border-nexus-accent/50"
          placeholder="过滤条目（如 ApiLog、BOOT-INF/lib）"
          value={filter}
          onChange={e => setFilter(e.target.value)}
        />
      </div>

      {err ? (
        <div className="flex-1 flex items-center justify-center text-[12px] text-nexus-error px-4 text-center">
          {err}
        </div>
      ) : loading ? (
        <div className="flex-1 flex items-center justify-center text-[12px] text-nexus-muted">解析 jar…</div>
      ) : (
        <div ref={parentRef} className="flex-1 overflow-auto">
          <div style={{ height: virtualizer.getTotalSize(), position: 'relative' }}>
            {items.map(vi => {
              const e = filtered[vi.index];
              return (
                <div
                  key={e.name}
                  className="absolute left-0 top-0 w-full flex items-center gap-2 px-3 text-[12px] cursor-pointer hover:bg-nexus-hover"
                  style={{ height: ROW_HEIGHT, transform: `translateY(${vi.index * ROW_HEIGHT}px)` }}
                  title={isNestedJar(e.name) ? `进入 ${e.name}` : e.name}
                  onClick={() => {
                    if (isNestedJar(e.name)) {
                      setNested([...nested, e.name]);
                    } else {
                      void openJarEntry(path, nested, e);
                    }
                  }}
                >
                  <span className="text-nexus-text truncate flex-1">{e.name}</span>
                  <span className="text-nexus-muted flex-shrink-0 select-none">{formatSize(e.size)}</span>
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}
