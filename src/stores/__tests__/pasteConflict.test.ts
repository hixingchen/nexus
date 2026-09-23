import { test } from 'node:test';
import assert from 'node:assert/strict';

import { usePasteConflictStore } from '../pasteConflict.ts';
import type { PasteResponse } from '../../utils/pasteResult.ts';

/** 探测那一轮的完整回包——`ask` 收的就是它（含源清单，决策后要原样回传后端） */
type ConflictResponse = Extract<PasteResponse, { status: 'conflict' }>;

/**
 * 粘贴同名弹框的状态机。
 *
 * 守的是一处**只有 Promise 才会有的**失败模式：`ask` 返回的 Promise 若没被结掉，
 * 调用方（`pasteInto`）就永远停在 `await` 上——症状是那一行"正在粘贴…"再也不消失，
 * 而且没有任何报错。触发它的现实路径不罕见：弹框开着时用户去点了另一处的粘贴。
 */

const conflict: ConflictResponse = {
  status: 'conflict',
  conflicts: [{ name: 'a.txt', existing_is_dir: false }],
  sources: ['D:/s/a.txt'],
};

test('ask 打开弹框，settle 把选择交给调用方', async () => {
  const { ask, settle } = usePasteConflictStore.getState();
  const answer = ask(conflict);
  assert.deepEqual(usePasteConflictStore.getState().pending, conflict, 'ask 之后弹框该是打开的');

  settle('overwrite');
  assert.equal(await answer, 'overwrite');
  assert.equal(usePasteConflictStore.getState().pending, null, '结掉之后弹框该关掉');
});

test('取消（settle null）也要把 Promise 结掉', async () => {
  const { ask, settle } = usePasteConflictStore.getState();
  const answer = ask(conflict);
  settle(null);
  assert.equal(await answer, null);
  assert.equal(usePasteConflictStore.getState().pending, null);
});

test('弹框还开着时又发起一次 ask：前一个必须被结掉，而不是永远悬着', async () => {
  const { ask, settle } = usePasteConflictStore.getState();
  const first = ask(conflict);
  // 第二个冲突的形状与第一个不同，用来确认"结掉的是前一个、生效的是后一个"
  const second: ConflictResponse = {
    status: 'conflict',
    conflicts: [{ name: 'b.txt', existing_is_dir: true }],
    sources: ['D:/s/b'],
  };
  const secondAnswer = ask(second);

  assert.equal(await first, null, '被顶掉的那次必须按"取消"结掉，否则那个入口的 await 永远不返回');
  assert.deepEqual(usePasteConflictStore.getState().pending, second, '弹框要显示的是后一次');

  settle('skip');
  assert.equal(await secondAnswer, 'skip');
});

test('settle 之后 resolver 已清空：重复 settle 不会把上一次的选择再送一遍', async () => {
  const { ask, settle } = usePasteConflictStore.getState();
  const answer = ask(conflict);
  settle('rename');
  settle('overwrite'); // 弹框的 onClose 与按钮可能同帧先后来到

  assert.equal(await answer, 'rename');
  assert.equal(usePasteConflictStore.getState().pending, null);
  assert.equal(usePasteConflictStore.getState().resolver, null);
});
