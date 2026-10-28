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
    rider::{Phase, Rider, RiderKind},
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
            source: self.source,
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
///
/// 凑单也是**替人做决定**，所以只有 AI 管得着的骑手在名单里：模拟骑手，
/// 以及 AutoPilot 托管的真人（`hosted`）。在线真人不算——他手上那单是玩家
/// 自己在跑，要不要顺手再拎一份得他自己点。
pub fn merge_riders(world: &World, hosted: &[RiderId]) -> Vec<(RiderId, u32, u32)> {
    let mut out = Vec::new();
    for r in &world.riders {
        if !r.online
            || !ai_may_assign(world, r.id, hosted)
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

/// AI 能不能替这个骑手派单？
///
/// 模拟骑手归 AI；真人只有交给 AutoPilot 托管时才算 AI 的人。**不能只看
/// `RiderKind::Human`**：连着 WS 的玩家也是 Human，AI 替他接单等于抢操作。
/// 「托没托管」是外圈（`human::host`）算出来的事实，一路用 `hosted` 传进来，
/// `sim/` 不自己猜。
fn ai_may_assign(world: &World, rid: RiderId, hosted: &[RiderId]) -> bool {
    match world.rider(rid) {
        Some(r) if r.kind == RiderKind::Human => hosted.contains(&rid),
        Some(_) => true,
        None => false,
    }
}

/// 批量贪心指派：骑手与订单各只能出现一次。
///
/// `idle_riders` 是空闲且归 AI 管的骑手，`hosted` 是当前 AutoPilot 托管的真人
/// （用来判定等餐中的真人能不能代管凑单）。
///
/// 复杂度 O(骑手 × 订单 × log n)。**不用匈牙利算法**——10×5 规模最优解没意义，
/// 而且贪心的结果玩家能看懂为什么。
pub fn batch_decide(
    world: &World,
    new_orders: &[OrderId],
    idle_riders: &[RiderId],
    hosted: &[RiderId],
) -> Vec<Assignment> {
    let merge_window = world.cfg.merge.wait_for_merge_min;
    let merging = if merge_window > 0 {
        merge_riders(world, hosted)
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
            // 名单本该只含 AI 管得着的骑手；再挡一道，免得调用方塞进在线真人。
            if !ai_may_assign(world, *rid, hosted) {
                continue;
            }
            // 拒单 / 观望记录：TTL 内别再把这一单塞回去。恢复评估靠时间或
            // 订单离开大厅，不是靠下一次贪心「猜」。
            if world.is_refused(*rid, *oid) || world.is_held(*rid) {
                continue;
            }
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
            // 观望窗口内连凑单也不参与；拒单只挡具体这一单。
            if world.is_refused(*rid, *oid) || world.is_held(*rid) {
                continue;
            }
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
    // 骑手剩余容量（`effective_max_held - 当前持单数`），按需算一次并缓存。
    let mut room_left: HashMap<RiderId, usize> = HashMap::new();
    let mut merchant_load: HashMap<u32, u32> = HashMap::new();
    let mut used_orders = HashSet::new();
    let mut out = Vec::new();
    for mut c in candidates {
        if used_orders.contains(&c.order) {
            continue;
        }
        // 本次调用能给他几单 = min(cap, 他此刻还剩多少容量)。
        //
        // `accepts` / `merge_riders` 判的是**调用前**的世界状态，`rider_load` 数的是
        // **本次调用**，两者各管一头：已持 2 单、上限 3、cap 2 时，两条候选本来就都
        // 过得了前一道校验，只看 `rider_load` 就会派出 4 单。剩余容量在调用期间不会
        // 变（世界只在 tick 里推进），所以按骑手算一次就够；已经派出去的条数由
        // `rider_load` 即时累加，后面的候选看到的自然是扣减后的余量。
        let room = *room_left.entry(c.rider).or_insert_with(|| {
            world.rider(c.rider).map_or(0, |r| {
                (r.effective_max_held(&world.cfg) as usize).saturating_sub(r.held.len())
            })
        });
        if rider_load.get(&c.rider).copied().unwrap_or(0) >= per_rider_cap.min(room) {
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
        // 托管真人（AutoPilot）：这一单是 AI 替他点的，不是他自己接的。
        // 逐条来源要如实反映这件事，否则控制台分不清「模拟骑手」与「代管真人」。
        // 判定写成「真人**且**在托管名单里」而不是单看种类：漏传名单时宁可
        // 记成 `Algorithm`，也不能把在线真人的算法派单谎称成「AI 托管」。
        // LLM 点名的仍记 `Llm`（见 `validate_llm_plan`），人工记 `Manual`。
        if world
            .rider(c.rider)
            .is_some_and(|r| r.kind == RiderKind::Human)
            && hosted.contains(&c.rider)
        {
            c.source = DecisionSource::AiHosted;
        }
        out.push(c);
    }
    out
}

/// LLM 返回的原始计划（反序列化目标）。
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, Default)]
pub struct LlmPlan {
    pub assignments: Vec<LlmAssign>,
    /// 「这一单我不接 / 暂时观望」。**不是空计划的替代品**：它们会被落地成
    /// 短期排除项，明确告诉本地算法「不要再把这一单 / 这一轮塞给他」。
    #[serde(default)]
    pub refusals: Vec<LlmRefusal>,
    /// 本轮被 LLM 明确处理过的骑手（接单或拒绝/观望）。本地补位只碰不在
    /// 这个名单里的人——这是「不得悄悄反向覆盖有效决策」的执行依据。
    #[serde(default)]
    pub covered: Vec<RiderId>,
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

/// 一条拒单 / 观望。`Pass` 按**骑手级**处理（订单字段只用于说明是哪一单让他观望）。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct LlmRefusal {
    pub rider: RiderId,
    pub order: OrderId,
    pub kind: crate::sim::world::RefusalKind,
    #[serde(default)]
    pub reason: String,
}

/// 被丢掉的是哪一类意图。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntentLayer {
    /// 接单 / 拒单 / 观望。
    Act,
    /// 价格门槛 / 背单上限 / 速度档。
    Policy,
    /// 站长指令。
    StationOp,
}

impl IntentLayer {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Act => "act",
            Self::Policy => "policy",
            Self::StationOp => "station_op",
        }
    }
}

