//! 连接注册表。**权限过滤就在这一层完成。**
//!
//! ## 为什么过滤要放在下发那一刻
//!
//! 设计里反复强调「服务端鉴权过滤」：骑手只能看到自己的收入，高德 Key
//! 永远不下发。最省事也最不容易漏的做法是——**世界状态只有一个**，
//! 每条事件在下发时才按收件人角色决定「给不给、给什么版本」。
//!
//! 于是有了 [`Hub::broadcast`]：它接收一个 [`SimEvent`] 和 `&World`，
//! 对每个连接调用 [`render`]，得到该角色**应该看到的那一份 JSON**。
//!
//! ## 每个角色看到什么
//!
//! | 事件 | 骑手（本人） | 骑手（别人） | 观战 | 控制端 |
//! |---|---|---|---|---|
//! | `tick` / `weather_changed` / `order_broadcast` | ✔ | ✔ | ✔ | ✔ |
//! | `rider_state_changed` | 本人版（含收入） | 公开版 | 公开版 | 控制端版（含疲劳） |
//! | `rider_finance` | ✔ | ✘ | ✘ | ✔ |
//! | `fine_issued` | ✔ | ✘ | ✘ | ✔ |
//! | `task_message` | 只有自己那单 | ✘ | ✘ | ✔ |
//! | `day_closed` | 只有自己的流水 | ✘ | 只有汇总 | 全部 |

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::sim::{
    OrderId, RiderId,
    event::{DayLedger, RiderBrief, SimEvent},
    geo::{CoordSystem, LatLng, to_coord_system},
    world::World,
};

use super::protocol::{Ack, Envelope, Role};

/// 连接 id。
pub type ConnId = u64;

/// 一条在线连接。`tx` 是发给该客户端的出站队列。
///
/// 连接刚建立时角色是 [`Role::Spectator`] 且 `ready == false`；收到 `hello`
/// 后由 `app` 层调用 [`Hub::set_role`] / [`Hub::mark_ready`] 完成升级。
/// **在这之前，任何骑手动作都会被拒绝**（`HELLO_REQUIRED`）。
#[derive(Debug)]
pub struct Connection {
    pub id: ConnId,
    pub role: Role,
    pub name: String,
    /// 是否已完成握手。
    pub ready: bool,
    pub tx: UnboundedSender<String>,
}

/// hub 的累计统计，控制端可读。
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct HubStats {
    /// 累计接入过的连接数。
    pub accepted: u64,
    /// 当前在线连接数。
    pub online: usize,
    /// 骑手连接数。
    pub riders: usize,
    /// 因为队列满 / 连接关闭而丢弃的发送次数。
    pub dropped: u64,
    pub disconnects: u64,
}

/// 连接注册表。**单线程持有**——与 tick 循环同一个任务，因此不需要锁。
///
/// id 由调用方（`net::ws` 用全局原子计数器）分配，hub 只负责记账。
#[derive(Debug, Default)]
pub struct Hub {
    conns: Vec<Connection>,
    accepted: u64,
    dropped: u64,
    disconnects: u64,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    /// 接入一条连接。尚未握手，角色先按观战者算。
    pub fn register(
        &mut self,
        id: ConnId,
        role: Role,
        name: impl Into<String>,
        tx: UnboundedSender<String>,
    ) {
        self.accepted += 1;
        self.conns.push(Connection {
            id,
            role,
            name: name.into(),
            ready: false,
            tx,
        });
    }

    /// 握手完成，升级角色。
    pub fn set_role(&mut self, id: ConnId, role: Role, name: impl Into<String>) {
        if let Some(c) = self.conns.iter_mut().find(|c| c.id == id) {
            c.role = role;
            c.name = name.into();
        }
    }

    pub fn mark_ready(&mut self, id: ConnId) {
        if let Some(c) = self.conns.iter_mut().find(|c| c.id == id) {
            c.ready = true;
        }
    }

    pub fn is_ready(&self, id: ConnId) -> bool {
        self.conns.iter().any(|c| c.id == id && c.ready)
    }

