//! LLM 战术大脑（技术设计 §7.3，默认关）。
//!
//! ## 它做什么
//!
//! 每小时（模拟时间）看一眼「空闲骑手 × 待抢订单」，给出一份分配计划。
//! 一天最多 24 次。**不是每 tick**——每 tick 调 LLM 既慢又贵，而且违背
//! 「随机与快决策归算法」这条边界（玩法设计 §九）。
//!
//! ## 三条不可动摇的规则
//!
//! 1. **超时 3 秒 / 非法 JSON → 放弃本轮**，由 [`crate::sim::ai::batch_decide`] 兜底；
//! 2. **LLM 的输出必须过 [`validate_llm_plan`]**，骑手不存在 / 订单不存在 /
//!    重复分配一律丢弃；
//! 3. **LLM 不参与任何随机数**。它只影响「谁接哪一单」，不影响金额、事故、
//!    天气——否则 10 万 tick 的可复现性就没了。

use std::time::Instant;

use crate::sim::{
    OrderId, RiderId,
    ai::{self, Assignment, LlmPlan, LlmVerdict},
    geo::haversine_m,
    world::World,
};

use super::client::LlmClient;

/// 一天最多调用次数。设计给的硬上限。
pub const MAX_CALLS_PER_DAY: u32 = 24;

/// 塞进 prompt 的空闲骑手上限。再多没有意义——10×5 的规模贪心就够。
const MAX_RIDERS_IN_PROMPT: usize = 8;
/// 塞进 prompt 的待抢订单上限。
const MAX_ORDERS_IN_PROMPT: usize = 6;

/// 这一轮的计划是从哪来的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BrainSource {
    /// LLM 给了合法计划，且通过校验。
    Llm,
    /// 没配 Key / 开关是关的。
    #[default]
    Disabled,
    /// 本时段已调过，或今日额度用完，或没有可分配的东西。
    Throttled,
    /// 调了但失败（超时 / 非法 JSON / 全被丢弃），交给算法。
    Fallback,
}

impl BrainSource {
    pub fn as_str(self) -> &'static str {
        match self {
            BrainSource::Llm => "llm",
            BrainSource::Disabled => "disabled",
            BrainSource::Throttled => "throttled",
            BrainSource::Fallback => "fallback",
        }
    }
}

/// 一轮大脑的输出。
#[derive(Debug, Clone, Default)]
pub struct BrainOutcome {
    pub verdict: LlmVerdict,
    pub source: BrainSource,
    /// 失败或跳过的原因，给控制端显示。
    pub note: String,
    pub model: String,
    pub latency_ms: u64,
}

impl BrainOutcome {
    fn skipped(source: BrainSource, note: impl Into<String>) -> Self {
        Self {
            source,
            note: note.into(),
            ..Default::default()
        }
    }

    /// 有效的分配。空 `Vec` 表示「这轮用算法」。
    pub fn assignments(&self) -> &[Assignment] {
        &self.verdict.assignments
    }

