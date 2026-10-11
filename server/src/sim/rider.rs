//! 骑手：属性、状态机、速度档位、疲劳。

use serde::{Deserialize, Serialize};

use super::{
    OrderId, RiderId,
    config::{PEAK_THRESHOLD_MULTIPLIER, SimConfig},
    geo::LatLng,
    weather::Weather,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiderKind {
    Simulated,
    Human,
}

/// 完整行程：先去商家取餐，再到顾客。
const FULL_ROUTE: [Phase; 4] = [
    Phase::ToShop,
    Phase::WaitingFood,
    Phase::ToCustomer,
    Phase::ArrivedCustomer,
];

impl RiderKind {
    /// 这个骑手跑一单要经过的行程段，**按顺序**。
    ///
    /// 现在是给玩法分化留的口子：真人侧的取餐玩法（出餐慢 → 上报）还没做，
    /// 所以两类骑手暂时走同一段路。等上报做出来了，真人的分支改成
    /// `[Phase::ToCustomer, Phase::ArrivedCustomer]` 就是「A 到 B」的简化版，
    /// 不用动 tick 主循环——那里只按这个列表推进。
    ///
    /// **改这个列表会直接改玩法**，所以两边相等这件事由单测锁着。
    pub fn route(self) -> &'static [Phase] {
        match self {
            RiderKind::Simulated | RiderKind::Human => &FULL_ROUTE,
        }
    }
}

/// 三档速度模式（玩法设计 §3.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeedMode {
    /// 0 合规 30km/h。
    Compliant,
    /// 1 容忍超速 45km/h。
    Tolerant,
    /// 2 解除限速。
    Unrestricted,
}

impl SpeedMode {
    pub const ALL: [SpeedMode; 3] = [
        SpeedMode::Compliant,
        SpeedMode::Tolerant,
        SpeedMode::Unrestricted,
    ];

    pub fn index(self) -> usize {
        match self {
            SpeedMode::Compliant => 0,
            SpeedMode::Tolerant => 1,
            SpeedMode::Unrestricted => 2,
        }
    }

    pub fn from_index(i: usize) -> Option<Self> {
        Self::ALL.get(i).copied()
    }

    pub fn label(self) -> &'static str {
        match self {
            SpeedMode::Compliant => "合规",
            SpeedMode::Tolerant => "容忍超速",
            SpeedMode::Unrestricted => "解除限速",
        }
    }

    pub fn hourly_coefficient(self, cfg: &SimConfig) -> f32 {
        cfg.speed.hourly_coefficient[self.index()]
    }
    pub fn accident_rate(self, cfg: &SimConfig) -> f32 {
        cfg.speed.accident_rate[self.index()]
    }
    pub fn camera_rate(self, cfg: &SimConfig) -> f32 {
        cfg.speed.camera_rate[self.index()]
    }
    pub fn rating_delta(self, cfg: &SimConfig) -> f32 {
        cfg.speed.rating_delta[self.index()]
    }
}

/// 手持订单当前所处的行程段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// 骑手 → 商家。
    ToShop,
    /// 等餐。
    WaitingFood,
    /// 商家 → 顾客。
    ToCustomer,
    /// 已到顾客，等骑手点「交付」。
    ArrivedCustomer,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::ToShop => "待取货",
            Phase::WaitingFood => "等餐",
            Phase::ToCustomer => "待送达",
            Phase::ArrivedCustomer => "已到达",
        }
    }
}

