//! 模拟事件。tick 的唯一输出，由 net 层包装成协议事件下发。
//!
//! 序列化后正好是协议要求的形状：
//! `{ "name": "order_broadcast", "payload": { ... } }`

use serde::{Deserialize, Serialize};

use super::activity::{ActivityProgressView, PlatformActivity};

use super::{
    OrderId, RiderId,
    config::SimConfig,
    order::Order,
    rider::{Phase, Rider, RiderKind, SpeedMode},
    route::LegKind,
    weather::Weather,
};

/// 路线任务里的一腿。**只给控制台看**「这趟串了什么」：
///
/// `held` 只说明手上有哪几张单，看不出先后；合并单之后「先送谁」才是关键信息，
/// 所以这里把队首顺序原样下发，`current` 标出他正在跑的那一腿。
#[derive(Debug, Clone, Serialize)]
pub struct RouteLegBrief {
    pub order_id: OrderId,
    pub kind: LegKind,
    /// 这一腿还要多少分钟：取餐腿是等餐时间，送达腿是骑行时间。
    pub remaining_min: u32,
    /// 是不是当前这一腿（队首）。
    pub current: bool,
}

/// 路线快照。老存档 / 还没接单的骑手没有路线，给空数组——
/// 控制台就能用同一套渲染，不用为「有没有路线」分叉。
fn route_brief(r: &Rider) -> Vec<RouteLegBrief> {
    let Some(task) = r.route.as_ref() else {
        return Vec::new();
    };
    task.legs()
        .iter()
        .enumerate()
        .map(|(i, leg)| RouteLegBrief {
            order_id: leg.order_id,
            kind: leg.kind,
            remaining_min: leg.remaining_min,
            current: i == 0,
        })
        .collect()
}

/// 骑手对外快照。财务字段按角色决定是否填充。
#[derive(Debug, Clone, Serialize)]
pub struct RiderBrief {
    pub id: RiderId,
    /// 对外 id，形如 `r3`。
    pub key: String,
    pub name: String,
    pub kind: RiderKind,
    pub online: bool,
    pub injured: bool,
    pub frozen: bool,
    pub speed_mode: SpeedMode,
    pub level: u8,
    pub rating: f32,
    pub completed: u32,
    /// 今日已用掉的拒单次数。配合 `ConfigView::max_rejects_per_day` 显示额度。
    pub rejects_today: u32,
    pub held: Vec<OrderId>,
    /// 路线任务：队首是当前腿，走完出队（见 [`RouteLegBrief`]）。
    pub route: Vec<RouteLegBrief>,
    pub phase: Option<Phase>,
    pub lat: f64,
    pub lng: f64,
    /// 只在推给本人 / 控制端时有值。
    pub hourly_wage_cents: Option<i64>,
    pub takehome_cents: Option<i64>,
    pub cash_cents: Option<i64>,
    pub fines_cents: Option<i64>,
    /// 隐藏属性，只给控制端。
    pub fatigue: Option<f32>,
    /// 情绪 0~1，只给控制端。见 [`Rider::laziness`]。
    pub mood: Option<f32>,
    /// 情绪是不是已经低到在偷懒了，只给控制端。
    ///
    /// 让服务端算而不是把 `lazy_below` 甩给前端：阈值在配置里，前端自己比
    /// 就得再抄一份默认值，迟早对不上。
    pub slacking: bool,
    pub expected_value_cents: Option<i64>,
    /// 平台活动进度。**只给本人 / 控制端**，公开版是空数组。
    pub activities: Vec<ActivityProgressView>,
}

