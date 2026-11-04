/**
 * 服务卡片的「打开」目标解析（纯函数）。
 *
 * 规则（见 docs 里最终确认的方案）：
 * - 未启用 → 不显示打开按钮；
 * - 指定端口正在监听 → 打开指定端口；
 * - 指定端口未监听、但服务只有一个监听端口 → 安全回退到该端口（端口漂移 3000→3001）；
 * - 有多个监听端口且无法唯一确定 → 不替用户猜，返回 `choose` 让用户选；
 * - 一个端口都没有 → `none`（按钮禁用）。
 *
 * **回退只使用该服务进程树自己的监听端口**，因此绝不会打开到占用原端口的其他进程。
 */

export type OpenPortResolution =
  | { kind: 'disabled' }
  | { kind: 'none' }
  | { kind: 'open'; port: number }
  | { kind: 'choose'; ports: number[] };

export function resolveOpenPort(
  config: { enabled?: boolean; specifiedPort?: number },
  listeningPorts: readonly number[] | undefined,
): OpenPortResolution {
  if (!config.enabled) return { kind: 'disabled' };

  const ports = [...new Set(listeningPorts ?? [])].sort((a, b) => a - b);
  if (ports.length === 0) return { kind: 'none' };

  const specified = config.specifiedPort ?? 0;
  if (specified > 0 && ports.includes(specified)) {
    return { kind: 'open', port: specified };
  }
  if (ports.length === 1) {
    return { kind: 'open', port: ports[0] };
  }
  return { kind: 'choose', ports };
}
