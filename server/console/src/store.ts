import {
  PROTOCOL_VERSION,
  type AiOpBrief,
  type AiState,
  type ConfigView,
  type EventName,
  type EventPayloads,
  type Role,
  type ServerInfo,
  type Snapshot,
  type Tick,
} from "./protocol";

export type ConnState = "connecting" | "online" | "offline";

/** 日志条目。原来的 `innerHTML` 拼接改成结构化数据，渲染交给 React 转义。 */
export interface LogItem {
  id: number;
  /** 模拟时钟 `hh:mm`。 */
  at: string;
  text: string;
  tag?: string;
  tagColor?: string;
}

/**
 * AI 决策流里的一条。由 `llm_brain` / `llm_world` / `ai_decision` 三种事件汇总而来，
 * 保证控制台只对着**一个**列表就能看清「AI 干了什么、为什么、还能不能撤」。
 */
export interface AiDecisionEntry {
  /** 前端自增序号，仅用于 React key。 */
  id: number;
  /** 服务端决策 id；`0` = 不可否决（算法兜底 / 手动指派）。 */
  decisionId: number;
  /** 模拟时钟 `hh:mm`。 */
  at: string;
  /** `rider` | `station` | `director` | `algorithm` | `manual`。 */
  layer: string;
  source: string;
  note: string;
  /** 站长 / 烽火台的运营指令。 */
  ops: AiOpBrief[];
  /** 骑手层派单条数。 */
  assignments: number;
  dropped: number;
  model?: string;
  latencyMs?: number;
  /** 状态指纹 + 是否命中缓存（成本优化的观测口）。 */
  fingerprint?: string;
  cached?: boolean;
  /** 烽火台旁白。 */
  narrate?: string;
  /** 本地标记：已被人工否决。 */
  rejected?: boolean;
}

export interface State {
  role: Role | null;
  conn: ConnState;
  snap: Snapshot | null;
  /** 最近一帧 tick。高频字段单独存，避免和快照混在一起。 */
  tick: Tick | null;
  info: ServerInfo | null;
  logs: LogItem[];
  lastError: string | null;
  /** 首屏是否已经按商户范围缩放过地图。 */
  mapFitted: boolean;
  /** 三层 AI 状态（控制台 AI Tab 用）。来自 `server_info.ai` 与 `ai_state`。 */
  ai: AiState | null;
  /** AI 决策流，新的在前。 */
  decisions: AiDecisionEntry[];
}

type Notifier = (kind: "success" | "error" | "info", text: string) => void;

const MAX_LOGS = 300;
const MAX_DECISIONS = 60;

const initialState: State = {
  role: null,
  conn: "offline",
  snap: null,
  tick: null,
  info: null,
  logs: [],
  lastError: null,
  mapFitted: false,
  ai: null,
  decisions: [],
};

/**
 * 全局控制台状态。
 *
 * 刻意**不用 React state**：WebSocket 在 60x 档下 16ms 就来一帧，
 * 每帧都走 React 的调度器会把整棵树拖垮。这里用「外部 store + 选择器订阅」，
 * 只有真正关心变化的那几个组件重渲染——地图和列表完全不受 tick 影响。
 */
class ConsoleStore {
  private state: State = initialState;
  private listeners = new Set<() => void>();
  private ws: WebSocket | null = null;
  private token = "";
  private retry = 0;
  private lastResync = 0;
  private logSeq = 0;
  private decisionSeq = 0;
  private notifier: Notifier = () => {};
  /** 组件挂载后由 antd 的 message API 覆盖。 */
  onRosterChanged: (() => void) | null = null;

  getState = (): State => this.state;

  subscribe = (fn: () => void): (() => void) => {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  };

  setNotifier(fn: Notifier) {
    this.notifier = fn;
  }

  clearError() {
    if (this.state.lastError !== null) this.set({ lastError: null });
  }

  private set(patch: Partial<State>) {
    this.state = { ...this.state, ...patch };
    for (const fn of this.listeners) fn();
  }

  // ---------------- 连接 ----------------

