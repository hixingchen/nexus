import { cloneElement, isValidElement, useMemo, useRef } from 'react';
import type { ReactElement, ReactNode } from 'react';
import { useContextMenuPosition } from '../../hooks/useContextMenuPosition';
import { useClickOutside } from '../../hooks/useClickOutside';

/**
 * 右键菜单原语：容器（定位 + 关闭）与行按钮（图标 + 文案）。
 *
 * 抽它的理由（审计 CQ-14）：容器样式串 6 处、行按钮样式串 27 处、图标盒样式串 13 处
 * 此前各写一遍。服务条目里那份行按钮（原 MenuItem）本来是对的，但没有被复制出去，
 * 其余站点继续各写各的——改一次观感要改十几处，且必然改漏。
 *
 * 与 useContextMenuPosition 的分工：钩子只管「实测尺寸后夹进可用区」的算法，ref 留在
 * 本组件——每个菜单要量的是自己的尺寸，量完才知道往哪个方向夹。宽度常量（w-[180px]）
 * 因此不必再与 JS 同步：它只决定菜单视觉宽度，夹取用的是实测值。
 *
 * 关闭行为只有「点击外部」一种：6 处容器逐处核对过，都没有 Escape / scroll（捕获）/
 * 双 ref 的差异（对照 useClickOutside 的选项说明），所以不预置当前用不上的开关——
 * 将来谁需要再照着钩子的选项加，加的时候能看见这里为什么当初没加。
 */

/** 菜单容器：6 处容器逐字相同（编辑器标签那份是 z-[200] w-[170px]，语义不同故未并入） */
const MENU_BOX =
  'fixed z-[70] w-[180px] bg-nexus-surface border border-nexus-border/60 rounded-lg shadow-2xl overflow-hidden';

/** 图标盒：20×20 底衬（文件树 / 项目列表的条目用） */
const ICON_BOX =
  'w-5 h-5 rounded bg-nexus-bg border border-nexus-border/30 flex items-center justify-center flex-shrink-0';

/**
 * 色调档。四组类名一次说清「这个档长什么样」，站点只挑档不改类名：
 * - accent：图标静默、Hover 变主色 —— 默认档（文件树 / 项目列表 / 服务 / 模板 / 标签的大多数条目）
 * - danger：图标与文案都静默、Hover 变错误色 —— 删除类条目（删除项目 / 删除服务 / 删除模板）
 * - muted：图标恒静默、文案正文色（Hover 只变底色）—— 标签菜单「在目录树中定位」
 * - info / success / warning / error：图标恒用该色调色 —— 服务菜单的动作条目，与服务卡片按钮同色
 *
 * group 标记决定图标能不能随 Hover 着色：icon 里带 group-hover: 的档必须给按钮加 group，
 * 否则那些类名是死的。muted 档特意不加：它原样保留了那段 group-hover:，但原来的按钮就没有
 * group（那条规则从未生效过），保留死类是为了让类名与迁移前逐字一致。
 *
 * box 只对图标盒档有意义（图标盒边框的 Hover 色）。
 */
const TONE = {
  accent: {
    hover: 'hover:bg-nexus-accent/10',
    icon: 'text-nexus-muted group-hover:text-nexus-accent',
    box: 'group-hover:border-nexus-accent/30',
    label: 'text-nexus-text',
    group: true,
  },
  danger: {
    hover: 'hover:bg-nexus-error/10',
    icon: 'text-nexus-muted group-hover:text-nexus-error',
    box: 'group-hover:border-nexus-error/30',
    label: 'text-nexus-muted group-hover:text-nexus-error',
    group: true,
  },
  muted: {
    hover: 'hover:bg-nexus-accent/10',
    icon: 'text-nexus-muted group-hover:text-nexus-accent',
    box: '',
    label: 'text-nexus-text',
    group: false,
  },
  info: { hover: 'hover:bg-nexus-info/10', icon: 'text-nexus-info', box: '', label: 'text-nexus-text', group: false },
  success: { hover: 'hover:bg-nexus-success/10', icon: 'text-nexus-success', box: '', label: 'text-nexus-text', group: false },
  warning: { hover: 'hover:bg-nexus-warning/10', icon: 'text-nexus-warning', box: '', label: 'text-nexus-text', group: false },
  error: { hover: 'hover:bg-nexus-error/10', icon: 'text-nexus-error', box: '', label: 'text-nexus-text', group: false },
} as const;

