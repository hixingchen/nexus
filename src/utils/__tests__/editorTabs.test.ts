import { test } from 'node:test';
import assert from 'node:assert/strict';

import { tabsUnderPath } from '../editorTabs.ts';

/**
 * 删除文件/目录后要关掉哪些标签。
 *
 * 守的是一处真实症状：删掉一个目录，树里整棵消失在一次操作里，而标签是**按文件**开的——
 * 漏关的那些点回去只会报"文件不存在"。
 */

const tab = (path: string) => ({ id: `tab-${path}`, path });
const paths = (tabs: ReturnType<typeof tabsUnderPath>) => tabs.map((t) => t.path).sort();

test('删文件：只命中它自己', () => {
  const tabs = [tab('D:/work/proj/a.ts'), tab('D:/work/proj/b.ts')];
  assert.deepEqual(paths(tabsUnderPath(tabs, 'D:/work/proj/a.ts')), ['D:/work/proj/a.ts']);
});

test('删目录：它自己（若开着）与整棵子树下的文件全命中', () => {
  const tabs = [
    tab('D:/work/proj/src'),
    tab('D:/work/proj/src/a.ts'),
    tab('D:/work/proj/src/deep/b.ts'),
    tab('D:/work/proj/other.ts'),
  ];
  assert.deepEqual(paths(tabsUnderPath(tabs, 'D:/work/proj/src')), [
    'D:/work/proj/src',
    'D:/work/proj/src/a.ts',
    'D:/work/proj/src/deep/b.ts',
  ]);
});

test('按路径分量判断：删 src/a 不得波及 src/ab', () => {
  const tabs = [tab('D:/work/proj/src/ab/c.ts'), tab('D:/work/proj/src/a/d.ts')];
  assert.deepEqual(paths(tabsUnderPath(tabs, 'D:/work/proj/src/a')), ['D:/work/proj/src/a/d.ts']);
});

test('两种分隔符混用照样命中（树是"配置根原样 + / 拼子名"，搜索一律 /）', () => {
  const tabs = [tab('D:\\work\\proj/src/a.ts')];
  assert.deepEqual(paths(tabsUnderPath(tabs, 'D:/work/proj/src')), ['D:\\work\\proj/src/a.ts']);
  assert.deepEqual(paths(tabsUnderPath(tabs, 'D:\\work\\proj\\src')), ['D:\\work\\proj/src/a.ts']);
});

test('大小写不敏感（Windows：配置里的 d:\\work 与文件系统的 D:\\work 是同一目录）', () => {
  const tabs = [tab('D:/work/Proj/src/a.ts')];
  assert.deepEqual(paths(tabsUnderPath(tabs, 'd:/work/proj/src')), ['D:/work/Proj/src/a.ts']);
});

test('目录路径末尾带分隔符也能匹配', () => {
  const tabs = [tab('D:/work/proj/src/a.ts')];
  assert.deepEqual(paths(tabsUnderPath(tabs, 'D:/work/proj/src/')), ['D:/work/proj/src/a.ts']);
});

test('jar 内条目的标签随磁盘上的那个 jar 一起关掉', () => {
  const tabs = [tab('jar://D:/libs/app.jar!/com/A.class'), tab('D:/libs/other.jar')];
  assert.deepEqual(paths(tabsUnderPath(tabs, 'D:/libs/app.jar')), ['jar://D:/libs/app.jar!/com/A.class']);
});

test('jar 内条目的标签随磁盘上的那个 jar 一起关掉', () => {
  const tabs = [tab('jar://D:/libs/app.jar!/com/A.class'), tab('D:/libs/other.jar')];
  assert.deepEqual(paths(tabsUnderPath(tabs, 'D:/libs/app.jar')), ['jar://D:/libs/app.jar!/com/A.class']);
});

test('删掉**装着 jar 的目录**，jar 内的条目标签也要一起关（CQ-44）', () => {
  const tabs = [
    tab('jar://D:/proj/lib/app.jar!/com/A.class'),
    tab('jar://D:/proj/lib/app.jar!/inner.jar!/com/B.class'),
    tab('D:/proj/lib/other.txt'),          // 目录里的普通文件
    tab('jar://D:/proj/other/lib.jar!/x'), // 兄弟目录里的 jar：不能误伤
  ];
  // paths() 会排序，故这里也按排序后的顺序写
  assert.deepEqual(paths(tabsUnderPath(tabs, 'D:/proj/lib')), [
    'D:/proj/lib/other.txt',
    'jar://D:/proj/lib/app.jar!/com/A.class',
    'jar://D:/proj/lib/app.jar!/inner.jar!/com/B.class',
  ]);
});

test('空路径 / 无匹配 → 空数组（空路径不得退化成"命中一切"）', () => {
  const tabs = [tab('D:/work/proj/a.ts')];
  assert.deepEqual(tabsUnderPath(tabs, ''), []);
  assert.deepEqual(tabsUnderPath([], 'D:/work/proj'), []);
  assert.deepEqual(tabsUnderPath(tabs, 'E:/elsewhere'), []);
});
