//! OpenAI 兼容的 Chat Completions 客户端。
//!
//! 只实现一个能力：**要一段 JSON 回来**。因为整个系统里 LLM 的所有用途
//! （战术分配、任务判定、店名生成）都是「结构化判断，代码执行」，
//! 没有一处需要自由文本。
//!
//! 兼容 `gpt-4o-mini`（玩法设计 ADR #5）以及任何实现了
//! `POST {base}/chat/completions` 的服务（DeepSeek / Moonshot / vLLM / Ollama）。

use std::time::Duration;

use serde_json::{Value, json};

/// 默认 3 秒超时。**这是玩法设计的硬要求**：超时就放弃本轮，用算法兜底。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);

pub const ENV_BASE_URL: &str = "LLM_BASE_URL";
pub const ENV_API_KEY: &str = "LLM_API_KEY";
pub const ENV_MODEL: &str = "LLM_MODEL";
/// 也认这个变量名，方便直接复用现成的 OpenAI 环境。
pub const ENV_OPENAI_KEY: &str = "OPENAI_API_KEY";

/// 分层模型：骑手层高频、要便宜；烽火台层低频、可以贵一点。
pub const ENV_MODEL_RIDER: &str = "LLM_MODEL_RIDER";
pub const ENV_MODEL_STATION: &str = "LLM_MODEL_STATION";
pub const ENV_MODEL_DIRECTOR: &str = "LLM_MODEL_DIRECTOR";

#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub timeout: Duration,
    /// 决策类任务一律 0 温度：同样的世界状态应该给同样的计划。
    pub temperature: f32,
    pub max_tokens: u32,
    /// 要求服务端返回严格 JSON（OpenAI / DeepSeek 都支持）。
    pub json_mode: bool,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: String::new(),
            model: "gpt-4o-mini".to_string(),
            timeout: DEFAULT_TIMEOUT,
            temperature: 0.0,
            max_tokens: 800,
            json_mode: true,
        }
    }
}

impl LlmConfig {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            ..Self::default()
        }
    }

    /// 从环境变量读配置。
    pub fn from_env() -> Self {
        let d = Self::default();
        let api_key = std::env::var(ENV_API_KEY)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var(ENV_OPENAI_KEY).ok())
            .unwrap_or_default();
        Self {
            base_url: std::env::var(ENV_BASE_URL)
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(d.base_url),
            api_key,
            model: std::env::var(ENV_MODEL)
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(d.model),
            ..d
        }
    }

    pub fn has_key(&self) -> bool {
        !self.api_key.trim().is_empty()
    }

    /// 换一个模型名，其余照旧。用于按层挑模型。
    pub fn with_model(&self, model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..self.clone()
        }
    }

    /// 这一层实际用的模型名：有分层配置就用它，否则用全局模型。
    pub fn model_for(&self, models: &LayerModels, layer: LayerKey) -> String {
        models
            .get(layer)
            .map(str::to_string)
            .unwrap_or_else(|| self.model.clone())
    }

    /// 取一份「这一层专用」的配置。成本优化的全部实现就是这一行。
    pub fn for_layer(&self, models: &LayerModels, layer: LayerKey) -> Self {
        let model = self.model_for(models, layer);
        self.with_model(model)
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }
}

/// 三层 AI 的名字。**故意不依赖 `AiLayer`**：配置层不该认识调度层，
/// 否则 client → orchestrator 会绕出一个环。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerKey {
    Rider,
    Station,
    Director,
}

impl LayerKey {
    pub fn key(self) -> &'static str {
        match self {
            LayerKey::Rider => "rider",
            LayerKey::Station => "station",
            LayerKey::Director => "director",
        }
    }

    pub fn from_key(s: &str) -> Option<Self> {
        Some(match s {
            "rider" => LayerKey::Rider,
            "station" => LayerKey::Station,
            "director" => LayerKey::Director,
            _ => return None,
        })
    }

    pub const ALL: [LayerKey; 3] = [LayerKey::Rider, LayerKey::Station, LayerKey::Director];
}

/// 分层模型配置。全空 = 三层都用全局模型（默认行为，零配置）。
#[derive(Debug, Clone, Default)]
pub struct LayerModels {
    rider: Option<String>,
    station: Option<String>,
    director: Option<String>,
}

impl LayerModels {
    pub fn from_env() -> Self {
        let read = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        Self {
            rider: read(ENV_MODEL_RIDER),
            station: read(ENV_MODEL_STATION),
            director: read(ENV_MODEL_DIRECTOR),
        }
    }

