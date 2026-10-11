//! 路线任务：骑手的一条**腿队列**。
//!
//! 为什么需要它：位置推进原先挂在「每单一条腿」上（`HeldOrder.phase` +
//! `remaining_min`），多单时会互相拉扯——同一 tick 内各单分别改写 `rider.pos`，
//! 最后遍历到的那单说了算，于是既有抖动、又有「瞬移」。
//!
//! 这里把「骑手现在去哪」收敛成**唯一**的当前腿（队首），位置只由它驱动；
//! `HeldOrder` 的段字段退化为这条腿的镜像，只用于对外展示。
//!
//! 不变式：**取餐腿永远排在送达腿前面**。凑单窗口里新并入的单，它的取餐腿
//! 要插到第一个送达腿之前，而不是简单追加到队尾——否则骑手会拎着半批餐去送。
//!
//! 本模块是纯逻辑：不碰 IO、不碰 `World`，随机性一概没有（排序是确定性贪心）。

use serde::{Deserialize, Serialize};

use super::{
    OrderId,
    geo::{LatLng, haversine_m},
};

/// 一条腿的用途。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegKind {
    /// 骑手 → 商家取餐。
    Pickup,
    /// 商家 → 顾客送达。
    Dropoff,
}

impl LegKind {
    pub fn label(self) -> &'static str {
        match self {
            LegKind::Pickup => "取餐",
            LegKind::Dropoff => "送达",
        }
    }
}

/// 路线上的一个目标点。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteLeg {
    pub order_id: OrderId,
    pub kind: LegKind,
    /// 目标坐标：取餐腿是商家，送达腿是顾客。
    pub pos: LatLng,
    /// 这段的**总时长**（分钟）。`remaining_min` 归零后靠它算进度。
    pub total_min: u32,
    /// 当前腿剩余分钟。
    pub remaining_min: u32,
}

impl RouteLeg {
    pub fn new(order_id: OrderId, kind: LegKind, pos: LatLng, total_min: u32) -> Self {
        let total = total_min.max(1);
        Self {
            order_id,
            kind,
            pos,
            total_min: total,
            remaining_min: total,
        }
    }

    /// 当前腿进度 0.0~1.0（1.0 = 已走完，等待出队）。
    pub fn progress(&self) -> f32 {
        let total = self.total_min.max(1) as f32;
        let done = (self.total_min - self.remaining_min.min(self.total_min)) as f32;
        (done / total).clamp(0.0, 1.0)
    }
}

/// 骑手的路线任务。**队首是当前腿**，走完出队。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RouteTask {
    legs: Vec<RouteLeg>,
}

impl RouteTask {
    pub fn new() -> Self {
        Self::default()
    }

    /// 用一条腿起一条路线（手上还没有路线时）。
    pub fn from_leg(leg: RouteLeg) -> Self {
        Self { legs: vec![leg] }
    }

    /// 起手一条取餐腿（刚接到第一单时）。
    pub fn from_pickup(order_id: OrderId, shop: LatLng, travel_min: u32) -> Self {
        Self::from_leg(RouteLeg::new(order_id, LegKind::Pickup, shop, travel_min))
    }

    pub fn is_empty(&self) -> bool {
        self.legs.is_empty()
    }

    pub fn len(&self) -> usize {
        self.legs.len()
    }

    /// 当前腿（队首）。`None` = 路线跑完了。
    pub fn current(&self) -> Option<&RouteLeg> {
        self.legs.first()
    }

    pub fn current_mut(&mut self) -> Option<&mut RouteLeg> {
        self.legs.first_mut()
    }

    /// 当前腿的目标点。位置推进与顺路评分都读它。
    pub fn current_pos(&self) -> Option<LatLng> {
        self.current().map(|l| l.pos)
    }

    pub fn legs(&self) -> &[RouteLeg] {
        &self.legs
    }

    /// 当前腿走完：出队，返回它。
    ///
    /// 调用方负责把这笔推进同步给对应的 `HeldOrder`（镜像字段）。
    pub fn pop_front(&mut self) -> Option<RouteLeg> {
        if self.legs.is_empty() {
            None
        } else {
            Some(self.legs.remove(0))
        }
    }

    /// 第一个送达腿的下标。`None` = 全是要取的餐。
    fn first_dropoff(&self) -> Option<usize> {
        self.legs.iter().position(|l| l.kind == LegKind::Dropoff)
    }