    pub fn used_llm(&self) -> bool {
        self.source == BrainSource::Llm && !self.verdict.assignments.is_empty()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BrainStats {
    /// 真正打出去的 LLM 请求数。
    pub calls: u64,
    pub failures: u64,
    pub throttled: u64,
    /// 采纳的分配条数。
    pub applied: u64,
    /// 被校验丢弃的条数。
    pub dropped: u64,
}

/// 节流器：**「每时段 1 次、一天最多 24 次」这条约束的全部实现。**
///
/// 抽成独立结构体是为了能纯函数地测——节流逻辑错了，要么烧钱，
/// 要么 LLM 白开。这里不碰网络，只做算术。
#[derive(Debug, Clone)]
pub struct Throttle {
    day: u32,
    calls_today: u32,
    /// (天, 小时槽)。小时槽 = `sim_minute / 60`。
    last_slot: Option<(u32, u32)>,
    max_calls_per_day: u32,
}

impl Default for Throttle {
    fn default() -> Self {
        Self::new(MAX_CALLS_PER_DAY)
    }
}

impl Throttle {
    pub fn new(max_calls_per_day: u32) -> Self {
        Self {
            day: 0,
            calls_today: 0,
            last_slot: None,
            max_calls_per_day,
        }
    }

    pub fn calls_today(&self) -> u32 {
        self.calls_today
    }

    pub fn max_calls_per_day(&self) -> u32 {
        self.max_calls_per_day
    }

    /// 跨天自动重置。
    fn roll_to(&mut self, day: u32) {
        if self.day != day {
            self.day = day;
            self.calls_today = 0;
            self.last_slot = None;
        }
    }

    /// 现在该不该跳过？`Some(原因)` = 跳过。
    ///
    /// 检查顺序即优先级：本时段已用过 > 今日额度用完 > 没东西可分。
    ///
    /// 这是个 `&self` 的只读判断，所以**必须自己处理跨天**：
    /// 否则第 2 天会继续被第 1 天用掉的 24 次额度拦住，LLM 再也叫不醒。
    pub fn check(&self, day: u32, sim_minute: u32, nothing_to_do: bool) -> Option<String> {
        let slot = sim_minute / 60;
        // 昨天（或上一局）的记录一律不算数
        let same_day = self.day == day;
        if same_day {
            if let Some((_, s)) = self.last_slot
                && s == slot
            {
                return Some(format!("{slot} 点这一时段已经决策过了"));
            }
            if self.calls_today >= self.max_calls_per_day {
                return Some(format!(
                    "今日 LLM 调用已达上限 {} 次",
                    self.max_calls_per_day
                ));
            }
        }
        if nothing_to_do {
            return Some("没有可分配的骑手或订单".to_string());
        }
        None
    }

    /// 占掉本时段的坑。**必须在真正发请求之前调用**——
    /// 否则一次超时会导致同一小时内反复重试。
    pub fn begin(&mut self, day: u32, sim_minute: u32) {
        self.roll_to(day);
        self.last_slot = Some((day, sim_minute / 60));
        self.calls_today += 1;
    }

    pub fn slot_used(&self, day: u32, sim_minute: u32) -> bool {
        self.last_slot == Some((day, sim_minute / 60))
    }

    pub fn reset(&mut self) {
        self.calls_today = 0;
        self.last_slot = None;
    }
}

/// **阶段一**的产物：已经占好时段、备好提示词，就等一个 HTTP 结果。
///
/// 存在的意义是让 `app` 的 tick 循环**不必被 3 秒超时卡住**：
///
/// ```text
/// tick N   ：prepare() 占坑拿 prompt ──▶ 丢给后台任务去打 HTTP
/// tick N+1 ：finish(结果) ──▶ 拿到计划，塞进 TickInput::llm_plan
/// ```
///
/// 中间这段时间世界照常跑，因为计划本来就不是每 tick 都要的。
pub struct PreparedPlan {
    /// 发请求用的客户端（reqwest 客户端内部是 Arc，clone 很便宜）。
    pub client: LlmClient,
    system: String,
    user: String,
    started: Instant,
}

impl std::fmt::Debug for PreparedPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedPlan")
            .field("model", &self.client.model())
            .finish()
    }
}

impl PreparedPlan {
    pub fn model(&self) -> &str {
        self.client.model()
    }

    /// 拿提示词去打一次 HTTP。给后台任务用。
    ///
    /// **不返回 `Result`**：失败原因已经被压成字符串，一路走到
    /// [`TacticalBrain::finish`] 变成一句给控制端看的话。中间没人需要
    /// 区分「超时」还是「网络不通」——处理方式完全一样。
    pub async fn request(&self) -> PlanFetch {
        match self.client.complete_json(&self.system, &self.user).await {
            Ok(v) => PlanFetch::Json(v),
            Err(e) => PlanFetch::Failed(e.to_string()),
        }
    }
}

/// 一次 LLM 请求的结局。
#[derive(Debug)]
pub enum PlanFetch {
    /// HTTP 成功并解析出了 JSON。
    Json(serde_json::Value),
    /// 请求失败 / 超时 / 返回体不是合法 JSON。附可读原因。
    Failed(String),
}

