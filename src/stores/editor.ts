import { create } from 'zustand';
import { showNotification } from '../components/ui/Toast';
import type { FileTab } from '../types/editor';
import * as editorService from '../services/editor';

interface EditorStore {
  tabs: FileTab[];
  activeTabId: string | null;
  fileContent: string | null;
  /** 有未保存更改的标签 id */
  dirtyIds: string[];
  /** 定位请求：打开文件后滚动到指定行并高亮命中词（CodeViewer 消费后清除） */
  locate: { path: string; line: number; query: string } | null;
  /** 高亮清除信号：每次 +1，CodeViewer 监听后清空命中装饰 */
  hitSeq: number;
  /** 目录树定位请求：当前打开的文件（EditorTabs 定位图标触发，FileTree 消费） */
  revealPath: string | null;
  /** 定位触发信号：每次请求 +1（同一路径重复点击也生效） */
  revealSeq: number;

  /** 同步操作：打开标签页并设置内容 */
  openTab: (tab: FileTab, content: string) => void;
  closeTab: (id: string) => void;
  /** 批量关闭标签（逐个复用 closeTab 逻辑，含活动标签切换与草稿清理） */
  closeTabs: (ids: string[]) => void;
  setActiveTabId: (id: string) => void;
  setFileContent: (content: string | null) => void;
  /** 编辑器内容变更：写入草稿并标记当前标签未保存 */
  updateDraft: (content: string) => void;
  markDirty: (id: string) => void;
  markClean: (id: string) => void;
  setLocate: (locate: { path: string; line: number; query: string }) => void;
  clearLocate: () => void;
  /** 清除编辑器中的搜索命中高亮 */
  clearHits: () => void;
  /** 请求在目录树中定位指定文件（EditorTabs 定位图标调用） */
  requestReveal: (path: string) => void;
  /** 清除定位标记（用户主动切换文件/选中树节点时调用，避免定位高亮残留） */
  clearReveal: () => void;
}

/**
 * 文件内容缓存，避免切换标签时重复读取（LRU，最多 50 个文件 / 64MB）。
 * 字节上限：后端单文件读取上限 50MB，若仅按文件数限制最坏可驻留 ~2.5GB。
 */
const MAX_CACHE_SIZE = 50;
const MAX_CACHE_BYTES = 64 * 1024 * 1024;
const fileCache = new Map<string, string>();
let cacheBytes = 0;

/** 编辑大小上限（与后端 write_file 的 10MB 一致）：超过的文件只读查看 */
const MAX_EDIT_SIZE = 10 * 1024 * 1024;

/** 内建预览支持的图片扩展名（webview 原生解码，不走系统程序） */
const IMAGE_EXTS = new Set(['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg', 'ico', 'bmp', 'avif']);
/** 常见二进制扩展名：直接进 hex 视图，免整文件读入嗅探（jar 有专属浏览器，不在此列） */
const BINARY_EXTS = new Set([
  'ttf', 'otf', 'woff', 'woff2', 'eot', 'exe', 'dll', 'so', 'dylib', 'zip',
  'gz', 'tgz', 'tar', '7z', 'rar', 'pdf', 'doc', 'docx', 'xls', 'xlsx', 'ppt', 'pptx',
  'mp3', 'mp4', 'wav', 'flac', 'avi', 'mkv', 'mov', 'psd', 'ai', 'bin', 'dat', 'wasm', 'pyc',
]);

const getExt = (path: string) => (path.split('.').pop() ?? '').toLowerCase();

/** jar:// 虚拟路径 → (jar 路径, 嵌套链, 条目名) */
export function parseJarVirtualPath(path: string): { jarPath: string; nested: string[]; name: string } {
  const parts = path.slice('jar://'.length).split('!/');
  const jarPath = parts[0];
  const name = parts[parts.length - 1];
  const nested = parts.slice(1, -1);
  return { jarPath, nested, name };
}

/** 读取文本内容：jar 内条目走 jar 读取，.class 走 CFR/字节码视图，其余走 read_file（二进制抛 BINARY） */
async function fetchTextContent(path: string): Promise<{ content: string; size: number }> {
  if (path.startsWith('jar://')) {
    const { jarPath, nested, name } = parseJarVirtualPath(path);
    const res = await editorService.readJarEntry(jarPath, nested, name);
    return { content: res.content, size: res.size };
  }
  if (getExt(path) === 'class') {
    const content = await editorService.readClassFile(path);
    return { content, size: 0 };
  }
  const res = await editorService.readFile(path);
  if (res.is_binary) throw new Error('BINARY');
  return { content: res.content, size: res.size };
}

