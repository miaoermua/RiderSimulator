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

use std::collections::BTreeMap;

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
    /// 三种进法，优先级从高到低：
    ///
    /// 1. **骑手名 + 密码** → 账号登录。烽火台建过号才能进（见
    ///    [`crate::net::join`]）。这是给骑手用的正门。
    /// 2. **`token`** → 开发后门，见下。给了 `rider_name` 时它被忽略。
    /// 3. **都不给** → `DEV_ALLOW_CONTROL` 开着就是控制端，否则观战。
    ///
    /// 令牌语义：`rider:3` / `r3` 绑号、`new:<昵称>` 现场建号、
    /// `control:<令牌>` 控制端。**后门只在开发/演示用**——它绕过账号体系，
    /// 公网部署要靠 `RIDER_SIM_CONTROL_TOKEN` 之外的开关关掉。
    Hello {
        /// 骑手令牌（开发后门）。
        #[serde(default)]
        token: Option<String>,
        /// 骑手名字，也就是烽火台建的账号名。给了它就按账号登录。
        #[serde(default)]
        rider_name: Option<String>,
        /// 明文密码。只在 `rider_name` 一起给时才有意义。
        ///
        /// 它在网线上是什么状态取决于 `ws://` 还是 `wss://`——**局域网明文
        /// 是接受的取舍，公网部署必须套 TLS**，否则等于把密码广播出去。
        #[serde(default)]
        password: Option<String>,
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
    /// 拒单。
    ///
    /// 代价取决于这张单的来历：拒绝一张**还没接受的系统指派单**只吃当日
    /// 拒单额度（`max_rejects_per_day`），额度用完会被 `REJECT_LIMIT_REACHED`
    /// 挡回来；取消一张**已经接下的单**照旧罚 `fine.cancel_cents`。
    Reject { order_id: OrderId },
    /// 接受一张系统强制指派的单。
    ///
    /// 订单在指派那一刻就已经挂到骑手身上了（世界状态必须立刻自洽），
    /// 所以这条指令**不改调度状态**，只是把「待确认」标记摘掉——
    /// 从那以后这单就是骑手自己接的，再取消就得罚款。
    AcceptAssign { order_id: OrderId },
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

    /// 改自己的密码。旧密码必须对——**光连上了不足以改密**，
    /// 否则捡到一台没锁屏的手机就能改掉号。
    ChangePassword {
        old_password: String,
        new_password: String,
    },

    /// 骑手在任务对话里说一句话。顾客回复由服务端（LLM 或关键词）生成。
    TaskMessage { order_id: OrderId, text: String },

    /// 骑手在**订单会话**里打一句话（末端交付沟通）。
    ///
    /// 与 [`ClientMessage::TaskMessage`] 的区别：任务对话绑在「附加任务」上，
    /// 没挂任务的单会被直接拒掉；这里是**订单级**会话，任何一单在末端窗口
    /// （到达顾客后的 5–60 模拟分钟）里都能说话。
    ///
    /// 写回分两路：骑手这句立刻广播（`chat_message`，`from = "rider"`），
    /// 顾客的回应 / 索赔稍后由主循环裁决后再广播。**顾客是否开口由服务端决定**，
    /// 骑手说了话不等于顾客一定回。
    ChatSay { order_id: OrderId, text: String },

    /// 骑手点一条**预设话术**（不想打字时用）。
    ///
    /// 文本由服务端按 [`ChatPreset::text`] 补全，客户端只传枚举名——
    /// 「骑手说了什么」永远可审计，也不会因为客户端版本不同冒出两套话术。
    ChatPreset {
        order_id: OrderId,
        preset: ChatPreset,
    },

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
        /// 分层模型：`{"rider":"gpt-4o-mini","director":"gpt-4o"}`。
        ///
        /// 缺省 = 不改；某层传空串 = 该层回退到全局模型（不是「清空所有层」）。
        #[serde(default)]
        layer_models: Option<BTreeMap<String, String>>,
    },
    /// 问上游要一份可用模型清单（控制台的模型下拉靠它）。
    ///
    /// 只读元数据、不花 token，所以 LLM 没启用时也可以问。
    ListModels,
    /// 拉一份三层 AI 状态（控制台 AI Tab 打开时用）。
    AiQuery,
    /// 改三层 AI 开关与托管模式。字段缺省 = 不改。
    SetAi(AiPatch),
    /// 人工否决一条 AI 决策。
    RejectAi { decision_id: u64 },

    /// 临时插一张订单（调试 / 演示用）。
    ///
    /// 坐标是**控制台底图坐标系**下的值（= 当前 `coord_system`），
    /// 服务端入口就换算回内部的 GCJ-02——两个坐标系的换算只在这一处入口
    /// 发生，不让它渗进业务层。
    CreateOrder {
        merchant_name: String,
        merchant_lat: f64,
        merchant_lng: f64,
        #[serde(default)]
        customer_name: Option<String>,
        #[serde(default)]
        customer_address: Option<String>,
        customer_lat: f64,
        customer_lng: f64,
        /// 这张单的报价（分）。给了就**直接**当最终报价，不给才按计价规则算。
        /// 上界在服务端夹取，免得手滑输个天文数字把流水玩坏。
        #[serde(default)]
        fee_cents: Option<i64>,
    },

    /// 把某个骑手的钱包清零（余额、当日流水、罚款、保险、欠款、账单）。
    ///
    /// 纯给控制端「测试把账弄乱了，重来」用的，**不做任何业务校验**——
    /// 二次确认的责任在控制台那一侧。
    ResetRiderWallet { rider_id: RiderId },
}

