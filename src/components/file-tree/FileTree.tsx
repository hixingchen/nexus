import { useState, useEffect, useCallback, useMemo, useRef, memo } from 'react';
import { showNotification } from '../ui/Toast';
import { useEditorStore, loadAndOpenFile, parseJarVirtualPath } from '../../stores/editor';
import { useSearchModalStore } from '../../stores/searchModal';
import { useFileTreeStore } from '../../stores/fileTreeStore';
import { listJar, type JarEntryInfo } from '../../services/editor';
import { listDirectory, pasteFiles, copyFilesToClipboard, openInExplorer } from '../../services/system';
import { reportError } from '../../utils/error';
import { FolderClosed, FolderOpen, getIconSvg } from './FileIcons';
import { Chevron } from '../ui/Chevron';
import { SvgIcon } from '../ui/SvgIcon';
import { ContextMenu, ContextMenuItem } from '../ui/ContextMenu';
import { getDirColorClass } from '../../utils/fileColors';
import { getExtension } from '../../utils/path';
import { copyText } from '../../utils/clipboard';
import { pickRevealTarget, REVEAL_PRIORITY, type RevealTarget } from '../../utils/fileTree';
import type { FileEntry } from '../../types/file';

/**
 * 当前**挂载中**的树（项目目录树 + 各已展开的服务树）。定位时用它挑"最具体的那棵"。
 *
 * 为什么用模块级数组而不是 store：只有 FileTree 自己需要它，且读它的时机固定
 * （`revealSeq` 变化触发的那次渲染）——不需要订阅语义，多一层 store 只是多一层间接。
 * 数量很小（同时在展开状态的树通常 1~3 棵）。
 */
const mountedTrees: RevealTarget[] = [];

/**
 * 粘贴系统剪贴板文件到目标目录，随后刷新。
 *
 * 抽出来是因为两处入口（目录右键 / 空白区右键到项目根）此前各写一遍，
 * 且都把"部分成功"报成"已粘贴 N 个项目"——后端现在会回传失败清单，这里统一处理。
 */
async function pasteInto(targetDir: string, refresh: () => void) {
  try {
    const res = await pasteFiles(targetDir);
    if (res.failed.length > 0) {
      showNotification({
        variant: 'error',
        title: res.created.length > 0
          ? `已粘贴 ${res.created.length} 个，${res.failed.length} 个失败`
          : `粘贴失败（${res.failed.length} 个）`,
        description: res.failed.join('; '),
      });
    } else {
      showNotification({ variant: 'success', title: `已粘贴 ${res.created.length} 个项目` });
    }
    refresh();
  } catch (err) {
    reportError('粘贴失败', err);
  }
}
const INDENT_STEP = 14;
const BASE_PADDING = 18;
/** 目录展开时初始渲染条数，超出后显示"加载更多" */
const INITIAL_RENDER_LIMIT = 200;

/* ---- jar 虚拟节点（树内展开 jar 条目） ---- */

/**
 * jar 内目录节点的预构建子树缓存：合成目录路径（以 / 结尾）→ 子节点列表。
 *
 * **必须有上限**：这是模块级 Map，跨组件卸载活到进程结束。一个 3 万条目的 fat jar
 * 会产生数千个目录键，反复浏览不同的 jar 时只增不减（真实内存泄漏）。
 * 按插入顺序淘汰最旧的键（Map 保持插入顺序），上限远大于"同时展开的目录数"。
 * 淘汰后若用户再展开该目录，展开逻辑会重新列出承载它的 jar 并重建缓存（见 Entry 展开分支）。
 */
const JAR_TREE_CACHE_MAX = 512;
const jarTreeCache = new Map<string, FileEntry[]>();

function jarTreeCacheSet(key: string, value: FileEntry[]) {
  // 重新写入的键要移到队尾，否则"刚用过"的目录会被优先淘汰
  jarTreeCache.delete(key);
  jarTreeCache.set(key, value);
  while (jarTreeCache.size > JAR_TREE_CACHE_MAX) {
    const oldest = jarTreeCache.keys().next();
    if (oldest.done) break;
    jarTreeCache.delete(oldest.value);
  }
}

/** 路径段树节点 */
interface JarTreeNode {
  entry?: JarEntryInfo;
  children?: Map<string, JarTreeNode>;
}

