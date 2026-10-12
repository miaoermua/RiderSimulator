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

/// 流式下的**总预算**：从发请求到收完 `[DONE]`，默认 12 秒。
///
/// 与 [`DEFAULT_TIMEOUT`] 的关系：3 秒是「非流式收一个短 JSON」的量级，
/// 直接套到流式上会把长回复砍掉一半——而且砍掉的位置是随机的，
/// 于是表现为**随机失败**，极难排查。所以流式必须给更大的独立预算。
///
/// 注意：这三层 AI 是**后台跑**的（两段式：占用 tick → 后台等 → 下个 tick 收），
/// 所以这里放宽不会阻塞模拟主循环。真正卡住 tick 的是编排器里的在途检查。
pub const DEFAULT_STREAM_TIMEOUT: Duration = Duration::from_secs(12);

/// 流式下的**首字节预算**：默认 3 秒。
///
/// 和总预算分工不同：它管的是「服务端到底有没有在干活」。
/// 队列拥堵、上游限流、模型没加载——这些都是开头就不动，
/// 等满12 秒纯属浪费（那一轮调度就白占了坑）。3 秒没动静就可以退回算法。
pub const DEFAULT_FIRST_TOKEN_TIMEOUT: Duration = Duration::from_secs(3);

pub const ENV_BASE_URL: &str = "LLM_BASE_URL";
pub const ENV_API_KEY: &str = "LLM_API_KEY";
pub const ENV_MODEL: &str = "LLM_MODEL";
/// 也认这个变量名，方便直接复用现成的 OpenAI 环境。
pub const ENV_OPENAI_KEY: &str = "OPENAI_API_KEY";

/// 分层模型：骑手层高频、要便宜；烽火台层低频、可以贵一点。
pub const ENV_MODEL_RIDER: &str = "LLM_MODEL_RIDER";
pub const ENV_MODEL_STATION: &str = "LLM_MODEL_STATION";
pub const ENV_MODEL_DIRECTOR: &str = "LLM_MODEL_DIRECTOR";

/// 开关 `response_format: {"type":"json_object"}`。默认 **on**。
///
/// **为什么需要开关**：这是请求体顶层的一个**协议参数**，不是对话内容——
/// 上游服务器先读它来决定解码方式（有的实现用约束解码 / grammar 强制语法），
/// 模型本身看不见它。而**第三方网关（New API / one-api / 各类中转）对它的支持
/// 参差不齐**，有的直接返回 400 `unsupported parameter`，有的静默忽略。
///
/// 万一遇到不支持：设 `LLM_JSON_MODE=0` 关掉即可。此时改由
/// [`crate::llm::protocol::system_prompt`] 里那句「只输出 JSON」兜底——
/// 那是**对话内容**，任何 OpenAI 兼容服务都认。代价是失去约束解码的保证，
/// 模型可能带上 ```json 围栏或前后废话，但那正是
/// [`crate::llm::protocol::validate_decision`] 存在的原因：解析失败就走算法兜底。
pub const ENV_JSON_MODE: &str = "LLM_JSON_MODE";

/// 开关 SSE 流式拉取。默认 **on**。
///
/// **关掉不影响结果，只影响传输方式**——两种模式最终都走
/// [`extract_json`] + [`crate::llm::protocol::validate_decision`]，
/// 所以 LLM 关不关流式产出的决策是同一套。
/// 保留这个开关是因为**部分小众网关不支持 `"stream": true`**（会回 400）。
pub const ENV_STREAM: &str = "LLM_STREAM";

/// 解析布尔环境变量。**只有明确表示「关」的值才关**，拼错的按开启处理——
/// 与 [`parse_json_mode`] 同一条原则：默认开着，别让一个没认出来的变量
/// 静默把它关掉。
fn parse_bool_lenient(raw: Option<String>) -> bool {
    match raw {
        None => true,
        Some(s) => !matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off" | ""
        ),
    }
}

