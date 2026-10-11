//! 分层守卫：`sim/` 的两条硬规矩，靠 `cargo test` 兜着。
//!
//! `src/lib.rs` 里那张分层图说了两件事——`sim/` **零 IO**、且**不依赖 `human/`**。
//! 写在文档里的约束没人会记得，所以让它们在测试里红：判错了就是分层失效，
//! 而分层失效不会报错，只会慢慢烂掉。
//!
//! 这里读的是**源码文件**，所以它只能在 `tests/` 里做（`sim/` 自己不许碰 `fs`）。

use std::{
    fs,
    path::{Path, PathBuf},
};

/// 递归收集 `src/sim/` 下的 `.rs`，返回 `(路径, 内容)`。
fn sim_sources() -> Vec<(PathBuf, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sim");
    let mut out = Vec::new();
    let mut stack = vec![root];

    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => panic!("读不到 {}：{e}", dir.display()),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension() == Some(std::ffi::OsStr::new("rs")) {
                let text = fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("读不到 {}：{e}", path.display()));
                out.push((path, text));
            }
        }
    }

    assert!(!out.is_empty(), "一个 sim 源文件都没读到，路径大概写错了");
    out
}

/// 逐行找 `needle`，跳过注释行（文档里提到这些名字是允许的，代码里才算违规）。
fn offenders(needle: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for (path, text) in sim_sources() {
        for (i, line) in text.lines().enumerate() {
            // `//`、`///`、`//!` 一律算注释。sim 里没有 `//` 出现在字符串里的用法。
            let code = line.split("//").next().unwrap_or("");
            if code.contains(needle) {
                hits.push(format!("  {}:{}  {}", path.display(), i + 1, line.trim()));
            }
        }
    }
    hits
}

/// 规则一：`sim/` 不许反向依赖 `human/`。
///
/// `human → sim` 是单向的：真人的判定（在线与否、AI 要不要让位）集中在 `human/`，
/// `sim/` 只收结论（`TickInput::hosted_humans` / `TickInput::ai_off`）。
#[test]
fn sim_never_depends_on_human() {
    let hits = offenders("crate::human");
    let hits: Vec<String> = hits.into_iter().chain(offenders("human::")).collect();

    assert!(
        hits.is_empty(),
        "sim/ 里出现了对 human/ 的引用——分层反了。\n\
         改法是把判定结果当参数传进来，而不是在 sim 里问「谁是真人」：\n{}",
        hits.join("\n")
    );
}

/// 规则二：`sim/` 里不许有 IO 和系统时间。
///
/// 唯一的随机来源是 `World::rng`，唯一的时间来源是 `World::tick_count`。
/// 破了这条，同一个种子就跑不出同一份结果——而整个项目的回归网都建在这上面。
#[test]
fn sim_has_no_io_and_no_wall_clock() {
    let banned = [
        "std::fs",
        "std::net",
        "std::time::SystemTime",
        "SystemTime::now",
        "Instant::now",
        "reqwest",
        "tokio::net",
        "println!",
        "eprintln!",
        "tracing::",
        "log::",
        "thread::sleep",
    ];

    let hits: Vec<String> = banned.iter().flat_map(|needle| offenders(needle)).collect();

    assert!(
        hits.is_empty(),
        "sim/ 里出现了 IO 或系统时间——这一层必须零 IO、同种子可复现：\n{}",
        hits.join("\n")
    );
}
