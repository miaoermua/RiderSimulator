//! 地理编码 / 逆地理编码 / 距离测量。
//!
//! 用途见技术设计 §5.4：**虚拟商户的坐标不能手编**——虚构经纬度会落到海里。
//! 正确姿势是让 LLM 生成「店名 + 真实街道地址」，启动时批量转成坐标，
//! 结果写回 `merchants.json`，之后不再重复调用。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::client::{AmapClient, AmapError, value_to_i64};
use super::quota::QuotaKind;
use crate::sim::geo::LatLng;

/// 一个待转坐标的地点。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Place {
    pub name: String,
    /// 街道地址。**必须是真实存在的地址**，否则高德会返回空 `geocodes`。
    pub address: String,
    /// 城市（可选，但强烈建议给，能大幅提高命中率）。
    #[serde(default)]
    pub city: String,
}

/// 地理编码结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeocodeResult {
    /// 契约铁律：独立 `lat` / `lng` 字段，不用数组。
    pub lat: f64,
    pub lng: f64,
    pub formatted_address: String,
    /// 行政区划编码，用来判断有没有落到别的城市。
    pub adcode: String,
    pub city: String,
}

impl GeocodeResult {
    pub fn pos(&self) -> LatLng {
        LatLng::new(self.lat, self.lng)
    }

    /// 高德有时候会把地址解析到很远的地方（比如省会）。
    /// 超过 `max_km` 就当没解出来——宁可让商户退回城市中心。
    pub fn is_plausible(&self, near: LatLng, max_km: f64) -> bool {
        let d = crate::sim::geo::haversine_m(near, self.pos());
        d <= max_km * 1000.0
    }
}

/// 批量转换的结果。
#[derive(Debug, Default, Clone)]
pub struct BatchOutcome {
    pub resolved: Vec<(Place, GeocodeResult)>,
    /// 失败的地点 + 原因。**不中断整批**，剩下的继续转。
    pub failed: Vec<(Place, String)>,
    /// 真正发出去的请求数。
    pub requests: u32,
}

impl BatchOutcome {
    pub fn ok_count(&self) -> usize {
        self.resolved.len()
    }

    pub fn failed_count(&self) -> usize {
        self.failed.len()
    }
}

/// 地理编码服务。每次调用都会经过配额闸门。
#[derive(Clone, Debug)]
pub struct GeocodeService {
    client: AmapClient,
}

impl GeocodeService {
    pub fn new(client: AmapClient) -> Self {
        Self { client }
    }

    /// 从环境变量建。没有 Key 就返回 `None`。
    pub fn from_env() -> Option<Self> {
        AmapClient::from_env().map(Self::new)
    }

    pub fn client(&self) -> &AmapClient {
        &self.client
    }

    /// 地址 → 坐标。
    pub async fn geocode(&self, address: &str, city: &str) -> Result<GeocodeResult, AmapError> {
        let mut params = vec![("address".to_string(), address.to_string())];
        if !city.trim().is_empty() {
            params.push(("city".to_string(), city.to_string()));
        }
        let body = self
            .client
            .get("/v3/geocode/geo", &params, QuotaKind::Geocode)
            .await?;
        parse_geocode_body(&body).ok_or_else(|| {
            AmapError::Decode(format!(
                "地址「{address}」没有解析出坐标（高德返回空 geocodes）"
            ))
        })
    }

    /// 坐标 → 地址。
    pub async fn reverse(&self, at: LatLng) -> Result<GeocodeResult, AmapError> {
        let params = vec![("location".to_string(), at.amap_param())];
        let body = self
            .client
            .get("/v3/geocode/regeo", &params, QuotaKind::Geocode)
            .await?;
        parse_regeo_body(&body)
            .ok_or_else(|| AmapError::Decode("逆地理编码没有解析出地址".to_string()))
    }

    /// 距离测量。注意：**本地 `haversine` 是免费的**，
    /// 只有需要「沿路距离」且不想做路径规划时才用这个，省着点额度。
    pub async fn distance_m(&self, from: LatLng, to: LatLng) -> Result<u32, AmapError> {
        let params = vec![
            ("origins".to_string(), from.amap_param()),
            ("destination".to_string(), to.amap_param()),
            ("type".to_string(), "1".to_string()),
        ];
        let body = self
            .client
            .get("/v3/distance", &params, QuotaKind::Distance)
            .await?;
        let d = body
            .get("results")
            .and_then(|r| r.as_array())
            .and_then(|a| a.first())
            .and_then(|r| value_to_i64(r.get("distance")))
            .ok_or_else(|| AmapError::Decode("距离测量没有返回 results[0].distance".into()))?;
        Ok(d.max(0) as u32)
    }

