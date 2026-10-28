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

use lru::LruCache;

use crate::sim::{
    OrderId, RiderId,
    ai::{self, Assignment, LlmAssign, LlmPlan, LlmRefusal, LlmVerdict},
    world::{RefusalKind, World},
};

use super::{
    client::{LlmClient, LlmConfig},
    protocol::{
        self, BrainRequest, DecisionResponse, TrimStats, ValidatedPolicy, ValidatedStationOp,
    },
};

/// 一天最多调用次数。设计给的硬上限。
pub const MAX_CALLS_PER_DAY: u32 = 24;

/// 状态指纹缓存容量。1 次/时段 × 24 时段，留一倍余量。
const FINGERPRINT_CACHE_CAP: usize = 48;
/// 缓存的最长寿命（模拟分钟）。超过就不敢复用了——世界早就不是那个世界了。
const CACHE_TTL_MIN: u32 = 30;

/// 这一轮的计划是从哪来的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BrainSource {
    /// LLM 给了合法计划，且通过校验。
    Llm,
    /// 状态指纹命中缓存，**没花这次调用**。
    Cache,
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
            BrainSource::Cache => "cache",
            BrainSource::Disabled => "disabled",
            BrainSource::Throttled => "throttled",
            BrainSource::Fallback => "fallback",
        }
    }
}

/// 一条已落地的站长指令摘要。`detail` 是人类可读的补充（封顶值 / 订单号）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StationOpBrief {
    pub op: &'static str,
    pub rider: String,
    pub detail: String,
    pub why: String,
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
    /// 这一轮下发的策略调整（已钳制）。控制端用它解释「骑手行为怎么变了」。
    pub policies: Vec<ValidatedPolicy>,
    /// 这一轮落地的站长指令摘要（给控制端 / 事件用）。
    pub station_ops: Vec<StationOpBrief>,
    /// 状态指纹。相同指纹 = 相同决策输入，用于缓存与复现。
    pub fingerprint: u64,
    /// 请求裁剪统计。
    pub trim: TrimStats,
    /// 请求里塞了几个骑手 / 几单（控制端显示 prompt 规模）。
    pub prompt_riders: usize,
    pub prompt_orders: usize,
    pub prompt_bytes: usize,
    /// 这一轮被丢掉的所有意图及原因（协议层 + tick 复核）。
    ///
    /// 合并成一个列表是为了让「模型说了什么、为什么没执行」能在一处查完；
    /// 计数已经包含在 `stats.dropped` 里，别再累加一次。
    pub dropped: Vec<ai::DroppedIntent>,
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
        // 指纹命中的也是「LLM 的决策」，只是这轮没再花一次调用
        matches!(self.source, BrainSource::Llm | BrainSource::Cache)
            && !self.verdict.assignments.is_empty()
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
    /// 状态指纹命中、省下来的调用数。
    pub cached_hits: u64,
    /// 落地的策略调整条数。
    pub policies_applied: u64,
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
    /// (天, 模拟分钟)。**精确到分钟**，供多层编排器按间隔节流用。
    last_minute: Option<(u32, u32)>,
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
            last_minute: None,
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
            self.last_minute = None;
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
        self.last_minute = Some((day, sim_minute));
        self.calls_today += 1;
    }

    /// 记掉这一轮但**不算一次调用**。
    ///
    /// 给「搭车」的层用：站长层的限单 / 强派跟着骑手层那一轮请求一起下发
    /// （见 `llm::protocol` 的 `station_ops`），站长自己没有请求要发。但它同样要
    /// 推进间隔——否则它会永远「到期」（`minutes_since_last == u32::MAX`），
    /// 把真正能跑的骑手层饿死。
    pub fn skip(&mut self, day: u32, sim_minute: u32) {
        self.roll_to(day);
        self.last_slot = Some((day, sim_minute / 60));
        self.last_minute = Some((day, sim_minute));
    }

    pub fn slot_used(&self, day: u32, sim_minute: u32) -> bool {
        self.last_slot == Some((day, sim_minute / 60))
    }

    /// 距上次调用过了多少模拟分钟。**只读**，不会 roll。
    ///
    /// - 从没调过 → `u32::MAX`（立刻到期）；
    /// - 跨天（`day` 比记录晚）→ `u32::MAX`（新的一天从零算起）。
    pub fn minutes_since_last(&self, day: u32, sim_minute: u32) -> u32 {
        match self.last_minute {
            None => u32::MAX,
            Some((d, m)) => match day.cmp(&d) {
                std::cmp::Ordering::Greater => u32::MAX,
                std::cmp::Ordering::Less => 0,
                std::cmp::Ordering::Equal => sim_minute.saturating_sub(m),
            },
        }
    }

    pub fn reset(&mut self) {
        self.calls_today = 0;
        self.last_slot = None;
        self.last_minute = None;
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
    /// 这一轮的请求载荷。**留着它**：finish 要拿它算指纹、写缓存、给控制端展示。
    req: BrainRequest,
    /// 状态指纹。
    fingerprint: u64,
    /// 指纹命中缓存时，直接复用这份响应，**不发 HTTP、不占用今日额度**。
    cached: Option<Box<DecisionResponse>>,
    started: Instant,
}

