//! 烽火台 AI（世界导演，《三层AI架构设计》§5）。
//!
//! 三层里**最少动**的一层：60 分钟才看一眼，一次最多改两个世界参数。
//! 它的职责不是「让世界转起来」——那是下层骑手 AI 的事；它管的是**节奏**：
//! 午高峰下一场暴雨、连亏三天降一次佣金、把当天的故事用一句话讲出来。
//!
//! ## 它只碰世界参数，不碰具体的人和单
//!
//! 输出的是 [`WorldOp`]：调天气、调佣金、给一句叙事。**它不能派单、不能改骑手**——
//! 那些是站长与骑手层的权力。层与层之间靠这个划清边界，越权的一律在
//! [`validate_world_ops`] 里被丢掉。
//!
//! ## 四级校验，和骑手层同一套骨架
//!
//! ```text
//! ① 结构：逐条解析 WorldOp，解析不出来就丢这一条（不是整轮放弃）
//! ② 引用：天气码认得出吗？佣金是不是数字？
//! ③ 语义：同类操作去重（一轮里调两次天气只认第一次）
//! ④ 钳制：佣金封顶、叙事截断
//! ```
//!
//! ## 两段式，绝不阻塞 tick
//!
//! 和 [`super::brain::TacticalBrain`] 一样是 `prepare` / `finish`：HTTP 在后台任务里跑，
//! tick 该干嘛干嘛。由 [`super::orchestrator::AiOrchestrator`] 决定「这一轮轮到烽火台没有」。

use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::sim::weather::Weather;
use crate::sim::world::World;

use super::brain::BrainSource;
use super::client::{LlmClient, LlmConfig};

/// 叙事文本上限（字符）。日志面板一行放得下，也防止 LLM 写小作文。
pub const MAX_NARRATE_CHARS: usize = 60;
/// `why` 字段上限（字符）。它只用来解释「为什么这么改」，一句话足够。
pub const MAX_WHY_CHARS: usize = 24;
/// 佣金率上限。再高会把人赶跑，这条是硬线，不是建议。
pub const MAX_COMMISSION: f32 = 0.30;
/// 给烽火台的世界目标（写死在 prompt 里，玩家改不了——它是玩法骨架）。
pub const GLOBAL_GOAL: &str = "让直播间有戏剧性但不劝退：午晚高峰可以制造压力，深夜别下暴雨；\
连续亏损时放宽佣金，连续爆单时收紧；每次只改 1-2 个参数。";

// ───────────────────────── 请求 ─────────────────────────

/// 一轮烽火台请求的载荷。**请求侧只读**，不带任何可变世界。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldRequest {
    pub v: u8,
    pub world: WorldSnap,
    /// 最近几次世界操作，供它「别反复横跳」。
    #[serde(default)]
    pub history: Vec<String>,
    pub goal: String,
}

/// 剧本里的下一场天气变化。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WxNext {
    /// 生效时刻（当日模拟分钟）。
    pub at: u32,
    /// 天气短码。
    pub to: String,
}

/// 喂给烽火台的世界快照（省 token 版）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldSnap {
    pub day: u32,
    pub min: u32,
    /// 当前**对外可见**的天气（可能是烽火台的覆盖）。
    pub weather: String,
    pub commission: f32,
    pub orders: usize,
    pub delivered: u64,
    pub expired: u64,
    pub late: u64,
    pub backlog: usize,
    pub active_riders: usize,
    /// `weather` 的来源：`script` = 剧本，`director` = 烽火台下的限时覆盖。
    #[serde(default = "default_wx_src")]
    pub wx_src: String,
    /// 覆盖还剩多少分钟。`null` = 没有覆盖，或这条覆盖不过期。
    #[serde(default)]
    pub wx_left: Option<u64>,
    /// 剧本的下一场变化。`null` = 今天不会再变。
    #[serde(default)]
    pub wx_next: Option<WxNext>,
}

fn default_wx_src() -> String {
    "script".to_string()
}

