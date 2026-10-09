//! 路径规划 + LRU 缓存 + 超限降级（技术设计 §5.2 ②④）。
//!
//! 骑手永远在几个商圈里打转，AB 点重复率极高。缓存 2000 条能把日请求从
//! 上万压到 200 次以内。缓存没命中时调高德；高德挂了或配额用尽，
//! **静默降级成直线 + 抖动**——游戏不能因为 API 挂了卡住。

use std::num::NonZeroUsize;

use lru::LruCache;
use serde_json::Value;

use super::client::{AmapClient, AmapError, value_to_i64};
use super::quota::QuotaKind;
use crate::sim::geo::{
    LatLng, decode_amap_polyline, haversine_m, polyline_len_m, straight_line_with_jitter,
};
use crate::sim::world::ROAD_FACTOR;

/// 缓存容量。设计给的数字是 2000。
pub const CACHE_CAPACITY: usize = 2_000;
/// 降级轨迹的采样点数。
const FALLBACK_SAMPLES: usize = 32;

/// 配送用哪种出行方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RouteMode {
    /// 步行路径规划（v3）。
    #[default]
    Walking,
    /// 骑行路径规划（v4）。
    Bicycling,
}

impl RouteMode {
    pub fn path(self) -> &'static str {
        match self {
            RouteMode::Walking => "/v3/direction/walking",
            RouteMode::Bicycling => "/v4/direction/bicycling",
        }
    }

    /// 拿不到真实路线时的兜底速度（m/s）。
    pub fn default_speed_mps(self) -> f32 {
        match self {
            RouteMode::Walking => 1.4,
            RouteMode::Bicycling => 4.2,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            RouteMode::Walking => "walking",
            RouteMode::Bicycling => "bicycling",
        }
    }
}

/// 这条路线是从哪来的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteSource {
    /// 刚调了高德。
    Amap,
    /// LRU 缓存命中。
    Cache,
    /// 高德不可用 / 配额用尽，走了直线降级。
    Fallback,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub from: LatLng,
    pub to: LatLng,
    /// 沿路距离（米）。
    pub distance_m: u32,
    /// 预计耗时（秒）。
    pub duration_s: u32,
    /// 折线，`lat`/`lng` 独立字段。
    pub polyline: Vec<LatLng>,
    pub source: RouteSource,
}

impl Route {
    /// 平均速度（m/s）。用来跟档位限速做对比。
    pub fn avg_speed_mps(&self) -> f32 {
        if self.duration_s == 0 {
            return 0.0;
        }
        self.distance_m as f32 / self.duration_s as f32
    }

    pub fn is_degraded(&self) -> bool {
        self.source == RouteSource::Fallback
    }
}

/// 缓存键。**照抄设计文档**：5 位小数的四元组，顺序即方向。
///
/// 顺序敏感是有意的：A→B 和 B→A 的折线方向相反，混用会让骑手倒着走。
pub fn route_cache_key(from: LatLng, to: LatLng) -> String {
    format!(
        "{:.5}_{:.5}_{:.5}_{:.5}",
        from.lat, from.lng, to.lat, to.lng
    )
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RouteStats {
    pub hits: u64,
    pub misses: u64,
    /// 真正打到高德的请求数。验收标准「日 API 调用 < 200 次」看这个。
    pub requests: u64,
    pub fallbacks: u64,
    pub cache_len: usize,
}

impl RouteStats {
    pub fn hit_rate(&self) -> f32 {
        let total = self.hits + self.misses;
        if total == 0 {
            return 0.0;
        }
        self.hits as f32 / total as f32
    }
}

/// 路径服务。**永不失败**：最差也会给一条降级直线。
pub struct RouteService {
    client: Option<AmapClient>,
    cache: LruCache<String, Route>,
    mode: RouteMode,
    /// 降级轨迹的抖动种子。固定住才能保证可复现。
    jitter_seed: u64,
    stats: RouteStats,
}

impl std::fmt::Debug for RouteService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouteService")
            .field("mode", &self.mode)
            .field("online", &self.client.is_some())
            .field("stats", &self.stats)
            .finish()
    }
}

impl RouteService {
    /// 纯本地模式（没配 Key）。所有路线都是降级直线，但依然会进缓存。
    pub fn offline() -> Self {
        Self::new(None, RouteMode::default())
    }

    pub fn new(client: Option<AmapClient>, mode: RouteMode) -> Self {
        Self {
            client,
            cache: LruCache::new(NonZeroUsize::new(CACHE_CAPACITY).expect("容量非零")),
            mode,
            jitter_seed: 0xA11CE,
            stats: RouteStats::default(),
        }
    }

    /// 没配 Key 就用本地模式。
    pub fn from_env() -> Self {
        Self::new(AmapClient::from_env(), RouteMode::default())
    }

    pub fn mode(&self) -> RouteMode {
        self.mode
    }

    pub fn is_online(&self) -> bool {
        self.client.is_some()
    }

