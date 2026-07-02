import { useState, useEffect, useCallback, memo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { showNotification } from '../ui/Toast';
import { useEditorStore, loadAndOpenFile } from '../../stores/editor';
import { FolderClosed, FolderOpen, getIconSvg } from './FileIcons';
import { Chevron } from '../ui/Chevron';
import { SvgIcon } from '../ui/SvgIcon';
import type { FileEntry } from '../../types/file';

const getExtension = (name: string) => (name.split('.').pop() ?? '').toLowerCase();
const INDENT_STEP = 14;
const BASE_PADDING = 18;
/** 目录展开时初始渲染条数，超出后显示"加载更多" */
const INITIAL_RENDER_LIMIT = 200;

/* ---- Entry ---- */
interface EntryProps {
  e: FileEntry;
  /** 预计算的左侧缩进像素值 */
  indentPx: number;
  selectedPath: string | null;
  onSelect: (path: string) => void;
  /** 子级缩进像素值（indentPx + INDENT_STEP） */
  childIndentPx: number;
}

const Entry = memo(function Entry({ e, indentPx, selectedPath, onSelect, childIndentPx }: EntryProps) {
  const [open, setOpen] = useState(false);
  const [kids, setKids] = useState<FileEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [hover, setHover] = useState(false);
  const [showAll, setShowAll] = useState(false);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);
  const activeTab = useEditorStore(s => s.tabs.find(t => t.id === s.activeTabId));
  const active = activeTab?.path === e.path;
  const selected = selectedPath === e.path;

  const toggle = useCallback(async () => {
    if (!e.is_dir) return;
    if (open) { setOpen(false); return; }
    setLoading(true);
    try { setKids(await invoke<FileEntry[]>('list_directory', { path: e.path })); setOpen(true); }
    catch (err) { console.error('展开目录失败:', err); showNotification({ variant: 'error', title: '展开目录失败' }); } finally { setLoading(false); }
  }, [e, open]);

  const go = () => {
    onSelect(e.path);
    if (e.is_dir) { toggle(); } else { loadAndOpenFile(e.path, e.name); }
  };

  // 在资源管理器中打开
  const handleOpenInExplorer = async () => {
    setContextMenu(null);
    try {
      await invoke('open_in_explorer', { path: e.path });
    } catch (err) {
      console.error('打开资源管理器失败:', err);
      showNotification({ variant: 'error', title: '打开资源管理器失败' });
    }
  };

  // 复制路径
  const handleCopyPath = async () => {
    setContextMenu(null);
    try {
      await navigator.clipboard.writeText(e.path);
      showNotification({ variant: 'success', title: '路径已复制' });
    } catch (err) {
      console.error('复制路径失败:', err);
    }
  };

  // 复制文件名
  const handleCopyName = async () => {
    setContextMenu(null);
    try {
      await navigator.clipboard.writeText(e.name);
      showNotification({ variant: 'success', title: '文件名已复制' });
    } catch (err) {
      console.error('复制文件名失败:', err);
    }
  };

  // 右键菜单
  const handleContextMenu = (ev: React.MouseEvent) => {
    ev.preventDefault();
    ev.stopPropagation();
    setContextMenu({ x: ev.clientX, y: ev.clientY });
  };

  // 关闭右键菜单
  useEffect(() => {
    if (!contextMenu) return;
    const handleClose = () => setContextMenu(null);
    document.addEventListener('click', handleClose);
    return () => document.removeEventListener('click', handleClose);
  }, [contextMenu]);

  const iconSvg = e.is_dir
    ? (open ? FolderOpen : FolderClosed)
    : getIconSvg(getExtension(e.name));

  return (
    <div>
      <div
        className={`relative z-10 flex items-center h-[28px] cursor-pointer gap-1 ${
          active || selected
            ? 'bg-nexus-selected text-nexus-text'
            : 'text-nexus-text-muted hover:bg-nexus-hover'
        }`}
        style={{ paddingLeft: `${indentPx}px`, paddingRight: 10 }}
        onClick={go}
        onContextMenu={handleContextMenu}
        onMouseEnter={() => setHover(true)}
        onMouseLeave={() => setHover(false)}
      >
        {e.is_dir && (open || hover) && <Chevron open={open} />}
        {(!e.is_dir || (!open && !hover)) && <span className="w-[10px] flex-shrink-0" />}

        <SvgIcon
          svg={iconSvg}
          className={`flex-shrink-0 flex items-center justify-center ${e.is_dir ? 'text-nexus-muted' : 'text-nexus-text-muted'}`}
          style={{ width: 16, height: 16 }}
        />

        <span className="truncate text-[13px] ml-1">
          {e.name}
        </span>
      </div>

      {/* 右键菜单 */}
      {contextMenu && (
        <div
          className="fixed z-[70] w-[180px] bg-nexus-surface border border-nexus-border/60 rounded-lg shadow-2xl overflow-hidden"
          style={{
            left: Math.min(contextMenu.x, window.innerWidth - 188),
            top: Math.min(contextMenu.y, window.innerHeight - 200),
          }}
        >
          <div className="py-1.5 px-1.5">
            <button
              className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-md hover:bg-nexus-accent/10 transition-colors group text-left"
              onClick={handleOpenInExplorer}
            >
              <div className="w-5 h-5 rounded bg-nexus-bg border border-nexus-border/30 flex items-center justify-center flex-shrink-0 group-hover:border-nexus-accent/30">
                <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" className="text-nexus-muted group-hover:text-nexus-accent">
                  <path d="M1.5 3h2l1-1.5h4a1 1 0 011 1v5.5a1 1 0 01-1 1h-7a1 1 0 01-1-1V3z"/>
                </svg>
              </div>
              <span className="text-[12px] text-nexus-text">在资源管理器中打开</span>
            </button>

            <button
              className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-md hover:bg-nexus-accent/10 transition-colors group text-left"
              onClick={handleCopyPath}
            >
              <div className="w-5 h-5 rounded bg-nexus-bg border border-nexus-border/30 flex items-center justify-center flex-shrink-0 group-hover:border-nexus-accent/30">
                <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" className="text-nexus-muted group-hover:text-nexus-accent">
                  <rect x="3" y="3" width="5" height="5.5" rx=".8"/>
                  <path d="M2 2.5v4.5h.5V3.5h4V2.5H3a.5.5 0 00-.5.5z"/>
                </svg>
              </div>
              <span className="text-[12px] text-nexus-text">复制路径</span>
            </button>

            <button
              className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-md hover:bg-nexus-accent/10 transition-colors group text-left"
              onClick={handleCopyName}
            >
              <div className="w-5 h-5 rounded bg-nexus-bg border border-nexus-border/30 flex items-center justify-center flex-shrink-0 group-hover:border-nexus-accent/30">
                <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" className="text-nexus-muted group-hover:text-nexus-accent">
                  <rect x="1" y="2" width="8" height="6" rx="1"/>
                  <path d="M3 4h4M3 6h2"/>
                </svg>
              </div>
              <span className="text-[12px] text-nexus-text">复制文件名</span>
            </button>
          </div>
        </div>
      )}

      {open && e.is_dir && (
        <>
          {loading && (
            <div className="text-[11px] text-nexus-muted py-0.5 relative z-10" style={{ paddingLeft: `${childIndentPx + 20}px` }}>…</div>
          )}
          {!loading && (showAll ? kids : kids.slice(0, INITIAL_RENDER_LIMIT)).map(k => (
            <Entry key={k.path} e={k} indentPx={childIndentPx} selectedPath={selectedPath} onSelect={onSelect} childIndentPx={childIndentPx + INDENT_STEP} />
          ))}
          {!loading && !showAll && kids.length > INITIAL_RENDER_LIMIT && (
            <div
              className="text-[11px] text-nexus-muted py-0.5 cursor-pointer hover:text-nexus-text relative z-10"
              style={{ paddingLeft: `${childIndentPx + 20}px` }}
              onClick={(ev) => { ev.stopPropagation(); setShowAll(true); }}
            >
              还有 {kids.length - INITIAL_RENDER_LIMIT} 项…
            </div>
          )}
        </>
      )}
    </div>
  );
});