/// 骑手端的**预设话术**。末端交付沟通里最常见的那几句。
///
/// **文本在服务端补全**：客户端只发枚举名，服务端查表拿文本。这样既保证
/// 「骑手说了什么」永远可审计（不会有人把任意字符串伪装成"预设"），
/// 也不会因为客户端版本不同而出现两套话术。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatPreset {
    /// 放门口。
    LeaveAtDoor,
    /// 放前台 / 门卫 / 指定位置。
    LeaveAtDesk,
    /// 当面交付，我在门口等。
    HandToMe,
    /// 到了，电话联系。
    CallMe,
    /// 路上耽搁了，抱歉。
    SorryLate,
    /// 主动报备餐品有损，问怎么处理。
    ReportDamage,
}

impl ChatPreset {
    /// 全部选项。客户端渲染预设按钮、控制台做对照表都用它，**顺序即展示顺序**。
    pub const ALL: [ChatPreset; 6] = [
        ChatPreset::LeaveAtDoor,
        ChatPreset::LeaveAtDesk,
        ChatPreset::HandToMe,
        ChatPreset::CallMe,
        ChatPreset::SorryLate,
        ChatPreset::ReportDamage,
    ];

    /// 对外短码（事件 / 审计日志里记这个，不记中文）。
    pub fn key(self) -> &'static str {
        match self {
            ChatPreset::LeaveAtDoor => "leave_at_door",
            ChatPreset::LeaveAtDesk => "leave_at_desk",
            ChatPreset::HandToMe => "hand_to_me",
            ChatPreset::CallMe => "call_me",
            ChatPreset::SorryLate => "sorry_late",
            ChatPreset::ReportDamage => "report_damage",
        }
    }

    /// 按钮上的短标签。
    pub fn label(self) -> &'static str {
        match self {
            ChatPreset::LeaveAtDoor => "放门口",
            ChatPreset::LeaveAtDesk => "放前台",
            ChatPreset::HandToMe => "当面交付",
            ChatPreset::CallMe => "电话联系",
            ChatPreset::SorryLate => "抱歉晚了",
            ChatPreset::ReportDamage => "餐品有损",
        }
    }

    /// 骑手实际"说"出来的那句话。**这是唯一的文本来源。**
    pub fn text(self) -> &'static str {
        match self {
            ChatPreset::LeaveAtDoor => "您好，餐给您放门口了，麻烦您取一下。",
            ChatPreset::LeaveAtDesk => "您好，餐放到前台了，麻烦您取一下。",
            ChatPreset::HandToMe => "您好，我到门口了，当面交给您。",
            ChatPreset::CallMe => "您好，我到楼下了，方便的话我给您打个电话。",
            ChatPreset::SorryLate => "实在抱歉，路上耽搁了，餐马上送到。",
            ChatPreset::ReportDamage => "不好意思，路上有点颠簸，餐可能洒了一点，您看怎么处理？",
        }
    }
}

