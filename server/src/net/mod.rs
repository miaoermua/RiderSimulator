//! 网络层：协议 + 连接注册表 + WebSocket 接入。
//!
//! ```text
//! protocol.rs  消息定义、固定错误码、角色      纯数据，零 IO，可单测
//! hub.rs       连接注册表 + 按角色过滤下发     纯数据结构，可单测
//! api.rs       远程控制 / 派单 HTTP 接口       鉴权 + 搬运，不含规则
//! ws.rs        axum 接入，字节流 ⇄ Inbound     唯一碰 tokio/axum 的地方
//! ```
//!
//! **权限过滤放在 [`hub`]**，因为它是唯一「知道每个连接是谁」的地方。
//! 而**状态变更放在 `app`**，因为只有那里拿着 `&mut World`。
//! HTTP 接口（[`api`]）与 WebSocket 共用同一条 [`Inbound`] 队列，所以两条
//! 入口看到的永远是同一个世界，不需要加锁。

pub mod api;
pub mod hub;
pub mod protocol;
pub mod ws;

pub use api::{ApiAuth, ApiError, ApiState, DispatchItem, RpcReply, RpcRequest};
pub use hub::{
    ConnId, Connection, Hub, HubStats, event_name, render, render_snapshot, server_info,
};
pub use protocol::{
    Ack, ClientMessage, ConfigPatch, Envelope, MAX_FRAME_BYTES, PROTOCOL_VERSION, Role, Welcome,
    code,
};
pub use ws::{HEALTH_PATH, Inbound, OutboundTx, WS_PATH, next_conn_id, router};
