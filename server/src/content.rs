//! 启动时把 `data/` 下的剧本读进来。
//!
//! ## 为什么单独一层
//!
//! `sim/` 里禁止任何 IO（读文件也算），所以「读 json」这件事必须发生在外圈。
//! 这一层只做一件事：**把文件变成 `Vec<Merchant>` / `Vec<TaskTemplate>` /
//! `Vec<WeatherEvent>`，然后交出去**。读不到就合成一份兜底，服务照常能起。
//!
//! 查找顺序：
//!
//! 1. `RIDER_SIM_DATA` 环境变量指定的目录
//! 2. 当前工作目录下的 `data/`
//! 3. 编译期 `CARGO_MANIFEST_DIR/data/`（`cargo run` 时的常规情况）
//!
//! 三个都找不到就用 [`crate::sim::world::synth_merchants`] 合成，
//! 并把原因记在返回的警告里。

use std::path::{Path, PathBuf};

use crate::sim::{
    merchant::{Merchant, MerchantFile},
    task::{TaskFile, TaskTemplate},
    weather::{WeatherEvent, WeatherScript},
    world::{synth_merchants, synth_task},
};

/// 一副剧本。
#[derive(Debug, Clone, Default)]
pub struct Content {
    pub merchants: Vec<Merchant>,
    pub tasks: Vec<TaskTemplate>,
    pub weather_script: Vec<WeatherEvent>,
    /// 数据来源（真实目录 / `合成`），给日志用。
    pub source: String,
}

/// 读剧本。**永不失败**：读不到就合成兜底。
///
/// 第二个返回值是给人看的警告列表，调用方自己决定打不打日志。
pub fn load(dir: Option<&Path>, seed: u64) -> (Content, Vec<String>) {
    let mut warnings = Vec::new();

    let resolved = dir.map(Path::to_path_buf).or_else(default_dir);
    let dir = match resolved {
        Some(d) if d.is_dir() => d,
        Some(d) => {
            warnings.push(format!(
                "剧本目录 {} 不存在，改用合成数据（仅适合开发）",
                d.display()
            ));
            return (synthesize(seed), warnings);
        }
        None => {
            warnings.push("找不到 data/ 目录，改用合成数据（仅适合开发）".to_string());
            return (synthesize(seed), warnings);
        }
    };

    let merchants = match read_merchants(&dir.join("merchants.json")) {
        Ok(v) if !v.is_empty() => v,
        Ok(_) => {
            warnings.push(format!(
                "{}/merchants.json 是空的，改用合成商户",
                dir.display()
            ));
            synth_merchants(seed, 40)
        }
        Err(e) => {
            warnings.push(format!("读 merchants.json 失败（{e}），改用合成商户"));
            synth_merchants(seed, 40)
        }
    };

    let tasks = match read_tasks(&dir.join("tasks.json")) {
        Ok(v) if !v.is_empty() => v,
        Ok(_) => {
            warnings.push(format!("{}/tasks.json 是空的，改用内置任务", dir.display()));
            vec![synth_task()]
        }
        Err(e) => {
            warnings.push(format!("读 tasks.json 失败（{e}），改用内置任务"));
            vec![synth_task()]
        }
    };

    let weather_script = match read_weather(&dir.join("weather_script.json")) {
        Ok(v) => v,
        Err(e) => {
            // 天气剧本缺失只是「没有安排好的天气变化」，不影响可玩性
            warnings.push(format!(
                "读 weather_script.json 失败（{e}），本局天气保持晴朗"
            ));
            Vec::new()
        }
    };

    let content = Content {
        merchants,
        tasks,
        weather_script,
        source: dir.display().to_string(),
    };
    warnings.extend(validate(&content));
    (content, warnings)
}

/// 合成一副可玩的剧本（无文件环境）。
pub fn synthesize(seed: u64) -> Content {
    Content {
        merchants: synth_merchants(seed, 40),
        tasks: vec![synth_task()],
        weather_script: Vec::new(),
        source: "合成".into(),
    }
}

impl Content {
    /// 有没有商户真的解析出了坐标。全都没坐标的话，整局单都发不出来。
    pub fn located_merchants(&self) -> usize {
        self.merchants
            .iter()
            .filter(|m| m.coords().is_some())
            .count()
    }
}

fn default_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RIDER_SIM_DATA") {
        let p = PathBuf::from(p);
        if p.is_dir() {
            return Some(p);
        }
    }
    let cwd = PathBuf::from("data");
    if cwd.is_dir() {
        return Some(cwd);
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data");
    if manifest.is_dir() {
        return Some(manifest);
    }
    None
}

fn read_merchants(path: &Path) -> Result<Vec<Merchant>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let file: MerchantFile = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(file.merchants)
}

fn read_tasks(path: &Path) -> Result<Vec<TaskTemplate>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let file: TaskFile = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(file.tasks)
}

fn read_weather(path: &Path) -> Result<Vec<WeatherEvent>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let file: WeatherScript = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(file.events)
}

