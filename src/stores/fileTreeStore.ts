import { create } from 'zustand';

/**
 * 文件树选中态（PERF-18）。
 *
 * 为什么放进 store 而不是 `FileTree` 的 prop：选中态是「全树唯一、每次点击都变」的值，
 * 作为 prop 下发会让**每个**已挂载 `Entry` 的 `memo` 比较失败——点开一个文件就重渲染
 * 整棵已展开子树（深树展开到 2000 节点时约 2000 次组件渲染 + 约 8k 元素创建/次选中）。
 *
 * 放进 store 后，每个节点只订阅 `s => s.selectedPath === e.path` 这个**布尔值**：
 * 一次选中只有"失去选中的那个节点"与"获得选中的那个节点"会重渲染，其余节点的选择器
 * 结果不变（zustand 用 Object.is 比较订阅结果，布尔不变即不触发渲染）。
 */
interface FileTreeState {
  /** 当前选中的节点路径（= 活动标签的路径，或用户刚刚点击的那一项） */
  selectedPath: string | null;
  setSelectedPath: (path: string | null) => void;
}

export const useFileTreeStore = create<FileTreeState>((set) => ({
  selectedPath: null,
  setSelectedPath: (path) => set({ selectedPath: path }),
}));
