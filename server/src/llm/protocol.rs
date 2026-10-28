//! LLM 决策协议：请求载荷、响应解析、四级校验（《LLM协议设计》§2~§4）。
//!
//! `brain.rs` 管的是**什么时候调、调完怎么落库**；这里管的是**协议本身**：
//! 请求长什么样、响应怎么校验。分开之后，协议可以脱离 HTTP 单测——
//! 「非法输入会不会被丢」这种事不该需要真实网络才能验。
//!
//! 三条铁律（《LLM协议设计》§九）：
//! 1. 物理量（速度、步行时间、时间推进）绝不接受 LLM 干预，它只能调档位；
//! 2. 输出必须是有限集合或可钳制数值（白名单 + clamp）；
//! 3. 任何时刻都能不依赖 LLM 跑完整局——本校验层丢掉的一切算法都能接。
//!
//! 格式选择：数组 + 短键名 + 定长坐标 + 整数分。JSON 里每个字符都是 token，
//! `"phase"` 不如 `"ph"`，`{"lat":..,"lng":..}` 不如 `[lat,lng]`。

use std::cmp::Ordering;
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::sim::{
    OrderId, RiderId, ai,
    order::OrderStatus,
    rider::{PriceTier, RiderKind, SpeedMode},
    world::World,
};

/// 协议版本。不匹配则整轮放弃。
pub const PROTOCOL_VERSION: u8 = 1;
/// 请求体上限。超了就砍——**不是截断字符串，是整条丢弃**。
pub const MAX_PROMPT_BYTES: usize = 4096;
/// 塞进请求的骑手上限。
pub const MAX_RIDERS_IN_PROMPT: usize = 16;
/// 塞进请求的订单上限。
pub const MAX_ORDERS_IN_PROMPT: usize = 12;
/// 备注截断长度（字符数，不是字节数）。
pub const MAX_REMARK_CHARS: usize = 20;
/// `why` 字段截断长度。
pub const MAX_WHY_CHARS: usize = 24;

// ---------------------------------------------------------------------------
// 请求
// ---------------------------------------------------------------------------

/// 一次决策请求的完整载荷（§2.2）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BrainRequest {
    pub v: u8,
    pub sim: SimCtx,
    pub riders: Vec<RiderSnap>,
    pub orders: Vec<OrderSnap>,
}

/// 全局上下文。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SimCtx {
    pub day: u32,
    /// 当天第几分钟。
    pub min: u32,
    /// 总 tick。
    pub tick: u64,
    /// 天气短码：`c` 晴 / `o` 阴 / `f` 雾 / `r` 雨 / `hr` 暴雨 / `s` 雪 / `w` 大风。
    pub weather: String,
    /// 是否高峰 0/1。
    pub peak: u8,
    pub seed: u64,
}

/// 骑手快照（**只读输入**，LLM 不能改）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RiderSnap {
    /// `r1` 形式。
    pub id: String,
    pub pos: [f64; 2],
    /// 常驻区域中心。
    pub zone: [f64; 2],
    /// 速度档位 0/1/2。
    pub mode: u8,
    /// 疲劳 0~1。
    pub fat: f32,
    pub rate: f32,
    pub lvl: u8,
    /// 今日到手（分）。
    pub cash: i64,
    /// 在线分钟。
    pub on: u32,
    /// 当前背单**上限**（可被 LLM 调的那个值）。
    pub cap: u32,
    /// 当前时薪门槛（分/小时）。
    pub thr: i64,
    /// 熟练度 0~1。
    pub skill: f32,
    /// 实际骑行速度 km/h（只读）。
    pub ride: f32,
    /// 步行速度 m/s（只读，物理量）。
    pub walk: f32,
    pub held: Vec<HeldSnap>,
}

/// 手持单快照。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeldSnap {
    pub o: OrderId,
    /// `ts` 待取货 / `wf` 等餐 / `tc` 待送达 / `ac` 已到达。
    pub ph: String,
    /// 本段进度 0~1。
    pub prog: f32,
    /// 本段剩余分钟。
    pub left: u32,
}

/// 待抢订单快照。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderSnap {
    pub id: OrderId,
    pub from: [f64; 2],
    pub to: [f64; 2],
    /// 配送费（分，含重投补贴）。
    pub fee: i64,
    pub dist: u32,
    pub promise: u32,
    /// 承诺剩余分钟（负 = 已超时）。
    pub left: i32,
    pub prep: u32,
    pub mrate: f32,
    /// 商家步行难度 0/1/2（**物理量，只读**）。
    pub mwalk: u8,
    /// 顾客端步行难度 0/1/2（**物理量，只读**）。
    pub cwalk: u8,
    pub remark: String,
}

impl BrainRequest {
    /// 组装请求：取前 N 个空闲骑手与待抢订单。
    ///
    /// **顺序必须确定**——`idle` / `pending` 由世界按 id 排序给出，这里再截断，
    /// 保证同样的世界状态得到同样的 prompt（否则状态指纹缓存全是无效命中）。
    pub fn build(world: &World, idle: &[RiderId], pending: &[OrderId]) -> Self {
        let cfg = &world.cfg;
        let sim = SimCtx {
            day: world.day,
            min: world.sim_minute,
            tick: world.elapsed_min,
            weather: world.weather.code().to_string(),
            peak: u8::from(cfg.is_peak(world.sim_minute)),
            seed: cfg.seed,
        };

        let riders = idle
            .iter()
            .take(MAX_RIDERS_IN_PROMPT)
            .filter_map(|rid| world.rider(*rid))
            .map(|r| RiderSnap {
                id: crate::sim::rider_key(r.id),
                pos: [r.lat, r.lng],
                zone: [r.home_lat, r.home_lng],
                mode: r.effective_speed_mode(cfg).index() as u8,
                fat: r.fatigue,
                rate: r.rating,
                lvl: r.level,
                cash: r.cash_cents,
                on: r.online_minutes,
                cap: r.effective_max_held(cfg),
                thr: r.effective_threshold_cents_hour(cfg.is_peak(world.sim_minute), cfg) as i64,
                skill: r.strategy.skill,
                ride: r.speed_kmh(cfg, world.weather),
                walk: cfg.walk.walk_speed_mps(),
                held: r
                    .held
                    .iter()
                    .map(|h| HeldSnap {
                        o: h.order_id,
                        ph: phase_code(h.phase).to_string(),
                        prog: h.leg_progress(),
                        left: h.remaining_min,
                    })
                    .collect(),
            })
            .collect();

        let orders = pending
            .iter()
            .take(MAX_ORDERS_IN_PROMPT)
            .filter_map(|oid| world.order(*oid))
            // 只把还在大厅里的单喂给 LLM，免得它去抢已被接走的单
            .filter(|o| o.status == OrderStatus::Pending)
            .map(|o| OrderSnap {
                id: o.id,
                from: [o.merchant_lat, o.merchant_lng],
                to: [o.customer_lat, o.customer_lng],
                fee: o.quoted_fee_cents(),
                dist: o.distance_m,
                promise: o.promised_minutes,
                left: o.claim_deadline_min as i32 - world.elapsed_min as i32,
                prep: o
                    .prep_minutes
                    .unwrap_or_else(|| world.avg_prep_minutes(o.merchant_id)),
                mrate: world
                    .merchant(o.merchant_id)
                    .map(|m| m.rating)
                    .unwrap_or(4.5),
                mwalk: o.merchant_walk_difficulty,
                cwalk: o.customer_walk_difficulty,
                remark: truncate_chars(&o.remark, MAX_REMARK_CHARS),
            })
            .collect();

        let mut req = Self {
            v: PROTOCOL_VERSION,
            sim,
            riders,
            orders,
        };
        trim_to_budget(&mut req);
        req
    }

