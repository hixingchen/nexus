import { test } from 'node:test';
import assert from 'node:assert/strict';

import { useRunningStore } from '../runningStore.ts';
import { sameRunningStatus } from '../../utils/runningStatus.ts';

/**
 * 运行状态 store 的**等价判据**（`sameStatus`）。
 *
 * 它守的是一条"少比一个字段 = 界面少一个入口"的静默失效：内容没变时不能 `set`
 * （否则每 3 秒换一次数组引用，项目行/服务面板/目录树全量重渲），但**判成"没变"的
 * 前提是逐字段都比过**。漏比 `followed_log`（CQ-24）时，"日志文件刚被认领"这一拍会被
 * 判成没变化：store 里那份永远是 `null`，右键菜单的「取消跟随日志（xxx.log）」从此
 * 不出现，而日志面板看上去一切正常。
 *
 * `sameRunningStatus` 是纯函数，直接测它即可；生产侧只有 `refresh()` 一个调用点，
 * 不再需要仅供测试的 `setRunning` 入口（死代码，已删除）。
 */

function reset(): void {
  useRunningStore.setState({
    running: [],
    failed: [],
    loaded: true,
    stopSignal: { seq: 0, serviceIds: [] },
  });
}

test('followed_log 从 null 变成路径：必须判为“变了”（否则「取消跟随日志」入口永不出现）', () => {
  const before = { service_id: 's1', project_id: 'p1', followed_log: null, source: 'local' as const };
  // 后端认出日志文件后的那一拍：成员、顺序、其它字段都没变，只有它有了值
  const after = { service_id: 's1', project_id: 'p1', followed_log: 'D:/p/logs/app.log', source: 'local' as const };
  assert.equal(sameRunningStatus([before], [after], [], []), false);
});

test('source 变化要判为“变了”：同一个服务从本地运行变成开机启动器运行', () => {
  const local = [{ service_id: 's1', project_id: 'p1', followed_log: null, source: 'local' as const }];
  const boot = [{ service_id: 's1', project_id: 'p1', followed_log: null, source: 'boot' as const }];
  assert.equal(sameRunningStatus(local, boot, [], []), false);
});

test('三个字段逐项都比：改项目归属同样要判为“变了”', () => {
  const p1 = [{ service_id: 's1', project_id: 'p1', source: 'local' as const }];
  const p2 = [{ service_id: 's1', project_id: 'p2', source: 'local' as const }];
  assert.equal(sameRunningStatus(p1, p2, [], []), false);
});

test('内容真的没变时判为等价：每 3 秒一次轮询不该让订阅方全量重渲', () => {
  const r = [{ service_id: 's1', project_id: 'p1', followed_log: null, source: 'local' as const }];
  // 后端每次都返回**新数组**（同一份内容）：判据必须认出来
  assert.equal(sameRunningStatus(r, [{ ...r[0] }], [], []), true);
});

test('phase 变化要判为“变了”：启动中 → 已就绪', () => {
  const before = [{ service_id: 's1', project_id: 'p1', source: 'local' as const, phase: 'starting' as const }];
  const after = [{ ...before[0], phase: 'ready' as const }];
  assert.equal(sameRunningStatus(before, after, [], []), false);
});

test('监听端口变化要判为“变了”；内容相同则等价', () => {
  const a = [{ service_id: 's1', project_id: 'p1', source: 'local' as const, listening_ports: [8080] }];
  const b = [{ ...a[0], listening_ports: [8080, 5005] }];
  assert.equal(sameRunningStatus(a, b, [], []), false);
  const same = [{ ...a[0], listening_ports: [8080] }];
  assert.equal(sameRunningStatus(a, same, [], []), true);
});

test('failed 分支仍是逐字段全比（exit_code / timestamp 变了要判为“变了”）', () => {
  const r = [{ service_id: 's1', project_id: 'p1', followed_log: null, source: 'local' as const }];
  const f1 = [{ service_id: 's1', exit_code: 1, timestamp: 't1', source: 'local' as const }];
  const f2 = [{ service_id: 's1', exit_code: 2, timestamp: 't1', source: 'local' as const }];
  assert.equal(sameRunningStatus(r, r, f1, f2), false, '退出码变化必须被认出来');
});

/**
 * 显式停止信号：日志面板的自动关闭只认它，不认"running/failed 里暂时没有"。
 * 后者在自动重启 stop→start 的空窗里会误触发，把面板当开关拉。
 */
test('显式停止信号：seq 单调递增并带上本次停掉的服务', () => {
  reset();
  const before = useRunningStore.getState().stopSignal.seq;
  useRunningStore.getState().markStopped(['s1']);
  const sig = useRunningStore.getState().stopSignal;
  assert.equal(sig.seq, before + 1, '每次显式停止都应递增序号（打开日志时快照它来判断"是不是新停止"）');
  assert.deepEqual(sig.serviceIds, ['s1']);
});
