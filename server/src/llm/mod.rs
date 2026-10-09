//! LLM 接入（技术设计 §7.3、玩法设计 §6.3、§九）。
//!
//! ## 边界：LLM 只做「可以偶尔失败降级」的事
//!
//! 玩法设计 §九给了判断标准：
//!
//! ```text
//! 需要在 16 毫秒内完成？ → 算法
//! 需要结果可复现？        → 算法
//! 可以偶尔失败降级？      → 可以用 LLM
//! ```
//!
//! 所以这一层只有三个入口，全部「失败即降级」：
//!
//! | 入口 | 用途 | 降级目标 |
//! |---|---|---|
//! | [`brain::TacticalBrain`] | 每时段 1 次的战术分配 | [`crate::sim::ai::batch_decide`] |
//! | [`task::TaskJudge`] | 顾客自由输入的任务判定 | 关键词匹配 |
//! | [`client::LlmClient`] | 店名 / 文案一次性生成 | 预置文案 |
//!
//! ## 一条绝对不能破的规矩
//!
//! **随机性绝不来自 LLM。** 世界种子固定，所有随机数走 ChaCha8。
//! LLM 只能影响「谁接哪一单」「这单算不算完成」这类可以被人为指定的事，
//! 一旦它参与金额或事故判定，10 万 tick 的可复现性就没了。
//!
//! ## 没 Key 也是一种正常工作状态
//!
//! [`client::LlmClient::from_env`] 返回 `Option`。没有 Key 时战术大脑是
//! `Disabled`、任务判定走关键词、命令行照样能打印一整天的流水。

pub mod brain;
pub mod client;
pub mod task;

pub use brain::{
    BrainOutcome, BrainSource, BrainStats, MAX_CALLS_PER_DAY, PlanFetch, PreparedPlan,
    TacticalBrain, Throttle,
};
pub use client::{ENV_API_KEY, ENV_BASE_URL, ENV_MODEL, LlmClient, LlmConfig, LlmError};
pub use task::{RulingLimits, RulingSource, TaskJudge, TaskRuling};
