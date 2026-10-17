use super::*;
use crate::human::HostMode;
use crate::llm::{RulingSource, TaskRuling};
use crate::sim::order::{ExtraTask, OrderStatus};
use tokio::sync::mpsc::UnboundedReceiver;

fn world() -> World {
    let cfg = crate::sim::config::SimConfig {
        sim_rider_count: 3,
        ..Default::default()
    };
    World::new_for_test(cfg, 42)
}

fn app() -> App {
    App::new(world(), Store::in_memory().unwrap(), AppOptions::default())
}

fn control_app() -> App {
    let opts = AppOptions {
        allow_control: true,
        ..AppOptions::default()
    };
    App::new(world(), Store::in_memory().unwrap(), opts)
}

/// 接入一条连接，返回它的出站接收端。
fn open(app: &mut App, id: ConnId) -> UnboundedReceiver<String> {
    let (tx, rx) = unbounded_channel();
    app.on_inbound(Inbound::Open { conn_id: id, tx });
    rx
}

fn send(app: &mut App, id: ConnId, msg: ClientMessage) {
    app.on_inbound(Inbound::Msg { conn_id: id, msg });
}

fn close(app: &mut App, id: ConnId) {
    app.on_inbound(Inbound::Closed { conn_id: id });
}

/// 把队列里积压的出站消息全部解析出来。
fn drain(rx: &mut UnboundedReceiver<String>) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    while let Ok(t) = rx.try_recv() {
        out.push(serde_json::from_str(&t).expect("出站帧必须是合法 JSON"));
    }
    out
}

fn event<'a>(msgs: &'a [serde_json::Value], name: &str) -> Option<&'a serde_json::Value> {
    msgs.iter().find(|m| m["event"].as_str() == Some(name))
}

fn ack(msgs: &[serde_json::Value]) -> &serde_json::Value {
    event(msgs, "ack").expect("应该收到 ack")
}

fn rider_token(id: RiderId) -> String {
    format!("rider:{id}")
}

/// 完整握手，返回 (出站队列, welcome)。
fn hello(
    app: &mut App,
    id: ConnId,
    token: Option<&str>,
) -> (UnboundedReceiver<String>, serde_json::Value) {
    let mut rx = open(app, id);
    send(
        app,
        id,
        ClientMessage::Hello {
            token: token.map(str::to_string),
            rider_name: None,
            password: None,
            protocol_version: Some(PROTOCOL_VERSION.to_string()),
        },
    );
    let msgs = drain(&mut rx);
    let welcome = event(&msgs, "welcome")
        .expect("握手成功必须回 welcome")
        .clone();
    (rx, welcome)
}

// -----------------------------------------------------------------
// 握手闸门
// -----------------------------------------------------------------

#[test]
fn token_parsing_accepts_the_documented_forms() {
    assert_eq!(parse_token("rider:3"), Some(Token::Rider(3)));
    assert_eq!(parse_token("r3"), Some(Token::Rider(3)));
    assert_eq!(parse_token("  new:张师傅 "), Some(Token::New("张师傅")));
    assert_eq!(parse_token("garbage"), None);
    assert_eq!(parse_token(""), None);
}

#[test]
fn first_message_must_be_hello() {
    let mut app = app();
    let mut rx = open(&mut app, 1);
    send(&mut app, 1, ClientMessage::Ping);
    let v = drain(&mut rx);
    let err = event(&v, "error").expect("没握手就该报错");
    assert_eq!(err["payload"]["code"].as_str(), Some(code::HELLO_REQUIRED));
}

#[test]
fn bad_protocol_version_is_rejected_and_kicked() {
    let mut app = app();
    let mut rx = open(&mut app, 1);
    send(
        &mut app,
        1,
        ClientMessage::Hello {
            token: None,
            rider_name: None,
            password: None,
            protocol_version: Some("0.0".into()),
        },
    );
    let v = drain(&mut rx);
    let err = event(&v, "error").expect("版本不符就该报错");
    assert_eq!(
        err["payload"]["code"].as_str(),
        Some(code::PROTOCOL_MISMATCH)
    );
    assert!(!app.hub.contains(1), "版本不符必须踢掉连接");
}

#[test]
fn unknown_rider_token_is_unauthorized() {
    let mut app = app();
    let mut rx = open(&mut app, 1);
    send(
        &mut app,
        1,
        ClientMessage::Hello {
            token: Some("rider:999".into()),
            rider_name: None,
            password: None,
            protocol_version: None,
        },
    );
    let v = drain(&mut rx);
    let err = event(&v, "error").expect("不存在的骑手就该报错");
    assert_eq!(err["payload"]["code"].as_str(), Some(code::UNAUTHORIZED));
    assert!(!app.hub.contains(1));
}

#[test]
fn spectator_never_gets_a_rider_role_when_control_is_disabled() {
    let mut app = app();
    let (_rx, w) = hello(&mut app, 1, None);
    assert_eq!(w["payload"]["role"].as_str(), Some("spectator"));
    assert!(w["payload"]["rider_id"].is_null());
    assert_eq!(app.hub.role_of(1), Some(Role::Spectator));
}

#[test]
fn rider_hello_auto_goes_online_and_gets_its_own_finance() {
    let mut app = app();
    let rid = app.world.spawn_human_rider("张三");
    assert!(!app.world.rider(rid).unwrap().online, "真人骑手默认下线");

    let (_rx, w) = hello(&mut app, 1, Some(&rider_token(rid)));
    assert_eq!(w["payload"]["role"].as_str(), Some("rider"));
    assert_eq!(w["payload"]["rider_id"].as_u64(), Some(rid as u64));
    assert_eq!(
        w["payload"]["rider_key"].as_str(),
        Some(format!("r{rid}").as_str())
    );
    assert!(app.world.rider(rid).unwrap().online, "连上就该跑单");

    let riders = w["payload"]["snapshot"]["riders"].as_array().unwrap();
    let me = riders
        .iter()
        .find(|r| r["id"].as_u64() == Some(rid as u64))
        .unwrap();
    assert!(me["takehome_cents"].is_number(), "本人版快照必须含收入");
    let other = riders.iter().find(|r| r["id"].as_u64() == Some(1)).unwrap();
    assert!(other["takehome_cents"].is_null(), "别人的收入绝不下发");
}

#[test]
fn new_token_creates_a_human_rider() {
    let mut app = app();
    let before = app.world.riders.len();
    let (_rx, w) = hello(&mut app, 1, Some("new:李师傅"));
    assert_eq!(w["payload"]["role"].as_str(), Some("rider"));
    assert_eq!(app.world.riders.len(), before + 1);
    let rid = w["payload"]["rider_id"].as_u64().unwrap() as RiderId;
    assert_eq!(app.world.rider(rid).unwrap().name, "李师傅");
    assert!(app.world.rider(rid).unwrap().online);
}

// -----------------------------------------------------------------
// 抢单
// -----------------------------------------------------------------

#[test]
fn a_claim_moves_the_order_and_acks_with_the_report() {
    let mut app = app();
    let rid = app.world.spawn_human_rider("张三");
    let (mut rx, _w) = hello(&mut app, 1, Some(&rider_token(rid)));
    let oid = app.world.generate_order().expect("应该能出一单");

    send(
        &mut app,
        1,
        ClientMessage::Claim {
            order_id: oid,
            back_order: false,
        },
    );
    let v = drain(&mut rx);
    let a = ack(&v);
    assert_eq!(a["payload"]["action"].as_str(), Some("claim"));
    assert_eq!(a["payload"]["ok"].as_bool(), Some(true));
    assert_eq!(a["payload"]["data"]["order_id"].as_u64(), Some(oid as u64));
    assert_eq!(a["payload"]["data"]["rider_id"].as_u64(), Some(rid as u64));
    assert_eq!(app.world.order(oid).unwrap().assigned_to, Some(rid));
    assert!(event(&v, "order_claimed").is_some(), "抢单成功要广播");
}

#[test]
fn a_second_claim_on_the_same_order_fails_and_does_not_steal_it() {
    let mut app = app();
    let r1 = app.world.spawn_human_rider("甲");
    let r2 = app.world.spawn_human_rider("乙");
    let (mut c1, _) = hello(&mut app, 1, Some(&rider_token(r1)));
    let (mut c2, _) = hello(&mut app, 2, Some(&rider_token(r2)));
    let oid = app.world.generate_order().expect("应该能出一单");

    send(
        &mut app,
        1,
        ClientMessage::Claim {
            order_id: oid,
            back_order: false,
        },
    );
    let first = drain(&mut c1);
    assert_eq!(ack(&first)["payload"]["ok"].as_bool(), Some(true));

    send(
        &mut app,
        2,
        ClientMessage::Claim {
            order_id: oid,
            back_order: false,
        },
    );
    let second = drain(&mut c2);
    let a = ack(&second);
    assert_eq!(a["payload"]["ok"].as_bool(), Some(false));
    assert_eq!(a["payload"]["code"].as_str(), Some("ALREADY_CLAIMED"));
    assert_eq!(app.world.order(oid).unwrap().assigned_to, Some(r1));
}

