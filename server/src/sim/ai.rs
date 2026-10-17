//! AI 决策：单骑手打分 + 批量贪心指派 + LLM 计划校验。
//!
//! **算法负责 16ms 内必须完成的决策**（玩法设计 §9）。
//! LLM 可以给计划，但计划必须过 `validate_llm_plan`，不信任任何输出。

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use super::{
    OrderId, RiderId,
    config::{PEAK_PREP_FACTOR, PROMISE_MINUTES_RANGE, SimConfig, TYPICAL_TO_SHOP_M},
    event::{AiAssignment, DecisionSource},
    geo::{angle_between, bearing_deg, haversine_m},
    order::{Order, OrderTier},
    rider::{Phase, Rider},
    world::World,
};

/// 从骑手接单到送达的行程估算（分钟）。
#[derive(Debug, Clone, Copy, Serialize, Default)]
pub struct DeliveryPlan {
    pub to_shop_min: u32,
    pub prep_min: u32,
    pub to_customer_min: u32,
}

impl DeliveryPlan {
    pub fn total_min(&self) -> u32 {
        self.to_shop_min + self.prep_min + self.to_customer_min
    }
}

/// 一个候选分配。
#[derive(Debug, Clone)]
pub struct Assignment {
    pub rider: RiderId,
    pub order: OrderId,
    pub score: f32,
    pub reason: String,
    pub source: DecisionSource,
    pub plan: DeliveryPlan,
}

impl Assignment {
    /// 转成下发给控制端的事件载荷。
    pub fn to_event(&self) -> AiAssignment {
        AiAssignment {
            order_id: self.order,
            rider_id: self.rider,
            rider_key: crate::sim::rider_key(self.rider),
            score: self.score,
            reason: self.reason.clone(),
        }
    }
}

/// 路网绕行系数：**直线距离** ×1.3 近似真实骑行距离。
///
/// 只对直线距离用。`Order::distance_m` 在生成时已经乘过
/// [`super::world::ROAD_FACTOR`]（1.25），本身就是路网距离，**不能再乘这个系数**——
/// 历史上重复乘过一次，让实际送端行程比承诺假设多出约 30%。
const DETOUR_FACTOR: f32 = 1.3;

/// 取餐端步行秒数：商场内店要走进楼，高层还要等电梯。
pub fn merchant_walk_secs_of(cfg: &SimConfig, difficulty: u8) -> f32 {
    let w = &cfg.walk;
    w.merchant_walk_m(difficulty) as f32 / w.walk_speed_mps()
        + if w.needs_elevator(difficulty) {
            w.elevator_penalty_secs as f32
        } else {
            0.0
        }
}

/// 送端步行秒数：送到楼上还要走下来，所以算的是往返距离。
pub fn customer_walk_secs_of(cfg: &SimConfig, difficulty: u8) -> f32 {
    let w = &cfg.walk;
    w.customer_walk_m(difficulty) as f32 / w.walk_speed_mps()
}

/// 取餐端步行秒数的订单版。
pub fn merchant_walk_secs(world: &World, order: &Order) -> f32 {
    merchant_walk_secs_of(&world.cfg, order.merchant_walk_difficulty)
}

/// 送端步行秒数的订单版。
pub fn customer_walk_secs(world: &World, order: &Order) -> f32 {
    customer_walk_secs_of(&world.cfg, order.customer_walk_difficulty)
}

/// 送端行程分钟（骑行 + 顾客端步行往返）。
///
/// 与 [`plan_for`] 共用同一套算法，保证「估算的时间」和「实际推进的时间」一致。
pub fn customer_leg_min(world: &World, rider: &Rider, order: &Order) -> u32 {
    let ride_mps = rider.speed_mps(&world.cfg, world.weather).max(1.0);
    // `distance_m` 已经是路网距离（生成时乘过 ROAD_FACTOR），这里不再乘绕行系数。
    let ride_m = order.distance_m as f32;
    ((ride_m / ride_mps + customer_walk_secs(world, order)) / 60.0)
        .ceil()
        .max(1.0) as u32
}

