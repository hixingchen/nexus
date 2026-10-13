import { useEffect, useRef } from 'react';
import { open as openUrl } from '@tauri-apps/plugin-shell';
import { marked } from 'marked';
import DOMPurify, { type Config as PurifyConfig } from 'dompurify';
import readmeRaw from '../../../README.md?raw';
import { Modal } from '../ui/Modal';
import { reportError } from '../../utils/error';
import { headingSlug } from '../../utils/markdown';
import { REPO_URL, repoBlobUrl } from '../../utils/links';

/**
 * 使用文档 = **仓库 README 的原文**，编译期用 Vite 的 `?raw` 内嵌进来。
 *
 * 为什么是内嵌原文而不是另写一份帮助文档：两份文档从写下的那一刻就开始分叉，而分叉的方向
 * 永远是"仓库里的对、应用里的旧"——用户照着过期的说明操作，比没有说明更坏。
 * `?raw` 让 README 成为唯一真相：改文档就是改这里显示的内容，不需要谁记得同步。
 *
 * 链接**一律交给系统浏览器**（`@tauri-apps/plugin-shell` 的 `open`）：应用内有导航守卫
 * （`lib.rs`），webview 自己导航会被拦掉，表现是"点了没反应"；而且预览是
 * `dangerouslySetInnerHTML` 渲染的，放它导航等于让文档内容决定 webview 去哪儿。
 *
 * 相对链接**特意不走** `utils/markdown.ts` 的 `resolveMdLink`：那条路会把
 * `src-tauri/resources/cfr-LICENSE.txt` 解析成本机绝对路径交给编辑器打开，而应用里既没有
 * 仓库目录（用户装的是安装包，不是 clone），这个路径也必然落在文件访问白名单之外——
 * 结果是"点了报一个读文件失败"。指回 GitHub 网页（`repoBlobUrl`）是"点得动"与
 * "不越权"之间唯一同时成立的选项。
 */

/**
 * 清洗配置与 `components/editor/MarkdownPreview.tsx` **逐条一致**。
 *
 * 为什么不因为"README 是我们自己的文件"就省掉这一步：清洗口径一旦分叉，就会分叉在没人看的
 * 那一份上。README 会随 PR 变化（谁都能往里加一段 HTML），而"自己人写的内容"不是一条
 * 可以放松的安全边界；真出了事，这里少一条规则不会有任何提示。
 *
 * 具体条目（`style`/`form` 为什么必须禁）见 MarkdownPreview.tsx 的头注释与 SEC-25/26。
 */
const SANITIZE_CONFIG: PurifyConfig = {
  USE_PROFILES: { html: true },
  FORBID_TAGS: ['style', 'form'],
  FORBID_ATTR: ['style'],
};

/**
 * README 的渲染结果，模块级只算一次。
 *
 * 为什么不放 `useMemo`：内容是编译期常量，与 props / 生命周期无关；放模块级还顺带避开
 * "每次打开弹窗重排一遍 markdown"（marked 解析 + DOMPurify 清洗都不是零成本）。
 */
const README_HTML = DOMPurify.sanitize(marked.parse(readmeRaw, { async: false }) as string, SANITIZE_CONFIG);

/** README 里的标题（`h1`~`h6`），补 id 与找锚点用同一份选择器 */
const HEADING_SELECTOR = 'h1, h2, h3, h4, h5, h6';

