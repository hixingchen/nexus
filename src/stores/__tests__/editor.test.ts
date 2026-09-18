import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  useEditorStore,
  saveActiveFile,
  commitReloadedContent,
  settlePendingEdit,
  setPendingEditSource,
  clearPendingEditSource,
  unsavedDraftCount,
} from '../editor.ts';
import type { FileTab } from '../../types/editor.ts';

/**
 * 编辑器 store 的草稿/未保存状态机。
 *
 * 这组用例集中在**一处失败模式**上：内容读旧了。它会同时毁掉两个方向——
 * 保存时把旧内容写进磁盘（丢用户的编辑），关窗判定时看不见未保存的改动（丢草稿）。
 * 尤其是 PERF-13 引入的"大文档合帧"：编辑在 200ms 窗口里还没写回 store 时，
 * 任何按旧值读取的路径都是数据丢失。
 *
 * 本文件能跑是因为 `test/ts-hooks.mjs` 给 Node 补了无扩展名 import 的解析
 * （见 package.json 的 test 脚本）。
 */

let seq = 0;
/** 每个用例用独立 id，避免共享的模块级 Map（baselines/drafts）互相干扰 */
function freshTab(path: string): FileTab {
  seq += 1;
  return { id: `t${seq}`, name: path.split('/').pop() ?? path, path };
}

/**
 * 重置 store 的可变状态。模块级的 baselines/drafts 不在这里清——它们按 tab id 索引，
 * 而每个用例的 id 都不同（freshTab），不会互相干扰；会跨用例串的是 store 里的
 * tabs/activeTabId/dirtyIds（`updateDraft` 按活动标签判定，残留的活动标签会让断言错位）。
 */
function resetStore(): void {
  useEditorStore.setState({ tabs: [], activeTabId: null, fileContent: null, dirtyIds: [] });
}

function openTab(path: string, content: string): FileTab {
  const tab = freshTab(path);
  useEditorStore.getState().openTab(tab, content);
  return tab;
}

test('新打开的标签内容即基线：没有未保存改动', () => {
  resetStore();
  openTab('/p/a.ts', 'const a = 1;\n');
  assert.equal(useEditorStore.getState().dirtyIds.length, 0);
  assert.equal(unsavedDraftCount(), 0);
});

test('编辑后标记未保存，且内容回写到 fileContent（活动标签）', () => {
  resetStore();
  const tab = openTab('/p/b.ts', 'x');
  useEditorStore.getState().updateDraft('x1');
  const st = useEditorStore.getState();
  assert.equal(st.fileContent, 'x1');
  assert.deepEqual(st.dirtyIds, [tab.id]);
});

test('改回原内容即不再算未保存（与基线比较，不是"改过就一直脏"）', () => {
  resetStore();
  const tab = openTab('/p/c.ts', 'abc');
  useEditorStore.getState().updateDraft('abcd');
  assert.deepEqual(useEditorStore.getState().dirtyIds, [tab.id]);
  useEditorStore.getState().updateDraft('abc');
  assert.deepEqual(useEditorStore.getState().dirtyIds, [], '回到基线内容应清掉未保存标记');
});

test('CRLF 归一化后与基线一致 → 不算未保存（Windows 文件撤销回原样不该显示已改）', () => {
  resetStore();
  const tab = openTab('/p/d.ts', 'a\nb');
  useEditorStore.getState().updateDraft('a\r\nb');
  assert.deepEqual(useEditorStore.getState().dirtyIds, [], `CRLF 与 LF 应视为同一内容：${tab.id}`);
});

// ── 甲-①：无改动时 Ctrl+S 不写盘 ──────────────────────────────

test('没有未保存改动时 Ctrl+S 直接返回，不去写盘（甲-①）', async () => {
  resetStore();
  openTab('/p/clean.ts', 'const a = 1;\n');
  // 干净标签：必须在不触碰 IPC 的情况下返回 true。
  // 若哪天有人把这道跳过删了，这个测试会因为"没有 Tauri 环境却去调 invoke"而失败——
  // 它守的正是"打开文件 → 别的工具改了它 → 按 Ctrl+S 却弹冲突"那个困惑。
  assert.equal(await saveActiveFile(), true, '干净标签应直接返回成功');
  assert.deepEqual(useEditorStore.getState().dirtyIds, [], '不该因此产生未保存标记');
});

// ── 甲-②：重新加载的落地不变量 ────────────────────────────────

