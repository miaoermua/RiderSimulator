//! 派单范围：以骑手**有效当前位置**为圆心、后台配置的半径为界的可见性判定。
//!
//! ## 为什么单独一个模块
//!
//! 这条规则要在三个地方用，且必须是同一套口径：
//!
//! 1. `net::hub` 拼快照/事件时筛订单大厅（骑手只该看见圈内的单）；
//! 2. `sim::scheduler` 接单/加单时再校验一次（不能靠"客户端没收到"当权限）；
//! 3. 测试里直接断言边界行为。
//!
//! 分散写成三份必然漂移——尤其"等于半径算不算"这种边界，一处 `<=`
//! 一处 `<` 就是两个业务规则。所以判断只在这里写一遍。
//!
//! ## 零 IO
//!
//! 本模块在 `sim/` 下，受 `tests/layering.rs` 约束：不碰 fs、网络、数据库。
//! 时间从 `World.wall_ms` 入参拿，不从 `SystemTime::now()` 取——
//! 那既是 IO，也会让测试不可复现。

use crate::sim::geo::{self, LatLng};
use crate::sim::order::Order;
use crate::sim::rider::Rider;
use crate::sim::world::World;
use crate::sim::{OrderId, RiderId};

/// 定位有效期，毫秒。
///
/// ## 为什么是 5 分钟
///
/// 客户端正常上报周期是 30 秒（`location_service.dart` 的 `normal` 档，
/// 省电档 60 秒），5 分钟约等于**连续丢 5~10 次上报**。这个值要满足两头：
///
/// - 不能太短：手机在电梯、地下车库、隧道里断几十秒是常态，
///   一断就把大厅清空，骑手会觉得"App 坏了"；
/// - 不能太长：十来分钟前的位置可能已经在两公里外，
///   拿它当圆心派单等于给错单。
///
/// 选 5 分钟是因为它同时覆盖「省电档 60s × 5 次」和「一次短暂失联」，
/// 又明显短于「换了个片区」的量级。
pub const POSITION_TTL_MS: u64 = 5 * 60 * 1000;

/// 派单半径的合法区间（km）。后台输入、`set_config` 校验、UI 提示共用这一份。
///
/// - 下限 1：再小就等于关掉派单，那该用别的开关表达，不该让半径退化成 0；
/// - 上限 500：跨城通勤的量级（武汉↔长沙约 350km）。再往上，
///   "按距离派单"这件事本身就没意义了，还不如取消限制。
pub const RADIUS_MIN_KM: f64 = 1.0;
pub const RADIUS_MAX_KM: f64 = 500.0;

/// 配置默认值（km）。老配置没有这个字段时用它。
pub const RADIUS_DEFAULT_KM: f64 = 90.0;

/// 校验一个候选半径。合法返回 `Ok`，否则返回给人看的错误文案。
///
/// HTTP、WS、`apply_config_patch` 三处都调这一个函数：
/// 文案统一，范围也只有一份。**不做 clamp**——把 0 悄悄改成 1、
/// 把 9999 悄悄改成 500，管理员会以为配置生效了。
pub fn validate_radius_km(v: f64) -> Result<(), String> {
    if !v.is_finite() {
        return Err(format!("派单范围半径必须是有限数，收到 {v}"));
    }
    if v < RADIUS_MIN_KM || v > RADIUS_MAX_KM {
        return Err(format!(
            "派单范围半径需在 {RADIUS_MIN_KM}~{RADIUS_MAX_KM} km 之间，收到 {v}"
        ));
    }
    Ok(())
}

/// 一次位置判定的结果。分开而不是 `Option<bool>`，是为了让调用方能
/// 区分「没定位」和「定位过期」——两者的 UI 提示不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionState {
    /// 有可用位置：有限、在合法经纬度范围内、且未过期。
    Valid,
    /// 从没上报过（`position_report_wall_ms == 0`）。
    Never,
    /// 上报过，但超过 [`POSITION_TTL_MS`]。
    Stale,
    /// 经纬度本身非法（NaN / 无穷 / 越界）。
    Invalid,
}

impl PositionState {
    pub fn is_valid(self) -> bool {
        matches!(self, PositionState::Valid)
    }
}

/// 经纬度是否落在合法范围且为有限数。
///
/// `(0, 0)` 会被判为**合法坐标**——它是几内亚湾海面上的一个真实点。
/// 想拒绝它得靠"有没有上报过"（[`PositionState::Never`]），
/// 不能靠"零值等于没定位"：那在算法上站不住，也会误伤真正在赤道/本初子午线
/// 附近的坐标。
pub fn coords_finite_and_in_range(lat: f64, lng: f64) -> bool {
    lat.is_finite()
        && lng.is_finite()
        && (-90.0..=90.0).contains(&lat)
        && (-180.0..=180.0).contains(&lng)
}

