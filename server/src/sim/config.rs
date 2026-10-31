//! 数值速查表（玩法设计 §12）。**改平衡只改这里。**
//!
//! 所有金额单位一律「分」（i64）。时间单位一律「模拟分钟」，除非字段名带 `_secs`。

use serde::{Deserialize, Serialize};

use super::geo::CoordSystem;

/// 实时档下一个 tick 的真实时长：1 模拟分钟 = 真实 60 秒。
///
/// 这是「真实时间」与「1 分钟 1 秒」两个模式的分界：倍率档位压缩真实时间，
/// 实时档让模拟时钟跟真实钟表同步走。
pub const REALTIME_TICK_MS: u64 = 60_000;

/// 订单重投上限，**不是流单线**。
///
/// 抢单窗口内无人接、或骑手取消已接单时，订单**不消失**，而是加价重回大厅；
/// 连续这么多次都没人接之后，改由 [`crate::sim::scheduler::rescue_nearest`]
/// **强制指派给最近的骑手**。
///
/// 真实场景里订单不会因为一时没人接就作废——平台会加价、扩大派单范围，
/// 直到有骑手接走。所以这里从头到尾都不存在「流单」这个出口。
pub const MAX_REDISPATCH_ATTEMPTS: u32 = 3;

/// 每次重投的加价（分）。补贴全给骑手，平台不抽水。
pub const REDISPATCH_SURCHARGE_STEP_CENTS: i64 = 100;

/// 订单退回大厅重投时，给接手骑手保底留出的剩余时限（分钟）。
///
/// 单子在大厅里空转掉的时间不该由最终接手的骑手背：现实中平台重派时会重新给时限。
/// 没这个保底，「超时救单」塞给骑手的每一单都是注定超时的单，白跑还倒贴罚金。
pub const REDISPATCH_PROMISE_FLOOR_MIN: u32 = 30;

/// 高峰时段商家的出餐系数：实际出餐 = 商家均值 × 这个倍数。
///
/// **两处必须共用同一个值**，否则「承诺时限」和「真实出餐」各算各的——
/// 历史上正是这个不同步，让高峰期几乎每一单都注定超时：
/// - [`crate::sim::scheduler::attach`]：定这一单实际要等多久；
/// - [`crate::sim::ai::promise_minutes`]：定生成订单时给顾客的承诺时限。
pub const PEAK_PREP_FACTOR: f32 = 2.0;

/// 暴雨叠加高峰时的额外出餐系数。同样两处共用。
pub const STORM_PREP_FACTOR: f32 = 1.5;

/// 生成订单时，估计「骑手到店」用的典型接单距离（米，直线）。
///
/// 下单那刻还不知道谁会接，只能用抢单半径内的典型值代一代。
/// 不估这一段，承诺里就只剩「出餐 + 从商家到顾客」，骑手在路上跑的那截全靠分档缓冲硬兜。
pub const TYPICAL_TO_SHOP_M: f32 = 1_200.0;

/// 承诺时限的上下限（分钟）。
///
/// 上限给到 120：暴雨叠加高峰时「到店 + 出餐 + 送到 + **末端放置**」
/// 真的会到 100 分钟上下。原来是 90，那是**加末端放置窗口之前**定的——
/// 那时候承诺里不含那段（均值32.5 分钟），90 够用；窗口加入后，
/// 实测有 1/3 的订单（3km 单）承诺被 clamp 到上限、于是「到达即超时」。
/// 卡在 75 或 90 会把长单硬压成必然超时。
///
/// 下限 15 不动：太短的承诺没有博弈空间，骑手看一眼就不接。
pub const PROMISE_MINUTES_RANGE: (u32, u32) = (15, 120);

