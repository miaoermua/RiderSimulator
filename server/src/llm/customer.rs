//! AI 顾客（末端交付沟通）：「说什么、要不要索赔」。
//!
//! # 与 [`crate::llm::task::TaskJudge`] 同一条原则：**LLM 只说话，钱由代码算。**
//!
//! 真实场景里顾客的话是带**经济后果**的——「汤洒了一半，赔我」。如果让 LLM
//! 直接决定赔多少，模型随口报个 500 就直接改账本了。所以这里：
//!
//! - `reply` 截断到 [`MAX_REPLY_CHARS`]，防止模型吐长篇大论刷屏；
//! - `compensation_cents` 夹到 `[0, max_compensation_cents]`，**LLM 多要不走一分钱**；
//! - `fault` 必须是三个合法值之一，认不出来一律当「没人有错」——**宁可少扣，不可错扣**；
//! - 「没人有错」却要索赔 = 模型自相矛盾，金额直接抹平；
//! - 任何一步失败，回退到零延迟的预设回应：顾客照样说话，只是不索赔、不扣分。
//!
//! **触发与否不在这里**：要不要开口是 [`crate::sim::chat`] 掷骰子决定的
//! （本地算法、可复现、零 token）。本模块只在「已经决定开口」之后负责台词和索赔档位。

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::client::{LlmClient, LlmConfig};

/// 单次索赔的**代码硬顶**（分）。LLM 说要 5000 也没用。
pub const MAX_COMPENSATION_CENTS: i64 = 2000;
/// 顾客一句话的长度上限（字符）。真实顾客不会发小作文。
pub const MAX_REPLY_CHARS: usize = 120;
/// 评分只允许小幅波动：一次末端摩擦不该毁掉一天的评价。
pub const RATING_DELTA_MIN: f32 = -0.2;
pub const RATING_DELTA_MAX: f32 = 0.1;

/// 责任方。**钱从谁身上出由它决定**，这是「按责任分摊」的落点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fault {
    /// 骑手的问题（放置不当、颠簸、超时）。钱从骑手身上出。
    Rider,
    /// 商家的出餐问题（漏装、没封口、本来就凉）。**不动骑手的钱。**
    Merchant,
    /// 没人有错（顾客没接电话、地址写错）。不扣任何人。
    #[default]
    None,
}

impl Fault {
    pub fn as_str(self) -> &'static str {
        match self {
            Fault::Rider => "rider",
            Fault::Merchant => "merchant",
            Fault::None => "none",
        }
    }

    /// 要不要从骑手账上扣钱。
    pub fn hits_rider(self) -> bool {
        matches!(self, Fault::Rider)
    }
}

/// 这条回应是怎么来的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReplySource {
    /// LLM 产出且通过校验。
    Llm,
    /// 预设兜底（没配 Key、超时、输出非法）。
    #[default]
    Preset,
    /// LLM 输出了非法字段，整条作废后回退预设。
    Rejected,
}

/// 一次顾客回应。**所有字段都已夹取，可以拿起来直接执行。**
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatReply {
    /// 顾客说的话。已截断到 [`MAX_REPLY_CHARS`]。
    pub reply: String,
    /// 情绪：`满意` | `一般` | `不满`。
    pub mood: String,
    /// 责任方。
    pub fault: Fault,
    /// 索赔金额（分），已夹到 `[0, max_compensation_cents]`，恒 ≥ 0。
    pub compensation_cents: i64,
    /// 评分变化，已夹到 `[RATING_DELTA_MIN, RATING_DELTA_MAX]`。
    pub rating_delta: f32,
    #[serde(skip)]
    pub source: ReplySource,
}

impl ChatReply {
    /// **预设兜底。降级路径永远不产生经济后果**——顾客可以把话说得不满，
    /// 但钱和评分只能由 LLM 正常通道提出、再由 [`ChatReply::sanitize`]-级的代码夹取。
    pub fn preset(mood: &str, reply: &str) -> Self {
        Self {
            reply: truncate(reply, MAX_REPLY_CHARS),
            mood: mood.to_string(),
            fault: Fault::None,
            compensation_cents: 0,
            rating_delta: 0.0,
            source: ReplySource::Preset,
        }
    }
}

