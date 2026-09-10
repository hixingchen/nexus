import { useEffect, useLayoutEffect, useMemo, useRef, useState, useCallback, memo } from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';
import { useLogStore } from '../../stores/logStore';
import { useRunningStore } from '../../stores/runningStore';
import { logService } from '../../services/logService';
import { renderLine } from '../../utils/logFormatter';
import { isSubmitEnter } from '../../utils/keyboard';
import type { ServiceLogLine } from '../../services/logService';

interface LogViewerProps { serviceKey: string; serviceName?: string; maxHeight?: string; fill?: boolean; onClose?: () => void; }

/** 空数组常量（避免选择器每次返回新引用导致无谓重渲染） */
const EMPTY_LINES: ServiceLogLine[] = [];
/** 行高估算值（13px × leading-relaxed 1.625 ≈ 21px）；真实行高由 ResizeObserver 实测回填（长行换行更高） */
const ESTIMATED_ROW_H = 21;
/** 视口外预渲染行数：滚动留白兜底，同时限定「拖拽改宽时必须重新断行的行数」 */
const OVERSCAN = 12;
/** 日志上下内边距（交给 virtualizer 的 paddingStart/End，贴底计算与真实滚动高度同一口径） */
const PAD_Y = 12;
/** 搜索输入防抖间隔 */
const SEARCH_DEBOUNCE_MS = 200;

/**
 * 服务日志面板：虚拟滚动渲染（只挂载视口内约 30~50 行）。
 * 数据侧仍是「最多 2000 行」的滑动窗口，DOM 侧不再与之等量——
 * 日志再多也不会让拖拽分隔条时逐帧重排上千行文本（原实现为整块 <pre> innerHTML，
 * 宽度一变就要重新断行 2000 行，是拖拽卡顿的主因）。
 */
