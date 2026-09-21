import { useState, useMemo, useRef, useEffect } from 'react';
import { createPortal } from 'react-dom';
import { openInExplorer, openTerminal } from '../../services/system';
import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { openToolsApi, parseToolCommands, type Service, type ToolCommand } from '../../services/service';
import { useServiceActions, type ServiceActionName } from '../../hooks/useServiceActions';
import { useToolStore } from '../../stores/toolStore';
import { reportError } from '../../utils/error';
import { ServiceContextMenu } from './ServiceContextMenu';

interface Props {
  service: Service;
  running: boolean;
  /**
   * 意外失败（崩溃/秒退/spawn 失败）。
   *
   * 卡片上只用颜色表达（红点），不写"已失败 · 退出码 N"这类文字——退出码与发生时刻
   * 在日志面板头部与悬停说明里（见 utils/serviceFailure），那里才是看原因的地方。
   */
  failed: boolean;
  /** 正在跟随的日志文件（null = 没在跟随）：菜单据此给「取消跟随」入口 */
  followedLog: string | null;
  isEditing: boolean;
  onEdit: () => void;
  onRefresh: () => void;
  onContextMenu: (id: string, name: string) => void;
  onViewLog?: () => void;
  onRunToolCommand?: (serviceId: string, commandId: string, commandName: string) => void;
}

export function ServiceTreeEntry({
  service, running, failed, followedLog, isEditing, onEdit, onRefresh, onContextMenu, onViewLog, onRunToolCommand,
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
    // 失败的服务：点卡片直接看失败日志。失败后的第一诉求是"为什么挂了"，不是改配置
    // （编辑入口仍在右键菜单里，见 ServiceContextMenu 的「编辑服务」）
    if (failed) { onViewLog?.(); return; }
    onEdit();
  };
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

  // 服务动作与"服务列收起后的圆点条"共用一份实现（见 useServiceActions 的说明）
  const { busyId, runAction, unfollowLog } = useServiceActions(onRefresh);
  const busy = busyId === service.id;

  /** 卡片按钮入口：先阻止冒泡（卡片点击 = 打开编辑面板），再走共享动作 */
  const handleAction = (e: React.MouseEvent, action: ServiceActionName) => {
    e.stopPropagation();
    void runAction(service, action);
  };

  const handleContextMenu = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    // 统一走内部菜单（在资源管理器中打开 / 工具命令 / 删除服务），工具命令为空时自然只显示兜底项
    setContextMenu({ x: e.clientX, y: e.clientY });
  };

  // 关菜单由 ServiceContextMenu 统一负责（它要保证每个条目都关），这里只管动作本身
  const handleRunCommand = (cmd: ToolCommand) => {
    onRunToolCommand?.(service.id, cmd.id, cmd.name);
  };

  const handleOpenWithTool = async () => {
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

          {/* Hover 操作按钮（拖拽中隐藏；onPointerDown 阻止冒泡，长按按钮不触发拖拽）。
              用 opacity 而非 display：位置一直占着，hover 时服务名不会被按钮挤动。
              运行中与失败共用同一组按钮（日志/重启/停止）——失败态也要能"停止"：
              它既是收尾（结束残留进程，含另开窗口的游离进程），也是清掉红点的入口 */}
          <div
            className={`flex items-center gap-1 opacity-0 flex-shrink-0 ${isDragging ? '' : 'group-hover:opacity-100'}`}
            onPointerDown={(e) => e.stopPropagation()}
          >
            {running || failed ? (
              <>
                <button
                  className="px-2 py-1 text-[11px] bg-nexus-info/15 text-nexus-info rounded hover:bg-nexus-info/25 disabled:opacity-40"
                  disabled={busy}
                  onClick={e => { e.stopPropagation(); onViewLog?.(); }}
                  title={failed ? '查看失败日志' : '查看日志'}
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

      {/* 右键菜单（与"服务列收起后的圆点条"共用同一份，见 ServiceContextMenu）。
          Portal 到 body：服务面板有 transform 容器（折叠动画），fixed 定位的菜单
          若留在其内会以 transform 容器为包含块，导致坐标错位被裁剪而不可见 */}
      {contextMenu && createPortal(
        <ServiceContextMenu
          x={contextMenu.x}
          y={contextMenu.y}
          cwd={service.cwd}
          running={running}
          failed={failed}
          followedLog={followedLog}
          openToolName={boundTool?.name ?? null}
          toolCommands={toolCommands}
          onViewLog={onViewLog}
          onEdit={onEdit}
          onStart={() => void runAction(service, 'start')}
          onStop={() => void runAction(service, 'stop')}
          onRestart={() => void runAction(service, 'restart')}
          onOpenWithTool={handleOpenWithTool}
          onOpenInExplorer={() => void openInExplorer(service.cwd)}
          onOpenTerminal={() => void openTerminal(service.cwd)}
          onRunCommand={handleRunCommand}
          onUnfollowLog={() => void unfollowLog(service)}
          onDelete={() => onContextMenu(service.id, service.name)}
          onClose={() => setContextMenu(null)}
        />,
        document.body,
      )}
    </div>
  );
}

// 菜单本体与图标都在 ServiceContextMenu.tsx：它与"服务列收起后的圆点条"共用同一份
