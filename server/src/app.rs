//! 服务端主循环。**唯一持有 `&mut World` 的地方。**
//!
//! ## 单线程串行 = 抢单原子性
//!
//! 整个模拟跑在一个 tokio 任务里，`World` 就放在栈上。所有会改状态的入口
//! ——tick、抢单、切档、强制指派——都从这个任务里经过，天然互斥。
//! 这就是「10 个客户端抢 1 单，成功者恰好 1 个」不需要任何锁的原因。
//!
//! ```text
//!          ┌──────────── on_tick（每 tick_interval_ms 一次）────────────┐
//!          │  sim::tick(&mut world) → Vec<SimEvent> → hub.broadcast() │
//!          └─────────────────────────────────────────────────────────┘
//! socket ──▶ Inbound ──▶ on_inbound ──▶ scheduler::*(&mut world) ──▶ 事件
//! ```
//!
//! ## 两条「不许把循环卡住」的规矩
//!
//! 1. **绝不 `await` 网络。** LLM 计划走「本 tick 占坑、下 tick 收结果」
//!    的两段式（[`crate::llm::TacticalBrain::prepare`] / `finish`）；
//!    3 秒超时发生在后台任务里，tick 照常走。
//! 2. **任务裁决同理。** 骑手说一句话，服务端立刻回一句「已收到」，
//!    真正的判定结果稍后由 `ruling` 通道送回。

use std::net::SocketAddr;
use std::time::Duration;

use serde_json::json;
use tokio::sync::{
    mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    watch,
};
use tokio::time::{Instant, sleep_until};

use crate::llm::{LlmConfig, PlanFetch, PreparedPlan, TacticalBrain, TaskJudge, TaskRuling};
use crate::net::{
    Ack, ApiAuth, ApiError, ClientMessage, ConnId, DispatchItem, Envelope, Hub, Inbound,
    PROTOCOL_VERSION, Role, RpcReply, RpcRequest, code, render_snapshot, server_info,
};
use crate::sim::{
    OrderId, RiderId,
    ai::LlmPlan,
    economy,
    event::{DecisionSource, SimEvent},
    scheduler::{self, ClaimError},
    tick::{self, TickInput},
    world::World,
};
use crate::store::Store;

/// 服务端运行时选项。**与 `SimConfig` 分开**：这些是「服务器怎么跑」，
/// 不是「模拟世界怎么算」。
#[derive(Debug, Clone)]
pub struct AppOptions {
    /// 没带 token 的连接能不能拿控制权。
    ///
    /// **默认 `false`**：生产环境里控制端必须带 token，否则任何一个连上来
    /// 的客户端都能暂停全世界。开发时才显式打开。
    pub allow_control: bool,
    /// 控制端令牌。`hello.token = "control:<令牌>"` 与之相等才给控制权；
    /// `None` = 不接受令牌方式。
    pub control_token: Option<String>,
    /// 同时在线连接上限。
    pub max_connections: usize,
    /// 是否启用 LLM（没有 Key 时自动降级为关闭）。
    pub llm_enabled: bool,
    /// 每多少模拟分钟把进度写一次 `meta` 表。
    pub autosave_every_min: u64,
    /// 每多少 tick 给控制端推一次服务器状态。
    pub server_info_every_ticks: u64,
}

impl Default for AppOptions {
    fn default() -> Self {
        Self {
            allow_control: false,
            control_token: None,
            max_connections: 64,
            llm_enabled: false,
            autosave_every_min: 60,
            server_info_every_ticks: 60,
        }
    }
}

/// 后台任务送回来的一条任务裁决。
#[derive(Debug)]
struct PendingRuling {
    rider_id: RiderId,
    order_id: OrderId,
    step: usize,
    task_id: String,
    rider_text: String,
    ruling: TaskRuling,
}

/// 后台任务送回来的一轮 LLM 结果。
type PlanResult = (PreparedPlan, PlanFetch);

/// 主循环。
pub struct App {
    pub world: World,
    pub hub: Hub,
    pub store: Store,
    pub opts: AppOptions,

    brain: TacticalBrain,
    judge: TaskJudge,
    /// LLM 接入配置（Key / Base URL / 模型）。控制台可改，落库后重启仍生效。
    ///
    /// **绝不下发**：任何给客户端的地方都只报「配没配」，不报内容。
    llm_config: LlmConfig,

    inbound_tx: UnboundedSender<Inbound>,
    inbound_rx: Option<UnboundedReceiver<Inbound>>,
    ruling_tx: UnboundedSender<PendingRuling>,
    ruling_rx: Option<UnboundedReceiver<PendingRuling>>,
    plan_tx: UnboundedSender<PlanResult>,
    plan_rx: Option<UnboundedReceiver<PlanResult>>,

    /// 已占坑、正在后台打 HTTP 的那一轮。`None` = 没有在途请求。
    inflight_since: Option<Instant>,
    /// 已经拿到、等下一个 tick 消费的计划。
    pending_plan: Option<LlmPlan>,
    /// 暂停前的倍率，恢复时用。
    paused_multiplier: u32,
    /// 节拍需要重排。切档 / 对齐时钟后置位，让主循环从现在重新计时。
    ///
    /// 不重排会出两种洋相：实时档刚对齐完，被上一档剩下的节拍立刻推走一分钟；
    /// 从实时档切回 8x，要干等 60 秒才走下一格。
    beat_reset: bool,
    /// 最近一次大脑结论（给控制端看）。
    last_brain_note: String,
    tick_count: u64,
    /// 控制端闭包里产生的待广播事件。
    ///
    /// 闭包拿到的是 `&mut App`，没法同时调用 `self.publish(&mut self)`，
    /// 所以先攒在这里，出闭包后统一广播。
    outbox: Vec<SimEvent>,
    /// 控制端指令的业务失败。
    outbox_err: Option<ClaimError>,
}

impl App {
    /// 组装主循环。**不建数据库连接**——那个交给调用方，方便换成内存库。
    pub fn new(world: World, store: Store, opts: AppOptions) -> Self {
        let (inbound_tx, inbound_rx) = unbounded_channel();
        let (ruling_tx, ruling_rx) = unbounded_channel();
        let (plan_tx, plan_rx) = unbounded_channel();

        // LLM 接入的优先级：控制台存下来的 > 环境变量 > 空配置。
        //
        // 命令行没开 LLM（`RIDER_SIM_LLM` 缺省 false）时**不读环境变量**，
        // 避免「明明没开却在烧配额」；但控制台显式填过 Key 就认——
        // 那是人的明确意图，且要能在重启后恢复。
        let env_llm = opts.llm_enabled;
        let llm_config = load_llm_config(&store).unwrap_or_else(|| {
            if env_llm {
                LlmConfig::from_env()
            } else {
                LlmConfig::default()
            }
        });
        let (brain, judge) = if llm_config.has_key() {
            (
                TacticalBrain::from_config(&llm_config),
                TaskJudge::from_config(&llm_config),
            )
        } else {
            (TacticalBrain::disabled(), TaskJudge::disabled())
        };

        let mut opts = opts;
        // 「允许调用 LLM」= 命令行开过，或控制台配过 Key。
        opts.llm_enabled = env_llm || llm_config.has_key();

        let mut world = world;
        // 「真的去调」仍由控制台的 LLM 开关决定：控制台配的 Key 不会一重启就烧配额，
        // 只有命令行显式开过才在启动时自动打开大脑。
        world.cfg.llm_brain_enabled = env_llm && brain.is_enabled();

        // 控制台存过的骑手名单，重启后接着用。
        if let Some(names) = load_rider_names(&store) {
            world.apply_rider_names(&names);
        }

        let paused_multiplier = if world.cfg.speed_multiplier == 0 {
            1
        } else {
            world.cfg.speed_multiplier
        };

        Self {
            world,
            hub: Hub::new(),
            store,
            opts,
            brain,
            judge,
            llm_config,
            inbound_tx,
            inbound_rx: Some(inbound_rx),
            ruling_tx,
            ruling_rx: Some(ruling_rx),
            plan_tx,
            plan_rx: Some(plan_rx),
            inflight_since: None,
            pending_plan: None,
            paused_multiplier,
            beat_reset: false,
            last_brain_note: String::new(),
            tick_count: 0,
            outbox: Vec::new(),
            outbox_err: None,
        }
    }

    /// 给 HTTP 路由用的入口队列。
    pub fn inbound(&self) -> UnboundedSender<Inbound> {
        self.inbound_tx.clone()
    }

    pub fn llm_enabled(&self) -> bool {
        self.opts.llm_enabled && self.brain.is_enabled()
    }

    /// 当前一个 tick 的现实时长。
    pub fn period(&self) -> Duration {
        Duration::from_millis(self.world.cfg.tick_interval_ms().max(1))
    }

