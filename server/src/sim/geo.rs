//! 坐标与几何工具（纯函数）。
//!
//! 契约铁律：坐标永远是**独立字段** `lat` / `lng`，绝不用数组。
//! 高德用 `[lng, lat]`，GeoJSON 用 `[lat, lng]`，混用是最容易出的 bug。

use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

const EARTH_RADIUS_M: f64 = 6_371_000.0;

/// 经纬度（6 位小数）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LatLng {
    pub lat: f64,
    pub lng: f64,
}

impl LatLng {
    pub fn new(lat: f64, lng: f64) -> Self {
        Self {
            lat: round6(lat),
            lng: round6(lng),
        }
    }

    /// 高德 REST 的 `lng,lat` 形式。
    pub fn amap_param(&self) -> String {
        format!("{:.6},{:.6}", self.lng, self.lat)
    }

    /// 解析高德返回的 `lng,lat`。
    pub fn parse_amap(s: &str) -> Option<Self> {
        let (lng, lat) = s.split_once(',')?;
        Some(Self::new(
            lat.trim().parse().ok()?,
            lng.trim().parse().ok()?,
        ))
    }

    pub fn is_valid(&self) -> bool {
        self.lat.abs() <= 90.0 && self.lng.abs() <= 180.0 && (self.lat != 0.0 || self.lng != 0.0)
    }
}

/// 保留 6 位小数（契约要求）。
pub fn round6(v: f64) -> f64 {
    (v * 1_000_000.0).round() / 1_000_000.0
}

/// 大圆距离，米。
pub fn haversine_m(a: LatLng, b: LatLng) -> f64 {
    let lat1 = a.lat.to_radians();
    let lat2 = b.lat.to_radians();
    let dlat = (b.lat - a.lat).to_radians();
    let dlng = (b.lng - a.lng).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlng / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.sqrt().asin()
}

/// 整数米，最少 1 米（避免 0 距离导致除零）。
pub fn haversine_m_u32(a: LatLng, b: LatLng) -> u32 {
    (haversine_m(a, b).round() as u32).max(1)
}

/// 方位角，0~360，正北为 0。
pub fn bearing_deg(from: LatLng, to: LatLng) -> f32 {
    let lat1 = from.lat.to_radians();
    let lat2 = to.lat.to_radians();
    let dlng = (to.lng - from.lng).to_radians();
    let y = dlng.sin() * lat2.cos();
    let x = lat1.cos() * lat2.sin() - lat1.sin() * lat2.cos() * dlng.cos();
    ((y.atan2(x).to_degrees() + 360.0) % 360.0) as f32
}

/// 两个方位角的夹角，0~180。
pub fn angle_between(b1: f32, b2: f32) -> f32 {
    let d = (b1 - b2).abs() % 360.0;
    if d > 180.0 { 360.0 - d } else { d }
}

/// 折线总长度，米。
pub fn polyline_len_m(polyline: &[LatLng]) -> f64 {
    polyline.windows(2).map(|w| haversine_m(w[0], w[1])).sum()
}

/// 在折线上按累计弧长取距离 `dist_m` 处的点。
pub fn point_at_distance(polyline: &[LatLng], dist_m: f64) -> LatLng {
    let Some(first) = polyline.first().copied() else {
        return LatLng::new(0.0, 0.0);
    };
    if polyline.len() == 1 || dist_m <= 0.0 {
        return first;
    }
    let mut acc = 0.0;
    for w in polyline.windows(2) {
        let seg = haversine_m(w[0], w[1]);
        if acc + seg >= dist_m {
            let t = if seg <= f64::EPSILON {
                0.0
            } else {
                (dist_m - acc) / seg
            };
            return LatLng::new(
                w[0].lat + (w[1].lat - w[0].lat) * t,
                w[0].lng + (w[1].lng - w[0].lng) * t,
            );
        }
        acc += seg;
    }
    *polyline.last().unwrap()
}

/// 按弧长等距采样出 `n` 个点（含首尾）。
pub fn sample_by_distance(polyline: &[LatLng], n: usize) -> Vec<LatLng> {
    let n = n.max(2);
    let total = polyline_len_m(polyline);
    if total <= f64::EPSILON {
        return vec![polyline.first().copied().unwrap_or(LatLng::new(0.0, 0.0)); n];
    }
    (0..n)
        .map(|i| point_at_distance(polyline, total * i as f64 / (n - 1) as f64))
        .collect()
}

