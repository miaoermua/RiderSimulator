//! 全局状态。tick 循环唯一的可变对象。

use std::collections::HashMap;

use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::Serialize;

use super::{
    MINUTES_PER_DAY, OrderId, RiderId, activity,
    ai::{self, LlmPlan},
    config::{
        MAX_REDISPATCH_ATTEMPTS, REDISPATCH_PROMISE_FLOOR_MIN, REDISPATCH_SURCHARGE_STEP_CENTS,
        SimConfig,
    },
    economy,
    event::{DayLedger, RiderBrief},
    geo::{CoordSystem, LatLng, bearing_deg, haversine_m},
    merchant::Merchant,
    notice,
    order::{ExtraTask, FeeBreakdown, Order, OrderStatus, OrderTier},
    rider::{Phase, Rider, RiderKind, SpeedMode},
    task::TaskTemplate,
    weather::{Weather, WeatherEvent},
};

/// 路网绕行系数：直线 → 实际骑行距离。
pub const ROAD_FACTOR: f64 = 1.25;

/// 判定「商家密集」的半径（米）。两家店在这个距离内就算邻居。
///
/// 用来把新订单往店多的那片撒——同批单之间够近，才可能合成一条腿一起送。
pub const MERCHANT_CLUSTER_M: f64 = 800.0;

/// 运行统计。
#[derive(Debug, Clone, Default, Serialize)]
pub struct SimStats {
    pub orders_created: u64,
    /// 彻底流单的订单数。**当前任何路径都不再产生**，恒为 0——
    /// 重投用尽会走强制指派（见 `orders_rescued`），留着只是为了协议/回放兼容。
    pub orders_expired: u64,
    /// 重投用尽后被**强制指派给最近骑手**救回来的订单数。
    pub orders_rescued: u64,
    pub orders_delivered: u64,
    pub orders_late: u64,
    pub claims: u64,
    /// 控制端改派次数（只数成功转手的，失败的重试不算）。
    pub reassigns: u64,
    pub accidents: u32,
    pub camera_fines: u32,
    /// LLM 战术大脑调用次数（一天最多 24 次）。
    pub llm_calls: u32,
    /// LLM 失败 / 被校验丢弃后回退算法的次数。
    pub llm_fallbacks: u32,
}

/// 重投的归宿。**没有「流单」这一种**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redispatch {
    /// 加价重回大厅，等下一轮抢单。
    ToHall,
    /// 重投次数用尽：订单**仍留在大厅**，等调用方把它强制指派给最近的骑手
    /// （[`crate::sim::scheduler::rescue_nearest`]）。它绝不会被判流单。
    NeedsRescue,
}

/// 临时插单的入参。坐标**已经**是内部坐标系（GCJ-02，入口处换算过）。
///
/// 与控制台「临时创建订单」面板一一对应：商家名 / 两端坐标 / 金额，
/// 其余字段（距离、承诺时限、抽水）由服务端按与自然生成单同一套规则补齐。
#[derive(Debug, Clone)]
pub struct CustomOrderSpec {
    pub merchant_name: String,
    pub merchant_lat: f64,
    pub merchant_lng: f64,
    pub customer_name: Option<String>,
    pub customer_address: Option<String>,
    pub customer_lat: f64,
    pub customer_lng: f64,
    /// 这张单的报价（分）。`Some` 时**直接**当最终报价，`None` 才按计价规则算。
    pub fee_cents: Option<i64>,
}

/// 临时单报价的下界（1 元）：手滑输 0 会算出一张白跑的单。
pub const MIN_CUSTOM_FEE_CENTS: i64 = 100;
/// 临时单报价的上界（2000 元）：防止一个天文数字把整局流水玩坏。
pub const MAX_CUSTOM_FEE_CENTS: i64 = 200_000;

/// 计价规则的只读视图（「单价系统」的对外口径）。
///
/// 与 [`crate::sim::config::FeeRules`] 的区别：这里只暴露控制台要展示与要调的量，
/// 阶梯表 `distance_tiers` 原样下发供前端描述「几公里内加多少钱」，
/// 但只能改代码——它是玩法设计的骨架，不适合在运行时随手拖。
#[derive(Debug, Clone, Serialize)]
pub struct FeeView {
    /// 起步价（分）。含 `distance_tiers[0].0` 以内的里程。
    pub base_fee_cents: i64,
    /// 里程阶梯：(距离下界米, 总附加费分)。取不超过本单距离的最大门槛。
    pub distance_tiers: Vec<(u32, i64)>,
    /// 超出最后一档后每公里单价（分/km）。
    pub per_km_over_cents: i64,
    /// 等级加成（分/级）。
    pub level_bonus_per_level_cents: i64,
    /// 当前时段的高峰系数。
    pub surge_multiplier: f32,
    /// 订单重投上限与每次加价，前端用来解释「为什么这单变贵了」。
    pub max_redispatch_attempts: u32,
    pub redispatch_surcharge_step_cents: i64,
}

/// 控制端可调项的只读视图。
#[derive(Debug, Clone, Serialize)]
pub struct ConfigView {
    pub seed: u64,
    pub speed_multiplier: u32,
    pub paused: bool,
    /// 实时档：1 模拟分钟 = 真实 60 秒。
    pub realtime: bool,
    pub tick_interval_ms: u64,
    pub commission_rate: f32,
    pub discriminative_commission: bool,
    pub claim_window_secs: u32,
    pub allow_speeding: bool,
    pub allow_unrestricted: bool,
    /// 允许真人骑手未到位置就点到店 / 送达。
    pub allow_rider_violation: bool,
    pub backorder_enabled: bool,
    pub backorder_max: u32,
    pub max_orders_per_day: u32,
    /// 每天能拒掉多少张指派单，`0` = 不限。
    pub max_rejects_per_day: u32,
    pub ai_batch_enabled: bool,
    pub llm_brain_enabled: bool,
    pub show_expected_value: bool,
    pub sim_rider_count: u32,
    pub is_peak: bool,
    pub surge_multiplier: f32,
    /// 当前允许的档位（UI 灰显用）。
    pub allowed_speed_modes: Vec<SpeedMode>,
    /// 计价规则（控制台「单价系统」面板据此渲染）。
    pub fee: FeeView,
    pub insurance_per_day_cents: i64,
    pub backorder_late_buffer_min: u32,
    /// 算法一批里给**一个骑手**最多派几单。
    pub merge_max_per_rider: u32,
    /// 单个商家同时存在的未完成单量上限，`0` = 不限。
    pub merge_merchant_cap: u32,
    /// 凑单窗口（分钟）：在店等餐的骑手愿意为同店新单多等多久。
    pub merge_wait_min: u32,
    /// 事故总开关。
    pub accident_enabled: bool,
    /// 事故率全局倍率，`1.0` = 设计值（0.5% / 4% / 22%）。
    pub accident_rate_multiplier: f32,
    /// 情绪（懒惰）总开关。
    pub mood_enabled: bool,
    /// 情绪见底时接单门槛最多 ×几（懒惰的力度）。
    pub mood_lazy_threshold_max: f32,
    /// 下发坐标的坐标系（方案 A：内部始终 GCJ-02，这里只决定出口转换）。
    pub coord_system: CoordSystem,
}

/// 商户精简信息。
#[derive(Debug, Clone, Serialize)]
pub struct MerchantBrief {
    pub id: u32,
    pub name: String,
    pub address: String,
    pub lat: f64,
    pub lng: f64,
    pub rating: f32,
    pub signature: String,
    pub avg_prep_min: u32,
}

