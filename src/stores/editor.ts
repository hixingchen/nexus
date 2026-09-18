import { create } from 'zustand';
import { notify } from '../utils/notify';
import { reportError } from '../utils/error';
import { getExtension } from '../utils/path';
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
  /** 已消费的定位请求序号（= revealSeq 表示最近一次定位已被目录树命中） */
  revealConsumedSeq: number;

  /**
   * 每文件打开序号：文件树打开（loadAndOpenFile）时递增。
   * CodeViewer 缓存 key 含此序号——重新打开文件 = 新会话（重建编辑器、撤销历史清空），
   * 标签切换（switchToTab）不递增（保留历史）。修复"输入一个字符 Ctrl+Z 应回到打开时状态"
   */
  fileOpenSeq: Record<string, number>;
  /** 同步操作：打开标签页并设置内容 */
  openTab: (tab: FileTab, content: string, activate?: boolean) => void;
  closeTab: (id: string) => void;
  /** 批量关闭标签（逐个复用 closeTab 逻辑，含活动标签切换与草稿清理） */
  closeTabs: (ids: string[]) => void;
  setActiveTabId: (id: string) => void;
  setFileContent: (content: string | null) => void;
  /** 编辑器内容变更：写入草稿并标记当前标签未保存 */
  updateDraft: (content: string) => void;
  markClean: (id: string, content?: string) => void;
  setLocate: (locate: { path: string; line: number; query: string }) => void;
  clearLocate: () => void;
  /** 清除编辑器中的搜索命中高亮 */
  clearHits: () => void;
  /** 请求在目录树中定位指定文件（EditorTabs 定位图标调用） */
  requestReveal: (path: string) => void;
  /** 目录树实际消费定位请求（命中节点滚动/选中后置位；用于未命中时的提示判定） */
  markRevealConsumed: () => void;
  /** 清除定位标记（用户主动切换文件/选中树节点时调用，避免定位高亮残留） */
  clearReveal: () => void;
}

/**
 * 文件内容缓存，避免切换标签时重复读取（LRU，最多 50 个文件 / 64MB）。
 * 字节上限：后端单文件读取上限 50MB，若仅按文件数限制最坏可驻留 ~2.5GB。
 */
const MAX_CACHE_SIZE = 50;
const MAX_CACHE_BYTES = 64 * 1024 * 1024;
/**
 * 单条内容入缓存的上限。超过则不入缓存（切回时重新读盘）：
 * 原实现的淘汰是"腾空到装得下"，一条 40MB 文件会瞬间清空整个缓存，
 * 之后所有标签切换都变成缓存未命中（并触发重新读盘 + 内容晚到）。
 */
const MAX_CACHE_ENTRY_BYTES = 8 * 1024 * 1024;
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


/**
 * 换行符规范化：\r\n / \r / \n 统一为 \n。
 * CodeMirror 编辑时会规范化文档换行（实测：CRLF 文件输入一个字符后整个文档变 \n），
 * 撤销回原文件后 doc.toString() 与基线/打开时内容的换行符可能不同——
 * 比较前统一，否则 Windows CRLF 文件撤销回最初版本永远算"未保存"
 */
function normalizeEOL(s: string): string {
  return s.replace(/\r\n?/g, '\n');
}

/** jar:// 虚拟路径 → (jar 路径, 嵌套链, 条目名) */
export function parseJarVirtualPath(path: string): { jarPath: string; nested: string[]; name: string } {
  const parts = path.slice('jar://'.length).split('!/');
  const jarPath = parts[0];
  const name = parts[parts.length - 1];
  const nested = parts.slice(1, -1);
  return { jarPath, nested, name };
}

/** 每文件的原始字节特征（换行风格 + 编码 + 读取时 mtime）：保存时按此写回，保证字节保真；
 *  mtime 用于保存前检测"文件已被外部改动"，避免静默覆盖 IDE/git 改过的版本 */