/// 夹取上限。控制台可以调低，但**改不高过 [`MAX_COMPENSATION_CENTS`]**。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChatLimits {
    /// 单次通话超时。跟战术大脑、任务判定一致，3 秒。
    pub timeout: Duration,
    /// 单次索赔上限（分）。
    pub max_compensation_cents: i64,
}

impl Default for ChatLimits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(3),
            max_compensation_cents: MAX_COMPENSATION_CENTS,
        }
    }
}

impl ChatLimits {
    pub fn clamped(mut self) -> Self {
        self.max_compensation_cents = self
            .max_compensation_cents
            .clamp(0, MAX_COMPENSATION_CENTS);
        self
    }
}

/// 系统提示词。**钱的边界写在提示里，但真正的闸门在代码里。**
pub const SYSTEM_PROMPT: &str = "你是一个外卖订单的顾客。只输出 JSON，不要任何解释。\n\
输出格式：{\"reply\": string, \"mood\": \"满意\"|\"一般\"|\"不满\", \"fault\": \"rider\"|\"merchant\"|\"none\", \"compensation_cents\": int, \"rating_delta\": number}\n\
reply 是顾客对骑手说的话，20 字以内，口语化，符合你的订单情况。\n\
只有餐品确实出了问题（洒了、凉了、少了、放错地方）才提赔偿，compensation_cents 是索赔金额（单位：分），最高 2000，没出问题填 0。\n\
fault 填责任方：骑手放置或配送不当填 rider，商家出餐问题填 merchant，没人有错填 none。\n\
rating_delta 范围 -0.2 到 0.1：满意可以给小正数，不满给小负数。";

/// 顾客大脑。
#[derive(Clone, Debug)]
pub struct CustomerBrain {
    client: Option<LlmClient>,
    limits: ChatLimits,
}

impl CustomerBrain {
    pub fn new(client: Option<LlmClient>) -> Self {
        Self {
            client,
            limits: ChatLimits::default(),
        }
    }

    pub fn from_env() -> Self {
        Self::new(LlmClient::from_env())
    }

    /// 从一份显式配置建（控制台里填的 Key 走这条路）。没 Key 即全预设。
    pub fn from_config(cfg: &LlmConfig) -> Self {
        Self::new(LlmClient::new(cfg.clone()).ok())
    }

    /// 关闭状态：全部走预设话术。
    pub fn disabled() -> Self {
        Self::new(None)
    }

    pub fn with_limits(mut self, limits: ChatLimits) -> Self {
        self.limits = limits.clamped();
        self
    }

    pub fn is_enabled(&self) -> bool {
        self.client.is_some()
    }

    pub fn limits(&self) -> ChatLimits {
        self.limits
    }

    /// 顾客主动开口（骑手还没说话）。**永不返回 Err**，最差回退 `fallback`。
    ///
    /// `fallback` 由 [`crate::sim::chat::opening_line`] 给，与 LLM 看到的是同一份画像。
    pub async fn open(&self, context: &str, fallback: &str) -> ChatReply {
        let fb = ChatReply::preset("一般", fallback);
        let Some(client) = self.client.as_ref() else {
            return fb;
        };
        let user = format!(
            "订单情况：{context}\n骑手刚到门口，你（顾客）主动说第一句话。"
        );
        self.call(client, &user, fb).await
    }

    /// 回应骑手说的话。**永不返回 Err**，最差回退预设。
    ///
    /// `history` 是这单此前的对话（`说话人：内容`），只取最近几条，别把上下文喂爆。
    pub async fn respond(&self, context: &str, rider_text: &str, history: &[String]) -> ChatReply {
        let fb = ChatReply::preset("一般", "好的，知道了，谢谢师傅。");
        let Some(client) = self.client.as_ref() else {
            return fb;
        };
        let mut user = format!("订单情况：{context}\n");
        if !history.is_empty() {
            user.push_str(&format!("此前对话：{}\n", history.join(" / ")));
        }
        user.push_str(&format!(
            "骑手说：{rider_text}\n请以顾客身份回一句话。如果餐品有问题就提出来，没问题就说句知道了。"
        ));
        self.call(client, &user, fb).await
    }

