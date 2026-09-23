import { test, afterEach } from 'node:test';
import assert from 'node:assert/strict';

import { pasteInto } from '../pasteActions.ts';
import { usePasteConflictStore } from '../pasteConflict.ts';
import { setNotifier, type NotifyOptions } from '../../utils/notify.ts';
import type { ConflictPolicy, PasteResponse, PasteOutcome } from '../../utils/pasteResult.ts';

/**
 * 粘贴的两阶段流程（探测 → 弹框 → 带策略再调一次）。
 *
 * 这组用例集中在**接线**上：发出去的参数对不对、该问的时候问没问、用户的选择有没有原样
 * 带回后端。类型检查看不见这些——`pasteInto(targetDir, policy, probe.sources)` 与
 * `pasteInto(targetDir, null, null)` 两行都通得过，但落下去的是两份完全不同的结果。
 *
 * 用假传输层而不是打桩 `invoke`：要断言的就是"交给后端的是什么"，
 * 换掉这一个出口比换掉整个 IPC 层更贴近被测对象。
 */

type Call = { targetDir: string; policy: ConflictPolicy | null; sources: string[] | null };

/** 按剧本依次返回；每次返回**新建的对象**（真后端也是），并记下收到的参数 */
function fakePaste(script: PasteResponse[]) {
  const calls: Call[] = [];
  const paste = async (targetDir: string, policy: ConflictPolicy | null, sources: string[] | null) => {
    calls.push({ targetDir, policy, sources });
    const next = script.shift();
    if (!next) throw new Error(`假传输层被调用 ${calls.length} 次，超出剧本`);
    return next;
  };
  return { paste, calls };
}

const done = (o: Partial<PasteOutcome> = {}): PasteResponse => ({
  status: 'done', created: [], failed: [], skipped: [], replaced: [], skipped_by_user: [], ...o,
});

const conflictResp = (names: string[]): PasteResponse => ({
  status: 'conflict',
  conflicts: names.map(name => ({ name, existing_is_dir: false })),
  sources: names.map(n => `D:/src/${n}`),
});

/** 让探测那轮的微任务全部跑完（`pasteInto` 会停在 `await ask(...)` 上等用户） */
const settleMicrotasks = () => new Promise(r => setTimeout(r, 0));

afterEach(() => {
  // 弹框是模块级单例：用例失败在中间时它可能还挂着，会串到下一个用例
  usePasteConflictStore.getState().settle(null);
  setNotifier(null);
});

test('没有同名冲突：只探测一次就粘完，不弹框也不问第二遍', async () => {
  const notices: NotifyOptions[] = [];
  setNotifier(o => notices.push(o));
  const { paste, calls } = fakePaste([done({ created: ['D:/p/a.txt'] })]);
  const busy: boolean[] = [];
  let refreshed = 0;

  await pasteInto('D:/p', () => { refreshed++; }, b => busy.push(b), paste);

  assert.equal(calls.length, 1, '没有同名就不该有第二次调用');
  assert.deepEqual(calls[0], { targetDir: 'D:/p', policy: null, sources: null });
  assert.equal(notices.length, 1, '要报一条结果');
  assert.equal(notices[0].title, '已粘贴「a.txt」');
  assert.equal(refreshed, 1, '粘完要刷新目录');
  assert.deepEqual(busy, [true, false], '进行中提示要有始有终');
});

test('有同名冲突：先弹框，拿到策略后带上源清单再调一次', async () => {
  const notices: NotifyOptions[] = [];
  setNotifier(o => notices.push(o));
  const probe = conflictResp(['a.txt']);
  const { paste, calls } = fakePaste([
    probe,
    done({ created: ['D:/p/a.txt'], replaced: ['D:/p/a.txt'] }),
  ]);
  let refreshed = 0;

  const flow = pasteInto('D:/p', () => { refreshed++; }, () => {}, paste);
  await settleMicrotasks();

  assert.deepEqual(
    usePasteConflictStore.getState().pending,
    probe,
    '探测到同名必须先弹框，而不是自作主张',
  );
  assert.equal(calls.length, 1, '用户还没选，不该已经复制了');

  usePasteConflictStore.getState().settle('overwrite');
  await flow;

  assert.equal(calls.length, 2);
  assert.deepEqual(
    calls[1],
    { targetDir: 'D:/p', policy: 'overwrite', sources: ['D:/src/a.txt'] },
    '第二次必须带上策略，并把探测那轮的源清单原样传回',
  );
  assert.equal(refreshed, 1);
  assert.ok(notices[0].description?.includes('回收站'), `要报清旧文件去哪了: ${notices[0].description}`);
});

test('取消：一次都不复制，也不弹结果提示（磁盘上什么都没发生）', async () => {
  const notices: NotifyOptions[] = [];
  setNotifier(o => notices.push(o));
  const { paste, calls } = fakePaste([conflictResp(['a.txt'])]);
  let refreshed = 0;

  const flow = pasteInto('D:/p', () => { refreshed++; }, () => {}, paste);
  await settleMicrotasks();
  usePasteConflictStore.getState().settle(null);
  await flow;

  assert.equal(calls.length, 1, '取消之后不该再调后端');
  assert.equal(refreshed, 0, '什么都没粘，刷新没有意义');
  assert.equal(notices.length, 0, '用户主动取消，不必再挨一条提示');
});

test('第二次仍要求决策：报错说清楚，而不是静默什么都不做', async () => {
  const notices: NotifyOptions[] = [];
  setNotifier(o => notices.push(o));
  // 后端没按策略执行（策略没落地）——用户点了「覆盖」却什么都没发生，必须说出来
  const { paste } = fakePaste([conflictResp(['a.txt']), conflictResp(['a.txt'])]);

  const flow = pasteInto('D:/p', () => {}, () => {}, paste);
  await settleMicrotasks();
  usePasteConflictStore.getState().settle('overwrite');
  await flow;

  assert.equal(notices.length, 1, '不能让这次粘贴悄无声息地结束');
  assert.equal(notices[0].variant, 'error');
  assert.ok(notices[0].title.includes('未执行'), `实际: ${notices[0].title}`);
});

test('后端报错：走错误提示，且进行中状态一定要收回来', async () => {
  const notices: NotifyOptions[] = [];
  setNotifier(o => notices.push(o));
  const paste = async () => { throw new Error('剪贴板中没有文件'); };
  const busy: boolean[] = [];

  await pasteInto('D:/p', () => {}, b => busy.push(b), paste);

  assert.equal(notices[0].variant, 'error');
  assert.equal(notices[0].description, '剪贴板中没有文件');
  assert.deepEqual(busy, [true, false], '抛错也必须退掉"正在粘贴…"');
});