/// 配送费规则。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeRules {
    pub base_fee_cents: i64,
    /// 距离分段**下界**（米）与对应的总附加费（分）。
    ///
    /// 语义：取「不超过本单距离的最大门槛」的 `cents` 作为附加费。
    /// 对应玩法设计 §1.1：`(1500,100) (3000,200) (5000,350) (8000,350)`，
    /// 即 1.5km 以下 0 元、1.5~3km 1.00 元、3~5km 2.00 元、5~8km 3.50 元，
    /// 超过 8000m 后再按 `per_km_over_cents` 叠加。
    pub distance_tiers: Vec<(u32, i64)>,
    /// 超过最后一档后每公里附加（分/km）。
    pub per_km_over_cents: i64,
    /// 通勤高峰系数：(起始分钟, 结束分钟, 系数)
    pub surge_windows: Vec<(u32, u32, f32)>,
    /// 等级加成：每级 20 分。
    pub level_bonus_per_level_cents: i64,
}

/// 罚单规则。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FineRules {
    pub wait_timeout_cents: i64,
    pub deliver_timeout_cents: i64,
    pub camera_cents: i64,
    pub complaint_cents: i64,
    pub cancel_cents: i64,
    pub backorder_collapse_cents: i64,
    pub insurance_per_day_cents: i64,
    pub accident_medical_min_cents: i64,
    pub accident_medical_max_cents: i64,
    /// 被拍冻结账号（分钟）——比罚款更痛。
    pub camera_detention_min: u32,
}

/// 背单规则。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackOrderRules {
    pub max_held: u32,
    pub detour_threshold_m: u32,
    pub bonus_min_cents: i64,
    pub bonus_max_cents: i64,
    /// 承诺时限后的缓冲（分钟）。
    pub late_buffer_min: u32,
    /// 同店合并出餐：为凑成一炉，最早好的那份**最多**再等多几分钟。
    ///
    /// 有上限是为了不让一份慢餐绑架已经好了的餐——那等于拿顾客的时间补贴
    /// 「一起送」。设 0 就退回各出各的。
    #[serde(default = "default_merge_wait_min")]
    pub merge_wait_max_min: u32,
}

/// [`BackOrderRules::merge_wait_max_min`] 的缺省值：老存档没这个字段时用它。
fn default_merge_wait_min() -> u32 {
    3
}

/// 合并单 / 凑单规则。
///
/// 同一家店的多单该一起取、一起送——这是真实骑手最主要的省钱手段。
/// 这几项管的就是「什么时候该把单凑到一起」，以及「别让一家店把单全发了」。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeRules {
    /// 单个商家同时存在的**未完成**订单上限。0 = 不限。默认 2。
    ///
    /// 没有它，热门店（`rating^6` 加权）能把整条街的单都发出大厅，
    /// 一堆同店单谁也没法合并，只能一个个单独跑。
    #[serde(default = "default_merchant_order_cap")]
    pub max_active_orders_per_merchant: u32,
    /// 算法同一批里允许给**一个骑手**派几单。默认 1（＝原来的一批一单）。
    pub algorithm_max_per_rider: u32,
    /// 凑单窗口（分钟）：骑手到店后愿意多留一会儿，等同店的新单派过来。
    /// 0 = 不等。
    #[serde(default = "default_merge_window_min")]
    pub wait_for_merge_min: u32,
}

fn default_merchant_order_cap() -> u32 {
    2
}

fn default_merge_window_min() -> u32 {
    2
}

impl Default for MergeRules {
    fn default() -> Self {
        Self {
            max_active_orders_per_merchant: default_merchant_order_cap(),
            algorithm_max_per_rider: 1,
            wait_for_merge_min: default_merge_window_min(),
        }
    }
}

/// 速度档位规则（三档）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeedRules {
    pub complaint_speed_kmh: [f32; 3],
    /// 时薪系数：**只用于期望值展示与 AI 策略，不直接乘到账单上**。
    /// 真实的时薪差异由行程时间自然产生。
    pub hourly_coefficient: [f32; 3],
    pub accident_rate: [f32; 3],
    pub camera_rate: [f32; 3],
    pub rating_delta: [f32; 3],
    pub detain_min: u32,
    /// 合规档的基准速度 km/h（用于行程时间）。
    pub base_speed_kmh: f32,
}