/// 一条意图被丢弃的**原因**。
///
/// 只记总数是不够的：事后没法区分「模型不守规矩」「骑手这轮不可派」
/// 「订单已经被抢走」——三种情况该分别改提示词、改调度、改时序。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DropReason {
    /// `r` 不是 `r<数字>` 形式。
    BadRiderKey,
    /// 骑手 id 在世界里不存在。
    UnknownRider,
    /// 不归 AI 管：模拟骑手已下线 / 未托管给 AutoPilot 的真人。
    RiderNotManaged,
    /// 归 AI 管，但本轮不可派单（在途、背满、受伤、日单量上限）。
    RiderNotEligible,
    /// 订单 id 不存在。
    UnknownOrder,
    /// 订单已经不在大厅（被抢 / 已送达 / 已取消）。
    OrderNotPending,
    /// 同一骑手在一轮里给了多条同类意图。
    DuplicateRider,
    /// 同一订单被多个骑手认领 / 重复拒单。
    DuplicateOrder,
    /// 同一轮里既接单又拒单，语义冲突。
    ConflictingIntent,
    /// 该单仍在拒单有效期内。
    RefusedOrder,
    /// 骑手正处在观望窗口内。
    RiderHeld,
    /// `evaluate` 给不出可行分（例如到店就已经来不及）。
    NotScorable,
    /// 站长指令无法解析成四种合法形状之一。
    BadStationOp,
}

impl DropReason {
    /// 稳定的小写码：事件、日志、测试断言都用它，别用 `Debug`。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadRiderKey => "bad_rider_key",
            Self::UnknownRider => "unknown_rider",
            Self::RiderNotManaged => "rider_not_managed",
            Self::RiderNotEligible => "rider_not_eligible",
            Self::UnknownOrder => "unknown_order",
            Self::OrderNotPending => "order_not_pending",
            Self::DuplicateRider => "duplicate_rider",
            Self::DuplicateOrder => "duplicate_order",
            Self::ConflictingIntent => "conflicting_intent",
            Self::RefusedOrder => "refused_order",
            Self::RiderHeld => "rider_held",
            Self::NotScorable => "not_scorable",
            Self::BadStationOp => "bad_station_op",
        }
    }
}

/// 被丢弃的一条意图。骑手 / 订单可能缺一项：key 本身解析不出来时就没有 id。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedIntent {
    pub layer: IntentLayer,
    pub rider: Option<RiderId>,
    pub order: Option<OrderId>,
    pub reason: DropReason,
}

impl DroppedIntent {
    pub fn new(
        layer: IntentLayer,
        rider: Option<RiderId>,
        order: Option<OrderId>,
        reason: DropReason,
    ) -> Self {
        Self {
            layer,
            rider,
            order,
            reason,
        }
    }

    /// `act r7/o42 refused_order` 形式，给日志和 note 用。
    pub fn brief(&self) -> String {
        let rider = self.rider.map(crate::sim::rider_key).unwrap_or_default();
        let order = self.order.map(|o| format!("o{o}")).unwrap_or_default();
        format!(
            "{} {}/{} {}",
            self.layer.as_str(),
            if rider.is_empty() { "-" } else { &rider },
            if order.is_empty() { "-" } else { &order },
            self.reason.as_str()
        )
    }
}

/// 普通派单是否撞上「短期排除项」。`None` = 可以派。
///
/// **只拦普通派单**：`rescue_nearest` 有意不查这个——那是「不允许流单」的
/// 最后一手，为了不让某一单死在拒单记录里，必须能越过个人意愿。
pub fn refusal_block(world: &World, rider: RiderId, order: OrderId) -> Option<DropReason> {
    if world.is_held(rider) {
        return Some(DropReason::RiderHeld);
    }
    match world.was_refused(rider, order) {
        Some(super::world::RefusalKind::Reject) => Some(DropReason::RefusedOrder),
        // `Pass` 是骑手级观望，`is_held` 已经覆盖；这里兜防御性分支。
        Some(super::world::RefusalKind::Pass) => Some(DropReason::RiderHeld),
        None => None,
    }
}

/// LLM 计划校验结果。
#[derive(Debug, Clone, Default)]
pub struct LlmVerdict {
    pub assignments: Vec<Assignment>,
    /// 通过引用校验的拒单 / 观望。
    pub refusals: Vec<LlmRefusal>,
    /// 本轮**有效决策覆盖到**的骑手（接单 + 拒单/观望）。
    pub covered: Vec<RiderId>,
    /// 被丢弃的每一条意图及原因。**不再只有计数**。
    pub drops: Vec<DroppedIntent>,
}

impl LlmVerdict {
    pub fn dropped(&self) -> usize {
        self.drops.len()
    }
}

