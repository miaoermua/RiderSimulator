//! 设计文档 §九 的验收测试。
//!
//! 这五项是「不做完不许进入下一阶段」的硬指标：
//!   1. 连续 10 万 tick 不 panic，账目自洽（现金**允许为负**，其余分项非负）；
//!   2. 10 个客户端抢 1 单，成功者恰好 1 个；
//!   3. 同一个种子跑两遍，结果完全一致；
//!   4. 时间不漂移（tick 数与模拟分钟严格 1:1）；
//!   5. 一条 CLI 命令打印出完整的一天流水（`bin/ledger.rs`）。

use rider_sim::sim::{
    RiderId,
    config::{MAX_REDISPATCH_ATTEMPTS, SimConfig},
    event::{DecisionSource, SimEvent},
    order::OrderStatus,
    rider::SpeedMode,
    scheduler,
    tick::{TickInput, tick},
    world::World,
};
use rider_sim::store::Store;

/// 造一个「标准测试世界」：40 个商户、若干骑手，全部在线。
fn world(seed: u64) -> World {
    World::new_for_test(SimConfig::default(), seed)
}

/// 账目自洽性检查。
///
/// 关键变化：**现金允许为负**。罚单超过当日收入时骑手就是在倒欠平台，
/// 这是设计要的结果（见 `economy::debit` 的说明），不能再像旧版那样把余额
/// 夹在 0、把差额藏进 `debt_cents`——那会让「跑一天倒贴钱」这件事在账单上
/// 彻底消失。所以这里改成检查：现金不为溢出产物、其余分项非负、欠款字段
/// 在新版逻辑下恒为 0。
fn assert_books_consistent(w: &World, at_tick: u64) {
    for r in &w.riders {
        assert!(r.fines_cents >= 0, "tick {at_tick}: 骑手 {} 罚单为负", r.id);
        assert!(
            r.insurance_cents >= 0,
            "tick {at_tick}: 骑手 {} 保险为负",
            r.id
        );
        assert!(
            r.gross_cents >= 0,
            "tick {at_tick}: 骑手 {} 毛收入为负",
            r.id
        );
        // 新版 `debit` 只扣现金、不写欠款，所以欠款必须一直是 0。
        // 若哪天又有人把它当「夹到 0 的垃圾桶」用，这里会立刻炸。
        assert_eq!(
            r.debt_cents, 0,
            "tick {at_tick}: 骑手 {} 欠款应为 0，实际 {}",
            r.id, r.debt_cents
        );
        assert!(
            r.cash_cents > i64::MIN / 2,
            "tick {at_tick}: 骑手 {} 现金离谱 {}（疑似金额溢出）",
            r.id,
            r.cash_cents
        );
        assert_eq!(
            r.takehome_cents(),
            r.cash_cents,
            "tick {at_tick}: 骑手 {} 到手口径与现金对不上",
            r.id
        );
        assert!(r.lifetime_completed >= r.completed);
        assert!(r.level >= 1, "等级必须从 1 开始");
        assert!((4.0..=5.0).contains(&r.rating), "评分越界：{}", r.rating);
    }
}

/// 明细里每条金额都必须能对上钱，且必须写清原因。
fn assert_ledger_consistent(w: &World) {
    for r in &w.riders {
        for e in &r.last_ledger {
            assert!(!e.reason.is_empty(), "流水必须带原因：{:?}", e);
        }
    }
}

#[test]
fn hundred_thousand_ticks_no_panic_and_books_stay_consistent() {
    let mut w = world(20260407);
    // 故意让一部分骑手开到「无所谓」档，好把事故 / 罚单 / 欠款这些分支跑出来。
    for (i, r) in w.riders.iter_mut().enumerate() {
        r.speed_mode = match i % 4 {
            0 => SpeedMode::Unrestricted,
            1 => SpeedMode::Tolerant,
            _ => SpeedMode::Compliant,
        };
    }

    for i in 1..=100_000u64 {
        let _events = tick(&mut w, TickInput::default());
        if i % 1_000 == 0 {
            assert_books_consistent(&w, i);
            assert_ledger_consistent(&w);
        }
    }
    assert_books_consistent(&w, 100_000);
    assert!(w.elapsed_min == 100_000, "tick 数必须等于模拟分钟数");
    assert!(w.stats.orders_created > 0, "跑了 10 万分钟居然一单没出");
    assert!(
        w.stats.orders_delivered > 0,
        "跑了 10 万分钟居然一单没送出去"
    );
}