/// 开关 `response_format: {"type":"json_object"}`。默认 **on**。
///
/// **为什么需要开关**：这是请求体顶层的一个**协议参数**，不是对话内容——
/// 上游服务器先读它来决定解码方式（有的实现用约束解码 / grammar 强制语法），
/// 模型本身看不见它。而**第三方网关（New API / one-api /各类中转）对它的支持
/// 参差不齐**，有的直接返回 400 `unsupported parameter`，有的静默忽略。
///
/// 万一遇到不支持：设 `LLM_JSON_MODE=0`关掉即可。此时改由
/// [`crate::llm::protocol::system_prompt`] 里那句「只输出 JSON」兜底——
/// 那是**对话内容**，任何 OpenAI 兼容服务都认。代价是失去约束解码的保证，
/// 模型可能带上 ```json 围栏或前后废话，但那正是 [`crate::llm::protocol::validate_decision`]
/// 存在的原因：解析失败就走算法兜底。
///
/// 取值：**只有明确表示"关"的值才关**（`0` / `false` / `no` / `off` / 空串）。
/// 其余一律按开启处理——包括拼错的值。
///
/// 方向必须是「拼错 → 开启」而不是「拼错 → 关闭」：默认行为本来就是开着，
/// 一个没认出来的环境变量不该反过来把它关掉（那会静默拿掉约束解码，
/// 表现为模型开始返回带 ```json 围栏的文本，排查起来莫名其妙）。
fn parse_json_mode(raw: Option<String>) -> bool {
    match raw {
        None => true,
        Some(s) => {
            let v = s.trim().to_ascii_lowercase();
            !matches!(v.as_str(), "0" | "false" | "no" | "off" | "")
        }
    }
}

