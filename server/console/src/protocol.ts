/**
 * 线与前端之间的类型契约。
 *
 * 权威定义在 `server/src/net/protocol.rs`，摘要见 `packages/protocol.json`。
 * 这里只描述控制台实际用到的字段；服务端新增字段时不必同步改这里。
 */

export const PROTOCOL_VERSION = "1.0";

export type Role = "control" | "spectator" | "rider";
export type RiderKind = "simulated" | "human";
export type SpeedMode = "compliant" | "tolerant" | "unrestricted";
export type Phase = "to_shop" | "waiting_food" | "to_customer" | "arrived_customer";
export type Weather =
  | "clear"
  | "cloudy"
  | "overcast"
  | "fog"
  | "light_rain"
  | "moderate_rain"
  | "heavy_rain"
  | "windy"
  | "snow"
  | "rainstorm";
export type OrderStatus =
  | "pending"
  | "assigned"
  | "picked_up"
  | "delivered"
  | "late"
  | "expired"
  | "cancelled";

/**
 * 一条决策的来源。
 *
 * **「AI 决策」与「LLM 决策」是两个不同的概念**：只有 `llm` 是真的问了模型，
 * 其余都是纯算法 / 人工（`algorithm_rescue` 是超时救单，也是算法）。
 */
export type DecisionSource =
  | "algorithm"
  | "llm"
  | "algorithm_fallback"
  | "algorithm_rescue"
  | "manual"
  | "ai_hosted";

/** 骑手层大脑一轮结果的来源（比 `DecisionSource` 多出缓存 / 跳过态）。 */
export type BrainSource = "llm" | "cache" | "disabled" | "throttled" | "fallback";

/** 坐标系。方案 A：服务端内部保持 GCJ-02，只在下发快照时按需转换。 */
export type CoordSystem = "gcj02" | "wgs84";

/** 路线任务里的一腿。`held` 只说手上有哪几张单，看不出先后，这里才有顺序。 */
export interface RouteLegBrief {
  order_id: number;
  kind: "pickup" | "dropoff";
  /** 这一腿还要多少分钟：取餐腿是等餐时间，送达腿是骑行时间。 */
  remaining_min: number;
  /** 是不是当前这一腿（队首）。 */
  current: boolean;
}

export interface RiderBrief {
  id: number;
  key: string;
  name: string;
  kind: RiderKind;
  online: boolean;
  injured: boolean;
  frozen: boolean;
  speed_mode: SpeedMode;
  level: number;
  rating: number;
  completed: number;
  /** 今日已用掉的拒单次数。配合 `ConfigView.max_rejects_per_day` 显示额度。 */
  rejects_today: number;
  held: number[];
  /** 路线任务：队首是当前腿，走完出队。旧存档 / 没接单的骑手是空数组。 */
  route: RouteLegBrief[];
  phase: Phase | null;
  lat: number;
  lng: number;
  /** 以下收入字段只对本人 / 控制端下发，其余角色为 null。 */
  hourly_wage_cents: number | null;
  takehome_cents: number | null;
  cash_cents: number | null;
  fines_cents: number | null;
  /** 隐藏属性，只给控制端。 */
  fatigue: number | null;
  /** 情绪 0~1，只给控制端。低于配置里的 `lazy_below` 就开始偷懒。 */
  mood: number | null;
  /** 情绪是不是已经低到在偷懒了（挑单 + 骑得慢），只给控制端。 */
  slacking: boolean;
  expected_value_cents: number | null;
}

/** 单笔订单的计价明细。服务端全量下发，控制台据此把「单价」摊开给人看。 */
export interface FeeBreakdown {
  base_fee_cents: number;
  distance_extra_cents: number;
  rain_bonus_cents: number;
  level_bonus_cents: number;
  /** 高峰系数，已乘进 `quoted_fee_cents`。 */
  surge_multiplier: number;
  commission_cents: number;
  quoted_fee_cents: number;
  takehome_cents: number;
}

