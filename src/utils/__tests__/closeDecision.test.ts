import test from 'node:test';
import assert from 'node:assert/strict';
import { decideCloseAction } from '../closeDecision';

test('普通关窗：无草稿直接退出，有草稿弹确认', () => {
  assert.equal(decideCloseAction({ trayQuit: false, closeToTray: false, dirtyCount: 0 }), 'exit');
  assert.equal(decideCloseAction({ trayQuit: false, closeToTray: false, dirtyCount: 3 }), 'ask');
});

test('关闭到托盘：普通关窗一律隐藏，草稿留在内存里不弹确认', () => {
  assert.equal(decideCloseAction({ trayQuit: false, closeToTray: true, dirtyCount: 0 }), 'hide');
  assert.equal(decideCloseAction({ trayQuit: false, closeToTray: true, dirtyCount: 5 }), 'hide');
});

test('托盘退出优先于关闭到托盘：否则开启托盘模式后永远退不出应用', () => {
  assert.equal(decideCloseAction({ trayQuit: true, closeToTray: true, dirtyCount: 0 }), 'exit');
  assert.equal(decideCloseAction({ trayQuit: true, closeToTray: true, dirtyCount: 2 }), 'ask');
  assert.equal(decideCloseAction({ trayQuit: true, closeToTray: false, dirtyCount: 0 }), 'exit');
});