    pub fn stats(&self) -> RouteStats {
        RouteStats {
            cache_len: self.cache.len(),
            ..self.stats
        }
    }

    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    /// 取一条路线。**这是唯一的对外入口，且不会返回 Err。**
    pub async fn route(&mut self, from: LatLng, to: LatLng) -> Route {
        let key = route_cache_key(from, to);
        if let Some(hit) = self.cache.get(&key) {
            self.stats.hits += 1;
            let mut r = hit.clone();
            r.source = RouteSource::Cache;
            return r;
        }
        self.stats.misses += 1;

        if let Some(client) = self.client.clone() {
            match self.fetch_amap(&client, from, to).await {
                Ok(route) => {
                    self.stats.requests += 1;
                    self.cache.put(key, route.clone());
                    return route;
                }
                Err(e) => {
                    self.stats.requests += 1;
                    // 降级是**预期内**的行为，不打 error：设计明确要求静默降级。
                    tracing::debug!(error = %e, "高德路径规划不可用，走直线降级");
                }
            }
        }

        self.stats.fallbacks += 1;
        let route = Self::fallback(from, to, self.mode, self.jitter_seed);
        // 降级结果也缓存：避免每单都去撞已经挂掉的接口。
        self.cache.put(key, route.clone());
        route
    }

    /// 只查缓存，不触发任何 IO。
    pub fn peek_cache(&mut self, from: LatLng, to: LatLng) -> Option<&Route> {
        self.cache.get(&route_cache_key(from, to))
    }

    async fn fetch_amap(
        &self,
        client: &AmapClient,
        from: LatLng,
        to: LatLng,
    ) -> Result<Route, AmapError> {
        let params = vec![
            ("origin".to_string(), from.amap_param()),
            ("destination".to_string(), to.amap_param()),
        ];
        let body = client
            .get(self.mode.path(), &params, QuotaKind::Route)
            .await?;
        parse_route_body(&body, from, to).ok_or_else(|| {
            AmapError::Decode("路径规划响应里找不到 route.paths / data.paths".to_string())
        })
    }

    /// 降级轨迹：直线 + 抖动，距离按路网系数放大。
    pub fn fallback(from: LatLng, to: LatLng, mode: RouteMode, seed: u64) -> Route {
        let straight = haversine_m(from, to) * ROAD_FACTOR;
        let distance_m = (straight.round() as u32).max(1);
        let speed = mode.default_speed_mps().max(0.5);
        let duration_s = ((distance_m as f32 / speed).round() as u32).max(1);
        Route {
            from,
            to,
            distance_m,
            duration_s,
            polyline: straight_line_with_jitter(from, to, seed, FALLBACK_SAMPLES),
            source: RouteSource::Fallback,
        }
    }
}

