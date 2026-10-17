//! 平台活动：跑够 N 单给奖金。
//!
//! ## 为什么进度记在骑手上
//!
//! 「活动开启后完成的单数」是**每人一份**的状态：活动定义是平台级的（放在
//! [`World::activities`]），进度是骑手级的（放在 [`Rider::activities`]）。
//! 放在一起会变成「全服共用一个计数器」，第一个跑够的人把所有人的奖都领了。
//!
//! ## 只发一次
//!
//! 达成那一刻当场入账（`cash_cents` 与 `gross_cents` 都加），并把
//! `rewarded` 钉住。之后继续跑单还会涨 `completed`，但不会再发第二笔——
//! 活动是「冲单奖」，不是「每 N 单返一次」。
//!
//! ## 从零开始
//!
//! 活动一建立，所有人的进度就是 0（第一次完成单时才建 [`ActivityProgress`]），
//! 开启前的单不追溯。这是 2026-10-17 定下的口径：对老骑手与新骑手一样公平，
//! 也免得「刚建完活动就有人立刻达成」。

use serde::{Deserialize, Serialize};

use super::RiderId;
use super::event::SimEvent;
use super::rider::{LedgerEntry, Rider};
use super::world::World;

/// 一个平台活动。由烽火台创建，可随时上下架。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformActivity {
    pub id: u32,
    pub title: String,
    /// 目标单数（活动期间完成多少单算达成）。
    pub target_orders: u32,
    /// 奖金（分）。达成后一次性打入今日到手。
    pub reward_cents: i64,
    /// 是否在进行中。下架后不再累计，也不发奖；已发的不收回。
    pub active: bool,
    /// 建活动时的模拟分钟（自开局起算），只用于展示。
    pub created_at_min: u64,
}

/// 某个骑手在某个活动上的进度。
///
/// `serde(default)` 是给老存档的：没有这个字段就等于没参加过任何活动。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityProgress {
    pub activity_id: u32,
    /// 活动期间完成的单数。
    pub completed: u32,
    /// 奖金是否已发。发过就不再发。
    pub rewarded: bool,
}

impl ActivityProgress {
    /// 对外视图。手机端只关心「跑了几单、还能不能领」。
    pub fn view(&self) -> ActivityProgressView {
        ActivityProgressView {
            activity_id: self.activity_id,
            completed: self.completed,
            rewarded: self.rewarded,
        }
    }
}

/// 下发给手机端的进度。只发给本人（见 `RiderBrief::own`）。
#[derive(Debug, Clone, Serialize)]
pub struct ActivityProgressView {
    pub activity_id: u32,
    pub completed: u32,
    pub rewarded: bool,
}

/// 建一个活动并返回 id。参数合法性由调用方（HTTP 层）保证。
pub fn create(
    world: &mut World,
    title: impl Into<String>,
    target_orders: u32,
    reward_cents: i64,
) -> u32 {
    let id = world.next_activity_id;
    world.next_activity_id += 1;
    world.activities.push(PlatformActivity {
        id,
        title: title.into(),
        target_orders,
        reward_cents,
        active: true,
        created_at_min: world.elapsed_min,
    });
    id
}

/// 上架 / 下架。返回 `false` = 没这个活动。
pub fn set_active(world: &mut World, id: u32, active: bool) -> bool {
    let Some(a) = world.activities.iter_mut().find(|a| a.id == id) else {
        return false;
    };
    a.active = active;
    true
}

/// 删掉一个活动。返回 `false` = 没这个活动。
///
/// 进度不跟着删：骑手那份历史留着，重建成同一个 id 的概率不存在
/// （`next_activity_id` 只增不减），所以不会串进度。
pub fn remove(world: &mut World, id: u32) -> bool {
    let before = world.activities.len();
    world.activities.retain(|a| a.id != id);
    before != world.activities.len()
}

/// 记一单完成，并为达成的活动发奖。
///
/// 返回奖励事件（可能为空）。**在 `scheduler::deliver` 里结算完主收入之后调用**：
/// 这样活动奖金是「额外的一笔」，不会混进订单本身的抽水 / 罚款计算。
pub fn credit(world: &mut World, rider_id: RiderId) -> Vec<SimEvent> {
    let running: Vec<(u32, String, u32, i64)> = world
        .activities
        .iter()
        .filter(|a| a.active && a.target_orders > 0)
        .map(|a| (a.id, a.title.clone(), a.target_orders, a.reward_cents))
        .collect();
    if running.is_empty() {
        return Vec::new();
    }
    let now = world.elapsed_min;
    let Some(rider) = world.riders.iter_mut().find(|r| r.id == rider_id) else {
        return Vec::new();
    };

    let mut events = Vec::new();
    for (id, title, target, reward) in running {
        let idx = match rider.activities.iter().position(|p| p.activity_id == id) {
            Some(i) => i,
            None => {
                rider.activities.push(ActivityProgress {
                    activity_id: id,
                    completed: 0,
                    rewarded: false,
                });
                rider.activities.len() - 1
            }
        };
        let (completed, reached) = {
            let progress = &mut rider.activities[idx];
            progress.completed += 1;
            (
                progress.completed,
                !progress.rewarded && progress.completed >= target,
            )
        };
        if !reached {
            continue;
        }
        rider.activities[idx].rewarded = true;
        // 平台补贴：到手与毛收入都记，避免对账时「毛收入小于到手」。
        rider.cash_cents += reward;
        rider.gross_cents += reward;
        rider.push_ledger(LedgerEntry {
            at_min: now,
            order_id: None,
            kind: "activity_reward".into(),
            amount_cents: reward,
            reason: format!("活动奖励：{title}"),
        });
        events.push(SimEvent::ActivityReward {
            rider_id,
            activity_id: id,
            title,
            amount_cents: reward,
            completed,
            target_orders: target,
            takehome_cents: rider.takehome_cents(),
        });
    }
    // 没达成的单也要把进度推回去：活动页的进度条不该等到下次重连才动。
    // 放在循环外，一单只发一条，不按活动数量刷屏。
    events.push(SimEvent::ActivityProgressChanged {
        rider_id,
        activities: progress_views(rider),
    });
    events
}