impl WorldSnap {
    fn of(world: &World) -> Self {
        let ov = world.weather_override.as_ref();
        WorldSnap {
            day: world.day,
            min: world.sim_minute,
            weather: world.weather.code().to_string(),
            commission: world.cfg.commission_rate,
            orders: world.orders.len(),
            delivered: world.stats.orders_delivered,
            expired: world.stats.orders_expired,
            late: world.stats.orders_late,
            backlog: world.pending_orders().len(),
            active_riders: world.riders.iter().filter(|r| r.online).count(),
            wx_src: if ov.is_some() { "director" } else { "script" }.to_string(),
            // `u64::MAX` = 不过期，报 null 而不是一个天文数字。
            wx_left: ov.and_then(|o| {
                (o.expires_at != u64::MAX).then(|| o.expires_at.saturating_sub(world.elapsed_min))
            }),
            wx_next: world
                .weather_script
                .get(world.script_cursor)
                .map(|e| WxNext {
                    at: e.at,
                    to: e.to.code().to_string(),
                }),
        }
    }
}

impl WorldRequest {
    pub fn build(world: &World, history: &[String]) -> Self {
        // 历史只带最近 6 条，多了就成流水账了
        let history = history
            .iter()
            .rev()
            .take(6)
            .rev()
            .cloned()
            .collect::<Vec<_>>();
        WorldRequest {
            v: super::protocol::PROTOCOL_VERSION,
            world: WorldSnap::of(world),
            history,
            goal: GLOBAL_GOAL.to_string(),
        }
    }
}

// ───────────────────────── 响应 ─────────────────────────

/// 烽火台的回包。`world_ops` 故意收成 `Value`：**逐条解析**，
/// 某一条坏掉不影响其它条（总线上一个未知 op 不该整轮作废）。
#[derive(Debug, Clone, Deserialize)]
pub struct WorldResponse {
    #[serde(default)]
    pub v: u8,
    #[serde(default)]
    pub world_ops: Vec<serde_json::Value>,
}

/// 烽火台能做的三种世界操作。
///
/// 注意**没有派单、没有改骑手**——那是站长与骑手层的权力。
/// （设计稿里的 `spawn_event` 等世界侧效果实现后再加进来。）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum WorldOp {
    /// 调天气。`v` 是天气短码（见 [`Weather::from_code`]）。
    SetWeather {
        #[serde(default)]
        v: String,
        #[serde(default)]
        why: String,
    },
    /// 调平台佣金率。`v` 是 0.0~1.0 之间的小数。
    SetCommission {
        #[serde(default)]
        v: f32,
        #[serde(default)]
        why: String,
    },
    /// 给直播间留一句话。**只影响气氛，不改任何数值**。
    Narrate {
        #[serde(default)]
        text: String,
    },
}

/// 通过全部四级校验、可以直接落地的世界操作。
///
/// 枚举变体故意和 [`WorldOp`] 一一对应——`apply_world_ops` 拿到它时
/// 不再需要做任何判断，能落地的都已经合法。
#[derive(Debug, Clone, PartialEq)]
pub enum ValidatedWorldOp {
    SetWeather { weather: Weather, why: String },
    SetCommission { rate: f32, why: String },
    Narrate(String),
}

/// 一轮世界操作的校验结论。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorldVerdict {
    pub ops: Vec<ValidatedWorldOp>,
    /// 被丢弃的条目数（结构 / 引用 / 语义 / 越权）。
    pub dropped: usize,
    /// 整轮被打回的原因。`Some` 时 `ops` 必为空。
    pub rejected: Option<String>,
}

impl WorldVerdict {
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// 这轮里有没有会改世界数值的操作（叙事不算）。
    pub fn has_world_change(&self) -> bool {
        self.ops
            .iter()
            .any(|o| !matches!(o, ValidatedWorldOp::Narrate(_)))
    }
}