    pub fn role_of(&self, id: ConnId) -> Option<Role> {
        self.conns.iter().find(|c| c.id == id).map(|c| c.role)
    }

    pub fn contains(&self, id: ConnId) -> bool {
        self.conns.iter().any(|c| c.id == id)
    }

    /// 断开连接。骑手下线由 `app` 层决定（这里只移出广播列表）。
    pub fn unregister(&mut self, id: ConnId) {
        let before = self.conns.len();
        self.conns.retain(|c| c.id != id);
        if self.conns.len() != before {
            self.disconnects += 1;
        }
    }

    pub fn len(&self) -> usize {
        self.conns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.conns.is_empty()
    }

    pub fn stats(&self) -> HubStats {
        HubStats {
            accepted: self.accepted,
            online: self.conns.len(),
            riders: self
                .conns
                .iter()
                .filter(|c| matches!(c.role, Role::Rider(_)))
                .count(),
            dropped: self.dropped,
            disconnects: self.disconnects,
        }
    }

    /// 现在连接着这条骑手的客户端数量（同一个骑手可以多端登录）。
    pub fn rider_conn_count(&self, rider_id: RiderId) -> usize {
        self.conns
            .iter()
            .filter(|c| c.role.rider_id() == Some(rider_id))
            .count()
    }

    /// **所有连着活人客户端的骑手**（去重、升序）。
    ///
    /// 这是 [`crate::human::ai_should_drive`] 的 `has_live_conn` 的数据源：
    /// 玩家断线 = 从这个列表里消失 = AI 下一 tick 接手。
    pub fn live_rider_ids(&self) -> Vec<RiderId> {
        let mut ids: Vec<RiderId> = self
            .conns
            .iter()
            .filter_map(|c| c.role.rider_id())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// 发给单条连接。
    pub fn send_to(&mut self, id: ConnId, env: &Envelope) {
        let Ok(text) = serde_json::to_string(env) else {
            self.dropped += 1;
            return;
        };
        let failed = self
            .conns
            .iter()
            .filter(|c| c.id == id)
            .filter(|c| c.tx.send(text.clone()).is_err())
            .count();
        self.dropped += failed as u64;
    }

    /// 发给某个骑手的**所有**连接。
    pub fn send_to_rider(&mut self, rider_id: RiderId, env: &Envelope) {
        self.send_filtered(env, Some(rider_id), false);
    }

    /// 只发给控制端。
    pub fn send_to_control(&mut self, env: &Envelope) {
        self.send_filtered(env, None, true);
    }

    /// 应答。因为 `Ack` 是服务端内部消息，直接走信封。
    pub fn send_ack(&mut self, id: ConnId, ack: &Ack) {
        let value = serde_json::to_value(ack).unwrap_or(Value::Null);
        self.send_to(id, &Envelope::new("ack", value));
    }

    /// 发给某条连接一个错误信封（没有 ack 上下文时用）。
    pub fn send_error(&mut self, id: ConnId, code: &str, message: &str) {
        self.send_to(
            id,
            &Envelope::new("error", json!({ "code": code, "message": message })),
        );
    }

    fn send_filtered(&mut self, env: &Envelope, rider: Option<RiderId>, control_only: bool) {
        let Ok(text) = serde_json::to_string(env) else {
            self.dropped += 1;
            return;
        };
        let failed = self
            .conns
            .iter()
            .filter(|c| match (rider, control_only) {
                (Some(rid), _) => c.role == Role::Rider(rid) || c.role == Role::Control,
                (None, true) => c.role == Role::Control,
                (None, false) => true,
            })
            .filter(|c| c.tx.send(text.clone()).is_err())
            .count();
        self.dropped += failed as u64;
    }

    /// **核心：把一条模拟事件按角色过滤后广播出去。**
    ///
    /// 返回这次事件实际投递的连接数，方便测试断言「别人的收入没被发出去」。
    pub fn broadcast(&mut self, world: &World, ev: &SimEvent) -> usize {
        let now = world.elapsed_min;
        let name = event_name(ev);
        let mut delivered = 0usize;
        let mut failed = 0u64;

        for c in &self.conns {
            let Some(payload) = render(world, ev, c.role, now) else {
                continue;
            };
            let env = Envelope::new(name, payload);
            match serde_json::to_string(&env) {
                Ok(text) => {
                    if c.tx.send(text).is_err() {
                        failed += 1;
                    } else {
                        delivered += 1;
                    }
                }
                Err(_) => failed += 1,
            }
        }
        self.dropped += failed;
        delivered
    }
}

/// `SimEvent` 的固定事件名（与 serde 标签一致）。
pub fn event_name(ev: &SimEvent) -> &'static str {
    match ev {
        SimEvent::Tick { .. } => "tick",
        SimEvent::OrderBroadcast { .. } => "order_broadcast",
        SimEvent::OrderClaimed { .. } => "order_claimed",
        SimEvent::OrderReassigned { .. } => "order_reassigned",
        SimEvent::OrderExpired { .. } => "order_expired",
        SimEvent::AssignRejected { .. } => "assign_rejected",
        SimEvent::OrderPickedUp { .. } => "order_picked_up",
        SimEvent::OrderDelivered { .. } => "order_delivered",
        SimEvent::RiderStateChanged { .. } => "rider_state_changed",
        SimEvent::RiderFinance { .. } => "rider_finance",
        SimEvent::WeatherChanged { .. } => "weather_changed",
        SimEvent::FineIssued { .. } => "fine_issued",
        SimEvent::ConfigChanged { .. } => "config_changed",
        SimEvent::AiDecision { .. } => "ai_decision",
        SimEvent::Narration { .. } => "narration",
        SimEvent::TaskMessage { .. } => "task_message",
        SimEvent::ChatOpen { .. } => "chat_open",
        SimEvent::ChatMessage { .. } => "chat_message",
        SimEvent::DayClosed { .. } => "day_closed",
    }
}

