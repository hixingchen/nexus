import { useState, useRef, useEffect, useMemo } from 'react';
import { createPortal } from 'react-dom';
import { invoke } from '@tauri-apps/api/core';
import type { ServiceTemplate } from '../../services/service';
import { showNotification } from '../ui/Toast';

interface Props {
  tpl: ServiceTemplate;
  /** 正在添加中（整列表禁用，防重复添加） */
  busy: boolean;
  /** 是否正在编辑此模板（高亮显示，与服务条目一致） */
  isEditing: boolean;
  /** 点击卡片：打开/关闭模板编辑面板（与服务条目 toggle 行为一致） */
  onEdit: (tpl: ServiceTemplate) => void;
  onAdd: (tpl: ServiceTemplate) => void;
  /** 请求删除（父组件弹确认框，与服务删除流程一致） */
  onRequestDelete: (tpl: ServiceTemplate) => void;
}

/** 模板条目：视觉与服务条目一致（圆点 + 名称 + Hover 操作），点击打开编辑面板，Hover/右键提供「添加到项目 / 删除模板」 */
export function TemplateTreeEntry({ tpl, busy, isEditing, onEdit, onAdd, onRequestDelete }: Props) {
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);

  const handleContextMenu = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setContextMenu({ x: e.clientX, y: e.clientY });
  };

  const handleOpenInExplorer = async () => {
    setContextMenu(null);
    try {
      await invoke('open_in_explorer', { path: tpl.cwd });
    } catch (err) {
      console.error('打开资源管理器失败:', err);
      showNotification({ variant: 'error', title: '打开资源管理器失败' });
    }
  };

  const handleOpenTerminal = async () => {
    setContextMenu(null);
    try {
      await invoke('open_terminal', { path: tpl.cwd });
    } catch (err) {
      console.error('打开终端失败:', err);
      showNotification({ variant: 'error', title: '打开终端失败' });
    }
  };

  return (
    <div className="px-2 py-0.5">
      <div
        className={`cursor-pointer group rounded-md px-3 py-2.5 ${
          isEditing
            ? 'bg-nexus-accent/10 border border-nexus-accent/30'
            : 'bg-nexus-bg/30 border border-nexus-border hover:border-nexus-muted/70'
        } ${busy ? 'opacity-60 pointer-events-none' : ''}`}
        onClick={() => onEdit(tpl)}
        onContextMenu={handleContextMenu}
        title={`${tpl.command}${tpl.cwd ? ` (${tpl.cwd})` : ''}`}
      >
        <div className="flex items-center gap-2">
          {/* 状态圆点（模板无运行态，恒为灰色，与服务条目视觉一致） */}
          <span className="w-[7px] h-[7px] rounded-full flex-shrink-0 bg-nexus-muted/40" />
          <span className="flex-1 text-[13px] text-nexus-text font-medium truncate">{tpl.name}</span>

          {/* Hover 操作：添加到项目（删除在右键菜单） */}
          <div className="flex items-center gap-1 opacity-0 group-hover:opacity-100 flex-shrink-0">
            <button
              className="px-2 py-1 text-[11px] bg-nexus-accent/15 text-nexus-accent rounded hover:bg-nexus-accent/25 font-medium"
              onClick={e => { e.stopPropagation(); onAdd(tpl); }}
              title="添加到当前项目"
            >添加到项目</button>
          </div>
        </div>
      </div>

      {/* 右键菜单。
          Portal 到 body：服务面板有 transform 容器（折叠动画），fixed 定位的菜单
          若留在其内会以 transform 容器为包含块，导致坐标错位被裁剪而不可见 */}
      {contextMenu && createPortal(
        <TemplateContextMenu
          x={contextMenu.x}
          y={contextMenu.y}
          hasCwd={!!tpl.cwd}
          onOpenInExplorer={handleOpenInExplorer}
          onOpenTerminal={handleOpenTerminal}
          onDelete={() => { setContextMenu(null); onRequestDelete(tpl); }}
          onClose={() => setContextMenu(null)}
        />,
        document.body,
      )}
    </div>
  );
}

// ── 右键菜单组件（参考服务条目菜单，工具命令排除：模板不可运行） ──

interface TemplateContextMenuProps {
  x: number;
  y: number;
  hasCwd: boolean;
  onOpenInExplorer: () => void;
  onOpenTerminal: () => void;
  onDelete: () => void;
  onClose: () => void;
}

const TemplateContextMenu = ({ x, y, hasCwd, onOpenInExplorer, onOpenTerminal, onDelete, onClose }: TemplateContextMenuProps) => {
  const menuRef = useRef<HTMLDivElement | null>(null);

  // 点击外部关闭菜单
  useEffect(() => {
    const handleClose = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        onClose();
      }
    };
    document.addEventListener('mousedown', handleClose);
    return () => document.removeEventListener('mousedown', handleClose);
  }, [onClose]);

  // 计算菜单位置，确保不超出屏幕
  const menuStyle = useMemo(() => {
    const itemCount = (hasCwd ? 2 : 0) + 1;
    const menuHeight = itemCount * 34 + 24;
    const maxX = window.innerWidth - 180 - 8;
    const maxY = window.innerHeight - menuHeight - 8;
    return {
      left: Math.min(x, maxX),
      top: Math.min(y, maxY),
    };
  }, [x, y, hasCwd]);

  return (
    <div
      ref={menuRef}
      className="fixed z-[70] w-[180px] bg-nexus-surface border border-nexus-border/60 rounded-lg shadow-2xl overflow-hidden"
      style={menuStyle}
    >
      {/* 打开资源管理器 / 打开终端 */}
      {hasCwd && (
        <div className="py-1.5 px-1.5">
          <button
            className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-md hover:bg-nexus-accent/10 transition-colors group text-left"
            onClick={onOpenInExplorer}
          >
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" className="text-nexus-muted group-hover:text-nexus-accent flex-shrink-0">
              <path d="M1.5 3h2l1-1.5h4a1 1 0 011 1v5.5a1 1 0 01-1 1h-7a1 1 0 01-1-1V3z"/>
            </svg>
            <span className="text-[12px] text-nexus-text">在资源管理器中打开</span>
          </button>
          <button
            className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-md hover:bg-nexus-accent/10 transition-colors group text-left"
            onClick={onOpenTerminal}
          >
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" className="text-nexus-muted group-hover:text-nexus-accent flex-shrink-0">
              <path d="M1.5 2.5l3.5 2.5-3.5 2.5"/><line x1="6.5" y1="8" x2="8.5" y2="8"/>
            </svg>
            <span className="text-[12px] text-nexus-text">打开终端</span>
          </button>
        </div>
      )}

      {/* 删除 */}
      <div className="border-t border-nexus-border/30 py-1.5 px-1.5">
        <button
          className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-md hover:bg-nexus-error/10 transition-colors group text-left"
          onClick={onDelete}
        >
          <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.3" className="text-nexus-muted group-hover:text-nexus-error flex-shrink-0">
            <path d="M2.5 3h5M3.5 3V2a.5.5 0 01.5-.5h2a.5.5 0 01.5.5v1M4 4.5v3M6 4.5v3M3 3l.5 6a1 1 0 001 .5h3a1 1 0 001-.5L9 3"/>
          </svg>
          <span className="text-[12px] text-nexus-muted group-hover:text-nexus-error">删除模板</span>
        </button>
      </div>
    </div>
  );
};
