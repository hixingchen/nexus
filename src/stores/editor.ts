import { create } from 'zustand';
import { showNotification } from '../components/ui/Toast';
import type { FileTab } from '../types/editor';
import * as editorService from '../services/editor';

interface EditorStore {
  tabs: FileTab[];
  activeTabId: string | null;
  fileContent: string | null;

  /** 同步操作：打开标签页并设置内容 */
  openTab: (tab: FileTab, content: string) => void;
  closeTab: (id: string) => void;
  setActiveTabId: (id: string) => void;
  setFileContent: (content: string | null) => void;
}

/** 文件内容缓存，避免切换标签时重复读取（LRU，最多 50 个文件） */
const MAX_CACHE_SIZE = 50;
const fileCache = new Map<string, string>();

function getCachedContent(path: string): string | undefined {
  const cached = fileCache.get(path);
  if (cached !== undefined) {
    fileCache.delete(path);
    fileCache.set(path, cached);
  }
  return cached;
}

function setCacheContent(path: string, content: string): void {
  if (fileCache.size >= MAX_CACHE_SIZE) {
    const firstKey = fileCache.keys().next().value;
    if (firstKey) fileCache.delete(firstKey);
  }
  fileCache.set(path, content);
}

function removeCacheForPath(path: string): void {
  fileCache.delete(path);
}

export const useEditorStore = create<EditorStore>((set, get) => ({
  tabs: [],
  activeTabId: null,
  fileContent: null,

  openTab: (tab, content) => {
    const { tabs } = get();
    setCacheContent(tab.path, content);
    set({
      tabs: [...tabs, tab],
      activeTabId: tab.id,
      fileContent: content,
    });
  },

  closeTab: (id) => {
    const { tabs, activeTabId } = get();
    const closedTab = tabs.find(t => t.id === id);
    const newTabs = tabs.filter(t => t.id !== id);
    let newActiveId = activeTabId;

    if (activeTabId === id) {
      const idx = tabs.findIndex(t => t.id === id);
      newActiveId = newTabs[Math.min(idx, newTabs.length - 1)]?.id ?? null;
    }

    if (closedTab && !newTabs.some(t => t.path === closedTab.path)) {
      removeCacheForPath(closedTab.path);
    }

    set({ tabs: newTabs, activeTabId: newActiveId });
  },

  setActiveTabId: (id) => {
    set({ activeTabId: id });
  },

  setFileContent: (content) => {
    set({ fileContent: content });
  },
}));

// ── 异步操作（组件调用） ──────────────────────────────────

/**
 * 打开文件：检查缓存 → 读取内容 → 更新 store
 * 由组件调用，store 不直接执行异步操作
 */
export async function loadAndOpenFile(path: string, name: string): Promise<void> {
  const { tabs, openTab, setActiveTabId, setFileContent } = useEditorStore.getState();

  // 检查是否已打开
  const existing = tabs.find(t => t.path === path);
  if (existing) {
    setActiveTabId(existing.id);
    const cached = getCachedContent(path);
    if (cached !== undefined) {
      setFileContent(cached);
      return;
    }
    try {
      const content = await editorService.readFile(path);
      setCacheContent(path, content);
      setFileContent(content);
    } catch (e) {
      console.error('读取文件内容失败:', e);
      showNotification({ variant: 'error', title: '读取文件内容失败' });
      setFileContent(null);
    }
    return;
  }

  // 新标签
  const tab: FileTab = { id: `tab-${Date.now()}`, name, path };
  try {
    const content = await editorService.readFile(path);
    openTab(tab, content);
  } catch (e) {
    console.error('读取文件内容失败:', e);
    showNotification({ variant: 'error', title: '读取文件内容失败' });
    openTab(tab, '');
  }
}

/**
 * 切换活动标签：检查缓存 → 读取内容 → 更新 store
 */
export async function switchToTab(id: string): Promise<void> {
  const { tabs, setActiveTabId, setFileContent } = useEditorStore.getState();
  const tab = tabs.find(t => t.id === id);
  if (!tab) return;

  setActiveTabId(id);
  const cached = getCachedContent(tab.path);
  if (cached !== undefined) {
    setFileContent(cached);
    return;
  }
  try {
    const content = await editorService.readFile(tab.path);
    setCacheContent(tab.path, content);
    setFileContent(content);
  } catch (e) {
    console.error('读取文件内容失败:', e);
    showNotification({ variant: 'error', title: '读取文件内容失败' });
    setFileContent(null);
  }
}
