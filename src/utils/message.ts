/**
 * 错误文案归一化（无任何 import，可被 Node 内置测试器直接跑）。
 *
 * 为什么单独成文件：它是"任何抛出物 → 可读字符串"这条纯规则，也是用户看到的所有错误
 * 文案的来源，值得被断言钉住。原本另一条理由是 `utils/error.ts` 依赖 UI 层
 * （直接 `showNotification`）导致整体进不了 node 测试——该依赖已由 `utils/notify.ts`
 * 的端口反转去掉，`utils/error.ts` 现在同样可测（见 ARCH-17）。
 */

/** 把任意抛出物归一为可读文案（后端错误一律是字符串，前端可能是 Error/其它） */
export function toMessage(e: unknown): string {
  if (typeof e === 'string') return e;
  if (e instanceof Error) return e.message;
  return String(e);
}