interface FileMeta {
  line_ending: 'lf' | 'crlf' | 'cr';
  /** lossy = 既非合法 UTF-8 也非合法 GB18030（解码出了替换字符），按原编码写回会损坏文件 */
  encoding: 'utf8' | 'gb18030' | 'lossy';
  /** 读取/上次写入后的修改时间（毫秒）；null = 未知（新建或读不到） */
  modified: number | null;
}
const fileMetaCache = new Map<string, FileMeta>();

/** 读取文本内容：jar 内条目走 jar 读取，.class 走 CFR/字节码视图，其余走 read_file（二进制抛 BINARY） */
async function fetchTextContent(path: string): Promise<{ content: string; size: number }> {
  if (path.startsWith('jar://')) {
    const { jarPath, nested, name } = parseJarVirtualPath(path);
    const res = await editorService.readJarEntry(jarPath, nested, name);
    return { content: res.content, size: res.size };
  }
  if (getExtension(path, { lower: true }) === 'class') {
    const content = await editorService.readClassFile(path);
    return { content, size: 0 };
  }
  const res = await editorService.readFile(path);
  if (res.is_binary) throw new Error('BINARY');
  // jar 内条目/class 无磁盘 mtime 概念：清掉旧值，避免把上一个同名文件的 mtime 带过来
  fileMetaCache.set(path, { line_ending: res.line_ending, encoding: res.encoding, modified: res.modified });
  return { content: res.content, size: res.size };
}

/** 按文件原始换行风格还原（CodeMirror 编辑会规范化换行，保存时转回原风格防 git 误报变更） */
function toOriginalEnding(text: string, lineEnding: 'lf' | 'crlf' | 'cr'): string {
  if (lineEnding === 'crlf') return text.replace(/\n/g, '\r\n');
  if (lineEnding === 'cr') return text.replace(/\n/g, '\r');
  return text;
}

/**
 * 打开新标签（读取期间可能已被其他调用打开：已有标签则切换过去，恢复其未保存草稿）
 *
 * `openSeq` 为发起读取时的请求序号：晚到的结果（用户已去点别的文件）只入标签不抢焦点。
 */
function openLoadedTab(tab: FileTab, content: string, openSeq?: number) {
  const normalized = normalizeEOL(content);
  const isLatestRequest = openSeq === undefined || openSeq === latestOpenSeq;
  const already = useEditorStore.getState().tabs.find(t => t.path === tab.path);
  if (already) {
    setCacheContent(tab.path, normalized);
    const draft = drafts.get(already.id);
    if (isLatestRequest) useEditorStore.getState().setFileContent(draft ?? normalized);
    if (isLatestRequest) useEditorStore.getState().setActiveTabId(already.id);
  } else {
    // 晚到的结果不激活：内容入列表与缓存即可，活动标签保持用户当下选的那个
    useEditorStore.getState().openTab(tab, normalized, isLatestRequest);
  }
}

