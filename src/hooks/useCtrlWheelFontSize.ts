import { useEffect, type RefObject } from 'react';
import { LAYOUT_KEYS, saveLayout, useLayoutStore } from '../stores/layoutStore';
import { parseEditorFontSize, stepEditorFontSize } from '../utils/editorFont';
import { createWheelZoomHandler } from '../utils/wheelFontZoom';

/**
 * 在指定区域里用 **Ctrl（macOS 也认 Cmd）+ 滚轮**调「文本字号」。
 *
 * 字号写回 `LAYOUT_KEYS.editorFontSize`（与设置 > 编辑器同一个键）：代码视图、
 * Markdown 预览、日志面板读的都是它，所以三处同步，且下次启动仍生效。
 *
 * 为什么必须挂原生 `wheel` 而不是 React `onWheel`：React 的 wheel 监听是**被动**的，
 * `preventDefault()` 不生效——WebView2 会顺带把整个页面缩放掉（用户看到的是"界面变大"，
 * 而不是字号变了）。这里用 `{ passive: false }` 的原生监听截住默认行为；只有按住
 * 修饰键时才拦，普通滚动完全不受影响。
 *
 * 注意 `ref` 指向的必须是**常驻**的容器（滚动区/编辑器外层）。挂载后再出现的元素
 * 不会重新绑定——本仓这三处（CodeViewer / MarkdownPreview / LogViewer）的容器都是常驻的。
 */
export function useCtrlWheelFontSize<T extends HTMLElement>(ref: RefObject<T>) {
  useEffect(() => {
    const el = ref.current;
    if (!el) return;

    // 修饰键判断 / 增量累计 / preventDefault 在纯函数里（见 utils/wheelFontZoom.ts）
    const onWheel = createWheelZoomHandler((deltaY) => {
      const current = parseEditorFontSize(
        useLayoutStore.getState().values?.[LAYOUT_KEYS.editorFontSize],
      );
      const next = stepEditorFontSize(current, deltaY);
      if (next !== current) {
        // 走统一写入口：同步进 store（三处读到的同一个值）+ 500ms 防抖落库
        saveLayout({ [LAYOUT_KEYS.editorFontSize]: String(next) });
      }
    });

    el.addEventListener('wheel', onWheel, { passive: false });
    return () => el.removeEventListener('wheel', onWheel);
  }, [ref]);
}
