//! 高德 API 接入（技术设计 §五）。
//!
//! ## 为什么这一层这么啰嗦
//!
//! 个人认证的配额是 **5000 / 5000 / 5000 / 100**——周边搜索只有 100 次/天，
//! 是整个系统最硬的约束。所以这一层不是「简单地包一下 HTTP」，而是四件事：
//!
//! | 对策 | 落在哪个文件 |
//! |---|---|
//! | ① 商户库内置，不查搜索接口 | `data/merchants.json`（本模块只做换城市时的批量转坐标） |
//! | ② 路径规划带 LRU 缓存 | [`route::RouteService`] |
//! | ③ 解析 `status` / `infocode` 双层错误 | [`client::AmapError`] |
//! | ④ 超限静默降级为直线 | [`route::RouteService::route`] |
//!
//! ## 一条铁律
//!
//! **没有 Key 不是错误，是「纯本地模式」。** [`AmapClient::from_env`] 返回 `Option`，
//! 上层拿到 `None` 就退回 `RouteService::offline()`，游戏照跑。
//! 绝不允许「因为没配高德 Key 就起不来」。

pub mod client;
pub mod geocode;
pub mod quota;
pub mod route;

pub use client::{AMAP_KEY_ENV, AmapClient, AmapConfig, AmapError};
pub use geocode::{BatchOutcome, GeocodeResult, GeocodeService, Place};
pub use quota::{Quota, QuotaExceeded, QuotaKind};
pub use route::{Route, RouteMode, RouteService, RouteSource, RouteStats};
