//! 派单与骑手动作。**抢单在这里保证原子性。**
//!
//! 所有入口都要求 `&mut World`。因为整个模拟跑在一个串行的 tokio 任务里，
//! 天然互斥，**不需要分布式锁**。验收标准：10 个客户端同时抢 1 单，成功者恰好 1 个。

use rand::RngExt;
use serde::Serialize;

use super::{
    OrderId, RiderId,
    ai::{self, Assignment},
    config::SimConfig,
    economy,
    event::{DecisionSource, SimEvent},
    order::{OrderStatus, OrderTier},
    rider::{HeldOrder, Phase, Rider, RiderKind, SpeedMode},
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
    if rider.held.len() as u32 >= world.cfg.backorder.max_held {
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
    Ok(assignment)
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
    if rider.held.len() as u32 >= world.cfg.backorder.max_held {
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
            2.0
        } else {
            1.0
        };
        let extreme = if world.weather.is_storm() && world.cfg.is_peak(world.sim_minute) {
            1.5
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
    }

    if let Some(rider) = world.rider_mut(assignment.rider) {
        rider.held.push(HeldOrder::new(
            assignment.order,
            Phase::ToShop,
            plan.to_shop_min.max(1),
            back_order,
            now,
        ));
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
    held.phase = Phase::WaitingFood;
    held.awaiting_ack = false;
    held.remaining_min = 0;
    held.wait_started_min = now;
    held.wait_fined = false;
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
    held.phase = Phase::ArrivedCustomer;
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
    rider.held.retain(|h| h.order_id != order_id);
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

/// 主动拒单（未接的单不能拒，只能忽略）。
pub fn reject_order(
    world: &mut World,
    rider_id: RiderId,
    order_id: OrderId,
) -> Result<Vec<SimEvent>, ClaimError> {
    let now = world.elapsed_min;
    let cfg = world.cfg.clone();
    let rider = world.rider_mut(rider_id).ok_or(ClaimError::RiderNotFound)?;
    if rider.is_holding(order_id) {
        // 已接的单只能取消，扣 5 元
        economy::issue_fine(
            rider,
            cfg.fine.cancel_cents,
            "cancel",
            format!("主动取消已接订单 #{order_id}"),
            Some(order_id),
            now,
        );
        rider.held.retain(|h| h.order_id != order_id);
        if let Some(o) = world.order_mut(order_id) {
            o.status = OrderStatus::Cancelled;
            o.assigned_to = None;
        }
        return Ok(vec![
            SimEvent::FineIssued {
                rider_id,
                kind: "cancel".into(),
                amount_cents: cfg.fine.cancel_cents,
                reason: "取消已接订单".into(),
            },
            SimEvent::OrderExpired { order_id },
        ]);
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

/// 骑手是否是真人的辅助判断。
pub fn is_human(rider: &Rider) -> bool {
    rider.kind == RiderKind::Human
}