    /// 绑定端口 + 跑主循环。Ctrl-C 或监听失败时退出。
    pub async fn serve(self, addr: SocketAddr) -> std::io::Result<()> {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let bound = listener.local_addr().unwrap_or(addr);
        tracing::info!(
            addr = %bound,
            ws = %format!("ws://{bound}{}", crate::net::WS_PATH),
            console = %format!("http://{bound}/"),
            api = %format!("http://{bound}{}", crate::net::api::STATE_PATH),
            seed = self.world.seed,
            db = %self.store.path().display(),
            llm = self.llm_enabled(),
            "骑手模拟器已启动"
        );

        let router = crate::net::router(
            self.inbound(),
            ApiAuth {
                token: self.opts.control_token.clone(),
                allow_control: self.opts.allow_control,
            },
        );
        let (tx, rx) = watch::channel(false);
        let app_task = tokio::spawn(self.run(rx));

        let server = axum::serve(listener, router).with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("收到中断信号，正在收尾");
        });
        let result = server.await;
        let _ = tx.send(true);
        let _ = app_task.await;
        result
    }

    /// 主循环本体。
    ///
    /// `select!` 的四条腿：定时 tick、网络消息、任务裁决、LLM 计划。
    /// 只有第一条会推进世界，其余三条都只是「改变输入」。
    pub async fn run(mut self, mut shutdown: watch::Receiver<bool>) -> std::io::Result<()> {
        let mut inbound = self.inbound_rx.take().expect("run 只能调用一次");
        let mut rulings = self.ruling_rx.take().expect("run 只能调用一次");
        let mut plans = self.plan_rx.take().expect("run 只能调用一次");

        self.maybe_start_plan();
        let mut next = Instant::now() + self.period();

        loop {
            tokio::select! {
                _ = sleep_until(next) => {
                    self.on_tick();
                    next = Instant::now() + self.period();
                }
                Some(msg) = inbound.recv() => {
                    self.on_inbound(msg);
                    // 控速类指令会改节拍长度（16ms 到 60s 都可能），
                    // 处理完立刻重排下一次 tick，别让世界等旧的节拍。
                    if self.beat_reset {
                        self.beat_reset = false;
                        next = Instant::now() + self.period();
                    }
                }
                Some(r) = rulings.recv() => self.on_ruling(r),
                Some((p, f)) = plans.recv() => self.on_plan(p, f),
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        break;
                    }
                }
            }
        }

        self.final_flush();
        Ok(())
    }

    // -----------------------------------------------------------------
    // tick
    // -----------------------------------------------------------------

    fn on_tick(&mut self) {
        // 暂停：倍率 0 时世界完全冻住，但网络照常收发（否则客户端会以为掉线）。
        if self.world.cfg.speed_multiplier == 0 {
            return;
        }

        // 兜底：后台任务如果因为意外没送回结果，别让 LLM 永久卡死。
        if let Some(since) = self.inflight_since
            && since.elapsed() > Duration::from_secs(10)
        {
            tracing::warn!("LLM 计划超时未归，放弃这一轮");
            self.inflight_since = None;
        }

        let plan = self.pending_plan.take();
        let events = tick::tick(&mut self.world, TickInput { llm_plan: plan });
        self.tick_count += 1;
        self.publish(&events);

        if self.opts.server_info_every_ticks > 0
            && self
                .tick_count
                .is_multiple_of(self.opts.server_info_every_ticks)
        {
            self.push_server_info();
        }
        self.maybe_start_plan();
    }

    /// 广播事件，并顺手处理需要落库的部分。
    fn publish(&mut self, events: &[SimEvent]) {
        for ev in events {
            if let SimEvent::DayClosed { day, ledgers, .. } = ev {
                match self.store.save_day(*day, ledgers) {
                    Ok(r) => tracing::info!(day, ledgers = r.ledgers, "日结已落库"),
                    Err(e) => {
                        tracing::warn!(error = %e, "日结落库失败，本局数据只留在内存")
                    }
                }
            }
            self.hub.broadcast(&self.world, ev);
        }

        let every = self.opts.autosave_every_min.max(1);
        if self.world.elapsed_min > 0 && self.world.elapsed_min.is_multiple_of(every) {
            let _ =
                self.store
                    .save_progress(self.world.seed, self.world.day, self.world.elapsed_min);
        }
    }

    /// 给控制端推一份服务器状态（不含任何 Key）。
    fn push_server_info(&mut self) {
        let mut info = server_info(&self.world, &self.hub.stats(), self.llm_enabled(), false);
        if let Some(obj) = info.as_object_mut() {
            obj.insert("brain_note".into(), json!(self.last_brain_note));
            obj.insert("brain_calls_today".into(), json!(self.brain.calls_today()));
            obj.insert(
                "brain_max_calls_per_day".into(),
                json!(self.brain.max_calls_per_day()),
            );
            // 只发给控制端（send_to_control），所以可以回模型信息；
            // **Key 永远不出现在这里**，只报「配没配」。
            obj.insert(
                "llm_configured".into(),
                json!(self.llm_config.has_key()),
            );
            obj.insert("llm_base_url".into(), json!(self.llm_config.base_url));
            obj.insert("llm_model".into(), json!(self.llm_config.model));
        }
        self.hub
            .send_to_control(&Envelope::new("server_info", info));
    }

    fn final_flush(&mut self) {
        let _ = self
            .store
            .save_progress(self.world.seed, self.world.day, self.world.elapsed_min);
        let day = self.world.day.max(1);
        let ledgers = self.world.last_day_ledgers.clone();
        if !ledgers.is_empty() {
            let _ = self.store.save_day(day, &ledgers);
        }
        tracing::info!(day = self.world.day, "主循环已停止");
    }

    // -----------------------------------------------------------------
    // LLM 两段式
    // -----------------------------------------------------------------

    /// 如果条件允许，占一个时段坑并把 HTTP 丢给后台。
    fn maybe_start_plan(&mut self) {
        if self.inflight_since.is_some() {
            return;
        }
        if !self.llm_enabled() || !self.world.cfg.llm_brain_enabled {
            return;
        }
        let idle = self.world.idle_riders();
        let pending = self.world.pending_orders();
        match self.brain.prepare(&self.world, &idle, &pending) {
            Ok(prepared) => {
                let tx = self.plan_tx.clone();
                self.inflight_since = Some(Instant::now());
                tokio::spawn(async move {
                    let fetched = prepared.request().await;
                    if tx.send((prepared, fetched)).is_err() {
                        tracing::debug!("主循环已退出，丢弃这一轮 LLM 结果");
                    }
                });
            }
            Err(outcome) => {
                // Disabled / Throttled 都属于「正常不调」，只记不给客户端刷屏
                self.last_brain_note = outcome.note;
            }
        }
    }

    fn on_plan(&mut self, prepared: PreparedPlan, fetched: PlanFetch) {
        self.inflight_since = None;
        let idle = self.world.idle_riders();
        let pending = self.world.pending_orders();
        let (outcome, plan) = self
            .brain
            .finish(prepared, fetched, &self.world, &idle, &pending);
        self.last_brain_note = format!("[{}] {}", outcome.source.as_str(), outcome.note);

        if outcome.source != crate::llm::BrainSource::Disabled {
            let stats = self.brain.stats();
            self.hub.send_to_control(&Envelope::new(
                "llm_brain",
                json!({
                    "source": outcome.source.as_str(),
                    "note": outcome.note,
                    "model": outcome.model,
                    "latency_ms": outcome.latency_ms,
                    "assignments": outcome.verdict.assignments.len(),
                    "dropped": outcome.verdict.dropped,
                    "calls_today": self.brain.calls_today(),
                    "max_calls_per_day": self.brain.max_calls_per_day(),
                    "stats": {
                        "calls": stats.calls,
                        "failures": stats.failures,
                        "applied": stats.applied,
                        "dropped": stats.dropped,
                    },
                }),
            ));
        }

        if plan.is_some() {
            // tick 里的 run_ai 会再校验一次，然后把它变成真实指派。
            self.pending_plan = plan;
        }
    }

    // -----------------------------------------------------------------
    // 来自 socket 的消息
    // -----------------------------------------------------------------

    fn on_inbound(&mut self, msg: Inbound) {
        match msg {
            Inbound::Open { conn_id, tx } => {
                if self.hub.len() >= self.opts.max_connections {
                    tracing::warn!(conn_id, "连接数已达上限，拒绝接入");
                    // 丢掉发送端 → ws 写任务结束 → socket 关闭
                    drop(tx);
                    return;
                }
                self.hub.register(conn_id, Role::Spectator, "", tx);
            }
            Inbound::BadFrame {
                conn_id,
                code,
                message,
            } => {
                tracing::debug!(conn_id, %message, "收到无法解析的帧");
                self.hub.send_error(conn_id, code, &message);
            }
            Inbound::Closed { conn_id } => self.on_closed(conn_id),
            Inbound::Msg { conn_id, msg } => self.on_client(conn_id, msg),
            Inbound::Rpc { req, reply } => {
                let result = self.handle_rpc(req);
                // HTTP 那头可能已经超时走人了，发不出去很正常
                let _ = reply.send(result);
            }
        }
    }

    // -----------------------------------------------------------------
    // 来自 HTTP 控制接口的调用
    // -----------------------------------------------------------------

    /// 执行一次远程调用。**规则全在这里**，`net::api` 只负责搬运。
    ///
    /// 和 WS 控制端的差别只有一个：没有连接可回 `ack`，结果直接当返回值。
    /// 事件照常广播——控制台（烽火台）看到的东西与手点按钮完全一致。
    fn handle_rpc(&mut self, req: RpcRequest) -> RpcReply {
        match req {
            RpcRequest::State => Ok(self.rpc_state()),
            RpcRequest::Dispatch(items) => self.rpc_dispatch(items),
            RpcRequest::SetConfig(patch) => self.rpc_set_config(&patch),
            RpcRequest::SetRiderOnline { rider_id, online } => {
                self.rpc_set_online(rider_id, online)
            }
        }
    }

    /// 控制端视角的完整快照 + 服务器上下文。外部工具靠它挑人挑单。
    fn rpc_state(&self) -> serde_json::Value {
        let mut snap = render_snapshot(&self.world, Role::Control, self.world.elapsed_min);
        if let Some(obj) = snap.as_object_mut() {
            obj.insert(
                "server_info".to_string(),
                server_info(&self.world, &self.hub.stats(), self.llm_enabled(), false),
            );
        }
        snap
    }

    /// 批量派单。**一条失败不挡后面的**——外部工具一次能拿到全部结果。
    fn rpc_dispatch(&mut self, items: Vec<DispatchItem>) -> RpcReply {
        let mut results = Vec::with_capacity(items.len());
        let mut events = Vec::new();
        let mut decisions = Vec::new();
        let mut applied = 0u32;

        for item in items {
            let order_id = item.order_id;
            let Some(rider_id) = item.rider() else {
                results.push(json!({
                    "order_id": order_id,
                    "ok": false,
                    "code": "RIDER_NOT_FOUND",
                    "message": "rider_id 无法解析（用数字 3 或字符串 \"r3\"）",
                }));
                continue;
            };

            match scheduler::assign_forced(&mut self.world, order_id, rider_id) {
                Ok(mut assignment) => {
                    if let Some(reason) = item
                        .reason
                        .as_deref()
                        .map(str::trim)
                        .filter(|r| !r.is_empty())
                    {
                        assignment.reason = reason.to_string();
                    }
                    events.extend(self.claim_events(&assignment));
                    decisions.push(assignment.to_event());
                    applied += 1;
                    results.push(json!({
                        "order_id": order_id,
                        "rider_id": rider_id,
                        "rider_key": crate::sim::rider_key(rider_id),
                        "ok": true,
                        "planned_minutes": assignment.plan.total_min(),
                        "reason": assignment.reason,
                    }));
                }
                Err(e) => results.push(json!({
                    "order_id": order_id,
                    "rider_id": rider_id,
                    "rider_key": crate::sim::rider_key(rider_id),
                    "ok": false,
                    "code": e.code(),
                    "message": e.message(),
                })),
            }
        }

        if !decisions.is_empty() {
            events.push(SimEvent::AiDecision {
                source: DecisionSource::Manual,
                assignments: decisions,
                note: "外部接口派单".to_string(),
            });
        }
        if !events.is_empty() {
            self.publish(&events);
        }

        let failed = results.len() as u32 - applied;
        Ok(json!({ "applied": applied, "failed": failed, "results": results }))
    }

    /// 与 WS 的 `set_config` 走同一套夹取逻辑。
    fn rpc_set_config(&mut self, patch: &crate::net::ConfigPatch) -> RpcReply {
        let changed = self.apply_config_patch(patch);
        let events: Vec<SimEvent> = changed
            .into_iter()
            .filter_map(|id| scheduler::state_event(&self.world, id))
            .collect();
        if !events.is_empty() {
            self.publish(&events);
        }
        self.broadcast_config_changed();
        match serde_json::to_value(self.world.config_view()) {
            Ok(v) => Ok(v),
            Err(e) => Err(ApiError::internal(format!("配置序列化失败：{e}"))),
        }
    }

    fn rpc_set_online(&mut self, rider_id: RiderId, online: bool) -> RpcReply {
        match scheduler::set_online(&mut self.world, rider_id, online) {
            Ok(events) => {
                self.publish(&events);
                self.broadcast_config_changed();
                Ok(json!({ "rider_id": rider_id, "online": online }))
            }
            Err(e) => Err(ApiError::conflict(e.code(), e.message())),
        }
    }

    fn on_closed(&mut self, conn_id: ConnId) {
        let role = self.hub.role_of(conn_id);
        self.hub.unregister(conn_id);

        // 真人骑手的最后一个连接断开 → 自动下线（手上还有单时不允许，忽略失败）
        if let Some(Role::Rider(rid)) = role
            && self.hub.rider_conn_count(rid) == 0
        {
            tracing::info!(rider_id = rid, "骑手连接全部断开");
            if let Ok(events) = scheduler::set_online(&mut self.world, rid, false) {
                self.publish(&events);
            }
            // 手上有单：保持在线，等单送完
        }
        self.push_server_info();
    }

    fn on_client(&mut self, conn_id: ConnId, msg: ClientMessage) {
        use ClientMessage as M;

        // ---- 握手闸门：第一条必须是 hello ----
        if !self.hub.is_ready(conn_id) {
            return match msg {
                M::Hello {
                    token,
                    protocol_version,
                } => self.do_hello(conn_id, token, protocol_version),
                _ => self
                    .hub
                    .send_error(conn_id, code::HELLO_REQUIRED, "第一条消息必须是 hello"),
            };
        }

        let Some(role) = self.hub.role_of(conn_id) else {
            return;
        };

        match msg {
            M::Hello { .. } => {
                self.hub.send_error(conn_id, code::BAD_REQUEST, "重复握手")
            }
            M::Resync => self.send_snapshot(conn_id, role),
            M::Ping => {
                let t = self.world.elapsed_min;
                self.hub
                    .send_to(conn_id, &Envelope::new("pong", json!({ "elapsed_min": t })));
            }

            // ---------------- 骑手动作 ----------------
            M::Claim {
                order_id,
                back_order,
            } => match role.rider_id() {
                Some(rid) => self.do_claim(conn_id, rid, order_id, back_order),
                None => self.not_a_rider(conn_id, "claim"),
            },
            M::Reject { order_id } => match role.rider_id() {
                Some(rid) => {
                    let r = scheduler::reject_order(&mut self.world, rid, order_id);
                    self.finish_simple(conn_id, "reject", r)
                }
                None => self.not_a_rider(conn_id, "reject"),
            },
            M::Urge { order_id } => match role.rider_id() {
                Some(rid) => {
                    let r = scheduler::urge_order(&mut self.world, rid, order_id);
                    match r {
                        Ok((success, events)) => {
                            self.publish(&events);
                            self.hub.send_ack(
                                conn_id,
                                &Ack::ok_with("urge", json!({ "success": success })),
                            );
                        }
                        Err(e) => self.ack_err(conn_id, "urge", e),
                    }
                }
                None => self.not_a_rider(conn_id, "urge"),
            },
            M::ArriveShop { order_id } => match role.rider_id() {
                Some(rid) => {
                    let r = scheduler::arrive_shop(&mut self.world, rid, order_id);
                    self.finish_simple(conn_id, "arrive_shop", r)
                }
                None => self.not_a_rider(conn_id, "arrive_shop"),
            },
            M::ArriveCustomer { order_id } => match role.rider_id() {
                Some(rid) => {
                    let r = scheduler::arrive_customer(&mut self.world, rid, order_id);
                    self.finish_simple(conn_id, "arrive_customer", r)
                }
                None => self.not_a_rider(conn_id, "arrive_customer"),
            },
            M::Deliver { order_id } => match role.rider_id() {
                Some(rid) => {
                    let r = scheduler::deliver(&mut self.world, rid, order_id);
                    self.finish_simple(conn_id, "deliver", r)
                }
                None => self.not_a_rider(conn_id, "deliver"),
            },
            M::SetOnline { online } => match role.rider_id() {
                Some(rid) => {
                    let r = scheduler::set_online(&mut self.world, rid, online);
                    self.finish_simple(conn_id, "set_online", r)
                }
                None => self.not_a_rider(conn_id, "set_online"),
            },
            M::SetSpeedMode { mode } => match role.rider_id() {
                Some(rid) => {
                    let r = scheduler::set_speed_mode(&mut self.world, rid, mode);
                    self.finish_simple(conn_id, "set_speed_mode", r)
                }
                None => self.not_a_rider(conn_id, "set_speed_mode"),
            },
            M::TaskMessage { order_id, text } => match role.rider_id() {
                Some(rid) => self.do_task_message(conn_id, rid, order_id, text),
                None => self.not_a_rider(conn_id, "task_message"),
            },

            // ---------------- 控制端 ----------------
            M::SetPaused { paused } => self.control(conn_id, role, "set_paused", |app| {
                if paused {
                    if app.world.cfg.speed_multiplier != 0 {
                        app.paused_multiplier = app.world.cfg.speed_multiplier;
                    }
                    app.world.cfg.speed_multiplier = 0;
                } else if app.world.cfg.speed_multiplier == 0 {
                    app.world.cfg.speed_multiplier = app.paused_multiplier.max(1);
                }
                app.beat_reset = true;
                Some(json!({ "speed_multiplier": app.world.cfg.speed_multiplier }))
            }),
            M::SetSpeedMultiplier { multiplier } => {
                self.control(conn_id, role, "set_speed_multiplier", |app| {
                    let m = multiplier.min(60);
                    app.world.cfg.speed_multiplier = m;
                    if m > 0 {
                        // 选倍率档 = 退出实时档：两个模式互斥。
                        app.world.cfg.realtime = false;
                        app.paused_multiplier = m;
                    }
                    app.beat_reset = true;
                    Some(json!({
                        "speed_multiplier": m,
                        "realtime": app.world.cfg.realtime,
                        "tick_interval_ms": app.world.cfg.tick_interval_ms(),
                    }))
                })
            }
            M::SetWeather { weather } => self.control(conn_id, role, "set_weather", |app| {
                if app.world.set_weather(weather) {
                    app.outbox.push(SimEvent::WeatherChanged {
                        weather,
                        label: weather.label(),
                        speed_factor: weather.speed_factor(),
                        rain_bonus_cents: weather.rain_bonus_cents(),
                    });
                }
                Some(json!({ "weather": weather.label() }))
            }),
            M::SetCommissionRate { rate } => self.control(conn_id, role, "set_commission_rate", |app| {
                let r = rate.clamp(0.0, 0.30);
                app.world.cfg.commission_rate = r;
                Some(json!({ "commission_rate": r }))
            }),
            M::ForceAssign { order_id, rider_id } => {
                self.control(conn_id, role, "force_assign", |app| match scheduler::assign_forced(
                    &mut app.world,
                    order_id,
                    rider_id,
                ) {
                    Ok(assignment) => {
                        let events = app.claim_events(&assignment);
                        app.outbox.extend(events);
                        Some(json!({ "order_id": order_id, "rider_id": rider_id }))
                    }
                    Err(e) => {
                        app.outbox_err = Some(e);
                        None
                    }
                })
            }
            M::SetLlmEnabled { enabled } => self.control(conn_id, role, "set_llm_enabled", |app| {
                let actual = enabled && app.brain.is_enabled();
                app.opts.llm_enabled = actual;
                app.world.cfg.llm_brain_enabled = actual;
                Some(json!({ "llm_enabled": actual }))
            }),
            M::SetConfig(patch) => self.control(conn_id, role, "set_config", |app| {
                let changed = app.apply_config_patch(&patch);
                for id in changed {
                    if let Some(ev) = scheduler::state_event(&app.world, id) {
                        app.outbox.push(ev);
                    }
                }
                serde_json::to_value(app.world.config_view()).ok()
            }),
            M::SetRiderOnline { rider_id, online } => {
                self.control(conn_id, role, "set_rider_online", |app| {
                    match scheduler::set_online(&mut app.world, rider_id, online) {
                        Ok(events) => {
                            app.outbox.extend(events);
                            Some(json!({ "rider_id": rider_id, "online": online }))
                        }
                        Err(e) => {
                            app.outbox_err = Some(e);
                            None
                        }
                    }
                })
            }
            M::SetRealtime {
                enabled,
                wall_minute,
            } => self.control(conn_id, role, "set_realtime", |app| {
                app.world.cfg.realtime = enabled;
                let mut aligned = None;
                if enabled {
                    // 对齐真实钟表：分钟数由控制端浏览器给（服务端不引时区库），
                    // 缺省用 UTC 兜底。
                    let minute = wall_minute.unwrap_or_else(utc_minute_of_day);
                    aligned = Some(app.world.align_clock(minute));
                }
                // 对齐完要从「现在」重新计时：否则上一档剩下的那半个节拍
                // 会立刻把刚对齐的钟面推走一分钟。
                app.beat_reset = true;
                // 实时档 60 秒才一个 tick，不补这一帧控制台的时钟会「卡住」。
                app.outbox.push(SimEvent::Tick {
                    day: app.world.day,
                    sim_minute: app.world.sim_minute,
                    elapsed_min: app.world.elapsed_min,
                    speed_multiplier: app.world.cfg.speed_multiplier,
                });
                Some(json!({
                    "realtime": enabled,
                    "wall_minute": aligned,
                    "sim_minute": app.world.sim_minute,
                    "tick_interval_ms": app.world.cfg.tick_interval_ms(),
                }))
            }),
            M::RenameRider { rider_id, name } => {
                self.control(conn_id, role, "rename_rider", |app| {
                    let changed = app.world.rename_rider(rider_id, &name);
                    if changed {
                        if let Some(ev) = scheduler::state_event(&app.world, rider_id) {
                            app.outbox.push(ev);
                        }
                        app.persist_rider_names();
                    }
                    Some(json!({ "changed": changed, "rider_id": rider_id }))
                })
            }
            M::SetRiderNames { names } => self.control(conn_id, role, "set_rider_names", |app| {
                let changed = app.world.apply_rider_names(&names);
                for id in &changed {
                    if let Some(ev) = scheduler::state_event(&app.world, *id) {
                        app.outbox.push(ev);
                    }
                }
                app.persist_rider_names();
                Some(json!({
                    "changed": changed.len(),
                    "names": app.world.sim_rider_names(),
                }))
            }),
            M::SetLlmConfig {
                api_key,
                base_url,
                model,
            } => self.control(conn_id, role, "set_llm_config", |app| {
                app.apply_llm_config(api_key, base_url, model);
                // 配完立刻把新的「配没配 / 什么模型」推给控制端，表单才能回显。
                app.push_server_info();
                Some(json!({
                    "llm_configured": app.llm_config.has_key(),
                    "base_url": app.llm_config.base_url,
                    "model": app.llm_config.model,
                }))
            }),
        }
    }

    /// 应用控制台提交的 LLM 配置：改配置 → 重建 brain/judge → 落库。
    ///
    /// `None` = 这一项不动；`api_key` 传空串 = 清除 Key（连带把大脑开关落下）。
    /// Key 只进数据库，不下发、不写日志。
    fn apply_llm_config(
        &mut self,
        api_key: Option<String>,
        base_url: Option<String>,
        model: Option<String>,
    ) {
        if let Some(k) = api_key {
            self.llm_config.api_key = k.trim().to_string();
        }
        if let Some(u) = base_url {
            let u = u.trim();
            self.llm_config.base_url = if u.is_empty() {
                LlmConfig::default().base_url
            } else {
                u.to_string()
            };
        }
        if let Some(m) = model {
            let m = m.trim();
            self.llm_config.model = if m.is_empty() {
                LlmConfig::default().model
            } else {
                m.to_string()
            };
        }

        // 重建客户端：战术大脑与任务判定共用同一份配置。
        self.brain = TacticalBrain::from_config(&self.llm_config);
        self.judge = TaskJudge::from_config(&self.llm_config);

        let has_key = self.llm_config.has_key();
        self.opts.llm_enabled = has_key;
        if !has_key {
            // 没 Key 就没有 LLM 可言，开关一并落下，
            // 免得 UI 显示「已开启」却毫无反应。
            self.world.cfg.llm_brain_enabled = false;
        }
        self.persist_llm_config();
    }

    /// 把当前 LLM 配置落库。Key 明文入库——本地模拟器，不上 vault。
    fn persist_llm_config(&self) {
        let _ = self.store.set_meta("llm_api_key", &self.llm_config.api_key);
        let _ = self.store.set_meta("llm_base_url", &self.llm_config.base_url);
        let _ = self.store.set_meta("llm_model", &self.llm_config.model);
    }

    /// 把当前模拟骑手名单落库，重启后接着用。
    fn persist_rider_names(&self) {
        if let Ok(t) = serde_json::to_string(&self.world.sim_rider_names()) {
            let _ = self.store.set_meta("rider_names", &t);
        }
    }

    /// 应用 `set_config`，返回状态被连带改动的骑手。
    fn apply_config_patch(&mut self, p: &crate::net::ConfigPatch) -> Vec<RiderId> {
        let cfg = &mut self.world.cfg;
        if let Some(v) = p.claim_window_secs {
            cfg.claim_window_secs = v.clamp(1, 10);
        }
        if let Some(v) = p.allow_speeding {
            cfg.allow_speeding = v;
        }
        if let Some(v) = p.allow_unrestricted {
            cfg.allow_unrestricted = v;
        }
        if let Some(v) = p.backorder_enabled {
            cfg.backorder_enabled = v;
        }
        if let Some(v) = p.backorder_max {
            cfg.backorder.max_held = v.clamp(1, 5);
        }
        if let Some(v) = p.discriminative_commission {
            cfg.discriminative_commission = v;
        }
        if let Some(v) = p.show_expected_value {
            cfg.show_expected_value = v;
        }
        if let Some(v) = p.ai_batch_enabled {
            cfg.ai_batch_enabled = v;
        }
        if let Some(v) = p.max_orders_per_day {
            cfg.max_orders_per_day = v;
        }
        // 只影响下发时的坐标转换，世界状态不动。
        if let Some(v) = p.coord_system {
            cfg.coord_system = v;
        }
        // 计价规则只影响之后新生成的订单，已在跑的单沿用当时的报价。
        if let Some(v) = p.base_fee_cents {
            cfg.fee.base_fee_cents = v.clamp(0, 2_000);
        }
        if let Some(v) = p.per_km_over_cents {
            cfg.fee.per_km_over_cents = v.clamp(0, 1_000);
        }
        if let Some(v) = p.level_bonus_per_level_cents {
            cfg.fee.level_bonus_per_level_cents = v.clamp(0, 500);
        }

        let mut changed = self.world.enforce_speed_policy();
        if let Some(n) = p.sim_rider_count {
            changed.extend(self.world.set_sim_rider_count(n));
        }
        changed.sort_unstable();
        changed.dedup();
        changed
    }

    /// 握手。
    fn do_hello(&mut self, conn_id: ConnId, token: Option<String>, proto: Option<String>) {
        if let Some(v) = proto.as_deref()
            && v != PROTOCOL_VERSION
        {
            self.hub.send_error(
                conn_id,
                code::PROTOCOL_MISMATCH,
                &format!("服务端协议 {PROTOCOL_VERSION}，客户端 {v}"),
            );
            self.hub.unregister(conn_id);
            return;
        }

        let (role, name) = match token.as_deref() {
            None => {
                if self.opts.allow_control {
                    (Role::Control, "控制端".to_string())
                } else {
                    (Role::Spectator, "观战".to_string())
                }
            }
            Some(t) => match parse_token(t) {
                Some(Token::Control(secret)) => match self.opts.control_token.as_deref() {
                    Some(expected) if !expected.is_empty() && expected == secret => {
                        (Role::Control, "控制端".to_string())
                    }
                    _ => {
                        self.hub
                            .send_error(conn_id, code::UNAUTHORIZED, "控制端令牌无效");
                        self.hub.unregister(conn_id);
                        return;
                    }
                },
                Some(Token::Rider(id)) => match self.world.rider(id) {
                    Some(r) => (Role::Rider(id), r.name.clone()),
                    None => {
                        self.hub.send_error(
                            conn_id,
                            code::UNAUTHORIZED,
                            &format!("骑手 {} 不存在", crate::sim::rider_key(id)),
                        );
                        self.hub.unregister(conn_id);
                        return;
                    }
                },
                Some(Token::New(nick)) => {
                    let nick = nick.trim();
                    let nick = if nick.is_empty() { "新骑手" } else { nick };
                    let id = self.world.spawn_human_rider(nick);
                    let name = self
                        .world
                        .rider(id)
                        .map(|r| r.name.clone())
                        .unwrap_or_else(|| nick.to_string());
                    tracing::info!(rider_id = id, %name, "新骑手接入");
                    (Role::Rider(id), name)
                }
                None => {
                    self.hub
                        .send_error(conn_id, code::UNAUTHORIZED, "无法识别的令牌");
                    self.hub.unregister(conn_id);
                    return;
                }
            },
        };

        self.hub.set_role(conn_id, role, name);
        self.hub.mark_ready(conn_id);

        let mut events = Vec::new();
        if let Role::Rider(id) = role {
            // 骑手连上来就是想跑单。手上有单时 set_online 会失败，忽略即可。
            if let Ok(evs) = scheduler::set_online(&mut self.world, id, true) {
                events = evs;
            }
        }

        let snapshot = render_snapshot(&self.world, role, self.world.elapsed_min);
        let welcome = crate::net::Welcome {
            protocol_version: PROTOCOL_VERSION,
            role: role.as_str(),
            rider_id: role.rider_id(),
            rider_key: role.rider_id().map(crate::sim::rider_key),
            tick_interval_ms: self.world.cfg.tick_interval_ms(),
            server_minute: self.world.sim_minute,
            snapshot,
        };
        let value = serde_json::to_value(&welcome).unwrap_or(serde_json::Value::Null);
        self.hub.send_to(conn_id, &Envelope::new("welcome", value));

        if !events.is_empty() {
            self.publish(&events);
        }
        self.push_server_info();
    }

    fn send_snapshot(&mut self, conn_id: ConnId, role: Role) {
        let snap = render_snapshot(&self.world, role, self.world.elapsed_min);
        self.hub.send_to(conn_id, &Envelope::new("snapshot", snap));
    }
}

