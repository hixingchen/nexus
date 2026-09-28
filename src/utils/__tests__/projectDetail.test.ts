import { test } from 'node:test';
import assert from 'node:assert/strict';

import { selectProjectDetail, servicesPending, type DetailSlot } from '../projectDetail.ts';

/**
 * 切项目时"显示谁的详情"。守的症状：上一个项目的详情被当成新项目的渲染出去
 * （服务列显示上一个项目的服务 →「暂无服务」→ 整片「加载中…」，一帧一个样）。
 */

type D = { project: { id: string; name: string } };

const slot = (projectId: string, name: string): DetailSlot<D> => ({
  projectId,
  detail: { project: { id: projectId, name } },
});

test('同源的 state 优先：它就是当前项目的详情', () => {
  const cache = new Map<string, D>([['p-a', { project: { id: 'p-a', name: '缓存的 A' } }]]);
  const got = selectProjectDetail(slot('p-a', '新拉的 A'), 'p-a', cache);
  assert.equal(got?.project.name, '新拉的 A');
});

test('不同源的 state 绝不能当成当前项目的详情（这就是那一帧闪烁）', () => {
  const cache = new Map<string, D>();
  assert.equal(selectProjectDetail(slot('p-a', 'A'), 'p-b', cache), null);
});

test('不同源但缓存里有本项目 → 用缓存（切回看过的项目，首帧即完整界面）', () => {
  const cache = new Map<string, D>([['p-b', { project: { id: 'p-b', name: '缓存的 B' } }]]);
  assert.equal(selectProjectDetail(slot('p-a', 'A'), 'p-b', cache)?.project.name, '缓存的 B');
});

test('都没有 → null（真·加载中，界面显示加载态而不是别人的数据）', () => {
  assert.equal(selectProjectDetail(null, 'p-b', new Map()), null);
  assert.equal(selectProjectDetail(slot('p-a', 'A'), 'p-b', new Map()), null);
});

test('服务列表为空：详情未落地才算"还没到"，不能显示成「暂无服务」', () => {
  assert.equal(servicesPending(slot('p-a', 'A'), 'p-b', []), true, '切项目中，列表空 = 未知');
  assert.equal(servicesPending(null, 'p-b', []), true, '首次进入，列表空 = 未知');
});

test('服务列表为空：详情已落地 → 真的是"这个项目没有服务"', () => {
  assert.equal(servicesPending(slot('p-b', 'B'), 'p-b', []), false);
});

test('服务列表非空：无论详情到没到都不算"还没到"（缓存里已有本项目的服务）', () => {
  assert.equal(servicesPending(null, 'p-b', ['s1']), false);
  assert.equal(servicesPending(slot('p-a', 'A'), 'p-b', ['s1']), false);
});