    async fn call(&self, client: &LlmClient, user: &str, fallback: ChatReply) -> ChatReply {
        let raw = match client.complete_json(SYSTEM_PROMPT, user).await {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(error = %e, "顾客大脑调用失败，回退预设话术");
                return fallback;
            }
        };
        self.sanitize(&raw, fallback)
    }

    /// 校验 + 夹取。**这是真正的裁决点，LLM 的每一个数字都要过这里。**
    fn sanitize(&self, raw: &Value, fallback: ChatReply) -> ChatReply {
        let Some(reply) = raw.get("reply").and_then(|v| v.as_str()) else {
            return ChatReply {
                source: ReplySource::Rejected,
                ..fallback
            };
        };
        let reply = reply.trim();
        if reply.is_empty() {
            return ChatReply {
                source: ReplySource::Rejected,
                ..fallback
            };
        }

        let mood = match raw.get("mood").and_then(|v| v.as_str()) {
            Some("满意") => "满意",
            Some("不满") => "不满",
            // 认不出来就当中性，不放大情绪。
            _ => "一般",
        };

        let fault = match raw.get("fault").and_then(|v| v.as_str()) {
            Some("rider") => Fault::Rider,
            Some("merchant") => Fault::Merchant,
            // **认不出来的责任方一律当「没人有错」**：宁可少扣钱，不可错扣钱。
            _ => Fault::None,
        };

        let claimed = as_cents(raw.get("compensation_cents"))
            .clamp(0, self.limits.max_compensation_cents);
        // 没人有错却要索赔 = 模型自相矛盾，金额抹平。
        let compensation_cents = if fault == Fault::None { 0 } else { claimed };

        let rating_delta = as_f64(raw.get("rating_delta"))
            .clamp(RATING_DELTA_MIN as f64, RATING_DELTA_MAX as f64) as f32;

        ChatReply {
            reply: truncate(reply, MAX_REPLY_CHARS),
            mood: mood.to_string(),
            fault,
            compensation_cents,
            rating_delta,
            source: ReplySource::Llm,
        }
    }
}

/// 数字字段的宽松解析：LLM 经常把 `800` 写成 `"800"` 或 `800.0`。
fn as_cents(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse::<i64>().unwrap_or(0),
        _ => 0,
    }
}

