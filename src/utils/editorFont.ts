/**
 * 文本字号（用户设置）的**纯逻辑**：区间、默认值与解析。
 *
 * 它管的是"读文本的那几个面"，各自带一个**改动前就有的偏移量**（保留它们，没动过设置的
 * 用户看到的界面与以前完全一致）：
 * - 代码视图（CodeMirror）：正好是设置值（默认 14）
 * - Markdown 预览（`.md-preview`：编辑器里的预览 + 帮助 > 使用文档）：−1px
 * - 日志面板（`LogViewer`）：−1px；它的虚拟滚动行高估算与实测缓存也跟着走
 *
 * 为什么放在 `utils/` 而不是 `layoutStore`：本模块零依赖，能被 node 直接跑
 * （见同目录的 `__tests__/editorFont.test.ts`），而 `stores/layoutStore.ts` 顶层
 * import 了 `services/service`（进而 `@tauri-apps/api/core`），在测试进程里加载不起来。
 * 键名仍登记在 `layoutStore` 的 `LAYOUT_KEYS`（那是键的唯一注册表），这里只管"怎么解析"。
 */

/** 字号下限（px）：再小就看不清，而这个设置的用途是"看着舒服" */
export const EDITOR_FONT_SIZE_MIN = 12;
/** 字号上限（px）：再大一行就剩几个字符，代码反而更难读 */
export const EDITOR_FONT_SIZE_MAX = 20;
/** 默认字号（px）：与改动前的写死值一致，因此没设过的用户看到的界面不变 */
export const EDITOR_FONT_SIZE_DEFAULT = 14;

/**
 * 代码视图的字体族（编辑器与设置面板里的预览**共用这一份**）。
 *
 * 这个栈此前只写在 `CodeViewer` 的 CodeMirror 主题里；预览要"看起来就是编辑器里那样"
 * 就得用同一个栈，复制一份出来两边迟早会不一致（预览说 16px 好看，回到编辑器却是另一种字）。
 * 注意 `index.css` 里 `.cm-scroller` 另有一份带 `'Cascadia Code'` 的旧栈——那份被本栈覆盖
 * （`.cm-content` 更内层），保留原样不动，避免顺手改掉一次既有的字体渲染。
 */
export const CODE_FONT_STACK = "'JetBrains Mono', 'Fira Code', Consolas, monospace";

/**
 * 把布局库里存的字符串解析成一个可用字号。
 *
 * 为什么要夹紧而不是直接 `Number()`：布局库是**手改得动**的（用户能改 DB，旧版本或将来
 * 的版本也可能写进别的值），而 `0`、`1e9`、`"abc"` 的后果分别是"编辑器看不见"、
 * "一行一个字"和 `NaN` 字号（浏览器直接丢弃该声明，退回继承值——症状最难查的一种）。
 *
 * 空值为什么单独判：`Number('')` 是 **0** 而不是 `NaN`，少了这一步，空串会被夹成下限 12
 * ——于是"从没设置过"会被显示成"用户选了最小号"，而这个差别在界面上看不出来。
 */
export function parseEditorFontSize(raw: string | null | undefined): number {
  if (raw == null || raw.trim() === '') return EDITOR_FONT_SIZE_DEFAULT;
  const n = Number(raw);
  if (!Number.isFinite(n)) return EDITOR_FONT_SIZE_DEFAULT;
  return Math.min(EDITOR_FONT_SIZE_MAX, Math.max(EDITOR_FONT_SIZE_MIN, Math.round(n)));
}
