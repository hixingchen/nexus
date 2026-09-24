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
  useRunningStore.setState({ running: [], failed: [], loaded: true });
}

test('followed_log 从 null 变成路径：必须落库（否则「取消跟随日志」入口永不出现）', () => {
  reset();
  const before = { service_id: 's1', project_id: 'p1', followed_log: null };
  useRunningStore.getState().setRunning([before], []);

  // 后端认出日志文件后的那一拍：成员、顺序、其它字段都没变，只有它有了值
  const after = { service_id: 's1', project_id: 'p1', followed_log: 'D:/p/logs/app.log' };
  useRunningStore.getState().setRunning([after], []);

  assert.equal(
    useRunningStore.getState().running[0]?.followed_log,
    'D:/p/logs/app.log',
    'followed_log 变了就不算"没变化"：判据漏比它时这里仍是 null',
  );
});

test('三个字段逐项都比：改项目归属同样要落库（同服务 id 换了项目）', () => {
  reset();
  useRunningStore.getState().setRunning([{ service_id: 's1', project_id: 'p1' }], []);
  useRunningStore.getState().setRunning([{ service_id: 's1', project_id: 'p2' }], []);
  assert.equal(useRunningStore.getState().running[0]?.project_id, 'p2');
});

test('内容真的没变时不换引用：每 3 秒一次轮询不该让订阅方全量重渲', () => {
  reset();
  const r = [{ service_id: 's1', project_id: 'p1', followed_log: null }];
  useRunningStore.getState().setRunning(r, []);
  const first = useRunningStore.getState().running;

  // 后端每次都返回**新数组**（同一份内容）：必须认出来并保持原引用
  useRunningStore.getState().setRunning([{ ...r[0] }], []);
  assert.equal(useRunningStore.getState().running, first, '内容等价就不该 set（引用必须原样）');
});

test('failed 分支仍是逐字段全比（exit_code / timestamp 变了要落库）', () => {
  reset();
  const r = [{ service_id: 's1', project_id: 'p1', followed_log: null }];
  useRunningStore.getState().setRunning(r, [{ service_id: 's1', exit_code: 1, timestamp: 't1' }]);
  useRunningStore.getState().setRunning(r, [{ service_id: 's1', exit_code: 2, timestamp: 't1' }]);
  assert.equal(useRunningStore.getState().failed[0]?.exit_code, 2, '退出码变化必须被认出来');
});