/// `hello.token` 的解析结果。
#[derive(Debug, PartialEq, Eq)]
enum Token<'a> {
    /// 绑定到已存在的骑手。
    Rider(RiderId),
    /// 现场创建一个真人骑手，`&str` 是昵称。
    New(&'a str),
    /// 控制端，`&str` 要与 `RIDER_SIM_CONTROL_TOKEN` 一致。
    Control(&'a str),
}

/// 解析令牌。接受 `rider:3` / `r7` / `new:张师傅` / `control:<令牌>`。
fn parse_token(raw: &str) -> Option<Token<'_>> {
    let t = raw.trim();
    if let Some(secret) = t.strip_prefix("control:") {
        return Some(Token::Control(secret));
    }
    if let Some(nick) = t.strip_prefix("new:") {
        return Some(Token::New(nick));
    }
    let num = t
        .strip_prefix("rider:")
        .unwrap_or(t)
        .strip_prefix('r')
        .unwrap_or_else(|| t.strip_prefix("rider:").unwrap_or(t));
    num.parse::<RiderId>().ok().map(Token::Rider)
}

/// 给 `ClaimError` 找一个协议错误码。**`scheduler` 是唯一权威。**
fn claim_code(e: &ClaimError) -> &'static str {
    e.code()
}

/// 控制端指令的执行结果占位。`control` 用它区分「成功」和「业务失败」。
impl App {
    fn not_a_rider(&mut self, conn_id: ConnId, action: &str) {
        self.hub.send_ack(
            conn_id,
            &Ack::err(action, code::NOT_A_RIDER, "这个连接没有绑定骑手"),
        );
    }

