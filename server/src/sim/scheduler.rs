//! 派单与骑手动作。**抢单在这里保证原子性。**
//!
//! 所有入口都要求 `&mut World`。因为整个模拟跑在一个串行的 tokio 任务里，
//! 天然互斥，**不需要分布式锁**。验收标准：10 个客户端同时抢 1 单，成功者恰好 1 个。

use rand::RngExt;
use serde::Serialize;

use super::{
    OrderId, RiderId,
    ai::{self, Assignment},
    config::{PEAK_PREP_FACTOR, STORM_PREP_FACTOR, SimConfig},
    economy,
    event::{DecisionSource, SimEvent},
    order::{OrderStatus, OrderTier},
    rider::{HeldOrder, Phase, RiderKind, SpeedMode},
    route::{LegKind, RouteLeg, RouteTask},
    world::World,
};

/// 抢单 / 动作失败的原因。协议里是**固定字符串枚举**，中文会改，枚举不会改。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ClaimError {
    OrderNotFound,
    RiderNotFound,
    AlreadyClaimed,
    OrderExpired,
    RiderOffline,
    RiderFrozen,
    RiderInjured,
    RatingTooLow,
    HeldLimitReached,
    BackOrderDisabled,
    BackOrderDetourTooLong,
    WrongPhase,
    NotAssignedToYou,
    FeatureDisabled,
    /// 今日拒单额度已用完——指派单拒不掉了，只能送。
    RejectLimitReached,
}

impl ClaimError {
    /// 协议里的**固定字符串错误码**（技术设计 §4.1）。
    ///
    /// 中文文案随时会改，错误码不能改——客户端靠它做分支。
    /// 这里手写一遍是为了让 net 层不必把 `ClaimError` 序列化成 JSON 再取字符串；
    /// `code_matches_serde_representation` 测试保证两者永不漂移。
    pub fn code(self) -> &'static str {
        match self {
            ClaimError::OrderNotFound => "ORDER_NOT_FOUND",
            ClaimError::RiderNotFound => "RIDER_NOT_FOUND",
            ClaimError::AlreadyClaimed => "ALREADY_CLAIMED",
            ClaimError::OrderExpired => "ORDER_EXPIRED",
            ClaimError::RiderOffline => "RIDER_OFFLINE",
            ClaimError::RiderFrozen => "RIDER_FROZEN",
            ClaimError::RiderInjured => "RIDER_INJURED",
            ClaimError::RatingTooLow => "RATING_TOO_LOW",
            ClaimError::HeldLimitReached => "HELD_LIMIT_REACHED",
            ClaimError::BackOrderDisabled => "BACK_ORDER_DISABLED",
            ClaimError::BackOrderDetourTooLong => "BACK_ORDER_DETOUR_TOO_LONG",
            ClaimError::WrongPhase => "WRONG_PHASE",
            ClaimError::NotAssignedToYou => "NOT_ASSIGNED_TO_YOU",
            ClaimError::FeatureDisabled => "FEATURE_DISABLED",
            ClaimError::RejectLimitReached => "REJECT_LIMIT_REACHED",
        }
    }

    /// 给用户看的中文说明。
    pub fn message(self) -> &'static str {
        match self {
            ClaimError::OrderNotFound => "订单不存在",
            ClaimError::RiderNotFound => "骑手不存在",
            ClaimError::AlreadyClaimed => "订单已被抢走",
            ClaimError::OrderExpired => "订单已过期",
            ClaimError::RiderOffline => "你还没上线",
            ClaimError::RiderFrozen => "账号被冻结中",
            ClaimError::RiderInjured => "受伤停工中",
            ClaimError::RatingTooLow => "评分不足，接不了这一档的单",
            ClaimError::HeldLimitReached => "手持单已达上限",
            ClaimError::BackOrderDisabled => "控制端关闭了背单",
            ClaimError::BackOrderDetourTooLong => "绕路太远，不划算",
            ClaimError::WrongPhase => "当前状态不允许这个操作",
            ClaimError::NotAssignedToYou => "这不是你的单",
            ClaimError::FeatureDisabled => "该功能未开启",
            ClaimError::RejectLimitReached => "你今天拒绝已超出上限",
        }
    }
}

