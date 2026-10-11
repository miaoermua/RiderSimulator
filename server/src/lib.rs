//! rider-sim 服务端库。
//!
//! 分层（依赖方向严格单向）：
//!
//! ```text
//! sim/     纯逻辑，零 IO，可单测       ← 被所有上层依赖，自己不依赖任何东西
//! human/   真人身份 + AI 让位判定      ← 依赖 sim，sim 反向禁止
//! amap/    高德 REST + LRU + 配额      ← IO
//! llm/     真 LLM 决策（OpenAI 兼容）  ← IO
//! store/   SQLite                     ← IO
//! net/     协议 + hub + WebSocket      ← IO
//! app.rs   tick 主循环，把上面几层粘起来
//! ```
//!
//! `sim/` 里禁止日志、读文件、发网络请求。所有外部输入（路线、LLM 计划）
//! 都作为参数传进 `sim::tick::tick`。
//!
//! `sim/` 里也禁止 `crate::human`：模拟的规则对真人和 AI 一视同仁，
//! 需要知道「谁是真人」的判定集中在 `human/`。两条约束都有源码级断言
//! （`server/tests/layering.rs`）。

// 分阶段编译：已落地的模块先接上，其余仍在实现中。
pub mod amap;
pub mod app;
pub mod auth;
pub mod content;
pub mod human;
pub mod llm;
pub mod net;
pub mod sim;
pub mod store;

/// 协议版本，握手时下发给客户端。
pub const PROTOCOL_VERSION: &str = "1.0";