impl std::fmt::Debug for PreparedPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedPlan")
            .field("model", &self.client.model())
            .field("fingerprint", &self.fingerprint)
            .field("cached", &self.cached.is_some())
            .finish()
    }
}

impl PreparedPlan {
    pub fn model(&self) -> &str {
        self.client.model()
    }

    /// 这一轮的请求载荷（只读）。控制端「AI 决策流」用它展示喂了什么。
    pub fn request_payload(&self) -> &BrainRequest {
        &self.req
    }

    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    /// 这一次是不是走的缓存（没打 HTTP）。
    pub fn is_cached(&self) -> bool {
        self.cached.is_some()
    }

    /// 拿提示词去打一次 HTTP。给后台任务用。
    ///
    /// 缓存命中时**直接把上次的响应还回来**——`async` 立刻返回，不会碰网络。
    ///
    /// **不返回 `Result`**：失败原因已经被压成字符串，一路走到
    /// [`TacticalBrain::finish`] 变成一句给控制端看的话。中间没人需要
    /// 区分「超时」还是「网络不通」——处理方式完全一样。
    pub async fn request(&self) -> PlanFetch {
        if let Some(cached) = &self.cached {
            return PlanFetch::Json(
                serde_json::to_value(cached.as_ref()).unwrap_or(serde_json::Value::Null),
            );
        }
        let system = protocol::system_prompt();
        let user = protocol::user_prompt(&self.req);
        match self.client.complete_json(system, &user).await {
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

/// 缓存里的一条决策。
#[derive(Debug, Clone)]
struct CachedDecision {
    day: u32,
    sim_minute: u32,
    resp: DecisionResponse,
}

/// 战术大脑。持有客户端与节流状态。
pub struct TacticalBrain {
    client: Option<LlmClient>,
    throttle: Throttle,
    stats: BrainStats,
    /// 最近一次失败的描述，给控制端。
    last_error: String,
    /// 状态指纹 → 上一轮响应。命中就省一次调用（《LLM协议设计》§6.3）。
    cache: LruCache<u64, CachedDecision>,
}

impl std::fmt::Debug for TacticalBrain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TacticalBrain")
            .field("enabled", &self.client.is_some())
            .field("throttle", &self.throttle)
            .field("stats", &self.stats)
            .field("cache_len", &self.cache.len())
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
            cache: LruCache::new(
                std::num::NonZeroUsize::new(FINGERPRINT_CACHE_CAP).expect("容量非零"),
            ),
        }
    }

    /// 查一眼缓存。**顺带做过期检查**——过期的当作没有。
    fn cache_lookup(
        &self,
        fingerprint: u64,
        day: u32,
        sim_minute: u32,
    ) -> Option<DecisionResponse> {
        let hit = self.cache.peek(&fingerprint)?;
        if hit.day != day {
            return None;
        }
        let age = sim_minute.saturating_sub(hit.sim_minute);
        if age > CACHE_TTL_MIN {
            return None;
        }
        Some(hit.resp.clone())
    }

