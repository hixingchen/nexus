import { test } from 'node:test';
import assert from 'node:assert/strict';

import { useRunningStore } from '../runningStore.ts';

/**
 * 运行状态 store 的**等价判据**（`sameStatus`）。
 *
 * 它守的是一条"少比一个字段 = 界面少一个入口"的静默失效：内容没变时不能 `set`
 * （否则每 3 秒换一次数组引用，项目行/服务面板/目录树全量重渲），但**判成"没变"的
 * 前提是逐字段都比过**。漏比 `followed_log`（CQ-24）时，"日志文件刚被认领"这一拍会被
 * 判成没变化：store 里那份永远是 `null`，右键菜单的「取消跟随日志（xxx.log）」从此
 * 不出现，而日志面板看上去一切正常。
 *
 * 为什么用 `setRunning` 而不是直接测 `sameStatus`：它是模块私有（仅本文件使用），
 * 而这几条断言的落点本来就是"store 里的数据有没有被更新"，走公开入口更贴近真实调用。
 */

function reset(): void {
  useRunningStore.setState({
    running: [],
    failed: [],
    loaded: true,
    stopSignal: { seq: 0, serviceIds: [] },
  });
}

test('followed_log 从 null 变成路径：必须落库（否则「取消跟随日志」入口永不出现）', () => {
  reset();
  const before = { service_id: 's1', project_id: 'p1', followed_log: null, source: 'local' as const };
  useRunningStore.getState().setRunning([before], []);

  // 后端认出日志文件后的那一拍：成员、顺序、其它字段都没变，只有它有了值
  const after = { service_id: 's1', project_id: 'p1', followed_log: 'D:/p/logs/app.log', source: 'local' as const };
  useRunningStore.getState().setRunning([after], []);

  assert.equal(
    useRunningStore.getState().running[0]?.followed_log,
    'D:/p/logs/app.log',
    'followed_log 变了就不算"没变化"：判据漏比它时这里仍是 null',
  );
});

test('source 变化要落库：同一个服务从本地运行变成开机启动器运行', () => {
  reset();
  useRunningStore.getState().setRunning(
    [{ service_id: 's1', project_id: 'p1', followed_log: null, source: 'local' }], [],
  );
  useRunningStore.getState().setRunning(
    [{ service_id: 's1', project_id: 'p1', followed_log: null, source: 'boot' }], [],
  );
  assert.equal(useRunningStore.getState().running[0]?.source, 'boot');
});

test('三个字段逐项都比：改项目归属同样要落库（同服务 id 换了项目）', () => {
  reset();
  useRunningStore.getState().setRunning([{ service_id: 's1', project_id: 'p1', source: 'local' }], []);
  useRunningStore.getState().setRunning([{ service_id: 's1', project_id: 'p2', source: 'local' }], []);
  assert.equal(useRunningStore.getState().running[0]?.project_id, 'p2');
});

test('内容真的没变时不换引用：每 3 秒一次轮询不该让订阅方全量重渲', () => {
  reset();
  const r = [{ service_id: 's1', project_id: 'p1', followed_log: null, source: 'local' as const }];
  useRunningStore.getState().setRunning(r, []);
  const first = useRunningStore.getState().running;

  // 后端每次都返回**新数组**（同一份内容）：必须认出来并保持原引用
  useRunningStore.getState().setRunning([{ ...r[0] }], []);
  assert.equal(useRunningStore.getState().running, first, '内容等价就不该 set（引用必须原样）');
});

test('failed 分支仍是逐字段全比（exit_code / timestamp 变了要落库）', () => {
  reset();
  const r = [{ service_id: 's1', project_id: 'p1', followed_log: null, source: 'local' as const }];
  useRunningStore.getState().setRunning(r, [{ service_id: 's1', exit_code: 1, timestamp: 't1', source: 'local' }]);
  useRunningStore.getState().setRunning(r, [{ service_id: 's1', exit_code: 2, timestamp: 't1', source: 'local' }]);
  assert.equal(useRunningStore.getState().failed[0]?.exit_code, 2, '退出码变化必须被认出来');
});

/**
 * 显式停止信号：日志面板的自动关闭只认它，不认"running/failed 里暂时没有"。
 * 后者在自动重启 stop→start 的空窗里会误触发，把面板当开关拉。
 */
test('显式停止信号：seq 单调递增并带上本次停掉的服务；轮询写入不会抹掉它', () => {
  reset();
  const before = useRunningStore.getState().stopSignal.seq;
  useRunningStore.getState().markStopped(['s1']);
  const sig = useRunningStore.getState().stopSignal;
  assert.equal(sig.seq, before + 1, '每次显式停止都应递增序号（打开日志时快照它来判断"是不是新停止"）');
  assert.deepEqual(sig.serviceIds, ['s1']);

  // 轮询/动作后的状态刷新不能清掉这个信号：否则"停止 → 刷新 → 打开日志"之间
  // 那次停止会被一次轮询抹平，面板关不掉
  useRunningStore.getState().setRunning([], []);
  assert.deepEqual(useRunningStore.getState().stopSignal, sig);
});