    /// 把一条腿插进队列，**并保持「取餐在前、送达在后」的不变式**。
    ///
    /// - 取餐腿：插到第一个送达腿之前（新单的餐一定要先取）。
    /// - 送达腿：追加到队尾（送达顺序由 [`Self::sort_dropoffs_nearest`] 决定）。
    pub fn push_leg(&mut self, leg: RouteLeg) {
        match leg.kind {
            LegKind::Dropoff => self.legs.push(leg),
            LegKind::Pickup => match self.first_dropoff() {
                Some(i) => self.legs.insert(i, leg),
                None => self.legs.push(leg),
            },
        }
    }

    /// 订单被取消 / 重投时，把它在路线上的腿全部摘掉。
    ///
    /// 返回是否真的动过路线——调用方据此决定要不要重排。
    pub fn remove_order(&mut self, order_id: OrderId) -> bool {
        let before = self.legs.len();
        self.legs.retain(|l| l.order_id != order_id);
        before != self.legs.len()
    }

    /// 骑手到了一处取餐点：把**同一地点**的所有取餐腿一次性出队。
    ///
    /// 同一家店的多单本来就在同一处取餐——人到店了，这批餐就都算「已到取餐点」。
    /// 否则第二单还得再空耗一遍到店时长，而人明明已经站在店里。
    ///
    /// 返回被摘掉的订单，调用方据此知道这趟到店一口气覆盖了哪几单。
    pub fn complete_pickups_at(&mut self, shop: LatLng) -> Vec<OrderId> {
        let mut done = Vec::new();
        self.legs.retain(|l| {
            let hit = l.kind == LegKind::Pickup && l.pos == shop;
            if hit {
                done.push(l.order_id);
            }
            !hit
        });
        done
    }

    /// 该订单在路线上的腿。
    pub fn leg_of(&self, order_id: OrderId) -> Option<&RouteLeg> {
        self.legs.iter().find(|l| l.order_id == order_id)
    }

    /// 该订单是当前腿吗（只有当前腿才推进位置）。
    pub fn is_current(&self, order_id: OrderId) -> bool {
        self.current().is_some_and(|l| l.order_id == order_id)
    }

    /// 按「就近优先」重排送达段（最近邻贪心），取餐段原地不动。
    ///
    /// 为什么最近邻够了：真实场景里同商家、相邻商家的单占绝大多数，顾客之间
    /// 往往也挨得很近（甚至同一栋楼），2-opt 那点收益换不回复杂度。`from` 是
    /// 骑手当前位置——排序从脚下开始，而不是从第一个点开始。
    pub fn sort_dropoffs_nearest(&mut self, from: LatLng) {
        let Some(start) = self.first_dropoff() else {
            return;
        };
        let mut rest: Vec<RouteLeg> = self.legs.drain(start..).collect();
        if rest.len() <= 1 {
            self.legs.extend(rest);
            return;
        }
        let mut ordered = Vec::with_capacity(rest.len());
        let mut cursor = from;
        while !rest.is_empty() {
            let mut best = 0usize;
            let mut best_d = f64::MAX;
            for (i, l) in rest.iter().enumerate() {
                let d = haversine_m(cursor, l.pos);
                if d < best_d {
                    best_d = d;
                    best = i;
                }
            }
            let leg = rest.remove(best);
            cursor = leg.pos;
            ordered.push(leg);
        }
        self.legs.extend(ordered);
    }

    /// 送达腿的订单顺序（给控制端看「这批怎么送」）。
    pub fn dropoff_order(&self) -> Vec<OrderId> {
        self.legs
            .iter()
            .filter(|l| l.kind == LegKind::Dropoff)
            .map(|l| l.order_id)
            .collect()
    }