/** 计价规则快照，控制台「计价」面板据此渲染。 */
export interface FeeView {
  /** 起步价（分），含第一档里程。 */
  base_fee_cents: number;
  /** 里程阶梯：(距离下界米, 该档总附加费分)。取不超过本单距离的最大门槛。 */
  distance_tiers: [number, number][];
  /** 超出最后一档后每公里单价（分/km）。 */
  per_km_over_cents: number;
  /** 等级加成（分/级）。 */
  level_bonus_per_level_cents: number;
  /** 当前时段高峰系数。 */
  surge_multiplier: number;
  /** 订单重投上限与每次加价，用来解释「为什么这单变贵了」。 */
  max_redispatch_attempts: number;
  redispatch_surcharge_step_cents: number;
}

export interface ExtraTask {
  name: string;
}

export interface Order {
  id: number;
  merchant_id: number;
  merchant_name: string;
  merchant_lat: number;
  merchant_lng: number;
  customer_name: string;
  customer_address: string;
  remark: string;
  customer_lat: number;
  customer_lng: number;
  distance_m: number;
  promised_minutes: number;
  tier: string;
  created_at_min: number;
  claim_deadline_min: number;
  fee: FeeBreakdown;
  /** 退回大厅重投的次数：没人接、或骑手取消都会累加。累到上限才判流单。 */
  redispatch_count: number;
  /** 重投叠加的调度补贴（分），全给骑手、不抽水。 */
  surcharge_cents: number;
  status: OrderStatus;
  assigned_to: number | null;
  forced: boolean;
  back_order: boolean;
  task: ExtraTask | null;
  urge_count: number;
  prep_minutes: number | null;
}

export interface MerchantBrief {
  id: number;
  name: string;
  address: string;
  lat: number;
  lng: number;
  rating: number;
  signature: string;
  avg_prep_min: number;
}

export interface SimStats {
  orders_created: number;
  orders_expired: number;
  orders_delivered: number;
  orders_late: number;
  claims: number;
  accidents: number;
  camera_fines: number;
  llm_calls: number;
  llm_fallbacks: number;
}

export interface ConfigView {
  seed: number;
  speed_multiplier: number;
  paused: boolean;
  realtime: boolean;
  tick_interval_ms: number;
  commission_rate: number;
  discriminative_commission: boolean;
  claim_window_secs: number;
  allow_speeding: boolean;
  allow_unrestricted: boolean;
  /** 允许真人骑手做违规操作（没过到位置就点「到店 / 送达」）。 */
  allow_rider_violation: boolean;
  backorder_enabled: boolean;
  backorder_max: number;
  max_orders_per_day: number;
  /** 每日拒单额度。**0 = 不限**。拒完额度后指派单就拒不掉了。 */
  max_rejects_per_day: number;
  ai_batch_enabled: boolean;
  llm_brain_enabled: boolean;
  show_expected_value: boolean;
  sim_rider_count: number;
  is_peak: boolean;
  surge_multiplier: number;
  allowed_speed_modes: SpeedMode[];
  /** 计价规则（单价系统）。 */
  fee: FeeView;
  insurance_per_day_cents: number;
  backorder_late_buffer_min: number;
  /** 算法一批里给一个骑手最多派几单（1~3）。 */
  merge_max_per_rider: number;
  /** 单个商家同时存在的未完成单量上限，0 = 不限。 */
  merge_merchant_cap: number;
  /** 凑单窗口（分钟）：在店等餐的骑手愿意为同店新单多等多久。 */
  merge_wait_min: number;
  /** 事故总开关。 */
  accident_enabled: boolean;
  /** 事故率全局倍率，1.0 = 设计值（0.5% / 4% / 22%）。 */
  accident_rate_multiplier: number;
  /** 情绪（懒惰）总开关。 */
  mood_enabled: boolean;
  /** 情绪见底时接单门槛最多 ×几（懒惰的力度）。 */
  mood_lazy_threshold_max: number;
  /** 坐标系：方案 A 里由控制端设置，服务端下发快照时据此转换。 */
  coord_system: CoordSystem;
}

