import { useState, useMemo, useRef, useEffect } from 'react';
import { createPortal } from 'react-dom';
import { openInExplorer, openTerminal } from '../../services/system';
import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { openToolsApi, processApi, parseToolCommands, type Service, type ToolCommand } from '../../services/service';
import { startSingleService, stopSingleService } from '../../stores/serviceActions';
import { useToolStore } from '../../stores/toolStore';
import { reportError } from '../../utils/error';
import { ContextMenu, ContextMenuItem } from '../ui/ContextMenu';

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

/** 服务动作：卡片按钮与右键菜单共同的三档操作 */
type ServiceActionName = 'start' | 'stop' | 'restart';

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

  // 绑定的打开工具（服务设置中选择，全局工具库共享）
  const boundToolId = useToolStore(s => s.bindings[service.id]);
  const openTools = useToolStore(s => s.openTools);
  const boundTool = openTools.find(t => t.id === boundToolId);

  // 解析工具命令（DB 里的 TEXT 列属不受信数据，用带校验的解析器而不是裸 as 断言）
  const toolCommands = useMemo(
    () => parseToolCommands(service.tool_commands),
    [service.tool_commands],
  );

  /** 服务动作的唯一实现：卡片 Hover 按钮与右键菜单共用，避免两处口径分叉 */
  const runAction = async (action: ServiceActionName) => {
    setBusy(true);
    try {
      if (action === 'start') {
        // 共享动作层：启动进程 + 追加该服务的文件监听（口径与详情页一致）
        await startSingleService(service.project_id, service.id);
      } else if (action === 'stop') {
        // 停止进程 + 移除监听 + 清日志（后端已清缓冲，前端缓存同步清）
        await stopSingleService(service.project_id, service.id);
      }
      else await processApi.restart(service.id);
      onRefresh();
    } catch (err: unknown) {
      reportError(`${action === 'start' ? '启动' : action === 'stop' ? '停止' : '重启'}服务失败`, err);
      // 启动失败：后端已记失败状态（spawn 失败），刷新让卡片显示"失败"按钮
      if (action === 'start') onRefresh();
    }
    setBusy(false);
  };

  /** 卡片按钮入口：先阻止冒泡（卡片点击 = 打开编辑面板），再走共享动作 */
  const handleAction = (e: React.MouseEvent, action: ServiceActionName) => {
    e.stopPropagation();
    void runAction(action);
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

  const handleOpenWithTool = async () => {
    setContextMenu(null);
    try {
      await openToolsApi.openWith(service.id);
    } catch (err) {
      reportError('用工具打开失败', err);
    }
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
        <ServiceContextMenu
          x={contextMenu.x}
          y={contextMenu.y}
          cwd={service.cwd}
          running={running}
          failed={failed}
          openToolName={boundTool?.name ?? null}
          toolCommands={toolCommands}
          onViewLog={onViewLog ? () => { setContextMenu(null); onViewLog(); } : undefined}
          onStart={() => { setContextMenu(null); void runAction('start'); }}
          onStop={() => { setContextMenu(null); void runAction('stop'); }}
          onRestart={() => { setContextMenu(null); void runAction('restart'); }}
          onOpenWithTool={handleOpenWithTool}
          onOpenInExplorer={() => {
            setContextMenu(null);
            void openInExplorer(service.cwd);
          }}
          onOpenTerminal={() => {
            setContextMenu(null);
            void openTerminal(service.cwd);
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

/** 查看日志：文档 + 折角（失败态复用同一图标，靠色调区分） */
const LogIcon = () => (
  <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
    <path d="M2 1.5h3L7.5 4v4.5H2z"/><path d="M5 1.5V4h2.5"/>
  </svg>
);

/** 重启：环形箭头（与卡片 ↻ 同义） */
const RestartIcon = () => (
  <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1">
    <path d="M8.75 5a3.75 3.75 0 1 1-3.75-3.75c1.05 0 2.05.42 2.81 1.14L8.75 3.33"/>
    <path d="M8.75 1.25v2.08H6.67"/>
  </svg>
);

/** 启动：实心三角（与卡片 ▶ 同形，也和"工具命令"条目同义） */
const PlayIcon = () => (
  <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
    <polygon points="3,1.2 3,8.8 8.6,5" fill="currentColor"/>
  </svg>
);

/** 停止：实心方块（与卡片 ■ 同形） */
const StopIcon = () => (
  <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
    <rect x="2" y="2" width="6" height="6" rx="1" fill="currentColor"/>
  </svg>
);

interface ServiceContextMenuProps {
  x: number;
  y: number;
  cwd: string;
  /** 运行中：显示 查看日志 / 重启服务 / 停止服务（与卡片按钮一致） */
  running: boolean;
  /** 意外失败：显示 查看失败日志 / 重新启动（与卡片按钮一致） */
  failed: boolean;
  /** 绑定的打开工具名（null = 未绑定，不显示该项） */
  openToolName: string | null;
  toolCommands: ToolCommand[];
  /** 查看日志（调用方无日志入口时不传，该条目随之隐藏） */
  onViewLog?: () => void;
  onStart: () => void;
  onStop: () => void;
  onRestart: () => void;
  onOpenWithTool: () => void;
  onOpenInExplorer: () => void;
  onOpenTerminal: () => void;
  onRunCommand: (cmd: ToolCommand) => void;
  onDelete: () => void;
  onClose: () => void;
}

/** 服务条目的右键菜单：容器/定位/关闭与行按钮都走 ui/ContextMenu 原语，本组件只留条目编排 */
const ServiceContextMenu = ({ x, y, cwd, running, failed, openToolName, toolCommands, onViewLog, onStart, onStop, onRestart, onOpenWithTool, onOpenInExplorer, onOpenTerminal, onRunCommand, onDelete, onClose }: ServiceContextMenuProps) => (
  <ContextMenu x={x} y={y} onClose={onClose}>
    {/* 服务动作：与卡片 Hover 按钮一一对应（运行中=日志/重启/停止，失败=失败日志/重启，未运行=启动）。
        卡片按钮只在 Hover 时出现，右键是同一批操作的第二入口，两处共用 runAction */}
    <div className="border-b border-nexus-border/30 py-1.5 px-1.5">
      {running ? (
        <>
          {onViewLog && <ContextMenuItem icon={<LogIcon />} label="查看日志" tone="info" truncate onClick={onViewLog} />}
          <ContextMenuItem icon={<RestartIcon />} label="重启服务" tone="warning" truncate onClick={onRestart} />
          <ContextMenuItem icon={<StopIcon />} label="停止服务" tone="error" truncate onClick={onStop} />
        </>
      ) : failed ? (
        <>
          {onViewLog && <ContextMenuItem icon={<LogIcon />} label="查看失败日志" tone="error" truncate onClick={onViewLog} />}
          <ContextMenuItem icon={<PlayIcon />} label="重新启动" tone="success" truncate onClick={onStart} />
        </>
      ) : (
        <ContextMenuItem icon={<PlayIcon />} label="启动服务" tone="success" truncate onClick={onStart} />
      )}
    </div>

    {/* 用绑定的工具打开（服务设置中选择，如 IDEA / VS Code） */}
    {openToolName && (
      <div className="py-1.5 px-1.5">
        <ContextMenuItem
          icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
            <path d="M2 1h6a1 1 0 011 1v6a1 1 0 01-1 1H2a1 1 0 01-1-1V2a1 1 0 011-1z"/><path d="M1.5 6.5h7M3.5 6.5V9"/>
          </svg>}
          label={`用 ${openToolName} 打开`}
          truncate
          onClick={onOpenWithTool}
        />
      </div>
    )}

    {/* 打开资源管理器 / 打开终端 */}
    {cwd && (
      <div className="py-1.5 px-1.5">
        <ContextMenuItem
          icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
            <path d="M1.5 3h2l1-1.5h4a1 1 0 011 1v5.5a1 1 0 01-1 1h-7a1 1 0 01-1-1V3z"/>
          </svg>}
          label="在资源管理器中打开"
          onClick={onOpenInExplorer}
        />
        <ContextMenuItem
          icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
            <path d="M1.5 2.5l3.5 2.5-3.5 2.5"/><line x1="6.5" y1="8" x2="8.5" y2="8"/>
          </svg>}
          label="打开终端"
          onClick={onOpenTerminal}
        />
      </div>
    )}

    {/* 工具命令 */}
    {toolCommands.length > 0 && (
      <div className={`py-1.5 px-1.5 ${cwd ? 'border-t border-nexus-border/30' : ''}`}>
        {toolCommands.map(cmd => (
          <ContextMenuItem
            key={cmd.id}
            icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none">
              <polygon points="3,1 3,9 9,5" fill="currentColor"/>
            </svg>}
            label={cmd.name}
            truncate
            onClick={() => onRunCommand(cmd)}
          />
        ))}
      </div>
    )}

    {/* 删除 */}
    <div className="border-t border-nexus-border/30 py-1.5 px-1.5">
      <ContextMenuItem
        tone="danger"
        icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.3">
          <path d="M2.5 3h5M3.5 3V2a.5.5 0 01.5-.5h2a.5.5 0 01.5.5v1M4 4.5v3M6 4.5v3M3 3l.5 6a1 1 0 001 .5h3a1 1 0 001-.5L9 3"/>
        </svg>}
        label="删除服务"
        onClick={onDelete}
      />
    </div>
  </ContextMenu>
);
