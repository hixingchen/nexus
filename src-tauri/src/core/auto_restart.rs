//! 崩溃自动重启的**决策状态机**（不碰进程 / DB / IPC）。
//!
//! 只回答两件事：这次失败值不值得自动重启、下一次该在什么时候重启。
//! 真正的重启由两处执行：
//! - 本地服务：`commands::process::auto_restart_tick`（后台线程，窗口最小化也生效）；
//! - 开机启动器：`boot_runner`（Nexus 界面根本没开也生效）。
//!
//! ## 与文件监听自动重启的分工
//!
//! 文件变更引发的重启由前端 `autoRestart.ts` 负责——只有它知道"代码还在变"，
//! 它的尾沿去抖 / 观察窗 / 最多 3 次重试正是为"改到一半"设计的。
//! 本模块只管**进程自己死了**（稳定运行后意外退出）。两者可能同时发生：
//! 执行重启用 `ProcessManager::auto_restart`，它在生命周期锁内复核"失败记录还是不是
//! 同一次"——已被用户或文件监听重启过的不会再重启一遍。
//!
//! ## 预算与放弃
//!
//! 规则在 `core::restart_policy` 里（两边共用）：启动失败不重启；稳定运行后崩溃才重启；
//! 10 分钟内最多 5 次，退避 2/5/15/30/60 秒。用尽后保留失败态与日志，并**只通知一次**
//! （`GiveUp`），不会每秒刷屏。

use std::collections::{HashMap, HashSet};

use crate::core::process::FailedService;
use crate::core::restart_policy::{classify_exit, next_restart_delay, ExitKind, WINDOW_MS};

/// 服务既不在失败表、也不在运行表的宽限期。
///
/// 自动重启的 `stop → start` 之间有一段两边都查不到的空窗；宽限期保证监督器
/// 不会在空窗里把预算/计划清掉，从而把"崩溃循环"误当成"用户停了服务"。
const ORPHAN_GRACE_MS: u64 = 30_000;

/// 监督器要求执行的一次重启。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RestartPlan {
    pub service_id: String,
    /// 失败记录的时间戳：执行前据此复核仍是同一次失败（见 `ProcessManager::auto_restart`）
    pub failure_timestamp: String,
    /// 第几次自动重启（从 1 开始，用于日志与提示）
    pub attempt: u32,
    pub exit_code: Option<i32>,
    pub uptime_ms: Option<u64>,
}

/// 预算用尽：保留失败态，通知用户一次。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GiveUpPlan {
    pub service_id: String,
    pub attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AutoRestartAction {
    Restart(RestartPlan),
    GiveUp(GiveUpPlan),
}

#[derive(Default)]
struct RestartState {
    /// 当前正在处理的失败记录时间戳
    failure_key: String,
    /// 窗口内已安排的自动重启时刻（epoch ms）
    restart_times: Vec<u64>,
    /// 已排期、尚未到点的重启
    due_at_ms: Option<u64>,
    /// 当前失败已经排过期或用尽预算（同一条失败记录不再重复处理）
    handled: bool,
    /// 当前失败已通知过"预算用尽"
    gave_up: bool,
    /// 最近一次在失败表/运行表里看到该服务的时刻（用于清理状态）
    last_seen_ms: u64,
}

/// 崩溃自动重启监督器。由后台线程持有，跨轮次保留预算与计划。
#[derive(Default)]
pub(crate) struct AutoRestartSupervisor {
    states: HashMap<String, RestartState>,
}

