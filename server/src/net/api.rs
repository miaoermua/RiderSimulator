//! 远程控制 / 派单 HTTP 接口。
//!
//! 给 workbuddy 这类「不方便讲 WebSocket」的外部工具用：发一个 HTTP 请求，
//! 主循环执行完再把结果原样吐回来。
//!
//! ## 不另起状态通道
//!
//! 请求被包成 [`RpcRequest`] 塞进与 `/ws` 同一个 [`Inbound`] 队列，排在同
//! 一条队里执行——`&mut World` 只有一个，天然串行、天然原子。这里只做
//! 「鉴权 + 搬运 + 等回包」，一行游戏规则都不写。
//!
//! ```text
//! curl ──HTTP──▶ axum handler ──Inbound::Rpc──▶ app (World) ──oneshot──▶ 回包
//! ```
//!
//! ## 鉴权
//!
//! 与 `/ws` 的 `hello` 同一套：`Authorization: Bearer <control token>`。
//! 服务端没配 `RIDER_SIM_CONTROL_TOKEN` 时退化成 `RIDER_SIM_ALLOW_CONTROL`
//! ——开着就放行，关着就 403。

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header::AUTHORIZATION},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{mpsc::UnboundedSender, oneshot};

use super::{
    protocol::{AiPatch, ConfigPatch},
    ws::Inbound,
};
use crate::sim::{OrderId, RiderId};

/// 读当前世界。
pub const STATE_PATH: &str = "/api/state";
/// 派单：把订单指派给骑手。
pub const DISPATCH_PATH: &str = "/api/dispatch";
/// 改派：把**还没取到餐**的单从一个骑手手上转给另一个骑手。
///
/// 与 [`DISPATCH_PATH`] 分开而不是合成一个接口：派单只认大厅里**无主**的单，
/// 改派只认**别人手上还没取餐**的单，前置条件正相反，失败原因也不一样
/// （`ALREADY_CLAIMED` vs `WRONG_PHASE`）。合成一个，调用方就分不清自己错在哪。
pub const REASSIGN_PATH: &str = "/api/reassign";
/// 批量改控制端开关。
pub const CONFIG_PATH: &str = "/api/config";
/// 强制某个骑手上线 / 下线。
pub const RIDER_ONLINE_PATH: &str = "/api/riders/{id}/online";
/// 校准某个骑手的位置（`{"lat":..,"lng":..}`）。只改坐标，不动状态机。
pub const RIDER_POSITION_PATH: &str = "/api/riders/{id}/position";
/// 三层 AI 的观测与开关：`GET` 读状态，`POST` 改开关 / 托管模式。
pub const AI_PATH: &str = "/api/ai";
/// `GET` 读三层 AI 状态。与 `GET /api/ai` 等价，留给只认显式路径的调用方。
pub const AI_STATE_PATH: &str = "/api/ai/state";
/// `POST` 改开关 / 托管模式。与 `POST /api/ai` 等价。
pub const AI_CONFIG_PATH: &str = "/api/ai/config";
/// 人工否决一条 AI 决策。
pub const AI_REJECT_PATH: &str = "/api/ai/reject";
/// 骑手账号：`GET` 列名单（含手机可达地址），`POST` 建号。
///
/// 建号的响应里带**明文密码**和二维码——这是全系统唯一一次能看到明文的
/// 机会（库只存 Argon2 哈希），见 [`crate::net::join`] 的取舍说明。
pub const ACCOUNTS_PATH: &str = "/api/accounts";
/// `POST` 改 / 重置某个账号的密码。`password` 缺省时服务端生成一个并回显。
pub const ACCOUNT_PASSWORD_PATH: &str = "/api/accounts/{name}/password";
/// `DELETE` 删账号。
pub const ACCOUNT_PATH: &str = "/api/accounts/{name}";
/// 平台活动：`GET` 列全部（含已下架），`POST` 新建。
pub const ACTIVITIES_PATH: &str = "/api/activities";
/// `POST` 上架 / 下架某个活动（`{"active": false}`）。
pub const ACTIVITY_ACTIVE_PATH: &str = "/api/activities/{id}/active";
/// `DELETE` 删掉某个活动。
pub const ACTIVITY_ITEM_PATH: &str = "/api/activities/{id}";