/// 评分规则。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RatingRules {
    pub initial: f32,
    pub min: f32,
    pub max: f32,
    pub on_time: f32,
    pub late: f32,
    pub accident: f32,
    pub good_review: f32,
    pub bad_review_max: f32,
    pub tier_normal: f32,
    pub tier_premium: f32,
    pub tier_express: f32,
}

/// 等级规则。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LevelRules {
    /// 各等级起始单量：0/50/150/400/1000/2500
    pub thresholds: Vec<u32>,
    pub coefficients: Vec<f32>,
}

/// 高峰期挑单系数：高峰时只接更高时薪的单（玩法设计 §5.1）。
///
/// 与 LLM 的「期望单价档位倍率」是**相乘**关系，两者互不替代：
/// 这一层是系统定的，LLM 那层是骑手 AI 定的。
pub const PEAK_THRESHOLD_MULTIPLIER: f32 = 1.15;

/// 步行规则（《LLM协议设计》§5.1）。
///
/// 现实中骑手到商场要停车、走路进楼、找店、坐电梯——「商场单」难跑就在这里。
/// 这些量是**物理量**：只由代码给出，绝不接受 LLM 干预（否则轨迹不可复现）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WalkRules {
    /// 步行速度 m/s。1.2 是正常步速，负重（提餐）取 1.0。
    pub walk_speed_mps: f32,
    /// 按商家步行难度分档的额外步行距离（米）。
    ///
    /// 0 = 路边店，停车即取；1 = 商场内店，需步行进入；2 = 高层 / 深巷，电梯或长距步行。
    pub merchant_walk_m: [u32; 3],
    /// 电梯 / 找店的固定时间罚（秒）。
    pub elevator_penalty_secs: u32,
    /// 交付端（送到顾客）的步行难度分档距离（米）。**为往返距离**。
    pub customer_walk_m: [u32; 3],
}

impl Default for WalkRules {
    fn default() -> Self {
        Self {
            walk_speed_mps: 1.2,
            merchant_walk_m: [0, 120, 350],
            elevator_penalty_secs: 90,
            customer_walk_m: [30, 100, 280],
        }
    }
}

impl WalkRules {
    /// 取某一档的商家步行距离，越界一律按 0 处理（不 panic）。
    pub fn merchant_walk_m(&self, difficulty: u8) -> u32 {
        *self.merchant_walk_m.get(difficulty as usize).unwrap_or(&0)
    }

    /// 取某一档的顾客步行距离（往返），越界按 0 处理。
    pub fn customer_walk_m(&self, difficulty: u8) -> u32 {
        *self.customer_walk_m.get(difficulty as usize).unwrap_or(&0)
    }

    /// 该档是否需要额外付出电梯 / 找店时间罚。
    pub fn needs_elevator(&self, difficulty: u8) -> bool {
        difficulty >= 2
    }

    pub fn walk_speed_mps(&self) -> f32 {
        self.walk_speed_mps.max(0.5)
    }
}

/// 疲劳规则。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct FatigueRules {
    pub per_online_hour: f32,
    pub per_order: f32,
    pub per_rest_10min: f32,
    pub max: f32,
    /// 疲劳对速度上限的最大削减比例。
    pub speed_penalty_max: f32,
    /// 疲劳对事故率的加权。
    pub accident_weight: f32,
}