/** 打开新标签（读取期间可能已被其他调用打开：已有标签则切换过去，恢复其未保存草稿） */
function openLoadedTab(tab: FileTab, content: string) {
  const already = useEditorStore.getState().tabs.find(t => t.path === tab.path);
  if (already) {
    setCacheContent(tab.path, content);
    const draft = drafts.get(already.id);
    useEditorStore.getState().setFileContent(draft ?? content);
    useEditorStore.getState().setActiveTabId(already.id);
  } else {
    useEditorStore.getState().openTab(tab, content);
  }
}

/**
 * 每标签：已打开/最后保存的基线内容（用于 dirty 推导）。
 * 编辑后内容与基线一致（如撤销回原样）→ 不算未保存
 */
const baselines = new Map<string, string>();
/**
 * 每标签：未保存的草稿内容。切换到该标签时优先用草稿恢复，
 * 否则草稿只存在 store.fileContent（活动标签）里，切换标签会丢失
 */
const drafts = new Map<string, string>();

function getCachedContent(path: string): string | undefined {
  const cached = fileCache.get(path);
  if (cached !== undefined) {
    fileCache.delete(path);
    fileCache.set(path, cached);
  }
  return cached;
}

function setCacheContent(path: string, content: string): void {
  const bytes = content.length * 2; // UTF-16 单元数
  const prev = fileCache.get(path);
  if (prev !== undefined) {
    cacheBytes -= prev.length * 2;
    fileCache.delete(path);
  }
  // 按总字节数或文件数上限淘汰最久未使用的条目
  while (fileCache.size > 0 && (cacheBytes + bytes > MAX_CACHE_BYTES || fileCache.size >= MAX_CACHE_SIZE)) {
    const firstKey = fileCache.keys().next().value;
    if (!firstKey) break;
    const old = fileCache.get(firstKey);
    if (old !== undefined) cacheBytes -= old.length * 2;
    fileCache.delete(firstKey);
  }
  cacheBytes += bytes;
  fileCache.set(path, content);
}

function removeCacheForPath(path: string): void {
  const prev = fileCache.get(path);
  if (prev !== undefined) {
    cacheBytes -= prev.length * 2;
    fileCache.delete(path);
  }
}

export const useEditorStore = create<EditorStore>((set, get) => ({
  tabs: [],
  activeTabId: null,
  fileContent: null,
  dirtyIds: [],
  locate: null,
  hitSeq: 0,
  revealPath: null,
  revealSeq: 0,

  openTab: (tab, content) => {
    const { tabs } = get();
    setCacheContent(tab.path, content);
    baselines.set(tab.id, content);
    set({
      tabs: [...tabs, tab],
      activeTabId: tab.id,
      fileContent: content,
    });
  },

  closeTab: (id) => {
    const { tabs, activeTabId, dirtyIds } = get();
    const closedTab = tabs.find(t => t.id === id);
    const newTabs = tabs.filter(t => t.id !== id);
    const newDirty = dirtyIds.filter(d => d !== id);
    baselines.delete(id);
    drafts.delete(id);

    if (closedTab && !newTabs.some(t => t.path === closedTab.path)) {
      removeCacheForPath(closedTab.path);
    }

    // 关闭的不是活动标签：无需处理内容
    if (activeTabId !== id) {
      set({ tabs: newTabs, dirtyIds: newDirty });
      return;
    }

    const idx = tabs.findIndex(t => t.id === id);
    const newActiveId = newTabs[Math.min(idx, newTabs.length - 1)]?.id ?? null;

    if (!newActiveId) {
      set({ tabs: newTabs, activeTabId: null, fileContent: null, dirtyIds: newDirty });
      return;
    }

    // 关闭活动标签后展示新活动标签的内容，避免残留已关闭文件的内容
    // 优先恢复未保存草稿，其次缓存，最后读盘
    const nextTab = newTabs.find(t => t.id === newActiveId)!;
    const draft = drafts.get(nextTab.id);
    if (draft !== undefined) {
      set({ tabs: newTabs, activeTabId: newActiveId, fileContent: draft, dirtyIds: newDirty });
    } else {
      const cached = getCachedContent(nextTab.path);
      if (cached !== undefined) {
        set({ tabs: newTabs, activeTabId: newActiveId, fileContent: cached, dirtyIds: newDirty });
      } else {
        set({ tabs: newTabs, activeTabId: newActiveId, dirtyIds: newDirty });
        void switchToTab(newActiveId);
      }
    }
  },

  closeTabs: (ids) => {
    for (const id of ids) get().closeTab(id);
  },

  setActiveTabId: (id) => {
    set({ activeTabId: id });
  },

  setFileContent: (content) => {
    set({ fileContent: content });
  },

  updateDraft: (content) => {
    const { activeTabId, dirtyIds } = get();
    if (!activeTabId) return;
    set({ fileContent: content });
    // 与基线一致（如 Ctrl+Z 撤销回已保存版本）→ 草稿作废并清 dirty；
    // 否则存草稿（切换标签时可恢复）并标 dirty
    const baseline = baselines.get(activeTabId);
    if (baseline !== undefined && baseline === content) {
      drafts.delete(activeTabId);
      if (dirtyIds.includes(activeTabId)) {
        set({ dirtyIds: dirtyIds.filter(d => d !== activeTabId) });
      }
    } else {
      drafts.set(activeTabId, content);
      if (!dirtyIds.includes(activeTabId)) {
        set({ dirtyIds: [...dirtyIds, activeTabId] });
      }
    }
  },

  markDirty: (id) => {
    const { dirtyIds } = get();
    if (!dirtyIds.includes(id)) set({ dirtyIds: [...dirtyIds, id] });
  },

  markClean: (id) => {
    const { dirtyIds, fileContent } = get();
    // 保存成功后：内容成为新基线，草稿作废
    baselines.set(id, fileContent ?? '');
    drafts.delete(id);
    if (dirtyIds.includes(id)) set({ dirtyIds: dirtyIds.filter(d => d !== id) });
  },

  setLocate: (locate) => {
    set({ locate });
  },

  clearLocate: () => {
    set({ locate: null });
  },

  /** 清除编辑器中的搜索命中高亮（搜索弹窗关闭时调用） */
  clearHits: () => {
    set({ hitSeq: get().hitSeq + 1 });
  },

  /** 目录树定位请求：路径 + 序号递增（同一文件重复定位也生效） */
  requestReveal: (path) => {
    set({ revealPath: path, revealSeq: get().revealSeq + 1 });
  },

  clearReveal: () => {
    set({ revealPath: null });
  },
}));

