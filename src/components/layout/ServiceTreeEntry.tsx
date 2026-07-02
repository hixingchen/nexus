import { useState, useMemo, useRef, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { processApi, type Service, type ToolCommand } from '../../services/service';
import { showNotification } from '../ui/Toast';

interface Props {
  service: Service;
  running: boolean;
  isEditing: boolean;
  onEdit: () => void;
  onRefresh: () => void;
  onContextMenu: (e: React.MouseEvent, id: string, name: string) => void;
  onViewLog?: () => void;
  onRunToolCommand?: (serviceId: string, commandId: string, commandName: string) => void;
}

export function ServiceTreeEntry({
  service, running, isEditing, onEdit, onRefresh, onContextMenu, onViewLog, onRunToolCommand,
}: Props) {
  const [busy, setBusy] = useState(false);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);

  // 解析工具命令
  const toolCommands = useMemo(() => {
    try {
      return JSON.parse(service.tool_commands || '[]') as ToolCommand[];
    } catch {
      return [];
    }
  }, [service.tool_commands]);

  const handleAction = async (e: React.MouseEvent, action: 'start' | 'stop' | 'restart') => {
    e.stopPropagation();
    setBusy(true);
    try {
      if (action === 'start') await processApi.start(service.id);
      else if (action === 'stop') await processApi.stop(service.id);
      else await processApi.restart(service.id);
      onRefresh();
    } catch (err: unknown) {
      console.error(String(err));
      showNotification({ variant: 'error', title: `${action === 'start' ? '启动' : action === 'stop' ? '停止' : '重启'}服务失败`, description: String(err) });
    }
    setBusy(false);
  };

  const handleContextMenu = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (toolCommands.length > 0) {
      setContextMenu({ x: e.clientX, y: e.clientY });
    } else {
      onContextMenu(e, service.id, service.name);
    }
  };

  const handleRunCommand = (cmd: ToolCommand) => {
    setContextMenu(null);
    onRunToolCommand?.(service.id, cmd.id, cmd.name);
  };

  return (
    <div className="px-2 py-0.5">
      <div
        className={`cursor-pointer group rounded-md px-3 py-2.5 ${
          isEditing
            ? 'bg-nexus-accent/10 border border-nexus-accent/30'
            : 'bg-nexus-bg/30 border border-nexus-border hover:border-nexus-muted/70'
        }`}
        onClick={onEdit}
        onContextMenu={handleContextMenu}
      >
        <div className="flex items-center gap-2">
          {/* 运行状态 */}
          <span className={`w-[7px] h-[7px] rounded-full flex-shrink-0 ${
            running ? 'bg-nexus-success' : 'bg-nexus-muted/40'
          }`} />

          {/* 名称 */}
          <span className="flex-1 text-[13px] text-nexus-text font-medium truncate">{service.name}</span>

          {/* Hover 操作按钮 */}
          <div className="flex items-center gap-1 opacity-0 group-hover:opacity-100 flex-shrink-0">
            {running ? (
              <>
                <button
                  className="px-2 py-1 text-[11px] bg-nexus-info/15 text-nexus-info rounded hover:bg-nexus-info/25 disabled:opacity-40"
                  disabled={busy}
                  onClick={e => { e.stopPropagation(); onViewLog?.(); }}
                  title="查看日志"
                >日志</button>
                <button
                  className="px-2 py-1 text-[11px] bg-nexus-warning/15 text-nexus-warning rounded hover:bg-nexus-warning/25 disabled:opacity-40"
                  disabled={busy}
                  onClick={e => handleAction(e, 'restart')}
                  title="重启"
                >↻</button>
                <button
                  className="px-2 py-1 text-[11px] bg-nexus-error/15 text-nexus-error rounded hover:bg-nexus-error/25 disabled:opacity-40"
                  disabled={busy}
                  onClick={e => handleAction(e, 'stop')}
                  title="停止"
                >■</button>
              </>
            ) : (
              <button
                className="px-2 py-1 text-[11px] bg-nexus-success/15 text-nexus-success rounded hover:bg-nexus-success/25 disabled:opacity-40"
                disabled={busy}
                onClick={e => handleAction(e, 'start')}
                title="启动"
              >▶</button>
            )}
          </div>
        </div>
      </div>

      {/* 右键菜单 */}
      {contextMenu && (
        <ContextMenu
          x={contextMenu.x}
          y={contextMenu.y}
          cwd={service.cwd}
          toolCommands={toolCommands}
          onOpenInExplorer={async () => {
            setContextMenu(null);
            try {
              await invoke('open_in_explorer', { path: service.cwd });
            } catch (err) {
              console.error('打开资源管理器失败:', err);
              showNotification({ variant: 'error', title: '打开资源管理器失败' });
            }
          }}
          onRunCommand={handleRunCommand}
          onDelete={() => {
            setContextMenu(null);
            onContextMenu(new MouseEvent('contextmenu') as any, service.id, service.name);
          }}
          onClose={() => setContextMenu(null)}
        />
      )}
    </div>
  );
}

