import { notify } from './notify';
import { reportError } from './error';

/**
 * 写入系统剪贴板，成功/失败统一提示。
 *
 * 剪贴板受权限门控，失败必须让用户知道（原实现只写 console，用户点了"复制"却什么都没发生）。
 *
 * label 的两种文案是既有口径：「路径」→「路径已复制」/「复制路径失败」；
 * 「文件名」→「文件名已复制」/「复制文件名失败」（FileTree 与 ProjectListItem 原本
 * 各写一遍，文案已逐字一致，这里保持逐字不变）。
 *
 * **不要为 Toast 的「复制错误详情」加 `label=null` 这类静默分支**：那会让本模块反向
 * import `components/ui/Toast`，与 `Toast → clipboard` 形成循环依赖（构建期 Vite 会警告，
 * 运行期的绑定可能是 undefined）。那个静默变体已内联在 Toast 组件里——它本来就是 UI 行为。
 */
export async function copyText(text: string, label: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
  } catch (e) {
    reportError(`复制${label}失败`, e);
    return;
  }
  notify({ variant: 'success', title: `${label}已复制` });
}
