//! 全局状态。tick 循环唯一的可变对象。

use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::Serialize;

use super::{
    MINUTES_PER_DAY, OrderId, RiderId,
    ai::LlmPlan,
    config::SimConfig,
    economy,
    event::{DayLedger, RiderBrief},
    geo::{LatLng, bearing_deg, haversine_m},
    merchant::Merchant,
    order::{ExtraTask, Order, OrderStatus, OrderTier},
    rider::{Phase, Rider, RiderKind, SpeedMode},
    task::TaskTemplate,
    weather::{Weather, WeatherEvent},
};

/// 路网绕行系数：直线 → 实际骑行距离。
pub const ROAD_FACTOR: f64 = 1.25;

/// 运行统计。
#[derive(Debug, Clone, Default, Serialize)]
pub struct SimStats {
    pub orders_created: u64,
    pub orders_expired: u64,
    pub orders_delivered: u64,
    pub orders_late: u64,
    pub claims: u64,
    pub accidents: u32,
    pub camera_fines: u32,
    /// LLM 战术大脑调用次数（一天最多 24 次）。
    pub llm_calls: u32,
    /// LLM 失败 / 被校验丢弃后回退算法的次数。
    pub llm_fallbacks: u32,
}

/// 控制端可调项的只读视图。
#[derive(Debug, Clone, Serialize)]
pub struct ConfigView {
    pub seed: u64,
    pub speed_multiplier: u32,
    pub paused: bool,
    pub tick_interval_ms: u64,
    pub commission_rate: f32,
    pub discriminative_commission: bool,
    pub claim_window_secs: u32,
    pub allow_speeding: bool,
    pub allow_unrestricted: bool,
    pub backorder_enabled: bool,
    pub backorder_max: u32,
    pub max_orders_per_day: u32,
    pub ai_batch_enabled: bool,
    pub llm_brain_enabled: bool,
    pub show_expected_value: bool,
    pub sim_rider_count: u32,
    pub is_peak: bool,
    pub surge_multiplier: f32,
    /// 当前允许的档位（UI 灰显用）。
    pub allowed_speed_modes: Vec<SpeedMode>,
    pub base_fee_cents: i64,
    pub insurance_per_day_cents: i64,
    pub backorder_late_buffer_min: u32,
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
    pub fn rider_current_target(&self, rider: &Rider) -> Option<LatLng> {
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
        let allowed = |m: SpeedMode, cfg: &SimConfig| match m {
            SpeedMode::Compliant => true,
            SpeedMode::Tolerant => cfg.allow_speeding,
            SpeedMode::Unrestricted => cfg.allow_unrestricted,
        };
        let cfg = &self.cfg;
        let mut changed = Vec::new();
        for r in &mut self.riders {
            if !allowed(r.speed_mode, cfg) || !allowed(r.strategy.speed_mode, cfg) {
                r.speed_mode = SpeedMode::Compliant;
                r.strategy.speed_mode = SpeedMode::Compliant;
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
        let candidates: Vec<(u32, String, LatLng, f32, u32)> = self
            .merchants
            .iter()
            .filter_map(|m| {
                m.coords()
                    .map(|c| (m.id, m.name.clone(), c, m.rating, m.avg_prep_min))
            })
            .collect();
        if candidates.is_empty() {
            return None;
        }

        // 评分加权：rating^6，把 4.5 和 4.9 的差距拉到 1.7 倍
        let weights: Vec<f64> = candidates.iter().map(|c| (c.3 as f64).powi(6)).collect();
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
        let (merchant_id, merchant_name, merchant_pos, _rating, prep) = candidates[idx].clone();

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

        // 承诺时间 = 出餐 + 配送(合规速度) + 缓冲，向上取整到 5 分钟
        let compliant_mps = 30.0f32 / 3.6;
        let travel_min = (road_distance_m as f32 / compliant_mps / 60.0).ceil();
        let buffer = match tier {
            OrderTier::Normal => 12.0,
            OrderTier::Premium => 10.0,
            OrderTier::Express => 6.0,
        };
        let promised_minutes =
            (((prep as f32 + travel_min + buffer) / 5.0).ceil() * 5.0).clamp(15.0, 75.0) as u32;

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
            created_at_min: now,
            claim_deadline_min: now + self.cfg.claim_window_ticks() as u64,
            picked_up_at_min: None,
            delivered_at_min: None,
            fee,
            status: OrderStatus::Pending,
            assigned_to: None,
            forced: false,
            back_order: false,
            back_order_bonus_cents: 0,
            task,
            urge_count: 0,
            prep_minutes: None,
        });
        self.stats.orders_created += 1;
        Some(id)
    }