/// **四级校验流水线**。和骑手层的 `validate_decision` 同一套思路：
/// 坏输入不 panic、不整轮放弃，只丢坏的那条。
pub fn validate_world_ops(prev: Option<Weather>, resp: &WorldResponse) -> WorldVerdict {
    let mut out = WorldVerdict::default();

    // ① 结构
    if resp.v != super::protocol::PROTOCOL_VERSION {
        out.rejected = Some(format!(
            "协议版本不符：期望 {}，收到 {}",
            super::protocol::PROTOCOL_VERSION,
            resp.v
        ));
        return out;
    }
    if resp.world_ops.is_empty() {
        out.rejected = Some("没有世界操作".into());
        return out;
    }
    if resp.world_ops.len() > 4 {
        out.rejected = Some(format!("一次给太多操作（{} 条）", resp.world_ops.len()));
        return out;
    }

    // ②③④ 逐条解析 → 引用 → 语义去重 → 钳制
    let mut seen_weather = false;
    let mut seen_commission = false;
    for raw in &resp.world_ops {
        let Ok(op) = serde_json::from_value::<WorldOp>(raw.clone()) else {
            out.dropped += 1;
            continue;
        };
        match op {
            WorldOp::SetWeather { v, why } => {
                if seen_weather {
                    out.dropped += 1; // ③ 同类去重
                    continue;
                }
                let Some(weather) = Weather::from_code(&v) else {
                    out.dropped += 1; // ② 认不出的天气码
                    continue;
                };
                if Some(weather) == prev {
                    out.dropped += 1; // ③ 和现在一样，改了等于没改
                    continue;
                }
                seen_weather = true;
                out.ops.push(ValidatedWorldOp::SetWeather {
                    weather,
                    why: clip(&why, MAX_WHY_CHARS), // ④ 截断
                });
            }
            WorldOp::SetCommission { v, why } => {
                if seen_commission {
                    out.dropped += 1;
                    continue;
                }
                if !v.is_finite() || v <= 0.0 {
                    out.dropped += 1; // ② 非法数值
                    continue;
                }
                seen_commission = true;
                out.ops.push(ValidatedWorldOp::SetCommission {
                    rate: v.min(MAX_COMMISSION), // ④ 封顶
                    why: clip(&why, MAX_WHY_CHARS),
                });
            }
            WorldOp::Narrate { text } => {
                let text = clip(&text, MAX_NARRATE_CHARS); // ④ 截断
                if text.is_empty() {
                    out.dropped += 1; // ② 空话不算
                    continue;
                }
                out.ops.push(ValidatedWorldOp::Narrate(text));
            }
        }
    }

    if out.ops.is_empty() {
        out.rejected = Some(format!("{} 条操作全无效", out.dropped));
    }
    out
}

/// 按字符（不是字节）截断——中文一个字符 3 字节，按字节截会切碎。
fn clip(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        return t.to_string();
    }
    t.chars().take(max).collect()
}

// ───────────────────────── 提示词 ─────────────────────────

pub fn system_prompt() -> &'static str {
    r#"你是外卖直播间的"世界导演"。你看的是全局参数，不是某个人某一单。
只能输出 JSON，不要解释、不要 Markdown。

格式：
{"v":1,"world_ops":[
  {"op":"set_weather","v":"hr","why":"午高峰下暴雨制造压力"},
  {"op":"set_commission","v":0.12,"why":"连续亏损放宽佣金"},
  {"op":"narrate","text":"天说变就变，一场暴雨压了下来"}
]}

规则：
- 一轮最多 2 条世界操作；不确定就只 narrate。
- 天气码：c 晴 / o 阴 / f 雾 / r 小雨 / hr 暴雨 / w 大风 / s 雪。
- set_weather/set_commission 每轮各最多一次；佣金 ≤ 0.30。
- narrate 是一句人话（≤60 字），像直播间的旁白，不要数字堆砌。
- 不要重复 history 里刚做过的调整。
- weather 是当前对外生效的天气；wx_src 说明它来自剧本还是你此前的覆盖，
  wx_left 是你这条覆盖还剩几分钟（null = 没覆盖或不过期），wx_next 是剧本下一场变化。
  覆盖到期会自动交还剧本，wx_left 快见底时别反复重下同一场天气。"#
}

