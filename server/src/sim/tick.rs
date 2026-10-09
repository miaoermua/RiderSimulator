//! 固定 tick 循环。**时间权威只在这里。**
//!
//! 1 tick = 1 模拟分钟。时间倍率只改 tick 间隔（在 app 层），不改这里。
//! 所有截止时间用 `elapsed_min`（单调），展示用 `sim_minute`（当日 0:00 起算）。

use super::{
    OrderId, RiderId,
    ai::{self, LlmPlan},
    economy,
    event::{DayLedger, DecisionSource, SimEvent},
    geo::LatLng,
    order::OrderStatus,
    rider::{Phase, RiderKind},
    risk, scheduler,
    world::World,
};

/// tick 的外部输入。**IO 的结果只能从这里进来。**
#[derive(Debug, Clone, Default)]
pub struct TickInput {
    /// app 层异步拿到的 LLM 计划（可能为 None）。
    pub llm_plan: Option<LlmPlan>,
}

/// 推进一个 tick。
pub fn tick(world: &mut World, input: TickInput) -> Vec<SimEvent> {
    let mut events = Vec::new();
    // 0. 消费外部输入
    world.llm_plan = input.llm_plan;

    // 1. 时钟 + 疲劳
    let day_rolled = world.advance_clock();
    world.accrue_online();
    events.push(SimEvent::Tick {
        day: world.day,
        sim_minute: world.sim_minute,
        elapsed_min: world.elapsed_min,
        speed_multiplier: world.cfg.speed_multiplier,
    });

    // 2. 天气剧本
    if let Some(w) = world.advance_weather_script() {
        events.push(SimEvent::WeatherChanged {
            weather: w,
            label: w.label(),
            speed_factor: w.speed_factor(),
            rain_bonus_cents: w.rain_bonus_cents(),
        });
    }

    // 3. 生成新单（在线骑手越多，单越多）
    spawn_orders(world, &mut events);

    // 4. 抢单窗口过期的单
    for id in world.expire_stale_orders() {
        events.push(SimEvent::OrderExpired { order_id: id });
    }

    // 5. 骑手行程推进 + 风险
    advance_riders(world, &mut events);

    // 6. AI 分配
    if world.cfg.ai_batch_enabled {
        run_ai(world, &mut events);
    }

    // 7. 日结
    if day_rolled {
        close_day(world, &mut events);
    }

    // 8. 定期清理历史订单
    if world.elapsed_min.is_multiple_of(720) {
        world.compact_orders();
    }

    events
}

/// 按在线骑手数决定出单量。
fn spawn_orders(world: &mut World, events: &mut Vec<SimEvent>) {
    let interval = world.cfg.order_spawn_interval_min.max(1) as u64;
    if !world.elapsed_min.is_multiple_of(interval) {
        return;
    }
    let online = world.riders.iter().filter(|r| r.online).count() as u32;
    let pending = world.pending_orders().len() as u32;
    // 供大于求：单量随骑手数上升，但大厅积压超过 6 单就少发
    if pending >= 6 {
        return;
    }
    let n = (online / 2).clamp(1, 3);
    let window_ms = world.cfg.claim_window_secs as u64 * 1000;
    for id in world.generate_order_batch(n) {
        if let Some(order) = world.order(id) {
            events.push(SimEvent::OrderBroadcast {
                order: Box::new(order.clone()),
                window_ms,
                expires_in_ms: window_ms,
            });
        }
    }
}