/// 取出 `SimEvent` 序列化后的 `payload` 部分。
///
/// `SimEvent` 带 `tag = "name", content = "payload"`，直接 `to_value` 会得到
/// `{ "name": ..., "payload": ... }`。而 [`render`] 的返回值要直接当信封的
/// payload 用，所以这里只取内层。
fn payload_of(ev: &SimEvent) -> Option<Value> {
    let v = serde_json::to_value(ev).ok()?;
    Some(v.get("payload").cloned().unwrap_or(v))
}

/// 出站坐标的字段名对。
///
/// `geo.rs` 的契约是「坐标永远是独立字段 `lat`/`lng`」，订单额外带上
/// `merchant_*` / `customer_*` 前缀。按字段名递归改写，一处就能覆盖
/// 骑手 / 订单 / 商户所有出口，不必逐个结构体改。
const COORD_PAIRS: [(&str, &str); 3] = [
    ("lat", "lng"),
    ("merchant_lat", "merchant_lng"),
    ("customer_lat", "customer_lng"),
];

/// 递归把 JSON 里的坐标字段转到目标坐标系。
fn convert_coords(v: &mut Value, target: CoordSystem) {
    match v {
        Value::Object(map) => {
            for (lat_key, lng_key) in COORD_PAIRS {
                let pair = map
                    .get(lat_key)
                    .and_then(Value::as_f64)
                    .zip(map.get(lng_key).and_then(Value::as_f64));
                if let Some((lat, lng)) = pair {
                    let p = to_coord_system(LatLng::new(lat, lng), target);
                    map.insert(lat_key.to_string(), json!(p.lat));
                    map.insert(lng_key.to_string(), json!(p.lng));
                }
            }
            for child in map.values_mut() {
                convert_coords(child, target);
            }
        }
        Value::Array(arr) => {
            for child in arr.iter_mut() {
                convert_coords(child, target);
            }
        }
        _ => {}
    }
}