/// 骑手能不能接这个单（只读检查）。
pub fn check_claimable(
    world: &World,
    order_id: OrderId,
    rider_id: RiderId,
) -> Result<(), ClaimError> {
    let order = world.order(order_id).ok_or(ClaimError::OrderNotFound)?;
    let rider = world.rider(rider_id).ok_or(ClaimError::RiderNotFound)?;
    let now = world.elapsed_min;

    match order.status {
        OrderStatus::Pending => {}
        OrderStatus::Expired => return Err(ClaimError::OrderExpired),
        _ => return Err(ClaimError::AlreadyClaimed),
    }
    if now > order.claim_deadline_min {
        return Err(ClaimError::OrderExpired);
    }
    if !rider.online {
        return Err(ClaimError::RiderOffline);
    }
    if rider.is_frozen(now) {
        return Err(ClaimError::RiderFrozen);
    }
    if rider.is_injured(now) {
        return Err(ClaimError::RiderInjured);
    }
    if !economy::can_claim_tier(&world.cfg, rider.rating, order.tier) {
        return Err(ClaimError::RatingTooLow);
    }
    if rider.reached_daily_cap(&world.cfg) {
        return Err(ClaimError::HeldLimitReached);
    }
    Ok(())
}

/// **抢单（原子）**。多个客户端并发调用时，只有第一个能成功。
pub fn try_claim(
    world: &mut World,
    order_id: OrderId,
    rider_id: RiderId,
) -> Result<Assignment, ClaimError> {
    check_claimable(world, order_id, rider_id)?;
    let assignment = ai::evaluate(world, rider_id, order_id).ok_or(ClaimError::OrderNotFound)?;
    attach(world, &assignment);
    Ok(assignment)
}

/// 控制端强制指派：无视评分门槛与抢单窗口，但骑手端仍要确认。
pub fn assign_forced(
    world: &mut World,
    order_id: OrderId,
    rider_id: RiderId,
) -> Result<Assignment, ClaimError> {
    let order = world.order(order_id).ok_or(ClaimError::OrderNotFound)?;
    let rider = world.rider(rider_id).ok_or(ClaimError::RiderNotFound)?;
    let now = world.elapsed_min;

    if order.status != OrderStatus::Pending && order.status != OrderStatus::Expired {
        return Err(ClaimError::AlreadyClaimed);
    }
    if !rider.online {
        return Err(ClaimError::RiderOffline);
    }
    if rider.is_injured(now) {
        return Err(ClaimError::RiderInjured);
    }
    if rider.held.len() as u32 >= rider.effective_max_held(&world.cfg) {
        return Err(ClaimError::HeldLimitReached);
    }

    let mut assignment =
        ai::evaluate(world, rider_id, order_id).ok_or(ClaimError::OrderNotFound)?;
    assignment.source = DecisionSource::Manual;
    assignment.reason = "控制端强制指派".to_string();
    attach(world, &assignment);
    if let Some(o) = world.order_mut(order_id) {
        o.forced = true;
    }
    // 记下「还没被接受」。骑手点接受走 `accept_assign`，点拒绝走
    // `reject_order`——后者靠这个标记才知道该吃拒单额度而不是扣 5 元罚款。
    if let Some(r) = world.rider_mut(rider_id) {
        r.assign_pending = Some(order_id);
    }
    Ok(assignment)
}

/// 骑手确认接受一张强制指派单。
///
/// 订单在 `assign_forced` 时就已经挂到骑手身上了（世界状态必须立刻自洽，
/// 否则这单在别的端看起来还是无主的）。所以这里**不改任何调度状态**，
/// 只把 `assign_pending` 摘掉——从此这单就是骑手自己接的了，
/// 后面再取消就要按「主动取消」罚钱。
pub fn accept_assign(
    world: &mut World,
    rider_id: RiderId,
    order_id: OrderId,
) -> Result<(), ClaimError> {
    let rider = world.rider_mut(rider_id).ok_or(ClaimError::RiderNotFound)?;
    match rider.assign_pending {
        Some(id) if id == order_id => {
            rider.assign_pending = None;
            Ok(())
        }
        // 不是待确认的指派单：可能已经接受过，或者已经拒绝了。
        // 不报错——客户端的「接受」是幂等的，重发不该弹错。
        _ => Ok(()),
    }
}