/// 数据体检。**把明显会坑人的数据在启动时就喊出来**，别等跑到中午才发现。
fn validate(c: &Content) -> Vec<String> {
    let mut w = Vec::new();

    let located = c.located_merchants();
    if located == 0 {
        w.push("没有任何商户带坐标：订单无法生成，请先跑一次地理编码".to_string());
    } else if located * 2 < c.merchants.len() {
        w.push(format!(
            "只有 {located}/{} 家商户有坐标，单量会明显偏少",
            c.merchants.len()
        ));
    }

    for t in &c.tasks {
        if t.steps.is_empty() {
            w.push(format!("任务 `{}` 没有任何步骤，永远不会完成", t.id));
            continue;
        }
        for (i, s) in t.steps.iter().enumerate() {
            if s.completion_keywords.is_empty() {
                w.push(format!(
                    "任务 `{}` 第 {} 步没有完成关键词，只能靠 LLM 判定",
                    t.id,
                    i + 1
                ));
            }
        }
    }

    // 天气剧本必须按时间递增，否则脚本指针会跳过事件。
    for pair in c.weather_script.windows(2) {
        if pair[1].at <= pair[0].at {
            w.push(format!(
                "天气剧本时刻不是递增的（{} <= {}），部分天气会被跳过",
                pair[1].at, pair[0].at
            ));
            break;
        }
    }
    for e in &c.weather_script {
        if e.at >= 1440 {
            w.push(format!("天气事件时刻 {} 超出一天（0..1439）", e.at));
            break;
        }
    }

    w
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::weather::Weather;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("rider_sim_content_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn missing_dir_falls_back_to_synthesized_data_with_a_warning() {
        let missing = std::env::temp_dir().join("rider_sim_definitely_not_here_xyz");
        let (c, w) = load(Some(&missing), 42);
        assert!(!c.merchants.is_empty());
        assert!(!c.tasks.is_empty());
        assert!(!w.is_empty(), "降级必须留痕");
        assert_eq!(c.source, "合成");
    }

    #[test]
    fn reads_a_real_data_directory() {
        let dir = tmp_dir("real");
        std::fs::write(
            dir.join("merchants.json"),
            r#"{"city":"武汉","merchants":[
                {"id":1,"name":"老王热干面","city":"武汉","address":"光谷","lat":30.51,"lng":114.41,"rating":4.7,"avg_prep_min":10,"signature":"热干面"},
                {"id":2,"name":"张记烧烤","city":"武汉","address":"鲁巷","lat":30.52,"lng":114.42,"rating":4.5,"avg_prep_min":15,"signature":"烤串"}
            ]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("tasks.json"),
            r#"{"tasks":[{"id":"t1","name":"放门口","steps":[{"rider_prompt":"放哪","completion_keywords":["门口"]}]}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("weather_script.json"),
            r#"{"day":"随机","events":[{"at":600,"to":"heavy_rain"},{"at":900,"to":"clear"}]}"#,
        )
        .unwrap();

        let (c, w) = load(Some(&dir), 1);
        assert_eq!(c.merchants.len(), 2);
        assert_eq!(c.merchants[0].name, "老王热干面");
        assert_eq!(c.located_merchants(), 2);
        assert_eq!(c.tasks.len(), 1);
        assert_eq!(c.weather_script.len(), 2);
        assert_eq!(c.weather_script[0].to, Weather::HeavyRain);
        assert!(w.is_empty(), "干净数据不该有警告：{w:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_data_still_starts_and_reports_what_is_missing() {
        let dir = tmp_dir("partial");
        std::fs::write(
            dir.join("merchants.json"),
            r#"{"city":"武汉","merchants":[
                {"id":1,"name":"没坐标的店","city":"武汉","address":"?","lat":null,"lng":null,"rating":4.0,"avg_prep_min":8,"signature":"x"}
            ]}"#,
        )
        .unwrap();

        let (c, w) = load(Some(&dir), 1);
        assert_eq!(c.merchants.len(), 1);
        assert!(!c.tasks.is_empty(), "任务缺失要兜底");
        assert!(w.iter().any(|m| m.contains("没有任何商户带坐标")), "{w:?}");
        assert!(w.iter().any(|m| m.contains("tasks.json")), "{w:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_json_does_not_panic() {
        let dir = tmp_dir("bad");
        std::fs::write(dir.join("merchants.json"), "{ not json at all").unwrap();
        let (c, w) = load(Some(&dir), 3);
        assert!(!c.merchants.is_empty(), "必须兜底");
        assert!(w.iter().any(|m| m.contains("merchants.json")), "{w:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_catches_a_non_monotonic_weather_script() {
        let mut c = synthesize(1);
        c.weather_script = vec![
            WeatherEvent {
                at: 900,
                to: Weather::HeavyRain,
            },
            WeatherEvent {
                at: 600,
                to: Weather::Snow,
            },
        ];
        let w = validate(&c);
        assert!(w.iter().any(|m| m.contains("递增")), "{w:?}");
    }

    #[test]
    fn validate_catches_steps_without_keywords() {
        let mut c = synthesize(1);
        c.tasks[0].steps[0].completion_keywords.clear();
        let w = validate(&c);
        assert!(w.iter().any(|m| m.contains("完成关键词")), "{w:?}");
    }

    #[test]
    fn real_project_data_directory_is_usable() {
        // 刻意不传 dir，走 default_dir：仓库里带了 data/，必须真的能读出来。
        let (c, w) = load(None, 9);
        assert!(
            c.located_merchants() > 0,
            "仓库自带的 data/ 应该有带坐标的商户；警告：{w:?}"
        );
    }
}
