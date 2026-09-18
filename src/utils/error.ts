import { notify } from './notify';
import { toMessage } from './message';

// 归一化函数本体在 utils/message.ts（无依赖，便于纯逻辑测试）；
// 这里再导出一次，保持"错误相关工具都从 utils/error 取"的既有调用点不变。
// 本文件同样不含 React 依赖：通知走 utils/notify.ts 的端口（ARCH-17）。
export { toMessage };

interface ReportOptions {
  variant?: 'error' | 'warning' | 'info';
  /**
   * 覆盖通知标题（默认用 `op`）。
   *
   * 存在的理由：`op` 同时充当控制台前缀与界面标题，而两者要的粒度不同——
   * 控制台要"哪条订阅失败了"（便于排障），界面要"用户看得懂的一句话"。
   * 缺这个字段时，那几处只能整个重写 `reportError`（`stores/editor.ts` 的保存冲突提示
   * 就因此一直游离在单一出口之外）。
   */
  title?: string;
  /** 覆盖默认文案（用于把后端错误翻译成可行动的提示） */
  description?: string;
  /** 覆盖默认时长；错误类默认 8s、警告 6s（见 Toast 的按级默认） */
  duration?: number;
  /** 只进控制台不弹通知（后台轮询等用户未主动触发的场景） */
  silent?: boolean;
  /** 通知上的行动按钮（见 `notify` 的说明）：错误现场能直接给一步操作 */
  action?: { label: string; run: () => void };
}

/**
 * 统一错误上报出口：控制台留详细（含操作名与原因），界面给可读文案。
 *
 * 为什么要有单一出口：此前"`console.error('X失败:', e)` + `showNotification(...)`"成对写了
 * 39 处，改一次错误呈现（统一文案、统一时长、去重）要动 39 个地方；两种归一化写法
 * （`String(e)` 与 `typeof e === 'string' ? ...`）还会让同一种后端错误在不同入口显示不同文案。
 */
export function reportError(op: string, e: unknown, opts?: ReportOptions): void {
  console.error(`${op}:`, e);
  if (opts?.silent) return;
  notify({
    variant: opts?.variant ?? 'error',
    title: opts?.title ?? op,
    description: opts?.description ?? toMessage(e),
    ...(opts?.duration !== undefined ? { duration: opts.duration } : {}),
    ...(opts?.action ? { action: opts.action } : {}),
  });
}