// ── 异步操作（组件调用） ──────────────────────────────────

let tabSeq = 0;
/** 同一 path 的并发读取去重（双击文件只创建一个标签） */
const pendingLoads = new Map<string, Promise<void>>();

/**
 * 打开文件：检查缓存 → 读取内容 → 更新 store
 * 由组件调用，store 不直接执行异步操作
 */
export async function loadAndOpenFile(path: string, name: string): Promise<void> {
  // jar 虚拟路径（树内展开的 jar 条目）：路由到 jar 条目打开，不读磁盘
  if (path.startsWith('jar://')) {
    const { jarPath, nested, name: entryName } = parseJarVirtualPath(path);
    await openJarEntry(jarPath, nested, { name: entryName });
    return;
  }
  // 检查是否已打开
  const existing = useEditorStore.getState().tabs.find(t => t.path === path);
  if (existing) {
    await loadContentInto(existing.id, path);
    return;
  }

  // 并发去重：同一 path 正在加载时复用，读取完成后切换过去
  const pending = pendingLoads.get(path);
  if (pending) {
    await pending;
    const tab = useEditorStore.getState().tabs.find(t => t.path === path);
    if (tab) useEditorStore.getState().setActiveTabId(tab.id);
    return;
  }

  const tab: FileTab = { id: `tab-${Date.now()}-${++tabSeq}`, name, path };
  const p = (async () => {
    try {
      const ext = getExt(path);
      if (IMAGE_EXTS.has(ext)) {
        // 图片：内建预览（ImageViewer 自行加载，内容不进 store）
        tab.readonly = true;
        tab.viewerType = 'image';
        openLoadedTab(tab, '');
        return;
      }
      if (ext === 'jar') {
        // jar 包：内建浏览（条目列表 → 点开 class/资源），只读
        tab.readonly = true;
        tab.viewerType = 'jar';
        openLoadedTab(tab, '');
        return;
      }
      if (BINARY_EXTS.has(ext)) {
        // 已知二进制类型：直接 hex 视图，免整文件读入嗅探
        tab.readonly = true;
        tab.viewerType = 'hex';
        openLoadedTab(tab, '');
        return;
      }
      if (ext === 'class') tab.readonly = true; // 字节码视图只读
      const { content, size } = await fetchTextContent(path);
      if (size > MAX_EDIT_SIZE) tab.readonly = true;
      openLoadedTab(tab, content);
    } catch (e) {
      if (e instanceof Error && e.message === 'BINARY') {
        // 扩展名未命中黑名单的二进制：NUL 嗅探后进 hex 视图
        tab.readonly = true;
        tab.viewerType = 'hex';
        openLoadedTab(tab, '');
        return;
      }
      console.error('读取文件内容失败:', e);
      showNotification({ variant: 'error', title: '读取文件内容失败' });
      const already = useEditorStore.getState().tabs.find(t => t.path === path);
      if (!already) useEditorStore.getState().openTab(tab, '');
    }
  })();
  pendingLoads.set(path, p);
  try {
    await p;
  } finally {
    pendingLoads.delete(path);
  }
}

