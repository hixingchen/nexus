import { test } from 'node:test';
import assert from 'node:assert/strict';

import { resolveOpenPort } from '../serviceOpen.ts';

test('未启用打开：无论有多少监听端口都不显示打开目标', () => {
  assert.deepEqual(resolveOpenPort({ enabled: false }, [3000]), { kind: 'disabled' });
  assert.deepEqual(resolveOpenPort({}, [3000, 5005]), { kind: 'disabled' });
});

test('启用但没有监听端口：返回 none（按钮应禁用而不是打开空白页）', () => {
  assert.deepEqual(resolveOpenPort({ enabled: true }, []), { kind: 'none' });
  assert.deepEqual(resolveOpenPort({ enabled: true }, undefined), { kind: 'none' });
});

test('指定端口正在监听：优先打开它（即使还有别的端口）', () => {
  assert.deepEqual(
    resolveOpenPort({ enabled: true, specifiedPort: 3000 }, [5005, 3000, 8080]),
    { kind: 'open', port: 3000 },
  );
});

test('指定端口漂移且只有一个监听端口：安全回退到该端口', () => {
  assert.deepEqual(
    resolveOpenPort({ enabled: true, specifiedPort: 3000 }, [3001]),
    { kind: 'open', port: 3001 },
  );
});

test('指定端口漂移且有多个监听端口：不猜，交给用户选', () => {
  assert.deepEqual(
    resolveOpenPort({ enabled: true, specifiedPort: 3000 }, [3001, 5005]),
    { kind: 'choose', ports: [3001, 5005] },
  );
});

test('未指定端口：唯一端口直接打开，多端口交给用户选', () => {
  assert.deepEqual(resolveOpenPort({ enabled: true }, [3001]), { kind: 'open', port: 3001 });
  assert.deepEqual(
    resolveOpenPort({ enabled: true }, [5005, 3001]),
    { kind: 'choose', ports: [3001, 5005] },
  );
});

test('端口去重并升序：重复与乱序输入不影响选择结果', () => {
  assert.deepEqual(
    resolveOpenPort({ enabled: true }, [5005, 3001, 5005]),
    { kind: 'choose', ports: [3001, 5005] },
  );
});