    pub fn get(&self, layer: LayerKey) -> Option<&str> {
        match layer {
            LayerKey::Rider => self.rider.as_deref(),
            LayerKey::Station => self.station.as_deref(),
            LayerKey::Director => self.director.as_deref(),
        }
    }

    pub fn set(&mut self, layer: LayerKey, model: Option<String>) {
        let v = model
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        match layer {
            LayerKey::Rider => self.rider = v,
            LayerKey::Station => self.station = v,
            LayerKey::Director => self.director = v,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rider.is_none() && self.station.is_none() && self.director.is_none()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// 没配 Key。**这意味着「LLM 功能未启用」，不是故障。**
    #[error("未配置 LLM Key（环境变量 {ENV_API_KEY}），本功能已跳过")]
    KeyMissing,

    #[error("LLM 请求超时（>{0:?}），放弃本轮并回退到算法")]
    Timeout(Duration),

    #[error("LLM 请求失败：{0}")]
    Http(#[from] reqwest::Error),

    #[error("LLM 返回 HTTP {status}：{body}")]
    Status { status: u16, body: String },

    #[error("LLM 响应里没有 choices[0].message.content")]
    Empty,

    #[error("LLM 输出不是合法 JSON：{detail}；原文：{raw}")]
    BadJson { raw: String, detail: String },

    #[error("LLM 输出缺少字段：{0}")]
    MissingField(&'static str),
}

impl LlmError {
    /// 能不能靠算法兜底。除「缺 Key」外都可以。
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            LlmError::Timeout(_) | LlmError::Http(_) | LlmError::Status { .. }
        )
    }
}

/// LLM 客户端。可克隆、可共享。
#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    cfg: LlmConfig,
}

impl std::fmt::Debug for LlmClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 绝不打印 api_key
        f.debug_struct("LlmClient")
            .field("base_url", &self.cfg.base_url)
            .field("model", &self.cfg.model)
            .field("has_key", &self.cfg.has_key())
            .finish()
    }
}

impl LlmClient {
    /// 没有 Key 就返回 `None`——调用方退回算法，不报错。
    pub fn from_env() -> Option<Self> {
        let cfg = LlmConfig::from_env();
        if cfg.has_key() {
            LlmClient::new(cfg).ok()
        } else {
            None
        }
    }

    pub fn new(cfg: LlmConfig) -> Result<Self, LlmError> {
        if !cfg.has_key() {
            return Err(LlmError::KeyMissing);
        }
        let http = reqwest::Client::builder()
            .timeout(cfg.timeout)
            .user_agent(concat!("rider-sim/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { http, cfg })
    }

    pub fn config(&self) -> &LlmConfig {
        &self.cfg
    }

    pub fn model(&self) -> &str {
        &self.cfg.model
    }

    /// 要一段 JSON。`system` 必须是「只输出 JSON」的指令。
    pub async fn complete_json(&self, system: &str, user: &str) -> Result<Value, LlmError> {
        let mut body = json!({
            "model": self.cfg.model,
            "temperature": self.cfg.temperature,
            "max_tokens": self.cfg.max_tokens,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user },
            ],
        });
        if self.cfg.json_mode {
            body["response_format"] = json!({ "type": "json_object" });
        }

        let resp = self
            .http
            .post(self.cfg.endpoint())
            .bearer_auth(&self.cfg.api_key)
            .json(&body)
            // 单独设一次超时：即使 Client 被复用，决策也不能等太久。
            .timeout(self.cfg.timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    LlmError::Timeout(self.cfg.timeout)
                } else {
                    LlmError::Http(e)
                }
            })?;

        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(LlmError::Status {
                status: status.as_u16(),
                body: truncate(&text, 300),
            });
        }

        let envelope: Value = serde_json::from_str(&text).map_err(|e| LlmError::BadJson {
            raw: truncate(&text, 300),
            detail: e.to_string(),
        })?;

        let content = envelope
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .ok_or(LlmError::Empty)?;

        extract_json(content).ok_or_else(|| LlmError::BadJson {
            raw: truncate(content, 300),
            detail: "无法从内容里提取出 JSON 对象".to_string(),
        })
    }
}