/// 战术大脑。持有客户端与节流状态。
pub struct TacticalBrain {
    client: Option<LlmClient>,
    throttle: Throttle,
    stats: BrainStats,
    /// 最近一次失败的描述，给控制端。
    last_error: String,
}

impl std::fmt::Debug for TacticalBrain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TacticalBrain")
            .field("enabled", &self.client.is_some())
            .field("throttle", &self.throttle)
            .field("stats", &self.stats)
            .finish()
    }
}

impl TacticalBrain {
    /// 没有客户端 = 关闭状态。所有 `plan` 都会返回 [`BrainSource::Disabled`]。
    pub fn new(client: Option<LlmClient>) -> Self {
        Self {
            client,
            throttle: Throttle::default(),
            stats: BrainStats::default(),
            last_error: String::new(),
        }
    }

    /// 从环境变量建。没配 Key 就是关闭状态。
    pub fn from_env() -> Self {
        Self::new(LlmClient::from_env())
    }

    /// 显式关掉。
    pub fn disabled() -> Self {
        Self::new(None)
    }

    pub fn is_enabled(&self) -> bool {
        self.client.is_some()
    }

    pub fn stats(&self) -> BrainStats {
        self.stats
    }

    pub fn calls_today(&self) -> u32 {
        self.throttle.calls_today()
    }

    pub fn max_calls_per_day(&self) -> u32 {
        self.throttle.max_calls_per_day()
    }

    pub fn last_error(&self) -> &str {
        &self.last_error
    }

    pub fn model(&self) -> &str {
        self.client.as_ref().map(|c| c.model()).unwrap_or("")
    }

    /// 手动重置节流（换局时用）。
    pub fn reset_throttle(&mut self) {
        self.throttle.reset();
        self.last_error.clear();
    }

    /// 只读地看一眼节流器。
    pub fn throttle(&self) -> &Throttle {
        &self.throttle
    }

    /// 本时段是否已经调过。
    pub fn slot_used(&self, day: u32, sim_minute: u32) -> bool {
        self.throttle.slot_used(day, sim_minute)
    }

    /// **阶段一**：要不要调这一轮？要调的话占坑并把提示词装好。
    ///
    /// 同步、极快（只做算术和字符串拼接），**不打网络**。所以可以放心在
    /// tick 循环里调用。`Err` 里是「这轮不调」的完整结论，直接就是最终结果。
    pub fn prepare(
        &mut self,
        world: &World,
        idle: &[RiderId],
        pending: &[OrderId],
    ) -> Result<PreparedPlan, BrainOutcome> {
        let Some(client) = self.client.clone() else {
            return Err(BrainOutcome::skipped(
                BrainSource::Disabled,
                "未配置 LLM，使用算法决策",
            ));
        };

        if let Some(reason) = self.throttle.check(
            world.day,
            world.sim_minute,
            idle.is_empty() || pending.is_empty(),
        ) {
            self.stats.throttled += 1;
            return Err(BrainOutcome::skipped(BrainSource::Throttled, reason));
        }

        // 先占坑再调用：万一超时，本时段也不会重试到天亮。
        self.throttle.begin(world.day, world.sim_minute);
        self.stats.calls += 1;

        let (system, user) = build_prompt(world, idle, pending);
        Ok(PreparedPlan {
            client,
            system,
            user,
            started: Instant::now(),
        })
    }

