//! 订单：生命周期、费用构成、评分门槛分层。

use serde::{Deserialize, Serialize};

use super::{OrderId, RiderId, geo::LatLng};

/// 订单状态。协议里必须是**字符串**，不用数字。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    /// 大厅里等人抢。
    Pending,
    /// 已抢 / 已指派。
    Assigned,
    /// 已取货，配送中。
    PickedUp,
    /// 已送达。
    Delivered,
    /// 送达超时（仍算完成，但扣钱掉分）。
    Late,
    /// 重投次数用尽，彻底没人接的单。
    ///
    /// 注意它**不是**「一次没人接」的意思：超窗或被取消都只触发加价重投
    /// （见 [`crate::sim::world::World::release_to_hall`]），投满
    /// [`crate::sim::config::MAX_REDISPATCH_ATTEMPTS`] 次才落到这里。
    Expired,
    /// 保留值：当前任何路径都不再产生。
    ///
    /// 骑手取消已接单在过去会落到这里、订单一去不返，现在改为退回大厅重投。
    /// 变体留着是因为它仍出现在协议与历史流水里，删掉会破坏回放兼容。
    Cancelled,
}

impl OrderStatus {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            OrderStatus::Pending | OrderStatus::Assigned | OrderStatus::PickedUp
        )
    }
    pub fn is_finished(self) -> bool {
        !self.is_active()
    }
}

/// 平台派单的两种口径（控制端派单弹窗二选一）。
///
/// - [`AssignMode::Offer`]：指派单——骑手端弹确认页，可以拒绝，拒一次吃一格
///   `max_rejects_per_day` 额度；
/// - [`AssignMode::Forced`]：强制派单——只能点同意，服务端会挡回任何 `reject`。
///
/// 额度用尽被强制下线的骑手，之后收到的平台单**一律按 `Forced`** 处理
/// （见 `scheduler::assign_forced`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AssignMode {
    #[default]
    Offer,
    Forced,
}

/// 评分门槛分层（玩法设计 §8.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderTier {
    /// ≥4.6 可抢普通单。
    Normal,
    /// ≥4.8 可抢优质单（距离远、单价高）。
    Premium,
    /// ≥4.9 可抢急单（加价 30%）。
    Express,
}

/// 费用明细，全部「分」。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct FeeBreakdown {
    pub base_fee_cents: i64,
    pub distance_extra_cents: i64,
    pub rain_bonus_cents: i64,
    pub level_bonus_cents: i64,
    pub surge_multiplier: f32,
    /// 平台抽水（只抽配送费，不抽雨补）。
    pub commission_cents: i64,
    /// 抢单时展示的报价 = 各项之和。
    pub quoted_fee_cents: i64,
    /// 到手 = 报价 - 抽水。
    pub takehome_cents: i64,
}

/// 订单携带的附加任务（玩法设计 §6）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtraTask {
    pub id: String,
    pub name: String,
    pub step: usize,
    pub done: bool,
    pub failed: bool,
    pub reward_cents: i64,
    pub penalty_cents: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: OrderId,
    pub merchant_id: u32,
    pub merchant_name: String,
    pub merchant_lat: f64,
    pub merchant_lng: f64,

    pub customer_name: String,
    pub customer_address: String,
    pub remark: String,
    pub customer_lat: f64,
    pub customer_lng: f64,

    /// 路线距离，整数米。
    pub distance_m: u32,
    pub promised_minutes: u32,
    pub tier: OrderTier,

    /// 商家步行难度 0/1/2（路边店 / 商场内 / 高层深巷）。
    ///
    /// **物理量**：由商家数据在生成时定下，运行期不变（见 `WalkRules`）。
    pub merchant_walk_difficulty: u8,
    /// 顾客端步行难度 0/1/2（送到门口 / 进小区上楼 / 高层深巷）。
    pub customer_walk_difficulty: u8,

    /// 生成时刻（自开局起算的模拟分钟，单调）。
    pub created_at_min: u64,
    /// 抢单窗口截止（tick 数换算，见 SimConfig::claim_window_ticks）。
    pub claim_deadline_min: u64,
    pub picked_up_at_min: Option<u64>,
    pub delivered_at_min: Option<u64>,

    pub fee: FeeBreakdown,
    /// 无人接 / 被取消而重投大厅的次数，见 [`crate::sim::config::MAX_REDISPATCH_ATTEMPTS`]。
    ///
    /// 注意它和 `status == Expired` 不是一回事：重投过但最终被接走的单也有计数。
    pub redispatch_count: u32,
    /// 重投叠加的调度补贴（分）。**全给骑手**，不计入抽水基数。
    ///
    /// 单独存而不是并进 `fee`，是为了让 `fee` 始终是「按计费规则算出来的报价」，
    /// 补贴是派单博弈的产物，两者混在一起就再也说不清单价了。
    pub surcharge_cents: i64,
    pub status: OrderStatus,
    pub assigned_to: Option<RiderId>,
    /// 平台派过来的单（控制端指派 / 改派），骑手端要弹确认框。
    ///
    /// 只表示「不是自己抢的」；能不能拒由 [`Order::assign_mode`] 决定。
    pub forced: bool,
    /// 待确认期间是哪种派单：`offer` 可拒 / `forced` 只能同意。
    ///
    /// `None` = 不是平台派过来的（大厅抢的 / AI 派的 / 系统救单）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assign_mode: Option<AssignMode>,
    /// 背单（绕路顺带）。
    pub back_order: bool,
    pub back_order_bonus_cents: i64,

    pub task: Option<ExtraTask>,
    /// 已催单次数。
    pub urge_count: u32,
    /// 商家实际出餐耗时（分钟），到店后确定。
    pub prep_minutes: Option<u32>,
    /// 实时出餐秒数。`Some` = 这单按**真实墙钟**出餐（控制台临时单用），
    /// 真人骑手的等餐倒计时走秒，不再按模拟分钟一格一格跳。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prep_seconds: Option<u32>,
}

