//! 经济系统：报价、抽水、结算、罚单、评分、等级。
//!
//! 全部金额单位「分」（i64）。**RMB 1:1 还原**，不做游戏币缩放。

use super::{
    OrderId,
    config::SimConfig,
    order::{FeeBreakdown, Order, OrderStatus, OrderTier},
    rider::{LedgerEntry, Rider},
    weather::Weather,
};

/// 距离附加费（分段）。
pub fn distance_extra_cents(cfg: &SimConfig, distance_m: u32) -> i64 {
    let tiers = &cfg.fee.distance_tiers;
    let mut extra = 0;
    for (threshold_m, cents) in tiers {
        if distance_m >= *threshold_m {
            extra = *cents;
        }
    }
    if let Some((last_m, _)) = tiers.last()
        && distance_m > *last_m
    {
        let over_km = (distance_m - *last_m) as f64 / 1000.0;
        extra += (over_km * cfg.fee.per_km_over_cents as f64).round() as i64;
    }
    extra
}

/// 等级加成：等级 × 20 分（1 级 0 元，6 级 1.00 元）。
pub fn level_bonus_cents(cfg: &SimConfig, level: u8) -> i64 {
    (level.max(1) as i64 - 1) * cfg.fee.level_bonus_per_level_cents
}

/// 完成单量 → 等级。
pub fn level_from_completed(cfg: &SimConfig, completed: u32) -> u8 {
    let mut level = 1u8;
    for (i, threshold) in cfg.level.thresholds.iter().enumerate() {
        if completed >= *threshold {
            level = (i + 1) as u8;
        }
    }
    level.min(cfg.level.coefficients.len() as u8)
}

/// 等级系数（只用于展示与 AI 期望值，不直接乘账单）。
pub fn level_coefficient(cfg: &SimConfig, level: u8) -> f32 {
    let i = (level.max(1) as usize - 1).min(cfg.level.coefficients.len() - 1);
    cfg.level.coefficients[i]
}

/// 评分能否抢该档次的单。
pub fn can_claim_tier(cfg: &SimConfig, rating: f32, tier: OrderTier) -> bool {
    let r = &cfg.rating_rules;
    match tier {
        OrderTier::Normal => rating >= r.tier_normal,
        OrderTier::Premium => rating >= r.tier_premium,
        OrderTier::Express => rating >= r.tier_express,
    }
}

/// 抽水率。歧视性抽水开启时：越能干越被抽。
pub fn commission_rate_for(cfg: &SimConfig, rider: &Rider) -> f32 {
    let base = cfg.commission_rate.clamp(0.0, 0.30);
    if !cfg.discriminative_commission {
        return base;
    }
    let uplift = if rider.rating >= cfg.rating_rules.tier_premium || rider.completed >= 20 {
        1.3
    } else if rider.rating < cfg.rating_rules.tier_normal {
        0.7
    } else {
        1.0
    };
    (base * uplift).clamp(0.0, 0.30)
}

/// 计算报价。
pub fn quote(
    cfg: &SimConfig,
    distance_m: u32,
    weather: Weather,
    sim_minute: u32,
    level: u8,
    tier: OrderTier,
    commission_rate: f32,
) -> FeeBreakdown {
    let base_fee_cents = cfg.fee.base_fee_cents;
    let distance_extra_cents = distance_extra_cents(cfg, distance_m);
    let level_bonus_cents = level_bonus_cents(cfg, level);
    let surge_multiplier = cfg.surge_multiplier(sim_minute);
    let rain_bonus_cents = weather.rain_bonus_cents();

    let delivery_pre = (base_fee_cents + distance_extra_cents + level_bonus_cents) as f32;
    let mut delivery_fee = (delivery_pre * surge_multiplier).round() as i64;
    if tier == OrderTier::Express {
        // 急单加价 30%
        delivery_fee = (delivery_fee as f32 * 1.3).round() as i64;
    }

    let commission_base = if cfg.commission_on_rain_bonus {
        delivery_fee + rain_bonus_cents
    } else {
        delivery_fee
    };
    let commission_cents = (commission_base as f32 * commission_rate).round() as i64;
    let quoted_fee_cents = delivery_fee + rain_bonus_cents;

    FeeBreakdown {
        base_fee_cents,
        distance_extra_cents,
        rain_bonus_cents,
        level_bonus_cents,
        surge_multiplier,
        commission_cents,
        quoted_fee_cents,
        takehome_cents: quoted_fee_cents - commission_cents,
    }
}

