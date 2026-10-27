import { useState, useRef } from 'react';
import { createPortal } from 'react-dom';
import { openInExplorer, openTerminal } from '../../services/system';
import { useDndMonitor } from '@dnd-kit/core';
import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import type { ServiceTemplate } from '../../services/service';
import { ContextMenu, ContextMenuItem } from '../ui/ContextMenu';

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
  /** 导出这一个模板到 JSON 文件（右键菜单；发给别人或单独备份） */
  onExport: (tpl: ServiceTemplate) => void;
}

/** 模板条目：视觉与服务条目一致（圆点 + 名称 + Hover 操作），点击打开编辑面板，Hover/右键提供「添加到项目 / 导出 / 删除模板」 */
export function TemplateTreeEntry({ tpl, busy, isEditing, onEdit, onAdd, onRequestDelete, onExport }: Props) {
  // dnd-kit 可排序：长按卡片 250ms 进入拖拽（快速点击照常打开编辑面板；添加中禁用拖拽）
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: tpl.id,
    disabled: busy,
  });
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
      if (e.active.id === tpl.id && !(e.activatorEvent instanceof KeyboardEvent)) draggedRef.current = true;
    },
    onDragEnd: (e) => { if (e.active.id === tpl.id) clearDragFlagSoon(); },
    onDragCancel: (e) => { if (e.active.id === tpl.id) clearDragFlagSoon(); },
  });
  const handleClick = () => {
    if (draggedRef.current) { draggedRef.current = false; return; }
    onEdit(tpl);
  };
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);

  /**
   * 键盘分工（UX-8）：卡片经 dnd-kit 的 attributes 拿到 `role="button"` + `tabIndex=0`
   * ——辅助技术把它朗读成按钮，所以 **Enter 必须是"卡片动作"（等价点击，即打开编辑面板）**；
   * **Space 留给 dnd-kit 的键盘排序**（它的屏幕阅读器说明正是"按空格拾起"，方向键移动、
   * 再按空格/Enter 落下、Esc 取消），`KeyboardSensor` 照旧挂着，键盘排序能力不变。
   *
   * 为什么不能把 Enter 也转发给 dnd：它的激活器默认把 Enter 也算作"开始拖拽"
   * （`defaultKeyboardCodes.start = [Space, Enter]`），转发过去 Enter 就又变回了拖拽。
   */
  const handleKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    // 焦点在内层按钮（添加到项目）上时按键归那个按钮：转发给 dnd 会让 Enter 与 Space
    // 都被激活器 preventDefault 掉，这个按钮用键盘根本按不动
    if (e.target !== e.currentTarget) return;
    if (e.key === 'Enter') {
      // 拖拽进行中：Enter 是"落下"（dnd 挂在 document 上的监听会处理），不该顺手开一次面板
      if (!isDragging) { e.preventDefault(); handleClick(); }
      return;
    }
    listeners?.onKeyDown?.(e);
  };

  const handleContextMenu = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setContextMenu({ x: e.clientX, y: e.clientY });
  };

  const handleOpenInExplorer = () => {
    setContextMenu(null);
    void openInExplorer(tpl.cwd);
  };

  const handleOpenTerminal = () => {
    setContextMenu(null);
    void openTerminal(tpl.cwd);
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
        } ${busy ? 'opacity-60 pointer-events-none' : ''} ${isDragging ? 'shadow-[0_16px_48px_rgba(0,0,0,0.5)] ring-2 ring-nexus-accent/30 border-nexus-accent/50 bg-nexus-bg cursor-grabbing' : ''}`}
        onClick={handleClick}
        onKeyDown={handleKeyDown}
        onContextMenu={handleContextMenu}
        title={isDragging ? '拖拽排序' : `${tpl.command}${tpl.cwd ? ` (${tpl.cwd})` : ''}`}
      >
        <div className="flex items-center gap-2">
          {/* 状态圆点（模板无运行态，恒为灰色，与服务条目视觉一致） */}
          <span className="w-[7px] h-[7px] rounded-full flex-shrink-0 bg-nexus-muted/40" />
          <span className="flex-1 text-[13px] text-nexus-text font-medium truncate">{tpl.name}</span>

          {/* Hover 操作：添加到项目（删除在右键菜单；拖拽中隐藏；onPointerDown 阻止冒泡，长按按钮不触发拖拽） */}
          <div
            className={`flex items-center gap-1 opacity-0 flex-shrink-0 has-[:focus-visible]:opacity-100 ${isDragging ? '' : 'group-hover:opacity-100'}`}
            onPointerDown={(e) => e.stopPropagation()}
          >
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
          onExport={() => { setContextMenu(null); onExport(tpl); }}
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
  onExport: () => void;
  onClose: () => void;
}

// 容器/定位/关闭与行按钮走 ui/ContextMenu 原语：口径与服务条目菜单同源
// （原实现手写「固定 180px 宽 + 估算高度」夹取，估算偏小会让末尾条目落到屏幕外）
const TemplateContextMenu = ({ x, y, hasCwd, onOpenInExplorer, onOpenTerminal, onDelete, onExport, onClose }: TemplateContextMenuProps) => (
  <ContextMenu x={x} y={y} onClose={onClose}>
    {/* 打开资源管理器 / 打开终端 */}
    {hasCwd && (
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

    {/* 导出这一个（发给别人 / 单独备份）：与"打开位置"分开一组——那是定位，这是搬运配置。
        图标走 10×10 坐标系：菜单里其它条目都是这个规格（这条原先用了 14 的坐标系却把
        宽高写成 10，图被整体缩了一圈，跟旁边的图标不是一个粗细） */}
    <div className={`py-1.5 px-1.5 ${hasCwd ? 'border-t border-nexus-border/30' : ''}`}>
      <ContextMenuItem
        icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" strokeLinejoin="round">
          <path d="M5 7.5V2.5"/><polyline points="2.8,4.7 5,2.5 7.2,4.7"/><line x1="2" y1="8.5" x2="8" y2="8.5"/>
        </svg>}
        label="导出这个模板"
        onClick={onExport}
      />
    </div>

    {/* 删除 */}
    <div className="border-t border-nexus-border/30 py-1.5 px-1.5">
      <ContextMenuItem
        tone="danger"
        icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.3">
          <path d="M2.5 3h5M3.5 3V2a.5.5 0 01.5-.5h2a.5.5 0 01.5.5v1M4 4.5v3M6 4.5v3M3 3l.5 6a1 1 0 001 .5h3a1 1 0 001-.5L9 3"/>
        </svg>}
        label="删除模板"
        onClick={onDelete}
      />
    </div>
  </ContextMenu>
);