/// 生成订单时定「承诺送达时限」（分钟）。
///
/// 必须和 [`plan_for`] 算的是同一件事——「一个骑手跑完这一单要多久」——
/// 否则承诺与真实行程脱节，超时就变成必然。这里补上三段最常被漏掉的时间：
///
/// 1. **骑手到店**（含取餐步行）：下单时还不知道谁会接，按 [`TYPICAL_TO_SHOP_M`] 估；
/// 2. **高峰出餐**：用与 `scheduler::attach` 同一个 [`PEAK_PREP_FACTOR`]，
///    而不是商家均值——高峰的实际出餐是均值的两倍；
/// 3. **末端放置窗口**：到门口之后还要停车、上楼、放门口，
///    这段时间由 [`chat::roll_window`] 掷出来（每单都有，5~60 分钟）。
///    **漏掉这一段会导致「到达即超时」**——交付回归测试
///    `a_days_deliveries_are_not_all_late` 就是在守这条线。
///
/// 速度一律按合规档的 [`SimConfig::speed`] 基准算：骑手跑得更快是自己省下的时间，
/// 不该写进承诺。暴雨叠加与疲劳同样不进承诺，那是留给玩家的博弈空间。
pub fn promise_minutes(
    world: &World,
    merchant_id: u32,
    road_distance_m: u32,
    merchant_walk_difficulty: u8,
    customer_walk_difficulty: u8,
    tier: OrderTier,
) -> u32 {
    let cfg = &world.cfg;
    let base_mps = (cfg.speed.base_speed_kmh / 3.6).max(1.0);

    // 到店段：典型接单距离走的是直线，这里该乘绕行系数。
    let to_shop_secs = TYPICAL_TO_SHOP_M * DETOUR_FACTOR / base_mps
        + merchant_walk_secs_of(cfg, merchant_walk_difficulty);

    // 出餐段：与 attach 同一套系数，但取期望值（不含暴雨叠加与 ±20% 抖动）。
    let peak = if cfg.is_peak(world.sim_minute) {
        PEAK_PREP_FACTOR
    } else {
        1.0
    };
    let prep_min = (world.avg_prep_minutes(merchant_id) as f32 * peak)
        .round()
        .max(1.0);

    // 送端段：路网距离 + 顾客端步行往返。
    let to_customer_secs =
        road_distance_m as f32 / base_mps + customer_walk_secs_of(cfg, customer_walk_difficulty);

    // 末端放置窗口：到门口之后还要停车 / 上楼 / 放门口的实打实的时间。
    // 这一段**每单都有**（与顾客开不开口无关，见 `chat::roll_window` 注释），
    // 所以取窗口的**期望值**——用上限会让所有单都显得宽裕，用下限又必然超时。
    let window_secs = expected_window_min() * 60.0;

    // 分档缓冲：急单给得少，那是它加价的来源。
    let buffer_min = match tier {
        OrderTier::Normal => 12.0,
        OrderTier::Premium => 10.0,
        OrderTier::Express => 6.0,
    };

    let total_min =
        (to_shop_secs + prep_min * 60.0 + to_customer_secs + window_secs) / 60.0 + buffer_min;
    let (lo, hi) = PROMISE_MINUTES_RANGE;
    ((total_min / 5.0).ceil() * 5.0).clamp(lo as f32, hi as f32) as u32
}

/// 末端放置窗口的**期望值**（模拟分钟）。
///
/// `roll_window` 在 `[MIN, MAX]` 上均匀掷，所以期望就是中点。
/// 抽成函数是为了让承诺计算和测试用同一个口径——将来若把掷法改成
/// 偏态分布（比如多数 3~8 分钟、偶尔 60 分钟），只改这一处。
pub fn expected_window_min() -> f32 {
    (super::chat::MIN_WINDOW_MIN + super::chat::MAX_WINDOW_MIN) as f32 / 2.0
}

/// 估算行程（骑行 + 步行）。
///
/// 「商场单」难跑就在这里：骑行时间之外，还要停车、走进楼、找店、坐电梯。
/// 步行段是**物理量**，由 [`super::config::WalkRules`] 给出，**不接受 LLM 干预**
/// （《LLM协议设计》§5.1、设计红线 1）。
pub fn plan_for(world: &World, rider: &Rider, order: &Order) -> DeliveryPlan {
    let ride_mps = rider.speed_mps(&world.cfg, world.weather).max(1.0);
    let to_shop_ride_m = haversine_m(rider.pos(), order.merchant_pos()) as f32 * DETOUR_FACTOR;
    let to_shop_min = ((to_shop_ride_m / ride_mps + merchant_walk_secs(world, order)) / 60.0)
        .ceil()
        .max(1.0) as u32;

    // 已到店则用实际出餐耗时，否则用商家平均值估算
    let prep = order
        .prep_minutes
        .unwrap_or_else(|| world.avg_prep_minutes(order.merchant_id));

    DeliveryPlan {
        to_shop_min,
        prep_min: prep,
        to_customer_min: customer_leg_min(world, rider, order),
    }
}

