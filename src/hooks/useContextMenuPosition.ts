import { useLayoutEffect, useState } from 'react';
import { useAiPanelReserve } from './useAiPanelReserve';

/** 菜单与可用区边缘的最小间距 */
const EDGE_GAP = 8;

/**
 * 右键菜单/浮层定位：先按点击点渲染，挂载后**实测自身尺寸**再夹进「可用区」内。
 *
 * 为什么不用固定像素估算：菜单高度随条目数变化（工具命令数、模板项、以后新增的菜单项），
 * 估算一旦偏小，贴屏幕底部弹出的菜单最后几项会被切掉且点不到；实测一次即可，
 * 也不用维护任何魔数。
 *
 * 可用区 = 窗口 −（右侧）AI 面板占用宽度：面板是原生子 WebView，盖在所有 DOM
 * 之上——只夹进窗口的话，在编辑器标签等靠右位置右键，菜单右半会伸到面板下面，
 * 既看不见也点不到。窄窗口下面板比内容区还宽时菜单放不下，此时退回左缘对齐
 * （至少左半可见可点），不做进一步压缩。
 *
 * anchor 传菜单状态对象本身（每次右键都是新对象 → 每次打开重新测量；
 * 打开期间对象引用稳定 → 不会反复测量）。测量在读布局后于 paint 前完成，不会看到跳动。
 */
export function useContextMenuPosition<T extends { x: number; y: number }, E extends HTMLElement>(
  ref: { readonly current: E | null },
  anchor: T | null,
): { left: number; top: number } {
  const [pos, setPos] = useState({ left: anchor?.x ?? 0, top: anchor?.y ?? 0 });
  const panelReserve = useAiPanelReserve();

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el || !anchor) return;
    const { width, height } = el.getBoundingClientRect();
    const rightBound = window.innerWidth - panelReserve;
    setPos({
      left: Math.max(EDGE_GAP, Math.min(anchor.x, rightBound - width - EDGE_GAP)),
      top: Math.max(EDGE_GAP, Math.min(anchor.y, window.innerHeight - height - EDGE_GAP)),
    });
  }, [ref, anchor, panelReserve]);

  return pos;
}
