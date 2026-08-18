import { useEffect, useRef, useCallback, useState, useMemo } from 'react';
import { useLogStore } from '../../stores/logStore';
import { useRunningStore } from '../../stores/runningStore';
import { logService } from '../../services/logService';
import { renderLine } from '../../utils/logFormatter';
import type { ServiceLogLine } from '../../services/logService';

interface LogViewerProps { serviceKey: string; serviceName?: string; maxHeight?: string; fill?: boolean; onClose?: () => void; }

/** 渲染上限 = 数据缓冲上限（只保留最新 2000 行，数据与 DOM 一致） */
const RENDER_CAP = 2000;
/** 空数组常量（避免选择器每次返回新引用导致无谓重渲染） */
const EMPTY_LINES: ServiceLogLine[] = [];
/** 搜索结果渲染上限（超出只渲染尾部，防止逐字输入/大日志下全量 innerHTML 卡顿） */
const SEARCH_RENDER_CAP = 500;
/** 搜索输入防抖间隔 */
const SEARCH_DEBOUNCE_MS = 200;

export function LogViewer({ serviceKey, serviceName: serviceNameProp, maxHeight, fill, onClose }: LogViewerProps) {
  const preRef = useRef<HTMLPreElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const lastVersionRef = useRef(-1);
  const renderedCountRef = useRef(0);
  const pausedRef = useRef(false);
  /** 搜索态是否渲染过搜索结果（退出搜索时用于强制全量重建） */
  const searchRenderedRef = useRef(false);
  /** 上次渲染时数据头部行的引用：变化 = 滑动窗口滚动（满 5000 行后行号增量失效）→ 全量重建 */
  const lastHeadRef = useRef<ServiceLogLine | null>(null);
  /** 上次渲染时数据最后一条的文本：变化（\r 单行刷新合并，行数不变）→ 更新 DOM 最后一行 */
  const lastTextRef = useRef<string | null>(null);
  /** 上一次的暂停状态：从暂停恢复时强制全量重建（直接渲染最新 2000 行） */
  const lastPausedRef = useRef(false);
  /** 暂停开始时的累计新增行数（差值 = 暂停期间新增 N 行） */
  const baseAddedRef = useRef(0);

  const [paused, setPaused] = useState(false);
  const [searchTerm, setSearchTerm] = useState('');
  const [searchIdx, setSearchIdx] = useState(0);

  // 双数据源：跟随显示最新 2000 行；暂停显示暂停视图（快照，满 2000 后冻结）
  const followLines = useLogStore((s) => s.logs[serviceKey]) ?? EMPTY_LINES;
  const pausedLines = useLogStore((s) => s.pausedLogs[serviceKey]) ?? EMPTY_LINES;
  const lines = paused ? pausedLines : followLines;
  const version = useLogStore((s) => s.version[serviceKey]) ?? 0;

  // 暂停开始时记录累计新增行数基准，差值 = 暂停期间新增 N 行（精确计数，\r 刷新帧不计）
  const totalAdded = useLogStore(s => s.totalAdded[serviceKey]) ?? 0;
  useEffect(() => {
    if (paused) {
      baseAddedRef.current = useLogStore.getState().totalAdded[serviceKey] ?? 0;
    }
  }, [paused, serviceKey]);
  const newSincePause = paused ? Math.max(0, totalAdded - baseAddedRef.current) : 0;

  // ── 运行状态 ──────────────────────────────────────────────

  const isRunning = useRunningStore(s => s.running.some(r => r.service_id === serviceKey));
  // 以后端缓冲为权威同步日志：
  // - 空快照（正常停止已清空）→ 清本地缓存，避免展示已清空的旧日志（含失败日志）
  // - 非空快照（崩溃保留 / 运行中）→ 覆盖本地缓存（后端保证先写缓冲再 emit，快照不丢行）
  const syncLogsFromBackend = useCallback(() => {
    logService.getServiceLogs(serviceKey).then(
      (snapshot) => {
        if (snapshot.length === 0) {
          useLogStore.getState().clearLogs(serviceKey);
        } else {
          useLogStore.getState().setLogs(serviceKey, snapshot);
        }
      },
      (e) => { console.error('同步服务日志失败:', serviceKey, e); }
    );
  }, [serviceKey]);

  // 运行中 → 已停止（崩溃/秒退/手动停止）：同步后端日志——
  // 崩溃时快照含退出码行（system），正常停止时后端已清空、同步清本地缓存
  const prevRunningRef = useRef(isRunning);
  useEffect(() => {
    const was = prevRunningRef.current;
    prevRunningRef.current = isRunning;
    if (was && !isRunning) {
      syncLogsFromBackend();
    }
  }, [isRunning, serviceKey, syncLogsFromBackend]);

  // ── 搜索 ──────────────────────────────────────────────────

  // 输入防抖：逐字输入不触发全量匹配 + 全量渲染
  const [debouncedTerm, setDebouncedTerm] = useState('');
  useEffect(() => {
    const t = setTimeout(() => setDebouncedTerm(searchTerm), SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(t);
  }, [searchTerm]);

  const searchActive = debouncedTerm.trim().length > 0;
  const filteredLines = useMemo(() => {
    if (!searchActive) return null;
    return lines.filter(l => l.text.toLowerCase().includes(debouncedTerm.toLowerCase()));
  }, [lines, searchActive, debouncedTerm]);
  const searchMatches = filteredLines?.length ?? 0;

  // ── 切换服务：重置 ────────────────────────────────────────

  // 卸载时清理暂停视图（释放暂停期间保留的数据）
  useEffect(() => () => {
    useLogStore.getState().resumeLogs(serviceKey);
  }, [serviceKey]);

  useEffect(() => {
    lastVersionRef.current = -1;
    renderedCountRef.current = 0;
    if (preRef.current) preRef.current.innerHTML = '';
    if (scrollRef.current) scrollRef.current.scrollTop = 0;
    // 切换服务：清理上个服务的暂停状态
    useLogStore.getState().resumeLogs(serviceKey);
    // 打开面板：无条件以后端缓冲为准同步（后端清空过则本地缓存一并清掉）
    syncLogsFromBackend();
  }, [serviceKey, syncLogsFromBackend]);

  // ── 搜索渲染（独立 effect）：只按搜索词重建，不随日志增量刷新 ──

  useEffect(() => {
    if (!searchActive) return;
    const pre = preRef.current;
    if (!pre) return;
    searchRenderedRef.current = true;
    renderSearchResults(pre, filteredLines, debouncedTerm, lastVersionRef, renderedCountRef);
  }, [searchActive, debouncedTerm, filteredLines]);

  // ── DOM 渲染：version 检测 + 增量追加（搜索激活时跳过）────

  useEffect(() => {
    const pre = preRef.current;
    if (!pre) return;

    // 与 state 同步（handlePause 已同步更新过，此处保证所有渲染路径一致）
    pausedRef.current = paused;

    if (searchActive) {
      // 搜索视图由上方独立 effect 渲染；此处避免高频日志触发全量重建
      searchRenderedRef.current = true;
      return;
    }

    // 从暂停恢复：直接全量渲染最新 2000 行（跟随数据源一直在后台维护）
    // 注意：必须在任何提前 return 之前更新 lastPausedRef，否则恢复判断失效
    const justResumed = lastPausedRef.current && !paused;
    lastPausedRef.current = paused;

    // 退出搜索后强制全量重建一次，恢复完整日志视图
    const justExitedSearch = searchRenderedRef.current;
    searchRenderedRef.current = false;

    const prevVer = lastVersionRef.current;
    lastVersionRef.current = version;

    // 数据头部引用变化 = 滑动窗口已滚动（2000 行满后旧行被顶掉、新行进来，
    // 数组长度不变导致按行号增量失效）→ 必须全量重建
    const headLine = lines[0] ?? null;
    const headChanged = headLine !== lastHeadRef.current;
    lastHeadRef.current = headLine;

    // 数据最后一条文本变化（\r 单行刷新合并：行数不变但内容更新）→ 更新 DOM 最后一行
    const lastLine = lines[lines.length - 1];
    const lastTextChanged = lastLine !== undefined && lastLine.text !== lastTextRef.current;
    lastTextRef.current = lastLine?.text ?? null;

    // 首次渲染 / 退出搜索 / 日志被裁剪或清空 / 窗口滑动 / 从暂停恢复 → 全量重建；否则增量追加
    const needFull = prevVer < 0 || lines.length < renderedCountRef.current || justExitedSearch || headChanged || justResumed;

    if (needFull) {
      renderFull(pre, lines, renderedCountRef);
    } else {
      renderIncremental(pre, lines, renderedCountRef);
      if (lastTextChanged && lastLine && pre.lastElementChild) {
        // \r 刷新行合并：同步更新 DOM 最后一行
        pre.lastElementChild.innerHTML = lineHtml(lastLine);
      }
    }

    scrollToBottom(pausedRef, scrollRef);
  }, [lines, version, searchActive, paused]);

  // ── 暂停 ──────────────────────────────────────────────────
  // 暂停 = 快照当前跟随数据源为暂停视图（独立数据源，满 2000 行后冻结，不会再滚动）；
  // 恢复 = 丢弃暂停视图，切回跟随数据源（其始终维护最新 2000 行）

  const handlePause = useCallback(() => {
    const next = !paused;
    // 同步更新 ref（不等 React 提交/effect）：排队的 rAF 回调在下一帧绘制前执行，
    // 若 ref 更新滞后，点击暂停的瞬间仍会滚到底部
    pausedRef.current = next;
    setPaused(next);
    if (next) {
      useLogStore.getState().pauseLogs(serviceKey);
    } else {
      useLogStore.getState().resumeLogs(serviceKey);
    }
  }, [paused, serviceKey]);

  // pausedRef 与 state 同步（在渲染提交后的 effect 中，不在 setState updater 里做 DOM 副作用）
  // 恢复跟随时跳到底部：effect 在渲染提交后执行，scrollHeight 是最新 DOM 高度，
  // 避免在 updater 中同步读取旧 scrollHeight 导致"关闭重开才看到最新"的问题
  useEffect(() => {
    if (!paused && scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [paused]);


  // ── 搜索导航 ──────────────────────────────────────────────

  const goMatch = useCallback((dir: 1 | -1) => {
    if (!filteredLines || filteredLines.length === 0) return;
    const next = ((searchIdx + dir) % filteredLines.length + filteredLines.length) % filteredLines.length;
    setSearchIdx(next);
    // 滚动到匹配行（仅当该行在渲染范围内；超出渲染上限的旧匹配无法定位）
    const pre = preRef.current;
    if (pre) {
      const offset = filteredLines.length > SEARCH_RENDER_CAP ? filteredLines.length - SEARCH_RENDER_CAP : 0;
      const rel = next - offset;
      if (rel >= 0) {
        pre.querySelector(`[data-match-idx="${rel}"]`)?.scrollIntoView({ block: 'nearest' });
      }
    }
  }, [searchIdx, filteredLines]);

  // ── 快捷键 ────────────────────────────────────────────────

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key === 'f') {
        e.preventDefault();
        setTimeout(() => searchRef.current?.focus(), 50);
      }
      if (e.key === 'Escape' && document.activeElement === searchRef.current) {
        searchRef.current?.blur();
      }
    };
    document.addEventListener('keydown', handler);
    return () => document.removeEventListener('keydown', handler);
  }, []);

  // serviceKey 为 service_id（UUID），显示名由父组件传入；split(':') 仅作兼容兜底
  const serviceName = serviceNameProp ?? serviceKey.split(':').pop() ?? serviceKey;

  return (
    <div className={`flex flex-col ${fill ? 'h-full' : ''}`}>
      <LogHeader
        serviceName={serviceName}
        lineCount={lines.length}
        isRunning={isRunning}
        onClose={onClose}
        searchTerm={searchTerm}
        setSearchTerm={setSearchTerm}
        setSearchIdx={setSearchIdx}
        searchRef={searchRef}
        searchActive={searchActive}
        searchMatches={searchMatches}
        searchIdx={searchIdx}
        onGoMatch={goMatch}
        paused={paused}
        onPause={handlePause}
        newSincePause={newSincePause}
        onClear={() => {
          useLogStore.getState().clearLogs(serviceKey);
          if (preRef.current) preRef.current.innerHTML = '';
          lastVersionRef.current = -1;
        }}
      />

      <div ref={scrollRef} className={`overflow-auto bg-[#0d1117] ${fill ? 'flex-1' : ''}`}
        style={fill ? undefined : { maxHeight: maxHeight ?? '220px' }}>
        <pre ref={preRef} className="font-mono text-[13px] leading-relaxed text-[#c9d1d9]/80 whitespace-pre-wrap break-all px-4 py-3 m-0 min-h-full"/>
      </div>
    </div>
  );
}

