//! 三层 AI 的统一编排器（《三层AI架构设计》§2）。
//!
//! ## 它是唯一出口
//!
//! 骑手 AI、站长 AI、烽火台 AI 都要打 HTTP，如果各打各的，就会出现
//! 「三层同时超时、互相挤掉配额、日志里全是重试」。所以系统里**只留一个口子**：
//! 所有层都来找 [`AiOrchestrator`]，由它统一决定「这一 tick 轮到谁」。
//!
//! ```text
//! ┌─────────────────────────┐
//! │    AiOrchestrator       │  预算 / 频率 / 并发 统一管
//! └────────────┬────────────┘
//!   ┌──────────┼──────────┐
//! RiderBrain  StationBrain  DirectorBrain
//! ```
//!
//! ## 三条硬约束
//!
//! 1. **按间隔节流**：每层有各自的最小间隔（骑手 8 分钟、站长 20、烽火台 60）；
//! 2. **总预算封顶**：跨层累计，`calls_today >= max_calls_per_day` 就谁都不调；
//! 3. **同一时刻只允许一个在飞**：否则三层抢配额，谁都拿不到完整的 3 秒超时预算。
//!
//! 优先级是 `Director > Station > Rider`——世界不推进（天气/事件），底下两层全白搭。
//! 高优先级的层到期就该它先调，不被低优先级挤占。
//!
//! ## 本层不碰世界
//!
//! 这里只做「该不该调、轮到谁」的算术（[`AiOrchestrator::next_due`] 是**只读**的，
//! 可以在 tick 循环里放心调）。真正调 LLM、校验、写回世界由各层自己的 brain 负责，
//! 写回前一律过 [`super::protocol::validate_decision`]。

use crate::sim::world::World;

use super::{
    brain::Throttle,
    client::{LlmClient, LlmConfig},
};

/// 一天的跨层总调用预算。默认 96 次——够三层按间隔各跑一阵，又不至于烧穿配额。
///
/// 参考《三层AI架构设计》§控制端「调用：22/96」。单层战术大脑的 24 次上限
/// 在这里不适用：那是一个层的预算，这里是三层的总和。
pub const DEFAULT_MAX_CALLS_PER_DAY: u32 = 96;

/// AI 层标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AiLayer {
    Rider,
    Station,
    Director,
}

impl AiLayer {
    /// 按**优先级**排好的三层（Director 最优先）。遍历调度时用这个顺序。
    pub const ALL: [AiLayer; 3] = [AiLayer::Director, AiLayer::Station, AiLayer::Rider];

    /// 层的优先级。数字越小越优先——预算不够时保高优先级。
    pub fn priority(self) -> u8 {
        match self {
            // 导演 > 站长 > 骑手：世界不推进，底下全白搭
            AiLayer::Director => 0,
            AiLayer::Station => 1,
            AiLayer::Rider => 2,
        }
    }

    /// 建议的最小调用间隔（模拟分钟）。
    pub fn min_interval_min(self) -> u32 {
        match self {
            AiLayer::Director => 60,
            AiLayer::Station => 20,
            AiLayer::Rider => 8,
        }
    }

    /// 对外短码（控制端 / 事件用）。
    pub fn key(self) -> &'static str {
        match self {
            AiLayer::Rider => "rider",
            AiLayer::Station => "station",
            AiLayer::Director => "director",
        }
    }

    pub fn from_key(s: &str) -> Option<Self> {
        match s {
            "rider" => Some(AiLayer::Rider),
            "station" => Some(AiLayer::Station),
            "director" | "beacon" => Some(AiLayer::Director),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AiLayer::Rider => "骑手 AI",
            AiLayer::Station => "站长 AI",
            AiLayer::Director => "烽火台 AI",
        }
    }

    /// 数组下标。三种数组（`throttles` / `layer_stats`）都靠它定位。
    ///
    /// 显式写出来，**不依赖枚举声明顺序**——哪天有人重排了 variant，
    /// 这里也不会悄悄错位。
    fn index(self) -> usize {
        match self {
            AiLayer::Rider => 0,
            AiLayer::Station => 1,
            AiLayer::Director => 2,
        }
    }
}