/// 把丢弃原因聚成 `（refused_order×2, rider_held×1）`；没有丢弃时返回空串。
///
/// 顺序按码字典序（`BTreeMap`），同输入永远同一句，方便 diff 与断言。
pub fn summarize_drops(drops: &[DroppedIntent]) -> String {
    if drops.is_empty() {
        return String::new();
    }
    let mut counts = std::collections::BTreeMap::new();
    for d in drops {
        *counts.entry(d.reason.as_str()).or_insert(0usize) += 1;
    }
    let detail = counts
        .iter()
        .map(|(k, n)| format!("{k}×{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("（{detail}）")
}

/// 校验 LLM 输出。**不信任 LLM 的任何措辞。**
///
/// 丢弃规则（每条都记 [`DroppedIntent`]，不静默丢）：
/// - 骑手不在本轮名单 → 丢（`rider_not_eligible`）
/// - 订单不在待抢列表 → 丢（`order_not_pending`）
/// - 骑手身上已无空位（`held.len() >= effective_max_held`，含 `cap = 0`）
///   → 丢（`rider_not_eligible`）
/// - 骑手重复 / 订单重复 → 丢
/// - 该单仍在拒单 TTL 内、或骑手在观望窗口内 → 丢（`refused_order` / `rider_held`）
///
/// 拒单 / 观望约束必须在这里也查一遍——`batch_decide` 里的过滤只管本地补位，
/// 覆盖不到 LLM 直接点名派单这条路径。
pub fn validate_llm_plan(
    world: &World,
    plan: &LlmPlan,
    idle: &[RiderId],
    pending: &[OrderId],
) -> LlmVerdict {
    let mut riders = HashSet::new();
    let mut orders = HashSet::new();
    let mut out = Vec::new();
    let mut drops: Vec<DroppedIntent> = Vec::new();

    for a in &plan.assignments {
        let Some(rid) = crate::sim::parse_rider_key(&a.rider_id) else {
            drops.push(DroppedIntent::new(
                IntentLayer::Act,
                None,
                Some(a.order_id),
                DropReason::BadRiderKey,
            ));
            continue;
        };
        let act = |r: Option<RiderId>, reason: DropReason| {
            DroppedIntent::new(IntentLayer::Act, r, Some(a.order_id), reason)
        };
        if world.rider(rid).is_none() {
            drops.push(act(Some(rid), DropReason::UnknownRider));
            continue;
        }
        if !pending.contains(&a.order_id) {
            drops.push(act(Some(rid), DropReason::OrderNotPending));
            continue;
        }
        if !idle.contains(&rid) {
            drops.push(act(Some(rid), DropReason::RiderNotEligible));
            continue;
        }
        // 容量：`idle` 只回答「这一轮能不能点他」，不回答「他身上还有没有位置」。
        // 名单按 `is_free` 收人，而 `is_free` 只看「手上有没有单」——`cap_override = 0`
        // （站长下的「只把手上的送完，不再接新单」）的骑手只要手上是空的，照样进名单、
        // 照样被点名。少这一道，LLM 直派就成了能绕开 `cap = 0` 的入口：
        // `assign_forced` / `reassign` / `claim_extra` / `rescue_nearest` /
        // `batch_decide` 都按 `effective_max_held` 拦着。
        //
        // 复用 `effective_max_held`：策略覆盖、配置上限、二者取小都已经在里面，
        // 这里不重算。判定放在 `riders.insert` 之前——背满的人每一条 claim 都该报
        // 同一个原因码，不能被前一条改写成 `duplicate_rider`。
        let has_room = world
            .rider(rid)
            .is_some_and(|r| (r.effective_max_held(&world.cfg) as usize) > r.held.len());
        if !has_room {
            drops.push(act(Some(rid), DropReason::RiderNotEligible));
            continue;
        }
        // 短期排除项先查：被挡下的意图不该占掉「这一轮这个骑手已被 LLM 处理」
        // 的位置，否则后面一条真正可行的单就永远轮不到他。
        if let Some(reason) = refusal_block(world, rid, a.order_id) {
            drops.push(act(Some(rid), reason));
            continue;
        }
        if !riders.insert(rid) {
            drops.push(act(Some(rid), DropReason::DuplicateRider));
            continue;
        }
        if !orders.insert(a.order_id) {
            drops.push(act(Some(rid), DropReason::DuplicateOrder));
            continue;
        }
        let Some(mut assign) = evaluate(world, rid, a.order_id) else {
            drops.push(act(Some(rid), DropReason::NotScorable));
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

    // 拒单 / 观望：只接受「本轮名单里、没在接单、订单还在大厅」的那些。
    //
    // 一条也不通过时由调用方决定怎么兜底；这里不替它猜。
    let mut refusals = Vec::new();
    let mut pairs = HashSet::new();
    for rf in &plan.refusals {
        let refuse = |reason: DropReason| {
            DroppedIntent::new(IntentLayer::Act, Some(rf.rider), Some(rf.order), reason)
        };
        if !idle.contains(&rf.rider) {
            drops.push(refuse(DropReason::RiderNotEligible));
            continue;
        }
        if riders.contains(&rf.rider) {
            drops.push(refuse(DropReason::ConflictingIntent));
            continue;
        }
        if !pending.contains(&rf.order) {
            drops.push(refuse(DropReason::OrderNotPending));
            continue;
        }
        if !pairs.insert((rf.rider, rf.order)) {
            drops.push(refuse(DropReason::DuplicateOrder));
            continue;
        }
        refusals.push(rf.clone());
    }

    // 覆盖范围 = 接单的人 + 明确拒单/观望的人。**只认校验通过的条目**：
    // 被丢掉的决策等于没有发生过，骑手应当回到「可被本地补位」。
    let mut covered: Vec<RiderId> = riders.iter().copied().collect();
    covered.extend(refusals.iter().map(|r| r.rider));
    covered.sort_unstable();
    covered.dedup();

    LlmVerdict {
        assignments: out,
        refusals,
        covered,
        drops,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{
        config::{PEAK_PREP_FACTOR, SimConfig, TYPICAL_TO_SHOP_M},
        geo::LatLng,
        rider::SpeedMode,
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

    /// LLM 的速度覆盖必须真的缩短预估腿时长，而不只是提示词里好看。
    /// 行程时间在**腿开始时**估算，所以这里比较的是同一位置的 `plan_for`。
    #[test]
    fn speed_policy_shortens_the_estimated_legs() {
        let mut w = world();
        let rid = w.sim_rider_ids()[0];
        w.rider_mut(rid).unwrap().speed_mode = SpeedMode::Compliant;
        let oid = w.generate_order_batch(1)[0];

        let before = plan_for(
            &w,
            w.rider(rid).expect("骑手在"),
            w.order(oid).expect("订单在"),
        );

        w.rider_mut(rid).unwrap().policy_override.mode_override = Some(SpeedMode::Tolerant);
        let rider = w.rider(rid).expect("骑手在");
        let after = plan_for(&w, rider, w.order(oid).expect("订单在"));

        assert!(
            after.total_min() < before.total_min(),
            "覆盖生效后腿时长应变短：before={} after={}",
            before.total_min(),
            after.total_min()
        );
        assert_eq!(
            w.rider(rid).unwrap().speed_mode,
            SpeedMode::Compliant,
            "覆盖不得写穿个人档位"
        );
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
        let res = batch_decide(&w, &pending, &idle, &[]);
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
        assert_eq!(v.dropped(), 4);
        assert_eq!(v.assignments[0].source, DecisionSource::Llm);
        assert!(v.assignments[0].reason.starts_with("[LLM]"));
    }

    /// 拒单记录必须挡住 **LLM 直接点名**这条路径，不能只挡本地补位。
    ///
    /// 修复前：`batch_decide` 里查 `is_refused`，`validate_llm_plan` 不查——
    /// 于是模型只要再写一次 `r7/o3`，刚被拒掉的单就又回到他手上。
    #[test]
    fn a_rejected_order_is_not_handed_back_by_the_plan() {
        let mut w = world();
        w.generate_order_batch(2);
        let pending: Vec<_> = w.orders.iter().map(|o| o.id).collect();
        let idle: Vec<_> = w.riders.iter().map(|r| r.id).collect();
        let (rid, oid) = (idle[0], pending[0]);

        w.record_refusals(&[LlmRefusal {
            rider: rid,
            order: oid,
            kind: crate::sim::world::RefusalKind::Reject,
            reason: "太远".into(),
        }]);

        let plan = LlmPlan {
            assignments: vec![LlmAssign {
                rider_id: format!("r{rid}"),
                order_id: oid,
                reason: "又觉得顺路了".into(),
            }],
            ..Default::default()
        };
        let v = validate_llm_plan(&w, &plan, &idle, &pending);
        assert!(v.assignments.is_empty(), "拒单还在有效期，不能再派回去");
        assert_eq!(v.drops.len(), 1);
        assert_eq!(v.drops[0].reason, DropReason::RefusedOrder);
        assert_eq!(v.drops[0].layer, IntentLayer::Act);
        assert_eq!(v.drops[0].rider, Some(rid));
        assert_eq!(v.drops[0].order, Some(oid));
        assert!(
            v.covered.is_empty(),
            "被挡掉的意图不算覆盖，必须让算法有机会补位"
        );
    }

    /// 观望是骑手级窗口：窗口内 LLM 也不能点他，理由要报 `rider_held`。
    #[test]
    fn a_held_rider_is_not_handed_an_order_by_the_plan() {
        let mut w = world();
        w.generate_order_batch(2);
        let pending: Vec<_> = w.orders.iter().map(|o| o.id).collect();
        let idle: Vec<_> = w.riders.iter().map(|r| r.id).collect();
        let (rid, oid) = (idle[0], pending[1]);

        w.record_refusals(&[LlmRefusal {
            rider: rid,
            order: oid,
            kind: crate::sim::world::RefusalKind::Pass,
            reason: "等更值的单".into(),
        }]);

        let plan = LlmPlan {
            assignments: vec![LlmAssign {
                rider_id: format!("r{rid}"),
                order_id: oid,
                reason: "".into(),
            }],
            ..Default::default()
        };
        let v = validate_llm_plan(&w, &plan, &idle, &pending);
        assert!(v.assignments.is_empty(), "观望窗口内不许派单");
        assert_eq!(v.drops[0].reason, DropReason::RiderHeld);
    }

    /// 记录失效后必须能重新评估：拒一次不能等于永久黑名单。
    #[test]
    fn an_expired_refusal_lets_the_plan_through_again() {
        let mut w = world();
        w.generate_order_batch(2);
        let pending: Vec<_> = w.orders.iter().map(|o| o.id).collect();
        let idle: Vec<_> = w.riders.iter().map(|r| r.id).collect();
        let (rid, oid) = (idle[0], pending[0]);
        let ttl = w.cfg.llm_refusal_ttl_min as u64;

        w.record_refusals(&[LlmRefusal {
            rider: rid,
            order: oid,
            kind: crate::sim::world::RefusalKind::Reject,
            reason: "太远".into(),
        }]);
        w.elapsed_min = ttl;

        let plan = LlmPlan {
            assignments: vec![LlmAssign {
                rider_id: format!("r{rid}"),
                order_id: oid,
                reason: "窗口过了".into(),
            }],
            ..Default::default()
        };
        let v = validate_llm_plan(&w, &plan, &idle, &pending);
        assert_eq!(v.assignments.len(), 1, "TTL 到期后可以重新评估");
        assert!(v.drops.is_empty());
        assert_eq!(v.assignments[0].source, DecisionSource::Llm);
    }

    /// 混合计划：被挡的只丢那一条，同轮其它派单照常生效。
    #[test]
    fn a_blocked_claim_does_not_sink_the_rest_of_the_plan() {
        let mut w = world();
        w.generate_order_batch(3);
        let pending: Vec<_> = w.orders.iter().map(|o| o.id).collect();
        let idle: Vec<_> = w.riders.iter().map(|r| r.id).collect();
        let (r0, r1) = (idle[0], idle[1]);

        w.record_refusals(&[LlmRefusal {
            rider: r0,
            order: pending[0],
            kind: crate::sim::world::RefusalKind::Reject,
            reason: "不划算".into(),
        }]);

        let plan = LlmPlan {
            assignments: vec![
                LlmAssign {
                    rider_id: format!("r{r0}"),
                    order_id: pending[0],
                    reason: "".into(),
                },
                LlmAssign {
                    rider_id: format!("r{r1}"),
                    order_id: pending[1],
                    reason: "".into(),
                },
            ],
            ..Default::default()
        };
        let v = validate_llm_plan(&w, &plan, &idle, &pending);
        assert_eq!(v.assignments.len(), 1, "有效的那条不能被连坐");
        assert_eq!(v.assignments[0].rider, r1);
        assert_eq!(v.drops.len(), 1);
        assert_eq!(v.drops[0].reason, DropReason::RefusedOrder);
    }

    /// 托管真人的算法补位要标成 `AiHosted`：这单是 AI 替他点的。
    #[test]
    fn an_autopilot_human_is_marked_as_ai_hosted() {
        let mut w = world();
        w.generate_order_batch(2);
        let pending = w.pending_orders();
        let hid = w.spawn_human_rider("小赵");
        // 掉线被 AutoPilot 接管：身体还在线（算法视角），但没有真人操作。
        w.rider_mut(hid).expect("在").online = true;

        let res = batch_decide(&w, &pending, &[hid], &[hid]);
        assert!(!res.is_empty(), "托管真人应该能被兜底派单");
        assert!(res.iter().all(|a| a.rider == hid));
        assert_eq!(
            res[0].source,
            DecisionSource::AiHosted,
            "算法替托管真人做的决策必须标成 AI 托管，不能混进模拟骑手的 Algorithm"
        );
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
        let res = batch_decide(&w, &[same], &[], &[]);
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
        let res = batch_decide(&w, &[other], &[], &[]);
        assert!(res.is_empty(), "不同商家的单不该塞给在店等餐的骑手");
    }

    /// 在线真人在店里等餐时，算法**不得**替他凑单。
    ///
    /// 凑单通道以前只看 `online`，不看这具身体归谁管：连着 WS 的玩家照样会被
    /// 塞一单，还会被标成 `AiHosted`（「AI 托管」），两头都错。
    #[test]
    fn a_live_human_is_never_handed_a_merge_order() {
        use crate::sim::order::AssignMode;
        use crate::sim::{scheduler, world::CustomOrderSpec};

        let mut w = world();
        w.cfg.merge.wait_for_merge_min = 5;
        let hid = w.spawn_human_rider("小赵");
        w.rider_mut(hid).unwrap().online = true;

        w.generate_order_batch(1);
        let first = w.pending_orders()[0];
        scheduler::assign_forced(&mut w, first, hid, AssignMode::Offer).expect("强制指派该成功");
        let shop = w.order(first).unwrap().merchant_pos();
        let shop_name = w.order(first).unwrap().merchant_name.clone();
        {
            let r = w.rider_mut(hid).unwrap();
            let h = r.held_mut(first).unwrap();
            h.phase = Phase::WaitingFood;
            h.remaining_min = 10;
            h.leg_total_min = 10;
        }

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
        let res = batch_decide(&w, &[same], &[], &[]);
        assert!(
            res.is_empty(),
            "在线真人的单该由他自己接，算法不能凑单（实际派了 {} 条）",
            res.len()
        );
    }

    /// 反过来：AutoPilot 托管的真人是 AI 的人，凑单照旧，来源记 `AiHosted`。
    #[test]
    fn a_hosted_human_still_gets_merge_orders_from_the_ai() {
        use crate::sim::order::AssignMode;
        use crate::sim::{scheduler, world::CustomOrderSpec};

        let mut w = world();
        w.cfg.merge.wait_for_merge_min = 5;
        let hid = w.spawn_human_rider("小赵");
        // 托管真人通常还留着掉线前的 online 标记——托管与否不看它，看外圈名单。
        w.rider_mut(hid).unwrap().online = true;

        w.generate_order_batch(1);
        let first = w.pending_orders()[0];
        scheduler::assign_forced(&mut w, first, hid, AssignMode::Offer).expect("强制指派该成功");
        let shop = w.order(first).unwrap().merchant_pos();
        let shop_name = w.order(first).unwrap().merchant_name.clone();
        {
            let r = w.rider_mut(hid).unwrap();
            let h = r.held_mut(first).unwrap();
            h.phase = Phase::WaitingFood;
            h.remaining_min = 10;
            h.leg_total_min = 10;
        }

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
        let res = batch_decide(&w, &[same], &[], &[hid]);
        assert_eq!(res.len(), 1, "托管真人的凑单该由 AI 代管");
        assert_eq!((res[0].rider, res[0].order), (hid, same));
        assert_eq!(res[0].source, DecisionSource::AiHosted);
    }

    /// 观望是**骑手级**的：窗口内连凑单也不参与，人就站在同店门口也不塞。
    #[test]
    fn a_held_rider_is_not_handed_a_merge_order() {
        use crate::sim::order::AssignMode;
        use crate::sim::{scheduler, world::CustomOrderSpec};

        let mut w = world();
        w.cfg.merge.wait_for_merge_min = 5;
        w.set_sim_rider_count(1);
        let rid = w.sim_rider_ids()[0];

        w.generate_order_batch(1);
        let first = w.pending_orders()[0];
        scheduler::assign_forced(&mut w, first, rid, AssignMode::Offer).expect("强制指派该成功");
        let shop = w.order(first).unwrap().merchant_pos();
        let shop_name = w.order(first).unwrap().merchant_name.clone();
        {
            let r = w.rider_mut(rid).unwrap();
            let h = r.held_mut(first).unwrap();
            h.phase = Phase::WaitingFood;
            h.remaining_min = 10;
            h.leg_total_min = 10;
        }

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
        // 先确认这个场面本来会派出去，再叠加观望——否则「空」说明不了任何事。
        assert_eq!(batch_decide(&w, &[same], &[], &[]).len(), 1);

        w.record_refusals(&[LlmRefusal {
            rider: rid,
            order: first,
            kind: crate::sim::world::RefusalKind::Pass,
            reason: "手上有单，先送完".into(),
        }]);
        assert!(
            batch_decide(&w, &[same], &[], &[]).is_empty(),
            "观望窗口内不该给他凑单"
        );
    }

    /// 造一个「手上已有 `held` 单、正卡在同一家店门口等餐」的骑手。
    ///
    /// 返回那家店的 `(店名, 坐标)`，方便接着往同一家店下单，制造凑单场面。
    /// 用 `assign_forced` 塞单：它绕开抢单窗口与意愿评估，且会立刻改世界状态，
    /// 所以这里拿到的 `held` 和真实跑过一轮之后是一回事。
    fn rider_waiting_at_shop(w: &mut World, rid: RiderId, held: usize) -> (String, LatLng) {
        use crate::sim::order::AssignMode;
        use crate::sim::{scheduler, world::CustomOrderSpec};

        assert!(held >= 1, "至少得先有一单，否则不是「等餐中」");
        w.generate_order_batch(1);
        let first = w.pending_orders()[0];
        scheduler::assign_forced(w, first, rid, AssignMode::Offer).expect("第一单该塞得进");
        let (name, pos) = {
            let o = w.order(first).expect("订单在");
            (o.merchant_name.clone(), o.merchant_pos())
        };

        let mut mine = vec![first];
        for i in 1..held {
            let more = w.create_custom_order(CustomOrderSpec {
                merchant_name: name.clone(),
                merchant_lat: pos.lat,
                merchant_lng: pos.lng,
                customer_name: None,
                customer_address: None,
                customer_lat: pos.lat + 0.001 * (i as f64 + 1.0),
                customer_lng: pos.lng,
                fee_cents: Some(20_000),
                prep_seconds: None,
            });
            scheduler::assign_forced(w, more, rid, AssignMode::Offer).expect("同店再塞一单该成功");
            mine.push(more);
        }

        // 全部卡在等餐段：这是凑单通道唯一认的状态。
        for oid in mine {
            let h = w
                .rider_mut(rid)
                .expect("骑手在")
                .held_mut(oid)
                .expect("单在他手上");
            h.phase = Phase::WaitingFood;
            h.remaining_min = 30;
            h.leg_total_min = 30;
        }
        (name, pos)
    }

    /// 往同一家店再挂 `n` 单（报价给足，保证越过接单门槛）。
    fn shop_orders(w: &mut World, shop: &(String, LatLng), n: usize) -> Vec<OrderId> {
        use crate::sim::world::CustomOrderSpec;
        (0..n)
            .map(|i| {
                w.create_custom_order(CustomOrderSpec {
                    merchant_name: shop.0.clone(),
                    merchant_lat: shop.1.lat,
                    merchant_lng: shop.1.lng,
                    customer_name: None,
                    customer_address: None,
                    customer_lat: shop.1.lat + 0.010 + 0.001 * i as f64,
                    customer_lng: shop.1.lng,
                    fee_cents: Some(20_000),
                    prep_seconds: None,
                })
            })
            .collect()
    }

    /// 把决定落地，用**真实的**持单数收尾（不是断言内部计数）。
    fn apply_all(w: &mut World, assignments: &[Assignment]) {
        for a in assignments {
            crate::sim::scheduler::apply_assignment(w, a);
        }
    }

    /// 剩余容量 1、cap 2：一轮只能再加 1 单，落地后正好顶到 `max_held`。
    ///
    /// 修复前这里会派 2 单：`merge_riders` 判的是调用前的「2 < 3」，
    /// `rider_load` 只数本次调用（2 ≤ cap），谁都没算「只剩 1 个位置」。
    #[test]
    fn remaining_room_below_the_cap_limits_the_call() {
        let mut w = world();
        w.set_sim_rider_count(1);
        w.cfg.merge.wait_for_merge_min = 5;
        w.cfg.merge.algorithm_max_per_rider = 2;
        // 商家维度不限，免得「单店未完成单量」把候选数量先卡住，掩盖骑手侧的容量。
        w.cfg.merge.max_active_orders_per_merchant = 0;
        let rid = w.sim_rider_ids()[0];

        let shop = rider_waiting_at_shop(&mut w, rid, 2);
        let cands = shop_orders(&mut w, &shop, 3);

        let got = batch_decide(&w, &cands, &[], &[]);
        assert_eq!(got.len(), 1, "只剩 1 个位置，一轮只能加 1 单：{got:?}");
        assert_eq!(got[0].rider, rid);

        apply_all(&mut w, &got);
        assert_eq!(
            w.rider(rid).expect("骑手在").held.len(),
            3,
            "实际持单数不得越过 max_held"
        );
    }

    /// 剩余容量（4）大于 cap（2）：cap 仍然是本轮的上限。
    ///
    /// 与上一条合起来说明「cap 和剩余容量是两个约束，取小」：不是谁替代谁。
    #[test]
    fn the_cap_still_binds_when_there_is_plenty_of_room() {
        let mut w = world();
        w.set_sim_rider_count(1);
        w.cfg.merge.wait_for_merge_min = 5;
        w.cfg.backorder.max_held = 5;
        w.cfg.merge.algorithm_max_per_rider = 2;
        w.cfg.merge.max_active_orders_per_merchant = 0;
        let rid = w.sim_rider_ids()[0];

        let shop = rider_waiting_at_shop(&mut w, rid, 1);
        let cands = shop_orders(&mut w, &shop, 4);

        let got = batch_decide(&w, &cands, &[], &[]);
        assert_eq!(got.len(), 2, "还剩 4 个位置，但 cap 是 2：{got:?}");
        assert!(got.iter().all(|a| a.rider == rid));

        apply_all(&mut w, &got);
        assert_eq!(w.rider(rid).expect("骑手在").held.len(), 3);
    }

    /// 剩余容量 0：一个位置都没有时，哪怕名单把骑手塞进来也不许派。
    ///
    /// 真人的 `accepts` 直接返回 true（不看背单上限），凑单又只被 `merge_riders`
    /// 在调用前挡过一次，所以这里是「调用内再兜一道」的关键路径。
    #[test]
    fn a_rider_at_max_held_is_never_handed_another_order() {
        let mut w = world();
        w.cfg.merge.wait_for_merge_min = 5;
        w.cfg.merge.algorithm_max_per_rider = 2;
        let hid = w.spawn_human_rider("阿甘");
        w.rider_mut(hid).expect("骑手在").online = true;

        let shop = rider_waiting_at_shop(&mut w, hid, 3);
        assert_eq!(
            w.rider(hid).expect("骑手在").held.len(),
            w.cfg.backorder.max_held as usize,
            "前提：这个人已经背满"
        );
        let cands = shop_orders(&mut w, &shop, 2);

        // 两个名单都故意把他塞进去：容量守卫必须自己挡住。
        let got = batch_decide(&w, &cands, &[hid], &[hid]);
        assert!(got.is_empty(), "背满了还派单就是越界：{got:?}");
        assert_eq!(
            w.rider(hid).expect("骑手在").held.len(),
            w.cfg.backorder.max_held as usize,
            "持单数不变"
        );
    }

    /// 多笔候选：前面成功分配吃掉的容量，后面的候选必须立刻看到。
    ///
    /// 剩 2 个位置、cap 3、4 笔候选 → 只能出 2 单。若只看 cap（3）就会派出 5 单。
    #[test]
    fn later_candidates_see_the_room_taken_by_earlier_ones() {
        let mut w = world();
        w.set_sim_rider_count(1);
        w.cfg.merge.wait_for_merge_min = 5;
        w.cfg.backorder.max_held = 4;
        w.cfg.merge.algorithm_max_per_rider = 3;
        // 商家维度不限：否则默认的单店 2 单会提前挡下第 3 笔，测不到骑手容量。
        w.cfg.merge.max_active_orders_per_merchant = 0;
        let rid = w.sim_rider_ids()[0];

        let shop = rider_waiting_at_shop(&mut w, rid, 2);
        let cands = shop_orders(&mut w, &shop, 4);

        let got = batch_decide(&w, &cands, &[], &[]);
        assert_eq!(got.len(), 2, "剩 2 个位置，第 3 笔必须被挡下：{got:?}");
        let mut orders: Vec<OrderId> = got.iter().map(|a| a.order).collect();
        orders.sort_unstable();
        orders.dedup();
        assert_eq!(orders.len(), 2, "同一单不许重复派");

        apply_all(&mut w, &got);
        assert_eq!(w.rider(rid).expect("骑手在").held.len(), 4);
    }

    /// 正常凑单不回归：还有位置时，同店订单照旧能并进来。
    #[test]
    fn a_merge_still_goes_through_when_there_is_room() {
        let mut w = world();
        w.set_sim_rider_count(1);
        w.cfg.merge.wait_for_merge_min = 5;
        let rid = w.sim_rider_ids()[0];

        let shop = rider_waiting_at_shop(&mut w, rid, 1);
        let cands = shop_orders(&mut w, &shop, 2);

        let got = batch_decide(&w, &cands, &[], &[]);
        assert_eq!(got.len(), 1, "cap 默认 1，凑单照旧成立：{got:?}");
        assert_eq!(
            (got[0].rider, got[0].source),
            (rid, DecisionSource::Algorithm)
        );

        apply_all(&mut w, &got);
        assert_eq!(w.rider(rid).expect("骑手在").held.len(), 2);
    }

    /// 一个人满了不影响别人：容量是按骑手各算各的。
    #[test]
    fn a_rider_at_capacity_does_not_block_the_others() {
        let mut w = world();
        w.cfg.merge.wait_for_merge_min = 5;
        w.cfg.merge.max_active_orders_per_merchant = 0;
        w.set_sim_rider_count(2);
        let ids = w.sim_rider_ids();
        let (busy, free) = (ids[0], ids[1]);

        // busy 已持 2 单（剩 1），free 空手（剩 3）。
        let shop = rider_waiting_at_shop(&mut w, busy, 2);
        let cands = shop_orders(&mut w, &shop, 3);

        let got = batch_decide(&w, &cands, &[free], &[]);
        let got_busy: Vec<_> = got.iter().filter(|a| a.rider == busy).collect();
        let got_free: Vec<_> = got.iter().filter(|a| a.rider == free).collect();
        assert_eq!(got_busy.len(), 1, "busy 只剩 1 个位置：{got:?}");
        assert_eq!(got_free.len(), 1, "free 该拿到另一单：{got:?}");
        assert_ne!(got_busy[0].order, got_free[0].order);

        apply_all(&mut w, &got);
        assert_eq!(w.rider(busy).expect("骑手在").held.len(), 3);
        assert_eq!(w.rider(free).expect("骑手在").held.len(), 1);
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
        let res = batch_decide(&w, &pending, &[rid], &[]);
        assert_eq!(res.len(), 2, "上限 2 时该给这一个骑手派两单");
        assert!(res.iter().all(|a| a.rider == rid));
    }

    /// 只带接单意图的 LLM 计划（其余字段留默认）。
    fn claim_plan(claims: &[(RiderId, OrderId)]) -> LlmPlan {
        LlmPlan {
            assignments: claims
                .iter()
                .map(|(r, o)| LlmAssign {
                    rider_id: crate::sim::rider_key(*r),
                    order_id: *o,
                    reason: "测试".into(),
                })
                .collect(),
            model: "test".into(),
            ..Default::default()
        }
    }

    /// 生产路径上真实的名册（`tick::ai_roster` 的输出）。
    ///
    /// 用它而不是手搓 `vec![rid]`，是为了让测试走**同一条**名单：漏点正在于
    /// `ai_roster` 只按 `is_free`（手上有单没有）收人，不问「还有没有位置」。
    fn roster(w: &World) -> Vec<RiderId> {
        crate::sim::tick::ai_roster(w, &crate::sim::tick::TickInput::default())
    }

    /// 按 `cap_rider` 的写法下一条背单上限：**必须同时盖时间戳**。
    ///
    /// 只写 `cap_override` 不给 `set_at_elapsed`，下一 tick 会被 `expire_policies`
    /// 当成「缺字段的老存档」清掉（`world.rs:710-716`），测试就会测到另一件事。
    fn set_cap(w: &mut World, rid: RiderId, cap: u32) {
        let set_at_min = w.sim_minute as u64;
        let set_at_elapsed = w.elapsed_min;
        if let Some(r) = w.rider_mut(rid) {
            r.policy_override.cap_override = Some(cap);
            r.policy_override.set_at_min = set_at_min;
            r.policy_override.set_at_elapsed = Some(set_at_elapsed);
        }
    }

    /// 零容量：`cap_rider: 0` = 只把手上的送完、不再接新单。
    ///
    /// 这个人手上是空的，所以 `is_free` 为真、会进 `ai_roster`——LLM 直派曾经
    /// 是唯一能绕开这个语义的入口（`accepts` / `try_claim` 都按容量拦着）。
    #[test]
    fn a_zero_cap_rider_cannot_be_claimed_by_the_llm() {
        let mut w = world();
        w.set_sim_rider_count(1);
        let rid = w.sim_rider_ids()[0];
        w.rider_mut(rid).unwrap().online = true;
        set_cap(&mut w, rid, 0);
        w.generate_order_batch(1);
        let oid = w.pending_orders()[0];

        assert_eq!(
            w.rider(rid).unwrap().effective_max_held(&w.cfg),
            0,
            "前提：站长把上限压到 0"
        );
        let idle = roster(&w);
        assert!(
            idle.contains(&rid),
            "前提：手上没单 ⇒ 名单照样收他，这正是漏点"
        );

        let v = validate_llm_plan(&w, &claim_plan(&[(rid, oid)]), &idle, &w.pending_orders());
        assert!(
            v.assignments.is_empty(),
            "cap = 0 的人不该再接到新单：{:?}",
            v.assignments
        );
        assert!(v.covered.is_empty(), "没落地的意图不能算进 covered");
        assert_eq!(v.dropped(), 1);
        assert_eq!(v.drops[0].reason, DropReason::RiderNotEligible);
        assert_eq!(v.drops[0].reason.as_str(), "rider_not_eligible");
        assert_eq!((v.drops[0].rider, v.drops[0].order), (Some(rid), Some(oid)));
        assert_eq!(v.drops[0].layer, IntentLayer::Act);
        assert_eq!(summarize_drops(&v.drops), "（rider_not_eligible×1）");
    }

    /// 还有位置时正常放行：容量守卫不是「一律不许 LLM 派单」。
    #[test]
    fn an_llm_claim_still_passes_while_there_is_room() {
        let mut w = world();
        w.set_sim_rider_count(1);
        let rid = w.sim_rider_ids()[0];
        w.rider_mut(rid).unwrap().online = true;
        w.generate_order_batch(1);
        let oid = w.pending_orders()[0];
        assert!(
            w.rider(rid).unwrap().effective_max_held(&w.cfg) > 0,
            "前提：默认上限 > 0"
        );

        let idle = roster(&w);
        let v = validate_llm_plan(&w, &claim_plan(&[(rid, oid)]), &idle, &w.pending_orders());
        assert!(v.drops.is_empty(), "有位置不该丢：{:?}", v.drops);
        assert_eq!(v.assignments.len(), 1);
        assert_eq!((v.assignments[0].rider, v.assignments[0].order), (rid, oid));
        assert_eq!(v.assignments[0].source, DecisionSource::Llm);
        assert_eq!(v.covered, vec![rid]);
    }

    /// 满载拒绝：持单数已经顶到有效容量，新 claim 不得落地。
    #[test]
    fn a_full_rider_cannot_be_topped_up_by_the_llm() {
        let mut w = world();
        w.set_sim_rider_count(1);
        let rid = w.sim_rider_ids()[0];
        w.rider_mut(rid).unwrap().online = true;
        let shop = rider_waiting_at_shop(&mut w, rid, 3);
        let extra = shop_orders(&mut w, &shop, 1)[0];

        let full = w.rider(rid).unwrap().effective_max_held(&w.cfg) as usize;
        assert_eq!(w.rider(rid).unwrap().held.len(), full, "前提：已背满");
        assert!(
            !w.rider(rid).unwrap().is_free(w.elapsed_min, &w.cfg),
            "背满的人其实进不了名单"
        );

        // 名单本该不含他；就算被塞进来，容量守卫也得挡住。
        let v = validate_llm_plan(
            &w,
            &claim_plan(&[(rid, extra)]),
            &[rid],
            &w.pending_orders(),
        );
        assert!(
            v.assignments.is_empty(),
            "背满了还派单就是越界：{:?}",
            v.assignments
        );
        assert_eq!(v.drops[0].reason, DropReason::RiderNotEligible);
        assert_eq!(w.rider(rid).unwrap().held.len(), full, "校验不该动世界");
    }

    /// 混合计划隔离：一个人因容量被丢，不该连累同一份计划里的其他人。
    #[test]
    fn one_capped_rider_does_not_poison_the_rest_of_the_plan() {
        let mut w = world();
        w.set_sim_rider_count(2);
        let ids = w.sim_rider_ids();
        let (capped, normal) = (ids[0], ids[1]);
        for r in [capped, normal] {
            w.rider_mut(r).unwrap().online = true;
        }
        set_cap(&mut w, capped, 0);
        let pending = w.generate_order_batch(2);

        let idle = roster(&w);
        assert!(idle.contains(&capped), "cap = 0 但手上空 ⇒ 仍在名单里");
        assert!(idle.contains(&normal));

        let v = validate_llm_plan(
            &w,
            &claim_plan(&[(capped, pending[0]), (normal, pending[1])]),
            &idle,
            &w.pending_orders(),
        );
        assert_eq!(v.assignments.len(), 1, "只该丢超容量那一条：{:?}", v.drops);
        assert_eq!(
            (v.assignments[0].rider, v.assignments[0].order),
            (normal, pending[1])
        );
        assert_eq!(v.covered, vec![normal], "被丢的人不该算进 covered");
        assert_eq!(v.dropped(), 1);
        assert_eq!(v.drops[0].rider, Some(capped));
        assert_eq!(v.drops[0].reason, DropReason::RiderNotEligible);
    }

    /// 覆盖值大于配置上限：配置上限才是天花板（不能因为覆盖写了 9 就有 9 个位置）。
    #[test]
    fn the_config_ceiling_caps_what_the_override_can_hand_out() {
        let mut w = world();
        w.set_sim_rider_count(1);
        w.cfg.backorder.max_held = 3;
        let rid = w.sim_rider_ids()[0];
        w.rider_mut(rid).unwrap().online = true;
        set_cap(&mut w, rid, 9);
        assert_eq!(
            w.rider(rid).unwrap().effective_max_held(&w.cfg),
            3,
            "覆盖不能突破配置上限"
        );

        let shop = rider_waiting_at_shop(&mut w, rid, 3);
        let extra = shop_orders(&mut w, &shop, 1)[0];
        let v = validate_llm_plan(
            &w,
            &claim_plan(&[(rid, extra)]),
            &[rid],
            &w.pending_orders(),
        );
        assert!(
            v.assignments.is_empty(),
            "覆盖写着 9，但配置只给 3：{:?}",
            v.assignments
        );
        assert_eq!(v.drops[0].reason, DropReason::RiderNotEligible);
    }

    /// 覆盖值小于配置上限：覆盖值生效（`cap_override = 1` 时第 2 单就进不来）。
    #[test]
    fn the_override_reduces_the_room_the_llm_can_use() {
        let mut w = world();
        w.set_sim_rider_count(2);
        let ids = w.sim_rider_ids();
        let (full, free) = (ids[0], ids[1]);
        for r in [full, free] {
            w.rider_mut(r).unwrap().online = true;
            set_cap(&mut w, r, 1);
        }
        assert_eq!(w.rider(full).unwrap().effective_max_held(&w.cfg), 1);

        let shop = rider_waiting_at_shop(&mut w, full, 1);
        let extra = shop_orders(&mut w, &shop, 1)[0];
        let other = w.generate_order_batch(1)[0];

        let mut idle = roster(&w);
        idle.push(full); // 名单本该不含他（手上有单），故意塞进来
        let v = validate_llm_plan(
            &w,
            &claim_plan(&[(full, extra), (free, other)]),
            &idle,
            &w.pending_orders(),
        );
        assert_eq!(
            v.assignments.len(),
            1,
            "满了的那条该丢、空手的那条该过：{:?}",
            v.drops
        );
        assert_eq!(
            (v.assignments[0].rider, v.assignments[0].order),
            (free, other)
        );
        assert_eq!(v.covered, vec![free]);
        assert_eq!(v.dropped(), 1);
        assert_eq!(v.drops[0].rider, Some(full));
        assert_eq!(v.drops[0].reason, DropReason::RiderNotEligible);
    }

    /// 普通派单（算法贪心）也要尊重有效拒单记录：只绕开那一对，
    /// 不能因为拒过一单就把骑手整体排除。
    #[test]
    fn the_greedy_fill_respects_a_refusal_without_blacklisting_the_rider() {
        use crate::sim::world::RefusalKind;

        let mut w = world();
        w.cfg.order_spawn_interval_min = 100_000;
        // 门槛压到 0，确保「接不接」只取决于拒单记录。
        for r in &mut w.riders {
            r.strategy.accept_threshold_cents_hour = 0;
            r.strategy.detour_tolerance_m = u32::MAX;
        }
        let ids = w.generate_order_batch(2);
        let (refused, other) = (ids[0], ids[1]);
        let rid = w.sim_rider_ids()[0];
        w.record_refusals(&[LlmRefusal {
            rider: rid,
            order: refused,
            kind: RefusalKind::Reject,
            reason: "太远".into(),
        }]);

        let got = batch_decide(&w, &ids, &[rid], &[]);
        assert!(
            !got.iter().any(|a| a.order == refused),
            "拒过的单不能再塞回同一个人：{got:?}"
        );
        assert!(
            got.iter().any(|a| a.order == other && a.rider == rid),
            "同一骑手仍然可以接别的单：{got:?}"
        );
    }
}
