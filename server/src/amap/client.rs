//! 高德 REST 客户端。
//!
//! 这个模块只负责三件事：
//! 1. 发请求（带 Key、带 QPS 闸门）；
//! 2. **解析高德的双层错误体系**（技术设计 §5.2 ③）——HTTP 200 也可能是失败；
//! 3. 把配额类失败翻译成 [`AmapError::Quota`]，好让上层静默降级。

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::Mutex;

use super::quota::{Quota, QuotaExceeded, QuotaKind};

/// 高德 Web 服务默认入口。
pub const AMAP_BASE: &str = "https://restapi.amap.com";
/// 读 Key 的环境变量名。
pub const AMAP_KEY_ENV: &str = "AMAP_WEB_KEY";

/// 每次请求的超时。高德偶发慢响应，超过这个时间就别等它了。
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct AmapConfig {
    /// **Web 服务**类型的 Key（不是 Android 平台那个）。
    pub web_key: String,
    pub base_url: String,
    pub timeout: Duration,
    /// QPS 上限，个人认证是 5。
    pub qps: u32,
}

impl Default for AmapConfig {
    fn default() -> Self {
        Self {
            web_key: String::new(),
            base_url: AMAP_BASE.to_string(),
            timeout: DEFAULT_TIMEOUT,
            qps: 5,
        }
    }
}

impl AmapConfig {
    pub fn new(web_key: impl Into<String>) -> Self {
        Self {
            web_key: web_key.into(),
            ..Self::default()
        }
    }

    /// 从环境变量读 Key。读不到返回空 Key，[`AmapClient::from_env`] 会返回 `None`。
    pub fn from_env() -> Self {
        let web_key = std::env::var(AMAP_KEY_ENV).unwrap_or_default();
        Self::new(web_key)
    }

    pub fn has_key(&self) -> bool {
        !self.web_key.trim().is_empty()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AmapError {
    /// 没配 Key。**这不是运行时错误，是「没接高德」这个正常状态。**
    #[error("未配置高德 Web 服务 Key（环境变量 {AMAP_KEY_ENV}），本次走本地估算")]
    KeyMissing,

    #[error("高德请求失败：{0}")]
    Http(#[from] reqwest::Error),

    /// HTTP 200，但 `status != "1"`。
    #[error("高德业务错误 infocode={code}：{info}")]
    Business { code: String, info: String },

    #[error("高德响应无法解析：{0}")]
    Decode(String),

    #[error(transparent)]
    Quota(#[from] QuotaExceeded),
}

impl AmapError {
    /// 能不能靠「换条路算」绕过。配额和缺 Key 都可以，业务错误要看情况。
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            AmapError::KeyMissing | AmapError::Quota(_) | AmapError::Http(_)
        )
    }

    /// 这个 `infocode` 是不是「超限/被熔断」。
    ///
    /// 高德的超限码有一堆（日超限、并发超限、海外超限……），
    /// 命名在不同接口上还不统一，所以这里按前缀 + 白名单双重判断。
    pub fn is_quota_infocode(code: &str) -> bool {
        matches!(
            code,
            // 日访问量超限 / 配额用尽
            "10003" | "10005" | "10026" | "10029" | "10044" | "10045"
            // QPS / 并发超限
            | "10021" | "10022" | "10023" | "10024" | "10025"
        )
    }
}

/// 高德 REST 客户端。**可以安全地被多任务共享**（内部只有 `Arc`）。
#[derive(Clone)]
pub struct AmapClient {
    http: reqwest::Client,
    cfg: AmapConfig,
    quota: Arc<Mutex<Quota>>,
}

impl std::fmt::Debug for AmapClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 绝不打印 Key
        f.debug_struct("AmapClient")
            .field("base_url", &self.cfg.base_url)
            .field("has_key", &self.cfg.has_key())
            .field("qps", &self.cfg.qps)
            .finish()
    }
}

impl AmapClient {
    /// 建一个客户端。没有 Key 时返回 `None`——**调用方应当退回纯本地模式，而不是报错**。
    pub fn from_env() -> Option<Self> {
        let cfg = AmapConfig::from_env();
        if cfg.has_key() {
            Self::new(cfg).ok()
        } else {
            None
        }
    }

