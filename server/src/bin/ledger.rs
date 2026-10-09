//! 账单 CLI——一天一条命令看完。
//!
//! 对应技术设计 §9 阶段一硬指标第 5 条：「能用一条 CLI 命令打印出完整的一天流水」，
//! 也让 `docs/` 里那些指标不用开控制台就能核对。
//!
//! ```text
//! cargo run --bin ledger -- days             # 库里有哪些天
//! cargo run --bin ledger -- day 3            # 第 3 天的完整流水
//! cargo run --bin ledger -- day 3 --json     # 给脚本吃
//! cargo run --bin ledger -- top 20           # 生涯排行榜
//! ```
//!
//! 数据库路径取 `--db`，没给就读 `RIDER_SIM_DB`，再没有就用 `rider_sim.db`。
//! **只读**：除了 `Store::open` 会建表，这个程序不写任何数据。

use rider_sim::sim::event::DayLedger;
use rider_sim::store::{Career, Store};

const DEFAULT_DB: &str = "rider_sim.db";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("错误：{e}");
            std::process::exit(1);
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let mut db = std::env::var("RIDER_SIM_DB").unwrap_or_else(|_| DEFAULT_DB.to_string());
    let mut json = false;
    let mut rest: Vec<&str> = Vec::new();

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--db" => {
                db = it
                    .next()
                    .ok_or_else(|| "--db 后面要跟路径".to_string())?
                    .clone();
            }
            "--json" => json = true,
            "-h" | "--help" | "help" => return usage(None),
            other => rest.push(other),
        }
    }

    let Some(cmd) = rest.first() else {
        return usage(None);
    };

    // 只在认识了子命令之后才碰数据库——否则 `ledger help` 会在工作目录里
    // 凭空建一个 rider_sim.db。
    let open = || Store::open(&db).map_err(|e| format!("打开 {db} 失败：{e}"));

    match *cmd {
        "days" => {
            let store = open()?;
            let days = store.days().map_err(|e| e.to_string())?;
            if json {
                println!("{}", serde_json::to_string(&days).unwrap());
            } else if days.is_empty() {
                println!("{db} 里还没有任何流水。");
            } else {
                println!("{db} 里有流水的天：{days:?}");
            }
        }
        "day" => {
            let store = open()?;
            let day: u32 = rest
                .get(1)
                .ok_or_else(|| "用法：day <天数>".to_string())?
                .parse()
                .map_err(|_| "天数必须是整数".to_string())?;
            let ledgers = store.load_day(day).map_err(|e| e.to_string())?;

            if ledgers.is_empty() {
                return Err(format!("第 {day} 天没有流水"));
            }
            if json {
                println!("{}", serde_json::to_string(&ledgers).unwrap());
            } else {
                print!("{}", render_day(day, &ledgers));
            }
        }
        "top" => {
            let store = open()?;
            let limit: u32 = match rest.get(1) {
                Some(s) => s.parse().map_err(|_| "人数必须是整数".to_string())?,
                None => 10,
            };
            let board = store.leaderboard(limit).map_err(|e| e.to_string())?;
            if json {
                println!("{}", serde_json::to_string(&board).unwrap());
            } else {
                print!("{}", render_top(&board));
            }
        }
        other => return usage(Some(other)),
    }
    Ok(())
}

fn usage(bad: Option<&str>) -> Result<(), String> {
    if let Some(b) = bad {
        eprintln!("不认识的子命令：{b}\n");
    }
    println!(
        "账单 CLI\n\
         \n\
         用法：\n\
         \x20 ledger [--db <路径>] days              列出有流水的天\n\
         \x20 ledger [--db <路径>] day <天数> [--json]  打印某一天完整流水\n\
         \x20 ledger [--db <路径>] top [人数] [--json]  生涯排行榜（默认 10）\n\
         \n\
         数据库路径缺省读环境变量 RIDER_SIM_DB，再退到 {DEFAULT_DB}。"
    );
    Ok(())
}

/// 元 → 「¥1.23」。全篇统一用这个，避免有的地方手写除以 100。
fn money(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let c = cents.abs();
    format!("{sign}¥{}.{:02}", c / 100, c % 100)
}

fn render_day(day: u32, ledgers: &[DayLedger]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "\n第 {day} 天流水（{} 名骑手）\n\n",
        ledgers.len()
    ));
    out.push_str(&format!(
        "{:<6}{:<10}{:>5}{:>5}{:>9}{:>11}{:>10}{:>10}{:>10}{:>7}{:>5}{:>10}\n",
        "key",
        "姓名",
        "单量",
        "迟到",
        "在线",
        "到手",
        "罚款",
        "保险",
        "时薪",
        "评分",
        "等级",
        "欠款"
    ));

    let mut total = DayLedgerTotals::default();
    for l in ledgers {
        total.add(l);
        out.push_str(&format!(
            "{:<6}{:<10}{:>5}{:>5}{:>8}m{:>11}{:>10}{:>10}{:>10}{:>7.1}{:>5}{:>10}\n",
            l.key,
            l.name,
            l.completed,
            l.late_count,
            l.online_minutes,
            money(l.takehome_cents),
            money(l.fines_cents),
            money(l.insurance_cents),
            money(l.hourly_wage_cents),
            l.rating,
            l.level,
            money(l.debt_cents),
        ));
    }

    out.push_str(&format!(
        "\n合计：{} 单，到手 {}，罚款 {}，保险 {}，在线 {} 分钟，迟到 {} 次\n",
        total.completed,
        money(total.takehome),
        money(total.fines),
        money(total.insurance),
        total.online_minutes,
        total.late,
    ));
    out
}