/// 出口挂钩：内部是 GCJ-02，只有目标不是它时才转换。
fn apply_coord_system(world: &World, v: &mut Value) {
    let target = world.cfg.coord_system;
    if target != CoordSystem::Gcj02 {
        convert_coords(v, target);
    }
}

/// **权限过滤 + 坐标转换的唯一出口。**
///
/// 先按角色过滤，再把结果里的坐标转到配置的坐标系——保证每条出站消息
/// 都走同一道转换，不会哪条事件漏掉。
pub fn render(world: &World, ev: &SimEvent, role: Role, now_min: u64) -> Option<Value> {
    let mut v = render_filtered(world, ev, role, now_min)?;
    apply_coord_system(world, &mut v);
    Some(v)
}

/// **权限过滤的核心函数。** `None` = 这个角色不该看到这条事件。
///
/// 纯函数，因此可以脱离 WebSocket 单测——权限漏了是最严重的一类 bug。
fn render_filtered(world: &World, ev: &SimEvent, role: Role, now_min: u64) -> Option<Value> {
    let is_control = role == Role::Control;
    let me = role.rider_id();

    // 只发给特定骑手（+ 控制端）的事件
    let gate = |target: RiderId| -> bool { is_control || me == Some(target) };

    match ev {
        SimEvent::RiderFinance { rider_id, .. } => {
            if !gate(*rider_id) {
                return None;
            }
            payload_of(ev)
        }
        SimEvent::FineIssued { rider_id, .. } => {
            if !gate(*rider_id) {
                return None;
            }
            payload_of(ev)
        }
        SimEvent::AssignRejected { rider_id, .. } => {
            // 只有本人和控制端关心「你拒掉了第几张指派单」。
            if !gate(*rider_id) {
                return None;
            }
            payload_of(ev)
        }
        SimEvent::OrderReassigned {
            from_rider_id,
            to_rider_id,
            ..
        } => {
            // 两位当事人 + 控制端。原骑手要看到手上的单少了一张（否则他会一直
            // 以为自己还得去这家店），新骑手要看到多了一张（还要点确认）。
            // 对别人来说这单从头到尾都在别人手上，没有可看的信息——全服广播只会
            // 把「谁的活被抢了」摆给全服看。
            if is_control || me == Some(*from_rider_id) || me == Some(*to_rider_id) {
                payload_of(ev)
            } else {
                None
            }
        }
        SimEvent::RiderStateChanged { rider_id, .. } => {
            // 关键：按角色重建 brief，而不是转发事件里那份公开版。
            let rider = world.rider(*rider_id)?;
            let brief = match role {
                Role::Control => RiderBrief::control(rider, &world.cfg, now_min),
                Role::Rider(id) if id == *rider_id => RiderBrief::own(rider, &world.cfg, now_min),
                _ => RiderBrief::public(rider, &world.cfg, now_min),
            };
            Some(json!({
                "rider_id": rider_id,
                "rider": serde_json::to_value(brief).ok()?,
            }))
        }
        SimEvent::TaskMessage { order_id, .. } => {
            if is_control {
                return payload_of(ev);
            }
            let owner = world.order(*order_id).and_then(|o| o.assigned_to);
            if owner.is_some() && owner == me {
                payload_of(ev)
            } else {
                None
            }
        }
        SimEvent::DayClosed { ledgers, .. } => {
            let mut out = payload_of(ev)?;
            let visible: Vec<DayLedger> = match role {
                Role::Control => ledgers.clone(),
                // 观战者只看汇总，看不到任何人的流水
                Role::Spectator => Vec::new(),
                Role::Rider(id) => ledgers
                    .iter()
                    .filter(|l| l.rider_id == id)
                    .cloned()
                    .collect(),
            };
            out["ledgers"] = serde_json::to_value(visible).ok()?;
            Some(out)
        }
        // 其余事件都是公开信息
        _ => payload_of(ev),
    }
}