/// 背单：绕路不超阈值就允许叠一单。
pub fn claim_extra(
    world: &mut World,
    order_id: OrderId,
    rider_id: RiderId,
) -> Result<(Assignment, i64), ClaimError> {
    if !world.cfg.backorder_enabled {
        return Err(ClaimError::BackOrderDisabled);
    }
    let rider = world.rider(rider_id).ok_or(ClaimError::RiderNotFound)?;
    if rider.held.is_empty() {
        // 手上没单，走普通抢单
        let a = try_claim(world, order_id, rider_id)?;
        return Ok((a, 0));
    }
    if rider.held.len() as u32 >= rider.effective_max_held(&world.cfg) {
        return Err(ClaimError::HeldLimitReached);
    }
    check_claimable(world, order_id, rider_id)?;

    // 绕路距离 = (骑手→新商家 + 新商家→新顾客) 减去 当前路线长度
    let cfg_backorder = world.cfg.backorder.clone();
    let assignment = ai::evaluate(world, rider_id, order_id).ok_or(ClaimError::OrderNotFound)?;
    let detour_m = {
        let r = world.rider(rider_id).unwrap();
        let o = world.order(order_id).unwrap();
        let extra = super::geo::haversine_m(r.pos(), o.merchant_pos()) as u32 + o.distance_m;
        extra.saturating_sub(rider_current_leg(world, rider_id))
    };
    if detour_m > cfg_backorder.detour_threshold_m * 3 {
        return Err(ClaimError::BackOrderDetourTooLong);
    }

    // 背单补偿 0.50 ~ 1.20 元，绕路越多补越多
    let ratio = (detour_m as f32 / (cfg_backorder.detour_threshold_m * 3) as f32).clamp(0.0, 1.0);
    let bonus = cfg_backorder.bonus_min_cents
        + ((cfg_backorder.bonus_max_cents - cfg_backorder.bonus_min_cents) as f32 * ratio).round()
            as i64;
    if let Some(o) = world.order_mut(order_id) {
        o.back_order = true;
        o.back_order_bonus_cents = bonus;
    }

    let mut assignment = assignment;
    assignment.reason = format!("背单 绕路 {detour_m}m 补 {}分", bonus);
    attach(world, &assignment);
    if let Some(o) = world.order_mut(order_id) {
        o.back_order = true;
        o.back_order_bonus_cents = bonus;
    }
    Ok((assignment, bonus))
}

/// 当前手持单的剩余行程长度（粗算，用于背单绕路判定）。
fn rider_current_leg(world: &World, rider_id: RiderId) -> u32 {
    let Some(rider) = world.rider(rider_id) else {
        return 0;
    };
    let Some(first) = rider.held.first() else {
        return 0;
    };
    let Some(order) = world.order(first.order_id) else {
        return 0;
    };
    match first.phase {
        Phase::ToShop => super::geo::haversine_m(rider.pos(), order.merchant_pos()) as u32,
        _ => order.distance_m,
    }
}

/// 把 AI / LLM（已校验）的分配落到世界上。
///
/// 与 `try_claim` 共用同一个 `attach`，因此「抢单改状态」永远只有一处，
/// 不可能出现客户端抢单和 AI 派单走上两条不同代码路径的情况。
pub fn apply_assignment(world: &mut World, assignment: &Assignment) {
    attach(world, assignment);
}

/// 超时 / 被取消的单的最后一手：**不允许流单**，强制指派给最近的骑手。
///
/// 与 [`try_claim`] 的区别：绕开抢单窗口、评分门槛和接单意愿评估，只看
/// 「谁离商家最近、还背得下」。真实平台在单快黄了的时候也是这么兜的
/// （扩大派单范围、直接塞单），所以这是**算法**决策，跟 LLM 无关——
/// 上报时来源是 [`DecisionSource::AlgorithmRescue`]。
///
/// 只派给**模拟骑手**：真人玩家在线时手里的单是他在玩的，AI 不抢。
///
/// 返回 `None` 表示一个骑手都派不出去（全离线 / 全受伤 / 都背满了），
/// 此时订单**继续留在大厅**（已经加价、窗口已重置），等下一轮再救。
/// 无论哪条路，订单都不会被判流单。
pub fn rescue_nearest(world: &mut World, order_id: OrderId) -> Option<Assignment> {
    let now = world.elapsed_min;
    let cfg = world.cfg.clone();
    let order = world.order(order_id)?;
    if order.status != OrderStatus::Pending {
        return None;
    }
    let merchant_pos = order.merchant_pos();

    // 最近的优先；距离一样就挑背单少的（更可能真跑得动）。
    let mut best: Option<(f64, usize, RiderId)> = None;
    for r in &world.riders {
        if r.kind != RiderKind::Simulated || !r.online {
            continue;
        }
        if r.is_frozen(now) || r.is_injured(now) {
            continue;
        }
        if r.held.len() as u32 >= r.effective_max_held(&cfg) {
            continue;
        }
        let d = super::geo::haversine_m(r.pos(), merchant_pos);
        let better = match best {
            None => true,
            Some((best_d, best_held, _)) => d < best_d || (d == best_d && r.held.len() < best_held),
        };
        if better {
            best = Some((d, r.held.len(), r.id));
        }
    }

    let (dist_m, _, rider_id) = best?;
    let mut assignment = ai::evaluate(world, rider_id, order_id)?;
    assignment.source = DecisionSource::AlgorithmRescue;
    assignment.reason = format!(
        "超时救单：重投已用尽，强制塞给最近的骑手（距商家约 {:.0}m）",
        dist_m
    );
    attach(world, &assignment);
    world.stats.orders_rescued += 1;
    Some(assignment)
}

