import { test } from 'node:test';
import assert from 'node:assert/strict';

import { needsFavoriteDivider } from '../projectList.ts';
import type { Project } from '../../services/service.ts';

/**
 * 窄轨分隔线的边界条件。
 *
 * 这是本文件唯一的规则：**全收藏 / 全非收藏 / 只有一个项目时都不该画线**——
 * 画错的表现是"列表里出现一条没有意义的横线"，而这条判断总共就一个表达式，
 * 很容易在下次改动时被顺手简化掉。
 */

function p(id: string, pinned: boolean): Project {
  return { id, name: id, path: `/p/${id}`, pinned, sort_index: 0 };
}

test('只画在"收藏块与其余"的那个交界处', () => {
  const list = [p('a', true), p('b', true), p('c', false), p('d', false)];
  assert.equal(needsFavoriteDivider(list, 0), false, '第一格之前不画');
  assert.equal(needsFavoriteDivider(list, 1), false, '同是收藏，不画');
  assert.equal(needsFavoriteDivider(list, 2), true, '收藏 → 非收藏 的交界：画');
  assert.equal(needsFavoriteDivider(list, 3), false, '同是非收藏，不画（只能有一条线）');
});

test('全收藏 / 全非收藏 / 单个项目都不画', () => {
  assert.equal(needsFavoriteDivider([p('a', true), p('b', true)], 1), false, '全收藏');
  assert.equal(needsFavoriteDivider([p('a', false), p('b', false)], 1), false, '全非收藏');
  assert.equal(needsFavoriteDivider([p('a', true)], 0), false, '只有一个项目');
});