/// 按角色渲染完整快照。
///
/// - `Rider(self)`：自己的 brief 升级为「本人版」（含收入），其余保持公开版；
/// - `Control`：全部升级为控制端版（含疲劳、期望收益）；
/// - `Spectator`：全部公开版，且不含任何个人收入。
pub fn render_snapshot(world: &World, role: Role, now_min: u64) -> Value {
    let mut snap = world.public_snapshot();
    let me = role.rider_id();
    snap.riders = world
        .riders
        .iter()
        .map(|r| match role {
            Role::Control => RiderBrief::control(r, &world.cfg, now_min),
            Role::Rider(id) if id == r.id => RiderBrief::own(r, &world.cfg, now_min),
            _ => RiderBrief::public(r, &world.cfg, now_min),
        })
        .collect();
    // 观战者与普通骑手看不到别人的日结流水
    if role != Role::Control {
        snap.last_day_ledgers = match me {
            Some(id) => snap
                .last_day_ledgers
                .iter()
                .filter(|l| l.rider_id == id)
                .cloned()
                .collect(),
            None => Vec::new(),
        };
    }
    let mut v = serde_json::to_value(&snap).unwrap_or(Value::Null);
    apply_coord_system(world, &mut v);
    v
}

/// 给控制端的额外上下文：hub 状态 + 服务端配置（不含任何 Key）。
pub fn server_info(world: &World, hub: &HubStats, llm_enabled: bool, amap_enabled: bool) -> Value {
    json!({
        "hub": hub,
        "llm_enabled": llm_enabled,
        "amap_enabled": amap_enabled,
        "seed": world.seed,
        "elapsed_min": world.elapsed_min,
        "tick_interval_ms": world.cfg.tick_interval_ms(),
        "stats": world.stats,
    })
}