/**
 * 每标签：最后保存（或打开）的基线内容，是"未保存"判定的**唯一**依据。
 *
 * 历史教训：这里曾并行维护四张 Map（baselines / openedContents / sessionStarts / drafts），
 * dirty 判定要求与其中任意一张相等即为"干净"。而 sessionStarts 记录的是"编辑器会话起点"，
 * 其内容可能是未保存草稿（切回标签重建编辑器时传入的就是当前缓冲区）——于是"撤销回该锚点"
 * 会把草稿删掉并清掉未保存标记，用户改动无声消失；openedContents 同理会造成
 * "保存后改回打开时内容 → 显示已保存而磁盘是另一版本"。现统一为只与基线比较：
 * 任何"看起来干净但其实没保存"的情况都不再可能，代价只是保守地把这类状态显示为未保存。
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
  // 超大文件不入缓存：否则"腾空到装得下"会把其它标签的缓存全部挤掉（见 MAX_CACHE_ENTRY_BYTES 注释）
  if (bytes > MAX_CACHE_ENTRY_BYTES) return;
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
  fileOpenSeq: {},
  locate: null,
  hitSeq: 0,
  revealPath: null,
  revealSeq: 0,
  revealConsumedSeq: 0,

  openTab: (tab, content, activate = true) => {
    const { tabs } = get();
    // 全链路统一存归一化内容（\n）：CodeMirror 建 doc 时也做同样归一，缓存/基线/展示三者一致
    // 才能命中 stateCache（否则 CRLF 文件每次切回都重建、撤销历史与光标丢失）；
    // 写盘时由 saveActiveFile 按 read_file 返回的原换行风格还原
    const normalized = normalizeEOL(content);
    setCacheContent(tab.path, normalized);
    baselines.set(tab.id, normalized);
    // activate=false：只入列表（晚到的读盘结果不抢当前标签），此时不得改 fileContent
    set(activate
      ? { tabs: [...tabs, tab], activeTabId: tab.id, fileContent: normalized }
      : { tabs: [...tabs, tab] });
  },

  closeTab: (id) => {
    // 先结算未合帧的编辑：下面会删掉该标签的草稿，结算晚了会留下孤儿草稿
    settlePendingEdit();
    const { tabs, activeTabId, dirtyIds } = get();
    const closedTab = tabs.find(t => t.id === id);
    const newTabs = tabs.filter(t => t.id !== id);
    const newDirty = dirtyIds.filter(d => d !== id);
    baselines.delete(id);
    drafts.delete(id);

    if (closedTab && !newTabs.some(t => t.path === closedTab.path)) {
      removeCacheForPath(closedTab.path);
      fileMetaCache.delete(closedTab.path);
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
    const { activeTabId } = get();
    if (!activeTabId) return;
    applyDraft(activeTabId, content, set, get);
  },

  markClean: (id, content?: string) => {
    const { dirtyIds, fileContent, tabs } = get();
    // 标签可能在保存的 await 期间被关闭：此时不再写基线，避免留下孤儿状态
    if (!tabs.some(t => t.id === id)) return;
    // 保存成功后：实际写入磁盘的内容成为新基线，草稿作废。
    // content 由调用方传写盘快照——避免 await 期间用户又编辑导致
    // fileContent 已是新内容、baseline 错位（撤销到保存点时 dirty 无法清除）
    baselines.set(id, normalizeEOL(content ?? fileContent ?? ''));
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

  markRevealConsumed: () => {
    set({ revealConsumedSeq: get().revealSeq });
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
 * 最近一次"打开文件"请求的序号。
 *
 * 为什么需要：读盘是异步的（`.class` 还要走 CFR 反编译，上限 15s），而打开完成的动作里包含
 * `setActiveTabId`。用户点了慢文件 A 又去点/切到 B 时，A 的读盘结果晚到会把活动标签**抢回 A**，
 * 表现为"我明明点的是 B，界面跳到 A"。同仓其他异步加载点（项目列表展开、文件树目录、
 * 搜索结果）都已有"最新请求才生效"的守卫，这里补齐。
 * 标签本身仍会入列表（内容不丢），只是不再抢焦点。
 */
let latestOpenSeq = 0;

/**
 * 每标签的编辑代数：每次用户编辑 +1。
 *
 * 用于"读盘结果晚到"的判定（见 loadContentInto）：请求发出前记下代数，结果回来时若已变，
 * 说明用户在这段窗口里已经改过内容，此时**必须丢弃磁盘内容**，否则用户刚敲的字被覆盖
 * （更糟的是保存路径取 fileContent，会把覆盖后的内容写回磁盘）。
 */