    /// 取餐腿的订单顺序。
    pub fn pickup_order(&self) -> Vec<OrderId> {
        self.legs
            .iter()
            .filter(|l| l.kind == LegKind::Pickup)
            .map(|l| l.order_id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(lat: f64, lng: f64) -> LatLng {
        LatLng::new(lat, lng)
    }

    #[test]
    fn pickup_legs_never_sit_behind_dropoffs() {
        let mut r = RouteTask::from_pickup(1, p(30.50, 114.40), 5);
        r.push_leg(RouteLeg::new(1, LegKind::Dropoff, p(30.55, 114.45), 10));
        // 第二单的取餐腿必须插到送达腿之前
        r.push_leg(RouteLeg::new(2, LegKind::Pickup, p(30.50, 114.40), 3));

        assert_eq!(r.pickup_order(), vec![1, 2]);
        assert_eq!(r.dropoff_order(), vec![1]);
        assert_eq!(
            r.legs().iter().map(|l| l.kind).collect::<Vec<_>>(),
            vec![LegKind::Pickup, LegKind::Pickup, LegKind::Dropoff],
            "取餐腿全都得排在送达腿前面"
        );
    }

    #[test]
    fn current_leg_is_the_only_moving_one() {
        let mut r = RouteTask::from_pickup(1, p(30.50, 114.40), 5);
        r.push_leg(RouteLeg::new(2, LegKind::Pickup, p(30.50, 114.40), 3));
        assert!(r.is_current(1));
        assert!(!r.is_current(2), "第二单还没轮到它走");
    }

    #[test]
    fn nearest_first_orders_dropoffs_by_distance() {
        let mut r = RouteTask::from_pickup(1, p(30.50, 114.40), 1);
        // 故意乱序：先塞远的，再塞近的
        r.push_leg(RouteLeg::new(1, LegKind::Dropoff, p(30.60, 114.50), 9));
        r.push_leg(RouteLeg::new(2, LegKind::Dropoff, p(30.51, 114.41), 9));
        r.push_leg(RouteLeg::new(3, LegKind::Dropoff, p(30.55, 114.45), 9));

        // 从商家出发，最近的先送
        r.sort_dropoffs_nearest(p(30.50, 114.40));
        assert_eq!(r.dropoff_order(), vec![2, 3, 1]);
    }

    #[test]
    fn sort_keeps_pickups_in_front() {
        let mut r = RouteTask::from_pickup(1, p(30.50, 114.40), 1);
        r.push_leg(RouteLeg::new(2, LegKind::Pickup, p(30.50, 114.40), 1));
        r.push_leg(RouteLeg::new(1, LegKind::Dropoff, p(30.60, 114.50), 9));
        r.push_leg(RouteLeg::new(2, LegKind::Dropoff, p(30.51, 114.41), 9));

        r.sort_dropoffs_nearest(p(30.50, 114.40));
        assert_eq!(r.pickup_order(), vec![1, 2], "取餐段不该被重排");
        assert_eq!(r.dropoff_order(), vec![2, 1]);
    }

    #[test]
    fn remove_order_strips_both_legs() {
        let mut r = RouteTask::from_pickup(1, p(30.50, 114.40), 1);
        r.push_leg(RouteLeg::new(1, LegKind::Dropoff, p(30.55, 114.45), 9));
        r.push_leg(RouteLeg::new(2, LegKind::Pickup, p(30.50, 114.40), 1));
        assert!(r.remove_order(1));
        assert_eq!(r.len(), 1);
        assert_eq!(r.pickup_order(), vec![2]);
        assert!(!r.remove_order(999), "不在路线上的单不该报告动过");
    }

    #[test]
    fn pop_front_walks_the_whole_route_then_returns_none() {
        let mut r = RouteTask::from_pickup(1, p(30.50, 114.40), 2);
        r.push_leg(RouteLeg::new(1, LegKind::Dropoff, p(30.55, 114.45), 4));
        assert_eq!(r.pop_front().unwrap().kind, LegKind::Pickup);
        assert_eq!(r.pop_front().unwrap().kind, LegKind::Dropoff);
        assert!(r.pop_front().is_none());
        assert!(r.is_empty());
    }

    #[test]
    fn progress_tracks_remaining_and_never_divides_by_zero() {
        let leg = RouteLeg::new(1, LegKind::Pickup, p(30.5, 114.4), 10);
        assert!((leg.progress() - 0.0).abs() < 1e-6);

        let mut leg = RouteLeg::new(1, LegKind::Pickup, p(30.5, 114.4), 10);
        leg.remaining_min = 4;
        assert!((leg.progress() - 0.6).abs() < 1e-6);

        let zero = RouteLeg::new(1, LegKind::Pickup, p(30.5, 114.4), 0);
        assert_eq!(zero.total_min, 1);
    }
}