/// 入账。返回实际到手。
///
/// 会先抵扣 `debt_cents`——那是旧版本「扣款扣到 0 为止」留下的欠款字段，
/// 现在 `debit` 不再产生欠款（余额直接为负），这里保留抵扣只是为了
/// 让旧存档里的欠款能被还得掉，不会凭空消失。
pub fn credit(rider: &mut Rider, amount_cents: i64) -> i64 {
    if amount_cents <= 0 {
        return 0;
    }
    let offset = rider.debt_cents.min(amount_cents);
    rider.debt_cents -= offset;
    let net = amount_cents - offset;
    rider.cash_cents += net;
    net
}

/// 扣款。**余额允许为负。**
///
/// 罚单是可能超过当日收入的：跑一整天赚 30 元、撞一次车赔 50 元，
/// 账上就该老老实实显示 -20 元。旧实现把余额夹在 0、把差额塞进
/// `debt_cents`，账单上根本看不出骑手在倒欠平台——测试时尤其会被藏住，
/// 所以这里改成直接扣成负数。
pub fn debit(rider: &mut Rider, amount_cents: i64) {
    if amount_cents <= 0 {
        return;
    }
    rider.cash_cents -= amount_cents;
}

/// 清空钱包：余额、当日流水、罚款、保险、欠款全部归零，账单也清干净。
///
/// 给控制端「测试把账弄乱了，重来」用，**不做任何校验**——
/// 调用方（控制台）负责二次确认。返回清空前的余额，供提示语使用。
pub fn reset_wallet(rider: &mut Rider) -> i64 {
    let before = rider.cash_cents;
    rider.cash_cents = 0;
    rider.gross_cents = 0;
    rider.fines_cents = 0;
    rider.insurance_cents = 0;
    rider.debt_cents = 0;
    rider.last_ledger.clear();
    before
}

/// 开罚单：记钱 + 记明细（必须带原因）。
pub fn issue_fine(
    rider: &mut Rider,
    amount_cents: i64,
    kind: &str,
    reason: impl Into<String>,
    order_id: Option<OrderId>,
    now_min: u64,
) {
    if amount_cents <= 0 {
        return;
    }
    rider.fines_cents += amount_cents;
    debit(rider, amount_cents);
    rider.push_ledger(LedgerEntry {
        at_min: now_min,
        order_id,
        kind: kind.to_string(),
        amount_cents: -amount_cents,
        reason: reason.into(),
    });
}

/// 调整评分并夹在 [4.0, 5.0]。
pub fn adjust_rating(cfg: &SimConfig, rider: &mut Rider, delta: f32) -> f32 {
    let r = &cfg.rating_rules;
    let before = rider.rating;
    rider.rating = (rider.rating + delta).clamp(r.min, r.max);
    rider.rating - before
}