/// 鉴权配置。由 `main.rs` 的环境变量决定。
#[derive(Debug, Clone, Default)]
pub struct ApiAuth {
    /// `RIDER_SIM_CONTROL_TOKEN`。`None` = 不接受令牌方式。
    pub token: Option<String>,
    /// `RIDER_SIM_ALLOW_CONTROL`：没配令牌时是否放行。
    pub allow_control: bool,
}

impl ApiAuth {
    fn check(&self, headers: &HeaderMap) -> Result<(), ApiError> {
        let presented = headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|t| !t.is_empty());

        if let Some(expected) = &self.token {
            return match presented {
                Some(got) if got == expected => Ok(()),
                _ => Err(ApiError::new(
                    StatusCode::UNAUTHORIZED,
                    "UNAUTHORIZED",
                    "控制端令牌无效",
                )),
            };
        }
        if self.allow_control {
            return Ok(());
        }
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "CONTROL_DISABLED",
            "服务端未开控制端：请设置 RIDER_SIM_CONTROL_TOKEN",
        ))
    }
}

/// 一条要交给主循环执行的指令。
#[derive(Debug)]
pub enum RpcRequest {
    /// 取控制端视角的完整快照。
    State,
    /// 派单。整批按顺序执行，一条失败不挡后面的。
    Dispatch(Vec<DispatchItem>),
    /// 改派。整批按顺序执行，一条失败不挡后面的。
    Reassign(Vec<DispatchItem>),
    /// 改开关。
    SetConfig(ConfigPatch),
    /// 上 / 下线。
    SetRiderOnline { rider_id: RiderId, online: bool },
    /// **校准骑手坐标**：只改位置，不动状态机（手上还是那些单，阶段照旧）。
    ///
    /// 给「手机没定位权限 / 定位漂了」的真人骑手用——否则只能自己走回去。
    /// 坐标范围已在 HTTP 层校验，这里只兜底（HTTP 层可以被绕过）。
    SetRiderPosition {
        rider_id: RiderId,
        lat: f64,
        lng: f64,
    },
    /// 读三层 AI 的完整状态（开关 / 预算 / 统计 / 站点 KPI / 否决记录）。
    AiState,
    /// 改 AI 开关与托管模式。
    SetAi(super::protocol::AiPatch),
    /// 人工否决一条决策。
    RejectAi { decision_id: u64 },
    /// 账号名单 + 手机可达的接入地址。
    Accounts,
    /// 建号。`password` 缺省 = 服务端生成一个（响应里回显明文 + 二维码）。
    CreateAccount {
        name: String,
        password: Option<String>,
    },
    /// 改 / 重置密码。`password` 缺省 = 服务端生成一个（响应里回显）。
    SetAccountPassword {
        name: String,
        password: Option<String>,
    },
    /// 删号。
    DeleteAccount { name: String },
    /// 列活动（含已下架的）。
    Activities,
    /// 建活动。达成后奖金自动入账。
    CreateActivity {
        title: String,
        target_orders: u32,
        reward_cents: i64,
    },
    /// 上架 / 下架。
    SetActivityActive { id: u32, active: bool },
    /// 删活动。
    DeleteActivity { id: u32 },
}

/// 主循环回给 HTTP 的结果。
pub type RpcReply = Result<Value, ApiError>;

/// 协议错误。`code` 与 WS 侧一样是**固定字符串枚举**。
#[derive(Debug, Clone, Serialize)]
pub struct ApiError {
    #[serde(skip)]
    pub status: StatusCode,
    pub code: String,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &str, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.to_string(),
            message: message.into(),
        }
    }

    /// 业务上「不合法」但不代表服务端出错，统一 409。
    pub fn conflict(code: &str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "BAD_REQUEST", message)
    }

    pub fn not_found(code: &str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message)
    }

    /// 服务端自己的锅。
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = json!({ "error": { "code": self.code, "message": self.message } });
        (self.status, Json(body)).into_response()
    }
}