/// 事故规则（玩法设计 §3.3）。
///
/// 真实掷骰只认这一组加 [`SpeedRules::accident_rate`]：
/// `档位基准率 × 天气加成 × 全局倍率 + 疲劳惩罚`，再被 `max_probability` 夹一刀。
///
/// **为什么要有「全局倍率」**：档位基准率（0.5% / 4% / 22%）是文档定死的教学数字，
/// 但那是「一单 22%」——跑一天几十单，体感就是一直在进医院。倍率是给控制端
/// **调手感**用的，不去篡改设计值本身。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AccidentRules {
    /// 总开关。关掉就完全不掷事故（演示 / 排查用）。
    pub enabled: bool,
    /// 全局倍率。`1.0` = 文档设计值。
    pub rate_multiplier: f32,
    /// 概率上限。
    ///
    /// **没有它，「解除限速 + 暴雨」会直接推到必出事**：22% × 6 = 132%，
    /// 再叠疲劳惩罚。钳到 0.6 之后最坏也就是「六成概率」，仍是重罚但可玩。
    pub max_probability: f32,
}

impl Default for AccidentRules {
    fn default() -> Self {
        Self {
            enabled: true,
            rate_multiplier: 1.0,
            max_probability: 0.6,
        }
    }
}

/// 新骑手的默认开局情绪。
///
/// 抽成常量是因为它有两个读法：真实新建走 `MoodRules::initial`，
/// 老存档反序列化走 `Rider::mood` 的 `#[serde(default)]`——两者必须同值，
/// 否则「读档进来的骑手」和「新加进来的骑手」开局情绪不一样。
pub const DEFAULT_MOOD: f32 = 0.70;

/// 情绪规则：**情绪低就变懒**——挑单、骑得慢。
///
/// 情绪取值 0~1，`lazy_below` 之上是「正常干活」，之下才开始偷懒，
/// 懒惰系数 `(lazy_below − mood) / lazy_below` 线性放大到两个出口：
/// 接单时薪门槛上浮、骑行速度打折。也就是说「累了烦了」不是硬停机，
/// 而是**悄悄变难派、变慢**——这才像真人。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MoodRules {
    pub enabled: bool,
    /// 开局情绪（0~1）。
    pub initial: f32,
    /// 低于这个值才开始懒。高于它时懒惰系数恒为 0。
    pub lazy_below: f32,
    /// 情绪见底时，接单时薪门槛最多 ×几。`1.0` = 情绪不影响接单。
    pub lazy_threshold_max: f32,
    /// 情绪见底时，骑行速度最多打几折。`1.0` = 情绪不影响速度。
    pub lazy_speed_min: f32,
    /// 每笔罚款掉多少情绪。
    pub per_fine: f32,
    /// 每单超时掉多少情绪。
    pub per_late: f32,
    /// 每单准时送达涨多少情绪。
    pub per_on_time: f32,
    /// 在线每小时掉多少情绪（在线越久越烦）。
    pub per_online_hour: f32,
    /// 收工（下线）时一次性回多少情绪。
    pub per_rest: f32,
}

impl Default for MoodRules {
    fn default() -> Self {
        Self {
            enabled: true,
            initial: DEFAULT_MOOD,
            lazy_below: 0.50,
            lazy_threshold_max: 1.35,
            lazy_speed_min: 0.85,
            per_fine: -0.06,
            per_late: -0.04,
            per_on_time: 0.008,
            per_online_hour: -0.05,
            per_rest: 0.12,
        }
    }
}

/// `dispatch_radius_km` 的 serde 默认值。
///
/// 单独一个函数是因为 `#[serde(default = "...")]` 只认函数路径，不认关联常量。
/// 具体数值仍然取自 `dispatch` 模块，保持"一个数只写一次"。
fn default_dispatch_radius_km() -> f64 {
    crate::sim::dispatch::RADIUS_DEFAULT_KM
}

/// `recommend_percent` 的 serde 默认值：老配置没有这个字段时用 20%。
fn default_recommend_percent() -> u32 {
    crate::sim::dispatch::RECOMMEND_PERCENT_DEFAULT
}

