//! 骑手：属性、状态机、速度档位、疲劳。

use serde::{Deserialize, Serialize};

use super::{
    OrderId, RiderId,
    activity::ActivityProgress,
    config::{PEAK_THRESHOLD_MULTIPLIER, SimConfig},
    geo::LatLng,
    route::{LegKind, RouteTask},
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
    /// 等餐开始的**真实墙钟毫秒**。实时出餐单（`Order::prep_seconds`）按它算秒。
    ///
    /// 0 = 还没记过（老存档 / 单测构造），这种单退回按模拟分钟推进。
    #[serde(default)]
    pub wait_started_wall_ms: u64,
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
            wait_started_wall_ms: 0,
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
            &[
                Phase::ToShop,
                Phase::WaitingFood,
                Phase::ToCustomer,
                Phase::ArrivedCustomer
            ][..],
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
    /// 这一轮策略的生效时间（当日分钟），仅用于控制端展示与审计。
    pub set_at_min: u64,
    /// 这一轮策略的生效时间（**开局以来的绝对分钟**），用于过期检查。
    ///
    /// 不能用 `set_at_min` 做过期：那是当日分钟，跨天会归零。
    /// `None` = 不知道这轮策略何时下的（缺字段的老存档），**不是**「0 分钟」；
    /// 用 `Option` 而不是拿 0 当哨兵，是因为 0 是合法的开局时间戳。
    #[serde(default)]
    pub set_at_elapsed: Option<u64>,
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

/// 老存档缺 `mood` 时补的值。走 [`DEFAULT_MOOD`] 是为了和新骑手开局一致。
fn default_mood() -> f32 {
    super::config::DEFAULT_MOOD
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
    /// 情绪 0~1（玩法设计 §3.4）。低于 `mood.lazy_below` 就开始偷懒：
    /// 接单门槛上浮、骑得更慢，见 [`Rider::laziness`]。
    ///
    /// 和 `fatigue` 的分工：疲劳是**体力**，涨了就骑得慢（速度直接打折）；
    /// 情绪是**心情**，跌了才变懒（挑单 + 磨洋工），两者出口不同、来源也不同
    /// ——罚款 / 超时砸的是情绪，跑单时长砸的是疲劳。
    ///
    /// 老存档没有这个字段，默认给 [`DEFAULT_MOOD`]；**不能默认 0**——
    /// 那等于读一次档全员摆烂。
    #[serde(default = "default_mood")]
    pub mood: f32,

    pub online: bool,
    pub online_minutes: u32,
    /// 累计历史完成单量（决定等级，不随日结清零）。
    pub lifetime_completed: u32,

    pub held: Vec<HeldOrder>,
    /// 路线任务：骑手此刻真正在走的那条**腿队列**，队首是当前腿。
    ///
    /// 与 `held` 的分工：`held` 是「手上有哪些单」的清单（订单级状态 + 对外镜像），
    /// 这里才是「人在往哪走」。位置推进只认这条路线——从前每单一腿各推一次，
    /// 同一 tick 内互相拉扯，那才是抖动 / 瞬移的来源。
    ///
    /// `None` = 没在跑任何路段（手上没活，或停在同一处等餐）。
    #[serde(default)]
    pub route: Option<RouteTask>,
    /// 系统刚指派过来、骑手还没点「接受」的那张单。
    ///
    /// 为什么需要单独标记：「拒绝一张还没接受的指派单」和「取消一张
    /// 已经接下的单」代价完全不同——前者只吃每日拒单额度，后者要罚
    /// `fine.cancel_cents`（见 `scheduler::reject_order`）。而指派发生时
    /// 订单就已经挂进 `held` 了，光看 `held` 分不出这两种情况。
    #[serde(default)]
    pub assign_pending: Option<OrderId>,
    /// 今天因拒单额度用尽被强制下线。
    ///
    /// 与普通的 `online == false` 的区别：本人不能自己上线（`set_online` 会挡），
    /// 平台要派单只能派强制单；日结清零。手上没送完的单不受影响。
    #[serde(default)]
    pub forced_offline: bool,
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

    /// 平台活动进度，每人一份。老存档没有这个字段 = 没参加过任何活动。
    #[serde(default)]
    pub activities: Vec<ActivityProgress>,

    /// 最近几件值得记住的事，按时间**从旧到新**，最多 [`MAX_EPISODES`] 条。
    ///
    /// 私有是有意的：写入只留 [`Rider::push_episode`] 一个口子，
    /// 谁都没法直接 `push` 一下把上限绕过去。读走 [`Rider::episodes`]。
    /// 老存档没有这个字段 = 还没记过事。
    #[serde(default)]
    episodes: Vec<RiderEpisode>,
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

/// 每位骑手最多留几条近期经历。写满就丢最旧的。
///
/// 定成 5 条是**展示预算**而不是存储预算：这串东西迟早要进 LLM 提示词，
/// 每条都花 token，而「最近五件事」已经够说清这人这两天顺不顺。
/// 想放宽就改这里——上限只有一处，别在别处再写死一个数。
pub const MAX_EPISODES: usize = 5;

/// [`RiderEpisode::note`] 的字数上限。
///
/// 按**字符**算而不是字节：一个汉字三字节，按字节截会把汉字劈成乱码。
pub const MAX_EPISODE_NOTE_CHARS: usize = 24;

/// 一段近期经历的类型。
///
/// 只用粗粒度分类，不细分原因：细分留给 `note`（一句话）和订单本身。
/// 这些码进存档、也进将来的提示词，**改名等于改存档格式**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeKind {
    /// 送达一单（无论准时还是超时，`note` 里分）。
    Delivered,
    /// 拒了一单（拒掉平台指派、还没接下的那单）。
    Refused,
    /// 暂缓接单：进入观望窗口，一段时间内不接受自动补位。
    ///
    /// 注意别和 `Rider::held`（手上正在跑的单）混为一谈——这里的 `Held`
    /// 是「他主动往后退了一步」，不是「他手上多了一单」。
    Held,
    /// 被罚款（钱真的从账上扣掉了，`delta_cents` 为负）。
    Fined,
    /// 被强制派单这类不由自己决定的事。
    Forced,
}

/// 一条近期经历：骑手身上发生过的、值得记住的一件小事。
///
/// 与 [`LedgerEntry`] 的分工：账单记的是**钱**（每一笔收支都要有，可以很长），
/// 这里记的是**事**（只留最近几条，可以没有金额）。两者都挂在骑手身上、
/// 都按时间追加，但容量差了 40 倍——别把账单塞进经历。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiderEpisode {
    /// 事件发生的模拟时间：**自开局起的绝对分钟**（`World::elapsed_min`）。
    ///
    /// 不用当日分钟（`sim_minute`）：经历跨天不清，用当日分钟会让昨天的
    /// 23:50 排在今天的 00:10 后面。
    pub at_min: u64,
    pub kind: EpisodeKind,
    /// 关联订单。不是每件事都有单（比如罚款可能来自事故）。
    #[serde(default)]
    pub order_id: Option<OrderId>,
    /// 这次经历带来的**实际**经济变化，单位分：赚了为正、罚了为负、纯事件为 0。
    /// 不做估算——写进来的必须是账上真的动过的数。
    #[serde(default)]
    pub delta_cents: i64,
    /// 一句话说明。长度由 [`RiderEpisode::new`] 统一截断。
    #[serde(default)]
    pub note: String,
}

