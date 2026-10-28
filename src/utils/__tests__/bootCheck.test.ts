import { test } from 'node:test';
import assert from 'node:assert/strict';

import { parseBootCheck, serializeBootCheck } from '../bootCheck.ts';

test('空串/空白表示无检查', () => {
  assert.deepEqual(parseBootCheck(''), { kind: 'none', value: '' });
  assert.deepEqual(parseBootCheck('   '), { kind: 'none', value: '' });
  assert.deepEqual(serializeBootCheck('none', 'ignored'), { ok: true, json: '', error: '' });
});

test('解析四种形状', () => {
  assert.deepEqual(parseBootCheck('{"type":"delay","seconds":5}'), { kind: 'delay', value: '5' });
  assert.deepEqual(
    parseBootCheck('{"type":"tcp","host":"127.0.0.1","port":3306,"timeoutSecs":60}'),
    { kind: 'tcp', value: '127.0.0.1:3306' },
  );
  assert.deepEqual(
    parseBootCheck('{"type":"logContains","pattern":"ready","timeoutSecs":120}'),
    { kind: 'logContains', value: 'ready' },
  );
  assert.deepEqual(parseBootCheck('not-json'), { kind: 'none', value: '' });
});

test('序列化并校验范围', () => {
  assert.deepEqual(serializeBootCheck('delay', '5'), {
    ok: true, json: '{"type":"delay","seconds":5}', error: '',
  });
  assert.equal(serializeBootCheck('delay', '-1').ok, false);
  assert.equal(serializeBootCheck('delay', '1.5').ok, false);
  assert.deepEqual(serializeBootCheck('tcp', '127.0.0.1:3306'), {
    ok: true,
    json: '{"type":"tcp","host":"127.0.0.1","port":3306,"timeoutSecs":60}',
    error: '',
  });
  assert.equal(serializeBootCheck('tcp', '127.0.0.1').ok, false);
  assert.equal(serializeBootCheck('tcp', ':3306').ok, false);
  assert.equal(serializeBootCheck('tcp', '127.0.0.1:0').ok, false);
  assert.deepEqual(serializeBootCheck('logContains', 'ready for connections'), {
    ok: true,
    json: '{"type":"logContains","pattern":"ready for connections","timeoutSecs":120}',
    error: '',
  });
  assert.equal(serializeBootCheck('logContains', '').ok, false);
});