/// 单层的调用统计，给控制端的「预算与质量」面板用。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LayerStats {
    /// 真正打出去的请求数（含失败）。
    pub calls: u64,
    pub failures: u64,
    /// 被采纳并写回世界的决策条数。
    pub applied: u64,
    /// 被校验层丢弃的条数。
    pub dropped: u64,
    /// 回退到算法的次数（超时 / 非法 JSON / 全被丢弃）。
    pub fallbacks: u64,
}

impl LayerStats {
    /// 这一层的成功率（0.0~1.0）。没调过算 1.0——「没出错」比「没数据」更贴切。
    pub fn success_rate(&self) -> f32 {
        if self.calls == 0 {
            return 1.0;
        }
        let ok = self.calls.saturating_sub(self.failures);
        ok as f32 / self.calls as f32
    }
}

/// 统一编排器。**系统里唯一的 LLM 调用出口。**
pub struct AiOrchestrator {
    /// 三层共享同一个 HTTP 客户端（内部是 Arc，clone 便宜）。
    client: Option<LlmClient>,
    /// 各层独立的节流器。
    throttles: [Throttle; 3],
    /// 当前在飞的那一层。同一时刻只允许一个请求。
    inflight: Option<AiLayer>,
    /// 各层的开关。关掉的层不参与调度（控制台「三层开关」/ 未实现的层先关）。
    enabled: [bool; 3],
    /// 预算所属的「天」。跨天自动重置 `calls_today`。
    day: u32,
    /// 今日总调用数（跨层累计）。
    calls_today: u32,
    max_calls_per_day: u32,
    /// 各层统计。
    layer_stats: [LayerStats; 3],
    /// 最近一次的说明，给控制端。
    last_note: String,
}

impl std::fmt::Debug for AiOrchestrator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiOrchestrator")
            .field("enabled", &self.client.is_some())
            .field("inflight", &self.inflight)
            .field("calls_today", &self.calls_today)
            .field("max_calls_per_day", &self.max_calls_per_day)
            .finish()
    }
}

impl AiOrchestrator {
    /// 没有客户端 = 关闭状态。`next_due` 永远返回 `None`，所有层退回算法。
    pub fn new(client: Option<LlmClient>) -> Self {
        Self::with_budget(client, DEFAULT_MAX_CALLS_PER_DAY)
    }

    pub fn with_budget(client: Option<LlmClient>, max_calls_per_day: u32) -> Self {
        Self {
            client,
            throttles: std::array::from_fn(|_| Throttle::new(u32::MAX)),
            inflight: None,
            enabled: [true; 3],
            day: 0,
            calls_today: 0,
            max_calls_per_day,
            layer_stats: [LayerStats::default(); 3],
            last_note: String::new(),
        }
    }

    /// 从一份显式配置建（控制台填的 Key 走这条路）。没 Key 就是关闭状态。
    pub fn from_config(cfg: &LlmConfig) -> Self {
        Self::new(LlmClient::new(cfg.clone()).ok())
    }

    /// 显式关掉。
    pub fn disabled() -> Self {
        Self::new(None)
    }

    pub fn is_enabled(&self) -> bool {
        self.client.is_some()
    }

    /// 共享的 HTTP 客户端。各层 brain 用它发请求。
    pub fn client(&self) -> Option<&LlmClient> {
        self.client.as_ref()
    }

    pub fn max_calls_per_day(&self) -> u32 {
        self.max_calls_per_day
    }

    /// 今日已用额度。**跨天自动从零算起**，所以它是 `&self` 的只读判断。
    pub fn calls_today(&self, day: u32) -> u32 {
        self.calls_today_for(day)
    }

    /// 还剩几次额度。
    pub fn remaining_budget(&self, day: u32) -> u32 {
        self.max_calls_per_day
            .saturating_sub(self.calls_today_for(day))
    }

    fn calls_today_for(&self, day: u32) -> u32 {
        if self.day == day { self.calls_today } else { 0 }
    }

    pub fn inflight(&self) -> Option<AiLayer> {
        self.inflight
    }

    /// 这一层是否启用。关掉的层不参与调度。
    pub fn layer_enabled(&self, layer: AiLayer) -> bool {
        self.enabled[layer.index()]
    }

    /// 开 / 关一层。控制台的三层开关与「未实现的层先关」都走这里。
    pub fn set_layer_enabled(&mut self, layer: AiLayer, on: bool) {
        self.enabled[layer.index()] = on;
    }

