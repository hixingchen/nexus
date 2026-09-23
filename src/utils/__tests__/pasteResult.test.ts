import { test } from 'node:test';
import assert from 'node:assert/strict';

import { describePaste, type PasteOutcome } from '../pasteResult.ts';

/**
 * 粘贴结果的文案规则。
 *
 * 守的是五处真实症状：① 重名改号成 " (2)" 之后界面不说是哪个名字落了地；
 * ② 被跳过的联接点让复制结果内容不全，界面却说"已粘贴 N 个项目"；
 * ③ 部分成功时只说"已粘贴 N 个"，把失败的那半吞掉；
 * ④ 覆盖掉了旧文件却只报"已粘贴 N 个"——用户不知道哪个旧东西没了、它去哪了；
 * ⑤ 用户亲手选的「跳过」被报成"粘贴失败"或干脆不报。
 */

const outcome = (o: Partial<PasteOutcome>): PasteOutcome => ({
  created: [], failed: [], skipped: [], replaced: [], skipped_by_user: [], ...o,
});

test('单个文件：名字进标题，重名改号后用户能看见新名字', () => {
  const n = describePaste(outcome({ created: ['D:/p/src/a.txt'] }));
  assert.equal(n.variant, 'success');
  assert.equal(n.title, '已粘贴「a.txt」');
  assert.equal(n.description, undefined, '单个项目时不该再列一遍名字');
});

test('重名落成 " (2)": 标题直接给出落地名', () => {
  assert.equal(describePaste(outcome({ created: ['D:/p/src/a (2).txt'] })).title, '已粘贴「a (2).txt」');
});

test('多个项目：列出发地名（截断到 5 个 + 总数）', () => {
  const n = describePaste(outcome({ created: ['D:/p/a.txt', 'D:/p/b.txt'] }));
  assert.equal(n.title, '已粘贴 2 个项目');
  assert.equal(n.description, '落地：a.txt、b.txt');

  const many = describePaste(outcome({
    created: ['D:/p/1', 'D:/p/2', 'D:/p/3', 'D:/p/4', 'D:/p/5', 'D:/p/6', 'D:/p/7'],
  }));
  assert.equal(many.title, '已粘贴 7 个项目');
  assert.equal(many.description, '落地：1、2、3、4、5 等 7 个');
});

test('路径两种分隔符都认（树的子路径用 /，配置里的根可能带 \\）', () => {
  assert.equal(describePaste(outcome({ created: ['D:\\work\\proj\\a.txt'] })).title, '已粘贴「a.txt」');
  assert.equal(describePaste(outcome({ created: ['D:\\work/proj/a.txt'] })).title, '已粘贴「a.txt」');
});

test('全部失败：error 档，原因原样带上', () => {
  const n = describePaste(outcome({ failed: ['a: 源文件不存在', 'b: 拒绝访问'] }));
  assert.equal(n.variant, 'error');
  assert.equal(n.title, '粘贴失败（2 个）');
  assert.equal(n.description, 'a: 源文件不存在；b: 拒绝访问');
});

test('部分成功：标题同时给出两个数，失败原因不吞', () => {
  const n = describePaste(outcome({ created: ['D:/p/a.txt'], failed: ['b: 拒绝访问'] }));
  assert.equal(n.variant, 'error');
  assert.equal(n.title, '已粘贴 1 个，1 个失败');
  assert.equal(n.description, '失败：b: 拒绝访问');
});

test('跳过链接：不能只在日志里——结果内容不全是用户必须知道的事', () => {
  const n = describePaste(outcome({ created: ['D:/p/node_modules'], skipped: ['D:/p/node_modules/link'] }));
  assert.equal(n.variant, 'warning', '内容不全的复制不能报成纯成功');
  assert.equal(n.title, '已粘贴「node_modules」', '单个项目仍应报出落地名');
  assert.ok(n.description?.includes('1 个符号链接/联接点被跳过'), `实际: ${n.description}`);
  assert.ok(n.description?.includes('link'), '要说清是哪个被跳过了');
});

test('剪贴板里只有链接：说"没有复制任何内容"，而不是"已粘贴 0 个项目"', () => {
  const n = describePaste(outcome({ skipped: ['D:/p/link'] }));
  assert.equal(n.variant, 'warning');
  assert.equal(n.title, '没有复制任何内容');
  assert.ok(n.description?.includes('link'), `实际: ${n.description}`);
});

test('失败 + 跳过同时出现：两条都要说，档位取更严重的 error', () => {
  const n = describePaste(outcome({ created: ['D:/p/a'], failed: ['b: 拒绝访问'], skipped: ['D:/p/link'] }));
  assert.equal(n.variant, 'error');
  assert.equal(n.title, '已粘贴 1 个，1 个失败');
  assert.ok(n.description?.includes('跳过'), `实际: ${n.description}`);
  assert.ok(n.description?.includes('失败'), `实际: ${n.description}`);
});

test('覆盖：说清走的是哪个旧文件、它去了回收站', () => {
  const n = describePaste(outcome({ created: ['D:/p/a.txt'], replaced: ['D:/p/a.txt'] }));
  // 用户自己在弹框里选的覆盖，照办就不该报成警告——改了这条判据，每次覆盖都会挨一句黄字
  assert.equal(n.variant, 'success');
  assert.equal(n.title, '已粘贴「a.txt」');
  assert.ok(n.description?.includes('替换'), `实际: ${n.description}`);
  assert.ok(n.description?.includes('回收站'), '必须说清旧文件去哪了', `实际: ${n.description}`);
});

test('覆盖多个：列名单（截断到 5 个 + 总数）', () => {
  const names = ['a', 'b', 'c', 'd', 'e', 'f', 'g'].map(x => `D:/p/${x}.txt`);
  const n = describePaste(outcome({ created: names, replaced: names }));
  assert.equal(n.title, '已粘贴 7 个项目');
  assert.ok(n.description?.includes('7 个同名项被替换'), `实际: ${n.description}`);
  assert.ok(n.description?.includes('a.txt、b.txt、c.txt、d.txt、e.txt 等 7 个'), `实际: ${n.description}`);
});

test('全部被用户跳过：说成"已跳过"，而不是"粘贴失败"或"没有复制任何内容"', () => {
  const n = describePaste(outcome({ skipped_by_user: ['D:/p/a.txt'] }));
  assert.equal(n.variant, 'warning');
  assert.equal(n.title, '已跳过 1 个同名项');
  assert.ok(n.description?.includes('a.txt'), `实际: ${n.description}`);
});

test('部分被用户跳过：其余照常粘贴，跳过的单列一条', () => {
  const n = describePaste(outcome({ created: ['D:/p/y.txt'], skipped_by_user: ['D:/p/x.txt'] }));
  assert.equal(n.title, '已粘贴「y.txt」');
  assert.ok(n.description?.includes('x.txt'), `实际: ${n.description}`);
});

test('覆盖 + 跳过 + 链接跳过同时出现：三件事都要说，档位取更严重的 warning', () => {
  const n = describePaste(outcome({
    created: ['D:/p/a.txt', 'D:/p/b.txt'],
    replaced: ['D:/p/a.txt'],
    skipped_by_user: ['D:/p/c.txt'],
    skipped: ['D:/p/link'],
  }));
  assert.equal(n.variant, 'warning', '有链接被跳过 = 内容不全，不能报成纯成功');
  assert.equal(n.title, '已粘贴 2 个项目');
  for (const word of ['落地', '替换', '跳过', '链接']) {
    assert.ok(n.description?.includes(word), `缺「${word}」: ${n.description}`);
  }
});