    /// 序列化后的字节数（预算断言与裁剪都用它）。
    pub fn body_bytes(&self) -> usize {
        serde_json::to_vec(self).map(|v| v.len()).unwrap_or(0)
    }

    /// 请求里的所有骑手 id（校验引用用）。
    pub fn rider_ids(&self) -> Vec<RiderId> {
        self.riders
            .iter()
            .filter_map(|r| crate::sim::parse_rider_key(&r.id))
            .collect()
    }

    /// 请求里的所有订单 id。
    pub fn order_ids(&self) -> Vec<OrderId> {
        self.orders.iter().map(|o| o.id).collect()
    }
}

/// 相位短码。
pub fn phase_code(p: crate::sim::rider::Phase) -> &'static str {
    use crate::sim::rider::Phase;
    match p {
        Phase::ToShop => "ts",
        Phase::WaitingFood => "wf",
        Phase::ToCustomer => "tc",
        Phase::ArrivedCustomer => "ac",
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

/// 裁剪统计，给控制端看「这轮砍了多少」。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrimStats {
    pub trimmed_riders: usize,
    pub trimmed_orders: usize,
    pub truncated_remarks: usize,
}

/// 超预算时的裁剪（§2.4）。优先级：备注 → 尾部订单 → 尾部骑手 → 硬砍。
///
/// 顺序完全确定（先按价值排序再截断），否则同样的世界状态会生成不同的 prompt，
/// 「同样状态 → 同样决策」的可复现性就没了。
pub fn trim_to_budget(req: &mut BrainRequest) -> TrimStats {
    let mut stats = TrimStats::default();

    for o in &mut req.orders {
        if o.remark.chars().count() > MAX_REMARK_CHARS {
            o.remark = truncate_chars(&o.remark, MAX_REMARK_CHARS);
            stats.truncated_remarks += 1;
        }
    }

    // 按价值排序（每承诺分钟的钱），让值得跑的单留在前面
    req.orders.sort_by(|a, b| {
        order_value(b)
            .total_cmp(&order_value(a))
            .then(a.id.cmp(&b.id))
    });
    // 手上越空、在线越久，越值得让 LLM 看见
    req.riders.sort_by(|a, b| {
        a.held
            .len()
            .cmp(&b.held.len())
            .then(b.on.cmp(&a.on))
            .then(a.id.cmp(&b.id))
    });

    while req.body_bytes() > MAX_PROMPT_BYTES && !req.orders.is_empty() {
        req.orders.pop();
        stats.trimmed_orders += 1;
    }
    while req.body_bytes() > MAX_PROMPT_BYTES && req.riders.len() > 1 {
        req.riders.pop();
        stats.trimmed_riders += 1;
    }
    stats
}

/// 订单价值：每承诺分钟能挣多少分。用于裁剪排序与去重择优。
fn order_value(o: &OrderSnap) -> f32 {
    o.fee as f32 / o.promise.max(1) as f32
}

// ---------------------------------------------------------------------------
// 响应
// ---------------------------------------------------------------------------

/// 一次性动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActKind {
    Claim,
    Reject,
    Pass,
}

impl ActKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ActKind::Claim => "claim",
            ActKind::Reject => "reject",
            ActKind::Pass => "pass",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ActKind::Claim => "接单",
            ActKind::Reject => "拒单",
            ActKind::Pass => "观望",
        }
    }
}

/// 一条动作。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub r: String,
    pub act: ActKind,
    pub o: OrderId,
    #[serde(default)]
    pub why: String,
}

/// 一条策略调整。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyPatch {
    pub r: String,
    #[serde(default)]
    pub price: PriceTier,
    #[serde(default)]
    pub cap: Option<u32>,
    #[serde(default)]
    pub mode: Option<u8>,
    #[serde(default)]
    pub why: String,
}

/// 站长的一类运营指令（《三层AI架构设计》§4.2）。
///
/// 和骑手层的区别：骑手只能管自己（接/拒单、期望价、背单上限、速度档），
/// 站长管的是**别人**——强制派单、限单、强制下线、召回培训。
///
/// `r` / `o` 用短键名，理由同骑手层：每个字符都是 token。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum StationOp {
    /// 限制某骑手的背单上限（0 = 只许把手上的送完，不再接新单）。
    CapRider {
        r: String,
        cap: u32,
        #[serde(default)]
        why: String,
    },
    /// 强制把某单派给某骑手。**绕过骑手本人的意愿**，但逃不过 tick 的双保险校验。
    ForceAssign {
        r: String,
        o: OrderId,
        #[serde(default)]
        why: String,
    },
    /// 强制某骑手下线（疲劳 / 安全）。手上单会由算法接手。
    ForceOffline {
        r: String,
        #[serde(default)]
        why: String,
    },
    /// 召回培训：不改即时状态，只记一次（后续影响评级 / 派单权重）。
    RecallTraining {
        r: String,
        #[serde(default)]
        why: String,
    },
}

/// LLM 的完整响应。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DecisionResponse {
    #[serde(default)]
    pub v: u8,
    #[serde(default)]
    pub acts: Vec<Action>,
    #[serde(default)]
    pub policies: Vec<PolicyPatch>,
    /// 站长的运营指令。**故意收成 `Value` 逐条解析**：某一条坏掉不该整轮作废，
    /// 也方便协议向前兼容（不认识的 op 直接丢）。
    #[serde(default)]
    pub station_ops: Vec<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// 校验层：代码的最终解释权（§4）