/// 把一笔强制指派包成推给控制台的事件（与 AI 派单共用 `order_claimed`）。
///
/// `forced` 置位是为了让人一眼看出「这单不是谁抢的，是系统塞的」。
pub fn claimed_event(world: &World, a: &Assignment) -> SimEvent {
    SimEvent::OrderClaimed {
        order_id: a.order,
        rider_id: a.rider,
        rider_name: world
            .rider(a.rider)
            .map(|r| r.name.clone())
            .unwrap_or_default(),
        quoted_fee_cents: world
            .order(a.order)
            .map(|o| o.quoted_fee_cents())
            .unwrap_or(0),
        forced: true,
    }
}

/// 真正把订单挂到骑手身上。这是「抢单」唯一改状态的地方。
fn attach(world: &mut World, assignment: &Assignment) {
    let now = world.elapsed_min;
    let plan = assignment.plan;
    let back_order = world
        .order(assignment.order)
        .map(|o| o.back_order)
        .unwrap_or(false);

    // 出餐耗时：商家均值 × 高峰系数 × 抖动 ±20%
    let prep = {
        let merchant_avg = world.avg_prep_minutes(
            world
                .order(assignment.order)
                .map(|o| o.merchant_id)
                .unwrap_or(0),
        );
        let peak = if world.cfg.is_peak(world.sim_minute) {
            PEAK_PREP_FACTOR
        } else {
            1.0
        };
        let extreme = if world.weather.is_storm() && world.cfg.is_peak(world.sim_minute) {
            STORM_PREP_FACTOR
        } else {
            1.0
        };
        let jitter: f32 = world.rng.random_range(0.8..1.2);
        ((merchant_avg as f32 * peak * extreme * jitter).round() as u32).max(1)
    };

    if let Some(order) = world.order_mut(assignment.order) {
        order.status = OrderStatus::Assigned;
        order.assigned_to = Some(assignment.rider);
        order.prep_minutes = Some(prep);
        // 订单在大厅里空转的那段时间不是接单骑手的账。
        // 骑手刚接单就出发，却要替平台在大厅里待的那十几分钟买单，
        // 等于「接单即超时」。重投退回时已有 `renew_promise` 兜底，
        // 但**首次**派出去之前的空转没人管——这里补上（见 `Order::credit_hall_idle`）。
        order.credit_hall_idle(now);
    }

    let shop = world.order(assignment.order).map(|o| o.merchant_pos());
    if let Some(rider) = world.rider_mut(assignment.rider) {
        let to_shop = plan.to_shop_min.max(1);
        rider.held.push(HeldOrder::new(
            assignment.order,
            Phase::ToShop,
            to_shop,
            back_order,
            now,
        ));
        // 路线：已有路线就把取餐腿插进去（自动排到第一个送达腿之前，
        // 保证餐先取齐），没有就起一条。
        //
        // `held` 记「手上有哪些单」，路线记「脚步往哪迈」——合并单就是在这里发生的：
        // 第二单不再自己找路，而是并入同一串腿。
        if let Some(shop) = shop {
            let leg = RouteLeg::new(assignment.order, LegKind::Pickup, shop, to_shop);
            match rider.route.as_mut() {
                Some(route) => route.push_leg(leg),
                None => rider.route = Some(RouteTask::from_pickup(assignment.order, shop, to_shop)),
            }
        }
    }
    world.stats.claims += 1;
}

