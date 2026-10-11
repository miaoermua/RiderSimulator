//! AI 托管与无缝让位（《三层AI架构设计》§6）。
//!
//! 「开服即玩」的地基：玩家不碰控制台，世界由 AI 自己跑；玩家手机扫码接入，
//! AI 立刻让位。这中间的判定**只有这一条规则**，所以它值得单独一个文件、
//! 单独一堆单测——判错了就是「AI 跟玩家抢操作」或者「玩家掉线被扣一堆钱」。
//!
//! ## 为什么在 `human/` 而不是 `sim/`
//!
//! 本文件问的是「**现在谁是真人**」。`sim/` 其余代码都不需要知道这件事，
//! 而它依赖的「谁连着 WS」本来就是外圈的事实——所以依赖方向是 `human → sim`，
//! 落到 `sim` 手上的只有结论（见 [`crate::sim::tick::TickInput::hosted_humans`]）。

use serde::{Deserialize, Serialize};

use crate::sim::{
    RiderId,
    rider::{Rider, RiderKind},
    world::World,
};

/// 托管模式。控制端可选。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HostMode {
    /// 只托管模拟骑手。真人骑手掉线就停在原地（默认）。
    #[default]
    SimulatedOnly,
    /// 全托管：真人骑手掉线后 AI 自动接管，玩家回来时无缝交还。
    AutoPilot,
    /// 全停机：不托管任何骑手，AI 全关（纯手动模式）。
    Off,
}

impl HostMode {
    pub const ALL: [HostMode; 3] = [HostMode::SimulatedOnly, HostMode::AutoPilot, HostMode::Off];

    pub fn key(self) -> &'static str {
        match self {
            HostMode::SimulatedOnly => "simulated_only",
            HostMode::AutoPilot => "auto_pilot",
            HostMode::Off => "off",
        }
    }

    pub fn from_key(s: &str) -> Option<Self> {
        match s {
            "simulated_only" => Some(HostMode::SimulatedOnly),
            "auto_pilot" | "autopilot" => Some(HostMode::AutoPilot),
            "off" => Some(HostMode::Off),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            HostMode::SimulatedOnly => "仅模拟骑手",
            HostMode::AutoPilot => "自动托管",
            HostMode::Off => "全关（纯手动）",
        }
    }
}

/// **AI 是否该托管这个骑手？** 判定顺序即优先级——真人意愿永远高于 AI。
///
/// 这条规则是整个接管机制的唯一实现，所以它必须一眼看懂：
///
/// ```text
/// Off                                     → 谁都不托管
/// Simulated                               → 永远托管
/// Human + 在线                            → 绝对不托管（玩家在操作）
/// Human + 掉线 + SimulatedOnly             → 不托管（等他回来）
/// Human + 掉线 + AutoPilot                 → 托管（免得他卡死被扣钱）
/// ```
///
/// `has_live_conn` 由连接层（hub）给，**不缓存**：玩家断线要立刻让 AI 接手，
/// 缓存一拍就会出现「玩家已经走了，骑手还杵在原地」。
pub fn ai_should_drive(rider: &Rider, has_live_conn: bool, mode: HostMode) -> bool {
    match mode {
        HostMode::Off => false,

        HostMode::SimulatedOnly | HostMode::AutoPilot => match rider.kind {
            RiderKind::Simulated => true,

            // 真人骑手在线 → AI 让位。**这条最重要**：玩家正在操作，AI 绝不能插手。
            RiderKind::Human if has_live_conn => false,

            // 真人骑手掉线：SimulatedOnly 不动等他回来；AutoPilot 接管，
            // 免得他手持订单超时、扣钱、掉评分——回来发现「我什么都没干就被扣钱」。
            RiderKind::Human => mode == HostMode::AutoPilot,
        },
    }
}

/// **AI 现在能代谁派单？** 这是「骑手层」唯一该用的空闲名单。
///
/// 和 [`World::idle_riders`] 的区别有两处，两处都是托管机制的要点：
/// - 在线的真人**被排除**——玩家在操作，AI 必须让位（§6.3「骑手 AI 停止输出决策」）。
/// - 掉线的真人只有在 `AutoPilot` 下才**被纳入**——否则单会派给他然后卡死在那儿。
///
/// `is_live` 由连接层（hub）给，**不缓存**：玩家断线要立刻让 AI 接手。
///
/// 名单的**算**只有一份，在 [`crate::sim::tick::ai_roster`]；这份是为了给
/// 「还没进 tick」的场合（拼 LLM 提示词）用。两边条件同源，不会漂。
pub fn ai_idle_riders(
    world: &World,
    mode: HostMode,
    is_live: impl Fn(RiderId) -> bool,
) -> Vec<RiderId> {
    world
        .riders
        .iter()
        .filter(|r| ai_should_drive(r, is_live(r.id), mode))
        .filter(|r| r.is_free(world.elapsed_min, &world.cfg))
        .map(|r| r.id)
        .collect()
}