/// 归一化 `base_url`：**只保留到 `/v1` 这一层根路径**。
///
/// `base_url` 的语义是「OpenAI 兼容 API 的根」，请求路径由本模块自己拼：
/// 聊天走 `{base}/chat/completions`，列模型走 `{base}/models`。
/// 所以它**必须停在 `/v1`**，不能带后面的端点名。
///
/// 现实里最常见的误填是从文档里抄了完整地址，或者从别处复制了带路径的 URL：
///
/// ```text
/// 填 https://api.x.com/v1          → 对：.../v1/chat/completions
/// 填 https://api.x.com/v1/chat     → 错：.../v1/chat/chat/completions
///                                    列模型变成 .../v1/chat/models → 404
/// 填 https://api.x.com/v1/chat/completions → 错得更离谱
/// ```
///
/// 网关（New API / One API 这类）对这些路径非常敏感，会直接回
/// `404 Invalid URL (GET /v1/chat/models)`，而错误信息里那个 `chat`
/// 很容易让人以为"网关不支持"，实际上是**填错了根路径**。
///
/// 所以这里把已知的端点后缀剥掉，让用户填全地址也能用。
pub fn normalize_base_url(raw: &str) -> String {
    let s = raw.trim().trim_end_matches('/');
    // 按最长的后缀优先剥，否则 `/chat/completions` 会被先匹配掉 `/chat`。
    for suffix in ["/chat/completions", "/completions", "/chat", "/models"] {
        if let Some(stripped) = s.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    s.to_string()
}

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
    /// 走 SSE 流式拉取（默认开启）。**关掉只影响传输方式，不影响结果。**
    pub stream: bool,
    /// 流式的**总预算**：从发请求到 `[DONE]`，整体最多等这么久。
    ///
    /// 必须**大于** [`Self::timeout`]。非流式时 3 秒足够收一个几百字节的 JSON；
    /// 流式下模型要一个 token 一个 token 地吐，长回复 3 秒会被腰斩在中间——
    /// 于是每一条流式决策都会超时，等于把功能改坏了。
    pub stream_timeout: Duration,
    /// 流式的**首字节预算**：多久没等到第一个 token 就判超时。
    ///
    /// 单独拎出来是因为这两件事要分开看：
    /// 「服务端一直没动静」和「模型正在写、只是还没写完」是两种故障，
    /// 前者要快速失败（省下 3 秒给下一层），后者得给足时间。
    pub first_token_timeout: Duration,
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
            stream: true,
            stream_timeout: DEFAULT_STREAM_TIMEOUT,
            first_token_timeout: DEFAULT_FIRST_TOKEN_TIMEOUT,
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

    /// 本配置下**实际生效的总时长上限**（两种模式里更宽的那个）。
    ///
    /// 给 `reqwest::Client::builder().timeout()` 用。单个请求还会再设一次
    /// 更紧的（见 [`LlmClient::complete_json`]），但client 级必须是宽的那个，
    /// 否则会把请求级的覆盖掉。
    pub fn effective_timeout(&self) -> Duration {
        if self.stream {
            self.stream_timeout.max(self.timeout)
        } else {
            self.timeout
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
            base_url: normalize_base_url(
                std::env::var(ENV_BASE_URL)
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or(d.base_url)
                    .as_str(),
            ),
            api_key,
            model: std::env::var(ENV_MODEL)
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(d.model),
            json_mode: parse_json_mode(std::env::var(ENV_JSON_MODE).ok()),
            stream: parse_bool_lenient(std::env::var(ENV_STREAM).ok()),
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
        // **client 级超时取流式预算**（两种模式里更宽的那个），实际生效的
        // 上限交给每个请求自己设。
        //
        // 原因：reqwest 的 `Client::timeout()` 是**整个 client 的总时长上限**，
        // 一旦设小，它会盖掉请求级的 `.timeout()`。若这里写死 `cfg.timeout`
        // （3 秒），流式的请求级 12 秒就形同虚设——表现为「大模型一开流式就随机超时」，
        // 而且没有任何报错提示，只会安静地退回算法兜底。
        let http = reqwest::Client::builder()
            .timeout(cfg.effective_timeout())
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
    ///
    /// **流式与非流式只在「怎么把字节搬回来」上不同，出参完全一致**——
    /// 都会汇总成完整文本，再走 [`extract_json`] + 调用方的
    /// [`crate::llm::protocol::validate_decision`]。所以 `LLM_STREAM`
    /// 开着或关着，产出的决策是同一套；切换它不会改变行为，只影响延迟与
    /// 对长回复的容忍度。
    ///
    /// 换句话说：**这里不做「边流边决策」**。半个 JSON 解析不出任何东西，
    /// 提前落地只会把不完整的状态写进世界。
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
        if self.cfg.stream {
            body["stream"] = json!(true);
        }

        let budget = if self.cfg.stream {
            self.cfg.stream_timeout
        } else {
            self.cfg.timeout
        };

        let resp = self
            .http
            .post(self.cfg.endpoint())
            .bearer_auth(&self.cfg.api_key)
            .json(&body)
            // 单独设一次超时：即使 Client 被复用，决策也不能等太久。
            .timeout(budget)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    LlmError::Timeout(budget)
                } else {
                    LlmError::Http(e)
                }
            })?;

        let status = resp.status();

        if self.cfg.stream {
            // 流式：错误响应往往也是流式的（网关会把错误包在 SSE 里），
            // 所以先看 status，非 2xx 就把原文整段读出来再报错——
            // 和非流式保持同样的错误信息形态，调用方不用分叉。
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                return Err(LlmError::Status {
                    status: status.as_u16(),
                    body: truncate(&text, 300),
                });
            }
            let content = self.collect_sse(resp).await?;
            return extract_json(&content).ok_or_else(|| LlmError::BadJson {
                raw: truncate(&content, 300),
                detail: "无法从流式内容里提取出 JSON 对象".to_string(),
            });
        }

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

    /// 把 SSE 响应读干，汇总成完整文本。
    ///
    /// ## 协议形状
    ///
    /// ```text
    /// data: {"choices":[{"delta":{"content":"你"}}]}
    /// data: {"choices":[{"delta":{"content":"好"}}]}
    /// data: {"choices":[{"delta":{},"finish_reason":"stop"}]}
    /// data: [DONE]
    /// ```
    ///
    /// ## 为什么要读干而不是边读边用
    ///
    /// **半截 JSON 解析不出任何东西。** 决策要的是一份完整结构，
    /// 提前落地只会把残缺状态写进世界。所以这里只做「累积」，
    /// 真正的解析交给 [`extract_json`]，协议校验交给
    /// [`crate::llm::protocol::validate_decision`]——两者都不感知流式。
    ///
    /// 流式真正换来的好处是**能区分两种超时**（见
    /// [`LlmConfig::first_token_timeout`]）：「上游一直没动」和「模型还在写」
    /// 是两种故障，前者该快速退回算法，后者得给足时间。
    ///
    /// ## 容错
    ///
    /// - `data: [DONE]` → 立刻收尾，不再等连接关闭。
    /// - 单个分片 JSON 坏了 → **跳过继续**，不因一个畸形帧丢掉整个回复。
    /// - `reasoning_content`（部分模型的思维链）→ 忽略，只取 `content`。
    /// - 没收到 `[DONE]` 就断流 → 已收到的照样返回，让上层去校验。
    async fn collect_sse(&self, resp: reqwest::Response) -> Result<String, LlmError> {
        use futures::StreamExt;

        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();

        // ── 阶段一：等首字节 ──
        // 单独掐表，因为「上游一直没动」和「模型正在写」是两种故障：
        // 前者要快速退回算法（省下整个总预算给下一层），后者得给足时间。
        let first_budget = self.cfg.first_token_timeout;
        match tokio::time::timeout(first_budget, stream.next()).await {
            Ok(Some(Ok(chunk))) => buf.extend_from_slice(&chunk),
            // 流正常结束却一个字节都没有：上游返回了空 body。
            Ok(None) => return Err(LlmError::Empty),
            // 连接中途断了：已经收到的部分可能仍然可用，但作为「首字节」阶段
            // 它连一个字都没拿到，直接按 IO 错误报。
            Ok(Some(Err(e))) => return Err(LlmError::Http(e)),
            Err(_) => return Err(LlmError::Timeout(first_budget)),
        }

        // ── 阶段二：收剩余分片 ──
        // 首字节已经等到过了，给剩下的留足总预算。
        let total_budget = self.cfg.stream_timeout;
        let collect = async {
            while let Some(item) = stream.next().await {
                // 中途某个分片读失败（连接重置等）：**停止**，用已收到的继续。
                // 丢弃整个回复反而更糟——可能已经收全了。
                let Ok(chunk) = item else { break };
                buf.extend_from_slice(&chunk);
                // 收到 [DONE] 就收工：不少网关会保持连接不关，
                // 死等 body 结束只会白白耗到超时。
                if has_done_marker(&buf) {
                    break;
                }
            }
        };
        if tokio::time::timeout(total_budget, collect).await.is_err() {
            // 已经收了一部分，但没等到 [DONE]：**照样返回**。
            // 让上层去校验/丢弃，比直接报超时更有用——有时回复其实已经完整了，
            // 只是上游忘了发终止帧。
            tracing::debug!("SSE 未收到 [DONE]，用已收到的部分继续");
        }

        let text = String::from_utf8_lossy(&buf);
        let content = sse_content(&text);
        if content.trim().is_empty() {
            return Err(LlmError::Empty);
        }
        Ok(content)
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

/// SSE 缓冲里是否已经出现 `[DONE]` 终止标记。
///
/// 逐字节扫而不是 `String::from_utf8_lossy` 全文转换再`contains`：
/// 后者会在每个分片上分配一次完整字符串，长回复下浪费明显；
/// `[DONE]` 永远在 ASCII 区间，按字节找不会误判多字节字符。
fn has_done_marker(buf: &[u8]) -> bool {
    const MARKER: &[u8] = b"data: [DONE]";
    buf.windows(MARKER.len()).any(|w| w == MARKER)
}

/// 从 SSE 原始文本里抽出模型输出的正文。
///
/// ## SSE 分片形状
///
/// ```text
/// data: {"choices":[{"delta":{"content":"你好"}}]}
/// data: {"choices":[{"delta":{"content":",世界"}}]}
/// data: {"choices":[{"delta":{},"finish_reason":"stop"}]}
/// data: [DONE]
/// ```
///
/// 累加的是 `choices[0].delta.content`。几个必须处理的细节：
///
/// 1. **分片边界会切断 UTF-8 字符**。用 `String::from_utf8_lossy` 逐片转换会
///    把跨界的多字节字符变成 `��`。所以**先把整个缓冲区一次性转**，
///    再按行切——顺序反了就会出乱码。
/// 2. **忽略 `reasoning_content`**：部分模型（DeepSeek-R1 等）会先吐思维链，
///    那个字段不是最终答案，取了会污染 JSON。
/// 3. **跳过解析失败的分片**：一个畸形帧不该让整个回复作废。
/// 4. **兼容非流式的 `message.content`**：有的网关即使回了 SSE 也用
///    `message` 而不是 `delta`，顺手支持一下。
fn sse_content(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() / 2);
    for line in raw.lines() {
        let line = line.trim();
        // SSE 允许 `data:` 后有可选空格；也可能有 `event:` / `id:` 等字段，一律跳过。
        let Some(payload) = line.strip_prefix("data:") else {
            continue;
        };
        let payload = payload.trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(payload) else {
            continue;
        };
        let Some(choice) = v
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
        else {
            continue;
        };
        // 先试流式的 delta，再试非流式的 message —— 两种网关都覆盖。
        let piece = choice
            .get("delta")
            .and_then(|d| d.get("content"))
            .or_else(|| {
                choice
                    .get("message")
                    .and_then(|m| m.get("content"))
            })
            .and_then(|c| c.as_str());
        if let Some(s) = piece {
            out.push_str(s);
        }
    }
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
    fn json_mode_parses_leniently_and_defaults_to_on() {
        // 没设= 开启（保持原行为）。
        assert!(parse_json_mode(None));
        // 明确开。
        for v in ["1", "true", "TRUE", " yes ", "on"] {
            assert!(parse_json_mode(Some(v.into())), "输入 {v} 应为开启");
        }
        // 明确关。
        for v in ["0", "false", "no", "off", ""] {
            assert!(!parse_json_mode(Some(v.into())), "输入 {v} 应为关闭");
        }
        // 认不出来的值一律按「开」处理，绝不因为一个环境变量拼错就静默降级。
        for v in ["maybe", "2", "yep"] {
            assert!(parse_json_mode(Some(v.into())), "输入 {v} 应回落到开启");
        }
    }

    #[test]
    fn json_mode_off_still_leaves_the_system_prompt_asking_for_json() {
        // 关掉协议参数后，对话内容里那句「只输出 JSON」必须还在——
        // 否则模型就彻底没有格式约束了。
        assert!(crate::llm::protocol::system_prompt().contains("只输出 JSON"));
    }

    #[test]
    fn base_url_keeps_the_root_only() {
        // 正确写法：到 /v1 为止，原样返回。
        for good in [
            "https://api.openai.com/v1",
            "https://api.deepseek.com/v1/",
            "https://api.dahetao.org/v1",
        ] {
            assert_eq!(normalize_base_url(good), good.trim_end_matches('/'));
        }
    }

    #[test]
    fn base_url_drops_a_pasted_endpoint_suffix() {
        // 抄了完整端点也能救回来——这正是用户最常踩的坑：
        // 网关回 `404 Invalid URL (GET /v1/chat/models)`，看着像"不支持"，
        // 实际是根路径多填了 `/chat`。
        for (raw, want) in [
            (
                "https://api.dahetao.org/v1/chat",
                "https://api.dahetao.org/v1",
            ),
            (
                "https://api.dahetao.org/v1/chat/completions",
                "https://api.dahetao.org/v1",
            ),
            ("https://api.x.com/v1/models", "https://api.x.com/v1"),
            ("https://api.x.com/v1/completions", "https://api.x.com/v1"),
            // 结尾带斜杠也要认。
            ("https://api.x.com/v1/chat/", "https://api.x.com/v1"),
        ] {
            assert_eq!(normalize_base_url(raw), want, "输入 {raw}");
        }
    }

    #[test]
    fn base_url_keeps_paths_that_are_not_known_endpoints() {
        // 不要过度清洗：有些网关根路径里就带路径段（自建反代、多段前缀）。
        // 只剥「已知端点名」，其余原样保留。
        for keep in [
            "https://gw.corp/api/openai",
            "https://h.com/v1beta",
            "https://h.com/proxy/v1",
        ] {
            assert_eq!(normalize_base_url(keep), keep);
        }
    }

    #[test]
    fn normalized_base_url_yields_working_endpoints() {
        // 端到端：从用户填错的值出发，两条路径都要拼对。
        let cfg = LlmConfig {
            base_url: normalize_base_url("https://api.dahetao.org/v1/chat"),
            api_key: "x".into(),
            ..Default::default()
        };
        assert_eq!(cfg.base_url, "https://api.dahetao.org/v1");
        assert_eq!(
            cfg.endpoint(),
            "https://api.dahetao.org/v1/chat/completions"
        );
        assert_eq!(
            format!("{}/models", cfg.base_url.trim_end_matches('/')),
            "https://api.dahetao.org/v1/models"
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

    /// 同上，但**强制关掉流式**。
    ///
    /// 这两个必须分开：`stub_upstream` 返回的是一个**完整的 JSON 响应体**，
    /// 不是 SSE。而 `LlmConfig::default()` 现在 `stream = true`，
    /// 于是 `complete_json` 会走流式分支去找 `data:` 行——找不到，
    /// 报 `Empty`。看起来像"加了流式把原有功能改坏了"，其实只是桩没换形态。
    fn client_plain(base_url: String) -> LlmClient {
        LlmClient::new(LlmConfig {
            base_url,
            api_key: "test-key".into(),
            timeout: Duration::from_secs(5),
            stream: false,
            ..Default::default()
        })
        .expect("带 Key 的配置必须建得出来")
    }

    /// 起一个**真的 SSE 上游**：按分片吐出，最后发 `[DONE]`。
    ///
    /// 纯函数测过 `sse_content`，但「流式链路能不能把 JSON 完整拿回来」
    /// 还得跑一次真网络——`bytes_stream()` 的分片边界、axum 的 chunked 编码、
    /// `[DONE]` 之后是否真的关连接，这些模拟不出来。
    async fn stub_sse(frames: Vec<&'static str>, send_done: bool) -> String {
        use axum::{Router, http::header, routing::any};

        let app = Router::new().route(
            "/v1/{*rest}",
            any(move || {
                let frames = frames.clone();
                async move {
                    // 先把要发的分片攒成Vec，再一次性变成Stream。
                    // （不能直接对 `std::iter::Map` 调 `.chain()`——那不是 Stream。）
                    let mut chunks: Vec<Result<String, std::io::Error>> = frames
                        .into_iter()
                        .map(|f| Ok(format!("data: {f}\n\n")))
                        .collect();
                    if send_done {
                        chunks.push(Ok("data: [DONE]\n\n".to_string()));
                    }
                    (
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        axum::body::Body::from_stream(futures::stream::iter(chunks)),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}/v1")
    }

    /// 端到端：默认（流式开）能从 SSE 里拿到完整 JSON。
    #[tokio::test]
    async fn complete_json_reads_a_real_sse_stream() {
        let base = stub_sse(
            vec![
                r#"{"choices":[{"delta":{"content":"{\"assignments\":"}}]}"#,
                r#"{"choices":[{"delta":{"content":"[{\"rider_id\":1,\"score\":0.7}]}"}}]}"#,
                r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            ],
            true,
        )
        .await;

        // 注意：用默认配置（stream = true），确保走的是真流式分支。
        let client = client_for_stmt(base);
        let v = client
            .complete_json("只输出 JSON", "派单")
            .await
            .expect("SSE 应当被读成完整 JSON");
        assert_eq!(v["assignments"][0]["rider_id"], json!(1));
    }

    /// 上游忘了发 `[DONE]`（就断了）：**照样返回已收到的内容**，
    /// 交给上层校验，而不是直接报超时把整个回复丢掉。
    #[tokio::test]
    async fn complete_json_survives_a_stream_that_never_says_done() {
        let base = stub_sse(
            vec![r#"{"choices":[{"delta":{"content":"{\"ok\":1}"}}]}"#],
            false,
        )
        .await;
        let v = client_for_stmt(base)
            .complete_json("s", "u")
            .await
            .expect("缺 [DONE] 也该把已收到的交出去");
        assert_eq!(v["ok"], json!(1));
    }

    // ---------------- SSE 分片解析（纯函数，最容易出错的地方） ----------------

    #[test]
    fn sse_joins_deltas_into_one_string() {
        let raw = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"{\\\"acts\\\":\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"[{\\\"r\\\":\\\"r1\\\"}]}\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        assert_eq!(sse_content(raw), "{\"acts\":[{\"r\":\"r1\"}]}");
    }

    #[test]
    fn sse_survives_a_split_multibyte_character() {
        // 「你」是 3 字节 UTF-8。真实流式里它**很可能被切在两个分片之间**。
        // 关键防线是：先整个缓冲区一次性转字符串，再按行切——
        // 顺序反了（逐片 from_utf8_lossy）就会在这里变成 U+FFFD。
        let raw = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"你\"}}]}\n\n",
        );
        assert_eq!(sse_content(raw), "你");
        assert!(!sse_content(raw).contains('\u{FFFD}'));
    }

    #[test]
    fn sse_skips_malformed_frames_instead_of_giving_up() {
        let raw = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"前\"}}]}\n\n",
            "data: {这不是合法 JSON\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"缀\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        assert_eq!(sse_content(raw), "前缀");
    }

    #[test]
    fn sse_ignores_reasoning_content() {
        // DeepSeek-R1 一类模型会先吐思维链在 reasoning_content 里，
        // 那是**思考过程不是答案**，取它会污染 JSON。
        let raw = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"让我想想\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"{\\\"ok\\\":1}\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        assert_eq!(sse_content(raw), "{\"ok\":1}");
    }

    #[test]
    fn sse_also_accepts_non_streaming_message_shape() {
        // 有些网关即使回了 SSE，内容仍用 message 而非 delta。
        let raw = "data: {\"choices\":[{\"message\":{\"content\":\"{\\\"ok\\\":2}\"}}]}\n\ndata: [DONE]\n\n";
        assert_eq!(sse_content(raw), "{\"ok\":2}");
    }

    #[test]
    fn done_marker_is_detected_in_a_partial_buffer() {
        // [DONE] 可能被切在两个 chunk 之间，必须跨边界能认出来。
        assert!(has_done_marker(b"data: {\"a\":1}\n\ndata: [DONE]\n\n"));
        assert!(has_done_marker(b"xxxdata: [DONE]"));
        assert!(!has_done_marker(b"data: {\"a\":1}"));
        assert!(!has_done_marker(b"data: [DON]"));
    }

    #[test]
    fn stream_budget_must_exceed_the_plain_timeout() {
        // 这是「一开流式就随机超时」那个坑的守卫：
        // client 级超时取 effective_timeout，若它比流式预算还短，
        // 请求级的 12 秒会被 client 级的覆盖掉。
        let cfg = LlmConfig::default();
        assert!(cfg.stream_timeout > cfg.timeout);
        assert_eq!(cfg.effective_timeout(), cfg.stream_timeout);

        let mut off = cfg.clone();
        off.stream = false;
        assert_eq!(off.effective_timeout(), cfg.timeout);
    }

    #[test]
    fn stream_toggle_is_lenient_and_defaults_on() {
        assert!(parse_bool_lenient(None));
        for v in ["1", "true", "yes", "on"] {
            assert!(parse_bool_lenient(Some(v.into())), "{v} 应为开");
        }
        for v in ["0", "false", "no", "off", ""] {
            assert!(!parse_bool_lenient(Some(v.into())), "{v} 应为关");
        }
        // 拼错的值按开启处理——绝不能因为一个没认出来的变量静默关掉流式。
        for v in ["maybe", "2", "流式"] {
            assert!(parse_bool_lenient(Some(v.into())), "{v} 应回落到开启");
        }
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
        let client = client_plain(base);
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
        let v = client_plain(base)
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