/// 判断某个骑手此刻的位置能不能用来筛派单范围。
pub fn position_state(rider: &Rider, now_wall_ms: u64) -> PositionState {
    if !coords_finite_and_in_range(rider.lat, rider.lng) {
        return PositionState::Invalid;
    }
    if rider.position_report_wall_ms == 0 {
        return PositionState::Never;
    }
    // 时间倒退（改过系统时钟、或 wall_ms 还没走到）时按"刚上报"处理，
    // 不判过期——否则时钟一抖，所有人的大厅瞬间清空。
    if now_wall_ms < rider.position_report_wall_ms {
        return PositionState::Valid;
    }
    if now_wall_ms - rider.position_report_wall_ms > POSITION_TTL_MS {
        return PositionState::Stale;
    }
    PositionState::Valid
}

/// 骑手的有效位置，无效时返回 `None`。
pub fn valid_position(rider: &Rider, now_wall_ms: u64) -> Option<LatLng> {
    position_state(rider, now_wall_ms)
        .is_valid()
        .then(|| LatLng {
            lat: rider.lat,
            lng: rider.lng,
        })
}

/// 订单的取货点。用商家坐标，**不用**顾客坐标——派单范围看的是
/// 「骑手愿不愿意过去取」，送哪儿是接单之后的事（而且跨城单可能很远，
/// 拿送货点当圆心会把所有长途单都筛掉）。
pub fn pickup_point(order: &Order) -> Option<LatLng> {
    coords_finite_and_in_range(order.merchant_lat, order.merchant_lng).then(|| LatLng {
        lat: order.merchant_lat,
        lng: order.merchant_lng,
    })
}

/// 订单取货点是否落在骑手当前位置的派单半径内。
///
/// **等于半径算通过**（`<=`）。边界上"刚好 90.000 km"没有业务含义上的
/// 特殊性，选闭区间是因为它更容易解释，也更容易写测试。
///
/// 骑手无有效位置、订单缺取货坐标时一律返回 `false`——宁可少看见单，
/// 也不能因为数据缺失就退化成"不限制"。
pub fn order_in_range(rider: &Rider, order: &Order, radius_km: f64, now_wall_ms: u64) -> bool {
    let Some(here) = valid_position(rider, now_wall_ms) else {
        return false;
    };
    let Some(pickup) = pickup_point(order) else {
        return false;
    };
    let limit_m = radius_km * 1000.0;
    if !limit_m.is_finite() || limit_m < 0.0 {
        return false;
    }
    geo::haversine_m(here, pickup) <= limit_m
}

/// 按当前配置和墙钟，筛出这个骑手能看见的待抢订单。
///
/// 传入的是**已经过状态筛选**的大厅列表（`OrderStatus::Pending` 等），
/// 这里只负责距离这一维——别把状态机判断也塞进来，否则两处各有一套
/// "什么算可抢"，迟早对不上。
pub fn visible_orders<'a>(
    world: &World,
    rider: &Rider,
    hall: impl IntoIterator<Item = &'a Order>,
) -> Vec<&'a Order> {
    let radius = world.cfg.dispatch_radius_km;
    let now = world.wall_ms;
    hall.into_iter()
        .filter(|o| order_in_range(rider, o, radius, now))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个"刚上报过位置"的骑手。
    fn rider_at(lat: f64, lng: f64, reported_at: u64) -> Rider {
        use crate::sim::{config::SimConfig, rider::RiderKind};

        let cfg = SimConfig::default();
        let mut r = Rider::new(1, RiderKind::Human, "测试骑手", LatLng::new(lat, lng), &cfg);
        r.lat = lat;
        r.lng = lng;
        r.position_report_wall_ms = reported_at;
        r
    }

    #[test]
    fn coords_validation() {
        assert!(coords_finite_and_in_range(30.5, 114.3));
        assert!(coords_finite_and_in_range(90.0, 180.0));
        assert!(coords_finite_and_in_range(-90.0, -180.0));
        // (0,0) 是合法坐标，不是"没定位"。
        assert!(coords_finite_and_in_range(0.0, 0.0));
        assert!(!coords_finite_and_in_range(91.0, 0.0));
        assert!(!coords_finite_and_in_range(0.0, 181.0));
        assert!(!coords_finite_and_in_range(f64::NAN, 0.0));
        assert!(!coords_finite_and_in_range(0.0, f64::INFINITY));
    }

    #[test]
    fn position_state_transitions() {
        let now = 1_000_000u64;
        assert_eq!(
            position_state(&rider_at(30.0, 114.0, 0), now),
            PositionState::Never
        );
        assert_eq!(
            position_state(&rider_at(30.0, 114.0, now), now),
            PositionState::Valid
        );
        assert_eq!(
            position_state(&rider_at(30.0, 114.0, now - POSITION_TTL_MS), now),
            PositionState::Valid,
            "刚好等于 TTL 不算过期"
        );
        assert_eq!(
            position_state(&rider_at(30.0, 114.0, now - POSITION_TTL_MS - 1), now),
            PositionState::Stale
        );
        // 时间倒退按"刚上报"处理，不清空大厅。
        assert_eq!(
            position_state(&rider_at(30.0, 114.0, now + 5000), now),
            PositionState::Valid
        );
        assert_eq!(
            position_state(&rider_at(f64::NAN, 114.0, now), now),
            PositionState::Invalid
        );
    }
}

