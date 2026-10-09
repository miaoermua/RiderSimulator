//! WebSocket 协议（技术设计 §4）。
//!
//! ## 三件必须钉死的事
//!
//! 1. **状态权威在服务端。** 客户端只能发「意图」（抢单、切档、上线），
//!    所有状态变更都在 tick 循环里发生；客户端拿到的永远只是快照。
//! 2. **错误码是固定字符串**（[`crate::sim::scheduler::ClaimError::code`]），
//!    中文文案随便改，客户端靠 `code` 分支。
//! 3. **鉴权在服务端做过滤。** 骑手看不到别人的收入、看不到高德 Key、
//!    看不到控制端字段。过滤发生在 [`super::hub`] 下发那一刻，不在这里。
//!
//! 消息形状：
//!
//! ```text
//! 客户端 → 服务端：{ "type": "claim", "order_id": 42 }
//! 服务端 → 客户端：{ "event": "order_claimed", "payload": { ... } }
//! ```

use serde::{Deserialize, Serialize};

use crate::sim::{OrderId, RiderId, geo::CoordSystem, rider::SpeedMode};

/// 协议版本。客户端版本不一致时服务端拒绝并返回 `PROTOCOL_MISMATCH`。
pub use crate::PROTOCOL_VERSION;

/// 单条消息的硬上限。防止恶意客户端塞爆内存。
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// 客户端 → 服务端
// ---------------------------------------------------------------------------

/// 客户端发来的消息。
///
/// 所有字段都带默认值容忍策略：字段缺失 = 用法错误 = 返回 `BAD_REQUEST`，
/// **绝不 panic**。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// 握手。**必须是第一条消息**，否则连接被关闭。
    ///
    /// 不带 `token` = 观战者；带 `rider_id` = 绑定到该骑手。
    Hello {
        /// 骑手令牌。协议层只做映射，真正的校验在 `app` 层。
        ///
        /// - 缺省 → 控制端 / 观战者（受 `DEV_ALLOW_CONTROL` 环境变量约束）
        /// - `rider:<id>` → 绑定骑手 `<id>`
        /// - `new:<昵称>` → 现场创建一个真人骑手
        #[serde(default)]
        token: Option<String>,
        /// 客户端协议版本，不一致直接拒绝。
        #[serde(default)]
        protocol_version: Option<String>,
    },

    /// 重新拉一份完整快照（断线重连用）。
    Resync,

    /// 抢单。
    Claim {
        order_id: OrderId,
        /// 走背单逻辑（绕路顺带）。
        #[serde(default)]
        back_order: bool,
    },
    /// 拒单（把已指派但未取货的单退回大厅）。
    Reject { order_id: OrderId },
    /// 催单。
    Urge { order_id: OrderId },
    /// 到店确认（真人骑手必须点）。
    ArriveShop { order_id: OrderId },
    /// 到达顾客。
    ArriveCustomer { order_id: OrderId },
    /// 确认送达。
    Deliver { order_id: OrderId },

    /// 上下线。
    SetOnline { online: bool },
    /// 切速度档位。
    SetSpeedMode { mode: SpeedMode },

    /// 骑手在任务对话里说一句话。顾客回复由服务端（LLM 或关键词）生成。
    TaskMessage { order_id: OrderId, text: String },

    /// 心跳。服务端回 `pong`。
    Ping,

    // ---------------- 控制端 ----------------
    /// 暂停 / 继续（等价于把倍率设成 0）。
    SetPaused { paused: bool },
    /// 时间倍率（1 / 2 / 4 / 8）。
    SetSpeedMultiplier { multiplier: u32 },
    /// 手动改天气。
    SetWeather {
        weather: crate::sim::weather::Weather,
    },
    /// 调整平台抽水比例。
    SetCommissionRate { rate: f32 },
    /// 强制指派。
    ForceAssign {
        order_id: OrderId,
        rider_id: RiderId,
    },
    /// 开关 LLM 战术大脑。
    SetLlmEnabled { enabled: bool },
    /// 批量改控制端开关（玩法设计 §7.3）。缺省字段 = 不改。
    SetConfig(ConfigPatch),
    /// 强制某个骑手上线 / 下线。手上有单时不能下线。
    SetRiderOnline { rider_id: RiderId, online: bool },
    /// 切换实时档：1 模拟分钟 = 真实 60 秒。
    ///
    /// `wall_minute` 是真实钟表的「当日第几分钟」（0..1439），由控制端浏览器给出，
    /// 服务端不引入时区库。缺省时用 UTC 兜底。
    SetRealtime {
        enabled: bool,
        #[serde(default)]
        wall_minute: Option<u32>,
    },
    /// 给单个骑手改名（控制端 / 骑手卡片上的就近改名）。
    RenameRider { rider_id: RiderId, name: String },
    /// 按名单批量重命名模拟骑手（顺序对应 id 升序，空行跳过）。
    SetRiderNames { names: Vec<String> },
    /// 配置 LLM 接入。字段缺省 = 不改；`api_key` 传空串 = 清除 Key。
    SetLlmConfig {
        #[serde(default)]
        api_key: Option<String>,
        #[serde(default)]
        base_url: Option<String>,
        #[serde(default)]
        model: Option<String>,
    },
}