/// 从模型输出里抠出 JSON 对象。
///
/// 即使开了 `json_object` 模式，也有模型会多嘴几句或者套一层 ```json 围栏，
/// 所以这里做三级降级：直接解析 → 剥围栏 → 取第一个 `{` 到最后一个 `}`。
pub fn extract_json(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return Some(v);
    }

    let unfenced = strip_code_fence(trimmed);
    if let Ok(v) = serde_json::from_str::<Value>(&unfenced) {
        return Some(v);
    }

    let start = unfenced.find('{')?;
    let end = unfenced.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Value>(&unfenced[start..=end]).ok()
}

/// 去掉 ```json ... ``` 围栏。
fn strip_code_fence(text: &str) -> String {
    let t = text.trim();
    if !t.starts_with("```") {
        return t.to_string();
    }
    let mut body = &t[3..];
    // 跳过语言标记那一行
    if let Some(nl) = body.find('\n') {
        let lang = body[..nl].trim();
        if lang.chars().all(|c| c.is_ascii_alphanumeric()) {
            body = &body[nl + 1..];
        }
    }
    body.trim_end().trim_end_matches("```").trim().to_string()
}

fn truncate(s: &str, n: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= n {
        return t.to_string();
    }
    let cut: String = t.chars().take(n).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_models_fall_back_to_the_global_model() {
        let base = LlmConfig::new("k").with_model("gpt-4o-mini");
        let empty = LayerModels::default();
        assert!(empty.is_empty());
        for l in LayerKey::ALL {
            assert_eq!(base.for_layer(&empty, l).model, "gpt-4o-mini");
        }

        let mut models = LayerModels::default();
        models.set(LayerKey::Rider, Some("gpt-4o-mini".into()));
        models.set(LayerKey::Director, Some("gpt-4o".into()));
        assert_eq!(
            base.for_layer(&models, LayerKey::Rider).model,
            "gpt-4o-mini"
        );
        assert_eq!(base.for_layer(&models, LayerKey::Director).model, "gpt-4o");
        // 没配的层仍然用全局模型
        assert_eq!(
            base.for_layer(&models, LayerKey::Station).model,
            "gpt-4o-mini"
        );

        // 空白字符串等于没配
        let mut blank = LayerModels::default();
        blank.set(LayerKey::Rider, Some("   ".into()));
        assert!(blank.is_empty());
    }

    #[test]
    fn layer_keys_round_trip() {
        for l in LayerKey::ALL {
            assert_eq!(LayerKey::from_key(l.key()), Some(l));
        }
        assert_eq!(LayerKey::from_key("nope"), None);
    }

    #[test]
    fn missing_key_disables_the_feature_without_panicking() {
        let cfg = LlmConfig::new("   ");
        assert!(!cfg.has_key());
        assert!(matches!(
            LlmClient::new(cfg).unwrap_err(),
            LlmError::KeyMissing
        ));
    }

    #[test]
    fn endpoint_is_joined_without_double_slash() {
        let cfg = LlmConfig {
            base_url: "https://api.deepseek.com/v1/".into(),
            api_key: "x".into(),
            ..Default::default()
        };
        assert_eq!(
            cfg.endpoint(),
            "https://api.deepseek.com/v1/chat/completions"
        );
    }

    #[test]
    fn extracts_plain_json() {
        let v = extract_json(r#"{"assignments":[]}"#).unwrap();
        assert_eq!(v["assignments"], json!([]));
    }

    #[test]
    fn extracts_json_from_code_fence() {
        let text = "```json\n{\"assignments\":[{\"rider_id\":\"r1\",\"order_id\":42}]}\n```";
        let v = extract_json(text).unwrap();
        assert_eq!(v["assignments"][0]["rider_id"], "r1");
    }

    #[test]
    fn extracts_json_after_chatter() {
        let text = "好的，我的分配如下：\n{\"assignments\":[]}\n希望有帮助！";
        let v = extract_json(text).unwrap();
        assert_eq!(v["assignments"], json!([]));
    }

    #[test]
    fn rejects_non_json() {
        assert!(extract_json("我觉得今天不适合跑单").is_none());
        assert!(extract_json("").is_none());
    }

    #[test]
    fn timeout_is_transient_but_missing_key_is_not() {
        assert!(LlmError::Timeout(Duration::from_secs(3)).is_transient());
        assert!(!LlmError::KeyMissing.is_transient());
    }

    #[test]
    fn timeout_default_matches_the_design() {
        assert_eq!(LlmConfig::default().timeout, Duration::from_secs(3));
    }
}