/// 三层 AI 的开关补丁。**每个字段都可选**，缺省 = 这一项不动。
///
/// 与 `ConfigPatch` 一样，越界 / 认不出的值在服务端被拒或夹取，协议层不做判断。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AiPatch {
    /// 骑手层开关。
    #[serde(default)]
    pub rider: Option<bool>,
    /// 站长层开关。站长指令搭骑手层下发（`station_ops`），自己不单独发请求：
    /// 打开它只是让「站长该决策了」这一轮到点时也顺带跑一轮骑手。
    #[serde(default)]
    pub station: Option<bool>,
    /// 烽火台层开关。
    #[serde(default)]
    pub director: Option<bool>,
    /// 「允许调用 LLM」总开关。
    #[serde(default)]
    pub enabled: Option<bool>,
    /// 托管模式：`simulated_only` | `auto_pilot` | `off`。
    #[serde(default)]
    pub host_mode: Option<String>,
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
    /// 每日拒单额度，0 = 不限。
    #[serde(default)]
    pub max_rejects_per_day: Option<u32>,
    /// 下发坐标的坐标系。内部始终 GCJ-02，这里只影响出口转换。
    #[serde(default)]
    pub coord_system: Option<CoordSystem>,

    // ---- 合并单 / 凑单 ----
    /// 算法一批里给一个骑手最多派几单，1~3。
    #[serde(default)]
    pub merge_max_per_rider: Option<u32>,
    /// 单个商家同时存在的未完成单量上限，0 = 不限（夹到 0~6）。
    #[serde(default)]
    pub merge_merchant_cap: Option<u32>,
    /// 凑单窗口（分钟），0 = 不等（夹到 0~10）。
    #[serde(default)]
    pub merge_wait_min: Option<u32>,

    // ---- 事故 / 情绪 ----
    /// 事故总开关。
    #[serde(default)]
    pub accident_enabled: Option<bool>,
    /// 事故率全局倍率，夹到 0~3（1.0 = 设计值）。
    #[serde(default)]
    pub accident_rate_multiplier: Option<f32>,
    /// 情绪（懒惰）总开关。
    #[serde(default)]
    pub mood_enabled: Option<bool>,
    /// 情绪见底时接单门槛倍率，夹到 1~2（1.0 = 情绪不影响接单）。
    #[serde(default)]
    pub mood_lazy_threshold_max: Option<f32>,

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
    /// 令牌无效 / 骑手不存在 /**密码不对**。
    pub const UNAUTHORIZED: &str = "UNAUTHORIZED";
    /// 骑手还没有账号——得先去烽火台建号。
    ///
    /// 与 `UNAUTHORIZED` 分开，是为了让骑手分得清「密码打错了」和
    /// 「我还没被建号」：后者该去找管理端，不是接着试密码。
    pub const NO_ACCOUNT: &str = "NO_ACCOUNT";
    /// 名字或密码不合规范（长度、字符）。密码太短走这里。
    pub const BAD_CREDENTIALS: &str = "BAD_CREDENTIALS";
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
                rider_name: None,
                password: None,
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
            layer_models,
        } = m
        else {
            panic!("应解析为 set_llm_config");
        };
        assert_eq!(api_key, None);
        assert_eq!(base_url, None);
        assert_eq!(model.as_deref(), Some("deepseek-chat"));
        assert_eq!(layer_models, None, "没提分层模型就不该动它");

        // 分层模型：只动提到的那几层。
        let m: ClientMessage = serde_json::from_str(
            r#"{"type":"set_llm_config","layer_models":{"rider":"gpt-4o-mini","director":""}}"#,
        )
        .unwrap();
        let ClientMessage::SetLlmConfig {
            api_key,
            layer_models,
            ..
        } = m
        else {
            panic!("应解析为 set_llm_config");
        };
        assert_eq!(api_key, None, "只改分层模型不该顺手清掉 Key");
        let layers = layer_models.expect("该带上分层模型");
        assert_eq!(layers.get("rider").map(String::as_str), Some("gpt-4o-mini"));
        assert_eq!(
            layers.get("director").map(String::as_str),
            Some(""),
            "空串是「这层回退全局模型」，必须原样传下去"
        );
    }

    #[test]
    fn parses_list_models() {
        let m: ClientMessage = serde_json::from_str(r#"{"type":"list_models"}"#).unwrap();
        assert!(matches!(m, ClientMessage::ListModels));
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