/// `set_config` 的载荷。每个字段都是可选的，越界值在服务端夹取。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ConfigPatch {
    /// 抢单窗口秒数，1~10。
    #[serde(default)]
    pub claim_window_secs: Option<u32>,
    #[serde(default)]
    pub allow_speeding: Option<bool>,
    #[serde(default)]
    pub allow_unrestricted: Option<bool>,
    #[serde(default)]
    pub backorder_enabled: Option<bool>,
    /// 背单上限，1~5。
    #[serde(default)]
    pub backorder_max: Option<u32>,
    #[serde(default)]
    pub discriminative_commission: Option<bool>,
    #[serde(default)]
    pub show_expected_value: Option<bool>,
    #[serde(default)]
    pub ai_batch_enabled: Option<bool>,
    /// AI 骑手数量，0~8。
    #[serde(default)]
    pub sim_rider_count: Option<u32>,
    /// 每日单量上限，0 = 不限。
    #[serde(default)]
    pub max_orders_per_day: Option<u32>,
    /// 下发坐标的坐标系。内部始终 GCJ-02，这里只影响出口转换。
    #[serde(default)]
    pub coord_system: Option<CoordSystem>,

    // ---- 计价规则（「单价系统」）----
    /// 起步价（分），0~2000。
    #[serde(default)]
    pub base_fee_cents: Option<i64>,
    /// 超出里程阶梯后每公里单价（分/km），0~1000。
    #[serde(default)]
    pub per_km_over_cents: Option<i64>,
    /// 等级加成（分/级），0~500。
    #[serde(default)]
    pub level_bonus_per_level_cents: Option<i64>,
}

// ---------------------------------------------------------------------------
// 服务端 → 客户端
// ---------------------------------------------------------------------------

/// 服务端事件的信封。
///
/// `event` 是**固定字符串**，值来自 [`crate::sim::event::SimEvent`] 的
/// serde 标签（`tick` / `order_broadcast` / …），另有几个传输层事件。
#[derive(Debug, Clone, Serialize)]
pub struct Envelope {
    pub event: String,
    pub payload: serde_json::Value,
}

impl Envelope {
    pub fn new(event: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            event: event.into(),
            payload,
        }
    }

    /// 把 `SimEvent` 包成信封，复用它的 serde 标签。
    pub fn from_event(ev: &crate::sim::event::SimEvent) -> Self {
        let value = match serde_json::to_value(ev) {
            Ok(v) => v,
            Err(e) => {
                return Self::new(
                    "error",
                    serde_json::json!({ "code": "INTERNAL", "message": e.to_string() }),
                );
            }
        };
        let name = value
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let payload = value.get("payload").cloned().unwrap_or(value);
        Self::new(name, payload)
    }
}