fn as_f64(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// 按**字符**截断（不是字节），中文不会被切碎。
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn brain() -> CustomerBrain {
        CustomerBrain::disabled()
    }

    #[test]
    fn disabled_brain_only_speaks_presets() {
        assert!(!brain().is_enabled());
        let fb = ChatReply::preset("一般", "放门口就行");
        assert_eq!(fb.compensation_cents, 0);
        assert_eq!(fb.rating_delta, 0.0);
        assert_eq!(fb.fault, Fault::None);
        assert_eq!(fb.source, ReplySource::Preset);
    }

    #[test]
    fn a_valid_reply_passes_through() {
        let raw = json!({
            "reply": "汤洒了一半，这怎么吃啊",
            "mood": "不满",
            "fault": "rider",
            "compensation_cents": 800,
            "rating_delta": -0.15,
        });
        let r = brain().sanitize(&raw, ChatReply::preset("一般", "嗯"));
        assert_eq!(r.source, ReplySource::Llm);
        assert_eq!(r.reply, "汤洒了一半，这怎么吃啊");
        assert_eq!(r.fault, Fault::Rider);
        assert_eq!(r.compensation_cents, 800);
        assert!((r.rating_delta + 0.15).abs() < 1e-6);
    }

    #[test]
    fn compensation_is_capped_no_matter_what_the_model_says() {
        let raw = json!({
            "reply": "赔我",
            "mood": "不满",
            "fault": "rider",
            "compensation_cents": 999_999,
            "rating_delta": -5.0,
        });
        let r = brain().sanitize(&raw, ChatReply::preset("一般", "嗯"));
        assert_eq!(r.compensation_cents, MAX_COMPENSATION_CENTS);
        assert_eq!(r.rating_delta, RATING_DELTA_MIN);
    }

    #[test]
    fn a_lower_limit_from_config_is_respected() {
        let b = brain().with_limits(ChatLimits {
            max_compensation_cents: 300,
            ..Default::default()
        });
        let raw = json!({
            "reply": "赔我",
            "mood": "不满",
            "fault": "rider",
            "compensation_cents": 2000,
            "rating_delta": 0.0,
        });
        assert_eq!(b.sanitize(&raw, ChatReply::preset("一般", "嗯")).compensation_cents, 300);
    }

    #[test]
    fn limits_can_never_be_raised_above_the_hard_cap() {
        let b = brain().with_limits(ChatLimits {
            max_compensation_cents: 50_000,
            ..Default::default()
        });
        assert_eq!(b.limits().max_compensation_cents, MAX_COMPENSATION_CENTS);
    }

    #[test]
    fn unknown_fault_never_costs_the_rider_money() {
        let raw = json!({
            "reply": "餐凉了",
            "mood": "不满",
            "fault": "kitchen",       // 不是合法值
            "compensation_cents": 900,
            "rating_delta": -0.1,
        });
        let r = brain().sanitize(&raw, ChatReply::preset("一般", "嗯"));
        assert_eq!(r.fault, Fault::None);
        assert_eq!(r.compensation_cents, 0, "认不出责任方就不许扣钱");
        assert!(!r.fault.hits_rider());
    }

    #[test]
    fn no_fault_but_asking_for_money_is_erased() {
        let raw = json!({
            "reply": "算了不用赔",
            "mood": "一般",
            "fault": "none",
            "compensation_cents": 500,
            "rating_delta": 0.0,
        });
        let r = brain().sanitize(&raw, ChatReply::preset("一般", "嗯"));
        assert_eq!(r.compensation_cents, 0, "没人有错却索赔 = 模型自相矛盾");
    }

    #[test]
    fn merchant_fault_does_not_touch_the_rider() {
        let raw = json!({
            "reply": "商家漏装了，不怪你",
            "mood": "一般",
            "fault": "merchant",
            "compensation_cents": 600,
            "rating_delta": 0.0,
        });
        let r = brain().sanitize(&raw, ChatReply::preset("一般", "嗯"));
        assert_eq!(r.fault, Fault::Merchant);
        assert!(!r.fault.hits_rider());
        assert_eq!(r.compensation_cents, 600, "钱要赔，但不从骑手身上出");
    }

    #[test]
    fn string_numbers_are_accepted() {
        let raw = json!({
            "reply": "洒了点",
            "mood": "不满",
            "fault": "rider",
            "compensation_cents": "800",
            "rating_delta": "-0.1",
        });
        let r = brain().sanitize(&raw, ChatReply::preset("一般", "嗯"));
        assert_eq!(r.compensation_cents, 800);
        assert!((r.rating_delta + 0.1).abs() < 1e-6);
    }

    #[test]
    fn a_garbage_payload_falls_back_to_the_preset() {
        let raw = json!({ "reply": "", "mood": "不满" });
        let r = brain().sanitize(&raw, ChatReply::preset("一般", "放门口就行"));
        assert_eq!(r.source, ReplySource::Rejected);
        assert_eq!(r.reply, "放门口就行");
        assert_eq!(r.compensation_cents, 0);

        let missing = json!({ "mood": "不满" });
        let r2 = brain().sanitize(&missing, ChatReply::preset("一般", "放门口就行"));
        assert_eq!(r2.source, ReplySource::Rejected);
    }

    #[test]
    fn long_replies_are_truncated_by_character() {
        let long = "汤".repeat(300);
        let raw = json!({ "reply": long, "mood": "不满", "fault": "none" });
        let r = brain().sanitize(&raw, ChatReply::preset("一般", "嗯"));
        assert_eq!(r.reply.chars().count(), MAX_REPLY_CHARS + 1, "截断后带一个省略号");
        assert!(r.reply.ends_with('…'));
    }
}