// ---------------------------------------------------------------------------

/// 通过校验的一条动作。
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedAct {
    pub rider: RiderId,
    pub order: OrderId,
    pub act: ActKind,
    pub why: String,
}

/// 通过校验的一条策略。
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedPolicy {
    pub rider: RiderId,
    pub price: PriceTier,
    pub cap: Option<u32>,
    pub mode: Option<SpeedMode>,
    pub why: String,
}

/// 通过校验的一条站长指令。
#[derive(Debug, Clone, PartialEq)]
pub enum ValidatedStationOp {
    CapRider {
        rider: RiderId,
        cap: u32,
        why: String,
    },
    ForceAssign {
        rider: RiderId,
        order: OrderId,
        why: String,
    },
    ForceOffline {
        rider: RiderId,
        why: String,
    },
    RecallTraining {
        rider: RiderId,
        why: String,
    },
}

impl ValidatedStationOp {
    pub fn op_name(&self) -> &'static str {
        match self {
            ValidatedStationOp::CapRider { .. } => "cap_rider",
            ValidatedStationOp::ForceAssign { .. } => "force_assign",
            ValidatedStationOp::ForceOffline { .. } => "force_offline",
            ValidatedStationOp::RecallTraining { .. } => "recall_training",
        }
    }

    pub fn rider(&self) -> RiderId {
        match self {
            ValidatedStationOp::CapRider { rider, .. }
            | ValidatedStationOp::ForceAssign { rider, .. }
            | ValidatedStationOp::ForceOffline { rider, .. }
            | ValidatedStationOp::RecallTraining { rider, .. } => *rider,
        }
    }
}

/// 校验结论。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DecisionVerdict {
    pub acts: Vec<ValidatedAct>,
    pub policies: Vec<ValidatedPolicy>,
    /// 站长的运营指令（cap / force_assign / force_offline / recall_training）。
    pub station_ops: Vec<ValidatedStationOp>,
    /// 被丢弃的每一条意图及原因（骑手 / 订单 / 为什么）。**不静默丢**。
    pub drops: Vec<ai::DroppedIntent>,
    /// `Some(原因)` = 整轮放弃（等于没给计划，回退算法）。
    pub rejected: Option<String>,
}

impl DecisionVerdict {
    /// 这轮有没有任何可执行的东西。
    pub fn is_empty(&self) -> bool {
        self.acts.is_empty() && self.policies.is_empty() && self.station_ops.is_empty()
    }

    pub fn dropped(&self) -> usize {
        self.drops.len()
    }

    pub fn dropped_in(&self, layer: ai::IntentLayer) -> usize {
        self.drops.iter().filter(|d| d.layer == layer).count()
    }

    pub fn dropped_acts(&self) -> usize {
        self.dropped_in(ai::IntentLayer::Act)
    }

    pub fn dropped_policies(&self) -> usize {
        self.dropped_in(ai::IntentLayer::Policy)
    }

    pub fn dropped_station_ops(&self) -> usize {
        self.dropped_in(ai::IntentLayer::StationOp)
    }
}

/// 这个骑手现在归 AI 管吗？（**不看是否空闲**——空闲与否是派单资格，不是管理权）
///
/// - 模拟骑手：在线即归 AI；下线 = 不接受新指令。
/// - 真人：只有外圈明确交出来（AutoPilot 托管）才归 AI。**在线的真人永远
///   返回 `false`**——玩家自己在操作，AI 必须让位，绝不能替他接单。
fn managed_by_ai(world: &World, rid: RiderId, hosted: &[RiderId]) -> bool {
    match world.rider(rid) {
        Some(r) if r.kind == RiderKind::Simulated => r.online,
        Some(_) => hosted.contains(&rid),
        None => false,
    }
}