impl RiderBrief {
    fn base(r: &Rider, _cfg: &SimConfig, now_min: u64) -> Self {
        let phase = r.held.first().map(|h| h.phase);
        Self {
            id: r.id,
            key: crate::sim::rider_key(r.id),
            name: r.name.clone(),
            kind: r.kind,
            online: r.online,
            injured: r.is_injured(now_min),
            frozen: r.is_frozen(now_min),
            speed_mode: r.speed_mode,
            level: r.level,
            rating: r.rating,
            completed: r.completed,
            rejects_today: r.rejected,
            held: r.held_ids(),
            route: route_brief(r),
            phase,
            lat: r.lat,
            lng: r.lng,
            hourly_wage_cents: None,
            takehome_cents: None,
            cash_cents: None,
            fines_cents: None,
            fatigue: None,
            mood: None,
            slacking: false,
            expected_value_cents: None,
            activities: Vec::new(),
        }
    }

    /// 给其他骑手看的版本：**不含任何收入信息**（权限隔离在服务端做）。
    pub fn public(r: &Rider, cfg: &SimConfig, now_min: u64) -> Self {
        Self::base(r, cfg, now_min)
    }

    /// 给本人看的版本。
    pub fn own(r: &Rider, cfg: &SimConfig, now_min: u64) -> Self {
        let mut b = Self::base(r, cfg, now_min);
        b.hourly_wage_cents = Some(r.hourly_wage_cents());
        b.takehome_cents = Some(r.takehome_cents());
        b.cash_cents = Some(r.cash_cents);
        b.fines_cents = Some(r.fines_cents);
        b.activities = super::activity::progress_views(r);
        b
    }

    /// 给控制端看的版本。
    pub fn control(r: &Rider, cfg: &SimConfig, now_min: u64) -> Self {
        let mut b = Self::own(r, cfg, now_min);
        b.fatigue = Some(r.fatigue);
        b.mood = Some(r.mood);
        b.slacking = r.is_slacking(cfg);
        if cfg.show_expected_value {
            b.expected_value_cents = Some(r.expected_value_cents(cfg));
        }
        b
    }
}

/// 日结流水。
///
/// 需要 `Deserialize` 是因为 `store` 会把它以 JSON 落库再读回来。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DayLedger {
    pub rider_id: RiderId,
    pub key: String,
    pub name: String,
    /// 到手（分）。
    pub takehome_cents: i64,
    pub gross_cents: i64,
    pub fines_cents: i64,
    pub insurance_cents: i64,
    pub completed: u32,
    pub late_count: u32,
    pub online_minutes: u32,
    pub hourly_wage_cents: i64,
    pub rating: f32,
    pub level: u8,
    /// 欠款（分）。
    pub debt_cents: i64,
}

/// AI 分配结果。
#[derive(Debug, Clone, Serialize)]
pub struct AiAssignment {
    pub order_id: OrderId,
    pub rider_id: RiderId,
    pub rider_key: String,
    pub score: f32,
    pub reason: String,
}

/// AI 决策来源。
///
/// 「AI 决策」与「LLM 决策」是两个**不同的**概念，控制台必须能分得清：
/// [`Algorithm`](DecisionSource::Algorithm)、[`AlgorithmFallback`](DecisionSource::AlgorithmFallback)、
/// [`AlgorithmRescue`](DecisionSource::AlgorithmRescue) 都是纯算法（LLM 关着也能跑），
/// 只有 [`Llm`](DecisionSource::Llm) 是真的问了模型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionSource {
    /// 贪心算法。
    Algorithm,
    /// 真 LLM 战术大脑。
    Llm,
    /// LLM 失败/被校验丢弃后回退到算法。
    AlgorithmFallback,
    /// 超时救单：重投次数用尽，算法强制指派给最近的骑手（不允许流单）。
    AlgorithmRescue,
    /// 控制端手动指派。
    Manual,
    /// AI 托管（真人骑手掉线后由 AI 自动决策，见 `human::host`）。
    AiHosted,
}

impl DecisionSource {
    pub fn label(self) -> &'static str {
        match self {
            DecisionSource::Algorithm => "算法贪心",
            DecisionSource::Llm => "LLM 战术大脑",
            DecisionSource::AlgorithmFallback => "LLM 失败，算法兜底",
            DecisionSource::AlgorithmRescue => "超时救单",
            DecisionSource::Manual => "控制端指派",
            DecisionSource::AiHosted => "AI 托管",
        }
    }

    /// 这一条决策**是否真的来自 LLM**。
    ///
    /// 控制台据此把决策流分成两类展示；算法兜底、超时救单都算算法。
    pub fn is_llm(self) -> bool {
        matches!(self, DecisionSource::Llm)
    }
}

