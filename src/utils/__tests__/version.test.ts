import { test } from 'node:test';
import assert from 'node:assert/strict';

import { compareVersions } from '../version.ts';

/**
 * 版本比较（"检查更新"据此判断有没有新版）。
 *
 * 钉住两条容易漏的规则：
 * - 主段相等时**无预发布 > 有预发布**：漏了它"正式版发布了"永远判不出来
 * - 预发布段要**逐段比**：只看"有没有后缀"的话，`1.0.5-rc.1 → 1.0.5-rc.2` 会被判成
 *   相等。dsh 那边踩过这个坑——它发布的版本全是 rc/alpha，于是升级永远检测不到
 */

const gt = (a: string, b: string) =>
  assert.ok((compareVersions(a, b) ?? 0) > 0, `${a} 应当比 ${b} 新`);
const lt = (a: string, b: string) =>
  assert.ok((compareVersions(a, b) ?? 0) < 0, `${a} 应当比 ${b} 旧`);
const eq = (a: string, b: string) =>
  assert.equal(compareVersions(a, b), 0, `${a} 与 ${b} 应当相等`);

test('主段按数值逐段比（不是字典序）', () => {
  gt('1.1.0', '1.0.0');
  gt('1.0.1', '1.0.0');
  gt('2.0.0', '1.9.9');
  // 字符串比较会把 "1.10" 判成小于 "1.9"，这里必须是数值比较
  gt('1.10.0', '1.9.0');
  eq('1.0.0', '1.0.0');
});

test('主段相等时：无预发布 > 有预发布', () => {
  gt('1.0.0', '1.0.0-rc.1');
  gt('1.0.0', '1.0.0-alpha');
  lt('1.0.0-rc.1', '1.0.0');
});

test('预发布段逐段比（只看"有没有后缀"会判成相等）', () => {
  gt('1.0.5-rc.2', '1.0.5-rc.1');
  gt('1.0.5-alpha.2', '1.0.5-alpha.1');
  gt('1.0.5-alpha.10', '1.0.5-alpha.9');
  eq('1.0.5-rc.1', '1.0.5-rc.1');
});

test('预发布段：数字 < 字母；公共前缀相同时段数少的更小', () => {
  gt('1.0.0-rc', '1.0.0-1');
  gt('1.0.0-rc.1', '1.0.0-rc');
});

test('宽容接受 v 前缀与不足三段的写法', () => {
  eq('v1.0.0', '1.0.0');
  eq('V1.2.3', '1.2.3');
  eq('1.0', '1.0.0');
  eq('1', '1.0.0');
  gt('v1.1', '1.0.9');
});

test('解析不了返回 null——不能当成"没有新版本"', () => {
  // 把解析失败当成 0 的话，界面上会显示"已是最新"，把一个错误说成了结论
  assert.equal(compareVersions('', '1.0.0'), null);
  assert.equal(compareVersions('1.0.0', 'latest'), null);
  assert.equal(compareVersions('1.x.0', '1.0.0'), null);
  assert.equal(compareVersions('1..0', '1.0.0'), null);
});