// ── 右键菜单组件 ──────────────────────────────────────────

interface ContextMenuProps {
  x: number;
  y: number;
  cwd: string;
  toolCommands: ToolCommand[];
  onOpenInExplorer: () => void;
  onRunCommand: (cmd: ToolCommand) => void;
  onDelete: () => void;
  onClose: () => void;
}

const ContextMenu = ({ x, y, cwd, toolCommands, onOpenInExplorer, onRunCommand, onDelete, onClose }: ContextMenuProps) => {
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
    const menuWidth = 200;
    const menuHeight = toolCommands.length * 36 + 80;
    const maxX = window.innerWidth - menuWidth - 8;
    const maxY = window.innerHeight - menuHeight - 8;
    return {
      left: Math.min(x, maxX),
      top: Math.min(y, maxY),
    };
  }, [x, y, toolCommands.length]);

  return (
    <div
      ref={menuRef}
      className="fixed z-[70] w-[180px] bg-nexus-surface border border-nexus-border/60 rounded-lg shadow-2xl overflow-hidden"
      style={menuStyle}
    >
      {/* 打开资源管理器 */}
      {cwd && (
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
        </div>
      )}

      {/* 工具命令 */}
      {toolCommands.length > 0 && (
        <div className={`py-1.5 px-1.5 ${cwd ? 'border-t border-nexus-border/30' : ''}`}>
          {toolCommands.map(cmd => (
            <button
              key={cmd.id}
              className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-md hover:bg-nexus-accent/10 transition-colors group text-left"
              onClick={() => onRunCommand(cmd)}
            >
              <svg width="10" height="10" viewBox="0 0 10 10" fill="none" className="text-nexus-muted group-hover:text-nexus-accent flex-shrink-0">
                <polygon points="3,1 3,9 9,5" fill="currentColor"/>
              </svg>
              <span className="text-[12px] text-nexus-text truncate">{cmd.name}</span>
            </button>
          ))}
        </div>
      )}

      {/* 分隔线和删除 */}
      <div className="border-t border-nexus-border/30 py-1.5 px-1.5">
        <button
          className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-md hover:bg-nexus-error/10 transition-colors group text-left"
          onClick={onDelete}
        >
          <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.3" className="text-nexus-muted group-hover:text-nexus-error flex-shrink-0">
            <path d="M2.5 3h5M3.5 3V2a.5.5 0 01.5-.5h2a.5.5 0 01.5.5v1M4 4.5v3M6 4.5v3M3 3l.5 6a1 1 0 001 .5h3a1 1 0 001-.5L9 3"/>
          </svg>
          <span className="text-[12px] text-nexus-muted group-hover:text-nexus-error">删除服务</span>
        </button>
      </div>
    </div>
  );
};
