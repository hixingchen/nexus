import { ContextMenu, ContextMenuItem } from '../ui/ContextMenu';

/**
 * 项目条目的右键菜单，两个视图共用：左侧完整列表（`ProjectList`）与它收起后的窄轨（`ProjectRail`）。
 *
 * 抽它的理由与 `ui/ContextMenu` 当初抽原语同源：这两处菜单必须**逐项一致**——用户看到的
 * 应该是"同一个菜单出现在两个地方"，不是两个各自长出来的菜单。复制一份 JSX 的代价不是今天
 * 出错，而是以后改文案/换图标时必漏一处，于是同一个动作在两个视图里长得不一样。
 *
 * 前四项（启停 / 收藏 / 资源管理器 / 终端）两处都有；复制/编辑/删除只有完整列表有，
 * 对应的 `onXxx` 不传即不渲染——窄轨没有这些入口，也不该为一个右键挂上三个编辑弹窗。
 *
 * 组件只负责渲染与"点完关菜单"：状态（运行中/已收藏）由调用点**现查**后传入，
 * 动作由调用点通过 `useProjectActions` 接通。它不自己取数，因此不关心菜单是从哪个视图开的。
 */

/* 图标与卡片/服务菜单同源：启动是实心三角、停止是实心方块，用户认的是形状不是位置 */
const PLAY_ICON = (
  <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
    <polygon points="3,1.2 3,8.8 8.6,5" fill="currentColor" />
  </svg>
);

const STOP_ICON = (
  <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
    <rect x="2" y="2" width="6" height="6" rx="1" fill="currentColor" />
  </svg>
);

/** 收藏：与卡片同源的那段 path——实心=已收藏，文案才是状态的出口 */
const starIcon = (pinned: boolean) => (
  <svg width="10" height="10" viewBox="0 0 12 12" fill={pinned ? 'currentColor' : 'none'}
    stroke="currentColor" strokeWidth="1.5" strokeLinejoin="round">
    <path d="M7.5 1.5L10.5 4.5 8 7l1.5 4-7-7L7 2.5l.5-1z" />
  </svg>
);

const EXPLORER_ICON = (
  <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
    <path d="M1.5 3h2l1-1.5h4a1 1 0 011 1v5.5a1 1 0 01-1 1h-7a1 1 0 01-1-1V3z" />
  </svg>
);

const TERMINAL_ICON = (
  <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
    <path d="M1.5 2.5l3.5 2.5-3.5 2.5" /><line x1="6.5" y1="8" x2="8.5" y2="8" />
  </svg>
);

const COPY_ICON = (
  <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
    <rect x="2.5" y="3" width="5" height="5.5" rx=".8" />
    <path d="M2 2.5v4.5h.5V3.5h4V2.5H3a.5.5 0 00-.5.5z" />
  </svg>
);

const EDIT_ICON = (
  <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
    <path d="M7 2l1 1-5.5 5.5H1.5V7.5L7 2z" />
  </svg>
);

const TRASH_ICON = (
  <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
    <path d="M2.5 3h5M3.5 3V2a.5.5 0 01.5-.5h2a.5.5 0 01.5.5v1M4 4.5v3M6 4.5v3M3 3l.5 6a1 1 0 001 .5h3a1 1 0 001-.5L9 3" />
  </svg>
);

interface ProjectContextMenuProps {
  /** 点击点坐标（先按它渲染，挂载后由 ContextMenu 实测尺寸夹进可用区） */
  x: number;
  y: number;
  /** 目标项目当前是否收藏：调用点现查（菜单可能开着好几秒，列表会重新加载） */
  pinned: boolean;
  /** 目标项目当前是否运行：同上，且运行状态本来就由轮询实时变 */
  running: boolean;
  /** 启停进行中：对应条目禁用（与卡片按钮同一口径，避免第二次启动） */
  busy: boolean;
  onClose: () => void;
  onStart: () => void;
  onStop: () => void;
  onTogglePin: () => void;
  onOpenExplorer: () => void;
  onOpenTerminal: () => void;
  /** 复制/编辑/删除：只有完整列表传（窄轨不传，这三项随之不渲染） */
  onDuplicate?: () => void;
  onEdit?: () => void;
  onDelete?: () => void;
}

export function ProjectContextMenu({
  x, y, pinned, running, busy,
  onClose, onStart, onStop, onTogglePin, onOpenExplorer, onOpenTerminal,
  onDuplicate, onEdit, onDelete,
}: ProjectContextMenuProps) {
  /** 所有条目共用的收尾：先执行动作再关菜单（与各站点迁移前的写法一致） */
  const run = (fn: () => void) => { fn(); onClose(); };

  return (
    <ContextMenu x={x} y={y} onClose={onClose}>
      {/* 项目动作：与卡片右侧那两个按钮一一对应（运行中=停止，否则=启动；收藏按当前状态切文案）。
          放最上面：这两个是最高频的动作，与服务菜单把"启动/停止"排在最前是同一取舍 */}
      <div className="py-1.5 px-1.5">
        {running ? (
          <ContextMenuItem iconBox tone="error" icon={STOP_ICON} label="停止项目"
            disabled={busy} onClick={() => run(onStop)} />
        ) : (
          <ContextMenuItem iconBox tone="success" icon={PLAY_ICON} label="启动项目"
            disabled={busy} onClick={() => run(onStart)} />
        )}
        <ContextMenuItem iconBox icon={starIcon(pinned)} label={pinned ? '取消收藏' : '收藏'}
          onClick={() => run(onTogglePin)} />
      </div>

      {/* 定位（两个视图都有）与编辑（只有完整列表有） */}
      <div className="border-t border-nexus-border/30 py-1.5 px-1.5">
        <ContextMenuItem iconBox icon={EXPLORER_ICON} label="在资源管理器中打开" onClick={() => run(onOpenExplorer)} />
        <ContextMenuItem iconBox icon={TERMINAL_ICON} label="打开终端" onClick={() => run(onOpenTerminal)} />
        {onDuplicate && <ContextMenuItem iconBox icon={COPY_ICON} label="复制项目" onClick={() => run(onDuplicate)} />}
        {onEdit && <ContextMenuItem iconBox icon={EDIT_ICON} label="编辑项目" onClick={() => run(onEdit)} />}
      </div>

      {onDelete && (
        <div className="border-t border-nexus-border/30 py-1.5 px-1.5">
          <ContextMenuItem iconBox tone="danger" icon={TRASH_ICON} label="删除项目" onClick={() => run(onDelete)} />
        </div>
      )}
    </ContextMenu>
  );
}