// ── 渲染辅助函数 ──────────────────────────────────────────

function renderSearchResults(
  pre: HTMLPreElement,
  filteredLines: ServiceLogLine[] | null,
  searchTerm: string,
  lastVersionRef: React.MutableRefObject<number>,
  renderedCountRef: React.MutableRefObject<number>,
) {
  if (!filteredLines) return;
  const display = filteredLines.length > SEARCH_RENDER_CAP
    ? filteredLines.slice(-SEARCH_RENDER_CAP)
    : filteredLines;
  // 行上带 data-match-idx（相对索引），供导航滚动定位
  pre.innerHTML = display.map((l, i) =>
    `<div class="log-line search-match" data-match-idx="${i}"><span class="log-ts">${fmtTime(l.timestamp)}</span>${renderLine(l.text, searchTerm)}</div>`
  ).join('\n');
  lastVersionRef.current = -1; // 搜索态不参与增量渲染
  renderedCountRef.current = 0;
}

/** 单行日志的 DOM 结构（span + display:block，便于按行裁剪）；system 行（生命周期标记）独立样式 */
function lineHtml(l: ServiceLogLine): string {
  const cls = l.stream === 'system' ? 'log-line log-line-system' : 'log-line';
  return `<span class="${cls}"><span class="log-ts">${fmtTime(l.timestamp)}</span>${renderLine(l.text, '')}</span>`;
}