/// 活动定义的对外视图（公开信息：所有人都能看到有什么活动）。
pub fn active_views(world: &World) -> Vec<&PlatformActivity> {
    world.activities.iter().filter(|a| a.active).collect()
}

/// 给某位骑手的进度视图，供 `RiderBrief::own` 使用。
pub fn progress_views(rider: &Rider) -> Vec<ActivityProgressView> {
    rider
        .activities
        .iter()
        .map(ActivityProgress::view)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::config::SimConfig;

    fn credit_n(world: &mut World, id: RiderId, n: usize) -> Vec<SimEvent> {
        let mut out = Vec::new();
        for _ in 0..n {
            out.extend(credit(world, id));
        }
        out
    }

    /// 只数「发钱」事件。每单都会带一条进度事件，不能拿事件总数当发奖次数。
    fn rewards(events: &[SimEvent]) -> usize {
        events
            .iter()
            .filter(|e| matches!(e, SimEvent::ActivityReward { .. }))
            .count()
    }

    fn progress_of(world: &World, activity_id: u32) -> ActivityProgressView {
        world.riders[0]
            .activities
            .iter()
            .find(|p| p.activity_id == activity_id)
            .map(ActivityProgress::view)
            .expect("进度留下了")
    }

    #[test]
    fn a_reward_is_paid_once_and_counts_from_zero() {
        let mut w = World::new_for_test(SimConfig::default(), 7);
        let rid = w.riders[0].id;
        let before = w.riders[0].cash_cents;

        let id = create(&mut w, "冲单奖", 3, 500);
        // 建活动之前跑的单不追溯：进度一开始是空的。
        assert!(w.riders[0].activities.is_empty());

        assert_eq!(rewards(&credit_n(&mut w, rid, 2)), 0);
        assert_eq!(w.riders[0].cash_cents, before, "没到目标不发钱");

        let events = credit(&mut w, rid);
        assert_eq!(rewards(&events), 1, "第 3 单达成，发一笔");
        assert_eq!(w.riders[0].cash_cents, before + 500);

        // 再跑单只涨进度，不再发第二笔。
        assert_eq!(rewards(&credit_n(&mut w, rid, 2)), 0);
        assert_eq!(w.riders[0].cash_cents, before + 500);

        let p = w.riders[0]
            .activities
            .iter()
            .find(|p| p.activity_id == id)
            .expect("进度留下了");
        assert_eq!(p.completed, 5);
        assert!(p.rewarded);
    }

    #[test]
    fn several_activities_track_separately() {
        let mut w = World::new_for_test(SimConfig::default(), 11);
        let rid = w.riders[0].id;
        let before = w.riders[0].cash_cents;

        let easy = create(&mut w, "小目标", 1, 100);
        let hard = create(&mut w, "大目标", 5, 2000);

        let events = credit(&mut w, rid);
        assert_eq!(rewards(&events), 1, "只有小目标当场达成");
        assert_eq!(w.riders[0].cash_cents, before + 100);

        assert!(progress_of(&w, easy).rewarded);
        assert_eq!(progress_of(&w, hard).completed, 1);
        assert!(!progress_of(&w, hard).rewarded);

        credit_n(&mut w, rid, 4);
        assert_eq!(progress_of(&w, hard).completed, 5);
        assert!(progress_of(&w, hard).rewarded);
        assert_eq!(w.riders[0].cash_cents, before + 100 + 2000);
    }

    #[test]
    fn an_inactive_activity_stops_counting() {
        let mut w = World::new_for_test(SimConfig::default(), 13);
        let rid = w.riders[0].id;
        let id = create(&mut w, "暂停的活动", 2, 300);

        assert!(set_active(&mut w, id, false));
        assert_eq!(rewards(&credit_n(&mut w, rid, 5)), 0);
        assert!(w.riders[0].activities.is_empty(), "下架后连进度都不记");
        assert!(!set_active(&mut w, 999, true));
    }
}