/// 送达结算。
///
/// 返回 (流水明细, 是否超时)。金额绝不出现负数（欠款单独记）。
pub fn settle_delivery(
    cfg: &SimConfig,
    rider: &mut Rider,
    order: &mut Order,
    now_min: u64,
) -> (LedgerEntry, bool) {
    let buffer = if order.back_order {
        cfg.backorder.late_buffer_min
    } else {
        0
    };
    let late = order.is_late(now_min, buffer);

    // 流水按骑手实拿口径记：含重投资补贴，但不含背单补偿（那是平台另掏的）。
    rider.gross_cents += order.quoted_fee_cents();

    // 到手 = 报价 - 抽水 + 重投补贴 + 背单补偿
    let mut takehome = order.takehome_cents() + order.back_order_bonus_cents;
    if late {
        // 超时：只赔配送费，雨补照给（雨天跑单的体面）
        takehome -= cfg.fine.deliver_timeout_cents.min(takehome.max(0));
    }

    let credited = credit(rider, takehome.max(0));
    order.delivered_at_min = Some(now_min);
    order.status = if late {
        OrderStatus::Late
    } else {
        OrderStatus::Delivered
    };

    rider.completed += 1;
    rider.lifetime_completed += 1;
    rider.level = level_from_completed(cfg, rider.lifetime_completed);
    rider.fatigue = (rider.fatigue + cfg.fatigue.per_order).clamp(0.0, cfg.fatigue.max);
    if late {
        rider.late_count += 1;
        adjust_rating(cfg, rider, cfg.rating_rules.late);
    } else {
        adjust_rating(cfg, rider, cfg.rating_rules.on_time);
    }

    let entry = LedgerEntry {
        at_min: now_min,
        order_id: Some(order.id),
        kind: "delivery".to_string(),
        amount_cents: credited,
        reason: if late {
            format!(
                "订单 #{} 超时送达，赔付 {} 分",
                order.id, cfg.fine.deliver_timeout_cents
            )
        } else {
            // 重投过的单要单独标出补贴，否则流水里的到手金额对不上计价明细。
            let surcharge = if order.surcharge_cents > 0 {
                format!(" + 重投补贴 {}", order.surcharge_cents)
            } else {
                String::new()
            };
            format!(
                "订单 #{} 配送费 {} + 雨补 {}{surcharge} - 抽水 {}",
                order.id,
                order.fee.base_fee_cents
                    + order.fee.distance_extra_cents
                    + order.fee.level_bonus_cents,
                order.fee.rain_bonus_cents,
                order.fee.commission_cents
            )
        },
    };
    rider.push_ledger(entry.clone());
    (entry, late)
}

