import { test } from 'node:test';
import assert from 'node:assert/strict';

import { shouldSyncProjectDraft } from '../projectEditDraft.ts';

/**
 * 回归：先编辑项目 A、关闭，再编辑项目 B。
 *
 * B 打开的第一帧里 `name/path` 还是 A 的值；如果此时允许写草稿，A 的内容会被写进
 * B 的草稿键，B 初始化时再读回来——用户看到“编辑 B 却显示 A 的信息”。
 * 因此只有“表单已经为当前项目种过值”才允许写。
 */
test('切换项目后、新项目尚未种入表单时不得同步草稿', () => {
  assert.equal(shouldSyncProjectDraft('b', 'a'), false, 'A 种的值不能写进 B 的草稿');
  assert.equal(shouldSyncProjectDraft('b', null), false, '首次打开前没有种过，不能写');
  assert.equal(shouldSyncProjectDraft(null, 'a'), false, '弹窗关闭时不写草稿');
});

test('只有当前项目已经种入表单才允许同步草稿', () => {
  assert.equal(shouldSyncProjectDraft('b', 'b'), true);
});