type ContextMenuTone = keyof typeof TONE;

/**
 * 把色调类名落到图标上。
 * shrink：图标自己要带 flex-shrink-0 吗——图标盒档由盒子承担（盒子是 flex 子项），
 * 裸图标档要自己带（svg 就是 flex 子项）。
 */
function withIconClass(icon: ReactNode, cls: string, shrink: boolean): ReactNode {
  // svg 元素：类名直接合并进去，DOM 与迁移前一致。不外包 span——inline svg 从 flex 子项
  // 变成 span 里的行内元素后，行盒会被字体撑高（约 2px），基线对齐也偏 1~2px
  if (isValidElement(icon) && typeof icon.type === 'string') {
    return cloneElement(
      icon as ReactElement<{ className?: string }>,
      { className: shrink ? `${cls} flex-shrink-0` : cls },
    );
  }
  // 自定义图标组件没有 className 入口（服务菜单的 LogIcon 等组件不接 props），只能外包一层
  // 着色——它们原本就是包着的（原 MenuItem 的 <span className={tone.icon}>{icon}</span>）
  return <span className={shrink ? `flex-shrink-0 ${cls}` : cls}>{icon}</span>;
}

interface ContextMenuProps {
  /** 点击点坐标：先按它渲染，挂载后由钩子实测尺寸再夹进可用区 */
  x: number;
  y: number;
  /** 点击菜单外部：菜单条目的关闭由各自 onClick 负责（与原实现一致，两处都要） */
  onClose: () => void;
  /** 额外容器样式：空白区菜单的内边距原先写在容器上（py-1.5 px-1.5） */
  className?: string;
  children: ReactNode;
}

export function ContextMenu({ x, y, onClose, className, children }: ContextMenuProps) {
  const menuRef = useRef<HTMLDivElement | null>(null);
  useClickOutside(menuRef, onClose);

  // anchor 传菜单坐标对象：每次右键都是新对象 → 打开时重新测量；打开期间引用稳定 → 不反复测量。
  // 菜单关闭即卸载，下次打开是全新挂载，memo 不会跨次复用旧坐标
  const anchor = useMemo(() => ({ x, y }), [x, y]);
  const style = useContextMenuPosition(menuRef, anchor);

  return (
    <div ref={menuRef} className={className ? `${MENU_BOX} ${className}` : MENU_BOX} style={style}>
      {children}
    </div>
  );
}

interface ContextMenuItemProps {
  label: string;
  /** 图标：传 svg 元素（色调类名由原语合并进去）或自定义图标组件（外包一层着色）；不传则只有文案 */
  icon?: ReactNode;
  /** 图标盒（20×20 底衬）：文件树 / 项目列表的条目用，其余用裸图标 */
  iconBox?: boolean;
  /** 色调档（默认 accent），见 TONE */
  tone?: ContextMenuTone;
  /** 长文案截断（工具命令名、绑定的工具名可能很长） */
  truncate?: boolean;
  /** 禁用态。只有传了本 prop 的站点才带禁用样式：不传的站点原实现也没有这串类名，
   *  统一带上会让 26 处类名与迁移前不一致（`:disabled` 选不中时它虽是死类，但没必要） */
  disabled?: boolean;
  onClick?: () => void;
}

export function ContextMenuItem({ label, icon, iconBox, tone = 'accent', truncate, disabled, onClick }: ContextMenuItemProps) {
  const t = TONE[tone];
  // group 标记：图标确实靠 group-hover: 着色才加（没有图标的行加了没用，白改类名）
  const group = icon && t.group ? ' group' : '';
  const disabledCls = disabled !== undefined ? ' disabled:opacity-40 disabled:hover:bg-transparent' : '';

  return (
    <button
      className={`w-full flex items-center gap-2 px-2.5 py-1.5 rounded-md ${t.hover} transition-colors${group} text-left${disabledCls}`}
      disabled={disabled}
      onClick={onClick}
    >
      {icon && (iconBox
        ? <div className={`${ICON_BOX} ${t.box}`}>{withIconClass(icon, t.icon, false)}</div>
        : withIconClass(icon, t.icon, true))}
      <span className={`text-[12px] ${t.label}${truncate ? ' truncate' : ''}`}>{label}</span>
    </button>
  );
}