/** jar 条目扁平列表 → 目录树 FileEntry（目录在前、名称排序；目录子树写入缓存供懒展开） */
function buildJarTree(realPath: string, nested: string[], entries: JarEntryInfo[]): FileEntry[] {
  const root = new Map<string, JarTreeNode>();
  for (const e of entries) {
    const segs = e.name.split('/');
    let node = root;
    for (let i = 0; i < segs.length - 1; i++) {
      let child = node.get(segs[i]);
      if (!child || !child.children) {
        child = { children: new Map() };
        node.set(segs[i], child);
      }
      node = child.children!;
    }
    node.set(segs[segs.length - 1], { entry: e });
  }

  const base = `jar://${realPath}!/${nested.map(n => `${n}!/`).join('')}`;
  const walk = (map: Map<string, JarTreeNode>, dirPath: string): FileEntry[] => {
    const dirs: FileEntry[] = [];
    const files: FileEntry[] = [];
    for (const [name, node] of map) {
      const childPath = `${dirPath}${name}`;
      if (node.children) {
        const dirPathWithSlash = `${childPath}/`;
        jarTreeCacheSet(dirPathWithSlash, walk(node.children, dirPathWithSlash));
        dirs.push({ name, path: dirPathWithSlash, is_dir: true, size: 0, extension: null });
      } else {
        const e = node.entry!;
        files.push({
          name,
          path: childPath,
          is_dir: false,
          size: e.size,
          // 无点名的 extension 是 null（不是 ''）：FileEntry 契约如此，保持原样
          extension: name.includes('.') ? getExtension(name, { lower: true }) : null,
        });
      }
    }
    dirs.sort((a, b) => a.name.toLowerCase().localeCompare(b.name.toLowerCase()));
    files.sort((a, b) => a.name.toLowerCase().localeCompare(b.name.toLowerCase()));
    return [...dirs, ...files];
  };
  return walk(root, base);
}

/* ---- Entry ---- */
interface EntryProps {
  e: FileEntry;
  /** 预计算的左侧缩进像素值 */
  indentPx: number;
  /** 定位目标路径（标签右键「在目录树中定位」触发）：目录自动展开链，文件滚动高亮 */
  revealPath: string | null;
  /** 定位触发信号：每次请求 +1（同一路径重复点击也生效） */
  revealSeq: number;
  onSelect: (path: string) => void;
  /** 子级缩进像素值（indentPx + INDENT_STEP） */
  childIndentPx: number;
}

