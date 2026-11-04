import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  classifyLogLines,
  filterLogsBySeverity,
} from '../logFilter.ts';
import type { ServiceLogLine } from '../../services/logService.ts';

let seq = 0;
function line(text: string, stream: ServiceLogLine['stream'] = 'stdout'): ServiceLogLine {
  return { seq: ++seq, timestamp: '2026-11-04T10:00:00.000+08:00', stream, text };
}

function severities(texts: string[], stream: ServiceLogLine['stream'] = 'stdout') {
  return classifyLogLines(texts.map(t => line(t, stream))).map(c => c.severity);
}

test('常见日志格式的级别识别（表驱动）', () => {
  const cases: Array<[string, string]> = [
    ['2026-11-04 10:00:00.123 ERROR 42 --- [main] c.a.Foo : boom', 'error'],
    ['2026-11-04 10:00:00.123 WARN 42 --- [main] c.a.Foo : slow', 'warn'],
    ['2026-11-04 10:00:00.123 INFO 42 --- [main] c.a.Foo : started', 'info'],
    ['[ERROR] build failed', 'error'],
    ['[WARNING] slow request', 'warn'],
    ['[INFO] started', 'info'],
    ['ERROR: missing config', 'error'],
    ['level=ERROR service down', 'error'],
    ['level: debug verbose output', 'debug'],
    ['{"level":"error","msg":"x"}', 'error'],
    ['04-Nov-2026 10:00:00.123 SEVERE [main] org.apache.Foo - boom', 'error'],
    ['DEBUG verbose output', 'debug'],
    ['plain message without a level', 'unknown'],
  ];
  for (const [text, expected] of cases) {
    assert.equal(severities([text])[0], expected, text);
  }
});

test('正文里的 error 不被误判：URL、更长单词、JSON 消息字段', () => {
  const texts = [
    'GET https://api.example.com/error 500',
    'https://api.example.com/errors returned 200',
    'ERRORS happened in the upstream module',
    'compile-errors.log was archived',
    '{"message":"no error found","code":0}',
  ];
  assert.deepEqual(severities(texts), texts.map(() => 'unknown'));
});

test('Python / Node 的异常头算错误', () => {
  assert.deepEqual(severities([
    'Traceback (most recent call last):',
    'ValueError: bad value',
    'TypeError: x is not a function',
  ]), ['error', 'error', 'error']);
});

test('ANSI 彩色日志里的级别也能识别', () => {
  assert.equal(severities(['\x1b[31mERROR\x1b[0m boom'])[0], 'error');
});

test('错误后的堆栈续行继承错误级别（Java / Python / Node）', () => {
  const texts = [
    '2026-11-04 10:00:00.123 ERROR 42 --- [main] c.a.Foo : boom',
    '    at com.acme.Foo.bar(Foo.java:42)',
    '    at com.acme.Baz.qux(Baz.java:7)',
    '  Caused by: java.lang.NullPointerException',
    '    ... 3 more',
    '2026-11-04 10:00:01.000 INFO 42 --- [main] c.a.Foo : recovered',
  ];
  const classes = classifyLogLines(texts.map(t => line(t)));
  assert.deepEqual(classes.map(c => c.severity), ['error', 'error', 'error', 'error', 'error', 'info']);
  assert.deepEqual(classes.map(c => c.continuation), [false, true, true, true, true, false]);
});

test('Python traceback 的缩进行与 ^ 标记随错误保留', () => {
  const texts = [
    'Traceback (most recent call last):',
    '  File "/app/main.py", line 10, in <module>',
    '    run()',
    '    ^^^',
    'ValueError: bad value',
  ];
  const classes = classifyLogLines(texts.map(t => line(t)));
  assert.deepEqual(classes.map(c => c.severity), ['error', 'error', 'error', 'error', 'error']);
  assert.deepEqual(classes.map(c => c.continuation), [false, true, true, true, false]);
});

test('警告后的缩进行继承警告；普通行后的缩进行不算续行', () => {
  const afterWarn = classifyLogLines([
    line('WARNING: slow'),
    line('    details here'),
  ]);
  assert.deepEqual(afterWarn.map(c => c.severity), ['warn', 'warn']);
  assert.equal(afterWarn[1].continuation, true);

  const afterInfo = classifyLogLines([
    line('INFO: started'),
    line('    details here'),
  ]);
  assert.deepEqual(afterInfo.map(c => c.severity), ['info', 'unknown']);
  assert.equal(afterInfo[1].continuation, false);
});

test('堆栈续行本身带明确级别时不继承（例如缩进的 INFO）', () => {
  const classes = classifyLogLines([
    line('ERROR: boom'),
    line('  INFO: unrelated nested output'),
  ]);
  assert.deepEqual(classes.map(c => c.severity), ['error', 'info']);
  assert.equal(classes[1].continuation, false);
});

test('system 生命周期行：失败归错误，告警归警告，普通行不误判', () => {
  assert.deepEqual(severities([
    '启动失败: 系统找不到指定的文件。',
    '进程已退出（退出码: 1）',
    '命令失败（退出码 1）：npm run dev',
    '⚠ 未能纳入 Job Object：拒绝访问',
    '服务已启动',
  ], 'system'), ['error', 'error', 'error', 'warn', 'unknown']);
});

test('过滤：错误模式保留主行与续行，警告及以上额外保留警告', () => {
  const lines = [
    line('ERROR: boom'),
    line('    at Foo.bar(Foo.java:1)'),
    line('    at Baz.qux(Baz.java:2)'),
    line('WARNING: slow'),
    line('INFO: ok'),
    line('ERROR: second'),
    line('Caused by: NPE'),
  ];

  const errors = filterLogsBySeverity(lines, 'error');
  assert.deepEqual(errors.rows.map(l => l.text), [
    'ERROR: boom',
    '    at Foo.bar(Foo.java:1)',
    '    at Baz.qux(Baz.java:2)',
    'ERROR: second',
    'Caused by: NPE',
  ]);
  assert.equal(errors.errorCount, 2);
  assert.equal(errors.warnCount, 0);
  assert.equal(errors.continuationCount, 3);

  const warnings = filterLogsBySeverity(lines, 'warn');
  assert.equal(warnings.rows.length, 6);
  assert.equal(warnings.errorCount, 2);
  assert.equal(warnings.warnCount, 1);
  assert.equal(warnings.continuationCount, 3);

  const all = filterLogsBySeverity(lines, 'all');
  assert.equal(all.rows, lines, '全部模式应原样返回，不做复制');
  assert.equal(all.errorCount, 0);
});
