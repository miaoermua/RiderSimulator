//! OpenAI 兼容的 Chat Completions 客户端。
//!
//! 只实现一个能力：**要一段 JSON 回来**。因为整个系统里 LLM 的所有用途
//! （战术分配、任务判定、店名生成）都是「结构化判断，代码执行」，
//! 没有一处需要自由文本。
//!
//! 兼容 `gpt-4o-mini`（玩法设计 ADR #5）以及任何实现了
//! `POST {base}/chat/completions` 的服务（DeepSeek / Moonshot / vLLM / Ollama）。

use std::time::Duration;

use serde::Serialize;
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

    /// 序列化成控制台认的形状：`{"rider":"gpt-4o-mini","director":"gpt-4o"}`。
    ///
    /// 没配的层**不出现在对象里**——控制台据此区分「没提这层」和「这层回退全局」。
    pub fn to_json(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        for layer in LayerKey::ALL {
            if let Some(model) = self.get(layer) {
                map.insert(
                    layer.key().to_string(),
                    serde_json::Value::String(model.to_string()),
                );
            }
        }
        serde_json::Value::Object(map)
    }

    /// 从控制台/库里存过的对象读回。认不出的层名静默忽略，坏值当没配。
    pub fn from_json(value: &serde_json::Value) -> Self {
        let mut out = Self::default();
        let Some(obj) = value.as_object() else {
            return out;
        };
        for (key, raw) in obj {
            if let (Some(layer), Some(model)) = (LayerKey::from_key(key.trim()), raw.as_str()) {
                out.set(layer, Some(model.to_string()));
            }
        }
        out
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

/// 上游给出的一个可用模型。
///
/// 控制台要靠它给出「这一层用哪个模型」的下拉框——不然只能手打模型名，
/// 打错了要等到下一轮决策才能从错误里发现。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelInfo {
    /// 填进 `model` 字段的就是它。
    pub id: String,
    /// 归属方。OpenAI 兼容服务多半是组织名，本地服务通常不给。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owned_by: Option<String>,
    /// 创建时间戳（秒）。有些服务不给。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<u64>,
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
        Self::metadata_only(cfg)
    }

    /// 建一个「只问元数据」的客户端（列模型用）：**不因缺 Key 报错**。
    ///
    /// 本地推理服务（Ollama / vLLM / LM Studio）大多不校验 Key，
    /// 「能不能列出模型」跟「LLM 决策有没有启用」是两回事，别混在一起。
    pub fn metadata_only(cfg: LlmConfig) -> Result<Self, LlmError> {
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

    /// 上游 `GET {base}/models`：控制台的模型下拉靠它，免得手打模型名打错。
    ///
    /// 只读元数据、不产生 token 花费，所以缺 Key 也能问（见 [`LlmClient::metadata_only`]）。
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, LlmError> {
        let url = format!("{}/models", self.cfg.base_url.trim_end_matches('/'));
        let mut req = self.http.get(url).timeout(self.cfg.timeout);
        if self.cfg.has_key() {
            req = req.bearer_auth(&self.cfg.api_key);
        }

        let resp = req.send().await.map_err(|e| {
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
        Ok(parse_models(&envelope))
    }
}

/// 从 `/models` 的响应里抠出模型清单，按名字排序去重。
///
/// 兼容几种常见形状，因为「OpenAI 兼容」在实践中并不只有一种：
/// `{"data":[{"id":...}]}`（OpenAI / DeepSeek / vLLM）、
/// `{"models":["qwen2.5:7b"]}`（部分本地服务）、
/// `{"models":[{"name":...}]}`（少数服务把 id 叫 name）。
pub fn parse_models(envelope: &Value) -> Vec<ModelInfo> {
    let rows = envelope
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| envelope.get("models").and_then(Value::as_array));

    let mut out: Vec<ModelInfo> = rows
        .map(|rows| {
            rows.iter()
                .filter_map(|row| match row {
                    Value::String(s) => {
                        let id = s.trim();
                        if id.is_empty() {
                            None
                        } else {
                            Some(ModelInfo {
                                id: id.to_string(),
                                owned_by: None,
                                created: None,
                            })
                        }
                    }
                    Value::Object(o) => {
                        let id = ["id", "name", "model"]
                            .iter()
                            .find_map(|k| o.get(*k).and_then(Value::as_str))
                            .map(str::trim)
                            .filter(|s| !s.is_empty())?;
                        Some(ModelInfo {
                            id: id.to_string(),
                            owned_by: o
                                .get("owned_by")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            created: o.get("created").and_then(Value::as_u64),
                        })
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    out
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
    fn layer_models_round_trip_through_json() {
        let mut models = LayerModels::default();
        models.set(LayerKey::Rider, Some("gpt-4o-mini".into()));
        models.set(LayerKey::Director, Some("gpt-4o".into()));

        let json = models.to_json();
        // 没配的层不该出现在对象里 —— 控制台据此区分「没提」和「回退全局」。
        assert!(json.get("rider").is_some());
        assert!(json.get("director").is_some());
        assert!(json.get("station").is_none());

        let back = LayerModels::from_json(&json);
        assert_eq!(back.get(LayerKey::Rider), Some("gpt-4o-mini"));
        assert_eq!(back.get(LayerKey::Director), Some("gpt-4o"));
        assert_eq!(back.get(LayerKey::Station), None);
    }

    #[test]
    fn layer_models_from_json_ignores_junk() {
        // 不是对象、认不出的层名、非字符串的值，都不该 panic，也不该写进去。
        assert!(LayerModels::from_json(&serde_json::json!(null)).is_empty());
        assert!(LayerModels::from_json(&serde_json::json!([1, 2])).is_empty());

        let back = LayerModels::from_json(&serde_json::json!({
            "rider": "gpt-4o-mini",
            "director": "gpt-4o",
            "nope": "x",          // 认不出，忽略
            "station": 42,        // 类型不对，忽略
        }));
        assert_eq!(back.get(LayerKey::Rider), Some("gpt-4o-mini"));
        assert_eq!(back.get(LayerKey::Director), Some("gpt-4o"));
        assert_eq!(back.get(LayerKey::Station), None);
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

    // -----------------------------------------------------------------
    // HTTP 层：起一个假上游，验证真的发得出去、真的解析得回来
    // -----------------------------------------------------------------

    /// 起一个只回固定响应的假上游，返回它的 base_url（`/v1`）。
    ///
    /// 顺手把收到的请求体记下来，好断言「我们发出去的是不是想发的东西」。
    async fn stub_upstream(
        status: u16,
        body: &'static str,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<Value>>>) {
        use axum::{Router, routing::any};

        let seen: std::sync::Arc<std::sync::Mutex<Vec<Value>>> = Default::default();
        let sink = seen.clone();
        let app = Router::new().route(
            "/v1/{*rest}",
            any(move |req: axum::extract::Request| {
                let sink = sink.clone();
                async move {
                    let bytes = axum::body::to_bytes(req.into_body(), 64 * 1024)
                        .await
                        .unwrap_or_default();
                    if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
                        sink.lock().unwrap().push(v);
                    }
                    (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        [("content-type", "application/json")],
                        body,
                    )
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}/v1"), seen)
    }

    fn client_for_stmt(base_url: String) -> LlmClient {
        LlmClient::new(LlmConfig {
            base_url,
            api_key: "test-key".into(),
            timeout: Duration::from_secs(5),
            ..Default::default()
        })
        .expect("带 Key 的配置必须建得出来")
    }

    #[test]
    fn parses_the_common_models_shapes() {
        // OpenAI / DeepSeek / vLLM：data 数组 + 对象
        let v = json!({"data":[
            {"id":"gpt-4o","owned_by":"openai","created":1},
            {"id":"gpt-4o-mini"},
            {"id":"gpt-4o"},
            {"id":"   "}
        ]});
        let got: Vec<String> = parse_models(&v).into_iter().map(|m| m.id).collect();
        assert_eq!(
            got,
            vec!["gpt-4o", "gpt-4o-mini"],
            "要排序 + 去重 + 丢掉空名"
        );
        assert_eq!(parse_models(&v)[0].owned_by.as_deref(), Some("openai"));

        // 部分本地服务：models 数组 + 纯字符串
        let v = json!({"models":["qwen2.5:7b","llama3.1:8b"]});
        let got: Vec<String> = parse_models(&v).into_iter().map(|m| m.id).collect();
        assert_eq!(got, vec!["llama3.1:8b", "qwen2.5:7b"]);

        // 少数服务把 id 叫 name / model
        let v = json!({"models":[{"name":"m1"},{"model":"m2"}]});
        assert_eq!(parse_models(&v).len(), 2);

        // 什么都没给：空清单，不 panic
        assert!(parse_models(&json!({})).is_empty());
        assert!(parse_models(&json!({"data":"nope"})).is_empty());
    }

    #[tokio::test]
    async fn list_models_hits_the_upstream_and_parses_it() {
        let (base, _seen) = stub_upstream(
            200,
            r#"{"data":[{"id":"zeta"},{"id":"alpha","owned_by":"acme"}]}"#,
        )
        .await;
        let models = client_for_stmt(base).list_models().await.expect("该成功");
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "zeta"]
        );
        assert_eq!(models[0].owned_by.as_deref(), Some("acme"));
    }

    #[tokio::test]
    async fn list_models_surfaces_upstream_rejections() {
        let (base, _seen) = stub_upstream(401, r#"{"error":"bad key"}"#).await;
        let err = client_for_stmt(base).list_models().await.unwrap_err();
        match err {
            LlmError::Status { status, .. } => assert_eq!(status, 401),
            other => panic!("该报 Status，实际是 {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_models_reports_garbage_instead_of_panicking() {
        let (base, _seen) = stub_upstream(200, "<html>502 Bad Gateway</html>").await;
        let err = client_for_stmt(base).list_models().await.unwrap_err();
        assert!(matches!(err, LlmError::BadJson { .. }), "实际是 {err:?}");
    }

    /// 本地推理服务大多不校验 Key：**没有 Key 也要能列模型**。
    #[tokio::test]
    async fn listing_models_works_without_a_key() {
        let (base, _seen) = stub_upstream(200, r#"{"models":["qwen2.5:7b"]}"#).await;
        let cfg = LlmConfig {
            base_url: base,
            api_key: String::new(),
            ..Default::default()
        };
        assert!(
            matches!(LlmClient::new(cfg.clone()), Err(LlmError::KeyMissing)),
            "决策客户端仍然要求 Key——缺 Key 就是没启用"
        );
        let models = LlmClient::metadata_only(cfg)
            .expect("元数据客户端不该因为缺 Key 失败")
            .list_models()
            .await
            .unwrap();
        assert_eq!(models[0].id, "qwen2.5:7b");
    }

    /// `complete_json` 发出去的请求体得是我们要的形状（模型名 / 0 温度 / JSON 模式）。
    #[tokio::test]
    async fn complete_json_sends_the_configured_request() {
        let (base, seen) = stub_upstream(
            200,
            r#"{"choices":[{"message":{"content":"{\"assignments\":[]}"}}]}"#,
        )
        .await;
        let client = client_for_stmt(base);
        let v = client
            .complete_json("只输出 JSON", "派单")
            .await
            .expect("该成功");
        assert_eq!(v["assignments"], json!([]));

        let sent = seen.lock().unwrap().clone();
        assert_eq!(sent.len(), 1, "只该发一次");
        let body = &sent[0];
        assert_eq!(body["model"], json!(client.model()));
        assert_eq!(body["temperature"], json!(0.0), "决策必须 0 温度");
        assert_eq!(body["response_format"]["type"], json!("json_object"));
        assert_eq!(body["messages"][0]["content"], json!("只输出 JSON"));
    }

    /// 上游把 JSON 包在 ``` 围栏里也得能解析出来（真实模型经常这么干）。
    #[tokio::test]
    async fn complete_json_unwraps_a_fenced_answer() {
        let (base, _seen) = stub_upstream(
            200,
            r#"{"choices":[{"message":{"content":"```json\n{\"v\":1}\n```"}}]}"#,
        )
        .await;
        let v = client_for_stmt(base)
            .complete_json("s", "u")
            .await
            .expect("该成功");
        assert_eq!(v["v"], json!(1));
    }

    /// 上游返回 429 要给 `Status`（可回退），返回 200 却没有 content 要给 `Empty`。
    #[tokio::test]
    async fn complete_json_separates_transport_failures_from_empty_answers() {
        let (busy, _seen) = stub_upstream(429, r#"{"error":{"message":"rate limited"}}"#).await;
        let err = client_for_stmt(busy)
            .complete_json("s", "u")
            .await
            .unwrap_err();
        assert!(err.is_transient(), "限流该可回退，实际是 {err:?}");

        let (empty, _seen) = stub_upstream(200, r#"{"choices":[]}"#).await;
        let err = client_for_stmt(empty)
            .complete_json("s", "u")
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::Empty), "实际是 {err:?}");
        assert!(!err.is_transient(), "空回答不是网络问题");
    }
}
