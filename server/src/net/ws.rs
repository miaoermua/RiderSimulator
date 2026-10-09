//! axum WebSocket 接入。
//!
//! ## 这一层只做「搬运」，不做判断
//!
//! 它把字节流变成 [`Inbound`] 消息丢给 `app` 的 tick 任务，反过来把
//! `Hub` 塞进出站队列的字符串写回 socket。**所有权限判断、状态变更都在
//! `app` 里发生**——因为只有那里拿着 `&mut World`，天然串行、天然原子。
//!
//! ```text
//! socket ──text──▶ 解析 ClientMessage ──▶ Inbound::Msg ──▶ app (World)
//! socket ◀──text── Hub 出站队列 ◀────── Hub::send_*  ◀──── app
//! ```
//!
//! 解析失败不会断开连接，只是回一个 `BAD_REQUEST`——客户端的 bug 不该
//! 让骑手掉线。

use std::sync::atomic::{AtomicU64, Ordering};

use axum::{
    Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::{Html, IntoResponse, Response},
    routing::get,
};
use futures::{SinkExt, StreamExt};
use tokio::sync::{
    mpsc::{UnboundedSender, unbounded_channel},
    oneshot,
};

use super::{
    api::{self, ApiAuth, ApiState},
    hub::ConnId,
    protocol::{ClientMessage, MAX_FRAME_BYTES, code},
};

/// WebSocket 路径。
pub const WS_PATH: &str = "/ws";
/// 健康检查路径。
pub const HEALTH_PATH: &str = "/healthz";

/// 全局连接 id 分配器。跨所有连接单调递增，方便日志里唯一定位一条连接。
static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

/// 分配一个连接 id。
pub fn next_conn_id() -> ConnId {
    NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed)
}

/// ws → app 的消息。
#[derive(Debug)]
pub enum Inbound {
    /// 新连接建立（还没握手，角色未知）。
    Open {
        conn_id: ConnId,
        tx: UnboundedSender<String>,
    },
    /// 收到一条**解析成功**的客户端消息。
    Msg { conn_id: ConnId, msg: ClientMessage },
    /// 帧解析失败。`action` 是能推断出的动作名，供 ack 用。
    BadFrame {
        conn_id: ConnId,
        code: &'static str,
        message: String,
    },
    /// 连接断开。`app` 负责把骑手标成离线（如果它手上没单）。
    Closed { conn_id: ConnId },
    /// 来自 HTTP 控制接口的一次调用。走同一条队列，执行完用 `reply` 回包。
    Rpc {
        req: super::api::RpcRequest,
        reply: oneshot::Sender<super::api::RpcReply>,
    },
}

/// 出站方向：`app` 通过它把文本塞进某个连接的队列。
pub type OutboundTx = UnboundedSender<String>;

/// 烽火台（控制台）页面路径。
pub const CONSOLE_PATH: &str = "/";

/// 烽火台页面，编译期打进二进制，部署时不用带静态目录。
const CONSOLE_HTML: &str = include_str!("../../static/console.html");

/// 组装 HTTP 路由。
///
/// `inbound` 是 app 主循环的入口队列；`/healthz` 用来给负载均衡探活。
/// `/api/**` 是给外部工具（workbuddy 等）用的远程控制接口，和 WebSocket
/// 共用同一条入站队列，因此不需要额外加锁。
pub fn router(inbound: UnboundedSender<Inbound>, auth: ApiAuth) -> Router {
    let api = api::routes(ApiState {
        inbound: inbound.clone(),
        auth,
    });
    Router::new()
        .route(CONSOLE_PATH, get(console))
        .route(WS_PATH, get(handler))
        .route(HEALTH_PATH, get(healthz))
        .with_state(inbound)
        .merge(api)
}

async fn healthz() -> impl IntoResponse {
    "ok"
}

/// 页面本身不含任何数据，控制权仍由 `/ws` 的 `hello` 决定。
async fn console() -> Html<&'static str> {
    Html(CONSOLE_HTML)
}

async fn handler(
    ws: WebSocketUpgrade,
    State(inbound): State<UnboundedSender<Inbound>>,
) -> Response {
    ws.on_upgrade(move |socket| serve(socket, inbound))
}

