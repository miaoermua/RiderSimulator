//! 配送站与站点 KPI（《三层AI架构设计》§4.1）。
//!
//! 现状是**单站点**：全城骑手归一个站。但数据结构按多站点设计（`riders` 列表），
//! 以后切成多站点不需要改调用方。
//!
//! KPI 是站长 AI 的**优化目标**，也是控制台「站长在为什么拼命」的解释。
//! 它必须是多目标的——只看准时率，站长会疯狂限单把骑手饿死。

use serde::Serialize;

use super::{RiderId, order::OrderStatus, world::World};

pub type StationId = u32;

/// 配送站。站长 AI 的管理单元。
#[derive(Debug, Clone, Serialize)]
pub struct Station {
    pub id: StationId,
    pub name: String,
    pub center_lat: f64,
    pub center_lng: f64,
    pub radius_m: u32,
    /// 归属骑手。当前只有一个站点时是全量。
    pub rider_ids: Vec<RiderId>,
}

impl Station {
    /// 当前世界默认的那个站：全城一个站，圆心取商家质心，半径覆盖所有商家。
    ///
    /// 纯函数、只看世界，**不引入随机数**——同一个世界永远给出同一个站。
    pub fn for_world(world: &World) -> Self {
        let pts: Vec<super::geo::LatLng> =
            world.merchants.iter().filter_map(|m| m.coords()).collect();
        let (center_lat, center_lng) = if pts.is_empty() {
            (0.0, 0.0)
        } else {
            let n = pts.len() as f64;
            (
                pts.iter().map(|p| p.lat).sum::<f64>() / n,
                pts.iter().map(|p| p.lng).sum::<f64>() / n,
            )
        };

        // 半径 = 圆心到最远商家的距离 × 1.2，至少 500m。
        let center = super::geo::LatLng::new(center_lat, center_lng);
        let radius_m = pts
            .iter()
            .map(|p| super::geo::haversine_m(center, *p))
            .fold(0.0_f64, f64::max);
        let radius_m = (radius_m * 1.2).max(500.0) as u32;

        Self {
            id: 1,
            name: "江城配送站".to_string(),
            center_lat,
            center_lng,
            radius_m,
            rider_ids: world.riders.iter().map(|r| r.id).collect(),
        }
    }
}

/// 站点 KPI。**这是站长 AI 的优化目标**，也是它的评分依据。
///
/// 三个目标互相拉扯：准时率 ↑ 会诱惑站长限单；履约率 ↑ 会诱惑强派；
/// 骑手收入 ↑ 会诱惑无限接单。代码把三个都摆出来，站长 AI 才能权衡。
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct StationKpi {
    /// 今日完成单量。
    pub completed: u32,
    /// 今日超时单量。
    pub late_count: u32,
    /// 准时率 0~1。站长最核心的 KPI。没有完成单时记为 1.0（没超时过）。
    pub on_time_rate: f32,
    /// 平均时薪（分）。太低说明骑手赚不到钱，会流失。
    pub avg_hourly_cents: i64,
    /// 活跃骑手数 / 总骑手数。
    pub active_riders: u32,
    pub total_riders: u32,
    /// 待处理订单数（无人接的）。
    pub backlog: u32,
    /// 平均等餐分钟（大厅里积压单的平均等待时长）。
    pub avg_wait_min: f32,
}

impl StationKpi {
    /// 按站点归属的骑手 + 当下世界算一份 KPI。
    ///
    /// 全部取自**当日计数**（`Rider::completed` / `late_count` / `online_minutes`），
    /// 这些字段在日结时清零，所以天然就是「今日」口径。
    pub fn compute(world: &World, station: &Station) -> Self {
        let mut completed = 0u32;
        let mut late_count = 0u32;
        let mut active_riders = 0u32;
        let mut total_riders = 0u32;
        let mut hourly_sum = 0i64;
        let mut hourly_n = 0i64;

        for &id in &station.rider_ids {
            let Some(r) = world.rider(id) else { continue };
            total_riders += 1;
            completed += r.completed;
            late_count += r.late_count;
            if r.online {
                active_riders += 1;
            }
            // 只统计「跑过一段时间的在线骑手」，刚上线的时薪没有意义。
            if r.online_minutes > 0 {
                hourly_sum += r.hourly_wage_cents();
                hourly_n += 1;
            }
        }

        let backlog = world
            .orders
            .iter()
            .filter(|o| o.status == OrderStatus::Pending)
            .count() as u32;
        // 积压单在大厅的平均等待时长：站长最该看的数字。
        let wait_sum: u64 = world
            .orders
            .iter()
            .filter(|o| o.status == OrderStatus::Pending)
            .map(|o| world.elapsed_min.saturating_sub(o.created_at_min))
            .sum();
        let avg_wait_min = if backlog == 0 {
            0.0
        } else {
            wait_sum as f32 / backlog as f32
        };

        let on_time_rate = if completed == 0 {
            1.0
        } else {
            (completed.saturating_sub(late_count)) as f32 / completed as f32
        };

        Self {
            completed,
            late_count,
            on_time_rate,
            avg_hourly_cents: if hourly_n == 0 {
                0
            } else {
                hourly_sum / hourly_n
            },
            active_riders,
            total_riders,
            backlog,
            avg_wait_min,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::config::SimConfig;

    fn world() -> World {
        World::new_for_test(SimConfig::default(), 7)
    }

    #[test]
    fn station_covers_every_rider() {
        let w = world();
        let s = Station::for_world(&w);
        assert_eq!(s.rider_ids.len(), w.riders.len());
        assert!(s.radius_m >= 500, "半径至少有下限");
        assert!(s.center_lat.abs() > 0.0, "圆心应落在商家质心上");
    }

    #[test]
    fn empty_world_gives_zeroed_kpi() {
        let mut w = world();
        for r in &mut w.riders {
            r.completed = 0;
            r.late_count = 0;
        }
        let s = Station::for_world(&w);
        let kpi = StationKpi::compute(&w, &s);
        assert_eq!(kpi.completed, 0);
        assert_eq!(kpi.on_time_rate, 1.0, "没完成单就没有超时");
        assert_eq!(kpi.total_riders, w.riders.len() as u32);
    }

    #[test]
    fn kpi_tracks_late_rate_and_backlog() {
        let mut w = world();
        // 造 4 单完成、1 单超时 → 准时率 0.75。
        w.riders[0].completed = 4;
        w.riders[0].late_count = 1;
        let s = Station::for_world(&w);
        let kpi = StationKpi::compute(&w, &s);
        assert_eq!(kpi.completed, 4);
        assert_eq!(kpi.late_count, 1);
        assert!((kpi.on_time_rate - 0.75).abs() < 1e-6);

        // 大厅里投一批单 → backlog 与等待时长都大于 0。
        w.generate_order_batch(6);
        let kpi = StationKpi::compute(&w, &s);
        assert_eq!(kpi.backlog, 6);
        w.elapsed_min += 10;
        let later = StationKpi::compute(&w, &s);
        assert!(later.avg_wait_min > kpi.avg_wait_min, "等得越久均值越大");
    }

    #[test]
    fn kpi_is_deterministic() {
        let w = world();
        let s = Station::for_world(&w);
        assert_eq!(
            StationKpi::compute(&w, &s),
            StationKpi::compute(&w, &s),
            "同一世界必须给出同一份 KPI"
        );
    }
}