/// 手持的一单。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeldOrder {
    pub order_id: OrderId,
    pub phase: Phase,
    /// 当前行程段剩余分钟。
    pub remaining_min: u32,
    /// 当前行程段的**总时长**（分钟）。`remaining_min` 归零时用它算进度。
    ///
    /// LLM 需要「骑手卡在哪一段、进度多少」才能判「这单能不能再背一单」，
    /// 只给剩余分钟是判不出的（见《LLM协议设计》§4.2 `leg_progress`）。
    pub leg_total_min: u32,
    /// 倒计时已归零、等骑手点按钮（真人骑手才有）。
    pub awaiting_ack: bool,
    /// 背单（多背一单）。
    pub back_order: bool,
    /// 等餐已经开始，记一下用于罚款判定。
    pub wait_started_min: u64,
    /// 等餐超时罚单是否已经开过（每单只罚一次）。
    pub wait_fined: bool,
    /// 到达顾客的时刻（`elapsed_min`）。`None` = 还没到门口。
    ///
    /// 末端放置窗口与订单会话都按它起算：窗口到点没、顾客该不该开口，
    /// 都得先知道「到多久了」。
    pub arrive_at_min: Option<u64>,
    /// 末端放置窗口长度（模拟分钟，5–60）。到达时由
    /// [`crate::sim::chat::roll_window`] 掷出。
    ///
    /// 1x 时它正好是 5–60 真实秒——停车、上楼、放门口的实际耗时。
    /// 真人骑手在窗口内可随时点「送达」；AI 托管则等窗口走完再自动交付，
    /// 否则「到达即送达」根本不给对话留缝。
    pub deliver_window_min: u32,
    /// 这单的顾客是否已经开过口。**每单最多触发一次会话**，免得同一单反复骚扰。
    pub chat_opened: bool,
}

impl HeldOrder {
    /// 新建手持单。结构体加字段时集中改这里，避免到处漏字段。
    pub fn new(
        order_id: OrderId,
        phase: Phase,
        remaining_min: u32,
        back_order: bool,
        now_min: u64,
    ) -> Self {
        Self {
            order_id,
            phase,
            remaining_min,
            leg_total_min: remaining_min.max(1),
            awaiting_ack: false,
            back_order,
            wait_started_min: now_min,
            wait_fined: false,
            arrive_at_min: None,
            deliver_window_min: 0,
            chat_opened: false,
        }
    }

    /// 切换到新的行程段：总时长与剩余分钟一起重置。
    ///
    /// **进入新段必须走这里**，否则 `leg_progress` 会拿旧总时长算，进度失真。
    pub fn set_leg(&mut self, phase: Phase, total_min: u32) {
        self.phase = phase;
        let total = total_min.max(1);
        self.leg_total_min = total;
        self.remaining_min = total;
    }

    /// 当前行程段进度 0.0~1.0（1.0 = 该段已走完、等待切换）。
    pub fn leg_progress(&self) -> f32 {
        let total = self.leg_total_min.max(1) as f32;
        let done = (self.leg_total_min - self.remaining_min.min(self.leg_total_min)) as f32;
        (done / total).clamp(0.0, 1.0)
    }

    /// 该段还剩多少比例（1.0 = 刚出发，0.0 = 已到点）。
    pub fn leg_remaining_ratio(&self) -> f32 {
        1.0 - self.leg_progress()
    }
}

#[cfg(test)]
mod leg_progress_tests {
    use super::*;

    #[test]
    fn progress_tracks_remaining() {
        let mut h = HeldOrder::new(1, Phase::ToShop, 10, false, 0);
        assert_eq!(h.leg_total_min, 10);
        assert!((h.leg_progress() - 0.0).abs() < 1e-6);
        h.remaining_min = 7;
        assert!((h.leg_progress() - 0.3).abs() < 1e-6);
        assert!((h.leg_remaining_ratio() - 0.7).abs() < 1e-6);
        h.set_leg(Phase::ToCustomer, 8);
        assert_eq!(h.leg_total_min, 8);
        assert_eq!(h.remaining_min, 8);
        assert!((h.leg_progress() - 0.0).abs() < 1e-6);
    }

    #[test]
    fn progress_never_divides_by_zero() {
        let h = HeldOrder::new(1, Phase::ToShop, 0, false, 0);
        assert_eq!(h.leg_total_min, 1);
        // 剩余为 0 = 该段已走完
        assert!((h.leg_progress() - 1.0).abs() < 1e-6);
    }

    /// 真人和模拟骑手现在走**同一段路**。
    ///
    /// 这条测试是「有人改玩法」的警报：哪天真人要简化成 A→B（跳过取餐），
    /// 它会红，提醒你确认那是玩法决策而不是手滑。
    #[test]
    fn both_kinds_share_the_full_route_today() {
        assert_eq!(RiderKind::Simulated.route(), RiderKind::Human.route());
        assert_eq!(
            RiderKind::Human.route(),
            &[Phase::ToShop, Phase::WaitingFood, Phase::ToCustomer, Phase::ArrivedCustomer][..],
            "真人一旦砍掉前两段，tick 里那些取餐关口就再也不会被真人碰到"
        );
    }
}