/// 解析高德 polyline 字符串 `lng,lat;lng,lat;...`。
pub fn decode_amap_polyline(s: &str) -> Vec<LatLng> {
    s.split(';')
        .filter_map(|p| LatLng::parse_amap(p.trim()))
        .collect()
}

/// 降级轨迹：直线 + 垂直偏移 3~8m + 抖动。
///
/// **降级轨迹必须看起来像在走**，完美直线一眼假。
///
/// 两个端点（`i == 0` 和 `i == n-1`）严格落在 `from` / `to` 上，
/// 不加任何偏移：轨迹的起点就是骑手当前位置，终点就是商家/顾客门口，
/// 否则客户端上骑手会凭空闪现一小段。
pub fn straight_line_with_jitter(
    from: LatLng,
    to: LatLng,
    seed: u64,
    samples: usize,
) -> Vec<LatLng> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let n = samples.clamp(2, 256);
    let (dlat, dlng) = (
        (to.lat - from.lat) / (n - 1) as f64,
        (to.lng - from.lng) / (n - 1) as f64,
    );
    // 垂直于前进方向的单位向量
    let norm = (dlat * dlat + dlng * dlng).sqrt().max(1e-12);
    let (ny, nx) = (-dlng / norm, dlat / norm);
    // 1 度纬度 ≈ 111_320 m
    const M_PER_DEG: f64 = 111_320.0;

    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64 / (n - 1) as f64;
        let base_lat = from.lat + (to.lat - from.lat) * t;
        let base_lng = from.lng + (to.lng - from.lng) * t;

        let is_endpoint = i == 0 || i == n - 1;
        let (off, jitter) = if is_endpoint {
            (0.0, 0.0)
        } else {
            let amp = rng.random_range(3.0..8.0);
            let sign = if rng.random_bool(0.5) { 1.0 } else { -1.0 };
            (
                amp * sign / M_PER_DEG,
                rng.random_range(-0.3f64..0.3) / M_PER_DEG,
            )
        };
        out.push(LatLng::new(
            base_lat + ny * off + jitter,
            base_lng + nx * off + jitter,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haversine_matches_known_distance() {
        // 武汉光谷 -> 汉口江滩，约 17km
        let a = LatLng::new(30.5077, 114.4154);
        let b = LatLng::new(30.6124, 114.3024);
        let d = haversine_m(a, b);
        assert!((14_000.0..19_000.0).contains(&d), "d={d}");
    }

    #[test]
    fn parse_amap_order_is_lng_lat() {
        let p = LatLng::parse_amap("114.415400,30.507700").unwrap();
        assert_eq!(p.lat, 30.5077);
        assert_eq!(p.lng, 114.4154);
    }

    #[test]
    fn polyline_and_sample() {
        let line = vec![LatLng::new(30.0, 114.0), LatLng::new(30.0, 114.01)];
        let len = polyline_len_m(&line);
        assert!(len > 900.0 && len < 1100.0);
        let pts = sample_by_distance(&line, 5);
        assert_eq!(pts.len(), 5);
        assert_eq!(pts[0], line[0]);
        assert_eq!(pts[4], line[1]);
    }

    #[test]
    fn jitter_is_deterministic() {
        let a = LatLng::new(30.0, 114.0);
        let b = LatLng::new(30.01, 114.01);
        let p1 = straight_line_with_jitter(a, b, 7, 32);
        let p2 = straight_line_with_jitter(a, b, 7, 32);
        assert_eq!(p1, p2);
        assert_eq!(p1.len(), 32);
        // 端点必须严格落在 AB 上，否则客户端上骑手会闪现
        assert_eq!(p1.first(), Some(&a));
        assert_eq!(p1.last(), Some(&b));
        // 中间点不允许是完美直线
        let straight = sample_by_distance(&[a, b], 32);
        assert!((1..31).any(|i| p1[i] != straight[i]), "中间点应有偏移");
        // 偏移量应该在 3~8m 这个量级，不能飘到几十米外
        for i in 1..31 {
            let drift = haversine_m(p1[i], straight[i]);
            assert!(drift <= 10.0, "第 {i} 点偏移了 {drift:.2}m，太大了");
        }
    }

    #[test]
    fn angle_between_wraps() {
        assert!((angle_between(350.0, 10.0) - 20.0).abs() < 1e-3);
        assert!((angle_between(0.0, 180.0) - 180.0).abs() < 1e-3);
    }
}
