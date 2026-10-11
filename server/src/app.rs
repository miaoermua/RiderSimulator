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

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::Duration;

use axum::http::StatusCode;
use serde_json::{Value, json};
use tokio::sync::{
    mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    watch,
};
use tokio::time::{Instant, sleep_until};

use crate::human::{self, HostMode};
use crate::llm::{
    AiLayer, AiOrchestrator, DirectorBrain, LayerKey, LayerModels, LlmClient, LlmConfig, ModelInfo,
    PlanFetch, PreparedPlan, PreparedWorldPlan, TacticalBrain, TaskJudge, TaskRuling,
    ValidatedWorldOp, WorldFetch,
    customer::{ChatReply, CustomerBrain, ReplySource},
};
use crate::net::{
    Ack, ApiAuth, ApiError, ClientMessage, ConnId, DispatchItem, Envelope, Hub, Inbound,
    PROTOCOL_VERSION, Role, RpcReply, RpcRequest, code, render_snapshot, server_info,
};
use crate::sim::{
    OrderId, RiderId,
    ai::LlmPlan,
    economy,
    event::{AiOpBrief, DecisionSource, SimEvent},
    geo::{LatLng, from_coord_system},
    rider::RiderKind,
    rider_key,
    scheduler::{self, ClaimError},
    station::{Station, StationKpi},
    tick::{self, TickInput},
    world::{CustomOrderSpec, World},
};
use crate::store::{ChatRecord, Store};

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
    /// 烽火台告诉骑手「连哪个地址」时该用哪个（`主机` 或 `主机:端口`）。
    ///
    /// 缺省时按 [`crate::net::join::reachable_hosts`] 的规则自己推：显式配置 >
    /// 监听地址 > 探测到的局域网 IP > 回环兜底。
    ///
    /// **开着代理 / VPN 的机器必须显式配它**：那种环境下默认路由指向虚拟网卡，
    /// 探测出来的是 fake-ip 段，骑手照着连必然超时。
    pub public_addr: Option<String>,
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
            public_addr: None,
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

/// 一个等着开口的顾客。
///
/// 由 `on_tick` 从 [`SimEvent::ChatOpen`] 收下来，攒进队列。**不在收到的那一刻
/// 就发请求**：LLM 占坑是全局串行的（见 [`AiOrchestrator`]），有别的层在飞时
/// 得让路，所以先排队、下一 tick 再试。
#[derive(Debug)]
struct PendingChat {
    rider_id: RiderId,
    order_id: OrderId,
    /// 画像一行摘要，直接进提示词。
    context: String,
    /// LLM 不可用时的兜底开场白（零延迟、可复现）。
    fallback: String,
}

/// 后台送回来的一次顾客回应。
#[derive(Debug)]
struct ChatInbound {
    rider_id: RiderId,
    order_id: OrderId,
    reply: ChatReply,
}

/// 后台「列出模型」的结果。
///
/// 带上 `conn_id` 是因为这是**点名要的**：谁点的回给谁，
/// 不像世界状态那样广播给所有控制端。
#[derive(Debug)]
struct ModelListing {
    conn_id: ConnId,
    outcome: Result<Vec<ModelInfo>, String>,
}

/// 后台任务送回来的一轮 AI 结果。
///
/// 三层**共用同一条通道**：编排器保证同一时刻只有一个在飞请求，
/// 所以回来的必定是当时占坑的那一层。
#[derive(Debug)]
enum AiInbound {
    /// 骑手层：一批动作 + 策略。`u64` 是决策 id。
    Rider(u64, PreparedPlan, PlanFetch),
    /// 烽火台层：世界参数调整 + 旁白。`u64` 是决策 id。
    Director(u64, PreparedWorldPlan, WorldFetch),
}

/// 主循环。
pub struct App {
    pub world: World,
    pub hub: Hub,
    pub store: Store,
    pub opts: AppOptions,
    /// 实际绑定的监听地址，[`App::serve`] 回填。
    ///
    /// 烽火台要拿它拼「手机的扫码地址」，所以不能在 `new` 里写死——
    /// 端口 0 只有在 `bind` 之后才知道最后落到哪儿。
    bound: Option<SocketAddr>,

    brain: TacticalBrain,
    judge: TaskJudge,
    /// 烽火台大脑（世界导演）。只碰世界参数，不碰具体的人和单。
    director: DirectorBrain,
    /// **AI 顾客**：末端交付沟通里「说什么、要不要索赔」。
    ///
    /// 与三层共用同一个 [`AiOrchestrator`]，所以它花的额度就是派单决策的额度。
    customer: CustomerBrain,
    /// 三层 LLM 的统一出口。**由它决定「这一 tick 轮到哪一层」。**
    orchestrator: AiOrchestrator,
    /// LLM 接入配置（Key / Base URL / 模型）。控制台可改，落库后重启仍生效。
    ///
    /// **绝不下发**：任何给客户端的地方都只报「配没配」，不报内容。
    llm_config: LlmConfig,
    /// 分层模型（成本优化）。全空 = 三层共用全局模型。
    layer_models: LayerModels,

    inbound_tx: UnboundedSender<Inbound>,
    inbound_rx: Option<UnboundedReceiver<Inbound>>,
    ruling_tx: UnboundedSender<PendingRuling>,
    ruling_rx: Option<UnboundedReceiver<PendingRuling>>,
    plan_tx: UnboundedSender<AiInbound>,
    plan_rx: Option<UnboundedReceiver<AiInbound>>,
    /// 「列出模型」的后台结果回传口（网络慢，不能在主循环里等）。
    models_tx: UnboundedSender<ModelListing>,
    models_rx: Option<UnboundedReceiver<ModelListing>>,
    /// 顾客回应的后台回传口（同上：绝不把循环卡在 HTTP 上）。
    chat_tx: UnboundedSender<ChatInbound>,
    chat_rx: Option<UnboundedReceiver<ChatInbound>>,
    /// 攒着等开口的顾客。**有上限**：堆太多说明额度长期不够，
    /// 与其无限增长，不如把最老的丢掉——反正它那份兜底话术也过期了。
    pending_chats: VecDeque<PendingChat>,