export interface Snapshot {
  day: number;
  sim_minute: number;
  elapsed_min: number;
  weather: Weather;
  weather_label: string;
  weather_speed_factor: number;
  rain_bonus_cents: number;
  config: ConfigView;
  riders: RiderBrief[];
  hall: Order[];
  active_orders: Order[];
  merchants: MerchantBrief[];
  stats: SimStats;
  claim_window_ms: number;
}

export interface Tick {
  day: number;
  sim_minute: number;
  elapsed_min: number;
  speed_multiplier: number;
}

/** 三层 AI 的一层。`layer` 同时也是 `AiDecision.layer` 的取值。 */
export type AiLayerKey = "rider" | "station" | "director";

/** 托管模式：真人骑手在线 / 掉线时，AI 能不能代管。 */
export type HostMode = "off" | "simulated_only" | "auto_pilot";

export interface AiLayerState {
  layer: AiLayerKey;
  /** 这一层是否参与调度。 */
  enabled: boolean;
  /** 最短调用间隔（模拟分钟）。 */
  interval_min: number;
  calls: number;
  failures: number;
  /** 校验通过、真正落地的条数。 */
  applied: number;
  /** 被校验丢掉的条数。 */
  dropped: number;
  /** 回退到算法的次数。 */
  fallbacks: number;
}

export interface AiStationKpi {
  completed: number;
  late_count: number;
  /** 0~1。 */
  on_time_rate: number;
  avg_hourly_cents: number;
  active_riders: number;
  total_riders: number;
  backlog: number;
  avg_wait_min: number;
}

export interface AiState {
  llm_configured: boolean;
  llm_model: string;
  llm_base_url: string;
  brain_enabled: boolean;
  host_mode: HostMode;
  /** 每层实际用的模型（成本优化：高频层可以配便宜模型）。 */
  models: Record<string, string>;
  /** 状态指纹缓存。命中率越高越省钱。 */
  cache: { hits: number; calls: number; hit_rate: number };
  budget: {
    calls_today: number;
    max_calls_per_day: number;
    /** 在飞的那一层；没有则 null。 */
    inflight: AiLayerKey | null;
  };
  layers: AiLayerState[];
  station: {
    id: number;
    name: string;
    rider_count: number;
    kpi: AiStationKpi;
  };
  /** 还能被否决的那一条决策；没有则 null。 */
  rejectable: { decision_id: number; layer: string } | null;
  reject_log: Array<{ decision_id: number; layer: string; result: string }>;
}

/** 上游给出的一个可用模型（`GET {base}/models`）。控制台的模型下拉靠它。 */
export interface ModelInfo {
  /** 填进 `model` / `layer_models` 的就是它。 */
  id: string;
  /** 归属方。OpenAI 兼容服务多半是组织名，本地服务通常不给。 */
  owned_by?: string;
  created?: number;
}

/** 一条 AI 运营指令的人类可读摘要。 */
export interface AiOpBrief {
  op: string;
  target: string;
  detail: string;
  why: string;
}

export interface ServerInfo {
  llm_enabled?: boolean;
  llm_configured?: boolean;
  llm_base_url?: string;
  llm_model?: string;
  brain_note?: string;
  brain_calls_today?: number;
  brain_max_calls_per_day?: number;
  /** 三层 AI 的统一状态（控制台 AI Tab 用）。 */
  ai?: AiState;
  hub?: Record<string, unknown>;
  [k: string]: unknown;
}

export interface AckPayload {
  action: string;
  ok: boolean;
  code?: string;
  message?: string;
  data?: Record<string, unknown>;
}

