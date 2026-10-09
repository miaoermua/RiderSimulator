//! AB 点圆滑轨迹生成（纯函数）。
//!
//! **不要直线插值**——真实骑行要跟路网。高德正常时用真实 polyline，
//! 超配额 / 断网时用 `fallback_trajectory` 降级，但轨迹必须看起来像在走。

use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;

use super::geo::{LatLng, sample_by_distance, straight_line_with_jitter};
use serde::Serialize;

/// 轨迹点。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Waypoint {
    pub lat: f64,
    pub lng: f64,
    /// 从出发算起的毫秒偏移。
    pub at_ms: u32,
}

/// 采样间隔（秒）。250ms 一个点，手机上足够丝滑。
const SAMPLE_DT_S: f32 = 0.25;
/// 起步加速时间（秒）。
const ACCEL_S: f32 = 2.0;
/// 抵达前减速时间（秒）。
const DECEL_S: f32 = 3.0;

/// Catmull-Rom 插值（tension = 0.5）。
pub fn catmull_rom(points: &[LatLng], tension: f32) -> Vec<LatLng> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut out: Vec<LatLng> = Vec::with_capacity(points.len() * 4);
    for i in 0..points.len() - 1 {
        let p0 = points[i.saturating_sub(1)];
        let p1 = points[i];
        let p2 = points[i + 1];
        let p3 = points[(i + 2).min(points.len() - 1)];
        // 每段插 4 个点
        for step in 0..4 {
            let t = step as f64 / 4.0;
            out.push(LatLng::new(
                catmull_rom_scalar(p0.lat, p1.lat, p2.lat, p3.lat, t, tension as f64),
                catmull_rom_scalar(p0.lng, p1.lng, p2.lng, p3.lng, t, tension as f64),
            ));
        }
    }
    out.push(*points.last().unwrap());
    out
}

fn catmull_rom_scalar(p0: f64, p1: f64, p2: f64, p3: f64, t: f64, tension: f64) -> f64 {
    let t2 = t * t;
    let t3 = t2 * t;
    let m1 = (p2 - p0) * tension;
    let m2 = (p3 - p1) * tension;
    (2.0 * t3 - 3.0 * t2 + 1.0) * p1
        + (t3 - 2.0 * t2 + t) * m1
        + (-2.0 * t3 + 3.0 * t2) * p2
        + (t3 - t2) * m2
}

/// 速度曲线：起步 2 秒加速到 80%，匀速，抵达前 3 秒减速到 0。
///
/// 返回相对目标速度的系数。
pub fn speed_factor(u: f32, total_time_s: f32) -> f32 {
    let t = u * total_time_s;
    let ramp_up = if ACCEL_S <= f32::EPSILON {
        1.0
    } else {
        (t / ACCEL_S).clamp(0.0, 1.0).powf(0.7) * 0.8 + 0.2 * (t / ACCEL_S).clamp(0.0, 1.0)
    };
    let remaining = total_time_s - t;
    let ramp_down = if DECEL_S <= f32::EPSILON {
        1.0
    } else {
        (remaining / DECEL_S).clamp(0.0, 1.0).powf(0.6)
    };
    (ramp_up.min(ramp_down)).clamp(0.05, 1.0)
}

