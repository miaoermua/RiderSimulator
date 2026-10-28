//! 风险系统：事故、超速被拍、疲劳。
//!
//! 设计意图（玩法设计 §3.2）：**档位 2 的期望值是负的**。
//! 算下来亏 8.9 元/单，但玩家在赌的时候不知道这个数字。
//!
//! **实掷与展示同源**：事故率一律取 [`SpeedRules::accident_rate`] 的档位基准值，
//! 期望值面板（[`Rider::expected_value_cents`]）读的是同一个数。从前这边自己
//! 硬编码 0.005 再加超速惩罚，解除限速档因为标称限速 999 把惩罚算爆、
//! 被夹到 95%，面板却还写着 22%——「出事率太高」的根因就在这个脱节。

use rand::{Rng, RngExt};

use super::{
    config::SimConfig,
    rider::{Rider, SpeedMode},
    weather::Weather,
};

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

/// 事故概率 = 档位基准率 × 天气加成 × 全局倍率 + 疲劳惩罚。
///
/// 最后被 [`AccidentRules::max_probability`] 夹一刀：暴雨（×6）叠满疲劳时，
/// 解除限速档算出来会超过 100%，不夹就是「必定进医院」。
///
/// [`AccidentRules::max_probability`]: super::config::AccidentRules::max_probability
pub fn accident_probability(
    cfg: &SimConfig,
    mode: SpeedMode,
    weather: Weather,
    fatigue: f32,
) -> f32 {
    if !cfg.accident.enabled {
        return 0.0;
    }
    let base = cfg.speed.accident_rate[mode.index()] * weather.accident_multiplier();
    let fatigue_penalty = fatigue.clamp(0.0, 1.0) * cfg.fatigue.accident_weight;
    let p = (base + fatigue_penalty) * cfg.accident.rate_multiplier.max(0.0);
    p.clamp(0.0, cfg.accident.max_probability.clamp(0.0, 1.0))
}

/// 掷一次事故。
pub fn roll_accident(
    rng: &mut impl Rng,
    cfg: &SimConfig,
    mode: SpeedMode,
    weather: Weather,
    fatigue: f32,
) -> Option<AccidentOutcome> {
    let p = accident_probability(cfg, mode, weather, fatigue);
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
        // 用生效档位：LLM 把档位改到解限速后，期望值必须跟着变，
        // 否则控制端会拿旧档位的概率去解释新行为。
        let p = self.effective_speed_mode(cfg).accident_rate(cfg);
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

    /// 实掷率必须**逐位等于**档位基准率——面板上写 22%，掷出来就得是 22%。
    #[test]
    fn accident_rate_matches_the_panel_and_grows_with_tier() {
        let cfg = SimConfig::default();
        let calm = accident_probability(&cfg, SpeedMode::Compliant, Weather::Clear, 0.0);
        let tolerant = accident_probability(&cfg, SpeedMode::Tolerant, Weather::Clear, 0.0);
        let fast = accident_probability(&cfg, SpeedMode::Unrestricted, Weather::Clear, 0.0);
        let tired = accident_probability(&cfg, SpeedMode::Compliant, Weather::Clear, 1.0);
        assert_eq!(calm, 0.005);
        assert_eq!(tolerant, 0.04);
        assert_eq!(fast, 0.22, "解除限速档不该再被标称限速 999 撑爆");
        assert!(tired > calm);
    }

    /// 总开关、全局倍率、概率上限三个旋钮都得真的管用。
    #[test]
    fn weather_switch_and_multiplier_all_take_effect() {
        let cfg = SimConfig::default();
        let clear = accident_probability(&cfg, SpeedMode::Compliant, Weather::Clear, 0.0);
        let storm = accident_probability(&cfg, SpeedMode::Compliant, Weather::Rainstorm, 0.0);
        assert!((storm / clear - 6.0).abs() < 1e-4);

        // 解除限速 + 暴雨 + 全疲劳：算出来 134%，必须被上限夹住
        let worst = accident_probability(&cfg, SpeedMode::Unrestricted, Weather::Rainstorm, 1.0);
        assert_eq!(worst, cfg.accident.max_probability);

        let mut off = cfg.clone();
        off.accident.enabled = false;
        assert_eq!(
            accident_probability(&off, SpeedMode::Unrestricted, Weather::Rainstorm, 1.0),
            0.0
        );

        let mut half = cfg.clone();
        half.accident.rate_multiplier = 0.5;
        let p = accident_probability(&half, SpeedMode::Tolerant, Weather::Clear, 0.0);
        assert!((p - 0.02).abs() < 1e-6, "p={p}");
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

    /// 期望值必须读**生效档位**：LLM 把档位抬到解限速后，
    /// 控制端看到的期望值要同步变成负的，而不是继续按个人基线算。
    #[test]
    fn expected_value_follows_the_effective_speed_mode() {
        let cfg = SimConfig {
            allow_speeding: true,
            allow_unrestricted: true,
            ..Default::default()
        };
        let mut r = Rider::new(
            1,
            RiderKind::Simulated,
            "AI",
            LatLng::new(30.5, 114.4),
            &cfg,
        );
        r.speed_mode = SpeedMode::Compliant;
        let calm = r.expected_value_cents(&cfg);

        r.policy_override.mode_override = Some(SpeedMode::Unrestricted);
        let fast = r.expected_value_cents(&cfg);
        assert!(fast < calm, "calm={calm} fast={fast}");

        r.policy_override = Default::default();
        assert_eq!(r.expected_value_cents(&cfg), calm);
    }

    #[test]
    fn accident_roll_is_bounded() {
        let cfg = SimConfig::default();
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let mut hits = 0;
        for _ in 0..10_000 {
            if let Some(a) = roll_accident(
                &mut rng,
                &cfg,
                SpeedMode::Compliant,
                Weather::LightRain,
                0.3,
            ) {
                hits += 1;
                assert!(a.off_duty_min >= 60);
                assert!(a.medical_cents >= cfg.fine.accident_medical_min_cents);
                assert!(a.medical_cents <= cfg.fine.accident_medical_max_cents + 1);
                assert!((1..=3).contains(&a.severity));
            }
        }
        // 合规 0.5% * 小雨 1.5 = 0.75% + 疲劳 0.6% → 大约 1.35%
        assert!((60..220).contains(&hits), "hits={hits}");
    }
}
