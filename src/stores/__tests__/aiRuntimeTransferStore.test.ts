import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';

import { useAiRuntimeTransferStore } from '../aiRuntimeTransferStore.ts';

/**
 * 导出 / 导入状态：这里守的是"设置面板关掉之后"的语义——
 * 传输还在后端跑，状态必须留在全局 store 里，重新打开面板能接着看。
 */

function reset() {
  useAiRuntimeTransferStore.setState({ kind: null, progress: null, error: '' });
}

beforeEach(reset);

function progress(kind: 'export' | 'import', done: number) {
  return { kind, phase: '打包', detail: 'd', done, total: 100 };
}

test('start：进入传输态并清掉上一次的错误与进度', () => {
  useAiRuntimeTransferStore.getState().fail('上一次失败');
  useAiRuntimeTransferStore.getState().start('export');
  const s = useAiRuntimeTransferStore.getState();
  assert.equal(s.kind, 'export');
  assert.equal(s.progress, null);
  assert.equal(s.error, '');
});

test('进度只在 store 里：组件卸载（不再有订阅者）不影响它', () => {
  const s = useAiRuntimeTransferStore.getState();
  s.start('export');
  s.setProgress(progress('export', 40));
  assert.equal(useAiRuntimeTransferStore.getState().progress?.done, 40);
  // 关闭设置面板等价于组件卸载；store 照常接收下一次事件
  useAiRuntimeTransferStore.getState().setProgress(progress('export', 80));
  assert.equal(useAiRuntimeTransferStore.getState().progress?.done, 80);
});

test('迟到的上一轮事件不能覆盖当前传输', () => {
  const s = useAiRuntimeTransferStore.getState();
  s.start('import');
  s.setProgress(progress('import', 10));
  // 上一次导出遗留在事件队列里的事件
  s.setProgress(progress('export', 99));
  assert.equal(useAiRuntimeTransferStore.getState().progress?.done, 10);
});

test('succeed / fail：结束传输；失败原因保留给面板展示', () => {
  const s = useAiRuntimeTransferStore.getState();
  s.start('import');
  s.succeed();
  assert.equal(useAiRuntimeTransferStore.getState().kind, null);
  assert.equal(useAiRuntimeTransferStore.getState().error, '');

  useAiRuntimeTransferStore.getState().start('export');
  useAiRuntimeTransferStore.getState().fail('包不完整');
  const end = useAiRuntimeTransferStore.getState();
  assert.equal(end.kind, null, '失败也必须退出传输态，否则按钮永远被进度占着');
  assert.equal(end.error, '包不完整');
});
