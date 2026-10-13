/**
 * 文件变更触发的**自动重启**：尾沿去抖 + 失败重试。
 *
 * ## 它修的是什么
 *
 * 原实现（`RestartConfirm` 里那段）是两个问题的叠加：
 *
 * 1. **冷却期内的事件被丢弃**：`if (cooldown.has(sid)) continue` —— 2 秒窗口里到达的变更
 *    既不排队也不重新计时。于是 AI 连续改代码时，服务跑的往往不是最新代码；更糟的是
 *    **最后一次变更可能正好落在窗口里被吞掉**，之后不再有新事件，服务就停在那个旧状态上。
 * 2. **重启完就撒手**：`processApi.restart()` 是发了不管，而它返回 `Ok` 只代表 spawn 成功。
 *    服务秒退由后端 2 秒后的心跳发现，那时前端早把这次重启忘了——**没有任何重试**。
 *
 * 两条叠起来就是你遇到的现象：自动重启撞在"文件写到一半 / 旧进程还没释放端口"上 →
 * 服务秒退 → 再也没人管 → 手动点一下就好了（那时文件写完了、端口也空了）。
 *
 * ## 这一版的规则
 *
 * - 同一服务的变更**合并**为一轮（`change` 在轮内只置一个待办标记，不丢弃）；
 * - 一轮 = 重启 → 等节流窗口（`throttleMs`）→
 *   - 期间又有变更 → **立刻再起一轮**（尾沿：代码又变了，这一轮的结果已经过时），
 *     这不算"重试"，所以不消耗重试次数；
 *   - 期间没变更 → 再等到观察窗口结束（`verifyMs`）看结果；
 * - 判定为 **`failed` 才重试**（最多 `maxAttempts`，退避 `backoffMs`）。
 *   `unknown`（还在启动 / 状态未知）**一律不动它**——重试会把"启动慢但健康"的服务
 *   越搞越糟，那比不重试更坏；
 * - 重试用尽仍然失败才通知用户（`onGiveUp`）。中间的重试是静默的：改动代码时每两秒
 *   弹一条"正在重试"只会淹没日志面板。
 *
 * ## 为什么依赖全部注入
 *
 * `restart` / `probe` / `sleep` 都是参数：真实实现要 IPC、要 store、要真的等几秒，
 * 而"尾沿有没有补上""失败重试了几次""unknown 有没有被误重试"这些判断恰恰是这段代码
 * 全部的价值所在——注进去之后它们可以在毫秒内被逐条钉住（见 `__tests__/autoRestart.test.ts`）。
 * 这与 `core/process.rs` 把运行表/句柄/发送端变成参数是同一套路子。
 */

/** 服务在一次探查里的状态（`unknown` = 还没起来也没失败：仍在启动中，或状态未知） */
export type ProbeOutcome = 'running' | 'failed' | 'unknown';

export interface AutoRestartDeps {
  /** 重启服务；抛错 = 这次重启**根本没发出去**（spawn 失败等） */
  restart: (serviceId: string) => Promise<void>;
  /**
   * 探查服务状态。实现方必须拿**权威数据**判定（不能拿几秒前的缓存），
   * 因为 `unknown` 与 `failed` 的处理完全相反。
   */
  probe: (serviceId: string) => Promise<ProbeOutcome>;
  /** 等待（注入以便测试用假时钟） */
  sleep: (ms: number) => Promise<void>;
  /** 重试用尽仍然失败：这是唯一会打扰用户的时刻 */
  onGiveUp: (info: { serviceId: string; serviceName: string; attempts: number }) => void;
  /** 重启命令本身报错（IPC/数据库/spawn 失败） */
  onError: (serviceId: string, error: unknown) => void;

  /** 节流窗口（ms）：期内到达的变更只记为待办，不再起新轮 */
  throttleMs?: number;
  /** 观察窗口（ms，自重启起算）：到这里才判定成败 —— 要给服务留出"起来或秒退"的时间 */
  verifyMs?: number;
  /** 一轮里最多重启几次（含首次） */
  maxAttempts?: number;
  /** 第 n 次重试前等多久（ms），越界取最后一项 */
  backoffMs?: number[];
}