/// 订单是否属于某个骑手。
pub fn owns(world: &World, rider: RiderId, order: OrderId) -> bool {
    world
        .order(order)
        .map(|o| o.assigned_to == Some(rider))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{
        config::SimConfig,
        event::{DayLedger, DecisionSource, SimEvent},
        geo::LatLng,
        rider::{Phase, RiderKind, SpeedMode},
        world::World,
    };
    use tokio::sync::mpsc::unbounded_channel;

    fn world() -> World {
        let cfg = SimConfig {
            sim_rider_count: 3,
            ..Default::default()
        };
        World::new_for_test(cfg, 42)
    }

    fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>) -> Vec<Value> {
        let mut out = Vec::new();
        while let Ok(t) = rx.try_recv() {
            out.push(serde_json::from_str(&t).unwrap());
        }
        out
    }

    #[test]
    fn finance_only_reaches_its_owner_and_control() {
        let w = world();
        let mut hub = Hub::new();
        let (tx1, mut rx1) = unbounded_channel();
        let (tx2, mut rx2) = unbounded_channel();
        let (txc, mut rxc) = unbounded_channel();
        hub.register(1, Role::Rider(1), "甲", tx1);
        hub.register(2, Role::Rider(2), "乙", tx2);
        hub.register(3, Role::Control, "控制", txc);

        let ev = SimEvent::RiderFinance {
            rider_id: 1,
            delta_cents: 500,
            kind: "deliver".into(),
            reason: "送达".into(),
            takehome_cents: 500,
            hourly_wage_cents: 3000,
        };
        let delivered = hub.broadcast(&w, &ev);
        assert_eq!(delivered, 2, "只有本人与控制端");
        assert_eq!(drain(&mut rx1).len(), 1);
        assert!(drain(&mut rx2).is_empty(), "别人的收入绝不能下发");
        assert_eq!(drain(&mut rxc).len(), 1);
    }

    #[test]
    fn rider_state_is_downgraded_for_others_and_upgraded_for_self() {
        let w = world();
        assert!(w.rider(1).is_some());
        let ev = crate::sim::scheduler::state_event(&w, 1).unwrap();

        // 本人：有收入字段
        let mine = render(&w, &ev, Role::Rider(1), w.elapsed_min).unwrap();
        assert!(mine["rider"]["takehome_cents"].is_number());
        assert!(mine["rider"]["cash_cents"].is_number());

        // 别人：必须是公开版，收入字段是 null
        let other = render(&w, &ev, Role::Rider(2), w.elapsed_min).unwrap();
        assert!(
            other["rider"]["takehome_cents"].is_null(),
            "别人不该看到收入"
        );

        // 控制端：多出疲劳
        let ctrl = render(&w, &ev, Role::Control, w.elapsed_min).unwrap();
        assert!(ctrl["rider"]["fatigue"].is_number());
    }

    #[test]
    fn spectators_never_see_money_in_a_snapshot() {
        let w = world();
        let v = render_snapshot(&w, Role::Spectator, w.elapsed_min);
        for r in v["riders"].as_array().unwrap() {
            assert!(r["takehome_cents"].is_null());
            assert!(r["cash_cents"].is_null());
            assert!(r["fatigue"].is_null());
        }
        assert!(v["last_day_ledgers"].as_array().unwrap().is_empty());
    }

    #[test]
    fn snapshot_only_carries_own_ledger_for_a_rider() {
        let mut w = world();
        w.last_day_ledgers = vec![
            ledger(1, "甲", 100),
            ledger(2, "乙", 200),
            ledger(3, "丙", 300),
        ];
        let mine = render_snapshot(&w, Role::Rider(2), w.elapsed_min);
        let ls = mine["last_day_ledgers"].as_array().unwrap();
        assert_eq!(ls.len(), 1);
        assert_eq!(ls[0]["rider_id"], 2);

        let ctrl = render_snapshot(&w, Role::Control, w.elapsed_min);
        assert_eq!(ctrl["last_day_ledgers"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn task_message_is_scoped_to_the_assigned_rider() {
        let mut w = world();
        w.generate_order_batch(1);
        let oid = w.orders.last().unwrap().id;
        w.orders.last_mut().unwrap().assigned_to = Some(2);
        let ev = SimEvent::TaskMessage {
            order_id: oid,
            from: "customer",
            text: "放门口".into(),
            step: 1,
            task_id: "t".into(),
        };
        assert!(render(&w, &ev, Role::Rider(2), w.elapsed_min).is_some());
        assert!(render(&w, &ev, Role::Rider(1), w.elapsed_min).is_none());
        assert!(render(&w, &ev, Role::Spectator, w.elapsed_min).is_none());
        assert!(render(&w, &ev, Role::Control, w.elapsed_min).is_some());
    }

    #[test]
    fn public_events_reach_everyone() {
        let w = world();
        let ev = SimEvent::WeatherChanged {
            weather: crate::sim::weather::Weather::LightRain,
            label: "下雨",
            speed_factor: 0.85,
            rain_bonus_cents: 100,
        };
        assert!(render(&w, &ev, Role::Spectator, w.elapsed_min).is_some());
        assert!(render(&w, &ev, Role::Rider(1), w.elapsed_min).is_some());
        assert!(render(&w, &ev, Role::Control, w.elapsed_min).is_some());
    }

    /// 改派只给两位当事人 + 控制端：对第三人来说这单从头到尾都挂在别人名下，
    /// 广播出去只是把「谁的活被抢了」摆给全服看。
    #[test]
    fn order_reassigned_reaches_only_the_two_riders_involved() {
        let w = world();
        let ev = SimEvent::OrderReassigned {
            order_id: 1,
            from_rider_id: 2,
            from_rider_name: "乙".into(),
            to_rider_id: 3,
            to_rider_name: "丙".into(),
            phase: Phase::WaitingFood,
        };
        assert!(render(&w, &ev, Role::Control, w.elapsed_min).is_some());
        assert!(
            render(&w, &ev, Role::Rider(2), w.elapsed_min).is_some(),
            "原骑手得知道手上少了一张"
        );
        assert!(
            render(&w, &ev, Role::Rider(3), w.elapsed_min).is_some(),
            "新骑手得知道多了一张（还要点确认）"
        );
        assert!(render(&w, &ev, Role::Rider(9), w.elapsed_min).is_none());
        assert!(render(&w, &ev, Role::Spectator, w.elapsed_min).is_none());
    }

    #[test]
    fn day_closed_hides_other_riders_ledgers_from_a_rider() {
        let w = world();
        let ev = SimEvent::DayClosed {
            day: 1,
            ledgers: vec![ledger(1, "甲", 100), ledger(2, "乙", 200)],
            total_takehome_cents: 300,
            total_orders: 4,
        };
        let mine = render(&w, &ev, Role::Rider(2), w.elapsed_min).unwrap();
        assert_eq!(mine["ledgers"].as_array().unwrap().len(), 1);
        assert_eq!(mine["total_takehome_cents"], 300, "汇总仍是全局的");

        let spec = render(&w, &ev, Role::Spectator, w.elapsed_min).unwrap();
        assert!(spec["ledgers"].as_array().unwrap().is_empty());
    }

    #[test]
    fn hub_tracks_online_counts_and_disconnects() {
        let mut hub = Hub::new();
        let (tx, _rx) = unbounded_channel();
        let id = 1;
        hub.register(id, Role::Rider(1), "甲", tx);
        let (tx2, _rx2) = unbounded_channel();
        hub.register(2, Role::Control, "控制", tx2);
        let s = hub.stats();
        assert_eq!(s.online, 2);
        assert_eq!(s.riders, 1);
        assert_eq!(s.accepted, 2);
        assert_eq!(hub.rider_conn_count(1), 1);
        hub.unregister(id);
        assert_eq!(hub.len(), 1);
        assert_eq!(hub.stats().disconnects, 1);
        assert_eq!(hub.rider_conn_count(1), 0);
    }

    #[test]
    fn dropped_counter_grows_when_the_receiver_is_gone() {
        let w = world();
        let mut hub = Hub::new();
        let (tx, rx) = unbounded_channel();
        drop(rx);
        hub.register(1, Role::Spectator, "幽灵", tx);
        let ev = SimEvent::OrderExpired { order_id: 1 };
        assert_eq!(hub.broadcast(&w, &ev), 0);
        assert_eq!(hub.stats().dropped, 1);
    }

    #[test]
    fn event_names_are_snake_case_and_stable() {
        assert_eq!(
            event_name(&SimEvent::OrderExpired { order_id: 1 }),
            "order_expired"
        );
        assert_eq!(
            event_name(&SimEvent::AiDecision {
                source: DecisionSource::Algorithm,
                layer: "algorithm",
                assignments: vec![],
                ops: vec![],
                decision_id: 0,
                note: String::new(),
            }),
            "ai_decision"
        );
    }

    #[test]
    fn broadcast_returns_the_number_of_deliveries() {
        let w = world();
        let mut hub = Hub::new();
        let (t1, mut r1) = unbounded_channel();
        let (t2, mut r2) = unbounded_channel();
        hub.register(1, Role::Rider(1), "甲", t1);
        hub.register(2, Role::Control, "控制", t2);
        let ev = SimEvent::OrderExpired { order_id: 3 };
        assert_eq!(hub.broadcast(&w, &ev), 2);
        assert_eq!(drain(&mut r1).len(), 1);
        assert_eq!(drain(&mut r2).len(), 1);
    }

    fn ledger(id: RiderId, name: &str, takehome: i64) -> DayLedger {
        DayLedger {
            rider_id: id,
            key: crate::sim::rider_key(id),
            name: name.into(),
            takehome_cents: takehome,
            gross_cents: takehome,
            fines_cents: 0,
            insurance_cents: 0,
            completed: 1,
            late_count: 0,
            online_minutes: 60,
            hourly_wage_cents: 3000,
            rating: 5.0,
            level: 1,
            debt_cents: 0,
        }
    }

    #[test]
    fn a_rider_can_have_several_connections() {
        let mut hub = Hub::new();
        let (t1, mut r1) = unbounded_channel();
        let (t2, mut r2) = unbounded_channel();
        hub.register(1, Role::Rider(7), "手机", t1);
        hub.register(2, Role::Rider(7), "平板", t2);
        assert_eq!(hub.rider_conn_count(7), 2);
        hub.send_to_rider(7, &Envelope::new("pong", json!({})));
        assert_eq!(drain(&mut r1).len(), 1);
        assert_eq!(drain(&mut r2).len(), 1);
    }

    #[test]
    fn new_rider_is_public_by_default() {
        let mut w = world();
        let home = LatLng::new(30.5, 114.4);
        let id = w.add_rider(RiderKind::Human, "新人", home);
        let r = w.rider(id).unwrap();
        assert_eq!(r.speed_mode, SpeedMode::Compliant);
    }

    #[test]
    fn a_connection_starts_unready_and_can_be_upgraded() {
        let mut hub = Hub::new();
        let (tx, _rx) = unbounded_channel();
        hub.register(1, Role::Spectator, "", tx);
        assert!(!hub.is_ready(1));
        assert_eq!(hub.role_of(1), Some(Role::Spectator));
        hub.set_role(1, Role::Rider(3), "张三");
        hub.mark_ready(1);
        assert!(hub.is_ready(1));
        assert_eq!(hub.role_of(1), Some(Role::Rider(3)));
        assert_eq!(hub.rider_conn_count(3), 1);
        assert!(!hub.is_ready(99), "不存在的连接不能算已握手");
        assert_eq!(hub.role_of(99), None);
    }

    #[test]
    fn owns_matches_assignment() {
        let mut w = world();
        w.generate_order_batch(1);
        let oid = w.orders.last().unwrap().id;
        assert!(!owns(&w, 1, oid));
        w.orders.last_mut().unwrap().assigned_to = Some(1);
        assert!(owns(&w, 1, oid));
    }

    #[test]
    fn server_info_never_contains_api_keys() {
        let w = world();
        let v = server_info(&w, &HubStats::default(), true, true);
        let text = v.to_string();
        assert!(!text.contains("key"), "server_info 里不该出现 key：{text}");
    }

    #[test]
    fn outbound_coords_follow_the_configured_system() {
        let mut w = world();
        w.generate_order_batch(1);
        assert!(w.rider(1).is_some(), "测试世界应有骑手");
        assert!(!w.orders.is_empty(), "应生成一单");

        let gcj = render_snapshot(&w, Role::Control, w.elapsed_min);
        let gcj_rider_lat = gcj["riders"][0]["lat"].as_f64().unwrap();
        let gcj_merchant_lat = gcj["hall"][0]["merchant_lat"].as_f64().unwrap();

        w.cfg.coord_system = CoordSystem::Wgs84;
        let wgs = render_snapshot(&w, Role::Control, w.elapsed_min);
        let wgs_rider_lat = wgs["riders"][0]["lat"].as_f64().unwrap();
        let wgs_merchant_lat = wgs["hall"][0]["merchant_lat"].as_f64().unwrap();

        assert!(
            (gcj_rider_lat - wgs_rider_lat).abs() > 0.001,
            "切换到 WGS-84 后骑手坐标应去偏"
        );
        assert!(
            (gcj_merchant_lat - wgs_merchant_lat).abs() > 0.001,
            "订单里的商户坐标也应去偏"
        );
        assert_eq!(wgs["config"]["coord_system"], "wgs84");

        // 事件出口走同一道转换，否则骑手图标会在两种坐标间跳。
        let ev = crate::sim::scheduler::state_event(&w, 1).unwrap();
        let other = render(&w, &ev, Role::Rider(2), w.elapsed_min).unwrap();
        let ev_lat = other["rider"]["lat"].as_f64().unwrap();
        let internal_lat = w.rider(1).unwrap().pos().lat;
        assert!(
            (ev_lat - internal_lat).abs() > 0.001,
            "事件里的骑手坐标也应去偏"
        );

        // 世界内部状态不受影响：切回来必须还原。
        w.cfg.coord_system = CoordSystem::Gcj02;
        let back = render_snapshot(&w, Role::Control, w.elapsed_min);
        assert_eq!(back["riders"][0]["lat"].as_f64().unwrap(), gcj_rider_lat);
    }
}
