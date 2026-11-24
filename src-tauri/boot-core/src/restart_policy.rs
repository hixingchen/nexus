//! 服务崩溃自动重启的**策略常量与纯判定**（无 IO、无时钟读取）。
//!
//! 本地服务监督器（`commands::process::auto_restart_tick`）与开机启动器
//! （`boot_runner`）共用这一份规则——两边各写一套的话，"什么算崩溃""退避多久"
//! 迟早会分叉，而用户看到的是同一个功能。
//!
//! ## 什么不算崩溃（本文件最重要的规则）
//!
//! 进程启动后**运行不足 [`MIN_UPTIME_MS`]** 就退出，判为 [`ExitKind::StartupFailure`]，
//! **不会**进入自动重启。这条规则直接挡掉了开发期最典型的循环：
//! AI 改代码改到一半 → 文件监听触发重启 → 新进程因代码不完整启动失败。
//! 那是一次"启动失败"，不是"稳定运行后崩溃"；此时文件监听自己的有界重试
//! （前端 `autoRestart.ts`，最多 3 次）才是唯一的处置方，后端不能再叠加一个循环。
//!
//! 退出码 0 判为 [`ExitKind::Clean`]：服务正常结束（一次性脚本等）不该被反复拉起。
//! 拿不到运行时长（例如 spawn 失败、状态过期）同样按启动失败处理——判不了就**不重启**，
//! 这是唯一安全的方向。

/// 稳定运行多久之后退出才算"崩溃"。短于它的退出按启动失败处理。
///
/// 10 秒覆盖了绝大多数"代码还没写完 / 端口没释放 / 依赖没起来"的启动失败，
/// 又足够短，不会把真正的崩溃漏成启动失败。
pub const MIN_UPTIME_MS: u64 = 10_000;

/// 统计窗口内最多自动重启几次（不含用户手动启动）。
///
/// 与 systemd 的 StartLimitBurst 同一思路：崩溃循环被窗口 + 次数双重封顶，
/// 用尽后保留失败态，等待人工处理。
pub const MAX_RESTARTS: usize = 5;

/// 重启预算的滑动窗口：窗口外的旧重启记录不再计数。
///
/// 也顺带承担"稳定运行后预算自然清零"：服务连续跑过一个窗口后再次崩溃，
/// 旧记录已滑出窗口，不会因为几天前的一次崩溃被提前判死刑。
pub const WINDOW_MS: u64 = 10 * 60_000;

/// 第 n 次自动重启前的退避（n 从 1 开始，越界取最后一项）。
///
/// 2s → 5s → 15s → 30s → 60s：前两次要快（用户正在等），后面拉长给依赖/端口留恢复时间。
pub const BACKOFF_MS: [u64; MAX_RESTARTS] = [2_000, 5_000, 15_000, 30_000, 60_000];

/// 一次退出的分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitKind {
    /// 正常结束（退出码 0）
    Clean,
    /// 启动阶段就失败（运行不足 `MIN_UPTIME_MS`，或运行时长不可知）
    StartupFailure,
    /// 稳定运行后意外退出
    Crash,
}

/// 判定一次退出是否值得自动重启。
///
/// `uptime_ms` 为 `None` 表示拿不到运行时长（spawn 失败、进程启动时刻未知）。
/// 此时按启动失败处理：宁可少重启一次，也不要在无法判断时进入重启循环。
pub fn classify_exit(exit_code: Option<i32>, uptime_ms: Option<u64>) -> ExitKind {
    if exit_code == Some(0) {
        return ExitKind::Clean;
    }
    match uptime_ms {
        Some(up) if up >= MIN_UPTIME_MS => ExitKind::Crash,
        _ => ExitKind::StartupFailure,
    }
}

