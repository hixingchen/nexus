import { create } from 'zustand';

interface SearchModalState {
  open: boolean;
  /** 搜索根目录（服务 cwd / 目录树节点路径） */
  root: string;
  /** 展示用标题（服务名 / 路径名） */
  title: string;
  openSearch: (root: string, title: string) => void;
  closeSearch: () => void;
}

/** 全局文件内容搜索弹窗（入口：左侧项目服务行 / 目录树节点右键菜单） */
export const useSearchModalStore = create<SearchModalState>((set) => ({
  open: false,
  root: '',
  title: '',
  openSearch: (root, title) => set({ open: true, root, title }),
  closeSearch: () => set({ open: false }),
}));