    fn ack_err(&mut self, conn_id: ConnId, action: &str, e: ClaimError) {
        self.hub
            .send_ack(conn_id, &Ack::err(action, claim_code(&e), e.message()));
    }

    /// 处理 `Result<Vec<SimEvent>, ClaimError>`：成功就广播 + ack。
    fn finish_simple(
        &mut self,
        conn_id: ConnId,
        action: &str,
        r: Result<Vec<SimEvent>, ClaimError>,
    ) {
        match r {
            Ok(events) => {
                self.publish(&events);
                self.hub.send_ack(conn_id, &Ack::ok(action));
            }
            Err(e) => self.ack_err(conn_id, action, e),
        }
    }

    // -----------------------------------------------------------------
    // 抢单
    // -----------------------------------------------------------------

    fn do_claim(&mut self, conn_id: ConnId, rid: RiderId, order_id: OrderId, back_order: bool) {
        let result = if back_order {
            scheduler::claim_extra(&mut self.world, order_id, rid)
                .map(|(assignment, bonus)| (assignment, Some(bonus)))
        } else {
            scheduler::try_claim(&mut self.world, order_id, rid).map(|a| (a, None))
        };

        match result {
            Ok((assignment, bonus)) => {
                let events = self.claim_events(&assignment);
                self.publish(&events);
                let takehome = self
                    .world
                    .order(order_id)
                    .map(|o| o.fee.takehome_cents)
                    .unwrap_or(0);
                self.hub.send_ack(
                    conn_id,
                    &Ack::ok_with(
                        "claim",
                        json!({
                            "order_id": order_id,
                            "rider_id": rid,
                            "takehome_cents": takehome,
                            "planned_minutes": assignment.plan.total_min(),
                            "reason": assignment.reason,
                            "back_order_bonus_cents": bonus,
                        }),
                    ),
                );
            }
            Err(e) => self.ack_err(conn_id, "claim", e),
        }
    }

