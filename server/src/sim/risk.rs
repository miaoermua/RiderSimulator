//! 风险系统：事故、超速被拍、疲劳。
//!
//! 设计意图（玩法设计 §3.2）：**档位 2 的期望值是负的**。
//! 算下来亏 8.9 元/单，但玩家在赌的时候不知道这个数字。

use rand::{Rng, RngExt};

use super::{
    config::SimConfig,
    rider::{Rider, SpeedMode},
    weather::Weather,
};

/// 基准速度（合规档 30km/h），用于算超速比。
pub const BASE_SPEED_KMH: f32 = 30.0;

#[derive(Debug, Clone, Copy)]
pub struct AccidentOutcome {
    /// 停工分钟。
    pub off_duty_min: u32,
    /// 医药费（分）。
    pub medical_cents: i64,
    pub rating_delta: f32,
    /// 1~3，越小越轻。
    pub severity: u8,
}

/// 事故概率 = 基准 × 天气加成 + 超速惩罚 + 疲劳惩罚。
pub fn accident_probability(
    cfg: &SimConfig,
    speed_ratio: f32,
    weather: Weather,
    fatigue: f32,
) -> f32 {
    let base = 0.005 * weather.accident_multiplier();
    let speed_penalty = (speed_ratio - 1.0).max(0.0) * 0.08;
    let fatigue_penalty = fatigue.clamp(0.0, 1.0) * cfg.fatigue.accident_weight;
    (base + speed_penalty + fatigue_penalty).clamp(0.0, 0.95)
}

/// 掷一次事故。
pub fn roll_accident(
    rng: &mut impl Rng,
    cfg: &SimConfig,
    speed_ratio: f32,
    weather: Weather,
    fatigue: f32,
) -> Option<AccidentOutcome> {
    let p = accident_probability(cfg, speed_ratio, weather, fatigue);
    if !rng.random_bool(p as f64) {
        return None;
    }
    let severity = match rng.random_range(0..100u32) {
        0..=54 => 1u8,
        55..=84 => 2,
        _ => 3,
    };
    let (off_duty_min, lo, hi) = match severity {
        1 => (
            60,
            cfg.fine.accident_medical_min_cents,
            cfg.fine.accident_medical_min_cents + 3000,
        ),
        2 => (
            90,
            cfg.fine.accident_medical_min_cents + 3000,
            cfg.fine.accident_medical_min_cents + 8000,
        ),
        _ => (
            180,
            cfg.fine.accident_medical_min_cents + 8000,
            cfg.fine.accident_medical_max_cents,
        ),
    };
    Some(AccidentOutcome {
        off_duty_min: off_duty_min + rng.random_range(0..30),
        medical_cents: rng.random_range(lo..=hi.max(lo + 1)),
        rating_delta: cfg.rating_rules.accident,
        severity,
    })
}

/// 超速被拍概率 = 档位系数 × 路段车流量 × 时段系数。
pub fn camera_fine_probability(mode: SpeedMode, traffic_factor: f32, is_peak: bool) -> f32 {
    let base = match mode {
        SpeedMode::Compliant => 0.0,
        SpeedMode::Tolerant => 0.008,
        SpeedMode::Unrestricted => 0.04,
    };
    let peak = if is_peak { 1.4 } else { 1.0 };
    (base * traffic_factor.clamp(0.3, 3.0) * peak).clamp(0.0, 0.9)
}

pub fn roll_camera_fine(
    rng: &mut impl Rng,
    mode: SpeedMode,
    traffic_factor: f32,
    is_peak: bool,
) -> bool {
    let p = camera_fine_probability(mode, traffic_factor, is_peak);
    p > 0.0 && rng.random_bool(p as f64)
}

/// 在线疲劳增长。
pub fn fatigue_online_gain(cfg: &SimConfig, minutes: u32) -> f32 {
    cfg.fatigue.per_online_hour * minutes as f32 / 60.0
}

/// 休息恢复（10 分钟一档，线性折算）。
pub fn fatigue_rest_recovery(cfg: &SimConfig, minutes: u32) -> f32 {
    cfg.fatigue.per_rest_10min * minutes as f32 / 10.0
}