/// 生成轨迹。`polyline` 是高德返回的路网（或降级直线）。
pub fn build_trajectory(polyline: &[LatLng], speed_mps: f32, seed: u64) -> Vec<Waypoint> {
    if polyline.len() < 2 {
        return Vec::new();
    }
    let speed = speed_mps.max(0.5);
    let total_len = super::geo::polyline_len_m(polyline);
    if total_len < 1.0 {
        return vec![Waypoint {
            lat: polyline[0].lat,
            lng: polyline[0].lng,
            at_ms: 0,
        }];
    }
    let total_time_s = (total_len / speed as f64) as f32;

    // 1. 等弧长采样
    let n = ((total_time_s / SAMPLE_DT_S).ceil() as usize).clamp(2, 2000);
    let samples = sample_by_distance(polyline, n);

    // 2. Catmull-Rom 让曲线圆滑，再按弧长重采样，点数不变
    let smooth = catmull_rom(&samples, 0.5);
    let pts = sample_by_distance(&smooth, n);

    // 3. 红灯停顿：平均每 2 分钟一个路口，停 5~20 秒
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let stop_count = (total_time_s / 120.0).floor() as usize;
    let mut stops: Vec<(f32, f32)> = (0..stop_count)
        .map(|_| {
            let at = rng.random_range(0.05f32..0.95);
            let pause = rng.random_range(5.0f32..20.0);
            (at, pause)
        })
        .collect();
    stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    // 4. 按速度曲线累加时间
    let seg_len = total_len / (n - 1) as f64;
    let mut acc_s = 0.0f32;
    let mut out = Vec::with_capacity(n);
    for (i, p) in pts.iter().enumerate() {
        if i > 0 {
            let u = i as f32 / (n - 1) as f32;
            let v = speed * speed_factor(u, total_time_s).max(0.05);
            acc_s += seg_len as f32 / v;
            for (at, pause) in &stops {
                if u >= *at && (i == 1 || (i - 1) as f32 / ((n - 1) as f32) < *at) {
                    acc_s += pause;
                }
            }
        }
        out.push(Waypoint {
            lat: p.lat,
            lng: p.lng,
            at_ms: (acc_s * 1000.0).round() as u32,
        });
    }
    out
}

/// 降级轨迹：直线 + 随机偏移（高德挂了也不能让游戏卡住）。
pub fn fallback_trajectory(from: LatLng, to: LatLng, speed_mps: f32, seed: u64) -> Vec<Waypoint> {
    let line = straight_line_with_jitter(from, to, seed, 48);
    build_trajectory(&line, speed_mps, seed ^ 0x9E37_79B9)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line() -> Vec<LatLng> {
        vec![
            LatLng::new(30.5077, 114.4154),
            LatLng::new(30.5100, 114.4200),
            LatLng::new(30.5140, 114.4280),
        ]
    }

    #[test]
    fn waypoints_are_monotonic_in_time() {
        let wp = build_trajectory(&line(), 4.0, 7);
        assert!(wp.len() > 5);
        for w in wp.windows(2) {
            assert!(w[1].at_ms >= w[0].at_ms, "time must not go backwards");
        }
        assert_eq!(wp[0].at_ms, 0);
    }

    #[test]
    fn reproducible_for_same_seed() {
        assert_eq!(
            build_trajectory(&line(), 4.0, 99),
            build_trajectory(&line(), 4.0, 99)
        );
        assert_ne!(
            build_trajectory(&line(), 4.0, 99),
            build_trajectory(&line(), 4.0, 100)
        );
    }

    #[test]
    fn slow_ride_takes_longer() {
        let fast = build_trajectory(&line(), 8.0, 1).last().unwrap().at_ms;
        let slow = build_trajectory(&line(), 2.0, 1).last().unwrap().at_ms;
        assert!(slow > fast * 2, "slow={slow} fast={fast}");
        // 直线距离约 1.5km，8m/s 应在 3~6 分钟内
        assert!(fast < 400_000, "fast={fast}");
    }

    #[test]
    fn speed_profile_starts_and_ends_slow() {
        assert!(speed_factor(0.0, 300.0) < 0.3);
        assert!(speed_factor(1.0, 300.0) < 0.3);
        assert!(speed_factor(0.5, 300.0) > 0.9);
    }

    #[test]
    fn fallback_trace_looks_alive() {
        let wp = fallback_trajectory(LatLng::new(30.5, 114.4), LatLng::new(30.52, 114.43), 4.0, 3);
        assert!(wp.len() > 5);
        // 不共线：存在偏离首尾连线的点
        let (a, b) = (wp[0], *wp.last().unwrap());
        let off = wp
            .iter()
            .map(|w| {
                let t = ((w.lng - a.lng) * (b.lng - a.lng) + (w.lat - a.lat) * (b.lat - a.lat))
                    / ((b.lng - a.lng).powi(2) + (b.lat - a.lat).powi(2));
                let px = a.lng + t * (b.lng - a.lng);
                let py = a.lat + t * (b.lat - a.lat);
                ((w.lng - px).powi(2) + (w.lat - py).powi(2)).sqrt()
            })
            .fold(0.0f64, f64::max);
        assert!(off > 0.0, "降级轨迹不能是完美直线");
    }
}