/// 日结：扣保险、清零当日计数。返回日结流水。
pub fn close_day(cfg: &SimConfig, rider: &mut Rider) -> (i64, i64, i64, i64) {
    rider.insurance_cents += cfg.fine.insurance_per_day_cents;
    debit(rider, cfg.fine.insurance_per_day_cents);
    let snapshot = (
        rider.takehome_cents(),
        rider.cash_cents,
        rider.fines_cents,
        rider.hourly_wage_cents(),
    );
    rider.cash_cents = 0;
    rider.gross_cents = 0;
    rider.fines_cents = 0;
    rider.insurance_cents = 0;
    rider.completed = 0;
    rider.rejected = 0;
    // 跨天了，昨天那张还没确认的指派单不该继续挂着——不然今天的第一次
    // 拒单会被算成「拒绝一张昨天塞过来的单」，额度和提示都会对不上。
    rider.assign_pending = None;
    rider.late_count = 0;
    rider.online_minutes = 0;
    rider.fatigue = 0.0;
    rider.last_ledger.clear();
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{
        geo::LatLng,
        rider::{Rider, RiderKind},
    };

    fn rider(cfg: &SimConfig) -> Rider {
        Rider::new(1, RiderKind::Human, "A", LatLng::new(30.5, 114.4), cfg)
    }

    #[test]
    fn distance_tiers_match_design() {
        let cfg = SimConfig::default();
        assert_eq!(distance_extra_cents(&cfg, 1000), 0);
        assert_eq!(distance_extra_cents(&cfg, 2000), 100);
        assert_eq!(distance_extra_cents(&cfg, 4000), 200);
        assert_eq!(distance_extra_cents(&cfg, 6000), 350);
        // 9000m = 350 + 1km*60
        assert_eq!(distance_extra_cents(&cfg, 9000), 410);
    }

    #[test]
    fn quote_breaks_down_correctly() {
        let cfg = SimConfig::default();
        // 平峰、晴天、1 级、1.2km → 350 分
        let q = quote(
            &cfg,
            1200,
            Weather::Clear,
            3 * 60,
            1,
            OrderTier::Normal,
            0.0,
        );
        assert_eq!(q.quoted_fee_cents, 350);
        assert_eq!(q.commission_cents, 0);
        assert_eq!(q.takehome_cents, 350);
    }

    #[test]
    fn commission_only_takes_delivery_fee() {
        let cfg = SimConfig::default();
        let q = quote(
            &cfg,
            1200,
            Weather::HeavyRain,
            3 * 60,
            1,
            OrderTier::Normal,
            0.20,
        );
        // 配送费 350，雨补 250
        assert_eq!(q.rain_bonus_cents, 250);
        assert_eq!(q.quoted_fee_cents, 600);
        assert_eq!(q.commission_cents, 70);
        assert_eq!(q.takehome_cents, 530);
    }

    #[test]
    fn express_surcharge_is_30pct() {
        let cfg = SimConfig::default();
        let normal = quote(
            &cfg,
            1200,
            Weather::Clear,
            3 * 60,
            1,
            OrderTier::Normal,
            0.0,
        );
        let express = quote(
            &cfg,
            1200,
            Weather::Clear,
            3 * 60,
            1,
            OrderTier::Express,
            0.0,
        );
        assert_eq!(
            express.quoted_fee_cents,
            (normal.quoted_fee_cents as f32 * 1.3).round() as i64
        );
    }

    #[test]
    fn level_and_rating() {
        let cfg = SimConfig::default();
        assert_eq!(level_from_completed(&cfg, 0), 1);
        assert_eq!(level_from_completed(&cfg, 60), 2);
        assert_eq!(level_from_completed(&cfg, 9999), 6);
        assert_eq!(level_bonus_cents(&cfg, 6), 100);
        assert!(can_claim_tier(&cfg, 4.9, OrderTier::Express));
        assert!(!can_claim_tier(&cfg, 4.5, OrderTier::Normal));
    }

    #[test]
    fn debit_can_push_the_wallet_negative() {
        let cfg = SimConfig::default();
        let mut r = rider(&cfg);
        r.cash_cents = 100;
        debit(&mut r, 300);
        // 罚款超过余额就显示负的，不再夹到 0
        assert_eq!(r.cash_cents, -200);
        assert_eq!(r.debt_cents, 0);
        // 入账原样加回来
        let credited = credit(&mut r, 500);
        assert_eq!(credited, 500);
        assert_eq!(r.cash_cents, 300);
    }

    #[test]
    fn reset_wallet_zeroes_everything() {
        let cfg = SimConfig::default();
        let mut r = rider(&cfg);
        r.cash_cents = -1234;
        r.gross_cents = 900;
        r.fines_cents = 2500;
        r.insurance_cents = 200;
        debit(&mut r, 0);
        r.push_ledger(LedgerEntry {
            at_min: 1,
            order_id: None,
            kind: "fine".to_string(),
            amount_cents: -500,
            reason: "测试".to_string(),
        });

        let before = reset_wallet(&mut r);
        assert_eq!(before, -1234);
        assert_eq!(r.cash_cents, 0);
        assert_eq!(r.gross_cents, 0);
        assert_eq!(r.fines_cents, 0);
        assert_eq!(r.insurance_cents, 0);
        assert_eq!(r.debt_cents, 0);
        assert!(r.last_ledger.is_empty());
    }

    #[test]
    fn discriminative_commission_hits_high_performers() {
        let cfg = SimConfig {
            commission_rate: 0.10,
            discriminative_commission: true,
            ..Default::default()
        };
        let mut r = rider(&cfg);
        r.rating = 4.9;
        assert!(commission_rate_for(&cfg, &r) > 0.10);
        r.rating = 4.4;
        r.completed = 0;
        assert!(commission_rate_for(&cfg, &r) < 0.10);
    }
}
