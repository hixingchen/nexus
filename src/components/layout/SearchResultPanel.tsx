import { useState, useEffect, useRef, useMemo, useCallback, memo } from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';
import { searchFiles, type SearchResultItem } from '../../services/editor';
import { locateFile, useEditorStore } from '../../stores/editor';
import { useSearchModalStore } from '../../stores/searchModal';
import { isSubmitEnter } from '../../utils/keyboard';

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

/**
 * 片段渲染缓存：同一 (查询词, 片段) 只切分一次。
 *
 * 折叠/展开文件头、面板高度拖拽都会让列表重渲染，而 `renderSnippet` 每次都要做
 * 大小写折叠 + `indexOf` 循环 + 构造 1~41 个 span；结果本身是不变值，缓存掉即可。
 */
const snippetCache = new Map<string, React.ReactNode>();
/** 缓存条目上限（超出整体清空：搜索词换了之后旧条目不再命中，清空成本可接受） */
const SNIPPET_CACHE_MAX = 2000;

function renderSnippetCached(snippet: string, query: string): React.ReactNode {
  const key = `${query}\u0000${snippet}`;
  const cached = snippetCache.get(key);
  if (cached !== undefined) return cached;
  const node = renderSnippet(snippet, query);
  if (snippetCache.size >= SNIPPET_CACHE_MAX) snippetCache.clear();
  snippetCache.set(key, node);
  return node;
}

/** 搜索面板默认高度（浮层，可拖拽调整；不占布局空间、不挤压编辑器/日志） */
const DEFAULT_PANEL_HEIGHT = 220;
/** 面板高度拖拽范围 */
const PANEL_MIN_HEIGHT = 100;
const PANEL_MAX_HEIGHT = () => window.innerHeight - 200;

interface Props {
  /** 右侧让位（px）= 服务列宽度：浮层覆盖在内容区上，需与服务列左缘对齐 */
  rightOffset?: number;
  /** 日志视图打开中：日志优先完整占据区域，搜索面板让位（不渲染，等同被日志挡住）。
   *  open 状态与搜索结果保留——关闭日志后自动恢复显示 */
  blocked?: boolean;
}

/** 底部搜索结果面板（浮层）：覆盖在编辑器之上，不改变其高度布局；
 *  右键服务/目录树节点触发 */
