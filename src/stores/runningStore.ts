import { create } from 'zustand';
import { processApi, type FailedService, type RunningService } from '../services/service';

/**
 * 运行状态共享 store：单一 3 秒轮询，useProjectList / useProjectDetail 共同订阅，
 * 避免两个 hook 各自轮询 get_running 造成重复 IPC 和双重重渲染。
 */

const POLL_INTERVAL_MS = 3000;
let intervalId: ReturnType<typeof setInterval> | null = null;

interface RunningStore {
  /** 运行中的服务（含所属项目） */
  running: RunningService[];
  /** 意外退出的服务（崩溃/秒退）：卡片显示失败按钮，日志保留可查看 */
  failed: FailedService[];
  /** 是否已成功拉取过（供下游区分"空列表"与"尚未加载"） */
  loaded: boolean;
  setRunning: (running: RunningService[], failed: FailedService[]) => void;
  refresh: () => Promise<void>;
  startPolling: () => void;
  stopPolling: () => void;
}

export const useRunningStore = create<RunningStore>((set, get) => ({
  running: [],
  failed: [],
  loaded: false,

  setRunning: (running, failed) => set({ running, failed, loaded: true }),

  /** 拉取一次最新状态（失败只记录日志：后端故障时每 3 秒弹 toast 会刷屏） */
  refresh: async () => {
    try {
      const r = await processApi.getRunning();
      const st = get();
      // 变更检测：内容未变则不 set——避免每 3 秒产生新数组引用，
      // 触发所有订阅方（项目行/服务面板/展开的目录树）全量重渲染
      const same = st.loaded
        && st.running.length === r.running.length
        && st.failed.length === r.failed.length
        && st.running.every((x, i) => {
          const y = r.running[i];
          return y && x.service_id === y.service_id && x.project_id === y.project_id;
        })
        && st.failed.every((x, i) => {
          const y = r.failed[i];
          return y && x.service_id === y.service_id && x.exit_code === y.exit_code && x.timestamp === y.timestamp;
        });
      if (same) return;
      set({ running: r.running, failed: r.failed, loaded: true });
    } catch (e) {
      console.error('获取运行状态失败:', e);
    }
  },

  /** 启动轮询（幂等：MainLayout 挂载时调用一次） */
  startPolling: () => {
    if (intervalId !== null) return;
    void get().refresh();
    intervalId = setInterval(() => { void get().refresh(); }, POLL_INTERVAL_MS);
  },

  stopPolling: () => {
    if (intervalId !== null) {
      clearInterval(intervalId);
      intervalId = null;
    }
  },
}));