#[test]
fn claiming_is_atomic_exactly_one_winner() {
    let mut w = world(7);
    // 清掉出厂骑手，换成 10 个真人骑手，模拟 10 个客户端并发点抢单。
    w.riders.clear();
    let mut riders: Vec<RiderId> = Vec::new();
    for i in 0..10 {
        riders.push(w.spawn_human_rider(format!("骑手{i}")));
    }
    // 真人骑手默认是下线状态，得先上线才能抢单。
    for rid in &riders {
        scheduler::set_online(&mut w, *rid, true).expect("上线应该成功");
    }
    let order = w.generate_order().expect("应该能出一单");

    // 单线程串行执行 —— 服务端唯一的执行模型。谁先到谁赢，没有竞态。
    let mut winners = 0;
    let mut losers = 0;
    for rid in &riders {
        match scheduler::try_claim(&mut w, order, *rid) {
            Ok(_) => winners += 1,
            Err(_) => losers += 1,
        }
    }
    assert_eq!(winners, 1, "10 个客户端抢 1 单，赢家必须恰好 1 个");
    assert_eq!(losers, 9);

    // 赢家只有一个，而且订单确实挂在他身上。
    let holder_id = w
        .riders
        .iter()
        .find(|r| r.is_holding(order))
        .expect("必须有一个骑手持单")
        .id;
    assert_eq!(w.order(order).unwrap().assigned_to, Some(holder_id));

    // 同一个人再抢一次也必须失败（不能重复接单）。
    assert!(scheduler::try_claim(&mut w, order, holder_id).is_err());
}

#[test]
fn same_seed_reproduces_identical_world() {
    let run = || {
        let mut w = world(12345);
        for _ in 0..3_000 {
            tick(&mut w, TickInput::default());
        }
        let rider_digest: Vec<(i64, u32, u32, f32)> = w
            .riders
            .iter()
            .map(|r| (r.cash_cents, r.completed, r.lifetime_completed, r.rating))
            .collect();
        (
            w.stats.orders_created,
            w.stats.orders_delivered,
            w.stats.accidents,
            w.stats.camera_fines,
            rider_digest,
        )
    };
    let a = run();
    let b = run();
    assert_eq!(a, b, "同种子必须逐比特可复现（LLM 绝不参与随机数）");
}

#[test]
fn time_never_drifts() {
    let mut w = world(99);
    let start_minute = w.sim_minute;
    for i in 1..=5_000u64 {
        tick(&mut w, TickInput::default());
        let expected = (start_minute as u64 + i) % 1440;
        assert_eq!(w.sim_minute as u64, expected, "第 {i} tick 后当日分钟漂了");
        assert_eq!(w.elapsed_min, i, "第 {i} tick 后单调分钟漂了");
    }
    assert_eq!(w.day as u64, 1 + (start_minute as u64 + 5_000) / 1440);
}