export function SearchResultPanel({ rightOffset = 0, blocked = false }: Props) {
  // 全仓唯一一处**不带 selector** 的 store 订阅（PERF-39）：当前这个 store 只有 3 个
  // 用户动作改写的字段，性能上无可测后果；风险是"以后加个每帧都变的字段就在这里静默变慢"。
  // 逐字段取，与其余 46 处订阅同口径。
  const open = useSearchModalStore(s => s.open);
  const root = useSearchModalStore(s => s.root);
  const title = useSearchModalStore(s => s.title);
  const closeSearch = useSearchModalStore(s => s.closeSearch);
  const [query, setQuery] = useState('');
  const [caseSensitive, setCaseSensitive] = useState(false);
  const [extensions, setExtensions] = useState('');
  const [status, setStatus] = useState<'idle' | 'searching' | 'done'>('idle');
  const [results, setResults] = useState<SearchResultItem[]>([]);
  /** 产出当前结果的查询词（与结果同步更新，见 triggerSearch） */
  const [resultQuery, setResultQuery] = useState('');
  const [truncated, setTruncated] = useState(false);
  /** 搜索根是文件、且它压根没被搜索时的原因（后端带回来的，见 SearchResponse.skipped） */
  const [skipped, setSkipped] = useState<string | null>(null);
  /**
   * 键盘选中的结果行（`rows` 下标；-1 = 未选中）。
   *
   * 只落在**命中行**上：文件头没有可定位的行号，方向键停在它上面时用户按回车
   * 不知道会发生什么。改输入框内容即重置——于是"回车 = 提交搜索"与"回车 = 打开
   * 选中行"不会打架（见输入框的 onKeyDown）。
   */
  const [activeIndex, setActiveIndex] = useState(-1);
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
    /** 结束标记：mouseup / blur / 卸载 任一先到即拆监听，重复调用安全 */
    let finished = false;

    const flush = () => {
      rafId = null;
      if (!pendingEvent || !dragStateRef.current) return;
      const { startY, startHeight } = dragStateRef.current;
      setPanelHeight(Math.max(PANEL_MIN_HEIGHT, Math.min(PANEL_MAX_HEIGHT(), startHeight + (startY - pendingEvent.clientY))));
      pendingEvent = null;
    };

    const handleUp = () => {
      if (finished) return;
      finished = true;
      try {
        if (rafId !== null) cancelAnimationFrame(rafId);
        dragStateRef.current = null;
        setDragging(false);
      } finally {
        removeListeners();
      }
    };

    const handleMove = (e: MouseEvent) => {
      // 无按键的移动 = 松手事件丢失（在窗口外松手），按松手处理
      if (e.buttons === 0) { handleUp(); return; }
      pendingEvent = e;
      if (rafId === null) rafId = requestAnimationFrame(flush);
    };

    const removeListeners = () => {
      document.removeEventListener('mousemove', handleMove);
      document.removeEventListener('mouseup', handleUp);
      window.removeEventListener('blur', handleUp);
    };

    document.addEventListener('mousemove', handleMove);
    document.addEventListener('mouseup', handleUp);
    // 拖拽中切走窗口（alt-tab）收不到 mouseup，用 blur 兜底结束
    window.addEventListener('blur', handleUp);
    return () => {
      finished = true;
      if (rafId !== null) cancelAnimationFrame(rafId);
      removeListeners();
    };
  }, [dragging]);

  /** 上一次打开时的搜索根：用来区分"换了范围"与"又打开同一个地方" */
  const lastRootRef = useRef<string | null>(null);

  /**
   * 每次打开：清结果、聚焦输入框（作废旧请求，防止上次响应污染）。
   *
   * **同一个根**重开时保留查询词与筛选条件：搜索通常是反复的（看一眼代码、再回来搜
   * 下一个词），每次清空等于每次重打。换了根则全清——不同的范围配上次的条件只会误导。
   * 结果无论如何都清：上一次的结果属于上一次的范围，留着比空着更危险。
   */
  useEffect(() => {
    if (!open) return;
    seqRef.current++;
    const rootChanged = lastRootRef.current !== root;
    lastRootRef.current = root;
    if (rootChanged) {
      setQuery('');
      setCaseSensitive(false);
      setExtensions('');
    }
    setStatus('idle');
    setResults([]);
    setResultQuery('');
    setTruncated(false);
    setSkipped(null);
    setError(null);
    setCollapsedPaths(new Set());
    setActiveIndex(-1);
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
    setActiveIndex(-1); // 新结果从"未选中"开始，回车回到"提交搜索"的语义
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
      // 生效查询词与结果一起存：展示高亮与点击定位都用它。
      // 若读当前的输入框内容，用户搜完再改几个字（未回车）就会出现"结果还是旧的、
      // 高亮/定位却按新词走"（搜索只在回车时执行，两者不同步）
      setResultQuery(q);
      setTruncated(res.truncated);
      setSkipped(res.skipped ?? null);
      setStatus('done');
      setError(null);
    }).catch((e: unknown) => {
      if (seq !== seqRef.current) return;
      setResults([]);
      setResultQuery('');
      setTruncated(false);
      setSkipped(null);
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

  /**
   * 扁平化行列表（文件头 + 命中行）：分组结构无法直接虚拟化，先摊平成行数组，
   * 再由虚拟滚动只渲染可视区的几十行。原来 1000 条命中 + 文件头会全部落在 DOM 里
   * （约 2000 个容器 + 数千个文本节点），每次折叠/展开都要全量重算。
   */
  type ResultRow =
    | { kind: 'header'; path: string; name: string; count: number }
    | { kind: 'hit'; path: string; name: string; line: number; snippet: string; index: number };
  const rows = useMemo<ResultRow[]>(() => {
    const out: ResultRow[] = [];
    for (const g of groups) {
      out.push({ kind: 'header', path: g.path, name: g.name, count: g.hits.length });
      if (collapsedPaths.has(g.path)) continue;
      g.hits.forEach((hit, i) => {
        out.push({ kind: 'hit', path: g.path, name: g.name, line: hit.line, snippet: hit.snippet, index: i });
      });
    }
    return out;
  }, [groups, collapsedPaths]);

  const scrollRef = useRef<HTMLDivElement | null>(null);
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: useCallback((i: number) => (rows[i]?.kind === 'header' ? 26 : 22), [rows]),
    overscan: 12,
  });
  const virtualItems = virtualizer.getVirtualItems();

  /**
   * 键盘上下移动选中行。**跳过文件头**：它没有可定位的行号，停在它上面时回车无事可做，
   * 用户会以为是坏了。到头就停住（不循环——循环会让"我已经翻到底了"失去反馈）。
   */
  const moveActive = useCallback((delta: number) => {
    setActiveIndex(prev => {
      let next = prev;
      for (let step = 0; step < rows.length; step++) {
        next += delta;
        if (next < 0 || next >= rows.length) return prev; // 到头：保持原位
        if (rows[next].kind === 'hit') return next;
      }
      return prev;
    });
  }, [rows]);

  // 选中行滚进视野：键盘导航必须看得见自己选到了哪一行（列表是虚拟滚动的）
  useEffect(() => {
    if (activeIndex >= 0) virtualizer.scrollToIndex(activeIndex, { align: 'auto' });
  }, [activeIndex, virtualizer]);

  const toggleCollapsed = useCallback((path: string) => {
    setCollapsedPaths(prev => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path); else next.add(path);
      return next;
    });
  }, []);

  /**
   * 把"该给面板留多高"写到根元素的 CSS 变量上，主内容区拿它做 padding-bottom。
   *
   * 为什么不是浮层直接盖在内容上：面板贴底、内容区高度不变时，滚到底的最后几行
   * **永远**落在面板背后——不是"滚一下就能看见"，而是根本看不见。
   *
   * 为什么走 CSS 变量而不把高度提到 store：拖拽调整高度时 `panelHeight` 每帧都在变，
   * 经 store 传导会让整个内容区（含 CodeMirror）每帧走一遍 React 渲染 + 布局；
   * 写变量只让浏览器做一次布局，React 这一侧完全不动。
   */
  useEffect(() => {
    const root = document.documentElement;
    root.style.setProperty('--nexus-search-panel-h', open && !blocked ? `${panelHeight}px` : '0px');
    // 卸载（切项目/关面板）时清零，否则内容区会一直空着一块
    return () => { root.style.setProperty('--nexus-search-panel-h', '0px'); };
  }, [open, blocked, panelHeight]);

  // 日志视图打开时让位（日志优先完整占据区域，搜索面板等同被日志挡住）；
  // open/查询结果保留，关闭日志后自动恢复
  if (!open || blocked) return null;

  return (
    <div
      // 浮层（absolute 覆盖）：不占文档流高度——否则与日志/编辑器争空间，
      // 表现为「先开搜索把日志顶上去、先开日志又把搜索挤出可视区」两种不一致
      // z-[55]：低于编辑面板 z-[60]、模态遮罩 z-[65]、右键菜单 z-[70]
      className="absolute left-0 bottom-0 z-[55] flex flex-col bg-nexus-surface border-t border-nexus-border shadow-2xl overflow-hidden"
      style={{ height: panelHeight, right: rightOffset }}
      // Esc 关闭：输入框 / 扩展名框 / 结果区的焦点都能冒泡到这里。
      // 面板是一次性的浮层，Esc 是搜索界面的肌肉记忆（stopPropagation 免得
      // 同一个 Esc 再被外层的弹窗处理一次）
      onKeyDown={e => {
        if (e.key === 'Escape') {
          e.stopPropagation();
          handleClose();
        }
      }}
    >
      {/* 拖拽条：上下调整面板高度 */}
      <div
        className={`flex-shrink-0 cursor-row-resize transition-colors ${dragging ? 'bg-nexus-accent' : 'bg-nexus-border hover:bg-nexus-accent'}`}
        style={{ height: 3 }}
        onMouseDown={handleDragStart}
        title="拖拽调整高度"
      />
      {/* 头部：范围 + 输入 + 搜索 + 选项 + 关闭。
          可换行 + min-h：AI 面板/服务列同开时主区可能只剩几百像素，
          单行不换行会把扩展名框和「关闭」挤出可视区（都是 flex-shrink-0） */}
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1 px-3 min-h-[38px] py-1 border-b border-nexus-border flex-shrink-0">
        <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.4" className="text-nexus-muted flex-shrink-0">
          <circle cx="5" cy="5" r="3.5"/><line x1="7.8" y1="7.8" x2="10.5" y2="10.5"/>
        </svg>
        <span className="text-[12px] text-nexus-text truncate max-w-[160px] flex-shrink-0" title={`${title} · ${root}`}>
          搜索「{title}」
        </span>
        <input
          ref={inputRef}
          className="flex-1 min-w-[120px] px-2.5 py-1.5 text-[12px] bg-nexus-bg border border-nexus-border rounded-md text-nexus-text font-mono placeholder:text-nexus-muted/50 focus:outline-none focus:border-nexus-accent transition-colors"
          placeholder="输入内容，回车搜索（↑↓ 选结果，Esc 关闭）"
          value={query}
          // 改了内容就丢掉键盘选中：否则回车会变成"打开上一次选中的那行"而不是提交新搜索
          onChange={e => { setQuery(e.target.value); setActiveIndex(-1); }}
          onKeyDown={e => {
            if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
              // 单行输入框里这两个键本无作用，拦下来做结果选择
              e.preventDefault();
              moveActive(e.key === 'ArrowDown' ? 1 : -1);
              return;
            }
            if (isSubmitEnter(e)) {
              const row = rows[activeIndex];
              if (row?.kind === 'hit') locateFile(row.path, row.name, row.line, resultQuery);
              else triggerSearch();
            }
          }}
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
        {/* 扩展名筛选带常驻标签：原先只有 placeholder，一输入标签就没了——
            回头看到这个框会想不起来它是干什么的 */}
        <label
          className="flex items-center gap-1 flex-shrink-0 text-[11px] text-nexus-muted"
          title="扩展名筛选，逗号分隔，留空即全部"
        >
          扩展名
          <input
            className="w-[96px] px-2 py-1 text-[11px] bg-nexus-bg border border-nexus-border rounded-md text-nexus-text font-mono placeholder:text-nexus-muted/50 focus:outline-none focus:border-nexus-accent transition-colors"
            placeholder="ts,vue"
            value={extensions}
            onChange={e => setExtensions(e.target.value)}
          />
        </label>
        <button
          className="flex-shrink-0 px-3 py-1.5 text-[12px] text-nexus-muted border border-nexus-border rounded-md hover:text-nexus-text hover:border-nexus-muted transition-colors"
          onClick={handleClose}
        >关闭</button>
      </div>

      {/* 结果列表（虚拟滚动：只渲染可视区的行） */}
      <div ref={scrollRef} className="flex-1 overflow-auto">
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
          <div className="flex flex-col items-center justify-center h-full px-4 text-center gap-1">
            {/* 空结果有两种含义，别把"压根没搜"说成"没找到"：用户在后者会去改搜索词，
                而真正该改的是扩展名筛选框（或他本就不该期待在 png 里搜到文本） */}
            {skipped ? (
              <>
                <span className="text-[12px] text-nexus-warning">这个文件没有被搜索</span>
                <span className="text-[11px] text-nexus-muted max-w-[420px]">{skipped}</span>
              </>
            ) : (
              <span className="text-[12px] text-nexus-muted">未找到匹配内容</span>
            )}
            {truncated && <span className="text-[11px] text-nexus-muted/60">（扫描文件过多已中止）</span>}
          </div>
        )}
        {results.length > 0 && !error && (
          <div style={{ height: virtualizer.getTotalSize(), position: 'relative', width: '100%' }}>
            {virtualItems.map(vi => {
              const row = rows[vi.index];
              if (!row) return null;
              return (
                <div
                  key={vi.key}
                  data-index={vi.index}
                  ref={virtualizer.measureElement}
                  className="absolute left-0 top-0 w-full"
                  style={{ transform: `translateY(${vi.start}px)` }}
                >
                  {row.kind === 'header' ? (
                    <ResultGroupHeader
                      path={row.path}
                      label={relPath(row.path)}
                      count={row.count}
                      collapsed={collapsedPaths.has(row.path)}
                      onToggle={toggleCollapsed}
                    />
                  ) : (
                    <ResultHitRow
                      path={row.path}
                      name={row.name}
                      line={row.line}
                      snippet={row.snippet}
                      query={resultQuery}
                      active={vi.index === activeIndex}
                    />
                  )}
                </div>
              );
            })}
          </div>
        )}
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

