import {
  PROTOCOL_VERSION,
  SOURCE_LABEL,
  isLlmSource,
  type AccountJoin,
  type AccountList,
  type AssignMode,
  type ActivityView,
  type AiOpBrief,
  type AiState,
  type ConfigView,
  type EventName,
  type EventPayloads,
  type ModelInfo,
  type NoticeView,
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
  /** **这一条是否真的来自 LLM**（算法 / 兜底 / 救单 / 人工都不算）。 */
  llm: boolean;
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

/**
 * 订单会话里的一条消息（末端交付沟通）。
 *
 * 与 `LogItem` 分开：日志是给运维看的流水，会话是**按订单聚合**的业务数据。
 * 烽火台要看的是「这一单骑手和顾客说了什么、赔了多少钱」，不是又一行文本。
 */
export interface ChatEntry {
  /** 前端自增序号，仅用于 React key。 */
  id: number;
  orderId: number;
  /** 模拟时钟 `hh:mm`。 */
  at: string;
  /** `rider` | `customer` | `system` */
  from: string;
  /** `preset` | `free` | `reply` | `ruling` */
  kind: string;
  text: string;
  /** 归属天。**时效一天**，`day_closed` 时整体清空。 */
  day: number;
  /** 这条消息带来的赔付（分）。`0` = 无经济后果。 */
  compensationCents: number;
  /** `rider` | `merchant` | `none`。钱从谁身上出由它决定。 */
  fault: string;
}

/** 上游模型清单的加载态。 */
export interface ModelListState {
  loading: boolean;
  error: string | null;
  models: ModelInfo[];
  /** 当前各层实际用的模型（layer -> model），下拉框回显用。 */
  current: Record<string, string>;
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
  /** 上游模型清单（`ai_models` 事件）。没拉过时为 null。 */
  models: ModelListState | null;
  /** 订单会话消息，新的在前。**时效一天**：`day_closed` 时清空。 */
  chats: ChatEntry[];
}

type Notifier = (kind: "success" | "error" | "info", text: string) => void;

const MAX_LOGS = 300;
const MAX_DECISIONS = 60;
/** 会话消息上限。一天几百单，按订单聚合后留这么多够看了。 */
const MAX_CHATS = 400;
/** 责任方 → 中文。**钱从谁身上出**，一眼看清。 */
export const FAULT_LABEL: Record<string, string> = {
  rider: "骑手",
  merchant: "商家",
  none: "无人责任",
};

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
  models: null,
  chats: [],
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
  private chatSeq = 0;
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

  /**
   * 往订单会话里塞一条（新的在前，超出上限截断）。
   *
   * 时间戳用消息自带的 `sim_minute`，不读快照：会话可能落在两帧快照之间，
   * 用快照时间会把先后顺序抹平。
   */
  private addChat(p: EventPayloads["chat_message"]) {
    const item: ChatEntry = {
      id: ++this.chatSeq,
      orderId: p.order_id,
      at: hhmm(p.sim_minute),
      from: p.from,
      kind: p.kind,
      text: p.text,
      day: p.day,
      compensationCents: p.compensation_cents ?? 0,
      fault: p.fault ?? "none",
    };
    this.set({ chats: [item, ...this.state.chats].slice(0, MAX_CHATS) });
  }

  /** 请求一份三层 AI 状态（打开 AI Tab 时调）。 */
  queryAi() {
    this.send("ai_query");
  }

  /** 问上游要一份可用模型清单（打开设置面板 / 点刷新时调）。 */
  listModels() {
    const prev = this.state.models;
    this.set({
      models: {
        loading: true,
        error: null,
        models: prev?.models ?? [],
        current: prev?.current ?? {},
      },
    });
    this.send("list_models");
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

  // ---------------- 调试动作 ----------------

  /**
   * 临时插一张订单。
   *
   * 坐标按**当前下发坐标系**（`ConfigView::coord_system`）传——底图也是那个系，
   * 换算交给服务端入口，控制台不自己造第二套坐标。
   */
  createOrder(fields: {
    merchant_name: string;
    merchant_lat: number;
    merchant_lng: number;
    customer_name?: string;
    customer_address?: string;
    customer_lat: number;
    customer_lng: number;
    fee_cents?: number;
    /** 出餐时间（秒）。缺省由服务端补 30 秒，按真实墙钟出餐。 */
    prep_seconds?: number;
  }) {
    this.send("create_order", fields);
  }

  /**
   * 清空某个骑手的钱包。
   *
   * 纯测试辅助。**二次确认由调用方负责**——这个动作不可撤销，
   * 服务端不做任何业务校验，也不给撤销入口。
   */
  resetRiderWallet(riderId: number) {
    this.send("reset_rider_wallet", { rider_id: riderId });
  }

  /**
   * 改派：把**还没取到餐**的单从原骑手手上转给另一个骑手。
   *
   * 服务端只认「已指派但未取货」的单，取到餐之后一律回 `WRONG_PHASE`。
   * 所以按钮只在那一档出现（见 `OrdersPanel`），这里不做兜底判断——
   * 前端和后端各判一次、判得还不一样，是更难查的那种 bug。
   */
  reassign(orderId: number, riderId: number, mode: AssignMode = "offer") {
    this.send("reassign", { order_id: orderId, rider_id: riderId, mode });
  }

  // ---------------- 骑手账号（HTTP） ----------------

  /**
   * 打一发同源的 `/api/**`。
   *
   * 与 WS 用**同一套鉴权**：带上控制令牌。服务端没配令牌、只开了
   * `RIDER_SIM_ALLOW_CONTROL` 时，不带令牌也放行。
   */
  private async request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const headers: Record<string, string> = {};
    if (this.token) headers.Authorization = `Bearer ${this.token}`;
    if (body !== undefined) headers["Content-Type"] = "application/json";

    const res = await fetch(path, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
    });

    const text = await res.text();
    let data: unknown = null;
    if (text) {
      try {
        data = JSON.parse(text);
      } catch {
        data = null;
      }
    }
    if (!res.ok) {
      const err = (data as { error?: { code?: string; message?: string } } | null)?.error;
      throw new Error(err?.message || err?.code || `HTTP ${res.status}`);
    }
    return data as T;
  }

  /** 拉账号名单 + 手机可达地址。 */
  listAccounts(): Promise<AccountList> {
    return this.request<AccountList>("GET", "/api/accounts");
  }

  /**
   * 换「手机该连哪儿」。`null` / 空串 = 恢复环境变量或自动探测。
   *
   * **只对本次运行有效**：服务端不落库，重启就回到原来的地址。
   * 换完立刻生效，之后建的号 / 重置的密码都会按新地址出二维码。
   */
  setJoinAddr(addr: string | null): Promise<AccountList> {
    return this.request<AccountList>("POST", "/api/join", { addr });
  }

  /** 建号。`password` 缺省 = 服务端生成一个，响应里回显明文 + 二维码。 */
  createAccount(name: string, password?: string): Promise<AccountJoin> {
    const body: Record<string, string> = { name };
    if (password) body.password = password;
    return this.request<AccountJoin>("POST", "/api/accounts", body);
  }

  // ---------------- 平台活动（HTTP） ----------------

  /** 全部活动（含已下架的）。手机端看的是快照里的同一份数据。 */
  listActivities(): Promise<{ activities: ActivityView[] }> {
    return this.request<{ activities: ActivityView[] }>("GET", "/api/activities");
  }

  /**
   * 建一个活动。**达成即自动入账**，不需要骑手点领取。
   *
   * `target_orders` 必须 > 0：0 单等于「所有人下一单就达标」，纯送钱。
   */
  createActivity(fields: {
    title: string;
    target_orders: number;
    reward_cents: number;
  }): Promise<{ id: number }> {
    return this.request<{ id: number }>("POST", "/api/activities", fields);
  }

  /** 上架 / 下架。下架后不再累计、不再发奖；已发的不收回。 */
  setActivityActive(id: number, active: boolean): Promise<unknown> {
    return this.request("POST", `/api/activities/${id}/active`, { active });
  }

  /** 删掉活动。已发的奖金不收回。 */
  deleteActivity(id: number): Promise<unknown> {
    return this.request("DELETE", `/api/activities/${id}`);
  }

  // ---------------- 首页通知（HTTP） ----------------

  /** 读当前通知。手机端快照里也有同一份，这里给首屏兜底。 */
  getNotice(): Promise<{ notice: NoticeView | null }> {
    return this.request<{ notice: NoticeView | null }>("GET", "/api/notice");
  }

  /**
   * 写（覆盖）首页通知。**单条**：没有「新建」，永远是在改这一条。
   *
   * `activity_id` 不传 / `null` = 和活动无关；传了就必须是已存在的活动。
   */
  setNotice(fields: {
    title: string;
    body: string;
    activity_id?: number | null;
  }): Promise<{ notice: NoticeView }> {
    return this.request<{ notice: NoticeView }>("POST", "/api/notice", fields);
  }

  /** 清空通知。手机端首页那条条子随之收起。 */
  clearNotice(): Promise<{ cleared: boolean }> {
    return this.request<{ cleared: boolean }>("DELETE", "/api/notice");
  }

  /** 改 / 重置密码。`password` 缺省 = 服务端生成一个。 */
  setAccountPassword(name: string, password?: string): Promise<AccountJoin> {
    const body: Record<string, string> = {};
    if (password) body.password = password;
    return this.request<AccountJoin>(
      "POST",
      `/api/accounts/${encodeURIComponent(name)}/password`,
      body,
    );
  }

  /** 删号。返回是否真的删掉了一条。 */
  deleteAccount(name: string): Promise<{ name: string; deleted: boolean }> {
    return this.request<{ name: string; deleted: boolean }>(
      "DELETE",
      `/api/accounts/${encodeURIComponent(name)}`,
    );
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
      case "ai_models": {
        const m = p as EventPayloads["ai_models"];
        this.set({
          models: {
            loading: false,
            error: m.ok ? null : m.message ?? "拉取失败",
            models: m.models ?? [],
            current: m.current?.layers ?? {},
          },
        });
        if (!m.ok) this.notifier("error", `模型列表拉取失败：${m.message ?? "未知原因"}`);
        break;
      }
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
        // 三种来源要分开念：抢的、站长派的指派单、站长派的强制单。
        const how =
          c.assign_mode === "forced"
            ? "强制派单接下"
            : c.assign_mode === "offer"
              ? "接受指派单"
              : c.forced
                ? "平台派单接下"
                : "抢到";
        this.log(`#${c.order_id} 被 ${c.rider_name} ${how}（${yuan(c.quoted_fee_cents)}）`);
        break;
      }
      case "order_reassigned": {
        const r = p as EventPayloads["order_reassigned"];
        // 「取餐中」是两段：去店路上、店里等餐。站长关心的是人卡在哪一段，
        // 所以分开念——只写「取餐中」，看日志的人还得自己去翻地图。
        const where = r.phase === "waiting_food" ? "店里等餐" : "去店路上";
        this.log(
          `#${r.order_id} 改派：${r.from_rider_name}（${where}）→ ${r.to_rider_name}`,
          "改派",
          // Tag 的 color 走 Ant Design 预置调色板名，不引自定义色值。
          "purple",
        );
        break;
      }
      case "order_expired":
        this.log(`#${(p as EventPayloads["order_expired"]).order_id} 无人接，流单`);
        break;
      case "assign_rejected": {
        const r = p as EventPayloads["assign_rejected"];
        // 上限 0 = 不限，别把它念成「0/0」。
        const quota = r.limit === 0 ? "不限" : `${r.used}/${r.limit}`;
        this.log(
          `骑手 #${r.rider_id} 拒掉指派单 #${r.order_id}（今日 ${quota}）` +
            (r.forced_offline ? "，额度用尽已被强制下线" : ""),
          r.forced_offline ? "强制下线" : undefined,
          r.forced_offline ? "red" : undefined,
        );
        break;
      }
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
      case "activities_changed": {
        // 快照只在连接时来一次，活动列表靠这条事件就地更新。
        const a = p as EventPayloads["activities_changed"];
        const snap = this.state.snap;
        if (snap) this.set({ snap: { ...snap, activities: a.activities } });
        break;
      }
      case "notice_changed": {
        // 单条通知：整块替换，手机端据此显示 / 收起首页条子。
        const n = p as EventPayloads["notice_changed"];
        const snap = this.state.snap;
        if (snap) this.set({ snap: { ...snap, notice: n.notice } });
        break;
      }
      case "activity_progress": {
        const a = p as EventPayloads["activity_progress"];
        const snap = this.state.snap;
        if (!snap) break;
        const next = mergeRider(snap.riders, a.rider_id, {
          activities: a.activities,
        });
        this.set({ snap: { ...snap, riders: next } });
        break;
      }
      case "activity_reward": {
        const a = p as EventPayloads["activity_reward"];
        this.log(
          `活动奖励「${a.title}」+${yuan(a.amount_cents)}（${a.completed}/${a.target_orders} 单）`,
          "活动",
          "green",
        );
        break;
      }
      case "ai_decision": {
        const a = p as EventPayloads["ai_decision"];
        const llm = isLlmSource(a.source);
        if (a.note) this.log(a.note, SOURCE_LABEL[a.source] ?? a.source, llm ? "blue" : "default");
        // 进决策流的条件：
        //  - 站长 / 烽火台的运营指令（一定有 ops）；
        //  - **非 LLM 但值得一提**的兜底 / 救单 / 人工指派 / 托管，且真的派了单。
        // 纯算法批量派单每 tick 都来，不进（噪音）。
        const noteworthy =
          a.source === "algorithm_fallback" ||
          a.source === "algorithm_rescue" ||
          a.source === "manual" ||
          a.source === "ai_hosted";
        const hasOps = (a.ops?.length ?? 0) > 0;
        if (hasOps || (noteworthy && (a.assignments?.length ?? 0) > 0)) {
          this.pushDecision({
            decisionId: a.decision_id ?? 0,
            layer: a.layer ?? "algorithm",
            source: a.source,
            llm,
            note: a.note ?? "",
            ops: a.ops ?? [],
            assignments: a.assignments?.length ?? 0,
            dropped: 0,
          });
        }
        break;
      }
      case "llm_brain": {
        const b = p as EventPayloads["llm_brain"];
        // 算法兜底不是 LLM 决策，不进流（否则刷屏）。
        // 缓存命中要进 —— 它同样是模型的决策，只是这轮没再花钱。
        if (b.source === "fallback") break;
        this.pushDecision({
          decisionId: b.decision_id ?? 0,
          layer: b.layer ?? "rider",
          source: b.source,
          llm: isLlmSource(b.source),
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
          llm: isLlmSource(w.source),
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
      case "chat_message": {
        this.addChat(p as EventPayloads["chat_message"]);
        break;
      }
      case "day_closed": {
        const d = p as EventPayloads["day_closed"];
        this.log(`第 ${d.day} 天结束：${d.total_orders} 单，骑手合计到手 ${yuan(d.total_takehome_cents)}`);
        // 会话时效一天：服务端日结时清了库，控制台跟着清，
        // 免得翻旧账时还看得见早就该没了的对话。
        this.set({ chats: [] });
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
    // resync / ai_query / list_models 是内部动作，不该弹提示
    // （list_models 的真实结果随后由 `ai_models` 事件专门呈现）。
    if (a.action === "resync" || a.action === "ai_query" || a.action === "list_models") return;
    // 调试动作说清楚「动了哪一个、动了多少」，比一句「成功」有用。
    if (a.action === "reset_rider_wallet") {
      const cleared = Number(a.data?.cleared_cents ?? 0);
      this.notifier(
        "success",
        `已清空骑手 #${a.data?.rider_id} 的钱包（清空前 ¥${yuan(cleared)}）`,
      );
      this.resync();
      return;
    }
    if (a.action === "create_order") {
      this.notifier("success", `已创建订单 #${a.data?.order_id}`);
      this.resync();
      return;
    }
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