/// 这一单的预估到手（分/单）。
pub fn estimated_takehome_cents(world: &World, rider: &Rider, order: &Order) -> i64 {
    let rate = super::economy::commission_rate_for(&world.cfg, rider);
    // 含重投补贴：加价会直接抬高预估时薪，订单因此更容易越过接单门槛。
    let base = order.quoted_fee_cents();
    base - (base as f32 * rate).round() as i64 + order.back_order_bonus_cents
}

/// 单骑手打分（玩法设计 §7.1）。
///
/// ```text
/// score = 时薪×0.4 + 顺路度×0.3 + 距离偏好×0.2 + 商家评分×0.1 - 背单风险×0.25
/// ```
pub fn evaluate(world: &World, rider_id: RiderId, order_id: OrderId) -> Option<Assignment> {
    let rider = world.rider(rider_id)?;
    let order = world.order(order_id)?;
    let cfg = &world.cfg;

    let plan = plan_for(world, rider, order);
    let total_min = plan.total_min().max(1) as f32;
    let takehome = estimated_takehome_cents(world, rider, order);

    // 时薪（分/小时），60 元/h 记满分
    let hourly = (takehome as f32 / total_min * 60.0 / 6000.0).clamp(0.0, 1.0);

    // 顺路度：与手持单目标方向夹角；空闲骑手按距离衰减
    let detour = if let Some(dest) = world.rider_current_target(rider) {
        let cur = bearing_deg(rider.pos(), dest);
        let new_dir = bearing_deg(rider.pos(), order.merchant_pos());
        1.0 - angle_between(cur, new_dir) / 180.0
    } else {
        let d = haversine_m(rider.pos(), order.merchant_pos()) as f32;
        (1.0 - d / 3000.0).clamp(0.0, 1.0)
    };

    // 距离偏好：1~3km 最舒服
    let dist_km = order.distance_m as f32 / 1000.0;
    let distance_pref = (1.0 - (dist_km - 2.0).abs() / 4.0).clamp(0.0, 1.0);

    // 商家评分
    let merchant_rating = world
        .merchant(order.merchant_id)
        .map(|m| ((m.rating - 4.0) / 1.0).clamp(0.0, 1.0))
        .unwrap_or(0.5);

    // 同店合并：手里已经有这家店的单，这一单几乎不额外增加路程，还能一起取、
    // 一起送——真实骑手最主要的省钱手段，算法要主动往这上面靠。
    let same_shop = rider
        .held
        .iter()
        .filter_map(|h| world.order(h.order_id))
        .any(|o| o.merchant_id == order.merchant_id);
    let merge_bonus = if same_shop { 0.15 } else { 0.0 };

    // 背单风险：手持越多越危险；预计送达会超时再加罚
    let held_ratio = rider.held.len() as f32 / cfg.backorder.max_held.max(1) as f32;
    let will_be_late =
        world.elapsed_min + total_min as u64 > order.created_at_min + order.promised_minutes as u64;
    let backorder_risk = (held_ratio + if will_be_late { 0.5 } else { 0.0 }).min(1.0);

    let score =
        hourly * 0.4 + detour * 0.3 + distance_pref * 0.2 + merchant_rating * 0.1 + merge_bonus
            - backorder_risk * 0.25;

    let reason = format!(
        "时薪 ¥{:.1}/h · 顺路 {:.0}% · {:.1}km{}{}",
        takehome as f32 / total_min * 60.0 / 100.0,
        detour * 100.0,
        dist_km,
        if same_shop { " · 同店合并" } else { "" },
        if will_be_late { " · 预计超时" } else { "" }
    );

    Some(Assignment {
        rider: rider_id,
        order: order_id,
        score,
        reason,
        source: DecisionSource::Algorithm,
        plan,
    })
}

