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
//! ## 分层：协议在 [`protocol`]，时序在 [`brain`]
//!
//! [`protocol`] 定义「请求长什么样、响应怎么校验」，可以脱离 HTTP 单测；
//! [`brain`] 负责「什么时候调、调完怎么落库、失败怎么降级」。
//! 校验层是**代码的最终解释权**——《LLM协议设计》§4.1 的四级流水线在这里实现，
//! LLM 说什么都要先过它。
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
pub mod customer;
pub mod director_brain;
pub mod orchestrator;
pub mod protocol;
pub mod task;

pub use brain::{
    BrainOutcome, BrainSource, BrainStats, MAX_CALLS_PER_DAY, PlanFetch, PreparedPlan,
    StationOpBrief, TacticalBrain, Throttle,
};
pub use client::{
    ENV_API_KEY, ENV_BASE_URL, ENV_JSON_MODE, ENV_MODEL, ENV_MODEL_DIRECTOR, ENV_MODEL_RIDER,
    ENV_MODEL_STATION, LayerKey, LayerModels, LlmClient, LlmConfig, LlmError, ModelInfo,
    normalize_base_url,
};
pub use director_brain::{
    DirectorBrain, DirectorOutcome, PreparedWorldPlan, ValidatedWorldOp, WorldFetch, WorldOp,
    WorldRequest, WorldResponse, WorldVerdict, validate_world_ops,
};
pub use orchestrator::{AiLayer, AiOrchestrator, DEFAULT_MAX_CALLS_PER_DAY, LayerStats};
pub use protocol::{
    ActKind, Action, BrainRequest, DecisionResponse, DecisionVerdict, MAX_PROMPT_BYTES,
    MAX_RIDERS_IN_PROMPT, PROTOCOL_VERSION, PolicyPatch, RiderSnap, SimCtx, StationOp, TrimStats,
    ValidatedAct, ValidatedPolicy, ValidatedStationOp, state_fingerprint, system_prompt,
    user_prompt, validate_decision,
};
pub use task::{RulingLimits, RulingSource, TaskJudge, TaskRuling};