#[test]
fn spectator_cannot_claim_or_control() {
    let mut app = app();
    let (mut rx, _w) = hello(&mut app, 1, None);
    let oid = app.world.generate_order().expect("应该能出一单");

    send(
        &mut app,
        1,
        ClientMessage::Claim {
            order_id: oid,
            back_order: false,
        },
    );
    let v = drain(&mut rx);
    assert_eq!(ack(&v)["payload"]["code"].as_str(), Some(code::NOT_A_RIDER));

    send(&mut app, 1, ClientMessage::SetPaused { paused: true });
    let v = drain(&mut rx);
    assert_eq!(
        ack(&v)["payload"]["code"].as_str(),
        Some(code::NOT_A_CONTROLLER)
    );
    assert_ne!(app.world.cfg.speed_multiplier, 0, "观战者不许暂停世界");
}

// -----------------------------------------------------------------
// 控制端
// -----------------------------------------------------------------

#[test]
fn set_realtime_aligns_the_clock_and_pushes_a_frame() {
    let mut app = control_app();
    let (mut ctrl, _) = hello(&mut app, 1, None);
    drain(&mut ctrl);

    send(
        &mut app,
        1,
        ClientMessage::SetRealtime {
            enabled: true,
            wall_minute: Some(14 * 60 + 30),
        },
    );
    assert!(app.world.cfg.realtime);
    assert_eq!(app.world.sim_minute, 870, "时钟要对齐真实 14:30");
    assert_eq!(
        app.world.cfg.tick_interval_ms(),
        crate::sim::config::REALTIME_TICK_MS
    );

    let v = drain(&mut ctrl);
    let a = ack(&v);
    assert_eq!(a["payload"]["data"]["realtime"].as_bool(), Some(true));
    assert_eq!(a["payload"]["data"]["wall_minute"].as_u64(), Some(870));
    assert_eq!(
        a["payload"]["data"]["tick_interval_ms"].as_u64(),
        Some(60_000)
    );
    // 实时档 60 秒才一个 tick，调完时钟必须立刻补一帧，否则控制台会卡住。
    assert!(event(&v, "tick").is_some(), "对齐时钟后要推一帧 tick");

    // 对齐完要让主循环重排节拍，不然会被上一档剩下的节拍推走一分钟。
    assert!(app.beat_reset, "切实时档后要重排节拍");

    // 选倍率档 = 退出实时档。
    send(
        &mut app,
        1,
        ClientMessage::SetSpeedMultiplier { multiplier: 4 },
    );
    assert!(!app.world.cfg.realtime);
    assert_eq!(app.world.cfg.tick_interval_ms(), 250);
    assert!(app.beat_reset, "切倍率档后也要重排");
}

#[test]
fn rider_renames_are_broadcast_and_batch_names_skip_blanks() {
    let mut app = control_app();
    let (mut ctrl, _) = hello(&mut app, 1, None);
    drain(&mut ctrl);
    let ids = app.world.sim_rider_ids();

    send(
        &mut app,
        1,
        ClientMessage::RenameRider {
            rider_id: ids[0],
            name: "  张三 ".into(),
        },
    );
    assert_eq!(app.world.rider(ids[0]).unwrap().name, "张三");
    let v = drain(&mut ctrl);
    assert_eq!(ack(&v)["payload"]["data"]["changed"].as_bool(), Some(true));
    assert!(
        event(&v, "rider_state_changed").is_some(),
        "改名要让所有端看到新名字"
    );

    // 批量名单：空名字不覆盖原名，顺序按 id 升序。
    send(
        &mut app,
        1,
        ClientMessage::SetRiderNames {
            names: vec!["   ".into(), "李四".into()],
        },
    );
    assert_eq!(
        app.world.rider(ids[0]).unwrap().name,
        "张三",
        "空名字要跳过"
    );
    assert_eq!(app.world.rider(ids[1]).unwrap().name, "李四");

    // 非法名字：changed=false，世界不变。
    send(
        &mut app,
        1,
        ClientMessage::RenameRider {
            rider_id: ids[1],
            name: "   ".into(),
        },
    );
    assert_eq!(app.world.rider(ids[1]).unwrap().name, "李四");
    assert_eq!(app.world.sim_rider_names()[1], "李四");
}

#[test]
fn control_can_create_a_custom_order_that_lands_in_the_hall() {
    let mut app = control_app();
    let (mut ctrl, _) = hello(&mut app, 1, None);
    drain(&mut ctrl);
    let before = app.world.orders.len();

    send(
        &mut app,
        1,
        ClientMessage::CreateOrder {
            merchant_name: "  夜宵摊 ".into(),
            merchant_lat: 30.5,
            merchant_lng: 114.4,
            customer_name: Some("小张".into()),
            customer_address: None,
            customer_lat: 30.55,
            customer_lng: 114.45,
            fee_cents: Some(2222),
        },
    );
    let v = drain(&mut ctrl);
    assert_eq!(ack(&v)["payload"]["ok"].as_bool(), Some(true));
    let id = ack(&v)["payload"]["data"]["order_id"]
        .as_u64()
        .expect("要回订单号") as OrderId;

    assert_eq!(app.world.orders.len(), before + 1);
    let o = app.world.order(id).expect("订单在");
    assert_eq!(o.merchant_name, "夜宵摊", "名字两端空白该被 trim");
    assert_eq!(o.customer_name, "小张");
    assert_eq!(o.fee.quoted_fee_cents, 2222, "手填金额要原样生效");
    assert_eq!(o.status, OrderStatus::Pending);
    // 不等下一拍 tick，创建完就广播，别的端立刻能在摇单池里看到。
    assert!(
        event(&v, "order_broadcast").is_some(),
        "新单必须立刻广播：{v:?}"
    );
}

#[test]
fn create_order_rejects_an_impossible_coordinate() {
    let mut app = control_app();
    let (mut ctrl, _) = hello(&mut app, 1, None);
    drain(&mut ctrl);
    let before = app.world.orders.len();

    send(
        &mut app,
        1,
        ClientMessage::CreateOrder {
            merchant_name: "鬼摊".into(),
            merchant_lat: 0.0,
            merchant_lng: 0.0,
            customer_name: None,
            customer_address: None,
            customer_lat: 30.5,
            customer_lng: 114.4,
            fee_cents: None,
        },
    );
    let v = drain(&mut ctrl);
    let a = ack(&v);
    assert_eq!(a["payload"]["ok"].as_bool(), Some(false));
    assert_eq!(a["payload"]["code"].as_str(), Some(code::BAD_REQUEST));
    assert_eq!(app.world.orders.len(), before, "坐标不合法不许动世界");
}

#[test]
fn control_can_reset_a_rider_wallet_that_went_negative() {
    let mut app = control_app();
    let (mut ctrl, _) = hello(&mut app, 1, None);
    drain(&mut ctrl);
    let rid = app.world.sim_rider_ids()[0];

    // 先把余额弄成负的（模拟测试把账玩乱了）。
    let r = app.world.rider_mut(rid).expect("骑手在");
    r.cash_cents = -8888;
    r.gross_cents = 1234;
    r.fines_cents = 10_000;

    send(
        &mut app,
        1,
        ClientMessage::ResetRiderWallet { rider_id: rid },
    );
    let v = drain(&mut ctrl);
    assert_eq!(ack(&v)["payload"]["ok"].as_bool(), Some(true));
    assert_eq!(
        ack(&v)["payload"]["data"]["cleared_cents"].as_i64(),
        Some(-8888),
        "要把清空前的余额回给控制台写提示"
    );

    let r = app.world.rider(rid).expect("骑手在");
    assert_eq!(r.cash_cents, 0);
    assert_eq!(r.gross_cents, 0);
    assert_eq!(r.fines_cents, 0);
    assert!(r.last_ledger.is_empty());
    // 别的端也要立刻看到钱包归零。
    assert!(event(&v, "rider_state_changed").is_some(), "{v:?}");

    // 不存在的骑手：业务失败，不改任何东西。
    send(
        &mut app,
        1,
        ClientMessage::ResetRiderWallet { rider_id: 9999 },
    );
    let v = drain(&mut ctrl);
    assert_eq!(ack(&v)["payload"]["code"].as_str(), Some("RIDER_NOT_FOUND"));
}

