import { useEffect, useLayoutEffect, useMemo, useRef } from 'react';
import { open as openUrl } from '@tauri-apps/plugin-shell';
import { marked } from 'marked';
import DOMPurify, { type Config as PurifyConfig } from 'dompurify';
import { readImageData } from '../../services/editor';
import { useEditorStore, loadAndOpenFile, saveActiveFile, flushPendingEdits } from '../../stores/editor';
import { reportError } from '../../utils/error';
import { imageMimeOf } from '../../utils/path';
import { markdownImageSrc, resolveMdLink, headingSlug } from '../../utils/markdown';

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

/**
 * 每个文件「上次看到哪儿」的滚动**比例**（模块级，跨组件实例与跨会话存活）。
 *
 * 为什么需要：预览是**随标签切换整体卸载重挂**的（`ProjectDetail` 只渲染活动标签的查看器），
 * 而滚动位置挂在 DOM 上——重挂之后它就是 0，于是切走再切回会回到第一行。
 * `CodeViewer` 早就有同一套机制（它那里的 `scrollByPath`），预览当初漏了；
 * 而 md 现在**默认就是预览**，所以这个缺口从"偶尔"变成了"每次"。
 *
 * 为什么是**持续跟踪**而不是"卸载前读一把"：React 卸载时先摘 DOM 再跑清理，
 * 那时读 `scrollTop` 永远是 0（`CodeViewer` 的 `trackScroll` 注释里记着这个坑的完整推理：
 * "症状极具误导性——每次都忠实地还原到第一行"）。
 *
 * 为什么存**比例**而不是像素：文件可能被改过（内容重排），窗口或面板宽度也可能变过，
 * 那时像素值指的是另一个位置；比例至少还停在文档的同一个相对位置。
 * 布局没变时比例是精确还原（同一个 max 乘回去）——也就是最常见的"切走又切回"。
 */
const scrollRatioByPath = new Map<string, number>();

/** 最多记多少个文件的比例（与 CodeViewer 的缓存上限同一个量级，防长期使用后无限增长） */
const MAX_SCROLL_ENTRIES = 64;

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
    const ratio = scrollRatioByPath.get(filePath);
    /**
     * 没有记录 = 本次会话还没看过这个文件 → **从头开始**。
     *
     * 这个显式置 0 不能省：md→md 切换时组件不重新挂载、DOM 元素被复用，
     * `scrollTop` 会原样留在**上一个文件**的像素值上（只被新内容的长度夹一下），
     * 于是"打开一个没看过的文件"会停在文档中间——那不叫"记住了位置"，
     * 而是把上一个文件的位置错当成这个文件的。
     */
    el.scrollTop = ratio === undefined ? 0 : ratio * max;
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
      const path = filePathRef.current;
      // 先删再设：让它排到 Map 末尾，下面按插入序淘汰最旧的
      scrollRatioByPath.delete(path);
      scrollRatioByPath.set(path, el.scrollTop / max);
      while (scrollRatioByPath.size > MAX_SCROLL_ENTRIES) {
        const oldest = scrollRatioByPath.keys().next().value;
        if (oldest === undefined) break;
        scrollRatioByPath.delete(oldest);
      }
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
    <div
      ref={rootRef}
      className="md-preview h-full overflow-auto bg-nexus-editor px-6 py-5"
      onClick={handleClick}
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}
