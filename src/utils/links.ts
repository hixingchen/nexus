/**
 * 仓库相关的地址：**唯一来源**（换仓库时只改这一个文件）。
 *
 * 为什么单独成模块：此前它只是 `TitleBar.tsx` 里的一个常量，注释写着"关于与检查更新都从
 * 这里取"。而现在要用它的地方变成了三处——检查更新（GitHub release API）、关于对话框、
 * 使用文档（内嵌 README 里的**相对链接**要指回仓库网页）。散着写就是三个地方各存一份
 * 仓库名，"只改一处"这句话当场失效。
 */

/** 仓库主页（关于对话框 / 打开主页） */
export const REPO_URL = 'https://github.com/hixingchen/nexus';

/**
 * 最新 release：检查更新拿它比版本号。
 *
 * 注意 CSP：`tauri.conf.json` 的 `connect-src` 必须包含 `https://api.github.com`，
 * 否则这个 fetch 会被拦掉（表现是"检查更新失败"，而原因只在控制台里）。
 */
export const LATEST_RELEASE_API = 'https://api.github.com/repos/hixingchen/nexus/releases/latest';

/**
 * 新建 issue（帮助 > 报告问题**直接**打开它）。
 *
 * 为什么不做一个自己的"报告问题"弹窗（曾经做过，已删）：它当时要做的事是"显示诊断信息 +
 * 复制到剪贴板 + 再打开这个页面"，而那三件事里真正必须由应用做的一件都没有——多一层弹窗
 * 只是让用户多读一屏字、多点一次按钮。发现问题的人要的是**尽快把问题说出来**，
 * 路径越短越好，所以这里直接跳转（要贴日志的话，路径在 设置 > 通用 > 数据与日志 里能复制）。
 */
export const NEW_ISSUE_URL = `${REPO_URL}/issues/new`;

/** 默认分支名：内嵌 README 的相对链接要拼成网页地址，分支改名只影响这一处 */
const DEFAULT_BRANCH = 'main';

/**
 * 仓库内文件 → 它在 GitHub 上的网页地址。
 *
 * 为什么需要：使用文档是**内嵌在应用里的 README 原文**（见 `UsageDocsModal`），
 * 而 README 里的相对链接（如 `src-tauri/resources/cfr-LICENSE.txt`）在本机没有对应位置
 * ——应用里既没有仓库目录，相对路径也走不通文件白名单。把它们指回仓库网页，
 * 是"点得动"与"不越权"之间唯一同时成立的选项。
 */
export function repoBlobUrl(path: string): string {
  const clean = path.trim().replace(/^\.\//, '').replace(/^\/+/, '');
  return `${REPO_URL}/blob/${DEFAULT_BRANCH}/${clean}`;
}