/// 四级校验流水线（§4.1）。
///
/// ```text
/// ① 结构校验：版本对不对？有没有内容？          → 失败整轮放弃
/// ② 引用校验：骑手 / 订单是否真实存在且在状态内？→ 失败丢该条
/// ③ 语义校验：重复 claim？越界枚举？            → 失败丢该条
/// ④ 约束钳制：cap / mode 是否在允许区间？       → 一律 clamp，不丢弃
/// ```
///
/// **必须查活的世界状态**：这几秒里订单可能已被抢走，
/// 只按占坑时的快照校验会让重复抢单漏过去。
///
/// `roster` 是**本轮可接受普通派单**的骑手（`tick::ai_roster` 的输出），
/// `hosted` 是本轮由 AutoPilot 代管的真人。两者都由调用方按当前控制模式
/// 算好再传进来——协议层不应该自己猜谁在线、谁托管。
pub fn validate_decision(
    world: &World,
    resp: &DecisionResponse,
    roster: &[RiderId],
    hosted: &[RiderId],
) -> DecisionVerdict {
    let mut out = DecisionVerdict::default();

    // ① 结构校验
    if resp.v != 0 && resp.v != PROTOCOL_VERSION {
        out.rejected = Some(format!("协议版本不匹配（收到 {}）", resp.v));
        return out;
    }
    if resp.acts.is_empty() && resp.policies.is_empty() && resp.station_ops.is_empty() {
        out.rejected = Some("acts / policies / station_ops 都是空的".to_string());
        return out;
    }

    // ② 引用校验
    struct Cand {
        rider: RiderId,
        order: OrderId,
        act: ActKind,
        why: String,
        score: f32,
    }
    let mut cands: Vec<Cand> = Vec::new();
    for a in &resp.acts {
        let mut drop = |r: Option<RiderId>, reason: ai::DropReason| {
            out.drops.push(ai::DroppedIntent::new(
                ai::IntentLayer::Act,
                r,
                Some(a.o),
                reason,
            ));
        };
        let Some(rid) = crate::sim::parse_rider_key(&a.r) else {
            drop(None, ai::DropReason::BadRiderKey);
            continue;
        };
        if world.rider(rid).is_none() {
            drop(Some(rid), ai::DropReason::UnknownRider);
            continue;
        }
        // 管理权：模拟骑手在线 / 真人已被托管。未托管的真人**不在这里放行**。
        if !managed_by_ai(world, rid, hosted) {
            drop(Some(rid), ai::DropReason::RiderNotManaged);
            continue;
        }
        // 派单资格：在途 / 背满 / 受伤 / 日单量上限 / 离线都由名单收紧。
        if !roster.contains(&rid) {
            drop(Some(rid), ai::DropReason::RiderNotEligible);
            continue;
        }
        let Some(order) = world.order(a.o) else {
            drop(Some(rid), ai::DropReason::UnknownOrder);
            continue;
        };
        if order.status != OrderStatus::Pending {
            drop(Some(rid), ai::DropReason::OrderNotPending);
            continue;
        }
        // 短期排除项：拒单 TTL 内 / 观望窗口内，普通派单不许重复塞回去。
        // 只管 `claim`——`reject`/`pass` 本身不是派单，重复表达是幂等的。
        if a.act == ActKind::Claim
            && let Some(reason) = ai::refusal_block(world, rid, a.o)
        {
            drop(Some(rid), reason);
            continue;
        }
        cands.push(Cand {
            rider: rid,
            order: a.o,
            act: a.act,
            // why 只截断不丢弃：文案长了不该判一条合法决策死刑
            why: truncate_chars(&a.why, MAX_WHY_CHARS),
            score: ai::evaluate(world, rid, a.o)
                .map(|x| x.score)
                .unwrap_or(f32::NEG_INFINITY),
        });
    }

    // ③ 语义校验：去重必须**分两趟**做。
    //
    // 一趟同时管「同骑手」和「同订单」会互相拆台：后面那条既可能在
    // 订单维度胜出、又可能在骑手维度落败，两个 keep 都不为真，
    // 结果整单没人接。分两趟，每趟只解一个维度。
    let mut keep = vec![true; cands.len()];
    // 落选原因：只在第一次落选时写，后面两趟不会互相覆盖。
    let mut why: Vec<Option<ai::DropReason>> = vec![None; cands.len()];

    // ③a 一个骑手一次只能 claim 一单 → 留分最高的
    let mut best_rider: HashMap<RiderId, usize> = HashMap::new();
    for (i, c) in cands.iter().enumerate() {
        if c.act != ActKind::Claim {
            continue;
        }
        match best_rider.get(&c.rider) {
            Some(&j) if cands[j].score.total_cmp(&c.score) != Ordering::Less => {
                keep[i] = false;
                why[i] = Some(ai::DropReason::DuplicateRider);
            }
            Some(&j) => {
                keep[j] = false;
                why[j] = Some(ai::DropReason::DuplicateRider);
                best_rider.insert(c.rider, i);
            }
            None => {
                best_rider.insert(c.rider, i);
            }
        }
    }

    // ③b 一单只能被一个骑手 claim → 留分最高的
    let mut best_claim: HashMap<OrderId, usize> = HashMap::new();
    for (i, c) in cands.iter().enumerate() {
        if !keep[i] || c.act != ActKind::Claim {
            continue;
        }
        match best_claim.get(&c.order) {
            Some(&j) if cands[j].score.total_cmp(&c.score) != Ordering::Less => {
                keep[i] = false;
                why[i] = Some(ai::DropReason::DuplicateOrder);
            }
            Some(&j) => {
                keep[j] = false;
                why[j] = Some(ai::DropReason::DuplicateOrder);
                best_claim.insert(c.order, i);
            }
            None => {
                best_claim.insert(c.order, i);
            }
        }
    }

    for (i, c) in cands.into_iter().enumerate() {
        if !keep[i] {
            if let Some(reason) = why[i] {
                out.drops.push(ai::DroppedIntent::new(
                    ai::IntentLayer::Act,
                    Some(c.rider),
                    Some(c.order),
                    reason,
                ));
            }
            continue;
        }
        out.acts.push(ValidatedAct {
            rider: c.rider,
            order: c.order,
            act: c.act,
            why: c.why,
        });
    }

    // ④ 约束钳制
    //
    // 策略只管「归 AI 管」的人：在线的真人自己决定速度和背单，AI 无权替他设。
    // 这里**不看空闲**——背着单的骑手照样可以调 cap / 档位。
    for p in &resp.policies {
        let mut drop = |r: Option<RiderId>, reason: ai::DropReason| {
            out.drops.push(ai::DroppedIntent::new(
                ai::IntentLayer::Policy,
                r,
                None,
                reason,
            ));
        };
        let Some(rid) = crate::sim::parse_rider_key(&p.r) else {
            drop(None, ai::DropReason::BadRiderKey);
            continue;
        };
        if world.rider(rid).is_none() {
            drop(Some(rid), ai::DropReason::UnknownRider);
            continue;
        }
        if !managed_by_ai(world, rid, hosted) {
            drop(Some(rid), ai::DropReason::RiderNotManaged);
            continue;
        }
        // cap 超上限一律 clamp（LLM 突不破物理上限，但也不必罚它）
        let cap = p.cap.map(|c| c.min(world.cfg.backorder.max_held));
        // mode 非法 / 被开关否决 → 降级为「不覆盖」
        let mode = p
            .mode
            .and_then(|m| SpeedMode::from_index(m as usize))
            .filter(|m| world.cfg.speed_mode_allowed(*m));
        out.policies.push(ValidatedPolicy {
            rider: rid,
            price: p.price,
            cap,
            mode,
            why: truncate_chars(&p.why, MAX_WHY_CHARS),
        });
    }

    // ⑤ 站长指令：引用要真、一骑手一轮一条、cap 要钳。
    //
    // 顺序：先逐条解析（结构）→ 查骑手/订单（引用）→ 一骑手一条（语义）→ clamp（约束）。
    //
    // **只对模拟骑手**：站长指令是「运营动作」，`force_offline` 会一直留到日结，
    // `cap` 会改写他自己的策略；托管真人的排班属于玩家账户，AI 不该替他决定
    // 什么时候下线。托管真人要派单走 `claim`（已在 `roster` 里），不需要站长通道。
    let mut seen_rider: HashMap<RiderId, ()> = HashMap::new();
    for raw in &resp.station_ops {
        let mut drop = |r: Option<RiderId>, o: Option<OrderId>, reason: ai::DropReason| {
            out.drops.push(ai::DroppedIntent::new(
                ai::IntentLayer::StationOp,
                r,
                o,
                reason,
            ));
        };
        let Ok(op) = serde_json::from_value::<StationOp>(raw.clone()) else {
            drop(None, None, ai::DropReason::BadStationOp);
            continue;
        };

        // 目标骑手必须真实存在、且是模拟骑手（AI 管不了真人）。
        let target = match &op {
            StationOp::CapRider { r, .. }
            | StationOp::ForceAssign { r, .. }
            | StationOp::ForceOffline { r, .. }
            | StationOp::RecallTraining { r, .. } => crate::sim::parse_rider_key(r),
        };
        let Some(rid) = target else {
            drop(None, None, ai::DropReason::BadRiderKey);
            continue;
        };
        let Some(rider) = world.rider(rid) else {
            drop(Some(rid), None, ai::DropReason::UnknownRider);
            continue;
        };
        if rider.kind != RiderKind::Simulated {
            drop(Some(rid), None, ai::DropReason::RiderNotManaged);
            continue;
        }
        // 语义：一个骑手一轮只接受一条站长指令（cap 与 force_offline 打架时以第一条为准）。
        if seen_rider.contains_key(&rid) {
            drop(Some(rid), None, ai::DropReason::DuplicateRider);
            continue;
        }

        let why = |w: &String| truncate_chars(w, MAX_WHY_CHARS);
        let validated = match op {
            StationOp::CapRider { cap, why: w, .. } => ValidatedStationOp::CapRider {
                rider: rid,
                cap: cap.min(world.cfg.backorder.max_held),
                why: why(&w),
            },
            StationOp::ForceAssign { o, why: w, .. } => {
                // 强派也得指向一张还在大厅里的真单，否则丢。
                match world.order(o) {
                    Some(order) if order.status == OrderStatus::Pending => {
                        ValidatedStationOp::ForceAssign {
                            rider: rid,
                            order: o,
                            why: why(&w),
                        }
                    }
                    Some(_) => {
                        drop(Some(rid), Some(o), ai::DropReason::OrderNotPending);
                        continue;
                    }
                    None => {
                        drop(Some(rid), Some(o), ai::DropReason::UnknownOrder);
                        continue;
                    }
                }
            }
            StationOp::ForceOffline { why: w, .. } => ValidatedStationOp::ForceOffline {
                rider: rid,
                why: why(&w),
            },
            StationOp::RecallTraining { why: w, .. } => ValidatedStationOp::RecallTraining {
                rider: rid,
                why: why(&w),
            },
        };
        seen_rider.insert(rid, ());
        out.station_ops.push(validated);
    }

    if out.is_empty() {
        out.rejected = Some("所有条目都不合法".to_string());
    }
    out
}