#[test]
fn rider_accept_assign_clears_the_pending_flag() {
    let mut app = control_app();
    let (mut ctrl, _) = hello(&mut app, 1, None);
    drain(&mut ctrl);
    let rid = app.world.sim_rider_ids()[0];
    let (mut rider_rx, _) = hello(&mut app, 2, Some(&rider_token(rid)));
    drain(&mut rider_rx);

    let oid = app.world.generate_order_batch(1)[0];
    send(
        &mut app,
        1,
        ClientMessage::ForceAssign {
            order_id: oid,
            rider_id: rid,
            mode: crate::sim::order::AssignMode::Offer,
        },
    );
    drain(&mut ctrl);
    assert_eq!(
        app.world.rider(rid).expect("骑手在").assign_pending,
        Some(oid)
    );

    send(&mut app, 2, ClientMessage::AcceptAssign { order_id: oid });
    let v = drain(&mut rider_rx);
    assert_eq!(ack(&v)["payload"]["action"].as_str(), Some("accept_assign"));
    assert_eq!(app.world.rider(rid).expect("骑手在").assign_pending, None);

    // 幂等：重发不许弹错（网络重传很常见）。
    send(&mut app, 2, ClientMessage::AcceptAssign { order_id: oid });
    let v = drain(&mut rider_rx);
    assert_eq!(ack(&v)["payload"]["ok"].as_bool(), Some(true));
}

/// 强制派单没有「拒绝」这个动作：服务端挡回，状态原样保留；
/// 指派单则照常能拒（吃一次额度）。
#[test]
fn forced_assign_cannot_be_rejected_but_offer_can() {
    let mut app = control_app();
    let (mut ctrl, _) = hello(&mut app, 1, None);
    drain(&mut ctrl);
    let rid = app.world.sim_rider_ids()[0];
    let (mut rider_rx, _) = hello(&mut app, 2, Some(&rider_token(rid)));
    drain(&mut rider_rx);

    let forced_oid = app.world.generate_order_batch(1)[0];
    send(
        &mut app,
        1,
        ClientMessage::ForceAssign {
            order_id: forced_oid,
            rider_id: rid,
            mode: crate::sim::order::AssignMode::Forced,
        },
    );
    let ctrls = drain(&mut ctrl);
    assert_eq!(
        event(&ctrls, "order_claimed").expect("要看到抢单")["payload"]["assign_mode"].as_str(),
        Some("forced"),
        "控制台要能分清这是强制派单"
    );

    send(
        &mut app,
        2,
        ClientMessage::Reject {
            order_id: forced_oid,
        },
    );
    let v = drain(&mut rider_rx);
    let a = ack(&v);
    assert_eq!(a["payload"]["ok"].as_bool(), Some(false));
    assert_eq!(a["payload"]["code"].as_str(), Some("ASSIGN_FORCED"));
    assert_eq!(a["payload"]["message"].as_str(), Some("强制派单不能拒绝"));
    let r = app.world.rider(rid).expect("骑手在");
    assert_eq!(r.assign_pending, Some(forced_oid), "拒绝失败不能摘掉待确认");
    assert_eq!(r.rejected, 0, "被挡回的拒绝不计额度");
    assert_eq!(
        app.world.order(forced_oid).expect("订单在").status,
        OrderStatus::Assigned,
        "拒不掉就等于接下了"
    );

    // 同一骑手的下一张派成指派单，就能正常拒。
    let offer_oid = app.world.generate_order_batch(1)[0];
    send(
        &mut app,
        1,
        ClientMessage::ForceAssign {
            order_id: offer_oid,
            rider_id: rid,
            mode: crate::sim::order::AssignMode::Offer,
        },
    );
    drain(&mut ctrl);
    send(
        &mut app,
        2,
        ClientMessage::Reject {
            order_id: offer_oid,
        },
    );
    let v = drain(&mut rider_rx);
    assert_eq!(ack(&v)["payload"]["ok"].as_bool(), Some(true));
    let ev = event(&v, "assign_rejected").expect("拒单要进事件流");
    assert_eq!(ev["payload"]["used"].as_u64(), Some(1));
    assert_eq!(ev["payload"]["forced_offline"].as_bool(), Some(false));
    assert_eq!(
        app.world.rider(rid).expect("骑手在").rejected,
        1,
        "指派单拒掉要吃额度"
    );
    assert_eq!(
        app.world.order(offer_oid).expect("订单在").status,
        OrderStatus::Pending,
        "拒掉要回大厅"
    );
}

#[test]
fn reject_limit_round_trips_through_config_and_the_wire() {
    let mut app = control_app();
    let (mut ctrl, _) = hello(&mut app, 1, None);
    drain(&mut ctrl);

    // 默认 6 单；改配置后立刻体现在下发的 config_view 里。
    send(
        &mut app,
        1,
        ClientMessage::SetConfig(crate::net::ConfigPatch {
            max_rejects_per_day: Some(3),
            ..Default::default()
        }),
    );
    let v = drain(&mut ctrl);
    assert_eq!(
        ack(&v)["payload"]["data"]["max_rejects_per_day"].as_u64(),
        Some(3)
    );
    assert_eq!(app.world.cfg.max_rejects_per_day, 3);

    // 拒到额度用满，第 4 张指派单必须拒不掉，且骑手端收到中文提示。
    let rid = app.world.sim_rider_ids()[0];
    let (mut rider_rx, _) = hello(&mut app, 2, Some(&rider_token(rid)));
    drain(&mut rider_rx);

    for i in 1..=3 {
        let oid = app.world.generate_order_batch(1)[0];
        send(
            &mut app,
            1,
            ClientMessage::ForceAssign {
                order_id: oid,
                rider_id: rid,
                mode: crate::sim::order::AssignMode::Offer,
            },
        );
        drain(&mut ctrl);
        send(&mut app, 2, ClientMessage::Reject { order_id: oid });
        let v = drain(&mut rider_rx);
        assert_eq!(
            ack(&v)["payload"]["ok"].as_bool(),
            Some(true),
            "第 {i} 张该拒得掉"
        );
        assert!(
            event(&v, "assign_rejected").is_some(),
            "本人要收到拒单已记：{v:?}"
        );
    }

    let oid = app.world.generate_order_batch(1)[0];
    send(
        &mut app,
        1,
        ClientMessage::ForceAssign {
            order_id: oid,
            rider_id: rid,
            mode: crate::sim::order::AssignMode::Offer,
        },
    );
    drain(&mut ctrl);
    send(&mut app, 2, ClientMessage::Reject { order_id: oid });
    let v = drain(&mut rider_rx);
    let a = ack(&v);
    assert_eq!(a["payload"]["ok"].as_bool(), Some(false));
    // 额度满的那一刻人已经被强制下线了，后续平台单只能是强制派单：
    // 不是「额度挡住拒绝」，而是「强制单根本没有拒绝这个动作」。
    assert_eq!(a["payload"]["code"].as_str(), Some("ASSIGN_FORCED"));
    assert_eq!(a["payload"]["message"].as_str(), Some("强制派单不能拒绝"));
    let r = app.world.rider(rid).expect("骑手在");
    assert!(r.is_holding(oid), "拒不掉 = 归他，单不能跑");
    // 这单是**强制派单**，待确认标记得留着——他还要点「同意」（或等 30 秒自动接下）。
    assert_eq!(r.assign_pending, Some(oid), "强制单的确认框不能自己消失");
    assert_eq!(r.rejected, 3, "被挡下的尝试不计入额度");
    assert!(r.forced_offline, "拒满额度要强制下线");
    assert!(!r.online, "被强制下线的人不该在线");

    // 今天不许自己上线：错误码要能让手机端说清原因。
    send(&mut app, 2, ClientMessage::SetOnline { online: true });
    let v = drain(&mut rider_rx);
    let a = ack(&v);
    assert_eq!(a["payload"]["code"].as_str(), Some("FORCED_OFFLINE"));
}