export function LogViewer({ serviceKey, serviceName: serviceNameProp, maxHeight, fill, onClose }: LogViewerProps) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
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

  // 输入防抖：逐字输入不触发全量匹配
  const [debouncedTerm, setDebouncedTerm] = useState('');
  useEffect(() => {
    const t = setTimeout(() => setDebouncedTerm(searchTerm), SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(t);
  }, [searchTerm]);

  // 搜索态渲染匹配行（虚拟滚动下无需再截断尾部：任意匹配都可达、可定位）
  const searchActive = debouncedTerm.trim().length > 0;
  const rows = useMemo(() => {
    if (!searchActive) return lines;
    const term = debouncedTerm.toLowerCase();
    return lines.filter(l => l.text.toLowerCase().includes(term));
  }, [lines, searchActive, debouncedTerm]);
  const searchMatches = searchActive ? rows.length : 0;
  /** 传给行组件的搜索词：非搜索态为空串（与旧的整块渲染同口径） */
  const rowTerm = searchActive ? debouncedTerm : '';

  // ── 虚拟列表 ──────────────────────────────────────────────

  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ESTIMATED_ROW_H,
    overscan: OVERSCAN,
    // 贴底语义：行高实测值变化（长行换行 / 拖拽改宽）时按总高差补偿滚动位置，
    // 保证「始终看着最新一行」不被重排或窗口滑动打断
    anchorTo: 'end',
    paddingStart: PAD_Y,
    paddingEnd: PAD_Y,
  });
  const virtualItems = virtualizer.getVirtualItems();

  // 切换服务：清空行高实测缓存（缓存以索引为键，会被新服务的行错误复用）并回到顶部；
  // 声明在贴底 effect 之前——layout effect 按声明顺序执行，先归零再贴底，最终落在最新一行
  useLayoutEffect(() => {
    virtualizer.measure();
    virtualizer.scrollToOffset(0);
    // 切换服务：清理上个服务的暂停状态
    useLogStore.getState().resumeLogs(serviceKey);
    // 打开面板：无条件以后端缓冲为准同步（后端清空过则本地缓存一并清掉）
    syncLogsFromBackend();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [serviceKey, syncLogsFromBackend]);

  // 卸载时清理暂停视图（释放暂停期间保留的数据）
  useEffect(() => () => {
    useLogStore.getState().resumeLogs(serviceKey);
  }, [serviceKey]);

  // 跟随模式：每次数据更新后贴底（等价旧实现的 scrollTop = scrollHeight）。
  // 用 layout effect 让新行与目标滚动位置同帧提交，避免先看到旧位置再跳到底部；
  // 暂停 / 搜索态不贴底（与旧实现一致：暂停冻结视图、搜索时定位由 goMatch 接管）
  useLayoutEffect(() => {
    if (paused || searchActive) return;
    virtualizer.scrollToEnd();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [version, rows.length, paused, searchActive]);

  // ── 暂停 ──────────────────────────────────────────────────
  // 暂停 = 快照当前跟随数据源为暂停视图（独立数据源，满 2000 行后冻结，不会再滚动）；
  // 恢复 = 丢弃暂停视图，切回跟随数据源（其始终维护最新 2000 行）

  const handlePause = useCallback(() => {
    const next = !paused;
    setPaused(next);
    if (next) {
      useLogStore.getState().pauseLogs(serviceKey);
    } else {
      useLogStore.getState().resumeLogs(serviceKey);
    }
  }, [paused, serviceKey]);

  // ── 搜索导航 ──────────────────────────────────────────────

  const goMatch = useCallback((dir: 1 | -1) => {
    if (rows.length === 0) return;
    const next = ((searchIdx + dir) % rows.length + rows.length) % rows.length;
    setSearchIdx(next);
    // 目标行滚到视口中央（虚拟列表任意行都可达，不再受渲染截断限制）
    virtualizer.scrollToIndex(next, { align: 'center' });
  }, [searchIdx, rows.length, virtualizer]);

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
          virtualizer.measure(); // 清空行高实测缓存：重新运行后索引 0..N 的行与旧日志无关
        }}
      />

      {/* 文本样式（字体/字号/行高/换行）放在滚动容器上由行继承，与旧 <pre> 的表现一致；
          行高实测值依赖这里的 leading-relaxed，改动需同步 ESTIMATED_ROW_H */}
      <div ref={scrollRef}
        className={`overflow-auto bg-[#0d1117] font-mono text-[13px] leading-relaxed text-[#c9d1d9]/80 whitespace-pre-wrap break-all px-4 ${fill ? 'flex-1' : ''}`}
        style={fill ? undefined : { maxHeight: maxHeight ?? '220px' }}>
        <div style={{ height: virtualizer.getTotalSize(), position: 'relative' }}>
          {virtualItems.map(vi => {
            const line = rows[vi.index];
            if (!line) return null;
            return (
              <div
                key={vi.key}
                data-index={vi.index}
                ref={virtualizer.measureElement}
                className="absolute left-0 top-0 w-full"
                style={{ transform: `translateY(${vi.start}px)` }}
              >
                <LogRow line={line} searchTerm={rowTerm} />
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

// ── 行渲染 ────────────────────────────────────────────────

/**
 * 单行日志（与原 innerHTML 渲染同一套结构与样式：时间戳 span + renderLine 着色）。
 * memo：滚动、拖拽、窗口滑动都不重建行 DOM，只有该行内容或搜索词变化才重渲染
 */
const LogRow = memo(function LogRow({ line, searchTerm }: { line: ServiceLogLine; searchTerm: string }) {
  const cls = line.stream === 'system' ? 'log-line log-line-system' : 'log-line';
  return (
    <div
      className={cls}
      dangerouslySetInnerHTML={{ __html: `<span class="log-ts">${fmtTime(line.timestamp)}</span>${renderLine(line.text, searchTerm)}` }}
    />
  );
});

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
      {/* 运行态语义色用项目 nexus-success（与服务卡片状态点同源，避免双绿不一致） */}
      <span className="w-[7px] h-[7px] rounded-full bg-nexus-success flex-shrink-0"/>
      <span className="text-[13px] text-[#c9d1d9] font-medium truncate">{serviceName}</span>
      {/* 状态标签：随运行状态变化（原为硬编码"运行中"，服务停止后显示错误状态） */}
      <span className={`text-[11px] px-1.5 py-0.5 rounded-md border flex-shrink-0 ${
        isRunning
          ? 'bg-nexus-success/15 text-nexus-success border-nexus-success/30'
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
          onKeyDown={e => { if (isSubmitEnter(e)) { e.preventDefault(); onGoMatch(e.shiftKey ? -1 : 1); } }}/>
        {searchActive && (
          <>
            <span className="text-[11px] text-[#8b949e] font-mono tabular-nums w-[32px] text-right">{searchMatches > 0 ? `${Math.min(searchIdx + 1, searchMatches)}/${searchMatches}` : '0/0'}</span>
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
