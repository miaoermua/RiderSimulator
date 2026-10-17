//! 固定 tick 循环。**时间权威只在这里。**
//!
//! 1 tick = 1 模拟分钟。时间倍率只改 tick 间隔（在 app 层），不改这里。
//! 所有截止时间用 `elapsed_min`（单调），展示用 `sim_minute`（当日 0:00 起算）。

use super::{
    OrderId, RiderId,
    ai::{self, LlmPlan},
    chat, economy,
    event::{DayLedger, DecisionSource, SimEvent},
    geo::LatLng,
    order::OrderStatus,
    rider::{Phase, RiderKind},
    risk,
    route::{LegKind, RouteLeg, RouteTask},
    scheduler,
    world::{Redispatch, World},
};

/// tick 的外部输入。**IO 的结果只能从这里进来。**
///
/// 这里放的是**结论**不是事实：外圈知道网络和托管模式，`sim` 只知道
/// 「这一 tick 谁归 AI 管」。所以 [`crate::human`] 的枚举一个都不出现在本文件里。
#[derive(Debug, Clone, Default)]
pub struct TickInput {
    /// app 层异步拿到的 LLM 计划（可能为 None）。
    pub llm_plan: Option<LlmPlan>,
    /// AI 这一 tick 是不是**全停机**。
    ///
    /// `true` 时不派单、不救单、不留计划——纯手动跑单。
    pub ai_off: bool,
    /// 本 tick **交给 AI 代管的真人**。
    ///
    /// 只列真人：模拟骑手天生归 AI 管，不用外圈再报一遍。**空 = 只托管模拟骑手**
    /// （与旧的 `SimulatedOnly` 默认逐字等价）；列进来的那几位意味着 AI 代他操作
    /// （旧的 `AutoPilot` 下的掉线真人）。
    ///
    /// **不缓存**：玩家一断线，下一 tick 这份名单就该变。
    pub hosted_humans: Vec<RiderId>,
}

impl TickInput {
    /// 这一 tick 由 AI 替这个骑手操作吗？只对真人有意义。
    pub fn hosts(&self, rid: RiderId) -> bool {
        self.hosted_humans.contains(&rid)
    }
}