const Entry = memo(function Entry({ e, indentPx, revealPath, revealSeq, onSelect, childIndentPx }: EntryProps) {
  const [open, setOpen] = useState(false);
  const [kids, setKids] = useState<FileEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [hover, setHover] = useState(false);
  const [showAll, setShowAll] = useState(false);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);
  /** 展开请求序号：加载中折叠后丢弃过期响应，避免目录被意外重新展开 */
  const toggleSeqRef = useRef(0);
  const entryRef = useRef<HTMLDivElement | null>(null);
  // 只订阅"我这个节点是否被选中"这一个布尔（PERF-18）：订阅整个 selectedPath 会让
  // 每次选中都重渲染整棵已展开子树（见 stores/fileTreeStore.ts 的说明）
  const selected = useFileTreeStore(s => s.selectedPath === e.path);
  /** 定位目标文件（滚动 + 高亮） */
  const reveal = revealPath === e.path;

  /** 可展开节点：磁盘目录 / 磁盘 jar / jar 内目录（jar://…/）/ jar 内嵌套 jar */
  const isJarPath = e.path.startsWith('jar://');
  const isJarFile = getExtension(e.name, { lower: true }) === 'jar';
  const expandable = e.is_dir || isJarFile;

  /** 重新加载当前磁盘目录内容（展开与粘贴后刷新共用）。序号复用：粘贴刷新不会与过期展开响应竞争 */
  const reloadDir = useCallback(async () => {
    const seq = ++toggleSeqRef.current;
    setLoading(true);
    try {
      const list = await listDirectory(e.path);
      if (seq !== toggleSeqRef.current) return;
      setKids(list);
      setShowAll(false);
      setOpen(true);
    } catch (err) {
      if (seq !== toggleSeqRef.current) return;
      reportError('刷新目录失败', err);
    } finally {
      if (seq === toggleSeqRef.current) setLoading(false);
    }
  }, [e.path]);

  const toggle = useCallback(async () => {
    if (!expandable) return;
    if (open) { setOpen(false); return; }
    const seq = ++toggleSeqRef.current;
    setLoading(true);
    try {
      if (isJarPath) {
        // jar 内目录：展开构建时预缓存的子树
        let cached = jarTreeCache.get(e.path);
        if (!cached) {
          // 缓存未命中（被 LRU 淘汰，或首次进入）：重新列出**承载它的那一层**并重建虚拟树，
          // 重建过程会把整棵子树的目录都回填进缓存。不能落到下面的"按嵌套 jar 处理"分支——
          // 那会把普通目录当成嵌套 jar 去打开，必然失败。
          const { jarPath, nested } = parseJarVirtualPath(e.path);
          const list = await listJar(jarPath, nested);
          if (seq !== toggleSeqRef.current) return;
          buildJarTree(jarPath, nested, list);
          cached = jarTreeCache.get(e.path);
        }
        if (cached) {
          if (seq !== toggleSeqRef.current) return;
          setKids(cached);
          setOpen(true);
          return;
        }
        // jar 内嵌套 jar：列出下一层条目
        const { jarPath, nested, name } = parseJarVirtualPath(e.path);
        const chain = [...nested, name];
        const list = await listJar(jarPath, chain);
        if (seq !== toggleSeqRef.current) return;
        setKids(buildJarTree(jarPath, chain, list));
        setOpen(true);
        return;
      }
      if (isJarFile) {
        // 磁盘上的 jar：列出条目为虚拟子树
        const list = await listJar(e.path, []);
        if (seq !== toggleSeqRef.current) return;
        setKids(buildJarTree(e.path, [], list));
        setOpen(true);
        return;
      }
      reloadDir();
    } catch (err) {
      if (seq !== toggleSeqRef.current) return;
      reportError('展开目录失败', err);
    } finally {
      if (seq === toggleSeqRef.current) setLoading(false);
    }
  }, [e, open, expandable, isJarPath, isJarFile, reloadDir]);

  const go = () => {
    onSelect(e.path);
    if (expandable) { toggle(); } else { loadAndOpenFile(e.path, e.name); }
  };

  // 定位请求（点击标签栏定位图标）：revealPath 在此目录下 → 自动加载并展开。
  // 触发信号用 revealSeq（同一路径重复点击也生效）；依赖不含 open——用户手动收起后不会被重新展开
  useEffect(() => {
    if (!e.is_dir || !revealPath || open) return;
    if (!revealPath.startsWith(e.path + '/')) return;
    const seq = ++toggleSeqRef.current;
    setLoading(true);
    listDirectory(e.path)
      .then(list => { if (seq === toggleSeqRef.current) { setKids(list); setOpen(true); } })
      .catch((err: unknown) => {
        if (seq !== toggleSeqRef.current) return;
        reportError('展开目录失败', err, { variant: 'warning' });
      })
      .finally(() => { if (seq === toggleSeqRef.current) setLoading(false); });
    // 依赖不含 open：open 故意不入依赖（见上方注释）
  }, [revealSeq, e.path, e.is_dir]);

  // 定位到目标文件：滚动使其尽量居中（上下留出上下文）。
  // 同步选中态：避免与之前点击选中的文件残留两个高亮
  useEffect(() => {
    if (!reveal) return;
    // 命中定位：标记已消费（EditorTabs 据此判断"目录树未展开"给出提示）
    useEditorStore.getState().markRevealConsumed();
    entryRef.current?.scrollIntoView({ block: 'center' });
    onSelect(e.path);
    // 触发信号用 revealSeq（reveal 由 revealPath 推导，不入依赖）
  }, [revealSeq]);

  // 在资源管理器中打开（openInExplorer 内部已带失败提示）
  const handleOpenInExplorer = () => {
    setContextMenu(null);
    void openInExplorer(e.path);
  };

  // 复制路径（剪贴板受权限门控，成功/失败提示统一在 copyText 里）
  const handleCopyPath = async () => {
    setContextMenu(null);
    await copyText(e.path, '路径');
  };

  // 复制文件名
  const handleCopyName = async () => {
    setContextMenu(null);
    await copyText(e.name, '文件名');
  };

  // 搜索文件内容（目录 → 搜该目录；文件 → 搜该文件自身）
  const handleOpenSearch = () => {
    setContextMenu(null);
    useSearchModalStore.getState().openSearch(e.path, e.name);
  };

  // 复制文件/文件夹到系统剪贴板（可在资源管理器中 Ctrl+V 粘贴）
  const handleCopy = async () => {
    setContextMenu(null);
    try {
      await copyFilesToClipboard([e.path]);
      showNotification({ variant: 'success', title: '已复制，可在资源管理器中粘贴' });
    } catch (err) {
      reportError('复制到剪贴板失败', err);
    }
  };

  // 粘贴系统剪贴板中的文件到当前目录（成功后刷新目录）
  const handlePaste = async () => {
    setContextMenu(null);
    await pasteInto(e.path, reloadDir);
  };

  // 右键菜单
  const handleContextMenu = (ev: React.MouseEvent) => {
    ev.preventDefault();
    ev.stopPropagation();
    setContextMenu({ x: ev.clientX, y: ev.clientY });
  };

  const iconSvg = e.is_dir
    ? (open ? FolderOpen : FolderClosed)
    : getIconSvg(getExtension(e.name, { lower: true }));
  /** 生成物/依赖目录名淡化（规则表见 fileColors.ts） */
  const dirColor = e.is_dir ? getDirColorClass(e.name) : null;

  return (
    <div>
      <div
        ref={entryRef}
        className={`relative z-10 flex items-center h-[28px] cursor-pointer gap-1 ${
          selected || reveal
            ? 'bg-nexus-selected text-nexus-text'
            : 'text-nexus-text-muted hover:bg-nexus-hover'
        }`}
        style={{ paddingLeft: `${indentPx}px`, paddingRight: 10 }}
        onClick={go}
        onContextMenu={handleContextMenu}
        onMouseEnter={() => setHover(true)}
        onMouseLeave={() => setHover(false)}
      >
        {expandable && (open || hover) && <Chevron open={open} />}
        {(!expandable || (!open && !hover)) && <span className="w-[10px] flex-shrink-0" />}

        <SvgIcon
          svg={iconSvg}
          className={`flex-shrink-0 flex items-center justify-center ${e.is_dir ? 'text-nexus-muted' : 'text-nexus-text-muted'}`}
          style={{ width: 16, height: 16 }}
        />

        <span className={`truncate text-[13px] ml-1 ${dirColor ?? ''}`}>
          {e.name}
        </span>
      </div>

      {/* 右键菜单 */}
      {contextMenu && (
        <ContextMenu x={contextMenu.x} y={contextMenu.y} onClose={() => setContextMenu(null)}>
          {/* 磁盘 jar：打开浏览器标签视图 */}
          {isJarFile && !isJarPath && (
            <div className="py-1.5 px-1.5">
              <ContextMenuItem
                iconBox
                icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
                  <rect x="1" y="2" width="8" height="6" rx="1"/>
                  <path d="M3.5 2v1M6.5 2v1"/>
                </svg>}
                label="打开 jar 浏览器"
                onClick={() => {
                  setContextMenu(null);
                  loadAndOpenFile(e.path, e.name);
                }}
              />
            </div>
          )}

          {/* 搜索文件内容（主操作，独立一组；jar 虚拟节点不支持） */}
          {!isJarPath && (
            <div className={`py-1.5 px-1.5 ${isJarFile ? 'border-t border-nexus-border/30' : ''}`}>
              <ContextMenuItem
                iconBox
                icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
                  <circle cx="4.2" cy="4.2" r="3"/><line x1="6.5" y1="6.5" x2="8.8" y2="8.8"/>
                </svg>}
                label="搜索文件内容"
                onClick={handleOpenSearch}
              />
            </div>
          )}

          {/* 复制 / 粘贴（磁盘节点；jar 虚拟节点不支持） */}
          {!isJarPath && (
            <div className="border-t border-nexus-border/30 py-1.5 px-1.5">
              <ContextMenuItem
                iconBox
                icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
                  <rect x="3.5" y="0.8" width="5.5" height="6" rx="0.8"/>
                  <rect x="1" y="3.2" width="5.5" height="6" rx="0.8"/>
                </svg>}
                label="复制"
                onClick={handleCopy}
              />

              {e.is_dir && (
                <ContextMenuItem
                  iconBox
                  icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
                    <rect x="2" y="1.2" width="6" height="7.6" rx="0.8"/>
                    <path d="M4.5 2.7h1"/>
                    <path d="M5 4.5v3M3.7 6.2 5 7.5l1.3-1.3"/>
                  </svg>}
                  label="粘贴到此处"
                  onClick={handlePaste}
                />
              )}
            </div>
          )}

          {/* 在资源管理器中打开 / 复制路径 / 复制文件名 */}
          <div className="border-t border-nexus-border/30 py-1.5 px-1.5">
            {!isJarPath && (
              <ContextMenuItem
                iconBox
                icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
                  <path d="M1.5 3h2l1-1.5h4a1 1 0 011 1v5.5a1 1 0 01-1 1h-7a1 1 0 01-1-1V3z"/>
                </svg>}
                label="在资源管理器中打开"
                onClick={handleOpenInExplorer}
              />
            )}

            <ContextMenuItem
              iconBox
              icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
                <rect x="3" y="3" width="5" height="5.5" rx=".8"/>
                <path d="M2 2.5v4.5h.5V3.5h4V2.5H3a.5.5 0 00-.5.5z"/>
              </svg>}
              label="复制路径"
              onClick={handleCopyPath}
            />

            <ContextMenuItem
              iconBox
              icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
                <rect x="1" y="2" width="8" height="6" rx="1"/>
                <path d="M3 4h4M3 6h2"/>
              </svg>}
              label="复制文件名"
              onClick={handleCopyName}
            />
          </div>
        </ContextMenu>
      )}

      {open && expandable && (
        <>
          {loading && (
            <div className="text-[11px] text-nexus-muted py-0.5 relative z-10" style={{ paddingLeft: `${childIndentPx + 20}px` }}>…</div>
          )}
          {!loading && (showAll ? kids : kids.slice(0, INITIAL_RENDER_LIMIT)).map(k => (
            <Entry key={k.path} e={k} indentPx={childIndentPx} revealPath={revealPath} revealSeq={revealSeq} onSelect={onSelect} childIndentPx={childIndentPx + INDENT_STEP} />
          ))}
          {!loading && !showAll && kids.length > INITIAL_RENDER_LIMIT && (
            <div
              className="text-[11px] text-nexus-muted py-0.5 cursor-pointer hover:text-nexus-text relative z-10"
              style={{ paddingLeft: `${childIndentPx + 20}px` }}
              onClick={(ev) => { ev.stopPropagation(); setShowAll(true); }}
            >
              还有 {kids.length - INITIAL_RENDER_LIMIT} 项…
            </div>
          )}
        </>
      )}
    </div>
  );
});