    /// 抢单成功要广播的事件：谁抢到了 + 该骑手的新状态。
    fn claim_events(&self, assignment: &crate::sim::ai::Assignment) -> Vec<SimEvent> {
        let mut events = Vec::new();
        let rider_name = self
            .world
            .rider(assignment.rider)
            .map(|r| r.name.clone())
            .unwrap_or_default();
        let (quoted, forced) = self
            .world
            .order(assignment.order)
            .map(|o| (o.quoted_fee_cents(), o.forced))
            .unwrap_or((0, false));
        events.push(SimEvent::OrderClaimed {
            order_id: assignment.order,
            rider_id: assignment.rider,
            rider_name,
            quoted_fee_cents: quoted,
            forced,
        });
        if let Some(ev) = scheduler::state_event(&self.world, assignment.rider) {
            events.push(ev);
        }
        events
    }

    // -----------------------------------------------------------------
    // 控制端
    // -----------------------------------------------------------------

    /// 控制端指令的统一入口。
    ///
    /// **权限检查在这里，不在 `net`。** `net` 只知道「谁连上来了」，
    /// 不知道「谁能改世界」。
    fn control<F>(&mut self, conn_id: ConnId, role: Role, action: &str, f: F)
    where
        F: FnOnce(&mut App) -> Option<serde_json::Value>,
    {
        if role != Role::Control {
            self.hub.send_ack(
                conn_id,
                &Ack::err(action, code::NOT_A_CONTROLLER, "只有控制端能改世界"),
            );
            return;
        }

        self.outbox.clear();
        self.outbox_err = None;
        let data = f(self);
        let events = std::mem::take(&mut self.outbox);
        let err = self.outbox_err.take();

        if !events.is_empty() {
            self.publish(&events);
        }
        if let Some(e) = err {
            return self.ack_err(conn_id, action, e);
        }

        // 配置类改动要让所有客户端知道，否则 UI 上的倍率/天气会停在旧值。
        self.broadcast_config_changed();

        match data {
            Some(v) => self.hub.send_ack(conn_id, &Ack::ok_with(action, v)),
            None => self.hub.send_ack(conn_id, &Ack::ok(action)),
        }
    }

    fn broadcast_config_changed(&mut self) {
        let ev = SimEvent::ConfigChanged {
            speed_multiplier: self.world.cfg.speed_multiplier,
            commission_rate: self.world.cfg.commission_rate,
            weather: self.world.weather,
        };
        self.publish(&[ev]);
    }

    // -----------------------------------------------------------------
    // 任务对话
    // -----------------------------------------------------------------

    /// 骑手在任务对话里说一句话：立刻回执，判定丢后台。
    fn do_task_message(&mut self, conn_id: ConnId, rid: RiderId, order_id: OrderId, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return self.hub.send_ack(
                conn_id,
                &Ack::err("task_message", code::BAD_REQUEST, "不能发空消息"),
            );
        }

        let Some(order) = self.world.order(order_id) else {
            return self.hub.send_ack(
                conn_id,
                &Ack::err(
                    "task_message",
                    ClaimError::OrderNotFound.code(),
                    "订单不存在",
                ),
            );
        };
        if order.assigned_to != Some(rid) {
            return self.hub.send_ack(
                conn_id,
                &Ack::err(
                    "task_message",
                    ClaimError::NotAssignedToYou.code(),
                    "这不是你的订单",
                ),
            );
        }
        let Some(task) = order.task.clone() else {
            return self.hub.send_ack(
                conn_id,
                &Ack::err("task_message", code::NO_TASK, "这一单没有附加任务"),
            );
        };

        let Some(tpl) = self.world.tasks.iter().find(|t| t.id == task.id).cloned() else {
            // 订单上挂着模板里没有的任务：数据问题，但不该让骑手卡住。
            return self.hub.send_ack(
                conn_id,
                &Ack::err("task_message", code::INTERNAL, "任务模板缺失，请联系管理员"),
            );
        };
        let Some(step_def) = tpl.steps.get(task.step).cloned() else {
            return self.hub.send_ack(
                conn_id,
                &Ack::err(
                    "task_message",
                    code::TASK_FINISHED,
                    "这一单的任务已经走完了",
                ),
            );
        };

        // 骑手这句先广播出去（本人 + 控制端能收到）
        self.publish(&[SimEvent::TaskMessage {
            order_id,
            from: "rider",
            text: text.clone(),
            step: task.step,
            task_id: task.id.clone(),
        }]);

        let scenario = format!("{}：{}", tpl.name, step_def.rider_prompt);
        let keywords = step_def.completion_keywords.clone();
        let judge = self.judge.clone();
        let tx = self.ruling_tx.clone();
        let step = task.step;
        let task_id = task.id.clone();

        tokio::spawn(async move {
            let ruling = judge.judge(&scenario, &text, &keywords).await;
            let msg = PendingRuling {
                rider_id: rid,
                order_id,
                step,
                task_id,
                rider_text: text,
                ruling,
            };
            if tx.send(msg).is_err() {
                tracing::debug!("主循环已退出，丢弃这次任务裁决");
            }
        });

        self.hub.send_ack(
            conn_id,
            &Ack::ok_with("task_message", json!({ "status": "judging" })),
        );
    }

    /// 后台裁决回来了。**这里是唯一改任务进度的地方。**
    fn on_ruling(&mut self, r: PendingRuling) {
        let mut events = Vec::new();

        if r.ruling.task_done {
            let total_steps = self.step_count(&r.task_id).max(1);
            if let Some(order) = self.world.order_mut(r.order_id)
                && let Some(task) = order.task.as_mut()
            {
                // 只有当裁决针对的正是当前步骤时才推进，防止乱序消息跳步。
                if task.step == r.step {
                    task.step += 1;
                    task.done = task.step >= total_steps;
                }
            }
            events.push(SimEvent::TaskMessage {
                order_id: r.order_id,
                from: "system",
                text: format!("✔ {}", r.ruling.reason),
                step: r.step,
                task_id: r.task_id.clone(),
            });

            // 完成奖励：钱进现金。金额来自模板，已经过夹取。
            let reward = self.step_reward(&r.task_id, r.step).unwrap_or(0);
            if reward != 0 {
                if let Some(rider) = self.world.rider_mut(r.rider_id) {
                    economy::credit(rider, reward);
                }
                let (takehome, hourly) = self
                    .world
                    .rider(r.rider_id)
                    .map(|x| (x.takehome_cents(), x.hourly_wage_cents()))
                    .unwrap_or((0, 0));
                events.push(SimEvent::RiderFinance {
                    rider_id: r.rider_id,
                    delta_cents: reward,
                    kind: "task_reward".into(),
                    reason: format!("附加任务完成：{}", r.ruling.reason),
                    takehome_cents: takehome,
                    hourly_wage_cents: hourly,
                });
            }
        } else {
            // 没完成：顾客回一句，或者解释为什么不算。
            let reply = self.customer_reply(&r.task_id, r.step, &r.ruling.reason);
            events.push(SimEvent::TaskMessage {
                order_id: r.order_id,
                from: "customer",
                text: reply,
                step: r.step,
                task_id: r.task_id.clone(),
            });
        }

        // 判定带来的评分变化（`TaskRuling` 已经把它夹到 [-0.1, 0.1]）
        if r.ruling.rating_delta != 0.0 {
            let cfg = self.world.cfg.clone();
            if let Some(rider) = self.world.rider_mut(r.rider_id) {
                economy::adjust_rating(&cfg, rider, r.ruling.rating_delta);
            }
        }

        self.publish(&events);
        tracing::debug!(
            rider_id = r.rider_id,
            order_id = r.order_id,
            done = r.ruling.task_done,
            source = r.ruling.source.as_str(),
            text = %r.rider_text,
            "任务裁决完成"
        );
    }

    fn step_count(&self, task_id: &str) -> usize {
        self.world
            .tasks
            .iter()
            .find(|t| t.id == task_id)
            .map(|t| t.steps.len())
            .unwrap_or(1)
    }

    fn step_reward(&self, task_id: &str, step: usize) -> Option<i64> {
        self.world
            .tasks
            .iter()
            .find(|t| t.id == task_id)?
            .steps
            .get(step)
            .map(|s| s.reward_cents)
    }

    /// 从模板里挑一句顾客回话。**走世界自己的固定种子 RNG**，
    /// 所以同一局重放出来的对话完全一致。
    fn customer_reply(&mut self, task_id: &str, step: usize, fallback: &str) -> String {
        use rand::RngExt;
        let candidates = self
            .world
            .tasks
            .iter()
            .find(|t| t.id == task_id)
            .and_then(|t| t.steps.get(step))
            .map(|s| s.customer_responses.clone())
            .unwrap_or_default();
        if candidates.is_empty() {
            return fallback.to_string();
        }
        let idx = self.world.rng.random_range(0..candidates.len());
        candidates[idx].clone()
    }
}

/// 从库里读回控制台存过的 LLM 配置。没 Key 就当没配过。
fn load_llm_config(store: &Store) -> Option<LlmConfig> {
    fn non_empty(s: Option<String>) -> Option<String> {
        s.filter(|s| !s.trim().is_empty())
    }

    let api_key = non_empty(store.meta("llm_api_key").ok().flatten())?;
    let mut cfg = LlmConfig::default();
    cfg.api_key = api_key;
    if let Some(u) = non_empty(store.meta("llm_base_url").ok().flatten()) {
        cfg.base_url = u;
    }
    if let Some(m) = non_empty(store.meta("llm_model").ok().flatten()) {
        cfg.model = m;
    }
    Some(cfg)
}

