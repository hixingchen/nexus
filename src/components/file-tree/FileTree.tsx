import { useState, useEffect, useCallback, useRef, memo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { showNotification } from '../ui/Toast';
import { useEditorStore, loadAndOpenFile } from '../../stores/editor';
import { useSearchModalStore } from '../../stores/searchModal';
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
  /** 定位目标路径（标签右键「在目录树中定位」触发）：目录自动展开链，文件滚动高亮 */
  revealPath: string | null;
  /** 定位触发信号：每次请求 +1（同一路径重复点击也生效） */
  revealSeq: number;
  onSelect: (path: string) => void;
  /** 子级缩进像素值（indentPx + INDENT_STEP） */
  childIndentPx: number;
}

const Entry = memo(function Entry({ e, indentPx, selectedPath, revealPath, revealSeq, onSelect, childIndentPx }: EntryProps) {
  const [open, setOpen] = useState(false);
  const [kids, setKids] = useState<FileEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [hover, setHover] = useState(false);
  const [showAll, setShowAll] = useState(false);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);
  /** 展开请求序号：加载中折叠后丢弃过期响应，避免目录被意外重新展开 */
  const toggleSeqRef = useRef(0);
  const entryRef = useRef<HTMLDivElement | null>(null);
  const selected = selectedPath === e.path;
  /** 定位目标文件（滚动 + 高亮） */
  const reveal = revealPath === e.path;

  const toggle = useCallback(async () => {
    if (!e.is_dir) return;
    if (open) { setOpen(false); return; }
    const seq = ++toggleSeqRef.current;
    setLoading(true);
    try {
      const list = await invoke<FileEntry[]>('list_directory', { path: e.path });
      if (seq !== toggleSeqRef.current) return; // 期间用户已折叠，丢弃结果
      setKids(list);
      setOpen(true);
    } catch (err) {
      if (seq !== toggleSeqRef.current) return;
      console.error('展开目录失败:', err);
      showNotification({ variant: 'error', title: '展开目录失败' });
    } finally {
      if (seq === toggleSeqRef.current) setLoading(false);
    }
  }, [e, open]);

  const go = () => {
    onSelect(e.path);
    if (e.is_dir) { toggle(); } else { loadAndOpenFile(e.path, e.name); }
  };

  // 定位请求（点击标签栏定位图标）：revealPath 在此目录下 → 自动加载并展开。
  // 触发信号用 revealSeq（同一路径重复点击也生效）；依赖不含 open——用户手动收起后不会被重新展开
  useEffect(() => {
    if (!e.is_dir || !revealPath || open) return;
    if (!revealPath.startsWith(e.path + '/')) return;
    const seq = ++toggleSeqRef.current;
    setLoading(true);
    invoke<FileEntry[]>('list_directory', { path: e.path })
      .then(list => { if (seq === toggleSeqRef.current) { setKids(list); setOpen(true); } })
      .catch((err: unknown) => {
        if (seq !== toggleSeqRef.current) return;
        console.error('展开目录失败:', err);
      })
      .finally(() => { if (seq === toggleSeqRef.current) setLoading(false); });
    // eslint-disable-next-line react-hooks/exhaustive-deps -- open 故意不入依赖（见上方注释）
  }, [revealSeq, e.path, e.is_dir]);

  // 定位到目标文件：滚动使其尽量居中（上下留出上下文）。
  // 同步选中态：避免与之前点击选中的文件残留两个高亮
  useEffect(() => {
    if (!reveal) return;
    entryRef.current?.scrollIntoView({ block: 'center' });
    onSelect(e.path);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- 触发信号用 revealSeq（reveal 由 revealPath 推导）
  }, [revealSeq]);

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

  // 搜索文件内容（目录 → 搜该目录；文件 → 搜该文件自身）
  const handleOpenSearch = () => {
    setContextMenu(null);
    useSearchModalStore.getState().openSearch(e.path, e.name);
  };

  // 右键菜单
  const handleContextMenu = (ev: React.MouseEvent) => {
    ev.preventDefault();
    ev.stopPropagation();
    setContextMenu({ x: ev.clientX, y: ev.clientY });
  };

  // 关闭右键菜单。
  // 用 mousedown 而非 click：右键新节点触发的是 contextmenu（不触发 click），
  // click 会导致旧菜单残留（同一时间出现两个菜单）；
  // 点在菜单内部不关闭，否则菜单项点击事件会因菜单先卸载而失效
  const menuRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (!contextMenu) return;
    const handleClose = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setContextMenu(null);
      }
    };
    document.addEventListener('mousedown', handleClose);
    return () => document.removeEventListener('mousedown', handleClose);
  }, [contextMenu]);

  const iconSvg = e.is_dir
    ? (open ? FolderOpen : FolderClosed)
    : getIconSvg(getExtension(e.name));

  return (
    <div>
      <div
        ref={entryRef}
        className={`relative z-10 flex items-center h-[28px] cursor-pointer gap-1 ${
          selected || reveal
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
          ref={menuRef}
          className="fixed z-[70] w-[180px] bg-nexus-surface border border-nexus-border/60 rounded-lg shadow-2xl overflow-hidden"
          style={{
            left: Math.min(contextMenu.x, window.innerWidth - 188),
            top: Math.min(contextMenu.y, window.innerHeight - 200),
          }}
        >
          {/* 搜索文件内容（主操作，独立一组） */}
          <div className="py-1.5 px-1.5">
            <button
              className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-md hover:bg-nexus-accent/10 transition-colors group text-left"
              onClick={handleOpenSearch}
            >
              <div className="w-5 h-5 rounded bg-nexus-bg border border-nexus-border/30 flex items-center justify-center flex-shrink-0 group-hover:border-nexus-accent/30">
                <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" className="text-nexus-muted group-hover:text-nexus-accent">
                  <circle cx="4.2" cy="4.2" r="3"/><line x1="6.5" y1="6.5" x2="8.8" y2="8.8"/>
                </svg>
              </div>
              <span className="text-[12px] text-nexus-text">搜索文件内容</span>
            </button>
          </div>

          {/* 在资源管理器中打开 / 复制路径 / 复制文件名 */}
          <div className="border-t border-nexus-border/30 py-1.5 px-1.5">
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
            <Entry key={k.path} e={k} indentPx={childIndentPx} selectedPath={selectedPath} revealPath={revealPath} revealSeq={revealSeq} onSelect={onSelect} childIndentPx={childIndentPx + INDENT_STEP} />
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
  /** 活动标签路径：打开/切换文件时同步为树选中态（保持单一高亮） */
  const activeTabPath = useEditorStore(s => s.tabs.find(t => t.id === s.activeTabId)?.path ?? null);
  /** 定位请求（标签右键「在目录树中定位」触发）：路径 + 信号 */
  const revealPath = useEditorStore(s => s.revealPath);
  const revealSeq = useEditorStore(s => s.revealSeq);

  // 打开/切换文件 → 树选中态跟随 + 清除定位标记（定位高亮是临时的，
  // 用户切换文件后以当前选中文件为主，避免两个高亮）
  useEffect(() => {
    if (activeTabPath) {
      setSelectedPath(activeTabPath);
      useEditorStore.getState().clearReveal();
    }
  }, [activeTabPath]);

  // 用户手动点击树节点：同样以新选中为主，清除定位标记
  const handleSelect = (path: string) => {
    setSelectedPath(path);
    useEditorStore.getState().clearReveal();
  };
  /** 根目录加载序号：快速切换目录时丢弃旧响应，避免显示错目录内容 */
  const rootSeqRef = useRef(0);

  useEffect(() => {
    if (!rootPath) { setEntries([]); setErr(null); return; }
    const seq = ++rootSeqRef.current;
    invoke<FileEntry[]>('list_directory', { path: rootPath })
      .then(list => { if (seq === rootSeqRef.current) setEntries(list); })
      .catch((e: unknown) => { if (seq === rootSeqRef.current) setErr(String(e)); });
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
          <Entry key={ent.path} e={ent} indentPx={basePadding} selectedPath={selectedPath} revealPath={revealPath} revealSeq={revealSeq} onSelect={handleSelect} childIndentPx={basePadding + INDENT_STEP} />
        ))}
      </div>
    </div>
  );
}
