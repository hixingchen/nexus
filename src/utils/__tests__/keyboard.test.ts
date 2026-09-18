import { test } from 'node:test';
import assert from 'node:assert/strict';

import { isSubmitEnter } from '../keyboard.ts';

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