/// 一条派单指令。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchItem {
    /// `3` 和 `"r3"` 都认——外部工具手里多半是从快照里抄来的 key。
    pub rider_id: Value,
    pub order_id: OrderId,
    /// 派单理由，会进控制台的事件流。缺省用服务端的默认文案。
    #[serde(default)]
    pub reason: Option<String>,
}

impl DispatchItem {
    /// 解析出骑手 id；给不出就 `None`（调用方报 `RIDER_NOT_FOUND`）。
    pub fn rider(&self) -> Option<RiderId> {
        parse_rider_value(&self.rider_id)
    }
}

/// 数字 `3`、字符串 `"3"`、`"r3"` 都解析成 `3`。
fn parse_rider_value(v: &Value) -> Option<RiderId> {
    if let Some(n) = v.as_u64() {
        return u32::try_from(n).ok();
    }
    if let Some(n) = v.as_i64() {
        return u32::try_from(n).ok();
    }
    let s = v.as_str()?.trim();
    let digits = s.strip_prefix('r').unwrap_or(s);
    digits.parse().ok()
}

/// `POST /api/dispatch` 的请求体。三种写法都收：
///
/// ```json
/// {"assignments":[{"rider_id":"r3","order_id":42}]}   // 批量
/// {"rider_id":3,"order_id":42,"reason":"顺路"}          // 单条
/// [{"rider_id":3,"order_id":42}]                        // 裸数组
/// ```
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum DispatchBody {
    Batch { assignments: Vec<DispatchItem> },
    Single(DispatchItem),
    List(Vec<DispatchItem>),
}

impl DispatchBody {
    fn into_items(self) -> Vec<DispatchItem> {
        match self {
            DispatchBody::Batch { assignments } => assignments,
            DispatchBody::Single(item) => vec![item],
            DispatchBody::List(items) => items,
        }
    }
}

/// 交给 axum 的共享状态。
#[derive(Clone)]
pub struct ApiState {
    pub inbound: UnboundedSender<Inbound>,
    pub auth: ApiAuth,
}

impl ApiState {
    /// 鉴权 → 投递 → 等主循环回包。
    async fn call(&self, headers: &HeaderMap, req: RpcRequest) -> Response {
        if let Err(e) = self.auth.check(headers) {
            return e.into_response();
        }
        let (tx, rx) = oneshot::channel();
        if self.inbound.send(Inbound::Rpc { req, reply: tx }).is_err() {
            return ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "INTERNAL", "主循环已退出")
                .into_response();
        }
        match rx.await {
            Ok(Ok(v)) => Json(v).into_response(),
            Ok(Err(e)) => e.into_response(),
            Err(_) => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL",
                "主循环没有回应",
            )
            .into_response(),
        }
    }
}

/// 组装 `/api/**` 路由。返回 `Router<()>`，可以直接 merge 进主 router。
pub fn routes(state: ApiState) -> Router {
    Router::new()
        .route(STATE_PATH, get(state_route))
        .route(DISPATCH_PATH, post(dispatch_route))
        .route(REASSIGN_PATH, post(reassign_route))
        .route(CONFIG_PATH, post(config_route))
        .route(RIDER_ONLINE_PATH, post(rider_online_route))
        .route(RIDER_POSITION_PATH, post(rider_position_route))
        .route(AI_PATH, get(ai_state_route).post(ai_config_route))
        .route(AI_STATE_PATH, get(ai_state_route))
        .route(AI_CONFIG_PATH, post(ai_config_route))
        .route(AI_REJECT_PATH, post(ai_reject_route))
        .route(
            ACCOUNTS_PATH,
            get(accounts_route).post(create_account_route),
        )
        .route(ACCOUNT_PASSWORD_PATH, post(account_password_route))
        .route(ACCOUNT_PATH, delete(account_delete_route))
        .route(
            ACTIVITIES_PATH,
            get(activities_route).post(create_activity_route),
        )
        .route(ACTIVITY_ACTIVE_PATH, post(activity_active_route))
        .route(ACTIVITY_ITEM_PATH, delete(activity_delete_route))
        .with_state(state)
}