pub fn user_prompt(req: &WorldRequest) -> String {
    let w = &req.world;
    let mut s = format!(
        "第{}天 {:02}:{:02}｜天气 {}｜佣金 {:.0}%｜待接 {} 单｜已送 {}｜超时 {}｜超时未接 {}｜在线骑手 {}\n目标：{}\n",
        w.day,
        w.min / 60,
        w.min % 60,
        w.weather,
        w.commission * 100.0,
        w.backlog,
        w.delivered,
        w.late,
        w.expired,
        w.active_riders,
        req.goal
    );
    if req.history.is_empty() {
        s.push_str("历史：（无）");
    } else {
        s.push_str("最近操作（新→旧）：");
        for h in req.history.iter().rev() {
            s.push_str(&format!("\n- {h}"));
        }
    }
    s
}

// ───────────────────────── 大脑 ─────────────────────────

/// 烽火台大脑。只持有客户端——节流 / 预算全在 [`super::orchestrator::AiOrchestrator`]。
#[derive(Default)]
pub struct DirectorBrain {
    client: Option<LlmClient>,
    last_error: String,
}

/// 一轮已占坑、等着打 HTTP 的烽火台请求。
pub struct PreparedWorldPlan {
    pub client: LlmClient,
    req: WorldRequest,
    started: Instant,
}

impl std::fmt::Debug for PreparedWorldPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedWorldPlan")
            .field("model", &self.client.model())
            .finish()
    }
}

impl PreparedWorldPlan {
    pub fn model(&self) -> &str {
        self.client.model()
    }

    pub fn request_payload(&self) -> &WorldRequest {
        &self.req
    }

    /// 打一次 HTTP。失败原因压成字符串，不做 `Result`——处理方式完全一样。
    pub async fn request(&self) -> WorldFetch {
        let system = system_prompt();
        let user = user_prompt(&self.req);
        match self.client.complete_json(system, &user).await {
            Ok(v) => WorldFetch::Json(v),
            Err(e) => WorldFetch::Failed(e.to_string()),
        }
    }
}

/// 一次烽火台请求的结局。
#[derive(Debug)]
pub enum WorldFetch {
    Json(serde_json::Value),
    Failed(String),
}

/// 烽火台一轮的结论，给控制端与日志用。
#[derive(Debug, Clone)]
pub struct DirectorOutcome {
    pub source: BrainSource,
    pub note: String,
    pub model: String,
    pub latency_ms: u64,
    pub ops: Vec<ValidatedWorldOp>,
    pub narrate: Option<String>,
    pub dropped: usize,
}

impl DirectorOutcome {
    fn skipped(source: BrainSource, note: impl Into<String>) -> Self {
        DirectorOutcome {
            source,
            note: note.into(),
            model: String::new(),
            latency_ms: 0,
            ops: Vec::new(),
            narrate: None,
            dropped: 0,
        }
    }
}

impl DirectorBrain {
    pub fn new(client: Option<LlmClient>) -> Self {
        DirectorBrain {
            client,
            last_error: String::new(),
        }
    }

    pub fn from_config(cfg: &LlmConfig) -> Self {
        Self::new(LlmClient::new(cfg.clone()).ok())
    }

    pub fn disabled() -> Self {
        Self::new(None)
    }

    pub fn is_enabled(&self) -> bool {
        self.client.is_some()
    }

    pub fn last_error(&self) -> &str {
        &self.last_error
    }

    /// **阶段一**：组装请求并占坑。HTTP 由返回值的 `request()` 在后台跑。
    pub fn prepare(
        &self,
        world: &World,
        history: &[String],
    ) -> Result<PreparedWorldPlan, DirectorOutcome> {
        let Some(client) = self.client.clone() else {
            return Err(DirectorOutcome::skipped(
                BrainSource::Disabled,
                "未配置 LLM，世界参数保持脚本驱动",
            ));
        };
        Ok(PreparedWorldPlan {
            client,
            req: WorldRequest::build(world, history),
            started: Instant::now(),
        })
    }

