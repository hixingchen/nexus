import { test } from 'node:test';
import assert from 'node:assert/strict';

import { rememberScroll, recallScroll } from '../editorScroll.ts';

/**
 * 源码 / 预览共用的滚动记忆（`utils/editorScroll`）。
 *
 * 守的是用户报过的症状：源码停在第 100 行，切到预览落回第一行——两种模式各记各的。
 * 现在两者写同一张表，切换时能按比例接上；"最后是谁写的"决定源码挂载时用精确快照
 * 还是用这份近似比例。
 */

test('同一文件的两种模式共享一条记录：后写的覆盖先写的，并带上写入者', () => {
  rememberScroll('/p/doc.md', 'source', 0.5);
  assert.deepEqual(recallScroll('/p/doc.md'), { ratio: 0.5, writer: 'source' });

  rememberScroll('/p/doc.md', 'preview', 0.25);
  assert.deepEqual(recallScroll('/p/doc.md'), { ratio: 0.25, writer: 'preview' });
});

test('不同文件互不影响', () => {
  rememberScroll('/p/a.md', 'source', 0.1);
  rememberScroll('/p/b.md', 'preview', 0.9);
  assert.equal(recallScroll('/p/a.md')?.ratio, 0.1);
  assert.equal(recallScroll('/p/b.md')?.ratio, 0.9);
  assert.equal(recallScroll('/p/missing.md'), undefined);
});

test('越界比例被夹回 0~1（滚动回弹会给出负值 / 略大于 1）', () => {
  rememberScroll('/p/neg.md', 'source', -0.3);
  assert.equal(recallScroll('/p/neg.md')?.ratio, 0);
  rememberScroll('/p/over.md', 'preview', 1.4);
  assert.equal(recallScroll('/p/over.md')?.ratio, 1);
  rememberScroll('/p/nan.md', 'source', Number.NaN);
  assert.equal(recallScroll('/p/nan.md'), undefined, '非有限值不记录');
});

test('上限 64：超出后淘汰最久没被读写的条目', () => {
  for (let i = 0; i < 64; i++) rememberScroll(`/p/f${i}.md`, 'source', i / 100);
  // 读一下最旧的那条，把它挪到 LRU 末尾
  assert.equal(recallScroll('/p/f0.md')?.ratio, 0);
  for (let i = 64; i < 70; i++) rememberScroll(`/p/f${i}.md`, 'source', i / 100);
  assert.equal(recallScroll('/p/f0.md')?.ratio, 0, '被读过的条目不该先淘汰');
  assert.equal(recallScroll('/p/f1.md'), undefined, '最久没被读写的应被淘汰');
});
