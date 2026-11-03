import { test } from 'node:test';
import assert from 'node:assert/strict';

import { createLogSearchCache, filterLogLinesIncremental } from '../logSearch.ts';
import type { ServiceLogLine } from '../../services/logService.ts';

const line = (seq: number, text: string): ServiceLogLine =>
  ({ seq, timestamp: 't', stream: 'stdout', text });

test('搜索词不变时，旧行不重复匹配，只测新增行', () => {
  const cache = createLogSearchCache();
  let calls = 0;
  const matches = (text: string, term: string) => { calls += 1; return text.includes(term); };

  const a = line(1, 'alpha');
  const b = line(2, 'beta');
  assert.deepEqual(
    filterLogLinesIncremental([a, b], 'a', cache, matches).map(l => l.seq),
    [1, 2],
  );
  assert.equal(calls, 2, '首轮两行各测一次');

  const c = line(3, 'gamma');
  assert.deepEqual(
    filterLogLinesIncremental([a, b, c], 'a', cache, matches).map(l => l.seq),
    [1, 2, 3],
  );
  assert.equal(calls, 3, '第二轮只新增 c 需要测试，a/b 命中缓存');
});

test('搜索词变化时缓存失效，全量重测', () => {
  const cache = createLogSearchCache();
  let calls = 0;
  const matches = (text: string, term: string) => { calls += 1; return text.includes(term); };
  const a = line(1, 'alpha');
  filterLogLinesIncremental([a], 'a', cache, matches);
  calls = 0;
  assert.deepEqual(filterLogLinesIncremental([a], 'zzz', cache, matches), []);
  assert.equal(calls, 1, '换词后同一行必须重新匹配');
});