/// 握手成功。
#[derive(Debug, Clone, Serialize)]
pub struct Welcome {
    pub protocol_version: &'static str,
    /// `rider` | `control` | `spectator`
    pub role: &'static str,
    /// 绑定到的骑手（控制端 / 观战者为 `None`）。
    pub rider_id: Option<RiderId>,
    /// `r3` 形式。
    pub rider_key: Option<String>,
    /// 服务端 tick 间隔（毫秒），客户端据此安排平滑插值。
    pub tick_interval_ms: u64,
    pub server_minute: u32,
    pub snapshot: serde_json::Value,
}

/// 通用应答。`ok=false` 时 `code` 是固定错误码。
#[derive(Debug, Clone, Serialize)]
pub struct Ack {
    pub action: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// 附带的业务数据（例如抢单成功后的报价）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl Ack {
    pub fn ok(action: impl Into<String>) -> Self {
        Self {
            action: action.into(),
            ok: true,
            code: None,
            message: None,
            data: None,
        }
    }

    pub fn ok_with(action: impl Into<String>, data: serde_json::Value) -> Self {
        Self {
            action: action.into(),
            ok: true,
            code: None,
            message: None,
            data: Some(data),
        }
    }

    pub fn err(
        action: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            action: action.into(),
            ok: false,
            code: Some(code.into()),
            message: Some(message.into()),
            data: None,
        }
    }
}

/// 传输层固定错误码。与 `ClaimError::code()` 互不重叠、互不覆盖。
pub mod code {
    /// 帧不是合法 JSON / 结构不符。
    pub const BAD_REQUEST: &str = "BAD_REQUEST";
    /// 超过 [`super::protocol::MAX_FRAME_BYTES`]。
    pub const FRAME_TOO_LARGE: &str = "FRAME_TOO_LARGE";
    /// 首条消息不是 `hello`。
    pub const HELLO_REQUIRED: &str = "HELLO_REQUIRED";
    /// 协议版本不一致。
    pub const PROTOCOL_MISMATCH: &str = "PROTOCOL_MISMATCH";
    /// 令牌无效 / 骑手不存在。
    pub const UNAUTHORIZED: &str = "UNAUTHORIZED";
    /// 没绑定骑手却发了骑手动作。
    pub const NOT_A_RIDER: &str = "NOT_A_RIDER";
    /// 非控制端发了控制指令。
    pub const NOT_A_CONTROLLER: &str = "NOT_A_CONTROLLER";
    /// 该功能被配置关闭。
    pub const FEATURE_DISABLED: &str = "FEATURE_DISABLED";
    /// 这一单没挂附加任务，却发了任务对话。
    pub const NO_TASK: &str = "NO_TASK";
    /// 任务步骤已经全部走完。
    pub const TASK_FINISHED: &str = "TASK_FINISHED";
    /// 服务端内部错误。
    pub const INTERNAL: &str = "INTERNAL";
}

