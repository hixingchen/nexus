import { compareVersions } from './version';

/**
 * 启动时自动检查更新的判定结果。
 *
 * - `notify`：确实有新版本，且用户没有跳过它 → 提示
 * - `skipped`：有新版本，但用户对**这一版**点过"跳过" → 静默
 * - `none`：当前已是最新（或开发版比线上还新）→ 静默
 * - `unknown`：版本号解析不了 → 不能当成"已是最新"，但自动检查也不打扰用户
 */
export type UpdateNoticeDecision = 'notify' | 'skipped' | 'none' | 'unknown';

/** 去掉前导 `v`：GitHub 的 tag 是 `v1.9.1`，本机版本是 `1.9.1` */
function normalizeTag(raw: string): string {
  return raw.trim().replace(/^[vV]/, '');
}

/**
 * 判断这次自动检查要不要提示。
 *
 * "跳过此版本"按**版本号精确匹配**：跳过了 v1.9.1，v1.10.0 发布时仍会提示——
 * 否则一次跳过等于永久静默。手动「检查更新」不走这里（它永远显示结果）。
 */
export function decideUpdateNotice(
  latestTag: string,
  currentVersion: string,
  skippedVersion: string | null | undefined,
): UpdateNoticeDecision {
  const cmp = compareVersions(latestTag, currentVersion);
  if (cmp === null) return 'unknown';
  if (cmp <= 0) return 'none';
  if (skippedVersion && normalizeTag(skippedVersion) === normalizeTag(latestTag)) return 'skipped';
  return 'notify';
}
