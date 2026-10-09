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

/** 坐标系。方案 A：服务端内部保持 GCJ-02，只在下发快照时按需转换。 */
export type CoordSystem = "gcj02" | "wgs84";

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
  held: number[];
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
  backorder_enabled: boolean;
  backorder_max: number;
  max_orders_per_day: number;
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

export interface ServerInfo {
  llm_enabled?: boolean;
  llm_configured?: boolean;
  llm_base_url?: string;
  llm_model?: string;
  brain_note?: string;
  brain_calls_today?: number;
  brain_max_calls_per_day?: number;
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
  ai_decision: { source: string; note?: string };
  task_message: { order_id: number; from: string; text: string };
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
