//! 骑手：属性、状态机、速度档位、疲劳。

use serde::{Deserialize, Serialize};

use super::{OrderId, RiderId, config::SimConfig, geo::LatLng, weather::Weather};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiderKind {
    Simulated,
    Human,
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
    /// 倒计时已归零、等骑手点按钮（真人骑手才有）。
    pub awaiting_ack: bool,
    /// 背单（多背一单）。
    pub back_order: bool,
    /// 等餐已经开始，记一下用于罚款判定。
    pub wait_started_min: u64,
    /// 等餐超时罚单是否已经开过（每单只罚一次）。
    pub wait_fined: bool,
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
            awaiting_ack: false,
            back_order,
            wait_started_min: now_min,
            wait_fined: false,
        }
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
    /// 被拍冻结到（自开局起算的模拟分钟）。
    pub frozen_until_min: u64,
    /// 事故停工到。
    pub injured_until_min: u64,

    pub strategy: SimStrategy,
    /// 日常活动中心（用于派单距离偏好）。
    pub home_lat: f64,
    pub home_lng: f64,

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
            home_lat: pos.lat,
            home_lng: pos.lng,
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

    /// 空闲 = 在线 + 手上没单 + 没受伤 + 没被冻结 + 没到单量上限。
    pub fn is_idle(&self, now_min: u64, cfg: &SimConfig) -> bool {
        self.online
            && self.held.is_empty()
            && !self.is_injured(now_min)
            && !self.is_frozen(now_min)
            && !self.reached_daily_cap(cfg)
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

    /// 在线小时数（等餐时间也算在线——这是真实骑手的痛点）。
    pub fn online_hours(&self) -> f32 {
        self.online_minutes as f32 / 60.0
    }

    /// 时薪（分/小时）。**这是整个模拟器的核心 KPI。**
    ///
    /// 注意：`cash_cents` 是**已经**扣过抽水、罚单、保险，并抵扣过欠款的净额
    /// （见 `economy::debit` / `economy::credit`）。所以这里不能再减一次
    /// `fines_cents` / `insurance_cents`，否则就是重复扣款。`fines_cents` 与
    /// `insurance_cents` 只用于账单明细展示。
    pub fn hourly_wage_cents(&self) -> i64 {
        let hours = self.online_hours();
        if hours < 0.05 {
            return 0;
        }
        (self.cash_cents as f32 / hours).round() as i64
    }

    /// 今日到手 = 真正落袋的净额。恒 ≥ 0（扣不到的部分记在 `debt_cents`）。
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
}
