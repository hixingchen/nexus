import { test } from 'node:test';
import assert from 'node:assert/strict';

import { pickRevealTarget, REVEAL_PRIORITY, type RevealTarget } from '../fileTree.ts';

/**
 * 定位规则：**根最长者胜出**。
 *
 * 守的是一处真实症状：项目目录树与服务目录树同时展开时，同一个文件在两棵树里都有，
 * 若两棵树都响应，左面板最后停在哪棵取决于谁最后滚动——"有时跳到服务里、有时跳到项目里"。
 */

const project: RevealTarget = { root: 'D:/work/proj', priority: REVEAL_PRIORITY.project };
const svc: RevealTarget = { root: 'D:/work/proj/code/jp-ui', priority: REVEAL_PRIORITY.service };
const svcSibling: RevealTarget = { root: 'D:/work/proj/code/jp-console', priority: REVEAL_PRIORITY.service };

test('服务目录下的文件由服务树响应（根更长 = 更具体），项目树不响应', () => {
  const path = 'D:/work/proj/code/jp-ui/src/main.ts';
  assert.equal(pickRevealTarget([project, svc, svcSibling], path), svc);
});

test('只在项目目录下（不在任何服务里）的文件由项目树兜底', () => {
  const path = 'D:/work/proj/README.md';
  assert.equal(pickRevealTarget([project, svc, svcSibling], path), project);
});

test('两个服务目录嵌套时由更深的那棵响应', () => {
  const outer: RevealTarget = { root: 'D:/work/proj/code', priority: REVEAL_PRIORITY.service };
  const inner: RevealTarget = { root: 'D:/work/proj/code/jp-ui', priority: REVEAL_PRIORITY.service };
  assert.equal(pickRevealTarget([outer, inner], 'D:/work/proj/code/jp-ui/a.ts'), inner);
  assert.equal(pickRevealTarget([outer, inner], 'D:/work/proj/code/jp-console/b.ts'), outer);
});

test('同根时服务树优先（项目根正好也是某个服务的工作目录时）', () => {
  const sameRootProject: RevealTarget = { root: 'D:/work/proj', priority: REVEAL_PRIORITY.project };
  const sameRootSvc: RevealTarget = { root: 'D:/work/proj', priority: REVEAL_PRIORITY.service };
  assert.equal(
    pickRevealTarget([sameRootProject, sameRootSvc], 'D:/work/proj/a.ts'),
    sameRootSvc,
    '同根平局：服务树才是"主角"',
  );
});

test('按路径分量判断：兄弟前缀不误命中', () => {
  const p: RevealTarget = { root: 'D:/work/proj', priority: REVEAL_PRIORITY.project };
  assert.equal(pickRevealTarget([p], 'D:/work/proj-evil/a.ts'), null, 'proj-evil 不是 proj 之内');
  assert.equal(pickRevealTarget([p], 'D:/work/proj'), p, '根自身命中');
});

test('根末尾带分隔符照样命中；反斜杠路径也认', () => {
  const withSlash: RevealTarget = { root: 'D:/work/proj/', priority: REVEAL_PRIORITY.project };
  assert.equal(pickRevealTarget([withSlash], 'D:/work/proj/a.ts'), withSlash);
  const back: RevealTarget = { root: 'D:\\work\\proj', priority: REVEAL_PRIORITY.project };
  assert.equal(pickRevealTarget([back], 'D:\\work\\proj\\a.ts'), back);
});

test('没有树包含该文件 / 空路径 → null（调用方据此走"未定位到"提示）', () => {
  assert.equal(pickRevealTarget([project, svc], 'E:/elsewhere/a.ts'), null);
  assert.equal(pickRevealTarget([project, svc], ''), null);
  assert.equal(pickRevealTarget([], 'D:/work/proj/a.ts'), null);
  assert.equal(pickRevealTarget([{ root: '', priority: 0 }], 'D:/a.ts'), null, '空根不算包含一切');
});