// ---------------- 骑手账号（HTTP `/api/accounts`） ----------------
//
// 账号是**管理动作**，走一次性 HTTP 而不是 16ms 的 WS 帧流：服务端把明文
// 密码只放在那一份响应里（库里只有 Argon2 哈希），WS 那条路上根本没有它。

/** 一条账号记录。**故意没有密码字段**——哈希只在服务端单独取。 */
export interface Account {
  /** 账号名 = 骑手名。 */
  name: string;
  /** 建号时间（Unix 秒）。 */
  created_at: number;
  /** 最后一次改密时间（Unix 秒）。 */
  updated_at: number;
  /** 这一局里有没有对应的真人身体；没登录过就是 null。 */
  rider_id: number | null;
  rider_key: string | null;
  /** 有身体时才有值。 */
  online: boolean | null;
}

/**
 * 一条「扫码即登录」的全部材料。
 *
 * `qr_svg` 是服务端画好的 SVG 文本——控制台不用再引二维码库。
 * `uri` 里带着**明文密码**，见 `server/src/net/join.rs` 的取舍说明。
 */
export interface JoinInfo {
  ws_url: string;
  /** `ridersim://login?host=&port=&name=&password=` 连接串。二维码内容就是它。 */
  uri: string;
  /** 画不出来时为 null（内容编不成码，极少见）。 */
  qr_svg: string | null;
}

/** 建号 / 改密的响应。`password` 是**全系统唯一一次**能看到明文的地方。 */
export interface AccountJoin {
  name: string;
  password: string;
  join: JoinInfo | null;
  hosts: string[];
  primary_host: string | null;
}

/** `GET /api/accounts`：名单 + 手机可达地址。 */
export interface AccountList {
  accounts: Account[];
  hosts: string[];
  primary_host: string | null;
}