/// 硬指标 #5：不用开控制台，一条命令看完一天流水。
///
/// 这里跑的是真链路：tick 出 `DayClosed` → 落 SQLite → 子进程跑
/// `ledger` 二进制读回来。中间任何一环断了都会失败。
#[test]
fn one_cli_command_prints_a_full_day_ledger() {
    let mut w = world(31415);
    w.cfg.speed_multiplier = 60;

    let mut day = 0u32;
    let mut ledgers = Vec::new();
    for _ in 0..(2 * 1440) {
        for ev in tick(&mut w, TickInput::default()) {
            if let SimEvent::DayClosed {
                day: d, ledgers: l, ..
            } = ev
            {
                day = d;
                ledgers = l;
            }
        }
        if day > 0 {
            break;
        }
    }
    assert!(day > 0, "跑了两天还没日结");
    assert!(!ledgers.is_empty(), "日结流水不能为空");

    let mut db = std::env::temp_dir();
    db.push(format!("rider-acceptance-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let mut store = Store::open(&db).expect("建库失败");
    store.save_day(day, &ledgers).expect("落库失败");

    let run = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_ledger"))
            .args(args)
            .output()
            .expect("ledger 二进制没跑起来");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    let db_s = db.display().to_string();

    let (ok, stdout, stderr) = run(&["--db", &db_s, "days"]);
    assert!(ok, "days 失败：{stderr}");
    assert!(
        stdout.contains(&day.to_string()),
        "days 没列出第 {day} 天：{stdout}"
    );

    let (ok, stdout, stderr) = run(&["--db", &db_s, "day", &day.to_string()]);
    assert!(ok, "day 失败：{stderr}");
    assert!(stdout.contains(&format!("第 {day} 天流水")));
    // 每个骑手一行，名字必须出现
    for l in &ledgers {
        assert!(
            stdout.contains(&l.key),
            "流水里少了骑手 {}：{stdout}",
            l.key
        );
    }
    assert!(stdout.contains("合计："), "流水末尾必须给合计");

    let (ok, stdout, _) = run(&["--db", &db_s, "day", &day.to_string(), "--json"]);
    assert!(ok);
    let parsed: Vec<serde_json::Value> =
        serde_json::from_str(&stdout).expect("--json 必须是合法 JSON");
    assert_eq!(parsed.len(), ledgers.len(), "--json 条数必须与日结一致");

    let (ok, stdout, _) = run(&["--db", &db_s, "top", "5"]);
    assert!(ok);
    assert!(stdout.contains("生涯排行榜"));

    // 查不存在的天必须失败，而不是静默给空
    let bad_day = day + 99;
    let (ok, _, stderr) = run(&["--db", &db_s, "day", &bad_day.to_string()]);
    assert!(!ok, "查不存在的天必须非 0 退出");
    assert!(stderr.contains("没有流水"), "错误提示不清：{stderr}");

    let _ = std::fs::remove_file(&db);
}

#[test]
fn speed_multiplier_only_changes_real_time_pacing() {
    let mut cfg = SimConfig::default();
    for (mult, expect_ms) in [(1u32, 1000u64), (2, 500), (10, 100), (60, 16), (0, 1000)] {
        cfg.speed_multiplier = mult;
        assert_eq!(
            cfg.tick_interval_ms(),
            expect_ms,
            "{mult}x 的 tick 间隔不对"
        );
    }
    // 倍率不改变「1 tick = 1 模拟分钟」这个语义。
    let mut fast = world(1);
    fast.cfg.speed_multiplier = 60;
    for i in 1..=1_000u64 {
        tick(&mut fast, TickInput::default());
        assert_eq!(fast.elapsed_min, i);
    }
}

/// 硬约束：**不允许流单**。跑一整天（1 模拟分钟 1 tick），任何一单都不许落到 `Expired`。
#[test]
fn no_order_ever_flows_away_in_a_full_day() {
    let mut w = world(20261010);
    for _ in 0..1_440u64 {
        tick(&mut w, TickInput::default());
    }
    let flowed: Vec<_> = w
        .orders
        .iter()
        .filter(|o| o.status == OrderStatus::Expired)
        .map(|o| o.id)
        .collect();
    assert!(flowed.is_empty(), "这些单流掉了：{flowed:?}");
    assert_eq!(w.stats.orders_expired, 0, "整个模拟里不允许有流单出口");
    assert!(w.stats.orders_created > 0, "跑了一天居然一单没出");
}

/// 送达算法回归：跑完一整天，超时不能是常态。
///
/// 修复前承诺时限里既没有「骑手到店」那一段，也没把高峰出餐算进去，
/// 跑一天几乎每一单都超时——顺手还把「背单全崩」的连坐罚款刷爆。
/// 这里不要求每单都准时（超速罚款、事故停工、暴雨都是设计里该有的压力），
/// 只要求别全军覆没。
#[test]
fn a_days_deliveries_are_not_all_late() {
    let mut w = world(20261011);
    for _ in 0..1_440u64 {
        tick(&mut w, TickInput::default());
    }
    let delivered = w.stats.orders_delivered;
    let late = w.stats.orders_late;
    println!("一天：送达 {delivered} 单，超时 {late} 单");
    assert!(delivered > 0, "跑了一天居然一单都没送到");
    assert!(
        late * 100 / delivered < 60,
        "一天送到 {delivered} 单，其中超时 {late} 单——承诺时限又和实际行程脱节了"
    );
}

/// 没人抢的单在重投用尽后被**强制指派给最近的骑手**，而不是流掉。
///
/// 前半段把 AI 全停机（`ai_off`）制造「全城没人接单」，让订单一路重投到顶；
/// 后半段把 AI 打开，订单必须被救单——来源标成 `algorithm_rescue`，事件带 `forced`。
#[test]
fn exhausted_order_is_force_assigned_to_the_nearest_rider() {
    let mut w = world(4242);
    let off = TickInput {
        ai_off: true,
        ..Default::default()
    };
    let on = TickInput::default(); // 只托管模拟骑手

    // 1. AI 全停机：没人抢单，订单反复重投直到把次数耗光。
    for _ in 0..60u64 {
        tick(&mut w, off.clone());
    }
    let exhausted: Vec<_> = w
        .orders
        .iter()
        .filter(|o| {
            o.status == OrderStatus::Pending && o.redispatch_count >= MAX_REDISPATCH_ATTEMPTS
        })
        .map(|o| o.id)
        .collect();
    assert!(
        !exhausted.is_empty(),
        "全停机跑了 60 分钟，总该有单把重投次数耗光"
    );
    assert_eq!(w.stats.orders_expired, 0, "停机期间也不许流单");

    // 2. 打开 AI：这些单必须被强制指派（`forced: true`），而不是一直挂着。
    let mut rescued: Vec<_> = Vec::new();
    let mut rescue_decisions = 0usize;
    for _ in 0..40u64 {
        for ev in tick(&mut w, on.clone()) {
            match ev {
                SimEvent::OrderClaimed {
                    order_id,
                    forced: true,
                    ..
                } => rescued.push(order_id),
                // 救单必须进决策流，来源标成 `algorithm_rescue`：它是**算法**决策，不是 LLM。
                SimEvent::AiDecision {
                    source: DecisionSource::AlgorithmRescue,
                    layer,
                    decision_id,
                    ..
                } => {
                    rescue_decisions += 1;
                    assert_eq!(layer, "algorithm");
                    assert_eq!(decision_id, 0, "救单不可人工否决");
                }
                _ => {}
            }
        }
        if !rescued.is_empty() {
            break;
        }
    }
    assert!(!rescued.is_empty(), "重投用尽的单必须被救单，不能一直挂着");
    assert!(
        rescue_decisions > 0,
        "救单要进决策流并标成 algorithm_rescue"
    );
    assert!(w.stats.orders_rescued > 0, "救单次数要记账");
    assert!(
        rescued.iter().any(|id| exhausted.contains(id)),
        "被救的必须就是那些重投耗光的单：救了 {rescued:?}，耗光的是 {exhausted:?}"
    );
    assert_eq!(w.stats.orders_expired, 0);
}

/// 救单挑的是**最近的**那个骑手。
///
/// 直接把一个骑手挪到商家门口、另一个挪到城市另一头，救单必须选近的那个。
#[test]
fn rescue_picks_the_closest_available_rider() {
    let mut w = world(777);
    let ids = w.generate_order_batch(1);
    let order_id = ids[0];
    let merchant = w.order(order_id).expect("订单还在").merchant_pos();

    let near = w.riders[0].id;
    let far = w.riders[1].id;
    w.rider_mut(near).expect("骑手在").set_pos(merchant);
    w.rider_mut(far)
        .expect("骑手在")
        .set_pos(rider_sim::sim::geo::LatLng::new(
            merchant.lat + 0.5,
            merchant.lng + 0.5,
        ));

    let a = scheduler::rescue_nearest(&mut w, order_id).expect("必须救得回来");
    assert_eq!(a.rider, near, "救单要选最近的骑手");
    assert!(!a.reason.is_empty(), "救单必须说明理由");
    assert!(w.rider(near).expect("骑手在").is_holding(order_id));
    assert_eq!(w.stats.orders_rescued, 1);
}

/// 一个骑手都派不出去时，订单**留在大厅**，绝不流单。
#[test]
fn rescue_holds_the_order_when_nobody_is_available() {
    let mut w = world(31);
    let ids = w.generate_order_batch(1);
    let order_id = ids[0];
    for r in w.riders.iter_mut() {
        r.online = false;
    }
    assert!(
        scheduler::rescue_nearest(&mut w, order_id).is_none(),
        "没人能接就不该强塞"
    );
    let o = w.order(order_id).expect("订单还在");
    assert_eq!(o.status, OrderStatus::Pending, "派不出去就留在厅里等");
    assert_eq!(w.stats.orders_expired, 0, "派不出去也不许流单");
    assert!(w.stats.orders_rescued == 0);
}

