import { create } from 'zustand';
import { processApi, type FailedService, type RunningService } from '../services/service';
import type { StopSignal } from '../utils/logPanel';
import { sameRunningStatus } from '../utils/runningStatus';

/**
 * 运行状态共享 store：单一 3 秒轮询，useProjectList / useProjectDetail 共同订阅，
 * 避免两个 hook 各自轮询 get_running 造成重复 IPC 和双重重渲染。
 */

const POLL_INTERVAL_MS = 3000;
let intervalId: ReturnType<typeof setInterval> | null = null;

/**
 * 是否已有一个 refresh 在飞。**只用于轮询的在途保护**（PERF-20）。
 *
 * 为什么需要：`get_running` 是后端最重的命令——它要 `try_wait` 每个服务、跑一遍
 * `adopt_survivors` 的全系统进程快照、对每个未认领的服务扫 7 个日志目录；停尸路径
 * 最坏单服务 ~5s。3 秒一次无条件再发，会让多个阻塞任务在 tokio 线程池里排队。
 * 跳过这一拍没有正确性代价：数据下一次就回来（不拦**用户动作**触发的 refresh，
 * 那条必须立刻可见）。
 */
let refreshInFlight = false;

/**
 * 最近一次**成功**拿到后端状态的时刻（模块级，不触发渲染）。
 *
 * 用途：自动重启的观察窗里，多个服务可能同时每 1.5 秒探一次；每次都强制 `refresh()`
 * 会让最重的 `get_running`（全系统进程快照 + TCP 表）被打成每秒好几次。调用方用
 * `refreshIfStale` 共享同一份新鲜数据，最多每 ~1 秒真查一次。
 */
let lastFetchAt = 0;

/**
 * 状态代际号：每次"被采纳的写入"（轮询响应或动作后的主动刷新）都 +1。
 *
 * 为什么需要：轮询响应在途时，一次用户动作（启停服务）会走 `refresh()` 拿到
 * 更新的数据；若随后到达的**更早发出的**轮询响应照样写入，界面会把刚启动的服务显示成未运行
 * （下一次轮询才纠正，中间这 3 秒用户会以为没启动成功）。规则：响应只在"发出后没有任何
 * 更新的写入"时才被采纳。
 */
let statusSeq = 0;

interface RunningStore {
  /** 运行中的服务（含所属项目） */
  running: RunningService[];
  /** 意外退出的服务（崩溃/秒退）：卡片显示失败按钮，日志保留可查看 */
  failed: FailedService[];
  /** 是否已成功拉取过（供下游区分"空列表"与"尚未加载"） */
  loaded: boolean;
  /**
   * **用户显式停止**的信号（`seq` 单调递增，`serviceIds` 是这次确实停掉的服务）。
   *
   * 为什么不能拿"running/failed 里暂时没有它"当"已被停止"：自动重启与手动重启都是
   * 后端 `stop → start`，中间有一段服务既不在 running 也不在 failed 的空窗；按运行态
   * 差值判断会把日志面板当开关拉，高频挂载/卸载还会让 DOM 与 React 状态脱节
   * （日志冻结、✕ 点不动、新日志被挤到最下方）。只有显式停止动作才发这个信号。
   */
  stopSignal: StopSignal;
  /** 记录一次显式停止（由 `stores/serviceActions.ts` 的停止动作调用） */
  markStopped: (serviceIds: string[]) => void;
  refresh: () => Promise<void>;
  /**
   * 仅当现有数据早于 `maxAgeMs` 时才真的查一次。
   *
   * 自动重启的观察窗会高频探测（每个在重启的服务每 1.5 秒一次）；共享同一份
   * `get_running` 结果，避免把最重的命令打成每秒多次。
   */
  refreshIfStale: (maxAgeMs: number) => Promise<void>;
  startPolling: () => void;
  stopPolling: () => void;
}

export const useRunningStore = create<RunningStore>((set, get) => ({
  running: [],
  failed: [],
  loaded: false,
  stopSignal: { seq: 0, serviceIds: [] },

  markStopped: (serviceIds) =>
    set((s) => ({ stopSignal: { seq: s.stopSignal.seq + 1, serviceIds } })),

  /** 拉取一次最新状态（失败只记录日志：后端故障时每 3 秒弹 toast 会刷屏） */
  refresh: async () => {
    const reqSeq = statusSeq; // 请求发出时的代际
    refreshInFlight = true;
    try {
      const r = await processApi.getRunning();
      // 请求已返回就记"数据新鲜"——即使下面因为代际更晚而丢弃这次响应，
      // 后端那一份状态本身就是刚查到的
      lastFetchAt = Date.now();
      // 期间有更新的写入（动作结果 / 更晚的刷新）→ 丢弃这次过期响应
      if (statusSeq !== reqSeq) return;
      const st = get();
      // 变更检测：内容未变则不 set——避免每 3 秒产生新数组引用，
      // 触发所有订阅方（项目行/服务面板/展开的目录树）全量重渲染
      if (st.loaded && sameRunningStatus(st.running, r.running, st.failed, r.failed)) return;
      statusSeq++;
      set({ running: r.running, failed: r.failed, loaded: true });
    } catch (e) {
      // 只留控制台：3 秒轮询的后台请求，失败时保留上一次状态即可（下次轮询自愈），
      // 弹 toast 会变成「后端一抖动就刷屏」
      console.error('获取运行状态失败:', e);
    } finally {
      refreshInFlight = false;
    }
  },

  refreshIfStale: async (maxAgeMs) => {
    if (Date.now() - lastFetchAt < maxAgeMs) return;
    await get().refresh();
  },

  /** 启动轮询（幂等：MainLayout 挂载时调用一次） */
  startPolling: () => {
    if (intervalId !== null) return;
    void get().refresh();
    intervalId = setInterval(() => {
      // 上一拍还没回来就跳过（PERF-20）：后端那三个阶段是重活，堆起来只会互相拖慢
      if (refreshInFlight) return;
      void get().refresh();
    }, POLL_INTERVAL_MS);
  },

  stopPolling: () => {
    if (intervalId !== null) {
      clearInterval(intervalId);
      intervalId = null;
    }
  },
}));
