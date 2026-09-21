import { ContextMenu, ContextMenuItem } from '../ui/ContextMenu';
import { type ToolCommand } from '../../services/service';

/**
 * 服务条目的右键菜单，两个视图共用：服务列展开时的卡片（`ServiceTreeEntry`）
 * 与收起后的圆点条（`ProjectDetail` 的 `CollapsedView`）。
 *
 * 抽它的理由与 `ProjectContextMenu` 一致：两处菜单必须**逐项一致**，复制一份 JSX 的代价
 * 不是今天出错，而是改文案/换图标时必漏一处。差异靠"传不传某个 onXxx"表达，不靠两份代码：
 * - `onViewLog` 不传 → 没有日志入口（收起态由点击圆点承担，但那边照样传了，见下）
 * - `onDelete` 不传 → **不渲染删除服务**：收起态只有 24px 的圆点，误触代价高
 * - `openToolName` / `toolCommands` 为空 → 对应分组自然消失
 *
 * 组件只负责渲染与"点完关菜单"：动作由调用点接（`useServiceActions`），
 * 运行/失败状态由调用点算好传入。
 */

/** 查看日志：文档 + 折角（失败态复用同一图标，靠色调区分） */
const LogIcon = () => (
  <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
    <path d="M2 1.5h3L7.5 4v4.5H2z" /><path d="M5 1.5V4h2.5" />
  </svg>
);

/** 重启：环形箭头（与卡片 ↻ 同义） */
const RestartIcon = () => (
  <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1">
    <path d="M8.75 5a3.75 3.75 0 1 1-3.75-3.75c1.05 0 2.05.42 2.81 1.14L8.75 3.33" />
    <path d="M8.75 1.25v2.08H6.67" />
  </svg>
);

/** 启动：实心三角（与卡片 ▶ 同形，也和"工具命令"条目同义） */
const PlayIcon = () => (
  <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
    <polygon points="3,1.2 3,8.8 8.6,5" fill="currentColor" />
  </svg>
);

/** 跟随日志：文档 + 右下角折线（与"查看日志"区分：那个是折角的文档） */
const FileIcon = () => (
  <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
    <path d="M2 1.5h3L7.5 4v4.5H2z" /><path d="M5 1.5V4h2.5" />
    <path d="M3 7.5h1.2l.8-1.4.8 2.2.7-1.1H8" />
  </svg>
);