/// 期望单价档位（《LLM协议设计》§3.4）。
///
/// **为什么用档位而不是「期望单价 = 850 分」**：LLM 会输出 `99999` / `-1` /
/// `"8块5"`，钳制之后语义就没了。档位是有限集合，可精确校验、可白名单枚举；
/// 档位到门槛的**具体倍率由代码决定**，LLM 只能在骑手自己的基准上做相对调整
/// ——它无法通过输出极端值操纵经济系统。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PriceTier {
    /// 什么单都接，冲单量。
    Low,
    #[default]
    Normal,
    /// 挑单。
    High,
    /// 只等高价值单。
    Premium,
}

impl PriceTier {
    pub const ALL: [PriceTier; 4] = [
        PriceTier::Low,
        PriceTier::Normal,
        PriceTier::High,
        PriceTier::Premium,
    ];

    /// 协议里用的短代号（与 `llm/protocol.rs` 的 `low`/`normal`/`high`/`premium` 一致）。
    pub fn key(self) -> &'static str {
        match self {
            PriceTier::Low => "low",
            PriceTier::Normal => "normal",
            PriceTier::High => "high",
            PriceTier::Premium => "premium",
        }
    }

    pub fn from_key(s: &str) -> Option<Self> {
        match s {
            "low" => Some(PriceTier::Low),
            "normal" => Some(PriceTier::Normal),
            "high" => Some(PriceTier::High),
            "premium" => Some(PriceTier::Premium),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PriceTier::Low => "低（冲量）",
            PriceTier::Normal => "正常",
            PriceTier::High => "高（挑单）",
            PriceTier::Premium => "极高",
        }
    }

    /// 档位 → 时薪门槛倍率。基准是骑手自己的 `accept_threshold_cents_hour`。
    pub fn multiplier(self) -> f32 {
        match self {
            PriceTier::Low => 0.75,
            PriceTier::Normal => 1.00,
            PriceTier::High => 1.30,
            PriceTier::Premium => 1.80,
        }
    }
}

/// LLM 下发的策略调整，**叠加**在骑手原有策略之上（《LLM协议设计》§5.5）。
///
/// 三层倍率叠加语义：`基准门槛 × 高峰系数 × price_tier 倍率`。
/// 默认值（Normal / None / None）必须是**恒等变换**——这样 LLM 关闭时
/// 系统行为与加协议之前逐字节一致。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StrategyOverride {
    /// 期望单价档位 → 门槛倍率。
    pub price_tier: PriceTier,
    /// 背单上限覆盖。`None` = 用配置默认；**永远被 `cfg.backorder.max_held` 二次钳制**。
    pub cap_override: Option<u32>,
    /// 速度档位覆盖。`None` = 用骑手自己设的；仍需过 `allow_speeding` 等开关。
    pub mode_override: Option<SpeedMode>,
    /// 这一轮策略的生效时间（模拟分钟），用于过期检查与审计。
    pub set_at_min: u64,
    /// 下达这轮策略的说明（≤24 字），仅用于控制端展示。
    #[serde(default)]
    pub note: String,
}

impl StrategyOverride {
    /// 是否是「什么都没改」的默认覆盖。
    pub fn is_identity(&self) -> bool {
        self.price_tier == PriceTier::Normal
            && self.cap_override.is_none()
            && self.mode_override.is_none()
    }
}

/// 模拟骑手的策略参数（玩法设计 §5.1）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SimStrategy {
    /// 最低时薪门槛（分/小时）。
    pub accept_threshold_cents_hour: i64,
    pub max_orders_per_day: u32,
    pub accept_extra_tasks: bool,
    pub speed_mode: SpeedMode,
    pub detour_tolerance_m: u32,
    /// 0~1，影响等餐耐心与找楼效率。
    pub skill: f32,
}

