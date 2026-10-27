import { useEffect } from 'react';
import { open as openUrl } from '@tauri-apps/plugin-shell';
import { getAppVersion } from '../services/system';
import { LAYOUT_KEYS, saveLayout, useLayoutStore } from '../stores/layoutStore';
import { notify } from '../utils/notify';
import { reportError } from '../utils/error';
import { decideUpdateNotice } from '../utils/updateNotice';
import { LATEST_RELEASE_API, REPO_URL } from '../utils/links';

/**
 * 启动后**静默**检查一次新版本（一次会话一次，失败不打扰）。
 *
 * 为什么不做成弹窗、也不在每次启动都提示同一个版本：这是开发者工具，更新提示应当
 * 可忽略、不打断。提示里给「下载更新」与「跳过此版本」两个动作；跳过按版本号记录，
 * 下一个版本发布时仍会提示（手动「检查更新」永远显示结果，不受跳过影响）。
 *
 * 为什么没有 cleanup：本 hook 挂在 App 上、生命周期等于整个页面。StrictMode 在开发期
 * 会双跑 effect，如果在 cleanup 里 abort，第一轮请求被取消、第二轮又被模块级 `started`
 * 挡住，结果**开发环境永远检查不到更新**。fire-and-forget 最稳。
 */
let started = false;

export function useUpdateNotice(): void {
  useEffect(() => {
    if (started) return;
    started = true;

    const controller = new AbortController();
    // 启动检查是"顺带看一眼"：网络不通时不能让请求悬到用户关窗口
    const timer = setTimeout(() => controller.abort(), 10_000);

    void (async () => {
      try {
        // 版本号问后端要（二进制自己的版本）；跳过记录在 layout KV 里
        const [current, layout] = await Promise.all([
          getAppVersion(),
          useLayoutStore.getState().ensureLoaded(),
        ]);
        const res = await fetch(LATEST_RELEASE_API, {
          headers: { Accept: 'application/vnd.github+json' },
          signal: controller.signal,
        });
        if (!res.ok) throw new Error(`GitHub 返回 ${res.status}`);
        const data = (await res.json()) as { tag_name?: string; html_url?: string };
        const latest = (data.tag_name ?? '').trim();
        const decision = decideUpdateNotice(latest, current, layout[LAYOUT_KEYS.updateSkippedVersion]);

        if (decision === 'unknown') {
          // 解析失败不能当成"已是最新"，但自动检查不该为它弹提示——只留一行控制台
          console.warn('自动检查更新：版本号无法比较', { latest, current });
          return;
        }
        if (decision !== 'notify') return;

        notify({
          variant: 'info',
          title: `Nexus 有新版本 ${latest}`,
          description: `当前 ${current}。可跳过这一版，后续版本仍会提示。`,
          duration: 15_000,
          actions: [
            {
              label: '下载更新',
              run: () => {
                void openUrl(data.html_url || `${REPO_URL}/releases`)
                  .catch((e) => reportError('打开下载页失败', e));
              },
            },
            {
              label: '跳过此版本',
              variant: 'ghost',
              run: () => {
                saveLayout({ [LAYOUT_KEYS.updateSkippedVersion]: latest });
                notify({
                  title: `已跳过 ${latest}`,
                  description: '之后的版本仍会提示；手动「检查更新」不受影响',
                  duration: 3500,
                });
              },
            },
          ],
        });
      } catch (e) {
        // 离线 / GitHub 限流 / 请求被 aborted：自动检查失败不影响使用，只留控制台线索
        console.warn('自动检查更新失败（不影响使用）:', e);
      } finally {
        clearTimeout(timer);
      }
    })();
  }, []);
}