/// 全量状态快照（公开部分）。连接建立 / 断线重连时下发。
#[derive(Debug, Clone, Serialize)]
pub struct PublicSnapshot {
    pub protocol_version: &'static str,
    pub day: u32,
    pub sim_minute: u32,
    pub elapsed_min: u64,
    pub weather: Weather,
    pub weather_label: &'static str,
    pub weather_speed_factor: f32,
    pub rain_bonus_cents: i64,
    pub config: ConfigView,
    pub riders: Vec<RiderBrief>,
    pub hall: Vec<Order>,
    pub active_orders: Vec<Order>,
    pub merchants: Vec<MerchantBrief>,
    pub stats: SimStats,
    pub last_day_ledgers: Vec<DayLedger>,
    /// 抢单窗口（毫秒）。
    pub claim_window_ms: u64,
    /// 平台活动定义（含已下架的，手机端自己过滤）。进度在骑手身上。
    pub activities: Vec<activity::PlatformActivity>,
    /// 首页通知（烽火台写的单条公告）。`None` = 没有通知。
    pub notice: Option<notice::Notice>,
}

/// 全局状态。
pub struct World {
    pub cfg: SimConfig,
    pub seed: u64,
    pub rng: ChaCha8Rng,
    /// 第几天，从 1 开始。
    pub day: u32,
    /// 当日 0:00 起算的模拟分钟 0..1440。
    pub sim_minute: u32,
    /// 自开局起算的单调分钟。**所有截止时间都用这个**（跨日不会回绕）。
    pub elapsed_min: u64,
    pub riders: Vec<Rider>,
    pub orders: Vec<Order>,
    pub merchants: Vec<Merchant>,
    pub tasks: Vec<TaskTemplate>,
    pub weather: Weather,
    pub weather_script: Vec<WeatherEvent>,
    pub script_cursor: usize,
    pub next_order_id: OrderId,
    pub next_rider_id: RiderId,
    pub stats: SimStats,
    /// 本 tick 待消费的 LLM 计划（由 app 层异步拿到后塞进来）。
    pub llm_plan: Option<LlmPlan>,
    pub last_day_ledgers: Vec<DayLedger>,
    /// 平台活动定义。进度不在这里——见 [`activity`]。
    pub activities: Vec<activity::PlatformActivity>,
    /// 下一个活动 id，只增不减。
    pub next_activity_id: u32,
    /// 首页通知。单条，改了就覆盖——见 [`notice`]。
    pub notice: Option<notice::Notice>,
}

impl World {
    pub fn new(
        cfg: SimConfig,
        merchants: Vec<Merchant>,
        tasks: Vec<TaskTemplate>,
        weather_script: Vec<WeatherEvent>,
    ) -> Self {
        let seed = cfg.seed;
        let mut w = Self {
            cfg,
            seed,
            rng: ChaCha8Rng::seed_from_u64(seed),
            day: 1,
            sim_minute: 6 * 60,
            elapsed_min: 0,
            riders: Vec::new(),
            orders: Vec::new(),
            merchants,
            tasks,
            weather: Weather::Clear,
            weather_script,
            script_cursor: 0,
            next_order_id: 1,
            next_rider_id: 1,
            stats: SimStats::default(),
            llm_plan: None,
            last_day_ledgers: Vec::new(),
            activities: Vec::new(),
            next_activity_id: 1,
            notice: None,
        };
        for _ in 0..w.cfg.sim_rider_count {
            w.spawn_sim_rider();
        }
        w
    }

    /// 测试用：合成一座城市的商户。
    pub fn new_for_test(cfg: SimConfig, seed: u64) -> Self {
        let mut cfg = cfg;
        cfg.seed = seed;
        let merchants = synth_merchants(seed, 40);
        let tasks = vec![synth_task()];
        let mut w = Self::new(cfg, merchants, tasks, Vec::new());
        for r in &mut w.riders {
            r.online = true;
            r.strategy.speed_mode = SpeedMode::Tolerant;
        }
        w
    }

    // ---------- 访问 ----------

    pub fn rider(&self, id: RiderId) -> Option<&Rider> {
        self.riders.iter().find(|r| r.id == id)
    }
    pub fn rider_mut(&mut self, id: RiderId) -> Option<&mut Rider> {
        self.riders.iter_mut().find(|r| r.id == id)
    }
    pub fn order(&self, id: OrderId) -> Option<&Order> {
        self.orders.iter().find(|o| o.id == id)
    }
    pub fn order_mut(&mut self, id: OrderId) -> Option<&mut Order> {
        self.orders.iter_mut().find(|o| o.id == id)
    }
    pub fn merchant(&self, id: u32) -> Option<&Merchant> {
        self.merchants.iter().find(|m| m.id == id)
    }

    /// 商家平均出餐分钟（高峰系数在 scheduler::attach 时叠加）。
    pub fn avg_prep_minutes(&self, merchant_id: u32) -> u32 {
        self.merchant(merchant_id)
            .map(|m| m.avg_prep_min)
            .unwrap_or(10)
    }

    /// 骑手当前这一段要去哪。
    ///
    /// 优先读**路线当前腿**——那才是人真正朝着的点。从前只看 `held.first()`，
    /// 而 `held` 的顺序是「接单先后」，跟脚步没关系：多单时顺路度评分一直在
    /// 拿一个错的目标算方向。
    pub fn rider_current_target(&self, rider: &Rider) -> Option<LatLng> {
        if let Some(pos) = rider.current_leg_pos() {
            return Some(pos);
        }
        // 没有路线时（老存档、手工构造的手持单）退回旧口径。
        let first = rider.held.first()?;
        let order = self.order(first.order_id)?;
        Some(match first.phase {
            Phase::ToShop | Phase::WaitingFood => order.merchant_pos(),
            _ => order.customer_pos(),
        })
    }

    /// 骑手当前朝向（顺路提示 / LLM 上下文用）。
    pub fn rider_heading(&self, rider: &Rider) -> Option<f32> {
        Some(bearing_deg(rider.pos(), self.rider_current_target(rider)?))
    }

    pub fn pending_orders(&self) -> Vec<OrderId> {
        self.orders
            .iter()
            .filter(|o| o.status == OrderStatus::Pending)
            .map(|o| o.id)
            .collect()
    }

    pub fn idle_riders(&self) -> Vec<RiderId> {
        self.riders
            .iter()
            .filter(|r| r.is_idle(self.elapsed_min, &self.cfg))
            .map(|r| r.id)
            .collect()
    }

    pub fn orders_of(&self, rider_id: RiderId) -> Vec<Order> {
        self.orders
            .iter()
            .filter(|o| o.assigned_to == Some(rider_id) && o.status.is_active())
            .cloned()
            .collect()
    }

    // ---------- 骑手 ----------

    pub fn add_rider(&mut self, kind: RiderKind, name: impl Into<String>, home: LatLng) -> RiderId {
        let id = self.next_rider_id;
        self.next_rider_id += 1;
        let cfg = self.cfg.clone();
        let mut rider = Rider::new(id, kind, name, home, &cfg);
        if kind == RiderKind::Simulated {
            rider.online = true;
            rider.strategy.accept_threshold_cents_hour = self.rng.random_range(2000..3500);
            rider.strategy.speed_mode = match self.rng.random_range(0..100u32) {
                0..=59 => SpeedMode::Compliant,
                60..=94 => SpeedMode::Tolerant,
                _ => SpeedMode::Unrestricted,
            };
            rider.strategy.skill = self.rng.random_range(0.3f32..0.95);
            rider.speed_mode = rider.strategy.speed_mode;
        }
        self.riders.push(rider);
        id
    }

    pub fn spawn_sim_rider(&mut self) -> RiderId {
        let id = self.next_rider_id;
        let home = self.random_home();
        let name = format!("AI-{}", (b'A' + (id.saturating_sub(1) % 26) as u8) as char);
        self.add_rider(RiderKind::Simulated, name, home)
    }

    /// 真人骑手在随机商圈附近出生。
    pub fn spawn_human_rider(&mut self, name: impl Into<String>) -> RiderId {
        let home = self.random_home();
        self.add_rider(RiderKind::Human, name, home)
    }

    /// 找一具**已经存在的**同名真人骑手。
    ///
    /// 账号登录靠它复用身体：账号是长期身份，骑手只是这一局里的一具身体，
    /// 服务端一重启身体就没了、账号还在。所以「同一个人重连」应当是找到旧身体，
    /// 而不是再长出一具同名的来。模拟骑手不算——它们不登录。
    pub fn human_rider_named(&self, name: &str) -> Option<RiderId> {
        self.riders
            .iter()
            .find(|r| r.kind == RiderKind::Human && r.name == name)
            .map(|r| r.id)
    }