    fn cache_store(
        &mut self,
        fingerprint: u64,
        day: u32,
        sim_minute: u32,
        resp: &DecisionResponse,
    ) {
        self.cache.put(
            fingerprint,
            CachedDecision {
                day,
                sim_minute,
                resp: resp.clone(),
            },
        );
    }

    /// 清空指纹缓存（换局 / 改配置时用——旧指纹对应的是旧世界）。
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    /// 从环境变量建。没配 Key 就是关闭状态。
    pub fn from_env() -> Self {
        Self::new(LlmClient::from_env())
    }

    /// 从一份显式配置建（控制台里填的 Key 走这条路）。
    ///
    /// 配置里没有 Key → 关闭状态；有 Key 但 HTTP 客户端建不起来 → 也是关闭状态。
    /// 两种失败都不报错，调用方退回算法即可。
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

        // 组装请求（含预算裁剪）。**必须在占坑之前算指纹**——
        // 否则命中缓存也白占了本时段的额度。
        let req = BrainRequest::build(world, idle, pending);
        if req.riders.is_empty() || req.orders.is_empty() {
            self.stats.throttled += 1;
            return Err(BrainOutcome::skipped(
                BrainSource::Throttled,
                "没有可放进 prompt 的骑手或订单",
            ));
        }
        let fingerprint = protocol::state_fingerprint(&req);

        // 状态指纹命中 → 复用上一轮响应，省钱也省延迟。
        if let Some(resp) = self.cache_lookup(fingerprint, world.day, world.sim_minute) {
            self.stats.cached_hits += 1;
            return Ok(PreparedPlan {
                client,
                req,
                fingerprint,
                cached: Some(Box::new(resp)),
                started: Instant::now(),
            });
        }

        // 先占坑再调用：万一超时，本时段也不会重试到天亮。
        self.throttle.begin(world.day, world.sim_minute);
        self.stats.calls += 1;

