import { useState, useEffect, useMemo, useRef } from 'react';
import { createPortal } from 'react-dom';
import { open as openUrl } from '@tauri-apps/plugin-shell';
import { openInExplorer, openTerminal } from '../../services/system';
import { useDndMonitor } from '@dnd-kit/core';
import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { openToolsApi, parseToolCommands, processApi, type FailedService, type RunningService, type Service, type ToolCommand } from '../../services/service';
import { useServiceActions, type ServiceActionName } from '../../hooks/useServiceActions';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useToolStore } from '../../stores/toolStore';
import { usePortConflictStore } from '../../stores/portConflict';
import { reportError } from '../../utils/error';
import { portConflictSummary } from '../../utils/serviceFailure';
import { resolveOpenPort } from '../../utils/serviceOpen';
import { ServiceContextMenu } from './ServiceContextMenu';

/** 开机启动图标（不区分 S4U/密码；模式与是否已装启动器放 tooltip 与设置页） */
const POWER_ICON = (
  <svg width="11" height="11" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round">
    <path d="M6 1.2v4" />
    <path d="M3.2 2.8a4.2 4.2 0 106.6 0" />
  </svg>
);

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
  /**
   * 失败详情（含日志里识别出的端口冲突）。
   *
   * 卡片上除红点外，只额外展示"端口 X 被谁占用"这一条可行动信息——退出码/时刻
   * 仍在日志面板与悬停说明里，避免把一行卡片撑成诊断面板。
   */
  failure?: FailedService | null;
  /** 正在跟随的日志文件（null = 没在跟随）：菜单据此给「取消跟随」入口 */
  followedLog: string | null;
  /** 运行详情：阶段（启动中/已就绪）与自动发现的监听端口；null = 未运行 */
  runningInfo: RunningService | null;
  isEditing: boolean;
  onEdit: () => void;
  onRefresh: () => void;
  onContextMenu: (id: string, name: string) => void;
  onViewLog?: () => void;
  onRunToolCommand?: (serviceId: string, commandId: string, commandName: string) => void;
}

