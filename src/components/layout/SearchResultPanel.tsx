import { useState, useEffect, useRef, useMemo } from 'react';
import { searchFiles, type SearchResultItem } from '../../services/editor';
import { locateFile, useEditorStore } from '../../stores/editor';
import { useSearchModalStore } from '../../stores/searchModal';

/** 命中片段中的高亮区间 */
function renderSnippet(snippet: string, query: string) {
  if (!query) return snippet;
  const lower = snippet.toLowerCase();
  const qLower = query.toLowerCase();
  const parts: { text: string; hit: boolean }[] = [];
  let rest = snippet;
  let idx = 0;
  while (true) {
    const pos = lower.indexOf(qLower, idx);
    if (pos === -1) { parts.push({ text: rest, hit: false }); break; }
    parts.push({ text: rest.slice(0, pos), hit: false });
    parts.push({ text: rest.slice(pos, pos + qLower.length), hit: true });
    rest = rest.slice(pos + qLower.length);
    idx = pos + qLower.length;
    if (parts.length > 40) { parts.push({ text: rest, hit: false }); break; } // 防御：极端重复命中
  }
  return parts.map((p, i) => p.hit
    ? <span key={i} className="bg-nexus-accent/25 text-nexus-accent rounded-sm px-0.5">{p.text}</span>
    : <span key={i}>{p.text}</span>);
}

/** 搜索面板默认高度（可拖拽调整，挤压编辑器区域） */
const DEFAULT_PANEL_HEIGHT = 220;
/** 面板高度拖拽范围 */
const PANEL_MIN_HEIGHT = 100;
const PANEL_MAX_HEIGHT = () => window.innerHeight - 200;