#[test]
fn saved_names_and_llm_config_can_be_read_back() {
    let store = Store::in_memory().unwrap();
    assert!(load_rider_names(&store).is_none());
    assert!(load_llm_config(&store).is_none());

    store.set_meta("rider_names", r#"["甲","乙"]"#).unwrap();
    assert_eq!(
        load_rider_names(&store).unwrap(),
        vec!["甲".to_string(), "乙".to_string()]
    );

    store.set_meta("llm_api_key", "sk-test").unwrap();
    store.set_meta("llm_model", "deepseek-chat").unwrap();
    let cfg = load_llm_config(&store).unwrap();
    assert!(cfg.has_key());
    assert_eq!(cfg.model, "deepseek-chat");
    assert_eq!(cfg.base_url, "https://api.openai.com/v1", "没存过就回默认");

    // 空 Key = 没配过；坏 JSON = 没存过。
    store.set_meta("llm_api_key", "   ").unwrap();
    assert!(load_llm_config(&store).is_none());
    store.set_meta("rider_names", "{不是数组").unwrap();
    assert!(load_rider_names(&store).is_none());
}

#[test]
fn console_llm_config_never_echoes_the_key() {
    let mut app = control_app();
    assert!(!app.llm_enabled(), "默认没有 Key 就不能调 LLM");
    let (mut ctrl, _) = hello(&mut app, 1, None);
    drain(&mut ctrl);

    send(
        &mut app,
        1,
        ClientMessage::SetLlmConfig {
            api_key: Some("sk-secret-value".into()),
            base_url: Some("https://api.deepseek.com/v1".into()),
            model: Some("deepseek-chat".into()),
            layer_models: None,
        },
    );

    assert!(app.llm_config.has_key());
    assert!(app.brain.is_enabled(), "填了 Key 就该有客户端");
    assert!(app.llm_enabled(), "有 Key = 允许调用");
    // 但「真的去调」仍要控制台显式打开开关，避免重启就烧配额。
    assert!(!app.world.cfg.llm_brain_enabled);

    let v = drain(&mut ctrl);
    let text = serde_json::to_string(&v).unwrap();
    assert!(!text.contains("sk-secret-value"), "Key 绝不能回传：{text}");
    assert_eq!(
        ack(&v)["payload"]["data"]["model"].as_str(),
        Some("deepseek-chat")
    );

    // 清空 Key：连大脑开关一起落下。
    send(
        &mut app,
        1,
        ClientMessage::SetLlmConfig {
            api_key: Some(String::new()),
            base_url: None,
            model: None,
            layer_models: None,
        },
    );
    assert!(!app.llm_config.has_key());
    assert!(!app.llm_enabled());
    assert!(!app.brain.is_enabled());
    assert!(!app.world.cfg.llm_brain_enabled);
}

#[test]
fn console_can_set_per_layer_models_and_clear_them() {
    use std::collections::BTreeMap;

    let mut app = control_app();
    let (mut ctrl, _) = hello(&mut app, 1, None);
    drain(&mut ctrl);

    let mut layers = BTreeMap::new();
    layers.insert("rider".to_string(), "gpt-4o-mini".to_string());
    layers.insert("director".to_string(), "gpt-4o".to_string());
    layers.insert("station".to_string(), "   ".to_string()); // 空白 = 回退全局
    layers.insert("查无此层".to_string(), "x".to_string()); // 认不出的层名忽略

    send(
        &mut app,
        1,
        ClientMessage::SetLlmConfig {
            api_key: Some("sk-test".into()),
            base_url: Some("https://api.deepseek.com/v1".into()),
            model: Some("deepseek-chat".into()),
            layer_models: Some(layers),
        },
    );

    // 分层真的落到了各层大脑头上。
    assert_eq!(
        app.llm_config
            .for_layer(&app.layer_models, LayerKey::Rider)
            .model,
        "gpt-4o-mini"
    );
    assert_eq!(
        app.llm_config
            .for_layer(&app.layer_models, LayerKey::Director)
            .model,
        "gpt-4o"
    );
    assert_eq!(
        app.llm_config
            .for_layer(&app.layer_models, LayerKey::Station)
            .model,
        "deepseek-chat",
        "空白层要回退全局模型"
    );

    // 回执里带上当前分层，控制台才能回显；空白层不该出现。
    let v = drain(&mut ctrl);
    let echoed = &ack(&v)["payload"]["data"]["models"];
    assert_eq!(echoed["rider"].as_str(), Some("gpt-4o-mini"));
    assert_eq!(echoed["director"].as_str(), Some("gpt-4o"));
    assert!(
        echoed.get("station").is_none(),
        "空白层不该被记成配过：{echoed}"
    );

    // 再发空串 = 这一层退回全局，且不该动到别的层。
    let mut clear = BTreeMap::new();
    clear.insert("rider".to_string(), String::new());
    send(
        &mut app,
        1,
        ClientMessage::SetLlmConfig {
            api_key: None,
            base_url: None,
            model: None,
            layer_models: Some(clear),
        },
    );
    assert_eq!(
        app.llm_config
            .for_layer(&app.layer_models, LayerKey::Rider)
            .model,
        "deepseek-chat"
    );
    assert_eq!(
        app.llm_config
            .for_layer(&app.layer_models, LayerKey::Director)
            .model,
        "gpt-4o",
        "清一层不该动到别的层"
    );
}

#[test]
fn layer_models_survive_a_restart() {
    let store = Store::in_memory().unwrap();
    let mut saved = LayerModels::default();
    saved.set(LayerKey::Rider, Some("gpt-4o-mini".into()));
    store
        .set_meta("llm_layer_models", &saved.to_json().to_string())
        .unwrap();

    let back = load_layer_models(&store).expect("存过就该读得回来");
    assert_eq!(back.get(LayerKey::Rider), Some("gpt-4o-mini"));
    assert_eq!(back.get(LayerKey::Station), None);
}

#[test]
fn control_commands_change_the_world_and_notify_everyone() {
    let mut app = control_app();
    let (mut ctrl, w) = hello(&mut app, 1, None);
    assert_eq!(w["payload"]["role"].as_str(), Some("control"));

    let (mut rider, _) = hello(&mut app, 2, Some(&rider_token(1)));
    drain(&mut ctrl); // 骑手上线会给控制端推 server_info

    send(
        &mut app,
        1,
        ClientMessage::SetSpeedMultiplier { multiplier: 4 },
    );
    assert_eq!(app.world.cfg.speed_multiplier, 4);

    let v = drain(&mut ctrl);
    assert_eq!(
        ack(&v)["payload"]["data"]["speed_multiplier"].as_u64(),
        Some(4)
    );
    assert!(event(&v, "config_changed").is_some());

    let rv = drain(&mut rider);
    assert!(
        event(&rv, "config_changed").is_some(),
        "配置改动必须通知所有人"
    );
}

#[test]
fn llm_toggle_without_a_key_stays_off() {
    let mut app = control_app();
    let (mut rx, _w) = hello(&mut app, 1, None);
    send(&mut app, 1, ClientMessage::SetLlmEnabled { enabled: true });
    let v = drain(&mut rx);
    assert_eq!(
        ack(&v)["payload"]["data"]["llm_enabled"].as_bool(),
        Some(false),
        "没有 Key 时打开也只是关闭"
    );
    assert!(!app.opts.llm_enabled);
    assert!(!app.world.cfg.llm_brain_enabled);
}

// -----------------------------------------------------------------
// 连接生命周期
// -----------------------------------------------------------------

#[test]
fn resync_and_ping_are_served() {
    let mut app = app();
    let (mut rx, _w) = hello(&mut app, 1, None);

    send(&mut app, 1, ClientMessage::Resync);
    let v = drain(&mut rx);
    assert!(event(&v, "snapshot").is_some());

    send(&mut app, 1, ClientMessage::Ping);
    let v = drain(&mut rx);
    let pong = event(&v, "pong").expect("ping 必须回 pong");
    assert_eq!(
        pong["payload"]["elapsed_min"].as_u64(),
        Some(app.world.elapsed_min)
    );
}

#[test]
fn disconnecting_the_last_connection_takes_the_rider_offline() {
    let mut app = app();
    let rid = app.world.spawn_human_rider("张三");
    let (_rx, _w) = hello(&mut app, 1, Some(&rider_token(rid)));
    assert!(app.world.rider(rid).unwrap().online);

    close(&mut app, 1);
    assert!(!app.world.rider(rid).unwrap().online, "断线该下班");
}

#[test]
fn a_rider_holding_orders_stays_online_after_disconnect() {
    let mut app = app();
    let rid = app.world.spawn_human_rider("张三");
    let (mut rx, _w) = hello(&mut app, 1, Some(&rider_token(rid)));
    let oid = app.world.generate_order().expect("应该能出一单");
    send(
        &mut app,
        1,
        ClientMessage::Claim {
            order_id: oid,
            back_order: false,
        },
    );
    assert_eq!(ack(&drain(&mut rx))["payload"]["ok"].as_bool(), Some(true));

    close(&mut app, 1);
    assert!(app.world.rider(rid).unwrap().online, "手上有单不能下线");
}

/// 托管名单跟着「谁连着 WS」实时变：玩家在线 AI 让位，掉线后 AutoPilot 接手。
#[test]
fn ai_roster_yields_to_a_connected_human_and_picks_him_up_when_he_drops() {
    let mut app = app();
    let rid = app.world.spawn_human_rider("张三");
    let (_rx, _w) = hello(&mut app, 1, Some(&rider_token(rid)));

    assert!(
        !app.ai_roster().contains(&rid),
        "玩家自己连着，AI 不许替他派单"
    );

    close(&mut app, 1);

    app.host_mode = HostMode::SimulatedOnly;
    assert!(
        !app.ai_roster().contains(&rid),
        "SimulatedOnly 该等他回来，不该替他跑"
    );

    app.host_mode = HostMode::AutoPilot;
    assert!(
        app.ai_roster().contains(&rid),
        "AutoPilot 该接过掉线真人的单"
    );
}

#[test]
fn connection_limit_drops_extra_clients() {
    let opts = AppOptions {
        max_connections: 1,
        ..AppOptions::default()
    };
    let mut app = App::new(world(), Store::in_memory().unwrap(), opts);
    let _rx1 = open(&mut app, 1);
    let mut rx2 = open(&mut app, 2);
    assert!(app.hub.contains(1));
    assert!(!app.hub.contains(2), "超限连接不该进注册表");
    assert!(rx2.try_recv().is_err(), "被拒绝的连接应当立刻断开");
}

// -----------------------------------------------------------------
// tick
// -----------------------------------------------------------------

#[test]
fn a_tick_advances_the_clock_and_broadcasts_to_everyone() {
    let mut app = app();
    let (mut rx, _w) = hello(&mut app, 1, None);
    let before = app.world.elapsed_min;
    app.on_tick();
    assert_eq!(app.world.elapsed_min, before + 1);
    let v = drain(&mut rx);
    assert!(event(&v, "tick").is_some(), "每 tick 都要广播时钟");
}

#[test]
fn a_paused_world_does_not_tick() {
    let mut app = app();
    app.world.cfg.speed_multiplier = 0;
    let before = app.world.elapsed_min;
    app.on_tick();
    assert_eq!(app.world.elapsed_min, before, "暂停时世界必须冻住");
}

#[test]
fn brain_prepare_is_skipped_when_llm_is_off() {
    let mut app = app();
    assert!(!app.llm_enabled());
    app.maybe_start_plan();
    assert!(app.inflight_since.is_none());
    assert!(app.pending_plan.is_none());
}

#[test]
fn station_layer_stays_disabled_until_implemented() {
    // 站长层还没实现，必须关掉，否则编排器会把额度空耗在一个不会占坑的层上，
    // 永远轮不到骑手层。
    let app = app();
    assert!(!app.orchestrator.layer_enabled(AiLayer::Station));
}

/// 控制台可以把站长开关打开（它记的是「站长该决策了」的意图）。
/// 打开之后**绝不能把骑手层饿死**：站长这一轮到点时顺手把间隔记掉，
/// 名额留给骑手层那一轮请求（站长的限单 / 强派搭它的车下发）。
///
/// 这个用例必须让骑手那一轮**真的走到占坑**（大厅有单 + 有可派的空闲骑手）。
/// 否则 `prepare` 会以「没东西可分」提前返回，压根到不了 `begin(Rider)`：
/// 断言看着过了，其实只是空转，饿死不饿死它根本测不出来。
///
/// 跑在 tokio 运行时里是必需的：占上坑之后 `maybe_start_plan` 会 `tokio::spawn`
/// 那个请求，没有运行时会直接 panic。
#[tokio::test]
async fn an_enabled_station_layer_does_not_starve_the_rider_layer() {
    use crate::llm::LlmClient;
    let mut app = app();
    let client = || LlmClient::new(LlmConfig::new("test-key")).ok();
    app.opts.llm_enabled = true;
    app.world.cfg.llm_brain_enabled = true;
    app.brain = TacticalBrain::new(client());
    app.orchestrator = AiOrchestrator::new(client());

    // 前提：大厅有单、路上有空骑手。
    app.world.generate_order_batch(4);
    assert!(!app.world.pending_orders().is_empty(), "大厅得有单");
    assert!(!app.ai_roster().is_empty(), "得有可派的空闲骑手");

    // 让烽火台先占掉自己的间隔，于是「最优先的到期层」变成站长
    assert!(app.orchestrator.begin(AiLayer::Director, &app.world));
    app.orchestrator.finish(AiLayer::Director);
    app.orchestrator.set_layer_enabled(AiLayer::Station, true);
    assert_eq!(
        app.orchestrator.next_due(&app.world),
        Some(AiLayer::Station)
    );

    let budget = app.orchestrator.calls_today(app.world.day);
    app.maybe_start_plan();

    assert_eq!(
        app.orchestrator.stats(AiLayer::Station).calls,
        0,
        "站长不单独发请求"
    );
    assert_eq!(
        app.orchestrator.inflight(),
        Some(AiLayer::Rider),
        "站长这一轮必须顺手把骑手那一轮跑起来，而不是把这一档空耗掉"
    );
    assert_eq!(
        app.orchestrator.stats(AiLayer::Rider).calls,
        1,
        "真正占坑的是骑手层"
    );
    assert_eq!(
        app.orchestrator.calls_today(app.world.day),
        budget + 1,
        "这一轮只花掉骑手层那一次额度，站长那一次不扣"
    );

    // 站长只是被记掉了间隔，不是被关掉：再过它一个间隔就重新到期。
    // （世界是从 06:00 开局的，所以只能相对推进，不能写成绝对分钟。）
    app.orchestrator.finish(AiLayer::Rider);
    app.inflight_since = None;
    app.inflight_layer = None;
    app.world.sim_minute += AiLayer::Station.min_interval_min();
    assert_eq!(
        app.orchestrator.next_due(&app.world),
        Some(AiLayer::Station),
        "过了站长一个间隔，它该重新到期——只是这一次被记掉，不是被永久关掉"
    );
}

#[test]
fn director_world_ops_apply_to_the_world() {
    use crate::sim::weather::Weather;
    let mut app = app();
    let target = if app.world.weather == Weather::HeavyRain {
        Weather::Fog
    } else {
        Weather::HeavyRain
    };
    app.pending_world_ops = vec![
        ValidatedWorldOp::SetWeather {
            weather: target,
            why: "测试".into(),
        },
        ValidatedWorldOp::SetCommission {
            rate: 0.18,
            why: "测试".into(),
        },
        ValidatedWorldOp::Narrate("天说变就变".into()),
    ];

    app.apply_pending_world_ops();

    assert_eq!(app.world.weather, target, "天气要落地");
    assert!(
        (app.world.cfg.commission_rate - 0.18).abs() < 1e-6,
        "佣金要落地"
    );
    assert!(app.pending_world_ops.is_empty(), "落地后要清空");
    assert!(
        app.world_history
            .iter()
            .any(|h| h.starts_with("set_weather=")),
        "操作史要记下来喂下一轮 prompt"
    );
}

// -----------------------------------------------------------------
// 三层 AI：状态 / 开关 / 人工否决
// -----------------------------------------------------------------

#[test]
fn ai_state_reports_layers_budget_and_station_kpi() {
    let app = app();
    let st = app.ai_state();
    let layers = st["layers"].as_array().expect("要给三层状态");
    assert_eq!(layers.len(), 3, "骑手 / 站长 / 烽火台");
    assert!(st["budget"]["max_calls_per_day"].as_u64().unwrap() > 0);
    assert_eq!(st["station"]["rider_count"], app.world.riders.len());
    assert!(st["station"]["kpi"]["total_riders"].as_u64().unwrap() > 0);
    assert!(st["rejectable"].is_null(), "没有在途决策时不可否决");
    assert!(!st["llm_configured"].as_bool().unwrap(), "默认没配 Key");
}

#[test]
fn ai_state_reports_layer_models_and_cache_hit_rate() {
    let app = app();
    let st = app.ai_state();
    // 没配分层模型时三层都落到全局模型 —— 与旧行为一致。
    for layer in ["rider", "station", "director"] {
        assert_eq!(st["models"][layer], app.llm_config.model);
    }
    let cache = &st["cache"];
    assert_eq!(cache["hits"], 0);
    assert_eq!(cache["calls"], 0);
    assert_eq!(cache["hit_rate"], 0.0, "零调用不能除出 NaN");
}

#[test]
fn set_ai_toggles_layers_and_host_mode() {
    use crate::net::AiPatch;
    let mut app = app();
    let out = app
        .rpc_set_ai(&AiPatch {
            director: Some(false),
            host_mode: Some("autopilot".into()),
            enabled: Some(true),
            ..Default::default()
        })
        .expect("合法 patch 不该报错");
    let director = out["layers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["layer"] == "director")
        .expect("要报出烽火台那一层");
    assert!(!director["enabled"].as_bool().unwrap(), "烽火台被关");
    assert_eq!(out["host_mode"], "auto_pilot");
    assert!(app.world.cfg.llm_brain_enabled);

    let err = app.rpc_set_ai(&AiPatch {
        host_mode: Some("胡说".into()),
        ..Default::default()
    });
    assert!(err.is_err(), "未知托管模式要报错");
}

#[test]
fn reject_reverts_a_plan_that_has_not_landed() {
    let mut app = app();
    // 手搓一条「已产出、还没被 tick 消费」的骑手计划。
    app.pending_plan = Some(LlmPlan {
        decision_id: 7,
        ..Default::default()
    });
    app.pending_decision = Some((7, "rider"));

    let out = app.rpc_reject_ai(7).unwrap();
    assert_eq!(out["rejected"], true);
    assert!(app.pending_plan.is_none(), "否决后不给 tick 用");
    assert!(app.pending_decision.is_none());
    assert_eq!(app.reject_log[0]["result"], "reverted");
}

#[test]
fn reject_reverts_pending_world_ops() {
    let mut app = app();
    app.pending_world_ops = vec![ValidatedWorldOp::Narrate("别下暴雨".into())];
    app.pending_decision = Some((3, "director"));

    let out = app.rpc_reject_ai(3).unwrap();
    assert_eq!(out["rejected"], true);
    assert!(app.pending_world_ops.is_empty(), "世界操作要撤回");
}

#[test]
fn reject_of_an_applied_decision_only_leaves_a_record() {
    let mut app = app();
    let out = app.rpc_reject_ai(999).unwrap();
    assert_eq!(out["rejected"], false, "已经生效的撤不回来");
    assert_eq!(out["reason"], "该决策已生效或不存在，仅记入台账");
    assert_eq!(app.reject_log[0]["result"], "already_applied");
}

// -----------------------------------------------------------------
// 任务对话
// -----------------------------------------------------------------

/// 把一单挂到骑手名下，并塞一个「放门口」附加任务在指定步骤。
fn attach_task(app: &mut App, oid: OrderId, rid: RiderId, step: usize) {
    let o = app.world.order_mut(oid).unwrap();
    o.assigned_to = Some(rid);
    o.task = Some(ExtraTask {
        id: "leave_at_door".into(),
        name: "放门口".into(),
        step,
        done: false,
        failed: false,
        reward_cents: 0,
        penalty_cents: 0,
    });
}

#[tokio::test]
async fn task_message_requires_owning_the_order() {
    let mut app = app();
    let rid = app.world.spawn_human_rider("张三");
    let (mut rx, _w) = hello(&mut app, 1, Some(&rider_token(rid)));
    let oid = app.world.generate_order().expect("应该能出一单");

    send(
        &mut app,
        1,
        ClientMessage::TaskMessage {
            order_id: oid,
            text: "放门口".into(),
        },
    );
    let v = drain(&mut rx);
    assert_eq!(
        ack(&v)["payload"]["code"].as_str(),
        Some(ClaimError::NotAssignedToYou.code())
    );
}

#[test]
fn a_ruling_advances_the_task_and_pays_the_reward() {
    let mut app = app();
    let rid = app.world.spawn_human_rider("张三");
    let (mut rx, _w) = hello(&mut app, 1, Some(&rider_token(rid)));
    let oid = app.world.generate_order().expect("应该能出一单");
    attach_task(&mut app, oid, rid, 1);

    let reward = app
        .step_reward("leave_at_door", 1)
        .expect("模板里该有这一档奖励");
    let before = app.world.rider(rid).unwrap().cash_cents;

    app.on_ruling(PendingRuling {
        rider_id: rid,
        order_id: oid,
        step: 1,
        task_id: "leave_at_door".into(),
        rider_text: "1234".into(),
        ruling: TaskRuling {
            task_done: true,
            reason: "说对密码".into(),
            rating_delta: 0.0,
            penalty_cents: 0,
            source: RulingSource::Keyword,
        },
    });

    let task = app.world.order(oid).unwrap().task.clone().unwrap();
    assert_eq!(task.step, 2, "判定通过要推进步骤");
    assert_eq!(app.world.rider(rid).unwrap().cash_cents, before + reward);

    let v = drain(&mut rx);
    assert!(event(&v, "rider_finance").is_some(), "完成奖励要发钱");
    assert!(event(&v, "task_message").is_some());
}

#[test]
fn a_failed_ruling_keeps_the_step_and_lets_the_customer_answer() {
    let mut app = app();
    let rid = app.world.spawn_human_rider("张三");
    let (mut rx, _w) = hello(&mut app, 1, Some(&rider_token(rid)));
    let oid = app.world.generate_order().expect("应该能出一单");
    attach_task(&mut app, oid, rid, 1);
    let before = app.world.rider(rid).unwrap().cash_cents;

    app.on_ruling(PendingRuling {
        rider_id: rid,
        order_id: oid,
        step: 1,
        task_id: "leave_at_door".into(),
        rider_text: "啊？".into(),
        ruling: TaskRuling {
            task_done: false,
            reason: "没听懂".into(),
            rating_delta: 0.0,
            penalty_cents: 0,
            source: RulingSource::Keyword,
        },
    });

    let task = app.world.order(oid).unwrap().task.clone().unwrap();
    assert_eq!(task.step, 1, "没完成不能推进步骤");
    assert_eq!(app.world.rider(rid).unwrap().cash_cents, before);

    let v = drain(&mut rx);
    let msg = event(&v, "task_message").expect("顾客该回一句");
    assert_eq!(msg["payload"]["from"].as_str(), Some("customer"));
    assert!(!msg["payload"]["text"].as_str().unwrap().is_empty());
}

// -----------------------------------------------------------------
// 远程控制接口（HTTP → Inbound::Rpc）
// -----------------------------------------------------------------

fn rpc(app: &mut App, req: RpcRequest) -> RpcReply {
    app.handle_rpc(req)
}

fn item(v: serde_json::Value) -> DispatchItem {
    serde_json::from_value(v).expect("测试里的派单载荷必须合法")
}

/// 找一个「在线且手上没单」的骑手——强制指派只吃这种。
fn free_rider(app: &App) -> RiderId {
    app.world
        .riders
        .iter()
        .find(|r| r.online && r.held.is_empty())
        .map(|r| r.id)
        .expect("测试世界必须有空闲在线骑手")
}

#[test]
fn rpc_state_uses_the_control_view_but_no_server_secrets() {
    let mut app = control_app();
    let v = rpc(&mut app, RpcRequest::State).expect("取快照不该失败");

    let riders = v["riders"].as_array().expect("要有骑手列表");
    assert!(!riders.is_empty());
    assert!(
        riders[0]["takehome_cents"].is_number(),
        "控制端版本必须带收入，否则没法核算"
    );
    assert!(v["hall"].is_array());
    assert!(v["server_info"]["hub"].is_object());
    // server_info 明确不带任何 Key（技术设计 §4.4）
    let info = &v["server_info"];
    assert!(info["llm_enabled"].is_boolean());
    assert!(info["amap_enabled"].is_boolean());
}

#[test]
fn rpc_notice_overwrites_one_row_and_reaches_the_snapshot() {
    let mut app = control_app();
    let (mut ctrl, _w) = hello(&mut app, 1, None);

    let v = rpc(
        &mut app,
        RpcRequest::SetNotice {
            title: " 冲单奖 ".into(),
            body: " 跑满 10 单再奖 10 元 ".into(),
            activity_id: None,
        },
    )
    .expect("写通知不该失败");
    assert_eq!(v["notice"]["title"].as_str(), Some("冲单奖"), "标题要 trim");
    assert_eq!(
        v["notice"]["body"].as_str(),
        Some("跑满 10 单再奖 10 元"),
        "正文要 trim"
    );

    let msgs = drain(&mut ctrl);
    let ev = event(&msgs, "notice_changed").expect("必须广播，否则骑手要重连才看得见");
    assert_eq!(ev["payload"]["notice"]["title"].as_str(), Some("冲单奖"));
    assert!(ev["payload"]["notice"]["activity_id"].is_null());

    // 单条：再写一次就是覆盖，不是追加
    rpc(
        &mut app,
        RpcRequest::SetNotice {
            title: "第二版".into(),
            body: "换句话".into(),
            activity_id: None,
        },
    )
    .expect("覆盖不该失败");
    assert_eq!(
        app.world.notice.as_ref().map(|n| n.title.as_str()),
        Some("第二版")
    );

    // 快照里也带着，刚重连的骑手能立刻看到
    let snap = rpc(&mut app, RpcRequest::State).expect("取快照不该失败");
    assert_eq!(snap["notice"]["title"].as_str(), Some("第二版"));

    // 清空后广播 null，手机端据此收起条子
    drain(&mut ctrl);
    rpc(&mut app, RpcRequest::ClearNotice).expect("清空不该失败");
    let msgs = drain(&mut ctrl);
    let ev = event(&msgs, "notice_changed").expect("清空也要广播");
    assert!(ev["payload"]["notice"].is_null());
    assert!(app.world.notice.is_none());
}

#[test]
fn rpc_notice_rejects_bad_text_and_unknown_activity() {
    let mut app = control_app();

    let err = rpc(
        &mut app,
        RpcRequest::SetNotice {
            title: "   ".into(),
            body: "正文".into(),
            activity_id: None,
        },
    )
    .expect_err("空标题该被拒");
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);

    let err = rpc(
        &mut app,
        RpcRequest::SetNotice {
            title: "标题".into(),
            body: String::new(),
            activity_id: None,
        },
    )
    .expect_err("空正文该被拒");
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);

    let err = rpc(
        &mut app,
        RpcRequest::SetNotice {
            title: "标题".into(),
            body: "正文".into(),
            activity_id: Some(9999),
        },
    )
    .expect_err("关联一个不存在的活动该被拒");
    assert_eq!(err.code, "ACTIVITY_NOT_FOUND");
    assert_eq!(err.status, axum::http::StatusCode::CONFLICT);
    assert!(app.world.notice.is_none(), "校验失败不能留下半截状态");
}

