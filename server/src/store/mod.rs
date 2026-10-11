//! SQLite 持久化。
//!
//! ## 只存「跨局有意义」的东西
//!
//! 局内的实时状态（订单、骑手位置）**不落库**——它们每秒都在变，写进去
//! 纯属浪费，而且会让「固定种子可复现」这条约束变得含糊。这一层只存三类
//! 数据：
//!
//! | 表 | 内容 | 用途 |
//! |---|---|---|
//! | `day_ledger` | 每天每人的日结流水（JSON） | 历史账单、赛季统计 |
//! | `career` | 骑手生涯累计（天数、单量、收入、评分） | 「老骑手」成长感 |
//! | `meta` | 局级元信息（种子、当前天、已完成 tick） | 重启后接着跑 |
//! | `account` | 骑手账号（名字 + Argon2 密码哈希） | 名字+密码登录、烽火台建号 |
//!
//! ## 一条原则：**存不下就当没有**
//!
//! 数据库打不开、表建不出来、写失败——一律降级为 `None` / 空结果并打日志，
//! **绝不让持久化问题挡住服务启动**。与高德/LLM 的处理方式一致。

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::sim::{event::DayLedger, rider_key};

/// 持久化相关的错误。**都只在内部消化**，调用方拿到的永远是 `Option` / 空集合。
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("SQLite 错误：{0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("序列化日结流水失败：{0}")]
    Json(#[from] serde_json::Error),
}

type Result<T> = std::result::Result<T, StoreError>;

/// 骑手生涯统计。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Career {
    pub rider_key: String,
    pub name: String,
    /// 出勤天数。
    pub days: u32,
    pub completed: u32,
    pub takehome_cents: i64,
    pub fines_cents: i64,
    pub rating: f32,
    pub level: u8,
}

/// 一次日结写入的结果，给控制端显示。
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize)]
pub struct FlushReport {
    pub ledgers: usize,
    pub careers_updated: usize,
}

/// 一个骑手账号。**故意不带密码哈希**——哈希只从 [`Store::account_hash`] 单独取，
/// 免得它跟着账户列表、日志、控制端响应到处跑。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Account {
    /// 账号名 = 骑手名。
    pub name: String,
    /// 建号时间（Unix 秒）。
    pub created_at: i64,
    /// 最后一次改密时间（Unix 秒）。
    pub updated_at: i64,
}

/// 一条待落库的会话消息。
///
/// 字段全是借用的（`&'a str`）：事件里的 `from` / `kind` / `fault` 本来就是
/// `&'static str`，没必要为了写一次库把它们转成 `String`。
#[derive(Debug, Clone, Copy)]
pub struct ChatRecord<'a> {
    pub order_id: u32,
    /// 归属天（0:00 起算的第几天）。时效按它算。
    pub day: u32,
    /// 发生时的当日分钟（0..1439）。
    pub sim_minute: u32,
    /// `rider` | `customer` | `system`
    pub sender: &'a str,
    /// `preset` | `free` | `reply` | `ruling`
    pub kind: &'a str,
    pub text: &'a str,
    /// 这条消息带来的赔付（分）。恒 ≥ 0。
    pub compensation_cents: i64,
    /// `rider` | `merchant` | `none`
    pub fault: &'a str,
}

/// SQLite 句柄。**单线程持有**（与 tick 循环同一个任务）。
pub struct Store {
    conn: Connection,
    path: PathBuf,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").field("path", &self.path).finish()
    }
}