export function ServiceTreeEntry({
  service, running, failed, failure = null, followedLog, runningInfo, isEditing, onEdit, onRefresh, onContextMenu, onViewLog, onRunToolCommand,
}: Props) {
  // dnd-kit 可排序：长按卡片 250ms 进入拖拽（快速点击照常打开编辑面板）
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({ id: service.id });
  const style = {
    transform: CSS.Transform.toString(transform),
    transition: isDragging ? 'none' : transition,
    zIndex: isDragging ? 50 : 'auto' as const,
  };
  /** 本次点击前发生过**指针**拖拽（长按松手会触发 click，需跳过编辑打开） */
  const draggedRef = useRef(false);
  // 只有指针拖拽才需要"松手后吞掉那次补发的 click"（UX-8）：键盘拖拽（Space 起步）
  // 不会补发 click，若也置脏，拖拽排序后第一次点击/Enter 会被白白吞掉一次。
  // 判据取拖拽的激活事件类型——它由 dnd-kit 在 document 级按键监听里处理，
  // 不受这里对 Enter 的接管影响（见 handleKeyDown）。
  // 拖拽一结束就安排清掉：那次补发的 click 与拖拽结束在**同一个事件循环回合**里，
  // 而"松手落在卡片之外"时它根本不会来——标记若一直留着，下次真实点击/Enter 就被吞了
  const clearDragFlagSoon = () => { setTimeout(() => { draggedRef.current = false; }, 0); };
  useDndMonitor({
    onDragStart: (e) => {
      if (e.active.id === service.id && !(e.activatorEvent instanceof KeyboardEvent)) draggedRef.current = true;
    },
    onDragEnd: (e) => { if (e.active.id === service.id) clearDragFlagSoon(); },
    onDragCancel: (e) => { if (e.active.id === service.id) clearDragFlagSoon(); },
  });
  const handleClick = () => {
    if (draggedRef.current) { draggedRef.current = false; return; }
    // 一律打开编辑面板，**失败态也不例外**：日志已经有卡片上的「日志」按钮（失败时
    // 文案是"查看失败日志"）与右键菜单两个入口，而"点卡片"这个动作在运行、未运行两种
    // 状态下都是"看 / 改这个服务的配置"。按状态改它的含义，同一张卡片点下去的结果就
    // 不可预期了——失败后想改配置（改命令、换目录再重启）反而要先想起右键菜单
    onEdit();
  };
  /**
   * 键盘分工（UX-8）：卡片经 dnd-kit 的 attributes 拿到 `role="button"` + `tabIndex=0`
   * ——辅助技术把它朗读成按钮，所以 **Enter 必须是"卡片动作"（等价点击）**；
   * **Space 留给 dnd-kit 的键盘排序**（它的屏幕阅读器说明正是"按空格拾起"，
   * 方向键移动、再按空格/Enter 落下、Esc 取消），`KeyboardSensor` 照旧挂着，键盘排序能力不变。
   *
   * 为什么不能把 Enter 也转发给 dnd：它的激活器默认把 Enter 也算作"开始拖拽"
   * （`defaultKeyboardCodes.start = [Space, Enter]`），转发过去 Enter 就又变回了拖拽。
   */
  const handleKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    // 焦点在内层按钮（日志 / 重启 / 停止 / 启动）上时按键归那个按钮：转发给 dnd 会让
    // Enter 与 Space 都被激活器 preventDefault 掉，这些按钮用键盘根本按不动
    if (e.target !== e.currentTarget) return;
    if (e.key === 'Enter') {
      // 拖拽进行中：Enter 是"落下"（dnd 挂在 document 上的监听会处理），不该顺手开一次面板
      if (!isDragging) { e.preventDefault(); handleClick(); }
      return;
    }
    listeners?.onKeyDown?.(e);
  };
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);

  // ── 服务健康：状态徽章 + 详情弹层 ─────────────────────────
  const [healthOpen, setHealthOpen] = useState(false);
  const [healthPos, setHealthPos] = useState<{ left: number; top: number; width: number } | null>(null);
  const healthAnchorRef = useRef<HTMLButtonElement | null>(null);
  const healthPopoverRef = useRef<HTMLDivElement | null>(null);
  useClickOutside(healthPopoverRef, () => setHealthOpen(false), {
    refs: [healthAnchorRef],
    escape: true,
    captureScroll: true,
  });

  const ports = runningInfo?.listening_ports ?? [];
  const openResolution = resolveOpenPort(
    { enabled: service.open_enabled, specifiedPort: service.open_port },
    ports,
  );
  const phase = runningInfo?.phase;
  const phaseLabel = phase === 'starting' ? '启动中'
    : phase === 'unready' ? '启动超时'
    : phase === 'ready' ? '已就绪'
    : '运行中';
  const phaseHint = phase === 'starting'
    ? '进程已启动，尚在就绪宽限期内'
    : phase === 'unready'
      ? '进程仍在运行，但就绪检查超时'
      : '进程存活超过就绪宽限期';
  const phaseWarn = phase === 'starting' || phase === 'unready';

  // 开机启动：徽章只留一个电源图标，凭据模式与“启动器是否已安装”放 tooltip（细节在设置页）。
  // 「跟随项目启动」是独立开关，不再影响这里。
  const bootModeLabel = (service.boot_credential_mode ?? 's4u') === 'password' ? '保存密码' : 'S4U';
  const bootTooltip = `已配置开机启动（${bootModeLabel}）；需在「设置 > 服务开机启动」确认已安装对应启动器后才生效`;

  const openPort = (port: number) => {
    void openUrl(`http://localhost:${port}`).catch(err => reportError('打开服务地址失败', err));
  };

  const toggleHealth = (e: React.MouseEvent) => {
    e.stopPropagation();
    const anchor = healthAnchorRef.current;
    if (!anchor) return;
    if (healthOpen) { setHealthOpen(false); return; }
    const rect = anchor.getBoundingClientRect();
    const width = 264;
    setHealthPos({
      left: Math.max(8, Math.min(rect.left, window.innerWidth - width - 8)),
      top: rect.bottom + 6,
      width,
    });
    setHealthOpen(true);
  };

  // 服务停止/重启后详情不再成立：收起，避免下次启动时旧弹层自己冒出来
  useEffect(() => {
    if (!runningInfo) setHealthOpen(false);
  }, [runningInfo]);

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
  // 按 id 问"这条在忙吗"，而不是拿单个 busyId 比（CQ-40：同时操作两条服务时，
  // 单个值会被后来者顶掉，先前那条返回时还会把后者重新点亮）
  const { isBusy, runAction, unfollowLog } = useServiceActions(onRefresh);
  const busy = isBusy(service.id);

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

  const portConflict = failure?.port_conflict ?? null;

  /**
   * 失败卡片上的端口冲突：打开同一个处置弹框。
   *
   * 失败态的服务已经退出，"处理"之后要做的是**重启**而不是"启动"；弹框只负责
   * 结束占用者，真正的重启在这里执行并刷新卡片状态。
   */
  const handlePortConflict = async (e: React.MouseEvent) => {
    e.stopPropagation();
    if (!portConflict) return;
    const proceed = await usePortConflictStore.getState().ask({
      serviceName: service.name,
      conflict: portConflict,
      actionLabel: '重启服务',
    });
    if (!proceed) return;
    try {
      await processApi.restart(service.id);
    } catch (err: unknown) {
      reportError('重启服务失败', err);
    } finally {
      onRefresh();
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
        onKeyDown={handleKeyDown}
        onContextMenu={handleContextMenu}
        title={isDragging ? '拖拽排序' : undefined}
      >
        <div className="flex items-center gap-2">
          {/* 状态圆点：唯一常驻状态指示。运行中时点击打开健康弹层。
              常态不显示「已就绪/启动中」文字，避免和时有时无的「打开」按钮一起把行撑得参差。 */}
          {running ? (
            <button
              ref={healthAnchorRef}
              type="button"
              onPointerDown={e => e.stopPropagation()}
              onClick={toggleHealth}
              aria-haspopup="dialog"
              aria-expanded={healthOpen}
              aria-label={`${service.name}：${phaseLabel}，查看服务健康`}
              title={`${phaseLabel} · ${phaseHint}（点击查看详情）`}
              className={`w-5 h-5 -ml-1 flex items-center justify-center rounded-full flex-shrink-0 transition-colors hover:bg-nexus-hover/50 ${
                phaseWarn ? 'text-nexus-warning' : 'text-nexus-success'
              }`}
            >
              <span className={`w-[7px] h-[7px] rounded-full ${
                phaseWarn ? 'bg-nexus-warning' : 'bg-nexus-success'
              } ${phase === 'starting' ? 'animate-pulse' : ''}`} />
            </button>
          ) : (
            <span className="w-5 h-5 -ml-1 flex items-center justify-center flex-shrink-0">
              <span className={`w-[7px] h-[7px] rounded-full ${
                failed ? 'bg-nexus-error' : 'bg-nexus-muted/40'
              }`} />
            </span>
          )}

          {/* 开机启动：只在配置了开机启动时显示电源图标，未配置不留空位。
              不写文字、不区分凭据模式（详见 tooltip 与设置页） */}
          {service.boot_enabled && (
            <span
              role="img"
              aria-label={bootTooltip}
              title={bootTooltip}
              className="w-4 h-4 flex items-center justify-center flex-shrink-0 text-nexus-accent"
            >
              {POWER_ICON}
            </span>
          )}

          {/* 名称 + 失败时可行动的端口冲突一行 */}
          <span className="flex-1 min-w-0">
            <span className="block truncate text-[13px] text-nexus-text font-medium">{service.name}</span>
            {portConflict && (
              <button
                type="button"
                className="mt-0.5 block max-w-full text-left text-[10.5px] text-nexus-error/85 break-words hover:text-nexus-error hover:underline"
                title={`${portConflictSummary(portConflict)} · 点击处理`}
                // 卡片整体可拖拽/点击编辑：按钮自己的按下与点击都不能冒泡
                onPointerDown={e => e.stopPropagation()}
                onClick={e => void handlePortConflict(e)}
              >
                {portConflictSummary(portConflict)} · 处理
              </button>
            )}
          </span>

          {/* Hover 操作按钮（拖拽中隐藏；onPointerDown 阻止冒泡，长按按钮不触发拖拽）。
              用 opacity 而非 display：位置一直占着，hover 时服务名不会被按钮挤动。
              运行中与失败共用同一组按钮（日志/重启/停止）——失败态也要能"停止"：
              它既是收尾（结束残留进程，含另开窗口的游离进程），也是清掉红点的入口。
              键盘可达用 `:has(:focus-visible)` 而不是 `focus-within`：鼠标点过按钮后焦点
              留在按钮上，`focus-within` 会让这一排按钮在鼠标移开后一直亮着（点「日志」
              打开面板后尤其明显）；`:focus-visible` 只认真键盘焦点。 */}
          <div
            className={`flex items-center gap-1 opacity-0 flex-shrink-0 has-[:focus-visible]:opacity-100 ${isDragging ? '' : 'group-hover:opacity-100'}`}
            onPointerDown={(e) => e.stopPropagation()}
          >
            {running || failed ? (
              <>
                {running && (openResolution.kind === 'open' || openResolution.kind === 'choose') && (
                  <button
                    className="px-2 py-1 text-[11px] bg-nexus-accent/15 text-nexus-accent rounded hover:bg-nexus-accent/25"
                    onPointerDown={e => e.stopPropagation()}
                    onClick={e => {
                      e.stopPropagation();
                      if (openResolution.kind === 'open') openPort(openResolution.port);
                      // 多端口无法唯一确定：打开弹层让用户选，不替用户猜
                      else toggleHealth(e);
                    }}
                    title={openResolution.kind === 'open'
                      ? `在浏览器中打开 http://localhost:${openResolution.port}`
                      : `有 ${openResolution.ports.length} 个监听端口，点击选择要打开的那个`}
                  >打开</button>
                )}
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

      {/* 健康详情弹层：portal 到 body，避免被服务列/滚动容器裁剪 */}
      {healthOpen && healthPos && runningInfo && createPortal(
        <div
          ref={healthPopoverRef}
          role="dialog"
          aria-label={`${service.name} 服务健康`}
          className="fixed z-[70] bg-nexus-surface border border-nexus-border/60 rounded-lg shadow-2xl p-3"
          style={{ left: healthPos.left, top: healthPos.top, width: healthPos.width }}
        >
          <p className="text-[12px] font-medium text-nexus-text truncate" title={service.name}>
            {service.name} · 服务健康
          </p>
          <dl className="mt-2 space-y-1 text-[11px] text-nexus-muted">
            <div className="flex gap-2">
              <dt className="w-[52px] flex-shrink-0">状态</dt>
              <dd className={phaseWarn ? 'text-nexus-warning' : 'text-nexus-success'}>{phaseLabel}</dd>
            </div>
            <div className="flex gap-2">
              <dt className="w-[52px] flex-shrink-0">监听端口</dt>
              <dd className="min-w-0 break-words">{ports.length > 0 ? ports.join('、') : '未发现'}</dd>
            </div>
            <div className="flex gap-2">
              <dt className="w-[52px] flex-shrink-0">来源</dt>
              <dd>{runningInfo.source === 'boot' ? '开机启动器' : 'Nexus 启动'}</dd>
            </div>
          </dl>
          {service.open_enabled && (service.open_port ?? 0) > 0 && ports.length > 0
            && !ports.includes(service.open_port!) && (
            <p className="mt-2 text-[10.5px] text-nexus-warning leading-relaxed">
              指定端口 {service.open_port} 未监听，当前监听 {ports.join('、')}。
            </p>
          )}
          <p className="mt-2 text-[10px] text-nexus-muted/70 leading-relaxed">
            {runningInfo.source === 'boot'
              ? '就绪阶段采自开机启动器的状态记录。'
              : '未配置健康检查：进程存活超过 1 秒即视为已就绪；端口为自动发现（仅 TCP）。'}
          </p>
          {service.open_enabled && ports.length > 0 && (
            <div className="mt-2 space-y-1">
              {ports.map(port => (
                <button
                  key={port}
                  className="w-full flex items-center justify-between gap-2 px-2 py-1.5 text-[11px] bg-nexus-accent/10 text-nexus-text rounded hover:bg-nexus-accent/20 transition-colors"
                  onClick={() => { openPort(port); setHealthOpen(false); }}
                >
                  <span className="truncate">http://localhost:{port}</span>
                  <span className="text-nexus-accent flex-shrink-0">打开</span>
                </button>
              ))}
            </div>
          )}
          {service.open_enabled && ports.length === 0 && (
            <p className="mt-2 text-[10.5px] text-nexus-muted/60">未发现监听端口，暂时无法打开。</p>
          )}
        </div>,
        document.body,
      )}

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