/// AI 分配：优先用 LLM 计划（经校验），否则算法贪心。
fn run_ai(world: &mut World, events: &mut Vec<SimEvent>) {
    let pending = world.pending_orders();
    let idle = world.idle_riders();
    if pending.is_empty() || idle.is_empty() {
        world.llm_plan = None;
        return;
    }

    let plan = world.llm_plan.take();
    let (assignments, source, note) = match plan {
        Some(p) if world.cfg.llm_brain_enabled => {
            let verdict = ai::validate_llm_plan(world, &p, &idle, &pending);
            if verdict.assignments.is_empty() {
                // LLM 一条都没通过校验 → 算法兜底
                world.stats.llm_fallbacks += 1;
                (
                    ai::batch_decide(world, &pending, &idle),
                    DecisionSource::AlgorithmFallback,
                    format!("LLM（{}）输出全部被丢弃，算法兜底", p.model),
                )
            } else {
                world.stats.llm_calls += 1;
                let note = format!(
                    "LLM（{}）{}ms 给出 {} 条，丢弃 {} 条",
                    p.model,
                    p.latency_ms,
                    verdict.assignments.len(),
                    verdict.dropped
                );
                (verdict.assignments, DecisionSource::Llm, note)
            }
        }
        _ => (
            ai::batch_decide(world, &pending, &idle),
            DecisionSource::Algorithm,
            format!("贪心：{} 空闲骑手 / {} 待抢单", idle.len(), pending.len()),
        ),
    };

    if assignments.is_empty() {
        return;
    }
    for a in &assignments {
        scheduler::apply_assignment(world, a);
        events.push(SimEvent::OrderClaimed {
            order_id: a.order,
            rider_id: a.rider,
            rider_name: world
                .rider(a.rider)
                .map(|r| r.name.clone())
                .unwrap_or_default(),
            quoted_fee_cents: world
                .order(a.order)
                .map(|o| o.takehome_cents())
                .unwrap_or(0),
            forced: false,
        });
        if let Some(ev) = scheduler::state_event(world, a.rider) {
            events.push(ev);
        }
    }
    events.push(SimEvent::AiDecision {
        source,
        assignments: assignments.iter().map(|a| a.to_event()).collect(),
        note,
    });
}

/// 骑手行程推进。这是状态机唯一的推进入口。
fn advance_riders(world: &mut World, events: &mut Vec<SimEvent>) {
    let ids: Vec<RiderId> = world.riders.iter().map(|r| r.id).collect();
    let mut to_deliver: Vec<(RiderId, OrderId)> = Vec::new();

    for rid in ids {
        let held_ids: Vec<OrderId> = match world.rider(rid) {
            Some(r) => r.held_ids(),
            None => continue,
        };
        for oid in held_ids {
            step_one(world, rid, oid, &mut to_deliver, events);
        }
    }

    for (rid, oid) in to_deliver {
        if let Ok(evs) = scheduler::deliver(world, rid, oid) {
            events.extend(evs)
        }
    }
}