async fn activities_route(State(api): State<ApiState>, headers: HeaderMap) -> Response {
    api.call(&headers, RpcRequest::Activities).await
}

/// `POST /api/activities` —— 建一个活动。
///
/// `reward_cents` 是**分**；控制台用元填表，转好再发。`target_orders` 必须 > 0，
/// 否则「跑 0 单给钱」会立刻给所有在线骑手发钱。
async fn create_activity_route(
    State(api): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let Some(title) = body.get("title").and_then(Value::as_str) else {
        return ApiError::bad_request("需要 title（活动标题）").into_response();
    };
    let Some(target_orders) = body.get("target_orders").and_then(Value::as_u64) else {
        return ApiError::bad_request("需要 target_orders（正整数）").into_response();
    };
    let Ok(target_orders) = u32::try_from(target_orders) else {
        return ApiError::bad_request("target_orders 太大").into_response();
    };
    // 允许只传 reward_yuan（控制台表单就是元），服务端当场换成整数分。
    let reward_cents = match body.get("reward_cents").and_then(Value::as_i64) {
        Some(cents) => cents,
        None => {
            let Some(yuan) = body.get("reward_yuan").and_then(Value::as_f64) else {
                return ApiError::bad_request("需要 reward_cents 或 reward_yuan").into_response();
            };
            (yuan * 100.0).round() as i64
        }
    };
    api.call(
        &headers,
        RpcRequest::CreateActivity {
            title: title.to_string(),
            target_orders,
            reward_cents,
        },
    )
    .await
}

async fn activity_active_route(
    State(api): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<u32>,
    Json(body): Json<Value>,
) -> Response {
    let active = body.get("active").and_then(Value::as_bool).unwrap_or(true);
    api.call(&headers, RpcRequest::SetActivityActive { id, active })
        .await
}

async fn activity_delete_route(
    State(api): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<u32>,
) -> Response {
    api.call(&headers, RpcRequest::DeleteActivity { id }).await
}

async fn accounts_route(State(api): State<ApiState>, headers: HeaderMap) -> Response {
    api.call(&headers, RpcRequest::Accounts).await
}

async fn create_account_route(
    State(api): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let Some(name) = body.get("name").and_then(Value::as_str) else {
        return ApiError::bad_request("需要 name").into_response();
    };
    let password = body
        .get("password")
        .and_then(Value::as_str)
        .map(str::to_string);
    api.call(
        &headers,
        RpcRequest::CreateAccount {
            name: name.to_string(),
            password,
        },
    )
    .await
}

async fn account_password_route(
    State(api): State<ApiState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let password = body
        .get("password")
        .and_then(Value::as_str)
        .map(str::to_string);
    api.call(&headers, RpcRequest::SetAccountPassword { name, password })
        .await
}

async fn account_delete_route(
    State(api): State<ApiState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    api.call(&headers, RpcRequest::DeleteAccount { name }).await
}

async fn ai_state_route(State(api): State<ApiState>, headers: HeaderMap) -> Response {
    api.call(&headers, RpcRequest::AiState).await
}

async fn ai_config_route(
    State(api): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let patch: AiPatch = match serde_json::from_value(body) {
        Ok(p) => p,
        Err(e) => {
            return ApiError::bad_request(format!("无法解析为 AI patch：{e}")).into_response();
        }
    };
    api.call(&headers, RpcRequest::SetAi(patch)).await
}

async fn ai_reject_route(
    State(api): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let Some(decision_id) = body
        .get("decision_id")
        .and_then(Value::as_u64)
        .or_else(|| body.get("id").and_then(Value::as_u64))
    else {
        return ApiError::bad_request("需要 decision_id").into_response();
    };
    api.call(&headers, RpcRequest::RejectAi { decision_id })
        .await
}

async fn state_route(State(api): State<ApiState>, headers: HeaderMap) -> Response {
    api.call(&headers, RpcRequest::State).await
}