/// 骑手是否愿意接（时薪门槛）。
///
/// 门槛是**三层倍率叠加**的结果（《LLM协议设计》§5.5）：
/// `骑手基准 × 高峰系数 × LLM 期望单价档位倍率`。
/// 默认档位 `Normal` 的倍率是 1.0，所以 LLM 关掉时这里与旧实现逐字节等价。
pub fn accepts(world: &World, rider: &Rider, order: &Order) -> bool {
    if rider.kind != super::rider::RiderKind::Simulated {
        return true;
    }
    if !rider.strategy.accept_extra_tasks && !rider.held.is_empty() {
        return false;
    }
    if rider.strategy.max_orders_per_day > 0 && rider.completed >= rider.strategy.max_orders_per_day
    {
        return false;
    }
    // 已到「本人愿意背的上限」就不再接（LLM 可以把 cap 压到 0 = 今日不背单）
    if rider.held.len() as u32 >= rider.effective_max_held(&world.cfg) {
        return false;
    }
    let plan = plan_for(world, rider, order);
    let takehome = estimated_takehome_cents(world, rider, order);
    let hourly = takehome as f32 / plan.total_min().max(1) as f32 * 60.0;
    let threshold =
        rider.effective_threshold_cents_hour(world.cfg.is_peak(world.sim_minute), &world.cfg);
    hourly >= threshold
}

/// 正在某家店门口等餐、且还愿意多等一会儿的骑手：`(骑手, 那家店, 这炉还剩几分钟)`。
///
/// 这些人手上已经有单，**不算空闲**，所以进不了 `idle_riders`。但同一家店的新单
/// 顺手塞给他是几乎零成本的：人就在店门口，多拎一份同店的餐就走。这正是
/// 「骑手在店附近等派单，一起送」——也是 [`crate::sim::config::MergeRules`] 里
/// `wait_for_merge_min` 存在的理由。
///
/// 只认「当前腿卡在等餐段」的单：在路上的骑手不在店门口，合并不了。
pub fn merge_riders(world: &World) -> Vec<(RiderId, u32, u32)> {
    let mut out = Vec::new();
    for r in &world.riders {
        if !r.online
            || r.is_injured(world.elapsed_min)
            || r.is_frozen(world.elapsed_min)
            || r.held.len() as u32 >= r.effective_max_held(&world.cfg)
        {
            continue;
        }
        for h in &r.held {
            if h.phase != Phase::WaitingFood {
                continue;
            }
            if let Some(o) = world.order(h.order_id) {
                out.push((r.id, o.merchant_id, h.remaining_min));
            }
            break;
        }
    }
    out
}