/** 路径最后一段（菜单里显示"跟的是哪个文件"，完整路径太长） */
function fileNameOf(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

/** 停止：实心方块（与卡片 ■ 同形） */
const StopIcon = () => (
  <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
    <rect x="2" y="2" width="6" height="6" rx="1" fill="currentColor" />
  </svg>
);

/** 编辑服务：铅笔 */
const EditIcon = () => (
  <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinejoin="round">
    <path d="M6.8 1.8l1.4 1.4-5.1 5.1-1.9.5.5-1.9z" />
  </svg>
);

interface ServiceContextMenuProps {
  x: number;
  y: number;
  /** 服务工作目录：为空则不显示"在资源管理器中打开 / 打开终端" */
  cwd: string;
  /** 运行中：显示 查看日志 / 重启服务 / 停止服务（与卡片按钮一致） */
  running: boolean;
  /** 意外失败：显示 查看失败日志 / 重新启动（与卡片按钮一致） */
  failed: boolean;
  /**
   * 正在跟随的日志文件路径（null = 没在跟随）。
   *
   * 服务用 `start` 另开窗口跑时（Tomcat 的 startup.bat）面板读的是这个文件——
   * 菜单里显示"跟的是哪个文件"并给一个取消入口，否则用户不知道那些蓝色日志从哪来。
   */
  followedLog: string | null;
  /** 取消跟随日志文件（服务照常运行） */
  onUnfollowLog: () => void;
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
  /**
   * 编辑服务：不传即不渲染。
   *
   * 失败态需要它：卡片本体被改成"点击看失败日志"（失败后第一诉求是诊断），
   * 编辑面板的入口就只剩这里了
   */
  onEdit?: () => void;
  /** 删除服务：不传即不渲染（服务列收起态就没有这个入口） */
  onDelete?: () => void;
  onClose: () => void;
}

export function ServiceContextMenu({
  x, y, cwd, running, failed, followedLog, openToolName, toolCommands, onViewLog,
  onStart, onStop, onRestart, onOpenWithTool, onOpenInExplorer, onOpenTerminal,
  onRunCommand, onUnfollowLog, onEdit, onDelete, onClose,
}: ServiceContextMenuProps) {
  /** 所有条目共用的收尾：先执行动作再关菜单（与各站点迁移前的写法一致） */
  const run = (fn: () => void) => { fn(); onClose(); };

  return (
    <ContextMenu x={x} y={y} onClose={onClose}>
      {/* 服务动作：与卡片 Hover 按钮一一对应（运行中=日志/重启/停止，失败=失败日志/重启，未运行=启动） */}
      <div className="border-b border-nexus-border/30 py-1.5 px-1.5">
        {running ? (
          <>
            {onViewLog && <ContextMenuItem icon={<LogIcon />} label="查看日志" tone="info" truncate onClick={() => run(onViewLog)} />}
            {/* 只在真的跟随时出现：文件名写进 label（截断），用户这才知道面板里那批
                蓝色日志读的是哪个文件 */}
            {followedLog && (
              <ContextMenuItem
                icon={<FileIcon />}
                label={`取消跟随日志（${fileNameOf(followedLog)}）`}
                truncate
                onClick={() => run(onUnfollowLog)}
              />
            )}
            <ContextMenuItem icon={<RestartIcon />} label="重启服务" tone="warning" truncate onClick={() => run(onRestart)} />
            <ContextMenuItem icon={<StopIcon />} label="停止服务" tone="error" truncate onClick={() => run(onStop)} />
          </>
        ) : failed ? (
          <>
            {onViewLog && <ContextMenuItem icon={<LogIcon />} label="查看失败日志" tone="error" truncate onClick={() => run(onViewLog)} />}
            <ContextMenuItem icon={<RestartIcon />} label="重新启动" tone="success" truncate onClick={() => run(onStart)} />
            {/* 失败态也要能停止：收尾残留进程（含另开窗口的游离进程）并清掉失败标记。
                与卡片那三个按钮逐项对齐（卡片 hover = 日志 / 重启 / 停止） */}
            <ContextMenuItem icon={<StopIcon />} label="停止服务" tone="error" truncate onClick={() => run(onStop)} />
            {/* 卡片本体在失败态下点击 = 看日志，所以编辑入口要在这里补上（不传的站点不渲染） */}
            {onEdit && <ContextMenuItem icon={<EditIcon />} label="编辑服务" truncate onClick={() => run(onEdit)} />}
          </>
        ) : (
          <ContextMenuItem icon={<PlayIcon />} label="启动服务" tone="success" truncate onClick={() => run(onStart)} />
        )}
      </div>

      {/* 用绑定的工具打开（服务设置中选择，如 IDEA / VS Code） */}
      {openToolName && (
        <div className="py-1.5 px-1.5">
          <ContextMenuItem
            icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
              <path d="M2 1h6a1 1 0 011 1v6a1 1 0 01-1 1H2a1 1 0 01-1-1V2a1 1 0 011-1z" /><path d="M1.5 6.5h7M3.5 6.5V9" />
            </svg>}
            label={`用 ${openToolName} 打开`}
            truncate
            onClick={() => run(onOpenWithTool)}
          />
        </div>
      )}

      {/* 打开资源管理器 / 打开终端（与上面"用工具打开"同属一个视觉分组，中间不画线——与迁移前一致） */}
      {cwd && (
        <div className="py-1.5 px-1.5">
          <ContextMenuItem
            icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
              <path d="M1.5 3h2l1-1.5h4a1 1 0 011 1v5.5a1 1 0 01-1 1h-7a1 1 0 01-1-1V3z" />
            </svg>}
            label="在资源管理器中打开"
            onClick={() => run(onOpenInExplorer)}
          />
          <ContextMenuItem
            icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
              <path d="M1.5 2.5l3.5 2.5-3.5 2.5" /><line x1="6.5" y1="8" x2="8.5" y2="8" />
            </svg>}
            label="打开终端"
            onClick={() => run(onOpenTerminal)}
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
                <polygon points="3,1 3,9 9,5" fill="currentColor" />
              </svg>}
              label={cmd.name}
              truncate
              onClick={() => run(() => onRunCommand(cmd))}
            />
          ))}
        </div>
      )}

      {/* 删除：只有展开态的服务卡片传了 onDelete */}
      {onDelete && (
        <div className="border-t border-nexus-border/30 py-1.5 px-1.5">
          <ContextMenuItem
            tone="danger"
            icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.3">
              <path d="M2.5 3h5M3.5 3V2a.5.5 0 01.5-.5h2a.5.5 0 01.5.5v1M4 4.5v3M6 4.5v3M3 3l.5 6a1 1 0 001 .5h3a1 1 0 001-.5L9 3" />
            </svg>}
            label="删除服务"
            onClick={() => run(onDelete)}
          />
        </div>
      )}
    </ContextMenu>
  );
}