export function UsageDocsModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const rootRef = useRef<HTMLDivElement | null>(null);

  /**
   * 给标题补 id，让 README 里的 `[文字](#小节)` 真能跳。
   *
   * 为什么自己做：marked v5 起移除了 `headerIds`（规则见 `utils/markdown.ts` 的 `headingSlug`，
   * 与编辑器预览共用同一套口径）。
   *
   * 为什么依赖 `open` 而不是挂载时跑一次：弹窗关闭时 `Modal` 返回 null、DOM 整个被卸载，
   * 重新打开是一批**新的**标题元素（没有 id）。只在挂载时跑，第二次打开锚点就全失效了。
   */
  useEffect(() => {
    if (!open) return;
    const root = rootRef.current;
    if (!root) return;
    for (const h of Array.from(root.querySelectorAll(HEADING_SELECTOR))) {
      if (h.id) continue; // 文档里已经手写了 id 的尊重原样
      const id = headingSlug(h.textContent ?? '');
      if (id) h.id = id;
    }
  }, [open]);

  /** 文档内锚点：滚到对应标题（找不到就什么都不做——一个写着旧章节名的目录不该报错） */
  const scrollToAnchor = (rawId: string) => {
    const root = rootRef.current;
    let id = rawId.trim();
    if (!root || !id) return;
    try {
      id = decodeURIComponent(id); // 链接可能是 %E5%AE%89%E8%A3%85 这种形式
    } catch {
      // 不是合法的百分号编码：原样拿去匹配。这里不能让 URIError 冒出去——
      // 它会把一次点击变成"点了没反应"，而原因（文档里写错了）谁也看不见
    }
    const el = root.querySelector(`[id="${CSS.escape(id)}"]`)
      // 大小写/标点与 slug 对不上时（手写的 `## 安装` 目录链接）再按 slug 比一遍
      ?? Array.from(root.querySelectorAll(HEADING_SELECTOR))
        .find(h => headingSlug(h.textContent ?? '') === headingSlug(id));
    el?.scrollIntoView({ block: 'start' });
  };

  /**
   * 链接分发（事件委托挂在渲染容器上）。
   *
   * 三类：锚点留在弹窗内滚；http(s)/mailto 交给系统；其余（相对路径）指回仓库网页。
   * 协议白名单与 `resolveMdLink` 同一口径：`javascript:`/`data:`/`file:` 一律不处理——
   * 刻意**不**把它们也拼成仓库地址，那会造出一个看着合法、实则 404 的链接，
   * 把"文档写错了"伪装成"仓库里没这个文件"。
   */
  const handleClick = (e: React.MouseEvent<HTMLDivElement>) => {
    // 收窄而不是断言：点中链接里的**文字**时 `e.target` 是文本节点，没有 `closest`，
    // `as Element` 会一路跑到 "closest is not a function"（同款坑见 TitleBar 的注释）
    const target = e.target;
    if (!(target instanceof Element)) return;
    const anchor = target.closest('a[href]');
    if (!anchor) return;
    // 一律拦下：href 来自文档内容，不能让它导航 webview（见文件头注释）
    e.preventDefault();

    const href = (anchor.getAttribute('href') ?? '').trim();
    if (!href) return;

    if (href.startsWith('#')) {
      scrollToAnchor(href.slice(1));
      return;
    }

    const scheme = /^([a-zA-Z][a-zA-Z0-9+.-]*):/.exec(href)?.[1]?.toLowerCase() ?? '';
    if (scheme === 'http' || scheme === 'https' || scheme === 'mailto') {
      void openUrl(href).catch((err) => reportError('打开链接失败', err));
      return;
    }
    if (scheme) return; // 上面那三个之外的协议：不处理，见 handleClick 的说明

    // 相对路径（如 `src-tauri/resources/cfr-LICENSE.txt`）：指回仓库网页，理由见文件头
    void openUrl(repoBlobUrl(href)).catch((err) => reportError('打开链接失败', err));
  };

  return (
    <Modal open={open} title="使用文档" onClose={onClose} width="900px">
      <div className="flex flex-col gap-2">
        <div className="flex items-center gap-3">
          <p className="flex-1 min-w-0 text-[12px] text-nexus-muted">
            这是仓库 README 的原文（随应用一起打包）。链接都在系统浏览器里打开，仓库内的相对链接指回 GitHub 网页。
          </p>
          <button
            className="flex-shrink-0 px-2 py-1 text-[11px] text-nexus-accent border border-nexus-accent/40 rounded hover:bg-nexus-accent/10"
            // 说清这一下去哪：README 很长，用户想找的东西未必在里面，得有个"直接去仓库看"的出口
            title="在系统浏览器中打开仓库主页"
            onClick={() => { void openUrl(REPO_URL).catch((err) => reportError('打开网页失败', err)); }}
          >在 GitHub 上打开</button>
        </div>

        {/* 滚动容器自己管：`Modal` 只在 sidebar 形态下限高，普通弹窗的内容再长也不会滚，
            而 README 一定比一屏长——不限高的话标题栏（含关闭按钮）会被顶出视口，点不到。
            正文排版复用全局的 `.md-preview`（index.css）：README 有表格与代码块，
            而那一套排版就是为本仓库的深色主题手写的，另写一份只会多出一处会漂移的取值。 */}
        <div className="max-h-[calc(100vh-190px)] overflow-y-auto rounded border border-nexus-border bg-nexus-bg/30 px-4 py-3">
          <div
            ref={rootRef}
            className="md-preview"
            onClick={handleClick}
            dangerouslySetInnerHTML={{ __html: README_HTML }}
          />
        </div>
      </div>
    </Modal>
  );
}