impl Order {
    pub fn merchant_pos(&self) -> LatLng {
        LatLng::new(self.merchant_lat, self.merchant_lng)
    }
    pub fn customer_pos(&self) -> LatLng {
        LatLng::new(self.customer_lat, self.customer_lng)
    }

    /// 骑手看到的报价 = 计费规则报价 + 调度补贴。
    pub fn quoted_fee_cents(&self) -> i64 {
        self.fee.quoted_fee_cents + self.surcharge_cents
    }

    /// 骑手到手 = 计费规则到手 + 调度补贴（补贴不抽水）。
    pub fn takehome_cents(&self) -> i64 {
        self.fee.takehome_cents + self.surcharge_cents
    }

    /// 是否超过承诺送达时间（含背单缓冲）。
    pub fn is_late(&self, now_min: u64, buffer_min: u32) -> bool {
        now_min > self.created_at_min + self.promised_minutes as u64 + buffer_min as u64
    }

    /// 逾期分钟数（用于赔付系数）。
    pub fn overdue_minutes(&self, now_min: u64, buffer_min: u32) -> u32 {
        let deadline = self.created_at_min + self.promised_minutes as u64 + buffer_min as u64;
        now_min.saturating_sub(deadline) as u32
    }

    /// 退回大厅时重新给时限：已经空转掉的时间不记在下一个骑手头上。
    ///
    /// 只在剩余时限低于 `floor_remaining_min` 时才补，且只补到「从下单起算刚好剩这么多」——
    /// 每轮只延长一点点，反复退回不会把承诺越推越长。
    ///
    /// 没有这一步，「超时救单」塞给骑手的每一单都必然超时：单子在大厅里空转多久，
    /// 接手的人就少跑多久，白跑一趟还倒贴罚金。
    pub fn renew_promise(&mut self, now_min: u64, floor_remaining_min: u32) {
        let idle = now_min.saturating_sub(self.created_at_min) as u32;
        let remaining = self.promised_minutes.saturating_sub(idle);
        if remaining < floor_remaining_min {
            self.promised_minutes = idle + floor_remaining_min;
        }
    }

    /// **首次派单时**补偿大厅空转的时间。
    ///
    /// 与 [`Self::renew_promise`] 同一个道理，但触发时机不同，所以必须单列：
    ///
    /// - `renew_promise` 处理的是「接了又退」，已经至少派出去过一次；
    /// - 这里处理的是**第一次**派出去之前，订单在大厅里空转的那段。
    ///
    /// 少了这一步，「下单 → 派单」的空转会被整段算到接单骑手头上：
    /// 骑手明明刚接单就出发，却要替平台在大厅里待的那十几分钟买单。
    /// 实测（seed 20261011）一整天 145 个送达单里 125 个超时，
    /// 时间线显示大量单在下单后 15~20 分钟才被派出去——
    /// 而承诺时限是按「下单时刻」起算的。
    ///
    /// **与 `renew_promise` 的区别**：这一段是**无条件**等额补偿的。
    /// 承诺时限本来就该从「骑手出发」起算——骑手没上车之前的时间，
    /// 平台自己没调度好，不该由接单的人买单。`renew_promise` 之所以还要留个
    /// 下限，是因为退回重投会**反复**发生，无条件补会让承诺越推越长。
    /// 首次派单每单只发生一次，所以可以直接补满。
    pub fn credit_hall_idle(&mut self, now_min: u64) {
        let idle = now_min.saturating_sub(self.created_at_min) as u32;
        // 已经在大厅里待过的部分，把承诺等额往后延。
        self.promised_minutes += idle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order() -> Order {
        Order {
            id: 1,
            merchant_id: 1,
            merchant_name: "老王热干面".into(),
            merchant_lat: 30.5,
            merchant_lng: 114.4,
            customer_name: "顾客".into(),
            customer_address: "光谷一路".into(),
            remark: String::new(),
            customer_lat: 30.51,
            customer_lng: 114.41,
            distance_m: 1200,
            promised_minutes: 30,
            tier: OrderTier::Normal,
            merchant_walk_difficulty: 0,
            customer_walk_difficulty: 0,
            created_at_min: 100,
            claim_deadline_min: 103,
            picked_up_at_min: None,
            delivered_at_min: None,
            fee: FeeBreakdown::default(),
            redispatch_count: 0,
            surcharge_cents: 0,
            status: OrderStatus::Pending,
            assigned_to: None,
            forced: false,
            assign_mode: None,
            back_order: false,
            back_order_bonus_cents: 0,
            task: None,
            urge_count: 0,
            prep_minutes: None,
            prep_seconds: None,
        }
    }

    #[test]
    fn late_uses_buffer() {
        let o = order();
        assert!(!o.is_late(134, 5));
        assert!(o.is_late(136, 5));
        assert_eq!(o.overdue_minutes(136, 5), 1);
    }

    #[test]
    fn status_helpers() {
        assert!(OrderStatus::Pending.is_active());
        assert!(OrderStatus::Delivered.is_finished());
        assert!(!OrderStatus::Expired.is_active());
    }
}