    pub fn stats(&self, layer: AiLayer) -> LayerStats {
        self.layer_stats[layer.index()]
    }

    pub fn all_stats(&self) -> [LayerStats; 3] {
        self.layer_stats
    }

    pub fn last_note(&self) -> &str {
        &self.last_note
    }

    /// 记一条说明，给控制端解释「这一轮为什么没调 / 调了什么」。
    pub fn note(&mut self, note: impl Into<String>) {
        self.last_note = note.into();
    }

    /// 清空全部节流与统计（换局时用——旧的间隔记录对应的是旧世界）。
    pub fn reset(&mut self) {
        for t in &mut self.throttles {
            t.reset();
        }
        self.inflight = None;
        self.calls_today = 0;
        self.layer_stats = [LayerStats::default(); 3];
        self.last_note.clear();
    }

    /// **这一 tick 该不该调？调哪一层？** 只读判断，可以在 tick 循环里放心调。
    ///
    /// 优先级：Director > Station > Rider。高优先级的层到了间隔就该它调，
    /// 不被低优先级挤占。
    pub fn next_due(&self, world: &World) -> Option<AiLayer> {
        // 没客户端：没什么可调的
        self.client.as_ref()?;
        // 同一时刻只允许一个在飞
        if self.inflight.is_some() {
            return None;
        }
        // 今日总预算封顶（跨层累计）
        if self.calls_today_for(world.day) >= self.max_calls_per_day {
            return None;
        }
        // 按优先级顺序检查
        AiLayer::ALL.into_iter().find(|&l| self.layer_due(l, world))
    }

    /// 这一层到点了吗？
    fn layer_due(&self, layer: AiLayer, world: &World) -> bool {
        if !self.enabled[layer.index()] {
            return false;
        }
        let t = &self.throttles[layer.index()];
        t.minutes_since_last(world.day, world.sim_minute) >= layer.min_interval_min()
    }

    /// 占坑：标记某层开始调，并扣一次总额度。**必须在真正发请求之前调用**——
    /// 否则一次超时会导致同一间隔内反复重试。
    ///
    /// 返回 `false` 表示这次没占上（已有在飞请求，或预算用完），调用方应放弃。
    pub fn begin(&mut self, layer: AiLayer, world: &World) -> bool {
        if self.inflight.is_some() {
            return false;
        }
        if self.calls_today_for(world.day) >= self.max_calls_per_day {
            return false;
        }
        self.roll_to(world.day);
        self.throttles[layer.index()].begin(world.day, world.sim_minute);
        self.calls_today += 1;
        self.layer_stats[layer.index()].calls += 1;
        self.inflight = Some(layer);
        true
    }

    /// **跳过一层的这一轮**：推进它的间隔记账，但不扣预算、不占并发、不记调用。
    ///
    /// 给「搭车」的层用——站长层的限单 / 强派跟着骑手层那一轮请求一起下发
    /// （见 [`super::protocol`] 的 `station_ops`），它自己没有请求要发。必须记掉
    /// 间隔：不然它永远「到期」，`next_due` 每次都返回它，骑手层再也轮不上。
    pub fn skip_interval(&mut self, layer: AiLayer, world: &World) {
        self.roll_to(world.day);
        self.throttles[layer.index()].skip(world.day, world.sim_minute);
    }

    /// 收尾：清掉在飞标记。HTTP 结果回来后（无论成败）都要调。
    pub fn finish(&mut self, layer: AiLayer) {
        if self.inflight == Some(layer) {
            self.inflight = None;
        }
    }

    /// 记一次失败，并清除在飞标记（失败也是一轮的结束）。
    pub fn fail(&mut self, layer: AiLayer) {
        self.layer_stats[layer.index()].failures += 1;
        self.finish(layer);
    }

    /// 记一次回退到算法。
    pub fn record_fallback(&mut self, layer: AiLayer) {
        self.layer_stats[layer.index()].fallbacks += 1;
    }

    /// 记一轮的采纳 / 丢弃条数。
    pub fn record_verdict(&mut self, layer: AiLayer, applied: u64, dropped: u64) {
        let s = &mut self.layer_stats[layer.index()];
        s.applied += applied;
        s.dropped += dropped;
    }

