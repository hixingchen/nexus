import { test } from 'node:test';
import assert from 'node:assert/strict';

import { shouldAutoCloseLog } from '../logPanel.ts';

/**
 * 日志面板自动关闭的判据（`shouldAutoCloseLog`）。
 *
 * 守的是那次真实故障：判据曾经看「running/failed 里暂时没有它」，而自动重启/手动重启
 * 都是后端 stop → start，中间有一段空窗——自动重启循环里面板被反复关掉，高频切换
 * 最终留下与 React 状态脱节的旧 DOM。现在判据**根本不接收运行态**：没得看就不会再误判。
 */

const sig = (seq: number, serviceIds: string[] = []) => ({ seq, serviceIds });

test('运行态短暂消失（自动重启空窗）不该关面板：判据不依赖 running/failed', () => {
  // 参数里没有任何运行态输入——这正是修复点：即使服务此刻不在 running，
  // 只要没有"用户显式停止"信号，面板就必须留着
  assert.equal(
    shouldAutoCloseLog({
      serviceId: 's1',
      serviceExists: true,
      stopSignal: sig(3, ['s2']),
      openedStopSeq: 3,
    }),
    false,
  );
});

test('打开面板之后用户显式停止 → 关闭', () => {
  assert.equal(
    shouldAutoCloseLog({
      serviceId: 's1',
      serviceExists: true,
      stopSignal: sig(4, ['s1']),
      openedStopSeq: 3,
    }),
    true,
  );
});

test('打开之前的停止信号不算数（"停止 → 再启动 → 打开日志"不能被立刻关掉）', () => {
  assert.equal(
    shouldAutoCloseLog({
      serviceId: 's1',
      serviceExists: true,
      stopSignal: sig(3, ['s1']), // 上一次会话留下的同 id 停止信号
      openedStopSeq: 3,           // 面板是这次信号之后打开的
    }),
    false,
  );
});

test('停的是别的服务 → 不关', () => {
  assert.equal(
    shouldAutoCloseLog({
      serviceId: 's1',
      serviceExists: true,
      stopSignal: sig(5, ['s2', 's3']),
      openedStopSeq: 3,
    }),
    false,
  );
});

test('服务已从项目配置中删除 → 无条件关闭（配置层面的确定事实）', () => {
  assert.equal(
    shouldAutoCloseLog({
      serviceId: 's1',
      serviceExists: false,
      stopSignal: sig(0),
      openedStopSeq: 0,
    }),
    true,
  );
});