impl Store {
    /// 打开（或新建）数据库并建表。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            let _ = std::fs::create_dir_all(dir);
        }
        let conn = Connection::open(&path)?;
        let store = Self { conn, path };
        store.migrate()?;
        Ok(store)
    }

    /// 内存库。测试与「不要持久化」模式用。
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self {
            conn,
            path: PathBuf::from(":memory:"),
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;

            CREATE TABLE IF NOT EXISTS day_ledger (
                day        INTEGER NOT NULL,
                rider_id   INTEGER NOT NULL,
                rider_key  TEXT    NOT NULL,
                json       TEXT    NOT NULL,
                PRIMARY KEY (day, rider_id)
            );

            CREATE TABLE IF NOT EXISTS career (
                rider_key      TEXT PRIMARY KEY,
                name           TEXT NOT NULL,
                days           INTEGER NOT NULL DEFAULT 0,
                completed      INTEGER NOT NULL DEFAULT 0,
                takehome_cents INTEGER NOT NULL DEFAULT 0,
                fines_cents    INTEGER NOT NULL DEFAULT 0,
                rating         REAL    NOT NULL DEFAULT 5.0,
                level          INTEGER NOT NULL DEFAULT 1
            );

            CREATE TABLE IF NOT EXISTS meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS account (
                name          TEXT PRIMARY KEY,
                password_hash TEXT    NOT NULL,
                created_at    INTEGER NOT NULL,
                updated_at    INTEGER NOT NULL
            );

            -- 订单会话（末端交付沟通）。**只增不改**，按 `day` 做一天时效。
            CREATE TABLE IF NOT EXISTS chat_message (
                id                 INTEGER PRIMARY KEY AUTOINCREMENT,
                order_id           INTEGER NOT NULL,
                day                INTEGER NOT NULL,
                sim_minute         INTEGER NOT NULL,
                sender             TEXT    NOT NULL,
                kind               TEXT    NOT NULL,
                text               TEXT    NOT NULL,
                compensation_cents INTEGER NOT NULL DEFAULT 0,
                fault              TEXT    NOT NULL DEFAULT 'none'
            );
            CREATE INDEX IF NOT EXISTS chat_message_day ON chat_message (day);
            CREATE INDEX IF NOT EXISTS chat_message_order ON chat_message (order_id, day);
            "#,
        )?;
        Ok(())
    }

    /// 写入一天的全部流水，并更新生涯累计。**幂等**：重复写同一天会覆盖。
    pub fn save_day(&mut self, day: u32, ledgers: &[DayLedger]) -> Result<FlushReport> {
        let tx = self.conn.transaction()?;
        for l in ledgers {
            let json = serde_json::to_string(l)?;
            tx.execute(
                "INSERT INTO day_ledger (day, rider_id, rider_key, json) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(day, rider_id) DO UPDATE SET json = excluded.json",
                params![day, l.rider_id, l.key, json],
            )?;
            // 生涯累计：只在「这天还没记过」时加。用 meta 里的一天一个哨兵实现。
            let sentinel = format!("career:{day}:{}", l.key);
            let recorded: Option<String> = tx
                .query_row(
                    "SELECT value FROM meta WHERE key = ?1",
                    params![sentinel],
                    |r| r.get(0),
                )
                .optional()?;
            if recorded.is_none() {
                tx.execute(
                    "INSERT INTO career (rider_key, name, days, completed, takehome_cents, fines_cents, rating, level)
                     VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6, ?7)
                     ON CONFLICT(rider_key) DO UPDATE SET
                        name           = excluded.name,
                        days           = career.days + 1,
                        completed      = career.completed + excluded.completed,
                        takehome_cents = career.takehome_cents + excluded.takehome_cents,
                        fines_cents    = career.fines_cents + excluded.fines_cents,
                        rating         = excluded.rating,
                        level          = excluded.level",
                    params![
                        l.key,
                        l.name,
                        l.completed,
                        l.takehome_cents,
                        l.fines_cents,
                        l.rating,
                        l.level
                    ],
                )?;
                tx.execute(
                    "INSERT INTO meta (key, value) VALUES (?1, '1')",
                    params![sentinel],
                )?;
            }
        }
        tx.commit()?;

        // 会话时效：一天。日结时顺手清掉更早的，**不需要额外的定时任务**。
        // 放在事务外——清理失败也不该让一天的流水白写。
        match self.purge_chats_before(day) {
            Ok(0) => {}
            Ok(n) => tracing::debug!(day, purged = n, "清掉了过期会话"),
            Err(e) => tracing::warn!(error = %e, "清理过期会话失败，下次日结再试"),
        }

        Ok(FlushReport {
            ledgers: ledgers.len(),
            careers_updated: ledgers.len(),
        })
    }

    /// 追加一条会话消息。
    ///
    /// 会话是**只增不改**的日志，所以用 `INSERT` 而不是 upsert——同一分钟发两条
    /// 同样的话，也该老老实实留两条。
    pub fn save_chat(&self, msg: &ChatRecord<'_>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO chat_message
                (order_id, day, sim_minute, sender, kind, text, compensation_cents, fault)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                msg.order_id,
                msg.day,
                msg.sim_minute,
                msg.sender,
                msg.kind,
                msg.text,
                msg.compensation_cents,
                msg.fault
            ],
        )?;
        Ok(())
    }

    /// 清掉 `day` **之前**的会话，返回删掉的行数（`day` 当天保留）。
    ///
    /// 由 [`Store::save_day`] 在日结时调用——过期一天即删。
    pub fn purge_chats_before(&self, day: u32) -> Result<usize> {
        let n = self
            .conn
            .execute("DELETE FROM chat_message WHERE day < ?1", params![day])?;
        Ok(n)
    }


    /// 读某一天的流水。
    pub fn load_day(&self, day: u32) -> Result<Vec<DayLedger>> {
        let mut stmt = self
            .conn
            .prepare("SELECT json FROM day_ledger WHERE day = ?1 ORDER BY rider_id")?;
        let rows = stmt.query_map(params![day], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            let json = row?;
            out.push(serde_json::from_str(&json)?);
        }
        Ok(out)
    }

    /// 库里已经有流水的天，升序。CLI 列账单用。
    pub fn days(&self) -> Result<Vec<u32>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT day FROM day_ledger ORDER BY day")?;
        let rows = stmt.query_map([], |r| r.get::<_, u32>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn career(&self, key: &str) -> Result<Option<Career>> {
        let c = self
            .conn
            .query_row(
                "SELECT rider_key, name, days, completed, takehome_cents, fines_cents, rating, level
                 FROM career WHERE rider_key = ?1",
                params![key],
                |r| {
                    Ok(Career {
                        rider_key: r.get(0)?,
                        name: r.get(1)?,
                        days: r.get(2)?,
                        completed: r.get(3)?,
                        takehome_cents: r.get(4)?,
                        fines_cents: r.get(5)?,
                        rating: r.get(6)?,
                        level: r.get(7)?,
                    })
                },
            )
            .optional()?;
        Ok(c)
    }

    /// 全部生涯记录，按收入降序（排行榜）。
    pub fn leaderboard(&self, limit: u32) -> Result<Vec<Career>> {
        let mut stmt = self.conn.prepare(
            "SELECT rider_key, name, days, completed, takehome_cents, fines_cents, rating, level
             FROM career ORDER BY takehome_cents DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |r| {
            Ok(Career {
                rider_key: r.get(0)?,
                name: r.get(1)?,
                days: r.get(2)?,
                completed: r.get(3)?,
                takehome_cents: r.get(4)?,
                fines_cents: r.get(5)?,
                rating: r.get(6)?,
                level: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 写一条元信息。
    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        let v = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(v)
    }

    /// 记住当前进度，重启后能接着跑。
    pub fn save_progress(&self, seed: u64, day: u32, elapsed_min: u64) -> Result<()> {
        self.set_meta("seed", &seed.to_string())?;
        self.set_meta("day", &day.to_string())?;
        self.set_meta("elapsed_min", &elapsed_min.to_string())?;
        Ok(())
    }

    pub fn load_progress(&self) -> Result<Option<(u64, u32, u64)>> {
        let (Some(seed), Some(day), Some(min)) = (
            self.meta("seed")?,
            self.meta("day")?,
            self.meta("elapsed_min")?,
        ) else {
            return Ok(None);
        };
        let parsed = (
            seed.parse::<u64>().ok(),
            day.parse::<u32>().ok(),
            min.parse::<u64>().ok(),
        );
        Ok(match parsed {
            (Some(s), Some(d), Some(m)) => Some((s, d, m)),
            _ => None,
        })
    }

    /// 某个骑手在某天的流水（账单页用）。
    pub fn rider_day(&self, day: u32, rider_id: crate::sim::RiderId) -> Result<Option<DayLedger>> {
        Ok(self
            .load_day(day)?
            .into_iter()
            .find(|l| l.rider_id == rider_id))
    }

    // ---- 账号 ----
    //
    // 名字已经由 `auth::normalize_name` 规范化过再进来，这里不再二次清洗：
    // 两侧都规范化会让「入库的到底是什么」变得说不清。

    /// 建号。**已经存在就返回 `Ok(false)`**，不动原密码——建号接口不能变成
    /// 「悄悄改掉别人密码」的后门。
    pub fn create_account(&self, name: &str, password_hash: &str) -> Result<bool> {
        let now = unix_now();
        let inserted = self.conn.execute(
            "INSERT INTO account (name, password_hash, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(name) DO NOTHING",
            params![name, password_hash, now],
        )?;
        Ok(inserted == 1)
    }

    /// 设 / 改密码。建号时保留 `created_at`。
    pub fn set_account_password(&self, name: &str, password_hash: &str) -> Result<()> {
        let now = unix_now();
        self.conn.execute(
            "INSERT INTO account (name, password_hash, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(name) DO UPDATE SET
                password_hash = excluded.password_hash,
                updated_at    = excluded.updated_at",
            params![name, password_hash, now],
        )?;
        Ok(())
    }

    /// 取密码哈希。登录校验用——这是全仓唯一会读出哈希的地方。
    pub fn account_hash(&self, name: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT password_hash FROM account WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// 账号列表，按建号时间。`created_at` 只有秒级精度，**同一秒内建的按名字
    /// 的 UTF-8 字节序**收尾——顺序必须确定，否则控制台列表会随查询计划抖。
    pub fn accounts(&self) -> Result<Vec<Account>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, created_at, updated_at FROM account ORDER BY created_at, name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Account {
                name: r.get(0)?,
                created_at: r.get(1)?,
                updated_at: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn account(&self, name: &str) -> Result<Option<Account>> {
        Ok(self
            .conn
            .query_row(
                "SELECT name, created_at, updated_at FROM account WHERE name = ?1",
                params![name],
                |r| {
                    Ok(Account {
                        name: r.get(0)?,
                        created_at: r.get(1)?,
                        updated_at: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// 改账号名。控制端给真人骑手改名时同步用——名字是账号的主键，
    /// 只能**搬**不能「新建」，否则骑手改完名，用旧名字还能登出一具新身体。
    ///
    /// 目标名字已经有账号时返回 `Err`（主键冲突），调用方应当在动世界之前就拦下。
    pub fn rename_account(&self, from: &str, to: &str) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE account SET name = ?2 WHERE name = ?1",
            params![from, to],
        )?;
        Ok(n == 1)
    }

    /// 删号。返回是否真的删掉了一个。
    pub fn delete_account(&self, name: &str) -> Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM account WHERE name = ?1", params![name])?;
        Ok(n == 1)
    }
}

/// 当前 Unix 秒。账号表的建号/改密时间戳用。
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 打不开数据库时的兜底：用内存库，服务照常起来。
///
/// 这是「持久化不该挡住服务启动」这条原则的落点。
pub fn open_or_memory(path: impl AsRef<Path>) -> (Store, Option<String>) {
    match Store::open(path.as_ref()) {
        Ok(s) => (s, None),
        Err(e) => {
            let warn = format!(
                "打开数据库 {} 失败（{e}），本次运行改为内存库，数据不会落盘",
                path.as_ref().display()
            );
            match Store::in_memory() {
                Ok(s) => (s, Some(warn)),
                // 内存库都建不出来只可能是 rusqlite 本身坏了；这时也只能panic，
                // 但要让调用方看见原因。
                Err(e2) => panic!("连内存库都开不了：{e2}"),
            }
        }
    }
}

impl Store {
    /// 某骑手的完整生涯键（与协议里的 `r3` 一致）。
    pub fn career_key(rider_id: crate::sim::RiderId) -> String {
        rider_key(rider_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::event::DayLedger;

    fn ledger(id: crate::sim::RiderId, name: &str, takehome: i64) -> DayLedger {
        DayLedger {
            rider_id: id,
            key: rider_key(id),
            name: name.into(),
            takehome_cents: takehome,
            gross_cents: takehome + 500,
            fines_cents: 500,
            insurance_cents: 300,
            completed: 8,
            late_count: 1,
            online_minutes: 480,
            hourly_wage_cents: 2500,
            rating: 4.9,
            level: 3,
            debt_cents: 0,
        }
    }

    #[test]
    fn round_trips_a_day_of_ledgers() {
        let mut s = Store::in_memory().unwrap();
        let day1 = vec![ledger(1, "甲", 12_000), ledger(2, "乙", 9_500)];
        let r = s.save_day(1, &day1).unwrap();
        assert_eq!(r.ledgers, 2);

        let back = s.load_day(1).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].takehome_cents, 12_000);
        assert_eq!(back[1].name, "乙");

        // 别人的日子是空的
        assert!(s.load_day(2).unwrap().is_empty());
    }

    #[test]
    fn saving_the_same_day_twice_does_not_double_count_career() {
        let mut s = Store::in_memory().unwrap();
        let day = vec![ledger(1, "甲", 10_000)];
        s.save_day(1, &day).unwrap();
        s.save_day(1, &day).unwrap();
        let c = s.career("r1").unwrap().unwrap();
        assert_eq!(c.days, 1, "同一天写两次只能算一天");
        assert_eq!(c.completed, 8);
        assert_eq!(c.takehome_cents, 10_000);
    }

    #[test]
    fn career_accumulates_across_days() {
        let mut s = Store::in_memory().unwrap();
        s.save_day(1, &[ledger(1, "甲", 10_000)]).unwrap();
        s.save_day(2, &[ledger(1, "甲", 8_000)]).unwrap();
        let c = s.career("r1").unwrap().unwrap();
        assert_eq!(c.days, 2);
        assert_eq!(c.completed, 16);
        assert_eq!(c.takehome_cents, 18_000);
        assert_eq!(c.fines_cents, 1_000);
    }

    #[test]
    fn leaderboard_is_ordered_by_earnings() {
        let mut s = Store::in_memory().unwrap();
        s.save_day(1, &[ledger(1, "甲", 5_000), ledger(2, "乙", 20_000)])
            .unwrap();
        let board = s.leaderboard(10).unwrap();
        assert_eq!(board[0].name, "乙");
        assert_eq!(board[1].name, "甲");
    }

    #[test]
    fn progress_survives_a_reopen() {
        let dir = std::env::temp_dir().join(format!("rider_sim_store_{}", std::process::id()));
        let file = dir.join("progress.db");
        let _ = std::fs::remove_file(&file);

        {
            let s = Store::open(&file).unwrap();
            s.save_progress(42, 3, 4_321).unwrap();
        }
        {
            let s = Store::open(&file).unwrap();
            assert_eq!(s.load_progress().unwrap(), Some((42, 3, 4_321)));
        }
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn progress_is_none_on_a_fresh_database() {
        let s = Store::in_memory().unwrap();
        assert_eq!(s.load_progress().unwrap(), None);
        assert_eq!(s.meta("nothing").unwrap(), None);
    }

    #[test]
    fn unknown_career_is_none_not_an_error() {
        let s = Store::in_memory().unwrap();
        assert!(s.career("r999").unwrap().is_none());
    }

    #[test]
    fn open_or_memory_falls_back_instead_of_failing() {
        // 一个不可能创建成功的位置（父路径是文件）
        let bogus = std::env::temp_dir().join(format!(
            "rider_sim_not_a_dir_{}/db.sqlite",
            std::process::id()
        ));
        let _ = std::fs::write(bogus.parent().unwrap(), b"i am a file");
        let (store, warn) = open_or_memory(&bogus);
        assert_eq!(store.path(), Path::new(":memory:"));
        assert!(warn.is_some(), "降级时必须给出原因");
        let _ = std::fs::remove_file(bogus.parent().unwrap());
    }

    #[test]
    fn rider_day_filters_by_id() {
        let mut s = Store::in_memory().unwrap();
        s.save_day(1, &[ledger(1, "甲", 100), ledger(2, "乙", 200)])
            .unwrap();
        assert_eq!(s.rider_day(1, 2).unwrap().unwrap().takehome_cents, 200);
        assert!(s.rider_day(1, 7).unwrap().is_none());
    }

    #[test]
    fn career_key_matches_protocol() {
        assert_eq!(Store::career_key(3), "r3");
        assert_eq!(Store::career_key(3), rider_key(3));
    }

    #[test]
    fn create_account_never_overwrites_an_existing_password() {
        let s = Store::in_memory().unwrap();
        assert!(s.create_account("张三", "hash-a").unwrap());
        // 建号接口不能被拿来悄悄改掉别人的密码
        assert!(!s.create_account("张三", "hash-b").unwrap());
        assert_eq!(s.account_hash("张三").unwrap().as_deref(), Some("hash-a"));
    }

    #[test]
    fn setting_a_password_keeps_the_creation_time() {
        let s = Store::in_memory().unwrap();
        s.set_account_password("张三", "hash-a").unwrap();
        let created = s.account("张三").unwrap().unwrap().created_at;

        s.set_account_password("张三", "hash-b").unwrap();
        let after = s.account("张三").unwrap().unwrap();
        assert_eq!(after.created_at, created, "改密不该改建号时间");
        assert!(after.updated_at >= created);
        assert_eq!(s.account_hash("张三").unwrap().as_deref(), Some("hash-b"));
    }

    #[test]
    fn accounts_list_and_delete() {
        let s = Store::in_memory().unwrap();
        assert!(s.account("nobody").unwrap().is_none());
        assert!(s.accounts().unwrap().is_empty());

        let mut created = ["甲", "乙", "丙"];
        for n in created {
            s.set_account_password(n, "h").unwrap();
        }
        // 三次建号落在同一秒里，所以只能按名字收尾，实际顺序 = UTF-8 字节序
        created.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        let names: Vec<_> = s.accounts().unwrap().into_iter().map(|a| a.name).collect();
        assert_eq!(names, created);

        assert!(s.delete_account("乙").unwrap());
        assert!(!s.delete_account("乙").unwrap(), "删第二次不该说删掉了");
        assert_eq!(s.accounts().unwrap().len(), 2);
        assert!(s.account_hash("乙").unwrap().is_none());
    }

    #[test]
    fn accounts_survive_a_reopen() {
        let file = std::env::temp_dir().join(format!(
            "rider_sim_accounts_{}/db.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&file);
        {
            let s = Store::open(&file).unwrap();
            s.set_account_password("张三", "hash-a").unwrap();
        }
        let s = Store::open(&file).unwrap();
        assert_eq!(s.account_hash("张三").unwrap().as_deref(), Some("hash-a"));
        let _ = std::fs::remove_file(&file);
    }
}