    pub fn new(cfg: AmapConfig) -> Result<Self, AmapError> {
        if !cfg.has_key() {
            return Err(AmapError::KeyMissing);
        }
        let http = reqwest::Client::builder()
            .timeout(cfg.timeout)
            .user_agent(concat!("rider-sim/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let quota = Arc::new(Mutex::new(Quota::for_test(
            super::quota::current_day(),
            cfg.qps,
        )));
        Ok(Self { http, cfg, quota })
    }

    pub fn config(&self) -> &AmapConfig {
        &self.cfg
    }

    pub fn quota(&self) -> &Arc<Mutex<Quota>> {
        &self.quota
    }

    /// 今天各类接口用掉了多少次。验收标准「日 API 调用 < 200 次」看这里。
    pub async fn usage(&self, kind: QuotaKind) -> u32 {
        self.quota.lock().await.used(kind)
    }

    /// 拿到一个名额：先等 QPS 槽位，再占用日配额。
    ///
    /// QPS 满了会自己 `sleep`，所以**不会**因为限流而去撞高德的动态熔断。
    async fn acquire(&self, kind: QuotaKind) -> Result<(), AmapError> {
        loop {
            let wait = {
                let mut q = self.quota.lock().await;
                match q.try_acquire(kind, Instant::now()) {
                    Ok(()) => return Ok(()),
                    // QPS 只是「等一会」，日配额才是「今天没了」
                    Err(QuotaExceeded::Qps { retry_after_ms }) => {
                        Duration::from_millis(retry_after_ms)
                    }
                    Err(e) => return Err(AmapError::Quota(e)),
                }
            };
            tokio::time::sleep(wait).await;
        }
    }

    /// 发起一次 GET，并把双层错误摊平成 `Result`。
    ///
    /// `params` 里**不要**自己塞 `key`，这里统一加。
    pub async fn get(
        &self,
        path: &str,
        params: &[(String, String)],
        kind: QuotaKind,
    ) -> Result<Value, AmapError> {
        self.acquire(kind).await?;

        let url = format!("{}{path}", self.cfg.base_url.trim_end_matches('/'));
        let resp = self
            .http
            .get(&url)
            .query(&[("key", self.cfg.web_key.as_str())])
            .query(params)
            .send()
            .await?;

        // 第一层：HTTP 状态
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(AmapError::Business {
                code: status.as_u16().to_string(),
                info: snippet(&text),
            });
        }

        // 第二层：业务 status / infocode
        let body: Value = serde_json::from_str(&text)
            .map_err(|e| AmapError::Decode(format!("{e}；原文：{}", snippet(&text))))?;

        let ok = match body.get("status") {
            // status 有时是字符串 "1"，有时是数字 1
            Some(Value::String(s)) => s == "1",
            Some(Value::Number(n)) => n.as_i64() == Some(1),
            _ => false,
        };
        if !ok {
            let code = body
                .get("infocode")
                .map(value_to_string)
                .unwrap_or_else(|| "?".to_string());
            let info = body
                .get("info")
                .map(value_to_string)
                .unwrap_or_else(|| "未知错误".to_string());

            if AmapError::is_quota_infocode(&code) {
                // 服务器说超限了，本地立刻打满，避免后续继续撞墙
                self.quota.lock().await.mark_exhausted(kind);
                return Err(AmapError::Quota(QuotaExceeded::Daily {
                    kind: kind.as_str(),
                    limit: kind.daily_limit(),
                }));
            }
            return Err(AmapError::Business { code, info });
        }

        Ok(body)
    }
}

/// 把 JSON 标量转成字符串（高德同一个字段有时给字符串有时给数字）。
pub fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// 高德有些字段是 `"1234"`，有些是 `1234`，统一取出来。
pub fn value_to_i64(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

pub fn value_to_f64(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// 错误信息里带上原文片段，出问题时不用抓包也能看。
fn snippet(text: &str) -> String {
    let t = text.trim();
    let cut: String = t.chars().take(200).collect();
    if t.chars().count() > 200 {
        format!("{cut}…")
    } else {
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_key_is_a_state_not_a_bug() {
        let cfg = AmapConfig {
            web_key: "  ".into(),
            ..Default::default()
        };
        assert!(!cfg.has_key());
        assert!(matches!(
            AmapClient::new(cfg).unwrap_err(),
            AmapError::KeyMissing
        ));
    }

    #[test]
    fn quota_infocodes_are_recognised() {
        for code in ["10003", "10005", "10021", "10022", "10044"] {
            assert!(AmapError::is_quota_infocode(code), "{code} 应被判为超限");
        }
        assert!(!AmapError::is_quota_infocode("10001")); // Key 非法，不是超限
        assert!(!AmapError::is_quota_infocode("10002")); // 服务未开通
    }

    #[test]
    fn scalars_parse_from_both_string_and_number() {
        let s = serde_json::json!("1234");
        let n = serde_json::json!(1234);
        assert_eq!(value_to_i64(Some(&s)), Some(1234));
        assert_eq!(value_to_i64(Some(&n)), Some(1234));
        assert_eq!(value_to_i64(Some(&serde_json::json!(""))), None);
    }
}
