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
    host::{self, HostMode},
    order::OrderStatus,
    rider::{Phase, Rider, RiderKind},
    risk, scheduler,
    world::{Redispatch, World},
};

/// tick 的外部输入。**IO 的结果只能从这里进来。**
#[derive(Debug, Clone, Default)]
pub struct TickInput {
    /// app 层异步拿到的 LLM 计划（可能为 None）。
    pub llm_plan: Option<LlmPlan>,
    /// 托管模式。`Off` = AI 全停机，这一 tick 不派单、也不需要谁去点按钮。
    /// 默认 [`HostMode::SimulatedOnly`]，所以不开托管模式时行为与从前一致。
    pub host_mode: HostMode,
    /// 本 tick **有活人连着 WS** 的骑手（hub 给）。**不缓存**：玩家一断线，
    /// 下一 tick 的 [`host::ai_should_drive`] 就该让 AI 接手。
    pub live_riders: Vec<RiderId>,
}

impl TickInput {
    /// 这个骑手这一 tick 由 AI 代管吗？判定只有 `host::ai_should_drive` 一条。
    pub fn ai_hosts(&self, rider: &Rider) -> bool {
        host::ai_should_drive(rider, self.live_riders.contains(&rider.id), self.host_mode)
    }
}

/// 推进一个 tick。
pub fn tick(world: &mut World, mut input: TickInput) -> Vec<SimEvent> {
    let mut events = Vec::new();
    // 0. 消费外部输入
    let ai_off = input.host_mode == HostMode::Off;
    // 托管模式「全停机」：AI 彻底不碰这一 tick（计划也一并丢掉，免得下一轮补派）。
    world.llm_plan = if ai_off { None } else { input.llm_plan.take() };

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

    // 4. 抢单窗口过期的单：加价回大厅；重投用尽就强制塞给最近的骑手。
    //
    // **不允许流单**：这里不会产生 `order_expired`。真派不出去（骑手全离线/全受伤）时
    // 订单继续留在大厅等着，进入下一轮。
    //
    // 救单是 AI 动作，所以 `HostMode::Off`（AI 全停机）下不做——那时订单就在大厅里等着，
    // 由玩家自己抢；它依然不会过期。
    let may_rescue = input.host_mode != HostMode::Off;
    for id in world.stale_pending_orders() {
        if world.release_to_hall(id) != Some(Redispatch::NeedsRescue) {
            continue;
        }
        if !may_rescue {
            continue;
        }
        if let Some(a) = scheduler::rescue_nearest(world, id) {
            events.push(scheduler::claimed_event(world, &a));
            // 救单也要进决策流：它不是 LLM 决策，控制台必须能一眼分清。
            let rider_name = world
                .rider(a.rider)
                .map(|r| r.name.clone())
                .unwrap_or_default();
            events.push(SimEvent::AiDecision {
                source: DecisionSource::AlgorithmRescue,
                layer: "algorithm",
                assignments: vec![a.to_event()],
                ops: Vec::new(),
                decision_id: 0,
                note: format!("超时救单：#{id} 重投用尽，强制塞给 {rider_name}"),
            });
        }
    }

    // 5. 骑手行程推进 + 风险
    advance_riders(world, &mut events, &input);

    // 6. AI 分配
    if world.cfg.ai_batch_enabled && !ai_off {
        run_ai(world, &mut events, &input);
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
///
/// 派单对象走 [`host::ai_idle_riders`]——**在线的真人不在名单里**（AI 让位），
/// 掉线的真人在 AutoPilot 下才在名单里（AI 接手）。
fn run_ai(world: &mut World, events: &mut Vec<SimEvent>, input: &TickInput) {
    let pending = world.pending_orders();
    let idle = host::ai_idle_riders(world, input.host_mode, |id| input.live_riders.contains(&id));
    if pending.is_empty() || idle.is_empty() {
        world.llm_plan = None;
        return;
    }

    let plan = world.llm_plan.take();
    // (分配、来源、层、决策 id、说明)
    let (assignments, source, layer, decision_id, note) = match plan {
        Some(p) if world.cfg.llm_brain_enabled => {
            let verdict = ai::validate_llm_plan(world, &p, &idle, &pending);
            if verdict.assignments.is_empty() {
                // LLM 一条都没通过校验 → 算法兜底
                world.stats.llm_fallbacks += 1;
                (
                    ai::batch_decide(world, &pending, &idle),
                    DecisionSource::AlgorithmFallback,
                    "rider",
                    0,
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
                (
                    verdict.assignments,
                    DecisionSource::Llm,
                    "rider",
                    p.decision_id,
                    note,
                )
            }
        }
        _ => (
            ai::batch_decide(world, &pending, &idle),
            DecisionSource::Algorithm,
            "algorithm",
            0,
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
        layer,
        assignments: assignments.iter().map(|a| a.to_event()).collect(),
        ops: Vec::new(),
        decision_id,
        note,
    });
}

/// 骑手行程推进。这是状态机唯一的推进入口。
fn advance_riders(world: &mut World, events: &mut Vec<SimEvent>, input: &TickInput) {
    let ids: Vec<RiderId> = world.riders.iter().map(|r| r.id).collect();
    let mut to_deliver: Vec<(RiderId, OrderId)> = Vec::new();

    for rid in ids {
        let held_ids: Vec<OrderId> = match world.rider(rid) {
            Some(r) => r.held_ids(),
            None => continue,
        };
        for oid in held_ids {
            step_one(world, rid, oid, &mut to_deliver, events, input);
        }
    }

    for (rid, oid) in to_deliver {
        if let Ok(evs) = scheduler::deliver(world, rid, oid) {
            events.extend(evs)
        }
    }
}

/// 推进单个 (骑手, 手持单)。
///
/// `needs_click` = 这一步要真人点按钮（取餐 / 送达）。它**不等于**「是不是真人」：
/// 掉线的真人在 AutoPilot 下由 AI 代管，AI 替他点，所以不该卡在那里等（§6.2）。
fn step_one(
    world: &mut World,
    rid: RiderId,
    oid: OrderId,
    to_deliver: &mut Vec<(RiderId, OrderId)>,
    events: &mut Vec<SimEvent>,
    input: &TickInput,
) {
    let now = world.elapsed_min;
    let weather = world.weather;
    let cfg = world.cfg.clone();

    let (phase, remaining, awaiting, prep, needs_click, speed_mps, target) = {
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
            rider.kind == RiderKind::Human && !input.ai_hosts(rider),
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

    // 真人骑手在倒计时归零后必须点按钮，兜底 10 分钟自动推进。
    // AI 代管的骑手（含 AutoPilot 下掉线的真人）不看这个闸门，直接往下走。
    if awaiting && needs_click {
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
            if needs_click {
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
            // 出餐完成 → 上路送货（骑行 + 顾客端步行往返，与 plan_for 同源）
            let travel = world
                .rider(rid)
                .and_then(|r| world.order(oid).map(|o| ai::customer_leg_min(world, r, o)))
                .unwrap_or_else(|| {
                    let road_m = world.order(oid).map(|o| o.distance_m).unwrap_or(0) as f32
                        * crate::sim::world::ROAD_FACTOR as f32;
                    ((road_m / speed_mps.max(1.0) / 60.0).ceil() as u32).max(1)
                });
            if let Some(rider) = world.rider_mut(rid)
                && let Some(h) = rider.held_mut(oid)
            {
                h.set_leg(Phase::ToCustomer, travel);
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
                h.leg_total_min = 1;
                h.awaiting_ack = needs_click;
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
            if !needs_click {
                to_deliver.push((rid, oid));
            }
        }
        Phase::ArrivedCustomer => {
            // 到门口了：玩家要自己点「送达」，AI 代管时替他按下这个按钮。
            if !needs_click {
                to_deliver.push((rid, oid));
            }
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
        h.set_leg(Phase::WaitingFood, prep.max(1));
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
    // 手上的单退回大厅：走统一的回流出口，别在这里自己拼状态。
    // （不累加重投次数——骑手是出事故进医院，不是拒单。这单迟早会有人接。）
    world.return_to_hall_uncharged(oid);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::config::SimConfig;

    fn is_ai_decision(e: &SimEvent) -> bool {
        matches!(e, SimEvent::AiDecision { .. })
    }

    /// 托管模式「全停机」：同样的世界，默认会派单，`Off` 则不派。
    #[test]
    fn ai_off_skips_the_whole_ai_pass() {
        // 用 ai.rs 里验证过「贪心一定会派出单」的那套参数，免得测试前提飘。
        let cfg = SimConfig {
            sim_rider_count: 4,
            ..Default::default()
        };
        let mut on = World::new_for_test(cfg.clone(), 42);
        on.generate_order_batch(4);
        let mut off = World::new_for_test(cfg, 42);
        off.generate_order_batch(4);

        let ev_on = tick(&mut on, TickInput::default());
        let ev_off = tick(
            &mut off,
            TickInput {
                host_mode: HostMode::Off,
                ..Default::default()
            },
        );

        assert!(
            ev_on.iter().any(is_ai_decision),
            "默认必须有 AI 分配（前提：有单 + 有空闲骑手）"
        );
        assert!(
            !ev_off.iter().any(is_ai_decision),
            "全停机时一条 AI 分配都不该有"
        );
        assert_eq!(off.pending_orders().len(), 4, "没人接的单要原样留在大厅");
    }

    /// 全停机也要把上一轮拿到的 LLM 计划丢掉，否则它会在下一轮偷偷被消费。
    #[test]
    fn ai_off_drops_an_incoming_plan() {
        let mut w = World::new_for_test(SimConfig::default(), 3);
        let plan = LlmPlan {
            decision_id: 42,
            ..Default::default()
        };
        tick(
            &mut w,
            TickInput {
                llm_plan: Some(plan),
                host_mode: HostMode::Off,
                ..Default::default()
            },
        );
        assert!(w.llm_plan.is_none(), "停机时计划要当场作废");
    }

    /// 只有一个掉线的真人：SimulatedOnly 时 AI 不碰他，AutoPilot 时 AI 接手。
    #[test]
    fn autopilot_assigns_to_an_offline_human_but_simulated_only_does_not() {
        let cfg = SimConfig {
            sim_rider_count: 0,
            ..Default::default()
        };
        let mut w = World::new_for_test(cfg, 11);
        let human = w.spawn_human_rider("张三");
        w.rider_mut(human).unwrap().online = false;
        w.generate_order_batch(2);

        let ev = tick(&mut w, TickInput::default());
        assert!(
            !ev.iter().any(is_ai_decision),
            "SimulatedOnly：他掉线了，AI 不能替他接单"
        );
        assert!(w.rider(human).unwrap().held.is_empty());

        let ev = tick(
            &mut w,
            TickInput {
                host_mode: HostMode::AutoPilot,
                ..Default::default()
            },
        );
        let Some(SimEvent::AiDecision { assignments, .. }) = ev.iter().find(|e| is_ai_decision(e))
        else {
            panic!("AutoPilot：AI 该接过他的单");
        };
        assert!(!assignments.is_empty());
    }

    /// AutoPilot 的离线真人：取餐 / 送达这些「要真人点按钮」的关口由 AI 代按，
    /// 所以那张单能一路送到头，而不是卡在待确认上。
    #[test]
    fn autopilot_drives_an_offline_human_all_the_way_to_delivery() {
        let cfg = SimConfig {
            sim_rider_count: 0,
            ..Default::default()
        };
        let mut w = World::new_for_test(cfg, 11);
        let human = w.spawn_human_rider("张三");
        w.rider_mut(human).unwrap().online = false;
        w.generate_order_batch(2);

        let mut delivered = false;
        for _ in 0..600 {
            let evs = tick(
                &mut w,
                TickInput {
                    host_mode: HostMode::AutoPilot,
                    ..Default::default()
                },
            );
            if evs
                .iter()
                .any(|e| matches!(e, SimEvent::OrderDelivered { .. }))
            {
                delivered = true;
                break;
            }
        }
        assert!(delivered, "AutoPilot 该把掉线真人的单送到头");
    }

    /// 真人在线时 AI 必须让位：哪怕他手上没单，也不进 AI 的派单名单。
    #[test]
    fn an_online_human_is_left_alone_by_the_ai() {
        let cfg = SimConfig {
            sim_rider_count: 0,
            ..Default::default()
        };
        let mut w = World::new_for_test(cfg, 11);
        let human = w.spawn_human_rider("张三");
        w.rider_mut(human).unwrap().online = true;
        w.generate_order_batch(2);

        let ev = tick(
            &mut w,
            TickInput {
                host_mode: HostMode::AutoPilot,
                live_riders: vec![human],
                ..Default::default()
            },
        );
        assert!(
            !ev.iter().any(is_ai_decision),
            "玩家自己在线，AI 不许替他接单"
        );
        assert!(w.rider(human).unwrap().held.is_empty());
    }

    // -----------------------------------------------------------------
    // LLM 决策 vs AI 决策：来源必须泾渭分明
    // -----------------------------------------------------------------

    /// 造一个有单、有空闲骑手的 LLM 大脑开着世界，返回 (世界, 第一条合法计划)。
    fn llm_world(seed: u64) -> (World, LlmPlan) {
        let cfg = SimConfig {
            sim_rider_count: 4,
            llm_brain_enabled: true,
            ..Default::default()
        };
        let mut w = World::new_for_test(cfg, seed);
        w.generate_order_batch(4);

        let rider_id = w.riders[0].id;
        let order_id = w.pending_orders()[0];
        let plan = LlmPlan {
            assignments: vec![ai::LlmAssign {
                rider_id: format!("r{rider_id}"),
                order_id,
                reason: "这条最顺路".into(),
            }],
            model: "gpt-4o-mini".into(),
            latency_ms: 120,
            note: String::new(),
            decision_id: 77,
        };
        (w, plan)
    }

    /// 合法 LLM 计划：来源必须标成 `Llm`，带上决策 id，并保留模型给出的理由。
    #[test]
    fn a_well_formed_llm_plan_is_reported_as_llm() {
        let (mut w, plan) = llm_world(42);
        let evs = tick(
            &mut w,
            TickInput {
                llm_plan: Some(plan),
                ..Default::default()
            },
        );

        let Some(SimEvent::AiDecision {
            source,
            layer,
            assignments,
            decision_id,
            note,
            ..
        }) = evs.iter().find(|e| is_ai_decision(e))
        else {
            panic!("LLM 给了合法计划，必须有一条决策");
        };
        assert_eq!(*source, DecisionSource::Llm);
        assert!(source.is_llm(), "来源标成 LLM 才允许算「LLM 决策」");
        assert_eq!(*layer, "rider", "骑手层的计划就该报骑手层");
        assert_eq!(*decision_id, 77, "决策 id 要原样透传给控制台否决");
        assert_eq!(assignments.len(), 1);
        assert!(assignments[0].reason.starts_with("[LLM]"));
        assert!(note.contains("gpt-4o-mini"), "理由里要带上用的模型：{note}");
        assert_eq!(w.stats.llm_calls, 1);
        assert_eq!(w.stats.llm_fallbacks, 0);
    }

    /// LLM 输出全是垃圾：一条都不能落地，来源必须标成「算法兜底」而非 LLM。
    #[test]
    fn a_garbage_llm_plan_falls_back_to_the_algorithm() {
        let (mut w, _) = llm_world(42);
        let garbage = LlmPlan {
            assignments: vec![ai::LlmAssign {
                rider_id: "r999".into(),
                order_id: 99999,
                reason: String::new(),
            }],
            model: "gpt-4o-mini".into(),
            decision_id: 88,
            ..Default::default()
        };
        let evs = tick(
            &mut w,
            TickInput {
                llm_plan: Some(garbage),
                ..Default::default()
            },
        );

        let Some(SimEvent::AiDecision {
            source,
            decision_id,
            ..
        }) = evs.iter().find(|e| is_ai_decision(e))
        else {
            panic!("兜底算法也该有一条决策");
        };
        assert_eq!(*source, DecisionSource::AlgorithmFallback);
        assert!(!source.is_llm(), "兜底不是 LLM 决策");
        assert_eq!(*decision_id, 0, "兜底不可否决");
        assert_eq!(w.stats.llm_fallbacks, 1);
        assert_eq!(w.stats.llm_calls, 0);
    }

    /// 大脑开关关着时，就算手里攥着 LLM 计划也不许用——来源必须是纯算法。
    #[test]
    fn a_plan_is_ignored_when_the_brain_switch_is_off() {
        let (mut w, plan) = llm_world(42);
        w.cfg.llm_brain_enabled = false;
        let evs = tick(
            &mut w,
            TickInput {
                llm_plan: Some(plan),
                ..Default::default()
            },
        );

        let Some(SimEvent::AiDecision { source, .. }) = evs.iter().find(|e| is_ai_decision(e))
        else {
            panic!("关掉 LLM 后算法仍要派单");
        };
        assert_eq!(*source, DecisionSource::Algorithm);
        assert!(!source.is_llm());
    }

    /// `is_llm` 只承认真模型：救单、兜底、人工、托管都不算 LLM 决策。
    #[test]
    fn only_the_real_model_counts_as_an_llm_decision() {
        assert!(DecisionSource::Llm.is_llm());
        for other in [
            DecisionSource::Algorithm,
            DecisionSource::AlgorithmFallback,
            DecisionSource::AlgorithmRescue,
            DecisionSource::Manual,
            DecisionSource::AiHosted,
        ] {
            assert!(!other.is_llm(), "{:?} 不该被算成 LLM 决策", other);
        }
    }
}