    /// 超过抢单窗口的待抢单 → Expired。
    pub fn expire_stale_orders(&mut self) -> Vec<OrderId> {
        let now = self.elapsed_min;
        let mut expired = Vec::new();
        for o in &mut self.orders {
            if o.status == OrderStatus::Pending && now > o.claim_deadline_min {
                o.status = OrderStatus::Expired;
                expired.push(o.id);
            }
        }
        self.stats.orders_expired += expired.len() as u64;
        expired
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

    /// 在线时长与疲劳累积（在线才算）。
    pub fn accrue_online(&mut self) {
        let gain = self.cfg.fatigue.per_online_hour / 60.0;
        let max = self.cfg.fatigue.max;
        for r in &mut self.riders {
            if r.online {
                r.online_minutes += 1;
                r.fatigue = (r.fatigue + gain).min(max);
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
            tick_interval_ms: self.cfg.tick_interval_ms(),
            commission_rate: self.cfg.commission_rate,
            discriminative_commission: self.cfg.discriminative_commission,
            claim_window_secs: self.cfg.claim_window_secs,
            allow_speeding: self.cfg.allow_speeding,
            allow_unrestricted: self.cfg.allow_unrestricted,
            backorder_enabled: self.cfg.backorder_enabled,
            backorder_max: self.cfg.backorder.max_held,
            max_orders_per_day: self.cfg.max_orders_per_day,
            ai_batch_enabled: self.cfg.ai_batch_enabled,
            llm_brain_enabled: self.cfg.llm_brain_enabled,
            show_expected_value: self.cfg.show_expected_value,
            sim_rider_count: self.cfg.sim_rider_count,
            is_peak: self.cfg.is_peak(self.sim_minute),
            surge_multiplier: self.cfg.surge_multiplier(self.sim_minute),
            allowed_speed_modes: allowed,
            base_fee_cents: self.cfg.fee.base_fee_cents,
            insurance_per_day_cents: self.cfg.fine.insurance_per_day_cents,
            backorder_late_buffer_min: self.cfg.backorder.late_buffer_min,
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
            claim_window_ms: self.cfg.claim_window_secs as u64 * 1000,
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
    fn generated_orders_are_valid() {
        let mut w = World::new_for_test(SimConfig::default(), 7);
        let ids = w.generate_order_batch(50);
        assert_eq!(ids.len(), 50);
        for o in &w.orders {
            assert!(o.distance_m >= 1);
            assert!((15..=75).contains(&o.promised_minutes));
            assert!(o.fee.quoted_fee_cents > 0, "报价必须为正");
            assert!(o.customer_pos().is_valid());
            assert!(o.merchant_pos().is_valid());
            assert_eq!(o.status, OrderStatus::Pending);
        }
    }

    #[test]
    fn stale_orders_expire() {
        let mut w = World::new_for_test(SimConfig::default(), 3);
        w.generate_order_batch(3);
        for _ in 0..10 {
            w.advance_clock();
        }
        let expired = w.expire_stale_orders();
        assert_eq!(expired.len(), 3);
        assert!(w.pending_orders().is_empty());
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