/** 服务端事件的载荷形状，按事件名索引。 */
export interface EventPayloads {
  welcome: {
    role: Role;
    rider_id: number | null;
    tick_interval_ms: number;
    snapshot?: Snapshot;
  };
  snapshot: Snapshot;
  server_info: ServerInfo;
  tick: Tick;
  rider_state_changed: { rider_id: number; rider: Partial<RiderBrief> | null };
  order_broadcast: { order: Order; window_ms: number; expires_in_ms: number };
  order_claimed: {
    order_id: number;
    rider_id: number;
    rider_name: string;
    quoted_fee_cents: number;
    forced: boolean;
  };
  order_expired: { order_id: number };
  /**
   * 改派：把**还没取到餐**的单从原骑手手上转给另一个骑手。
   *
   * 服务端只推给控制端和两位当事人——对第三人来说这单从头到尾都在别人手上。
   */
  order_reassigned: {
    order_id: number;
    from_rider_id: number;
    from_rider_name: string;
    to_rider_id: number;
    to_rider_name: string;
    /** 改派那一刻原骑手卡在哪一段：`to_shop`（去店路上）/ `waiting_food`（店里等餐）。 */
    phase: Phase;
  };
  /** 骑手拒掉了一张还没确认的指派单。只推给本人和控制端。 */
  assign_rejected: {
    rider_id: number;
    order_id: number;
    /** 含这一次，今日已用掉的额度。 */
    used: number;
    /** 上限；`0` = 不限。 */
    limit: number;
  };
  order_picked_up: { order_id: number; rider_id: number; prep_minutes: number };
  order_delivered: {
    order_id: number;
    rider_id: number;
    rider_name: string;
    late: boolean;
    takehome_cents: number;
    hourly_wage_cents: number;
  };
  weather_changed: { label: string; speed_factor: number; rain_bonus_cents: number };
  fine_issued: { rider_id: number; kind: string; amount_cents: number; reason: string };
  config_changed: { speed_multiplier: number; commission_rate: number; weather: Weather };
  ai_decision: {
    source: DecisionSource;
    /** 哪一层做的决策：`rider` | `station` | `director` | `algorithm` | `manual`。 */
    layer?: string;
    assignments?: Array<Record<string, unknown>>;
    /** 站长 / 烽火台的运营指令摘要。 */
    ops?: AiOpBrief[];
    /** 决策 id，人工否决时回传它。`0` = 不可否决。 */
    decision_id?: number;
    note?: string;
  };
  /** 三层 AI 状态（控制端专用，随 server_info 与 ai_query 下发）。 */
  ai_state: AiState;
  /** 上游模型清单（回给点名要它的那条连接）。 */
  ai_models: {
    ok: boolean;
    count: number;
    /** `ok: false` 时为空数组。 */
    models?: ModelInfo[];
    /** `ok: false` 时的失败原因。 */
    message?: string;
    /** 当前三层各用什么，供下拉框回显。 */
    current?: { global: string; layers: Record<string, string> };
  };
  /** 骑手层一轮 LLM 决策（控制端专用，比 ai_decision 更详细）。 */
  llm_brain: {
    decision_id: number;
    layer: string;
    source: string;
    note: string;
    model?: string;
    latency_ms?: number;
    assignments?: number;
    dropped?: number;
    policies?: Array<Record<string, unknown>>;
    station_ops?: Array<Record<string, unknown>>;
    fingerprint?: string;
    cached?: boolean;
    prompt_bytes?: number;
    trim?: Record<string, unknown>;
    stats?: Record<string, unknown>;
  };
  /** 烽火台一轮 LLM 决策（控制端专用）。 */
  llm_world: {
    decision_id: number;
    layer: string;
    source: string;
    note: string;
    model?: string;
    latency_ms?: number;
    ops?: Array<Record<string, unknown>>;
    narrate?: string | null;
    dropped?: number;
  };
  /** AI 说的一句话（烽火台旁白等）。只影响气氛，不改数值。 */
  narration: { layer: string; text: string };
  task_message: { order_id: number; from: string; text: string };
  /**
   * 订单会话消息（末端交付沟通）。骑手与顾客说的话都走它，按 `order_id` 归并。
   *
   * 与 `task_message` 分开：那条绑在附加任务上，这条是**订单级**的，
   * 末端窗口里任何一单都可能有。时效一天，服务端日结时清库。
   */
  chat_message: {
    order_id: number;
    from: string;
    text: string;
    /** `preset` | `free` | `reply` | `ruling`。前端据此换头像 / 配色。 */
    kind: string;
    /** 归属天（0:00 起算的第几天）。时效按它算。 */
    day: number;
    /** 发生时的当日分钟（0..1439）。 */
    sim_minute: number;
    /** 这条消息带来的赔付（分）。恒 ≥ 0，`0` = 无经济后果。 */
    compensation_cents: number;
    /** `rider` | `merchant` | `none`。钱从谁身上出由它决定。 */
    fault: string;
  };
  day_closed: { day: number; total_orders: number; total_takehome_cents: number };
  ack: AckPayload;
  error: { code: string; message: string };
}

export type EventName = keyof EventPayloads;

/** 控制端 → 服务端的指令。宽松类型：服务端只认它认识的字段。 */
export type ClientCommand = Record<string, unknown> & { type: string };

export interface Envelope<K extends EventName = EventName> {
  event: K;
  payload: EventPayloads[K];
}

/**
 * 这一条来源**是否真的来自 LLM**。
 *
 * `cache` 也算：指纹命中的同样是模型的决策，只是这一轮没再花钱。
 * 其余（算法、兜底、救单、人工、托管）一律不算。
 */
export function isLlmSource(source: string): boolean {
  return source === "llm" || source === "cache";
}

/** 来源的中文名。界面上不该出现机器名。 */
export const SOURCE_LABEL: Record<string, string> = {
  llm: "LLM",
  cache: "LLM·缓存",
  algorithm: "算法",
  algorithm_fallback: "算法兜底",
  algorithm_rescue: "超时救单",
  manual: "人工指派",
  ai_hosted: "AI 托管",
  disabled: "未启用",
  throttled: "限流跳过",
};
