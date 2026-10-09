//! LLM 任务判定（玩法设计 §6.3，二期）。
//!
//! # 核心原则：**LLM 只判断，不裁决。**
//!
//! ```text
//! 顾客自由输入
//!       ↓
//! LLM 输出结构化 JSON      ← 只是「看法」
//!       ↓
//! 代码校验字段合法性并执行   ← 真正的「裁决」，且不可绕过
//! ```
//!
//! 具体来说，代码负责：
//! - `task_done` 必须是布尔值，是别的类型就整条判定作废；
//! - `rating_delta` 必须夹到 `[-0.1, 0.1]`，LLM 说 −5 分也没用；
//! - `penalty_cents` 必须夹到 `[0, 上限]`，**LLM 不能让骑手倒欠钱**；
//! - 任何一步失败，回退到零延迟的关键词匹配。
//!
//! 换句话说：LLM 挂了、乱说、超时，任务系统照样能跑，只是判定粗糙一点。

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::sim::task::matches_any;

use super::client::{LlmClient, LlmConfig, LlmError};

/// 判定结果的来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RulingSource {
    /// LLM 判定且通过校验。
    Llm,
    /// 关键词匹配（LLM 没开或失败）。
    #[default]
    Keyword,
    /// LLM 输出非法，已作废并回退关键词。
    Rejected,
}

impl RulingSource {
    pub fn as_str(self) -> &'static str {
        match self {
            RulingSource::Llm => "llm",
            RulingSource::Keyword => "keyword",
            RulingSource::Rejected => "rejected",
        }
    }
}

/// 一次任务判定。**注意所有字段都已经过代码夹取，可以直接执行。**
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRuling {
    pub task_done: bool,
    pub reason: String,
    /// 已夹到 `[-0.1, 0.1]`。
    pub rating_delta: f32,
    /// 已夹到 `[0, max_penalty_cents]`，恒 ≥ 0。
    pub penalty_cents: i64,
    #[serde(skip)]
    pub source: RulingSource,
}

impl TaskRuling {
    /// 关键词兜底：命中 `completion_keywords` 任一即算完成。零延迟、可复现。
    pub fn from_keywords(customer_input: &str, completion_keywords: &[String]) -> Self {
        let done =
            !completion_keywords.is_empty() && matches_any(customer_input, completion_keywords);
        Self {
            task_done: done,
            reason: if done {
                "命中完成关键词".to_string()
            } else {
                "未命中完成关键词".to_string()
            },
            rating_delta: 0.0,
            penalty_cents: 0,
            source: RulingSource::Keyword,
        }
    }
}

/// 数值上限。**这些数字是「代码的最终解释权」**，不是给 LLM 的建议。
#[derive(Debug, Clone, Copy)]
pub struct RulingLimits {
    pub min_rating_delta: f32,
    pub max_rating_delta: f32,
    pub max_penalty_cents: i64,
    pub max_reason_chars: usize,
}

impl Default for RulingLimits {
    fn default() -> Self {
        Self {
            min_rating_delta: -0.1,
            max_rating_delta: 0.1,
            // 玩法设计 §6.2 的罚金上限。LLM 无权突破。
            max_penalty_cents: 500,
            max_reason_chars: 120,
        }
    }
}

impl RulingLimits {
    /// 单次通话超时。跟战术大脑一样是 3 秒。
    pub const TIMEOUT: Duration = Duration::from_secs(3);
}

/// 系统提示词。照抄玩法设计 §6.3 的格式约定。
pub const SYSTEM_PROMPT: &str = "你是一个外卖场景的裁决器。只输出 JSON，不要任何解释。\n\
输出格式：{\"task_done\": bool, \"reason\": string, \"rating_delta\": number, \"penalty_cents\": int}\n\
rating_delta 范围 -0.1 到 0.1。penalty_cents 必须是非负整数，且不得大于 500。\n\
判断依据：骑手的回复是否真正完成了这次任务对话的要求。";