    /// **阶段二**：把 HTTP 结果交回来，得到校验过的世界操作。
    ///
    /// 故意要求传 `*现在*` 的世界：校验要针对当下天气（这几秒里脚本可能已经换过天了）。
    pub fn finish(
        &mut self,
        prepared: PreparedWorldPlan,
        fetched: WorldFetch,
        world: &World,
    ) -> DirectorOutcome {
        let latency_ms = prepared.started.elapsed().as_millis() as u64;
        let model = prepared.client.model().to_string();

        let skip = |source: BrainSource, note: String, me: &mut Self| {
            me.last_error = note.clone();
            DirectorOutcome {
                source,
                note,
                model: model.clone(),
                latency_ms,
                ops: Vec::new(),
                narrate: None,
                dropped: 0,
            }
        };

        let value = match fetched {
            WorldFetch::Json(v) => v,
            WorldFetch::Failed(e) => {
                let note = format!("LLM 失败，世界参数维持原状：{e}");
                return skip(BrainSource::Fallback, note, self);
            }
        };

        let resp: WorldResponse = match serde_json::from_value(value) {
            Ok(r) => r,
            Err(e) => {
                let note = format!("LLM 输出不符合世界协议：{e}");
                return skip(BrainSource::Fallback, note, self);
            }
        };

        let verdict = validate_world_ops(Some(world.weather), &resp);
        if verdict.is_empty() {
            let why = verdict.rejected.unwrap_or_else(|| "无有效世界操作".into());
            let note = format!("{why}，世界参数维持原状");
            return skip(BrainSource::Fallback, note, self);
        }

        let narrate = verdict.ops.iter().find_map(|o| match o {
            ValidatedWorldOp::Narrate(t) => Some(t.clone()),
            _ => None,
        });
        let changes = verdict
            .ops
            .iter()
            .filter(|o| !matches!(o, ValidatedWorldOp::Narrate(_)))
            .count();
        let note = format!(
            "烽火台：{} 条世界操作{}、{} 条叙事，丢弃 {} 条",
            changes,
            if changes == 0 {
                "（只有旁白）"
            } else {
                ""
            },
            verdict.ops.len() - changes,
            verdict.dropped
        );
        DirectorOutcome {
            source: BrainSource::Llm,
            note,
            model,
            latency_ms,
            dropped: verdict.dropped,
            ops: verdict.ops,
            narrate,
        }
    }

