import { test } from 'node:test';
import assert from 'node:assert/strict';

import { decideUpdateNotice } from '../updateNotice.ts';

test('有新版本且没跳过 → 提示', () => {
  assert.equal(decideUpdateNotice('v1.10.0', '1.9.0', undefined), 'notify');
  assert.equal(decideUpdateNotice('v1.10.0', '1.9.0', ''), 'notify');
});

test('跳过的是这一版 → 静默（前导 v 不影响匹配）', () => {
  assert.equal(decideUpdateNotice('v1.10.0', '1.9.0', 'v1.10.0'), 'skipped');
  assert.equal(decideUpdateNotice('v1.10.0', '1.9.0', '1.10.0'), 'skipped');
});

test('跳过的是旧版本 → 新版本仍然提示（一次跳过不等于永久静默）', () => {
  assert.equal(decideUpdateNotice('v1.11.0', '1.9.0', '1.10.0'), 'notify');
});

test('已是最新 / 开发版比线上新 → 都不提示', () => {
  assert.equal(decideUpdateNotice('v1.9.0', '1.9.0', undefined), 'none');
  assert.equal(decideUpdateNotice('v1.9.0', '1.10.0', undefined), 'none');
});

test('版本号解析不了 → unknown（不能伪装成"已是最新"）', () => {
  assert.equal(decideUpdateNotice('latest', '1.9.0', undefined), 'unknown');
  assert.equal(decideUpdateNotice('v1.10.0', '', undefined), 'unknown');
});

test('预发布语义与 compareVersions 一致：正式版比 rc 新', () => {
  assert.equal(decideUpdateNotice('v1.10.0', '1.10.0-rc.1', undefined), 'notify');
  assert.equal(decideUpdateNotice('v1.10.0-rc.2', '1.10.0-rc.1', undefined), 'notify');
});
