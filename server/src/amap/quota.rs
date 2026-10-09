//! 高德配额守卫。
//!
//! 个人认证的配额极不对称（技术设计 §5.1）：
//!
//! | 接口 | 每日 | 是否瓶颈 |
//! |---|---|---|
//! | 步行 / 骑行路径规划 | 5000 | 否 |
//! | 地理编码 / 逆地理 | 5000 | 否 |
//! | 距离测量 | 5000 | 否 |
//! | **周边 / 关键字搜索** | **100** | **是** |
//!
//! 另外 QPS 上限 5，连续 10 秒 ≥50 次会被动态熔断。
//!
//! 所以这里做两件事：
//! 1. **日配额**：用完就返回 [`QuotaExceeded::Daily`]，由调用方静默降级成直线；
//! 2. **QPS 闸门**：`acquire` 会自己 sleep 到有槽位，不会去撞熔断。

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// 高德接口分类。每一类单独算日配额。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuotaKind {
    /// 步行 / 骑行路径规划。
    Route,
    /// 地理编码 / 逆地理编码。
    Geocode,
    /// 距离测量。
    Distance,
    /// 周边 / 关键字搜索（**只有 100 次/天，最大的瓶颈**）。
    Search,
}

impl QuotaKind {
    pub const ALL: [QuotaKind; 4] = [
        QuotaKind::Route,
        QuotaKind::Geocode,
        QuotaKind::Distance,
        QuotaKind::Search,
    ];

    pub fn index(self) -> usize {
        match self {
            QuotaKind::Route => 0,
            QuotaKind::Geocode => 1,
            QuotaKind::Distance => 2,
            QuotaKind::Search => 3,
        }
    }

    /// 个人认证的每日额度。**这是硬编码的常量，不是配置项**——
    /// 它是高德侧的事实，改了也没用。
    pub fn daily_limit(self) -> u32 {
        match self {
            QuotaKind::Route => 5_000,
            QuotaKind::Geocode => 5_000,
            QuotaKind::Distance => 5_000,
            QuotaKind::Search => 100,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            QuotaKind::Route => "route",
            QuotaKind::Geocode => "geocode",
            QuotaKind::Distance => "distance",
            QuotaKind::Search => "search",
        }
    }
}

