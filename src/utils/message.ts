/**
 * 错误文案归一化（无任何 import，可被 Node 内置测试器直接跑）。
 *
 * 为什么单独成文件：`utils/error.ts` 依赖 UI 层（showNotification），把它整体放进测试
 * 需要 React/DOM 环境；而"任何抛出物 → 可读字符串"这条规则本身是纯逻辑，也是用户
 * 看到的所有错误文案的来源，值得被断言钉住。
 */

/** 把任意抛出物归一为可读文案（后端错误一律是字符串，前端可能是 Error/其它） */
export function toMessage(e: unknown): string {
  if (typeof e === 'string') return e;
  if (e instanceof Error) return e.message;
  return String(e);
}