/// LLM 任务裁决器。
#[derive(Clone, Debug)]
pub struct TaskJudge {
    client: Option<LlmClient>,
    limits: RulingLimits,
}

impl TaskJudge {
    pub fn new(client: Option<LlmClient>) -> Self {
        Self {
            client,
            limits: RulingLimits::default(),
        }
    }

    pub fn from_env() -> Self {
        Self::new(LlmClient::from_env())
    }

    /// 从一份显式配置建（控制台里填的 Key 走这条路）。没 Key 即关键词兜底。
    pub fn from_config(cfg: &LlmConfig) -> Self {
        Self::new(LlmClient::new(cfg.clone()).ok())
    }

    /// 关闭状态：全部走关键词。
    pub fn disabled() -> Self {
        Self::new(None)
    }

    pub fn with_limits(mut self, limits: RulingLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn is_enabled(&self) -> bool {
        self.client.is_some()
    }

    pub fn limits(&self) -> RulingLimits {
        self.limits
    }

    /// 判定一次任务对话。**永不返回 Err**，最差回退关键词。
    pub async fn judge(
        &self,
        scenario: &str,
        customer_input: &str,
        completion_keywords: &[String],
    ) -> TaskRuling {
        let fallback = TaskRuling::from_keywords(customer_input, completion_keywords);
        let Some(client) = self.client.as_ref() else {
            return fallback;
        };

        let user = format!(
            "场景：{scenario}\n顾客说：{customer_input}\n骑手需要完成的动作：请判断是否已完成。"
        );
        let raw = match client.complete_json(SYSTEM_PROMPT, &user).await {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(error = %e, "LLM 任务判定失败，回退关键词");
                let mut r = fallback;
                r.reason = format!("LLM 不可用（{e}），按关键词判定");
                r.source = RulingSource::Rejected;
                return r;
            }
        };

        match sanitize_ruling(&raw, self.limits) {
            Ok(mut r) => {
                r.source = RulingSource::Llm;
                r
            }
            Err(e) => {
                // 字段不合法 = 这次「看法」作废，不是将错就错。
                tracing::debug!(error = %e, "LLM 任务判定字段非法，作废并回退关键词");
                let mut r = fallback;
                r.reason = format!("LLM 输出非法（{e}），按关键词判定");
                r.source = RulingSource::Rejected;
                r
            }
        }
    }
}

