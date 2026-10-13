import { test } from 'node:test';
import assert from 'node:assert/strict';

import { createAutoRestarter, type ProbeOutcome } from '../autoRestart.ts';

/**
 * 自动重启的决策逻辑。
 *
 * 这组用例守的是一条**用户报过的症状**：AI 改代码触发自动重启后，服务有时起不来，
 * 而手动点一下就好了。两个原因都在这里被钉住：
 * 1. 原来的 2 秒冷却**丢弃**窗口内的变更（最后一次改动可能永远没被加载）；
 * 2. 重启之后没人看结果，秒退就一直是失败态（没有任何重试）。
 *
 * 时间全部由 `sleep` 注入，所以这些用例是毫秒级的、也没有时序抖动。
 */

/** 手控时钟：`sleep` 不自己 resolve，由用例决定何时放行——于是"卡在哪一觉上"可断言 */
function createManualClock() {
  const waiters: Array<{ ms: number; resolve: () => void }> = [];
  return {
    sleep: (ms: number) => new Promise<void>((resolve) => { waiters.push({ ms, resolve }); }),
    /** 未放行的等待数（= 代码正卡在某一觉上） */
    pendingCount: () => waiters.length,
    /** 最近一次请求的等待时长（用来确认卡的是节流窗口还是观察窗口） */
    lastRequestedMs: () => waiters[waiters.length - 1]?.ms,
    /** 放行最早的那个等待 */
    releaseOne: () => { waiters.shift()?.resolve(); },
  };
}

/** 让已 resolve 的 promise 链一路推进到下一个未放行的等待 */
const flush = () => new Promise<void>((r) => setTimeout(r, 0));

/**
 * 反复"推进 → 放行"直到链条不再等待（本轮结束）。上限只是防跑飞。
 *
 * 注意每一步都要先 `flush` 再判断：`change()` 返回时链条还停在第一个 await 的微任务上，
 * 此刻 `pendingCount()` 是 0——先判断就会在"一次都没推进"的情况下误判成"已经结束"
 * （这个坑第一次就踩了：三条用例因此拿到的是初始状态）。
 */
async function drain(clock: ReturnType<typeof createManualClock>, maxRounds = 50) {
  for (let i = 0; i < maxRounds; i++) {
    await flush();
    if (clock.pendingCount() === 0) return;
    clock.releaseOne();
  }
  assert.fail('还有等待未放行（疑似死循环）');
}

/** 用例共用的脚手架：记录重启/探查/放弃/报错，probe 结果按脚本依次返回 */
function setup(opts: {
  /** 依次返回的探查结果；用尽后重复最后一项 */
  outcomes?: ProbeOutcome[];
  restartError?: unknown;
  probeError?: unknown;
  maxAttempts?: number;
}) {
  const clock = createManualClock();
  const restarts: string[] = [];
  const probes: string[] = [];
  const giveUps: Array<{ serviceId: string; serviceName: string; attempts: number }> = [];
  const errors: Array<{ serviceId: string; error: unknown }> = [];
  const outcomes = opts.outcomes ?? ['running'];

  const restarter = createAutoRestarter({
    restart: async (sid) => {
      restarts.push(sid);
      if (opts.restartError) throw opts.restartError;
    },
    probe: async (sid) => {
      probes.push(sid);
      if (opts.probeError) throw opts.probeError;
      return outcomes[Math.min(probes.length - 1, outcomes.length - 1)]!;
    },
    sleep: clock.sleep,
    onGiveUp: (info) => { giveUps.push(info); },
    onError: (serviceId, error) => { errors.push({ serviceId, error }); },
    throttleMs: 10,
    verifyMs: 30,
    maxAttempts: opts.maxAttempts ?? 3,
    backoffMs: [5, 7],
  });

  return { clock, restarter, restarts, probes, giveUps, errors };
}

test('重启后服务正常起来：只重启一次，不重试、不打扰用户', async () => {
  const t = setup({ outcomes: ['running'] });

  t.restarter.change('s1', 'web');
  await flush();                       // 第一次重启已发出
  assert.deepEqual(t.restarts, ['s1']);
  assert.equal(t.clock.pendingCount(), 1);
  assert.equal(t.clock.lastRequestedMs(), 10, '应当先卡在节流窗口（10ms）');

  t.clock.releaseOne();
  await flush();
  assert.equal(t.clock.lastRequestedMs(), 20, '节流窗口之后是"观察窗口的剩余部分"（30-10）');

  t.clock.releaseOne();
  await flush();
  assert.deepEqual(t.probes, ['s1']);
  assert.equal(t.restarter.activeCount(), 0, '起来之后这一轮应当结束');
  assert.equal(t.giveUps.length, 0);
  assert.equal(t.errors.length, 0);
});