    /// 骑手名的字符上限。够写「张三」「AI-小队长」，再长卡片就换行。
    pub const MAX_RIDER_NAME_CHARS: usize = 12;

    /// 清洗一个骑手名：去首尾空白、剔除控制字符、截断到上限。
    ///
    /// 返回 `None` = 这名字不能用（洗完全是空的）。
    pub fn sanitize_rider_name(raw: &str) -> Option<String> {
        let cleaned: String = raw
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .take(Self::MAX_RIDER_NAME_CHARS)
            .collect();
        match cleaned.trim() {
            "" => None,
            s => Some(s.to_string()),
        }
    }

    /// 给一个骑手改名。名字非法 / 骑手不存在 / 名字没变化都返回 `false`。
    pub fn rename_rider(&mut self, id: RiderId, raw: &str) -> bool {
        let Some(name) = Self::sanitize_rider_name(raw) else {
            return false;
        };
        match self.riders.iter_mut().find(|r| r.id == id) {
            Some(r) if r.name != name => {
                r.name = name;
                true
            }
            _ => false,
        }
    }

    /// 按名单批量重命名**模拟**骑手（顺序对应 id 从小到大）。
    ///
    /// 名单比骑手长时多余的名字忽略；比骑手短时后面的骑手保留原名。
    /// 返回真正改动的骑手 id。
    pub fn apply_rider_names(&mut self, names: &[String]) -> Vec<RiderId> {
        let ids = self.sim_rider_ids();
        let mut changed = Vec::new();
        for (id, raw) in ids.iter().zip(names.iter()) {
            if self.rename_rider(*id, raw) {
                changed.push(*id);
            }
        }
        changed
    }

    /// 模拟骑手的 id（按 id 升序）。名单与展示都依赖这个顺序。
    pub fn sim_rider_ids(&self) -> Vec<RiderId> {
        let mut ids: Vec<RiderId> = self
            .riders
            .iter()
            .filter(|r| r.kind == RiderKind::Simulated)
            .map(|r| r.id)
            .collect();
        ids.sort_unstable();
        ids
    }

    /// 模拟骑手当前的名字（按 id 升序），用于回显与落库。
    pub fn sim_rider_names(&self) -> Vec<String> {
        self.sim_rider_ids()
            .into_iter()
            .filter_map(|id| self.rider(id).map(|r| r.name.clone()))
            .collect()
    }

    /// 控制端调 AI 骑手数量（0~8）。返回状态有变化的骑手。
    ///
    /// 多出来的空闲 AI 下线；手上有单的先跑完，不强拉下线。
    /// 不够就先把离线的 AI 拉回来，再新建。
    pub fn set_sim_rider_count(&mut self, n: u32) -> Vec<RiderId> {
        let n = n.min(8);
        self.cfg.sim_rider_count = n;
        let mut changed = Vec::new();
        let mut online = 0u32;
        for r in self
            .riders
            .iter_mut()
            .filter(|r| r.kind == RiderKind::Simulated)
        {
            if r.online && !r.held.is_empty() {
                online += 1;
            } else if online < n {
                if !r.online {
                    r.online = true;
                    changed.push(r.id);
                }
                online += 1;
            } else if r.online {
                r.online = false;
                changed.push(r.id);
            }
        }
        while online < n {
            changed.push(self.spawn_sim_rider());
            online += 1;
        }
        changed
    }

    /// 关掉超速 / 解限速开关后，把不再允许的档位降回合规。返回被降档的骑手。
    pub fn enforce_speed_policy(&mut self) -> Vec<RiderId> {
        let allowed = |m: SpeedMode, cfg: &SimConfig| cfg.speed_mode_allowed(m);
        let cfg = &self.cfg;
        let mut changed = Vec::new();
        for r in &mut self.riders {
            if !allowed(r.speed_mode, cfg) || !allowed(r.strategy.speed_mode, cfg) {
                r.speed_mode = SpeedMode::Compliant;
                r.strategy.speed_mode = SpeedMode::Compliant;
                changed.push(r.id);
            }
            // LLM 下过档位覆盖的，开关关掉后同样要撤回——否则它会绕过 enforce
            if let Some(m) = r.policy_override.mode_override
                && !allowed(m, cfg)
            {
                r.policy_override.mode_override = None;
                changed.push(r.id);
            }
        }
        changed
    }

    fn random_home(&mut self) -> LatLng {
        let spots: Vec<LatLng> = self.merchants.iter().filter_map(|m| m.coords()).collect();
        if spots.is_empty() {
            return LatLng::new(30.5077, 114.4154);
        }
        let idx = self.rng.random_range(0..spots.len());
        spots[idx]
    }

    // ---------- 天气 ----------

    /// 按剧本推进天气。返回变化后的天气。
    pub fn advance_weather_script(&mut self) -> Option<Weather> {
        let mut changed = None;
        while self.script_cursor < self.weather_script.len() {
            let ev_at = self.weather_script[self.script_cursor].at;
            let to = self.weather_script[self.script_cursor].to;
            if self.sim_minute >= ev_at {
                self.script_cursor += 1;
                if to != self.weather {
                    self.weather = to;
                    changed = Some(to);
                }
            } else {
                break;
            }
        }
        changed
    }

    pub fn set_weather(&mut self, w: Weather) -> bool {
        if self.weather == w {
            return false;
        }
        self.weather = w;
        true
    }

    // ---------- 订单 ----------

    /// 生成一批新订单，返回新订单 id。
    pub fn generate_order_batch(&mut self, n: u32) -> Vec<OrderId> {
        (0..n).filter_map(|_| self.generate_order()).collect()
    }