  connect(token: string) {
    this.token = token;
    if (this.ws) {
      this.ws.onclose = null;
      this.ws.close();
    }
    const proto = location.protocol === "https:" ? "wss" : "ws";
    const ws = new WebSocket(`${proto}://${location.host}/ws`);
    this.ws = ws;
    this.set({ conn: "connecting" });

    ws.onopen = () => {
      const hello: Record<string, unknown> = {
        type: "hello",
        protocol_version: PROTOCOL_VERSION,
      };
      if (this.token) hello.token = `control:${this.token}`;
      ws.send(JSON.stringify(hello));
    };
    ws.onmessage = (e) => {
      let msg: { event?: string; payload?: unknown };
      try {
        msg = JSON.parse(e.data as string);
      } catch {
        return;
      }
      if (!msg.event) return;
      this.onEvent(msg.event as EventName, (msg.payload ?? {}) as EventPayloads[EventName]);
    };
    ws.onclose = () => {
      this.set({ conn: "offline" });
      if (this.ws !== ws) return;
      this.retry = Math.min(this.retry + 1, 5);
      setTimeout(() => this.connect(this.token), 1000 * this.retry);
    };
  }

  /** 主动断开（鉴权失败时用）：连身份一起清掉，界面退回登录态。 */
  private drop() {
    if (this.ws) {
      this.ws.onclose = null;
      this.ws.close();
      this.ws = null;
    }
    this.set({ conn: "offline", role: null });
  }

