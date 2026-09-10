import { useLayoutEffect, useState } from 'react';

/** 菜单与窗口边缘的最小间距 */
const EDGE_GAP = 8;

/**
 * 右键菜单/浮层定位：先按点击点渲染，挂载后**实测自身尺寸**再夹进取窗口内。
 *
 * 为什么不用固定像素估算：菜单高度随条目数变化（工具命令数、模板项、以后新增的菜单项），
 * 估算一旦偏小，贴屏幕底部弹出的菜单最后几项会被切掉且点不到；实测一次即可，
 * 也不用维护任何魔数。
 *
 * anchor 传菜单状态对象本身（每次右键都是新对象 → 每次打开重新测量；
 * 打开期间对象引用稳定 → 不会反复测量）。测量在读布局后于 paint 前完成，不会看到跳动。
 */
export function useContextMenuPosition<T extends { x: number; y: number }, E extends HTMLElement>(
  ref: { readonly current: E | null },
  anchor: T | null,
  gap = EDGE_GAP,
): { left: number; top: number } {
  const [pos, setPos] = useState({ left: anchor?.x ?? 0, top: anchor?.y ?? 0 });

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el || !anchor) return;
    const { width, height } = el.getBoundingClientRect();
    setPos({
      left: Math.max(gap, Math.min(anchor.x, window.innerWidth - width - gap)),
      top: Math.max(gap, Math.min(anchor.y, window.innerHeight - height - gap)),
    });
  }, [ref, anchor, gap]);

  return pos;
}
