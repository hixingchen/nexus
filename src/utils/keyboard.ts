import type { KeyboardEvent } from 'react';

/**
 * 回车提交守卫：中文/日文输入法用回车确认候选词时，keydown 也会带着 key==='Enter'
 * 冒到 onKeyDown——不加 isComposing 判断就会把「确认候选词」当成「提交表单」
 * （编辑项目弹窗里表现为当场改名并关闭）。
 * 表单类输入框的 onKeyDown 一律用它判提交。
 */
export function isSubmitEnter(e: KeyboardEvent): boolean {
  return e.key === 'Enter' && !e.nativeEvent.isComposing;
}

/**
 * Ctrl+F 全局拦截的放行判定：事件目标在 CodeMirror 内容区（`.cm-content`）内 → true。
 *
 * 为什么必须放行：`@codemirror/view` 的 keymap 在 `eventBelongsToEditor` 里会**丢弃
 * `defaultPrevented` 的 keydown**。全局监听在捕获阶段先 `preventDefault()` 的话，
 * 编辑器根本收不到这次按键，`Mod-f` 绑定不会执行——症状是"光标在编辑器里时 Ctrl+F
 * 毫无反应"。放行后由编辑器自己的 `Mod-f` 处理并 `preventDefault()`，原生查找框
 * 同样不会弹出，两边不冲突。
 *
 * 只放行 `.cm-content`（编辑器正文），不放行整个 `.cm-editor`：面板输入框等区域
 * 另有处理器，若放行它们反而会把原生查找框放出来。
 */
export function isEditorContentTarget(target: EventTarget | null): boolean {
  if (!target || typeof (target as Element).closest !== 'function') return false;
  return (target as Element).closest('.cm-content') !== null;
}

/**
 * 方向键在项目列表的条目之间移动焦点（UX-7），返回是否移动了。
 *
 * 为什么以 `[data-project-id]` 为锚：完整列表的卡片是 `div[tabindex]`、窄轨是 `button`，
 * 两种 DOM 长得完全不同，但都带这个属性——它是"这一格是哪个项目"的既有标记
 * （`ProjectList` 也用它做滚动定位）。焦点落到条目内部第一个可聚焦元素上；
 * 标记本身就可聚焦时（窄轨）就是它自己。
 *
 * 为什么不用 `document.querySelectorAll` 一把捞：列表与窄轨各有自己的容器，
 * 页面级查询会把两边混成一条序列（收起态切换时尤其明显）。
 */
export function focusProjectStep(current: HTMLElement, step: 1 | -1): boolean {
  const item = current.closest('[data-project-id]');
  if (!item) return false;
  for (
    let s = step === 1 ? item.nextElementSibling : item.previousElementSibling;
    s;
    s = step === 1 ? s.nextElementSibling : s.previousElementSibling
  ) {
    // 收藏分隔线之类的非条目节点：跳过（它们没有 data-project-id）
    if (!s.matches('[data-project-id]')) continue;
    const target = s.querySelector<HTMLElement>('[tabindex="0"]') ?? (s as HTMLElement);
    target.focus();
    return true;
  }
  return false;
}