    /// **阶段二**：把 HTTP 结果交回来，得到最终结论。
    ///
    /// 故意要求传 `world`：校验必须针对**当下**的世界状态，而不是占坑时那份
    /// 快照——这几秒里订单可能已经被抢走了。
    ///
    /// 返回值第二个元素是**可以直接塞进 `TickInput::llm_plan` 的计划**：
    /// 只有 LLM 确实给了一条以上合法分配时才是 `Some`。失败、超时、全被
    /// 丢弃一律 `None`，tick 里的 `run_ai` 自然会走算法兜底。
    pub fn finish(
        &mut self,
        prepared: PreparedPlan,
        fetched: PlanFetch,
        world: &World,
        idle: &[RiderId],
        pending: &[OrderId],
    ) -> (BrainOutcome, Option<LlmPlan>) {
        let latency_ms = prepared.started.elapsed().as_millis() as u64;
        let model = prepared.client.model().to_string();
        let skip = |source: BrainSource, note: String, model: String| {
            (
                BrainOutcome {
                    verdict: LlmVerdict::default(),
                    source,
                    note,
                    model,
                    latency_ms,
                },
                None,
            )
        };

        let value = match fetched {
            PlanFetch::Json(v) => v,
            PlanFetch::Failed(e) => {
                self.stats.failures += 1;
                self.last_error = e.clone();
                return skip(
                    BrainSource::Fallback,
                    format!("LLM 失败，回退算法：{e}"),
                    model,
                );
            }
        };

        let mut parsed: LlmPlan = match serde_json::from_value(value) {
            Ok(p) => p,
            Err(e) => {
                self.stats.failures += 1;
                self.last_error = e.to_string();
                return skip(
                    BrainSource::Fallback,
                    format!("LLM 输出不符合计划格式，回退算法：{e}"),
                    model,
                );
            }
        };
        parsed.model = model.clone();
        parsed.latency_ms = latency_ms;

        // 不信任任何输出：骑手/订单必须存在，且不能重复。
        let verdict = ai::validate_llm_plan(world, &parsed, idle, pending);
        self.stats.applied += verdict.assignments.len() as u64;
        self.stats.dropped += verdict.dropped as u64;

        if verdict.assignments.is_empty() {
            // 全部被丢弃 = 等于没给计划。**必须回退算法**，
            // 否则这一时段就白白空转了。
            self.stats.failures += 1;
            self.last_error = format!("LLM 计划 {} 条全部被校验丢弃", parsed.assignments.len());
            return skip(
                BrainSource::Fallback,
                format!(
                    "LLM 给了 {} 条分配，全部不合法（骑手/订单不存在或重复），回退算法",
                    parsed.assignments.len()
                ),
                model,
            );
        }

        let note = format!(
            "LLM 采纳 {} 条，丢弃 {} 条非法项",
            verdict.assignments.len(),
            verdict.dropped
        );
        parsed.note = note.clone();
        (
            BrainOutcome {
                verdict,
                source: BrainSource::Llm,
                note,
                model,
                latency_ms,
            },
            Some(parsed),
        )
    }

    /// 要一份计划（同步等待版）。
    ///
    /// **无论发生什么都不会返回 Err**——最差也是 `Fallback`，让算法接手。
    /// tick 循环请改用 `prepare` + `finish`，别在这里等网络。
    pub async fn plan(
        &mut self,
        world: &World,
        idle: &[RiderId],
        pending: &[OrderId],
    ) -> BrainOutcome {
        let prepared = match self.prepare(world, idle, pending) {
            Ok(p) => p,
            Err(outcome) => return outcome,
        };
        let fetched = prepared.request().await;
        self.finish(prepared, fetched, world, idle, pending).0
    }
}

