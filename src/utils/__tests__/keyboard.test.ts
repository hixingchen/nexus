import { test } from 'node:test';
import assert from 'node:assert/strict';

import { isEditorContentTarget, isSubmitEnter } from '../keyboard.ts';

/**
 * keyboard.ts 只 import 了 react 的**类型**，运行期无依赖，所以能直接在 node 下跑。
 * 这里守的是输入法场景：中日文输入法用回车确认候选词时，keydown 同样带
 * key === 'Enter'，若不看 isComposing 就会把"确认候选词"当成"提交表单"。
 */

type KeyEvent = Parameters<typeof isSubmitEnter>[0];

/** 只构造被测代码真正读的两个字段：key 与 nativeEvent.isComposing */
function keyEvent(key: string, isComposing: boolean): KeyEvent {
  return { key, nativeEvent: { isComposing } } as unknown as KeyEvent;
}

test('普通回车（非输入法）判定为提交', () => {
  assert.equal(isSubmitEnter(keyEvent('Enter', false)), true);
});

test('输入法组合中的回车不算提交——这是本函数存在的唯一理由', () => {
  assert.equal(isSubmitEnter(keyEvent('Enter', true)), false);
});

test('组合结束后的下一次回车恢复为提交（isComposing 是每帧的实时状态）', () => {
  assert.equal(isSubmitEnter(keyEvent('Enter', true)), false);
  assert.equal(isSubmitEnter(keyEvent('Enter', false)), true);
});

test('其它按键一律不提交（Escape / 空格 / 字母）', () => {
  assert.equal(isSubmitEnter(keyEvent('Escape', false)), false);
  assert.equal(isSubmitEnter(keyEvent(' ', false)), false);
  assert.equal(isSubmitEnter(keyEvent('a', false)), false);
});

test('小写 enter 不算（key 是大小写敏感的，不能改成不敏感判断）', () => {
  assert.equal(isSubmitEnter(keyEvent('enter', false)), false);
});

test('小键盘 Enter（NumpadEnter）不提交——与浏览器 key 取值一致的真实行为', () => {
  // 若哪天要支持小键盘回车，这条会红，提醒改动是有意为之而不是顺手加的。
  assert.equal(isSubmitEnter(keyEvent('NumpadEnter', false)), false);
});

test('输入法组合中按其它键不提交（组合期的 key 可能是 Process）', () => {
  assert.equal(isSubmitEnter(keyEvent('Process', true)), false);
});

/**
 * Ctrl+F 回归：main.tsx 的全局 preventDefault 一度让 CodeMirror 的 keymap 收不到按键
 * （@codemirror/view 会丢弃 defaultPrevented 的 keydown）。这里锁住"只有 .cm-content
 * 放行"这条判据——放行范围一放宽就会把原生查找框放出来，一收紧则编辑器内 Ctrl+F 失效。
 */
function fakeTarget(closestResult: object | null): EventTarget {
  return { closest: () => closestResult } as unknown as EventTarget;
}

test('事件目标在 CodeMirror 正文内 → 放行（交给编辑器 Mod-f）', () => {
  assert.equal(isEditorContentTarget(fakeTarget({})), true);
});

test('事件目标在编辑器外（文件树 / 标签栏 / body）→ 不拦截判据成立', () => {
  assert.equal(isEditorContentTarget(fakeTarget(null)), false);
});

test('target 为 null 或不是元素（window 等）→ 按"不在正文内"处理', () => {
  assert.equal(isEditorContentTarget(null), false);
  assert.equal(isEditorContentTarget({} as EventTarget), false);
});