    /// 批量把地点转成坐标。500 家商户约 500 次调用，一次跑完写回 `merchants.json`。
    ///
    /// **单个失败不影响整批**：某家店地址写得不对，不该拖死另外 499 家。
    pub async fn batch_geocode(&self, places: &[Place]) -> BatchOutcome {
        let mut out = BatchOutcome::default();
        for p in places {
            out.requests += 1;
            let query = if p.address.contains(&p.city) || p.city.is_empty() {
                p.address.clone()
            } else {
                format!("{}{}", p.city, p.address)
            };
            match self.geocode(&query, &p.city).await {
                Ok(r) => out.resolved.push((p.clone(), r)),
                Err(e) => out.failed.push((p.clone(), e.to_string())),
            }
        }
        out
    }
}

/// 解析 `/v3/geocode/geo` 的响应。
pub fn parse_geocode_body(body: &Value) -> Option<GeocodeResult> {
    let first = body.get("geocodes")?.as_array()?.first()?;
    let location = first.get("location").and_then(|v| v.as_str())?;
    let pos = LatLng::parse_amap(location)?;
    if !pos.is_valid() {
        return None;
    }
    Some(GeocodeResult {
        lat: pos.lat,
        lng: pos.lng,
        formatted_address: first
            .get("formatted_address")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        adcode: first
            .get("adcode")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        city: first
            .get("city")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

/// 解析 `/v3/geocode/regeo` 的响应。
pub fn parse_regeo_body(body: &Value) -> Option<GeocodeResult> {
    let re = body.get("regeocode")?;
    let comp = re.get("addressComponent");
    Some(GeocodeResult {
        // 逆地理编码的请求坐标就是答案本身；这里回填请求点由调用方负责，
        // 所以只信任高德回的地址信息。坐标用 `regeocode` 里没有的字段会变成 0，
        // 因此 gps 坐标取调用方传入的那个（见 `reverse_to`）。
        lat: 0.0,
        lng: 0.0,
        formatted_address: re
            .get("formatted_address")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        adcode: comp
            .and_then(|c| c.get("adcode"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        city: comp
            .and_then(|c| c.get("city"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

/// 逆地理时把请求坐标补回结果里（高德 `regeo` 不回传坐标）。
pub fn reverse_result_with_pos(mut r: GeocodeResult, at: LatLng) -> GeocodeResult {
    r.lat = at.lat;
    r.lng = at.lng;
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_geocode_response() {
        let body = serde_json::json!({
            "status": "1",
            "info": "OK",
            "geocodes": [{
                "formatted_address": "湖北省武汉市洪山区光谷广场",
                "province": "湖北省",
                "city": "武汉市",
                "adcode": "420111",
                "location": "114.415400,30.507700"
            }]
        });
        let r = parse_geocode_body(&body).expect("应该解析成功");
        assert_eq!(r.lat, 30.5077);
        assert_eq!(r.lng, 114.4154);
        assert_eq!(r.adcode, "420111");
        assert_eq!(r.city, "武汉市");
    }

    #[test]
    fn empty_geocodes_means_bad_address() {
        let body = serde_json::json!({ "status": "1", "info": "OK", "geocodes": [] });
        assert!(parse_geocode_body(&body).is_none());
    }

    #[test]
    fn parses_regeo_response() {
        let body = serde_json::json!({
            "status": "1",
            "regeocode": {
                "formatted_address": "湖北省武汉市洪山区珞喻路",
                "addressComponent": { "city": "武汉市", "adcode": "420111" }
            }
        });
        let r = parse_regeo_body(&body).expect("应该解析成功");
        assert_eq!(r.city, "武汉市");
        assert!(r.formatted_address.contains("珞喻路"));
    }

    #[test]
    fn implausible_result_is_rejected() {
        let wuhan = LatLng::new(30.5077, 114.4154);
        let r = GeocodeResult {
            lat: 39.9042,
            lng: 116.4074,
            formatted_address: "北京市".into(),
            adcode: "110000".into(),
            city: "北京市".into(),
        };
        assert!(
            !r.is_plausible(wuhan, 50.0),
            "北京离武汉 1000km+，应被判为不可信"
        );
        let near = GeocodeResult {
            lat: 30.51,
            lng: 114.42,
            ..r.clone()
        };
        assert!(near.is_plausible(wuhan, 5.0));
    }

    #[test]
    fn reverse_result_gets_request_position() {
        let at = LatLng::new(30.5077, 114.4154);
        let body = serde_json::json!({
            "status": "1",
            "regeocode": { "formatted_address": "光谷", "addressComponent": { "city": "武汉市" } }
        });
        let r = reverse_result_with_pos(parse_regeo_body(&body).unwrap(), at);
        assert_eq!(r.pos(), at);
    }
}