#[cfg(test)]
mod acceptance {
    //! 阶段 1 的验收项。与上面的单元测试分开：
    //! 那批测「单个函数的判定」，这批测**整条链路**
    //! （World 造真实订单 → 骑手位置 → 半径 → 可见性）。

    use super::*;
    use crate::sim::config::SimConfig;
    use crate::sim::world::World;

    /// 造一个测试世界，把 0 号骑手放到指定坐标并标记为"刚上报"。
    fn world_with_rider_at(lat: f64, lng: f64) -> (World, crate::sim::RiderId) {
        let mut w = World::new_for_test(SimConfig::default(), 7);
        let rid = w.riders[0].id;
        let now = w.wall_ms;
        let r = w.rider_mut(rid).expect("骑手在");
        r.report_position(LatLng::new(lat, lng), now);
        (w, rid)
    }

    /// 生成一单，并把它的取货点钉到指定坐标（其余字段保持真实生成的）。
    fn order_at(w: &mut World, lat: f64, lng: f64) -> Order {
        let oid = w.generate_order().expect("应该能出一单");
        let o = w.order_mut(oid).expect("刚生成的单在");
        o.merchant_lat = lat;
        o.merchant_lng = lng;
        o.clone()
    }

    #[test]
    fn default_radius_is_90_km() {
        assert_eq!(SimConfig::default().dispatch_radius_km, 90.0);
        // 老配置（JSON 里没有这个字段）也必须落到 90。
        let json = serde_json::to_string(&SimConfig::default()).unwrap();
        let stripped: serde_json::Value = {
            let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
            v.as_object_mut().unwrap().remove("dispatch_radius_km");
            v
        };
        let back: SimConfig = serde_json::from_value(stripped).expect("缺字段也要能读");
        assert_eq!(back.dispatch_radius_km, 90.0);
    }

    #[test]
    fn boundary_equal_is_inclusive() {
        let (mut w, rid) = world_with_rider_at(30.0, 114.0);
        let order = order_at(&mut w, 30.5, 114.3);
        let rider = w.rider(rid).unwrap().clone();
        let exact_km = geo::haversine_m(
            LatLng::new(rider.lat, rider.lng),
            LatLng::new(order.merchant_lat, order.merchant_lng),
        ) / 1000.0;
        let now = w.wall_ms;

        assert!(
            order_in_range(&rider, &order, exact_km + 0.001, now),
            "半径略大于距离：应通过"
        );
        // 恰好等于：拿半径 = 精确距离。haversine 的浮点结果自身就是这里算出来的，
        // 所以 `<=` 与 `<` 的差别在这条测试里才体现得出来。
        assert!(
            order_in_range(&rider, &order, exact_km, now),
            "半径等于距离：应通过（闭区间）"
        );
        assert!(
            !order_in_range(&rider, &order, exact_km - 0.01, now),
            "半径略小于距离：应拒绝"
        );
    }

    #[test]
    fn without_position_no_orders() {
        let (mut w, rid) = world_with_rider_at(30.0, 114.0);
        // 抹掉上报时间戳 = 从没定位过。
        w.rider_mut(rid).unwrap().position_report_wall_ms = 0;

        let orders: Vec<Order> = vec![order_at(&mut w, 30.0, 114.0)];
        let rider = w.rider(rid).unwrap().clone();
        assert!(
            !order_in_range(&rider, &orders[0], 90.0, w.wall_ms),
            "没有有效定位时，连同一地点的单都不该放行"
        );
        assert!(visible_orders(&w, &rider, &orders).is_empty());
    }

    #[test]
    fn stale_position_is_not_valid() {
        let (mut w, rid) = world_with_rider_at(30.0, 114.0);
        let order = order_at(&mut w, 30.0, 114.0);
        // 把上报时间推到 TTL 之外。
        let now = w.wall_ms;
        w.rider_mut(rid).unwrap().position_report_wall_ms = now - POSITION_TTL_MS - 1;

        let rider = w.rider(rid).unwrap().clone();
        assert_eq!(position_state(&rider, now), PositionState::Stale);
        assert!(
            !order_in_range(&rider, &order, 90.0, now),
            "过期定位不能当有效位置用"
        );
    }