test('重载后必须是「干净 + 新内容 + 新会话」（用户实测抓到过：重载后标签仍是未保存）', () => {
  resetStore();
  const tab = openTab('/p/reload.ts', 'V1');
  useEditorStore.getState().updateDraft('V1-改过的东西'); // 模拟：重载前有未保存改动
  assert.deepEqual(useEditorStore.getState().dirtyIds, [tab.id], '前提：重载前是脏的');
  const seqBefore = useEditorStore.getState().fileOpenSeq[tab.path] ?? 0;

  commitReloadedContent(tab.id, 'V2');

  const st = useEditorStore.getState();
  assert.deepEqual(st.dirtyIds, [], '重载后不能还挂着未保存标记（那正是用户看到圆点的原因）');
  assert.equal(st.fileContent, 'V2', '内容槽要换成磁盘上的新内容');
  assert.equal(st.fileOpenSeq[tab.path], seqBefore + 1, '要换会话序号：编辑器据此重建并清撤销历史');

  // 编辑器随后的"回声"（重建/同步那一拍会把同样的内容再报一次）不该把它重新标脏
  useEditorStore.getState().updateDraft('V2');
  assert.deepEqual(useEditorStore.getState().dirtyIds, [], '重载后编辑器自己的回声不该重新标脏');
});

test('重载会丢掉未保存改动（这就是它必须是显式动作、且提示里要说"请先复制"的原因）', () => {
  resetStore();
  const tab = openTab('/p/reload2.ts', 'V1');
  useEditorStore.getState().updateDraft('未保存的新内容');
  commitReloadedContent(tab.id, 'V2');
  useEditorStore.getState().updateDraft('V2'); // 回声
  // 回不到"未保存的新内容"——草稿已在提交时清掉（撤销历史也随之清空，见会话序号）
  assert.equal(useEditorStore.getState().fileContent, 'V2');
});

// ── PERF-13：未合帧编辑（大文档）的结算契约 ────────────────────

test('未合帧的编辑必须先被结算，关窗守卫才数得到它（否则"改一下立刻关窗"丢编辑）', () => {
  resetStore();
  const tab = openTab('/p/big.ts', 'A'.repeat(10));
  // 模拟大文档路径：编辑器登记了"取最新内容"的入口，但还没写回 store
  setPendingEditSource(tab.id, () => 'A'.repeat(10) + 'B');
  assert.deepEqual(useEditorStore.getState().dirtyIds, [], '登记本身不该改状态（还没结算）');

  assert.equal(unsavedDraftCount(), 1, '关窗判定必须先结算再计数');
  assert.deepEqual(useEditorStore.getState().dirtyIds, [tab.id]);
  assert.equal(useEditorStore.getState().fileContent, 'A'.repeat(10) + 'B', '内容要落到 store');
  clearPendingEditSource();
});

test('结算按登记的 tabId 落库，不认当时的活动标签（切走后再结算不会串标签）', () => {
  resetStore();
  const tabA = openTab('/p/one.ts', 'one');
  setPendingEditSource(tabA.id, () => 'one-edited');
  // 用户切到另一个标签：此刻 activeTabId 已不是 tabA
  const tabB = openTab('/p/two.ts', 'two');
  assert.equal(useEditorStore.getState().activeTabId, tabB.id);

  settlePendingEdit();

  const st = useEditorStore.getState();
  assert.deepEqual(st.dirtyIds, [tabA.id], `改动必须记在 tabA 名下，实际：${JSON.stringify(st.dirtyIds)}`);
  assert.equal(st.fileContent, 'two', 'tabB 的内容不得被 tabA 的编辑覆盖');
});

test('没有待结算内容时结算与计数都是 no-op（登记槽为空）', () => {
  resetStore();
  openTab('/p/e.ts', 'e');
  clearPendingEditSource();
  const before = useEditorStore.getState().dirtyIds.length;
  settlePendingEdit();
  assert.equal(useEditorStore.getState().dirtyIds.length, before);
  assert.equal(unsavedDraftCount(), before);
});

test('取过一次就不再重复结算（避免同一份内容被反复记为改动）', () => {
  resetStore();
  const tab = openTab('/p/f.ts', 'f');
  let calls = 0;
  setPendingEditSource(tab.id, () => { calls += 1; return 'f-edited'; });

  settlePendingEdit();
  settlePendingEdit();
  assert.equal(calls, 1, '同一份待结算内容只应被取一次');

  // 再改回基线内容：未保存标记按基线比较自动清掉
  useEditorStore.getState().updateDraft('f');
  assert.deepEqual(useEditorStore.getState().dirtyIds, []);
  clearPendingEditSource();
});

test('取到的内容是空串也照常落库（清空文件的编辑不能被当成"没有待结算内容"）', () => {
  resetStore();
  const tab = openTab('/p/g.ts', 'g');
  setPendingEditSource(tab.id, () => '');
  settlePendingEdit();
  const st = useEditorStore.getState();
  assert.equal(st.fileContent, '');
  assert.deepEqual(st.dirtyIds, [tab.id], '清空文件同样是未保存改动');
  clearPendingEditSource();
});