  send(type: string, fields: Record<string, unknown> = {}) {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      this.notifier("error", "未连接");
      return;
    }
    this.ws.send(JSON.stringify({ type, ...fields }));
  }

  resync() {
    this.lastResync = performance.now();
    this.send("resync");
  }

  log(text: string, tag?: string, tagColor?: string) {
    const at = this.state.snap ? hhmm(this.state.snap.sim_minute) : "--:--";
    const item: LogItem = { id: ++this.logSeq, at, text, tag, tagColor };
    this.set({ logs: [item, ...this.state.logs].slice(0, MAX_LOGS) });
  }

  /** 往 AI 决策流里塞一条（新的在前，超出上限截断）。 */
  private pushDecision(entry: Omit<AiDecisionEntry, "id" | "at">) {
    const at = this.state.snap ? hhmm(this.state.snap.sim_minute) : "--:--";
    const item: AiDecisionEntry = { ...entry, id: ++this.decisionSeq, at };
    this.set({ decisions: [item, ...this.state.decisions].slice(0, MAX_DECISIONS) });
  }

  /** 请求一份三层 AI 状态（打开 AI Tab 时调）。 */
  queryAi() {
    this.send("ai_query");
  }

  /** 改三层开关 / 托管模式。只传要改的字段。 */
  setAi(patch: {
    rider?: boolean;
    station?: boolean;
    director?: boolean;
    enabled?: boolean;
    host_mode?: string;
  }) {
    this.send("set_ai", patch);
  }

  /** 人工否决一条决策。 */
  rejectAi(decisionId: number) {
    this.send("reject_ai", { decision_id: decisionId });
  }

  // ---------------- 事件分发 ----------------

  private onEvent<K extends EventName>(name: K, p: EventPayloads[K]) {
    switch (name) {
      case "welcome": {
        const w = p as EventPayloads["welcome"];
        this.retry = 0;
        this.set({ role: w.role, conn: "online" });
        if (w.role !== "control") {
          this.notifier(
            "error",
            "没拿到控制权，只能观战。检查令牌或 RIDER_SIM_ALLOW_CONTROL。",
          );
        }
        if (w.snapshot) this.applySnapshot(w.snapshot);
        break;
      }
      case "snapshot":
        this.applySnapshot(p as Snapshot);
        break;
      case "server_info": {
        const info = p as ServerInfo;
        // `server_info.ai` 与独立的 `ai_state` 是同一份口径，谁先到用谁。
        this.set({ info, ...(info.ai ? { ai: info.ai } : {}) });
        break;
      }
      case "ai_state":
        this.set({ ai: p as AiState });
        break;
      case "tick":
        // 高频帧**不写回快照**：否则每帧快照引用都变，
        // 订阅快照的地图与列表会跟着 16ms 重渲染一次。
        // 只更新 `tick`，需要当前时间的组件自己优先读它。
        this.set({ tick: p as Tick });
        // 骑手位置只在完整快照里，按墙钟节流拉一份。
        if (performance.now() - this.lastResync > 1000) this.resync();
        break;
      case "rider_state_changed": {
        const r = p as EventPayloads["rider_state_changed"];
        const snap = this.state.snap;
        if (!snap || !r.rider) break;
        const next = mergeRider(snap.riders, r.rider_id, stripNulls(r.rider));
        this.set({ snap: { ...snap, riders: next } });
        break;
      }
      case "order_broadcast": {
        const o = (p as EventPayloads["order_broadcast"]).order;
        this.log(`新单 #${o.id} ${o.merchant_name} → ${o.customer_address}，报价 ${yuan(o.fee.quoted_fee_cents)}`);
        break;
      }
      case "order_claimed": {
        const c = p as EventPayloads["order_claimed"];
        this.log(`#${c.order_id} 被 ${c.rider_name} ${c.forced ? "强制指派" : "抢到"}（${yuan(c.quoted_fee_cents)}）`);
        break;
      }
      case "order_expired":
        this.log(`#${(p as EventPayloads["order_expired"]).order_id} 无人接，流单`);
        break;
      case "order_picked_up": {
        const u = p as EventPayloads["order_picked_up"];
        this.log(`#${u.order_id} 已取货（出餐 ${u.prep_minutes} 分钟）`);
        break;
      }
      case "order_delivered": {
        const d = p as EventPayloads["order_delivered"];
        this.log(
          `#${d.order_id} ${d.rider_name} 送达${d.late ? "（超时）" : ""}，到手 ${yuan(d.takehome_cents)}`,
          d.late ? "超时" : undefined,
          // Tag 的 color 走 Ant Design 预置调色板名，不引自定义色值。
          d.late ? "red" : undefined,
        );
        break;
      }
      case "weather_changed":
        this.log(`天气变为 ${(p as EventPayloads["weather_changed"]).label}`);
        this.resync();
        break;
      case "fine_issued": {
        const f = p as EventPayloads["fine_issued"];
        this.log(`罚款 ${f.reason} -${yuan(f.amount_cents)}`, "罚款", "red");
        break;
      }
      case "config_changed":
        this.resync();
        break;
      case "ai_decision": {
        const a = p as EventPayloads["ai_decision"];
        if (a.note) this.log(a.note, a.source, "blue");
        // 站长 / 烽火台的运营指令走这里进决策流；算法兜底不进（噪音）。
        if (a.ops && a.ops.length > 0) {
          this.pushDecision({
            decisionId: a.decision_id ?? 0,
            layer: a.layer ?? "station",
            source: a.source,
            note: a.note ?? "",
            ops: a.ops,
            assignments: a.assignments?.length ?? 0,
            dropped: 0,
          });
        }
        break;
      }
      case "llm_brain": {
        const b = p as EventPayloads["llm_brain"];
        // 算法兜底不是 AI 决策，不进流（否则刷屏）。
        // 缓存命中要进 —— 它同样是 AI 的决策，只是这轮没再花钱。
        if (b.source === "fallback") break;
        this.pushDecision({
          decisionId: b.decision_id ?? 0,
          layer: b.layer ?? "rider",
          source: b.source,
          note: b.note ?? "",
          ops: [],
          assignments: b.assignments ?? 0,
          dropped: b.dropped ?? 0,
          model: b.model,
          latencyMs: b.latency_ms,
          fingerprint: b.fingerprint,
          cached: b.cached,
        });
        break;
      }
      case "llm_world": {
        const w = p as EventPayloads["llm_world"];
        const ops: AiOpBrief[] = (w.ops ?? []).map((o) => {
          const op = String(o.op ?? "?");
          return {
            op,
            target: String(o.v ?? o.text ?? "-"),
            detail: String(o.label ?? ""),
            why: String(o.why ?? ""),
          };
        });
        this.pushDecision({
          decisionId: w.decision_id ?? 0,
          layer: w.layer ?? "director",
          source: w.source,
          note: w.note ?? "",
          ops,
          assignments: 0,
          dropped: w.dropped ?? 0,
          model: w.model,
          latencyMs: w.latency_ms,
          narrate: w.narrate ?? undefined,
        });
        break;
      }
      case "narration": {
        const n = p as EventPayloads["narration"];
        this.log(`[${n.layer}] ${n.text}`, "旁白", "purple");
        break;
      }
      case "task_message": {
        const t = p as EventPayloads["task_message"];
        this.log(`任务 #${t.order_id} [${t.from}] ${t.text}`);
        break;
      }
      case "day_closed": {
        const d = p as EventPayloads["day_closed"];
        this.log(`第 ${d.day} 天结束：${d.total_orders} 单，骑手合计到手 ${yuan(d.total_takehome_cents)}`);
        break;
      }
      case "ack":
        this.onAck(p as EventPayloads["ack"]);
        break;
      case "error": {
        const e = p as EventPayloads["error"];
        this.set({ lastError: e.message || e.code });
        this.notifier("error", `${e.code}：${e.message}`);
        if (e.code === "UNAUTHORIZED") this.drop();
        break;
      }
      default:
        break;
    }
  }

  private onAck(a: EventPayloads["ack"]) {
    if (!a.ok) {
      this.notifier("error", `${a.action} 失败：${a.message || a.code}`);
      return;
    }
    // 人工否决：服务端认定「撤不下来」时 data.ok 为 false，前一条已经提示过了。
    if (a.action === "reject_ai") {
      const id = Number(a.data?.decision_id ?? 0);
      if (a.data?.rejected && id > 0) {
        this.set({
          decisions: this.state.decisions.map((d) =>
            d.decisionId === id ? { ...d, rejected: true } : d,
          ),
        });
      }
      this.resync();
      return;
    }
    // resync / ai_query 是内部动作，不该弹提示。
    if (a.action === "resync" || a.action === "ai_query") return;
    this.notifier("success", `${a.action} 成功`);
    // 控制类指令的 ack 里带回了配置片段（如倍率、实时档），
    // 就地合并能让顶栏立刻响应，不必等下一次快照。
    if (a.data && this.state.snap) {
      this.set({
        snap: {
          ...this.state.snap,
          config: mergeConfig(this.state.snap.config, a.data),
        },
      });
    }
    this.resync();
  }

  private applySnapshot(snap: Snapshot) {
    this.set({ snap, tick: null });
  }

  /** 地图首次适配范围后调用，避免每帧重新 fitBounds。 */
  markMapFitted() {
    if (!this.state.mapFitted) this.set({ mapFitted: true });
  }
}

