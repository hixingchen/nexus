import { test } from 'node:test';
import assert from 'node:assert/strict';

import { useLogStore } from '../logStore.ts';
import type { LogStream } from '../../services/logService.ts';

/**
 * 日志缓冲的四处不变量（NEW-24①）：幂等合并 / 行数与字节双上限 / `\r` 刷新帧 / 暂停冻结。
 *
 * 这四条各自都对应过真实症状：合并漏了 → 同一行显示两遍（快照与实时事件两条路径）；
 * 上限漏了 → 构建刷屏时内存爆掉、或旧行挤掉新行；刷新帧当成新行 → webpack 进度条
 * 每帧占一行、把 2000 行窗口冲空；暂停冻结漏了 → "暂停"却还在滚。
 *
 * 每个用例用独立的服务 key：`lastSeq` / `byteTotals` 是模块级的，换个 key 就不互相干扰。
 */

const line = (serviceKey: string, data: string, seq: number, stream: LogStream = 'stdout') =>
  ({ serviceKey, stream, data, seq, timestamp: 't' });

const texts = (key: string) => (useLogStore.getState().logs[key] ?? []).map(l => l.text);
const pausedTexts = (key: string) => (useLogStore.getState().pausedLogs[key] ?? []).map(l => l.text);

test('幂等合并：seq 不大于已合并最大值的行被丢弃（快照与事件重复到达）', () => {
  const k = 'svc-idem';
  useLogStore.getState().bulkAppend([line(k, 'a', 1), line(k, 'b', 2)]);
  useLogStore.getState().bulkAppend([line(k, 'b', 2), line(k, 'c', 3)]);
  assert.deepEqual(texts(k), ['a', 'b', 'c'], '重复到达的第 2 行不该显示两遍');
});

test('快照到达时保留更"新"的实时行（否则打开面板瞬间刷出的几行永久消失）', () => {
  const k = 'svc-snap';
  useLogStore.getState().bulkAppend([line(k, '实时-5', 5)]);
  useLogStore.getState().setLogs(k, [
    { seq: 1, timestamp: 't', stream: 'stdout', text: '快照-1' },
    { seq: 3, timestamp: 't', stream: 'stdout', text: '快照-3' },
  ]);
  assert.deepEqual(texts(k), ['快照-1', '快照-3', '实时-5']);
  // 快照是初始状态不是"新"日志：累计新增计数要重置
  assert.equal(useLogStore.getState().totalAdded[k], 0);
});

test('\\r 刷新帧就地替换队尾，不新增行、不涨计数', () => {
  const k = 'svc-cr';
  useLogStore.getState().bulkAppend([line(k, '进度 10%', 1)]);
  const before = useLogStore.getState().totalAdded[k];
  useLogStore.getState().bulkAppend([line(k, '\r进度 20%', 2)]);
  assert.deepEqual(texts(k), ['进度 20%'], '进度帧应替换而不是追加');
  assert.equal(useLogStore.getState().totalAdded[k], before, '刷新帧不计入"新增 N 行"');
});

test('刷新帧不覆盖不同流的队尾（system 行不能被 stdout 进度帧改写）', () => {
  const k = 'svc-cr-stream';
  useLogStore.getState().bulkAppend([line(k, '已启动', 1, 'system')]);
  useLogStore.getState().bulkAppend([line(k, '\r进度 20%', 2, 'stdout')]);
  const got = texts(k);
  assert.equal(got[0], '已启动', 'system 行必须原样保留');
  assert.equal(got.length, 2, '不可合并时进度帧成为独立一行');
  // 落单的刷新帧文本保留 `\r` 前缀——与后端 push_log_line 的同口径行为一致
  // （它同样只在可合并时替换队尾；前端渲染时 CR 是空白字符，不显示异常）
  assert.ok(got[1].includes('进度 20%'));
});

test('行数上限：超过 2000 行从头部丢，保留最新', () => {
  const k = 'svc-lines';
  const items = Array.from({ length: 2100 }, (_, i) => line(k, `L${i}`, i + 1));
  useLogStore.getState().bulkAppend(items);
  const got = texts(k);
  assert.equal(got.length, 2000);
  assert.equal(got[0], 'L100');
  assert.equal(got[got.length - 1], 'L2099');
});

test('字节上限：单行超长时按总字节裁剪（保留最新）', () => {
  const k = 'svc-bytes';
  const big = 'x'.repeat(1_500_000);
  useLogStore.getState().bulkAppend([line(k, big, 1), line(k, big, 2)]);
  const got = texts(k);
  assert.equal(got.length, 1, '两行共 3MB > 2MB 上限，最旧的一行必须被丢掉');
  assert.equal(got[0], big);
});

test('暂停视图：满 2000 行后冻结，但 \\r 刷新帧仍更新最后一行', () => {
  const k = 'svc-pause';
  useLogStore.getState().bulkAppend(Array.from({ length: 2000 }, (_, i) => line(k, `L${i}`, i + 1)));
  useLogStore.getState().pauseLogs(k);
  useLogStore.getState().bulkAppend([line(k, '新行', 2001)]);
  assert.equal(pausedTexts(k).length, 2000, '满 2000 后新行不进入暂停视图（冻结）');
  useLogStore.getState().bulkAppend([line(k, '\r改过的末行', 2002)]);
  assert.equal(pausedTexts(k).length, 2000, '刷新帧不增加行数');
  assert.equal(pausedTexts(k)[1999], '改过的末行', '进度条在暂停视图里仍应更新');
});

test('clearLogs 之后行号从新会话重新计数（去重窗口不能把新行号当旧行丢掉）', () => {
  const k = 'svc-clear';
  useLogStore.getState().bulkAppend([line(k, '旧', 100)]);
  useLogStore.getState().clearLogs(k);
  useLogStore.getState().bulkAppend([line(k, '新会话第 1 行', 1)]);
  assert.deepEqual(texts(k), ['新会话第 1 行'], '清空后 seq 从头开始，属于同一会话的新行必须显示');
});