    /// 生成 1 单。商户按评分加权抽取（评分高的店单量多）。
    pub fn generate_order(&mut self) -> Option<OrderId> {
        // 单店未完成订单上限（0 = 不限）。没有它，热门店能把整条街的单都发出
        // 大厅，一堆同店单谁也没法合并，只能一个个单独跑。
        let cap = self.cfg.merge.max_active_orders_per_merchant;
        let mut active: HashMap<u32, u32> = HashMap::new();
        if cap > 0 {
            for o in &self.orders {
                if o.status.is_active() {
                    *active.entry(o.merchant_id).or_insert(0) += 1;
                }
            }
        }

        let candidates: Vec<(u32, String, LatLng, f32, u32, u8)> = self
            .merchants
            .iter()
            .filter(|m| cap == 0 || active.get(&m.id).copied().unwrap_or(0) < cap)
            .filter_map(|m| {
                m.coords().map(|c| {
                    (
                        m.id,
                        m.name.clone(),
                        c,
                        m.rating,
                        m.avg_prep_min,
                        m.walk_difficulty.min(2),
                    )
                })
            })
            .collect();
        if candidates.is_empty() {
            return None;
        }

        // 评分加权：rating^6，把 4.5 和 4.9 的差距拉到 1.7 倍。
        // 再乘一层「商家密集度」：店扎堆的地方单量更高，同批单才可能落在
        // 几百米内互相合并（见 [`MERCHANT_CLUSTER_M`]）。
        let weights: Vec<f64> = candidates
            .iter()
            .map(|c| {
                let near = candidates
                    .iter()
                    .filter(|o| haversine_m(c.2, o.2) <= MERCHANT_CLUSTER_M)
                    .count();
                let density = (1.0 + 0.5 * near.saturating_sub(1) as f64).min(3.0);
                (c.3 as f64).powi(6) * density
            })
            .collect();
        let total: f64 = weights.iter().sum();
        let mut pick = self.rng.random_range(0.0..total);
        let mut idx = 0usize;
        for (i, w) in weights.iter().enumerate() {
            pick -= w;
            if pick <= 0.0 {
                idx = i;
                break;
            }
        }
        let (merchant_id, merchant_name, merchant_pos, _rating, _prep, shop_walk) =
            candidates[idx].clone();

        // 顾客端步行难度：大多送到楼下，少数要上楼 / 进深巷。
        // 属于物理量，随世界种子确定，运行期不变。
        let cust_walk = match self.rng.random_range(0..100u32) {
            0..=59 => 0u8,
            60..=89 => 1,
            _ => 2,
        };

        // 顾客位置：0.5~4.5km，随机方向
        let dist_m = self.rng.random_range(500.0f64..4500.0);
        let bearing = self.rng.random_range(0.0f64..360.0).to_radians();
        let dlat = dist_m * bearing.cos() / 111_320.0;
        let cos_lat = merchant_pos.lat.to_radians().cos().max(0.1);
        let dlng = dist_m * bearing.sin() / (111_320.0 * cos_lat);
        let customer = LatLng::new(merchant_pos.lat + dlat, merchant_pos.lng + dlng);
        let road_distance_m = (haversine_m(merchant_pos, customer) * ROAD_FACTOR)
            .round()
            .max(1.0) as u32;

        let tier = match self.rng.random_range(0..100u32) {
            0..=79 => OrderTier::Normal,
            80..=94 => OrderTier::Premium,
            _ => OrderTier::Express,
        };

        // 承诺时间与「骑手真正跑一趟」同源：到店 + 出餐（含高峰）+ 送到，再加分档缓冲。
        // 老实现只算「出餐均值 + 一段固定速度的路程」，到店段与高峰出餐都没进承诺，
        // 高峰期因此几乎每一单都注定超时（见 `ai::promise_minutes`）。
        let promised_minutes = ai::promise_minutes(
            self,
            merchant_id,
            road_distance_m,
            shop_walk,
            cust_walk,
            tier,
        );

        let fee = economy::quote(
            &self.cfg,
            road_distance_m,
            self.weather,
            self.sim_minute,
            1,
            tier,
            self.cfg.commission_rate,
        );

        let task = if self.rng.random_bool(0.30) && !self.tasks.is_empty() {
            let t = &self.tasks[self.rng.random_range(0..self.tasks.len())];
            Some(ExtraTask {
                id: t.id.clone(),
                name: t.name.clone(),
                step: 0,
                done: false,
                failed: false,
                reward_cents: t.steps.first().map(|s| s.reward_cents).unwrap_or(0),
                penalty_cents: t.on_fail.penalty_cents,
            })
        } else {
            None
        };

        let id = self.next_order_id;
        self.next_order_id += 1;
        let now = self.elapsed_min;
        let customer_name = format!("顾客{}", self.rng.random_range(1000..9999));
        let customer_address = format!(
            "{}附近 · {}号楼",
            merchant_name,
            self.rng.random_range(1..30)
        );
        let remark = pick_remark(&mut self.rng);

        self.orders.push(Order {
            id,
            merchant_id,
            merchant_name,
            merchant_lat: merchant_pos.lat,
            merchant_lng: merchant_pos.lng,
            customer_name,
            customer_address,
            remark,
            customer_lat: customer.lat,
            customer_lng: customer.lng,
            distance_m: road_distance_m,
            promised_minutes,
            tier,
            merchant_walk_difficulty: shop_walk,
            customer_walk_difficulty: cust_walk,
            created_at_min: now,
            claim_deadline_min: now + self.cfg.claim_window_ticks() as u64,
            picked_up_at_min: None,
            delivered_at_min: None,
            fee,
            redispatch_count: 0,
            surcharge_cents: 0,
            status: OrderStatus::Pending,
            assigned_to: None,
            forced: false,
            assign_mode: None,
            back_order: false,
            back_order_bonus_cents: 0,
            task,
            urge_count: 0,
            prep_minutes: None,
        });
        self.stats.orders_created += 1;
        Some(id)
    }

    /// 临时插一张订单（控制端调试 / 演示用）。
    ///
    /// 与 [`World::generate_order`] 共用同一套字段填充与物理量推导，区别只有三点：
    ///
    /// 1. 商家名与两端坐标由调用方给定（入口处已换算回 GCJ-02）；
    /// 2. `fee_cents` 给了就直接当最终报价（夹在
    ///    [`MIN_CUSTOM_FEE_CENTS`]..[`MAX_CUSTOM_FEE_CENTS`]），不给才走计价规则；
    /// 3. 商家 id 按名字回查，查不到记 `0`（未知商家）——`avg_prep_minutes`
    ///    对此有 10 分钟的兜底，不会因此算出荒唐的承诺时限。
    ///
    /// 生成后**不进大厅广播**，由调用方决定怎么通知客户端。
    pub fn create_custom_order(&mut self, spec: CustomOrderSpec) -> OrderId {
        let merchant_pos = LatLng::new(spec.merchant_lat, spec.merchant_lng);
        let customer = LatLng::new(spec.customer_lat, spec.customer_lng);

        let merchant_name = match spec.merchant_name.trim() {
            "" => "临时商家".to_string(),
            s => s.chars().take(24).collect(),
        };

        // 按名字回查商户：名字对上就用它真实的步行难度（出餐时长由
        // `avg_prep_minutes` 按 `merchant_id` 自己取）。对不上（控制台随手编的
        // 名字）就记 0，走未知商家兜底。
        let (merchant_id, shop_walk) = match self.merchants.iter().find(|m| m.name == merchant_name)
        {
            Some(m) => (m.id, m.walk_difficulty.min(2)),
            None => (0, 0u8),
        };

        let road_distance_m = (haversine_m(merchant_pos, customer) * ROAD_FACTOR)
            .round()
            .max(1.0) as u32;
        let tier = OrderTier::Normal;
        let promised_minutes =
            ai::promise_minutes(self, merchant_id, road_distance_m, shop_walk, 0, tier);

        let mut fee = economy::quote(
            &self.cfg,
            road_distance_m,
            self.weather,
            self.sim_minute,
            1,
            tier,
            self.cfg.commission_rate,
        );
        if let Some(cents) = spec.fee_cents {
            // 手填报价：其余分项清零，只留「报价 + 抽水 + 到手」三件对得上的账，
            // 免得控制台看到一串与总额无关的里程/雨补明细还要自己心算。
            let quoted = cents.clamp(MIN_CUSTOM_FEE_CENTS, MAX_CUSTOM_FEE_CENTS);
            let commission = (quoted as f32 * self.cfg.commission_rate).round() as i64;
            fee = FeeBreakdown {
                base_fee_cents: quoted,
                distance_extra_cents: 0,
                rain_bonus_cents: 0,
                level_bonus_cents: 0,
                surge_multiplier: 1.0,
                commission_cents: commission,
                quoted_fee_cents: quoted,
                takehome_cents: quoted - commission,
            };
        }

        let id = self.next_order_id;
        self.next_order_id += 1;
        let now = self.elapsed_min;
        let customer_name = match spec.customer_name.as_deref().map(str::trim) {
            Some(s) if !s.is_empty() => s.chars().take(24).collect(),
            _ => format!("顾客{}", self.rng.random_range(1000..9999)),
        };
        let customer_address = match spec.customer_address.as_deref().map(str::trim) {
            Some(s) if !s.is_empty() => s.chars().take(48).collect(),
            _ => format!("{}附近", merchant_name),
        };

        self.orders.push(Order {
            id,
            merchant_id,
            merchant_name,
            merchant_lat: merchant_pos.lat,
            merchant_lng: merchant_pos.lng,
            customer_name,
            customer_address,
            remark: String::new(),
            customer_lat: customer.lat,
            customer_lng: customer.lng,
            distance_m: road_distance_m,
            promised_minutes,
            tier,
            merchant_walk_difficulty: shop_walk,
            customer_walk_difficulty: 0,
            created_at_min: now,
            claim_deadline_min: now + self.cfg.claim_window_ticks() as u64,
            picked_up_at_min: None,
            delivered_at_min: None,
            fee,
            redispatch_count: 0,
            surcharge_cents: 0,
            status: OrderStatus::Pending,
            assigned_to: None,
            forced: false,
            assign_mode: None,
            back_order: false,
            back_order_bonus_cents: 0,
            task: None,
            urge_count: 0,
            prep_minutes: None,
        });
        self.stats.orders_created += 1;
        id
    }

