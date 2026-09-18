import { create } from 'zustand';
import type { ToolCommand } from '../services/service';

/**
 * 服务/模板编辑面板的**表单草稿**（按编辑对象 id 存在模块级 store）。
 *
 * 背景：面板的字段全是 `useState` 从 prop 初始化，而挂载点带 key
 * （`ProjectDetail.tsx` 的 `key={editingService.id}` / `key={tpl-...}`）——
 * 于是"切换编辑对象"就等于卸载重挂载，未保存的编辑**无声消失**。共六条触发路径：
 * ① 点另一张服务卡 ② 再点同一张卡（收起）③ 点「日志」④ 收起服务列
 * ⑤ 服务↔模板互切 ⑥ 切换项目。
 *
 * 为什么用"草稿保存"而不是"每条路径弹确认"：六条路径里有五条是用户在做正常导航
 * （看日志、切换服务），挨个弹确认会很烦；草稿存下来后这些切换全部无损——面板重新
 * 打开时按草稿恢复并显示"未保存"标记，只有保存成功或显式「放弃修改」才清掉。
 * 切项目同样无损：draft 按 id 存，切回来还在。
 */
export interface ServiceFormDraft {
  name: string;
  command: string;
  cwd: string;
  watchPaths: string;
  watchInclude: string;
  watchExclude: string;
  envVars: string;
  restartMode: number;
  enabled: boolean;
  showFileTree: boolean;
  toolCommands: ToolCommand[];
  /** 模板模式的默认打开工具（服务模式不使用） */
  tplToolId: string;
}

interface DraftState {
  drafts: Record<string, ServiceFormDraft>;
  /** draft 传 null 等价于清除 */
  setDraft: (key: string, draft: ServiceFormDraft | null) => void;
}

export const useServiceDraftStore = create<DraftState>((set) => ({
  drafts: {},
  setDraft: (key, draft) => set((s) => {
    if (draft === null) {
      if (!(key in s.drafts)) return s; // 无变化：不制造新引用，避免多余重渲染
      const rest = { ...s.drafts };
      delete rest[key];
      return { drafts: rest };
    }
    return { drafts: { ...s.drafts, [key]: draft } };
  }),
}));

/** 草稿键：模板加前缀，避免与服务 id 撞键（与服务/模板共用一个面板组件） */
export function serviceDraftKey(id: string, mode: 'service' | 'template'): string {
  return mode === 'template' ? `tpl-${id}` : id;
}

/** 非 React 环境读草稿（挂载时初始化表单用） */
export function readServiceDraft(key: string): ServiceFormDraft | undefined {
  return useServiceDraftStore.getState().drafts[key];
}


/**
 * 清理已不存在的编辑目标留下的草稿（服务/模板被删除后）。
 * 不做的话关窗确认会一直把幽灵草稿算进去；也避免同 id 复用时旧草稿"复活"。
 *
 * 按类别清理：`kind='service'` 只看服务键（不以 `tpl-` 开头），`kind='template'` 只看模板键。
 * 两类草稿由不同的加载路径（项目详情 / 模板库）分别清理，混在一起会互相清空。
 */
export function pruneServiceDrafts(kind: 'service' | 'template', validIds: string[]): void {
  const valid = new Set(validIds.map(id => serviceDraftKey(id, kind)));
  const { drafts } = useServiceDraftStore.getState();
  const stale = Object.keys(drafts).filter(
    k => (kind === 'template') === k.startsWith('tpl-') && !valid.has(k),
  );
  if (stale.length === 0) return;
  const next = { ...drafts };
  for (const k of stale) delete next[k];
  useServiceDraftStore.setState({ drafts: next });
}
