import { test } from 'node:test';
import assert from 'node:assert/strict';

import { WHEEL_STEP_THRESHOLD, createWheelZoomHandler } from '../wheelFontZoom.ts';

/**
 * Ctrl+滚轮的字号事件判定。
 *
 * 这里守的是三件容易写错、且在界面上很难看出来原因的事：
 * ① 只有按住 Ctrl / Cmd 才拦默认行为（否则普通滚动被吃掉）；
 * ② 触控板的细碎增量要累计，不能一下冲到上限；
 * ③ 到上下限时仍要 `preventDefault`——否则 WebView2 会退化成整页缩放。
 */

interface Mods {
  ctrlKey?: boolean;
  metaKey?: boolean;
  altKey?: boolean;
}

function makeEvent(deltaY: number, mods: Mods = {}) {
  let prevented = false;
  const e = {
    ctrlKey: mods.ctrlKey ?? false,
    metaKey: mods.metaKey ?? false,
    altKey: mods.altKey ?? false,
    deltaY,
    preventDefault: () => {
      prevented = true;
    },
  };
  return { e, prevented: () => prevented };
}

function collectSteps() {
  const steps: number[] = [];
  const handler = createWheelZoomHandler((deltaY) => steps.push(deltaY));
  return { steps, handler };
}

test('Ctrl+滚轮向上：拦默认行为并走一步（deltaY 保持原号）', () => {
  const { steps, handler } = collectSteps();
  const { e, prevented } = makeEvent(-100, { ctrlKey: true });
  handler(e);
  assert.equal(prevented(), true, '必须拦住 WebView2 的整页缩放');
  assert.deepEqual(steps, [-100]);
});

test('macOS 的 Cmd 等价于 Ctrl', () => {
  const { steps, handler } = collectSteps();
  handler(makeEvent(-100, { metaKey: true }).e);
  assert.deepEqual(steps, [-100]);
});

test('没有修饰键 / 带 Alt：普通滚动，不拦也不改字号', () => {
  const { steps, handler } = collectSteps();
  const plain = makeEvent(-100);
  handler(plain.e);
  assert.equal(plain.prevented(), false, '普通滚动不能被吃掉');
  const alt = makeEvent(-100, { ctrlKey: true, altKey: true });
  handler(alt.e);
  assert.equal(alt.prevented(), false, 'Alt 组合留给别的功能');
  assert.deepEqual(steps, []);
});

test('横向滚轮（deltaY=0）不是缩放意图', () => {
  const { steps, handler } = collectSteps();
  const { e, prevented } = makeEvent(0, { ctrlKey: true });
  handler(e);
  assert.equal(prevented(), false);
  assert.deepEqual(steps, []);
});

test('触控板的细碎增量累计到阈值才走一步', () => {
  const { steps, handler } = collectSteps();
  // 两次不够、三次刚过阈值（对阈值变化也成立）
  const each = Math.floor((WHEEL_STEP_THRESHOLD - 1) / 2);
  assert.ok(each * 2 < WHEEL_STEP_THRESHOLD && each * 3 >= WHEEL_STEP_THRESHOLD);
  for (let i = 0; i < 2; i += 1) {
    const { e, prevented } = makeEvent(-each, { ctrlKey: true });
    handler(e);
    assert.equal(prevented(), true, '每次都要拦默认行为');
  }
  assert.deepEqual(steps, [], '累计未到阈值前不该改字号');
  handler(makeEvent(-each, { ctrlKey: true }).e);
  assert.deepEqual(steps, [-each * 3], '到阈值时把累计量一次交给调用方');
});

test('走完一步后重新累计（不会连续触发）', () => {
  const { steps, handler } = collectSteps();
  handler(makeEvent(-WHEEL_STEP_THRESHOLD, { ctrlKey: true }).e);
  handler(makeEvent(-10, { ctrlKey: true }).e);
  assert.equal(steps.length, 1);
});

test('反向滚动互相抵消：不误触发', () => {
  const { steps, handler } = collectSteps();
  handler(makeEvent(30, { ctrlKey: true }).e);
  handler(makeEvent(-30, { ctrlKey: true }).e);
  assert.deepEqual(steps, []);
});