        Ok(PreparedPlan {
            client,
            req,
            fingerprint,
            cached: None,
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
        world: &mut World,
        idle: &[RiderId],
        pending: &[OrderId],
        hosted: &[RiderId],
    ) -> (BrainOutcome, Option<LlmPlan>) {
        let latency_ms = prepared.started.elapsed().as_millis() as u64;
        let model = prepared.client.model().to_string();
        let fingerprint = prepared.fingerprint;
        let was_cached = prepared.is_cached();
        let req = prepared.req;
        let day = world.day;
        let sim_minute = world.sim_minute;
        let prompt_riders = req.riders.len();
        let prompt_orders = req.orders.len();
        let prompt_bytes = req.body_bytes();

        // 统一的「这轮没计划」出口：把请求侧的元信息补齐，控制端照样能看到喂了什么。
        macro_rules! skip {
            ($source:expr, $note:expr) => {
                skip!($source, $note, Vec::new())
            };
            ($source:expr, $note:expr, $drops:expr) => {{
                let note: String = $note;
                return (
                    BrainOutcome {
                        verdict: LlmVerdict::default(),
                        source: $source,
                        note,
                        model,
                        latency_ms,
                        policies: Vec::new(),
                        station_ops: Vec::new(),
                        fingerprint,
                        trim: TrimStats::default(),
                        prompt_riders,
                        prompt_orders,
                        prompt_bytes,
                        dropped: $drops,
                    },
                    None,
                );
            }};
        }

        let value = match fetched {
            PlanFetch::Json(v) => v,
            PlanFetch::Failed(e) => {
                self.stats.failures += 1;
                self.last_error = e.clone();
                skip!(BrainSource::Fallback, format!("LLM 失败，回退算法：{e}"));
            }
        };

        // ① 结构校验：JSON 都解不出来就整轮放弃。
        let resp: DecisionResponse = match serde_json::from_value(value) {
            Ok(r) => r,
            Err(e) => {
                self.stats.failures += 1;
                self.last_error = e.to_string();
                skip!(
                    BrainSource::Fallback,
                    format!("LLM 输出不符合决策协议，回退算法：{e}")
                );
            }
        };

        // ②③④ 引用 / 语义 / 钳制：全在协议层，查的是**当下**的世界。
        //
        // `hosted` 是本轮 AutoPilot 代管的真人：托管中的真人允许被 AI 派单，
        // 在线的真人**不在这里放行**（`roster` 也压根不含他们）。
        let verdict = protocol::validate_decision(world, &resp, idle, hosted);
        self.stats.dropped += verdict.dropped() as u64;

        if verdict.is_empty() {
            let why = verdict.rejected.unwrap_or_else(|| "无有效条目".into());
            self.stats.failures += 1;
            self.last_error = why.clone();
            // 丢弃明细要带出去：否则「模型说了 5 条、平台一条没执行」查不到原因。
            skip!(
                BrainSource::Fallback,
                format!("{why}，回退算法"),
                verdict.drops.clone()
            );
        }

        // 策略层落地（唯一一处写回世界的地方）。
        let policies = apply_policies(world, &verdict.policies);
        self.stats.policies_applied += policies.len() as u64;

        // 站长指令落地：cap / force_offline / recall_training 立即生效。
        let station_ops = apply_station_ops(world, &verdict.station_ops);

        // 把通过校验的 claim 折成 LlmPlan，交给 tick 的 run_ai。
        // tick 那边会**再校验一遍**（订单可能已被抢走）——这是有意的双保险。
        let mut assignments: Vec<LlmAssign> = verdict
            .acts
            .iter()
            .filter(|a| a.act == protocol::ActKind::Claim)
            .map(|a| LlmAssign {
                rider_id: crate::sim::rider_key(a.rider),
                order_id: a.order,
                reason: a.why.clone(),
            })
            .collect();
        // 拒单 / 观望：**不再被静默丢弃**。它们会变成 tick 里的短期排除项，
        // 明确告诉本地算法「这一单 / 这一轮别再塞给他」。
        let mut refusals: Vec<LlmRefusal> = Vec::new();
        for a in &verdict.acts {
            let kind = match a.act {
                protocol::ActKind::Claim => continue,
                protocol::ActKind::Reject => RefusalKind::Reject,
                protocol::ActKind::Pass => RefusalKind::Pass,
            };
            if refusals
                .iter()
                .any(|r| r.rider == a.rider && r.order == a.order)
            {
                continue;
            }
            refusals.push(LlmRefusal {
                rider: a.rider,
                order: a.order,
                kind,
                reason: a.why.clone(),
            });
        }
        // 站长的强派也折进同一个计划——同样过 tick 的双保险校验。
        for op in &verdict.station_ops {
            if let ValidatedStationOp::ForceAssign { rider, order, why } = op {
                assignments.push(LlmAssign {
                    rider_id: crate::sim::rider_key(*rider),
                    order_id: *order,
                    reason: why.clone(),
                });
            }
        }
        let mut plan = LlmPlan {
            assignments,
            refusals,
            covered: Vec::new(),
            model: model.clone(),
            latency_ms,
            note: String::new(),
            decision_id: 0, // 由 app 侧在收到结果时填上真实 id
        };
        let verdict_final = ai::validate_llm_plan(world, &plan, idle, pending);
        // 覆盖范围由**校验通过**的决策决定：被丢掉的条目等于没发生，
        // 对应骑手回到「可被本地补位」。
        plan.covered = verdict_final.covered.clone();
        self.stats.applied += verdict_final.assignments.len() as u64;
        self.stats.dropped += verdict_final.dropped() as u64;

        // 两层丢弃合并成一份：协议层挡掉的 + tick 复核挡掉的。
        let mut dropped = verdict.drops.clone();
        dropped.extend(verdict_final.drops.iter().cloned());

        // 缓存这一轮的响应（无论有没有 claim——策略也是一样可复用的）。
        self.cache_store(fingerprint, day, sim_minute, &resp);
        // 命中数在 `prepare` 里已经计过账，这里不能重复计。

        let source = if was_cached {
            BrainSource::Cache
        } else {
            BrainSource::Llm
        };
        let note = format!(
            "{}：{} 条动作（{} 条接单、{} 条拒/观望）、{} 条策略（{} 条落地）、{} 条站长指令、丢弃 {} 条{}",
            if was_cached {
                "指纹命中缓存"
            } else {
                "LLM 决策"
            },
            verdict.acts.len(),
            verdict_final.assignments.len(),
            verdict_final.refusals.len(),
            verdict.policies.len(),
            policies.len(),
            station_ops.len(),
            verdict.dropped() + verdict_final.dropped(),
            ai::summarize_drops(&dropped)
        );

        plan.note = note.clone();
        // 只要**有任何一条**校验通过的决策（接单或拒/观望），就必须把计划交给
        // tick——否则「明确拒单」会被当成「没有决策」，贪心立刻把单塞回去。
        let has_decision = !plan.assignments.is_empty() || !verdict_final.refusals.is_empty();
        (
            BrainOutcome {
                verdict: verdict_final,
                source,
                note,
                model,
                latency_ms,
                policies,
                station_ops,
                fingerprint,
                trim: TrimStats::default(),
                prompt_riders,
                prompt_orders,
                prompt_bytes,
                dropped,
            },
            if has_decision { Some(plan) } else { None },
        )
    }

    /// 要一份计划（同步等待版）。
    ///
    /// **无论发生什么都不会返回 Err**——最差也是 `Fallback`，让算法接手。
    /// tick 循环请改用 `prepare` + `finish`，别在这里等网络。
    pub async fn plan(
        &mut self,
        world: &mut World,
        idle: &[RiderId],
        pending: &[OrderId],
        hosted: &[RiderId],
    ) -> BrainOutcome {
        let prepared = match self.prepare(world, idle, pending) {
            Ok(p) => p,
            Err(outcome) => return outcome,
        };
        let fetched = prepared.request().await;
        self.finish(prepared, fetched, world, idle, pending, hosted)
            .0
    }
}

/// 把校验过的策略落到骑手身上，返回真正生效的那些。
///
/// 这是**唯一**写回世界的一处。三条约束在这里再确认一遍（协议层已钳过一次，
/// 这里是纵深防御）：
/// - `cap` 不超过 `cfg.backorder.max_held`；
/// - `mode` 必须被系统开关允许，否则整个覆盖当作没提；
/// - **基准门槛只读**：`price_tier` 只改倍率，改不了骑手自己的底线。
///
/// 另外：如果这轮策略和骑手现有的一模一样，就不算「生效」，
/// 免得控制端的「策略下发」计数每天虚涨。
fn apply_policies(world: &mut World, policies: &[ValidatedPolicy]) -> Vec<ValidatedPolicy> {
    use crate::sim::rider::{SpeedMode, StrategyOverride};

    // 先把「上限 + 开关」读成值：下面要可变借用骑手，就不能同时借 `world.cfg`。
    let max_held = world.cfg.backorder.max_held;
    let allow_speeding = world.cfg.allow_speeding;
    let allow_unrestricted = world.cfg.allow_unrestricted;
    let sim_minute = world.sim_minute as u64;
    let elapsed_min = world.elapsed_min;

    let mut applied = Vec::new();
    for p in policies {
        let cap = p.cap.map(|c| c.min(max_held));
        let mode = p.mode.filter(|m| match m {
            SpeedMode::Compliant => true,
            SpeedMode::Tolerant => allow_speeding,
            SpeedMode::Unrestricted => allow_unrestricted,
        });
        let Some(rider) = world.rider_mut(p.rider) else {
            continue;
        };
        let next = StrategyOverride {
            price_tier: p.price,
            cap_override: cap,
            mode_override: mode,
            set_at_min: sim_minute,
            set_at_elapsed: Some(elapsed_min),
            note: p.why.clone(),
        };
        if next.price_tier == rider.policy_override.price_tier
            && next.cap_override == rider.policy_override.cap_override
            && next.mode_override == rider.policy_override.mode_override
        {
            // 内容没变也要续期：LLM 每小时重复一次同样的策略，
            // 说明它现在仍然想要，不能因为「没变化」而让 TTL 到期失效。
            rider.policy_override.set_at_min = sim_minute;
            rider.policy_override.set_at_elapsed = Some(elapsed_min);
            continue;
        }
        rider.policy_override = next;
        applied.push(ValidatedPolicy {
            rider: p.rider,
            price: p.price,
            cap,
            mode,
            why: p.why.clone(),
        });
    }
    applied
}

/// 把校验过的站长指令落到世界，返回给控制端的说明。
///
/// 落地规则（协议层已查过引用，这里是**最后一米**）：
/// - `cap_rider`：写进 `policy_override.cap_override`，再钳一次上限；
/// - `force_offline`：直接把 `online` 置 false，手上单交回算法；
/// - `recall_training`：`recall_count += 1`，只记不改即时状态；
/// - `force_assign`：**这里不落地**——折进 `LlmPlan`，交给 tick 的双保险校验。
fn apply_station_ops(world: &mut World, ops: &[ValidatedStationOp]) -> Vec<StationOpBrief> {
    let max_held = world.cfg.backorder.max_held;
    let mut applied = Vec::new();

    for op in ops {
        let key = crate::sim::rider_key(op.rider());
        match op {
            ValidatedStationOp::CapRider { rider, cap, why } => {
                let cap = (*cap).min(max_held);
                // 站长指令也是「策略覆盖」，同样要盖时间戳：否则它会被
                // `expire_policies` 当成老存档在下一 tick 清掉（或者反过来，
                // 沿用上一轮 LLM 策略的时间，提前过期）。
                let set_at_min = world.sim_minute as u64;
                let set_at_elapsed = world.elapsed_min;
                if let Some(r) = world.rider_mut(*rider) {
                    r.policy_override.cap_override = Some(cap);
                    r.policy_override.note = why.clone();
                    r.policy_override.set_at_min = set_at_min;
                    r.policy_override.set_at_elapsed = Some(set_at_elapsed);
                }
                applied.push(StationOpBrief {
                    op: "cap_rider",
                    rider: key,
                    detail: format!("上限 {cap}"),
                    why: why.clone(),
                });
            }
            ValidatedStationOp::ForceOffline { rider, why } => {
                // 只停「接新单」：在途单继续由 `advance_riders` 走完，
                // 下一 tick 起 `ai_roster` 不再把他算进派单名单。
                // 回收在途单会打断订单生命周期与顾客承诺，不在本阶段范围。
                if let Some(r) = world.rider_mut(*rider) {
                    r.online = false;
                }
                applied.push(StationOpBrief {
                    op: "force_offline",
                    rider: key,
                    detail: "强制下线".into(),
                    why: why.clone(),
                });
            }
            ValidatedStationOp::RecallTraining { rider, why } => {
                if let Some(r) = world.rider_mut(*rider) {
                    r.recall_count += 1;
                }
                applied.push(StationOpBrief {
                    op: "recall_training",
                    rider: key,
                    detail: "召回培训".into(),
                    why: why.clone(),
                });
            }
            ValidatedStationOp::ForceAssign { order, why, .. } => {
                // 不动世界，只汇报——真正落地交给 tick 的 validate_llm_plan。
                applied.push(StationOpBrief {
                    op: "force_assign",
                    rider: key,
                    detail: format!("强派订单 {order}"),
                    why: why.clone(),
                });
            }
        }
    }
    applied
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
    fn prompt_follows_the_decision_protocol() {
        let w = world();
        let idle = w.idle_riders();
        let pending = w.pending_orders();
        let req = BrainRequest::build(&w, &idle, &pending);
        let system = protocol::system_prompt();
        let user = protocol::user_prompt(&req);
        // 系统提示词必须讲清 acts / policies 两个数组与白名单枚举
        assert!(system.contains("acts"));
        assert!(system.contains("policies"));
        assert!(system.contains("price"));
        assert!(system.contains("mode"));
        // 请求里必须带上 sim 上下文与两个数组
        assert!(user.contains("\"riders\""));
        assert!(user.contains("\"orders\""));
        assert!(user.contains("\"weather\""));
    }

    #[test]
    fn prompt_is_bounded_even_with_many_candidates() {
        let mut w = world();
        for _ in 0..30 {
            w.spawn_sim_rider();
        }
        let idle = w.idle_riders();
        let pending = w.pending_orders();
        let req = BrainRequest::build(&w, &idle, &pending);
        assert!(
            req.riders.len() <= protocol::MAX_RIDERS_IN_PROMPT,
            "骑手列表必须有上限，实际 {}",
            req.riders.len()
        );
        assert!(
            req.body_bytes() <= protocol::MAX_PROMPT_BYTES,
            "请求体必须落在预算内，实际 {} 字节",
            req.body_bytes()
        );
    }

    #[tokio::test]
    async fn disabled_brain_never_calls_out_and_says_so() {
        let mut w = world();
        let mut brain = TacticalBrain::disabled();
        assert!(!brain.is_enabled());
        let (idle, pending) = (w.idle_riders(), w.pending_orders());
        let out = brain.plan(&mut w, &idle, &pending, &[]).await;
        assert_eq!(out.source, BrainSource::Disabled);
        assert!(out.assignments().is_empty());
        assert_eq!(brain.stats().calls, 0);
    }

    #[tokio::test]
    async fn disabled_brain_wins_over_throttling() {
        let mut w = world();
        let mut brain = TacticalBrain::disabled();
        brain.throttle.begin(w.day, w.sim_minute);
        assert!(brain.slot_used(w.day, w.sim_minute));
        let (idle, pending) = (w.idle_riders(), w.pending_orders());
        let out = brain.plan(&mut w, &idle, &pending, &[]).await;
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

    #[test]
    fn apply_station_ops_caps_offlines_and_recalls() {
        let mut w = world();
        let ops = vec![
            ValidatedStationOp::CapRider {
                rider: 1,
                cap: 99,
                why: "暴雨".into(),
            },
            ValidatedStationOp::ForceOffline {
                rider: 2,
                why: "疲劳".into(),
            },
            ValidatedStationOp::RecallTraining {
                rider: 3,
                why: "准时率 68%".into(),
            },
            ValidatedStationOp::ForceAssign {
                rider: 1,
                order: 12345,
                why: "积压".into(),
            },
        ];
        let applied = apply_station_ops(&mut w, &ops);

        let max_held = w.cfg.backorder.max_held;
        assert_eq!(
            w.rider(1).unwrap().policy_override.cap_override,
            Some(max_held),
            "cap 要再钳一次上限"
        );
        assert_eq!(
            w.rider(1).unwrap().policy_override.set_at_elapsed,
            Some(w.elapsed_min),
            "站长指令也要盖时间戳，否则会被当成老存档清掉"
        );
        assert!(!w.rider(2).unwrap().online, "force_offline 要真的下线");
        assert_eq!(w.rider(3).unwrap().recall_count, 1, "召回要记一次");

        assert_eq!(applied.len(), 4);
        assert_eq!(applied[0].op, "cap_rider");
        assert_eq!(applied[3].op, "force_assign");
        assert!(applied[3].detail.contains("12345"));
    }

    /// 策略覆盖必须盖绝对时间戳；0 是合法值（开局第一轮就在 0 上）。
    #[test]
    fn policies_stamp_the_absolute_time_including_zero() {
        use crate::sim::rider::PriceTier;

        let mut w = world();
        assert_eq!(w.elapsed_min, 0);
        let applied = apply_policies(
            &mut w,
            &[ValidatedPolicy {
                rider: 1,
                price: PriceTier::High,
                cap: Some(2),
                mode: None,
                why: "测试".into(),
            }],
        );
        assert_eq!(applied.len(), 1);
        assert_eq!(
            w.rider(1).unwrap().policy_override.set_at_elapsed,
            Some(0),
            "0 要写成 Some(0)，不能留成「缺失」"
        );

        w.elapsed_min = 1;
        assert!(w.expire_policies().is_empty(), "下一 tick 不许立刻清掉");
    }
}
