//! 天气：速度系数、事故率加成、雨补。数值见 玩法设计 §2.1。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum Weather {
    #[default]
    Clear,
    Cloudy,
    Overcast,
    Fog,
    LightRain,
    ModerateRain,
    HeavyRain,
    Windy,
    Snow,
    Rainstorm,
}

impl Weather {
    /// 全部天气，按控制端按钮顺序排列。
    pub const ALL: [Weather; 10] = [
        Weather::Clear,
        Weather::Cloudy,
        Weather::Overcast,
        Weather::Fog,
        Weather::LightRain,
        Weather::ModerateRain,
        Weather::HeavyRain,
        Weather::Windy,
        Weather::Snow,
        Weather::Rainstorm,
    ];

    /// 速度系数。
    pub fn speed_factor(self) -> f32 {
        match self {
            Weather::Clear | Weather::Cloudy => 1.00,
            Weather::Overcast => 0.98,
            Weather::Fog => 0.70,
            Weather::LightRain => 0.85,
            Weather::ModerateRain => 0.75,
            Weather::HeavyRain => 0.62,
            Weather::Windy => 0.78,
            Weather::Snow => 0.55,
            Weather::Rainstorm => 0.45,
        }
    }

    /// 事故率加成。
    pub fn accident_multiplier(self) -> f32 {
        match self {
            Weather::Clear | Weather::Cloudy => 1.0,
            Weather::Overcast => 1.1,
            Weather::Fog => 2.5,
            Weather::LightRain => 1.5,
            Weather::ModerateRain => 2.2,
            Weather::HeavyRain => 3.5,
            Weather::Windy => 2.0,
            Weather::Snow => 4.0,
            Weather::Rainstorm => 6.0,
        }
    }

    /// 雨补（分）。
    pub fn rain_bonus_cents(self) -> i64 {
        match self {
            Weather::Clear | Weather::Cloudy | Weather::Overcast => 0,
            Weather::Fog => 100,
            Weather::LightRain => 80,
            Weather::ModerateRain => 150,
            Weather::HeavyRain => 250,
            Weather::Windy => 120,
            Weather::Snow => 300,
            Weather::Rainstorm => 400,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Weather::Clear => "clear",
            Weather::Cloudy => "cloudy",
            Weather::Overcast => "overcast",
            Weather::Fog => "fog",
            Weather::LightRain => "light_rain",
            Weather::ModerateRain => "moderate_rain",
            Weather::HeavyRain => "heavy_rain",
            Weather::Windy => "windy",
            Weather::Snow => "snow",
            Weather::Rainstorm => "rainstorm",
        }
    }

    /// 协议里的天气短码（省 token）。LLM 只需要区分「晴天 / 下雨 / 暴雨 / 雪」。
    pub fn code(self) -> &'static str {
        match self {
            Weather::Clear | Weather::Cloudy => "c",
            Weather::Overcast => "o",
            Weather::Fog => "f",
            Weather::LightRain | Weather::ModerateRain => "r",
            Weather::HeavyRain | Weather::Rainstorm => "hr",
            Weather::Windy => "w",
            Weather::Snow => "s",
        }
    }

    /// 短码 / 全名 → 天气。认不出来返回 `None`（烽火台 AI 的非法天气一律丢弃）。
    ///
    /// 短码优先（省 token 的协议用它）；同时容忍 `as_str` 的全名，方便人手写。
    pub fn from_code(s: &str) -> Option<Self> {
        let t = s.trim();
        Some(match t {
            "c" | "clear" | "cloudy" => Weather::Clear,
            "o" | "overcast" => Weather::Overcast,
            "f" | "fog" => Weather::Fog,
            "r" | "light_rain" => Weather::LightRain,
            "moderate_rain" => Weather::ModerateRain,
            "hr" | "heavy_rain" | "rainstorm" => Weather::HeavyRain,
            "w" | "windy" => Weather::Windy,
            "s" | "snow" => Weather::Snow,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Weather::Clear => "晴",
            Weather::Cloudy => "多云",
            Weather::Overcast => "阴",
            Weather::Fog => "雾",
            Weather::LightRain => "小雨",
            Weather::ModerateRain => "中雨",
            Weather::HeavyRain => "大雨",
            Weather::Windy => "大风",
            Weather::Snow => "雪",
            Weather::Rainstorm => "暴雨",
        }
    }

    /// 暴雨 + 晚高峰 = 极高峰，出餐 ×3。
    pub fn is_storm(self) -> bool {
        matches!(
            self,
            Weather::HeavyRain | Weather::Rainstorm | Weather::Snow
        )
    }
}

/// 天气剧本事件（data/weather_script.json）。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WeatherEvent {
    /// 生效时刻，模拟分钟（当日 0:00 起算）。
    pub at: u32,
    pub to: Weather,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WeatherScript {
    pub day: String,
    pub events: Vec<WeatherEvent>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_matches_design() {
        assert_eq!(Weather::Clear.speed_factor(), 1.00);
        assert_eq!(Weather::Rainstorm.speed_factor(), 0.45);
        assert_eq!(Weather::Snow.accident_multiplier(), 4.0);
        assert_eq!(Weather::Rainstorm.rain_bonus_cents(), 400);
        assert_eq!(Weather::Clear.rain_bonus_cents(), 0);
        // 速度随天气恶化单调不增
        for pair in Weather::ALL.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            assert!(a.speed_factor() >= b.speed_factor() || !b.is_storm());
        }
    }
}