/// 推进单个 (骑手, 手持单)。
fn step_one(
    world: &mut World,
    rid: RiderId,
    oid: OrderId,
    to_deliver: &mut Vec<(RiderId, OrderId)>,
    events: &mut Vec<SimEvent>,
) {
    let now = world.elapsed_min;
    let weather = world.weather;
    let cfg = world.cfg.clone();

    let (phase, remaining, awaiting, prep, is_human, speed_mps, target) = {
        let Some(rider) = world.rider(rid) else {
            return;
        };
        let Some(h) = rider.held.iter().find(|h| h.order_id == oid) else {
            return;
        };
        let Some(order) = world.order(oid) else {
            return;
        };
        let target = match h.phase {
            Phase::ToShop | Phase::WaitingFood => order.merchant_pos(),
            _ => order.customer_pos(),
        };
        (
            h.phase,
            h.remaining_min,
            h.awaiting_ack,
            order
                .prep_minutes
                .unwrap_or_else(|| world.avg_prep_minutes(order.merchant_id)),
            rider.kind == RiderKind::Human,
            rider.speed_mps(&cfg, weather),
            target,
        )
    };

    // 位置推进：每 tick 走 1/(remaining+1) 的距离，客户端看到的是连续移动
    if matches!(phase, Phase::ToShop | Phase::ToCustomer)
        && let Some(rider) = world.rider_mut(rid)
    {
        let from = rider.pos();
        let frac = 1.0 / (remaining as f64 + 1.0);
        rider.set_pos(LatLng::new(
            from.lat + (target.lat - from.lat) * frac,
            from.lng + (target.lng - from.lng) * frac,
        ));
    }

    // 真人骑手在倒计时归零后必须点按钮，兜底 10 分钟自动推进
    if awaiting {
        let stalled = world
            .rider(rid)
            .and_then(|r| r.held.iter().find(|h| h.order_id == oid))
            .map(|h| now.saturating_sub(h.wait_started_min))
            .unwrap_or(0);
        if stalled >= 10 {
            if let Some(rider) = world.rider_mut(rid)
                && let Some(h) = rider.held_mut(oid)
            {
                h.awaiting_ack = false;
                h.remaining_min = 0;
            }
        } else {
            return;
        }
    }

    match phase {
        Phase::ToShop => {
            if remaining > 1 {
                set_remaining(world, rid, oid, remaining - 1);
                return;
            }
            // 到店
            if is_human {
                if let Some(rider) = world.rider_mut(rid)
                    && let Some(h) = rider.held_mut(oid)
                {
                    h.remaining_min = 0;
                    h.awaiting_ack = true;
                    h.wait_started_min = now;
                }
            } else {
                enter_waiting_food(world, rid, oid, prep, now, events);
            }
        }
        Phase::WaitingFood => {
            // 等餐超时：超过出餐时间 10 分钟仍是罚单（设计：等餐超时 -2.00 元）
            let (waited, fined) = world
                .rider(rid)
                .and_then(|r| r.held.iter().find(|h| h.order_id == oid))
                .map(|h| (now.saturating_sub(h.wait_started_min) as u32, h.wait_fined))
                .unwrap_or((0, true));
            if !fined && waited > prep + 10 {
                if let Some(r) = world.rider_mut(rid) {
                    economy::issue_fine(
                        r,
                        cfg.fine.wait_timeout_cents,
                        "wait_timeout",
                        format!("商家出餐超时（承诺 {prep} 分钟，实际已等 {waited} 分钟）"),
                        Some(oid),
                        now,
                    );
                    if let Some(h) = r.held_mut(oid) {
                        h.wait_fined = true;
                    }
                }
                events.push(SimEvent::FineIssued {
                    rider_id: rid,
                    kind: "wait_timeout".into(),
                    amount_cents: cfg.fine.wait_timeout_cents,
                    reason: "商家出餐超时".into(),
                });
            }
            if remaining > 1 {
                set_remaining(world, rid, oid, remaining - 1);
                return;
            }
            // 出餐完成 → 上路送货
            let road_m = world.order(oid).map(|o| o.distance_m).unwrap_or(0) as f32
                * crate::sim::world::ROAD_FACTOR as f32;
            let travel = ((road_m / speed_mps.max(1.0) / 60.0).ceil() as u32).max(1);
            if let Some(rider) = world.rider_mut(rid)
                && let Some(h) = rider.held_mut(oid)
            {
                h.phase = Phase::ToCustomer;
                h.remaining_min = travel;
                h.awaiting_ack = false;
                h.wait_started_min = now;
            }
            if let Some(order) = world.order_mut(oid) {
                order.status = OrderStatus::PickedUp;
                order.picked_up_at_min = Some(now);
            }
            events.push(SimEvent::OrderPickedUp {
                order_id: oid,
                rider_id: rid,
                prep_minutes: prep,
            });
            // 上路这一刻掷风险
            roll_leg_risk(world, rid, oid, events);
        }
        Phase::ToCustomer => {
            if remaining > 1 {
                set_remaining(world, rid, oid, remaining - 1);
                return;
            }
            if let Some(rider) = world.rider_mut(rid)
                && let Some(h) = rider.held_mut(oid)
            {
                h.phase = Phase::ArrivedCustomer;
                h.remaining_min = 0;
                h.awaiting_ack = is_human;
                h.wait_started_min = now;
            }
            if let Some(rider) = world.rider(rid)
                && let Some(order) = world.order(oid)
            {
                let p = order.customer_pos();
                let _ = rider;
                if let Some(r) = world.rider_mut(rid) {
                    r.set_pos(p);
                }
            }
            if !is_human {
                to_deliver.push((rid, oid));
            }
        }
        Phase::ArrivedCustomer => {
            if let Some(ev) = scheduler::state_event(world, rid) {
                events.push(ev);
            }
        }
    }
    if let Some(ev) = scheduler::state_event(world, rid) {
        events.push(ev);
    }
}

fn set_remaining(world: &mut World, rid: RiderId, oid: OrderId, v: u32) {
    if let Some(rider) = world.rider_mut(rid)
        && let Some(h) = rider.held_mut(oid)
    {
        h.remaining_min = v;
    }
}

fn enter_waiting_food(
    world: &mut World,
    rid: RiderId,
    oid: OrderId,
    prep: u32,
    now: u64,
    events: &mut Vec<SimEvent>,
) {
    if let Some(rider) = world.rider_mut(rid)
        && let Some(h) = rider.held_mut(oid)
    {
        h.phase = Phase::WaitingFood;
        h.remaining_min = prep.max(1);
        h.awaiting_ack = false;
        h.wait_started_min = now;
        h.wait_fined = false;
    }
    if let Some(ev) = scheduler::state_event(world, rid) {
        events.push(ev);
    }
}