/* ---- root ---- */
export function FileTree({ rootPath, embedded }: {
  rootPath?: string;
  embedded?: boolean;
}) {
  const [entries, setEntries] = useState<FileEntry[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const [selectedPath, setSelectedPath] = useState<string | null>(null);

  useEffect(() => {
    if (!rootPath) { setEntries([]); setErr(null); return; }
    invoke<FileEntry[]>('list_directory', { path: rootPath })
      .then(setEntries)
      .catch((e: unknown) => setErr(String(e)));
  }, [rootPath]);

  const basePadding = embedded ? 4 : BASE_PADDING;

  return (
    <div className={`${embedded ? '' : 'h-full bg-nexus-surface'} flex flex-col select-none`}>
      {!embedded && (
        <div className="flex items-center h-[30px] px-4 text-[11px] font-semibold text-nexus-muted uppercase tracking-wider flex-shrink-0">
          {rootPath ? rootPath.split(/[/\\]/).pop() ?? '资源管理器' : '资源管理器'}
        </div>
      )}

      <div
        className={`overflow-y-auto overflow-x-hidden py-0.5 ${embedded ? '' : 'flex-1'}`}
      >
        {!rootPath && (
          <div className="px-4 py-10 text-center text-[11px] text-nexus-muted">
            <p className="mb-1">没有打开的文件夹</p>
            <p className="text-[10px] opacity-60">文件 → 打开文件夹</p>
          </div>
        )}
        {rootPath && err && (
          <div className="px-4 py-10 text-center text-[11px] text-nexus-error">{err}</div>
        )}
        {rootPath && !err && entries.length === 0 && (
          <div className="px-4 py-10 text-center text-[11px] text-nexus-muted">空目录</div>
        )}
        {!err && entries.map(ent => (
          <Entry key={ent.path} e={ent} indentPx={basePadding} selectedPath={selectedPath} onSelect={setSelectedPath} childIndentPx={basePadding + INDENT_STEP} />
        ))}
      </div>
    </div>
  );
}