    #[test]
    fn invalid_coords_cannot_bypass() {
        let (mut w, rid) = world_with_rider_at(30.0, 114.0);
        let order = order_at(&mut w, 30.0, 114.0);
        let now = w.wall_ms;

        for (lat, lng) in [(91.0, 114.0), (30.0, 181.0), (f64::NAN, 114.0)] {
            w.rider_mut(rid)
                .unwrap()
                .report_position(LatLng::new(lat, lng), now);
            let rider = w.rider(rid).unwrap().clone();
            assert!(
                !order_in_range(&rider, &order, 90.0, now),
                "非法坐标 ({lat},{lng}) 不能被当作有效位置"
            );
        }
    }

    #[test]
    fn cross_city_within_radius_passes() {
        // 武汉 → 黄石，直线约 90km 出头；把半径放到 200 就必须能看见。
        // 这条专门盯「不能按城市/行政区判断」——系统里根本没有城市字段，
        // 只有两个坐标之间的距离。
        let (mut w, rid) = world_with_rider_at(30.5928, 114.3055);
        let order = order_at(&mut w, 30.2000, 115.0389);
        let rider = w.rider(rid).unwrap().clone();
        let now = w.wall_ms;
        assert!(
            order_in_range(&rider, &order, 200.0, now),
            "跨城但距离在半径内：必须通过"
        );
        assert!(
            !order_in_range(&rider, &order, 50.0, now),
            "同一个订单，半径收小后必须拒绝"
        );
    }

    #[test]
    fn different_riders_get_different_results() {
        let mut w = World::new_for_test(SimConfig::default(), 11);
        let ids: Vec<_> = w.riders.iter().map(|r| r.id).collect();
        let now = w.wall_ms;
        // 甲的坐标钉在取货点旁，乙的钉在 500km 外。
        let order = order_at(&mut w, 30.0, 114.0);
        w.rider_mut(ids[0])
            .unwrap()
            .report_position(LatLng::new(30.0, 114.0), now);
        w.rider_mut(ids[1])
            .unwrap()
            .report_position(LatLng::new(35.0, 120.0), now);

        let a = w.rider(ids[0]).unwrap().clone();
        let b = w.rider(ids[1]).unwrap().clone();
        assert!(order_in_range(&a, &order, 90.0, now), "甲就在旁边");
        assert!(!order_in_range(&b, &order, 90.0, now), "乙在 500km 外");
    }

    #[test]
    fn validate_radius_rejects_bad_values() {
        assert!(validate_radius_km(90.0).is_ok());
        assert!(validate_radius_km(RADIUS_MIN_KM).is_ok());
        assert!(validate_radius_km(RADIUS_MAX_KM).is_ok());
        assert!(validate_radius_km(0.0).is_err(), "0 应被拒（下限 1）");
        assert!(validate_radius_km(-5.0).is_err());
        assert!(validate_radius_km(501.0).is_err(), "超过上限");
        assert!(validate_radius_km(f64::NAN).is_err(), "NaN 必须拒绝");
        assert!(validate_radius_km(f64::INFINITY).is_err(), "无穷必须拒绝");
    }

    #[test]
    fn claimed_order_is_unaffected_by_rider_moving() {
        // 已接单不该因为骑手走远而消失——这条守的是「别把大厅规则
        // 错误扩展成对所有订单操作一刀切」。
        let (mut w, rid) = world_with_rider_at(30.0, 114.0);
        let oid = w.generate_order().expect("出一单");
        let order = w.order_mut(oid).unwrap();
        order.merchant_lat = 30.0;
        order.merchant_lng = 114.0;
        crate::sim::scheduler::set_online(&mut w, rid, true).expect("上线");
        crate::sim::scheduler::try_claim(&mut w, oid, rid).expect("在范围内应能抢");

        // 骑手跑到 500km 外。
        let now = w.wall_ms;
        w.rider_mut(rid)
            .unwrap()
            .report_position(LatLng::new(35.0, 120.0), now);

        let o = w.order(oid).expect("单还在");
        assert!(
            o.assigned_to == Some(rid),
            "接单之后骑手再远，单也不该被收回"
        );
    }

    #[test]
    fn claim_out_of_range_is_rejected_by_scheduler() {
        // 绕过大厅直接调 try_claim：接口必须自己挡。
        let (mut w, rid) = world_with_rider_at(30.0, 114.0);
        let oid = w.generate_order().expect("出一单");
        {
            let o = w.order_mut(oid).unwrap();
            o.merchant_lat = 31.5; // 约 100km 外，超过默认 90
            o.merchant_lng = 114.0;
        }
        crate::sim::scheduler::set_online(&mut w, rid, true).expect("上线");
        let err = crate::sim::scheduler::try_claim(&mut w, oid, rid).expect_err("超出半径必须被拒");
        assert_eq!(err, crate::sim::scheduler::ClaimError::OutOfRange);
    }
}