/// 单局可调参数（控制端能改的都在这）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimConfig {
    pub seed: u64,
    pub fee: FeeRules,
    pub fine: FineRules,
    pub backorder: BackOrderRules,
    /// 合并单 / 凑单规则（同店合并、算法每骑手上限、商家订单上限）。
    #[serde(default)]
    pub merge: MergeRules,
    pub speed: SpeedRules,
    pub rating_rules: RatingRules,
    pub level: LevelRules,
    pub fatigue: FatigueRules,
    /// 事故规则（总开关 / 全局倍率 / 概率上限）。
    #[serde(default)]
    pub accident: AccidentRules,
    /// 情绪规则（情绪低 → 变懒）。
    #[serde(default)]
    pub mood: MoodRules,
    /// 步行规则（商场单 / 高层送货的时间成本）。
    #[serde(default)]
    pub walk: WalkRules,

    // --- 控制端开关 ---
    /// 平台抽水比例 0.0 ~ 0.30，默认 0。
    pub commission_rate: f32,
    /// 只抽配送费，不抽雨补。
    pub commission_on_rain_bonus: bool,
    /// 歧视性抽水：评分高 / 单量大的骑手抽水上浮。
    pub discriminative_commission: bool,
    /// 抢单窗口秒数 1~10。
    pub claim_window_secs: u32,
    /// 派单范围半径（km）。
    ///
    /// 圆心的骑手**有效当前定位**，判的是「骑手 → 订单**取货点**」的球面距离。
    /// 详见 `sim::dispatch`——那里同时定义了定位有效期与边界口径（等于半径算通过）。
    ///
    /// 默认 90、合法区间 1~500：默认值给一个"同城 + 邻近区县"的量级，
    /// 上下限的理由见 `dispatch::RADIUS_MIN_KM` / `RADIUS_MAX_KM`。
    ///
    /// `#[serde(default)]` 保证老配置（没有这个字段）读进来时自动落到 90，
    /// 不会因为缺字段反序列化失败。
    #[serde(default = "default_dispatch_radius_km")]
    pub dispatch_radius_km: f64,
    /// 推荐单概率（百分比，0~100）。
    ///
    /// 控制的是**订单被标上「推荐」的概率**，不是被接单/被派送的概率——
    /// 后者由派单算法决定，跟这个数无关。判定用的是
    /// `hash(seed, rider_id, order_id) % 100 < 概率`，同一骑手同一订单结果恒定，
    /// 刷新/重连/重启都不会让标签闪。
    #[serde(default = "default_recommend_percent")]
    pub recommend_percent: u32,
    pub allow_speeding: bool,
    pub allow_unrestricted: bool,
    /// 允许真人骑手做**违规操作**：路还没走完就点到店 / 到顾客（等价于
    /// 「人没到，手先到」）。关着的时候服务端直接拒绝，见
    /// `scheduler::arrive_shop` / `arrive_customer`。
    ///
    /// AI 骑手不受这条约束——它们按 tick 自己走，本来就没有抢先点的机会。
    #[serde(default)]
    pub allow_rider_violation: bool,
    /// 取送半径（米）：真人骑手只要真实坐标已经落在这个圈里，就算
    /// 「到了」——不必等这一段的时间进度走满。0 = 关闭半径判定。
    ///
    /// 与 [`Self::allow_rider_violation`] 是**并列**的两条放行理由，不是替代：
    /// 关着违规开关时，站到门口照样能点；开着时无论在哪都能点。
    #[serde(default = "default_arrive_radius_m")]
    pub arrive_radius_m: u32,
    pub backorder_enabled: bool,
    pub max_orders_per_day: u32,
    /// 每天能拒掉多少张**系统指派**的单，0 = 不限。
    ///
    /// 这是压住「指派单被随手拒掉」的唯一闸门——指派单本身不罚款
    /// （见 `scheduler::reject_order`），额度用完就只能照单全收。
    /// 照众包平台的现实做法：拒单是权利，但权利有配额。
    pub max_rejects_per_day: u32,
    pub ai_batch_enabled: bool,
    pub llm_brain_enabled: bool,
    /// LLM 策略覆盖（门槛档位 / 背单上限 / 速度档）的有效期，单位模拟分钟。
    ///
    /// 0 = 不过期。跨天一律清除：策略是临时覆盖，不能变成永久属性。
    #[serde(default = "default_llm_policy_ttl_min")]
    pub llm_policy_ttl_min: u32,
    /// 「拒这一单」的记录有效期（模拟分钟）。0 = 只在当前 tick 有效。
    #[serde(default = "default_llm_refusal_ttl_min")]
    pub llm_refusal_ttl_min: u32,
    /// 「观望」的骑手级窗口（模拟分钟）。0 = 只在当前 tick 有效。
    #[serde(default = "default_llm_hold_ttl_min")]
    pub llm_hold_ttl_min: u32,
    /// Director 天气覆盖的有效期（模拟分钟）。0 = 不过期。
    #[serde(default = "default_director_weather_ttl_min")]
    pub director_weather_ttl_min: u32,
    /// 控制端「显示期望值」开关，只影响下发字段。
    pub show_expected_value: bool,
    /// 模拟骑手数量 0~8。
    pub sim_rider_count: u32,
    /// 每多少模拟分钟尝试生成一批新单。
    pub order_spawn_interval_min: u32,
    /// 时间倍率：0=暂停，1/10/60。
    pub speed_multiplier: u32,
    /// 实时档：1 模拟分钟 = 真实 60 秒，世界按真实钟表速度推进。
    ///
    /// 打开时 [`SimConfig::tick_interval_ms`] 返回 [`REALTIME_TICK_MS`]，
    /// `speed_multiplier` 只保留「是否暂停」的语义（0 = 暂停）。
    pub realtime: bool,
    /// 下发坐标的坐标系。内部世界状态永远是 GCJ-02，这里只决定出口转换。
    #[serde(default)]
    pub coord_system: CoordSystem,
}

