import { useEffect, useRef, useCallback, useState, useMemo } from 'react';
import { useLogStore } from '../../stores/logStore';
import { logService } from '../../services/logService';
import { renderLine } from '../../utils/logFormatter';
import type { ServiceLogLine } from '../../services/logService';

interface LogViewerProps { serviceKey: string; maxHeight?: string; fill?: boolean; onClose?: () => void; }

const RENDER_CAP = 2000;

export function LogViewer({ serviceKey, maxHeight, fill, onClose }: LogViewerProps) {
  const preRef = useRef<HTMLPreElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const lastVersionRef = useRef(-1);
  const renderedCountRef = useRef(0);
  const pausedRef = useRef(false);

  const [paused, setPaused] = useState(false);
  const [searchTerm, setSearchTerm] = useState('');
  const [searchIdx, setSearchIdx] = useState(0);

  const lines = useLogStore((s) => s.logs[serviceKey]) ?? [];
  const version = useLogStore((s) => s.version[serviceKey]) ?? 0;

  // ── 搜索 ──────────────────────────────────────────────────

  const searchActive = searchTerm.trim().length > 0;
  const filteredLines = useMemo(() => {
    if (!searchActive) return null;
    return lines.filter(l => l.text.toLowerCase().includes(searchTerm.toLowerCase()));
  }, [lines, searchActive, searchTerm]);
  const searchMatches = filteredLines?.length ?? 0;

  // ── 切换服务：重置 ────────────────────────────────────────

  useEffect(() => {
    lastVersionRef.current = -1;
    renderedCountRef.current = 0;
    if (preRef.current) preRef.current.innerHTML = '';
    if (scrollRef.current) scrollRef.current.scrollTop = 0;
    const existing = useLogStore.getState().logs[serviceKey];
    if (!existing || existing.length === 0) {
      logService.getServiceLogs(serviceKey).then(
        (lines) => {
          if (lines.length > 0) useLogStore.getState().setLogs(serviceKey, lines);
        },
        () => { /* 日志为空或服务已停止，静默处理 */ }
      );
    }
  }, [serviceKey]);

  // ── DOM 渲染：version 检测 + 增量追加 ─────────────────────

  useEffect(() => {
    const pre = preRef.current;
    if (!pre) return;

    if (searchActive) {
      renderSearchResults(pre, filteredLines, searchTerm, lastVersionRef, version, renderedCountRef);
      return;
    }

    if (version === lastVersionRef.current && lastVersionRef.current >= 0) return;

    const prevVer = lastVersionRef.current;
    lastVersionRef.current = version;

    const needFull = prevVer < 0
      || lines.length <= renderedCountRef.current
      || lines.length >= RENDER_CAP;

    if (needFull) {
      renderFull(pre, lines, renderedCountRef);
    } else {
      renderIncremental(pre, lines, renderedCountRef);
    }

    scrollToBottom(pausedRef, scrollRef);
  }, [lines, version, searchActive, filteredLines, searchTerm]);

  // 退出搜索 → 触发全量重建
  useEffect(() => { if (!searchActive) { lastVersionRef.current = -1; renderedCountRef.current = 0; } }, [searchActive]);

  // ── 暂停 ──────────────────────────────────────────────────

  const handlePause = useCallback(() => {
    setPaused(prev => {
      const next = !prev;
      pausedRef.current = next;
      if (!next && scrollRef.current) scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
      return next;
    });
  }, []);

  // ── 搜索导航 ──────────────────────────────────────────────

  const goMatch = useCallback((dir: 1 | -1) => {
    if (!filteredLines || filteredLines.length === 0) return;
    const next = ((searchIdx + dir) % filteredLines.length + filteredLines.length) % filteredLines.length;
    setSearchIdx(next);
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

  const serviceName = serviceKey.split(':').pop() ?? serviceKey;

  return (
    <div className={`flex flex-col ${fill ? 'h-full' : ''}`}>
      <LogHeader
        serviceName={serviceName}
        lineCount={lines.length}
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
        onClear={() => {
          useLogStore.getState().clearLogs(serviceKey);
          if (preRef.current) preRef.current.innerHTML = '';
          lastVersionRef.current = -1;
        }}
      />

      <div ref={scrollRef} className={`overflow-auto bg-[#0d1117] ${fill ? 'flex-1' : ''}`}
        style={{ ...(fill ? {} : { maxHeight: maxHeight ?? '220px' }), scrollbarWidth: 'none' }}>
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
  version: number,
  renderedCountRef: React.MutableRefObject<number>,
) {
  if (!filteredLines) return;
  pre.innerHTML = filteredLines.map(l => renderLine(l.text, searchTerm)).join('\n');
  lastVersionRef.current = version;
  renderedCountRef.current = 0;
}

const BATCH_SIZE = 300;

function renderFull(
  pre: HTMLPreElement,
  lines: ServiceLogLine[],
  renderedCountRef: React.MutableRefObject<number>,
) {
  const display = lines.length > RENDER_CAP ? lines.slice(-RENDER_CAP) : lines;
  // 分批渲染，避免一次性插入大量 HTML 导致 UI 卡顿
  pre.innerHTML = '';
  renderBatches(pre, display, 0);
  renderedCountRef.current = lines.length;
}

function renderBatches(pre: HTMLPreElement, lines: ServiceLogLine[], startIdx: number) {
  const end = Math.min(startIdx + BATCH_SIZE, lines.length);
  const batch = lines.slice(startIdx, end).map(l => renderLine(l.text, '')).join('\n');
  pre.insertAdjacentHTML('beforeend', (startIdx > 0 ? '\n' : '') + batch);
  if (end < lines.length) {
    requestAnimationFrame(() => renderBatches(pre, lines, end));
  }
}

function renderIncremental(
  pre: HTMLPreElement,
  lines: ServiceLogLine[],
  renderedCountRef: React.MutableRefObject<number>,
) {
  const newLines = lines.slice(renderedCountRef.current);
  if (newLines.length > 0) {
    const html = newLines.map(l => renderLine(l.text, '')).join('\n');
    pre.insertAdjacentHTML('beforeend', '\n' + html);
    renderedCountRef.current = lines.length;
  }
}

function scrollToBottom(
  pausedRef: React.MutableRefObject<boolean>,
  scrollRef: React.RefObject<HTMLDivElement | null>,
) {
  if (!pausedRef.current && scrollRef.current) {
    requestAnimationFrame(() => {
      if (scrollRef.current) scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    });
  }
}

// ── 头部组件 ──────────────────────────────────────────────

function LogHeader({
  serviceName, lineCount, onClose,
  searchTerm, setSearchTerm, setSearchIdx, searchRef,
  searchActive, searchMatches, searchIdx, onGoMatch,
  paused, onPause, onClear,
}: {
  serviceName: string;
  lineCount: number;
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
      <span className="text-[11px] px-1.5 py-0.5 rounded-md bg-emerald-400/10 text-emerald-400 border border-emerald-400/20 flex-shrink-0">运行中</span>
      <span className="text-[12px] text-[#8b949e] flex-shrink-0">{lineCount.toLocaleString()} 行</span>
      <div className="flex-1"/>

      <div className="flex items-center gap-1.5 bg-[#0d1117] rounded-lg px-2.5 h-[30px] border border-[#30363d] focus-within:border-[#58a6ff] transition-colors">
        <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="#8b949e" strokeWidth="1.5"><circle cx="6.5" cy="6.5" r="4.5"/><path d="M10 10l4 4"/></svg>
        <input ref={searchRef as React.Ref<HTMLInputElement>} className="w-[130px] bg-transparent text-[12px] text-[#c9d1d9] outline-none placeholder:text-[#484f58] font-mono"
          placeholder="查找…" value={searchTerm}
          onChange={e => { setSearchTerm(e.target.value); setSearchIdx(() => 0); }}
          onKeyDown={e => { if (e.key === 'Enter') { e.preventDefault(); onGoMatch(e.shiftKey ? -1 : 1); } }}/>
        {searchActive && (
          <>
            <span className="text-[11px] text-[#8b949e] font-mono tabular-nums w-[32px] text-right">{searchMatches > 0 ? `${searchIdx + 1}/${searchMatches}` : '0/0'}</span>
            <button className="text-[#8b949e] hover:text-[#c9d1d9] text-[10px]" onClick={() => onGoMatch(-1)}>▲</button>
            <button className="text-[#8b949e] hover:text-[#c9d1d9] text-[10px]" onClick={() => onGoMatch(1)}>▼</button>
          </>
        )}
      </div>

      <button onClick={onPause}
        className={`h-[30px] px-2.5 flex items-center gap-1 rounded-lg text-[11px] border transition-colors flex-shrink-0 ${
          paused ? 'border-[#d29922]/30 text-[#d29922] bg-[#d29922]/10 hover:bg-[#d29922]/20'
                 : 'border-[#30363d] text-[#8b949e] hover:text-[#c9d1d9] hover:bg-white/5'}`}>
        {paused ? '已暂停' : '跟随'}
      </button>

      <button className="h-[30px] px-2.5 flex items-center gap-1 rounded-lg text-[11px] border border-[#30363d] text-[#8b949e] hover:text-[#c9d1d9] hover:bg-white/5 transition-colors flex-shrink-0"
        onClick={onClear}>
        <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round"><path d="M2 4h12M5.3 4V2.7c0-.4.3-.7.7-.7h4c.4 0 .7.3.7.7V4M6.7 7v5M9.3 7v5M3.3 4l.7 9.3c0 .4.3.7.7.7h6.6c.4 0 .7-.3.7-.7L12.7 4"/></svg>清空
      </button>
    </div>
  );
}