    /// 把订单退回大厅重投：阶梯加价、重置抢单窗口、累加重投次数。
    ///
    /// 返回 `None` 只表示订单不存在；重投次数用尽时返回 [`Redispatch::NeedsRescue`]，
    /// 订单**依然留在大厅**（继续加价、窗口重置，等着被救单），
    /// 因为「不允许流单」是这个模拟的硬约束。
    ///
    /// 这是「订单不会凭空消失」的唯一出口：无论是没人接还是骑手取消，
    /// 都从这里回到池子，由 `ai::evaluate` 重新挑合适的骑手。
    pub fn release_to_hall(&mut self, order_id: OrderId) -> Option<Redispatch> {
        let now = self.elapsed_min;
        let window = self.cfg.claim_window_ticks() as u64;
        let o = self.order_mut(order_id)?;

        // 次数封顶后不再增长：`count == MAX` 就是「该救单了」的稳定判据。
        o.redispatch_count = (o.redispatch_count + 1).min(MAX_REDISPATCH_ATTEMPTS);
        o.assigned_to = None;
        // 回到大厅就是一张普通待抢单：派单方式的「待选择」状态不能跟着走，
        // 否则下一次被抢走时客户端会以为还需要确认。
        o.forced = false;
        o.assign_mode = None;
        o.back_order = false;
        o.back_order_bonus_cents = 0;

        // 补贴按重投次数阶梯累加：加价既补偿骑手，也提高越过 AI 接单意愿门槛的概率。
        // 次数封顶 → 加价自然封顶，一张没人接的死单推不爆流水。
        o.surcharge_cents = REDISPATCH_SURCHARGE_STEP_CENTS * o.redispatch_count as i64;
        o.status = OrderStatus::Pending;
        o.claim_deadline_min = now + window;
        // 空转掉的时间不记在下一个骑手头上（见 `Order::renew_promise`）。
        o.renew_promise(now, REDISPATCH_PROMISE_FLOOR_MIN);

        if o.redispatch_count >= MAX_REDISPATCH_ATTEMPTS {
            Some(Redispatch::NeedsRescue)
        } else {
            Some(Redispatch::ToHall)
        }
    }

    /// 订单回到大厅等下一轮：**不加重投次数、不加价**。
    ///
    /// 给「没人做错事」的回流用：骑手半路出事故被送医，顾客的饭不能跟着一起消失。
    /// 与 [`World::release_to_hall`] 的区别只在记账口径，归宿一样——
    /// **回到池子里**，绝不判流单。窗口必须重置，否则这单再也抢不了。
    pub fn return_to_hall_uncharged(&mut self, order_id: OrderId) -> bool {
        let now = self.elapsed_min;
        let window = self.cfg.claim_window_ticks() as u64;
        let Some(o) = self.order_mut(order_id) else {
            return false;
        };
        o.status = OrderStatus::Pending;
        o.assigned_to = None;
        o.forced = false;
        o.assign_mode = None;
        o.picked_up_at_min = None;
        o.claim_deadline_min = now + window;
        // 事故退单同理：骑手倒下不是下一任的错（见 `Order::renew_promise`）。
        o.renew_promise(now, REDISPATCH_PROMISE_FLOOR_MIN);
        true
    }

    /// 超过抢单窗口、还挂在大厅的单。
    ///
    /// 只做查询，不改状态：调用方拿着这批 id 走
    /// [`World::release_to_hall`]，重投用尽的再交给
    /// [`crate::sim::scheduler::rescue_nearest`] 强制指派。
    pub fn stale_pending_orders(&self) -> Vec<OrderId> {
        let now = self.elapsed_min;
        self.orders
            .iter()
            .filter(|o| o.status == OrderStatus::Pending && now > o.claim_deadline_min)
            .map(|o| o.id)
            .collect()
    }

    /// 订单是否还有可能被接：大厅里等着、或被指派/配送中。
    ///
    /// 用于「不允许流单」的断言与看板——`false` 意味着这单已经出局
    /// （只可能是已送达/超时送达，或历史数据里的流单）。
    pub fn order_is_in_play(&self, order_id: OrderId) -> bool {
        self.order(order_id)
            .map(|o| o.status.is_active())
            .unwrap_or(false)
    }

    /// 清理历史订单，保留最近 500 条已结束的单，防止内存无界增长。
    pub fn compact_orders(&mut self) {
        const KEEP_FINISHED: usize = 500;
        let finished: Vec<usize> = self
            .orders
            .iter()
            .enumerate()
            .filter(|(_, o)| o.status.is_finished())
            .map(|(i, _)| i)
            .collect();
        if finished.len() <= KEEP_FINISHED {
            return;
        }
        let drop_n = finished.len() - KEEP_FINISHED;
        let to_drop: std::collections::HashSet<usize> = finished.into_iter().take(drop_n).collect();
        let mut i = 0usize;
        self.orders.retain(|_| {
            let keep = !to_drop.contains(&i);
            i += 1;
            keep
        });
    }

    // ---------- 时钟 ----------

    /// 时钟推进 1 分钟。跨日返回 true。
    pub fn advance_clock(&mut self) -> bool {
        self.sim_minute += 1;
        self.elapsed_min += 1;
        if self.sim_minute >= MINUTES_PER_DAY {
            self.sim_minute = 0;
            self.day += 1;
            return true;
        }
        false
    }

    /// 把模拟时钟对齐到真实的「当日第几分钟」（0..1439）。
    ///
    /// **只动 `sim_minute`，不动 `elapsed_min`**：所有截止时间都挂在
    /// `elapsed_min` 上，改它会让在跑的单凭空超时或凭空续命。`day` 也不动——
    /// 实时档的语义是「第 N 天的 14:30 对齐真实 14:30」，天数仍由世界自己累加。
    ///
    /// 返回对齐后的 `sim_minute`。
    pub fn align_clock(&mut self, minute_of_day: u32) -> u32 {
        self.sim_minute = minute_of_day % MINUTES_PER_DAY;
        self.sim_minute
    }

    /// 在线时长与疲劳累积（在线才算）。
    pub fn accrue_online(&mut self) {
        let gain = self.cfg.fatigue.per_online_hour / 60.0;
        let max = self.cfg.fatigue.max;
        // 情绪的主要下滑曲线是「在线时长」——罚款那种是突发的、偶发的，
        // 真正让人烦的是上班本身。关掉情绪时增益为 0，等价于没有这段。
        let mood_gain = if self.cfg.mood.enabled {
            self.cfg.mood.per_online_hour / 60.0
        } else {
            0.0
        };
        for r in &mut self.riders {
            if r.online {
                r.online_minutes += 1;
                r.fatigue = (r.fatigue + gain).min(max);
                r.mood = (r.mood + mood_gain).clamp(0.0, 1.0);
            }
        }
    }

    // ---------- 快照 ----------

