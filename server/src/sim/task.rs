//! 任务对话（预设部分）。
//!
//! **核心原则：LLM 只判断，不裁决。** 预设任务走关键词匹配，零延迟、可复现。
//! LLM 任务（可选）在 `crate::llm::task` 里，输出结构化 JSON 后由代码校验。

use serde::{Deserialize, Serialize};

/// 关键词状态机的失败处理。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TaskOutcome {
    #[serde(default)]
    pub rating_delta: f32,
    #[serde(default)]
    pub penalty_cents: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TaskStep {
    pub rider_prompt: String,
    /// 顾客可能说的话。
    #[serde(default)]
    pub customer_responses: Vec<String>,
    /// 触发下一步需要的骑手关键词。
    #[serde(default)]
    pub requires_keywords: Vec<String>,
    /// 命中即算完成的骑手关键词。
    #[serde(default)]
    pub completion_keywords: Vec<String>,
    #[serde(default)]
    pub reward_cents: i64,
    #[serde(default)]
    pub penalty_cents: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TaskTemplate {
    pub id: String,
    pub name: String,
    pub steps: Vec<TaskStep>,
    #[serde(default)]
    pub on_complete: TaskOutcome,
    #[serde(default)]
    pub on_fail: TaskOutcome,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TaskFile {
    pub tasks: Vec<TaskTemplate>,
}

/// 关键词匹配。空要求视为通过。
pub fn matches_any(text: &str, keywords: &[String]) -> bool {
    if keywords.is_empty() {
        return true;
    }
    let lower = text.to_lowercase();
    keywords.iter().any(|k| lower.contains(&k.to_lowercase()))
}

/// 预设任务的推进结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepResult {
    /// 命中完成关键词。
    Completed,
    /// 前进到下一步。
    Advanced,
    /// 没命中要求的关键词，原地不动。
    Rejected,
}

/// 推进一步。`step_index` 从 0 开始。
pub fn advance_step(template: &TaskTemplate, step_index: usize, rider_text: &str) -> StepResult {
    let Some(step) = template.steps.get(step_index) else {
        return StepResult::Completed;
    };
    if !step.completion_keywords.is_empty() && matches_any(rider_text, &step.completion_keywords) {
        return StepResult::Completed;
    }
    if matches_any(rider_text, &step.requires_keywords) {
        if step_index + 1 >= template.steps.len() {
            StepResult::Completed
        } else {
            StepResult::Advanced
        }
    } else {
        StepResult::Rejected
    }
}

/// 给顾客挑一句回复（纯函数，由调用方提供随机数）。
pub fn customer_reply(step: &TaskStep, pick: usize) -> String {
    if step.customer_responses.is_empty() {
        "……".to_string()
    } else {
        step.customer_responses[pick % step.customer_responses.len()].clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template() -> TaskTemplate {
        TaskTemplate {
            id: "leave_at_door".into(),
            name: "放门口".into(),
            steps: vec![
                TaskStep {
                    rider_prompt: "放哪个门？".into(),
                    customer_responses: vec!["放门口吧".into()],
                    requires_keywords: vec!["门".into()],
                    completion_keywords: vec![],
                    reward_cents: 0,
                    penalty_cents: 0,
                },
                TaskStep {
                    rider_prompt: "好的，已放好。".into(),
                    customer_responses: vec!["谢谢".into()],
                    requires_keywords: vec![],
                    completion_keywords: vec!["放好".into(), "谢谢".into()],
                    reward_cents: 0,
                    penalty_cents: 0,
                },
            ],
            on_complete: TaskOutcome::default(),
            on_fail: TaskOutcome {
                rating_delta: -0.02,
                penalty_cents: 0,
            },
        }
    }

    #[test]
    fn step_machine() {
        let t = template();
        assert_eq!(advance_step(&t, 0, "我打不开门"), StepResult::Advanced);
        assert_eq!(advance_step(&t, 0, "你好"), StepResult::Rejected);
        assert_eq!(advance_step(&t, 1, "已经放好了"), StepResult::Completed);
        assert_eq!(advance_step(&t, 9, "随便"), StepResult::Completed);
    }

    #[test]
    fn empty_keywords_always_match() {
        assert!(matches_any("任意内容", &[]));
        assert!(!matches_any("任意内容", &["密码".into()]));
    }

    #[test]
    fn customer_reply_wraps() {
        let step = &template().steps[0];
        assert_eq!(customer_reply(step, 5), "放门口吧");
        assert_eq!(
            customer_reply(
                &TaskStep {
                    rider_prompt: String::new(),
                    customer_responses: vec![],
                    requires_keywords: vec![],
                    completion_keywords: vec![],
                    reward_cents: 0,
                    penalty_cents: 0,
                },
                0
            ),
            "……"
        );
    }
}