/// 一条 AI 运营指令（站长 / 烽火台）的人类可读摘要，推给控制台的决策流用。
#[derive(Debug, Clone, Serialize)]
pub struct AiOpBrief {
    /// 指令名，如 `cap_rider` / `set_weather` / `narrate`。
    pub op: &'static str,
    /// 作用对象，人类可读（`r5` / `#42` / `-`）。
    pub target: String,
    /// 补充说明（钳制后的值 / 订单号）。
    pub detail: String,
    /// 为什么这么做。
    pub why: String,
}

/// 模拟事件。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "name", content = "payload", rename_all = "snake_case")]
pub enum SimEvent {
    /// 时间推进。
    Tick {
        day: u32,
        /// 当日 0:00 起算的模拟分钟。
        sim_minute: u32,
        /// 自开局起算的单调分钟。
        elapsed_min: u64,
        speed_multiplier: u32,
    },
    /// 新订单广播（含抢单窗口）。
    OrderBroadcast {
        order: Box<Order>,
        window_ms: u64,
        /// 顺路提示：与骑手当前手持单方向的夹角，由 net 层按人裁剪。
        expires_in_ms: u64,
    },
    OrderClaimed {
        order_id: OrderId,
        rider_id: RiderId,
        rider_name: String,
        quoted_fee_cents: i64,
        forced: bool,
    },
    /// 改派：把**还没取到餐**的单从一个骑手手上转给另一个骑手（控制端动作）。
    ///
    /// 只在「餐还在商家手里」时发生（`OrderStatus::Assigned`，即骑手还在去店路上
    /// 或在店里等餐）。餐一旦离手就在原骑手车上，保温与餐损都随人走，没有代送一说
    /// （见 [`crate::sim::scheduler::reassign`]）。
    ///
    /// **两边骑手都要收到**：原骑手以为自己手上还有这张单，新骑手是突然多出来一张
    /// （还得点确认）。所以可见性是「控制端 + 两位当事人」，不是全服广播。
    OrderReassigned {
        order_id: OrderId,
        from_rider_id: RiderId,
        from_rider_name: String,
        to_rider_id: RiderId,
        to_rider_name: String,
        /// 改派那一刻原骑手卡在哪一段：`to_shop`（去店路上）/ `waiting_food`（店里等餐）。
        phase: Phase,
    },
    OrderExpired {
        order_id: OrderId,
    },
    /// 骑手拒掉了一张系统指派单。**不罚款**，只吃当日拒单额度。
    AssignRejected {
        rider_id: RiderId,
        order_id: OrderId,
        /// 拒完之后今天一共拒掉几张。
        used: u32,
        /// 当日额度上限，`0` = 不限。
        limit: u32,
    },
    OrderPickedUp {
        order_id: OrderId,
        rider_id: RiderId,
        prep_minutes: u32,
    },
    OrderDelivered {
        order_id: OrderId,
        rider_id: RiderId,
        rider_name: String,
        late: bool,
        takehome_cents: i64,
        hourly_wage_cents: i64,
    },
    /// 骑手状态变化。`rider` 是**公开版**（不含收入），
    /// net 层按收件人身份升级为本人版 / 控制端版。
    RiderStateChanged {
        rider_id: RiderId,
        rider: Box<RiderBrief>,
    },
    /// 收入变动，只推给本人。
    RiderFinance {
        rider_id: RiderId,
        delta_cents: i64,
        kind: String,
        reason: String,
        takehome_cents: i64,
        hourly_wage_cents: i64,
    },
    WeatherChanged {
        weather: Weather,
        label: &'static str,
        speed_factor: f32,
        rain_bonus_cents: i64,
    },
    FineIssued {
        rider_id: RiderId,
        kind: String,
        amount_cents: i64,
        reason: String,
    },
    /// 控制端手动改档 / 上下线等配置生效。
    ConfigChanged {
        speed_multiplier: u32,
        commission_rate: f32,
        weather: Weather,
    },
    /// AI 分配结果与理由。
    AiDecision {
        source: DecisionSource,
        /// 哪一层做的决策：`rider` | `station` | `director` | `algorithm`。
        layer: &'static str,
        assignments: Vec<AiAssignment>,
        /// 站长 / 烽火台的运营指令摘要（控制台决策流用）。
        ops: Vec<AiOpBrief>,
        /// 决策 id，人工否决时回传它。`0` = 不可否决（算法兜底）。
        decision_id: u64,
        note: String,
    },
    /// AI 说的一句话（烽火台旁白等）。**只影响气氛，不改任何数值**。
    Narration {
        /// 说话的是哪一层：`director` | `station` | `rider`。
        layer: &'static str,
        text: String,
    },
    /// 任务对话（预设 / LLM 裁决）。
    TaskMessage {
        order_id: OrderId,
        /// `customer` | `rider` | `system`
        from: &'static str,
        text: String,
        step: usize,
        task_id: String,
    },
    /// 顾客想开口了（**服务端内部信号，不下发给客户端**）。
    ///
    /// sim 层只负责掷骰子（见 [`crate::sim::chat`]）；「说什么、要不要索赔」
    /// 是 IO，必须由 app 层拿走去调 LLM。带上画像摘要，免得 app 层再回查世界。
    ChatOpen {
        order_id: OrderId,
        rider_id: RiderId,
        /// 画像一行摘要，直接进提示词。
        context: String,
        /// LLM 不可用时的兜底开场白（零延迟，可复现）。
        fallback: String,
    },
    /// **订单会话消息**（末端交付沟通）。骑手端与烽火台都能看到。
    ///
    /// 与 [`SimEvent::TaskMessage`] 分开：那条绑在附加任务上，这条是订单级的，
    /// 末端窗口里任何一单都可能有。会话**按单聚合**，前端拿 `order_id` 归并。
    ChatMessage {
        order_id: OrderId,
        /// `rider` | `customer` | `system`
        from: &'static str,
        text: String,
        /// 消息类型：`preset`（预设话术）| `free`（骑手打字）| `reply`（顾客回应）
        /// | `ruling`（裁决结论）。前端据此换头像 / 配色。
        kind: &'static str,
        /// 归属天（0:00 起算的第几天）。**会话时效按它算**，过期即删。
        day: u32,
        /// 发生时的当日分钟（0..1439），仅用于展示时间戳。
        sim_minute: u32,
        /// 这条消息带来的赔付金额（分）。恒 ≥ 0，`0` = 无经济后果。
        compensation_cents: i64,
        /// 责任方：`rider` | `merchant` | `none`。钱从谁身上出由它决定。
        fault: &'static str,
    },
    /// 活动定义变化（新建 / 上下架 / 删除）。全体可见。
    ///
    /// 快照只在连接时下发，不广播这个事件的话，烽火台建完活动骑手要重连才看得见。
    ActivitiesChanged {
        activities: Vec<PlatformActivity>,
    },
    /// 活动进度变化（又完成了一单）。只推给本人与控制端。
    ActivityProgressChanged {
        rider_id: RiderId,
        activities: Vec<ActivityProgressView>,
    },
    /// 活动达成，奖金已自动入账。只推给本人与控制端。
    ActivityReward {
        rider_id: RiderId,
        activity_id: u32,
        title: String,
        /// 奖金（分）。
        amount_cents: i64,
        /// 达成时的完成单数（≥ 目标）。
        completed: u32,
        target_orders: u32,
        /// 入账后的今日到手，手机端直接拿来刷新顶部数字。
        takehome_cents: i64,
    },
    /// 日结。
    DayClosed {
        day: u32,
        ledgers: Vec<DayLedger>,
        total_takehome_cents: i64,
        total_orders: u32,
    },
}