#[test]
fn rpc_dispatch_assigns_the_order_and_tells_the_console() {
    let mut app = control_app();
    let (mut ctrl, _w) = hello(&mut app, 1, None);
    let oid = app.world.generate_order().expect("应该能出一单");
    let rid = free_rider(&app);

    let r = rpc(
        &mut app,
        RpcRequest::Dispatch(vec![item(json!({
            "rider_id": format!("r{rid}"),
            "order_id": oid,
            "reason": "老板指派，优先送",
        }))]),
    )
    .expect("派单不该失败");

    assert_eq!(r["applied"].as_u64(), Some(1));
    assert_eq!(r["failed"].as_u64(), Some(0));
    let first = &r["results"][0];
    assert_eq!(first["ok"].as_bool(), Some(true));
    assert_eq!(
        first["rider_key"].as_str(),
        Some(format!("r{rid}").as_str())
    );
    assert_eq!(first["reason"].as_str(), Some("老板指派，优先送"));
    assert_eq!(
        app.world.order(oid).unwrap().assigned_to,
        Some(rid),
        "订单必须真的落到那个骑手身上"
    );

    let msgs = drain(&mut ctrl);
    let claimed = event(&msgs, "order_claimed").expect("控制台要看到抢单");
    assert_eq!(claimed["payload"]["forced"].as_bool(), Some(true));
    let decision = event(&msgs, "ai_decision").expect("要留一条派单决策");
    assert_eq!(decision["payload"]["source"].as_str(), Some("manual"));
    assert_eq!(
        decision["payload"]["assignments"][0]["reason"].as_str(),
        Some("老板指派，优先送")
    );
}

