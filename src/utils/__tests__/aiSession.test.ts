import { test } from 'node:test';
import assert from 'node:assert/strict';

import { AI_SESSION_MODE_DEFAULT, parseAiSessionMode } from '../aiSession.ts';

test('默认是新建会话（没设置过 / 值为空都是 new）', () => {
  assert.equal(AI_SESSION_MODE_DEFAULT, 'new');
  assert.equal(parseAiSessionMode(undefined), 'new');
  assert.equal(parseAiSessionMode(null), 'new');
  assert.equal(parseAiSessionMode(''), 'new');
});

test('只有明确写入 resume 才恢复上次会话', () => {
  assert.equal(parseAiSessionMode('resume'), 'resume');
});

test('被写坏的脏值一律回退默认值，不改变用户预期', () => {
  assert.equal(parseAiSessionMode('RESUME'), 'new');
  assert.equal(parseAiSessionMode('new'), 'new');
  assert.equal(parseAiSessionMode('whatever'), 'new');
});
