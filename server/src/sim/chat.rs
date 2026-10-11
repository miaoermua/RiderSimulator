//! 末端交付沟通（订单级会话）：**要不要开口、窗口多长**。
//!
//! ## 为什么触发判定不能交给 LLM
//!
//! 真实配送里 95% 的单是「放门口 → 点送达」，全程零交流，顾客开口是少数事件。
//! 这个「少数」必须**可复现**：同一份存档、同一个种子，第 3 单会不会吵起来
//! 每次都该一样，否则 10 万 tick 的回放测试就废了。所以开口与否由
//! [`World::rng`](super::world::World::rng)（ChaCha8，绑世界种子）掷出，
//! 按订单画像加权；LLM 只在**已经决定开口之后**负责「说什么、要不要索赔」。
//!
//! ## 三层分工
//!
//! ```text
//! sim::chat            ← 要不要开口、窗口多长（纯计算，可复现、零 token）
//! llm::customer        ← 顾客说什么、索赔多少档（LLM，失败即降级到预设话术）
//! app::on_chat_ruling  ← 钱从谁身上出（代码裁决，不可绕过）
//! ```
//!
//! 用户拍板的是「本地算法 roll」：概率只来自这里的画像加权，与 LLM 无关。

use rand::RngExt;
use rand_chacha::ChaCha8Rng;

/// 末端放置窗口的下限（模拟分钟）。1x 时 = 5 真实秒。
pub const MIN_WINDOW_MIN: u32 = 5;
/// 末端放置窗口的上限（模拟分钟）。1x 时 = 60 真实秒。
pub const MAX_WINDOW_MIN: u32 = 60;

/// 静默基础概率：**95% 的单顾客不开口**（玩法设定）。
pub const BASE_OPEN_RATE: f64 = 0.05;
/// 加权之后的上限。再高就不像「偶发事件」了。
pub const MAX_OPEN_RATE: f64 = 0.40;

/// 汤汤水水：洒出来就是投诉。
const SPILL_HINTS: [&str; 10] = [
    "汤", "粥", "面", "粉", "饮", "奶茶", "咖啡", "锅", "煲", "汁",
];
/// 一颠就散。
const FRAGILE_HINTS: [&str; 6] = ["蛋糕", "寿司", "刺身", "甜品", "冰", "沙拉"];
/// 备注里提前打过招呼的，顾客更会盯着看。
const CARE_HINTS: [&str; 5] = ["洒", "小心", "别倒", "轻放", "别颠"];

/// 一次末端会话的画像。**全部字段都来自订单 / 骑手本身**，不引入新数据源——
/// 商户数据里没有品类字段，但 `signature`（招牌菜名）足够判「汤类 / 易碎」。
#[derive(Debug, Clone, Default)]
pub struct ChatContext {
    /// 商户招牌菜名。
    pub signature: String,
    /// 顾客备注。
    pub remark: String,
    /// 骑手当前评分。分越差，顾客越容易先开口挑刺。
    pub rider_rating: f32,
    /// 当日分钟（0..1439），判夜间。
    pub sim_minute: u32,
    /// 是否已超时。超时的单，顾客开口概率显著上升。
    pub late: bool,
}

impl ChatContext {
    /// 拼给 LLM 的一行画像。**降级路径也用它**，保证两条路上看到的世界一致。
    pub fn brief(&self) -> String {
        let mut parts = Vec::new();
        if !self.signature.is_empty() {
            parts.push(format!("餐品：{}", self.signature));
        }
        if !self.remark.is_empty() {
            parts.push(format!("备注：{}", self.remark));
        }
        parts.push(format!(
            "时段：{}",
            if is_night(self.sim_minute) {
                "夜间"
            } else {
                "白天"
            }
        ));
        parts.push(format!("骑手评分：{:.2}", self.rider_rating));
        if self.late {
            parts.push("状态：已超时".to_string());
        }
        parts.join("；")
    }
}

/// 是否夜间（21:00–07:00）。
pub fn is_night(sim_minute: u32) -> bool {
    !(7 * 60..21 * 60).contains(&sim_minute)
}

fn hits_any(text: &str, hints: &[&str]) -> bool {
    hints.iter().any(|h| text.contains(h))
}

/// 顾客开口的概率。**纯函数**，方便单测锁定数值。
pub fn open_probability(ctx: &ChatContext) -> f64 {
    let mut p = BASE_OPEN_RATE;
    if hits_any(&ctx.signature, &SPILL_HINTS) {
        p += 0.08;
    }
    if hits_any(&ctx.signature, &FRAGILE_HINTS) {
        p += 0.06;
    }
    if hits_any(&ctx.remark, &CARE_HINTS) {
        p += 0.05;
    }
    if is_night(ctx.sim_minute) {
        p += 0.04;
    }
    if ctx.rider_rating < 4.5 {
        p += 0.05;
    }
    if ctx.late {
        p += 0.10;
    }
    p.clamp(0.0, MAX_OPEN_RATE)
}

/// 掷一次：这单的顾客要不要开口？
pub fn should_open(rng: &mut ChaCha8Rng, ctx: &ChatContext) -> bool {
    rng.random_bool(open_probability(ctx).clamp(0.0, 1.0))
}

