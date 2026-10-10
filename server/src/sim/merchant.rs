//! 商户数据。300~500 家虚构商户，启动时从 `data/merchants.json` 读入。
//!
//! **不实时调用高德的周边搜索**（个人认证配额只有 100 次/日，是瓶颈）。
//! 坐标为 `null` 的商户在启动时批量地理编码一次，结果落回 json 不再重复调用。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Merchant {
    pub id: u32,
    pub name: String,
    pub city: String,
    pub address: String,
    /// 高德地理编码结果，null 表示尚未解析。
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    /// 商家评分，影响单量与出餐时间。
    pub rating: f32,
    /// 平均出餐分钟数 5~25。
    pub avg_prep_min: u32,
    pub signature: String,
    /// 步行难度 0/1/2（路边店 / 商场内 / 高层深巷）。
    ///
    /// 属于**离线标注**：生成 `merchants.json` 时由 LLM 依据「店名 + 地址类型」
    /// 批量打标（《LLM协议设计》§5.1），运行期零开销、可复现。缺省 0 = 路边店。
    #[serde(default)]
    pub walk_difficulty: u8,
}

impl Merchant {
    pub fn coords(&self) -> Option<crate::sim::geo::LatLng> {
        match (self.lat, self.lng) {
            (Some(lat), Some(lng)) if lat != 0.0 || lng != 0.0 => {
                Some(crate::sim::geo::LatLng::new(lat, lng))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerchantFile {
    pub city: String,
    pub merchants: Vec<Merchant>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct City {
    pub name: String,
    pub center_lat: f64,
    pub center_lng: f64,
    pub districts: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coords_none_when_unresolved() {
        let m = Merchant {
            id: 1,
            name: "测试店".into(),
            city: "武汉".into(),
            address: "光谷".into(),
            lat: None,
            lng: None,
            rating: 4.7,
            avg_prep_min: 12,
            signature: "热干面".into(),
            walk_difficulty: 0,
        };
        assert!(m.coords().is_none());
    }
}