/// 构造 (system, user) 提示词。
///
/// 抽成纯函数是为了能单测——**这里绝对不能有 IO**。
pub fn build_prompt(world: &World, idle: &[RiderId], pending: &[OrderId]) -> (String, String) {
    let system = "你是外卖平台的调度大脑。只输出 JSON，禁止任何解释、前后缀或 Markdown 代码块。\n\
         输出格式：{\"assignments\":[{\"rider_id\":\"r1\",\"order_id\":42,\"reason\":\"不超过20字的理由\"}]}\n\
         硬性约束：\n\
         1. rider_id 与 order_id 只能取自用户给出的两个列表，编造的会被丢弃；\n\
         2. 每个骑手最多分 1 单，每单最多分给 1 个骑手；\n\
         3. 宁缺毋滥：不划算的订单直接不给，assignments 允许为空数组；\n\
         4. reason 必须引用具体数字（距离 / 报酬 / 顺路度），不要写空话。".to_string();

    let mut user = String::new();
    user.push_str(&format!(
        "当前：第 {} 天 {:02}:{:02}，天气 {}（速度×{:.2}），路况系数 {:.2}\n",
        world.day,
        world.sim_minute / 60,
        world.sim_minute % 60,
        world.weather.label(),
        world.weather.speed_factor(),
        if world.cfg.is_peak(world.sim_minute) {
            1.3
        } else {
            1.0
        },
    ));

    user.push_str("空闲骑手：\n");
    let mut shown = 0;
    for rid in idle.iter().take(MAX_RIDERS_IN_PROMPT) {
        let Some(r) = world.rider(*rid) else { continue };
        user.push_str(&format!(
            "- {} | {} | 评分{:.2} | 手持{}单 | 时薪¥{:.1} | 位置({:.5},{:.5})\n",
            crate::sim::rider_key(r.id),
            r.speed_mode.label(),
            r.rating,
            r.held.len(),
            r.hourly_wage_cents() as f64 / 100.0,
            r.lat,
            r.lng,
        ));
        shown += 1;
    }
    if shown == 0 {
        user.push_str("- （无）\n");
    }

    user.push_str("待分配订单：\n");
    let mut shown_o = 0;
    for oid in pending.iter().take(MAX_ORDERS_IN_PROMPT) {
        let Some(o) = world.order(*oid) else { continue };
        // 从最近的空闲骑手看这单要跑多远——LLM 需要这个数才能谈「顺路」
        let nearest_km = idle
            .iter()
            .filter_map(|rid| world.rider(*rid))
            .map(|r| {
                haversine_m(
                    r.pos(),
                    crate::sim::geo::LatLng::new(o.merchant_lat, o.merchant_lng),
                )
            })
            .fold(f64::INFINITY, |acc: f64, d: f64| acc.min(d));
        let window = o.claim_deadline_min.saturating_sub(world.elapsed_min);
        user.push_str(&format!(
            "- #{} | {} | 送达距离{:.1}km | 报酬¥{:.2} | 承诺{}分钟 | 抢单窗口剩{}min | 最近骑手{:.2}km | 备注「{}」\n",
            o.id,
            o.merchant_name,
            o.distance_m as f64 / 1000.0,
            o.fee.quoted_fee_cents as f64 / 100.0,
            o.promised_minutes,
            window,
            if nearest_km.is_finite() { nearest_km / 1000.0 } else { -1.0 },
            o.remark,
        ));
        shown_o += 1;
    }
    if shown_o == 0 {
        user.push_str("- （无）\n");
    }

    user.push_str("请给出本轮分配计划。");
    (system, user)
}