/** 底部搜索结果面板：显示在文本区（编辑器）下方，右键服务/目录树节点触发 */
export function SearchResultPanel() {
  const { open, root, title, closeSearch } = useSearchModalStore();
  const [query, setQuery] = useState('');
  const [caseSensitive, setCaseSensitive] = useState(false);
  const [extensions, setExtensions] = useState('');
  const [status, setStatus] = useState<'idle' | 'searching' | 'done'>('idle');
  const [results, setResults] = useState<SearchResultItem[]>([]);
  const [truncated, setTruncated] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** 折叠的文件路径集合（默认全部展开，点击文件头折叠） */
  const [collapsedPaths, setCollapsedPaths] = useState<Set<string>>(new Set());
  const seqRef = useRef(0);
  const inputRef = useRef<HTMLInputElement>(null);

  // ── 高度拖拽（面板顶部分隔条） ──
  const [panelHeight, setPanelHeight] = useState(DEFAULT_PANEL_HEIGHT);
  const [dragging, setDragging] = useState(false);
  const dragStateRef = useRef<{ startY: number; startHeight: number } | null>(null);

  const handleDragStart = (e: React.MouseEvent) => {
    e.preventDefault();
    dragStateRef.current = { startY: e.clientY, startHeight: panelHeight };
    setDragging(true);
  };

  useEffect(() => {
    if (!dragging) return;
    let rafId: number | null = null;
    let pendingEvent: MouseEvent | null = null;

    const flush = () => {
      if (!pendingEvent || !dragStateRef.current) { rafId = null; return; }
      const { startY, startHeight } = dragStateRef.current;
      setPanelHeight(Math.max(PANEL_MIN_HEIGHT, Math.min(PANEL_MAX_HEIGHT(), startHeight + (startY - pendingEvent.clientY))));
      pendingEvent = null;
      rafId = null;
    };

    const handleMove = (e: MouseEvent) => {
      pendingEvent = e;
      if (rafId === null) rafId = requestAnimationFrame(flush);
    };
    const handleUp = () => {
      if (rafId !== null) cancelAnimationFrame(rafId);
      dragStateRef.current = null;
      setDragging(false);
    };

    document.addEventListener('mousemove', handleMove);
    document.addEventListener('mouseup', handleUp);
    return () => {
      if (rafId !== null) cancelAnimationFrame(rafId);
      document.removeEventListener('mousemove', handleMove);
      document.removeEventListener('mouseup', handleUp);
    };
  }, [dragging]);

  // 每次打开：重置状态并聚焦输入框（作废旧请求，防止上次响应污染）
  useEffect(() => {
    if (!open) return;
    seqRef.current++;
    setQuery('');
    setCaseSensitive(false);
    setExtensions('');
    setStatus('idle');
    setResults([]);
    setTruncated(false);
    setError(null);
    setCollapsedPaths(new Set());
    setTimeout(() => inputRef.current?.focus(), 50);
  }, [open, root]);

  // 关闭面板同时清除编辑器中的命中高亮
  const handleClose = () => {
    useEditorStore.getState().clearHits();
    closeSearch();
  };

  // 手动触发搜索（回车或搜索按钮），不做实时搜索
  const triggerSearch = () => {
    const q = query.trim();
    if (!q) return;
    const seq = ++seqRef.current;
    setStatus('searching');
    searchFiles({
      root,
      query: q,
      caseSensitive,
      extensions: extensions.split(',').map(s => s.trim()).filter(Boolean),
      maxResults: 1000,
    }).then(res => {
      if (seq !== seqRef.current) return;
      setResults(res.results);
      setTruncated(res.truncated);
      setStatus('done');
      setError(null);
    }).catch((e: unknown) => {
      if (seq !== seqRef.current) return;
      setResults([]);
      setTruncated(false);
      setStatus('done');
      setError(String(e));
    });
  };

  // 按文件分组（同一文件的多行命中合并，展开显示）
  const groups = useMemo(() => {
    const map = new Map<string, { path: string; name: string; hits: { line: number; snippet: string }[] }>();
    for (const item of results) {
      let g = map.get(item.path);
      if (!g) { g = { path: item.path, name: item.name, hits: [] }; map.set(item.path, g); }
      g.hits.push({ line: item.line, snippet: item.snippet });
    }
    return [...map.values()];
  }, [results]);

  // 相对搜索 root 的路径（完整绝对路径放 title 悬停查看）
  const relPath = (p: string) => p.startsWith(root + '/') ? p.slice(root.length + 1) : p;

  if (!open) return null;

  return (
    <div className="flex-shrink-0 flex flex-col bg-nexus-surface" style={{ height: panelHeight }}>
      {/* 拖拽条：上下调整面板高度 */}
      <div
        className={`flex-shrink-0 cursor-row-resize transition-colors ${dragging ? 'bg-nexus-accent' : 'bg-nexus-border hover:bg-nexus-accent'}`}
        style={{ height: 3 }}
        onMouseDown={handleDragStart}
        title="拖拽调整高度"
      />
      {/* 头部：范围 + 输入 + 搜索 + 选项 + 关闭 */}
      <div className="flex items-center gap-2 px-3 h-[38px] border-b border-nexus-border flex-shrink-0">
        <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.4" className="text-nexus-muted flex-shrink-0">
          <circle cx="5" cy="5" r="3.5"/><line x1="7.8" y1="7.8" x2="10.5" y2="10.5"/>
        </svg>
        <span className="text-[12px] text-nexus-text truncate max-w-[160px] flex-shrink-0" title={`${title} · ${root}`}>
          搜索「{title}」
        </span>
        <input
          ref={inputRef}
          className="flex-1 min-w-0 px-2.5 py-1.5 text-[12px] bg-nexus-bg border border-nexus-border rounded-md text-nexus-text font-mono placeholder:text-nexus-muted/50 focus:outline-none focus:border-nexus-accent transition-colors"
          placeholder="输入内容，回车搜索"
          value={query}
          onChange={e => setQuery(e.target.value)}
          onKeyDown={e => { if (e.key === 'Enter') triggerSearch(); }}
        />
        <button
          className="flex-shrink-0 px-3 py-1.5 text-[12px] bg-nexus-accent text-white rounded-md hover:bg-nexus-accent-hover disabled:opacity-40 disabled:cursor-not-allowed font-medium transition-colors"
          disabled={!query.trim() || status === 'searching'}
          onClick={triggerSearch}
        >{status === 'searching' ? '搜索中…' : '搜索'}</button>
        <button
          className={`flex-shrink-0 px-2.5 py-1.5 text-[11px] font-medium rounded-md border transition-colors ${
            caseSensitive
              ? 'text-nexus-accent bg-nexus-accent/10 border-nexus-accent/40'
              : 'text-nexus-muted border-nexus-border hover:text-nexus-text hover:border-nexus-muted'
          }`}
          onClick={() => setCaseSensitive(v => !v)}
          title={caseSensitive ? '区分大小写（已开启）' : '区分大小写（未开启）'}
        >Aa</button>
        <input
          className="w-[110px] flex-shrink-0 px-2 py-1 text-[11px] bg-nexus-bg border border-nexus-border rounded-md text-nexus-text font-mono placeholder:text-nexus-muted/50 focus:outline-none focus:border-nexus-accent transition-colors"
          placeholder="扩展名,如ts,vue"
          title="扩展名筛选，逗号分隔，留空全部"
          value={extensions}
          onChange={e => setExtensions(e.target.value)}
        />
        <button
          className="flex-shrink-0 px-3 py-1.5 text-[12px] text-nexus-muted border border-nexus-border rounded-md hover:text-nexus-text hover:border-nexus-muted transition-colors"
          onClick={handleClose}
        >关闭</button>
      </div>

      {/* 结果列表 */}
      <div className="flex-1 overflow-auto">
        {status === 'idle' && (
          <div className="flex items-center justify-center h-full text-[12px] text-nexus-muted">
            输入内容后回车或点击「搜索」
          </div>
        )}
        {status === 'searching' && (
          <div className="flex items-center justify-center h-full text-[12px] text-nexus-muted">搜索中…</div>
        )}
        {status === 'done' && error && (
          <div className="flex items-center justify-center h-full px-4 text-center text-[12px] text-nexus-error">{error}</div>
        )}
        {status === 'done' && !error && results.length === 0 && (
          <div className="flex flex-col items-center justify-center h-full px-4 text-center">
            <span className="text-[12px] text-nexus-muted mb-1">未找到匹配内容</span>
            {truncated && <span className="text-[11px] text-nexus-muted/60">（扫描文件过多已中止）</span>}
          </div>
        )}
        {groups.map(g => {
          const collapsed = collapsedPaths.has(g.path);
          return (
            <div key={g.path}>
              {/* 文件头：点击展开/折叠 */}
              <div
                className="flex items-center gap-1.5 px-3 py-1 cursor-pointer hover:bg-nexus-hover/50 transition-colors"
                onClick={() => {
                  setCollapsedPaths(prev => {
                    const next = new Set(prev);
                    if (next.has(g.path)) next.delete(g.path); else next.add(g.path);
                    return next;
                  });
                }}
                title={`${g.path}（${g.hits.length} 处命中）`}
              >
                <svg
                  className={`flex-shrink-0 text-nexus-muted transition-transform ${collapsed ? '' : 'rotate-90'}`}
                  width="10" height="10" viewBox="0 0 10 10" fill="none"
                  stroke="currentColor" strokeWidth="1.3" strokeLinecap="round"
                >
                  <polyline points="3,1 7,5 3,9" />
                </svg>
                <span className="flex-1 min-w-0 text-[12px] text-nexus-text font-medium truncate">{relPath(g.path)}</span>
                <span className="flex-shrink-0 text-[10px] text-nexus-muted bg-nexus-hover/60 rounded px-1 py-0.5">{g.hits.length}</span>
              </div>
              {/* 命中行（展开时） */}
              {!collapsed && g.hits.map((hit, i) => (
                <div
                  key={`${g.path}-${hit.line}-${i}`}
                  className="flex items-start gap-2 pl-7 pr-3 py-0.5 cursor-pointer hover:bg-nexus-hover/50 transition-colors"
                  onClick={() => locateFile(g.path, g.name, hit.line, query.trim())}
                  title={`${g.path}:${hit.line}（点击定位到该行）`}
                >
                  <span className="flex-shrink-0 text-[11px] text-nexus-muted font-mono leading-relaxed">:{hit.line}</span>
                  <div className="flex-1 min-w-0 text-[11px] text-nexus-text-muted font-mono truncate leading-relaxed">
                    {renderSnippet(hit.snippet, query.trim())}
                  </div>
                </div>
              ))}
            </div>
          );
        })}
      </div>

      {/* 底部状态 */}
      {status === 'done' && !error && results.length > 0 && (
        <div className="px-3 py-1 border-t border-nexus-border flex-shrink-0 text-[11px] text-nexus-muted">
          {results.length} 个结果{truncated ? '（已截断，仅显示前 1000 条，可加扩展名筛选缩小范围）' : ''} · 点击结果定位到对应行
        </div>
      )}
    </div>
  );
}
