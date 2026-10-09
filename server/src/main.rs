//! 服务入口：读配置 → 读剧本 → 建世界 → 起主循环。
//!
//! 这一层**只做装配**，不含任何业务规则。所有环境变量都有合理默认值，
//! 因此 `cargo run` 裸跑也能起服务（用合成数据 + 内存库）。
//!
//! ## 环境变量
//!
//! | 变量 | 默认 | 说明 |
//! |---|---|---|
//! | `RIDER_SIM_ADDR` | `127.0.0.1:8080` | 监听地址 |
//! | `RIDER_SIM_DB` | `rider_sim.db` | SQLite 路径；打不开自动降级内存库 |
//! | `RIDER_SIM_DATA` | 自动查找 `data/` | 剧本目录 |
//! | `RIDER_SIM_SEED` | 内置 | 随机种子 |
//! | `RIDER_SIM_ALLOW_CONTROL` | `false` | 无 token 的连接能否拿控制权（WS 与 HTTP 共用） |
//! | `RIDER_SIM_CONTROL_TOKEN` | 无 | 控制端令牌。WS 用 `hello.token = "control:<令牌>"`；HTTP 用 `Authorization: Bearer <令牌>` |
//! | `RIDER_SIM_LLM` | `false` | 是否启用 LLM（无 Key 时自动关闭） |
//! | `RIDER_SIM_MAX_CONNECTIONS` | `64` | 同时在线上限 |
//! | `RUST_LOG` | `info` | 日志级别 |

use std::net::SocketAddr;
use std::path::PathBuf;

use rider_sim::app::{App, AppOptions};
use rider_sim::content;
use rider_sim::sim::{config::SimConfig, world::World};
use rider_sim::store;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    init_tracing();

    // ---------- 1. 配置 ----------
    let mut cfg = SimConfig::default();
    if let Some(seed) = env_parse::<u64>("RIDER_SIM_SEED") {
        cfg.seed = seed;
    }

    // ---------- 2. 剧本 ----------
    let dir = std::env::var_os("RIDER_SIM_DATA").map(PathBuf::from);
    let (content, warnings) = content::load(dir.as_deref(), cfg.seed);
    for w in &warnings {
        tracing::warn!("剧本：{w}");
    }
    tracing::info!(
        source = %content.source,
        merchants = content.merchants.len(),
        located = content.located_merchants(),
        tasks = content.tasks.len(),
        weather_events = content.weather_script.len(),
        "剧本已加载"
    );

    // ---------- 3. 世界 ----------
    let world = World::new(
        cfg,
        content.merchants,
        content.tasks,
        content.weather_script,
    );

    // ---------- 4. 持久化（失败不挡启动）----------
    let db_path = std::env::var("RIDER_SIM_DB").unwrap_or_else(|_| "rider_sim.db".into());
    let (store, db_warn) = store::open_or_memory(&db_path);
    if let Some(w) = db_warn {
        tracing::warn!("{w}");
    }

    // ---------- 5. 运行选项 ----------
    let opts = AppOptions {
        allow_control: env_flag("RIDER_SIM_ALLOW_CONTROL", false),
        control_token: std::env::var("RIDER_SIM_CONTROL_TOKEN")
            .ok()
            .filter(|t| !t.trim().is_empty()),
        max_connections: env_parse("RIDER_SIM_MAX_CONNECTIONS").unwrap_or(64),
        llm_enabled: env_flag("RIDER_SIM_LLM", false),
        ..AppOptions::default()
    };

    // ---------- 6. 监听地址 ----------
    let addr: SocketAddr = std::env::var("RIDER_SIM_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()
        .expect("RIDER_SIM_ADDR 不是合法地址（形如 0.0.0.0:8080）");

    // ---------- 7. 起服务 ----------
    App::new(world, store, opts).serve(addr).await
}

fn init_tracing() {
    let level = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into());
    let filter = tracing_subscriber::EnvFilter::try_new(&level)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

/// 读一个环境变量并解析；缺失或解析失败都返回 `None`（走默认值）。
fn env_parse<T: std::str::FromStr>(key: &str) -> Option<T> {
    let raw = std::env::var(key).ok()?;
    match raw.trim().parse::<T>() {
        Ok(v) => Some(v),
        Err(_) => {
            tracing::warn!("环境变量 {key}={raw} 解析失败，改用默认值");
            None
        }
    }
}

/// 布尔开关：`1/true/yes/on` 为真，其余为假。
fn env_flag(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => default,
    }
}
