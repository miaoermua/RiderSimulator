//! 首页通知：烽火台写给骑手的一句话。
//!
//! 只有**一条**：改了就覆盖，删了就空。骑手首页那条通知就是它，点开看全文；
//! 可以关联一个平台活动，点开时顺带展示「我的进度」。
//!
//! 为什么不支持多条 / 轮播：手机上就一条窄条子，两条只会互相盖住；
//! 管理员想换话术，直接改这条即可，不需要历史记录。

use serde::{Deserialize, Serialize};

use super::world::World;

/// 单条通知。全服同一条，改了就覆盖。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notice {
    pub title: String,
    pub body: String,
    /// 关联的平台活动（可选）。手机端点开通知时附上这个活动的进度。
    pub activity_id: Option<u32>,
    /// 最后一次修改的模拟分钟（自开局起算），只用于展示。
    pub updated_at_min: u64,
}

/// 写入（覆盖）通知。参数合法性由调用方（HTTP 层）保证。
pub fn set(
    world: &mut World,
    title: impl Into<String>,
    body: impl Into<String>,
    activity_id: Option<u32>,
) -> Notice {
    let notice = Notice {
        title: title.into(),
        body: body.into(),
        activity_id,
        updated_at_min: world.elapsed_min,
    };
    world.notice = Some(notice.clone());
    notice
}

/// 清空通知。返回原本是否有过。
pub fn clear(world: &mut World) -> bool {
    world.notice.take().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::config::SimConfig;

    #[test]
    fn setting_twice_keeps_only_the_latest() {
        let mut w = World::new_for_test(SimConfig::default(), 7);
        set(&mut w, "第一版", "旧话术", None);
        let n = set(&mut w, "冲单奖", "跑满 10 单再奖 10 元", Some(3));

        assert_eq!(n.title, "冲单奖");
        let stored = w.notice.as_ref().expect("通知应已写入");
        assert_eq!(stored.body, "跑满 10 单再奖 10 元");
        assert_eq!(stored.activity_id, Some(3));
    }

    #[test]
    fn clearing_removes_it_and_reports_whether_it_existed() {
        let mut w = World::new_for_test(SimConfig::default(), 7);
        assert!(!clear(&mut w), "本来就没有，不该谎报删掉了");
        set(&mut w, "临时通知", "今晚暴雨", None);
        assert!(clear(&mut w));
        assert!(w.notice.is_none());
    }

    #[test]
    fn the_timestamp_follows_the_sim_clock_not_the_wall_clock() {
        let mut w = World::new_for_test(SimConfig::default(), 7);
        w.elapsed_min = 321;
        let n = set(&mut w, "六点开工", "记得看群消息", None);
        assert_eq!(n.updated_at_min, 321);
    }
}