/// 解析高德的路径响应。
///
/// v3（步行）放在 `route.paths[0]`，v4（骑行）放在 `data.paths[0]`，
/// 两个都试一遍，省得为出行方式分叉两套解析代码。
pub fn parse_route_body(body: &Value, from: LatLng, to: LatLng) -> Option<Route> {
    let paths = body
        .get("route")
        .and_then(|r| r.get("paths"))
        .or_else(|| body.get("data").and_then(|r| r.get("paths")))
        .and_then(|p| p.as_array())?;
    let first = paths.first()?;

    let distance_m = value_to_i64(first.get("distance")).unwrap_or(0).max(0) as u32;
    let duration_s = value_to_i64(first.get("duration")).unwrap_or(0).max(0) as u32;

    let mut polyline: Vec<LatLng> = Vec::new();
    if let Some(steps) = first.get("steps").and_then(|s| s.as_array()) {
        for step in steps {
            if let Some(pl) = step.get("polyline").and_then(|p| p.as_str()) {
                let pts = decode_amap_polyline(pl);
                // 相邻 step 的首尾点会重复，跳过首点避免折线出现零长段
                let start = if polyline.is_empty() {
                    0
                } else {
                    1.min(pts.len())
                };
                polyline.extend_from_slice(&pts[start..]);
            }
        }
    }
    if polyline.len() < 2 {
        // 高德给了距离但没给折线，至少把首尾连上
        polyline = vec![from, to];
    }

    // 高德的 distance 偶尔为 0（极端近距离），兜个底避免下游除零。
    let distance_m = if distance_m == 0 {
        (polyline_len_m(&polyline).round() as u32).max(1)
    } else {
        distance_m
    };
    let duration_s = if duration_s == 0 {
        ((distance_m as f32 / 1.4).round() as u32).max(1)
    } else {
        duration_s
    };

    Some(Route {
        from,
        to,
        distance_m,
        duration_s,
        polyline,
        source: RouteSource::Amap,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const A_LAT: f64 = 30.5077;
    const A_LNG: f64 = 114.4154;
    const B_LAT: f64 = 30.5127;
    const B_LNG: f64 = 114.4254;

    fn a() -> LatLng {
        LatLng::new(A_LAT, A_LNG)
    }
    fn b() -> LatLng {
        LatLng::new(B_LAT, B_LNG)
    }

    #[test]
    fn cache_key_is_directional_and_5_decimals() {
        let k1 = route_cache_key(a(), b());
        let k2 = route_cache_key(b(), a());
        assert_eq!(k1, "30.50770_114.41540_30.51270_114.42540");
        assert_ne!(k1, k2, "A→B 与 B→A 必须是两个键，否则折线会反向");
    }

    #[test]
    fn parses_v3_walking_response() {
        let body = serde_json::json!({
            "status": "1",
            "info": "OK",
            "route": {
                "paths": [{
                    "distance": "1523",
                    "duration": "1094",
                    "steps": [
                        { "polyline": "114.41540,30.50770;114.42000,30.51000" },
                        { "polyline": "114.42000,30.51000;114.42540,30.51270" }
                    ]
                }]
            }
        });
        let r = parse_route_body(&body, a(), b()).expect("应该解析成功");
        assert_eq!(r.distance_m, 1523);
        assert_eq!(r.duration_s, 1094);
        assert_eq!(r.polyline.first(), Some(&a()));
        assert_eq!(r.polyline.last(), Some(&b()));
        // 重复的接缝点被去掉了：2+2-1 = 3
        assert_eq!(r.polyline.len(), 3);
        assert_eq!(r.source, RouteSource::Amap);
    }

    #[test]
    fn parses_v4_bicycling_response() {
        let body = serde_json::json!({
            "status": "1",
            "data": {
                "paths": [{
                    "distance": 900,
                    "duration": 240,
                    "steps": [{ "polyline": "114.41540,30.50770;114.42540,30.51270" }]
                }]
            }
        });
        let r = parse_route_body(&body, a(), b()).expect("v4 也该能解析");
        assert_eq!(r.distance_m, 900);
        assert_eq!(r.duration_s, 240);
    }

    #[test]
    fn empty_paths_is_none_not_panic() {
        let body = serde_json::json!({ "status": "1", "route": { "paths": [] } });
        assert!(parse_route_body(&body, a(), b()).is_none());
        let body2 = serde_json::json!({ "status": "1" });
        assert!(parse_route_body(&body2, a(), b()).is_none());
    }

    #[test]
    fn missing_polyline_still_connects_endpoints() {
        let body = serde_json::json!({
            "status": "1",
            "route": { "paths": [{ "distance": "1000", "duration": "600" }] }
        });
        let r = parse_route_body(&body, a(), b()).unwrap();
        assert_eq!(r.polyline, vec![a(), b()]);
    }

    #[test]
    fn fallback_is_deterministic_and_longer_than_straight_line() {
        let f1 = RouteService::fallback(a(), b(), RouteMode::Bicycling, 7);
        let f2 = RouteService::fallback(a(), b(), RouteMode::Bicycling, 7);
        assert_eq!(f1, f2, "降级也必须可复现");
        assert!(f1.is_degraded());
        let straight = haversine_m(a(), b()) as u32;
        assert!(f1.distance_m > straight, "路网系数应该让它比直线长");
        assert_eq!(f1.polyline.first(), Some(&a()));
        assert_eq!(f1.polyline.last(), Some(&b()));
    }

    #[tokio::test]
    async fn offline_service_still_answers_and_caches() {
        let mut svc = RouteService::offline();
        let first = svc.route(a(), b()).await;
        assert!(first.is_degraded());
        let second = svc.route(a(), b()).await;
        assert_eq!(second.source, RouteSource::Cache);
        assert_eq!(second.distance_m, first.distance_m);

        let s = svc.stats();
        assert_eq!(s.misses, 1);
        assert_eq!(s.hits, 1);
        assert_eq!(s.requests, 0, "没有客户端就不该有请求");
        assert_eq!(s.hit_rate(), 0.5);
        assert_eq!(s.cache_len, 1);
    }

    #[tokio::test]
    async fn repeated_pairs_hit_the_cache() {
        // 模拟「骑手在几个商圈里打转」：只有 4 个不同的 AB 点。
        let pts = [
            LatLng::new(30.50, 114.40),
            LatLng::new(30.51, 114.41),
            LatLng::new(30.52, 114.42),
            LatLng::new(30.53, 114.43),
        ];
        let mut svc = RouteService::offline();
        for _ in 0..100 {
            for i in 0..pts.len() {
                let to = pts[(i + 1) % pts.len()];
                svc.route(pts[i], to).await;
            }
        }
        let s = svc.stats();
        assert!(s.hit_rate() > 0.8, "命中率应 > 80%，实际 {}", s.hit_rate());
        assert_eq!(s.cache_len, pts.len());
    }

    #[test]
    fn cache_is_bounded() {
        // 容量 2000，塞 2500 个不同的点不应该无界增长。
        let mut cache: LruCache<String, Route> =
            LruCache::new(NonZeroUsize::new(CACHE_CAPACITY).unwrap());
        for i in 0..2_500u32 {
            let from = LatLng::new(30.0 + i as f64 * 0.001, 114.0);
            let to = LatLng::new(30.0, 114.0 + i as f64 * 0.001);
            cache.put(
                route_cache_key(from, to),
                RouteService::fallback(from, to, RouteMode::Walking, i as u64),
            );
        }
        assert_eq!(cache.len(), CACHE_CAPACITY);
    }
}