/** 切换活动标签：检查缓存 → 读取内容 → 更新 store */
export async function switchToTab(id: string): Promise<void> {
  await loadContentInto(id);
}

/**
 * 打开 jar 内条目为虚拟只读标签（路径格式 jar://<jar>!/<nested>!/<name>）。
 * 后端已按类型处理：text/class 返回内容进编辑器，binary 由 HexViewer 内存模式渲染
 */
export async function openJarEntry(jarPath: string, nested: string[], entry: { name: string }): Promise<void> {
  const virtualPath = `jar://${jarPath}!/${[...nested, entry.name].join('!/')}`;
  const existing = useEditorStore.getState().tabs.find(t => t.path === virtualPath);
  if (existing) {
    await loadContentInto(existing.id, virtualPath);
    return;
  }
  const tab: FileTab = {
    id: `tab-${Date.now()}-${++tabSeq}`,
    name: entry.name.split('/').pop() ?? entry.name,
    path: virtualPath,
    readonly: true,
  };
  try {
    const res = await editorService.readJarEntry(jarPath, nested, entry.name);
    if (res.kind === 'binary') {
      tab.viewerType = 'hex';
      openLoadedTab(tab, '');
    } else {
      openLoadedTab(tab, res.content);
    }
  } catch (e) {
    console.error('读取 jar 条目失败:', e);
    showNotification({ variant: 'error', title: '读取 jar 条目失败', description: String(e) });
  }
}

/**
 * 打开文件并定位到指定行、高亮命中词（搜索结果点击）
 * 文件可能尚未打开（异步读取完成后定位生效）
 */
export async function locateFile(path: string, name: string, line: number, query: string): Promise<void> {
  await loadAndOpenFile(path, name);
  useEditorStore.getState().setLocate({ path, line, query });
}

/**
 * 保存当前活动标签（Ctrl+S / 保存按钮），成功返回 true
 * 保存后清 dirty 并同步更新缓存
 */
export async function saveActiveFile(): Promise<boolean> {
  const { tabs, activeTabId, fileContent } = useEditorStore.getState();
  if (!activeTabId) return false;
  const tab = tabs.find(t => t.id === activeTabId);
  if (!tab) return false;
  if (tab.readonly) {
    showNotification({ variant: 'warning', title: '文件过大，仅支持查看（超过 10 MB 不能编辑）' });
    return false;
  }
  try {
    await editorService.writeFile(tab.path, fileContent ?? '');
    setCacheContent(tab.path, fileContent ?? '');
    useEditorStore.getState().markClean(activeTabId);
    showNotification({ title: `已保存「${tab.name}」` });
    return true;
  } catch (e) {
    console.error('保存文件失败:', e);
    showNotification({ variant: 'error', title: '保存文件失败', description: String(e) });
    return false;
  }
}

/**
 * 将指定标签的内容加载为当前活动内容
 * 读取期间用户可能已切换到其他标签，此时丢弃结果避免内容错位
 */
async function loadContentInto(id: string, knownPath?: string): Promise<void> {
  const { tabs, setActiveTabId, setFileContent } = useEditorStore.getState();
  const tab = tabs.find(t => t.id === id);
  const path = knownPath ?? tab?.path;
  if (!path || !tab) return;

  setActiveTabId(id);
  // 未保存草稿优先（编辑中切走再切回，恢复草稿而非磁盘版本）
  const draft = drafts.get(tab.id);
  if (draft !== undefined) {
    setFileContent(draft);
    return;
  }
  // 图片/hex/jar 标签内容由查看器组件自行管理
  if (tab.viewerType === 'image' || tab.viewerType === 'hex' || tab.viewerType === 'jar') {
    setFileContent('');
    return;
  }
  const cached = getCachedContent(path);
  if (cached !== undefined) {
    setFileContent(cached);
    return;
  }
  try {
    const { content } = await fetchTextContent(path);
    // 读取期间用户可能已切换到其他标签，此时丢弃结果避免内容错位
    if (useEditorStore.getState().activeTabId !== id) return;
    setCacheContent(path, content);
    setFileContent(content);
  } catch (e) {
    if (useEditorStore.getState().activeTabId !== id) return;
    console.error('读取文件内容失败:', e);
    showNotification({ variant: 'error', title: '读取文件内容失败' });
    setFileContent(null);
  }
}