#[test]
fn rpc_dispatch_reports_each_failure_and_keeps_going() {
    let mut app = control_app();
    let oid = app.world.generate_order().expect("应该能出一单");
    let rid = free_rider(&app);

    let r = rpc(
        &mut app,
        RpcRequest::Dispatch(vec![
            // 解析不出来的 key
            item(json!({ "rider_id": "rider", "order_id": oid })),
            // 骑手不存在
            item(json!({ "rider_id": 9999, "order_id": oid })),
            // 正常
            item(json!({ "rider_id": rid, "order_id": oid })),
            // 同一单重复派
            item(json!({ "rider_id": rid, "order_id": oid })),
        ]),
    )
    .expect("批量派单本身不该失败");

    assert_eq!(r["applied"].as_u64(), Some(1));
    assert_eq!(r["failed"].as_u64(), Some(3));
    let results = r["results"].as_array().unwrap();
    assert_eq!(results[0]["code"].as_str(), Some("RIDER_NOT_FOUND"));
    assert_eq!(results[1]["code"].as_str(), Some("RIDER_NOT_FOUND"));
    assert_eq!(results[2]["ok"].as_bool(), Some(true));
    assert_eq!(results[3]["code"].as_str(), Some("ALREADY_CLAIMED"));
}

#[test]
fn rpc_set_online_turns_a_business_error_into_a_conflict() {
    let mut app = control_app();
    let err = rpc(
        &mut app,
        RpcRequest::SetRiderOnline {
            rider_id: 9999,
            online: false,
        },
    )
    .expect_err("不存在的骑手该报错");
    assert_eq!(err.code, "RIDER_NOT_FOUND");
    assert_eq!(err.status, axum::http::StatusCode::CONFLICT);
}