/// 给单测用：造一个「假装调用了 LLM」的客户端不可行（需要真实 HTTP），
/// 所以这里只测纯函数部分——提示词构造与节流逻辑。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{config::SimConfig, world::World};

    fn world() -> World {
        World::new_for_test(SimConfig::default(), 42)
    }

    #[test]
    fn prompt_contains_both_lists_and_the_json_contract() {
        let w = world();
        let idle = w.idle_riders();
        let pending = w.pending_orders();
        let (system, user) = build_prompt(&w, &idle, &pending);
        assert!(system.contains("assignments"));
        assert!(system.contains("order_id"));
        assert!(system.contains("rider_id"));
        assert!(user.contains("空闲骑手"));
        assert!(user.contains("待分配订单"));
        assert!(user.contains("第 1 天"));
    }

    #[test]
    fn prompt_is_bounded_even_with_many_candidates() {
        let mut w = world();
        for _ in 0..30 {
            w.spawn_sim_rider();
        }
        let idle = w.idle_riders();
        let pending = w.pending_orders();
        let (_, user) = build_prompt(&w, &idle, &pending);
        let lines = user.lines().filter(|l| l.starts_with("- r")).count();
        assert!(
            lines <= MAX_RIDERS_IN_PROMPT,
            "骑手列表必须有上限，实际 {lines}"
        );
    }

    #[tokio::test]
    async fn disabled_brain_never_calls_out_and_says_so() {
        let w = world();
        let mut brain = TacticalBrain::disabled();
        assert!(!brain.is_enabled());
        let out = brain.plan(&w, &w.idle_riders(), &w.pending_orders()).await;
        assert_eq!(out.source, BrainSource::Disabled);
        assert!(out.assignments().is_empty());
        assert_eq!(brain.stats().calls, 0);
    }

    #[tokio::test]
    async fn disabled_brain_wins_over_throttling() {
        let w = world();
        let mut brain = TacticalBrain::disabled();
        brain.throttle.begin(w.day, w.sim_minute);
        assert!(brain.slot_used(w.day, w.sim_minute));
        let out = brain.plan(&w, &w.idle_riders(), &w.pending_orders()).await;
        // 没有客户端时优先级是 Disabled —— 连「节流」都轮不到
        assert_eq!(out.source, BrainSource::Disabled);
    }

    #[test]
    fn same_hour_is_only_decided_once() {
        let mut t = Throttle::default();
        assert_eq!(t.check(1, 8 * 60, false), None, "第一轮应该放行");
        t.begin(1, 8 * 60);
        assert_eq!(t.calls_today(), 1);
        assert!(
            t.check(1, 8 * 60 + 30, false).is_some(),
            "同一小时内必须拦住"
        );
        assert_eq!(t.check(1, 9 * 60, false), None, "下一小时应该放行");
    }

    #[test]
    fn daily_cap_blocks_extra_calls_in_a_new_slot() {
        // 用一个小的上限来测「日额度」这条分支，避免和「同时段去重」混在一起。
        let mut t = Throttle::new(3);
        for hour in 0..3 {
            assert!(
                t.check(1, hour * 60, false).is_none(),
                "第 {hour} 小时应放行"
            );
            t.begin(1, hour * 60);
        }
        assert_eq!(t.calls_today(), 3);
        // 第 4 个时段是全新的时段，但还是必须被日上限拦住
        let reason = t.check(1, 5 * 60, false).expect("超过日上限必须被拦住");
        assert!(reason.contains('3'), "原因里要说清上限：{reason}");
        // 换一天就又能用了
        assert_eq!(t.check(2, 5 * 60, false), None);
    }

    #[test]
    fn full_day_of_slots_then_exhausted() {
        let mut t = Throttle::new(MAX_CALLS_PER_DAY);
        for hour in 0..MAX_CALLS_PER_DAY {
            assert!(
                t.check(1, hour * 60, false).is_none(),
                "第 {hour} 小时应放行"
            );
            t.begin(1, hour * 60);
        }
        assert_eq!(t.calls_today(), 24);
        assert!(
            t.check(1, 23 * 60 + 30, false).is_some(),
            "同小时重复必须拦住"
        );
    }

    #[test]
    fn nothing_to_do_does_not_burn_the_slot() {
        let t = Throttle::default();
        assert!(t.check(1, 600, true).is_some());
        // 关键：跳过的这一轮**不该**占用额度，下一分钟有单了还能用
        assert_eq!(t.calls_today(), 0);
        assert_eq!(t.check(1, 601, false), None);
    }

    #[test]
    fn throttle_resets_when_day_changes() {
        let mut t = Throttle::default();
        for hour in 0..24 {
            t.begin(1, hour * 60);
        }
        assert_eq!(t.calls_today(), 24);
        // 新的一天：同一小时（12 点）不该被昨天的记录挡住
        assert_eq!(t.check(2, 12 * 60, false), None);
        t.begin(2, 12 * 60);
        assert_eq!(t.calls_today(), 1);
        assert!(t.slot_used(2, 12 * 60));
        assert!(!t.slot_used(1, 12 * 60));
    }

    #[test]
    fn reset_clears_everything() {
        let mut t = Throttle::default();
        t.begin(3, 5 * 60);
        t.reset();
        assert_eq!(t.calls_today(), 0);
        assert!(!t.slot_used(3, 5 * 60));
    }

    #[test]
    fn daily_cap_is_the_documented_24() {
        assert_eq!(MAX_CALLS_PER_DAY, 24);
    }
}