test('节流窗口内的变更**不被丢弃**：窗口一结束必须补一轮（尾沿）', async () => {
  const t = setup({ outcomes: ['running'] });

  t.restarter.change('s1', 'web');
  await flush();
  assert.equal(t.restarts.length, 1);

  // 窗口内又改了一次文件（AI 连续写代码就是这个形态）
  t.restarter.change('s1', 'web');
  await flush();
  assert.equal(t.restarts.length, 1, '节流窗口内不该立刻再重启一次');

  t.clock.releaseOne();                // 放行节流窗口
  await flush();
  assert.equal(
    t.restarts.length,
    2,
    '窗口结束必须补一轮——否则这次变更的代码永远不会被加载（原实现就是把它丢了）',
  );

  // 第二轮没有再变更 → 走观察窗口 → 才去判定成败
  await drain(t.clock);
  assert.equal(t.probes.length, 1, '被 pending 取代的那一轮不该去判定成败（结果已经过时）');
  assert.equal(t.giveUps.length, 0);
});

test('服务秒退（failed）：重试到上限才提示用户一次', async () => {
  const t = setup({ outcomes: ['failed'], maxAttempts: 3 });

  t.restarter.change('s1', 'web');
  await drain(t.clock);

  assert.equal(t.restarts.length, 3, '首次 + 2 次重试');
  assert.equal(t.probes.length, 3);
  assert.equal(t.giveUps.length, 1, '只在重试用尽后才打扰用户');
  assert.equal(t.giveUps[0]!.attempts, 3);
  assert.equal(t.giveUps[0]!.serviceName, 'web');
  assert.equal(t.restarter.activeCount(), 0, '放弃之后不该留下进行中的轮');
});

test('重试途中起来了：立刻收手（不把剩下的重试次数用完）', async () => {
  const t = setup({ outcomes: ['failed', 'running'], maxAttempts: 3 });

  t.restarter.change('s1', 'web');
  await drain(t.clock);

  assert.equal(t.restarts.length, 2, '第二次重启就成功了，不该有第三次');
  assert.equal(t.probes.length, 2);
  assert.equal(t.giveUps.length, 0, '成功了就不该提示失败');
});

test('服务还在启动中（unknown）：**一次都不重试**', async () => {
  const t = setup({ outcomes: ['unknown'], maxAttempts: 3 });

  t.restarter.change('s1', 'web');
  await drain(t.clock);

  assert.equal(
    t.restarts.length,
    1,
    'unknown 不是失败：重试会把"启动慢但健康"的服务越搞越糟（比不重试更坏）',
  );
  assert.equal(t.giveUps.length, 0, 'unknown 不该报"自动重启失败"');
  assert.equal(t.errors.length, 0);
});

test('重启命令本身报错：报一次错就收手，不重试', async () => {
  const t = setup({ restartError: new Error('服务不存在'), maxAttempts: 3 });

  t.restarter.change('s1', 'web');
  await drain(t.clock);

  assert.equal(t.restarts.length, 1, '命令都发不出去，重试同一个命令大概率还是失败');
  assert.equal(t.errors.length, 1);
  assert.equal(t.giveUps.length, 0);
});

test('探查本身抛错：按 unknown 处理（不重试），并把原因报出来', async () => {
  const t = setup({ probeError: new Error('IPC 断了'), maxAttempts: 3 });

  t.restarter.change('s1', 'web');
  await drain(t.clock);

  assert.equal(t.restarts.length, 1, '判据缺失时最安全的动作是什么都不做');
  assert.equal(t.errors.length, 1);
  assert.equal(t.giveUps.length, 0);
});

test('本轮收尾那一刻又来了变更：自动再起一轮（不吞掉那次改动）', async () => {
  // maxAttempts=1：一轮只有一次重启，failed 即放弃——放弃时若还有待办，必须再起一轮
  const t = setup({ outcomes: ['failed'], maxAttempts: 1 });

  t.restarter.change('s1', 'web');
  await flush();
  assert.equal(t.restarts.length, 1);

  // 就在"最后一觉"期间又改了一次文件
  t.restarter.change('s1', 'web');
  await drain(t.clock);

  assert.equal(
    t.restarts.length,
    2,
    '收尾时的待办必须再起一轮，否则这次改动永远不会被加载',
  );
});

test('不同服务各记各的轮（一个服务的重启不阻塞另一个）', async () => {
  const t = setup({ outcomes: ['running'] });

  t.restarter.change('s1', 'web');
  t.restarter.change('s2', 'api');
  await flush();

  assert.deepEqual(t.restarts, ['s1', 's2']);
  assert.equal(t.restarter.activeCount(), 2);

  await drain(t.clock);
  assert.equal(t.restarter.activeCount(), 0);
  assert.equal(t.giveUps.length, 0);
});