    /// 已占坑、正在后台打 HTTP 的那一轮。`None` = 没有在途请求。
    inflight_since: Option<Instant>,
    /// 在飞的是哪一层（用于超时兜底时把编排器的坑还回去）。
    inflight_layer: Option<AiLayer>,
    /// 已经拿到、等下一个 tick 消费的计划。
    pending_plan: Option<LlmPlan>,
    /// 已经校验、等下一 tick 落地的世界操作（烽火台）。
    pending_world_ops: Vec<ValidatedWorldOp>,
    /// 烽火台自己的操作史，写进下一轮 prompt（「别反复横跳」）。
    world_history: Vec<String>,
    /// 真人在线时允许 AI 代管的模式（见 `human::host`）。
    host_mode: HostMode,
    /// 下一个 AI 决策 id（人工否决用它点名）。
    next_decision_id: u64,
    /// 已产出、**尚未落地**、因此还能被人工否决的那一条决策：`(id, layer)`。
    pending_decision: Option<(u64, &'static str)>,
    /// 人工否决记录（新的在前）。控制台展示，也留给后续决策参考。
    reject_log: Vec<serde_json::Value>,
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
        let (models_tx, models_rx) = unbounded_channel();
        let (chat_tx, chat_rx) = unbounded_channel();

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
        // 分层模型：骑手层高频要便宜，烽火台层低频可以贵一点。
        // 没配（默认）时三层都用全局模型，行为与从前逐字节一致。
        //
        // 控制台改过的分层模型落库，重启后优先于环境变量——否则在界面上
        // 选好的模型一重启就被 `RIDER_SIM_*` 覆盖回去，很难解释。
        let layer_models = load_layer_models(&store).unwrap_or_else(LayerModels::from_env);
        let (brain, judge, director, customer) = if llm_config.has_key() {
            (
                TacticalBrain::from_config(&llm_config.for_layer(&layer_models, LayerKey::Rider)),
                TaskJudge::from_config(&llm_config),
                DirectorBrain::from_config(
                    &llm_config.for_layer(&layer_models, LayerKey::Director),
                ),
                CustomerBrain::from_config(&llm_config),
            )
        } else {
            (
                TacticalBrain::disabled(),
                TaskJudge::disabled(),
                DirectorBrain::disabled(),
                CustomerBrain::disabled(),
            )
        };

        // 三层编排器：系统里唯一的 LLM 调用出口。没 Key 就是关闭状态。
        let mut orchestrator = AiOrchestrator::from_config(&llm_config);
        // 站长层的决策（限单 / 强派）目前**搭骑手层那一轮的车**一起下发
        // （见 `llm::protocol` 的 `station_ops`），所以编排器里不再单独占坑。
        orchestrator.set_layer_enabled(AiLayer::Station, false);

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
            bound: None,
            layer_models,
            brain,
            judge,
            director,
            customer,
            orchestrator,
            llm_config,
            inbound_tx,
            inbound_rx: Some(inbound_rx),
            ruling_tx,
            ruling_rx: Some(ruling_rx),
            plan_tx,
            plan_rx: Some(plan_rx),
            models_tx,
            models_rx: Some(models_rx),
            chat_tx,
            chat_rx: Some(chat_rx),
            pending_chats: VecDeque::new(),
            inflight_since: None,
            inflight_layer: None,
            pending_plan: None,
            pending_world_ops: Vec::new(),
            world_history: Vec::new(),
            host_mode: HostMode::default(),
            next_decision_id: 1,
            pending_decision: None,
            reject_log: Vec::new(),
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
    pub async fn serve(mut self, addr: SocketAddr) -> std::io::Result<()> {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let bound = listener.local_addr().unwrap_or(addr);
        self.bound = Some(bound);
        let join_hosts = crate::net::join::reachable_hosts(bound, self.opts.public_addr.as_deref());
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
        match join_hosts.first() {
            Some(h) if bound.ip().is_loopback() => tracing::warn!(
                join = %h,
                "只监听了回环地址，手机连不上——要让骑手扫码进来，请设 RIDER_SIM_ADDR=0.0.0.0:8080"
            ),
            Some(h) => tracing::info!(join = %h, "骑手扫码地址（烽火台面板里也能看到）"),
            None => {}
        }

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
    /// `select!` 的几条腿：定时 tick、网络消息、任务裁决、LLM 计划、模型清单。
    /// 只有第一条会推进世界，其余几条都只是「改变输入」。
    pub async fn run(mut self, mut shutdown: watch::Receiver<bool>) -> std::io::Result<()> {
        let mut inbound = self.inbound_rx.take().expect("run 只能调用一次");
        let mut rulings = self.ruling_rx.take().expect("run 只能调用一次");
        let mut plans = self.plan_rx.take().expect("run 只能调用一次");
        let mut models = self.models_rx.take().expect("run 只能调用一次");
        let mut chats = self.chat_rx.take().expect("run 只能调用一次");

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
                Some(ai) = plans.recv() => self.on_ai(ai),
                Some(m) = models.recv() => self.on_models(m),
                Some(c) = chats.recv() => self.on_chat_reply(c),
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
            if let Some(layer) = self.inflight_layer.take() {
                self.orchestrator.record_fallback(layer);
                self.orchestrator.finish(layer);
            }
        }

        // 烽火台上一轮拿到的世界操作，在这一 tick 前落地。
        self.apply_pending_world_ops();
        if matches!(self.pending_decision, Some((_, "director"))) {
            self.pending_decision = None; // 已落地，不能再撤
        }

        let plan = self.pending_plan.take();
        if plan.is_some() && matches!(self.pending_decision, Some((_, "rider"))) {
            self.pending_decision = None; // 已交给 tick，不能再撤
        }
        // 交出去的只是结论：谁是真人、该不该托管，都由 `human/` 判完再进来。
        let hosted_humans = self.hosted_humans();
        let events = tick::tick(
            &mut self.world,
            TickInput {
                llm_plan: plan,
                ai_off: self.host_mode == HostMode::Off,
                hosted_humans,
            },
        );
        self.tick_count += 1;

        // 顾客开口是**内部信号**：sim 层只负责掷骰子（可复现、零 token），
        // 「说什么」归 app 层。先收下来排队，等坑位空了再发请求——
        // **绝不在这里 await**，否则一次 HTTP 就把整个 tick 卡住了。
        for ev in &events {
            if let SimEvent::ChatOpen {
                order_id,
                rider_id,
                context,
                fallback,
            } = ev
            {
                // 队列有上限：堆太多说明额度长期不够，与其无限增长，
                // 不如丢掉最老的——它那份兜底开场白的时点早就过去了。
                if self.pending_chats.len() >= 32 {
                    self.pending_chats.pop_front();
                }
                self.pending_chats.push_back(PendingChat {
                    rider_id: *rider_id,
                    order_id: *order_id,
                    context: context.clone(),
                    fallback: fallback.clone(),
                });
            }
        }

        self.publish(&events);

        if self.opts.server_info_every_ticks > 0
            && self
                .tick_count
                .is_multiple_of(self.opts.server_info_every_ticks)
        {
            self.push_server_info();
        }
        self.maybe_start_plan();
        // 派单决策优先占坑；它没占上（或已经飞完）时，顾客才有机会开口。
        // 同一时刻只有一个在飞，所以对话确实会挤掉派单——这是当初的取舍。
        self.maybe_start_chat();
    }

    /// 广播事件，并顺手处理需要落库的部分。
    fn publish(&mut self, events: &[SimEvent]) {
        for ev in events {
            // `ChatOpen` 是内部信号，`on_tick` 已经把它落成一条 `ChatMessage` 了；
            // 这里再发一遍，客户端会收到一条没有内容的空消息。
            if matches!(ev, SimEvent::ChatOpen { .. }) {
                continue;
            }
            if let SimEvent::DayClosed { day, ledgers, .. } = ev {
                match self.store.save_day(*day, ledgers) {
                    Ok(r) => tracing::info!(day, ledgers = r.ledgers, "日结已落库"),
                    Err(e) => {
                        tracing::warn!(error = %e, "日结落库失败，本局数据只留在内存")
                    }
                }
            }
            // 会话落库：**包括骑手自己说的那条**。落库失败只是丢一条聊天记录，
            // 绝不能连累广播——玩家该看到的消息还得照发。
            if let SimEvent::ChatMessage {
                order_id,
                from,
                text,
                kind,
                day,
                sim_minute,
                compensation_cents,
                fault,
            } = ev
                && let Err(e) = self.store.save_chat(&ChatRecord {
                    order_id: *order_id,
                    day: *day,
                    sim_minute: *sim_minute,
                    sender: *from,
                    kind: *kind,
                    text: text.as_str(),
                    compensation_cents: *compensation_cents,
                    fault: *fault,
                })
            {
                tracing::warn!(error = %e, order = order_id, "会话消息落库失败");
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

    /// 三层 AI 的完整状态。控制台 AI Tab（`server_info.ai`）与
    /// `GET /api/ai` 共用这一份口径，避免两边各说各话。
    ///
    /// **不含任何 Key**，只报「配没配」。
    fn ai_state(&self) -> serde_json::Value {
        let station = Station::for_world(&self.world);
        let kpi = StationKpi::compute(&self.world, &station);
        let bs = self.brain.stats();
        let cache_total = bs.cached_hits + bs.calls;
        let hit_rate = if cache_total == 0 {
            0.0
        } else {
            bs.cached_hits as f64 / cache_total as f64
        };
        json!({
            "llm_configured": self.llm_config.has_key(),
            "llm_model": self.llm_config.model,
            "llm_base_url": self.llm_config.base_url,
            "brain_enabled": self.world.cfg.llm_brain_enabled,
            "host_mode": self.host_mode.key(),
            // 成本优化：三层各用什么模型 + 指纹缓存命中率。
            "models": LayerKey::ALL.iter().map(|&l| {
                (l.key().to_string(), json!(self.llm_config.model_for(&self.layer_models, l)))
            }).collect::<serde_json::Map<_, _>>(),
            "cache": {
                "hits": bs.cached_hits,
                "calls": bs.calls,
                "hit_rate": hit_rate,
            },
            "budget": {
                "calls_today": self.orchestrator.calls_today(self.world.day),
                "max_calls_per_day": self.orchestrator.max_calls_per_day(),
                "inflight": self.orchestrator.inflight().map(|l| l.key()),
            },
            "layers": AiLayer::ALL.iter().map(|&l| {
                let s = self.orchestrator.stats(l);
                json!({
                    "layer": l.key(),
                    "enabled": self.orchestrator.layer_enabled(l),
                    "interval_min": l.min_interval_min(),
                    "calls": s.calls,
                    "failures": s.failures,
                    "applied": s.applied,
                    "dropped": s.dropped,
                    "fallbacks": s.fallbacks,
                })
            }).collect::<Vec<_>>(),
            "station": {
                "id": station.id,
                "name": station.name,
                "rider_count": station.rider_ids.len(),
                "kpi": kpi,
            },
            // 还能撤的那一条决策（控制台据此亮「否决」按钮）。
            "rejectable": self.pending_decision.map(|(id, layer)| json!({
                "decision_id": id,
                "layer": layer,
            })),
            "reject_log": self.reject_log,
        })
    }

    /// `POST /api/ai`：改开关与托管模式。**只改给出的字段**。
    fn rpc_set_ai(&mut self, patch: &crate::net::AiPatch) -> RpcReply {
        if let Some(on) = patch.enabled {
            self.world.cfg.llm_brain_enabled = on;
        }
        for (layer, flag) in [
            (AiLayer::Rider, patch.rider),
            (AiLayer::Station, patch.station),
            (AiLayer::Director, patch.director),
        ] {
            if let Some(on) = flag {
                // 站长层搭骑手层的车（`station_ops`），不单独发请求：开了它只是
                // 让「站长该决策了」这一轮到点时也顺带跑一轮骑手（见 `maybe_start_plan`）。
                self.orchestrator.set_layer_enabled(layer, on);
            }
        }
        if let Some(mode) = patch.host_mode.as_deref() {
            let Some(m) = HostMode::from_key(mode) else {
                return Err(ApiError::bad_request(format!(
                    "未知托管模式：{mode}（可选 simulated_only / autopilot / off）"
                )));
            };
            self.host_mode = m;
        }
        Ok(self.ai_state())
    }

    /// `POST /api/ai/reject`：人工否决。
    ///
    /// 只有**还没落地**的决策撤得掉（骑手层的待指派、烽火台的待生效世界操作）。
    /// 已经生效的（策略、已经派出去的单）撤不回来，但会记进否决台账——
    /// 控制台看得见「人否决了什么」，后续 AI 也会读到这份记录。
    fn rpc_reject_ai(&mut self, decision_id: u64) -> RpcReply {
        let layer = match self.pending_decision {
            Some((id, layer)) if id == decision_id => {
                match layer {
                    "rider" => {
                        self.pending_plan = None;
                    }
                    "director" => {
                        self.pending_world_ops.clear();
                    }
                    _ => {}
                }
                self.pending_decision = None;
                self.reject_log.insert(
                    0,
                    json!({ "decision_id": decision_id, "layer": layer, "result": "reverted" }),
                );
                Some(layer)
            }
            _ => {
                self.reject_log.insert(
                    0,
                    json!({ "decision_id": decision_id, "layer": "unknown", "result": "already_applied" }),
                );
                None
            }
        };
        // 台账只留最近 20 条。
        self.reject_log.truncate(20);

        Ok(json!({
            "ok": layer.is_some(),
            "rejected": layer.is_some(),
            "decision_id": decision_id,
            "layer": layer,
            "reason": if layer.is_some() { "已撤销，未落地" } else { "该决策已生效或不存在，仅记入台账" },
        }))
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
            // 三层 AI 的统一口径：开关、预算、各层统计、站点 KPI、否决记录。
            obj.insert("ai".into(), self.ai_state());
            // 只发给控制端（send_to_control），所以可以回模型信息；
            // **Key 永远不出现在这里**，只报「配没配」。
            obj.insert("llm_configured".into(), json!(self.llm_config.has_key()));
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

    /// 如果条件允许，占一层坑并把 HTTP 丢给后台。
    ///
    /// 「轮到哪一层」全交给 [`AiOrchestrator::next_due`]：它统一管预算、间隔、
    /// 并发（同一时刻只有一个在飞）。这里只负责按层的类型组装请求。
    fn maybe_start_plan(&mut self) {
        if self.inflight_since.is_some() {
            return;
        }
        if !self.llm_enabled() || !self.world.cfg.llm_brain_enabled {
            return;
        }
        // 托管模式「全停机」：连 LLM 都不调了，纯手动跑单。
        if self.host_mode == HostMode::Off {
            return;
        }
        match self.orchestrator.next_due(&self.world) {
            Some(AiLayer::Director) => self.start_director(),
            Some(AiLayer::Rider) => self.start_rider(),
            // 站长层**搭骑手层的车**：限单 / 强派这些 `station_ops` 跟着骑手层那一轮
            // 请求一起下发（见 `llm::protocol`），站长自己不单独发请求。
            //
            // 但得记掉它的间隔——否则它会永远「到期」，`next_due` 每次都返回它，
            // 骑手层再也轮不上（控制台把站长开关打开就会踩到）。下一 tick 自然轮到骑手。
            Some(AiLayer::Station) => {
                self.orchestrator
                    .skip_interval(AiLayer::Station, &self.world);
                self.start_rider();
            }
            // 顾客对话**事件驱动**，不在 `ALL` 里，永远不会从 `next_due` 冒出来。
            // 它走 [`App::maybe_start_chat`] 那条独立入口。写出来只为让编译器满意。
            Some(AiLayer::Customer) => {}
            None => {}
        }
    }

    /// 这一 tick 该交给 AI 代管的**真人**（喂给 [`TickInput::hosted_humans`]）。
    ///
    /// 判定只有 [`human::ai_should_drive`] 一条。名单**每 tick 现算、不缓存**：
    /// 玩家断线要立刻让 AI 接手，缓存一拍就会出现「玩家已经走了，骑手还杵着」。
    fn hosted_humans(&self) -> Vec<RiderId> {
        human::hosted_humans(&self.world, self.host_mode, |id| {
            self.hub.rider_conn_count(id) > 0
        })
    }

    /// 骑手层该给谁派单。
    ///
    /// 名单由 [`tick::ai_roster`] 算：**在线的真人一律让位**（玩家在操作），掉线的
    /// 真人只有在 AutoPilot 下才由 AI 接手——那是 `human/` 的判定，这里只把结论
    /// 翻成 `hosted_humans` 递下去。LLM 的 prompt 和算法兜底共用同一份名单，
    /// 否则「LLM 推荐的人」和「校验时合法的人」会对不上。
    fn ai_roster(&self) -> Vec<RiderId> {
        tick::ai_roster(
            &self.world,
            &TickInput {
                ai_off: self.host_mode == HostMode::Off,
                hosted_humans: self.hosted_humans(),
                ..Default::default()
            },
        )
    }

    /// 占骑手层的坑，组装一批动作 + 策略的请求。
    fn start_rider(&mut self) {
        let idle = self.ai_roster();
        let pending = self.world.pending_orders();
        let prepared = match self.brain.prepare(&self.world, &idle, &pending) {
            Ok(p) => p,
            Err(outcome) => {
                // Disabled / Throttled 都属于「正常不调」，只记不给客户端刷屏
                self.last_brain_note = outcome.note;
                return;
            }
        };
        // 先占坑再发请求：万一超时，这一档也不会重试到天亮。
        if !self.orchestrator.begin(AiLayer::Rider, &self.world) {
            return;
        }
        let tx = self.plan_tx.clone();
        let id = self.take_decision_id();
        self.inflight_since = Some(Instant::now());
        self.inflight_layer = Some(AiLayer::Rider);
        tokio::spawn(async move {
            let fetched = prepared.request().await;
            if tx.send(AiInbound::Rider(id, prepared, fetched)).is_err() {
                tracing::debug!("主循环已退出，丢弃这一轮 LLM 结果");
            }
        });
    }

    /// 占烽火台层的坑，组装世界参数请求。
    fn start_director(&mut self) {
        let history = self.world_history.clone();
        let prepared = match self.director.prepare(&self.world, &history) {
            Ok(p) => p,
            Err(outcome) => {
                self.last_brain_note = outcome.note;
                return;
            }
        };
        if !self.orchestrator.begin(AiLayer::Director, &self.world) {
            return;
        }
        let tx = self.plan_tx.clone();
        let id = self.take_decision_id();
        self.inflight_since = Some(Instant::now());
        self.inflight_layer = Some(AiLayer::Director);
        tokio::spawn(async move {
            let fetched = prepared.request().await;
            if tx.send(AiInbound::Director(id, prepared, fetched)).is_err() {
                tracing::debug!("主循环已退出，丢弃这一轮 LLM 结果");
            }
        });
    }

    /// 分配一个决策 id。**单调递增**，控制台凭它否决某一条具体决策。
    fn take_decision_id(&mut self) -> u64 {
        let id = self.next_decision_id;
        self.next_decision_id += 1;
        id
    }

    /// 后台结果回来的统一入口，按层分发。
    fn on_ai(&mut self, ai: AiInbound) {
        match ai {
            AiInbound::Rider(id, p, f) => self.on_plan(id, p, f),
            AiInbound::Director(id, p, f) => self.on_world_plan(id, p, f),
        }
    }

    fn on_plan(&mut self, decision_id: u64, prepared: PreparedPlan, fetched: PlanFetch) {
        self.inflight_since = None;
        self.inflight_layer = None;
        self.orchestrator.finish(AiLayer::Rider);
        // 校验必须针对**当下**的名单：这几秒里玩家可能刚连上线（AI 该让位），
        // 也可能刚掉线（AutoPilot 下该接手）。
        let idle = self.ai_roster();
        let pending = self.world.pending_orders();
        let (outcome, plan) =
            self.brain
                .finish(prepared, fetched, &mut self.world, &idle, &pending);
        self.last_brain_note = format!("[{}] {}", outcome.source.as_str(), outcome.note);

        // 编排器统一记账：回退、采纳、丢弃。
        if outcome.source == crate::llm::BrainSource::Fallback {
            self.orchestrator.record_fallback(AiLayer::Rider);
        }
        self.orchestrator.record_verdict(
            AiLayer::Rider,
            outcome.verdict.assignments.len() as u64,
            outcome.verdict.dropped as u64,
        );

        if outcome.source != crate::llm::BrainSource::Disabled {
            let stats = self.brain.stats();
            // 策略下发也一并推给控制端：这是「骑手行为为什么变了」的唯一解释
            let policies: Vec<_> = outcome
                .policies
                .iter()
                .map(|p| {
                    json!({
                        "rider": crate::sim::rider_key(p.rider),
                        "price": p.price.key(),
                        "cap": p.cap,
                        "mode": p.mode.map(|m| m.index()),
                        "why": p.why,
                    })
                })
                .collect();
            // 站长指令摘要：这是「为什么某个骑手被限单 / 下线了」的唯一解释
            let station_ops: Vec<_> = outcome
                .station_ops
                .iter()
                .map(|o| {
                    json!({
                        "op": o.op,
                        "rider": o.rider,
                        "detail": o.detail,
                        "why": o.why,
                    })
                })
                .collect();
            self.hub.send_to_control(&Envelope::new(
                "llm_brain",
                json!({
                    "decision_id": decision_id,
                    "layer": "rider",
                    "source": outcome.source.as_str(),
                    "note": outcome.note,
                    "model": outcome.model,
                    "latency_ms": outcome.latency_ms,
                    "assignments": outcome.verdict.assignments.len(),
                    "dropped": outcome.verdict.dropped,
                    "policies": policies,
                    "station_ops": station_ops,
                    "fingerprint": format!("{:016x}", outcome.fingerprint),
                    // 指纹命中：这一轮没花钱，决策复用了上一轮。
                    "cached": outcome.source == crate::llm::BrainSource::Cache,
                    "prompt": {
                        "riders": outcome.prompt_riders,
                        "orders": outcome.prompt_orders,
                        "bytes": outcome.prompt_bytes,
                    },
                    "trim": {
                        "riders": outcome.trim.trimmed_riders,
                        "orders": outcome.trim.trimmed_orders,
                        "remarks": outcome.trim.truncated_remarks,
                    },
                    "calls_today": self.brain.calls_today(),
                    "max_calls_per_day": self.brain.max_calls_per_day(),
                    "stats": {
                        "calls": stats.calls,
                        "failures": stats.failures,
                        "applied": stats.applied,
                        "dropped": stats.dropped,
                        "cached_hits": stats.cached_hits,
                        "policies_applied": stats.policies_applied,
                    },
                }),
            ));
        }

        if let Some(mut plan) = plan {
            plan.decision_id = decision_id;
            // tick 里的 run_ai 会再校验一次，然后把它变成真实指派。
            self.pending_plan = Some(plan);
            // 还没落地 —— 控制台在下一 tick 之前否决，这单就真不派了。
            self.pending_decision = Some((decision_id, "rider"));
        }

        // 站长的运营指令已经生效（cap / 下线 / 召回），推一条事件进统一决策流，
        // 让所有角色都看得见「站长今天在管什么」，也让人能顺手否决它。
        // 决策 id 用同一轮的：站长指令是搭骑手层这一轮车下来的。
        if !outcome.station_ops.is_empty() {
            let ops = outcome
                .station_ops
                .iter()
                .map(|o| AiOpBrief {
                    op: o.op,
                    target: o.rider.clone(),
                    detail: o.detail.clone(),
                    why: o.why.clone(),
                })
                .collect();
            self.publish(&[SimEvent::AiDecision {
                source: DecisionSource::Llm,
                layer: "station",
                assignments: Vec::new(),
                ops,
                decision_id,
                note: outcome.note.clone(),
            }]);
        }
    }

    /// 烽火台一轮回来了：校验通过的世界操作攒到下一 tick 落地。
    fn on_world_plan(
        &mut self,
        decision_id: u64,
        prepared: PreparedWorldPlan,
        fetched: WorldFetch,
    ) {
        self.inflight_since = None;
        self.inflight_layer = None;
        self.orchestrator.finish(AiLayer::Director);

        let outcome = self.director.finish(prepared, fetched, &self.world);
        self.last_brain_note = format!("[{}] {}", outcome.source.as_str(), outcome.note);

        if outcome.source == crate::llm::BrainSource::Fallback {
            self.orchestrator.record_fallback(AiLayer::Director);
        }
        let changes = outcome
            .ops
            .iter()
            .filter(|o| !matches!(o, ValidatedWorldOp::Narrate(_)))
            .count() as u64;
        self.orchestrator
            .record_verdict(AiLayer::Director, changes, outcome.dropped as u64);

        if outcome.source != crate::llm::BrainSource::Disabled {
            let ops: Vec<_> = outcome
                .ops
                .iter()
                .map(|o| match o {
                    ValidatedWorldOp::SetWeather { weather, why } => json!({
                        "op": "set_weather",
                        "v": weather.code(),
                        "label": weather.label(),
                        "why": why,
                    }),
                    ValidatedWorldOp::SetCommission { rate, why } => json!({
                        "op": "set_commission",
                        "v": rate,
                        "why": why,
                    }),
                    ValidatedWorldOp::Narrate(text) => json!({
                        "op": "narrate",
                        "text": text,
                    }),
                })
                .collect();
            self.hub.send_to_control(&Envelope::new(
                "llm_world",
                json!({
                    "decision_id": decision_id,
                    "layer": "director",
                    "source": outcome.source.as_str(),
                    "note": outcome.note,
                    "model": outcome.model,
                    "latency_ms": outcome.latency_ms,
                    "ops": ops,
                    "narrate": outcome.narrate,
                    "dropped": outcome.dropped,
                }),
            ));
        }

        if !outcome.ops.is_empty() {
            self.pending_world_ops = outcome.ops;
            // 世界操作下一 tick 才落地 —— 之前都还能撤。
            self.pending_decision = Some((decision_id, "director"));
        }
    }

    /// 把上一轮烽火台拿到的世界操作落地。
    ///
    /// **这是烽火台唯一能改到世界的地方**——它只动全局参数（天气 / 佣金）与旁白，
    /// 永远碰不到具体的人和单。
    fn apply_pending_world_ops(&mut self) {
        if self.pending_world_ops.is_empty() {
            return;
        }
        let ops = std::mem::take(&mut self.pending_world_ops);
        let mut events: Vec<SimEvent> = Vec::new();

        for op in ops {
            match op {
                ValidatedWorldOp::SetWeather { weather, why } => {
                    if self.world.set_weather(weather) {
                        events.push(SimEvent::WeatherChanged {
                            weather,
                            label: weather.label(),
                            speed_factor: weather.speed_factor(),
                            rain_bonus_cents: weather.rain_bonus_cents(),
                        });
                        self.world_history
                            .push(format!("set_weather={} ({why})", weather.code()));
                    }
                }
                ValidatedWorldOp::SetCommission { rate, why } => {
                    self.world.cfg.commission_rate = rate;
                    events.push(SimEvent::ConfigChanged {
                        speed_multiplier: self.world.cfg.speed_multiplier,
                        commission_rate: rate,
                        weather: self.world.weather,
                    });
                    self.world_history
                        .push(format!("set_commission={rate:.2} ({why})"));
                }
                ValidatedWorldOp::Narrate(text) => {
                    events.push(SimEvent::Narration {
                        layer: "director",
                        text,
                    });
                }
            }
        }

        // 历史只留最近 12 条，够下一轮 prompt 参考即可。
        if self.world_history.len() > 12 {
            let drop = self.world_history.len() - 12;
            self.world_history.drain(0..drop);
        }

        self.publish(&events);
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
            RpcRequest::AiState => Ok(self.ai_state()),
            RpcRequest::SetAi(patch) => self.rpc_set_ai(&patch),
            RpcRequest::RejectAi { decision_id } => self.rpc_reject_ai(decision_id),
            RpcRequest::Accounts => Ok(self.rpc_accounts()),
            RpcRequest::CreateAccount { name, password } => {
                self.rpc_create_account(&name, password)
            }
            RpcRequest::SetAccountPassword { name, password } => {
                self.rpc_set_account_password(&name, password)
            }
            RpcRequest::DeleteAccount { name } => self.rpc_delete_account(&name),
        }
    }

    /// 烽火台「骑手接入」面板要的全部东西：名单 + 手机可达地址。
    ///
    /// 一次给全，因为面板打开就要用——分两个接口只会多一次往返，还会出现
    /// 「名单新、地址旧」的中间态。
    fn rpc_accounts(&self) -> Value {
        let accounts: Vec<Value> = self
            .store
            .accounts()
            .unwrap_or_default()
            .into_iter()
            .map(|a| {
                let body = self.world.human_rider_named(&a.name);
                json!({
                    "name": a.name,
                    "created_at": a.created_at,
                    "updated_at": a.updated_at,
                    // 有身体 = 这一局里进过场；在线 = 身体在场上跑
                    "rider_id": body,
                    "rider_key": body.map(rider_key),
                    "online": body.and_then(|id| self.world.rider(id)).map(|r| r.online),
                })
            })
            .collect();
        self.with_join_hosts(json!({ "accounts": accounts }))
    }

    /// 建号。`password` 缺省就生成一个——管理员不该被迫想密码。
    ///
    /// 返回里带**明文密码和二维码**：库里只有哈希，所以这是唯一一次机会。
    /// 骑手改完密码后旧二维码就失效了（密码变了），不用额外的撤销机制。
    fn rpc_create_account(&mut self, raw_name: &str, password: Option<String>) -> RpcReply {
        let Some(name) = crate::auth::normalize_name(raw_name) else {
            return Err(ApiError::bad_request("骑手名字不能是空的"));
        };
        let password = match password.filter(|p| !p.is_empty()) {
            Some(p) => {
                crate::auth::check_password(&p).map_err(ApiError::bad_request)?;
                p
            }
            None => crate::auth::random_password(),
        };
        let hash = crate::auth::hash_password(&password)
            .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, code::INTERNAL, e))?;

        let created = self
            .store
            .create_account(&name, &hash)
            .map_err(|e| Self::db_error("建号", e))?;
        if !created {
            return Err(ApiError::conflict(
                code::BAD_CREDENTIALS,
                format!("「{name}」已经有号了；要改密码就点重置"),
            ));
        }
        tracing::info!(%name, "烽火台建了号");
        Ok(self.account_join_payload(&name, &password))
    }

    /// 改 / 重置密码。账号必须已经存在——建号请走 [`App::rpc_create_account`]。
    fn rpc_set_account_password(&mut self, raw_name: &str, password: Option<String>) -> RpcReply {
        let Some(name) = crate::auth::normalize_name(raw_name) else {
            return Err(ApiError::bad_request("骑手名字不能是空的"));
        };
        if self.store.account(&name).unwrap_or(None).is_none() {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                code::NO_ACCOUNT,
                format!("「{name}」还没有号"),
            ));
        }
        let password = match password.filter(|p| !p.is_empty()) {
            Some(p) => {
                crate::auth::check_password(&p).map_err(ApiError::bad_request)?;
                p
            }
            None => crate::auth::random_password(),
        };
        let hash = crate::auth::hash_password(&password)
            .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, code::INTERNAL, e))?;
        self.store
            .set_account_password(&name, &hash)
            .map_err(|e| Self::db_error("改密码", e))?;
        tracing::info!(%name, "烽火台重置了密码");
        Ok(self.account_join_payload(&name, &password))
    }

    fn rpc_delete_account(&mut self, raw_name: &str) -> RpcReply {
        // 账号名按规范化后的形式存。建号 / 改密都规范化了，删号不规范化就会
        // 出现「建了删不掉」——名字里多个空格就删不着。
        let name = crate::auth::normalize_name(raw_name).unwrap_or_else(|| raw_name.to_string());
        let deleted = self
            .store
            .delete_account(&name)
            .map_err(|e| Self::db_error("删号", e))?;
        if deleted {
            tracing::info!(%name, "烽火台删了号");
        }
        Ok(json!({ "name": name, "deleted": deleted }))
    }

    /// 一条「扫码即登录」的全部材料：地址 + 连接串 + 二维码。
    fn account_join_payload(&self, name: &str, password: &str) -> Value {
        let mut payload = json!({ "name": name, "password": password });
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("join".to_string(), self.join_info(name, password));
        }
        self.with_join_hosts(payload)
    }

    fn join_info(&self, name: &str, password: &str) -> Value {
        let Some((host, port)) = self.primary_join_addr() else {
            return Value::Null;
        };
        let info = crate::net::join::JoinInfo::new(host, port, name, password);
        match info.qr_svg() {
            Ok(svg) => json!({
                "ws_url": info.ws_url(),
                "uri": info.login_uri(),
                "qr_svg": svg,
            }),
            Err(e) => {
                tracing::warn!(%name, "二维码没画出来：{e}");
                json!({ "ws_url": info.ws_url(), "uri": info.login_uri(), "qr_svg": Value::Null })
            }
        }
    }

    /// 手机可达地址列表，主推的排第一。
    ///
    /// 还没 `serve()` 起来时（单测里的 App）没有监听地址，按 `127.0.0.1:8080` 算。
    fn join_hosts(&self) -> Vec<String> {
        let bound = self
            .bound
            .unwrap_or_else(|| "127.0.0.1:8080".parse().expect("字面量地址"));
        crate::net::join::reachable_hosts(bound, self.opts.public_addr.as_deref())
    }

    fn primary_join_addr(&self) -> Option<(String, u16)> {
        let host = self.join_hosts().into_iter().next()?;
        let (h, p) = host.rsplit_once(':')?;
        Some((h.to_string(), p.parse().ok()?))
    }

    fn with_join_hosts(&self, mut payload: Value) -> Value {
        let hosts = self.join_hosts();
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("hosts".to_string(), json!(hosts));
            obj.insert("primary_host".to_string(), json!(hosts.first()));
        }
        payload
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
                layer: "manual",
                assignments: decisions,
                ops: Vec::new(),
                decision_id: 0,
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

    /// 客户端消息总入口。**只做分流，不干活。**
    ///
    /// 三个分支按「谁能发」划分，不按消息类型划分——因为权限边界才是这里
    /// 最容易出错的地方：漏掉一次 `role.rider_id()` 或 `self.control()`
    /// 就是越权（观战端能抢单、控制端能改配置）。分流写在一处、
    /// 每处内部再 match，新增消息只要放进正确的那个方法就自带正确权限。
    fn on_client(&mut self, conn_id: ConnId, msg: ClientMessage) {
        use ClientMessage as M;

        // ---- 握手闸门：第一条必须是 hello ----
        if !self.hub.is_ready(conn_id) {
            return match msg {
                M::Hello {
                    token,
                    rider_name,
                    password,
                    protocol_version,
                } => self.do_hello(conn_id, token, rider_name, password, protocol_version),
                _ => self
                    .hub
                    .send_error(conn_id, code::HELLO_REQUIRED, "第一条消息必须是 hello"),
            };
        }

        let Some(role) = self.hub.role_of(conn_id) else {
            return;
        };

        // 分流只做「消息族」判断，具体的字段解构留给各方法内部。
        // **不用 or-pattern列举具体变体**：这些变体的字段各不相同
        // （order_id / online / mode / patch），Rust 要求 or-pattern 各分支
        // 绑定一致，硬凑可读性反而更差。
        match msg {
            // ---- 骑手动作：全部要 `role.rider_id()``----
            M::Claim { .. }
            | M::Reject { .. }
            | M::AcceptAssign { .. }
            | M::Urge { .. }
            | M::ArriveShop { .. }
            | M::ArriveCustomer { .. }
            | M::Deliver { .. }
            | M::SetOnline { .. }
            | M::SetSpeedMode { .. }
            | M::TaskMessage { .. }
            | M::ChatSay { .. }
            | M::ChatPreset { .. }
            | M::ChangePassword { .. } => self.rider_action(conn_id, role, msg),

            // ---- 其余（控制端 + 杂项）----
            _ => self.control_or_misc(conn_id, role, msg),
        }
    }

    /// 骑手动作。**每一条都必须先过 `role.rider_id()`**。
    ///
    /// 单独成方法是为了让「这条消息要骑手身份」这件事在函数入口就成立——
    /// 审阅时只需确认这个方法里每个分支都拿得到 `rid`。
    fn rider_action(&mut self, conn_id: ConnId, role: Role, msg: ClientMessage) {
        use ClientMessage as M;

        // 一次性取骑手 id：拿不到就整组拒绝。这比每个分支各写一遍
        // `match role.rider_id() { Some(..) => .., None => not_a_rider }` 更短，
        // 也更难漏——新加分支时不会忘了写兜底。
        let Some(rid) = role.rider_id() else {
            let action = client_action_name(&msg);
            return self.not_a_rider(conn_id, action);
        };

        match msg {
            M::Claim {
                order_id,
                back_order,
            } => self.do_claim(conn_id, rid, order_id, back_order),
            M::Reject { order_id } => {
                let r = scheduler::reject_order(&mut self.world, rid, order_id);
                self.finish_simple(conn_id, "reject", r)
            }
            // 接受指派单只是把「待确认」标记摘掉，世界状态早在指派那一刻就定死了，
            // 所以这里不产生任何要广播的事件。
            M::AcceptAssign { order_id } => {
                let r = scheduler::accept_assign(&mut self.world, rid, order_id).map(|()| vec![]);
                self.finish_simple(conn_id, "accept_assign", r)
            }
            M::Urge { order_id } => {
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
            M::ArriveShop { order_id } => {
                let r = scheduler::arrive_shop(&mut self.world, rid, order_id);
                self.finish_simple(conn_id, "arrive_shop", r)
            }
            M::ArriveCustomer { order_id } => {
                let r = scheduler::arrive_customer(&mut self.world, rid, order_id);
                self.finish_simple(conn_id, "arrive_customer", r)
            }
            M::Deliver { order_id } => {
                let r = scheduler::deliver(&mut self.world, rid, order_id);
                self.finish_simple(conn_id, "deliver", r)
            }
            M::SetOnline { online } => {
                let r = scheduler::set_online(&mut self.world, rid, online);
                self.finish_simple(conn_id, "set_online", r)
            }
            M::SetSpeedMode { mode } => {
                let r = scheduler::set_speed_mode(&mut self.world, rid, mode);
                self.finish_simple(conn_id, "set_speed_mode", r)
            }
            M::ChangePassword {
                old_password,
                new_password,
            } => self.do_change_password(conn_id, rid, old_password, new_password),
            M::TaskMessage { order_id, text } => self.do_task_message(conn_id, rid, order_id, text),
            // 订单会话（末端交付沟通）：**不要求挂任务**，任何到手单都能说。
            M::ChatSay { order_id, text } => {
                self.do_chat_say(conn_id, rid, order_id, text, "chat_say")
            }
            M::ChatPreset { order_id, preset } => self.do_chat_say(
                conn_id,
                rid,
                order_id,
                preset.text().to_string(),
                "chat_preset",
            ),

            // 分流保证只有骑手动作会进到这里；其余消息不该出现。
            _ => self
                .hub
                .send_error(conn_id, code::BAD_REQUEST, "该消息不属于骑手动作"),
        }
    }

    /// 控制端指令 + 杂项（非骑手动作的消息）。
    ///
    /// 控制端分支几乎都用 [`Self::control`] 包一层——它统一检查
    /// `Role::Control`、统一发 ack、统一把闭包产生的事件塞进 `outbox`。
    /// 新增控制端指令请沿用这个包装，别手写权限判断。
    fn control_or_misc(&mut self, conn_id: ConnId, role: Role, msg: ClientMessage) {
        use ClientMessage as M;

        match msg {
            // ---------------- 杂项：任何角色都能发 ----------------
            M::Hello { .. } => self.hub.send_error(conn_id, code::BAD_REQUEST, "重复握手"),
            M::Resync => self.send_snapshot(conn_id, role),
            M::Ping => {
                let t = self.world.elapsed_min;
                self.hub
                    .send_to(conn_id, &Envelope::new("pong", json!({ "elapsed_min": t })));
            }

            // ---------------- 控制端 ----------------
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
            M::SetCommissionRate { rate } => {
                self.control(conn_id, role, "set_commission_rate", |app| {
                    let r = rate.clamp(0.0, 0.30);
                    app.world.cfg.commission_rate = r;
                    Some(json!({ "commission_rate": r }))
                })
            }
            M::ForceAssign { order_id, rider_id } => self.control(
                conn_id,
                role,
                "force_assign",
                |app| match scheduler::assign_forced(&mut app.world, order_id, rider_id) {
                    Ok(assignment) => {
                        let events = app.claim_events(&assignment);
                        app.outbox.extend(events);
                        Some(json!({ "order_id": order_id, "rider_id": rider_id }))
                    }
                    Err(e) => {
                        app.outbox_err = Some(e);
                        None
                    }
                },
            ),
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
            M::SetRiderOnline { rider_id, online } => self.control(
                conn_id,
                role,
                "set_rider_online",
                |app| match scheduler::set_online(&mut app.world, rider_id, online) {
                    Ok(events) => {
                        app.outbox.extend(events);
                        Some(json!({ "rider_id": rider_id, "online": online }))
                    }
                    Err(e) => {
                        app.outbox_err = Some(e);
                        None
                    }
                },
            ),
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
            M::RenameRider { rider_id, name } => match self.account_rename_plan(rider_id, &name) {
                Err(reason) => {
                    self.ack_fail(conn_id, "rename_rider", code::BAD_CREDENTIALS, &reason)
                }
                Ok(account_move) => {
                    self.control(conn_id, role, "rename_rider", move |app| {
                        let changed = app.world.rename_rider(rider_id, &name);
                        if changed {
                            // 账号名是主键，改了名就得跟着搬，否则旧名字还能登进一具新身体
                            if let Some((from, to)) = &account_move
                                && let Err(e) = app.store.rename_account(from, to)
                            {
                                // 重名在前面已经拦过，走到这儿只可能是库出了问题
                                tracing::warn!(%from, %to, "账号改名没落库：{e}");
                            }
                            if let Some(ev) = scheduler::state_event(&app.world, rider_id) {
                                app.outbox.push(ev);
                            }
                            app.persist_rider_names();
                        }
                        Some(json!({ "changed": changed, "rider_id": rider_id }))
                    })
                }
            },
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
                layer_models,
            } => self.control(conn_id, role, "set_llm_config", |app| {
                app.apply_llm_config(api_key, base_url, model, layer_models);
                // 配完立刻把新的「配没配 / 什么模型」推给控制端，表单才能回显。
                app.push_server_info();
                Some(json!({
                    "llm_configured": app.llm_config.has_key(),
                    "base_url": app.llm_config.base_url,
                    "model": app.llm_config.model,
                    "models": app.layer_models.to_json(),
                }))
            }),
            M::ListModels => self.control(conn_id, role, "list_models", |app| {
                // 真去问上游，但走后台任务：网络慢不能把世界卡住。
                app.spawn_list_models(conn_id);
                Some(json!({ "pending": true }))
            }),
            M::AiQuery => self.control(conn_id, role, "ai_query", |app| {
                let state = app.ai_state();
                app.push_ai_state();
                Some(state)
            }),
            M::SetAi(patch) => self.control(conn_id, role, "set_ai", |app| {
                let out = match app.rpc_set_ai(&patch) {
                    Ok(v) => v,
                    Err(e) => return Some(json!({ "ok": false, "message": e.message })),
                };
                app.push_ai_state();
                Some(out)
            }),
            M::RejectAi { decision_id } => self.control(conn_id, role, "reject_ai", |app| {
                let out = app
                    .rpc_reject_ai(decision_id)
                    .unwrap_or_else(|e| json!({ "ok": false, "message": e.message }));
                // 撤完立刻让控制端看到「还能撤什么」变了。
                app.push_ai_state();
                Some(out)
            }),
            // 临时插单：控制台地图上点两个点就能造一张单。
            M::CreateOrder {
                merchant_name,
                merchant_lat,
                merchant_lng,
                customer_name,
                customer_address,
                customer_lat,
                customer_lng,
                fee_cents,
            } => {
                if !valid_latlng(merchant_lat, merchant_lng)
                    || !valid_latlng(customer_lat, customer_lng)
                {
                    self.ack_fail(
                        conn_id,
                        "create_order",
                        code::BAD_REQUEST,
                        "两端坐标都得是有效经纬度",
                    );
                } else {
                    self.control(conn_id, role, "create_order", |app| {
                        // 控制台给的是**当前下发坐标系**下的坐标，世界内部一律 GCJ-02。
                        // 换算只在这一处入口发生，不让它渗进业务层。
                        let cs = app.world.cfg.coord_system;
                        let m = from_coord_system(LatLng::new(merchant_lat, merchant_lng), cs);
                        let c = from_coord_system(LatLng::new(customer_lat, customer_lng), cs);
                        let id = app.world.create_custom_order(CustomOrderSpec {
                            merchant_name,
                            merchant_lat: m.lat,
                            merchant_lng: m.lng,
                            customer_name,
                            customer_address,
                            customer_lat: c.lat,
                            customer_lng: c.lng,
                            fee_cents,
                        });
                        // 不等下一拍 tick：控制台点完「创建」就该在大厅看见它。
                        if let Some(order) = app.world.order(id) {
                            let window_ms = app.world.cfg.claim_window_secs as u64 * 1000;
                            app.outbox.push(SimEvent::OrderBroadcast {
                                order: Box::new(order.clone()),
                                window_ms,
                                expires_in_ms: window_ms,
                            });
                        }
                        Some(json!({ "order_id": id }))
                    })
                }
            }
            // 清空钱包：纯测试辅助，**不给任何业务校验**——二次确认在控制台那侧。
            M::ResetRiderWallet { rider_id } => {
                self.control(conn_id, role, "reset_rider_wallet", |app| {
                    let Some(rider) = app.world.rider_mut(rider_id) else {
                        app.outbox_err = Some(ClaimError::RiderNotFound);
                        return None;
                    };
                    let cleared = economy::reset_wallet(rider);
                    if let Some(ev) = scheduler::state_event(&app.world, rider_id) {
                        app.outbox.push(ev);
                    }
                    Some(json!({ "rider_id": rider_id, "cleared_cents": cleared }))
                })
            }

            // 骑手动作理论上已被 `on_client` 的分流拦走；走到这里说明
            // `on_client` 的分流表和这个 match 不同步。报明确错误，
            // 好过静默吞掉——同步漏了要立刻发现，而不是等到功能失灵。
            _ => self
                .hub
                .send_error(conn_id, code::BAD_REQUEST, "该消息不属于控制端指令"),
        }
    }

    /// 把三层 AI 状态推给控制端（`ai_state` 是一条**控制端专用**事件）。
    fn push_ai_state(&mut self) {
        let state = self.ai_state();
        self.hub.send_to_control(&Envelope::new("ai_state", state));
    }

    /// 应用控制台提交的 LLM 配置：改配置 → 重建 brain/judge → 落库。
    ///
    /// `None` = 这一项不动；`api_key` 传空串 = 清除 Key（连带把大脑开关落下）。
    /// Key 只进数据库，不下发、不写日志。
    ///
    /// `layer_models` 只覆盖传进来的那几层，值传空串 = 该层回退到全局模型。
    fn apply_llm_config(
        &mut self,
        api_key: Option<String>,
        base_url: Option<String>,
        model: Option<String>,
        layer_models: Option<std::collections::BTreeMap<String, String>>,
    ) {
        if let Some(k) = api_key {
            self.llm_config.api_key = k.trim().to_string();
        }
        if let Some(u) = base_url {
            let u = u.trim();
            self.llm_config.base_url = if u.is_empty() {
                LlmConfig::default().base_url
            } else {
                // 控制台里最容易把完整端点（…/v1/chat）当根路径填进来，
                // 归一化一次，省得下游拼出 `/v1/chat/models` 这种 404。
                crate::llm::normalize_base_url(u)
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
        if let Some(map) = layer_models {
            for (key, value) in map {
                // `LayerModels::set` 会把空串当 None —— 正好是「这层回退全局」。
                match LayerKey::from_key(key.trim()) {
                    Some(layer) => self.layer_models.set(layer, Some(value)),
                    // 认不出来的层名直接忽略：控制台名字改了不至于把整份配置打回去。
                    None => tracing::warn!(key = %key, "分层模型：认不出的层名，已忽略"),
                }
            }
        }

        // 重建客户端：三层大脑与任务判定共用同一份配置，按层挑模型。
        self.brain = TacticalBrain::from_config(
            &self
                .llm_config
                .for_layer(&self.layer_models, LayerKey::Rider),
        );
        self.judge = TaskJudge::from_config(&self.llm_config);
        self.director = DirectorBrain::from_config(
            &self
                .llm_config
                .for_layer(&self.layer_models, LayerKey::Director),
        );
        self.orchestrator = AiOrchestrator::from_config(&self.llm_config);
        // 站长的指令搭骑手层那一轮下发，不单独占坑。
        self.orchestrator.set_layer_enabled(AiLayer::Station, false);
        // 换了 Key / 模型，旧的间隔记录与操作史不再适用。
        self.world_history.clear();

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
        let _ = self
            .store
            .set_meta("llm_base_url", &self.llm_config.base_url);
        let _ = self.store.set_meta("llm_model", &self.llm_config.model);
        // 分层模型也要落库，否则控制台选好的模型一重启就回退。
        if let Ok(json) = serde_json::to_string(&self.layer_models.to_json()) {
            let _ = self.store.set_meta("llm_layer_models", &json);
        }
    }

    /// 后台去问上游有哪些模型，结果从 `models_tx` 回到主循环。
    ///
    /// **不在主循环里 await**：上游慢（或不通）时世界照跑。
    fn spawn_list_models(&mut self, conn_id: ConnId) {
        let cfg = self.llm_config.clone();
        let tx = self.models_tx.clone();
        tokio::spawn(async move {
            let outcome = match LlmClient::metadata_only(cfg) {
                Ok(client) => client.list_models().await.map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            };
            let _ = tx.send(ModelListing { conn_id, outcome });
        });
    }

    /// 把模型清单（或失败原因）回给点名要它的那条连接。
    fn on_models(&mut self, listing: ModelListing) {
        let payload = match listing.outcome {
            Ok(models) => json!({
                "ok": true,
                "count": models.len(),
                "models": models,
                // 顺带把「现在三层各用什么」带回去，下拉框可以直接回显。
                "current": {
                    "global": self.llm_config.model,
                    "layers": self.layer_models.to_json(),
                },
            }),
            Err(message) => json!({
                "ok": false,
                "count": 0,
                "models": [],
                "message": message,
            }),
        };
        self.hub
            .send_to(listing.conn_id, &Envelope::new("ai_models", payload));
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
        // 改上限**不回滚**已用掉的额度：拒单次数是当天既成事实，
        // 调低上限只会让「额度已满」更早出现，不会把已拒的还回去。
        if let Some(v) = p.max_rejects_per_day {
            cfg.max_rejects_per_day = v.min(999);
        }
        // 只影响下发时的坐标转换，世界状态不动。
        if let Some(v) = p.coord_system {
            cfg.coord_system = v;
        }
        // 合并单 / 凑单：只影响之后的派单与出单，已经在跑的单不动。
        if let Some(v) = p.merge_max_per_rider {
            cfg.merge.algorithm_max_per_rider = v.clamp(1, 3);
        }
        if let Some(v) = p.merge_merchant_cap {
            cfg.merge.max_active_orders_per_merchant = v.min(6);
        }
        if let Some(v) = p.merge_wait_min {
            cfg.merge.wait_for_merge_min = v.min(10);
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
    fn do_hello(
        &mut self,
        conn_id: ConnId,
        token: Option<String>,
        rider_name: Option<String>,
        password: Option<String>,
        proto: Option<String>,
    ) {
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

        // 正门：骑手名 + 密码。给了名字就**不再看令牌**——两者同时生效会出现
        // 「密码是对的，但令牌把人绑到了另一个骑手」这种说不清、也查不出的状态。
        let (role, name) = if let Some(raw) = rider_name.as_deref() {
            match self.login_with_password(raw, password.as_deref().unwrap_or_default()) {
                Ok(v) => v,
                Err((c, msg)) => {
                    self.hub.send_error(conn_id, c, &msg);
                    self.hub.unregister(conn_id);
                    return;
                }
            }
        } else {
            match token.as_deref() {
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
            }
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

    /// 名字 + 密码登录（烽火台建的号）。
    ///
    /// 失败给的是**协议错误码 + 一句给骑手看的话**：「密码不对」和「还没建号」
    /// 必须分得开，否则骑手会一直重试密码，而其实该去找管理端要二维码。
    fn login_with_password(
        &mut self,
        raw_name: &str,
        password: &str,
    ) -> Result<(Role, String), (&'static str, String)> {
        let Some(name) = crate::auth::normalize_name(raw_name) else {
            return Err((code::BAD_CREDENTIALS, "骑手名字不能是空的".into()));
        };

        // 查库失败按「没有这个号」处理——登录这条路宁可拒绝也不放行。
        // 库真挂了会在启动日志里有 warn，不会没人知道。
        let Some(stored) = self.store.account_hash(&name).unwrap_or(None) else {
            return Err((
                code::NO_ACCOUNT,
                format!("「{name}」还没建号，去烽火台要一张二维码"),
            ));
        };
        if !crate::auth::verify_password(&stored, password) {
            return Err((code::UNAUTHORIZED, "密码不对".into()));
        }

        // 复用世界里的同名身体：账号是长期身份，骑手只是这一局里的一具身体。
        // 服务端一重启身体就没了、账号还在，所以找不到就现建一具——这也是
        // 「重启后老账号还能登」的全部实现。
        let id = match self.world.human_rider_named(&name) {
            Some(id) => id,
            None => {
                let id = self.world.spawn_human_rider(&name);
                tracing::info!(rider_id = id, %name, "账号登录：这个号在本局还没有身体，建一具");
                id
            }
        };
        Ok((Role::Rider(id), name))
    }

    /// 骑手改自己的密码。**旧密码必须对**——连上了不等于能改这个号，
    /// 否则捡到一台没锁屏的手机就能把号改走。
    fn do_change_password(
        &mut self,
        conn_id: ConnId,
        rid: RiderId,
        old_password: String,
        new_password: String,
    ) {
        let Some(name) = self.world.rider(rid).map(|r| r.name.clone()) else {
            return self.ack_fail(conn_id, "change_password", code::UNAUTHORIZED, "骑手不存在");
        };
        if let Err(why) = crate::auth::check_password(&new_password) {
            return self.ack_fail(conn_id, "change_password", code::BAD_CREDENTIALS, &why);
        }
        let Some(stored) = self.store.account_hash(&name).unwrap_or(None) else {
            return self.ack_fail(
                conn_id,
                "change_password",
                code::NO_ACCOUNT,
                "这个骑手没有账号，改密码得先让烽火台建号",
            );
        };
        if !crate::auth::verify_password(&stored, &old_password) {
            return self.ack_fail(conn_id, "change_password", code::UNAUTHORIZED, "旧密码不对");
        }

        let result = crate::auth::hash_password(&new_password).and_then(|h| {
            self.store
                .set_account_password(&name, &h)
                .map_err(|e| e.to_string())
        });
        match result {
            Ok(()) => {
                tracing::info!(%name, "骑手改了自己的密码");
                self.hub.send_ack(conn_id, &Ack::ok("change_password"));
            }
            Err(e) => self.ack_fail(conn_id, "change_password", code::INTERNAL, &e),
        }
    }

    /// 真人骑手改名时账号要不要跟着搬。
    ///
    /// - `Ok(None)` → 不用搬：模拟骑手，或者这位真人骑手还没有账号。
    /// - `Ok(Some((from, to)))` → 得把账号从 `from` 搬到 `to`。
    /// - `Err` → **整个改名都不要做**。目标是别人已有的账号名时，世界改了名字、
    ///   账号却没搬，等于把那个账号的登录指向了一具陌生的身体。
    fn account_rename_plan(
        &self,
        rider_id: RiderId,
        new_name: &str,
    ) -> Result<Option<(String, String)>, String> {
        let Some(rider) = self.world.rider(rider_id) else {
            return Ok(None);
        };
        if rider.kind != RiderKind::Human {
            return Ok(None);
        }
        let old = rider.name.clone();
        // 名字不合法世界那边自己会拒，这里不用重复报错
        let Some(new) = World::sanitize_rider_name(new_name) else {
            return Ok(None);
        };
        if old == new {
            return Ok(None);
        }
        if self.store.account(&new).unwrap_or(None).is_some() {
            return Err(format!("「{new}」已经是某个骑手的账号名了，换一个"));
        }
        Ok(if self.store.account(&old).unwrap_or(None).is_some() {
            Some((old, new))
        } else {
            None
        })
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

/// 经纬度是否落在合法范围。控制台地图选点偶尔会漏传字段（默认 0.0），
/// 而 `(0, 0)` 是几内亚湾——真让它进世界会算出一张飞越大洋的单。
fn valid_latlng(lat: f64, lng: f64) -> bool {
    lat.is_finite()
        && lng.is_finite()
        && (-90.0..=90.0).contains(&lat)
        && (-180.0..=180.0).contains(&lng)
        // (0, 0) 本身就是合法坐标，但它同时是「字段漏传」的缺省值，
        // 在这个界面里只可能是误操作，直接当非法处理。
        && !(lat == 0.0 && lng == 0.0)
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

    /// 给一条「协议错误码 + 中文原因」的 ack。给不来自 [`ClaimError`] 的失败用
    /// （账号、密码这类）。
    fn ack_fail(&mut self, conn_id: ConnId, action: &str, code: &'static str, msg: &str) {
        self.hub.send_ack(conn_id, &Ack::err(action, code, msg));
    }

    /// 账号表写库失败。**只有这里会 500**——账号是管理动作，失败必须让烽火台
    /// 看见，不能像模拟数据那样悄悄咽掉。
    fn db_error(action: &str, e: impl std::fmt::Display) -> ApiError {
        tracing::warn!(action, "账号表写库失败：{e}");
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            code::INTERNAL,
            format!("{action}失败：{e}"),
        )
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
    // 订单会话（末端交付沟通）
    // -----------------------------------------------------------------

    /// 骑手在订单会话里说一句话。
    ///
    /// 与 [`App::do_task_message`] 的关键区别：**不要求订单挂任务**，也不推进
    /// 任何进度——它纯粹是末端沟通。骑手这句立刻广播；顾客是否回应由末端窗口
    /// 与订单画像决定（见 [`crate::sim::chat`]），**说了话不等于一定有人回**。
    ///
    /// `action` 是回执里的动作名（`chat_say` / `chat_preset`），只影响客户端
    /// 怎么匹配回执，不影响行为。
    fn do_chat_say(
        &mut self,
        conn_id: ConnId,
        rid: RiderId,
        order_id: OrderId,
        text: String,
        action: &'static str,
    ) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return self.hub.send_ack(
                conn_id,
                &Ack::err(action, code::BAD_REQUEST, "不能发空消息"),
            );
        }
        if text.chars().count() > crate::llm::customer::MAX_REPLY_CHARS {
            return self.hub.send_ack(
                conn_id,
                &Ack::err(action, code::BAD_REQUEST, "消息太长了，说重点"),
            );
        }
        if let Err(e) = self.check_chat_target(rid, order_id) {
            return self.ack_err(conn_id, action, e);
        }

        self.publish(&[SimEvent::ChatMessage {
            order_id,
            from: "rider",
            text,
            kind: "free",
            day: self.world.day,
            sim_minute: self.world.sim_minute,
            compensation_cents: 0,
            fault: "none",
        }]);
        self.hub
            .send_ack(conn_id, &Ack::ok_with(action, json!({ "status": "said" })));
    }

    /// 会话准入：单子存在，且是你的。
    ///
    /// **不检查末端窗口**——骑手想在窗口外补一句「餐放门口了」是合理的，
    /// 硬卡窗口只会让人困惑。
    fn check_chat_target(&self, rid: RiderId, order_id: OrderId) -> Result<(), ClaimError> {
        let Some(order) = self.world.order(order_id) else {
            return Err(ClaimError::OrderNotFound);
        };
        if order.assigned_to != Some(rid) {
            return Err(ClaimError::NotAssignedToYou);
        }
        Ok(())
    }

    /// 顾客开口（**兜底路径**）：立刻广播一句预设话术，不花额度、不等人。
    fn open_chat(&mut self, order_id: OrderId, fallback: String) {
        self.publish(&[SimEvent::ChatMessage {
            order_id,
            from: "customer",
            text: fallback,
            kind: "reply",
            day: self.world.day,
            sim_minute: self.world.sim_minute,
            compensation_cents: 0,
            fault: "none",
        }]);
    }

    // -----------------------------------------------------------------
    // 顾客对话（末端交付沟通）
    // -----------------------------------------------------------------

    /// 有顾客等着开口、且坑位空着，就发起一次对话调用。
    ///
    /// **与派单决策共用同一个坑位和同一条 96 次预算**：对话真的会挤掉派单的
    /// 额度，这是当初明确接受的取舍。挤不下时**不做替换**——顾客退回一句兜底
    /// 话术，骑手照样能正常跑单。
    fn maybe_start_chat(&mut self) {
        if self.pending_chats.is_empty() {
            return;
        }
        // 没配 Key / 大脑没开：不占额度，直接用兜底话术落地。
        if !self.customer.is_enabled() || !self.llm_enabled() {
            self.flush_chat_fallbacks();
            return;
        }
        if self.inflight_since.is_some() {
            return; // 有别的在飞，下一 tick 再来
        }
        if !self.orchestrator.begin(AiLayer::Customer, &self.world) {
            // 额度用完：兜底落地，别让顾客永远哑着。
            self.flush_chat_fallbacks();
            return;
        }
        let Some(pending) = self.pending_chats.pop_front() else {
            // 理论上进不来（上面刚判过非空），但坑已经占了，得还回去。
            self.orchestrator.finish(AiLayer::Customer);
            return;
        };
        let tx = self.chat_tx.clone();
        let brain = self.customer.clone();
        self.inflight_since = Some(Instant::now());
        self.inflight_layer = Some(AiLayer::Customer);
        tokio::spawn(async move {
            let reply = brain.open(&pending.context, &pending.fallback).await;
            if tx
                .send(ChatInbound {
                    rider_id: pending.rider_id,
                    order_id: pending.order_id,
                    reply,
                })
                .is_err()
            {
                tracing::debug!("主循环已退出，丢弃这一次顾客回应");
            }
        });
    }

    /// 攒着的顾客开口全部用兜底话术落地（**不花额度**）。
    fn flush_chat_fallbacks(&mut self) {
        while let Some(p) = self.pending_chats.pop_front() {
            self.open_chat(p.order_id, p.fallback);
        }
    }

    /// 顾客的回应回来了。
    fn on_chat_reply(&mut self, inbound: ChatInbound) {
        self.inflight_since = None;
        self.inflight_layer = None;
        self.orchestrator.finish(AiLayer::Customer);
        if inbound.reply.source == ReplySource::Rejected {
            // 模型吐了非法 JSON，整条作废、退回预设——记一次回退。
            self.orchestrator.record_fallback(AiLayer::Customer);
        }
        self.apply_customer_reply(inbound.rider_id, inbound.order_id, inbound.reply);
    }

    /// 顾客说完一句话：**先落地经济后果，再广播**。
    ///
    /// 钱只从「骑手责任」身上出——商家出餐问题不动骑手的账，没人有错则分文不取。
    /// 这是「按责任分摊」的最终落点：LLM 只给看法，金额早在 [`ChatReply`] 里
    /// 被夹取过了，这里只是执行。
    fn apply_customer_reply(&mut self, rid: RiderId, oid: OrderId, reply: ChatReply) {
        let now_min = self.world.elapsed_min;
        let mut fined = 0i64;
        if reply.fault.hits_rider() && reply.compensation_cents > 0 {
            if let Some(r) = self.world.rider_mut(rid) {
                economy::issue_fine(
                    r,
                    reply.compensation_cents,
                    "customer_compensation",
                    format!("顾客索赔：{}", reply.reply),
                    Some(oid),
                    now_min,
                );
            }
            fined = reply.compensation_cents;
        }
        if reply.rating_delta != 0.0 {
            // 先克隆配置：`rider_mut` 要可变借用整个 world，没法再借它的 cfg。
            let cfg = self.world.cfg.clone();
            if let Some(r) = self.world.rider_mut(rid) {
                economy::adjust_rating(&cfg, r, reply.rating_delta);
            }
        }
        if fined > 0 {
            tracing::info!(rider = rid, order = oid, cents = fined, "顾客索赔已扣款");
        }

        self.publish(&[SimEvent::ChatMessage {
            order_id: oid,
            from: "customer",
            text: reply.reply,
            kind: "reply",
            day: self.world.day,
            sim_minute: self.world.sim_minute,
            compensation_cents: fined,
            fault: reply.fault.as_str(),
        }]);
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
        cfg.base_url = crate::llm::normalize_base_url(&u);
    }
    if let Some(m) = non_empty(store.meta("llm_model").ok().flatten()) {
        cfg.model = m;
    }
    Some(cfg)
}

/// 从库里读回控制台存过的分层模型。
///
/// 只要库里**存过这一项**就认（哪怕是空对象）——那代表人在控制台上
/// 明确设定过的状态，不该被环境变量覆盖回去。
fn load_layer_models(store: &Store) -> Option<LayerModels> {
    let raw = store.meta("llm_layer_models").ok().flatten()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some(LayerModels::from_json(&value))
}

/// 从库里读回骑手名单。解析失败就当没存过。
fn load_rider_names(store: &Store) -> Option<Vec<String>> {
    let raw = store.meta("rider_names").ok().flatten()?;
    serde_json::from_str::<Vec<String>>(&raw).ok()
}

/// 消息 → 协议里的动作名。给「不是骑手身份」的拒绝回执用。
///
/// 拆出 [`Self::rider_action`] 之后，权限兜底只需要知道**是哪条消息**，
/// 不再逐分支硬编码字符串。控制端的同类需求走 [`App::control`]，
/// 它自己带 action 参数，不依赖这里。
fn client_action_name(msg: &ClientMessage) -> &'static str {
    use ClientMessage as M;
    match msg {
        M::Hello { .. } => "hello",
        M::Resync => "resync",
        M::Ping => "ping",
        M::Claim { .. } => "claim",
        M::Reject { .. } => "reject",
        M::AcceptAssign { .. } => "accept_assign",
        M::Urge { .. } => "urge",
        M::ArriveShop { .. } => "arrive_shop",
        M::ArriveCustomer { .. } => "arrive_customer",
        M::Deliver { .. } => "deliver",
        M::SetOnline { .. } => "set_online",
        M::SetSpeedMode { .. } => "set_speed_mode",
        M::ChangePassword { .. } => "change_password",
        M::TaskMessage { .. } => "task_message",
        M::ChatSay { .. } => "chat_say",
        M::ChatPreset { .. } => "chat_preset",
        M::SetPaused { .. } => "set_paused",
        M::SetSpeedMultiplier { .. } => "set_speed_multiplier",
        M::SetWeather { .. } => "set_weather",
        M::SetCommissionRate { .. } => "set_commission_rate",
        M::ForceAssign { .. } => "force_assign",
        M::SetLlmEnabled { .. } => "set_llm_enabled",
        M::SetConfig { .. } => "set_config",
        M::SetRiderOnline { .. } => "set_rider_online",
        M::SetRealtime { .. } => "set_realtime",
        M::RenameRider { .. } => "rename_rider",
        M::SetRiderNames { .. } => "set_rider_names",
        M::SetLlmConfig { .. } => "set_llm_config",
        M::ListModels => "list_models",
        M::AiQuery => "ai_query",
        M::SetAi { .. } => "set_ai",
        M::RejectAi { .. } => "reject_ai",
        M::CreateOrder { .. } => "create_order",
        M::ResetRiderWallet { .. } => "reset_rider_wallet",
    }
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
mod tests;
