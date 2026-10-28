/**
 * AI 运行时 npm 源的纯逻辑：默认值、常用预设、校验与归一化。
 *
 * 与 `src-tauri/src/core/runtime.rs::normalize_npm_registry` 保持同一口径：
 * 后端是最终门禁（设置写错时安装会在下载 Node 之前失败），这里是给输入框的即时反馈。
 * 两边规则一旦漂移，就会出现"界面说能存、安装时才报错"的割裂。
 */

/** 默认源：淘宝镜像（与 Node 压缩包的第一顺位镜像同一家） */
export const DEFAULT_NPM_REGISTRY = 'https://registry.npmmirror.com';
/** 官方源（境外网络 / 需要与官方完全一致时用） */
export const OFFICIAL_NPM_REGISTRY = 'https://registry.npmjs.org';

export type RegistryCheck =
  | { ok: true; value: string }
  | { ok: false; error: string };

/**
 * 校验并归一化：空值 → 默认淘宝源；否则必须是带主机名的 http(s) 地址。
 *
 * 规则与后端一致：不含空白、不允许用户名/密码（凭据应配在 `~/.npmrc`）、
 * 去掉末尾斜杠。额外用 `new URL()` 做一次严格解析（端口、编码等）。
 */
export function normalizeNpmRegistry(raw: string | null | undefined): RegistryCheck {
  const trimmed = (raw ?? '').trim();
  if (!trimmed) return { ok: true, value: DEFAULT_NPM_REGISTRY };
  if (/\s/.test(trimmed)) return { ok: false, error: '地址不能包含空白字符' };

  const scheme = /^(https?):\/\//i.exec(trimmed);
  if (!scheme) return { ok: false, error: '地址必须以 http:// 或 https:// 开头' };
  // `new URL('https:///repo')` 会把 repo 当主机（浏览器行为），后端不接受这种写法，
  // 这里先按原始字符串挡掉，保持两边一致
  const rest = trimmed.slice(scheme[0].length);
  if (!rest || rest.startsWith('/')) return { ok: false, error: '地址缺少主机名' };

  let url: URL;
  try {
    url = new URL(trimmed);
  } catch {
    return { ok: false, error: '不是合法的 URL（检查端口或特殊字符）' };
  }
  if (url.protocol !== 'http:' && url.protocol !== 'https:') {
    return { ok: false, error: '只支持 http:// 或 https://' };
  }
  if (!url.hostname) return { ok: false, error: '地址缺少主机名' };
  if (url.username || url.password) {
    return { ok: false, error: '地址里不要写用户名/密码，认证请配置在 ~/.npmrc' };
  }
  return { ok: true, value: trimmed.replace(/\/+$/, '') };
}