export interface AutoRestarter {
  /** 收到一次文件变更 */
  change: (serviceId: string, serviceName: string) => void;
  /** 正在进行的轮数（仅测试与自查用） */
  activeCount: () => number;
}

/** 节流窗口：与原来的 2 秒冷却同一数值（改动前就是这个节奏，这次只改"事件不再被丢弃"） */
const DEFAULT_THROTTLE_MS = 2000;
/**
 * 观察窗口：自重启起算 7 秒。
 *
 * 为什么是 7 而不是 2：秒退的服务要等后端心跳（2 秒一轮）把它写进失败表，
 * 服务本身启动也要时间——2 秒就开始判定会把"正在启动"误判成"没起来"。
 */
const DEFAULT_VERIFY_MS = 7000;
/** 一轮里最多重启 3 次（首次 + 2 次重试）：再多就不像"重试"而像"卡在重启循环里" */
const DEFAULT_MAX_ATTEMPTS = 3;
/** 退避：2 秒 → 5 秒（第三次不给——尝试次数只有 3 次，第 2 次重试前等到 5 秒已足够让端口冷却） */
const DEFAULT_BACKOFF_MS = [2000, 5000];

export function createAutoRestarter(deps: AutoRestartDeps): AutoRestarter {
  const { restart, probe, sleep, onGiveUp, onError } = deps;
  const throttleMs = deps.throttleMs ?? DEFAULT_THROTTLE_MS;
  const verifyMs = Math.max(deps.verifyMs ?? DEFAULT_VERIFY_MS, throttleMs);
  const maxAttempts = Math.max(1, deps.maxAttempts ?? DEFAULT_MAX_ATTEMPTS);
  const backoffMs = deps.backoffMs ?? DEFAULT_BACKOFF_MS;

  /** 每个服务当前那一轮的状态；**key 存在 = 有轮在跑**（这就是互斥的判据） */
  const cycles = new Map<string, { name: string; pending: boolean }>();

  async function run(serviceId: string, name: string): Promise<void> {
    const state = { name, pending: false };
    cycles.set(serviceId, state);

    let attempts = 0;
    try {
      for (;;) {
        attempts += 1;
        try {
          await restart(serviceId);
        } catch (e) {
          // 重启命令都没发出去：重试同一个命令大概率还是失败（配置错/服务被删），
          // 交给用户判断，不再自动重试
          onError(serviceId, e);
          return;
        }

        // 节流窗口：期间到达的变更只置待办，不另起一轮
        await sleep(throttleMs);
        if (state.pending) {
          state.pending = false;
          // 代码又变了 → 这一轮的结果已经过时。**不消耗重试次数**：
          // 这是"变更仍在继续"，不是"重启失败了"
          attempts = 0;
          continue;
        }

        // 变更停了：再等它把结果显出来（起不来就会进失败表）
        if (verifyMs > throttleMs) await sleep(verifyMs - throttleMs);
        let outcome: ProbeOutcome = 'unknown';
        try {
          outcome = await probe(serviceId);
        } catch (e) {
          // 探查本身失败（IPC 抖动等）→ 按 `unknown` 处理，**不重试**：
          // 判据缺失时最安全的动作是什么都不做，而不是去重启一个可能好好的服务
          onError(serviceId, e);
        }

        // running / unknown 都不再动它（unknown 见文件头：重试会毁掉"启动慢但健康"的服务）
        if (outcome !== 'failed') return;

        if (attempts >= maxAttempts) {
          onGiveUp({ serviceId, serviceName: name, attempts });
          return;
        }
        await sleep(backoffMs[Math.min(attempts - 1, backoffMs.length - 1)] ?? 0);
      }
    } finally {
      // 收尾时刻又来了变更（最后一觉期间）：再起一轮，否则那次变更的代码就永远没被加载
      const hadPending = state.pending;
      cycles.delete(serviceId);
      if (hadPending) void run(serviceId, name);
    }
  }

  return {
    change: (serviceId, serviceName) => {
      const running = cycles.get(serviceId);
      if (running) {
        running.pending = true;
        return;
      }
      void run(serviceId, serviceName);
    },
    activeCount: () => cycles.size,
  };
}