/** 时间戳显示为本地 HH:MM:SS */
function fmtTime(ts: string): string {
  if (!ts) return '';
  const d = new Date(ts);
  if (isNaN(d.getTime())) return '';
  const hh = String(d.getHours()).padStart(2, '0');
  const mm = String(d.getMinutes()).padStart(2, '0');
  const ss = String(d.getSeconds()).padStart(2, '0');
  return `${hh}:${mm}:${ss}`;
}

function renderFull(
  pre: HTMLPreElement,
  lines: ServiceLogLine[],
  renderedCountRef: React.MutableRefObject<number>,
) {
  const display = lines.length > RENDER_CAP ? lines.slice(-RENDER_CAP) : lines;
  // 一次性重建（不做 rAF 分批——批次与下一次 flush 的清空交错会导致内容损坏）
  pre.innerHTML = display.map(lineHtml).join('');
  renderedCountRef.current = lines.length;
}

function renderIncremental(
  pre: HTMLPreElement,
  lines: ServiceLogLine[],
  renderedCountRef: React.MutableRefObject<number>,
) {
  const newLines = lines.slice(renderedCountRef.current);
  if (newLines.length > 0) {
    pre.insertAdjacentHTML('beforeend', newLines.map(lineHtml).join(''));
  }
  // DOM 只保留尾部 RENDER_CAP 行，避免无限增长
  while (pre.children.length > RENDER_CAP) {
    pre.removeChild(pre.firstElementChild!);
  }
  renderedCountRef.current = lines.length;
}