/// 批量贪心指派：骑手与订单各只能出现一次。
///
/// 复杂度 O(骑手 × 订单 × log n)。**不用匈牙利算法**——10×5 规模最优解没意义，
/// 而且贪心的结果玩家能看懂为什么。
pub fn batch_decide(
    world: &World,
    new_orders: &[OrderId],
    idle_riders: &[RiderId],
) -> Vec<Assignment> {
    let merge_window = world.cfg.merge.wait_for_merge_min;
    let merging = if merge_window > 0 {
        merge_riders(world)
    } else {
        Vec::new()
    };

    let mut candidates: Vec<Assignment> = Vec::new();
    for oid in new_orders {
        let Some(o) = world.order(*oid) else {
            continue;
        };
        // 空闲骑手：什么都愿意接。
        for rid in idle_riders {
            let Some(r) = world.rider(*rid) else {
                continue;
            };
            if !accepts(world, r, o) {
                continue;
            }
            if let Some(c) = evaluate(world, *rid, *oid).filter(|c| c.score > 0.10) {
                candidates.push(c);
            }
        }
        // 在店等餐的骑手：只接**同一家店**的单，且新单出餐不能比这炉多等太久，
        // 否则就不是「凑一单」，而是让这炉餐白等一份慢餐。
        let o_prep = o
            .prep_minutes
            .unwrap_or_else(|| world.avg_prep_minutes(o.merchant_id));
        for (rid, mid, remaining) in &merging {
            if *mid != o.merchant_id {
                continue;
            }
            if o_prep > remaining + merge_window {
                continue;
            }
            let Some(r) = world.rider(*rid) else {
                continue;
            };
            if !accepts(world, r, o) {
                continue;
            }
            if let Some(c) = evaluate(world, *rid, *oid).filter(|c| c.score > 0.10) {
                candidates.push(c);
            }
        }
    }

    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.order.cmp(&b.order))
            .then(a.rider.cmp(&b.rider))
    });

    // 每骑手可派单数（默认 1，＝原来的一批一单）。调大就让骑手一次背多单。
    let per_rider_cap = world.cfg.merge.algorithm_max_per_rider.max(1) as usize;
    // 商家维度上限（0 = 不限）：一轮里给同一家店派几单。
    // 出单侧已经按同一个上限卡了「单店未完成单量」，这里再兜一道，
    // 免得某家店在算法轮次里刷爆候选集（性能）。
    let per_merchant_cap = world.cfg.merge.max_active_orders_per_merchant;

    let mut rider_load: HashMap<RiderId, usize> = HashMap::new();
    let mut merchant_load: HashMap<u32, u32> = HashMap::new();
    let mut used_orders = HashSet::new();
    let mut out = Vec::new();
    for c in candidates {
        if used_orders.contains(&c.order) {
            continue;
        }
        if rider_load.get(&c.rider).copied().unwrap_or(0) >= per_rider_cap {
            continue;
        }
        if per_merchant_cap > 0 {
            let mid = world.order(c.order).map(|o| o.merchant_id).unwrap_or(0);
            let load = merchant_load.get(&mid).copied().unwrap_or(0);
            if load >= per_merchant_cap {
                continue;
            }
            merchant_load.insert(mid, load + 1);
        }
        *rider_load.entry(c.rider).or_insert(0) += 1;
        used_orders.insert(c.order);
        out.push(c);
    }
    out
}

