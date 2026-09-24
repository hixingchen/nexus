import { useEffect, useMemo, useRef } from 'react';
import { open as openUrl } from '@tauri-apps/plugin-shell';
import { marked } from 'marked';
import DOMPurify, { type Config as PurifyConfig } from 'dompurify';
import { readImageData } from '../../services/editor';
import { useEditorStore, loadAndOpenFile, saveActiveFile, settlePendingEdit } from '../../stores/editor';
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

export function MarkdownPreview({ filePath }: { filePath: string }) {
  const rootRef = useRef<HTMLDivElement | null>(null);
  /**
   * 内容自己订阅，不由父组件传入（与 CodeViewer 同策略）：`fileContent` 每次按键都变，
   * 让 ProjectDetail 订阅它会把整棵子树拖进重渲染——而预览期间编辑器是卸载的，
   * 改动只可能来自外部（重新读盘），订阅放在本组件里最省。
   */
  const content = useEditorStore(s => s.fileContent) ?? '';

  // 挂载时先结算未合帧的编辑（PERF-13）：否则首帧渲染的是 store 里的旧内容
  useEffect(() => { settlePendingEdit(); }, []);

  // marked 默认不过滤（它也明确不做净化，见其文档）→ 必须紧跟一层 DOMPurify
  const html = useMemo(
    () => DOMPurify.sanitize(marked.parse(content, { async: false }) as string, SANITIZE_CONFIG),
    [content],
  );

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