/// 取送半径的默认值：100 米。老存档缺这个字段时也按它算。
fn default_arrive_radius_m() -> u32 {
    100
}

/// LLM 策略覆盖默认有效期：90 分钟（约一个半小时）。
fn default_llm_policy_ttl_min() -> u32 {
    90
}

/// 拒单记录默认有效期：30 分钟。
fn default_llm_refusal_ttl_min() -> u32 {
    30
}

/// 观望窗口默认 10 分钟：够表达「等更值的单」，又不会把人晾一小时。
fn default_llm_hold_ttl_min() -> u32 {
    10
}

/// Director 天气覆盖默认 60 分钟，与它的轮询节奏对齐。
fn default_director_weather_ttl_min() -> u32 {
    60
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            seed: 20261006,
            fee: FeeRules {
                base_fee_cents: 350,
                distance_tiers: vec![(1500, 100), (3000, 200), (5000, 350), (8000, 350)],
                per_km_over_cents: 60,
                surge_windows: vec![
                    (7 * 60, 9 * 60, 1.10),
                    (11 * 60, 13 * 60, 1.15),
                    (17 * 60, 20 * 60, 1.25),
                ],
                level_bonus_per_level_cents: 20,
            },
            fine: FineRules {
                wait_timeout_cents: 200,
                deliver_timeout_cents: 300,
                camera_cents: 500,
                complaint_cents: 200,
                cancel_cents: 500,
                backorder_collapse_cents: 200,
                insurance_per_day_cents: 200,
                accident_medical_min_cents: 2000,
                accident_medical_max_cents: 20000,
                camera_detention_min: 10,
            },
            backorder: BackOrderRules {
                max_held: 3,
                detour_threshold_m: 800,
                bonus_min_cents: 50,
                bonus_max_cents: 120,
                late_buffer_min: 5,
                merge_wait_max_min: 3,
            },
            merge: MergeRules::default(),
            speed: SpeedRules {
                complaint_speed_kmh: [30.0, 45.0, 999.0],
                hourly_coefficient: [1.00, 1.35, 2.20],
                accident_rate: [0.005, 0.04, 0.22],
                camera_rate: [0.0, 0.008, 0.04],
                rating_delta: [0.0, -0.01, -0.05],
                detain_min: 10,
                base_speed_kmh: 30.0,
            },
            rating_rules: RatingRules {
                initial: 4.8,
                min: 4.0,
                max: 5.0,
                on_time: 0.01,
                late: -0.02,
                accident: -0.05,
                good_review: 0.02,
                bad_review_max: -0.05,
                tier_normal: 4.6,
                tier_premium: 4.8,
                tier_express: 4.9,
            },
            level: LevelRules {
                thresholds: vec![0, 50, 150, 400, 1000, 2500],
                coefficients: vec![1.00, 1.04, 1.08, 1.12, 1.16, 1.20],
            },
            fatigue: FatigueRules {
                per_online_hour: 0.08,
                per_order: 0.01,
                per_rest_10min: 0.15,
                max: 1.0,
                speed_penalty_max: 0.25,
                accident_weight: 0.02,
            },
            accident: AccidentRules::default(),
            mood: MoodRules::default(),
            walk: WalkRules::default(),
            commission_rate: 0.0,
            commission_on_rain_bonus: false,
            discriminative_commission: false,
            // 3 秒 × 倍率 = 窗口 tick 数；实时档下退化为 1 tick（= 1 模拟分钟）。
            claim_window_secs: 3,
            // 90 km：同城 + 邻近区县的量级。数值与 `dispatch::RADIUS_DEFAULT_KM`
            // 同源，改一处即可，别在这里写第二份字面量。
            dispatch_radius_km: crate::sim::dispatch::RADIUS_DEFAULT_KM,
            recommend_percent: crate::sim::dispatch::RECOMMEND_PERCENT_DEFAULT,
            allow_speeding: true,
            allow_unrestricted: false,
            // 默认禁止：不到位置就点「到店 / 送达」是违规操作，得由控制端显式放行。
            allow_rider_violation: false,
            // 100 米内算「到了」，覆盖 GPS 漂移与「车停马路对面」的现实误差。
            arrive_radius_m: default_arrive_radius_m(),
            backorder_enabled: true,
            max_orders_per_day: 0,
            max_rejects_per_day: 6,
            ai_batch_enabled: true,
            // 玩法设计 §7.3：LLM 战术大脑**默认关**，由控制端 / 命令行显式打开。
            llm_brain_enabled: false,
            llm_policy_ttl_min: default_llm_policy_ttl_min(),
            llm_refusal_ttl_min: default_llm_refusal_ttl_min(),
            llm_hold_ttl_min: default_llm_hold_ttl_min(),
            director_weather_ttl_min: default_director_weather_ttl_min(),
            show_expected_value: false,
            sim_rider_count: 3,
            order_spawn_interval_min: 3,
            speed_multiplier: 1,
            // 真人骑手只能跑在实时档，而手机端是主玩法——默认就给实时。
            // 想快进看 AI 演练，在烽火台切倍率档即可（会请真人下线）。
            realtime: true,
            coord_system: CoordSystem::Gcj02,
        }
    }
}

