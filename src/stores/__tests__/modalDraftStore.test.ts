import { test } from 'node:test';
import assert from 'node:assert/strict';

import { useModalDraftStore, readModalDraft, MODAL_DRAFT_KEYS, type ProjectFormDraft } from '../modalDraftStore.ts';
import { useServiceDraftStore, pruneServiceDrafts, type ServiceFormDraft } from '../serviceDraftStore.ts';

/**
 * 弹窗表单草稿（UX-14）的语义。
 *
 * 守两条：① "关闭＝先离开"能否真正恢复内容（写进去、读得回来、显式放弃才清）；
 * ② 与**服务/模板草稿**互不干扰——这正是它单独一个 store、而不是塞进
 * serviceDraftStore 的理由：那边的 `pruneServiceDrafts` 会按服务/模板前缀清理键空间，
 * 项目/工具草稿放进去会被别处的正常加载静默删掉。第 ② 条这里用真 pruner 实测。
 */

/** 每个用例从空 store 开始（模块级单例：不清会互相串味） */
function reset(): void {
  useModalDraftStore.setState({ drafts: {} });
  useServiceDraftStore.setState({ drafts: {} });
}

test('写进去的草稿读得回来（关闭重开时靠它恢复表单）', () => {
  reset();
  useModalDraftStore.getState().setDraft(MODAL_DRAFT_KEYS.projectCreate, { name: '半填的项目', path: 'C:/x' });
  assert.deepEqual(readModalDraft<ProjectFormDraft>(MODAL_DRAFT_KEYS.projectCreate), { name: '半填的项目', path: 'C:/x' });
});

test('显式放弃（null）清掉草稿；对本来就没有的键是空操作', () => {
  reset();
  const key = MODAL_DRAFT_KEYS.tools;
  useModalDraftStore.getState().setDraft(key, { editingId: null, name: 't', executable: '', args: '' });
  useModalDraftStore.getState().setDraft(key, null);
  assert.equal(readModalDraft(key), undefined);

  // 空操作：不制造新引用（否则订阅者会被无意义的写入叫醒）
  const before = useModalDraftStore.getState().drafts;
  useModalDraftStore.getState().setDraft(key, null);
  assert.equal(useModalDraftStore.getState().drafts, before);
});

test('编辑项目的草稿按项目 id 分键：两个项目各自的内容互不覆盖', () => {
  reset();
  useModalDraftStore.getState().setDraft(MODAL_DRAFT_KEYS.projectEdit('a'), { name: 'A 改了一半', path: '/a' });
  useModalDraftStore.getState().setDraft(MODAL_DRAFT_KEYS.projectEdit('b'), { name: 'B 改了一半', path: '/b' });
  assert.equal(readModalDraft<ProjectFormDraft>(MODAL_DRAFT_KEYS.projectEdit('a'))?.name, 'A 改了一半');
  assert.equal(readModalDraft<ProjectFormDraft>(MODAL_DRAFT_KEYS.projectEdit('b'))?.name, 'B 改了一半');
});

test('服务/模板草稿的清理不会误删弹窗草稿（两套键空间隔离）', () => {
  reset();
  const serviceDraft: ServiceFormDraft = {
    name: 'svc', command: 'run', cwd: 'C:/x', watchPaths: '', watchInclude: '*', watchExclude: '',
    envVars: '', restartMode: 0, enabled: true, showFileTree: false, toolCommands: [], tplToolId: '',
    bootEnabled: false, bootOrder: 0, bootCheckKind: 'none', bootCheckValue: '', bootRequired: false,
    bootCredentialMode: 's4u',
  };
  useServiceDraftStore.getState().setDraft('svc-1', serviceDraft);
  useModalDraftStore.getState().setDraft(MODAL_DRAFT_KEYS.projectCreate, { name: 'n', path: 'p' });

  // 服务列表加载时按"当前项目里还剩哪些服务"清理：'svc-1' 不在列表里 → 删；
  // 弹窗草稿不属于任何服务，必须原样活着
  pruneServiceDrafts('service', []);

  assert.equal(useServiceDraftStore.getState().drafts['svc-1'], undefined);
  assert.equal(readModalDraft<ProjectFormDraft>(MODAL_DRAFT_KEYS.projectCreate)?.name, 'n');
});