/// 这一 tick 该交给 AI 代管的**真人**。产物直接喂给
/// [`TickInput::hosted_humans`]。
///
/// 只挑真人：模拟骑手天生归 AI 管，不用外圈再报一遍。
///
/// [`TickInput::hosted_humans`]: crate::sim::tick::TickInput::hosted_humans
pub fn hosted_humans(
    world: &World,
    mode: HostMode,
    is_live: impl Fn(RiderId) -> bool,
) -> Vec<RiderId> {
    world
        .riders
        .iter()
        .filter(|r| r.kind == RiderKind::Human)
        .filter(|r| ai_should_drive(r, is_live(r.id), mode))
        .map(|r| r.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{config::SimConfig, geo::LatLng, world::World};

    fn rider(kind: RiderKind) -> Rider {
        Rider::new(
            1,
            kind,
            "张三",
            LatLng {
                lat: 30.5,
                lng: 114.4,
            },
            &SimConfig::default(),
        )
    }

    #[test]
    fn simulated_riders_are_always_driven() {
        let sim = rider(RiderKind::Simulated);
        assert!(ai_should_drive(&sim, true, HostMode::SimulatedOnly));
        assert!(ai_should_drive(&sim, false, HostMode::SimulatedOnly));
        assert!(ai_should_drive(&sim, true, HostMode::AutoPilot));
        assert!(ai_should_drive(&sim, false, HostMode::AutoPilot));
    }

    #[test]
    fn online_human_is_never_driven() {
        let human = rider(RiderKind::Human);
        // 三种托管模式下，只要人在线，AI 都必须让位
        for mode in HostMode::ALL {
            assert!(
                !ai_should_drive(&human, true, mode),
                "玩家在线时 {mode:?} 不该插手"
            );
        }
    }

    #[test]
    fn offline_human_follows_the_mode() {
        let human = rider(RiderKind::Human);
        assert!(!ai_should_drive(&human, false, HostMode::SimulatedOnly));
        assert!(ai_should_drive(&human, false, HostMode::AutoPilot));
        assert!(!ai_should_drive(&human, false, HostMode::Off));
    }

    #[test]
    fn off_mode_drives_nobody() {
        let world = World::new_for_test(SimConfig::default(), 1);
        for r in &world.riders {
            assert!(!ai_should_drive(r, false, HostMode::Off));
        }
    }

    #[test]
    fn host_mode_keys_round_trip() {
        for m in HostMode::ALL {
            assert_eq!(HostMode::from_key(m.key()), Some(m));
        }
        assert_eq!(HostMode::from_key("nonsense"), None);
    }

    #[test]
    fn online_human_is_not_in_the_ai_roster() {
        let mut world = World::new_for_test(SimConfig::default(), 7);
        let human = world.spawn_human_rider("张三");
        world.rider_mut(human).unwrap().online = true;

        let roster = ai_idle_riders(&world, HostMode::AutoPilot, |id| id == human);
        assert!(!roster.contains(&human), "玩家自己在线，AI 不许给他派单");
        // 模拟骑手照常在里面
        assert!(
            world
                .riders
                .iter()
                .filter(|r| r.kind == RiderKind::Simulated)
                .all(|r| roster.contains(&r.id))
        );
    }

    #[test]
    fn autopilot_takes_over_an_offline_human_but_simulated_only_waits() {
        let mut world = World::new_for_test(SimConfig::default(), 7);
        let human = world.spawn_human_rider("张三");
        assert!(!world.rider(human).unwrap().online, "真人默认离线");

        let waiting = ai_idle_riders(&world, HostMode::SimulatedOnly, |_| false);
        assert!(!waiting.contains(&human), "SimulatedOnly 该等他回来");

        let hosted = ai_idle_riders(&world, HostMode::AutoPilot, |_| false);
        assert!(hosted.contains(&human), "AutoPilot 该接过他的单");

        let off = ai_idle_riders(&world, HostMode::Off, |_| false);
        assert!(off.is_empty(), "全停机谁都不派");
    }

    /// `hosted_humans` 只挑真人：模拟骑手天生归 AI，不该混进这份名单。
    #[test]
    fn hosted_humans_lists_only_humans() {
        let mut world = World::new_for_test(SimConfig::default(), 7);
        let human = world.spawn_human_rider("张三");
        assert!(!world.rider(human).unwrap().online, "真人默认离线");
        let sim_count = world
            .riders
            .iter()
            .filter(|r| r.kind == RiderKind::Simulated)
            .count();
        assert!(sim_count > 0, "前提：默认世界里得有模拟骑手");

        let hosted = hosted_humans(&world, HostMode::AutoPilot, |_| false);
        assert_eq!(hosted, vec![human], "AutoPilot 下掉线真人该被交出去");

        let waiting = hosted_humans(&world, HostMode::SimulatedOnly, |_| false);
        assert!(waiting.is_empty(), "SimulatedOnly 谁都不交");
    }
}
