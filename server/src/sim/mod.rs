//! 模拟内核：纯逻辑、零 IO、固定种子可复现。
//!
//! 本模块内**禁止**出现：日志、文件读写、网络请求、系统时间。
//! 唯一的时间来源是 `World::tick_count` / `World::sim_minute`。
//!
//! 同样**禁止** `crate::human`：真人的判定归 [`crate::human`]，这里只收结论
//! （见 [`tick::TickInput`]）。本模块是被所有上层依赖的一层，不许反向依赖。
//! 源码级断言见 `server/tests/layering.rs`。

pub mod activity;
pub mod ai;
pub mod chat;
pub mod config;
/// 派单范围：骑手有效位置 → 订单取货点的距离判定。见模块内注释。
pub mod dispatch;
pub mod economy;
pub mod event;
pub mod geo;
pub mod merchant;
pub mod notice;
pub mod order;
pub mod rider;
pub mod risk;
pub mod route;
pub mod scheduler;
pub mod station;
pub mod task;
pub mod tick;
pub mod trajectory;
pub mod weather;
pub mod world;

/// 订单 id。
pub type OrderId = u32;
/// 骑手 id。协议层展示为 `r{id}`（如 `r3`），LLM JSON 也用这个形式。
pub type RiderId = u32;

/// 一天的模拟分钟数。
pub const MINUTES_PER_DAY: u32 = 24 * 60;

/// 把骑手 id 转成对外字符串（LLM / 日志用）。
pub fn rider_key(id: RiderId) -> String {
    format!("r{id}")
}

/// 解析 `r3` → `3`。解析失败返回 `None`，调用方必须丢弃该条数据。
pub fn parse_rider_key(key: &str) -> Option<RiderId> {
    key.strip_prefix('r')?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rider_key_roundtrip() {
        assert_eq!(rider_key(3), "r3");
        assert_eq!(parse_rider_key("r3"), Some(3));
        assert_eq!(parse_rider_key("3"), None);
        assert_eq!(parse_rider_key("rX"), None);
    }
}