/// 从库里读回骑手名单。解析失败就当没存过。
fn load_rider_names(store: &Store) -> Option<Vec<String>> {
    let raw = store.meta("rider_names").ok().flatten()?;
    serde_json::from_str::<Vec<String>>(&raw).ok()
}

/// 兜底：按 UTC 折算「当日第几分钟」（0..1439）。
///
/// 只有控制端没给 `wall_minute` 时才用得上，所以不为了对齐钟表引入时区库——
/// 宁可差几个时区，也不给本地模拟器加一个依赖。
fn utc_minute_of_day() -> u32 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    ((secs % 86_400) / 60) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{RulingSource, TaskRuling};
    use crate::sim::order::ExtraTask;
    use tokio::sync::mpsc::UnboundedReceiver;

    fn world() -> World {
        let cfg = crate::sim::config::SimConfig {
            sim_rider_count: 3,
            ..Default::default()
        };
        World::new_for_test(cfg, 42)
    }

    fn app() -> App {
        App::new(world(), Store::in_memory().unwrap(), AppOptions::default())
    }

    fn control_app() -> App {
        let opts = AppOptions {
            allow_control: true,
            ..AppOptions::default()
        };
        App::new(world(), Store::in_memory().unwrap(), opts)
    }

    /// 接入一条连接，返回它的出站接收端。
    fn open(app: &mut App, id: ConnId) -> UnboundedReceiver<String> {
        let (tx, rx) = unbounded_channel();
        app.on_inbound(Inbound::Open { conn_id: id, tx });
        rx
    }

    fn send(app: &mut App, id: ConnId, msg: ClientMessage) {
        app.on_inbound(Inbound::Msg { conn_id: id, msg });
    }

    fn close(app: &mut App, id: ConnId) {
        app.on_inbound(Inbound::Closed { conn_id: id });
    }

    /// 把队列里积压的出站消息全部解析出来。
    fn drain(rx: &mut UnboundedReceiver<String>) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        while let Ok(t) = rx.try_recv() {
            out.push(serde_json::from_str(&t).expect("出站帧必须是合法 JSON"));
        }
        out
    }

    fn event<'a>(msgs: &'a [serde_json::Value], name: &str) -> Option<&'a serde_json::Value> {
        msgs.iter().find(|m| m["event"].as_str() == Some(name))
    }

    fn ack(msgs: &[serde_json::Value]) -> &serde_json::Value {
        event(msgs, "ack").expect("应该收到 ack")
    }

    fn rider_token(id: RiderId) -> String {
        format!("rider:{id}")
    }

    /// 完整握手，返回 (出站队列, welcome)。
    fn hello(
        app: &mut App,
        id: ConnId,
        token: Option<&str>,
    ) -> (UnboundedReceiver<String>, serde_json::Value) {
        let mut rx = open(app, id);
        send(
            app,
            id,
            ClientMessage::Hello {
                token: token.map(str::to_string),
                protocol_version: Some(PROTOCOL_VERSION.to_string()),
            },
        );
        let msgs = drain(&mut rx);
        let welcome = event(&msgs, "welcome")
            .expect("握手成功必须回 welcome")
            .clone();
        (rx, welcome)
    }

    // -----------------------------------------------------------------
    // 握手闸门
    // -----------------------------------------------------------------

    #[test]
    fn token_parsing_accepts_the_documented_forms() {
        assert_eq!(parse_token("rider:3"), Some(Token::Rider(3)));
        assert_eq!(parse_token("r3"), Some(Token::Rider(3)));
        assert_eq!(parse_token("  new:张师傅 "), Some(Token::New("张师傅")));
        assert_eq!(parse_token("garbage"), None);
        assert_eq!(parse_token(""), None);
    }

    #[test]
    fn first_message_must_be_hello() {
        let mut app = app();
        let mut rx = open(&mut app, 1);
        send(&mut app, 1, ClientMessage::Ping);
        let v = drain(&mut rx);
        let err = event(&v, "error").expect("没握手就该报错");
        assert_eq!(err["payload"]["code"].as_str(), Some(code::HELLO_REQUIRED));
    }

    #[test]
    fn bad_protocol_version_is_rejected_and_kicked() {
        let mut app = app();
        let mut rx = open(&mut app, 1);
        send(
            &mut app,
            1,
            ClientMessage::Hello {
                token: None,
                protocol_version: Some("0.0".into()),
            },
        );
        let v = drain(&mut rx);
        let err = event(&v, "error").expect("版本不符就该报错");
        assert_eq!(
            err["payload"]["code"].as_str(),
            Some(code::PROTOCOL_MISMATCH)
        );
        assert!(!app.hub.contains(1), "版本不符必须踢掉连接");
    }

    #[test]
    fn unknown_rider_token_is_unauthorized() {
        let mut app = app();
        let mut rx = open(&mut app, 1);
        send(
            &mut app,
            1,
            ClientMessage::Hello {
                token: Some("rider:999".into()),
                protocol_version: None,
            },
        );
        let v = drain(&mut rx);
        let err = event(&v, "error").expect("不存在的骑手就该报错");
        assert_eq!(err["payload"]["code"].as_str(), Some(code::UNAUTHORIZED));
        assert!(!app.hub.contains(1));
    }

    #[test]
    fn spectator_never_gets_a_rider_role_when_control_is_disabled() {
        let mut app = app();
        let (_rx, w) = hello(&mut app, 1, None);
        assert_eq!(w["payload"]["role"].as_str(), Some("spectator"));
        assert!(w["payload"]["rider_id"].is_null());
        assert_eq!(app.hub.role_of(1), Some(Role::Spectator));
    }

    #[test]
    fn rider_hello_auto_goes_online_and_gets_its_own_finance() {
        let mut app = app();
        let rid = app.world.spawn_human_rider("张三");
        assert!(!app.world.rider(rid).unwrap().online, "真人骑手默认下线");

        let (_rx, w) = hello(&mut app, 1, Some(&rider_token(rid)));
        assert_eq!(w["payload"]["role"].as_str(), Some("rider"));
        assert_eq!(w["payload"]["rider_id"].as_u64(), Some(rid as u64));
        assert_eq!(
            w["payload"]["rider_key"].as_str(),
            Some(format!("r{rid}").as_str())
        );
        assert!(app.world.rider(rid).unwrap().online, "连上就该跑单");

        let riders = w["payload"]["snapshot"]["riders"].as_array().unwrap();
        let me = riders
            .iter()
            .find(|r| r["id"].as_u64() == Some(rid as u64))
            .unwrap();
        assert!(me["takehome_cents"].is_number(), "本人版快照必须含收入");
        let other = riders.iter().find(|r| r["id"].as_u64() == Some(1)).unwrap();
        assert!(other["takehome_cents"].is_null(), "别人的收入绝不下发");
    }

    #[test]
    fn new_token_creates_a_human_rider() {
        let mut app = app();
        let before = app.world.riders.len();
        let (_rx, w) = hello(&mut app, 1, Some("new:李师傅"));
        assert_eq!(w["payload"]["role"].as_str(), Some("rider"));
        assert_eq!(app.world.riders.len(), before + 1);
        let rid = w["payload"]["rider_id"].as_u64().unwrap() as RiderId;
        assert_eq!(app.world.rider(rid).unwrap().name, "李师傅");
        assert!(app.world.rider(rid).unwrap().online);
    }

    // -----------------------------------------------------------------
    // 抢单
    // -----------------------------------------------------------------

    #[test]
    fn a_claim_moves_the_order_and_acks_with_the_report() {
        let mut app = app();
        let rid = app.world.spawn_human_rider("张三");
        let (mut rx, _w) = hello(&mut app, 1, Some(&rider_token(rid)));
        let oid = app.world.generate_order().expect("应该能出一单");

        send(
            &mut app,
            1,
            ClientMessage::Claim {
                order_id: oid,
                back_order: false,
            },
        );
        let v = drain(&mut rx);
        let a = ack(&v);
        assert_eq!(a["payload"]["action"].as_str(), Some("claim"));
        assert_eq!(a["payload"]["ok"].as_bool(), Some(true));
        assert_eq!(a["payload"]["data"]["order_id"].as_u64(), Some(oid as u64));
        assert_eq!(a["payload"]["data"]["rider_id"].as_u64(), Some(rid as u64));
        assert_eq!(app.world.order(oid).unwrap().assigned_to, Some(rid));
        assert!(event(&v, "order_claimed").is_some(), "抢单成功要广播");
    }

    #[test]
    fn a_second_claim_on_the_same_order_fails_and_does_not_steal_it() {
        let mut app = app();
        let r1 = app.world.spawn_human_rider("甲");
        let r2 = app.world.spawn_human_rider("乙");
        let (mut c1, _) = hello(&mut app, 1, Some(&rider_token(r1)));
        let (mut c2, _) = hello(&mut app, 2, Some(&rider_token(r2)));
        let oid = app.world.generate_order().expect("应该能出一单");

        send(
            &mut app,
            1,
            ClientMessage::Claim {
                order_id: oid,
                back_order: false,
            },
        );
        let first = drain(&mut c1);
        assert_eq!(ack(&first)["payload"]["ok"].as_bool(), Some(true));

        send(
            &mut app,
            2,
            ClientMessage::Claim {
                order_id: oid,
                back_order: false,
            },
        );
        let second = drain(&mut c2);
        let a = ack(&second);
        assert_eq!(a["payload"]["ok"].as_bool(), Some(false));
        assert_eq!(a["payload"]["code"].as_str(), Some("ALREADY_CLAIMED"));
        assert_eq!(app.world.order(oid).unwrap().assigned_to, Some(r1));
    }

    #[test]
    fn spectator_cannot_claim_or_control() {
        let mut app = app();
        let (mut rx, _w) = hello(&mut app, 1, None);
        let oid = app.world.generate_order().expect("应该能出一单");

        send(
            &mut app,
            1,
            ClientMessage::Claim {
                order_id: oid,
                back_order: false,
            },
        );
        let v = drain(&mut rx);
        assert_eq!(ack(&v)["payload"]["code"].as_str(), Some(code::NOT_A_RIDER));

        send(&mut app, 1, ClientMessage::SetPaused { paused: true });
        let v = drain(&mut rx);
        assert_eq!(
            ack(&v)["payload"]["code"].as_str(),
            Some(code::NOT_A_CONTROLLER)
        );
        assert_ne!(app.world.cfg.speed_multiplier, 0, "观战者不许暂停世界");
    }

    // -----------------------------------------------------------------
    // 控制端
    // -----------------------------------------------------------------

    #[test]
    fn set_realtime_aligns_the_clock_and_pushes_a_frame() {
        let mut app = control_app();
        let (mut ctrl, _) = hello(&mut app, 1, None);
        drain(&mut ctrl);

        send(
            &mut app,
            1,
            ClientMessage::SetRealtime {
                enabled: true,
                wall_minute: Some(14 * 60 + 30),
            },
        );
        assert!(app.world.cfg.realtime);
        assert_eq!(app.world.sim_minute, 870, "时钟要对齐真实 14:30");
        assert_eq!(
            app.world.cfg.tick_interval_ms(),
            crate::sim::config::REALTIME_TICK_MS
        );

        let v = drain(&mut ctrl);
        let a = ack(&v);
        assert_eq!(a["payload"]["data"]["realtime"].as_bool(), Some(true));
        assert_eq!(a["payload"]["data"]["wall_minute"].as_u64(), Some(870));
        assert_eq!(a["payload"]["data"]["tick_interval_ms"].as_u64(), Some(60_000));
        // 实时档 60 秒才一个 tick，调完时钟必须立刻补一帧，否则控制台会卡住。
        assert!(event(&v, "tick").is_some(), "对齐时钟后要推一帧 tick");

        // 对齐完要让主循环重排节拍，不然会被上一档剩下的节拍推走一分钟。
        assert!(app.beat_reset, "切实时档后要重排节拍");

        // 选倍率档 = 退出实时档。
        send(&mut app, 1, ClientMessage::SetSpeedMultiplier { multiplier: 4 });
        assert!(!app.world.cfg.realtime);
        assert_eq!(app.world.cfg.tick_interval_ms(), 250);
        assert!(app.beat_reset, "切倍率档后也要重排");
    }

    #[test]
    fn rider_renames_are_broadcast_and_batch_names_skip_blanks() {
        let mut app = control_app();
        let (mut ctrl, _) = hello(&mut app, 1, None);
        drain(&mut ctrl);
        let ids = app.world.sim_rider_ids();

        send(
            &mut app,
            1,
            ClientMessage::RenameRider {
                rider_id: ids[0],
                name: "  张三 ".into(),
            },
        );
        assert_eq!(app.world.rider(ids[0]).unwrap().name, "张三");
        let v = drain(&mut ctrl);
        assert_eq!(ack(&v)["payload"]["data"]["changed"].as_bool(), Some(true));
        assert!(
            event(&v, "rider_state_changed").is_some(),
            "改名要让所有端看到新名字"
        );

        // 批量名单：空名字不覆盖原名，顺序按 id 升序。
        send(
            &mut app,
            1,
            ClientMessage::SetRiderNames {
                names: vec!["   ".into(), "李四".into()],
            },
        );
        assert_eq!(app.world.rider(ids[0]).unwrap().name, "张三", "空名字要跳过");
        assert_eq!(app.world.rider(ids[1]).unwrap().name, "李四");

        // 非法名字：changed=false，世界不变。
        send(
            &mut app,
            1,
            ClientMessage::RenameRider {
                rider_id: ids[1],
                name: "   ".into(),
            },
        );
        assert_eq!(app.world.rider(ids[1]).unwrap().name, "李四");
        assert_eq!(app.world.sim_rider_names()[1], "李四");
    }

    #[test]
    fn saved_names_and_llm_config_can_be_read_back() {
        let store = Store::in_memory().unwrap();
        assert!(load_rider_names(&store).is_none());
        assert!(load_llm_config(&store).is_none());

        store.set_meta("rider_names", r#"["甲","乙"]"#).unwrap();
        assert_eq!(
            load_rider_names(&store).unwrap(),
            vec!["甲".to_string(), "乙".to_string()]
        );

        store.set_meta("llm_api_key", "sk-test").unwrap();
        store.set_meta("llm_model", "deepseek-chat").unwrap();
        let cfg = load_llm_config(&store).unwrap();
        assert!(cfg.has_key());
        assert_eq!(cfg.model, "deepseek-chat");
        assert_eq!(cfg.base_url, "https://api.openai.com/v1", "没存过就回默认");

        // 空 Key = 没配过；坏 JSON = 没存过。
        store.set_meta("llm_api_key", "   ").unwrap();
        assert!(load_llm_config(&store).is_none());
        store.set_meta("rider_names", "{不是数组").unwrap();
        assert!(load_rider_names(&store).is_none());
    }

    #[test]
    fn console_llm_config_never_echoes_the_key() {
        let mut app = control_app();
        assert!(!app.llm_enabled(), "默认没有 Key 就不能调 LLM");
        let (mut ctrl, _) = hello(&mut app, 1, None);
        drain(&mut ctrl);

        send(
            &mut app,
            1,
            ClientMessage::SetLlmConfig {
                api_key: Some("sk-secret-value".into()),
                base_url: Some("https://api.deepseek.com/v1".into()),
                model: Some("deepseek-chat".into()),
            },
        );

        assert!(app.llm_config.has_key());
        assert!(app.brain.is_enabled(), "填了 Key 就该有客户端");
        assert!(app.llm_enabled(), "有 Key = 允许调用");
        // 但「真的去调」仍要控制台显式打开开关，避免重启就烧配额。
        assert!(!app.world.cfg.llm_brain_enabled);

        let v = drain(&mut ctrl);
        let text = serde_json::to_string(&v).unwrap();
        assert!(!text.contains("sk-secret-value"), "Key 绝不能回传：{text}");
        assert_eq!(
            ack(&v)["payload"]["data"]["model"].as_str(),
            Some("deepseek-chat")
        );

        // 清空 Key：连大脑开关一起落下。
        send(
            &mut app,
            1,
            ClientMessage::SetLlmConfig {
                api_key: Some(String::new()),
                base_url: None,
                model: None,
            },
        );
        assert!(!app.llm_config.has_key());
        assert!(!app.llm_enabled());
        assert!(!app.brain.is_enabled());
        assert!(!app.world.cfg.llm_brain_enabled);
    }

    #[test]
    fn control_commands_change_the_world_and_notify_everyone() {
        let mut app = control_app();
        let (mut ctrl, w) = hello(&mut app, 1, None);
        assert_eq!(w["payload"]["role"].as_str(), Some("control"));

        let (mut rider, _) = hello(&mut app, 2, Some(&rider_token(1)));
        drain(&mut ctrl); // 骑手上线会给控制端推 server_info

        send(
            &mut app,
            1,
            ClientMessage::SetSpeedMultiplier { multiplier: 4 },
        );
        assert_eq!(app.world.cfg.speed_multiplier, 4);

        let v = drain(&mut ctrl);
        assert_eq!(
            ack(&v)["payload"]["data"]["speed_multiplier"].as_u64(),
            Some(4)
        );
        assert!(event(&v, "config_changed").is_some());

        let rv = drain(&mut rider);
        assert!(
            event(&rv, "config_changed").is_some(),
            "配置改动必须通知所有人"
        );
    }

    #[test]
    fn llm_toggle_without_a_key_stays_off() {
        let mut app = control_app();
        let (mut rx, _w) = hello(&mut app, 1, None);
        send(&mut app, 1, ClientMessage::SetLlmEnabled { enabled: true });
        let v = drain(&mut rx);
        assert_eq!(
            ack(&v)["payload"]["data"]["llm_enabled"].as_bool(),
            Some(false),
            "没有 Key 时打开也只是关闭"
        );
        assert!(!app.opts.llm_enabled);
        assert!(!app.world.cfg.llm_brain_enabled);
    }

    // -----------------------------------------------------------------
    // 连接生命周期
    // -----------------------------------------------------------------

    #[test]
    fn resync_and_ping_are_served() {
        let mut app = app();
        let (mut rx, _w) = hello(&mut app, 1, None);

        send(&mut app, 1, ClientMessage::Resync);
        let v = drain(&mut rx);
        assert!(event(&v, "snapshot").is_some());

        send(&mut app, 1, ClientMessage::Ping);
        let v = drain(&mut rx);
        let pong = event(&v, "pong").expect("ping 必须回 pong");
        assert_eq!(
            pong["payload"]["elapsed_min"].as_u64(),
            Some(app.world.elapsed_min)
        );
    }

    #[test]
    fn disconnecting_the_last_connection_takes_the_rider_offline() {
        let mut app = app();
        let rid = app.world.spawn_human_rider("张三");
        let (_rx, _w) = hello(&mut app, 1, Some(&rider_token(rid)));
        assert!(app.world.rider(rid).unwrap().online);

        close(&mut app, 1);
        assert!(!app.world.rider(rid).unwrap().online, "断线该下班");
    }

    #[test]
    fn a_rider_holding_orders_stays_online_after_disconnect() {
        let mut app = app();
        let rid = app.world.spawn_human_rider("张三");
        let (mut rx, _w) = hello(&mut app, 1, Some(&rider_token(rid)));
        let oid = app.world.generate_order().expect("应该能出一单");
        send(
            &mut app,
            1,
            ClientMessage::Claim {
                order_id: oid,
                back_order: false,
            },
        );
        assert_eq!(ack(&drain(&mut rx))["payload"]["ok"].as_bool(), Some(true));

        close(&mut app, 1);
        assert!(app.world.rider(rid).unwrap().online, "手上有单不能下线");
    }

    #[test]
    fn connection_limit_drops_extra_clients() {
        let opts = AppOptions {
            max_connections: 1,
            ..AppOptions::default()
        };
        let mut app = App::new(world(), Store::in_memory().unwrap(), opts);
        let _rx1 = open(&mut app, 1);
        let mut rx2 = open(&mut app, 2);
        assert!(app.hub.contains(1));
        assert!(!app.hub.contains(2), "超限连接不该进注册表");
        assert!(rx2.try_recv().is_err(), "被拒绝的连接应当立刻断开");
    }

    // -----------------------------------------------------------------
    // tick
    // -----------------------------------------------------------------

    #[test]
    fn a_tick_advances_the_clock_and_broadcasts_to_everyone() {
        let mut app = app();
        let (mut rx, _w) = hello(&mut app, 1, None);
        let before = app.world.elapsed_min;
        app.on_tick();
        assert_eq!(app.world.elapsed_min, before + 1);
        let v = drain(&mut rx);
        assert!(event(&v, "tick").is_some(), "每 tick 都要广播时钟");
    }

    #[test]
    fn a_paused_world_does_not_tick() {
        let mut app = app();
        app.world.cfg.speed_multiplier = 0;
        let before = app.world.elapsed_min;
        app.on_tick();
        assert_eq!(app.world.elapsed_min, before, "暂停时世界必须冻住");
    }

    #[test]
    fn brain_prepare_is_skipped_when_llm_is_off() {
        let mut app = app();
        assert!(!app.llm_enabled());
        app.maybe_start_plan();
        assert!(app.inflight_since.is_none());
        assert!(app.pending_plan.is_none());
    }

    // -----------------------------------------------------------------
    // 任务对话
    // -----------------------------------------------------------------

    /// 把一单挂到骑手名下，并塞一个「放门口」附加任务在指定步骤。
    fn attach_task(app: &mut App, oid: OrderId, rid: RiderId, step: usize) {
        let o = app.world.order_mut(oid).unwrap();
        o.assigned_to = Some(rid);
        o.task = Some(ExtraTask {
            id: "leave_at_door".into(),
            name: "放门口".into(),
            step,
            done: false,
            failed: false,
            reward_cents: 0,
            penalty_cents: 0,
        });
    }

    #[tokio::test]
    async fn task_message_requires_owning_the_order() {
        let mut app = app();
        let rid = app.world.spawn_human_rider("张三");
        let (mut rx, _w) = hello(&mut app, 1, Some(&rider_token(rid)));
        let oid = app.world.generate_order().expect("应该能出一单");

        send(
            &mut app,
            1,
            ClientMessage::TaskMessage {
                order_id: oid,
                text: "放门口".into(),
            },
        );
        let v = drain(&mut rx);
        assert_eq!(
            ack(&v)["payload"]["code"].as_str(),
            Some(ClaimError::NotAssignedToYou.code())
        );
    }

    #[test]
    fn a_ruling_advances_the_task_and_pays_the_reward() {
        let mut app = app();
        let rid = app.world.spawn_human_rider("张三");
        let (mut rx, _w) = hello(&mut app, 1, Some(&rider_token(rid)));
        let oid = app.world.generate_order().expect("应该能出一单");
        attach_task(&mut app, oid, rid, 1);

        let reward = app
            .step_reward("leave_at_door", 1)
            .expect("模板里该有这一档奖励");
        let before = app.world.rider(rid).unwrap().cash_cents;

        app.on_ruling(PendingRuling {
            rider_id: rid,
            order_id: oid,
            step: 1,
            task_id: "leave_at_door".into(),
            rider_text: "1234".into(),
            ruling: TaskRuling {
                task_done: true,
                reason: "说对密码".into(),
                rating_delta: 0.0,
                penalty_cents: 0,
                source: RulingSource::Keyword,
            },
        });

        let task = app.world.order(oid).unwrap().task.clone().unwrap();
        assert_eq!(task.step, 2, "判定通过要推进步骤");
        assert_eq!(app.world.rider(rid).unwrap().cash_cents, before + reward);

        let v = drain(&mut rx);
        assert!(event(&v, "rider_finance").is_some(), "完成奖励要发钱");
        assert!(event(&v, "task_message").is_some());
    }

    #[test]
    fn a_failed_ruling_keeps_the_step_and_lets_the_customer_answer() {
        let mut app = app();
        let rid = app.world.spawn_human_rider("张三");
        let (mut rx, _w) = hello(&mut app, 1, Some(&rider_token(rid)));
        let oid = app.world.generate_order().expect("应该能出一单");
        attach_task(&mut app, oid, rid, 1);
        let before = app.world.rider(rid).unwrap().cash_cents;

        app.on_ruling(PendingRuling {
            rider_id: rid,
            order_id: oid,
            step: 1,
            task_id: "leave_at_door".into(),
            rider_text: "啊？".into(),
            ruling: TaskRuling {
                task_done: false,
                reason: "没听懂".into(),
                rating_delta: 0.0,
                penalty_cents: 0,
                source: RulingSource::Keyword,
            },
        });

        let task = app.world.order(oid).unwrap().task.clone().unwrap();
        assert_eq!(task.step, 1, "没完成不能推进步骤");
        assert_eq!(app.world.rider(rid).unwrap().cash_cents, before);

        let v = drain(&mut rx);
        let msg = event(&v, "task_message").expect("顾客该回一句");
        assert_eq!(msg["payload"]["from"].as_str(), Some("customer"));
        assert!(!msg["payload"]["text"].as_str().unwrap().is_empty());
    }

    // -----------------------------------------------------------------
    // 远程控制接口（HTTP → Inbound::Rpc）
    // -----------------------------------------------------------------

    fn rpc(app: &mut App, req: RpcRequest) -> RpcReply {
        app.handle_rpc(req)
    }

    fn item(v: serde_json::Value) -> DispatchItem {
        serde_json::from_value(v).expect("测试里的派单载荷必须合法")
    }

    /// 找一个「在线且手上没单」的骑手——强制指派只吃这种。
    fn free_rider(app: &App) -> RiderId {
        app.world
            .riders
            .iter()
            .find(|r| r.online && r.held.is_empty())
            .map(|r| r.id)
            .expect("测试世界必须有空闲在线骑手")
    }

    #[test]
    fn rpc_state_uses_the_control_view_but_no_server_secrets() {
        let mut app = control_app();
        let v = rpc(&mut app, RpcRequest::State).expect("取快照不该失败");

        let riders = v["riders"].as_array().expect("要有骑手列表");
        assert!(!riders.is_empty());
        assert!(
            riders[0]["takehome_cents"].is_number(),
            "控制端版本必须带收入，否则没法核算"
        );
        assert!(v["hall"].is_array());
        assert!(v["server_info"]["hub"].is_object());
        // server_info 明确不带任何 Key（技术设计 §4.4）
        let info = &v["server_info"];
        assert!(info["llm_enabled"].is_boolean());
        assert!(info["amap_enabled"].is_boolean());
    }

    #[test]
    fn rpc_dispatch_assigns_the_order_and_tells_the_console() {
        let mut app = control_app();
        let (mut ctrl, _w) = hello(&mut app, 1, None);
        let oid = app.world.generate_order().expect("应该能出一单");
        let rid = free_rider(&app);

        let r = rpc(
            &mut app,
            RpcRequest::Dispatch(vec![item(json!({
                "rider_id": format!("r{rid}"),
                "order_id": oid,
                "reason": "老板指派，优先送",
            }))]),
        )
        .expect("派单不该失败");

        assert_eq!(r["applied"].as_u64(), Some(1));
        assert_eq!(r["failed"].as_u64(), Some(0));
        let first = &r["results"][0];
        assert_eq!(first["ok"].as_bool(), Some(true));
        assert_eq!(
            first["rider_key"].as_str(),
            Some(format!("r{rid}").as_str())
        );
        assert_eq!(first["reason"].as_str(), Some("老板指派，优先送"));
        assert_eq!(
            app.world.order(oid).unwrap().assigned_to,
            Some(rid),
            "订单必须真的落到那个骑手身上"
        );

        let msgs = drain(&mut ctrl);
        let claimed = event(&msgs, "order_claimed").expect("控制台要看到抢单");
        assert_eq!(claimed["payload"]["forced"].as_bool(), Some(true));
        let decision = event(&msgs, "ai_decision").expect("要留一条派单决策");
        assert_eq!(decision["payload"]["source"].as_str(), Some("manual"));
        assert_eq!(
            decision["payload"]["assignments"][0]["reason"].as_str(),
            Some("老板指派，优先送")
        );
    }

    #[test]
    fn rpc_dispatch_reports_each_failure_and_keeps_going() {
        let mut app = control_app();
        let oid = app.world.generate_order().expect("应该能出一单");
        let rid = free_rider(&app);

        let r = rpc(
            &mut app,
            RpcRequest::Dispatch(vec![
                // 解析不出来的 key
                item(json!({ "rider_id": "rider", "order_id": oid })),
                // 骑手不存在
                item(json!({ "rider_id": 9999, "order_id": oid })),
                // 正常
                item(json!({ "rider_id": rid, "order_id": oid })),
                // 同一单重复派
                item(json!({ "rider_id": rid, "order_id": oid })),
            ]),
        )
        .expect("批量派单本身不该失败");

        assert_eq!(r["applied"].as_u64(), Some(1));
        assert_eq!(r["failed"].as_u64(), Some(3));
        let results = r["results"].as_array().unwrap();
        assert_eq!(results[0]["code"].as_str(), Some("RIDER_NOT_FOUND"));
        assert_eq!(results[1]["code"].as_str(), Some("RIDER_NOT_FOUND"));
        assert_eq!(results[2]["ok"].as_bool(), Some(true));
        assert_eq!(results[3]["code"].as_str(), Some("ALREADY_CLAIMED"));
    }

    #[test]
    fn rpc_set_online_turns_a_business_error_into_a_conflict() {
        let mut app = control_app();
        let err = rpc(
            &mut app,
            RpcRequest::SetRiderOnline {
                rider_id: 9999,
                online: false,
            },
        )
        .expect_err("不存在的骑手该报错");
        assert_eq!(err.code, "RIDER_NOT_FOUND");
        assert_eq!(err.status, axum::http::StatusCode::CONFLICT);
    }

    #[test]
    fn rpc_set_online_works_and_broadcasts() {
        let mut app = control_app();
        let (mut ctrl, _w) = hello(&mut app, 1, None);
        let rid = free_rider(&app);

        let v = rpc(
            &mut app,
            RpcRequest::SetRiderOnline {
                rider_id: rid,
                online: false,
            },
        )
        .expect("空闲骑手可以下线");
        assert_eq!(v["online"].as_bool(), Some(false));
        assert!(!app.world.rider(rid).unwrap().online);

        let msgs = drain(&mut ctrl);
        assert!(event(&msgs, "rider_state_changed").is_some());
    }

    #[test]
    fn rpc_config_patch_is_clamped_and_broadcast() {
        let mut app = control_app();
        let (mut ctrl, _w) = hello(&mut app, 1, None);

        let patch = crate::net::ConfigPatch {
            // 越界值：服务端负责夹取，不需要客户端自觉
            claim_window_secs: Some(99),
            backorder_max: Some(0),
            ..Default::default()
        };
        let v = rpc(&mut app, RpcRequest::SetConfig(patch)).expect("改配置不该失败");

        assert_eq!(app.world.cfg.claim_window_secs, 10);
        assert_eq!(app.world.cfg.backorder.max_held, 1);
        assert_eq!(v["claim_window_secs"].as_u64(), Some(10));

        let msgs = drain(&mut ctrl);
        assert!(
            event(&msgs, "config_changed").is_some(),
            "配置改动必须广播，否则控制台停在旧值"
        );
    }
}