function mergeRider(
  riders: Snapshot["riders"],
  id: number,
  patch: Partial<Snapshot["riders"][number]>,
): Snapshot["riders"] {
  return riders.map((r) => (r.id === id ? { ...r, ...patch } : r));
}

/** 只保留非 null 字段：服务端按角色过滤后为 null 的字段不该覆盖已知值。 */
function stripNulls<T extends object>(o: T): Partial<T> {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(o)) {
    if (v !== null && v !== undefined) out[k] = v;
  }
  return out as Partial<T>;
}

/**
 * 把 ack.data 里**确实属于配置**的字段并进 ConfigView。
 *
 * 用白名单（遍历 config 自己的 key）而不是直接展开 `data`：
 * ack 里常混着 `sim_minute`、`wall_minute` 这类运行态字段，不该污染配置。
 */
function mergeConfig(cfg: ConfigView, data: Record<string, unknown>): ConfigView {
  const out: Record<string, unknown> = { ...cfg };
  for (const k of Object.keys(cfg)) {
    if (k in data) out[k] = data[k];
  }
  return out as unknown as ConfigView;
}

export function hhmm(m: number): string {
  const h = Math.floor(m / 60) % 24;
  return `${String(h).padStart(2, "0")}:${String(m % 60).padStart(2, "0")}`;
}

export function yuan(cents: number | null | undefined): string {
  return cents == null ? "-" : (cents / 100).toFixed(2);
}

/** 浏览器本地钟表的「当日第几分钟」，实时档靠它对齐真实时间。 */
export function nowMinuteOfDay(): number {
  const d = new Date();
  return d.getHours() * 60 + d.getMinutes();
}

export const store = new ConsoleStore();