#[test]
fn rpc_set_online_works_and_broadcasts() {
    let mut app = control_app();
    let (mut ctrl, _w) = hello(&mut app, 1, None);
    let rid = free_rider(&app);

    let v = rpc(
        &mut app,
        RpcRequest::SetRiderOnline {
            rider_id: rid,
            online: false,
        },
    )
    .expect("空闲骑手可以下线");
    assert_eq!(v["online"].as_bool(), Some(false));
    assert!(!app.world.rider(rid).unwrap().online);

    let msgs = drain(&mut ctrl);
    assert!(event(&msgs, "rider_state_changed").is_some());
}

#[test]
fn rpc_config_patch_is_clamped_and_broadcast() {
    let mut app = control_app();
    let (mut ctrl, _w) = hello(&mut app, 1, None);

    let patch = crate::net::ConfigPatch {
        // 越界值：服务端负责夹取，不需要客户端自觉
        claim_window_secs: Some(99),
        backorder_max: Some(0),
        ..Default::default()
    };
    let v = rpc(&mut app, RpcRequest::SetConfig(patch)).expect("改配置不该失败");

    assert_eq!(app.world.cfg.claim_window_secs, 10);
    assert_eq!(app.world.cfg.backorder.max_held, 1);
    assert_eq!(v["claim_window_secs"].as_u64(), Some(10));

    let msgs = drain(&mut ctrl);
    assert!(
        event(&msgs, "config_changed").is_some(),
        "配置改动必须广播，否则控制台停在旧值"
    );
}

// -----------------------------------------------------------------
// 账号体系：烽火台建号 → 扫码 / 手填登录
// -----------------------------------------------------------------

/// 名字 + 密码握手。返回出站队列，welcome / error 由调用方自己判。
fn hello_with_password(
    app: &mut App,
    id: ConnId,
    name: &str,
    password: &str,
) -> UnboundedReceiver<String> {
    let rx = open(app, id);
    send(
        app,
        id,
        ClientMessage::Hello {
            token: None,
            rider_name: Some(name.to_string()),
            password: Some(password.to_string()),
            protocol_version: None,
        },
    );
    rx
}

fn create_account(app: &mut App, name: &str, password: Option<&str>) -> serde_json::Value {
    rpc(
        app,
        RpcRequest::CreateAccount {
            name: name.to_string(),
            password: password.map(str::to_string),
        },
    )
    .expect("建号本身不该失败")
}

#[test]
fn create_account_returns_a_one_time_password_and_a_scannable_qr() {
    let mut app = control_app();
    let v = create_account(&mut app, " 张师傅 ", Some("pass1234"));

    assert_eq!(v["name"].as_str(), Some("张师傅"), "名字要规范化");
    assert_eq!(v["password"].as_str(), Some("pass1234"));
    let uri = v["join"]["uri"].as_str().expect("要给连接串");
    assert!(
        uri.starts_with("ridersim://login?"),
        "扫码走自定义 scheme：{uri}"
    );
    assert!(uri.contains("host=") && uri.contains("name=") && uri.contains("password="));
    assert!(v["join"]["qr_svg"].is_string(), "二维码就该画出来了");
    assert!(
        v["hosts"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false),
        "至少要给一个手机可达地址"
    );
}

#[test]
fn creating_an_account_without_a_password_mints_one() {
    let mut app = control_app();
    let v = create_account(&mut app, "李师傅", None);
    let pw = v["password"].as_str().expect("管理员不想密码时服务端生成");
    assert!(
        crate::auth::check_password(pw).is_ok(),
        "生成的密码必须合规"
    );

    // 生成的那个密码要真能登：库里存的哈希必须对得上
    let mut rx = hello_with_password(&mut app, 1, "李师傅", pw);
    assert!(event(&drain(&mut rx), "welcome").is_some());
}

#[test]
fn account_login_reuses_the_body_across_reconnects() {
    let mut app = control_app();
    create_account(&mut app, "张师傅", Some("pass1234"));

    let mut rx1 = hello_with_password(&mut app, 1, "张师傅", "pass1234");
    let msgs = drain(&mut rx1);
    let w1 = event(&msgs, "welcome").expect("正确的账号密码该放行");
    assert_eq!(w1["payload"]["role"].as_str(), Some("rider"));
    let rid = w1["payload"]["rider_id"].as_u64().expect("骑手要有个身体");

    close(&mut app, 1);

    let mut rx2 = hello_with_password(&mut app, 2, "张师傅", "pass1234");
    let msgs = drain(&mut rx2);
    let w2 = event(&msgs, "welcome").expect("重连还得能进");
    assert_eq!(
        w2["payload"]["rider_id"].as_u64(),
        Some(rid),
        "重连必须复用同一具身体，不能再长一具"
    );
    assert_eq!(
        app.world
            .riders
            .iter()
            .filter(|r| r.kind == RiderKind::Human && r.name == "张师傅")
            .count(),
        1
    );
}

