import { useState, useEffect, useRef } from 'react';
import { createPortal } from 'react-dom';
import { invoke } from '@tauri-apps/api/core';
import { type Project, type Service } from '../../services/service';
import { FileTree } from '../file-tree/FileTree';
import { useSearchModalStore } from '../../stores/searchModal';
import { showNotification } from '../ui/Toast';

interface Props {
  project: Project;
  selected: boolean;
  isExpanded: boolean;
  services: Service[];
  isRunning: boolean;
  actingId: string | null;
  expandedSvc: Set<string>;
  onSelect: () => void;
  onDoubleClick: (e: React.MouseEvent) => void;
  onContextMenu: (e: React.MouseEvent) => void;
  onStart: (e: React.MouseEvent) => void;
  onStop: (e: React.MouseEvent) => void;
  onTogglePin: (e: React.MouseEvent) => void;
  onToggleSvcExpand: (serviceId: string) => void;
}

export function ProjectListItem({
  project, selected, isExpanded, services, isRunning, actingId, expandedSvc,
  onSelect, onDoubleClick, onContextMenu, onStart, onStop, onTogglePin, onToggleSvcExpand,
}: Props) {
  const showTreeServices = services.filter(s => s.show_file_tree && s.cwd);
  const openSearch = useSearchModalStore(s => s.openSearch);

  // 服务行右键菜单（搜索文件内容）
  const [svcMenu, setSvcMenu] = useState<{ x: number; y: number; svc: Service } | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!svcMenu) return;
    const handleClose = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) setSvcMenu(null);
    };
    document.addEventListener('mousedown', handleClose);
    return () => document.removeEventListener('mousedown', handleClose);
  }, [svcMenu]);

  return (
    <>
    <div className="mb-1.5">
      <div
        className={`mx-2 rounded-md px-3 py-2.5 cursor-pointer group ${
          selected
            ? 'bg-nexus-accent/10 border border-nexus-accent/30'
            : 'bg-nexus-bg/30 border border-nexus-border hover:bg-nexus-hover hover:border-nexus-muted'
        }`}
        onClick={onSelect}
        onDoubleClick={onDoubleClick}
        onContextMenu={onContextMenu}
      >
        <div className="flex items-start gap-2">
          {/* 文件夹图标 */}
          <span className="flex-shrink-0 mt-px text-nexus-muted/60 group-hover:text-nexus-muted">
            <svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.2">
              <path d="M2 3.5a1 1 0 011-1h3l1.5 1.5H12a1 1 0 011 1v5a1 1 0 01-1 1H3a1 1 0 01-1-1V3.5z"/>
            </svg>
          </span>
          <div className="min-w-0 flex-1">
            <span className="text-[13px] text-nexus-text font-medium truncate block">{project.name}</span>
            {project.path && (
              <span className="text-[11px] text-nexus-muted truncate block mt-0.5 font-mono">
                {project.path}
              </span>
            )}
          </div>
          {/* 启动/停止按钮 */}
          {isRunning ? (
            <button
              className="flex-shrink-0 p-1 rounded text-nexus-error/70 hover:text-nexus-error hover:bg-nexus-error/10 disabled:opacity-30"
              disabled={actingId === project.id}
              onClick={onStop}
              title="停止"
            >
              <svg width="14" height="14" viewBox="0 0 12 12" fill="currentColor"><rect x="2" y="2" width="8" height="8" rx="1"/></svg>
            </button>
          ) : (
            <button
              className="flex-shrink-0 p-1 rounded text-nexus-success/70 hover:text-nexus-success hover:bg-nexus-success/10 disabled:opacity-30"
              disabled={actingId === project.id}
              onClick={onStart}
              title="启动"
            >
              {actingId === project.id ? (
                <svg width="14" height="14" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.5"><circle cx="6" cy="6" r="4"/></svg>
              ) : (
                <svg width="14" height="14" viewBox="0 0 12 12" fill="currentColor"><polygon points="3,1.5 10.5,6 3,10.5"/></svg>
              )}
            </button>
          )}

          {/* 置顶按钮 */}
          <button
            className={`flex-shrink-0 p-1 rounded transition-all ${
              project.pinned
                ? 'text-nexus-accent bg-nexus-accent/10'
                : 'text-nexus-muted/50 hover:text-nexus-accent hover:bg-nexus-accent/10'
            }`}
            onClick={onTogglePin}
            title={project.pinned ? '取消置顶' : '置顶'}
          >
            <svg width="14" height="14" viewBox="0 0 12 12" fill={project.pinned ? 'currentColor' : 'none'} stroke="currentColor" strokeWidth="1.5" strokeLinejoin="round">
              <path d="M7.5 1.5L10.5 4.5 8 7l1.5 4-7-7L7 2.5l.5-1z"/>
            </svg>
          </button>
        </div>
      </div>
      {/* 展开：服务列表 + 可选展开的目录树 */}
      {isExpanded && (
        <div className="border-t border-nexus-border/50 mt-1 mx-2 bg-nexus-bg/20 rounded-b-md">
          {showTreeServices.length === 0 ? (
            <div className="py-3 px-3 text-[11px] text-nexus-muted/50 text-center">
              暂无开启目录树的服务
            </div>
          ) : (
            showTreeServices.map(s => {
              const svcKey = `${project.id}:${s.id}`;
              const svcExpanded = expandedSvc.has(svcKey);
              return (
                <div key={s.id}>
                  <div
                    className={`flex items-center gap-2 px-3 py-1.5 cursor-pointer transition-colors ${
                      svcExpanded
                        ? 'text-nexus-text bg-nexus-bg/40'
                        : 'text-nexus-text-muted hover:bg-nexus-hover/30 hover:text-nexus-text'
                    }`}
                    onClick={() => onToggleSvcExpand(s.id)}
                    onContextMenu={(e) => {
                      e.preventDefault();
                      e.stopPropagation();
                      setSvcMenu({ x: e.clientX, y: e.clientY, svc: s });
                    }}
                    title="右键可搜索文件内容"
                  >
                    <svg
                      className={`flex-shrink-0 text-nexus-muted/60 transition-transform ${svcExpanded ? 'rotate-90' : ''}`}
                      width="10" height="10" viewBox="0 0 10 10" fill="none"
                      stroke="currentColor" strokeWidth="1.3" strokeLinecap="round"
                    >
                      <polyline points="3,1 7,5 3,9" />
                    </svg>
                    <span className="text-[13px] truncate">{s.name}</span>
                  </div>
                  {svcExpanded && (
                    <div className="ml-[11px] pl-2 border-l border-nexus-border/30">
                      <FileTree rootPath={s.cwd} embedded />
                    </div>
                  )}
                </div>
              );
            })
          )}
        </div>
      )}
    </div>

    {/* 服务行右键菜单：搜索文件内容 / 资源管理器 / 复制 */}
    {svcMenu && createPortal(
      <div
        ref={menuRef}
        className="fixed z-[70] w-[180px] bg-nexus-surface border border-nexus-border/60 rounded-lg shadow-2xl overflow-hidden"
        style={{
          left: Math.min(svcMenu.x, window.innerWidth - 188),
          top: Math.min(svcMenu.y, window.innerHeight - 200),
        }}
      >
        {/* 搜索文件内容 */}
        <div className="py-1.5 px-1.5">
          <button
            className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-md hover:bg-nexus-accent/10 transition-colors group text-left"
            onClick={() => {
              openSearch(svcMenu.svc.cwd, svcMenu.svc.name);
              setSvcMenu(null);
            }}
          >
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" className="text-nexus-muted group-hover:text-nexus-accent flex-shrink-0">
              <circle cx="4.2" cy="4.2" r="3"/><line x1="6.5" y1="6.5" x2="8.8" y2="8.8"/>
            </svg>
            <span className="text-[12px] text-nexus-text">搜索文件内容</span>
          </button>
        </div>

        {/* 在资源管理器中打开 / 复制路径 / 复制文件名 */}
        <div className="border-t border-nexus-border/30 py-1.5 px-1.5">
          <button
            className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-md hover:bg-nexus-accent/10 transition-colors group text-left"
            onClick={async () => {
              const { cwd } = svcMenu.svc;
              setSvcMenu(null);
              try {
                await invoke('open_in_explorer', { path: cwd });
              } catch (err) {
                console.error('打开资源管理器失败:', err);
                showNotification({ variant: 'error', title: '打开资源管理器失败' });
              }
            }}
          >
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" className="text-nexus-muted group-hover:text-nexus-accent flex-shrink-0">
              <path d="M1.5 3h2l1-1.5h4a1 1 0 011 1v5.5a1 1 0 01-1 1h-7a1 1 0 01-1-1V3z"/>
            </svg>
            <span className="text-[12px] text-nexus-text">在资源管理器中打开</span>
          </button>
          <button
            className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-md hover:bg-nexus-accent/10 transition-colors group text-left"
            onClick={async () => {
              const { cwd } = svcMenu.svc;
              setSvcMenu(null);
              try {
                await navigator.clipboard.writeText(cwd);
                showNotification({ variant: 'success', title: '路径已复制' });
              } catch (err) {
                console.error('复制路径失败:', err);
              }
            }}
          >
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" className="text-nexus-muted group-hover:text-nexus-accent flex-shrink-0">
              <rect x="3" y="3" width="5" height="5.5" rx=".8"/>
              <path d="M2 2.5v4.5h.5V3.5h4V2.5H3a.5.5 0 00-.5.5z"/>
            </svg>
            <span className="text-[12px] text-nexus-text">复制路径</span>
          </button>
          <button
            className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-md hover:bg-nexus-accent/10 transition-colors group text-left"
            onClick={async () => {
              const { name } = svcMenu.svc;
              setSvcMenu(null);
              try {
                await navigator.clipboard.writeText(name);
                showNotification({ variant: 'success', title: '文件名已复制' });
              } catch (err) {
                console.error('复制文件名失败:', err);
              }
            }}
          >
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" className="text-nexus-muted group-hover:text-nexus-accent flex-shrink-0">
              <rect x="1" y="2" width="8" height="6" rx="1"/>
              <path d="M3 4h4M3 6h2"/>
            </svg>
            <span className="text-[12px] text-nexus-text">复制文件名</span>
          </button>
        </div>
      </div>,
      document.body,
    )}
    </>
  );
}