fn render_top(board: &[Career]) -> String {
    let mut out = String::new();
    out.push_str(&format!("\n生涯排行榜（前 {} 名）\n\n", board.len()));
    out.push_str(&format!(
        "{:>4}{:<6}{:<10}{:>5}{:>6}{:>11}{:>10}{:>7}{:>5}\n",
        "名次", "key", "姓名", "出勤", "单量", "累计到手", "罚款", "评分", "等级"
    ));
    for (i, c) in board.iter().enumerate() {
        out.push_str(&format!(
            "{:>4}{:<6}{:<10}{:>5}{:>6}{:>11}{:>10}{:>7.1}{:>5}\n",
            i + 1,
            c.rider_key,
            c.name,
            c.days,
            c.completed,
            money(c.takehome_cents),
            money(c.fines_cents),
            c.rating,
            c.level,
        ));
    }
    out
}

#[derive(Default)]
struct DayLedgerTotals {
    completed: u32,
    late: u32,
    online_minutes: u32,
    takehome: i64,
    fines: i64,
    insurance: i64,
}

impl DayLedgerTotals {
    fn add(&mut self, l: &DayLedger) {
        self.completed += l.completed;
        self.late += l.late_count;
        self.online_minutes += l.online_minutes;
        self.takehome += l.takehome_cents;
        self.fines += l.fines_cents;
        self.insurance += l.insurance_cents;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rider_sim::sim::RiderId;

    fn ledger(id: RiderId, key: &str, name: &str, completed: u32, takehome: i64) -> DayLedger {
        DayLedger {
            rider_id: id,
            key: key.into(),
            name: name.into(),
            takehome_cents: takehome,
            gross_cents: takehome + 100,
            fines_cents: 300,
            insurance_cents: 200,
            completed,
            late_count: 1,
            online_minutes: 480,
            hourly_wage_cents: 2280,
            rating: 4.8,
            level: 2,
            debt_cents: 0,
        }
    }

    fn temp_db(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("rider-ledger-test-{tag}-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn money_keeps_two_decimals_and_sign() {
        assert_eq!(money(0), "¥0.00");
        assert_eq!(money(7), "¥0.07");
        assert_eq!(money(1234), "¥12.34");
        assert_eq!(money(-5), "-¥0.05");
    }

    #[test]
    fn day_listing_and_ledger_round_trip() {
        let path = temp_db("roundtrip");
        let mut store = Store::open(&path).unwrap();
        assert!(store.days().unwrap().is_empty(), "空库不该报错，该给空列表");

        store
            .save_day(2, &[ledger(1, "r1", "AI-A", 12, 18240)])
            .unwrap();
        store
            .save_day(1, &[ledger(2, "r2", "老王", 9, 9010)])
            .unwrap();

        assert_eq!(store.days().unwrap(), vec![1, 2], "天要升序");
        let day2 = store.load_day(2).unwrap();
        assert_eq!(day2.len(), 1);
        assert_eq!(day2[0].name, "AI-A");

        // 重复写同一天是幂等的（save_day 的约定）
        store
            .save_day(2, &[ledger(1, "r1", "AI-A", 13, 20000)])
            .unwrap();
        assert_eq!(store.load_day(2).unwrap()[0].completed, 13);
        assert_eq!(store.days().unwrap(), vec![1, 2]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn day_render_shows_rows_and_totals() {
        let rows = [
            ledger(1, "r1", "AI-A", 12, 18240),
            ledger(2, "r2", "老王", 9, 9010),
        ];
        let text = render_day(3, &rows);
        assert!(text.contains("第 3 天流水（2 名骑手）"));
        assert!(text.contains("AI-A"));
        assert!(text.contains("¥182.40"));
        assert!(text.contains("老王"));
        assert!(text.contains("¥90.10"));
        // 合计 = 172.50 到手、6.00 罚款、4.00 保险、21 单、960 分钟
        assert!(text.contains("合计：21 单，到手 ¥272.50，罚款 ¥6.00，保险 ¥4.00，在线 960 分钟"));
    }

    #[test]
    fn top_render_numbers_the_rows() {
        let board = vec![
            Career {
                rider_key: "r1".into(),
                name: "AI-A".into(),
                days: 3,
                completed: 30,
                takehome_cents: 45000,
                fines_cents: 900,
                rating: 4.9,
                level: 3,
            },
            Career {
                rider_key: "r2".into(),
                name: "老王".into(),
                days: 2,
                completed: 18,
                takehome_cents: 20000,
                fines_cents: 0,
                rating: 4.5,
                level: 1,
            },
        ];
        let text = render_top(&board);
        assert!(text.contains("生涯排行榜（前 2 名）"));
        assert!(text.contains("1r1"));
        assert!(text.contains("2r2"));
        assert!(text.contains("¥450.00"));
    }

    #[test]
    fn unknown_subcommand_prints_usage() {
        assert!(run(&["nope".to_string()]).is_ok());
        assert!(run(&[]).is_ok());
    }

    #[test]
    fn day_without_number_is_an_error() {
        let path = temp_db("missing-day");
        let err = run(&["--db".into(), path.display().to_string(), "day".into()]).unwrap_err();
        assert!(err.contains("day <天数>"), "实际：{err}");
        let _ = std::fs::remove_file(&path);
    }
}
