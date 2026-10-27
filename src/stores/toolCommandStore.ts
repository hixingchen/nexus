import { create } from 'zustand';
import type { ToolCommandResult } from '../services/service';
import { useToolLogStore } from './toolLogStore';

/**
 * 工具命令运行状态（全局）。
 *
 * 为什么放在 store 而不是 `useToolCommandRunner` 的 useState：
 * ① 弹窗可以被关掉而命令继续在后台跑——那份状态必须活得比弹窗和 ProjectDetail 久，
 *    否则切项目/换面板就丢，用户再也找不回正在跑的命令；
 * ② 同一时间可能有多条命令（关掉 A 去跑 B），单槽 state 会让先完成的那条**覆盖**
 *    后来者正在展示的结果；
 * ③ 重跑同一条命令需要能查到"它是不是还在跑"（见 runningByCommand），避免并行
 *    两份 `npm install` / `mvn` 互相踩文件。
 */
export interface ToolRun {
  runId: string;
  serviceId: string;
  commandId: string;
  commandName: string;
  loading: boolean;
  /**
   * 用户主动停止过（「停止命令」或「停止并关闭」）。
   *
   * 后台完成时据此**不弹**"执行完成/失败"通知：停止是他自己的动作，再提示一遍只是噪音；
   * 若此时弹窗已关，还会顺便把这次 run 静默清掉。
   */
  stopping: boolean;
  result: ToolCommandResult | null;
  error: string | null;
}

export interface BeginToolRun {
  runId: string;
  serviceId: string;
  commandId: string;
  commandName: string;
}

interface ToolCommandState {
  /** run_id → 运行（含已结束但可能还需要展示输出的后台运行） */
  runs: Record<string, ToolRun>;
  /** 当前弹窗展示的 run（null = 弹窗关闭） */
  openRunId: string | null;
  begin: (run: BeginToolRun) => void;
  finish: (runId: string, result: ToolCommandResult) => void;
  fail: (runId: string, error: string) => void;
  markStopping: (runId: string, stopping: boolean) => void;
  /** 打开指定 run 的弹窗（后台运行中的指示器 / "查看输出" 通知都走它） */
  open: (runId: string) => void;
  /**
   * 关闭弹窗。**不停止命令**：运行中的 run 保留在 runs 里（后台继续 + 指示器可再打开），
   * 已结束的 run 则连同输出一起丢掉（它已经没有再展示的入口了）。
   */
  closeDialog: () => void;
  remove: (runId: string) => void;
  /** 同一服务的同一条命令是否已在运行（防止重复执行） */
  runningByCommand: (serviceId: string, commandId: string) => ToolRun | undefined;
}

export const useToolCommandStore = create<ToolCommandState>((set, get) => ({
  runs: {},
  openRunId: null,

  begin: ({ runId, serviceId, commandId, commandName }) => {
    const prev = get().runs;
    const runs: Record<string, ToolRun> = {};
    for (const [id, r] of Object.entries(prev)) {
      if (r.loading) {
        runs[id] = r;
      } else {
        // 已结束的后台结果没有「再看一眼」的入口了（新命令会顶掉旧通知），释放输出
        useToolLogStore.getState().drop(id);
      }
    }
    runs[runId] = {
      runId, serviceId, commandId, commandName,
      loading: true, stopping: false, result: null, error: null,
    };
    set({ runs, openRunId: runId });
  },

  finish: (runId, result) => {
    const run = get().runs[runId];
    if (!run) return;
    // 用户主动停止后已关窗：没有展示对象，静默收拾，不再打扰
    if (get().openRunId !== runId && run.stopping) {
      get().remove(runId);
      return;
    }
    set(s => ({ runs: { ...s.runs, [runId]: { ...run, loading: false, result, error: null } } }));
  },

  fail: (runId, error) => {
    const run = get().runs[runId];
    if (!run) return;
    if (get().openRunId !== runId && run.stopping) {
      get().remove(runId);
      return;
    }
    set(s => ({ runs: { ...s.runs, [runId]: { ...run, loading: false, error } } }));
  },

  markStopping: (runId, stopping) => {
    const run = get().runs[runId];
    if (!run) return;
    set(s => ({ runs: { ...s.runs, [runId]: { ...run, stopping } } }));
  },

  open: (runId) => {
    if (!get().runs[runId]) return;
    set({ openRunId: runId });
  },

  closeDialog: () => {
    const runId = get().openRunId;
    if (!runId) return;
    const run = get().runs[runId];
    set({ openRunId: null });
    if (run && !run.loading) get().remove(runId);
  },

  remove: (runId) => {
    useToolLogStore.getState().drop(runId);
    set(s => {
      if (!(runId in s.runs)) return s;
      const runs = { ...s.runs };
      delete runs[runId];
      return { runs };
    });
  },

  runningByCommand: (serviceId, commandId) =>
    Object.values(get().runs).find(
      r => r.loading && r.serviceId === serviceId && r.commandId === commandId,
    ),
}));
