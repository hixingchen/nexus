import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { open as openUrl } from '@tauri-apps/plugin-shell';
import { marked } from 'marked';
import DOMPurify, { type Config as PurifyConfig } from 'dompurify';
import { readImageData } from '../../services/editor';
import { useEditorStore, loadAndOpenFile, saveActiveFile, flushPendingEdits } from '../../stores/editor';
import { reportError } from '../../utils/error';
import { imageMimeOf } from '../../utils/path';
import { markdownImageSrc, resolveMdLink, headingSlug } from '../../utils/markdown';
import { rememberScroll, recallScroll } from '../../utils/editorScroll';
import { isSubmitEnter } from '../../utils/keyboard';
import { useUiStore } from '../../stores/uiStore';

/**
 * Markdown 预览（只读）。
 *
 * **`DOMPurify` 是这里的安全边界，不是可选项**：预览的内容来自文件本身，而文件可能来自
 * 克隆的第三方仓库——`dangerouslySetInnerHTML` 直接吃它等于把 webview 交给文件内容。
 * 具体地：**打包态**下 CSP 已经挡住 `<script>`、内联事件处理器（`onerror=` 之类）、
 * `javascript:` 导航、iframe/object/form，但 **`style-src 'unsafe-inline'` 允许注入
 * `<style>`**（可以拿它做 UI 覆盖/伪装）——`FORBID_TAGS: ['style']` 正是为这一条写的。
 * 另外只放行 HTML profile（不含 SVG/MathML）：预览不需要它们，而它们带得动脚本向量。
 *
 * ⚠️ **dev 态没有 CSP**（SEC-25）：CSP 挂在 `tauri://` 协议的资源响应上，而 dev 的文档
 * 来自 Vite、不经过它——"被绕过时由 CSP 兜底"这层在开发环境下**不存在**，那一层由
 * `lib.rs` 的导航守卫与这里的 DOMPurify 顶上。别按"CSP 已经挡住了"来推 dev 的行为。
 *
 * 链接与图片**不由浏览器直接处理**（见 `utils/markdown.ts`）：hre 来自被预览的文件，
 * 放它导航就等于让文件决定 webview 去哪儿。这里全部拦下来自己分发。
 */
const SANITIZE_CONFIG: PurifyConfig = {
  USE_PROFILES: { html: true },
  // CSP 的 style-src 允许 'unsafe-inline'，所以样式类注入必须两条都堵：
  // `<style>` 元素与 `style="…"` 属性。DOMPurify 的 CSS 清洗只管"危险取值"
  // （url(javascript:) 之类），**不管"布局接管"**——`position:fixed;inset:0` 加个底色
  // 就能盖住整个界面做伪装。预览里的 md 不需要内联样式来承载内容。
  //
  // `form` 一并禁掉（SEC-26）：html profile 的白名单里**含 form/input/textarea/button/
  // select**（见 dompurify 的 html$1 数组），于是任意仓库的 md 都能在窗口里渲染出一个
  // 可交互的假登录框——UI 伪装。打包态 `form-action 'none'` 只挡提交，dev 态连提交也放行。
  // **不能顺手把 input 也禁掉**：任务列表（`- [x] 完成`）正是渲染成 `<input type=checkbox>`，
  // 禁了会把正常内容一起弄没。
  FORBID_TAGS: ['style', 'form'],
  FORBID_ATTR: ['style'],
};

// ── 预览内查找（Ctrl+F）────────────────────────────────────────
/** CSS Custom Highlight API 的两个高亮名：全部命中 / 当前命中（只画，不改 DOM） */
const FIND_ALL_HIGHLIGHT = 'nexus-md-find';
const FIND_CURRENT_HIGHLIGHT = 'nexus-md-find-current';
/** 查找输入防抖（与日志面板搜索同量级） */
const FIND_DEBOUNCE_MS = 150;
/** 命中数上限：再长的文档也不值得把整篇都标成高亮 */
const MAX_FIND_MATCHES = 1000;