impl SimConfig {
    /// 该时段的高峰系数。
    pub fn surge_multiplier(&self, sim_minute: u32) -> f32 {
        for (from, to, k) in &self.fee.surge_windows {
            if sim_minute >= *from && sim_minute < *to {
                return *k;
            }
        }
        1.0
    }

    /// 是否处于高峰（出餐变慢、车流量变大）。
    /// 该速度档位当前是否被系统开关允许。
    ///
    /// LLM 只能在这个白名单内调档（设计红线 2：输出必须是有限集合）。
    pub fn speed_mode_allowed(&self, mode: crate::sim::rider::SpeedMode) -> bool {
        use crate::sim::rider::SpeedMode;
        match mode {
            SpeedMode::Compliant => true,
            SpeedMode::Tolerant => self.allow_speeding,
            SpeedMode::Unrestricted => self.allow_unrestricted,
        }
    }

    pub fn is_peak(&self, sim_minute: u32) -> bool {
        self.surge_multiplier(sim_minute) > 1.0
    }

    /// 抢单窗口换算成 tick 数。1 tick = 1 模拟分钟，
    /// 倍率越高，同样的现实秒数对应的 tick 越多。
    ///
    /// 实时档下一个 tick 就是 60 真实秒，比任何窗口都长，
    /// 于是窗口退化成「1 tick」，抢单变成一颗 tick 内定胜负。
    pub fn claim_window_ticks(&self) -> u32 {
        if self.realtime {
            return 1;
        }
        let m = self.speed_multiplier.max(1);
        (self.claim_window_secs * m).max(1)
    }