/// 骑手动作产生的副作用事件。
pub fn state_event(world: &World, rider_id: RiderId) -> Option<SimEvent> {
    let rider = world.rider(rider_id)?;
    Some(SimEvent::RiderStateChanged {
        rider_id,
        rider: Box::new(super::event::RiderBrief::public(
            rider,
            &world.cfg,
            world.elapsed_min,
        )),
    })
}

/// 上下线。
pub fn set_online(
    world: &mut World,
    rider_id: RiderId,
    online: bool,
) -> Result<Vec<SimEvent>, ClaimError> {
    let cfg = world.cfg.clone();
    let rider = world.rider_mut(rider_id).ok_or(ClaimError::RiderNotFound)?;
    if !online && !rider.held.is_empty() {
        // 手上还有单不能下线
        return Err(ClaimError::WrongPhase);
    }
    rider.online = online;
    if !online {
        rider.fatigue = (rider.fatigue - cfg.fatigue.per_rest_10min).max(0.0);
        // 收工是唯一「主动回情绪」的手段：下线喘口气，心情回来一点。
        rider.shift_mood(cfg.mood.per_rest, &cfg);
    }
    Ok(state_event(world, rider_id).into_iter().collect())
}

/// 切速度档位。
pub fn set_speed_mode(
    world: &mut World,
    rider_id: RiderId,
    mode: SpeedMode,
) -> Result<Vec<SimEvent>, ClaimError> {
    let cfg = world.cfg.clone();
    if mode == SpeedMode::Tolerant && !cfg.allow_speeding {
        return Err(ClaimError::FeatureDisabled);
    }
    if mode == SpeedMode::Unrestricted && !cfg.allow_unrestricted {
        return Err(ClaimError::FeatureDisabled);
    }
    let rider = world.rider_mut(rider_id).ok_or(ClaimError::RiderNotFound)?;
    rider.speed_mode = mode;
    Ok(state_event(world, rider_id).into_iter().collect())
}

/// 到达商家（确认）。真人骑手必须点，倒计时归零后由 sim 兜底推进。
pub fn arrive_shop(
    world: &mut World,
    rider_id: RiderId,
    order_id: OrderId,
) -> Result<Vec<SimEvent>, ClaimError> {
    let now = world.elapsed_min;
    let rider = world.rider_mut(rider_id).ok_or(ClaimError::RiderNotFound)?;
    let held = rider
        .held_mut(order_id)
        .ok_or(ClaimError::NotAssignedToYou)?;
    if held.phase != Phase::ToShop {
        return Err(ClaimError::WrongPhase);
    }
    held.set_leg(Phase::WaitingFood, 1);
    held.awaiting_ack = false;
    held.remaining_min = 0;
    held.wait_started_min = now;
    held.wait_fined = false;
    // 到店 = 取餐腿跑完。同店多单一起出队，别让第二单再排一次到店。
    rider.complete_pickup_legs_at_current();
    Ok(state_event(world, rider_id).into_iter().collect())
}

/// 到达顾客。
pub fn arrive_customer(
    world: &mut World,
    rider_id: RiderId,
    order_id: OrderId,
) -> Result<Vec<SimEvent>, ClaimError> {
    let rider = world.rider_mut(rider_id).ok_or(ClaimError::RiderNotFound)?;
    let held = rider
        .held_mut(order_id)
        .ok_or(ClaimError::NotAssignedToYou)?;
    if held.phase != Phase::ToCustomer {
        return Err(ClaimError::WrongPhase);
    }
    held.set_leg(Phase::ArrivedCustomer, 1);
    held.awaiting_ack = false;
    held.remaining_min = 0;
    Ok(state_event(world, rider_id).into_iter().collect())
}