// ---------------------------------------------------------------------------
// 提示词
// ---------------------------------------------------------------------------

/// 系统提示词（§七）。目标是**短**：这些字每一轮都要花 token。
pub fn system_prompt() -> &'static str {
    "你是外卖骑手的策略大脑。为每个骑手决定：接哪些单、期望什么价位、最多同时背几单。\n\
     只输出 JSON，禁止解释、前后缀、Markdown 代码块。\n\
     格式：{\"acts\":[{\"r\":\"r1\",\"act\":\"claim\",\"o\":43,\"why\":\"时薪32\"}],\
     \"policies\":[{\"r\":\"r1\",\"price\":\"high\",\"cap\":2,\"mode\":1,\"why\":\"雨天保守\"}]}\n\
     硬性约束：\n\
     1. r 只能取自 riders 里的 id，o 只能取自 orders 里的 id，编造的会被丢弃；\n\
     2. act 只能是 claim / reject / pass；\n\
     3. price 只能是 low / normal / high / premium；\n\
     4. cap 是 0~3 的整数，超出会被截断；\n\
     5. mode 是 0 / 1 / 2，且可能被系统开关否决；\n\
     6. 每个骑手最多 claim 一单，每单最多被一个骑手 claim；\n\
     7. 宁缺毋滥：不划算的不要硬接，acts 允许是空数组；\n\
     8. why 不超过 20 字，必须引用具体数字；\n\
     9. 拒过单的骑手短期内再 claim 同一单会被丢弃，观望中的骑手也会被挡住——\n\
        窗口过了再考虑，别重复试探。\n\
     决策要点：\n\
     - 雨天 / 高峰提高 price 档位：少而精，别让骑手在路上冒险；\n\
     - fat 高的骑手把 cap 压低，避免超时和事故；\n\
     - mwalk / cwalk 是 2 的单要走进商场或上高层，实际耗时会明显变长；\n\
     - left 已经接近 0 的单，先算「骑手赶到 + 出餐」来不来得及，来不及就别接。\n\
      站长权限（另一可选数组，用来管别人，不是管自己）：\n\
      station_ops 每项形如 {\"op\":\"cap_rider\",\"r\":\"r5\",\"cap\":1,\"why\":\"暴雨\"}；\n\
      op 只能是 cap_rider / force_assign / force_offline / recall_training；\n\
      force_assign 要带 o（订单 id）；每个骑手一轮最多一条站长指令；\n\
      只在确实有必要时用（连续超时、疲劳过高、积压无人接），宁缺毋滥。"
}

/// 请求体 → 用户消息（紧凑 JSON）。
pub fn user_prompt(req: &BrainRequest) -> String {
    serde_json::to_string(req).unwrap_or_else(|_| "{}".to_string())
}

// ---------------------------------------------------------------------------
// 状态指纹（§6.3）
// ---------------------------------------------------------------------------

