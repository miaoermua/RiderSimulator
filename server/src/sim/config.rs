//! 数值速查表（玩法设计 §12）。**改平衡只改这里。**
//!
//! 所有金额单位一律「分」（i64）。时间单位一律「模拟分钟」，除非字段名带 `_secs`。

use serde::{Deserialize, Serialize};

use super::geo::CoordSystem;

/// 实时档下一个 tick 的真实时长：1 模拟分钟 = 真实 60 秒。
///
/// 这是「真实时间」与「1 分钟 1 秒」两个模式的分界：倍率档位压缩真实时间，
/// 实时档让模拟时钟跟真实钟表同步走。
pub const REALTIME_TICK_MS: u64 = 60_000;

/// 配送费规则。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeRules {
    pub base_fee_cents: i64,
    /// 距离分段**下界**（米）与对应的总附加费（分）。
    ///
    /// 语义：取「不超过本单距离的最大门槛」的 `cents` 作为附加费。
    /// 对应玩法设计 §1.1：`(1500,100) (3000,200) (5000,350) (8000,350)`，
    /// 即 1.5km 以下 0 元、1.5~3km 1.00 元、3~5km 2.00 元、5~8km 3.50 元，
    /// 超过 8000m 后再按 `per_km_over_cents` 叠加。
    pub distance_tiers: Vec<(u32, i64)>,
    /// 超过最后一档后每公里附加（分/km）。
    pub per_km_over_cents: i64,
    /// 通勤高峰系数：(起始分钟, 结束分钟, 系数)
    pub surge_windows: Vec<(u32, u32, f32)>,
    /// 等级加成：每级 20 分。
    pub level_bonus_per_level_cents: i64,
}

/// 罚单规则。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FineRules {
    pub wait_timeout_cents: i64,
    pub deliver_timeout_cents: i64,
    pub camera_cents: i64,
    pub complaint_cents: i64,
    pub cancel_cents: i64,
    pub backorder_collapse_cents: i64,
    pub insurance_per_day_cents: i64,
    pub accident_medical_min_cents: i64,
    pub accident_medical_max_cents: i64,
    /// 被拍冻结账号（分钟）——比罚款更痛。
    pub camera_detention_min: u32,
}

/// 背单规则。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackOrderRules {
    pub max_held: u32,
    pub detour_threshold_m: u32,
    pub bonus_min_cents: i64,
    pub bonus_max_cents: i64,
    /// 承诺时限后的缓冲（分钟）。
    pub late_buffer_min: u32,
}

/// 速度档位规则（三档）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeedRules {
    pub complaint_speed_kmh: [f32; 3],
    /// 时薪系数：**只用于期望值展示与 AI 策略，不直接乘到账单上**。
    /// 真实的时薪差异由行程时间自然产生。
    pub hourly_coefficient: [f32; 3],
    pub accident_rate: [f32; 3],
    pub camera_rate: [f32; 3],
    pub rating_delta: [f32; 3],
    pub detain_min: u32,
    /// 合规档的基准速度 km/h（用于行程时间）。
    pub base_speed_kmh: f32,
}

/// 评分规则。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RatingRules {
    pub initial: f32,
    pub min: f32,
    pub max: f32,
    pub on_time: f32,
    pub late: f32,
    pub accident: f32,
    pub good_review: f32,
    pub bad_review_max: f32,
    pub tier_normal: f32,
    pub tier_premium: f32,
    pub tier_express: f32,
}

/// 等级规则。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LevelRules {
    /// 各等级起始单量：0/50/150/400/1000/2500
    pub thresholds: Vec<u32>,
    pub coefficients: Vec<f32>,
}

/// 疲劳规则。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct FatigueRules {
    pub per_online_hour: f32,
    pub per_order: f32,
    pub per_rest_10min: f32,
    pub max: f32,
    /// 疲劳对速度上限的最大削减比例。
    pub speed_penalty_max: f32,
    /// 疲劳对事故率的加权。
    pub accident_weight: f32,
}