/// 完成交付：结算 + 从手持单里移除。
pub fn deliver(
    world: &mut World,
    rider_id: RiderId,
    order_id: OrderId,
) -> Result<Vec<SimEvent>, ClaimError> {
    let phase = {
        let rider = world.rider(rider_id).ok_or(ClaimError::RiderNotFound)?;
        rider
            .held_phase(order_id)
            .ok_or(ClaimError::NotAssignedToYou)?
    };
    if phase != Phase::ArrivedCustomer {
        return Err(ClaimError::WrongPhase);
    }
    let now = world.elapsed_min;
    let cfg = world.cfg.clone();

    let mut events = Vec::new();
    let mut order = world
        .order_mut(order_id)
        .ok_or(ClaimError::OrderNotFound)?
        .clone();
    let rider = world.rider_mut(rider_id).ok_or(ClaimError::RiderNotFound)?;

    let (entry, late) = economy::settle_delivery(&cfg, rider, &mut order, now);
    let takehome = rider.takehome_cents();
    let hourly = rider.hourly_wage_cents();
    rider.drop_order(order_id);
    let rider_name = rider.name.clone();
    world.stats.orders_delivered += 1;
    if late {
        world.stats.orders_late += 1;
    }

    // 落库：把结算后的订单写回
    if let Some(slot) = world.order_mut(order_id) {
        slot.status = order.status;
        slot.delivered_at_min = order.delivered_at_min;
    }
    // 位置挪到顾客处
    let dest = order.customer_pos();
    if let Some(r) = world.rider_mut(rider_id) {
        r.set_pos(dest);
    }

    events.push(SimEvent::OrderDelivered {
        order_id,
        rider_id,
        rider_name,
        late,
        takehome_cents: takehome,
        hourly_wage_cents: hourly,
    });
    events.push(SimEvent::RiderFinance {
        rider_id,
        delta_cents: entry.amount_cents,
        kind: entry.kind.clone(),
        reason: entry.reason.clone(),
        takehome_cents: takehome,
        hourly_wage_cents: hourly,
    });
    if late {
        events.push(SimEvent::FineIssued {
            rider_id,
            kind: "deliver_timeout".into(),
            amount_cents: cfg.fine.deliver_timeout_cents,
            reason: "送达超时".into(),
        });
    }
    // 背单全崩：任一单超时，全部手持单都罚
    if late {
        let held_ids = world
            .rider(rider_id)
            .map(|r| r.held_ids())
            .unwrap_or_default();
        for other in held_ids {
            if let Some(r) = world.rider_mut(rider_id) {
                economy::issue_fine(
                    r,
                    &cfg,
                    cfg.fine.backorder_collapse_cents,
                    "backorder_collapse",
                    format!("背单全崩：订单 #{order_id} 超时，手持单 #{other} 连坐"),
                    Some(other),
                    now,
                );
            }
            events.push(SimEvent::FineIssued {
                rider_id,
                kind: "backorder_collapse".into(),
                amount_cents: cfg.fine.backorder_collapse_cents,
                reason: format!("背单全崩，手持单 #{other} 连坐"),
            });
        }
    }
    if let Some(ev) = state_event(world, rider_id) {
        events.push(ev);
    }
    Ok(events)
}

/// 拒绝订单。
///
/// 分两种情况，代价**完全不同**：
///
/// 1. **拒绝系统指派单**（`rider.assign_pending` 正指向它）——骑手还没点接受，
///    这单是平台硬塞过来的。代价是吃掉一次**每日拒单额度**（`max_rejects_per_day`），
///    **不罚款**；额度用完就直接拒不掉（[`ClaimError::RejectLimitReached`]），
///    指派什么就得送什么。
/// 2. **取消已经接下的单**——骑手自己反悔，罚 `fine.cancel_cents`（5 元），
///    重投次数照算。
///
/// 两种情况订单都**退回大厅加价重投**，绝不流单——真实场景里骑手不要这单，
/// 平台会把单转给下一个骑手，不能让顾客的饭凭空消失。
pub fn reject_order(
    world: &mut World,
    rider_id: RiderId,
    order_id: OrderId,
) -> Result<Vec<SimEvent>, ClaimError> {
    let now = world.elapsed_min;
    let cfg = world.cfg.clone();

    // ---- 情况 1：拒绝一张还没接受的指派单 ----
    let pending = world
        .rider(rider_id)
        .is_some_and(|r| r.assign_pending == Some(order_id));
    if pending {
        let limit = cfg.max_rejects_per_day;
        let used = {
            let rider = world.rider_mut(rider_id).ok_or(ClaimError::RiderNotFound)?;
            if limit > 0 && rider.rejected >= limit {
                // 额度已满。这一单**拒不掉**——它就是你的了，所以顺手摘掉
                // 「待确认」标记：否则骑手会卡在一个既拒不掉、又退不掉的单上
                // （拒绝被额度挡住，退回又要先过同一个判断）。
                //
                // 命令仍然算失败，客户端据此弹「你今天拒绝已超出上限」——
                // 这是唯一会让骑手看见「拒绝被拒」的路径。
                rider.assign_pending = None;
                return Err(ClaimError::RejectLimitReached);
            }
            rider.assign_pending = None;
            rider.rejected += 1;
            rider.drop_order(order_id);
            rider.rejected
        };
        world.release_to_hall(order_id);
        return Ok(vec![SimEvent::AssignRejected {
            rider_id,
            order_id,
            used,
            limit,
        }]);
    }

    // ---- 情况 2：取消一张已经接下的单 ----
    let rider = world.rider_mut(rider_id).ok_or(ClaimError::RiderNotFound)?;
    if rider.is_holding(order_id) {
        economy::issue_fine(
            rider,
            &cfg,
            cfg.fine.cancel_cents,
            "cancel",
            format!("主动取消已接订单 #{order_id}"),
            Some(order_id),
            now,
        );
        rider.drop_order(order_id);

        // 回大厅重投，**绝不流单**：窗口内还能被人抢；
        // 窗口过了还没人接，由 tick 统一走强制指派（见 `tick` 第 4 步）。
        world.release_to_hall(order_id);
        return Ok(vec![SimEvent::FineIssued {
            rider_id,
            kind: "cancel".into(),
            amount_cents: cfg.fine.cancel_cents,
            reason: "取消已接订单".into(),
        }]);
    }
    let rider = world.rider_mut(rider_id).ok_or(ClaimError::RiderNotFound)?;
    rider.rejected += 1;
    Ok(vec![])
}