impl Default for SimStrategy {
    fn default() -> Self {
        Self {
            accept_threshold_cents_hour: 2800,
            max_orders_per_day: 0,
            accept_extra_tasks: true,
            speed_mode: SpeedMode::Compliant,
            detour_tolerance_m: 800,
            skill: 0.6,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rider {
    pub id: RiderId,
    pub kind: RiderKind,
    pub name: String,
    pub lat: f64,
    pub lng: f64,
    pub speed_mode: SpeedMode,

    /// 今日已入账（到手）。
    pub cash_cents: i64,
    /// 今日毛收入（抽水前）。
    pub gross_cents: i64,
    pub fines_cents: i64,
    pub insurance_cents: i64,
    /// 到手不足时记录欠款，下一单优先抵扣。
    pub debt_cents: i64,

    pub rating: f32,
    pub level: u8,
    pub completed: u32,
    pub rejected: u32,
    pub late_count: u32,
    /// 隐藏属性 0~1，玩家看不到数字。
    pub fatigue: f32,

    pub online: bool,
    pub online_minutes: u32,
    /// 累计历史完成单量（决定等级，不随日结清零）。
    pub lifetime_completed: u32,

    pub held: Vec<HeldOrder>,
    /// 系统刚指派过来、骑手还没点「接受」的那张单。
    ///
    /// 为什么需要单独标记：「拒绝一张还没接受的指派单」和「取消一张
    /// 已经接下的单」代价完全不同——前者只吃每日拒单额度，后者要罚
    /// `fine.cancel_cents`（见 `scheduler::reject_order`）。而指派发生时
    /// 订单就已经挂进 `held` 了，光看 `held` 分不出这两种情况。
    #[serde(default)]
    pub assign_pending: Option<OrderId>,
    /// 被拍冻结到（自开局起算的模拟分钟）。
    pub frozen_until_min: u64,
    /// 事故停工到。
    pub injured_until_min: u64,

    pub strategy: SimStrategy,
    /// LLM 下发的策略覆盖（默认 = 恒等变换，即不生效）。
    ///
    /// 与 `strategy` 的关系：`strategy` 是骑手自己的底线，这里是**外层调整**。
    /// 读的时候一律走 `effective_*()`，不要直接读这个字段。
    #[serde(default)]
    pub policy_override: StrategyOverride,
    /// 日常活动中心（用于派单距离偏好）。
    pub home_lat: f64,
    pub home_lng: f64,

    /// 被站长召回培训的次数（只记不改即时状态，后续影响评级 / 派单权重）。
    #[serde(default)]
    pub recall_count: u32,

    /// 最近一次收入变动的说明，给账单用。
    pub last_ledger: Vec<LedgerEntry>,
}

/// 账单明细一条。罚单必须带原因。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub at_min: u64,
    pub order_id: Option<OrderId>,
    pub kind: String,
    pub amount_cents: i64,
    pub reason: String,
}

impl Rider {
    pub fn new(
        id: RiderId,
        kind: RiderKind,
        name: impl Into<String>,
        pos: LatLng,
        cfg: &SimConfig,
    ) -> Self {
        Self {
            id,
            kind,
            name: name.into(),
            lat: pos.lat,
            lng: pos.lng,
            speed_mode: SpeedMode::Compliant,
            cash_cents: 0,
            gross_cents: 0,
            fines_cents: 0,
            insurance_cents: 0,
            debt_cents: 0,
            assign_pending: None,
            rating: cfg.rating_rules.initial,
            level: 1,
            completed: 0,
            rejected: 0,
            late_count: 0,
            fatigue: 0.0,
            online: false,
            online_minutes: 0,
            lifetime_completed: 0,
            held: Vec::new(),
            frozen_until_min: 0,
            injured_until_min: 0,
            strategy: SimStrategy::default(),
            policy_override: StrategyOverride::default(),
            home_lat: pos.lat,
            home_lng: pos.lng,
            recall_count: 0,
            last_ledger: Vec::new(),
        }
    }

    pub fn pos(&self) -> LatLng {
        LatLng::new(self.lat, self.lng)
    }

    pub fn set_pos(&mut self, p: LatLng) {
        self.lat = p.lat;
        self.lng = p.lng;
    }

    pub fn is_injured(&self, now_min: u64) -> bool {
        self.injured_until_min > now_min
    }

    pub fn is_frozen(&self, now_min: u64) -> bool {
        self.frozen_until_min > now_min
    }

    /// 手上没活 = 没单 + 没受伤 + 没被冻结 + 没到单量上限。
    ///
    /// **故意不含 `online`**：AutoPilot 下掉线的真人由 AI 代管，他本人是离线的，
    /// 但 AI 得能给他派单（见 [`crate::human::host::ai_idle_riders`]）。
    /// 所以「能不能接单」和「在不在线」是两件事，前者是 `is_free`，后者是 `is_idle`。
    pub fn is_free(&self, now_min: u64, cfg: &SimConfig) -> bool {
        self.held.is_empty()
            && !self.is_injured(now_min)
            && !self.is_frozen(now_min)
            && !self.reached_daily_cap(cfg)
    }

    /// 空闲 = 在线 + 手上没活。
    pub fn is_idle(&self, now_min: u64, cfg: &SimConfig) -> bool {
        self.online && self.is_free(now_min, cfg)
    }

    /// 单量上限 0 表示不限。
    pub fn reached_daily_cap(&self, cfg: &SimConfig) -> bool {
        cfg.max_orders_per_day > 0 && self.completed >= cfg.max_orders_per_day
    }

    pub fn held_ids(&self) -> Vec<OrderId> {
        self.held.iter().map(|h| h.order_id).collect()
    }

    pub fn held_phase(&self, order_id: OrderId) -> Option<Phase> {
        self.held
            .iter()
            .find(|h| h.order_id == order_id)
            .map(|h| h.phase)
    }

    pub fn is_holding(&self, order_id: OrderId) -> bool {
        self.held.iter().any(|h| h.order_id == order_id)
    }

    pub fn held_mut(&mut self, order_id: OrderId) -> Option<&mut HeldOrder> {
        self.held.iter_mut().find(|h| h.order_id == order_id)
    }

    /// 当前速度 km/h：档位限速 × 天气 × 疲劳削减。
    pub fn speed_kmh(&self, cfg: &SimConfig, weather: Weather) -> f32 {
        let limit = cfg.speed.complaint_speed_kmh[self.speed_mode.index()];
        let fatigue_penalty = 1.0 - self.fatigue.clamp(0.0, 1.0) * cfg.fatigue.speed_penalty_max;
        (limit * weather.speed_factor() * fatigue_penalty).max(3.0)
    }

    /// 米/秒。真实骑行速度上限约 8.3 m/s（30km/h）。
    pub fn speed_mps(&self, cfg: &SimConfig, weather: Weather) -> f32 {
        self.speed_kmh(cfg, weather) / 3.6
    }

    // ---- 策略覆盖的读取口（《LLM协议设计》§5.4 / §5.5）----
    //
    // 一律走这几个方法，不要直接读 `policy_override`：钳制逻辑只写一遍。

    /// 当前生效的接单时薪门槛（分/小时）。
    ///
    /// 三层倍率叠加：`骑手基准 × 高峰系数 × LLM 单价档位倍率`。
    /// 默认档位是 `Normal`（×1.0），所以 LLM 关闭时结果与不加协议完全一致。
    pub fn effective_threshold_cents_hour(&self, is_peak: bool) -> f32 {
        let base = self.strategy.accept_threshold_cents_hour as f32;
        let peak = if is_peak {
            PEAK_THRESHOLD_MULTIPLIER
        } else {
            1.0
        };
        base * peak * self.policy_override.price_tier.multiplier()
    }

    /// 当前生效的背单上限。**永远不能超过 `cfg.backorder.max_held`。**
    pub fn effective_max_held(&self, cfg: &SimConfig) -> u32 {
        let hard = cfg.backorder.max_held;
        self.policy_override.cap_override.unwrap_or(hard).min(hard)
    }

    /// 当前生效的速度档位：LLM 的覆盖必须先过系统开关（`allow_speeding` 等）。
    ///
    /// 没开关时返回骑手自己当前的档位——**不会因为 LLM 说 2 就解限速**。
    pub fn effective_speed_mode(&self, cfg: &SimConfig) -> SpeedMode {
        match self.policy_override.mode_override {
            Some(m) if cfg.speed_mode_allowed(m) => m,
            _ => self.speed_mode,
        }
    }

    /// 在线小时数（等餐时间也算在线——这是真实骑手的痛点）。
    pub fn online_hours(&self) -> f32 {
        self.online_minutes as f32 / 60.0
    }

    /// 时薪（分/小时）。**这是整个模拟器的核心 KPI。**
    ///
    /// 注意：`cash_cents` 是**已经**扣过抽水、罚单、保险的净额
    /// （见 `economy::debit` / `economy::credit`）。所以这里不能再减一次
    /// `fines_cents` / `insurance_cents`，否则就是重复扣款。`fines_cents` 与
    /// `insurance_cents` 只用于账单明细展示。
    ///
    /// 罚得比赚得多时这个值会是负的，这是有意的——跑一天倒欠钱，
    /// 时薪就该显示负数，不然「这趟白干还赔钱」这件事在 KPI 上完全看不到。
    pub fn hourly_wage_cents(&self) -> i64 {
        let hours = self.online_hours();
        if hours < 0.05 {
            return 0;
        }
        (self.cash_cents as f32 / hours).round() as i64
    }

    /// 今日到手 = 真正落袋的净额。**可以为负**（罚单超过收入时倒欠）。
    pub fn takehome_cents(&self) -> i64 {
        self.cash_cents
    }

    pub fn push_ledger(&mut self, entry: LedgerEntry) {
        self.last_ledger.push(entry);
        // 只留当天最近 200 条，避免内存无界增长。
        if self.last_ledger.len() > 200 {
            let drop = self.last_ledger.len() - 200;
            self.last_ledger.drain(0..drop);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rider(cfg: &SimConfig) -> Rider {
        let mut r = Rider::new(1, RiderKind::Human, "骑手A", LatLng::new(30.5, 114.4), cfg);
        r.online = true;
        r
    }

    #[test]
    fn hourly_wage_is_cash_over_hours() {
        let cfg = SimConfig::default();
        let mut r = rider(&cfg);
        r.online_minutes = 120;
        // cash_cents 已经扣过罚单与保险（economy::debit），这里就是净额。
        r.cash_cents = 3500;
        r.fines_cents = 300;
        r.insurance_cents = 200;
        assert_eq!(r.takehome_cents(), 3500);
        // 3500 分 / 2 小时 = 1750 分/小时
        assert_eq!(r.hourly_wage_cents(), 1750);
    }

    #[test]
    fn hourly_wage_guards_zero_online() {
        let cfg = SimConfig::default();
        let r = rider(&cfg);
        assert_eq!(r.hourly_wage_cents(), 0);
    }

    #[test]
    fn weather_slows_down() {
        let cfg = SimConfig::default();
        let mut r = rider(&cfg);
        r.speed_mode = SpeedMode::Tolerant;
        let clear = r.speed_mps(&cfg, Weather::Clear);
        let storm = r.speed_mps(&cfg, Weather::Rainstorm);
        assert!(clear > storm * 1.5, "clear={clear} storm={storm}");
        // 45 km/h = 12.5 m/s
        assert!((clear - 12.5).abs() < 0.01);
    }

    #[test]
    fn idle_requires_online_and_empty_hands() {
        let cfg = SimConfig::default();
        let mut r = rider(&cfg);
        assert!(r.is_idle(0, &cfg));
        r.online = false;
        assert!(!r.is_idle(0, &cfg));
        r.online = true;
        r.frozen_until_min = 10;
        assert!(!r.is_idle(5, &cfg));
        assert!(r.is_idle(10, &cfg));
    }

    #[test]
    fn is_free_ignores_online_so_autopilot_can_pick_up_an_offline_rider() {
        let cfg = SimConfig::default();
        let mut r = rider(&cfg);
        r.online = false;
        assert!(!r.is_idle(0, &cfg), "离线不算空闲");
        assert!(r.is_free(0, &cfg), "但「手上没活」，AI 可以代管");

        r.frozen_until_min = 10;
        assert!(!r.is_free(5, &cfg), "被冻结就不是没活");
    }
}