impl AutoRestartSupervisor {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 观察当前失败/运行快照，推进状态并返回本轮要执行的动作。
    ///
    /// `running` 是 `(service_id, 进程启动时刻 epoch ms)`：用来判断服务是否重新跑起来。
    /// 时间由调用方注入，测试可以在毫秒内走完整个退避/预算周期。
    ///
    /// 只处理 `source == "local"` 的失败：开机启动器拉起的服务由 runner 自己的监督逻辑
    /// 负责，两边都重启会在同一台机器上出现两份实例。
    pub(crate) fn poll(
        &mut self,
        failed: &[FailedService],
        running: &[(String, u64)],
        now_ms: u64,
    ) -> Vec<AutoRestartAction> {
        let mut out = Vec::new();
        let mut seen_failed: HashSet<&str> = HashSet::new();

        for f in failed {
            if f.source != "local" {
                continue;
            }
            seen_failed.insert(f.service_id.as_str());
            let state = self.states.entry(f.service_id.clone()).or_default();
            state.last_seen_ms = now_ms;

            // 服务设置里关掉了崩溃自动重启：同启动失败一样清掉计划、不排期。
            if !f.crash_restart {
                state.failure_key = f.timestamp.clone();
                state.due_at_ms = None;
                state.handled = false;
                continue;
            }

            let kind = classify_exit(f.exit_code, f.uptime_ms);
            if kind != ExitKind::Crash {
                // 启动失败 / 正常退出：不排期。把旧计划清掉，避免到点后去重启一个
                // 已经被新失败取代的服务。
                //
                // **不动 `gave_up`**：预算用尽后自动重启又失败（spawn 失败等）会以
                // "运行时长不可知"落到这里；如果把 gave_up 清掉，用户随后手动拉起
                // 服务时就不再触发下面的预算清零，下一次崩溃会被旧账直接判死刑。
                state.failure_key = f.timestamp.clone();
                state.due_at_ms = None;
                state.handled = false;
                continue;
            }

            if state.failure_key != f.timestamp {
                // 重启后再次崩溃 = 新的一次失败：旧计划作废，预算继续按滑动窗口累计。
                state.failure_key = f.timestamp.clone();
                state.due_at_ms = None;
                state.handled = false;
                state.gave_up = false;
            }

            // 已排期：到点才交付（退避窗口本身也是防止瞬间重启循环的一部分）
            if let Some(due) = state.due_at_ms {
                if now_ms >= due {
                    state.due_at_ms = None;
                    out.push(AutoRestartAction::Restart(RestartPlan {
                        service_id: f.service_id.clone(),
                        failure_timestamp: f.timestamp.clone(),
                        attempt: state.restart_times.len() as u32,
                        exit_code: f.exit_code,
                        uptime_ms: f.uptime_ms,
                    }));
                }
                continue;
            }
            if state.handled {
                continue;
            }

            // 预算检查 + 排期。`next_restart_delay` 会顺手 prune 窗口外的旧记录。
            let budget = {
                let times = &mut state.restart_times;
                let delay = next_restart_delay(times, now_ms);
                (delay, times.len() as u32)
            };
            match budget.0 {
                Some(delay) => {
                    state.due_at_ms = Some(now_ms.saturating_add(delay));
                    state.handled = true;
                }
                None => {
                    state.handled = true;
                    state.gave_up = true;
                    out.push(AutoRestartAction::GiveUp(GiveUpPlan {
                        service_id: f.service_id.clone(),
                        attempts: budget.1,
                    }));
                }
            }
        }

        // 服务重新跑起来：保留预算（自动重启后还得继续算账）；
        // 但"已放弃后又跑起来"只可能是用户/文件监听手动介入——给一次干净的预算，
        // 否则用户修好代码后，下一次崩溃会被十分钟前的旧账直接判死刑。
        let running_map: HashMap<&str, u64> =
            running.iter().map(|(id, started)| (id.as_str(), *started)).collect();
        for (id, started_at) in &running_map {
            let Some(state) = self.states.get_mut(*id) else { continue };
            state.last_seen_ms = now_ms;
            if state.gave_up {
                state.gave_up = false;
                state.handled = false;
                state.due_at_ms = None;
                state.failure_key.clear();
                state.restart_times.clear();
            } else if now_ms.saturating_sub(*started_at) >= WINDOW_MS {
                // 稳定运行超过一个窗口：旧的重启记录不再影响下一次崩溃
                state.restart_times.clear();
            }
        }

        // 清理：失败表/运行表都没有、且已过空窗期的服务（用户停止、服务被删）。
        self.states.retain(|id, state| {
            seen_failed.contains(id.as_str())
                || running_map.contains_key(id.as_str())
                || now_ms.saturating_sub(state.last_seen_ms) < ORPHAN_GRACE_MS
        });

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::restart_policy::{BACKOFF_MS, MAX_RESTARTS, MIN_UPTIME_MS};

    fn failed(id: &str, ts: &str, code: Option<i32>, uptime_ms: Option<u64>) -> FailedService {
        FailedService {
            service_id: id.into(),
            exit_code: code,
            timestamp: ts.into(),
            source: "local".into(),
            port_conflict: None,
            uptime_ms,
            crash_restart: true,
        }
    }

    /// 开发期最关键的回归：AI 改到一半导致的启动失败（运行不足 10 秒）
    /// **不得**进入自动重启，否则会形成"重启 → 又失败 → 再重启"的循环。
    #[test]
    fn test_startup_failure_never_schedules_restart() {
        let mut sup = AutoRestartSupervisor::new();
        let f = failed("s1", "t1", Some(1), Some(MIN_UPTIME_MS - 1));
        for now in [0, 60_000, 600_000, 3_600_000] {
            assert!(
                sup.poll(std::slice::from_ref(&f), &[], now).is_empty(),
                "启动失败不该排任何重启（now={}）",
                now
            );
        }
        // 正常结束（退出码 0）同样不重启
        let clean = failed("s2", "t2", Some(0), Some(999_999));
        assert!(sup.poll(&[clean], &[], 3_600_000).is_empty());
        // 运行时长不可知：保守不重启
        let unknown = failed("s3", "t3", Some(1), None);
        assert!(sup.poll(&[unknown], &[], 3_600_000).is_empty());
    }

    /// 稳定运行后崩溃：先排期，未到点不交付，到点交付一次；同一条失败不重复交付。
    #[test]
    fn test_crash_is_scheduled_after_backoff_exactly_once() {
        let mut sup = AutoRestartSupervisor::new();
        let f = failed("s1", "t1", Some(1), Some(MIN_UPTIME_MS));
        let t0 = 1_000_000;
        assert!(sup.poll(std::slice::from_ref(&f), &[], t0).is_empty(), "第一次只排期，不立刻重启");
        assert!(
            sup.poll(std::slice::from_ref(&f), &[], t0 + BACKOFF_MS[0] - 1).is_empty(),
            "退避没到，不能提前重启"
        );
        let actions = sup.poll(std::slice::from_ref(&f), &[], t0 + BACKOFF_MS[0]);
        assert_eq!(actions.len(), 1, "到点应交付一次重启");
        let AutoRestartAction::Restart(plan) = &actions[0] else {
            panic!("应当是重启动作: {:?}", actions);
        };
        assert_eq!(plan.service_id, "s1");
        assert_eq!(plan.attempt, 1);
        assert_eq!(plan.failure_timestamp, "t1");
        assert!(sup.poll(&[f], &[], t0 + BACKOFF_MS[0] + 1).is_empty(), "同一条失败不能重复重启");
    }

    /// 重启后再次崩溃：预算累计、退避递增，窗口内第 6 次放弃且只通知一次。
    #[test]
    fn test_restart_budget_gives_up_once_per_failure() {
        let mut sup = AutoRestartSupervisor::new();
        let mut now = 1_000_000u64;
        for (i, delay) in BACKOFF_MS.iter().enumerate() {
            let f = failed("s1", &format!("fail-{}", i), Some(1), Some(60_000));
            assert!(sup.poll(&[f], &[], now).is_empty(), "第 {} 次只排期", i + 1);
            let actions = sup.poll(
                &[failed("s1", &format!("fail-{}", i), Some(1), Some(60_000))],
                &[],
                now + *delay,
            );
            assert_eq!(actions.len(), 1, "第 {} 次应到点重启", i + 1);
            now += *delay + 30_000;
        }

        let sixth = failed("s1", "fail-5", Some(1), Some(60_000));
        let actions = sup.poll(std::slice::from_ref(&sixth), &[], now);
        assert_eq!(actions.len(), 1);
        let AutoRestartAction::GiveUp(plan) = &actions[0] else {
            panic!("第 6 次应放弃: {:?}", actions);
        };
        assert_eq!(plan.attempts, MAX_RESTARTS as u32);
        assert!(
            sup.poll(&[sixth], &[], now + 1_000).is_empty(),
            "同一次失败放弃后不能每秒重复通知"
        );
    }

    /// 预算用尽后，服务被用户/文件监听手动拉起：预算清零，下一次崩溃重新获得完整预算。
    ///
    /// 没有这条，"修好代码后第一次崩溃"会被十分钟内的旧账直接判成放弃。
    #[test]
    fn test_manual_restart_after_give_up_resets_budget() {
        let mut sup = AutoRestartSupervisor::new();
        let mut now = 1_000_000u64;
        for (i, delay) in BACKOFF_MS.iter().enumerate() {
            let ts = format!("fail-{}", i);
            let _ = sup.poll(&[failed("s1", &ts, Some(1), Some(60_000))], &[], now);
            let _ = sup.poll(
                &[failed("s1", &ts, Some(1), Some(60_000))],
                &[],
                now + *delay,
            );
            now += *delay + 1_000;
        }
        let sixth = failed("s1", "fail-5", Some(1), Some(60_000));
        let give_up = sup.poll(&[sixth], &[], now);
        assert!(matches!(give_up[0], AutoRestartAction::GiveUp(_)));

        // 用户手动启动（进程出现在运行表）→ 预算清零
        let _ = sup.poll(&[], &[("s1".into(), now)], now + 1_000);
        // 新崩溃：应重新排期（第一次退避），而不是立刻放弃
        let after = failed("s1", "fail-new", Some(1), Some(60_000));
        assert!(sup.poll(std::slice::from_ref(&after), &[], now + 2_000).is_empty(), "应重新排期");
        let actions = sup.poll(&[after], &[], now + 2_000 + BACKOFF_MS[0]);
        assert!(
            matches!(actions.as_slice(), [AutoRestartAction::Restart(_)]),
            "手动介入后应重新获得完整预算: {:?}",
            actions
        );
    }

    /// 服务设置里关掉「崩溃后自动重启」：即使是稳定运行后的崩溃也不排期。
    #[test]
    fn test_crash_restart_disabled_is_ignored() {
        let mut sup = AutoRestartSupervisor::new();
        let mut f = failed("s1", "t1", Some(1), Some(60_000));
        f.crash_restart = false;
        let t0 = 1_000_000;
        assert!(sup.poll(std::slice::from_ref(&f), &[], t0).is_empty());
        assert!(
            sup.poll(std::slice::from_ref(&f), &[], t0 + 60_000).is_empty(),
            "关闭后无论等多久都不该排期"
        );
    }

    /// 开机启动器拉起的失败不由本地监督器处理（否则会出现两份实例）。
    #[test]
    fn test_boot_source_is_ignored() {
        let mut sup = AutoRestartSupervisor::new();
        let mut f = failed("s1", "t1", Some(1), Some(60_000));
        f.source = "boot".into();
        assert!(sup.poll(&[f], &[], 1_000_000).is_empty());
    }

    /// 失败消失、服务也没跑（用户停止/删除）：空窗期后状态被清理，不泄漏。
    #[test]
    fn test_state_cleaned_after_service_disappears() {
        let mut sup = AutoRestartSupervisor::new();
        let f = failed("s1", "t1", Some(1), Some(60_000));
        let t0 = 1_000_000;
        let _ = sup.poll(&[f], &[], t0); // 排期
        let _ = sup.poll(&[], &[], t0 + 1_000); // 空窗期：保留
        assert_eq!(sup.states.len(), 1, "空窗期内不能丢状态（stop→start 之间两边都查不到）");
        let _ = sup.poll(&[], &[], t0 + ORPHAN_GRACE_MS + 1);
        assert!(sup.states.is_empty(), "超过空窗期应清理");
    }

    /// 待重启期间用户先手动重启了：失败记录消失，到点不得再重启一次。
    #[test]
    fn test_superseded_failure_is_not_restarted() {
        let mut sup = AutoRestartSupervisor::new();
        let f = failed("s1", "t1", Some(1), Some(60_000));
        let t0 = 1_000_000;
        let _ = sup.poll(&[f], &[], t0); // 排期
        // 用户/文件监听抢先重启：失败消失、服务已在运行
        let actions = sup.poll(&[], &[("s1".into(), t0)], t0 + BACKOFF_MS[0]);
        assert!(actions.is_empty(), "已被手动重启的失败不得再次自动重启: {:?}", actions);
    }
}