/// 催单：有概率被拒，扣评分。
pub fn urge_order(
    world: &mut World,
    rider_id: RiderId,
    order_id: OrderId,
) -> Result<(bool, Vec<SimEvent>), ClaimError> {
    let cfg = world.cfg.clone();
    // 先掷骰子再借用骑手，避免同时可变借用 world.rng 和 world.riders。
    let success = world.rng.random_bool(0.5);
    let rider = world.rider_mut(rider_id).ok_or(ClaimError::RiderNotFound)?;
    if rider.held_phase(order_id) != Some(Phase::WaitingFood) {
        return Err(ClaimError::WrongPhase);
    }
    let mut events = Vec::new();
    if !success {
        economy::adjust_rating(&cfg, rider, -0.01);
        events.push(SimEvent::FineIssued {
            rider_id,
            kind: "urge_rejected".into(),
            amount_cents: 0,
            reason: "催单被商家拒绝".into(),
        });
    }
    let rider = world.rider_mut(rider_id).ok_or(ClaimError::RiderNotFound)?;
    if let Some(h) = rider.held_mut(order_id) {
        h.remaining_min = h.remaining_min.saturating_sub(if success { 2 } else { 0 });
    }
    if let Some(ev) = state_event(world, rider_id) {
        events.push(ev);
    }
    Ok((success, events))
}