// ===========================================================================
// 推荐单 / 顺路单
// ===========================================================================

/// 顺路度的打标门槛：额外绕行不超过新单直达里程的 50%。
///
/// 50% 是「绕路还能接受」的分界：新单一趟 3 公里，绕 1.5 公里以内算顺路，
/// 再多就不如送完手上这单再接。低于 40% 会让大量"其实不顺"的单挂上标签，
/// 标签一泛滥就没有信息量了。
pub const ON_THE_WAY_MIN_PCT: u8 = 50;

/// 判定「同一个地点」的坐标容差，米。
///
/// 为什么不直接用 0：同一栋写字楼的两个入口、同一个小区的东西门，
/// 手机 GPS 给出的坐标能差几十米。50 米是「同一栋楼/同一个门」的量级，
/// 明显小于项目里 `arrive_radius_m` 的默认 100 米（那是"到了"的判定，
/// 本来就该更宽松），所以不会把相邻两栋楼误认成一处。
pub const SAME_SPOT_M: f64 = 50.0;

/// 推荐概率的默认值（百分比）。项目里没有既定的推荐策略，
/// 20% 的含义是「十张单里大概两张带推荐」，既不刷屏也不至于看不见。
pub const RECOMMEND_PERCENT_DEFAULT: u32 = 20;

/// 一张订单对**某个骑手**的标记。
///
/// **不落在 `Order` 上**：同一张单，甲看到可能是推荐、乙看到的不是，
/// 顺路度更是每人一套。写进订单本体就会变成"给全服打标签"，
/// 那正是任务书里明确不允许的错误。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OrderMarks {
    /// 推荐单。与 `on_the_way_pct` **互不排斥**：可以同时成立。
    pub recommended: bool,
    /// 顺路度（0~100 整数）。`None` = 不满足计算前提（没有配送中的单 / 缺坐标）。
    pub on_the_way_pct: Option<u8>,
    /// 与手上某笔配送中订单**同一个取货地点**。
    pub same_pickup: bool,
    /// 与手上某笔配送中订单**同一个送货地点**。
    pub same_dropoff: bool,
}

/// 稳定哈希：同一 (seed, rider, order) 永远得到同一个值。
///
/// 用它做推荐抽样，而不是 `rand::random()`——后者会让标签在每次刷新
/// 时随机闪，用户会以为界面坏了。FNV-1a 足够用：这里只要"分布均匀且确定"，
/// 不需要密码学强度。
fn stable_hash(seed: u64, rider: RiderId, order: OrderId) -> u64 {
    const PRIME: u64 = 0x100_0000_01b3;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ seed;
    for v in [rider as u64, order as u64] {
        for b in v.to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(PRIME);
        }
    }
    h
}

/// 这张单对这个骑手是否命中推荐概率。
///
/// 0% 永远不推；100% 恒推（`h % 100` 最大 99，`< 100` 恒真）。
fn recommend_hit(seed: u64, rider: RiderId, order: OrderId, percent: u32) -> bool {
    if percent == 0 {
        return false;
    }
    if percent >= 100 {
        return true;
    }
    (stable_hash(seed, rider, order) % 100) < percent as u64
}

/// 订单的送货点。
fn dropoff_point(order: &Order) -> Option<LatLng> {
    coords_finite_and_in_range(order.customer_lat, order.customer_lng).then(|| LatLng {
        lat: order.customer_lat,
        lng: order.customer_lng,
    })
}

/// 骑手手上**正在配送中**的订单。
///
/// 只认 `PickedUp`：`Assigned` 是"接了还没取"，`Pending` 是大厅里的，
/// 都不构成"当前路线"。任务书点名不许把"仅已接单/待取货"算进来。
fn delivering_orders<'a>(world: &'a World, rider_id: RiderId) -> Vec<&'a Order> {
    world
        .orders
        .iter()
        .filter(|o| o.status == crate::sim::order::OrderStatus::PickedUp)
        .filter(|o| o.assigned_to == Some(rider_id))
        .collect()
}

