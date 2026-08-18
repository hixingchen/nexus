import { useState, useMemo, useRef, useEffect } from 'react';
import { createPortal } from 'react-dom';
import { invoke } from '@tauri-apps/api/core';
import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { processApi, type Service, type ToolCommand } from '../../services/service';
import { useLogStore } from '../../stores/logStore';
import { showNotification } from '../ui/Toast';

interface Props {
  service: Service;
  running: boolean;
  /** 意外失败（崩溃/秒退/spawn 失败）：显示"失败"按钮，点击可查看日志 */
  failed: boolean;
  isEditing: boolean;
  onEdit: () => void;
  onRefresh: () => void;
  onContextMenu: (id: string, name: string) => void;
  onViewLog?: () => void;
  onRunToolCommand?: (serviceId: string, commandId: string, commandName: string) => void;
}

export function ServiceTreeEntry({
  service, running, failed, isEditing, onEdit, onRefresh, onContextMenu, onViewLog, onRunToolCommand,
}: Props) {
  // dnd-kit 可排序：长按卡片 250ms 进入拖拽（快速点击照常打开编辑面板）
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({ id: service.id });
  const style = {
    transform: CSS.Transform.toString(transform),
    transition: isDragging ? 'none' : transition,
    zIndex: isDragging ? 50 : 'auto' as const,
  };
  /** 本次点击前发生过拖拽（长按松手会触发 click，需跳过编辑打开） */
  const draggedRef = useRef(false);
  useEffect(() => {
    if (isDragging) draggedRef.current = true;
  }, [isDragging]);
  const handleClick = () => {
    if (draggedRef.current) { draggedRef.current = false; return; }
    onEdit();
  };
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
      else if (action === 'stop') {
        await processApi.stop(service.id);
        // 正常停止：日志清空（后端已清缓冲，前端缓存同步清）
        useLogStore.getState().clearLogs(service.id);
      }
      else await processApi.restart(service.id);
      onRefresh();
    } catch (err: unknown) {
      console.error(String(err));
      showNotification({ variant: 'error', title: `${action === 'start' ? '启动' : action === 'stop' ? '停止' : '重启'}服务失败`, description: String(err) });
      // 启动失败：后端已记失败状态（spawn 失败），刷新让卡片显示"失败"按钮
      if (action === 'start') onRefresh();
    }
    setBusy(false);
  };

  const handleContextMenu = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    // 统一走内部菜单（在资源管理器中打开 / 工具命令 / 删除服务），工具命令为空时自然只显示兜底项
    setContextMenu({ x: e.clientX, y: e.clientY });
  };

  const handleRunCommand = (cmd: ToolCommand) => {
    setContextMenu(null);
    onRunToolCommand?.(service.id, cmd.id, cmd.name);
  };

  return (
    <div className="px-2 py-0.5" ref={setNodeRef} style={style}>
      <div
        {...attributes}
        {...listeners}
        className={`cursor-pointer group rounded-md px-3 py-2.5 transition-colors select-none ${
          isEditing
            ? 'bg-nexus-accent/10 border border-nexus-accent/30'
            : 'bg-nexus-bg/30 border border-nexus-border hover:border-nexus-muted/70'
        } ${isDragging ? 'shadow-[0_16px_48px_rgba(0,0,0,0.5)] ring-2 ring-nexus-accent/30 border-nexus-accent/50 bg-nexus-bg cursor-grabbing' : ''}`}
        onClick={handleClick}
        onContextMenu={handleContextMenu}
        title={isDragging ? '拖拽排序' : undefined}
      >
        <div className="flex items-center gap-2">
          {/* 运行状态：绿=运行中，红=意外失败，灰=未运行 */}
          <span className={`w-[7px] h-[7px] rounded-full flex-shrink-0 ${
            running ? 'bg-nexus-success' : failed ? 'bg-nexus-error' : 'bg-nexus-muted/40'
          }`} />

          {/* 名称 */}
          <span className="flex-1 text-[13px] text-nexus-text font-medium truncate">{service.name}</span>

          {/* Hover 操作按钮（拖拽中隐藏；onPointerDown 阻止冒泡，长按按钮不触发拖拽） */}
          <div
            className={`flex items-center gap-1 opacity-0 flex-shrink-0 ${isDragging ? '' : 'group-hover:opacity-100'}`}
            onPointerDown={(e) => e.stopPropagation()}
          >
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
            ) : failed ? (
              <>
                {/* 失败按钮：点击查看日志（崩溃/秒退的报错是诊断关键） */}
                <button
                  className="px-2 py-1 text-[11px] bg-nexus-error/15 text-nexus-error rounded hover:bg-nexus-error/25 disabled:opacity-40"
                  disabled={busy}
                  onClick={e => { e.stopPropagation(); onViewLog?.(); }}
                  title="查看失败日志"
                >失败</button>
                <button
                  className="px-2 py-1 text-[11px] bg-nexus-success/15 text-nexus-success rounded hover:bg-nexus-success/25 disabled:opacity-40"
                  disabled={busy}
                  onClick={e => handleAction(e, 'start')}
                  title="重新启动"
                >▶</button>
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

      {/* 右键菜单。
          Portal 到 body：服务面板有 transform 容器（折叠动画），fixed 定位的菜单
          若留在其内会以 transform 容器为包含块，导致坐标错位被裁剪而不可见 */}
      {contextMenu && createPortal(
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
          onOpenTerminal={async () => {
            setContextMenu(null);
            try {
              await invoke('open_terminal', { path: service.cwd });
            } catch (err) {
              console.error('打开终端失败:', err);
              showNotification({ variant: 'error', title: '打开终端失败' });
            }
          }}
          onRunCommand={handleRunCommand}
          onDelete={() => {
            setContextMenu(null);
            onContextMenu(service.id, service.name);
          }}
          onClose={() => setContextMenu(null)}
        />,
        document.body,
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
  onOpenTerminal: () => void;
  onRunCommand: (cmd: ToolCommand) => void;
  onDelete: () => void;
  onClose: () => void;
}

const ContextMenu = ({ x, y, cwd, toolCommands, onOpenInExplorer, onOpenTerminal, onRunCommand, onDelete, onClose }: ContextMenuProps) => {
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
      {/* 打开资源管理器 / 打开终端 */}
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

      {/* 删除 */}
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