impl RiderEpisode {
    /// 造一条经历，顺手把 `note` 截到上限。
    ///
    /// **截断只写在这一处**：业务层照原话传进来，不自己剪——剪法一散开，
    /// 总有一条路径会漏，超长说明就会从那里漏进存档。
    pub fn new(
        at_min: u64,
        kind: EpisodeKind,
        order_id: Option<OrderId>,
        delta_cents: i64,
        note: impl AsRef<str>,
    ) -> Self {
        Self {
            at_min,
            kind,
            order_id,
            delta_cents,
            note: clamp_note(note.as_ref()),
        }
    }
}

/// 把说明截到 [`MAX_EPISODE_NOTE_CHARS`] 个字符。
///
/// 超长不报错、不拒绝：经历是**观测记录**，不是入参校验点——一句话写长了
/// 就不记，等于把真实发生的事吞掉。截断比丢事件好。
fn clamp_note(note: &str) -> String {
    note.chars().take(MAX_EPISODE_NOTE_CHARS).collect()
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
            forced_offline: false,
            rating: cfg.rating_rules.initial,
            level: 1,
            completed: 0,
            rejected: 0,
            late_count: 0,
            fatigue: 0.0,
            mood: cfg.mood.initial,
            online: false,
            online_minutes: 0,
            lifetime_completed: 0,
            held: Vec::new(),
            route: None,
            frozen_until_min: 0,
            injured_until_min: 0,
            strategy: SimStrategy::default(),
            policy_override: StrategyOverride::default(),
            home_lat: pos.lat,
            home_lng: pos.lng,
            recall_count: 0,
            last_ledger: Vec::new(),
            activities: Vec::new(),
            episodes: Vec::new(),
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

    /// 这单是不是骑手当前正在走的那条腿？
    ///
    /// **只有当前腿才推进位置与倒计时**。一个骑手同一时刻只有一条腿，
    /// 这就是「位置不再被多单互相拉扯」的落点。
    ///
    /// 没有路线信息时（老存档、绕过接单流程直接塞的单）**一律视为当前腿**，
    /// 退回「每单一腿」的旧行为——宁可偶尔抖一下，也不能让单烂在手里不动。
    pub fn is_current_leg(&self, order_id: OrderId) -> bool {
        match self.route.as_ref() {
            None => true,
            Some(route) => route.is_current(order_id),
        }
    }

    /// 路线跑空后把 `route` 收回 `None`，免得留一个空壳。
    pub fn prune_route(&mut self) {
        if self.route.as_ref().is_some_and(RouteTask::is_empty) {
            self.route = None;
        }
    }

    /// 从**手持单和路线**上一起摘掉这单。
    ///
    /// 只 `held.retain` 是旧做法：腿还赖在路线上，骑手就会一直以为「还得去这个地方」，
    /// 而队首被一张已经不存在的单占着，后面的单永远轮不到当前腿。
    pub fn drop_order(&mut self, order_id: OrderId) {
        self.held.retain(|h| h.order_id != order_id);
        if let Some(route) = self.route.as_mut() {
            route.remove_order(order_id);
        }
        self.prune_route();
    }

    /// 骑手到了一处取餐点：把该地点**所有**取餐腿一次性出队。
    ///
    /// 同店多单本来就在同一处取餐，人到店了这批餐就都算「已到取餐点」；若只弹
    /// 当前那条，第二单还得再空耗一遍到店时长——人明明已经站在店里。
    ///
    /// 自动（[`super::tick`]）和真人确认（[`super::scheduler`]）两条路都走这里，
    /// 免得两边对「到店」的理解长歪。
    pub fn complete_pickup_legs_at_current(&mut self) {
        let Some(shop) = self
            .route
            .as_ref()
            .and_then(RouteTask::current)
            .filter(|l| l.kind == LegKind::Pickup)
            .map(|l| l.pos)
        else {
            return;
        };
        if let Some(route) = self.route.as_mut() {
            route.complete_pickups_at(shop);
        }
        self.prune_route();
    }

    /// 当前腿的目标点——人此刻真正朝着的那个点。
    ///
    /// 顺路度、朝向提示都该读它，而不是 `held.first()`（那是「接单先后」，
    /// 跟脚步没关系）。
    pub fn current_leg_pos(&self) -> Option<LatLng> {
        self.route.as_ref().and_then(RouteTask::current_pos)
    }

    pub fn held_mut(&mut self, order_id: OrderId) -> Option<&mut HeldOrder> {
        self.held.iter_mut().find(|h| h.order_id == order_id)
    }

    /// 当前速度 km/h：**生效档位**限速 × 天气 × 疲劳削减 × 情绪折扣。
    ///
    /// 读 `effective_speed_mode` 而不是 `speed_mode`：LLM 的速度覆盖必须真的
    /// 改变腿时长与风险，而不只是提示词里好看。
    pub fn speed_kmh(&self, cfg: &SimConfig, weather: Weather) -> f32 {
        let limit = cfg.speed.complaint_speed_kmh[self.effective_speed_mode(cfg).index()];
        let fatigue_penalty = 1.0 - self.fatigue.clamp(0.0, 1.0) * cfg.fatigue.speed_penalty_max;
        let lazy_penalty = self.lazy_speed_factor(cfg);
        (limit * weather.speed_factor() * fatigue_penalty * lazy_penalty).max(3.0)
    }

    /// 懒惰系数 0~1：情绪越低越大，`mood.lazy_below` 及以上恒为 0。
    ///
    /// **懒惰不等于罢工**：它是一个系数，从两个软出口渗出来——接单时薪门槛
    /// 上浮（更挑单）、骑行速度打折（磨洋工）。情绪见底才拉满，
    /// 情绪正常时系数为 0，整套机制对原有行为零影响。
    pub fn laziness(&self, cfg: &SimConfig) -> f32 {
        let m = &cfg.mood;
        if !m.enabled || m.lazy_below <= 0.0 {
            return 0.0;
        }
        ((m.lazy_below - self.mood) / m.lazy_below).clamp(0.0, 1.0)
    }

    /// 懒惰对速度的折扣（1.0 = 不打折）。
    pub fn lazy_speed_factor(&self, cfg: &SimConfig) -> f32 {
        let lazy = self.laziness(cfg);
        1.0 - lazy * (1.0 - cfg.mood.lazy_speed_min.clamp(0.1, 1.0))
    }

    /// 情绪变动。**统一入口**：所有加减都走这里，才能保证钳在 0~1。
    pub fn shift_mood(&mut self, delta: f32, cfg: &SimConfig) {
        if !cfg.mood.enabled {
            return;
        }
        self.mood = (self.mood + delta).clamp(0.0, 1.0);
    }

    /// 情绪是不是已经低到在偷懒了（控制端标状态用）。
    pub fn is_slacking(&self, cfg: &SimConfig) -> bool {
        self.laziness(cfg) > 0.0
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
    /// 四层倍率叠加：`骑手基准 × 高峰系数 × LLM 单价档位倍率 × 懒惰上浮`。
    /// 默认档位是 `Normal`（×1.0）、情绪正常时懒惰系数为 0，
    /// 所以 LLM 与情绪都关闭 / 无感时，结果与从前完全一致。
    pub fn effective_threshold_cents_hour(&self, is_peak: bool, cfg: &SimConfig) -> f32 {
        let base = self.strategy.accept_threshold_cents_hour as f32;
        let peak = if is_peak {
            PEAK_THRESHOLD_MULTIPLIER
        } else {
            1.0
        };
        let lazy = 1.0 + self.laziness(cfg) * (cfg.mood.lazy_threshold_max.max(1.0) - 1.0);
        base * peak * self.policy_override.price_tier.multiplier() * lazy
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

    /// 最近的经历，按时间**从旧到新**（最后一条是最新的）。
    ///
    /// 顺序由追加顺序定死（`Vec`，不是 `HashMap`）：同一分钟内发生两件事时
    /// 先后就按调用顺序，重放可复现。
    pub fn episodes(&self) -> &[RiderEpisode] {
        &self.episodes
    }

    /// 记一条经历。**所有写入都必须走这里**：满了就从最旧的开始丢。
    ///
    /// `note` 在这里再截一次：字段是公开的，有人完全可能绕过
    /// [`RiderEpisode::new`] 直接拼一个结构体出来。容量上限是**存储**的性质，
    /// 不能指望调用方守规矩。
    ///
    /// 不在本轮撒到各个业务事件里——先把「记得住、有上限」立起来，
    /// 写哪些事件是下一步的事。
    pub fn push_episode(&mut self, mut episode: RiderEpisode) {
        episode.note = clamp_note(&episode.note);
        self.episodes.push(episode);
        if self.episodes.len() > MAX_EPISODES {
            let drop = self.episodes.len() - MAX_EPISODES;
            self.episodes.drain(0..drop);
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

    /// 速度覆盖必须真的改变实际速度，且**不能写穿个人档位**；
    /// 覆盖一撤，速度立刻回到个人基线。
    #[test]
    fn speed_policy_override_changes_actual_speed_and_restores_the_baseline() {
        let cfg = SimConfig {
            allow_speeding: true,
            allow_unrestricted: true,
            ..Default::default()
        };
        let mut r = rider(&cfg);
        r.speed_mode = SpeedMode::Compliant;

        let base = r.speed_kmh(&cfg, Weather::Clear);
        r.policy_override.mode_override = Some(SpeedMode::Unrestricted);
        let fast = r.speed_kmh(&cfg, Weather::Clear);
        assert!(fast > base * 2.0, "base={base} fast={fast}");
        assert_eq!(r.speed_mode, SpeedMode::Compliant, "覆盖不得改写个人档位");

        r.policy_override = Default::default();
        assert!((r.speed_kmh(&cfg, Weather::Clear) - base).abs() < 0.01);
    }

    /// 系统开关仍然是一票否决：没放开解限速，LLM 说 2 也没用。
    #[test]
    fn speed_policy_override_still_respects_the_system_switches() {
        let cfg = SimConfig::default();
        let mut r = rider(&cfg);
        r.speed_mode = SpeedMode::Compliant;
        r.policy_override.mode_override = Some(SpeedMode::Unrestricted);
        assert_eq!(r.effective_speed_mode(&cfg), SpeedMode::Compliant);
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

    /// 情绪正常时必须**完全无感**——这是这套机制敢默认开着的底线。
    #[test]
    fn a_normal_mood_rider_behaves_exactly_as_before() {
        let cfg = SimConfig::default();
        let r = rider(&cfg);
        assert_eq!(r.mood, cfg.mood.initial);
        assert_eq!(r.laziness(&cfg), 0.0);
        assert_eq!(r.lazy_speed_factor(&cfg), 1.0);
        assert!(!r.is_slacking(&cfg));

        let base = r.strategy.accept_threshold_cents_hour as f32 * PEAK_THRESHOLD_MULTIPLIER;
        assert_eq!(r.effective_threshold_cents_hour(true, &cfg), base);
    }

    /// 情绪跌到 `lazy_below` 之下才开始懒，而且是**线性**变懒：
    /// 门槛上浮、速度打折，两个出口一起收紧。
    #[test]
    fn low_mood_makes_the_rider_pickier_and_slower() {
        let cfg = SimConfig::default();
        let mut r = rider(&cfg);
        let fresh_thr = r.effective_threshold_cents_hour(false, &cfg);
        let fresh_spd = r.speed_kmh(&cfg, Weather::Clear);

        // 刚好卡在阈值上：还不懒
        r.mood = cfg.mood.lazy_below;
        assert_eq!(r.laziness(&cfg), 0.0);

        // 中点：系数正好一半
        r.mood = cfg.mood.lazy_below / 2.0;
        assert!((r.laziness(&cfg) - 0.5).abs() < 1e-6);

        // 见底：懒满
        r.mood = 0.0;
        assert_eq!(r.laziness(&cfg), 1.0);
        assert!(r.is_slacking(&cfg));
        let lazy_thr = r.effective_threshold_cents_hour(false, &cfg);
        let lazy_spd = r.speed_kmh(&cfg, Weather::Clear);
        assert!((lazy_thr / fresh_thr - cfg.mood.lazy_threshold_max).abs() < 1e-4);
        assert!((lazy_spd / fresh_spd - cfg.mood.lazy_speed_min).abs() < 1e-4);
        assert!(lazy_thr > fresh_thr, "懒了就挑单");
        assert!(lazy_spd < fresh_spd, "懒了就磨洋工");
    }

    /// 关掉开关 = 整套机制消失，而不是「懒了一半」。
    #[test]
    fn switching_mood_off_restores_the_old_behaviour() {
        let mut cfg = SimConfig::default();
        let mut r = rider(&cfg);
        r.mood = 0.0;
        assert!(r.is_slacking(&cfg));
        let lazy_spd = r.speed_kmh(&cfg, Weather::Clear);

        cfg.mood.enabled = false;
        assert_eq!(r.laziness(&cfg), 0.0);
        assert_eq!(r.lazy_speed_factor(&cfg), 1.0);
        assert!(!r.is_slacking(&cfg));
        assert!(r.speed_kmh(&cfg, Weather::Clear) > lazy_spd);
    }

    /// 配错的折扣系数不该造出「越懒越快」或者「速度归零」这种怪东西。
    #[test]
    fn lazy_factors_are_clamped() {
        let mut cfg = SimConfig::default();
        let mut r = rider(&cfg);
        r.mood = 0.0;

        cfg.mood.lazy_speed_min = 3.0;
        assert_eq!(r.lazy_speed_factor(&cfg), 1.0, "被夹回 1.0，不会变成加速");

        cfg.mood.lazy_speed_min = -1.0;
        assert!(
            (r.lazy_speed_factor(&cfg) - 0.1).abs() < 1e-6,
            "下限夹在 0.1，速度不会归零"
        );
    }

    /// 情绪永远钳在 0~1。**不能让它变负**：懒惰系数是除法算出来的，
    /// 负情绪会算出大于 1 的系数，门槛和速度都会翻车。
    #[test]
    fn shift_mood_never_leaves_the_unit_range() {
        let cfg = SimConfig::default();
        let mut r = rider(&cfg);
        r.shift_mood(-99.0, &cfg);
        assert_eq!(r.mood, 0.0);
        assert_eq!(r.laziness(&cfg), 1.0);
        r.shift_mood(99.0, &cfg);
        assert_eq!(r.mood, 1.0);

        // 开关关掉后连值都不该动
        let mut off = SimConfig::default();
        off.mood.enabled = false;
        let before = r.mood;
        r.shift_mood(-0.5, &off);
        assert_eq!(r.mood, before);
    }

    /// 老存档没有 `mood` 字段，读进来必须补默认值而不是 0——
    /// **默认 0 等于读一次档全员摆烂**。
    #[test]
    fn old_saves_without_mood_get_the_default() {
        let cfg = SimConfig::default();
        let r = rider(&cfg);
        let mut v = serde_json::to_value(&r).unwrap();
        v.as_object_mut().unwrap().remove("mood");
        let back: Rider = serde_json::from_value(v).unwrap();
        assert_eq!(back.mood, crate::sim::config::DEFAULT_MOOD);
    }

    // ---- 近期经历（有界的）----

    /// 造一条经历。测试只关心「记进去之后还在不在、顺序对不对」，
    /// 金额与订单号给个固定的非零值，方便断言没被改写。
    fn episode(at_min: u64, kind: EpisodeKind, note: &str) -> RiderEpisode {
        RiderEpisode::new(at_min, kind, Some(at_min as OrderId + 1), 0, note)
    }

    /// 新骑手身上一条经历都没有。
    #[test]
    fn a_new_rider_starts_with_no_episodes() {
        let cfg = SimConfig::default();
        assert!(rider(&cfg).episodes().is_empty());
    }

    /// 五个字段原样保存，一个都不许丢或改。
    #[test]
    fn an_episode_keeps_every_field_it_was_given() {
        let cfg = SimConfig::default();
        let mut r = rider(&cfg);
        r.push_episode(RiderEpisode::new(
            137,
            EpisodeKind::Fined,
            Some(42),
            -500,
            "逆行被拍",
        ));

        let e = &r.episodes()[0];
        assert_eq!(e.at_min, 137);
        assert_eq!(e.kind, EpisodeKind::Fined);
        assert_eq!(e.order_id, Some(42));
        assert_eq!(e.delta_cents, -500);
        assert_eq!(e.note, "逆行被拍");
    }

    /// 第 6 条进来时最旧的被挤掉：上限就是 5 条，不靠调用方自觉。
    #[test]
    fn only_the_latest_five_episodes_are_kept() {
        let cfg = SimConfig::default();
        let mut r = rider(&cfg);
        for i in 0..(MAX_EPISODES as u64 + 1) {
            r.push_episode(episode(i, EpisodeKind::Delivered, "送达"));
        }

        assert_eq!(r.episodes().len(), MAX_EPISODES);
        assert_eq!(r.episodes()[0].at_min, 1, "丢掉的是最旧的 0 号");
        assert_eq!(
            r.episodes()[MAX_EPISODES - 1].at_min,
            MAX_EPISODES as u64,
            "最新一条在末尾"
        );
    }

    /// 顺序 = 追加顺序（旧在前、新在后），不依赖任何遍历顺序。
    #[test]
    fn episodes_are_ordered_oldest_first() {
        let cfg = SimConfig::default();
        let mut r = rider(&cfg);
        for (i, kind) in [EpisodeKind::Held, EpisodeKind::Refused, EpisodeKind::Forced]
            .into_iter()
            .enumerate()
        {
            r.push_episode(RiderEpisode::new(i as u64 * 30, kind, None, 0, ""));
        }

        let kinds: Vec<EpisodeKind> = r.episodes().iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![EpisodeKind::Held, EpisodeKind::Refused, EpisodeKind::Forced]
        );
        let times: Vec<u64> = r.episodes().iter().map(|e| e.at_min).collect();
        assert_eq!(times, vec![0, 30, 60]);
    }

    /// 老存档没有 `episodes` 字段也能读，且读出来是空而不是报错。
    #[test]
    fn a_legacy_rider_without_episodes_still_loads() {
        let cfg = SimConfig::default();
        let mut r = rider(&cfg);
        r.push_episode(episode(9, EpisodeKind::Delivered, "送达"));
        let mut v = serde_json::to_value(&r).unwrap();
        v.as_object_mut().unwrap().remove("episodes");

        let back: Rider = serde_json::from_value(v).unwrap();
        assert!(back.episodes().is_empty(), "缺字段 = 还没记过事");
        assert_eq!(back.name, r.name, "其余字段照旧");
    }

    /// 日结照旧清当日计数与账单，但**不动**经历。
    #[test]
    fn closing_the_day_keeps_the_episodes() {
        let cfg = SimConfig::default();
        let mut r = rider(&cfg);
        r.completed = 4;
        r.push_ledger(LedgerEntry {
            at_min: 10,
            order_id: Some(1),
            kind: "delivery".to_string(),
            amount_cents: 800,
            reason: "测试".to_string(),
        });
        r.push_episode(episode(10, EpisodeKind::Delivered, "准时送达"));
        r.push_episode(episode(20, EpisodeKind::Fined, "超时赔付"));
        let before = r.episodes().to_vec();

        let _ = crate::sim::economy::close_day(&cfg, &mut r);

        assert_eq!(r.completed, 0, "当日计数照旧清零");
        assert!(r.last_ledger.is_empty(), "当天账单照旧清空");
        assert_eq!(r.episodes(), before.as_slice(), "经历不随日结清空");
    }

    /// `note` 被截到上限，而且是**按字符**截：中文不会被劈成半个字。
    #[test]
    fn a_long_note_is_cut_to_the_limit() {
        let cfg = SimConfig::default();
        let mut r = rider(&cfg);
        let long = "超".repeat(MAX_EPISODE_NOTE_CHARS * 8);
        r.push_episode(RiderEpisode::new(1, EpisodeKind::Held, None, 0, long));

        let note = &r.episodes()[0].note;
        assert_eq!(note.chars().count(), MAX_EPISODE_NOTE_CHARS);
        assert_eq!(
            note.len(),
            MAX_EPISODE_NOTE_CHARS * "超".len(),
            "按字符截，不是按字节"
        );
        assert_eq!(note, &"超".repeat(MAX_EPISODE_NOTE_CHARS));

        // 没超长的原样留着，别顺手加个省略号之类的东西。
        r.push_episode(RiderEpisode::new(
            2,
            EpisodeKind::Held,
            None,
            0,
            "正好五个字",
        ));
        assert_eq!(r.episodes()[1].note, "正好五个字");
    }

    /// 就算绕过 `RiderEpisode::new` 直接拼结构体，写进去的说明照样被截：
    /// 上限是**存储**的性质，不是构造函数的口头约定。
    #[test]
    fn a_note_written_around_the_constructor_is_still_clamped() {
        let cfg = SimConfig::default();
        let mut r = rider(&cfg);
        r.push_episode(RiderEpisode {
            at_min: 1,
            kind: EpisodeKind::Held,
            order_id: None,
            delta_cents: 0,
            note: "长".repeat(MAX_EPISODE_NOTE_CHARS * 4),
        });

        assert_eq!(r.episodes()[0].note.chars().count(), MAX_EPISODE_NOTE_CHARS);
    }

    /// 存档往返之后，内容和顺序都不变；`kind` 走的是 snake_case 码。
    #[test]
    fn episodes_survive_a_save_round_trip() {
        let cfg = SimConfig::default();
        let mut r = rider(&cfg);
        for i in 0..MAX_EPISODES as u64 {
            r.push_episode(RiderEpisode::new(
                i,
                EpisodeKind::Delivered,
                Some(i as OrderId),
                i as i64 * 10 + 1,
                format!("第{i}单"),
            ));
        }

        let json = serde_json::to_string(&r).unwrap();
        let back: Rider = serde_json::from_str(&json).unwrap();

        assert_eq!(back.episodes(), r.episodes());
        assert_eq!(back.episodes().len(), MAX_EPISODES);
        let times: Vec<u64> = back.episodes().iter().map(|e| e.at_min).collect();
        assert_eq!(times, vec![0, 1, 2, 3, 4], "顺序必须原样保留");
        assert!(
            json.contains("\"delivered\""),
            "类型的对外码是 snake_case，不是 Debug 名"
        );
    }
}
