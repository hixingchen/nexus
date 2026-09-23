import { create } from 'zustand';
import type { ConflictPolicy, PasteResponse } from '../utils/pasteResult';

/**
 * 待决策的一次冲突 = 探测那一轮的**完整回包**（含源清单）。
 *
 * 为什么整包传下去而不是只取 `conflicts`：`sources` 是决策后必须原样带回后端的东西
 * （它拿这个跟当前剪贴板比对），跟 `conflicts` 拆开就会出现"弹框拿到了名单、回传时
 * 忘了源清单"这种只在第二个入口才犯的错。
 */
type PendingConflict = Extract<PasteResponse, { status: 'conflict' }>;

interface PasteConflictState {
  /** 待决策的一次冲突；null = 弹框没打开 */
  pending: PendingConflict | null;
  /** 挂在当前弹框上的 resolver，由 `settle` 调用 */
  resolver: ((policy: ConflictPolicy | null) => void) | null;
  ask: (pending: PendingConflict) => Promise<ConflictPolicy | null>;
  /** `policy` 为 null = 取消（一个都不复制） */
  settle: (policy: ConflictPolicy | null) => void;
}

/**
 * 粘贴同名冲突弹框的状态。
 *
 * 为什么放 store 而不是各调用点自己持一份 state：粘贴有三个入口（目录行、文件行、空白区到
 * 项目根），而 `pasteInto` 是个**普通函数不是组件**——状态放这里，它就能 `await ask(...)`
 * 拿到用户的决定，三个入口一行都不用改。弹框本身在 `MainLayout` 挂一份（放 FileTree 里
 * 会随"项目树 + 每个服务树"各挂一个，同时渲染出好几层遮罩）。
 *
 * 为什么是 Promise 而不是回调：调用方是在**等**这个决定（等到了才继续粘），
 * 这正好是 Promise 的形状。
 */
export const usePasteConflictStore = create<PasteConflictState>((set, get) => ({
  pending: null,
  resolver: null,
  ask: (pending) => new Promise((resolve) => {
    // 上一次的弹框还挂着（用户连点了两处粘贴）：先把它按"取消"结掉。
    // 不结的话那个入口的 `await` 永远悬着，它的"正在粘贴…"再也退不掉
    get().resolver?.(null);
    set({ pending, resolver: resolve });
  }),
  settle: (policy) => {
    const resolve = get().resolver;
    // 先清状态再 resolve：调用方可能紧接着发起下一次 ask（比如后端回"剪贴板已改变"
    // 后重新探测），那时 pending 必须已经是空的
    set({ pending: null, resolver: null });
    resolve?.(policy);
  },
}));