    pub fn config_view(&self) -> ConfigView {
        let mut allowed = vec![SpeedMode::Compliant];
        if self.cfg.allow_speeding {
            allowed.push(SpeedMode::Tolerant);
        }
        if self.cfg.allow_unrestricted {
            allowed.push(SpeedMode::Unrestricted);
        }
        ConfigView {
            seed: self.seed,
            speed_multiplier: self.cfg.speed_multiplier,
            paused: self.cfg.speed_multiplier == 0,
            realtime: self.cfg.realtime,
            tick_interval_ms: self.cfg.tick_interval_ms(),
            commission_rate: self.cfg.commission_rate,
            discriminative_commission: self.cfg.discriminative_commission,
            claim_window_secs: self.cfg.claim_window_secs,
            allow_speeding: self.cfg.allow_speeding,
            allow_unrestricted: self.cfg.allow_unrestricted,
            allow_rider_violation: self.cfg.allow_rider_violation,
            backorder_enabled: self.cfg.backorder_enabled,
            backorder_max: self.cfg.backorder.max_held,
            max_orders_per_day: self.cfg.max_orders_per_day,
            max_rejects_per_day: self.cfg.max_rejects_per_day,
            ai_batch_enabled: self.cfg.ai_batch_enabled,
            llm_brain_enabled: self.cfg.llm_brain_enabled,
            show_expected_value: self.cfg.show_expected_value,
            sim_rider_count: self.cfg.sim_rider_count,
            is_peak: self.cfg.is_peak(self.sim_minute),
            surge_multiplier: self.cfg.surge_multiplier(self.sim_minute),
            allowed_speed_modes: allowed,
            fee: FeeView {
                base_fee_cents: self.cfg.fee.base_fee_cents,
                distance_tiers: self.cfg.fee.distance_tiers.clone(),
                per_km_over_cents: self.cfg.fee.per_km_over_cents,
                level_bonus_per_level_cents: self.cfg.fee.level_bonus_per_level_cents,
                surge_multiplier: self.cfg.surge_multiplier(self.sim_minute),
                max_redispatch_attempts: MAX_REDISPATCH_ATTEMPTS,
                redispatch_surcharge_step_cents: REDISPATCH_SURCHARGE_STEP_CENTS,
            },
            insurance_per_day_cents: self.cfg.fine.insurance_per_day_cents,
            backorder_late_buffer_min: self.cfg.backorder.late_buffer_min,
            merge_max_per_rider: self.cfg.merge.algorithm_max_per_rider,
            merge_merchant_cap: self.cfg.merge.max_active_orders_per_merchant,
            merge_wait_min: self.cfg.merge.wait_for_merge_min,
            accident_enabled: self.cfg.accident.enabled,
            accident_rate_multiplier: self.cfg.accident.rate_multiplier,
            mood_enabled: self.cfg.mood.enabled,
            mood_lazy_threshold_max: self.cfg.mood.lazy_threshold_max,
            coord_system: self.cfg.coord_system,
        }
    }

    /// 公开快照。**不含任何收入信息**，收入由 net 层按角色补。
    pub fn public_snapshot(&self) -> PublicSnapshot {
        PublicSnapshot {
            protocol_version: crate::PROTOCOL_VERSION,
            day: self.day,
            sim_minute: self.sim_minute,
            elapsed_min: self.elapsed_min,
            weather: self.weather,
            weather_label: self.weather.label(),
            weather_speed_factor: self.weather.speed_factor(),
            rain_bonus_cents: self.weather.rain_bonus_cents(),
            config: self.config_view(),
            riders: self
                .riders
                .iter()
                .map(|r| RiderBrief::public(r, &self.cfg, self.elapsed_min))
                .collect(),
            hall: self
                .orders
                .iter()
                .filter(|o| o.status == OrderStatus::Pending)
                .cloned()
                .collect(),
            active_orders: self
                .orders
                .iter()
                .filter(|o| o.status.is_active())
                .cloned()
                .collect(),
            merchants: self
                .merchants
                .iter()
                .map(|m| {
                    let c = m.coords().unwrap_or(LatLng::new(0.0, 0.0));
                    MerchantBrief {
                        id: m.id,
                        name: m.name.clone(),
                        address: m.address.clone(),
                        lat: c.lat,
                        lng: c.lng,
                        rating: m.rating,
                        signature: m.signature.clone(),
                        avg_prep_min: m.avg_prep_min,
                    }
                })
                .collect(),
            stats: self.stats.clone(),
            last_day_ledgers: self.last_day_ledgers.clone(),
            // 窗口的真实挂钟时长，而不是配置里那个「基准秒数」。
            // 实时档下窗口退化为 1 tick，这里必须跟着变成 60000，否则
            // 客户端会照着 3 秒去倒计时，而订单其实还能挂一分钟。
            claim_window_ms: self.cfg.claim_window_ticks() as u64 * self.cfg.tick_interval_ms(),
            activities: self.activities.clone(),
            notice: self.notice.clone(),
        }
    }
}

fn pick_remark(rng: &mut ChaCha8Rng) -> String {
    const POOL: [&str; 8] = [
        "",
        "不要香菜",
        "多放辣",
        "放门口，不要敲门",
        "少放盐",
        "帮我带瓶可乐",
        "餐别洒了",
        "到楼下打电话",
    ];
    POOL[rng.random_range(0..POOL.len())].to_string()
}

/// 测试 / 离线模式用的合成商户（真实数据从 data/merchants.json 读）。
pub fn synth_merchants(seed: u64, n: u32) -> Vec<Merchant> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed ^ 0x5EED_5EED);
    let center = LatLng::new(30.5077, 114.4154);
    const PREFIX: [&str; 8] = [
        "老王", "张记", "李姐", "陈氏", "川香", "湘味", "东北", "兰州",
    ];
    const KIND: [&str; 8] = [
        "热干面",
        "烧烤",
        "麻辣烫",
        "牛肉面",
        "砂锅",
        "炒饭",
        "奶茶",
        "卤味",
    ];
    const BLOCK: [&str; 4] = ["光谷", "珞喻路", "鲁巷", "关山"];
    (1..=n)
        .map(|id| {
            let p = LatLng::new(
                center.lat + rng.random_range(-0.03f64..0.03),
                center.lng + rng.random_range(-0.03f64..0.03),
            );
            let (pi, ki) = (rng.random_range(0..8), rng.random_range(0..8));
            Merchant {
                id,
                name: format!(
                    "{}{}（{}店）",
                    PREFIX[pi],
                    KIND[ki],
                    BLOCK[rng.random_range(0..4)]
                ),
                city: "武汉".into(),
                address: format!("武汉市洪山区珞喻路{}号", rng.random_range(1..500)),
                lat: Some(p.lat),
                lng: Some(p.lng),
                rating: rng.random_range(4.0f32..5.0),
                avg_prep_min: rng.random_range(5..25),
                signature: format!("招牌{}", KIND[(ki + 1) % 8]),
                // 合成数据没有人工标注，按「路边店为主、少数商场店」的分布给档
                walk_difficulty: match rng.random_range(0..100u32) {
                    0..=64 => 0,
                    65..=89 => 1,
                    _ => 2,
                },
            }
        })
        .collect()
}