/// 世界状态指纹：相同指纹 = 相同 prompt = 可复用上次决策。
///
/// 用 **FNV-1a 64** 而不是 `DefaultHasher`——后者跨进程不稳定，
/// 会让同一份录像在不同运行间给出不同决策，可复现性就没了。
///
/// **关键在量化**：位置取 100m 网格、时间取 5 分钟、只吃「决策相关字段」。
/// 全精度哈希永远不会命中，缓存就是白做的。
pub fn state_fingerprint(req: &BrainRequest) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    fn mix(h: &mut u64, s: &str) {
        for b in s.as_bytes() {
            *h ^= *b as u64;
            *h = h.wrapping_mul(FNV_PRIME);
        }
        *h ^= b'|' as u64;
        *h = h.wrapping_mul(FNV_PRIME);
    }

    /// 坐标 → 100m 网格（约 0.001 度）。
    fn grid(v: f64) -> i64 {
        (v * 1000.0).round() as i64
    }

    let mut h = FNV_OFFSET;
    // 时间取 5 分钟粒度：每 tick 都变的话缓存命中率是 0
    mix(&mut h, &format!("t{}", req.sim.min / 5));
    mix(&mut h, &req.sim.weather);
    mix(&mut h, &format!("p{}", req.sim.peak));

    for r in &req.riders {
        mix(&mut h, &r.id);
        mix(
            &mut h,
            &format!("{},{},{}", grid(r.pos[0]), grid(r.pos[1]), r.mode),
        );
        mix(&mut h, &format!("c{}", r.cap));
        for held in &r.held {
            mix(&mut h, &format!("{}:{}:{}", held.o, held.ph, held.left));
        }
    }
    for o in &req.orders {
        mix(&mut h, &format!("o{}:{}:{}", o.id, o.fee, o.left));
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{config::SimConfig, rider_key, world::World};

    fn world_with_orders(n: u32) -> World {
        let mut w = World::new_for_test(SimConfig::default(), 7);
        w.generate_order_batch(n);
        w
    }

    fn act(rider: RiderId, order: OrderId) -> Action {
        Action {
            r: rider_key(rider),
            act: ActKind::Claim,
            o: order,
            why: "顺路".into(),
        }
    }

    /// 默认语境：在线的模拟骑手都在名单里，没有托管真人。
    ///
    /// 之所以不直接传 `w.idle_riders()`：那是算法侧的旧名单，会把在线真人算进去，
    /// 与 `tick::ai_roster` 的口径不一致——测试必须跟生产同源。
    fn check(w: &World, resp: &DecisionResponse) -> DecisionVerdict {
        let roster: Vec<RiderId> = w
            .riders
            .iter()
            .filter(|r| r.kind == RiderKind::Simulated && r.online)
            .map(|r| r.id)
            .collect();
        validate_decision(w, resp, &roster, &[])
    }

    /// AutoPilot 托管的真人：名单里有他、托管名单里有他 → 允许接 LLM 的派单。
    ///
    /// 这条用的是**生产同源**的 `ai_roster`，两层的口径必须一致：
    /// `ai_roster` 说谁可以派，协议层就不许再按 `kind` 一刀切掉。
    #[test]
    fn an_autopilot_human_can_take_the_llm_order() {
        let mut w = world_with_orders(2);
        let hid = w.spawn_human_rider("小赵");
        let oid = w.pending_orders()[0];

        let roster = crate::sim::tick::ai_roster(
            &w,
            &crate::sim::tick::TickInput {
                hosted_humans: vec![hid],
                ..Default::default()
            },
        );
        assert!(roster.contains(&hid), "托管的真人应当进决策名单");

        let resp = DecisionResponse {
            v: PROTOCOL_VERSION,
            acts: vec![act(hid, oid)],
            ..Default::default()
        };
        let v = validate_decision(&w, &resp, &roster, &[hid]);
        assert_eq!(v.acts.len(), 1, "修复后托管真人的 claim 必须通过");
        assert_eq!(v.acts[0].rider, hid);
        assert_eq!(v.dropped(), 0);
    }

    /// 在线的真人：**即使有人错误地把他塞进名单**，协议层也必须挡住。
    ///
    /// 这是「修复托管过滤」最危险的连带面：一旦改成只按名单放行，
    /// 名单算错就等于 AI 直接替在线玩家接单。
    #[test]
    fn an_online_human_is_never_managed_by_the_ai() {
        let mut w = world_with_orders(2);
        let hid = w.spawn_human_rider("小赵");
        w.rider_mut(hid).expect("在").online = true;
        let oid = w.pending_orders()[0];

        // 真实名单里不含他（ai_roster 只放托管真人）
        let real = crate::sim::tick::ai_roster(&w, &crate::sim::tick::TickInput::default());
        assert!(!real.contains(&hid), "在线真人不可能进名单");

        // 就算名单被算错、他被塞了进来，管理权这道闸门仍然拦住
        let resp = DecisionResponse {
            v: PROTOCOL_VERSION,
            acts: vec![act(hid, oid)],
            ..Default::default()
        };
        let v = validate_decision(&w, &resp, &[hid], &[]);
        assert!(v.acts.is_empty(), "AI 不许替在线玩家接单");
        assert_eq!(v.drops.len(), 1);
        assert_eq!(v.drops[0].reason, ai::DropReason::RiderNotManaged);
    }

    /// 托管状态是在**校验那一刻**判定的：同一个世界，托管前挡住、托管后放行。
    #[test]
    fn hosted_state_is_judged_at_validation_time() {
        let mut w = world_with_orders(2);
        let hid = w.spawn_human_rider("小赵");
        let oid = w.pending_orders()[0];
        let resp = DecisionResponse {
            v: PROTOCOL_VERSION,
            acts: vec![act(hid, oid)],
            ..Default::default()
        };

        let before = validate_decision(&w, &resp, &[hid], &[]);
        assert_eq!(before.drops[0].reason, ai::DropReason::RiderNotManaged);

        let after = validate_decision(&w, &resp, &[hid], &[hid]);
        assert_eq!(after.acts.len(), 1, "托管名单一变，结论必须跟着变");
    }

    /// 策略跟随同一套管理权：托管真人可调 cap/档位，在线真人不许。
    #[test]
    fn policies_follow_the_same_ownership_rule() {
        let mut w = world_with_orders(1);
        let hid = w.spawn_human_rider("小赵");
        let resp = DecisionResponse {
            v: PROTOCOL_VERSION,
            policies: vec![PolicyPatch {
                r: rider_key(hid),
                price: PriceTier::High,
                cap: Some(2),
                mode: Some(2),
                why: "跑快点".into(),
            }],
            ..Default::default()
        };

        let hosted = validate_decision(&w, &resp, &[], &[hid]);
        assert_eq!(hosted.policies.len(), 1, "托管中由 AI 代管策略");
        assert_eq!(hosted.dropped(), 0);

        let online = validate_decision(&w, &resp, &[], &[]);
        assert!(online.policies.is_empty(), "在线真人的档位由他自己决定");
        assert_eq!(online.drops[0].reason, ai::DropReason::RiderNotManaged);
    }

    /// 协议层也要认拒单记录，且丢弃要带全「谁 / 哪单 / 为什么」。
    #[test]
    fn a_refused_order_is_dropped_with_a_reason_at_the_protocol_layer() {
        let mut w = world_with_orders(2);
        let rid = w.riders[0].id;
        let oid = w.pending_orders()[0];
        w.record_refusals(&[crate::sim::ai::LlmRefusal {
            rider: rid,
            order: oid,
            kind: crate::sim::world::RefusalKind::Reject,
            reason: "太远".into(),
        }]);

        let resp = DecisionResponse {
            v: PROTOCOL_VERSION,
            acts: vec![act(rid, oid)],
            ..Default::default()
        };
        let v = check(&w, &resp);
        assert!(v.acts.is_empty());
        assert_eq!(v.dropped_acts(), 1);
        assert_eq!(v.drops[0].reason, ai::DropReason::RefusedOrder);
        assert_eq!(v.drops[0].rider, Some(rid));
        assert_eq!(v.drops[0].order, Some(oid));
        assert_eq!(v.drops[0].layer, ai::IntentLayer::Act);
        assert_eq!(
            v.drops[0].brief(),
            format!("act r{rid}/o{oid} refused_order")
        );
    }

    /// 拒单只能挡住拒过这一单的那个骑手；别的骑手认领同一单照常通过。
    #[test]
    fn a_refusal_only_blocks_the_rider_who_refused() {
        let mut w = world_with_orders(2);
        let (r0, r1) = (w.riders[0].id, w.riders[1].id);
        let oid = w.pending_orders()[0];
        w.record_refusals(&[crate::sim::ai::LlmRefusal {
            rider: r0,
            order: oid,
            kind: crate::sim::world::RefusalKind::Reject,
            reason: "太远".into(),
        }]);

        let resp = DecisionResponse {
            v: PROTOCOL_VERSION,
            acts: vec![act(r0, oid), act(r1, oid)],
            ..Default::default()
        };
        let v = check(&w, &resp);
        assert_eq!(v.acts.len(), 1, "只该丢拒过单的那一条");
        assert_eq!((v.acts[0].rider, v.acts[0].order), (r1, oid));
        assert_eq!(v.dropped_acts(), 1);
        assert_eq!(v.drops[0].reason, ai::DropReason::RefusedOrder);
        assert_eq!(v.drops[0].rider, Some(r0));
    }

    /// 拒单 TTL 过了，协议层也要放行——否则「过期」在两层里说法不一致。
    #[test]
    fn an_expired_refusal_does_not_block_the_protocol_layer() {
        let mut w = world_with_orders(2);
        let rid = w.riders[0].id;
        let oid = w.pending_orders()[0];
        w.record_refusals(&[crate::sim::ai::LlmRefusal {
            rider: rid,
            order: oid,
            kind: crate::sim::world::RefusalKind::Reject,
            reason: "太远".into(),
        }]);
        w.elapsed_min = w.cfg.llm_refusal_ttl_min as u64;

        let resp = DecisionResponse {
            v: PROTOCOL_VERSION,
            acts: vec![act(rid, oid)],
            ..Default::default()
        };
        let v = check(&w, &resp);
        assert_eq!(v.acts.len(), 1, "TTL 到期后可以重新评估");
        assert_eq!((v.acts[0].rider, v.acts[0].order), (rid, oid));
        assert!(v.drops.is_empty());
    }

    #[test]
    fn request_is_deterministic_and_quantized() {
        let w = world_with_orders(4);
        let ids = w.idle_riders();
        let pids = w.pending_orders();
        let a = BrainRequest::build(&w, &ids, &pids);
        let b = BrainRequest::build(&w, &ids, &pids);
        assert_eq!(a, b, "同样的世界状态必须给出同样的请求");
        assert_eq!(state_fingerprint(&a), state_fingerprint(&b));

        // 只有位置变了（>100m）指纹才该变
        let mut c = a.clone();
        c.riders[0].pos[0] += 0.002;
        assert_ne!(state_fingerprint(&a), state_fingerprint(&c));
        // 位置挪 20m（同一格）不该变
        let mut d = a.clone();
        d.riders[0].pos[0] += 0.0002;
        assert_eq!(state_fingerprint(&a), state_fingerprint(&d));
    }

    #[test]
    fn request_carries_leg_progress_and_walk_cost() {
        let w = world_with_orders(3);
        let req = BrainRequest::build(&w, &w.idle_riders(), &w.pending_orders());
        assert!(!req.orders.is_empty(), "应该有订单");
        // 步行难度必须随单下发——LLM 判「商场单划不划算」全靠它
        for o in &req.orders {
            assert!(o.mwalk <= 2 && o.cwalk <= 2);
            assert!(o.fee > 0);
        }
    }

    #[test]
    fn fabricated_refs_are_dropped() {
        let w = world_with_orders(2);
        let pending = w.pending_orders();
        let resp = DecisionResponse {
            v: PROTOCOL_VERSION,
            acts: vec![
                Action {
                    r: "r999".into(),
                    act: ActKind::Claim,
                    o: pending[0],
                    why: String::new(),
                },
                Action {
                    r: rider_key(1),
                    act: ActKind::Claim,
                    o: 9999,
                    why: String::new(),
                },
                act(1, pending[0]),
            ],
            policies: vec![],
            station_ops: vec![],
        };
        let v = check(&w, &resp);
        assert_eq!(v.acts.len(), 1, "编造的引用必须被丢掉");
        assert_eq!(v.dropped_acts(), 2);
        assert!(v.rejected.is_none(), "有一条合法就不算整轮失败");
    }

    #[test]
    fn duplicate_claim_keeps_only_one() {
        let w = world_with_orders(2);
        let pending = w.pending_orders();
        let resp = DecisionResponse {
            v: PROTOCOL_VERSION,
            acts: vec![act(1, pending[0]), act(2, pending[0]), act(3, pending[1])],
            policies: vec![],
            station_ops: vec![],
        };
        let v = check(&w, &resp);
        assert_eq!(v.acts.len(), 2, "一单只能被一个骑手抢");
        assert_eq!(v.dropped_acts(), 1);
        assert_eq!(v.acts.iter().filter(|a| a.order == pending[0]).count(), 1);
    }

    #[test]
    fn one_rider_claims_at_most_one_order() {
        let w = world_with_orders(3);
        let pending = w.pending_orders();
        let resp = DecisionResponse {
            v: PROTOCOL_VERSION,
            acts: vec![act(1, pending[0]), act(1, pending[1])],
            policies: vec![],
            station_ops: vec![],
        };
        let v = check(&w, &resp);
        assert_eq!(v.acts.len(), 1, "一个骑手一次只能接一单");
        assert_eq!(v.dropped_acts(), 1);
    }

    #[test]
    fn cap_is_clamped_and_illegal_mode_becomes_none() {
        let w = world_with_orders(1);
        let max_held = w.cfg.backorder.max_held;
        let resp = DecisionResponse {
            v: PROTOCOL_VERSION,
            acts: vec![],
            policies: vec![
                PolicyPatch {
                    r: rider_key(1),
                    price: PriceTier::Premium,
                    cap: Some(99),
                    mode: Some(9),
                    why: "x".into(),
                },
                PolicyPatch {
                    r: "r404".into(),
                    price: PriceTier::High,
                    cap: Some(1),
                    mode: None,
                    why: String::new(),
                },
            ],
            station_ops: vec![],
        };
        let v = check(&w, &resp);
        assert_eq!(v.policies.len(), 1);
        assert_eq!(v.policies[0].cap, Some(max_held), "cap 必须被钳到上限");
        assert_eq!(v.policies[0].mode, None, "非法档位降级为不覆盖");
        assert_eq!(v.policies[0].price, PriceTier::Premium);
        assert_eq!(v.dropped_policies(), 1);
    }

    #[test]
    fn closed_gate_revokes_mode_override() {
        let mut w = world_with_orders(1);
        w.cfg.allow_unrestricted = false;
        let resp = DecisionResponse {
            v: PROTOCOL_VERSION,
            acts: vec![],
            policies: vec![PolicyPatch {
                r: rider_key(1),
                price: PriceTier::Normal,
                cap: None,
                mode: Some(2),
                why: String::new(),
            }],
            station_ops: vec![],
        };
        let v = check(&w, &resp);
        assert_eq!(
            v.policies[0].mode, None,
            "开关关着的时候，LLM 说 2 也不许解限速"
        );
    }

    #[test]
    fn empty_or_wrong_version_rejects_the_whole_round() {
        let w = world_with_orders(2);
        let empty = DecisionResponse {
            v: PROTOCOL_VERSION,
            acts: vec![],
            policies: vec![],
            station_ops: vec![],
        };
        let v = check(&w, &empty);
        assert!(v.rejected.is_some(), "空响应等于没给计划");

        let bad = DecisionResponse {
            v: 99,
            acts: vec![act(1, w.pending_orders()[0])],
            policies: vec![],
            station_ops: vec![],
        };
        let v = check(&w, &bad);
        assert!(v.rejected.is_some(), "版本不匹配必须整轮放弃");
        assert!(v.acts.is_empty());
    }

    #[test]
    fn why_is_truncated_not_dropped() {
        let w = world_with_orders(1);
        let order = w.pending_orders()[0];
        let resp = DecisionResponse {
            v: PROTOCOL_VERSION,
            acts: vec![Action {
                r: rider_key(1),
                act: ActKind::Claim,
                o: order,
                why: "这个骑手离商家只有三百米而且顺路度很高可以顺带再背一单".into(),
            }],
            policies: vec![],
            station_ops: vec![],
        };
        let v = check(&w, &resp);
        assert_eq!(v.acts.len(), 1);
        assert!(v.acts[0].why.chars().count() <= MAX_WHY_CHARS);
    }

    #[test]
    fn bad_json_is_rejected_by_deserialization() {
        // LLM 常犯的错：给出 JSON 但字段名不对 / 类型不对
        let j = serde_json::json!({"acts":[{"r":"r1","act":"grab","o":1}]});
        assert!(serde_json::from_value::<DecisionResponse>(j).is_err());
        // 但合法的简写要能解出来（省 token 的代价是默认值必须给对）
        let ok: DecisionResponse =
            serde_json::from_value(serde_json::json!({"acts":[{"r":"r1","act":"pass","o":1}]}))
                .expect("最小动作应该能解析");
        assert_eq!(ok.v, 0, "版本缺省为 0 = 不校验");
        assert!(ok.policies.is_empty());
    }

    // ───────────────────── 站长指令（station_ops） ─────────────────────

    fn ops_resp(ops: Vec<serde_json::Value>) -> DecisionResponse {
        DecisionResponse {
            v: PROTOCOL_VERSION,
            acts: vec![],
            policies: vec![],
            station_ops: ops,
        }
    }

    #[test]
    fn station_ops_alone_are_enough_and_cap_is_clamped() {
        let w = world_with_orders(1);
        let max_held = w.cfg.backorder.max_held;
        let resp = ops_resp(vec![serde_json::json!({
            "op": "cap_rider", "r": rider_key(1), "cap": 99, "why": "暴雨+连续超时"
        })]);
        let v = check(&w, &resp);
        assert!(v.rejected.is_none(), "只有站长指令也算有效一轮");
        assert!(v.acts.is_empty() && v.policies.is_empty());
        assert_eq!(v.station_ops.len(), 1);
        match &v.station_ops[0] {
            ValidatedStationOp::CapRider { rider, cap, .. } => {
                assert_eq!(*rider, 1);
                assert_eq!(*cap, max_held, "cap 必须钳到上限");
            }
            other => panic!("期望 cap_rider，得到 {other:?}"),
        }
    }

    #[test]
    fn station_op_on_unknown_rider_is_dropped() {
        let w = world_with_orders(1);
        let resp = ops_resp(vec![
            serde_json::json!({"op": "force_offline", "r": "r999", "why": "不存在"}),
            serde_json::json!({"op": "recall_training", "r": rider_key(1), "why": "准时率68%"}),
        ]);
        let v = check(&w, &resp);
        assert_eq!(v.dropped_station_ops(), 1);
        assert_eq!(v.station_ops.len(), 1);
        assert!(matches!(
            v.station_ops[0],
            ValidatedStationOp::RecallTraining { .. }
        ));
    }

    #[test]
    fn force_assign_requires_a_pending_order() {
        let w = world_with_orders(1);
        let good = w.pending_orders()[0];
        let bad = ops_resp(vec![
            serde_json::json!({"op": "force_assign", "r": rider_key(1), "o": 999999, "why": "编的"}),
            serde_json::json!({"op": "force_assign", "r": rider_key(2), "o": good, "why": "积压18分钟"}),
        ]);
        let v = check(&w, &bad);
        assert_eq!(v.dropped_station_ops(), 1, "指向不存在的单必须丢");
        assert_eq!(v.station_ops.len(), 1);
        assert!(matches!(
            v.station_ops[0],
            ValidatedStationOp::ForceAssign { order, .. } if order == good
        ));
    }

    #[test]
    fn one_station_op_per_rider_per_round() {
        let w = world_with_orders(1);
        let resp = ops_resp(vec![
            serde_json::json!({"op": "cap_rider", "r": rider_key(1), "cap": 1, "why": "先限"}),
            serde_json::json!({"op": "force_offline", "r": rider_key(1), "why": "后又下线"}),
        ]);
        let v = check(&w, &resp);
        assert_eq!(v.station_ops.len(), 1, "一个骑手一轮只认第一条");
        assert_eq!(v.dropped_station_ops(), 1);
        assert_eq!(v.station_ops[0].op_name(), "cap_rider");
    }

    #[test]
    fn unknown_station_op_is_dropped_not_fatal() {
        let w = world_with_orders(1);
        let resp = ops_resp(vec![
            serde_json::json!({"op": "explode", "r": rider_key(1)}),
            serde_json::json!({"op": "force_offline", "r": rider_key(1), "why": "疲劳"}),
        ]);
        let v = check(&w, &resp);
        assert_eq!(v.dropped_station_ops(), 1);
        assert_eq!(v.station_ops.len(), 1);
    }
}
