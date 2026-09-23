import { test } from 'node:test';
import assert from 'node:assert/strict';

import { compareVersions, matchNodeVersions } from '../version.ts';

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

/**
 * 可安装版本的搜索（Node 版本面板）。
 *
 * 两条规则各自的**反面**都要钉住：搜 `23` 时不能把 `22.23.x` 一起端出来（那是包含匹配的
 * 典型误伤），搜代号时又不能要求写全。
 */
const INDEX = [
  { version: 'v22.23.2', lts: 'Jod' },
  { version: 'v22.0.0', lts: 'Jod' },
  { version: 'v20.11.1', lts: 'Iron' },
  { version: 'v26.9.0', lts: '' },
];

test('搜版本号按整段前缀匹配', () => {
  const v22 = matchNodeVersions(INDEX, '22').map(v => v.version);
  assert.deepEqual(v22, ['v22.23.2', 'v22.0.0']);

  // 整段：23 不等于 22 这条线，不能命中 v22.23.2
  assert.deepEqual(matchNodeVersions(INDEX, '23'), []);
  assert.deepEqual(matchNodeVersions(INDEX, '22.23').map(v => v.version), ['v22.23.2']);
  // 完整版本号命中它自己
  assert.deepEqual(matchNodeVersions(INDEX, '22.23.2').map(v => v.version), ['v22.23.2']);
});

test('前导 v、结尾的点、空白、大小写都不影响搜索', () => {
  assert.deepEqual(matchNodeVersions(INDEX, 'v22').map(v => v.version), ['v22.23.2', 'v22.0.0']);
  assert.deepEqual(matchNodeVersions(INDEX, ' 22. ').map(v => v.version), ['v22.23.2', 'v22.0.0']);
  assert.deepEqual(matchNodeVersions(INDEX, 'jod').map(v => v.version), ['v22.23.2', 'v22.0.0']);
  assert.deepEqual(matchNodeVersions(INDEX, 'Iron').map(v => v.version), ['v20.11.1']);
});

test('空搜索词返回全部（展示多少条由界面决定）', () => {
  assert.equal(matchNodeVersions(INDEX, '').length, INDEX.length);
  assert.equal(matchNodeVersions(INDEX, '   ').length, INDEX.length);
  assert.equal(matchNodeVersions(INDEX, 'v').length, INDEX.length, '"v" 应当被当作没有搜索词');
});

test('搜不到就是空数组，不编出近似结果', () => {
  assert.deepEqual(matchNodeVersions(INDEX, '19'), []);
  assert.deepEqual(matchNodeVersions(INDEX, 'argon'), []);
});