/// 算一张新单与**所有**配送中订单的顺路情况，取最顺的那笔。
///
/// ## 算法（全部是球面直线距离，不是道路路线）
///
/// 设骑手在 `H`，手上这笔单要送到 `C`，新单从 `P` 取、送到 `D`：
///
/// ```text
/// 基线   = |H→C|                          不接新单，纯粹把手上这单送完
/// 插入   = |H→P| + |P→D| + |D→C|          接了新单之后的整条路线
/// 多绕   = max(0, 插入 − 基线)
/// 顺路度 = 100 − 100 × 多绕 / |P→D|
/// ```
///
/// 含义：**为了这单多跑的路，占它本身里程的百分之多少**。
/// 多绕为 0（新单正好在去 C 的路上）→ 100；多绕等于新单全程 → 0。
///
/// 局限必须说清楚：`| |` 是球面直线，不是路网距离，也不是行驶时间。
/// 所以这个百分比描述的是**几何上的绕路比例**，不承诺实际能省多少分钟。
/// 项目里没有可用的路线 API（`route_plan_page.dart` 也只画直线），
/// 与其编一个假的道路里程，不如给一个能验算的几何量。
fn detour_pct(here: LatLng, new_order: &Order, cur: &Order) -> Option<u8> {
    let (pickup, dropoff, cur_dropoff) = (
        pickup_point(new_order)?,
        dropoff_point(new_order)?,
        dropoff_point(cur)?,
    );
    let leg_new = geo::haversine_m(pickup, dropoff);
    // 新单取送点重合（数据异常）时无法定义"百分比"，直接不给标签。
    if leg_new <= 0.0 {
        return None;
    }
    let baseline = geo::haversine_m(here, cur_dropoff);
    let inserted =
        geo::haversine_m(here, pickup) + leg_new + geo::haversine_m(dropoff, cur_dropoff);
    // `max(0)`：理论上插入不会小于基线，但浮点误差可能给出 -0.0001 米，
    // 不夹一下会得到 100 以上的分数。
    let extra = (inserted - baseline).max(0.0);
    let pct = 100.0 - 100.0 * extra / leg_new;
    Some(pct.clamp(0.0, 100.0).round() as u8)
}

