/**
 * Markdown 预览的纯逻辑（便于直接测）。
 *
 * 渲染与清洗在组件里（`marked` + `DOMPurify`），这里只放**能被断言钉住**的部分：
 * 哪些文件算 md、预览里的链接与图片该往哪儿走。
 */

/** 支持预览的扩展名 */
export const MD_EXTENSIONS = new Set(['md', 'markdown', 'mdown', 'mkd']);

/** 该文件是否走 Markdown 预览（`.md` / `.markdown` / `.mdown` / `.mkd`） */
export function isMarkdownFile(path: string, opts?: { lower?: boolean }): boolean {
  const name = path.split(/[\\/]/).pop() ?? '';
  const dot = name.lastIndexOf('.');
  if (dot < 0) return false;
  const ext = name.slice(dot + 1);
  return MD_EXTENSIONS.has(opts?.lower === false ? ext : ext.toLowerCase());
}

/**
 * 标题文本 → 锚点 id（近似 GitHub 规则）：小写、去标点、空格转连字符、**保留中文**。
 *
 * 为什么需要：marked v5 起不再生成 `headerIds`（官方让用 `marked-gfm-heading-id` 插件），
 * 而"README 带目录"是常态——不给标题补 id，`[安装](#安装)` 这类链接点了没反应。
 */
export function headingSlug(text: string): string {
  return text
    .trim()
    .toLowerCase()
    .replace(/\s+/g, '-')
    .replace(/[^\p{L}\p{N}\-_]/gu, '');
}

/** 预览里一次点击的目标 */
export type MdLinkTarget =
  /** 外部链接（http/https/mailto…）：交给系统浏览器/邮件客户端 */
  | { kind: 'external'; url: string }
  /** 仓库内的相对路径：解析成绝对路径后在编辑器里打开 */
  | { kind: 'local'; path: string }
  /** 文档内锚点（#小节）：预览内滚动 */
  | { kind: 'anchor'; id: string };

/**
 * 解析预览里点击的链接。
 *
 * 为什么要自己解析而不是让浏览器导航：预览是 `dangerouslySetInnerHTML` 渲染的，
 * 链接的 href 来自被预览的文件——**直接让它导航等于把 webview 交给文件内容摆布**
 * （相对链接会跳走、`data:`/`javascript:` 之类会尝试执行）。这里把可点的范围收成三类，
 * 其余一律忽略（返回 null）。
 */
export function resolveMdLink(href: string, mdFilePath: string): MdLinkTarget | null {
  const raw = href.trim();
  if (!raw) return null;
  if (raw.startsWith('#')) {
    const id = raw.slice(1).trim();
    return id ? { kind: 'anchor', id } : null;
  }
  // 显式协议：只有这几个放行；javascript: / data: / file: 等一律忽略
  const scheme = /^([a-zA-Z][a-zA-Z0-9+.-]*):/.test(raw) ? raw.slice(0, raw.indexOf(':')).toLowerCase() : '';
  if (scheme) {
    return scheme === 'http' || scheme === 'https' || scheme === 'mailto'
      ? { kind: 'external', url: raw }
      : null;
  }
  // 无协议 = 相对路径（含 ./ ../ 与裸文件名）。去掉 ?query 与 #fragment
  const pathPart = raw.split('#')[0].split('?')[0];
  if (!pathPart || pathPart.startsWith('//')) return null; // 协议相对 URL 交给浏览器处理不了，忽略
  const dir = mdFilePath.replace(/[\\/][^\\/]*$/, '');
  const sep = mdFilePath.includes('\\') ? '\\' : '/';
  const joined = `${dir}${sep}${pathPart}`.replace(/[\\/]+/g, sep);
  return { kind: 'local', path: normalizeSegments(joined, sep) };
}

/** 折叠路径里的 `.` 与 `..`（不碰盘符；用于把 `../a/b.md` 变成绝对路径） */
function normalizeSegments(path: string, sep: string): string {
  const parts = path.split(sep);
  const out: string[] = [];
  for (const part of parts) {
    if (part === '.' || part === '') continue;
    if (part === '..' && out.length > 1) {
      out.pop();
      continue;
    }
    out.push(part);
  }
  // 保留 UNC / 盘符开头：被上面跳过的空串在 Windows 路径里是 `\\server\share` 的前导
  return path.startsWith(sep + sep) ? sep + sep + out.join(sep) : out.join(sep);
}

/**
 * 预览里的图片 src 该怎么取。
 *
 * `data:` 直接可用（CSP 允许）；http(s) 会被 CSP 的 `img-src 'self' data:` 挡掉，
 * 但我们仍然放它过去——浏览器只是不显示，不会出错；相对路径需要**读盘转 base64**
 * （复用 `read_image_data`，它自带白名单与大小上限）。
 */
export function markdownImageSrc(src: string, mdFilePath: string): { kind: 'inline' } | { kind: 'local'; path: string } | { kind: 'skip' } {
  const raw = src.trim();
  if (!raw) return { kind: 'skip' };
  if (raw.startsWith('data:image/')) return { kind: 'inline' };
  if (/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(raw)) return { kind: 'skip' }; // http(s)/其它协议：交给 CSP 决定
  const link = resolveMdLink(raw, mdFilePath);
  return link?.kind === 'local' ? { kind: 'local', path: link.path } : { kind: 'skip' };
}