/// 单条连接的读写循环。
///
/// **不返回 `Result`**：断线是正常事件，不是错误。
async fn serve(socket: WebSocket, inbound: UnboundedSender<Inbound>) {
    let conn_id = next_conn_id();
    let (mut sink, mut stream) = socket.split();

    // 出站队列：app 只管往里塞字符串，写 socket 的事在这里做。
    let (out_tx, mut out_rx) = unbounded_channel::<String>();

    // 写任务。队列被 drop（= 连接结束）时自动退出。
    let writer = tokio::spawn(async move {
        while let Some(text) = out_rx.recv().await {
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    if inbound
        .send(Inbound::Open {
            conn_id,
            tx: out_tx,
        })
        .is_err()
    {
        // app 已经退出了，直接收摊
        writer.abort();
        return;
    }

    while let Some(frame) = stream.next().await {
        let frame = match frame {
            Ok(f) => f,
            // 协议错误 / 底层断开
            Err(_) => break,
        };
        match frame {
            Message::Text(text) => {
                if text.len() > MAX_FRAME_BYTES {
                    let _ = inbound.send(Inbound::BadFrame {
                        conn_id,
                        code: code::FRAME_TOO_LARGE,
                        message: format!("帧超过 {} 字节", MAX_FRAME_BYTES),
                    });
                    continue;
                }
                let parsed: Result<ClientMessage, _> = serde_json::from_str(text.as_str());
                let _ = match parsed {
                    Ok(msg) => inbound.send(Inbound::Msg { conn_id, msg }),
                    Err(e) => inbound.send(Inbound::BadFrame {
                        conn_id,
                        code: code::BAD_REQUEST,
                        message: e.to_string(),
                    }),
                };
            }
            Message::Close(_) => break,
            // 二进制帧不是本协议的一部分；静默忽略而不是断开。
            Message::Binary(_) => {}
            Message::Ping(_) | Message::Pong(_) => {}
        }
    }

    let _ = inbound.send(Inbound::Closed { conn_id });
    // 等写任务把队列里剩下的消息发完再收尾
    if tokio::time::timeout(std::time::Duration::from_millis(200), writer)
        .await
        .is_err()
    {
        // 超时就不等了，socket 随连接一起销毁
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::protocol::{Ack, Envelope};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[test]
    fn conn_ids_are_unique_and_increasing() {
        let a = next_conn_id();
        let b = next_conn_id();
        assert!(b > a);
    }

    #[test]
    fn frame_limit_is_sane() {
        const { assert!(MAX_FRAME_BYTES >= 8 * 1024, "太小会误伤正常消息") };
        const { assert!(MAX_FRAME_BYTES <= 1024 * 1024, "太大会被恶意客户端打爆") };
    }

    #[tokio::test]
    async fn healthz_answers_ok() {
        let (tx, _rx) = unbounded_channel();
        let app = router(tx, ApiAuth::default());
        let res = app
            .oneshot(
                Request::builder()
                    .uri(HEALTH_PATH)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn ws_route_rejects_plain_get_without_upgrade_headers() {
        let (tx, _rx) = unbounded_channel();
        let app = router(tx, ApiAuth::default());
        let res = app
            .oneshot(Request::builder().uri(WS_PATH).body(Body::empty()).unwrap())
            .await
            .unwrap();
        // 缺 Upgrade 头时 axum 返回 426 / 400，总之不是 200
        assert_ne!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn api_routes_are_mounted_next_to_the_console() {
        let (tx, _rx) = unbounded_channel();
        let app = router(tx, ApiAuth::default());
        // 没开控制端时是 403，说明路由确实挂上了（而不是 404）
        let res = app
            .oneshot(
                Request::builder()
                    .uri(crate::net::api::STATE_PATH)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn envelope_serializes_the_way_clients_expect() {
        let env = Envelope::new("pong", serde_json::json!({}));
        let v: serde_json::Value = serde_json::to_value(&env).unwrap();
        assert_eq!(v["event"], "pong");
        assert!(v["payload"].is_object());
    }

    #[test]
    fn ack_error_codes_are_stable_strings() {
        let a = Ack::err("claim", code::NOT_A_RIDER, "观战者不能抢单");
        assert_eq!(code::NOT_A_RIDER, "NOT_A_RIDER");
        assert_eq!(a.code.as_deref(), Some("NOT_A_RIDER"));
    }
}