function scrollToBottom(
  pausedRef: React.MutableRefObject<boolean>,
  scrollRef: React.RefObject<HTMLDivElement | null>,
) {
  if (!pausedRef.current && scrollRef.current) {
    requestAnimationFrame(() => {
      // 回调执行时用户可能已点击暂停（排队期间状态变了）：
      // 再次检查，避免"点击暂停的瞬间仍然滚到底部"
      if (!pausedRef.current && scrollRef.current) {
        scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
      }
    });
  }
}

// ── 头部组件 ──────────────────────────────────────────────

function LogHeader({
  serviceName, lineCount, isRunning, onClose,
  searchTerm, setSearchTerm, setSearchIdx, searchRef,
  searchActive, searchMatches, searchIdx, onGoMatch,
  paused, onPause, onClear, newSincePause,
}: {
  serviceName: string;
  lineCount: number;
  isRunning: boolean;
  onClose?: () => void;
  searchTerm: string;
  setSearchTerm: (s: string) => void;
  setSearchIdx: (fn: (prev: number) => number) => void;
  searchRef: React.RefObject<HTMLInputElement | null>;
  searchActive: boolean;
  searchMatches: number;
  searchIdx: number;
  onGoMatch: (dir: 1 | -1) => void;
  paused: boolean;
  onPause: () => void;
  onClear: () => void;
  newSincePause: number;
}) {
  return (
    <div className="flex-shrink-0 flex items-center h-12 px-4 border-b border-[#30363d] bg-[#161b22] gap-3 select-none">
      {onClose && (
        <button className="h-7 w-7 flex items-center justify-center rounded-lg text-[#8b949e] hover:text-[#c9d1d9] hover:bg-white/5 transition-colors flex-shrink-0"
          onClick={onClose}>
          <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"><path d="M4 4l8 8M12 4l-8 8"/></svg>
        </button>
      )}
      <span className="w-[7px] h-[7px] rounded-full bg-emerald-400 flex-shrink-0"/>
      <span className="text-[13px] text-[#c9d1d9] font-medium truncate">{serviceName}</span>
      {/* 状态标签：随运行状态变化（原为硬编码"运行中"，服务停止后显示错误状态） */}
      <span className={`text-[11px] px-1.5 py-0.5 rounded-md border flex-shrink-0 ${
        isRunning
          ? 'bg-emerald-400/10 text-emerald-400 border-emerald-400/20'
          : 'bg-[#8b949e]/10 text-[#8b949e] border-[#30363d]'
      }`}>
        {isRunning ? '运行中' : '未运行'}
      </span>
      <span className="text-[12px] text-[#8b949e] flex-shrink-0" title="当前行数（只保留最新 2000 行）">
        {lineCount.toLocaleString()} 行
      </span>
      <div className="flex-1"/>

      <div className="flex items-center gap-1.5 bg-[#0d1117] rounded-lg px-2.5 h-[30px] border border-[#30363d] focus-within:border-[#58a6ff] transition-colors">
        <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="#8b949e" strokeWidth="1.5"><circle cx="6.5" cy="6.5" r="4.5"/><path d="M10 10l4 4"/></svg>
        <input ref={searchRef as React.Ref<HTMLInputElement>} className="w-[130px] bg-transparent text-[12px] text-[#c9d1d9] outline-none placeholder:text-[#484f58] font-mono"
          placeholder="查找…" value={searchTerm}
          onChange={e => { setSearchTerm(e.target.value); setSearchIdx(() => 0); }}
          onKeyDown={e => { if (e.key === 'Enter') { e.preventDefault(); onGoMatch(e.shiftKey ? -1 : 1); } }}/>
        {searchActive && (
          <>
            <span className="text-[11px] text-[#8b949e] font-mono tabular-nums w-[32px] text-right">{searchMatches > 0 ? `${Math.min(searchIdx + 1, searchMatches)}/${searchMatches}` : '0/0'}</span>
            {searchMatches > SEARCH_RENDER_CAP && (
              <span className="text-[10px] text-[#8b949e]/60 flex-shrink-0">仅显示尾部 {SEARCH_RENDER_CAP} 条</span>
            )}
            <button className="text-[#8b949e] hover:text-[#c9d1d9] text-[10px]" onClick={() => onGoMatch(-1)}>▲</button>
            <button className="text-[#8b949e] hover:text-[#c9d1d9] text-[10px]" onClick={() => onGoMatch(1)}>▼</button>
          </>
        )}
      </div>

      <button onClick={onPause}
        className={`h-[30px] px-2.5 flex items-center gap-1 rounded-lg text-[11px] border transition-colors flex-shrink-0 ${
          paused ? 'border-[#30363d] text-[#8b949e] hover:text-[#c9d1d9] hover:bg-white/5'
                 : 'border-[#d29922]/30 text-[#d29922] bg-[#d29922]/10 hover:bg-[#d29922]/20'}`}
        title={paused ? `恢复自动滚动到最新日志${newSincePause > 0 ? `（暂停期间新增 ${newSincePause} 行）` : ''}` : '暂停自动滚动（方便回溯查看）'}>
        {paused ? `跟随${newSincePause > 0 ? ` (${newSincePause})` : ''}` : '暂停'}
      </button>

      <button className="h-[30px] px-2.5 flex items-center gap-1 rounded-lg text-[11px] border border-[#30363d] text-[#8b949e] hover:text-[#c9d1d9] hover:bg-white/5 transition-colors flex-shrink-0"
        onClick={onClear}>
        <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round"><path d="M2 4h12M5.3 4V2.7c0-.4.3-.7.7-.7h4c.4 0 .7.3.7.7V4M6.7 7v5M9.3 7v5M3.3 4l.7 9.3c0 .4.3.7.7.7h6.6c.4 0 .7-.3.7-.7L12.7 4"/></svg>清空
      </button>
    </div>
  );
}