pub fn synth_task() -> TaskTemplate {
    use super::task::TaskStep;
    TaskTemplate {
        id: "leave_at_door".into(),
        name: "放门口".into(),
        steps: vec![
            TaskStep {
                rider_prompt: "您好，我快到了，放哪个门？".into(),
                customer_responses: vec!["放门口吧".into(), "门口有柜子".into()],
                requires_keywords: vec!["门".into()],
                completion_keywords: vec![],
                reward_cents: 0,
                penalty_cents: 0,
            },
            TaskStep {
                rider_prompt: "好的，门禁密码是多少？".into(),
                customer_responses: vec!["1234".into(), "没有密码".into()],
                requires_keywords: vec!["密码".into(), "门禁".into()],
                completion_keywords: vec!["没有".into()],
                reward_cents: 50,
                penalty_cents: 0,
            },
            TaskStep {
                rider_prompt: "已放门口，麻烦确认一下。".into(),
                customer_responses: vec!["收到了，谢谢".into()],
                requires_keywords: vec![],
                completion_keywords: vec!["谢谢".into(), "确认".into(), "收到".into()],
                reward_cents: 100,
                penalty_cents: 0,
            },
        ],
        on_complete: super::task::TaskOutcome {
            rating_delta: 0.02,
            penalty_cents: 0,
        },
        on_fail: super::task::TaskOutcome {
            rating_delta: -0.02,
            penalty_cents: 0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_rolls_over_at_midnight() {
        let cfg = SimConfig {
            sim_rider_count: 0,
            ..Default::default()
        };
        let mut w = World::new_for_test(cfg, 1);
        // 当日最后一个合法分钟是 1439，从 1438 推一次到 1439 不跨日。
        w.sim_minute = MINUTES_PER_DAY - 2;
        assert!(!w.advance_clock());
        assert!(w.advance_clock());
        assert_eq!(w.day, 2);
        assert_eq!(w.sim_minute, 0);
        assert_eq!(w.elapsed_min, 2);
    }

    #[test]
    fn align_clock_only_moves_the_wall_clock() {
        let cfg = SimConfig {
            sim_rider_count: 0,
            ..Default::default()
        };
        let mut w = World::new_for_test(cfg, 1);
        w.elapsed_min = 500;

        assert_eq!(w.align_clock(14 * 60 + 30), 870);
        assert_eq!(w.sim_minute, 870);
        // elapsed_min 是所有截止时间的唯一基准，对齐时钟不许动它。
        assert_eq!(w.elapsed_min, 500, "对齐时钟不该改 elapsed_min");
        assert_eq!(w.day, 1, "对齐时钟不该改天数");

        // 越界回绕到当日范围内。
        w.align_clock(MINUTES_PER_DAY + 5);
        assert_eq!(w.sim_minute, 5);
    }

    #[test]
    fn rider_names_are_sanitized_and_applied_in_order() {
        let cfg = SimConfig {
            sim_rider_count: 3,
            ..Default::default()
        };
        let mut w = World::new_for_test(cfg, 11);
        let ids = w.sim_rider_ids();
        assert_eq!(ids.len(), 3);

        // 名单按 id 升序生效；空名字跳过，保留原名。
        let names = ["  张三 ".to_string(), "李四".to_string(), "   ".to_string()];
        assert_eq!(w.apply_rider_names(&names), vec![ids[0], ids[1]]);
        assert_eq!(w.rider(ids[0]).unwrap().name, "张三");
        assert_eq!(w.rider(ids[1]).unwrap().name, "李四");
        assert!(w.rider(ids[2]).unwrap().name.starts_with("AI-"));

        // 超长截断、控制字符剔除。
        assert!(w.rename_rider(ids[2], "一二三四五六七八九十十一十二十三"));
        assert_eq!(
            w.rider(ids[2]).unwrap().name.chars().count(),
            World::MAX_RIDER_NAME_CHARS
        );
        assert!(w.rename_rider(ids[2], "带\n换行单"));
        assert_eq!(w.rider(ids[2]).unwrap().name, "带换行单");

        // 空名、不存在的骑手、名字没变化都拒绝。
        assert!(!w.rename_rider(ids[0], "   "));
        assert!(!w.rename_rider(999, "无名"));
        assert!(!w.rename_rider(ids[1], "李四"));
        assert_eq!(w.sim_rider_names().len(), 3);
    }

    #[test]
    fn generated_orders_are_valid() {
        let mut w = World::new_for_test(SimConfig::default(), 7);
        let ids = w.generate_order_batch(50);
        assert_eq!(ids.len(), 50);
        for o in &w.orders {
            assert!(o.distance_m >= 1);
            assert!(crate::sim::config::PROMISE_MINUTES_RANGE.0 <= o.promised_minutes);
            assert!(o.promised_minutes <= crate::sim::config::PROMISE_MINUTES_RANGE.1);
            assert!(o.fee.quoted_fee_cents > 0, "报价必须为正");
            assert!(o.customer_pos().is_valid());
            assert!(o.merchant_pos().is_valid());
            assert_eq!(o.status, OrderStatus::Pending);
        }
    }

    /// 临时插的单要把调用方填的东西原样带上，且补齐的物理量与自然单同源。
    #[test]
    fn custom_order_keeps_the_handed_in_fields() {
        let mut w = World::new_for_test(SimConfig::default(), 5);
        let known = w.merchants[0].name.clone();
        let id = w.create_custom_order(CustomOrderSpec {
            // 名字带空白：trim 之后应该还能命中真实商户。
            merchant_name: format!("  {known}  "),
            merchant_lat: 30.5,
            merchant_lng: 114.4,
            customer_name: Some("测试顾客".into()),
            customer_address: None,
            customer_lat: 30.51,
            customer_lng: 114.41,
            fee_cents: None,
        });
        let o = w.order(id).expect("订单在");
        assert_eq!(o.merchant_name, known);
        assert_ne!(o.merchant_id, 0, "名字对得上就该回填真实商户 id");
        assert_eq!(o.customer_name, "测试顾客");
        assert_eq!((o.merchant_lat, o.merchant_lng), (30.5, 114.4));
        assert_eq!((o.customer_lat, o.customer_lng), (30.51, 114.41));
        assert!(o.distance_m >= 1);
        assert!(
            crate::sim::config::PROMISE_MINUTES_RANGE.0 <= o.promised_minutes
                && o.promised_minutes <= crate::sim::config::PROMISE_MINUTES_RANGE.1
        );
        assert!(o.fee.quoted_fee_cents > 0);
        assert_eq!(o.status, OrderStatus::Pending);
        assert_eq!(o.assigned_to, None);
        assert!(!o.forced, "临时单不是强制指派单");
        assert!(o.customer_pos().is_valid());
    }

    /// 手填金额：给了就直接当报价，越界夹取；不给才走计价规则。
    #[test]
    fn custom_order_fee_can_be_forced_and_is_clamped() {
        let mut w = World::new_for_test(SimConfig::default(), 5);
        let spec = |fee: Option<i64>| CustomOrderSpec {
            // 编造的名字一定不在商户表里 → merchant_id 0，出餐走兜底。
            merchant_name: "自造商家".into(),
            merchant_lat: 30.5,
            merchant_lng: 114.4,
            customer_name: None,
            customer_address: None,
            customer_lat: 30.6,
            customer_lng: 114.5,
            fee_cents: fee,
        };

        let id = w.create_custom_order(spec(Some(1234)));
        let o = w.order(id).expect("订单在");
        assert_eq!(o.merchant_id, 0);
        assert_eq!(o.merchant_name, "自造商家");
        assert_eq!(o.fee.base_fee_cents, 1234);
        assert_eq!(o.fee.quoted_fee_cents, 1234);
        assert_eq!(
            o.fee.takehome_cents,
            1234 - o.fee.commission_cents,
            "到手必须 = 报价 - 抽水"
        );

        // 手滑 0 元会造出一张白跑的单，夹到下界；天文数字夹到上界。
        let low_id = w.create_custom_order(spec(Some(0)));
        assert_eq!(
            w.order(low_id).expect("订单在").fee.quoted_fee_cents,
            MIN_CUSTOM_FEE_CENTS
        );
        let high_id = w.create_custom_order(spec(Some(i64::MAX)));
        assert_eq!(
            w.order(high_id).expect("订单在").fee.quoted_fee_cents,
            MAX_CUSTOM_FEE_CENTS
        );

        // 不填金额时回到计价规则：报价为正，且明细能对上总额。
        let auto_id = w.create_custom_order(spec(None));
        let auto = w.order(auto_id).expect("订单在");
        assert!(auto.fee.quoted_fee_cents > 0);
        assert_eq!(
            auto.fee.quoted_fee_cents,
            auto.fee.base_fee_cents
                + auto.fee.distance_extra_cents
                + auto.fee.level_bonus_cents
                + auto.fee.rain_bonus_cents
        );
    }

    /// 退回大厅的单要给接手骑手重新留出时限。
    ///
    /// 否则「超时救单」塞过去的每一单都必然超时：单子在大厅空转多久，
    /// 接手的人就少跑多久，白跑一趟还倒贴罚金。
    #[test]
    fn redispatch_renews_the_promise() {
        let mut w = World::new_for_test(SimConfig::default(), 9);
        let id = w.generate_order_batch(1)[0];

        // 让单子在大厅干等，直到承诺被空转吃干净。
        //
        // **空转时长按承诺长短算，不写死60。** 承诺现在含末端放置窗口
        // （见 `ai::promise_minutes`），比以前长；写死 60 会在承诺
        // 超过 60 时「还没吃完就断言」，测试会因前提不成立而失败——
        // 那不是逻辑坏了，是假设过时了。
        let promised0 = w.order(id).expect("订单在").promised_minutes;
        let wait = promised0 + 10;
        for _ in 0..wait {
            w.advance_clock();
        }
        let created = w.order(id).expect("订单在").created_at_min;
        let idle = (w.elapsed_min - created) as u32;
        assert!(idle >= w.order(id).expect("订单在").promised_minutes);

        assert_eq!(w.release_to_hall(id), Some(Redispatch::ToHall));

        let promised = w.order(id).expect("订单在").promised_minutes;
        assert_eq!(
            promised - idle,
            REDISPATCH_PROMISE_FLOOR_MIN,
            "退回首后剩余时限应回到保底值"
        );

        // 再空转同样久：只补回空转掉的那一段，不会越滚越长。
        for _ in 0..wait {
            w.advance_clock();
        }
        let idle2 = (w.elapsed_min - created) as u32;
        w.release_to_hall(id);
        let promised2 = w.order(id).expect("订单在").promised_minutes;
        assert_eq!(promised2 - idle2, REDISPATCH_PROMISE_FLOOR_MIN);
        assert!(promised2 > promised);
    }

    #[test]
    fn stale_orders_are_redispatched_before_expiring() {
        let mut w = World::new_for_test(SimConfig::default(), 3);
        let ids = w.generate_order_batch(3);

        // 首轮超窗：不流单，加价回到池子里继续等人接。
        for _ in 0..10 {
            w.advance_clock();
        }
        for id in w.stale_pending_orders() {
            assert_eq!(w.release_to_hall(id), Some(Redispatch::ToHall));
        }
        assert_eq!(w.pending_orders().len(), 3, "订单应仍在大厅等人接");

        let o = w.order(ids[0]).expect("订单还在");
        assert_eq!(o.redispatch_count, 1);
        assert_eq!(o.surcharge_cents, REDISPATCH_SURCHARGE_STEP_CENTS);
        assert_eq!(
            o.quoted_fee_cents(),
            o.fee.quoted_fee_cents + REDISPATCH_SURCHARGE_STEP_CENTS,
            "加价必须体现在骑手看到的报价上"
        );
        assert_eq!(o.status, OrderStatus::Pending);

        // 再耗两轮，重投次数用尽 → **不是流单**，而是标记「该救单了」。
        for _ in 0..2 {
            for _ in 0..10 {
                w.advance_clock();
            }
            for id in w.stale_pending_orders() {
                w.release_to_hall(id);
            }
        }
        for id in &ids {
            let o = w.order(*id).expect("订单还在");
            assert_eq!(
                o.redispatch_count, MAX_REDISPATCH_ATTEMPTS,
                "重投次数封顶后不再增长"
            );
            assert_eq!(o.status, OrderStatus::Pending, "投满上限也不许流单");
            assert!(
                o.claim_deadline_min > w.elapsed_min,
                "窗口必须重置，否则再也抢不了"
            );
        }

        // 再往后走也一样：只会一直 NeedsRescue，永远不会变成 Expired。
        for _ in 0..3 {
            for _ in 0..10 {
                w.advance_clock();
            }
            for id in w.stale_pending_orders() {
                assert_eq!(w.release_to_hall(id), Some(Redispatch::NeedsRescue));
            }
        }
        assert_eq!(w.stats.orders_expired, 0, "整个模拟里不允许有流单出口");
        assert!(
            w.stale_pending_orders().is_empty(),
            "刚重置过窗口，这一轮不该还有超窗单"
        );
    }

    /// 补贴有上限：一张一直没人接的死单不能把加价推到天上。
    #[test]
    fn redispatch_surcharge_is_capped() {
        let mut w = World::new_for_test(SimConfig::default(), 1);
        let ids = w.generate_order_batch(1);
        for _ in 0..60 {
            for _ in 0..10 {
                w.advance_clock();
            }
            for id in w.stale_pending_orders() {
                w.release_to_hall(id);
            }
        }
        let o = w.order(ids[0]).expect("订单还在");
        assert_eq!(o.status, OrderStatus::Pending);
        assert_eq!(
            o.surcharge_cents,
            REDISPATCH_SURCHARGE_STEP_CENTS * MAX_REDISPATCH_ATTEMPTS as i64,
            "重投次数封顶，加价跟着封顶"
        );
    }

    /// 事故回流：不加重投次数、不加价，但窗口重置，订单仍在局里。
    #[test]
    fn accident_return_to_hall_is_not_charged() {
        let mut w = World::new_for_test(SimConfig::default(), 1);
        let ids = w.generate_order_batch(1);
        let order_id = ids[0];
        let rider_id = w.riders[0].id;
        crate::sim::scheduler::try_claim(&mut w, order_id, rider_id).expect("抢单失败");

        let before = w.order(order_id).expect("订单还在").surcharge_cents;
        assert!(w.return_to_hall_uncharged(order_id));

        let o = w.order(order_id).expect("订单还在");
        assert_eq!(o.status, OrderStatus::Pending);
        assert_eq!(o.redispatch_count, 0, "骑手出事故不算拒单");
        assert_eq!(o.surcharge_cents, before, "事故回流不加价");
        assert!(o.assigned_to.is_none());
        assert!(o.claim_deadline_min > w.elapsed_min);
        assert!(w.order_is_in_play(order_id), "回流后仍在局里");
    }

    #[test]
    fn cancelled_order_goes_back_to_hall() {
        let mut w = World::new_for_test(SimConfig::default(), 1);
        let ids = w.generate_order_batch(1);
        let order_id = ids[0];

        // 让骑手先接单，再取消。
        let rider_id = w.riders[0].id;
        crate::sim::scheduler::try_claim(&mut w, order_id, rider_id).expect("抢单失败");
        assert!(w.riders[0].is_holding(order_id));

        crate::sim::scheduler::reject_order(&mut w, rider_id, order_id).expect("取消失败");

        let o = w.order(order_id).expect("订单还在");
        assert_eq!(
            o.status,
            OrderStatus::Pending,
            "被取消的单要退回大厅而非作废"
        );
        assert!(o.assigned_to.is_none());
        assert_eq!(o.redispatch_count, 1);
        assert!(o.surcharge_cents > 0, "重投要加价");
        assert!(
            !w.riders[0].is_holding(order_id),
            "取消后骑手不应再持有该单"
        );
    }

    #[test]
    fn compaction_bounds_memory() {
        let mut w = World::new_for_test(SimConfig::default(), 5);
        w.generate_order_batch(20);
        for o in &mut w.orders {
            o.status = OrderStatus::Delivered;
        }
        let extra = w.generate_order_batch(600);
        for id in extra {
            if let Some(o) = w.order_mut(id) {
                o.status = OrderStatus::Expired;
            }
        }
        w.compact_orders();
        assert!(w.orders.len() < 700, "orders={}", w.orders.len());
    }

    #[test]
    fn snapshot_has_no_money_for_others() {
        let w = World::new_for_test(SimConfig::default(), 11);
        let snap = w.public_snapshot();
        assert_eq!(snap.protocol_version, crate::PROTOCOL_VERSION);
        for r in &snap.riders {
            assert!(r.takehome_cents.is_none(), "公开快照不能带收入");
            assert!(r.cash_cents.is_none());
        }
    }

    #[test]
    fn weather_script_advances() {
        let cfg = SimConfig {
            sim_rider_count: 0,
            ..Default::default()
        };
        let mut w = World::new_for_test(cfg, 2);
        w.weather_script = vec![
            WeatherEvent {
                at: 10,
                to: Weather::Cloudy,
            },
            WeatherEvent {
                at: 20,
                to: Weather::Rainstorm,
            },
        ];
        w.sim_minute = 0;
        assert!(w.advance_weather_script().is_none());
        w.sim_minute = 10;
        assert_eq!(w.advance_weather_script(), Some(Weather::Cloudy));
        w.sim_minute = 25;
        assert_eq!(w.advance_weather_script(), Some(Weather::Rainstorm));
        assert_eq!(w.weather, Weather::Rainstorm);
    }
}