/** 转义正则元字符：查找词按字面匹配（与源码查找一致） */
function escapeRegExp(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/**
 * 在**渲染后的文本节点**里找命中（大小写不敏感、字面匹配）。
 *
 * 用带 `i` 标志的正则而不是 `toLowerCase().indexOf`：Unicode 小写化会改变字符长度
 * （İ → i̇），小写副本里的下标套回原文会错位；正则的 `match.index` 始终是原文下标。
 *
 * 已知边界：命中**不跨行内元素**——`hello **world**` 里搜 "hello world" 找不到。
 * 源码模式搜的是原始 markdown（`**` 还在文本里），两种模式的"同一句话"本来就不完全等价；
 * 这里按用户看到的渲染文本匹配，符合"预览里查找"的直觉。
 */
function collectFindRanges(root: HTMLElement, query: string): Range[] {
  const ranges: Range[] = [];
  if (!query) return ranges;
  const re = new RegExp(escapeRegExp(query), 'gi');
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  let node: Node | null;
  while ((node = walker.nextNode())) {
    const text = node.nodeValue ?? '';
    if (!text) continue;
    re.lastIndex = 0;
    let m: RegExpExecArray | null;
    while ((m = re.exec(text)) !== null) {
      if (m[0].length === 0) { re.lastIndex += 1; continue; }
      const range = document.createRange();
      range.setStart(node, m.index);
      range.setEnd(node, m.index + m[0].length);
      ranges.push(range);
      if (ranges.length >= MAX_FIND_MATCHES) return ranges;
    }
  }
  return ranges;
}

/**
 * 预览的滚动位置与**源码模式共享**（`utils/editorScroll`）：两边都按"滚动比例"记录。
 *
 * 为什么需要：预览随标签切换整体卸载重挂（`ProjectDetail` 只渲染活动标签的查看器），
 * 滚动位置挂在 DOM 上——重挂之后就是 0，切走再切回会回到第一行。
 *
 * 为什么是**持续跟踪**而不是"卸载前读一把"：React 卸载时先摘 DOM 再跑清理，
 * 那时读 `scrollTop` 永远是 0（`CodeViewer` 的 `trackScroll` 注释里记着这个坑的完整推理：
 * "症状极具误导性——每次都忠实地还原到第一行"）。
 *
 * 为什么存**比例**而不是像素：文件可能被改过（内容重排），窗口或面板宽度也可能变过，
 * 那时像素值指的是另一个位置；比例至少还停在文档的同一个相对位置。**跨模式**也只能
 * 用比例——预览拿不到源码行号（见 `utils/editorScroll` 的说明）。
 */
export function MarkdownPreview({ filePath }: { filePath: string }) {
  const rootRef = useRef<HTMLDivElement | null>(null);
  /**
   * 当前文件路径的 ref：滚动监听只挂一次，事件里读它拿最新值。
   *
   * **必须在 render 期间赋值**（而不是放进 effect）：md→md 切换时组件不重新挂载，
   * 浏览器替换 innerHTML 后夹取 scrollTop 触发的那个 scroll 事件可能早于 effect 重跑，
   * 只有 render 期的赋值能保证那一刻读到的是新路径（见滚动跟踪 effect 的说明）。
   */
  const filePathRef = useRef(filePath);
  filePathRef.current = filePath;
  /**
   * 内容自己订阅，不由父组件传入（与 CodeViewer 同策略）：`fileContent` 每次按键都变，
   * 让 ProjectDetail 订阅它会把整棵子树拖进重渲染——而预览期间编辑器是卸载的，
   * 改动只可能来自外部（重新读盘），订阅放在本组件里最省。
   */
  const content = useEditorStore(s => s.fileContent) ?? '';

  // ── 预览内查找（Ctrl+F）──
  const [findOpen, setFindOpen] = useState(false);
  const [findQuery, setFindQuery] = useState('');
  const [findCount, setFindCount] = useState(0);
  const [findIndex, setFindIndex] = useState(0);
  const findInputRef = useRef<HTMLInputElement | null>(null);
  /** 当前命中的 Range（导航/滚动用；不参与渲染） */
  const findRangesRef = useRef<Range[]>([]);


  // 挂载时先结算未合帧的编辑（PERF-13）：否则首帧渲染的是 store 里的旧内容
  useEffect(() => { flushPendingEdits(); }, []);

  // marked 默认不过滤（它也明确不做净化，见其文档）→ 必须紧跟一层 DOMPurify
  const html = useMemo(
    () => DOMPurify.sanitize(marked.parse(content, { async: false }) as string, SANITIZE_CONFIG),
    [content],
  );

  /**
   * 还原上次看到的位置。
   *
   * 用 `useLayoutEffect` 而不是 `useEffect`：后者跑在浏览器绘制之后，用户会看到
   * "先在第一行闪一下、再跳到上次的位置"——比不还原更刺眼。依赖 `html`：内容变了
   * （重新读盘/在源码模式改过）也要落回同一个相对位置。
   *
   * 已知边界：文档里的**本地图片是异步读盘转 base64 的**（见下面那个 effect），
   * 图加载完会把内容撑高、max 变大，于是这次还原的位置会比上次略靠前。
   * 没去纠正它是有意的——纠正要么和用户自己的滚动打架（程序化滚动同样会触发 scroll
   * 事件，"这次滚动是不是用户干的"在同一个 tick 里分不干净），要么引入一个
   * 时间窗启发式，两种都比如实说明这个偏差更糟。
   */
  useLayoutEffect(() => {
    const el = rootRef.current;
    if (!el) return;
    const max = el.scrollHeight - el.clientHeight;
    if (max <= 0) return;   // 内容不足一屏：无处可滚（也不该把记录覆盖成 0）
    /**
     * 记忆里可能是**源码模式**刚写下的比例（用户从源码切过来，正是要接上的位置），
     * 也可能是预览自己上次的比例。
     *
     * 没有记录 = 本次会话还没在任何模式里看过这个文件 → **从头开始**。这个显式置 0
     * 不能省：md→md 切换时组件不重新挂载、DOM 元素被复用，`scrollTop` 会原样留在
     * **上一个文件**的像素值上（只被新内容的长度夹一下），于是"打开一个没看过的文件"
     * 会停在文档中间——那不叫"记住了位置"，而是把上一个文件的位置错当成这个文件的。
     */
    const memory = recallScroll(filePath);
    el.scrollTop = memory === undefined ? 0 : memory.ratio * max;
    // 落位后立刻记一次（写者=预览）：md→md 切换时滚动事件可能早于本 effect，
    // 由这里保证"当前文件、当前位置"一定写进了共享表（源码切回来时要用它）
    const after = el.scrollHeight - el.clientHeight;
    if (after > 0) rememberScroll(filePath, 'preview', el.scrollTop / after);
  }, [html, filePath]);

  /**
   * 持续记录滚动位置（挂在滚动事件上，**只挂一次**，靠 ref 读当前文件）。
   *
   * 为什么不能"卸载前读一把"：React 删除组件时先摘 DOM、再跑清理，那时读到的是 0
   * ——`CodeViewer` 的 `trackScroll` 注释里为这件事付过一次代价。
   * 位置要在**用户滚动的那一刻**就记下来，之后谁什么时候销毁组件都不影响它。
   *
   * 为什么 `filePath` 走 ref 而不是进依赖：这个组件在 md→md 切换时**不会重新挂载**
   * （`ProjectDetail` 在同一个位置渲染它，只换 prop），DOM 元素被复用、`innerHTML` 被替换，
   * 浏览器随之把 `scrollTop` 夹到新内容允许的范围内——**并触发一次 scroll 事件**。
   * 那次夹取记到"新文件"名下才是对的；若闭包还拿着旧路径，它就会把**旧文件**记下的位置
   * 覆盖成 0（典型场景：从长文档切到短文档，夹到 0，于是切回长文档还在第一行）。
   * 而 ref 在 render 期间就已更新（早于 commit 与任何事件），所以无论事件落到哪个监听器上，
   * 记的都是当时屏幕上真正显示的那个文件。
   */
  useEffect(() => {
    const el = rootRef.current;
    if (!el) return;
    const onScroll = () => {
      const max = el.scrollHeight - el.clientHeight;
      // 不足一屏时 max ≤ 0：不记（否则一个恒为 0 的比例会盖掉之前记住的位置）
      if (max <= 0) return;
      rememberScroll(filePathRef.current, 'preview', el.scrollTop / max);
    };
    // passive：滚动是高频事件，我们只是记录，不阻止默认行为
    el.addEventListener('scroll', onScroll, { passive: true });
    return () => el.removeEventListener('scroll', onScroll);
  }, []);

  /**
   * 给标题补 id，让 `[文字](#小节)` 这类目录链接真的能跳。
   *
   * 为什么自己做：marked v5 起移除了 `headerIds`（官方让用 `marked-gfm-heading-id`），
   * 而 README 带目录是常态。规则与 GitHub 近似（小写、空格转连字符、去标点、保留中文）。
   */
  useEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    for (const h of Array.from(root.querySelectorAll('h1, h2, h3, h4, h5, h6'))) {
      if (h.id) continue; // 原始 HTML 里已给 id 的尊重原样
      const id = headingSlug(h.textContent ?? '');
      if (id) h.id = id;
    }
  }, [html]);

  /**
   * 相对路径的图片读盘转 base64。
   *
   * 为什么在 DOM 上改 src 而不是重渲染：一次投影里可能有几十张图，重渲染会整篇重排；
   * 而且读盘是异步的，逐张到达即显示更自然。读失败只留空位——一张图失败不该弹错刷屏。
   */
  useEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    let alive = true;
    for (const img of Array.from(root.querySelectorAll('img'))) {
      const verdict = markdownImageSrc(img.getAttribute('src') ?? '', filePath);
      if (verdict.kind !== 'local') continue;
      readImageData(verdict.path)
        .then(b64 => {
          if (alive && img.isConnected) img.src = `data:${imageMimeOf(verdict.path)};base64,${b64}`;
        })
        .catch(() => { /* 图不存在/超限：留空位，不打扰 */ });
    }
    return () => { alive = false; };
  }, [html, filePath]);

  /**
   * 预览期间 Ctrl+S 仍然保存。
   *
   * 为什么需要：保存的键位挂在 CodeMirror 上，而预览时编辑器是**卸载**的——
   * 不补这一条就会出现"有未保存改动（标签带圆点）按 Ctrl+S 毫无反应"，
   * 与源码模式的行为不一致。保存读的是 store 里的内容，不需要编辑器在场。
   */
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 's') {
        e.preventDefault();
        void saveActiveFile();
      }
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, []);

  // ── 预览内查找（Ctrl+F）：与源码模式同一套肌肉记忆 ──────────

  /** 把命中集合注册成高亮（全部 + 当前）；老 WebView 没有 Highlight API 时降级为只滚动 */
  const applyFindHighlights = useCallback((ranges: Range[], index: number) => {
    if (typeof CSS === 'undefined' || !('highlights' in CSS)) return;
    if (ranges.length === 0) {
      CSS.highlights.delete(FIND_ALL_HIGHLIGHT);
      CSS.highlights.delete(FIND_CURRENT_HIGHLIGHT);
      return;
    }
    CSS.highlights.set(FIND_ALL_HIGHLIGHT, new Highlight(...ranges));
    const current = ranges[index];
    if (current) CSS.highlights.set(FIND_CURRENT_HIGHLIGHT, new Highlight(current));
    else CSS.highlights.delete(FIND_CURRENT_HIGHLIGHT);
  }, []);

  /** 把命中滚进视口（已在视口内则不动，避免每输入一个字就跳） */
  const scrollToMatch = useCallback((range: Range | undefined) => {
    const root = rootRef.current;
    if (!root || !range) return;
    const rect = range.getBoundingClientRect();
    const box = root.getBoundingClientRect();
    // 顶部留出查找条的高度：被浮层盖住的命中不能算"已经看得到"
    if (rect.top >= box.top + 48 && rect.bottom <= box.bottom - 8) return;
    root.scrollTop += rect.top - box.top - box.height / 3;
  }, []);

  const closeFind = useCallback(() => {
    setFindOpen(false);
    setFindQuery('');
    setFindCount(0);
    setFindIndex(0);
    findRangesRef.current = [];
    if (typeof CSS !== 'undefined' && 'highlights' in CSS) {
      CSS.highlights.delete(FIND_ALL_HIGHLIGHT);
      CSS.highlights.delete(FIND_CURRENT_HIGHLIGHT);
    }
  }, []);

  const goFind = useCallback((dir: 1 | -1) => {
    const ranges = findRangesRef.current;
    if (ranges.length === 0) return;
    setFindIndex((prev) => {
      const next = ((prev + dir) % ranges.length + ranges.length) % ranges.length;
      applyFindHighlights(ranges, next);
      scrollToMatch(ranges[next]);
      return next;
    });
  }, [applyFindHighlights, scrollToMatch]);

  /** Ctrl+F：打开/聚焦查找。只在预览挂载期间生效（源码模式由 CodeViewer 自己那套处理） */
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.altKey || e.key.toLowerCase() !== 'f') return;
      e.preventDefault();
      e.stopPropagation();
      setFindOpen(true);
      requestAnimationFrame(() => {
        findInputRef.current?.focus();
        findInputRef.current?.select();
      });
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, []);

  /** Esc 关闭查找（焦点不在输入框里也生效；有弹窗时让弹窗优先） */
  useEffect(() => {
    if (!findOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      if (useUiStore.getState().modalCount > 0) return;
      e.stopPropagation();
      closeFind();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [findOpen, closeFind]);

  /**
   * 查找词 / 渲染内容变化 → 重新收集命中（防抖）。
   *
   * 依赖 `html`：预览内容被外部更新时 DOM 会被整块替换，旧的 Range 全部失效，
   * 必须按新 DOM 重算；文件在两种模式间切换时这条也会把"同一查找词"带到新内容上。
   */
  useEffect(() => {
    if (!findOpen) return;
    const t = setTimeout(() => {
      const root = rootRef.current;
      if (!root) return;
      const ranges = collectFindRanges(root, findQuery);
      findRangesRef.current = ranges;
      setFindCount(ranges.length);
      setFindIndex(0);
      applyFindHighlights(ranges, 0);
      scrollToMatch(ranges[0]);
    }, FIND_DEBOUNCE_MS);
    return () => clearTimeout(t);
  }, [findQuery, findOpen, html, applyFindHighlights, scrollToMatch]);

  /** 卸载：高亮注册在 document 上，不随组件消失，必须主动清 */
  useEffect(() => () => {
    if (typeof CSS !== 'undefined' && 'highlights' in CSS) {
      CSS.highlights.delete(FIND_ALL_HIGHLIGHT);
      CSS.highlights.delete(FIND_CURRENT_HIGHLIGHT);
    }
  }, []);

  const handleClick = (e: React.MouseEvent<HTMLDivElement>) => {
    const anchor = (e.target as Element | null)?.closest?.('a');
    if (!anchor) return;
    // 一律拦下：hre 来自被预览的文件，不能让它导航 webview（见组件头注释）
    e.preventDefault();
    const target = resolveMdLink(anchor.getAttribute('href') ?? '', filePath);
    if (!target) return;
    if (target.kind === 'external') {
      void openUrl(target.url).catch((err) => reportError('打开链接失败', err));
      return;
    }
    if (target.kind === 'local') {
      const name = target.path.split(/[\\/]/).pop() ?? target.path;
      void loadAndOpenFile(target.path, name);
      return;
    }
    const el = rootRef.current?.querySelector(`[id="${CSS.escape(target.id)}"]`);
    el?.scrollIntoView({ block: 'start' });
  };

  return (
    <div className="relative h-full">
      <div
        ref={rootRef}
        className="md-preview h-full overflow-auto bg-nexus-editor px-6 py-5"
        onClick={handleClick}
        dangerouslySetInnerHTML={{ __html: html }}
      />

      {/* 查找条：浮在预览右上角（不参与文档流，内容滚动时不跟着滚） */}
      {findOpen && (
        <div className="absolute right-4 top-3 z-10 flex items-center gap-1.5 h-[32px] px-2.5 rounded-lg bg-nexus-surface border border-nexus-border shadow-lg">
          <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5"
            className="text-nexus-muted flex-shrink-0">
            <circle cx="6.5" cy="6.5" r="4.5"/><path d="M10 10l4 4"/>
          </svg>
          <input
            ref={findInputRef}
            aria-label="在预览中查找"
            className="w-[150px] bg-transparent text-[12px] text-nexus-text outline-none placeholder:text-nexus-muted/50 font-mono"
            placeholder="查找…"
            value={findQuery}
            onChange={e => setFindQuery(e.target.value)}
            onKeyDown={e => {
              if (e.key === 'Escape') {
                e.stopPropagation();
                closeFind();
                return;
              }
              if (isSubmitEnter(e)) {
                e.preventDefault();
                goFind(e.shiftKey ? -1 : 1);
              }
            }}
          />
          <span className="w-[46px] text-right text-[11px] text-nexus-muted font-mono tabular-nums flex-shrink-0">
            {findCount > 0 ? `${findIndex + 1}/${findCount}` : '0/0'}
          </span>
          <button
            aria-label="上一个匹配（Shift+Enter）"
            className="text-[10px] text-nexus-muted hover:text-nexus-text flex-shrink-0"
            onClick={() => goFind(-1)}
          >▲</button>
          <button
            aria-label="下一个匹配（Enter）"
            className="text-[10px] text-nexus-muted hover:text-nexus-text flex-shrink-0"
            onClick={() => goFind(1)}
          >▼</button>
          <button
            aria-label="关闭查找"
            className="ml-0.5 flex-shrink-0 p-0.5 rounded text-nexus-muted hover:text-nexus-text hover:bg-nexus-hover/50"
            onClick={closeFind}
          >
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round">
              <line x1="2" y1="2" x2="8" y2="8"/><line x1="8" y1="2" x2="2" y2="8"/>
            </svg>
          </button>
        </div>
      )}
    </div>
  );
}
