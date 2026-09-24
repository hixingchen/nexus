import { test } from 'node:test';
import assert from 'node:assert/strict';

import { isStickToBottom, STICK_TO_BOTTOM_EPSILON } from '../logFollow.ts';

/**
 * 这条判据守的是提交 4c6ab57 修的那个 bug：日志面板「跟随」静默停止。
 * 误判不会自愈（distance 只增不减），面板上没有任何提示——所以判据本身的值、
 * 判据在什么时机取，都必须是钉住的（审计 NEW-25）。
 *
 * 度量口径：scrollHeight = 内容总高，scrollTop = 已滚过的距离，clientHeight = 视口高。
 * 行高按项目里的 ESTIMATED_ROW_H = 21px 折算，一处新增 5 行 = 内容长高 105px。
 */

test('本批新增 5 行后仍判贴底：只要滚动位置跟着到了底部，判据就不该被这一批的高度带偏', () => {
  // 跟随中的 1000px 内容 / 400px 视口 → 滚到底时 scrollTop = 600
  assert.equal(isStickToBottom({ scrollHeight: 1000, scrollTop: 600, clientHeight: 400 }), true);
  // 新增 5 行（+105px）、程序贴底后 scrollTop 跟到 705 → 距离仍是 0
  assert.equal(isStickToBottom({ scrollHeight: 1105, scrollTop: 705, clientHeight: 400 }), true);
});

test('同一批新增，若在滚动位置跟上之前取判据就会误判——这正是判据只能在滚动事件里取的理由', () => {
  // 内容已经长高、scrollTop 还停在 600（贴底那一拍之间）：距离 105 > 容差 24
  assert.equal(isStickToBottom({ scrollHeight: 1105, scrollTop: 600, clientHeight: 400 }), false);
  // LogViewer 因此把判据放在滚动事件里（程序贴底也会触发滚动事件，届时距离已归零），
  // 而不是在贴底 effect 里现算——现算拿到的是上面这一组的数字，跟随会当场停掉且不自愈
});

test('用户上翻后不判贴底：跟随必须让位给"正在看历史"', () => {
  assert.equal(isStickToBottom({ scrollHeight: 1105, scrollTop: 200, clientHeight: 400 }), false);
  // 容差边界（默认 24px）：距离 24 判贴底、25 判不贴底——容差之内仍算跟随（`<=` 而非 `<`），
  // 因为程序贴底量到的距离常常是亚像素零头而不是精确 0
  assert.equal(STICK_TO_BOTTOM_EPSILON, 24);
  assert.equal(isStickToBottom({ scrollHeight: 424, scrollTop: 0, clientHeight: 400 }), true);
  assert.equal(isStickToBottom({ scrollHeight: 425, scrollTop: 0, clientHeight: 400 }), false);
});

test('恢复跟随 / 打开面板强制贴底：这两处判据必然为 false，所以调用方不能复用它', () => {
  // 打开/切换服务：日志常长于视口，scrollTop 停在 0 → 距离 4600
  assert.equal(isStickToBottom({ scrollHeight: 5000, scrollTop: 0, clientHeight: 400 }), false);
  // 暂停期间上翻后点「跟随」：距离 505
  assert.equal(isStickToBottom({ scrollHeight: 1105, scrollTop: 200, clientHeight: 400 }), false);
  // 正因如此，LogViewer.handlePause 的恢复分支与切换服务的 layout effect 都直接
  // `stickRef.current = true`（调用方语义，不经过判据）：若改成"再算一次判据"，
  // 前者会停在旧位置却显示"跟随中"，后者会落在最旧的第一行
});

test('距离为负（滚动越界回弹）也算贴底，不会把跟随判掉', () => {
  assert.equal(isStickToBottom({ scrollHeight: 1000, scrollTop: 601, clientHeight: 400 }), true);
});

test('容差可覆盖，且不改动默认值', () => {
  assert.equal(isStickToBottom({ scrollHeight: 1000, scrollTop: 400, clientHeight: 400 }, 200), true);
  assert.equal(isStickToBottom({ scrollHeight: 1000, scrollTop: 400, clientHeight: 400 }), false);
});