/// 推进一个 tick。
pub fn tick(world: &mut World, mut input: TickInput) -> Vec<SimEvent> {
    let mut events = Vec::new();
    // 0. 消费外部输入
    let ai_off = input.ai_off;
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
    // 救单是 AI 动作，所以 AI 全停机（`ai_off`）下不做——那时订单就在大厅里等着，
    // 由玩家自己抢；它依然不会过期。
    let may_rescue = !ai_off;
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

/// 本 tick 归 AI 管的空闲骑手。**这是名单唯一的算法。**
///
/// 判定顺序：
/// - [`TickInput::ai_off`] → 一个都不给（纯手动）。
/// - 模拟骑手 → 天生归 AI。
/// - 真人 → 只有外圈在 [`TickInput::hosted_humans`] 里明确交出来的才算；在线的真人
///   一律让位（§6.3「骑手 AI 停止输出决策」）。
///
/// 「能不能接单」用 `is_free` 而不是 `is_idle`：后者含在线判定，会把 AutoPilot 下
/// 该被接管的掉线真人挡在外面。
///
/// `pub` 是给 app 拼 LLM 提示词用的——**控制端看到的名单和 tick 实际派的名单
/// 必须同源**，否则「LLM 推荐的人」和「校验时合法的人」会对不上。
pub fn ai_roster(world: &World, input: &TickInput) -> Vec<RiderId> {
    if input.ai_off {
        return Vec::new();
    }
    world
        .riders
        .iter()
        .filter(|r| r.is_free(world.elapsed_min, &world.cfg))
        .filter(|r| match r.kind {
            RiderKind::Human => input.hosts(r.id),
            RiderKind::Simulated => true,
        })
        .map(|r| r.id)
        .collect()
}

/// AI 分配：优先用 LLM 计划（经校验），否则算法贪心。
///
/// 派单对象走 [`ai_roster`]——**在线的真人不在名单里**（AI 让位），
/// 掉线的真人在 AutoPilot 下才在名单里（AI 接手）。
fn run_ai(world: &mut World, events: &mut Vec<SimEvent>, input: &TickInput) {
    let pending = world.pending_orders();
    let idle = ai_roster(world, input);
    // 「在店门口等餐」的骑手不算空闲，但同店新单能顺手塞给他们（凑单窗口）。
    // 所以不能只看 `idle`：一屋子人全在等餐时这里要是提前 return，
    // 凑单就永远不会发生——而那正是这套机制要覆盖的场面。
    let merging = ai::merge_riders(world);
    if pending.is_empty() || (idle.is_empty() && merging.is_empty()) {
        world.llm_plan = None;
        return;
    }

    let plan = world.llm_plan.take();
    // (分配、来源、层、决策 id、说明)
    let (assignments, source, layer, decision_id, note) = match plan {
        // 凑单是算法自己的事：LLM 拿到的名单里没有「等餐中」的人，
        // 硬走它的计划只会把兜底说明写成「输出全被丢弃」，误导人。
        Some(p) if world.cfg.llm_brain_enabled && !idle.is_empty() => {
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
            format!(
                "贪心：{} 空闲骑手 / {} 待抢单{}",
                idle.len(),
                pending.len(),
                if merging.is_empty() {
                    String::new()
                } else {
                    format!("，另有 {} 人在店等餐可凑单", merging.len())
                }
            ),
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
            assign_mode: None,
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

/// 骑手到了一处取餐点：把该地点**所有**取餐腿一次性出队。
///
/// 同店多单本来就在同一处取餐，人到店了这批餐就都算「已到取餐点」。若只弹当前
/// 那条，第二单还得再空耗一遍到店时长——人明明已经站在店里。
fn complete_pickup_legs(world: &mut World, rid: RiderId) {
    if let Some(rider) = world.rider_mut(rid) {
        rider.complete_pickup_legs_at_current();
    }
}

/// 出餐上路：给这单挂一条送达腿，并把整队的送达段按「就近优先」重排。
///
/// 排序从骑手**当前位置**起算，而不是从商家起算——多单共处一车时先送谁，
/// 取决于人此刻在哪，这才是「顺路」的定义。
fn push_dropoff_leg(world: &mut World, rid: RiderId, oid: OrderId, travel_min: u32) {
    let Some(dest) = world.order(oid).map(|o| o.customer_pos()) else {
        return;
    };
    let Some(rider) = world.rider_mut(rid) else {
        return;
    };
    let leg = RouteLeg::new(oid, LegKind::Dropoff, dest, travel_min);
    match rider.route.as_mut() {
        Some(route) => route.push_leg(leg),
        None => rider.route = Some(RouteTask::from_leg(leg)),
    }
    let cursor = rider.pos();
    if let Some(route) = rider.route.as_mut() {
        route.sort_dropoffs_nearest(cursor);
    }
}

/// 推进单个 (骑手, 手持单)。
///
/// `needs_click` = 这一步要真人点按钮（取餐 / 送达）。它**不等于**「是不是真人」：
/// 掉线的真人在 AutoPilot 下由 AI 代管（外圈已把他列进
/// [`TickInput::hosted_humans`]），AI 替他点，所以不该卡在那里等（§6.2）。
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

    let (phase, remaining, awaiting, prep, needs_click, _speed_mps, target) = {
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
            rider.kind == RiderKind::Human && !input.hosts(rider.id),
            rider.speed_mps(&cfg, weather),
            target,
        )
    };

    // 位置推进：每 tick 走 1/(remaining+1) 的距离，客户端看到的是连续移动。
    //
    // **只有当前腿动。** 一个骑手同一时刻只有一条腿在走；多单共处一车时若每单
    // 各推一次，同一 tick 内会互相拉扯（`held` 里最后遍历到的那单说了算），
    // 观感就是抖动与「瞬移」。不是当前腿的单直接冻结——人还没上路去它那儿，
    // 连倒计时都不该走。（等餐段不在此列，那边的并行等餐另算。）
    if matches!(phase, Phase::ToShop | Phase::ToCustomer) {
        if !world.rider(rid).is_some_and(|r| r.is_current_leg(oid)) {
            return;
        }
        if let Some(rider) = world.rider_mut(rid) {
            let from = rider.pos();
            let frac = 1.0 / (remaining as f64 + 1.0);
            rider.set_pos(LatLng::new(
                from.lat + (target.lat - from.lat) * frac,
                from.lng + (target.lng - from.lng) * frac,
            ));
        }
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
                // 同店同批：先把这一炉的出餐时间对齐，再进等餐状态。
                let prep = merge_prep_for_shop(world, rid, oid, prep);
                enter_waiting_food(world, rid, oid, prep, now, events);
                // 到店 = 取餐腿跑完。同店多单一起出队，别让第二单再排一次到店。
                complete_pickup_legs(world, rid);
            }
        }
        Phase::WaitingFood => {
            // 实时出餐单（控制台可填秒，默认 30s）：秒级判定交给 `pump_waiting`，
            // 这里不再按模拟分钟一格一格推进，否则 30 秒的单会被 tick 抢跑成 1 分钟。
            let real_time_prep = world.order(oid).and_then(|o| o.prep_seconds).is_some()
                && world
                    .rider(rid)
                    .and_then(|r| r.held.iter().find(|h| h.order_id == oid))
                    // 没记到墙钟起点（老存档）就还是按分钟走，别把单卡死。
                    .is_some_and(|h| h.wait_started_wall_ms > 0);
            if world.rider(rid).is_some_and(|r| r.kind == RiderKind::Human) && real_time_prep {
                return;
            }
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
                        &cfg,
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
            finish_waiting(world, rid, oid, prep, now, events);
        }
        Phase::ToCustomer => {
            if remaining > 1 {
                set_remaining(world, rid, oid, remaining - 1);
                return;
            }
            // 到门口：先掷末端放置窗口，再掷一次「顾客会不会开口」。
            // 两件事都走 `world.rng`，与种子绑定、可复现（玩法设计 §九）。
            let (window, chat_open) = arrival_roll(world, rid, oid);
            if let Some(rider) = world.rider_mut(rid)
                && let Some(h) = rider.held_mut(oid)
            {
                h.phase = Phase::ArrivedCustomer;
                h.remaining_min = 0;
                h.leg_total_min = 1;
                h.awaiting_ack = needs_click;
                h.wait_started_min = now;
                h.arrive_at_min = Some(now);
                h.deliver_window_min = window;
                if chat_open.is_some() {
                    h.chat_opened = true;
                }
            }
            if let Some(order) = world.order(oid) {
                let p = order.customer_pos();
                if let Some(r) = world.rider_mut(rid) {
                    r.set_pos(p);
                }
            }
            if let Some((context, fallback)) = chat_open {
                // 顾客开口是 IO 前的事：这里只报「要开口」，
                // 「说什么、要不要索赔」由 app 层拿走去调 LLM。
                events.push(SimEvent::ChatOpen {
                    order_id: oid,
                    rider_id: rid,
                    context,
                    fallback,
                });
            }
            // **注意**：这里不再立刻 `to_deliver`。末端窗口的推进挪到下一 tick 的
            // `Phase::ArrivedCustomer` 分支——AI 托管也要停够窗口时间，
            // 否则「到达即送达」，顾客根本没机会说话。
        }
        Phase::ArrivedCustomer => {
            // 到门口了。窗口走完才算「放置完成」：
            // - 真人骑手：窗口内自己点「送达」（`awaiting_ack` 闸门在函数开头管着）；
            // - AI 托管：不看闸门，但要等窗口到点。
            let placed = world
                .rider(rid)
                .and_then(|r| r.held.iter().find(|h| h.order_id == oid))
                .map(|h| match h.arrive_at_min {
                    Some(t) => now.saturating_sub(t) >= h.deliver_window_min as u64,
                    // 老存档 / 手工构造的手持单没有到达时刻：不拦，免得卡死。
                    None => true,
                })
                .unwrap_or(true);
            if placed && !needs_click {
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

/// 同店同批的餐一起出：把这一炉的等待时间对齐，但**给「一起等」设个上限**。
///
/// 为什么对齐：同一家店的几单出自同一个灶台。若让先到点的那单单独出餐上路，
/// 骑手就拎着半批餐走，剩下那单还得把到店、等餐再走一遍——两头都不像话。
///
/// 为什么有上限：也不能让一份慢餐把已经好了的餐压在那儿干等，那是拿顾客的
/// 时间补贴「一起送」。超过 `merge_wait_max_min` 就各出各的。
///
/// 返回这一炉对齐后的等待分钟（给当前这单用），并顺手把兄弟单的剩余时间改齐。
fn merge_prep_for_shop(world: &mut World, rid: RiderId, oid: OrderId, prep: u32) -> u32 {
    let Some(merchant_id) = world.order(oid).map(|o| o.merchant_id) else {
        return prep;
    };
    let cap = world.cfg.backorder.merge_wait_max_min;
    if cap == 0 {
        return prep;
    }

    // 同店、且还在等餐的兄弟单：已上路的和别家店的不算。
    let siblings: Vec<(OrderId, u32)> = world
        .rider(rid)
        .map(|r| {
            r.held
                .iter()
                .filter(|h| h.order_id != oid && h.phase == Phase::WaitingFood)
                .map(|h| (h.order_id, h.remaining_min))
                .collect()
        })
        .unwrap_or_default();
    let same_shop: Vec<(OrderId, u32)> = siblings
        .into_iter()
        .filter(|(s, _)| {
            world
                .order(*s)
                .is_some_and(|o| o.merchant_id == merchant_id)
        })
        .collect();
    if same_shop.is_empty() {
        return prep;
    }

    let waits = same_shop
        .iter()
        .map(|(_, left)| *left)
        .chain(std::iter::once(prep));
    let earliest = waits.clone().min().unwrap_or(prep);
    let latest = waits.max().unwrap_or(prep);
    // 一起出，但最多让最早好的那份多等 cap 分钟。
    let aligned = latest.min(earliest.saturating_add(cap)).max(1);

    if let Some(rider) = world.rider_mut(rid) {
        for (s, _) in &same_shop {
            if let Some(h) = rider.held_mut(*s) {
                h.remaining_min = aligned;
            }
        }
    }
    aligned
}

fn enter_waiting_food(
    world: &mut World,
    rid: RiderId,
    oid: OrderId,
    prep: u32,
    now: u64,
    events: &mut Vec<SimEvent>,
) {
    // 先抄墙钟：下面要可变借 rider，不能再读 world。
    let wall = world.wall_ms;
    if let Some(rider) = world.rider_mut(rid)
        && let Some(h) = rider.held_mut(oid)
    {
        h.set_leg(Phase::WaitingFood, prep.max(1));
        h.awaiting_ack = false;
        h.wait_started_min = now;
        h.wait_started_wall_ms = wall;
        h.wait_fined = false;
    }
    if let Some(ev) = scheduler::state_event(world, rid) {
        events.push(ev);
    }
}

/// 出餐完成 → 上路送货。分钟级 [`tick`] 与秒级 [`pump_waiting`] 共用。
fn finish_waiting(
    world: &mut World,
    rid: RiderId,
    oid: OrderId,
    prep: u32,
    now: u64,
    events: &mut Vec<SimEvent>,
) {
    let travel = world
        .rider(rid)
        .and_then(|r| world.order(oid).map(|o| ai::customer_leg_min(world, r, o)))
        .unwrap_or_else(|| {
            // `distance_m` 已是路网距离，别再乘 ROAD_FACTOR（与 ai::customer_leg_min 同源）。
            let road_m = world.order(oid).map(|o| o.distance_m).unwrap_or(0) as f32;
            let speed_mps = world
                .rider(rid)
                .map(|r| r.speed_mps(&world.cfg, world.weather))
                .unwrap_or(1.0);
            ((road_m / speed_mps.max(1.0) / 60.0).ceil() as u32).max(1)
        });
    if let Some(rider) = world.rider_mut(rid)
        && let Some(h) = rider.held_mut(oid)
    {
        h.set_leg(Phase::ToCustomer, travel);
        h.awaiting_ack = false;
        h.wait_started_min = now;
    }
    // 出餐 = 生成送达腿。多单同店时第二条腿追加进同一串路线，
    // 再按「就近优先」重排——这就是「一起送」的排序落点。
    push_dropoff_leg(world, rid, oid, travel);
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

/// 秒级出餐推进：只处理真人、且带 `prep_seconds` 的等餐单。
///
/// 世界时钟仍由 [`tick`] 推进；这里用**真实墙钟**判断「出餐时间到没到」，
/// 不推进任何分钟级状态——所以 30 秒的单真的 30 秒出餐，不会被
/// 「一 tick = 一模拟分钟」四舍五入成 1 分钟。
///
/// 暂停或速度归零时不动：世界冻住，真实秒也不该偷跑。
pub fn pump_waiting(world: &mut World, wall_ms: u64, events: &mut Vec<SimEvent>) {
    world.wall_ms = wall_ms;
    if world.cfg.speed_multiplier == 0 {
        return;
    }
    let now = world.elapsed_min;
    // 先收集再改：判定只需要只读借用，动手时才拿可变借用。
    let mut waiting: Vec<(RiderId, OrderId, u64)> = Vec::new();
    for r in &world.riders {
        if r.kind != RiderKind::Human {
            continue;
        }
        for h in &r.held {
            if h.phase == Phase::WaitingFood && h.wait_started_wall_ms > 0 {
                waiting.push((r.id, h.order_id, h.wait_started_wall_ms));
            }
        }
    }
    let due: Vec<(RiderId, OrderId, u32)> = waiting
        .into_iter()
        .filter_map(|(rid, oid, started)| {
            let secs = world.order(oid).and_then(|o| o.prep_seconds)?;
            let waited_ms = wall_ms.saturating_sub(started);
            (waited_ms >= secs as u64 * 1000).then_some((rid, oid, secs))
        })
        .collect();
    for (rid, oid, secs) in due {
        // 事件里的 `prep_minutes` 是分钟口径，秒级单向上取整到分钟。
        let prep = secs.div_ceil(60).max(1);
        finish_waiting(world, rid, oid, prep, now, events);
    }
}

/// 到门口那一下：掷末端放置窗口 + 掷「顾客会不会开口」。
///
/// **纯计算**，随机性全部来自 `world.rng`（绑世界种子，可复现、零 token）。
/// 返回 `(窗口分钟, 顾客要开口时的画像摘要与兜底开场白)`。
fn arrival_roll(world: &mut World, rid: RiderId, oid: OrderId) -> (u32, Option<(String, String)>) {
    let window = chat::roll_window(&mut world.rng);

    // 先把画像抄出来再动 rng：`world.rider()` 是只读借用，
    // 和 `world.rng` 的可变借用不能同时活着。
    let profile = {
        let rider = world.rider(rid);
        let order = world.order(oid);
        match (rider, order) {
            (Some(r), Some(o)) => {
                let signature = world
                    .merchant(o.merchant_id)
                    .map(|m| m.signature.clone())
                    .unwrap_or_default();
                let buffer = if o.back_order {
                    world.cfg.backorder.late_buffer_min
                } else {
                    0
                };
                Some((
                    r.rating,
                    world.sim_minute,
                    signature,
                    o.remark.clone(),
                    o.is_late(world.elapsed_min, buffer),
                ))
            }
            _ => None,
        }
    };
    let Some((rider_rating, sim_minute, signature, remark, late)) = profile else {
        return (window, None);
    };

    let ctx = chat::ChatContext {
        signature,
        remark,
        rider_rating,
        sim_minute,
        late,
    };
    if chat::should_open(&mut world.rng, &ctx) {
        let fallback = chat::opening_line(&ctx).to_string();
        (window, Some((ctx.brief(), fallback)))
    } else {
        (window, None)
    }
}

/// 上路掷风险：超速被拍（冻结 10 分钟）+ 事故（停工 + 医药费 + 订单回大厅）。
fn roll_leg_risk(world: &mut World, rid: RiderId, oid: OrderId, events: &mut Vec<SimEvent>) {
    let now = world.elapsed_min;
    let cfg = world.cfg.clone();
    let is_peak = cfg.is_peak(world.sim_minute);

    let (mode, fatigue) = {
        let Some(r) = world.rider(rid) else { return };
        (r.speed_mode, r.fatigue)
    };

    // 1. 超速被拍
    if risk::roll_camera_fine(&mut world.rng, mode, 1.0, is_peak) {
        world.stats.camera_fines += 1;
        if let Some(r) = world.rider_mut(rid) {
            economy::issue_fine(
                r,
                &cfg,
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
    let Some(outcome) = risk::roll_accident(&mut world.rng, &cfg, mode, world.weather, fatigue)
    else {
        return;
    };
    world.stats.accidents += 1;

    if let Some(r) = world.rider_mut(rid) {
        economy::issue_fine(
            r,
            &cfg,
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
                ai_off: true,
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
                ai_off: true,
                ..Default::default()
            },
        );
        assert!(w.llm_plan.is_none(), "停机时计划要当场作废");
    }

    /// 只有一个掉线的真人：外圈不把他交出来时 AI 不碰他，交出来（AutoPilot）时 AI 接手。
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
                hosted_humans: vec![human],
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
                    hosted_humans: vec![human],
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

    /// 一个骑手身上压着**两家店**的单时，只有当前那条腿在动。
    ///
    /// 这是「多单不再互相拉扯、不再瞬移」的落点：从前每单一腿各推一次位置，
    /// 同一 tick 里互相覆盖，`held` 中最后遍历到的那单说了算，观感就是抖。
    #[test]
    fn only_the_current_leg_moves_when_a_rider_holds_two_shop_orders() {
        use crate::sim::geo::haversine_m;

        let mut cfg = SimConfig::default();
        cfg.sim_rider_count = 0;
        // 抬到 2 才可能让同一个人压两单（`effective_max_held` 读的就是它）
        cfg.backorder.max_held = 2;
        let mut w = World::new_for_test(cfg, 7);
        let rid = w.spawn_human_rider("张三");
        w.rider_mut(rid).unwrap().online = true;
        w.generate_order_batch(8);

        // 凑两单落在**不同**商家上——同商家会被一次到店全部出队，测不出「排队」
        let mut picked: Vec<OrderId> = Vec::new();
        for oid in w.pending_orders() {
            if picked.len() == 2 {
                break;
            }
            let Some(mid) = w.order(oid).map(|o| o.merchant_id) else {
                continue;
            };
            let same_shop = picked
                .iter()
                .any(|p| w.order(*p).is_some_and(|o| o.merchant_id == mid));
            if same_shop {
                continue;
            }
            if scheduler::assign_forced(&mut w, oid, rid, crate::sim::order::AssignMode::Offer)
                .is_ok()
            {
                picked.push(oid);
            }
        }
        assert_eq!(picked.len(), 2, "测试前提：要能凑出两单落在不同商家上");
        let (first, second) = (picked[0], picked[1]);

        // 把第一单的到店倒计时拉长，免得它第一 tick 就到店、把第二单顶成当前腿，
        // 那样就分不清「第二单没动」是因为冻结还是因为还没轮到。
        w.rider_mut(rid)
            .unwrap()
            .held_mut(first)
            .unwrap()
            .remaining_min = 10;
        let second_before = w
            .rider(rid)
            .unwrap()
            .held
            .iter()
            .find(|h| h.order_id == second)
            .map(|h| h.remaining_min)
            .expect("第二单得在手上");

        let shop_first = w.order(first).unwrap().merchant_pos();
        let start = w.rider(rid).unwrap().pos();
        let dist_before = haversine_m(start, shop_first);

        tick(
            &mut w,
            TickInput {
                ai_off: true,
                hosted_humans: vec![rid],
                ..Default::default()
            },
        );

        let rider = w.rider(rid).unwrap();
        assert!(
            rider.route.as_ref().is_some_and(|r| r.is_current(first)),
            "先接的那单才是当前腿"
        );
        let dist_after = haversine_m(rider.pos(), shop_first);
        assert!(
            dist_after < dist_before,
            "当前腿该朝第一单的商家挪：{dist_before:.0}m → {dist_after:.0}m"
        );
        let second_after = rider
            .held
            .iter()
            .find(|h| h.order_id == second)
            .map(|h| h.remaining_min);
        assert_eq!(
            second_after,
            Some(second_before),
            "第二单不是当前腿，倒计时一步都不该走"
        );
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

        // 玩家在线 → 外圈不会把他交给 AI，`hosted_humans` 自然是空的
        let ev = tick(&mut w, TickInput::default());
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

    /// 「合适就合并」的端到端验证：**全员都在等餐、一个空闲骑手都没有**时，
    /// 同店新单仍然要并到那位在店门口等餐的骑手手上。
    ///
    /// 这条测试盯的是 `run_ai` 那道「没空闲就提前 return」的闸门——漏了它，
    /// 凑单只会在「恰好还有闲人」时才发生，而那恰恰不是它要解决的场面。
    #[test]
    fn a_waiting_rider_picks_up_a_same_shop_order_even_with_no_idle_rider() {
        use crate::sim::{rider::Phase, world::CustomOrderSpec};

        let cfg = SimConfig {
            sim_rider_count: 1,
            // 关掉自然出单：不然大厅里会冒出别家的单，把唯一一个名额占掉。
            order_spawn_interval_min: 10_000,
            ..Default::default()
        };
        let mut w = World::new_for_test(cfg, 11);
        let rid = w.sim_rider_ids()[0];

        w.generate_order_batch(1);
        let first = w.pending_orders()[0];
        scheduler::assign_forced(&mut w, first, rid, crate::sim::order::AssignMode::Offer)
            .expect("先塞一单给他");
        // 到店倒计时压到 1 分钟，下一 tick 就到店进等餐。
        w.rider_mut(rid)
            .unwrap()
            .held_mut(first)
            .unwrap()
            .remaining_min = 1;

        tick(&mut w, TickInput::default());
        assert!(
            w.rider(rid)
                .unwrap()
                .held
                .iter()
                .any(|h| h.phase == Phase::WaitingFood),
            "测试前提：他得先在店门口等餐"
        );

        // 同一家店又来一单，报价给高些，肯定越过接单意愿门槛。
        let shop = w.order(first).unwrap().merchant_pos();
        let shop_name = w.order(first).unwrap().merchant_name.clone();
        let second = w.create_custom_order(CustomOrderSpec {
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

        tick(&mut w, TickInput::default());

        let held = w.rider(rid).unwrap().held_ids();
        assert!(
            held.contains(&second),
            "店里又来一单，该顺手并给他，实际手持 {held:?}"
        );

        // 控制台看到的路线：取餐腿必须排在送达腿前面（不能拎着半批餐去送）。
        let r = w.rider(rid).unwrap();
        let brief = crate::sim::event::RiderBrief::control(r, &w.cfg, w.elapsed_min);
        let first_dropoff = brief.route.iter().position(|l| l.kind == LegKind::Dropoff);
        let last_pickup = brief.route.iter().rposition(|l| l.kind == LegKind::Pickup);
        if let (Some(d), Some(p)) = (first_dropoff, last_pickup) {
            assert!(p < d, "取餐腿不该排在送达腿后面：{:?}", brief.route);
        }
        assert_eq!(
            brief.route.iter().filter(|l| l.current).count(),
            1,
            "同一时刻只有一条当前腿"
        );
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
