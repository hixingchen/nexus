import { create } from 'zustand';
import type { PortConflict } from '../services/service';

/**
 * 一次端口冲突的待决策请求。
 *
 * `actionLabel` 只用于按钮文案（「结束占用进程并启动服务 / 重启服务」）：
 * 弹框本身不执行启停，只负责让用户确认"要不要动占用者"，决定后 resolve 给调用方。
 */
export interface PortConflictRequest {
  serviceName: string;
  conflict: PortConflict;
  actionLabel?: string;
}

interface PortConflictState {
  /** 待决策的端口冲突；null = 弹框没打开 */
  pending: PortConflictRequest | null;
  /** 当前弹框的 resolver，由 `settle` 调用 */
  resolver: ((proceed: boolean) => void) | null;
  /** 打开弹框并等用户决定：true = 继续启动/重启，false = 取消 */
  ask: (request: PortConflictRequest) => Promise<boolean>;
  settle: (proceed: boolean) => void;
}

/**
 * 端口冲突确认弹框的状态。
 *
 * 为什么是 Promise：启动/重启的调用点是在**等**这个决定（等到了才继续），
 * 与 `pasteConflict` 同构。弹框在 `MainLayout` 挂一份——服务卡片、收起态圆点、
 * 启动前预检三处入口共用，各挂一份会叠出多层遮罩。
 */
export const usePortConflictStore = create<PortConflictState>((set, get) => ({
  pending: null,
  resolver: null,
  ask: (request) => new Promise((resolve) => {
    // 上一次弹框还挂着（用户连点了两个服务）：先按"取消"结掉，避免它的 await 永远悬着
    get().resolver?.(false);
    set({ pending: request, resolver: resolve });
  }),
  settle: (proceed) => {
    const resolve = get().resolver;
    // 先清状态再 resolve：调用方可能紧接着发起下一次 ask
    set({ pending: null, resolver: null });
    resolve?.(proceed);
  },
}));