/// **代码的最终解释权。** 校验并夹取 LLM 的字段。
///
/// 只有 `task_done` 是硬性字段：类型不对就整条作废。
/// 其余数值字段宽容处理（缺失/非法即取 0），但一律夹到合法区间。
pub fn sanitize_ruling(raw: &Value, limits: RulingLimits) -> Result<TaskRuling, LlmError> {
    let task_done = raw
        .get("task_done")
        .and_then(|v| v.as_bool())
        .ok_or(LlmError::MissingField("task_done"))?;

    let reason = raw
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .chars()
        .take(limits.max_reason_chars)
        .collect::<String>();

    let rating_delta = raw
        .get("rating_delta")
        .and_then(as_f64)
        .map(|v| v as f32)
        .unwrap_or(0.0)
        .clamp(limits.min_rating_delta, limits.max_rating_delta);

    let penalty_cents = raw
        .get("penalty_cents")
        .and_then(as_f64)
        .map(|v| v.round() as i64)
        .unwrap_or(0)
        .clamp(0, limits.max_penalty_cents);

    Ok(TaskRuling {
        task_done,
        reason,
        rating_delta: if rating_delta.is_nan() {
            0.0
        } else {
            rating_delta
        },
        penalty_cents,
        source: RulingSource::Llm,
    })
}

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        // 模型很爱把数字写成字符串
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn limits() -> RulingLimits {
        RulingLimits::default()
    }

    #[test]
    fn clamps_out_of_range_rating_delta() {
        let raw = json!({ "task_done": true, "reason": "完成了", "rating_delta": -5.0, "penalty_cents": 0 });
        let r = sanitize_ruling(&raw, limits()).unwrap();
        assert_eq!(r.rating_delta, -0.1, "LLM 说 -5 分也只能是 -0.1");

        let raw = json!({ "task_done": true, "rating_delta": 99.0 });
        assert_eq!(sanitize_ruling(&raw, limits()).unwrap().rating_delta, 0.1);
    }

    #[test]
    fn llm_cannot_make_the_rider_owe_money() {
        let raw = json!({ "task_done": false, "reason": "态度恶劣", "penalty_cents": 999_999 });
        let r = sanitize_ruling(&raw, limits()).unwrap();
        assert_eq!(r.penalty_cents, 500, "罚金上限由代码说了算");

        let raw = json!({ "task_done": false, "penalty_cents": -800 });
        assert_eq!(
            sanitize_ruling(&raw, limits()).unwrap().penalty_cents,
            0,
            "负数罚金一律归零"
        );
    }

    #[test]
    fn accepts_numbers_written_as_strings() {
        let raw = json!({ "task_done": true, "rating_delta": "-0.05", "penalty_cents": "200" });
        let r = sanitize_ruling(&raw, limits()).unwrap();
        assert!((r.rating_delta + 0.05).abs() < 1e-6);
        assert_eq!(r.penalty_cents, 200);
    }

    #[test]
    fn non_boolean_task_done_invalidates_the_whole_ruling() {
        for bad in [json!("true"), json!(1), json!(null), json!([true])] {
            let raw = json!({ "task_done": bad, "reason": "x" });
            assert!(
                matches!(
                    sanitize_ruling(&raw, limits()),
                    Err(LlmError::MissingField("task_done"))
                ),
                "{bad} 不该被当成布尔值"
            );
        }
    }

    #[test]
    fn missing_optional_fields_default_to_zero() {
        let raw = json!({ "task_done": true });
        let r = sanitize_ruling(&raw, limits()).unwrap();
        assert_eq!(r.rating_delta, 0.0);
        assert_eq!(r.penalty_cents, 0);
        assert!(r.reason.is_empty());
    }

    #[test]
    fn reason_is_truncated() {
        let long = "啊".repeat(500);
        let raw = json!({ "task_done": true, "reason": long });
        let r = sanitize_ruling(&raw, limits()).unwrap();
        assert_eq!(r.reason.chars().count(), limits().max_reason_chars);
    }

    #[test]
    fn keyword_fallback_matches_completion_keywords() {
        let kw = vec!["放好".to_string(), "谢谢".to_string()];
        assert!(TaskRuling::from_keywords("已经放好了，谢谢", &kw).task_done);
        assert!(!TaskRuling::from_keywords("我还在路上", &kw).task_done);
        // 关键词为空时不算完成，避免「什么都没配就自动通关」
        assert!(!TaskRuling::from_keywords("随便", &[]).task_done);
    }

    #[tokio::test]
    async fn disabled_judge_uses_keywords_without_any_io() {
        let judge = TaskJudge::disabled();
        assert!(!judge.is_enabled());
        let r = judge.judge("放门口", "放好了", &["放好".into()]).await;
        assert_eq!(r.source, RulingSource::Keyword);
        assert!(r.task_done);
        assert_eq!(r.penalty_cents, 0);
    }

    #[tokio::test]
    async fn llm_verdict_never_exceeds_the_limits() {
        // 用 sanitize 直接验证「执行侧」的边界，不需要真网络。
        let raw = json!({ "task_done": true, "reason": "好", "rating_delta": 0.5, "penalty_cents": 100_000 });
        let r = sanitize_ruling(&raw, limits()).unwrap();
        assert!(r.rating_delta <= 0.1);
        assert!(r.penalty_cents <= 500);
    }

    #[test]
    fn default_limits_match_the_design() {
        let l = limits();
        assert_eq!(l.min_rating_delta, -0.1);
        assert_eq!(l.max_rating_delta, 0.1);
        assert_eq!(l.max_penalty_cents, 500);
        assert_eq!(RulingLimits::TIMEOUT, Duration::from_secs(3));
    }
}
