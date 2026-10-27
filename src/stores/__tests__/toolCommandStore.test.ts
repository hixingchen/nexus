import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';

import { useToolCommandStore } from '../toolCommandStore.ts';
import { useToolLogStore } from '../toolLogStore.ts';

/**
 * 工具命令运行状态：这里守的是"关掉弹窗之后"的语义——
 * 命令继续在后台跑、状态不能丢、同一条命令不能再起第二个进程。
 */

const RESULT = { success: true, output: 'ok', exit_code: 0 };

function reset() {
  useToolCommandStore.setState({ runs: {}, openRunId: null });
  useToolLogStore.setState({ logs: {} });
}

beforeEach(reset);

function begin(runId = 'r1', serviceId = 's1', commandId = 'c1', commandName = '安装依赖') {
  useToolCommandStore.getState().begin({ runId, serviceId, commandId, commandName });
}

test('begin：新命令进入执行态并打开弹窗', () => {
  begin();
  const s = useToolCommandStore.getState();
  assert.equal(s.openRunId, 'r1');
  assert.equal(s.runs.r1.loading, true);
  assert.equal(s.runs.r1.commandName, '安装依赖');
});

test('运行中关弹窗 = 转入后台（run 保留、弹窗关闭）——关闭不停止命令', () => {
  begin();
  useToolCommandStore.getState().closeDialog();
  const s = useToolCommandStore.getState();
  assert.equal(s.openRunId, null);
  assert.ok(s.runs.r1, '后台运行中的 run 必须保留，否则指示器/再打开都找不到它');
  assert.equal(s.runs.r1.loading, true);
});

test('后台完成后仍可再次打开查看结果', () => {
  begin();
  useToolCommandStore.getState().closeDialog();
  useToolCommandStore.getState().finish('r1', RESULT);
  assert.equal(useToolCommandStore.getState().runs.r1.result?.output, 'ok');

  useToolCommandStore.getState().open('r1');
  assert.equal(useToolCommandStore.getState().openRunId, 'r1');
});

test('打开并关闭已结束的 run：关闭时连同结果一起清掉', () => {
  begin();
  useToolCommandStore.getState().finish('r1', RESULT);
  useToolCommandStore.getState().closeDialog();
  const s = useToolCommandStore.getState();
  assert.equal(s.openRunId, null);
  assert.equal(s.runs.r1, undefined);
});

test('启动新命令会清理已结束的后台 run（连同输出）', () => {
  begin('old');
  useToolCommandStore.getState().closeDialog();
  useToolCommandStore.getState().finish('old', RESULT);
  useToolLogStore.setState({ logs: { old: ['line'] } });

  begin('new');
  assert.equal(useToolCommandStore.getState().runs.old, undefined);
  assert.equal(useToolLogStore.getState().logs.old, undefined);
});

test('runningByCommand：同服务同命令在跑时能查到，结束后不再命中', () => {
  begin();
  assert.equal(useToolCommandStore.getState().runningByCommand('s1', 'c1')?.runId, 'r1');
  assert.equal(useToolCommandStore.getState().runningByCommand('s1', 'c2'), undefined);
  assert.equal(useToolCommandStore.getState().runningByCommand('s2', 'c1'), undefined);

  useToolCommandStore.getState().finish('r1', RESULT);
  assert.equal(useToolCommandStore.getState().runningByCommand('s1', 'c1'), undefined);
});

test('用户主动停止且已关窗：完成/失败都静默清理，不再弹"执行完成"', () => {
  begin();
  useToolCommandStore.getState().markStopping('r1', true);
  useToolCommandStore.getState().closeDialog();
  useToolCommandStore.getState().finish('r1', { success: false, output: '', exit_code: 130 });
  assert.equal(useToolCommandStore.getState().runs.r1, undefined);
});

test('用户主动停止但弹窗还开着：结果照常展示（停止也要能看到退出码）', () => {
  begin();
  useToolCommandStore.getState().markStopping('r1', true);
  useToolCommandStore.getState().finish('r1', { success: false, output: '', exit_code: 130 });
  const run = useToolCommandStore.getState().runs.r1;
  assert.equal(run.loading, false);
  assert.equal(run.result?.exit_code, 130);
});