/// 掷末端放置窗口（模拟分钟）：停车、上楼、放门口的实际耗时。
///
/// **每单都有**，与顾客开不开口无关——真人骑手要在这段时间里决定「放哪」，
/// AI 托管也要停一下，否则"到达即送达"根本不给对话留缝。
pub fn roll_window(rng: &mut ChaCha8Rng) -> u32 {
    rng.random_range(MIN_WINDOW_MIN..=MAX_WINDOW_MIN)
}

/// 顾客的**兜底开场白**。LLM 可用时会生成更贴题的一句，
/// 这条只在「没配 Key / 超时 / 输出非法」的降级路径上出现。
pub fn opening_line(ctx: &ChatContext) -> &'static str {
    if ctx.late {
        "师傅，都超时了，餐到哪了？"
    } else if hits_any(&ctx.signature, &SPILL_HINTS) {
        "师傅，汤别洒了哈，麻烦确认一下。"
    } else if hits_any(&ctx.signature, &FRAGILE_HINTS) {
        "师傅，蛋糕麻烦拿稳一点，谢谢。"
    } else if is_night(ctx.sim_minute) {
        "这么晚了，麻烦放门口轻一点，谢谢。"
    } else {
        "师傅，放门口就行，谢谢。"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    fn ctx(signature: &str, remark: &str) -> ChatContext {
        ChatContext {
            signature: signature.into(),
            remark: remark.into(),
            rider_rating: 4.9,
            sim_minute: 12 * 60,
            late: false,
        }
    }

    #[test]
    fn plain_order_stays_at_the_base_rate() {
        let p = open_probability(&ctx("招牌黄焖鸡", ""));
        assert!((p - BASE_OPEN_RATE).abs() < 1e-9, "普通单就该是 5%");
    }

    #[test]
    fn soup_and_cake_raise_the_odds() {
        let soup = open_probability(&ctx("招牌排骨汤", ""));
        let cake = open_probability(&ctx("生日蛋糕", ""));
        assert!(soup > BASE_OPEN_RATE);
        assert!(cake > BASE_OPEN_RATE);
        assert!(soup > cake, "汤比蛋糕更容易出事");
    }

    #[test]
    fn probability_never_exceeds_the_cap() {
        // 汤 + 冰（易碎）+ 备注关照 + 深夜 + 差评骑手 + 已超时：全叠满。
        let mut c = ctx("冰镇酸梅汤", "餐别洒了，轻放");
        c.rider_rating = 4.0;
        c.sim_minute = 2 * 60;
        c.late = true;
        assert_eq!(open_probability(&c), MAX_OPEN_RATE);
    }

    #[test]
    fn a_heavy_but_incomplete_profile_stays_under_the_cap() {
        // 汤 + 备注 + 差评骑手 = 0.05 + 0.08 + 0.05 + 0.05，不该被顶到上限。
        let mut c = ctx("招牌老火靓汤", "餐别洒了");
        c.rider_rating = 4.0;
        let p = open_probability(&c);
        assert!(p < MAX_OPEN_RATE);
        assert!((p - 0.23).abs() < 1e-9, "p={p}");
    }

    #[test]
    fn late_orders_are_the_loudest() {
        let mut base = ctx("招牌黄焖鸡", "");
        let quiet = open_probability(&base);
        base.late = true;
        assert!(open_probability(&base) - quiet >= 0.09);
    }

    #[test]
    fn most_orders_stay_silent() {
        // 用户要的「95% 不触发」：普通单跑 10000 次，开口率该落在基础值附近。
        let mut rng = ChaCha8Rng::seed_from_u64(2026);
        let c = ctx("招牌黄焖鸡", "");
        let hits = (0..10_000).filter(|_| should_open(&mut rng, &c)).count();
        let rate = hits as f64 / 10_000.0;
        assert!((rate - BASE_OPEN_RATE).abs() < 0.01, "rate={rate}");
    }

    #[test]
    fn window_is_inside_the_documented_range() {
        let mut rng = ChaCha8Rng::seed_from_u64(7);
        for _ in 0..500 {
            let w = roll_window(&mut rng);
            assert!((MIN_WINDOW_MIN..=MAX_WINDOW_MIN).contains(&w), "w={w}");
        }
    }

    #[test]
    fn roll_is_reproducible_for_the_same_seed() {
        let mut a = ChaCha8Rng::seed_from_u64(99);
        let mut b = ChaCha8Rng::seed_from_u64(99);
        let c = ctx("招牌排骨汤", "别洒");
        for _ in 0..200 {
            assert_eq!(should_open(&mut a, &c), should_open(&mut b, &c));
            assert_eq!(roll_window(&mut a), roll_window(&mut b));
        }
    }

    #[test]
    fn opening_line_matches_the_context() {
        let mut c = ctx("生日蛋糕", "");
        assert!(opening_line(&c).contains("蛋糕"));
        c = ctx("招牌黄焖鸡", "");
        c.late = true;
        assert!(opening_line(&c).contains("超时"));
        c = ctx("招牌黄焖鸡", "");
        c.sim_minute = 23 * 60;
        assert!(opening_line(&c).contains("这么晚"));
    }

    #[test]
    fn night_is_the_expected_band() {
        assert!(!is_night(12 * 60));
        assert!(is_night(23 * 60));
        assert!(is_night(2 * 60));
        assert!(!is_night(7 * 60));
    }
}
