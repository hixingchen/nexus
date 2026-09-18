import { create } from 'zustand';
import { processApi, type FailedService, type RunningService } from '../services/service';

/**
 * 运行状态共享 store：单一 3 秒轮询，useProjectList / useProjectDetail 共同订阅，
 * 避免两个 hook 各自轮询 get_running 造成重复 IPC 和双重重渲染。
 */

const POLL_INTERVAL_MS = 3000;
let intervalId: ReturnType<typeof setInterval> | null = null;

/**
 * 状态代际号：每次"被采纳的写入"（轮询响应或动作后的主动刷新）都 +1。
 *
 * 为什么需要：轮询响应在途时，一次用户动作（启停服务）会走 `load()` → `setRunning` 拿到
 * 更新的数据；若随后到达的**更早发出的**轮询响应照样写入，界面会把刚启动的服务显示成未运行
 * （下一次轮询才纠正，中间这 3 秒用户会以为没启动成功）。规则：响应只在"发出后没有任何
 * 更新的写入"时才被采纳。
 */
let statusSeq = 0;

/**
 * 比对两组运行/失败列表是否等价（逐字段、按下标）。
 * 供 refresh 与 setRunning 共用：后端每次返回的是新数组，
 * 内容未变时照样 set 等于换引用，会把所有订阅方（项目行/服务面板/展开的目录树）全部重渲染。
 *
 * 仅供本 store 使用，故不导出（仅文件内使用的工具函数不对外暴露）。
 */
function sameStatus(a: RunningService[], b: RunningService[], af: FailedService[], bf: FailedService[]): boolean {
  return a.length === b.length
    && af.length === bf.length
    && a.every((x, i) => {
      const y = b[i];
      return y && x.service_id === y.service_id && x.project_id === y.project_id;
    })
    && af.every((x, i) => {
      const y = bf[i];
      return y && x.service_id === y.service_id && x.exit_code === y.exit_code && x.timestamp === y.timestamp;
    });
}

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

  setRunning: (running, failed) => {
    const st = get();
    // 动作后的主动刷新：数据一定比在途轮询新，占一个代际（让在途响应作废）
    statusSeq++;
    // 与 refresh 同一套变更检测：内容未变则不 set（loaded 未置位时必须 set，首次要把 loaded 翻成 true）
    if (st.loaded && sameStatus(st.running, running, st.failed, failed)) return;
    set({ running, failed, loaded: true });
  },

  /** 拉取一次最新状态（失败只记录日志：后端故障时每 3 秒弹 toast 会刷屏） */
  refresh: async () => {
    const reqSeq = statusSeq; // 请求发出时的代际
    try {
      const r = await processApi.getRunning();
      // 期间有更新的写入（动作结果 / 更晚的刷新）→ 丢弃这次过期响应
      if (statusSeq !== reqSeq) return;
      const st = get();
      // 变更检测：内容未变则不 set——避免每 3 秒产生新数组引用，
      // 触发所有订阅方（项目行/服务面板/展开的目录树）全量重渲染
      if (st.loaded && sameStatus(st.running, r.running, st.failed, r.failed)) return;
      statusSeq++;
      set({ running: r.running, failed: r.failed, loaded: true });
    } catch (e) {
      // 只留控制台：3 秒轮询的后台请求，失败时保留上一次状态即可（下次轮询自愈），
      // 弹 toast 会变成「后端一抖动就刷屏」
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