    /// 现实上一个 tick 的毫秒数。
    ///
    /// - 暂停（倍率 0）：退回 1 秒节拍，`App::on_tick` 会直接跳过推进；
    /// - 实时档：1 模拟分钟 = 真实 60 秒；
    /// - 其余：倍率 1 时 = 1 秒 = 1 模拟分钟。
    pub fn tick_interval_ms(&self) -> u64 {
        if self.speed_multiplier == 0 {
            return 1000;
        }
        if self.realtime {
            return REALTIME_TICK_MS;
        }
        1000 / self.speed_multiplier as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::weather::Weather;

    #[test]
    fn surge_windows() {
        let c = SimConfig::default();
        assert_eq!(c.surge_multiplier(8 * 60), 1.10);
        assert_eq!(c.surge_multiplier(12 * 60), 1.15);
        assert_eq!(c.surge_multiplier(18 * 60), 1.25);
        assert_eq!(c.surge_multiplier(3 * 60), 1.00);
    }

    #[test]
    fn default_balance_matches_design() {
        let c = SimConfig::default();
        assert_eq!(c.fee.base_fee_cents, 350);
        assert_eq!(c.commission_rate, 0.0);
        assert_eq!(c.backorder.max_held, 3);
        assert_eq!(c.claim_window_secs, 3);
        assert_eq!(c.fine.insurance_per_day_cents, 200);
        assert_eq!(Weather::HeavyRain.rain_bonus_cents(), 250);
    }

    #[test]
    fn tick_interval_scales() {
        let mut c = SimConfig::default();
        // 这条测的是倍率档的换算，先退出实时档。
        c.realtime = false;
        assert_eq!(c.tick_interval_ms(), 1000);
        assert_eq!(c.claim_window_ticks(), 3);
        c.speed_multiplier = 60;
        assert_eq!(c.tick_interval_ms(), 16);
        assert_eq!(c.claim_window_ticks(), 180);
        // 暂停时 tick 间隔退回 1000ms（等价 1x），窗口仍按 3 秒算。
        c.speed_multiplier = 0;
        assert_eq!(c.tick_interval_ms(), 1000);
        assert_eq!(c.claim_window_ticks(), 3);
    }

    #[test]
    fn realtime_mode_runs_one_sim_minute_per_real_minute() {
        let mut c = SimConfig::default();
        assert!(c.realtime, "默认真实时间是开的：真人骑手要贴着现实时间跑");
        assert_eq!(c.tick_interval_ms(), REALTIME_TICK_MS);

        c.realtime = true;
        assert_eq!(c.tick_interval_ms(), REALTIME_TICK_MS);
        assert_eq!(c.tick_interval_ms(), 60_000);

        // 窗口长不过一个 tick，实时档退化成 1。
        assert_eq!(c.claim_window_ticks(), 1);
        c.claim_window_secs = 10;
        assert_eq!(c.claim_window_ticks(), 1);

        // 倍率不再影响节拍。
        c.speed_multiplier = 60;
        assert_eq!(c.tick_interval_ms(), REALTIME_TICK_MS);

        // 暂停优先于实时：世界冻住，节拍退回 1 秒等恢复。
        c.speed_multiplier = 0;
        assert_eq!(c.tick_interval_ms(), 1000);
    }
}
