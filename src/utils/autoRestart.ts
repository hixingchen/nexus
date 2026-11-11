/**
 * 文件变更触发的**自动重启**：尾沿去抖 + 失败重试。
 *
 * ## 与后端「崩溃自动重启」的分工
 *
 * 本状态机只管**文件变更**引发的重启——只有它知道"代码还在变"，所以尾沿去抖、
 * 观察窗与有限重试都在这里。进程**稳定运行后自己退出**由后端监督器负责
 * （`core/restart_policy` + `commands::process` 的后台线程）。两条线可能同时触发：
 * 后端执行自动重启前会在生命周期锁内复核失败记录是否仍是同一次，已被这里重启过的
 * 不会再重启一遍；反过来，启动失败（运行不足 10 秒）后端不会自动重启，
 * 免得 AI 改到一半时形成"重启 → 又失败 → 再重启"的循环。
 *
 * ## 它修的是什么
 *
 * 原实现（`RestartConfirm` 里那段）是三个问题的叠加：
 *
 * 1. **冷却期内的事件被丢弃**：`if (cooldown.has(sid)) continue` —— 2 秒窗口里到达的变更
 *    既不排队也不重新计时。于是 AI 连续改代码时，服务跑的往往不是最新代码；更糟的是
 *    **最后一次变更可能正好落在窗口里被吞掉**，之后不再有新事件，服务就停在那个旧状态上。
 * 2. **重启完就撒手**：`processApi.restart()` 是发了不管，而它返回 `Ok` 只代表 spawn 成功。
 *    服务秒退由后端 2 秒后的心跳发现，那时前端早把这次重启忘了——**没有任何重试**。
 * 3. **只探一次、窗口太短**（这一轮补的）：Maven / NPM 这类"先构建再启动"的服务，
 *    重启后几十秒内进程都活着（在跑构建），原实现 7 秒时探一次看到 `running` 就判成功。
 *    随后构建失败进失败表，已经没人观察——用户看到的就是"自动重启后起不来，手动重启
 *    又好了"（手动那次文件写完了、端口也空了）。
 *
 * 三条叠起来就是你遇到的现象：自动重启撞在"文件写到一半 / 旧进程还没释放端口 /
 * 构建还没结束"上 → 服务失败 → 再也没人管 → 手动点一下就好了。
 *
 * ## 这一版的规则
 *
 * - 同一服务的变更**合并**为一轮（`change` 在轮内只置一个待办标记，不丢弃）；
 * - 一轮 = 重启 → 等节流窗口（`throttleMs`）→ **观察窗（`verifyMs`）内轮询探头**：
 *   - 期间又有变更 → **立刻再起一轮**（尾沿：代码又变了，这一轮的结果已经过时），
 *     这不算"重试"，所以不消耗重试次数；
 *   - 探头为 `ready`（已监听端口）→ 连续两拍确认后收手（防"绑定端口后秒退"）；
 *   - 探头为 `failed` → 立即按失败处理（**不等到窗口结束**）；
 *   - 窗口结束时仍是 `running`（构建/启动中）或 `unknown` → 视为成功，**不重试**。
 *     "启动慢但健康"的服务不能因为"还没探到 ready"就反复重启；
 * - 判定为 **`failed` 才重试**（最多 `maxAttempts`，退避 `backoffMs`）；
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

/** 服务在一次探查里的状态 */
export type ProbeOutcome =
  /** 已监听端口：确定已就绪（build-then-run 的服务必须用它判定，不能只看进程在） */
  | 'ready'
  /** 进程活着但还没监听端口（构建 / 启动中）：继续观察，不能据此判成功 */
  | 'running'
  /** 意外退出（后端失败表）：可重试 */
  | 'failed'
  /** 既不在运行也不在失败表（重启还没落表 / 状态未知） */
  | 'unknown';

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
  /**
   * 观察窗口上限（ms，自重启起算）：期内**持续轮询**探头，失败随时捕获；
   * 到窗口结束仍是 running/unknown 则视为成功。
   *
   * 不能是"到点探一次"：Maven/NPM 先构建再启动，失败发生在构建结束时（几十秒），
   * 一次性探头的时点稍早就会把"还在构建"误判成成功。
   */
  verifyMs?: number;
  /** 观察窗的轮询间隔（ms） */
  pollMs?: number;
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
 * 观察窗口上限：自重启起算 60 秒（期内轮询，不是到点只看一眼）。
 *
 * 为什么需要这么长：Maven 服务的失败发生在构建结束时——`mvn package` 本身就可能
 * 跑几十秒，7 秒的窗口里进程一直"活着"（在构建），根本看不到随后的失败。
 * 60 秒足够覆盖常见构建；窗口只是**上限**——探到 `ready`（已监听端口）或 `failed`
 * 都会提前结束。
 */