/// 给大厅里每一张单算出这个骑手视角的标记。
///
/// `hall` 传的是**已经过半径筛选**的列表——推荐和顺路都不该绕过派单范围，
/// 所以这里不再自己筛一遍，而是假定调用方已经把不合格的单剔掉了。
pub fn marks_for_hall(world: &World, rider: &Rider, hall: &[Order]) -> Vec<(OrderId, OrderMarks)> {
    let delivering = delivering_orders(world, rider.id);
    let percent = world.cfg.recommend_percent;
    let now = world.wall_ms;
    let here = valid_position(rider, now);

    // 满载 / 已达日上限的骑手不给推荐标记：推荐了也接不了，
    // 那是在骗人点进去。顺路标记仍然照算——它描述的是"顺路与否"，
    // 与能不能接无关。
    let has_room = (rider.held.len() as u32) < rider.effective_max_held(&world.cfg)
        && !rider.reached_daily_cap(&world.cfg);

    hall.iter()
        .map(|o| {
            let recommended = has_room && recommend_hit(world.cfg.seed, rider.id, o.id, percent);

            let (mut pct, mut same_pickup, mut same_dropoff) = (None, false, false);
            if let Some(here) = here {
                for cur in &delivering {
                    // 同取货：比商家 ID，不比坐标——merchant_id 是稳定标识，
                    // 而同一家店可能因为数据源不同有一两米的坐标差。
                    if o.merchant_id == cur.merchant_id {
                        same_pickup = true;
                    }
                    // 同送货：只能比坐标。顾客没有稳定 ID，姓名/地址串都不够可靠
                    // （"3 号楼"在两个字串里可能写法不同），坐标容差见 `SAME_SPOT_M`。
                    if let (Some(a), Some(b)) = (dropoff_point(o), dropoff_point(cur))
                        && geo::haversine_m(a, b) <= SAME_SPOT_M
                    {
                        same_dropoff = true;
                    }
                    if let Some(p) = detour_pct(here, o, cur) {
                        pct = Some(pct.map_or(p, |best: u8| best.max(p)));
                    }
                }
            }
            // 达不到门槛就不挂标签：`Some(30)` 在客户端仍会显示成"顺路 30%"，
            // 那等于把"其实不顺"的单也标出来了。
            let on_the_way_pct = pct.filter(|p| *p >= ON_THE_WAY_MIN_PCT);

            (
                o.id,
                OrderMarks {
                    recommended,
                    on_the_way_pct,
                    same_pickup,
                    same_dropoff,
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod marks_tests {
    //! 推荐单 / 顺路单的验收项。

    use super::*;
    use crate::sim::config::SimConfig;
    use crate::sim::order::OrderStatus;
    use crate::sim::world::World;

    /// 造一个世界 + 一个"站在城里、刚上报过"的骑手。
    fn setup(percent: u32) -> (World, RiderId) {
        let cfg = SimConfig {
            recommend_percent: percent,
            ..Default::default()
        };
        let mut w = World::new_for_test(cfg, 21);
        let rid = w.riders[0].id;
        let now = w.wall_ms;
        let r = w.rider_mut(rid).expect("骑手在");
        let pos = LatLng::new(r.lat, r.lng);
        r.report_position(pos, now);
        (w, rid)
    }

    /// 生成一单并钉住取送坐标。
    fn order_xy(w: &mut World, plat: f64, plng: f64, dlat: f64, dlng: f64) -> Order {
        let oid = w.generate_order().expect("出一单");
        let o = w.order_mut(oid).expect("单在");
        o.merchant_lat = plat;
        o.merchant_lng = plng;
        o.customer_lat = dlat;
        o.customer_lng = dlng;
        o.clone()
    }

    /// 把订单塞给骑手并标成"配送中"。
    fn mark_delivering(w: &mut World, rid: RiderId, oid: OrderId) {
        let cur = w.rider(rid).expect("骑手在").lat;
        let o = w.order_mut(oid).expect("单在");
        o.status = OrderStatus::PickedUp;
        o.assigned_to = Some(rid);
        let _ = cur;
    }

    fn marks_of(
        w: &World,
        rid: RiderId,
        hall: &[Order],
    ) -> std::collections::HashMap<OrderId, OrderMarks> {
        let rider = w.rider(rid).expect("骑手在");
        marks_for_hall(w, rider, hall).into_iter().collect()
    }

    #[test]
    fn percent_zero_never_recommends() {
        let (mut w, rid) = setup(0);
        let hall: Vec<Order> = (0..20)
            .map(|i| {
                let lat = 30.0 + i as f64 * 0.001;
                order_xy(&mut w, lat, 114.0, lat + 0.01, 114.0)
            })
            .collect();
        let m = marks_of(&w, rid, &hall);
        assert!(m.values().all(|x| !x.recommended), "0% 时一张都不该被推荐");
    }

    #[test]
    fn percent_hundred_always_recommends() {
        let (mut w, rid) = setup(100);
        let hall: Vec<Order> = (0..20)
            .map(|i| {
                let lat = 30.0 + i as f64 * 0.001;
                order_xy(&mut w, lat, 114.0, lat + 0.01, 114.0)
            })
            .collect();
        let m = marks_of(&w, rid, &hall);
        assert!(m.values().all(|x| x.recommended), "100% 时每张都该被推荐");
    }

    #[test]
    fn recommend_is_stable_across_refreshes() {
        let (mut w, rid) = setup(50);
        let hall: Vec<Order> = (0..30)
            .map(|i| {
                let lat = 30.0 + i as f64 * 0.001;
                order_xy(&mut w, lat, 114.0, lat + 0.01, 114.0)
            })
            .collect();
        let a = marks_of(&w, rid, &hall);
        for _ in 0..5 {
            let b = marks_of(&w, rid, &hall);
            assert_eq!(a, b, "同样的输入必须得到同样的推荐结果，标签不能闪");
        }
    }

    #[test]
    fn recommend_needs_room() {
        // 满载的骑手不该被推荐——推荐了也接不了。
        let (mut w, rid) = setup(100);
        let hall = vec![order_xy(&mut w, 30.0, 114.0, 30.01, 114.0)];
        // 用 `cap_override = 0`（只把手上的送完、不再接新单）制造"满载不出力"，
        // 比手工往 held 里塞订单简单，也正好覆盖 `effective_max_held` 那条路径。
        let now_min = w.sim_minute as u64;
        let at_elapsed = w.elapsed_min;
        {
            let r = w.rider_mut(rid).unwrap();
            r.policy_override.cap_override = Some(0);
            r.policy_override.set_at_min = now_min;
            r.policy_override.set_at_elapsed = Some(at_elapsed);
        }
        assert_eq!(
            w.rider(rid).unwrap().effective_max_held(&w.cfg),
            0,
            "前提：上限为 0，等于满载"
        );
        let m = marks_of(&w, rid, &hall);
        assert!(
            !m[&hall[0].id].recommended,
            "满载时不该推荐（100% 概率也不推）"
        );
    }

    #[test]
    fn on_the_way_requires_picked_up() {
        let (mut w, rid) = setup(0);
        // 手上有一单被指派、但还没取货 —— 不构成"当前路线"。
        let cur = order_xy(&mut w, 30.0, 114.0, 30.02, 114.0);
        {
            let o = w.order_mut(cur.id).unwrap();
            o.status = OrderStatus::Assigned;
            o.assigned_to = Some(rid);
        }
        let hall = vec![order_xy(&mut w, 30.001, 114.0, 30.03, 114.0)];
        let m = marks_of(&w, rid, &hall);
        assert_eq!(
            m[&hall[0].id].on_the_way_pct, None,
            "只有 assigned 不算配送中，顺路度必须是 None"
        );
    }

    #[test]
    fn on_the_way_none_without_delivering() {
        let (mut w, rid) = setup(0);
        let hall = vec![order_xy(&mut w, 30.0, 114.0, 30.01, 114.0)];
        let m = marks_of(&w, rid, &hall);
        assert_eq!(m[&hall[0].id].on_the_way_pct, None);
        assert!(!m[&hall[0].id].same_pickup);
        assert!(!m[&hall[0].id].same_dropoff);
    }

    #[test]
    fn on_the_way_is_integer_and_bounded() {
        let (mut w, rid) = setup(0);
        let rlat = w.rider(rid).unwrap().lat;
        let rlng = w.rider(rid).unwrap().lng;
        // 手上这单：从骑手旁边取、送到 2km 外。
        let cur = order_xy(&mut w, rlat, rlng, rlat + 0.018, rlng);
        mark_delivering(&mut w, rid, cur.id);
        // 新单沿着去往同一个方向：应当很顺。
        let hall = vec![order_xy(&mut w, rlat + 0.002, rlng, rlat + 0.010, rlng)];
        let m = marks_of(&w, rid, &hall);
        let pct = m[&hall[0].id].on_the_way_pct.expect("顺路单应当有百分比");
        assert!(pct <= 100, "百分比不能超过 100");
        // 顺路度高才符合"沿着同一方向"的构造。
        assert!(pct >= ON_THE_WAY_MIN_PCT, "同向的新单应当打过标签门槛");
    }

    #[test]
    fn reverse_direction_is_not_on_the_way() {
        let (mut w, rid) = setup(0);
        let rlat = w.rider(rid).unwrap().lat;
        let rlng = w.rider(rid).unwrap().lng;
        // 手上一单要往北送。
        let cur = order_xy(&mut w, rlat, rlng, rlat + 0.02, rlng);
        mark_delivering(&mut w, rid, cur.id);
        // 新单要往南取、再往南送 —— 完全反向。
        let hall = vec![order_xy(&mut w, rlat - 0.03, rlng, rlat - 0.05, rlng)];
        let m = marks_of(&w, rid, &hall);
        assert_eq!(
            m[&hall[0].id].on_the_way_pct, None,
            "反向的单不该挂顺路标签"
        );
    }

    #[test]
    fn same_pickup_and_dropoff_are_independent() {
        let (mut w, rid) = setup(0);
        let rid_lat = w.rider(rid).unwrap().lat;
        let rid_lng = w.rider(rid).unwrap().lng;
        let cur = order_xy(&mut w, rid_lat + 0.001, rid_lng, rid_lat + 0.02, rid_lng);
        mark_delivering(&mut w, rid, cur.id);
        let cur_merchant = w.order(cur.id).unwrap().merchant_id;

        // 新单：同商家（同取货），但送货点 5km 外（不同送）。
        let mut a = order_xy(&mut w, rid_lat + 0.001, rid_lng, rid_lat + 0.06, rid_lng);
        {
            let o = w.order_mut(a.id).unwrap();
            o.merchant_id = cur_merchant;
        }
        a.merchant_id = cur_merchant;

        // 新单：不同商家，但送货点几乎重合（同送）。
        let b = order_xy(&mut w, rid_lat - 0.01, rid_lng, rid_lat + 0.0201, rid_lng);

        let hall = vec![a.clone(), b.clone()];
        let m = marks_of(&w, rid, &hall);

        assert!(m[&a.id].same_pickup, "同商家 → 同取货地点");
        assert!(!m[&a.id].same_dropoff, "送货点差 5km → 非同送货");

        assert!(!m[&b.id].same_pickup, "不同商家 → 非同取货");
        assert!(
            m[&b.id].same_dropoff,
            "送货点相差约 20m（< 50m 容差）→ 同送货"
        );
    }

    #[test]
    fn missing_coords_do_not_fake_a_score() {
        let (mut w, rid) = setup(0);
        let rlat = w.rider(rid).unwrap().lat;
        let cur = order_xy(&mut w, rlat, 114.0, rlat + 0.02, 114.0);
        mark_delivering(&mut w, rid, cur.id);

        // 新单送货坐标是 NaN → 算不出距离，不能编一个分数。
        let mut bad = order_xy(&mut w, rlat, 114.0, rlat + 0.01, 114.0);
        {
            let o = w.order_mut(bad.id).unwrap();
            o.customer_lat = f64::NAN;
        }
        bad.customer_lat = f64::NAN;

        let m = marks_of(&w, rid, &[bad.clone()]);
        assert_eq!(
            m[&bad.id].on_the_way_pct, None,
            "缺坐标时必须返回 None，不能伪造顺路度"
        );
    }

    #[test]
    fn marks_do_not_bypass_radius() {
        // 推荐/顺路都在"已通过半径"的列表上算。这条守的是调用约定：
        // 传进来的 hall 一旦包含范围外的单，函数本身不会帮忙挡。
        // 真正的挡板在 `render_snapshot`（先 retain 再算 marks）。
        let (mut w, rid) = setup(100);
        let far = order_xy(&mut w, 40.0, 120.0, 40.01, 120.0);
        let rider = w.rider(rid).expect("骑手在").clone();
        assert!(
            !order_in_range(&rider, &far, w.cfg.dispatch_radius_km, w.wall_ms),
            "前提：这单在半径外"
        );
    }
}