/// 上路掷风险：超速被拍（冻结 10 分钟）+ 事故（停工 + 医药费 + 订单回大厅）。
fn roll_leg_risk(world: &mut World, rid: RiderId, oid: OrderId, events: &mut Vec<SimEvent>) {
    let now = world.elapsed_min;
    let cfg = world.cfg.clone();
    let is_peak = cfg.is_peak(world.sim_minute);

    let (mode, ratio, fatigue) = {
        let Some(r) = world.rider(rid) else { return };
        (
            r.speed_mode,
            risk::speed_ratio(r, &cfg, world.weather),
            r.fatigue,
        )
    };

    // 1. 超速被拍
    if risk::roll_camera_fine(&mut world.rng, mode, 1.0, is_peak) {
        world.stats.camera_fines += 1;
        if let Some(r) = world.rider_mut(rid) {
            economy::issue_fine(
                r,
                cfg.fine.camera_cents,
                "camera",
                "超速被电子警察拍到",
                Some(oid),
                now,
            );
            r.frozen_until_min = now + cfg.speed.detain_min as u64;
            economy::adjust_rating(&cfg, r, risk::camera_rating_delta(&cfg, mode));
        }
        events.push(SimEvent::FineIssued {
            rider_id: rid,
            kind: "camera".into(),
            amount_cents: cfg.fine.camera_cents,
            reason: format!("超速被拍，冻结 {} 分钟", cfg.speed.detain_min),
        });
    }

    // 2. 事故
    let Some(outcome) = risk::roll_accident(&mut world.rng, &cfg, ratio, world.weather, fatigue)
    else {
        return;
    };
    world.stats.accidents += 1;

    if let Some(r) = world.rider_mut(rid) {
        economy::issue_fine(
            r,
            outcome.medical_cents,
            "accident",
            format!(
                "交通事故（严重度 {}），停工 {} 分钟",
                outcome.severity, outcome.off_duty_min
            ),
            Some(oid),
            now,
        );
        economy::adjust_rating(&cfg, r, outcome.rating_delta);
        r.injured_until_min = now + outcome.off_duty_min as u64;
        // 手上的单全部退回大厅（必须有人接）
        r.held.clear();
    }
    if let Some(order) = world.order_mut(oid) {
        order.status = OrderStatus::Pending;
        order.assigned_to = None;
        order.picked_up_at_min = None;
        order.claim_deadline_min = now + cfg.claim_window_ticks() as u64;
    }
    events.push(SimEvent::FineIssued {
        rider_id: rid,
        kind: "accident".into(),
        amount_cents: outcome.medical_cents,
        reason: format!("交通事故，停工 {} 分钟", outcome.off_duty_min),
    });
    events.push(SimEvent::OrderBroadcast {
        order: Box::new(world.order(oid).cloned().unwrap_or_else(|| unreachable!())),
        window_ms: cfg.claim_window_secs as u64 * 1000,
        expires_in_ms: cfg.claim_window_secs as u64 * 1000,
    });
    if let Some(ev) = scheduler::state_event(world, rid) {
        events.push(ev);
    }
}

/// 日结：扣保险、生成流水、清零当日计数。
fn close_day(world: &mut World, events: &mut Vec<SimEvent>) {
    let cfg = world.cfg.clone();
    let day = world.day - 1;
    let mut ledgers = Vec::new();

    for r in &mut world.riders {
        let ledger = DayLedger {
            rider_id: r.id,
            key: crate::sim::rider_key(r.id),
            name: r.name.clone(),
            takehome_cents: r.takehome_cents(),
            gross_cents: r.gross_cents,
            fines_cents: r.fines_cents,
            insurance_cents: r.insurance_cents,
            completed: r.completed,
            late_count: r.late_count,
            online_minutes: r.online_minutes,
            hourly_wage_cents: r.hourly_wage_cents(),
            rating: r.rating,
            level: r.level,
            debt_cents: r.debt_cents,
        };
        ledgers.push(ledger);
        let _ = economy::close_day(&cfg, r);
    }

    let total_takehome_cents = ledgers.iter().map(|l| l.takehome_cents).sum();
    let total_orders = ledgers.iter().map(|l| l.completed).sum();
    world.last_day_ledgers = ledgers.clone();
    // 天气剧本每天重放一遍
    world.script_cursor = 0;

    events.push(SimEvent::DayClosed {
        day,
        ledgers,
        total_takehome_cents,
        total_orders,
    });
}