const DEFAULT_VERIFY_MS = 60_000;
/**
 * 观察窗轮询间隔。后端心跳 2 秒一轮把崩溃写进失败表，1.5 秒的轮询能在一个心跳内
 * 发现它；共享 `runningStore.refreshIfStale` 后多服务并发探测也只多打一次 `get_running`。
 */
const DEFAULT_OBSERVE_POLL_MS = 1_500;
/**
 * `ready` 需要连续几拍确认。
 *
 * 有些服务绑定端口后几秒才退出（配置/依赖错误）；只探到一拍 ready 就收手会漏掉它。
 */
const READY_CONFIRM_POLLS = 2;
/** 一轮里最多重启 3 次（首次 + 2 次重试）：再多就不像"重试"而像"卡在重启循环里" */
const DEFAULT_MAX_ATTEMPTS = 3;
/** 退避：2 秒 → 5 秒（第三次不给——尝试次数只有 3 次，第 2 次重试前等到 5 秒已足够让端口冷却） */
const DEFAULT_BACKOFF_MS = [2000, 5000];

export function createAutoRestarter(deps: AutoRestartDeps): AutoRestarter {
  const { restart, probe, sleep, onGiveUp, onError } = deps;
  const throttleMs = deps.throttleMs ?? DEFAULT_THROTTLE_MS;
  const verifyMs = Math.max(deps.verifyMs ?? DEFAULT_VERIFY_MS, throttleMs);
  const pollMs = Math.max(1, deps.pollMs ?? DEFAULT_OBSERVE_POLL_MS);
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

        // 观察窗：轮询到窗口上限，而不是睡满再探一次。
        // build-then-run 的服务（Maven/NPM）失败发生在构建结束时——一次性探头
        // 只要早于那一刻，就会把"还在构建"误判成成功，之后没人再观察。
        const budget = Math.max(0, verifyMs - throttleMs);
        let outcome: ProbeOutcome = 'unknown';
        let waited = 0;
        let readyStreak = 0;
        let probeErrorReported = false;
        let changed = false;
        while (waited < budget) {
          const step = Math.min(pollMs, budget - waited);
          await sleep(step);
          waited += step;
          // 观察期间又变了：这一轮基于的代码已过时，立刻重开一轮（不算重试）
          if (state.pending) {
            state.pending = false;
            changed = true;
            break;
          }
          try {
            outcome = await probe(serviceId);
          } catch (e) {
            // 探查失败（IPC 抖动等）→ 按 unknown 继续观察；同一轮只报一次，避免刷屏。
            // 判据缺失时最安全的动作是什么都不做，绝不据此重启一个可能好好的服务。
            if (!probeErrorReported) {
              onError(serviceId, e);
              probeErrorReported = true;
            }
            outcome = 'unknown';
            continue;
          }
          if (outcome === 'failed') break;
          if (outcome === 'ready') {
            // 连续两拍确认：防"绑定端口后几秒才退出"被当成成功
            readyStreak += 1;
            if (readyStreak >= READY_CONFIRM_POLLS) break;
          } else {
            readyStreak = 0;
          }
        }
        if (changed) {
          attempts = 0;
          continue;
        }

        // 窗口结束仍是 running（还在构建/启动）或 unknown（状态不可判）→ 不动它：
        // 重试会把"启动慢但健康"的服务越搞越糟
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