async fn dispatch_route(
    State(api): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let body: DispatchBody = match serde_json::from_value(body) {
        Ok(b) => b,
        Err(e) => {
            return ApiError::bad_request(format!(
                "请求体应为 {{\"assignments\":[...]}} / 单条对象 / 数组：{e}"
            ))
            .into_response();
        }
    };
    let items = body.into_items();
    if items.is_empty() {
        return ApiError::bad_request("assignments 不能为空").into_response();
    }
    api.call(&headers, RpcRequest::Dispatch(items)).await
}

async fn reassign_route(
    State(api): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let body: DispatchBody = match serde_json::from_value(body) {
        Ok(b) => b,
        Err(e) => {
            return ApiError::bad_request(format!(
                "请求体应为 {{\"assignments\":[...]}} / 单条对象 / 数组：{e}"
            ))
            .into_response();
        }
    };
    let items = body.into_items();
    if items.is_empty() {
        return ApiError::bad_request("assignments 不能为空").into_response();
    }
    api.call(&headers, RpcRequest::Reassign(items)).await
}

async fn config_route(
    State(api): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let patch: ConfigPatch = match serde_json::from_value(body) {
        Ok(p) => p,
        Err(e) => {
            return ApiError::bad_request(format!("无法解析为 config patch：{e}")).into_response();
        }
    };
    api.call(&headers, RpcRequest::SetConfig(patch)).await
}

async fn rider_online_route(
    State(api): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<u32>,
    Json(body): Json<Value>,
) -> Response {
    let online = body.get("online").and_then(Value::as_bool).unwrap_or(true);
    api.call(
        &headers,
        RpcRequest::SetRiderOnline {
            rider_id: id,
            online,
        },
    )
    .await
}