    /// 跨天重置额度与间隔记录。
    fn roll_to(&mut self, day: u32) {
        if self.day != day {
            self.day = day;
            self.calls_today = 0;
            for t in &mut self.throttles {
                t.reset();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{config::SimConfig, world::World};

    fn world() -> World {
        World::new_for_test(SimConfig::default(), 7)
    }

    /// 造一个「有客户端」的编排器。构建 reqwest Client 不发网络，离线可用。
    fn orchestrated(budget: u32) -> AiOrchestrator {
        let client = LlmClient::new(LlmConfig::new("test-key")).ok();
        AiOrchestrator::with_budget(client, budget)
    }

    #[test]
    fn disabled_orchestrator_never_schedules() {
        let w = world();
        let orch = AiOrchestrator::disabled();
        assert!(!orch.is_enabled());
        assert_eq!(orch.next_due(&w), None);
        assert_eq!(orch.remaining_budget(w.day), DEFAULT_MAX_CALLS_PER_DAY);
    }

    #[test]
    fn priority_orders_director_over_station_over_rider() {
        assert_eq!(AiLayer::Director.priority(), 0);
        assert_eq!(AiLayer::Station.priority(), 1);
        assert_eq!(AiLayer::Rider.priority(), 2);
        // ALL 就是按优先级排好的
        assert_eq!(
            AiLayer::ALL.map(|l| l.priority()),
            [0, 1, 2],
            "ALL 必须按优先级降序排列"
        );
    }

    #[test]
    fn intervals_match_the_design() {
        assert_eq!(AiLayer::Director.min_interval_min(), 60);
        assert_eq!(AiLayer::Station.min_interval_min(), 20);
        assert_eq!(AiLayer::Rider.min_interval_min(), 8);
    }

    #[test]
    fn first_tick_schedules_the_highest_priority_layer() {
        let w = world();
        let orch = orchestrated(96);
        // 从没见过任何层 → 全部到期 → 返回最优先的 Director
        assert_eq!(orch.next_due(&w), Some(AiLayer::Director));
    }

    #[test]
    fn a_layer_is_not_due_again_until_its_interval_elapses() {
        let mut w = world();
        w.day = 1;
        w.sim_minute = 0;
        let mut orch = orchestrated(96);

        assert!(orch.begin(AiLayer::Director, &w));
        orch.finish(AiLayer::Director);

        // 刚调完：60 分钟间隔内不再轮到它
        assert!(!orch.layer_due(AiLayer::Director, &w));
        w.sim_minute = 59;
        assert!(!orch.layer_due(AiLayer::Director, &w));
        w.sim_minute = 60;
        assert!(orch.layer_due(AiLayer::Director, &w));
    }

    #[test]
    fn higher_priority_is_checked_before_lower_when_both_due() {
        let mut w = world();
        w.day = 1;
        w.sim_minute = 0;
        let mut orch = orchestrated(96);

        // 让 Director 刚调过（距离下次还有 60 分钟），于是轮到 Station
        assert!(orch.begin(AiLayer::Director, &w));
        orch.finish(AiLayer::Director);

        assert_eq!(
            orch.next_due(&w),
            Some(AiLayer::Station),
            "Director 未到期时该轮到次优先的 Station"
        );

        // 站长也调过之后轮到骑手
        assert!(orch.begin(AiLayer::Station, &w));
        orch.finish(AiLayer::Station);
        assert_eq!(orch.next_due(&w), Some(AiLayer::Rider));

        // 三个都调过 → 这一 tick 没事干
        assert!(orch.begin(AiLayer::Rider, &w));
        orch.finish(AiLayer::Rider);
        assert_eq!(orch.next_due(&w), None);
    }

    #[test]
    fn only_one_request_may_be_inflight() {
        let w = world();
        let mut orch = orchestrated(96);

        assert!(orch.begin(AiLayer::Director, &w));
        // 已经有在飞的请求：什么都不调度，也占不到新坑
        assert!(orch.inflight().is_some());
        assert_eq!(orch.next_due(&w), None);
        assert!(!orch.begin(AiLayer::Station, &w), "在飞时不能再占坑");

        orch.finish(AiLayer::Director);
        assert_eq!(orch.inflight(), None);
        // 收尾后又能调度了（Station 还没调过）
        assert_eq!(orch.next_due(&w), Some(AiLayer::Station));
    }

    #[test]
    fn total_budget_caps_across_layers() {
        let mut w = world();
        w.day = 1;
        let mut orch = orchestrated(2);

        assert!(orch.begin(AiLayer::Director, &w));
        orch.finish(AiLayer::Director);
        // 逐个间隔推进，让每层都可再调
        w.sim_minute += 60;
        assert!(orch.begin(AiLayer::Station, &w));
        orch.finish(AiLayer::Station);

        assert_eq!(orch.calls_today(w.day), 2);
        assert_eq!(orch.remaining_budget(w.day), 0);
        // 额度用完：谁都不再调度
        w.sim_minute += 60;
        assert_eq!(orch.next_due(&w), None);
        assert!(!orch.begin(AiLayer::Rider, &w), "预算用完不该能占坑");
    }

    #[test]
    fn budget_resets_on_a_new_day() {
        let mut w = world();
        w.day = 1;
        let mut orch = orchestrated(1);

        assert!(orch.begin(AiLayer::Director, &w));
        orch.finish(AiLayer::Director);
        assert_eq!(orch.remaining_budget(1), 0);
        assert_eq!(orch.next_due(&w), None);

        // 跨天：额度与间隔记录都清空，Director 又能调
        w.day = 2;
        w.sim_minute = 0;
        assert_eq!(orch.remaining_budget(2), 1);
        assert_eq!(orch.next_due(&w), Some(AiLayer::Director));
    }

    #[test]
    fn layer_stats_accumulate_independently() {
        let w = world();
        let mut orch = orchestrated(96);

        assert!(orch.begin(AiLayer::Rider, &w));
        orch.fail(AiLayer::Rider);
        orch.record_fallback(AiLayer::Rider);
        orch.record_verdict(AiLayer::Rider, 3, 1);

        let s = orch.stats(AiLayer::Rider);
        assert_eq!(s.calls, 1);
        assert_eq!(s.failures, 1);
        assert_eq!(s.fallbacks, 1);
        assert_eq!(s.applied, 3);
        assert_eq!(s.dropped, 1);
        assert_eq!(s.success_rate(), 0.0);

        // 其它层不受影响
        assert_eq!(orch.stats(AiLayer::Station), LayerStats::default());
        // 失败已经清掉了在飞标记
        assert_eq!(orch.inflight(), None);
    }

    #[test]
    fn a_disabled_layer_is_skipped() {
        let w = world();
        let mut orch = orchestrated(96);
        // 关掉最高优先级的 Director，就该轮到 Station
        orch.set_layer_enabled(AiLayer::Director, false);
        assert!(!orch.layer_enabled(AiLayer::Director));
        assert_eq!(orch.next_due(&w), Some(AiLayer::Station));

        // 三层全关 → 什么都不调
        for l in AiLayer::ALL {
            orch.set_layer_enabled(l, false);
        }
        assert_eq!(orch.next_due(&w), None);
    }

    /// 站长层「搭车」时靠 `skip_interval` 记掉自己的间隔：
    /// 它不该扣预算，也不该一直到期把骑手层饿死。
    #[test]
    fn a_skipped_layer_does_not_starve_the_next_one() {
        let mut w = world();
        w.day = 1;
        w.sim_minute = 0;
        let mut orch = orchestrated(96);

        // Director 刚调过、Station 还没调过 → 轮到 Station
        assert!(orch.begin(AiLayer::Director, &w));
        orch.finish(AiLayer::Director);
        assert_eq!(orch.next_due(&w), Some(AiLayer::Station));

        // 站长搭骑手层的车：记掉站长的间隔，不占坑不扣额度
        orch.skip_interval(AiLayer::Station, &w);
        assert_eq!(orch.calls_today(w.day), 1, "跳过不算调用");
        assert_eq!(orch.inflight(), None, "跳过不占并发");
        assert!(
            !orch.layer_due(AiLayer::Station, &w),
            "站长这一轮已经记掉了"
        );

        // 于是紧接着就轮到骑手层，而不是被站长堵住
        assert_eq!(orch.next_due(&w), Some(AiLayer::Rider));

        // 20 分钟后站长又该记一轮
        w.sim_minute = 20;
        assert!(orch.layer_due(AiLayer::Station, &w));
    }

    #[test]
    fn layer_keys_round_trip() {
        for l in AiLayer::ALL {
            assert_eq!(AiLayer::from_key(l.key()), Some(l));
        }
        assert_eq!(AiLayer::from_key("nonsense"), None);
    }
}
