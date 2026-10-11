//! 临时对比基准：与 `BenchTmp.java` **完全同算法、同数据规模**。
//!
//! 只测纯计算（对应 `sim::ai::plan_for` + `promise_minutes`），
//! 排除 IO 与对象分配干扰。跑完即删。

use std::time::Instant;

const BASE_SPEED_KMH: f32 = 30.0;
const WALK_MPS: f32 = 1.2;
const DETOUR_FACTOR: f32 = 1.3;
const TYPICAL_TO_SHOP_M: f32 = 1200.0;
const EXPECTED_WINDOW_MIN: f32 = 32.5;
const PEAK_PREP_FACTOR: f32 = 2.0;

const TICK_COUNT: u64 = 144_000;
const RIDER_COUNT: usize = 8;
const ORDER_COUNT: usize = 12;

const MODE_LIMIT: [f32; 3] = [30.0, 45.0, 999.0];

#[inline]
fn haversine(lat1: f64, lng1: f64, lat2: f64, lng2: f64) -> f64 {
    const R: f64 = 6_371_000.0;
    let d_lat = (lat2 - lat1).to_radians();
    let d_lng = (lng2 - lng1).to_radians();
    let a = (d_lat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (d_lng / 2.0).sin().powi(2);
    2.0 * R * a.sqrt().asin()
}

struct Data {
    rider_lat: [f64; RIDER_COUNT],
    rider_lng: [f64; RIDER_COUNT],
    order_from_lat: [f64; ORDER_COUNT],
    order_from_lng: [f64; ORDER_COUNT],
    order_dist_m: [u32; ORDER_COUNT],
    order_prep: [u32; ORDER_COUNT],
}

fn build_data() -> Data {
    let mut seed: u64 = 42;
    let mut next = move || {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((seed >> 40) % 1000) as f64 / 1000.0 - 0.5
    };
    let mut d = Data {
        rider_lat: [0.0; RIDER_COUNT],
        rider_lng: [0.0; RIDER_COUNT],
        order_from_lat: [0.0; ORDER_COUNT],
        order_from_lng: [0.0; ORDER_COUNT],
        order_dist_m: [0; ORDER_COUNT],
        order_prep: [0; ORDER_COUNT],
    };
    for i in 0..RIDER_COUNT {
        d.rider_lat[i] = 30.5 + next() * 0.2;
        d.rider_lng[i] = 114.4 + next() * 0.2;
    }
    for i in 0..ORDER_COUNT {
        d.order_from_lat[i] = 30.5 + next() * 0.1;
        d.order_from_lng[i] = 114.4 + next() * 0.1;
        next(); // 顾客 lat，占位保持与 Java 侧一致的消耗序列
        next(); // 顾客 lng
        d.order_dist_m[i] = 500 + ((seed >> 40) % 5000) as u32;
        d.order_prep[i] = 5 + ((seed >> 40) % 10) as u32;
    }
    d
}

#[inline]
fn plan_minutes(
    d: &Data,
    ri: usize,
    oi: usize,
    mode: usize,
    fatigue: f32,
    weather_factor: f32,
    merchant_walk: u32,
    customer_walk: u32,
    avg_prep: u32,
) -> f32 {
    let limit = MODE_LIMIT[mode];
    let ride_mps = (limit * weather_factor * (1.0 - fatigue * 0.25) / 3.6).max(1.0);
    let walk_mps = WALK_MPS.max(0.5);

    let to_shop_m =
        haversine(d.rider_lat[ri], d.rider_lng[ri], d.order_from_lat[oi], d.order_from_lng[oi])
            as f32
            * DETOUR_FACTOR;
    let to_shop_walk_s = merchant_walk as f32 / walk_mps
        + if merchant_walk >= 2 { 90.0 } else { 0.0 };
    let to_shop_min = ((to_shop_m / ride_mps + to_shop_walk_s) / 60.0).ceil().max(1.0);

    let prep_min = (avg_prep as f32 * PEAK_PREP_FACTOR).round().max(1.0) as i32;

    let cust_walk_s = customer_walk as f32 * 2.0 / walk_mps;
    let to_cust_min =
        ((d.order_dist_m[oi] as f32 * DETOUR_FACTOR / ride_mps + cust_walk_s) / 60.0)
            .ceil()
            .max(1.0);

    to_shop_min + prep_min as f32 + to_cust_min
}

#[inline]
fn promise_minutes(ride_mps: f32, merchant_walk: u32, customer_walk: u32, avg_prep: u32, buffer: u32) -> u32 {
    let to_shop_s = TYPICAL_TO_SHOP_M * DETOUR_FACTOR / ride_mps + merchant_walk as f32 / WALK_MPS;
    let prep_min = (avg_prep as f32 * PEAK_PREP_FACTOR).round().max(1.0);
    let to_cust_s = 3000.0 / ride_mps + customer_walk as f32 / WALK_MPS;
    let window_s = EXPECTED_WINDOW_MIN * 60.0;
    let total = (to_shop_s + prep_min * 60.0 + to_cust_s + window_s) / 60.0 + buffer as f32;
    ((total / 5.0).ceil() * 5.0).clamp(15.0, 120.0) as u32
}

fn main() {
    let d = build_data();

    // 热身，与 Java 侧同样排除 JIT 首次编译
    let mut warm = 0.0f32;
    for _ in 0..5000 {
        warm += plan_minutes(&d, 0, 0, 0, 0.2, 1.0, 120, 60, 8);
    }

    let t0 = Instant::now();
    let mut acc = 0.0f32;
    for t in 0..TICK_COUNT {
        for r in 0..RIDER_COUNT {
            let mode = r % 3;
            let fatigue = (t % 100) as f32 / 100.0;
            let wf = if t % 24 < 8 { 0.75 } else { 1.0 };
            for o in 0..ORDER_COUNT {
                acc += plan_minutes(
                    &d,
                    r,
                    o,
                    mode,
                    fatigue,
                    wf,
                    (o % 3) as u32 * 120,
                    (o % 3) as u32 * 90,
                    d.order_prep[o],
                );
            }
        }
        acc += promise_minutes(8.3, 120, 60, 8, 12) as f32;
    }
    let elapsed = t0.elapsed();

    let evaluations = TICK_COUNT * RIDER_COUNT as u64 * ORDER_COUNT as u64;
    let secs = elapsed.as_secs_f64();

    println!("=== Rust 基准（与 Java 侧同算法）===");
    println!("tick 数        : {TICK_COUNT}");
    println!(
        "骑手 × 订单   : {RIDER_COUNT} × {ORDER_COUNT} = {evaluations} 次评估"
    );
    println!("总耗时        : {secs:.3} s");
    println!("吞吐          : {} 评估/秒", (evaluations as f64 / secs) as i64);
    println!("单次评估      : {:.3} µs", elapsed.as_micros() as f64 / evaluations as f64);
    println!("每 tick       : {:.3} µs", elapsed.as_micros() as f64 / TICK_COUNT as f64);
    println!(
        "sink          : {}",
        (warm as i64) + (acc as i64)
    );
}