/// `POST /api/riders/{id}/position` —— **校准真人骑手的位置**。
///
/// 存在的理由：真人骑手的位置来自手机 GPS 上报，但**没定位权限 / 定位不准时
/// 就没有任何办法把他放回该在的地方**——没有这个接口，一个卡在错误位置的
/// 真人骑手只能自己走回去，或者干脆重新建号。
///
/// 语义上是「校准」而不是「传送」：**只改坐标，不动状态机**。
/// 手上的单还是那些单，阶段还是原来那个阶段。所以校准之后他得自己按流程
/// 走到店、按流程送达——不会凭空跳过 `arrive_shop` / `deliver` 那几步。
///
/// 坐标用哪套由**调用方**决定（这个接口不做换算）：服务端世界是 GCJ-02，
/// 但骑手端拿到的可能是 WGS-84，两者差几百米。要用别的坐标系，
/// 传 WGS-84 的值之前先换好。
async fn rider_position_route(
    State(api): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<u32>,
    Json(body): Json<Value>,
) -> Response {
    let Some(lat) = body.get("lat").and_then(Value::as_f64) else {
        return ApiError::bad_request("缺少 lat（数字）").into_response();
    };
    let Some(lng) = body.get("lng").and_then(Value::as_f64) else {
        return ApiError::bad_request("缺少 lng（数字）").into_response();
    };
    // 范围校验：越界的坐标会让 haversine 算出天文数字，把路线与时薪全带崩。
    // 而且静默接受一个明显写错的经度，比报错难查得多。
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lng) {
        return ApiError::bad_request(format!(
            "坐标越界：lat={lat} lng={lng}（纬度 -90..90，经度 -180..180）"
        ))
        .into_response();
    }
    api.call(
        &headers,
        RpcRequest::SetRiderPosition {
            rider_id: id,
            lat,
            lng,
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tokio::sync::mpsc::unbounded_channel;
    use tower::ServiceExt;

    fn bearer(t: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, format!("Bearer {t}").parse().unwrap());
        h
    }

    /// `RIDER_SIM_ALLOW_CONTROL=true`、没配令牌时的开发态。
    fn open_auth() -> ApiAuth {
        ApiAuth {
            token: None,
            allow_control: true,
        }
    }

    #[test]
    fn rider_value_accepts_number_and_key() {
        assert_eq!(parse_rider_value(&json!(3)), Some(3));
        assert_eq!(parse_rider_value(&json!("3")), Some(3));
        assert_eq!(parse_rider_value(&json!("r3")), Some(3));
        assert_eq!(parse_rider_value(&json!(" r7 ")), Some(7));
        assert_eq!(parse_rider_value(&json!("rider")), None);
        assert_eq!(parse_rider_value(&json!(null)), None);
        assert_eq!(parse_rider_value(&json!(-1)), None);
    }

    #[test]
    fn dispatch_body_accepts_all_three_shapes() {
        let batch: DispatchBody =
            serde_json::from_value(json!({"assignments":[{"rider_id":"r3","order_id":42}]}))
                .unwrap();
        assert_eq!(batch.into_items().len(), 1);

        let single: DispatchBody =
            serde_json::from_value(json!({"rider_id":3,"order_id":42,"reason":"顺路"})).unwrap();
        let items = single.into_items();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].reason.as_deref(), Some("顺路"));

        let list: DispatchBody = serde_json::from_value(
            json!([{"rider_id":3,"order_id":1},{"rider_id":4,"order_id":2}]),
        )
        .unwrap();
        assert_eq!(list.into_items().len(), 2);

        assert!(serde_json::from_value::<DispatchBody>(json!({"note":"空的"})).is_err());
    }

    #[test]
    fn token_takes_priority_over_allow_control() {
        let auth = ApiAuth {
            token: Some("s3cret".into()),
            allow_control: true,
        };
        assert!(auth.check(&bearer("s3cret")).is_ok());
        assert!(auth.check(&bearer("wrong")).is_err());
        assert!(auth.check(&HeaderMap::new()).is_err());
    }

    #[test]
    fn without_token_it_falls_back_to_allow_control() {
        let open = ApiAuth {
            token: None,
            allow_control: true,
        };
        assert!(open.check(&HeaderMap::new()).is_ok());
        let closed = ApiAuth {
            token: None,
            allow_control: false,
        };
        let err = closed.check(&HeaderMap::new()).unwrap_err();
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert_eq!(err.code, "CONTROL_DISABLED");
    }

    #[tokio::test]
    async fn unauthorized_request_never_reaches_the_loop() {
        let (tx, mut rx) = unbounded_channel();
        let app = routes(ApiState {
            inbound: tx,
            auth: ApiAuth {
                token: Some("s3cret".into()),
                allow_control: false,
            },
        });
        let res = app
            .oneshot(
                Request::builder()
                    .uri(STATE_PATH)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert!(rx.try_recv().is_err(), "没通过鉴权就不该打扰主循环");
    }

    #[tokio::test]
    async fn authorized_state_round_trips_into_inbound() {
        let (tx, mut rx) = unbounded_channel();
        let app = routes(ApiState {
            inbound: tx,
            auth: ApiAuth {
                token: Some("s3cret".into()),
                allow_control: false,
            },
        });
        let app2 = app.clone();
        let responder = tokio::spawn(async move {
            if let Some(Inbound::Rpc { reply, .. }) = rx.recv().await {
                let _ = reply.send(Ok(json!({"day": 1})));
            }
        });
        let res = app2
            .oneshot(
                Request::builder()
                    .uri(STATE_PATH)
                    .header(AUTHORIZATION, "Bearer s3cret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap()["day"], 1);
        responder.await.unwrap();
    }

    #[tokio::test]
    async fn empty_dispatch_is_rejected_before_queueing() {
        let (tx, mut rx) = unbounded_channel();
        let app = routes(ApiState {
            inbound: tx,
            auth: open_auth(),
        });
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(DISPATCH_PATH)
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"assignments":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn rider_online_defaults_to_true() {
        let (tx, mut rx) = unbounded_channel();
        let app = routes(ApiState {
            inbound: tx,
            auth: open_auth(),
        });
        let responder = tokio::spawn(async move {
            match rx.recv().await {
                Some(Inbound::Rpc {
                    req: RpcRequest::SetRiderOnline { rider_id, online },
                    reply,
                }) => {
                    assert_eq!(rider_id, 5);
                    assert!(online);
                    let _ = reply.send(Ok(json!({"ok": true})));
                }
                other => panic!("收到了意料之外的入站消息：{other:?}"),
            }
        });
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/riders/5/online")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        responder.await.unwrap();
    }
}