/// 单局可调参数（控制端能改的都在这）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimConfig {
    pub seed: u64,
    pub fee: FeeRules,
    pub fine: FineRules,
    pub backorder: BackOrderRules,
    pub speed: SpeedRules,
    pub rating_rules: RatingRules,
    pub level: LevelRules,
    pub fatigue: FatigueRules,

    // --- 控制端开关 ---
    /// 平台抽水比例 0.0 ~ 0.30，默认 0。
    pub commission_rate: f32,
    /// 只抽配送费，不抽雨补。
    pub commission_on_rain_bonus: bool,
    /// 歧视性抽水：评分高 / 单量大的骑手抽水上浮。
    pub discriminative_commission: bool,
    /// 抢单窗口秒数 1~10。
    pub claim_window_secs: u32,
    pub allow_speeding: bool,
    pub allow_unrestricted: bool,
    pub backorder_enabled: bool,
    pub max_orders_per_day: u32,
    pub ai_batch_enabled: bool,
    pub llm_brain_enabled: bool,
    /// 控制端「显示期望值」开关，只影响下发字段。
    pub show_expected_value: bool,
    /// 模拟骑手数量 0~8。
    pub sim_rider_count: u32,
    /// 每多少模拟分钟尝试生成一批新单。
    pub order_spawn_interval_min: u32,
    /// 时间倍率：0=暂停，1/10/60。
    pub speed_multiplier: u32,
    /// 实时档：1 模拟分钟 = 真实 60 秒，世界按真实钟表速度推进。
    ///
    /// 打开时 [`SimConfig::tick_interval_ms`] 返回 [`REALTIME_TICK_MS`]，
    /// `speed_multiplier` 只保留「是否暂停」的语义（0 = 暂停）。
    pub realtime: bool,
    /// 下发坐标的坐标系。内部世界状态永远是 GCJ-02，这里只决定出口转换。
    #[serde(default)]
    pub coord_system: CoordSystem,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            seed: 20261006,
            fee: FeeRules {
                base_fee_cents: 350,
                distance_tiers: vec![(1500, 100), (3000, 200), (5000, 350), (8000, 350)],
                per_km_over_cents: 60,
                surge_windows: vec![
                    (7 * 60, 9 * 60, 1.10),
                    (11 * 60, 13 * 60, 1.15),
                    (17 * 60, 20 * 60, 1.25),
                ],
                level_bonus_per_level_cents: 20,
            },
            fine: FineRules {
                wait_timeout_cents: 200,
                deliver_timeout_cents: 300,
                camera_cents: 500,
                complaint_cents: 200,
                cancel_cents: 500,
                backorder_collapse_cents: 200,
                insurance_per_day_cents: 200,
                accident_medical_min_cents: 2000,
                accident_medical_max_cents: 20000,
                camera_detention_min: 10,
            },
            backorder: BackOrderRules {
                max_held: 3,
                detour_threshold_m: 800,
                bonus_min_cents: 50,
                bonus_max_cents: 120,
                late_buffer_min: 5,
            },
            speed: SpeedRules {
                complaint_speed_kmh: [30.0, 45.0, 999.0],
                hourly_coefficient: [1.00, 1.35, 2.20],
                accident_rate: [0.005, 0.04, 0.22],
                camera_rate: [0.0, 0.008, 0.04],
                rating_delta: [0.0, -0.01, -0.05],
                detain_min: 10,
                base_speed_kmh: 30.0,
            },
            rating_rules: RatingRules {
                initial: 4.8,
                min: 4.0,
                max: 5.0,
                on_time: 0.01,
                late: -0.02,
                accident: -0.05,
                good_review: 0.02,
                bad_review_max: -0.05,
                tier_normal: 4.6,
                tier_premium: 4.8,
                tier_express: 4.9,
            },
            level: LevelRules {
                thresholds: vec![0, 50, 150, 400, 1000, 2500],
                coefficients: vec![1.00, 1.04, 1.08, 1.12, 1.16, 1.20],
            },
            fatigue: FatigueRules {
                per_online_hour: 0.08,
                per_order: 0.01,
                per_rest_10min: 0.15,
                max: 1.0,
                speed_penalty_max: 0.25,
                accident_weight: 0.02,
            },
            commission_rate: 0.0,
            commission_on_rain_bonus: false,
            discriminative_commission: false,
            claim_window_secs: 3,
            allow_speeding: true,
            allow_unrestricted: false,
            backorder_enabled: true,
            max_orders_per_day: 0,
            ai_batch_enabled: true,
            // 玩法设计 §7.3：LLM 战术大脑**默认关**，由控制端 / 命令行显式打开。
            llm_brain_enabled: false,
            show_expected_value: false,
            sim_rider_count: 3,
            order_spawn_interval_min: 3,
            speed_multiplier: 1,
            realtime: false,
            coord_system: CoordSystem::Gcj02,
        }
    }
}