/* ---- root ---- */
export function FileTree({ rootPath, embedded, kind = 'service' }: {
  rootPath?: string;
  embedded?: boolean;
  /**
   * 这棵树代表谁：服务的工作目录（默认）还是项目自己的目录。
   * 只影响定位时的平局优先级——服务根与项目根**相同时**（项目根正好是某服务的工作目录），
   * 该由服务树响应（它才是"主角"）。
   */
  kind?: 'project' | 'service';
}) {
  const [entries, setEntries] = useState<FileEntry[]>([]);
  const [err, setErr] = useState<string | null>(null);
  /** 选中态放 store（PERF-18）：见 stores/fileTreeStore.ts——作为 prop 下发会重渲染整棵子树 */
  const setSelectedPath = useFileTreeStore(s => s.setSelectedPath);
  /** 活动标签路径：打开/切换文件时同步为树选中态（保持单一高亮） */
  const activeTabPath = useEditorStore(s => s.tabs.find(t => t.id === s.activeTabId)?.path ?? null);
  /** 定位请求（标签右键「在目录树中定位」触发）：路径 + 信号 */
  const revealPath = useEditorStore(s => s.revealPath);
  const revealSeq = useEditorStore(s => s.revealSeq);

  /** 本树在定位竞争里的身份。**引用必须稳定**：每次渲染都新建对象会让下面的 effect 反复重登记 */
  const target = useMemo<RevealTarget>(
    () => ({ root: rootPath ?? '', priority: REVEAL_PRIORITY[kind] }),
    [rootPath, kind],
  );
  useEffect(() => {
    mountedTrees.push(target);
    return () => {
      const i = mountedTrees.indexOf(target);
      if (i >= 0) mountedTrees.splice(i, 1);
    };
  }, [target]);

  /**
   * 只让"最具体的那棵树"响应定位：不是它就把 `revealPath` 传成 null。
   *
   * `Entry` 的两条定位 effect 前提分别是 `!revealPath`（展开链）与 `!reveal`（命中高亮），
   * 传 null 即自然全不动作——**Entry 一行都不用改**。规则本身见 `utils/fileTree.ts`。
   */
  const revealForMe = pickRevealTarget(mountedTrees, revealPath ?? '') === target ? revealPath : null;

  // 打开/切换文件 → 树选中态跟随 + 清除定位标记（定位高亮是临时的，
  // 用户切换文件后以当前选中文件为主，避免两个高亮）。
  // 无条件赋值（含 null）：关掉最后一个标签后 activeTabPath 变 null，本地选中必须一起清，
  // 否则树里仍高亮着已关闭的文件（选中态与编辑器各说一套真相）
  useEffect(() => {
    setSelectedPath(activeTabPath);
    if (activeTabPath) useEditorStore.getState().clearReveal();
  }, [activeTabPath, setSelectedPath]);

  // 用户手动点击树节点：同样以新选中为主，清除定位标记。
  // useCallback 稳定引用：Entry 是 memo 的，回调每次新身份会让整棵已展开子树白重渲染
  const handleSelect = useCallback((path: string) => {
    setSelectedPath(path);
    useEditorStore.getState().clearReveal();
    // setSelectedPath 是 zustand 的 store setter：引用恒定，进依赖不会让身份变化
  }, [setSelectedPath]);
  /** 根目录加载序号：快速切换目录时丢弃旧响应，避免显示错目录内容 */
  const rootSeqRef = useRef(0);

  const loadRoot = useCallback(async () => {
    if (!rootPath) return;
    const seq = ++rootSeqRef.current;
    try {
      const list = await listDirectory(rootPath);
      if (seq === rootSeqRef.current) { setEntries(list); setErr(null); }
    } catch (e: unknown) {
      if (seq === rootSeqRef.current) setErr(String(e));
    }
  }, [rootPath]);

  useEffect(() => {
    if (!rootPath) { setEntries([]); setErr(null); return; }
    loadRoot();
  }, [loadRoot, rootPath]);

  /** 空白区域右键菜单（仅根目录存在时）：粘贴系统剪贴板文件到项目根 */
  const [rootMenu, setRootMenu] = useState<{ x: number; y: number } | null>(null);

  const handlePasteToRoot = async () => {
    setRootMenu(null);
    if (!rootPath) return;
    await pasteInto(rootPath, loadRoot);
  };

  const basePadding = embedded ? 4 : BASE_PADDING;

  return (
    <div className={`${embedded ? '' : 'h-full bg-nexus-surface'} flex flex-col select-none`}>
      {!embedded && (
        <div className="flex items-center h-[30px] px-4 text-[11px] font-semibold text-nexus-muted uppercase tracking-wider flex-shrink-0">
          {rootPath ? rootPath.split(/[/\\]/).pop() ?? '资源管理器' : '资源管理器'}
        </div>
      )}

      <div
        className={`overflow-y-auto overflow-x-hidden py-0.5 ${embedded ? '' : 'flex-1'}`}
        onContextMenu={(ev) => {
          // 空白区域右键 → 粘贴到项目根（节点自身的 contextmenu 已 stopPropagation，不会冲突）
          if (!rootPath) return;
          ev.preventDefault();
          setRootMenu({ x: ev.clientX, y: ev.clientY });
        }}
      >
        {!rootPath && (
          <div className="px-4 py-10 text-center text-[11px] text-nexus-muted">
            <p className="mb-1">没有打开的文件夹</p>
            <p className="text-[10px] opacity-60">文件 → 打开文件夹</p>
          </div>
        )}
        {rootPath && err && (
          <div className="px-4 py-10 text-center text-[11px] text-nexus-error">{err}</div>
        )}
        {rootPath && !err && entries.length === 0 && (
          <div className="px-4 py-10 text-center text-[11px] text-nexus-muted">空目录</div>
        )}
        {!err && entries.map(ent => (
          <Entry key={ent.path} e={ent} indentPx={basePadding} revealPath={revealForMe} revealSeq={revealSeq} onSelect={handleSelect} childIndentPx={basePadding + INDENT_STEP} />
        ))}

        {/* 空白区域右键菜单：粘贴到项目根目录 */}
        {rootMenu && rootPath && (
          <ContextMenu
            x={rootMenu.x}
            y={rootMenu.y}
            onClose={() => setRootMenu(null)}
            className="py-1.5 px-1.5"
          >
            <ContextMenuItem
              iconBox
              icon={<svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.2">
                <rect x="2" y="1.2" width="6" height="7.6" rx="0.8"/>
                <path d="M4.5 2.7h1"/>
                <path d="M5 4.5v3M3.7 6.2 5 7.5l1.3-1.3"/>
              </svg>}
              label="粘贴到项目根目录"
              onClick={handlePasteToRoot}
            />
          </ContextMenu>
        )}
      </div>
    </div>
  );
}