/// 还能不能再自动重启一次；能则返回这次重启前的退避时长，并把本次安排记入窗口。
///
/// 返回 `None` = 窗口内预算已用尽（调用方应保留失败态并通知用户）。
/// `restart_times` 由调用方持有：本地监督器与开机启动器各自保存自己那份。
pub fn next_restart_delay(restart_times: &mut Vec<u64>, now_ms: u64) -> Option<u64> {
    restart_times.retain(|t| now_ms.saturating_sub(*t) < WINDOW_MS);
    if restart_times.len() >= MAX_RESTARTS {
        return None;
    }
    let delay = BACKOFF_MS[restart_times.len()];
    restart_times.push(now_ms);
    Some(delay)
}

/// 当前 epoch 毫秒。与 [`classify_exit`] / [`next_restart_delay`] 分开，
/// 纯判定函数的时间由调用方注入，测试因此不需要真的等退避。
pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 退出码 0 = 正常结束；无论跑多久都不该自动重启。
    #[test]
    fn test_clean_exit_is_never_restarted() {
        assert_eq!(classify_exit(Some(0), Some(999_999)), ExitKind::Clean);
        assert_eq!(classify_exit(Some(0), None), ExitKind::Clean);
    }

    /// 核心边界：运行不足 10 秒 = 启动失败（AI 改到一半的那种），不自动重启。
    #[test]
    fn test_short_lived_exit_is_startup_failure() {
        assert_eq!(classify_exit(Some(1), Some(0)), ExitKind::StartupFailure);
        assert_eq!(classify_exit(Some(1), Some(MIN_UPTIME_MS - 1)), ExitKind::StartupFailure);
        assert_eq!(classify_exit(Some(1), Some(MIN_UPTIME_MS)), ExitKind::Crash);
        assert_eq!(classify_exit(Some(1), Some(MIN_UPTIME_MS + 1)), ExitKind::Crash);
        // 没有退出码（被信号结束）同样按运行时长判断
        assert_eq!(classify_exit(None, Some(MIN_UPTIME_MS)), ExitKind::Crash);
        // 运行时长不可知：保守按启动失败，绝不进入重启循环
        assert_eq!(classify_exit(Some(1), None), ExitKind::StartupFailure);
        assert_eq!(classify_exit(None, None), ExitKind::StartupFailure);
    }

    /// 预算：窗口内第 1..5 次给出递增退避，第 6 次返回 None；不越界 panic。
    #[test]
    fn test_restart_budget_and_backoff() {
        let mut times = Vec::new();
        let mut now = 1_000_000;
        for (i, expected) in BACKOFF_MS.iter().enumerate() {
            assert_eq!(
                next_restart_delay(&mut times, now),
                Some(*expected),
                "第 {} 次自动重启的退避不对",
                i + 1
            );
            assert_eq!(times.len(), i + 1);
            now += 30_000;
        }
        assert_eq!(next_restart_delay(&mut times, now), None, "窗口内第 6 次必须放弃");
        assert_eq!(times.len(), MAX_RESTARTS, "被拒绝的那次不该记入预算");
    }

    /// 窗口滑动：旧记录过期后重新获得预算（服务稳定运行了足够久）。
    #[test]
    fn test_restart_budget_slides_with_window() {
        let start = 1_000_000;
        let seed = || vec![start, start + 1_000, start + 2_000, start + 3_000, start + 4_000];

        // 刚好一个窗口之后：最早那条滑出（剩 4 条），下一次用第 5 档退避
        let mut times = seed();
        assert_eq!(
            next_restart_delay(&mut times, start + WINDOW_MS),
            Some(BACKOFF_MS[4]),
            "窗口外的旧记录必须被清掉"
        );
        assert_eq!(times.len(), MAX_RESTARTS, "滑出一条 + 本次新增一条");

        // 最后一条也滑出：完整预算回来了
        let mut times = seed();
        assert_eq!(
            next_restart_delay(&mut times, start + WINDOW_MS + 4_001),
            Some(BACKOFF_MS[0]),
            "所有旧重启记录都过窗后应重新用第一档退避"
        );
        assert_eq!(times.len(), 1);
    }
}