/// 客户端角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// 真人骑手，只看得到自己。
    Rider(RiderId),
    /// 控制端，看得到全部（含隐藏属性）。
    Control,
    /// 观战，只看公开信息（不含任何收入）。
    Spectator,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Rider(_) => "rider",
            Role::Control => "control",
            Role::Spectator => "spectator",
        }
    }

    pub fn rider_id(self) -> Option<RiderId> {
        match self {
            Role::Rider(id) => Some(id),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::event::SimEvent;

    #[test]
    fn client_messages_parse_from_json() {
        let m: ClientMessage = serde_json::from_str(r#"{"type":"claim","order_id":7}"#).unwrap();
        assert!(matches!(
            m,
            ClientMessage::Claim {
                order_id: 7,
                back_order: false
            }
        ));

        let m: ClientMessage = serde_json::from_str(r#"{"type":"hello"}"#).unwrap();
        assert!(matches!(
            m,
            ClientMessage::Hello {
                token: None,
                protocol_version: None
            }
        ));

        let m: ClientMessage =
            serde_json::from_str(r#"{"type":"set_online","online":true}"#).unwrap();
        assert!(matches!(m, ClientMessage::SetOnline { online: true }));
    }

    #[test]
    fn set_config_is_a_flat_partial_patch() {
        let m: ClientMessage = serde_json::from_str(
            r#"{"type":"set_config","claim_window_secs":5,"ai_batch_enabled":false}"#,
        )
        .unwrap();
        let ClientMessage::SetConfig(p) = m else {
            panic!("应解析为 set_config");
        };
        assert_eq!(p.claim_window_secs, Some(5));
        assert_eq!(p.ai_batch_enabled, Some(false));
        assert_eq!(p.sim_rider_count, None);

        let m: ClientMessage = serde_json::from_str(r#"{"type":"set_config"}"#).unwrap();
        assert!(matches!(m, ClientMessage::SetConfig(_)));
    }

    #[test]
    fn realtime_rename_and_llm_messages_parse() {
        let m: ClientMessage =
            serde_json::from_str(r#"{"type":"set_realtime","enabled":true,"wall_minute":870}"#)
                .unwrap();
        let ClientMessage::SetRealtime {
            enabled,
            wall_minute,
        } = m
        else {
            panic!("应解析为 set_realtime");
        };
        assert!(enabled);
        assert_eq!(wall_minute, Some(870));

        // wall_minute 可以缺省，由服务端兜底。
        let m: ClientMessage =
            serde_json::from_str(r#"{"type":"set_realtime","enabled":false}"#).unwrap();
        assert!(matches!(
            m,
            ClientMessage::SetRealtime {
                enabled: false,
                wall_minute: None
            }
        ));

        let m: ClientMessage =
            serde_json::from_str(r#"{"type":"rename_rider","rider_id":3,"name":"张三"}"#).unwrap();
        assert!(matches!(m, ClientMessage::RenameRider { rider_id: 3, .. }));

        let m: ClientMessage =
            serde_json::from_str(r#"{"type":"set_rider_names","names":["甲","乙"]}"#).unwrap();
        let ClientMessage::SetRiderNames { names } = m else {
            panic!("应解析为 set_rider_names");
        };
        assert_eq!(names, vec!["甲".to_string(), "乙".to_string()]);

        // 只改模型：api_key 缺省 = 不动，不能被解析成空串。
        let m: ClientMessage =
            serde_json::from_str(r#"{"type":"set_llm_config","model":"deepseek-chat"}"#).unwrap();
        let ClientMessage::SetLlmConfig {
            api_key,
            base_url,
            model,
        } = m
        else {
            panic!("应解析为 set_llm_config");
        };
        assert_eq!(api_key, None);
        assert_eq!(base_url, None);
        assert_eq!(model.as_deref(), Some("deepseek-chat"));
    }

    #[test]
    fn unknown_message_type_is_an_error_not_a_panic() {
        let r: Result<ClientMessage, _> = serde_json::from_str(r#"{"type":"nope"}"#);
        assert!(r.is_err());
    }

    #[test]
    fn envelope_reuses_sim_event_tag_and_flattens_payload() {
        let ev = SimEvent::OrderExpired { order_id: 5 };
        let env = Envelope::from_event(&ev);
        assert_eq!(env.event, "order_expired");
        assert_eq!(env.payload["order_id"], 5);
    }

    #[test]
    fn ack_error_carries_a_fixed_code() {
        let a = Ack::err("claim", code::NOT_A_RIDER, "观战者不能抢单");
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["code"], "NOT_A_RIDER");
        assert_eq!(v["action"], "claim");
    }

    #[test]
    fn role_strings_are_stable() {
        assert_eq!(Role::Rider(3).as_str(), "rider");
        assert_eq!(Role::Control.as_str(), "control");
        assert_eq!(Role::Spectator.as_str(), "spectator");
        assert_eq!(Role::Rider(9).rider_id(), Some(9));
        assert_eq!(Role::Control.rider_id(), None);
    }

    #[test]
    fn protocol_version_comes_from_the_crate_root() {
        assert_eq!(PROTOCOL_VERSION, crate::PROTOCOL_VERSION);
    }
}
