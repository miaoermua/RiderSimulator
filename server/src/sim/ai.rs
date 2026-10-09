//! AI 决策：单骑手打分 + 批量贪心指派 + LLM 计划校验。
//!
//! **算法负责 16ms 内必须完成的决策**（玩法设计 §9）。
//! LLM 可以给计划，但计划必须过 `validate_llm_plan`，不信任任何输出。

use std::collections::HashSet;

use serde::Serialize;

use super::{
    OrderId, RiderId,
    event::{AiAssignment, DecisionSource},
    geo::{angle_between, bearing_deg, haversine_m},
    order::Order,
    rider::Rider,
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

/// 路网绕行系数：直线距离 ×1.3 近似真实骑行距离。
const ROAD_FACTOR: f32 = 1.3;

/// 估算行程。
pub fn plan_for(world: &World, rider: &Rider, order: &Order) -> DeliveryPlan {
    let cfg = &world.cfg;
    let speed_mps = rider.speed_mps(cfg, world.weather).max(1.0);
    let to_shop_m = haversine_m(rider.pos(), order.merchant_pos()) as f32 * ROAD_FACTOR;
    let to_customer_m = order.distance_m as f32 * ROAD_FACTOR;
    // 已到店则用实际出餐耗时，否则用商家平均值估算
    let prep = order
        .prep_minutes
        .unwrap_or_else(|| world.avg_prep_minutes(order.merchant_id));
    DeliveryPlan {
        to_shop_min: (to_shop_m / speed_mps / 60.0).ceil().max(1.0) as u32,
        prep_min: prep,
        to_customer_min: (to_customer_m / speed_mps / 60.0).ceil().max(1.0) as u32,
    }
}

/// 这一单的预估到手（分/单）。
pub fn estimated_takehome_cents(world: &World, rider: &Rider, order: &Order) -> i64 {
    let rate = super::economy::commission_rate_for(&world.cfg, rider);
    let base = order.fee.quoted_fee_cents;
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

    // 背单风险：手持越多越危险；预计送达会超时再加罚
    let held_ratio = rider.held.len() as f32 / cfg.backorder.max_held.max(1) as f32;
    let will_be_late =
        world.elapsed_min + total_min as u64 > order.created_at_min + order.promised_minutes as u64;
    let backorder_risk = (held_ratio + if will_be_late { 0.5 } else { 0.0 }).min(1.0);

    let score = hourly * 0.4 + detour * 0.3 + distance_pref * 0.2 + merchant_rating * 0.1
        - backorder_risk * 0.25;

    let reason = format!(
        "时薪 ¥{:.1}/h · 顺路 {:.0}% · {:.1}km{}",
        takehome as f32 / total_min * 60.0 / 100.0,
        detour * 100.0,
        dist_km,
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
fn accepts(world: &World, rider: &Rider, order: &Order) -> bool {
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
    let plan = plan_for(world, rider, order);
    let takehome = estimated_takehome_cents(world, rider, order);
    let hourly = takehome as f32 / plan.total_min().max(1) as f32 * 60.0;
    // 高峰期挑单：只接高时薪
    let threshold = if world.cfg.is_peak(world.sim_minute) {
        rider.strategy.accept_threshold_cents_hour as f32 * 1.15
    } else {
        rider.strategy.accept_threshold_cents_hour as f32
    };
    hourly >= threshold
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
    let mut candidates: Vec<Assignment> = new_orders
        .iter()
        .flat_map(|oid| {
            idle_riders.iter().filter_map(move |rid| {
                let r = world.rider(*rid)?;
                let o = world.order(*oid)?;
                if !accepts(world, r, o) {
                    return None;
                }
                evaluate(world, *rid, *oid).filter(|c| c.score > 0.10)
            })
        })
        .collect();

    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.order.cmp(&b.order))
            .then(a.rider.cmp(&b.rider))
    });

    let mut used_riders = HashSet::new();
    let mut used_orders = HashSet::new();
    let mut out = Vec::new();
    for c in candidates {
        if used_riders.contains(&c.rider) || used_orders.contains(&c.order) {
            continue;
        }
        used_riders.insert(c.rider);
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
    use crate::sim::{config::SimConfig, world::World};

    fn world() -> World {
        let cfg = SimConfig {
            sim_rider_count: 4,
            ..Default::default()
        };
        World::new_for_test(cfg, 42)
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
}
