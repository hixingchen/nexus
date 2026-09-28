import { create } from 'zustand';

/**
 * 弹窗表单的**草稿**（按"弹窗 + 目标"为键，存在模块级 store）。
 *
 * 背景（UX-14）：这些弹窗的字段是 `useState` 从 prop 初始化的，关掉再打开就被重置
 * （或整个组件随父级卸载：ProjectList 在窄轨收起时整块卸载、ToolsManagerModal 挂在
 * 会卸载的 ServiceEditPanel 里）。于是在"新建项目 / 编辑项目 / 工具库"里填了一半，
 * 鼠标在暗色遮罩上点一下、或者顺手按个 Esc，内容**无声消失**——而同一份内容在
 * ServiceEditPanel 里有草稿保护（见 stores/serviceDraftStore），同一应用两套语义。
 *
 * 语义（与 ServiceEditPanel 对齐）：
 * - 关闭（点遮罩 / Esc / ✕）＝ 只是离开：草稿留下，重新打开同一个弹窗时恢复；
 * - 提交成功 / 用户显式放弃（「取消」按钮）＝ 清草稿——否则放弃过的编辑会在下次
 *   打开时"复活"。
 *
 * 为什么不复用 serviceDraftStore：
 * ① 它的键空间由 `pruneServiceDrafts` 按服务/模板前缀清理（`kind='service'` 会删掉
 *    所有不以 `tpl-` 开头的键）——项目/工具草稿放进去会被别处的正常加载**静默误删**；
 * ② 它的 `ServiceFormDraft` 是服务表单的字段集，装不下项目名/路径这类字段；
 * ③ CloseGuard 按它计数并说"另有 N 个**服务/模板**的配置改动未保存"，混进弹窗草稿
 *    会让那句提示说谎。
 */

/** 项目弹窗（新建 / 编辑共用）：两个字段就是它的全部表单 */
export interface ProjectFormDraft {
  name: string;
  path: string;
}

/** 工具库弹窗：`editingId` 为 null 表示"新增"模式（恢复时按 id 现查，见使用处） */
export interface ToolFormDraft {
  editingId: string | null;
  name: string;
  executable: string;
  args: string;
}

/**
 * 草稿键：定值 + 目标 id。编辑项目按项目 id 分开存——切到另一个项目再切回来，
 * 两个项目各自的草稿互不覆盖。
 */
export const MODAL_DRAFT_KEYS = {
  projectCreate: 'modal:project:create',
  projectEdit: (id: string) => `modal:project:edit:${id}`,
  tools: 'modal:tools',
} as const;

interface ModalDraftState {
  drafts: Record<string, unknown>;
  /** draft 传 null 等价于清除（与 serviceDraftStore 同款：无变化时不制造新引用） */
  setDraft: (key: string, draft: unknown | null) => void;
}

export const useModalDraftStore = create<ModalDraftState>((set) => ({
  drafts: {},
  setDraft: (key, draft) => set((s) => {
    if (draft === null) {
      if (!(key in s.drafts)) return s;
      const rest = { ...s.drafts };
      delete rest[key];
      return { drafts: rest };
    }
    return { drafts: { ...s.drafts, [key]: draft } };
  }),
}));

/**
 * 读草稿（挂载时 / 重新打开时初始化表单用）。
 *
 * 值只在**本进程内存**里流转、且只由下方两个使用方写入，形状由写入方保证，
 * 故这里用泛型断言而不是解析/校验（不是 DB/文件那种不受信数据）。
 */
export function readModalDraft<T>(key: string): T | undefined {
  return useModalDraftStore.getState().drafts[key] as T | undefined;
}