const editSeqByTab = new Map<string, number>();

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
  // 文件树打开 = 新会话：递增打开序号（CodeViewer 缓存 key 含此序号 → 重建编辑器、
  // 撤销历史清空——输入后 Ctrl+Z 应回到打开时的状态）。标签切换不经过此函数，历史保留
  {
    const st = useEditorStore.getState();
    const seq = (st.fileOpenSeq[path] ?? 0) + 1;
    useEditorStore.setState({ fileOpenSeq: { ...st.fileOpenSeq, [path]: seq } });
  }
  // 本次打开请求的序号：读盘期间用户又点了别的文件时，本请求的结果不再抢活动标签
  const mySeq = ++latestOpenSeq;
  // 检查是否已打开
  const existing = useEditorStore.getState().tabs.find(t => t.path === path);
  if (existing) {
    await loadContentInto(existing.id, path);
    return;
  }

  // 并发去重：同一 path 正在加载时复用，读取完成后切换过去（仍以"最新请求"为准）
  const pending = pendingLoads.get(path);
  if (pending) {
    await pending;
    const tab = useEditorStore.getState().tabs.find(t => t.path === path);
    if (tab && mySeq === latestOpenSeq) useEditorStore.getState().setActiveTabId(tab.id);
    return;
  }

  const tab: FileTab = { id: `tab-${Date.now()}-${++tabSeq}`, name, path };
  const p = (async () => {
    try {
      const ext = getExtension(path, { lower: true });
      if (IMAGE_EXTS.has(ext)) {
        // 图片：内建预览（ImageViewer 自行加载，内容不进 store）
        tab.readonly = true;
        tab.viewerType = 'image';
        openLoadedTab(tab, '', mySeq);
        return;
      }
      if (ext === 'jar') {
        // jar 包：内建浏览（条目列表 → 点开 class/资源），只读
        tab.readonly = true;
        tab.viewerType = 'jar';
        openLoadedTab(tab, '', mySeq);
        return;
      }
      if (BINARY_EXTS.has(ext)) {
        // 已知二进制类型：直接 hex 视图，免整文件读入嗅探
        tab.readonly = true;
        tab.viewerType = 'hex';
        openLoadedTab(tab, '', mySeq);
        return;
      }
      if (ext === 'class') tab.readonly = true; // 字节码视图只读
      const { content, size } = await fetchTextContent(path);
      if (size > MAX_EDIT_SIZE) tab.readonly = true;
      // 编码有损（既非 UTF-8 也非 GB18030）：允许查看与复制，但不允许落盘——
      // 按原编码写回会把解码时的替换字符固化进文件，原字节永久消失（后端同样会拒绝）。
      // 在这里就置只读，避免用户编辑半天后在保存时才发现
      if (fileMetaCache.get(path)?.encoding === 'lossy') {
        tab.readonly = true;
        tab.readonlyReason = '该文件不是合法的 UTF-8 / GB18030 编码，保存会损坏原文件，仅支持查看';
      }
      openLoadedTab(tab, content, mySeq);
    } catch (e) {
      if (e instanceof Error && e.message === 'BINARY') {
        // 扩展名未命中黑名单的二进制：NUL 嗅探后进 hex 视图
        tab.readonly = true;
        tab.viewerType = 'hex';
        openLoadedTab(tab, '', mySeq);
        return;
      }
      reportError('读取文件内容失败', e);
      const already = useEditorStore.getState().tabs.find(t => t.path === path);
      if (!already) {
        // 必须只读：空内容 + 可编辑 ⇒ 用户按 Ctrl+S 会把空内容写回磁盘，原文件被截断
        // （后端对 >50MB 文件直接拒绝读取，打开大文件必然走这条 catch）
        tab.readonly = true;
        useEditorStore.getState().openTab(tab, '', mySeq === latestOpenSeq);
      }
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
  const mySeq = ++latestOpenSeq;
  try {
    const res = await editorService.readJarEntry(jarPath, nested, entry.name);
    if (res.kind === 'binary') {
      tab.viewerType = 'hex';
      openLoadedTab(tab, '', mySeq);
    } else {
      openLoadedTab(tab, res.content, mySeq);
    }
  } catch (e) {
    reportError('读取 jar 条目失败', e);
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
 *
 * 保存期间禁止重入：连按两次 Ctrl+S 会对同一路径发起两个并发写入，
 * 后到的 rename 失败并把"保存文件失败"报给用户，而文件其实已写成功、
 * dirty 标记却因为走了错误分支而清不掉（用户看到"保存失败 + 未保存"的双重误导）。
 */
let savingInFlight = false;

/**
 * 保存指定标签（任意标签，不限于活动标签），成功返回 true。
 *
 * 内容来源分两种：活动标签取 `fileContent`（最新），非活动标签取 `drafts`（切走时留下的草稿）。
 * 关窗时的"保存全部"依赖这条路径——因此不能再假设"要保存的只有活动标签"。
 */
async function saveTabCore(id: string, withToast: boolean): Promise<boolean> {
  // **必须在读内容之前**：大文档的编辑可能还没写回 store（PERF-13 合帧），
  // 直接读 fileContent/drafts 会拿到最多 200ms 前的内容并把它写进磁盘
  settlePendingEdit();
  const { tabs, activeTabId, fileContent } = useEditorStore.getState();
  const tab = tabs.find(t => t.id === id);
  if (!tab) return true; // 标签已被关闭：无需保存
  if (tab.readonly) {
    if (withToast) {
      notify({
        variant: 'warning',
        title: tab.readonlyReason ?? '文件过大，仅支持查看（超过 10 MB 不能编辑）',
      });
    }
    return false;
  }
  const content = id === activeTabId ? fileContent : (drafts.get(id) ?? null);
  // 内容从未成功加载（读取失败/仍在加载中）：写盘会把磁盘上的原文件覆盖成空内容
  if (content === null) {
    if (withToast) notify({ variant: 'error', title: '文件内容未加载，已取消保存' });
    return false;
  }
  if (savingInFlight) return false;
  savingInFlight = true;
  try {
    // 写盘前捕获快照：markClean 的基线必须等于实际写入磁盘的内容，
    // 不能用 await 后的内容（保存期间用户可能已继续编辑）
    const contentToSave = content;
    // 按原文件换行风格/编码写回：CodeMirror 规范化换行 + UTF-8 写回会让
    // "编辑→撤销→保存"后的字节与原始文件不同（git 误报变更）
    const meta = fileMetaCache.get(tab.path);
    const bytesToWrite = meta ? toOriginalEnding(contentToSave, meta.line_ending) : contentToSave;
    // 回传打开时的 mtime：磁盘已被外部改动时后端拒绝写入，不再静默覆盖（P0-1）
    const newModified = await editorService.writeFile(tab.path, bytesToWrite, meta?.encoding, meta?.modified ?? null);
    // 保存期间标签可能已被关闭：此时不再写缓存与基线（markClean 内部也会校验）
    if (useEditorStore.getState().tabs.some(t => t.id === id)) {
      // 缓存与基线存编辑器内容（不转换），切换标签/撤销比较都在编辑器内容维度
      setCacheContent(tab.path, contentToSave);
      // 刷新 mtime 基线：否则下一次保存会拿打开时的旧值比对，被自己的上次写入判成冲突
      if (meta) meta.modified = newModified;
      useEditorStore.getState().markClean(id, contentToSave);
    }
    if (withToast) notify({ title: `已保存「${tab.name}」` });
    return true;
  } catch (e) {
    // 冲突单独提示：这是"文件被别的工具改过"，用户需要重新加载而不是反复重试
    const msg = String(e);
    if (msg.includes('已被外部修改')) {
      // duration 显式给 8s：冲突提示比普通警告更需要被读完，而 warning 的按级默认是 6s
      reportError('保存文件失败', e, {
        variant: 'warning',
        title: `「${tab.name}」已被其他程序修改，未保存`,
        description: '磁盘上的版本比编辑器里的新。点「重新加载」载入磁盘版本；如需保留当前修改，请先复制',
        duration: 8000,
        // 现场给一步操作：提示里让用户"自己去关标签再打开"是本轮修掉的缺口
        action: { label: '重新加载', run: () => { void reloadTab(id); } },
      });
    } else {
      reportError('保存文件失败', e);
    }
    return false;
  } finally {
    savingInFlight = false;
  }
}

/**
 * 未合帧编辑的**端口**（PERF-13）。
 *
 * 背景：大文档每次按键都 `doc.toString()` 会把整篇文档物化一遍（实测 10MB → 4.5ms/键，
 * 并按 100MB/s 产生垃圾），因此 `CodeViewer` 对超过 `EDIT_COALESCE_MIN_LENGTH` 的文档
 * 改为「攒 200ms 再写回 store」。问题是：**读内容的路径一旦读到旧值就会存旧内容**
 * （最坏是丢用户的编辑）。
 *
 * 对策：把"未写回的编辑"登记在这里，并把**所有读内容的路径**收敛到一个入口
 * （`settlePendingEdit`）——保存、切标签、关标签、关窗守卫都先经它取最新值。
 * 这不是"记得在每个地方 flush"：登记的是 `(tabId, 取文本)` 一对，物化后按 **tabId**
 * 落库（不是当时的 activeTabId），因此即便 flush 发生在切换标签之后也不会记到别的标签头上。
 */
let pendingEdit: { tabId: string; take: () => string | null } | null = null;

/** 由编辑器登记"未写回的编辑"（同一时刻只有一个活动编辑器，故用单槽） */
export function setPendingEditSource(tabId: string, take: () => string | null): void {
  pendingEdit = { tabId, take };
}

/** 撤销登记（编辑器销毁且已结算后调用） */
export function clearPendingEditSource(): void {
  pendingEdit = null;
}

/**
 * 物化并落库未合帧的编辑（无待处理内容时是 no-op）。**所有读取内容的路径都必须先经这里。**
 *
 * 先取出再物化：`take()` 可能触发 store 写入（进而重入本函数），清空槽位可避免重复落库。
 */
export function settlePendingEdit(): void {
  const p = pendingEdit;
  if (!p) return;
  pendingEdit = null;
  const text = p.take();
  if (text === null) return;
  applyDraft(p.tabId, text, useEditorStore.setState, useEditorStore.getState);
}

/**
 * 未保存草稿数（关窗守卫判定用）。**统计前先结算未合帧的编辑**：
 * 大文档的首次改动可能还在合帧窗口里（`dirtyIds` 尚未更新），直接读计数会漏掉它——
 * 用户以为"没有未保存内容"直接关窗，那次编辑就跟着进程一起消失。
 */
export function unsavedDraftCount(): number {
  settlePendingEdit();
  return useEditorStore.getState().dirtyIds.length;
}

/**
 * 把一份草稿内容落到指定标签（`updateDraft` 与 `settlePendingEdit` 的共同实现）。
 *
 * 为什么按 tabId 而不是 activeTabId：合帧路径落库时用户可能已经切到别的标签，
 * 按 activeTabId 写会把 A 的编辑记到 B 头上。
 */
function applyDraft(
  tabId: string,
  content: string,
  set: (partial: Partial<EditorStore>) => void,
  get: () => EditorStore,
): void {
  const { activeTabId, dirtyIds } = get();
  // fileContent 只表示"活动标签的当前内容"：非活动标签的编辑只进草稿
  if (tabId === activeTabId) set({ fileContent: content });
  editSeqByTab.set(tabId, (editSeqByTab.get(tabId) ?? 0) + 1);
  // dirty 判定只与"最后保存/打开的基线"比较（单一真相，见 baselines 注释）。
  //
  // 成本控制（每次按键都会走到这里）：
  // ① 仅在内容确实含 \r 时才做全文 EOL 归一化——LF 文件（绝大多数）省掉一次全文正则；
  // ② 先比长度：长度不同必然有改动，直接跳过与基线的全文比较。
  const baseline = baselines.get(tabId);
  const normalized = content.includes('\r') ? normalizeEOL(content) : content;
  // 无基线视为未保存；有基线时先比长度（不同必然有改动，免去全文比较），长度相同才做全串比较
  const isDirty = baseline === undefined
    || normalized.length !== baseline.length
    || normalized !== baseline;
  if (!isDirty) {
    drafts.delete(tabId);
    if (dirtyIds.includes(tabId)) {
      set({ dirtyIds: dirtyIds.filter(d => d !== tabId) });
    }
  } else {
    drafts.set(tabId, content);
    if (!dirtyIds.includes(tabId)) {
      set({ dirtyIds: [...dirtyIds, tabId] });
    }
  }
}

export async function saveActiveFile(): Promise<boolean> {
  const { activeTabId, dirtyIds } = useEditorStore.getState();
  if (!activeTabId) return false;
  // 没有未保存改动 → 什么都不做（甲-①）。两个理由：
  // ① 内容与磁盘一致，写一次纯属浪费；
  // ② 更关键的是**不该弹"已被其他程序修改"**——用户压根没改过，报冲突只会让人莫名其妙。
  //    这正是"打开文件 → 别的工具改了它 → 按 Ctrl+S"出现的那个困惑。
  // 不会因此丢掉"保存新文件"的能力：全仓没有"新建文件"入口（文件都是打开或粘贴来的），
  // 所以"干净"必然意味着磁盘上已有同样的内容。
  if (!dirtyIds.includes(activeTabId)) return true;
  return saveTabCore(activeTabId, true);
}

/**
 * 从**磁盘**重新加载指定标签（用户显式动作：标签右键菜单 / 保存冲突提示里的按钮）。
 *
 * 为什么必须有它：保存冲突时后端拒绝写入，而编辑器**不会**自动重载（自动重载在"本地有
 * 未保存改动"时就是静默丢改动）。此前用户只能"关标签再打开"来手工模拟这一步——
 * 冲突提示里那句"请关闭标签后重新打开"就是这个缺口的自白。
 *
 * 它会丢掉三样东西，所以**只能是显式动作**、不做自动重载：
 * - 该标签的未保存草稿（先结算未合帧的编辑再删草稿，否则它们会被当成"要保留的内容"）
 * - 撤销历史（递增 `fileOpenSeq` → CodeViewer 按新会话重建，与"文件树里重新打开"同语义）
 * - 内容缓存（不清则 `loadContentInto` 命中旧缓存，重载出来还是旧内容）
 */
export async function reloadTab(id: string): Promise<boolean> {
  const tab = useEditorStore.getState().tabs.find(t => t.id === id);
  if (!tab) return false;
  if (tab.readonly || tab.viewerType) {
    // 图片 / hex / jar 的内容由各自查看器管理，而且它们都不可保存——没有"保存冲突"可言
    notify({ variant: 'info', title: '该标签不支持从磁盘重新加载' });
    return false;
  }
  settlePendingEdit();
  drafts.delete(id);

  // ── 顺序是关键：**先把内容读回来，再一次性写进 store** ──────────────
  // 不要复用 `loadContentInto`：它读盘前就 `setFileContent(null)`，于是编辑器会先按
  // 空内容（或旧内容）重建一次，等新内容落地时只能走「外部内容同步」的文档替换——
  // 而那是**一次可撤销的事务**，结果就是重载完按 Ctrl+Z 能撤回重载前的内容、标签重新变脏。
  // 读盘期间一个字都不碰 store，重建就只发生一次且拿到的就是新内容：
  // 编辑器不 dispatch、不进撤销历史，重载 = 干净的新会话（与"文件树里重新打开"同语义）。
  const editsBefore = editSeqByTab.get(id) ?? 0;
  let raw: string;
  try {
    ({ content: raw } = await fetchTextContent(tab.path));
  } catch (e) {
    reportError('重新加载失败', e);
    return false;
  }
  // 读盘期间用户敲了字：重载会覆盖掉它们，放弃并如实告知（不静默丢输入）
  if ((editSeqByTab.get(id) ?? 0) !== editsBefore) {
    notify({
      variant: 'warning',
      title: '重新加载已取消',
      description: '读取期间该文件有新的输入，重载会丢掉它们。请先复制或保存后再试',
    });
    return false;
  }

  commitReloadedContent(id, raw);
  return true;
}

/**
 * 把刚读到的磁盘内容提交到标签（`reloadTab` 的落地部分）。
 *
 * 抽出来是为了**可测**：这三步构成"重载后必须成立"的不变量——干净（基线 = 磁盘内容）、
 * 内容槽已更新、会话序号已递增（编辑器据此重建并清撤销历史）。用户实测抓到过一次
 * 这里的破洞：重载后标签仍显示未保存圆点，按 Ctrl+Z 还能撤回重载。
 */
export function commitReloadedContent(id: string, raw: string): void {
  const tab = useEditorStore.getState().tabs.find(t => t.id === id);
  if (!tab) return;
  const normalized = normalizeEOL(raw);
  drafts.delete(id);
  setCacheContent(tab.path, normalized); // 缓存不同步的话，切走再切回会读到旧内容
  // 磁盘内容即新基线 → 立刻是"干净"（不重置的话标签会显示未保存）
  useEditorStore.getState().markClean(id, normalized);
  useEditorStore.setState(s => ({
    // 只有活动标签才更新内容槽；非活动标签只换会话序号，等切到它时按新内容重建
    ...(s.activeTabId === id ? { fileContent: normalized } : {}),
    fileOpenSeq: { ...s.fileOpenSeq, [tab.path]: (s.fileOpenSeq[tab.path] ?? 0) + 1 },
  }));
}

/**
 * 保存全部未保存标签（关窗确认的「保存全部并关闭」用它）。
 *
 * 串行执行：后端写盘是 atomic rename，但同一目录下的并发写在部分文件系统上会互相干扰；
 * 且失败时希望"先写成功的算成功"，串行最容易给出准确的成功/失败计数。
 * 返回仍然失败的标签数（0 = 全部保存成功，可以安全关闭）。
 */
export async function saveAllDirtyTabs(): Promise<{ saved: number; failed: number }> {
  const ids = [...useEditorStore.getState().dirtyIds];
  let saved = 0;
  let failed = 0;
  for (const id of ids) {
    const ok = await saveTabCore(id, false);
    if (ok) saved++;
    else failed++;
  }
  return { saved, failed };
}

/**
 * 将指定标签的内容加载为当前活动内容
 * 读取期间用户可能已切换到其他标签，此时丢弃结果避免内容错位
 */
async function loadContentInto(id: string, knownPath?: string): Promise<void> {
  // 先把上一个文件未合帧的编辑结算掉：它按 tabId 落库，所以即便此刻切换也不串标签
  settlePendingEdit();
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
    // 先清空活动内容：编辑器只在 filePath/openSeq 变化时消费 content，若沿用上一个文件的
    // 内容建文档，读盘期间它会显示成新文件的内容（并可被 Ctrl+S 写回错误文件）
    setFileContent(null);
    // 读盘窗口内的编辑代数：await 回来后若已变，说明用户已经敲过字，磁盘内容必须丢掉
    const editsBefore = editSeqByTab.get(id) ?? 0;
    const { content } = await fetchTextContent(path);
    // 读取期间用户可能已切换到其他标签，此时丢弃结果避免内容错位
    if (useEditorStore.getState().activeTabId !== id) return;
    // 读取期间用户已开始编辑（例如切回来立刻打字，而读盘还在排队）：
    // 覆盖会让刚敲的内容无声消失，并且保存路径正是取 fileContent → 会把覆盖后的内容写回磁盘
    if ((editSeqByTab.get(id) ?? 0) !== editsBefore) return;
    const normalized = normalizeEOL(content);
    setCacheContent(path, normalized);
    setFileContent(normalized);
  } catch (e) {
    if (useEditorStore.getState().activeTabId !== id) return;
    reportError('读取文件内容失败', e);
    setFileContent(null);
  }
}