/// 是否触发加速带来的评分惩罚（档位 1/2 每次被拍）。
pub fn camera_rating_delta(cfg: &SimConfig, mode: SpeedMode) -> f32 {
    mode.rating_delta(cfg)
}

/// 骑手的当前超速比。
pub fn speed_ratio(rider: &Rider, cfg: &SimConfig, weather: Weather) -> f32 {
    rider.speed_kmh(cfg, weather) / BASE_SPEED_KMH
}

impl Rider {
    /// 期望值（分）。控制端「显示期望值」开关打开时下发。
    ///
    /// 玩法设计 §3.2：`单次收益 − 事故率 × 单次事故成本`。
    /// 单次事故成本 = 医药费均值 40 元（4000 分）+ 停工 2 小时的机会成本 30 元（3000 分）。
    ///
    /// 这里刻意用常数而不是 `accident_medical_max_cents`：期望值要和设计文档
    /// 「档位 2 ≈ −8.9 元」这个教学数字对得上，不能拿最坏情况（200 元）去吓人。
    pub fn expected_value_cents(&self, cfg: &SimConfig) -> i64 {
        const AVG_FARE_CENTS: i64 = 650; // 约 6.5 元/单
        const AVG_MEDICAL_CENTS: i64 = 4000; // 医药费均值 40 元
        const DOWNTIME_LOSS_CENTS: i64 = 3000; // 停工 2 小时 ≈ 30 元
        let accident_cost = AVG_MEDICAL_CENTS + DOWNTIME_LOSS_CENTS;
        let p = self.speed_mode.accident_rate(cfg);
        AVG_FARE_CENTS - (p * accident_cost as f32).round() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{
        geo::LatLng,
        rider::{Rider, RiderKind},
    };
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    #[test]
    fn accident_probability_grows_with_speed_and_fatigue() {
        let cfg = SimConfig::default();
        let slow = accident_probability(&cfg, 1.0, Weather::Clear, 0.0);
        let fast = accident_probability(&cfg, 999.0 / 30.0, Weather::Clear, 0.0);
        let tired = accident_probability(&cfg, 1.0, Weather::Clear, 1.0);
        assert!(fast > slow * 20.0);
        assert!(tired > slow);
        assert_eq!(slow, 0.005);
    }

    #[test]
    fn weather_multiplies_accidents() {
        let cfg = SimConfig::default();
        let clear = accident_probability(&cfg, 1.0, Weather::Clear, 0.0);
        let storm = accident_probability(&cfg, 1.0, Weather::Rainstorm, 0.0);
        assert!((storm / clear - 6.0).abs() < 1e-4);
    }

    #[test]
    fn camera_fines_only_above_compliant() {
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        for _ in 0..1000 {
            assert!(!roll_camera_fine(&mut rng, SpeedMode::Compliant, 1.0, true));
        }
        assert_eq!(
            camera_fine_probability(SpeedMode::Compliant, 1.0, false),
            0.0
        );
    }

    #[test]
    fn unrestricted_mode_is_negative_ev() {
        let cfg = SimConfig::default();
        let mut r = Rider::new(
            1,
            RiderKind::Simulated,
            "AI",
            LatLng::new(30.5, 114.4),
            &cfg,
        );
        r.speed_mode = SpeedMode::Unrestricted;
        // 设计文档：约 -8.9 元
        let ev = r.expected_value_cents(&cfg);
        assert!((-950..=-800).contains(&ev), "ev={ev}");
        r.speed_mode = SpeedMode::Compliant;
        assert!(r.expected_value_cents(&cfg) > 600);
    }

    #[test]
    fn accident_roll_is_bounded() {
        let cfg = SimConfig::default();
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let mut hits = 0;
        for _ in 0..10_000 {
            if let Some(a) = roll_accident(&mut rng, &cfg, 1.0, Weather::LightRain, 0.3) {
                hits += 1;
                assert!(a.off_duty_min >= 60);
                assert!(a.medical_cents >= cfg.fine.accident_medical_min_cents);
                assert!(a.medical_cents <= cfg.fine.accident_medical_max_cents + 1);
                assert!((1..=3).contains(&a.severity));
            }
        }
        // 小雨 0.5% * 1.5 = 0.75% + 疲劳 → 大约 1.35%
        assert!((60..220).contains(&hits), "hits={hits}");
    }
}