/// 评分分档（给 UI 显示可抢范围）。
pub fn tier_available(cfg: &SimConfig, rating: f32) -> Vec<OrderTier> {
    [OrderTier::Normal, OrderTier::Premium, OrderTier::Express]
        .into_iter()
        .filter(|t| economy::can_claim_tier(cfg, rating, *t))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{config::SimConfig, world::World};

    /// 造一个「骑手手上有一张还没确认的指派单」的世界。
    fn world_with_pending_assign(limit: u32) -> (World, RiderId, OrderId) {
        let cfg = SimConfig {
            sim_rider_count: 1,
            max_rejects_per_day: limit,
            ..Default::default()
        };
        let mut w = World::new_for_test(cfg, 42);
        let rid = w.sim_rider_ids()[0];
        let oid = w.generate_order_batch(1)[0];
        assign_forced(&mut w, oid, rid).expect("能强制指派");
        (w, rid, oid)
    }

    #[test]
    fn rejecting_a_forced_assign_eats_quota_and_returns_to_hall() {
        let (mut w, rid, oid) = world_with_pending_assign(6);
        let evs = reject_order(&mut w, rid, oid).expect("第一次拒单应成功");
        assert!(
            matches!(
                evs.as_slice(),
                [SimEvent::AssignRejected {
                    rider_id,
                    order_id,
                    used: 1,
                    limit: 6,
                }] if *rider_id == rid && *order_id == oid
            ),
            "拒指派单要回一条 assign_rejected，带上已用/上限：{evs:?}"
        );

        let r = w.rider(rid).expect("骑手在");
        assert_eq!(r.rejected, 1);
        assert_eq!(r.assign_pending, None, "拒完就不该再挂着待确认");
        assert!(r.held.is_empty(), "单要从骑手手上摘掉");
        // 拒一张**没接受的指派单**只是把单还给平台，不罚钱——那是「取消已接单」的代价。
        assert_eq!(r.cash_cents, 0);
        assert_eq!(r.fines_cents, 0);
        assert_eq!(
            w.order(oid).expect("订单在").status,
            OrderStatus::Pending,
            "退回大厅继续等人接，绝不流单"
        );
    }

    #[test]
    fn reject_limit_blocks_further_rejections() {
        let (mut w, rid, oid) = world_with_pending_assign(1);
        reject_order(&mut w, rid, oid).expect("用掉唯一一次额度");

        let oid2 = w.generate_order_batch(1)[0];
        assign_forced(&mut w, oid2, rid).expect("能强制指派");
        let held_before = w.rider(rid).expect("骑手在").held.len();

        let err = reject_order(&mut w, rid, oid2).expect_err("额度用完必须拒绝");
        assert_eq!(err, ClaimError::RejectLimitReached);
        assert_eq!(err.code(), "REJECT_LIMIT_REACHED");
        assert_eq!(err.message(), "你今天拒绝已超出上限");

        let r = w.rider(rid).expect("骑手在");
        assert_eq!(r.rejected, 1, "被挡下的拒绝不许把额度继续往上加");
        assert_eq!(r.held.len(), held_before, "单还是骑手的，不许被摘走");
        assert_eq!(
            w.order(oid2).expect("订单在").status,
            OrderStatus::Assigned,
            "拒不掉就等于接下了"
        );
        assert_eq!(
            r.assign_pending, None,
            "拒不掉要顺手摘掉待确认标记，否则骑手会卡在拒不掉也退不掉的单上"
        );

        // 既然归他了，后面再退就是「取消已接单」：罚钱，且不再受额度阻拦。
        let cancel_cents = w.cfg.fine.cancel_cents;
        let evs = reject_order(&mut w, rid, oid2).expect("归他的单应该能退掉");
        assert!(matches!(evs.as_slice(), [SimEvent::FineIssued { .. }]));
        assert_eq!(w.rider(rid).expect("骑手在").cash_cents, -cancel_cents);
        assert_eq!(w.rider(rid).expect("骑手在").rejected, 1, "退款不算拒单");
    }

    #[test]
    fn zero_limit_means_no_cap() {
        let (mut w, rid, oid) = world_with_pending_assign(0);
        let evs = reject_order(&mut w, rid, oid).expect("0 表示不限，第一次当然能拒");
        assert!(
            matches!(
                evs.as_slice(),
                [SimEvent::AssignRejected {
                    limit: 0,
                    used: 1,
                    ..
                }]
            ),
            "上限 0 要原样透传成「不限」：{evs:?}"
        );

        // 再拒几轮也不该被挡住。
        for _ in 0..3 {
            let id = w.generate_order_batch(1)[0];
            assign_forced(&mut w, id, rid).expect("能强制指派");
            reject_order(&mut w, rid, id).expect("0 = 不限，永远拒得掉");
        }
        assert_eq!(w.rider(rid).expect("骑手在").rejected, 4);
    }

    /// 接受之后再取消 = 主动取消，罚钱但**不吃拒单额度**。
    ///
    /// 这是 `assign_pending` 存在的全部理由：分不清这两种情况，就会把
    /// 「不要系统塞来的单」和「自己反悔不送了」按同一个代价处理。
    #[test]
    fn accepting_then_cancelling_costs_a_fine_not_quota() {
        let (mut w, rid, oid) = world_with_pending_assign(6);
        accept_assign(&mut w, rid, oid).expect("接受应成功");
        assert_eq!(w.rider(rid).expect("骑手在").assign_pending, None);
        // 幂等：重复接受不报错、不改状态（客户端重发不该弹错）。
        accept_assign(&mut w, rid, oid).expect("重复接受是幂等的");

        let cancel_cents = w.cfg.fine.cancel_cents;
        let evs = reject_order(&mut w, rid, oid).expect("取消已接单应成功");
        assert!(
            matches!(evs.as_slice(), [SimEvent::FineIssued { .. }]),
            "取消已接单要开罚单：{evs:?}"
        );
        let r = w.rider(rid).expect("骑手在");
        assert_eq!(r.rejected, 0, "取消已接单不该算作拒单");
        assert_eq!(
            r.cash_cents, -cancel_cents,
            "钱包允许为负：罚单直接扣成负数"
        );
        assert_eq!(r.fines_cents, cancel_cents);
    }
}