impl SimConfig {
    /// 该时段的高峰系数。
    pub fn surge_multiplier(&self, sim_minute: u32) -> f32 {
        for (from, to, k) in &self.fee.surge_windows {
            if sim_minute >= *from && sim_minute < *to {
                return *k;
            }
        }
        1.0
    }

    /// 是否处于高峰（出餐变慢、车流量变大）。
    pub fn is_peak(&self, sim_minute: u32) -> bool {
        self.surge_multiplier(sim_minute) > 1.0
    }

    /// 抢单窗口换算成 tick 数。1 tick = 1 模拟分钟，
    /// 倍率越高，同样的现实秒数对应的 tick 越多。
    ///
    /// 实时档下一个 tick 就是 60 真实秒，比任何窗口都长，
    /// 于是窗口退化成「1 tick」，抢单变成一颗 tick 内定胜负。
    pub fn claim_window_ticks(&self) -> u32 {
        if self.realtime {
            return 1;
        }
        let m = self.speed_multiplier.max(1);
        (self.claim_window_secs * m).max(1)
    }

    /// 现实上一个 tick 的毫秒数。
    ///
    /// - 暂停（倍率 0）：退回 1 秒节拍，`App::on_tick` 会直接跳过推进；
    /// - 实时档：1 模拟分钟 = 真实 60 秒；
    /// - 其余：倍率 1 时 = 1 秒 = 1 模拟分钟。
    pub fn tick_interval_ms(&self) -> u64 {
        if self.speed_multiplier == 0 {
            return 1000;
        }
        if self.realtime {
            return REALTIME_TICK_MS;
        }
        1000 / self.speed_multiplier as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::weather::Weather;

    #[test]
    fn surge_windows() {
        let c = SimConfig::default();
        assert_eq!(c.surge_multiplier(8 * 60), 1.10);
        assert_eq!(c.surge_multiplier(12 * 60), 1.15);
        assert_eq!(c.surge_multiplier(18 * 60), 1.25);
        assert_eq!(c.surge_multiplier(3 * 60), 1.00);
    }

    #[test]
    fn default_balance_matches_design() {
        let c = SimConfig::default();
        assert_eq!(c.fee.base_fee_cents, 350);
        assert_eq!(c.commission_rate, 0.0);
        assert_eq!(c.backorder.max_held, 3);
        assert_eq!(c.claim_window_secs, 3);
        assert_eq!(c.fine.insurance_per_day_cents, 200);
        assert_eq!(Weather::HeavyRain.rain_bonus_cents(), 250);
    }

    #[test]
    fn tick_interval_scales() {
        let mut c = SimConfig::default();
        assert_eq!(c.tick_interval_ms(), 1000);
        assert_eq!(c.claim_window_ticks(), 3);
        c.speed_multiplier = 60;
        assert_eq!(c.tick_interval_ms(), 16);
        assert_eq!(c.claim_window_ticks(), 180);
        // 暂停时 tick 间隔退回 1000ms（等价 1x），窗口仍按 3 秒算。
        c.speed_multiplier = 0;
        assert_eq!(c.tick_interval_ms(), 1000);
        assert_eq!(c.claim_window_ticks(), 3);
    }

    #[test]
    fn realtime_mode_runs_one_sim_minute_per_real_minute() {
        let mut c = SimConfig::default();
        assert!(!c.realtime, "默认真实时间是关的");

        c.realtime = true;
        assert_eq!(c.tick_interval_ms(), REALTIME_TICK_MS);
        assert_eq!(c.tick_interval_ms(), 60_000);

        // 窗口长不过一个 tick，实时档退化成 1。
        assert_eq!(c.claim_window_ticks(), 1);
        c.claim_window_secs = 10;
        assert_eq!(c.claim_window_ticks(), 1);

        // 倍率不再影响节拍。
        c.speed_multiplier = 60;
        assert_eq!(c.tick_interval_ms(), REALTIME_TICK_MS);

        // 暂停优先于实时：世界冻住，节拍退回 1 秒等恢复。
        c.speed_multiplier = 0;
        assert_eq!(c.tick_interval_ms(), 1000);
    }
}
