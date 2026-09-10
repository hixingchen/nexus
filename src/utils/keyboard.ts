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