/// LLM 返回的原始计划（反序列化目标）。
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, Default)]
pub struct LlmPlan {
    pub assignments: Vec<LlmAssign>,
    /// 服务端补上的元信息，LLM 不产出。
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub latency_ms: u64,
    #[serde(default)]
    pub note: String,
    /// 决策 id（服务端分配，供控制台人工否决用）。`0` = 不可否决。
    #[serde(default)]
    pub decision_id: u64,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct LlmAssign {
    /// `r1` 形式。
    pub rider_id: String,
    pub order_id: OrderId,
    #[serde(default)]
    pub reason: String,
}

/// LLM 计划校验结果。
#[derive(Debug, Clone, Default)]
pub struct LlmVerdict {
    pub assignments: Vec<Assignment>,
    /// 被丢弃的条目数（骑手非法 / 订单不存在 / 重复）。
    pub dropped: usize,
}

/// 校验 LLM 输出。**不信任 LLM 的任何措辞。**
///
/// 丢弃规则：
/// - 骑手不在空闲列表 → 丢
/// - 订单不在待抢列表 → 丢
/// - 骑手重复 / 订单重复 → 丢
pub fn validate_llm_plan(
    world: &World,
    plan: &LlmPlan,
    idle: &[RiderId],
    pending: &[OrderId],
) -> LlmVerdict {
    let mut riders = HashSet::new();
    let mut orders = HashSet::new();
    let mut out = Vec::new();
    let mut dropped = 0usize;

    for a in &plan.assignments {
        let Some(rid) = crate::sim::parse_rider_key(&a.rider_id) else {
            dropped += 1;
            continue;
        };
        if !idle.contains(&rid) || !pending.contains(&a.order_id) {
            dropped += 1;
            continue;
        }
        if !riders.insert(rid) || !orders.insert(a.order_id) {
            dropped += 1;
            continue;
        }
        let Some(mut assign) = evaluate(world, rid, a.order_id) else {
            dropped += 1;
            continue;
        };
        assign.source = DecisionSource::Llm;
        assign.reason = if a.reason.trim().is_empty() {
            assign.reason
        } else {
            format!("[LLM] {}", a.reason.trim())
        };
        out.push(assign);
    }

    // 去重后仍可能有重复（理论上上面已拦），再兜一层。
    LlmVerdict {
        assignments: out,
        dropped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{
        config::{PEAK_PREP_FACTOR, SimConfig, TYPICAL_TO_SHOP_M},
        geo::LatLng,
        world::World,
    };

    fn world() -> World {
        let cfg = SimConfig {
            sim_rider_count: 4,
            ..Default::default()
        };
        World::new_for_test(cfg, 42)
    }

    /// 承诺时限必须盖住「一个骑手在典型接单距离上跑完这一单」要花的全部时间。
    ///
    /// 修复前这条是假的：承诺里既没有「骑手到店」那一段，出餐也只按商家均值算，
    /// 高峰时段（实际出餐翻倍）几乎每一单都注定超时。
    #[test]
    fn promise_covers_a_typical_peak_run() {
        let mut w = world();
        w.sim_minute = 18 * 60; // 晚高峰
        let ids = w.generate_order_batch(20);
        let rid = w.sim_rider_ids()[0];

        for id in ids {
            let order = w.order(id).expect("订单在").clone();

            // 把骑手放到「典型接单距离」上：承诺估的就是这一段。
            // 取 0.9 倍留点余量，免得在 ceil 边界上抖。
            let m = order.merchant_pos();
            let pos = LatLng::new(m.lat + (TYPICAL_TO_SHOP_M as f64 * 0.9) / 111_320.0, m.lng);
            w.rider_mut(rid).expect("骑手在").set_pos(pos);

            let rider = w.rider(rid).expect("骑手在").clone();
            let plan = plan_for(&w, &rider, &order);
            let prep_peak = (w.avg_prep_minutes(order.merchant_id) as f32 * PEAK_PREP_FACTOR)
                .round()
                .max(1.0) as u32;
            let to_customer = customer_leg_min(&w, &rider, &order);
            let need = plan.to_shop_min + prep_peak + to_customer;

            assert!(
                order.promised_minutes >= need,
                "订单 #{} 承诺 {} 分钟，实际要花 {need} 分钟（到店 {} + 高峰出餐 {} + 送端 {}）",
                order.id,
                order.promised_minutes,
                plan.to_shop_min,
                prep_peak,
                to_customer,
            );
        }
    }

    #[test]
    fn batch_decide_never_double_books() {
        let mut w = world();
        w.generate_order_batch(4);
        let pending: Vec<_> = w
            .orders
            .iter()
            .filter(|o| o.status == crate::sim::order::OrderStatus::Pending)
            .map(|o| o.id)
            .collect();
        let idle: Vec<_> = w
            .riders
            .iter()
            .filter(|r| r.is_idle(w.elapsed_min, &w.cfg))
            .map(|r| r.id)
            .collect();
        let res = batch_decide(&w, &pending, &idle);
        let riders: HashSet<_> = res.iter().map(|a| a.rider).collect();
        let orders: HashSet<_> = res.iter().map(|a| a.order).collect();
        assert_eq!(riders.len(), res.len());
        assert_eq!(orders.len(), res.len());
        assert!(res.len() <= pending.len().min(idle.len()));
    }

    #[test]
    fn llm_validation_drops_garbage() {
        let mut w = world();
        w.generate_order_batch(2);
        let pending: Vec<_> = w.orders.iter().map(|o| o.id).collect();
        let idle: Vec<_> = w.riders.iter().map(|r| r.id).collect();

        let plan = LlmPlan {
            assignments: vec![
                // 合法
                LlmAssign {
                    rider_id: format!("r{}", idle[0]),
                    order_id: pending[0],
                    reason: "顺路".into(),
                },
                // 重复骑手 → 丢
                LlmAssign {
                    rider_id: format!("r{}", idle[0]),
                    order_id: pending[1],
                    reason: "".into(),
                },
                // 不存在的骑手 → 丢
                LlmAssign {
                    rider_id: "r999".into(),
                    order_id: pending[1],
                    reason: "".into(),
                },
                // 不存在的订单 → 丢
                LlmAssign {
                    rider_id: format!("r{}", idle[1]),
                    order_id: 99999,
                    reason: "".into(),
                },
                // 非法格式 → 丢
                LlmAssign {
                    rider_id: "42".into(),
                    order_id: pending[1],
                    reason: "".into(),
                },
            ],
            ..Default::default()
        };
        let v = validate_llm_plan(&w, &plan, &idle, &pending);
        assert_eq!(v.assignments.len(), 1);
        assert_eq!(v.dropped, 4);
        assert_eq!(v.assignments[0].source, DecisionSource::Llm);
        assert!(v.assignments[0].reason.starts_with("[LLM]"));
    }

    #[test]
    fn evaluate_rejects_unknown_ids() {
        let w = world();
        assert!(evaluate(&w, 999, 1).is_none());
        assert!(evaluate(&w, 1, 999).is_none());
    }

    /// 单店未完成订单上限：出单不许把同一家店刷爆。
    ///
    /// 不加这道闸门，热门店（`rating^6` 加权）能把整条街的单都发出去，
    /// 一堆同店单分落在不同骑手上，谁也合不成。
    #[test]
    fn a_merchant_never_has_more_active_orders_than_the_cap() {
        let mut w = world();
        w.cfg.merge.max_active_orders_per_merchant = 2;
        w.generate_order_batch(40);

        let mut active: HashMap<u32, u32> = HashMap::new();
        for o in &w.orders {
            if o.status.is_active() {
                *active.entry(o.merchant_id).or_insert(0) += 1;
            }
        }
        assert!(!active.is_empty(), "40 次出单不该一单都出不来");
        for (mid, n) in active {
            assert!(n <= 2, "商家 {mid} 压了 {n} 单未完成，超过上限 2");
        }
    }

    /// 在店门口等餐的骑手会顺手接**同一家店**的新单（凑单窗口）；
    /// 别家店的单不接——人不在那家店门口，合并无从谈起。
    #[test]
    fn a_rider_waiting_for_food_merges_another_order_from_the_same_shop() {
        use crate::sim::{scheduler, world::CustomOrderSpec};

        let mut w = world();
        w.cfg.merge.wait_for_merge_min = 5;
        w.set_sim_rider_count(1);
        let rid = w.sim_rider_ids()[0];

        w.generate_order_batch(1);
        let first = w.pending_orders()[0];
        scheduler::assign_forced(&mut w, first, rid, crate::sim::order::AssignMode::Offer)
            .expect("强制指派该成功");

        let shop = w.order(first).unwrap().merchant_pos();
        let shop_name = w.order(first).unwrap().merchant_name.clone();
        // 摆到「在店等餐、这炉还要等 10 分钟」的状态上。
        {
            let r = w.rider_mut(rid).unwrap();
            let h = r.held_mut(first).unwrap();
            h.phase = Phase::WaitingFood;
            h.remaining_min = 10;
            h.leg_total_min = 10;
        }

        // 同一家店又来一单（报价给高一点，好越过接单意愿门槛）→ 该塞给他。
        let same = w.create_custom_order(CustomOrderSpec {
            merchant_name: shop_name,
            merchant_lat: shop.lat,
            merchant_lng: shop.lng,
            customer_name: None,
            customer_address: None,
            customer_lat: shop.lat + 0.01,
            customer_lng: shop.lng,
            fee_cents: Some(20_000),
            prep_seconds: None,
        });
        let res = batch_decide(&w, &[same], &[]);
        assert_eq!(res.len(), 1, "在店等餐的骑手该能顺手接同店单");
        assert_eq!((res[0].rider, res[0].order), (rid, same));

        // 别家的单不接：即使是同一个骑手、就在隔壁也不行。
        let other = w.create_custom_order(CustomOrderSpec {
            merchant_name: "别家店".into(),
            merchant_lat: shop.lat + 0.05,
            merchant_lng: shop.lng,
            customer_name: None,
            customer_address: None,
            customer_lat: shop.lat + 0.06,
            customer_lng: shop.lng,
            fee_cents: Some(20_000),
            prep_seconds: None,
        });
        let res = batch_decide(&w, &[other], &[]);
        assert!(res.is_empty(), "不同商家的单不该塞给在店等餐的骑手");
    }

    /// 算法每骑手上限可配：调到 2 时同一个骑手能一次吃下两单。
    #[test]
    fn algorithm_can_hand_two_orders_to_one_rider_when_the_cap_allows_it() {
        let mut w = world();
        w.set_sim_rider_count(1);
        w.cfg.merge.algorithm_max_per_rider = 2;
        w.generate_order_batch(4);

        let pending = w.pending_orders();
        let rid = w.sim_rider_ids()[0];
        let res = batch_decide(&w, &pending, &[rid]);
        assert_eq!(res.len(), 2, "上限 2 时该给这一个骑手派两单");
        assert!(res.iter().all(|a| a.rider == rid));
    }
}
