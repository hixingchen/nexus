import { useEffect, useRef } from 'react';
import type { RefObject } from 'react';

/** 事件目标是否落在某元素内（target 可能是非 Node：断言 as Node 会在极端目标上抛错） */
function containsTarget(el: HTMLElement | null, target: EventTarget | null): boolean {
  return el !== null && target instanceof Node && el.contains(target);
}

interface ClickOutsideOptions {
  /**
   * Escape 关闭（默认关）。只有显式需要键盘关闭的浮层才开——右键菜单原本没有这个
   * 行为，统一加上会变成"顺手改功能"而不是重构。
   */
  escape?: boolean;
  /**
   * 捕获阶段监听 scroll（默认关）。浮层的滚动容器不是 window 时必须开：scroll 不冒泡，
   * 冒泡阶段收不到内层容器的滚动。
   */
  captureScroll?: boolean;
  /** 主 ref 之外的"内部"区域（如浮层的触发器）：事件落在这些区域同样不关闭 */
  refs?: ReadonlyArray<RefObject<HTMLElement | null>>;
}

/**
 * 浮层「点击外部关闭」。
 *
 * 为什么抽成 hook：这段 effect 原先在 9 个右键菜单/下拉浮层里各写一遍，差异（是否带
 * Escape、是否带 scroll、单/双 ref）散在各自实现里——新增浮层的作者无从知道存在哪些
 * 约定，加一条全局规则（如"Escape 关闭右键菜单"）要改 9 处。
 *
 * 用 mousedown 而非 click：右键新目标触发的是 contextmenu（不触发 click），用 click
 * 会让旧浮层残留（同一时间出现两个）。
 *
 * 主 ref 未挂载（浮层已卸载）时事件一律忽略——原实现在闭包里都带这个隐含条件。
 * 监听因此常驻（没复刻各处的 `if (!菜单) return` 守卫）：守卫省掉的只是"关闭态挂一个
 * 监听"的开销，行为一致；关掉的是 no-op 的 setState（React 对同值更新直接跳过）。
 */
export function useClickOutside(
  ref: RefObject<HTMLElement | null>,
  onClose: () => void,
  options: ClickOutsideOptions = {},
): void {
  const { escape = false, captureScroll = false, refs } = options;
  // 监听只在 [ref, escape, captureScroll] 变化时重挂，所以 onClose/refs 必须经 ref 取最新：
  // 调用方传的多是内联箭头与内联数组，进依赖会让每次渲染都重挂一遍 document 监听
  const latest = useRef({ onClose, refs });
  useEffect(() => { latest.current = { onClose, refs }; });

  useEffect(() => {
    const inside = (target: EventTarget | null): boolean => {
      if (containsTarget(ref.current, target)) return true;
      for (const r of latest.current.refs ?? []) {
        if (containsTarget(r.current, target)) return true;
      }
      return false;
    };
    const close = () => latest.current.onClose();

    const onMouseDown = (e: MouseEvent) => {
      if (!ref.current) return;
      if (!inside(e.target)) close();
    };
    document.addEventListener('mousedown', onMouseDown);

    // scroll 只判浮层自身：触发器（refs）不会产生 scroll 事件，原实现同样不检查它
    const onScroll = captureScroll
      ? (e: Event) => {
          if (!ref.current) return;
          if (!containsTarget(ref.current, e.target)) close();
        }
      : null;
    if (onScroll) document.addEventListener('scroll', onScroll, true);

    const onKey = escape
      ? (e: KeyboardEvent) => { if (e.key === 'Escape') close(); }
      : null;
    if (onKey) document.addEventListener('keydown', onKey);

    return () => {
      document.removeEventListener('mousedown', onMouseDown);
      if (onScroll) document.removeEventListener('scroll', onScroll, true);
      if (onKey) document.removeEventListener('keydown', onKey);
    };
  }, [ref, escape, captureScroll]);
}