/**
 * 文件头行：`memo` + 回调由父级 `useCallback` 固定引用。
 * 折叠/展开某个文件时，其余文件头不会因新回调/新 Set 引用而重渲染。
 */
const ResultGroupHeader = memo(function ResultGroupHeader({
  path, label, count, collapsed, onToggle,
}: {
  path: string;
  label: string;
  count: number;
  collapsed: boolean;
  onToggle: (path: string) => void;
}) {
  return (
    <div
      className="flex items-center gap-1.5 px-3 py-1 cursor-pointer hover:bg-nexus-hover/50 transition-colors"
      onClick={() => onToggle(path)}
      title={`${path}（${count} 处命中）`}
    >
      <svg
        className={`flex-shrink-0 text-nexus-muted transition-transform ${collapsed ? '' : 'rotate-90'}`}
        width="10" height="10" viewBox="0 0 10 10" fill="none"
        stroke="currentColor" strokeWidth="1.3" strokeLinecap="round"
      >
        <polyline points="3,1 7,5 3,9" />
      </svg>
      <span className="flex-1 min-w-0 text-[12px] text-nexus-text font-medium truncate">{label}</span>
      <span className="flex-shrink-0 text-[10px] text-nexus-muted bg-nexus-hover/60 rounded px-1 py-0.5">{count}</span>
    </div>
  );
});

/**
 * 命中行：`memo` + 片段渲染走缓存（见 renderSnippetCached）。
 * 行内容只由 path/line/snippet/query 决定，重渲染时这几项引用不变即跳过。
 */
const ResultHitRow = memo(function ResultHitRow({
  path, name, line, snippet, query, active,
}: {
  path: string;
  name: string;
  line: number;
  snippet: string;
  query: string;
  /** 键盘选中态（↑↓ 移动）：与 hover 分开表示，让"回车会打开哪一行"始终看得见 */
  active: boolean;
}) {
  return (
    <div
      className={`flex items-start gap-2 pl-7 pr-3 py-0.5 cursor-pointer transition-colors ${
        active ? 'bg-nexus-accent/15' : 'hover:bg-nexus-hover/50'
      }`}
      onClick={() => locateFile(path, name, line, query)}
      title={`${path}:${line}（点击定位到该行）`}
    >
      <span className="flex-shrink-0 text-[11px] text-nexus-muted font-mono leading-relaxed">:{line}</span>
      <div className="flex-1 min-w-0 text-[11px] text-nexus-text-muted font-mono truncate leading-relaxed">
        {renderSnippetCached(snippet, query)}
      </div>
    </div>
  );
});
