import { getCurrentWindow } from '@tauri-apps/api/window';
import { open as openUrl } from '@tauri-apps/plugin-shell';
import { useCallback, useEffect, useState, useRef } from 'react';

interface TitleBarProps {
  projectName?: string | null;
}

export function TitleBar({ projectName }: TitleBarProps) {
  const appWindow = getCurrentWindow();
  const [isMaximized, setIsMaximized] = useState(false);

  useEffect(() => {
    appWindow.isMaximized().then(setIsMaximized);
    const u = appWindow.onResized(() => { appWindow.isMaximized().then(setIsMaximized); });
    return () => { u.then(fn => fn()); };
  }, [appWindow]);

  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    if ((e.target as HTMLElement).closest('button, [data-menu]')) return;
    appWindow.startDragging();
  }, [appWindow]);

  return (
    <div className="h-[32px] bg-nexus-titlebar flex items-center select-none flex-shrink-0" onMouseDown={handleMouseDown}>
      {/* ── 帮助菜单 ── */}
      <HelpMenu />

      {/* ── 项目名 ── */}
      <div className="flex-1 flex items-center justify-center h-full">
        <span className="text-[12px] text-nexus-text-muted">
          {projectName || ''}
        </span>
      </div>

      {/* ── 窗口控制 ── */}
      <div className="flex h-full">
        <button className="w-[46px] h-full flex items-center justify-center text-nexus-muted hover:bg-nexus-hover hover:text-nexus-text" onClick={() => appWindow.minimize()}>
          <svg width="10" height="1" viewBox="0 0 10 1" fill="currentColor"><rect width="10" height="1"/></svg>
        </button>
        <button className="w-[46px] h-full flex items-center justify-center text-nexus-muted hover:bg-nexus-hover hover:text-nexus-text" onClick={() => appWindow.toggleMaximize()} title={isMaximized ? "还原" : "最大化"}>
          {isMaximized ? (
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1"><rect x="2" y="0" width="7" height="7"/><polyline points="0,3 0,10 7,10"/></svg>
          ) : (
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1"><rect x="0.5" y="0.5" width="9" height="9"/></svg>
          )}
        </button>
        <button className="w-[46px] h-full flex items-center justify-center text-nexus-muted hover:bg-nexus-error hover:text-white" onClick={() => appWindow.close()}>
          <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2"><line x1="1" y1="1" x2="9" y2="9"/><line x1="9" y1="1" x2="1" y2="9"/></svg>
        </button>
      </div>
    </div>
  );
}

// ─── 帮助下拉菜单 ─────────────────────────────────────────

function HelpMenu() {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  }, [open]);

  return (
    <div className="relative h-full" ref={ref}>
      <button
        className={`h-full px-2.5 text-[12.5px] transition-colors ${
          open ? 'bg-nexus-hover text-nexus-text' : 'text-nexus-text-muted hover:text-nexus-text hover:bg-nexus-hover/40'
        }`}
        onClick={(e) => { e.stopPropagation(); setOpen(!open); }}
      >
        帮助
      </button>
      {open && (
        <div className="absolute top-full left-0 mt-0.5 min-w-[160px] bg-nexus-surface border border-nexus-border rounded-md shadow-xl py-1 z-50">
          <button
            className="w-full px-3 py-1.5 text-[12px] text-nexus-text hover:bg-nexus-accent hover:text-white text-left transition-colors"
            onClick={async (e) => {
              e.stopPropagation();
              setOpen(false);
              try {
                await openUrl('https://github.com/hixingchen/nexus');
              } catch (err) {
                console.error('打开网页失败:', err);
              }
            }}
          >
            关于 Nexus
          </button>
        </div>
      )}
    </div>
  );
}