#[test]
fn account_login_tells_missing_account_apart_from_wrong_password() {
    let mut app = control_app();

    // 没建号：要让骑手知道该去烽火台要二维码，而不是一直重试密码
    let mut rx = hello_with_password(&mut app, 1, "查无此人", "whatever");
    let msgs = drain(&mut rx);
    let err = event(&msgs, "error").expect("没建号就该拒");
    assert_eq!(err["payload"]["code"].as_str(), Some(code::NO_ACCOUNT));
    assert!(!app.hub.contains(1), "拒绝后要踢掉连接");

    create_account(&mut app, "张师傅", Some("rightpass"));

    let mut rx = hello_with_password(&mut app, 2, "张师傅", "wrongpass");
    let msgs = drain(&mut rx);
    let err = event(&msgs, "error").expect("密码错就该拒");
    assert_eq!(err["payload"]["code"].as_str(), Some(code::UNAUTHORIZED));
    assert!(!app.hub.contains(2));
}

#[test]
fn creating_the_same_account_again_conflicts_and_keeps_the_old_password() {
    let mut app = control_app();
    create_account(&mut app, "张师傅", Some("oldpass1"));

    let err = rpc(
        &mut app,
        RpcRequest::CreateAccount {
            name: "张师傅".into(),
            password: Some("newpass1".into()),
        },
    )
    .expect_err("重名建号必须被拒，否则老二维码会莫名失效");
    assert_eq!(err.status, axum::http::StatusCode::CONFLICT);
    assert_eq!(err.code, code::BAD_CREDENTIALS);

    // 老密码还在：冲突不能顺手把密码覆盖了
    let mut rx = hello_with_password(&mut app, 1, "张师傅", "oldpass1");
    assert!(event(&drain(&mut rx), "welcome").is_some());
}

#[test]
fn too_short_passwords_are_rejected_at_creation() {
    let mut app = control_app();
    let err = rpc(
        &mut app,
        RpcRequest::CreateAccount {
            name: "张师傅".into(),
            password: Some("123".into()),
        },
    )
    .expect_err("太短的密码该在服务端就被拦下");
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
}

#[test]
fn change_password_needs_the_old_one_then_swaps_it() {
    let mut app = control_app();
    create_account(&mut app, "张师傅", Some("oldpass1"));
    let mut rx = hello_with_password(&mut app, 1, "张师傅", "oldpass1");
    drain(&mut rx); // welcome

    // 旧密码错：光连上了不足以改密
    send(
        &mut app,
        1,
        ClientMessage::ChangePassword {
            old_password: "nope".into(),
            new_password: "newpass1".into(),
        },
    );
    let msgs = drain(&mut rx);
    let ack = event(&msgs, "ack").expect("改密要回 ack");
    assert_eq!(ack["payload"]["ok"].as_bool(), Some(false));
    assert_eq!(ack["payload"]["code"].as_str(), Some(code::UNAUTHORIZED));

    // 旧密码对
    send(
        &mut app,
        1,
        ClientMessage::ChangePassword {
            old_password: "oldpass1".into(),
            new_password: "newpass1".into(),
        },
    );
    let msgs = drain(&mut rx);
    let ack = event(&msgs, "ack").expect("改密要回 ack");
    assert_eq!(ack["payload"]["ok"].as_bool(), Some(true));
    assert_eq!(ack["payload"]["action"].as_str(), Some("change_password"));

    // 新密码能登、旧密码不能
    close(&mut app, 1);
    let mut rx2 = hello_with_password(&mut app, 2, "张师傅", "newpass1");
    assert!(event(&drain(&mut rx2), "welcome").is_some(), "新密码该能进");
    let mut rx3 = hello_with_password(&mut app, 3, "张师傅", "oldpass1");
    let msgs = drain(&mut rx3);
    let err = event(&msgs, "error").expect("旧密码该失效");
    assert_eq!(err["payload"]["code"].as_str(), Some(code::UNAUTHORIZED));
}

#[test]
fn resetting_a_password_on_a_missing_account_is_a_404() {
    let mut app = control_app();
    let err = rpc(
        &mut app,
        RpcRequest::SetAccountPassword {
            name: "查无此人".into(),
            password: None,
        },
    )
    .expect_err("没有这个号就不该有密码");
    assert_eq!(err.status, axum::http::StatusCode::NOT_FOUND);
    assert_eq!(err.code, code::NO_ACCOUNT);
}

#[test]
fn resetting_a_password_invalidates_the_old_qr() {
    let mut app = control_app();
    create_account(&mut app, "张师傅", Some("oldpass1"));

    let v = rpc(
        &mut app,
        RpcRequest::SetAccountPassword {
            name: "张师傅".into(),
            password: None,
        },
    )
    .expect("重置不该失败");
    let fresh = v["password"].as_str().expect("重置要回一次新明文");

    let mut rx = hello_with_password(&mut app, 1, "张师傅", "oldpass1");
    assert!(
        event(&drain(&mut rx), "error").is_some(),
        "旧二维码里的密码必须失效"
    );
    let mut rx2 = hello_with_password(&mut app, 2, "张师傅", fresh);
    assert!(event(&drain(&mut rx2), "welcome").is_some());
}

#[test]
fn rpc_accounts_marks_who_is_on_the_field() {
    let mut app = control_app();
    create_account(&mut app, "张师傅", Some("pass1234"));
    create_account(&mut app, "李师傅", Some("pass1234"));

    let before = rpc(&mut app, RpcRequest::Accounts).expect("取名单不该失败");
    let list = before["accounts"].as_array().expect("要有账号列表");
    assert_eq!(list.len(), 2);
    assert!(
        list.iter().all(|a| a["rider_id"].is_null()),
        "还没登录就不该有身体"
    );

    let _rx = hello_with_password(&mut app, 1, "张师傅", "pass1234");

    let after = rpc(&mut app, RpcRequest::Accounts).unwrap();
    let zhang = after["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"].as_str() == Some("张师傅"))
        .expect("张师傅该在名单里");
    assert!(zhang["rider_id"].is_number(), "登录后烽火台要看到身体");
    assert_eq!(zhang["online"].as_bool(), Some(true));
}

#[test]
fn deleting_an_account_stops_the_login() {
    let mut app = control_app();
    create_account(&mut app, "张师傅", Some("pass1234"));
    let v = rpc(
        &mut app,
        RpcRequest::DeleteAccount {
            name: "张师傅".into(),
        },
    )
    .expect("删号不该失败");
    assert_eq!(v["deleted"].as_bool(), Some(true));

    let mut rx = hello_with_password(&mut app, 1, "张师傅", "pass1234");
    let msgs = drain(&mut rx);
    let err = event(&msgs, "error").expect("删号后不该还能登");
    assert_eq!(err["payload"]["code"].as_str(), Some(code::NO_ACCOUNT));
}

#[test]
fn renaming_a_rider_moves_their_account_name() {
    let mut app = control_app();
    create_account(&mut app, "张师傅", Some("pass1234"));
    let mut rx = hello_with_password(&mut app, 1, "张师傅", "pass1234");
    drain(&mut rx); // welcome
    let rid = app
        .world
        .human_rider_named("张师傅")
        .expect("登录后该有身体");

    let (mut ctrl, _w) = hello(&mut app, 2, None);
    send(
        &mut app,
        2,
        ClientMessage::RenameRider {
            rider_id: rid,
            name: "张三".into(),
        },
    );
    drain(&mut ctrl);

    assert!(
        app.store.account("张三").unwrap().is_some(),
        "账号名要跟着骑手名一起搬"
    );
    assert!(
        app.store.account("张师傅").unwrap().is_none(),
        "旧名字不该还留着账号"
    );
}

#[test]
fn renaming_onto_an_existing_account_is_refused_whole() {
    let mut app = control_app();
    create_account(&mut app, "张师傅", Some("pass1234"));
    create_account(&mut app, "李师傅", Some("pass1234"));
    let mut rx = hello_with_password(&mut app, 1, "张师傅", "pass1234");
    drain(&mut rx);
    let rid = app.world.human_rider_named("张师傅").unwrap();

    let (mut ctrl, _w) = hello(&mut app, 2, None);
    send(
        &mut app,
        2,
        ClientMessage::RenameRider {
            rider_id: rid,
            name: "李师傅".into(),
        },
    );
    let msgs = drain(&mut ctrl);
    let ack = event(&msgs, "ack").expect("改名要回 ack");
    assert_eq!(ack["payload"]["ok"].as_bool(), Some(false));

    // 关键：不能世界改了名、账号却没搬——那会把李师傅的号指向张师傅的身体
    assert_eq!(
        app.world.rider(rid).unwrap().name,
        "张师傅",
        "改成别人已有的账号名时，整个改名都不该发生"
    );
    assert!(app.store.account("张师傅").unwrap().is_some());
    assert!(app.store.account("李师傅").unwrap().is_some());
}