/// 配额耗尽。**这不是错误，是降级信号。**
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QuotaExceeded {
    #[error("今日 {kind} 配额已用完（{limit} 次），降级为本地估算")]
    Daily { kind: &'static str, limit: u32 },
    #[error("QPS 超限，{retry_after_ms}ms 后再试")]
    Qps { retry_after_ms: u64 },
}

/// QPS 滑动窗口宽度。
const QPS_WINDOW: Duration = Duration::from_secs(1);

/// 计数器 + QPS 闸门。进程内单例，用 `Arc<tokio::sync::Mutex<Quota>>` 包起来共享。
#[derive(Debug)]
pub struct Quota {
    /// 当前计数所属的「天」（Unix 天数）。
    day: u32,
    used: [u32; 4],
    /// 最近一秒内的请求时刻，用来算 QPS。
    recent: VecDeque<Instant>,
    /// QPS 上限，默认 5。
    qps: u32,
    /// 关掉后所有检查直接放行（给自测/离线跑用）。
    enabled: bool,
}

impl Default for Quota {
    fn default() -> Self {
        Self::new()
    }
}

impl Quota {
    pub fn new() -> Self {
        Self {
            day: current_day(),
            used: [0; 4],
            recent: VecDeque::new(),
            qps: 5,
            enabled: true,
        }
    }

    /// 测试用：指定天与 QPS，绕过系统时钟。
    pub fn for_test(day: u32, qps: u32) -> Self {
        Self {
            day,
            used: [0; 4],
            recent: VecDeque::new(),
            qps,
            enabled: true,
        }
    }

    /// 关掉守卫：所有 `try_acquire` 直接成功。离线开发时用。
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::new()
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn qps(&self) -> u32 {
        self.qps
    }

    /// 当前计数所属的天。
    pub fn day(&self) -> u32 {
        self.day
    }

    pub fn used(&self, kind: QuotaKind) -> u32 {
        self.used[kind.index()]
    }

    pub fn remaining(&self, kind: QuotaKind) -> u32 {
        kind.daily_limit().saturating_sub(self.used(kind))
    }

    /// 全部接口今日合计调用次数。验收标准里「日 API 调用 < 200 次」看这个。
    pub fn total_used(&self) -> u32 {
        self.used.iter().sum()
    }

    /// 跨天自动清零。返回 `true` 表示确实翻天了。
    pub fn roll_day_if_needed(&mut self) -> bool {
        let today = current_day();
        if today != self.day {
            self.day = today;
            self.used = [0; 4];
            self.recent.clear();
            true
        } else {
            false
        }
    }

    /// 非阻塞地要一个名额。
    ///
    /// - 日配额用完 → [`QuotaExceeded::Daily`]
    /// - QPS 窗口满了 → [`QuotaExceeded::Qps`]，`retry_after_ms` 告诉调用方等多久
    ///
    /// 拿名额是「先占用后请求」，所以并发下不会互相挤爆配额。
    pub fn try_acquire(&mut self, kind: QuotaKind, now: Instant) -> Result<(), QuotaExceeded> {
        if !self.enabled {
            return Ok(());
        }
        self.roll_day_if_needed();

        if self.used(kind) >= kind.daily_limit() {
            return Err(QuotaExceeded::Daily {
                kind: kind.as_str(),
                limit: kind.daily_limit(),
            });
        }

        // 清掉窗口外的旧记录
        while let Some(front) = self.recent.front() {
            if now.duration_since(*front) >= QPS_WINDOW {
                self.recent.pop_front();
            } else {
                break;
            }
        }
        if self.recent.len() as u32 >= self.qps {
            let oldest = *self.recent.front().expect("非空");
            let waited = now.duration_since(oldest);
            let retry = QPS_WINDOW.saturating_sub(waited);
            return Err(QuotaExceeded::Qps {
                retry_after_ms: retry.as_millis().max(1) as u64,
            });
        }

        self.recent.push_back(now);
        self.used[kind.index()] += 1;
        Ok(())
    }

    /// 只计数不检查窗口（用于「请求真的发出去了」时补记，比如重试）。
    pub fn record(&mut self, kind: QuotaKind) {
        if !self.enabled {
            return;
        }
        self.roll_day_if_needed();
        self.used[kind.index()] += 1;
    }

    /// 高德明确回了「超限」类 `infocode` 时，直接把当日额度打满。
    ///
    /// 本地计数和服务器计数难免会飘（重启、多实例、别人的 Key）。
    /// 一旦被服务器拒绝，就别再试了——继续试只会撞熔断。
    pub fn mark_exhausted(&mut self, kind: QuotaKind) {
        if !self.enabled {
            return;
        }
        self.roll_day_if_needed();
        self.used[kind.index()] = kind.daily_limit();
    }
}

/// 「今天」= Unix 秒 / 86400。
///
/// 用 UTC 日界而不是本地日界：高德的配额也是按自然日重置的，差几小时无所谓，
/// 但**必须单调**，否则跨天清零会算出负的剩余额度。
pub fn current_day() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs() / 86_400) as u32,
        // 系统时钟早于 1970 只可能发生在时钟被改坏时，给个稳定值别 panic。
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_limit_is_the_documented_number() {
        assert_eq!(QuotaKind::Route.daily_limit(), 5_000);
        assert_eq!(QuotaKind::Geocode.daily_limit(), 5_000);
        assert_eq!(QuotaKind::Distance.daily_limit(), 5_000);
        // 搜索只有 100 次：这是整套设计里最紧的约束。
        assert_eq!(QuotaKind::Search.daily_limit(), 100);
    }

    #[test]
    fn daily_counter_runs_out() {
        let mut q = Quota::for_test(1, 5);
        let t0 = Instant::now();
        for i in 0..100 {
            q.try_acquire(QuotaKind::Search, t0 + Duration::from_secs(10 * i as u64))
                .expect("前 100 次应该都成功");
        }
        assert_eq!(q.remaining(QuotaKind::Search), 0);
        let err = q
            .try_acquire(QuotaKind::Search, t0 + Duration::from_secs(1000))
            .unwrap_err();
        assert!(matches!(err, QuotaExceeded::Daily { .. }));
        // Route 的额度不受影响
        assert_eq!(q.remaining(QuotaKind::Route), 5_000);
    }

    #[test]
    fn qps_gate_blocks_the_sixth_call_in_a_second() {
        let mut q = Quota::for_test(1, 5);
        let t0 = Instant::now();
        for i in 0..5 {
            q.try_acquire(QuotaKind::Route, t0 + Duration::from_millis(i))
                .expect("一秒内前 5 次应通过");
        }
        let err = q
            .try_acquire(QuotaKind::Route, t0 + Duration::from_millis(6))
            .unwrap_err();
        match err {
            QuotaExceeded::Qps { retry_after_ms } => assert!(retry_after_ms > 0),
            other => panic!("应该是 QPS 超限，实际 {other:?}"),
        }
        // 但 1 秒之后又能用了
        q.try_acquire(QuotaKind::Route, t0 + Duration::from_millis(1100))
            .expect("窗口滑过去就该放行");
    }

    #[test]
    fn day_roll_resets_counters() {
        let mut q = Quota::for_test(1, 5);
        let t0 = Instant::now();
        q.try_acquire(QuotaKind::Geocode, t0).unwrap();
        assert_eq!(q.used(QuotaKind::Geocode), 1);
        q.day = current_day().wrapping_sub(1); // 假装自己是昨天
        assert!(q.roll_day_if_needed());
        assert_eq!(q.used(QuotaKind::Geocode), 0);
    }

    #[test]
    fn disabled_guard_never_blocks() {
        let mut q = Quota::disabled();
        let t0 = Instant::now();
        for i in 0..10_000 {
            q.try_acquire(QuotaKind::Search, t0 + Duration::from_millis(i))
                .unwrap();
        }
        assert_eq!(q.used(QuotaKind::Search), 0, "关掉后不计数");
    }
}
