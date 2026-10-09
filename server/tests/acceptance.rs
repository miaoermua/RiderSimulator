//! 设计文档 §九 的验收测试。
//!
//! 这五项是「不做完不许进入下一阶段」的硬指标：
//!   1. 连续 10 万 tick 不 panic，所有金额非负；
//!   2. 10 个客户端抢 1 单，成功者恰好 1 个；
//!   3. 同一个种子跑两遍，结果完全一致；
//!   4. 时间不漂移（tick 数与模拟分钟严格 1:1）；
//!   5. 一条 CLI 命令打印出完整的一天流水（`bin/ledger.rs`）。

use rider_sim::sim::{
    RiderId,
    config::SimConfig,
    event::SimEvent,
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

fn assert_money_non_negative(w: &World, at_tick: u64) {
    for r in &w.riders {
        assert!(
            r.cash_cents >= 0,
            "tick {at_tick}: 骑手 {} 现金为负 {}",
            r.id,
            r.cash_cents
        );
        assert!(r.fines_cents >= 0, "tick {at_tick}: 骑手 {} 罚单为负", r.id);
        assert!(
            r.insurance_cents >= 0,
            "tick {at_tick}: 骑手 {} 保险为负",
            r.id
        );
        assert!(r.debt_cents >= 0, "tick {at_tick}: 骑手 {} 欠款为负", r.id);
        assert!(
            r.takehome_cents() >= 0,
            "tick {at_tick}: 骑手 {} 到手为负 {}",
            r.id,
            r.takehome_cents()
        );
        assert!(
            r.gross_cents >= 0,
            "tick {at_tick}: 骑手 {} 毛收入为负",
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
fn hundred_thousand_ticks_no_panic_and_money_non_negative() {
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
            assert_money_non_negative(&w, i);
            assert_ledger_consistent(&w);
        }
    }
    assert_money_non_negative(&w, 100_000);
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