    /// 同步拿一次结论。**无论发生什么都不会返回 Err**——最差也是 `Fallback`。
    pub async fn plan(&mut self, world: &World, history: &[String]) -> DirectorOutcome {
        let prepared = match self.prepare(world, history) {
            Ok(p) => p,
            Err(outcome) => return outcome,
        };
        let fetched = prepared.request().await;
        self.finish(prepared, fetched, world)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{config::SimConfig, world::World};

    fn world() -> World {
        World::new_for_test(SimConfig::default(), 3)
    }

    fn resp(v: u8, ops: Vec<serde_json::Value>) -> WorldResponse {
        WorldResponse { v, world_ops: ops }
    }

    #[test]
    fn request_snapshot_reflects_the_world() {
        let mut w = world();
        w.day = 2;
        w.sim_minute = 750;
        w.weather = Weather::Clear;
        let req = WorldRequest::build(&w, &[]);
        assert_eq!(req.world.day, 2);
        assert_eq!(req.world.min, 750);
        assert_eq!(req.world.weather, "c");
        assert_eq!(req.v, crate::llm::protocol::PROTOCOL_VERSION);
    }

    /// 烽火台必须知道「现在这场天气是谁定的、还剩多久、剧本下一步是什么」，
    /// 否则它会和自己刚下的覆盖打架。
    #[test]
    fn world_snap_reports_weather_override_state() {
        use crate::sim::weather::WeatherEvent;

        let mut w = world();
        w.weather_script = vec![WeatherEvent {
            at: 600,
            to: Weather::HeavyRain,
        }];
        w.elapsed_min = 100;
        w.sim_minute = 100;

        let plain = WorldRequest::build(&w, &[]);
        assert_eq!(plain.world.wx_src, "script", "没覆盖时天气来自剧本");
        assert!(plain.world.wx_left.is_none());
        assert_eq!(
            plain.world.wx_next,
            Some(WxNext {
                at: 600,
                to: "hr".into()
            }),
            "下一场剧本变化要告诉它"
        );

        w.set_weather_override(Weather::Fog, "测试");
        let req = WorldRequest::build(&w, &[]);
        assert_eq!(req.world.wx_src, "director");
        assert_eq!(req.world.weather, "f", "对外生效的是覆盖");
        assert_eq!(
            req.world.wx_left,
            Some(w.cfg.director_weather_ttl_min as u64),
            "剩余分钟按 TTL 算"
        );

        // TTL = 0 表示不过期：报 null，而不是一个天文数字。
        w.cfg.director_weather_ttl_min = 0;
        w.set_weather_override(Weather::Snow, "测试");
        assert!(WorldRequest::build(&w, &[]).world.wx_left.is_none());
    }

    #[test]
    fn history_is_capped_and_newest_last() {
        let w = world();
        let hist: Vec<String> = (1..=10).map(|i| format!("op{i}")).collect();
        let req = WorldRequest::build(&w, &hist);
        assert_eq!(req.history.len(), 6);
        assert_eq!(req.history.first().unwrap(), "op5");
        assert_eq!(req.history.last().unwrap(), "op10");
    }

    #[test]
    fn bad_version_rejects_the_whole_batch() {
        let v = validate_world_ops(
            Some(Weather::Clear),
            &resp(99, vec![serde_json::json!({"op":"narrate","text":"喂"})]),
        );
        assert!(v.is_empty());
        assert!(v.rejected.is_some());
    }

    #[test]
    fn unknown_op_is_dropped_not_fatal() {
        let v = validate_world_ops(
            Some(Weather::Clear),
            &resp(
                1,
                vec![
                    serde_json::json!({"op":"spawn_event","kind":"traffic_jam"}),
                    serde_json::json!({"op":"narrate","text":"路口堵死了"}),
                ],
            ),
        );
        assert_eq!(v.dropped, 1, "未知 op 丢一条");
        assert_eq!(v.ops.len(), 1, "叙事不受影响");
        assert_eq!(v.ops[0], ValidatedWorldOp::Narrate("路口堵死了".into()));
    }

    #[test]
    fn weather_code_must_be_known() {
        let v = validate_world_ops(
            Some(Weather::Clear),
            &resp(
                1,
                vec![
                    serde_json::json!({"op":"set_weather","v":"tornado","why":"x"}),
                    serde_json::json!({"op":"set_weather","v":"hr","why":"暴雨"}),
                ],
            ),
        );
        assert_eq!(v.dropped, 1);
        assert_eq!(v.ops.len(), 1);
        assert!(matches!(
            v.ops[0],
            ValidatedWorldOp::SetWeather {
                weather: Weather::HeavyRain,
                ..
            }
        ));
    }

    #[test]
    fn weather_dedup_keeps_only_the_first() {
        let v = validate_world_ops(
            Some(Weather::Clear),
            &resp(
                1,
                vec![
                    serde_json::json!({"op":"set_weather","v":"f","why":"先雾"}),
                    serde_json::json!({"op":"set_weather","v":"hr","why":"后雨"}),
                ],
            ),
        );
        assert_eq!(v.dropped, 1);
        assert_eq!(v.ops.len(), 1);
        assert!(matches!(
            v.ops[0],
            ValidatedWorldOp::SetWeather {
                weather: Weather::Fog,
                ..
            }
        ));
    }

    #[test]
    fn redundant_weather_change_is_dropped() {
        // 现在就是暴雨，再报暴雨等于没改
        let v = validate_world_ops(
            Some(Weather::HeavyRain),
            &resp(
                1,
                vec![serde_json::json!({"op":"set_weather","v":"hr","why":"还是暴雨"})],
            ),
        );
        assert!(v.is_empty());
        assert_eq!(v.dropped, 1);
    }

    #[test]
    fn commission_is_clamped_to_the_hard_ceiling() {
        let v = validate_world_ops(
            Some(Weather::Clear),
            &resp(
                1,
                vec![serde_json::json!({"op":"set_commission","v":0.9,"why":"狠一点"})],
            ),
        );
        assert_eq!(v.ops.len(), 1);
        match &v.ops[0] {
            ValidatedWorldOp::SetCommission { rate, .. } => {
                assert!((*rate - MAX_COMMISSION).abs() < 1e-6, "必须削到封顶");
            }
            other => panic!("期望 set_commission，得到 {other:?}"),
        }
    }

    #[test]
    fn negative_or_nan_commission_is_dropped() {
        let v = validate_world_ops(
            Some(Weather::Clear),
            &resp(
                1,
                vec![
                    serde_json::json!({"op":"set_commission","v":-0.1,"why":"坏"}),
                    serde_json::json!({"op":"narrate","text":"只剩旁白"}),
                ],
            ),
        );
        assert_eq!(v.dropped, 1);
        assert_eq!(v.ops.len(), 1);
        assert!(
            v.ops
                .iter()
                .all(|o| !matches!(o, ValidatedWorldOp::SetCommission { .. }))
        );
    }

    #[test]
    fn narrate_is_truncated_by_chars_not_bytes() {
        let long = "暴".repeat(200);
        let v = validate_world_ops(
            Some(Weather::Clear),
            &resp(1, vec![serde_json::json!({"op":"narrate","text":long})]),
        );
        match &v.ops[0] {
            ValidatedWorldOp::Narrate(t) => {
                assert_eq!(t.chars().count(), MAX_NARRATE_CHARS);
            }
            other => panic!("期望 narrate，得到 {other:?}"),
        }
    }

    #[test]
    fn empty_narrate_is_dropped() {
        let v = validate_world_ops(
            Some(Weather::Clear),
            &resp(1, vec![serde_json::json!({"op":"narrate","text":"   "})]),
        );
        assert!(v.is_empty());
        assert_eq!(v.dropped, 1);
    }

    #[test]
    fn too_many_ops_are_rejected_outright() {
        let ops: Vec<_> = (0..5)
            .map(|i| serde_json::json!({"op":"narrate","text":format!("旁白{i}")}))
            .collect();
        let v = validate_world_ops(Some(Weather::Clear), &resp(1, ops));
        assert!(v.is_empty());
        assert!(v.rejected.unwrap().contains("太多"));
    }

    #[test]
    fn has_world_change_ignores_pure_narration() {
        let mut v = validate_world_ops(
            Some(Weather::Clear),
            &resp(
                1,
                vec![serde_json::json!({"op":"narrate","text":"风平浪静"})],
            ),
        );
        assert!(!v.has_world_change());
        v.ops.push(ValidatedWorldOp::SetCommission {
            rate: 0.15,
            why: "试试".into(),
        });
        assert!(v.has_world_change());
    }

    #[test]
    fn disabled_brain_skips_cleanly() {
        let w = world();
        let b = DirectorBrain::disabled();
        assert!(!b.is_enabled());
        let err = b.prepare(&w, &[]).unwrap_err();
        assert_eq!(err.source, BrainSource::Disabled);
        assert!(err.ops.is_empty());
    }

    #[test]
    fn finish_turns_a_response_into_validated_ops() {
        let mut w = world();
        w.weather = Weather::Clear;
        let mut b = DirectorBrain::disabled();
        // 直接构造 PreparedWorldPlan 走 finish（跳过 HTTP）
        let prepared = PreparedWorldPlan {
            client: LlmClient::new(LlmConfig::new("test-key")).unwrap(),
            req: WorldRequest::build(&w, &[]),
            started: Instant::now(),
        };
        let fetched = WorldFetch::Json(serde_json::json!({
            "v": 1,
            "world_ops": [
                {"op":"set_weather","v":"hr","why":"午高峰暴雨"},
                {"op":"narrate","text":"天说变就变"}
            ]
        }));
        let out = b.finish(prepared, fetched, &w);
        assert_eq!(out.source, BrainSource::Llm);
        assert_eq!(out.ops.len(), 2);
        assert_eq!(out.narrate.as_deref(), Some("天说变就变"));
        assert!(out.note.contains("世界操作"));
    }

    #[test]
    fn finish_reports_failure_as_fallback() {
        let w = world();
        let mut b = DirectorBrain::disabled();
        let prepared = PreparedWorldPlan {
            client: LlmClient::new(LlmConfig::new("test-key")).unwrap(),
            req: WorldRequest::build(&w, &[]),
            started: Instant::now(),
        };
        let out = b.finish(prepared, WorldFetch::Failed("timeout".into()), &w);
        assert_eq!(out.source, BrainSource::Fallback);
        assert!(out.ops.is_empty());
        assert!(b.last_error().contains("timeout"));
    }
}
