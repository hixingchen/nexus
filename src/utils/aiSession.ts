/**
 * AI 面板的打开方式（设置 > AI 运行时 > AI 会话）。
 *
 * - `new`（默认）：每次打开/重建面板都是新会话（空白对话页）
 * - `resume`：接上该项目最近一次真正聊过的会话（由后端扫 dsh 的 projection cache
 *   找出来，跳过空白会话与已归档会话）
 *
 * 存在 layout 的 KV 表里（见 `stores/layoutStore` 的 `aiSessionMode`），值只可能是
 * 这两个字符串；读不到/被写坏一律按默认 `new` 处理，不让一条脏值改变用户预期。
 */
export type AiSessionMode = 'new' | 'resume';

export const AI_SESSION_MODE_DEFAULT: AiSessionMode = 'new';

export function parseAiSessionMode(raw: string | null | undefined): AiSessionMode {
  return raw === 'resume' ? 'resume' : AI_SESSION_MODE_DEFAULT;
}
