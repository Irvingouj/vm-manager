//! What the tests of shipping the Change history share (CONTRACT.md, "The
//! Change history in the platform database: Shipping"; ticket "The Change
//! history lives in the platform database, shipped from an outbox"):
//!
//! - counts of an Application's rows of `change_history`, read at once as
//!   the harness's superuser ([`count_now`]), and waits for them with a
//!   timeout of the test's choosing ([`wait_count`]);
//! - the checks that no change is there twice ([`duplicates`],
//!   [`assert_no_duplicates`]) and that every title written is there once
//!   ([`titles_not_once`]);
//! - a lock the harness holds on `change_history` ([`LockedHistory`]), and a
//!   row of `change_history` the harness writes and leaves uncommitted, in
//!   the way of the batch that would hold the same id ([`Blocker`]);
//! - the batches a shipment wrote, read as the rows of `change_history` that
//!   share an `xmin` ([`batches`]): CONTRACT.md says "each batch is one
//!   transaction in the platform database", and the rows a transaction
//!   inserts share its `xmin`;
//! - the program's log records waited for longer than the log's usual wait
//!   ([`wait_for_record_within`]) and searched for a value
//!   ([`records_holding`]);
//! - an Application installed from `fixtures/transactions.js` beside the
//!   tests' usual one ([`install_transactions`], [`run_steps`]);
//! - where the 100,000-row test writes down its time ([`report_path`]).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::history::{history_standard, platform_sql};
use crate::http::{Response, User};
use crate::own_database::{Own, OwnDatabase, literal, version_body_with_migrations};
use crate::sql::Sql;
use crate::vm::{Vm, fixture_bundle};

/// The most changes one batch may hold (CONTRACT.md, "Bounded").
pub const BATCH_ROWS: i64 = 1_000;

/// The most bytes of `old` and `new` one batch may hold past its first
/// change (CONTRACT.md, "Bounded": 4 MiB).
pub const BATCH_BYTES: i64 = 4 * 1024 * 1024;

/// How long the tests wait for a request that writes many rows to be
/// answered.
pub const LONG_REQUEST: Duration = Duration::from_secs(60);

/// The number of an Application's rows of `change_history` satisfying
/// `condition` (on the row `h`), read now, without waiting. Fails when
/// `change_history` cannot be read.
pub fn count_now(db: &OwnDatabase, condition: &str) -> i64 {
    count_with(&platform_sql(db), &db.application, condition)
}

/// As [`count_now`] on a connection the test keeps.
pub fn count_with(sql: &Sql, application: &str, condition: &str) -> i64 {
    match sql.try_batch(&format!(
        "SELECT count(*)::text FROM change_history h WHERE h.application = {} AND ({condition})",
        literal(application)
    )) {
        Ok(rows) => rows
            .into_iter()
            .next()
            .and_then(|r| r.into_iter().next().flatten())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        Err(e) => panic!(
            "cannot read `change_history` in the platform database (CONTRACT.md: \"The history is in the platform database, in the table `change_history`\"): {e}"
        ),
    }
}

/// Waits up to `timeout` for `db`'s Application to have at least `want`
/// rows of `change_history` satisfying `condition`; returns how many it
/// had last and how long the wait took.
pub fn wait_count(
    db: &OwnDatabase,
    condition: &str,
    want: i64,
    timeout: Duration,
) -> (i64, Duration) {
    let sql = platform_sql(db);
    let start = Instant::now();
    loop {
        let n = count_with(&sql, &db.application, condition);
        if n >= want || start.elapsed() >= timeout {
            return (n, start.elapsed());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The condition on a row `h` of `change_history` that it is the insert of
/// a row of `things` with this title.
pub fn inserted_title(title: &str) -> String {
    format!(
        "h.schema_name = 'public' AND h.table_name = 'things' AND h.kind = 'insert' AND h.new->>'title' = {}",
        literal(title)
    )
}

/// The condition on a row `h` that it is a change of a row of `things`
/// whose title, before or after, starts with `prefix`.
pub fn titled_like(prefix: &str) -> String {
    let pattern = literal(&format!(
        "{}%",
        prefix.replace('%', "\\%").replace('_', "\\_")
    ));
    format!(
        "h.table_name = 'things' AND (h.new->>'title' LIKE {pattern} OR h.old->>'title' LIKE {pattern})"
    )
}

/// Every change of `db`'s Application that is in `change_history` more
/// than once: rows alike in everything a change records but `id` and `at`
/// (the table, kind, key, old, new, changed, User, actor, request and
/// migration), with how many times each is there. The tests make every
/// change they make different from every other, so a row here is a change
/// shipped twice.
pub fn duplicates(db: &OwnDatabase) -> Vec<String> {
    platform_sql(db)
        .batch(&format!(
            "SELECT count(*)::text || ' x ' || h.kind || ' ' || h.schema_name || '.' || h.table_name
                    || ' key=' || coalesce(h.key::text, 'null') || ' new=' || left(coalesce(h.new::text, 'null'), 200)
                    || ' request=' || coalesce(h.request, 'null')
               FROM change_history h WHERE h.application = {}
              GROUP BY h.schema_name, h.table_name, h.kind, h.key::text, h.old::text, h.new::text,
                       h.changed::text, h.user_id, h.actor, h.request, h.migration
             HAVING count(*) > 1
             UNION ALL
             SELECT 'id ' || h.id::text || ' is there ' || count(*)::text || ' times'
               FROM change_history h WHERE h.application = {}
              GROUP BY h.id HAVING count(*) > 1",
            literal(&db.application),
            literal(&db.application)
        ))
        .into_iter()
        .filter_map(|row| row.into_iter().next().flatten())
        .collect()
}

/// Fails unless no change of `db`'s Application is in `change_history`
/// twice ([`duplicates`]).
pub fn assert_no_duplicates(db: &OwnDatabase, context: &str) {
    let twice = duplicates(db);
    assert!(
        twice.is_empty(),
        "{context}: expected every change in `change_history` exactly once (CONTRACT.md, \"Shipping: Exactly once\"), but these are there more than once:\n{}",
        twice.join("\n")
    );
}

/// The titles starting with `prefix` whose insert into `things` is not in
/// `change_history` exactly once, with how many times each is there, out of
/// the `expected` titles `prefix` followed by 1 to `expected`.
pub fn titles_not_once(db: &OwnDatabase, prefix: &str, expected: i64) -> Vec<String> {
    platform_sql(db)
        .batch(&format!(
            "WITH want AS (SELECT {} || g::text AS title FROM generate_series(1, {expected}) g),
                  got AS (SELECT h.new->>'title' AS title, count(*) AS n FROM change_history h
                           WHERE h.application = {} AND h.table_name = 'things' AND h.kind = 'insert'
                             AND h.new->>'title' LIKE {}
                           GROUP BY 1)
             SELECT coalesce(w.title, g.title) || ': ' || coalesce(g.n, 0)::text || ' times'
               FROM want w FULL JOIN got g ON g.title = w.title
              WHERE w.title IS NULL OR coalesce(g.n, 0) <> 1
              ORDER BY 1 LIMIT 20",
            literal(prefix),
            literal(&db.application),
            literal(&format!("{}%", prefix.replace('%', "\\%").replace('_', "\\_")))
        ))
        .into_iter()
        .filter_map(|row| row.into_iter().next().flatten())
        .collect()
}

/// The greatest `id` of `db`'s Application in `change_history` now, 0 when
/// it has none.
pub fn max_id_now(db: &OwnDatabase) -> i64 {
    platform_sql(db)
        .try_batch(&format!(
            "SELECT coalesce(max(id), 0)::text FROM change_history WHERE application = {}",
            literal(&db.application)
        ))
        .unwrap_or_else(|e| panic!("cannot read `change_history` in the platform database: {e}"))
        .into_iter()
        .next()
        .and_then(|r| r.into_iter().next().flatten())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// `LOCK TABLE change_history IN ACCESS EXCLUSIVE MODE`, held by the
/// harness's superuser in an open transaction on the platform database
/// until [`LockedHistory::release`] (or until dropped: the connection
/// closes). CONTRACT.md, "Shipping": "the tests hold a lock on
/// `change_history`".
pub struct LockedHistory {
    sql: Sql,
}

impl LockedHistory {
    /// Takes the lock; fails unless it was granted.
    pub fn lock(db: &OwnDatabase) -> Self {
        let sql = platform_sql(db);
        sql.batch("BEGIN");
        if let Err(e) = sql.try_batch("LOCK TABLE change_history IN ACCESS EXCLUSIVE MODE") {
            panic!(
                "cannot lock `change_history` in the platform database (CONTRACT.md: \"the tests hold a lock on `change_history`\"): {e}"
            );
        }
        Self { sql }
    }

    /// Ends the transaction, and with it the lock.
    pub fn release(self) {
        self.sql.batch("ROLLBACK");
    }
}

/// A row of `change_history` for `application` with the id `id`, written
/// by the harness's superuser and left uncommitted: until it is released,
/// a shipment whose batch holds that id waits on it (`(application, id)` is
/// unique, CONTRACT.md "Where it is kept"), so that the shipment stops in
/// its middle where the test wants it to.
pub struct Blocker {
    sql: Sql,
}

impl Blocker {
    /// Writes the row; an error when `change_history` refuses it (a
    /// column the contract does not name that wants a value, or the id
    /// already there).
    pub fn hold(db: &OwnDatabase, id: i64) -> Result<Self, String> {
        let sql = platform_sql(db);
        sql.batch("BEGIN");
        sql.try_batch(&format!(
            "INSERT INTO change_history (application, id, at, schema_name, table_name, kind, key, old, new,
                                         changed, user_id, actor, request, migration, structure)
             VALUES ({}, {id}, now(), 'public', 'things', 'insert', NULL, NULL,
                     '{{\"title\": \"the harness blocker\"}}'::jsonb, NULL, 'harness', 'user',
                     'harness-blocker', NULL, NULL)",
            literal(&db.application)
        ))?;
        Ok(Self { sql })
    }

    /// Rolls the row back: the shipment it held up goes on.
    pub fn release(self) {
        self.sql.batch("ROLLBACK");
    }
}

/// One batch of a shipment as `change_history` shows it: the rows of one
/// Application one transaction inserted.
#[derive(Clone, Debug)]
pub struct Batch {
    pub xmin: String,
    pub rows: i64,
    /// The bytes of `old` and `new` of every row, as `jsonb` prints them.
    pub bytes: i64,
    /// The same of the batch's first change (its lowest `id`).
    pub first_bytes: i64,
}

impl Batch {
    /// The bytes of `old` and `new` past the batch's first change.
    pub fn past_first(&self) -> i64 {
        self.bytes - self.first_bytes
    }
}

/// The batches of `db`'s Application's rows of `change_history` satisfying
/// `condition` (on the row `h`), by `xmin`.
pub fn batches(db: &OwnDatabase, condition: &str) -> Vec<Batch> {
    let size = "(coalesce(octet_length(h.old::text), 0) + coalesce(octet_length(h.new::text), 0))";
    platform_sql(db)
        .batch(&format!(
            "SELECT h.xmin::text, count(*)::text, sum({size})::text,
                    ((array_agg({size} ORDER BY h.id))[1])::text
               FROM change_history h WHERE h.application = {} AND ({condition})
              GROUP BY h.xmin::text ORDER BY min(h.id)",
            literal(&db.application)
        ))
        .into_iter()
        .map(|row| {
            let text = |i: usize| row.get(i).cloned().flatten().unwrap_or_default();
            let number = |i: usize| text(i).parse::<i64>().unwrap_or(0);
            Batch {
                xmin: text(0),
                rows: number(1),
                bytes: number(2),
                first_bytes: number(3),
            }
        })
        .collect()
}

/// Waits up to `timeout` for a record of the program's log that `matches`.
pub fn wait_for_record_within(
    vm: &Vm,
    timeout: Duration,
    matches: impl Fn(&Value) -> bool,
) -> Option<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(found) = vm.records().into_iter().find(|r| matches(r)) {
            return Some(found);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Every line of the program's standard error (its log records and
/// anything else) that holds `text` anywhere.
pub fn records_holding(vm: &Vm, text: &str) -> Vec<String> {
    vm.lines()
        .into_iter()
        .map(|line| line.raw)
        .filter(|line| line.contains(text))
        .collect()
}

/// Creates `name` and sends a first Version with `fixtures/transactions.js`
/// as its bundle (which runs steps: `tx.begin`, `execute`, `tx.commit`,
/// `history`, ...) and [`history_standard`]; returns its own database.
pub fn install_transactions(own: &Own, name: &str) -> OwnDatabase {
    own.create(name);
    let sent = own.send_body(
        name,
        &version_body_with_migrations(
            &fixture_bundle(crate::transactions::FIXTURE),
            &[],
            &history_standard(),
        ),
    );
    if sent.status != 201 {
        own.vm.fail(&format!(
            "the harness's own check: expected `201` sending transactions.js as a Version of {name:?}, got\n{}",
            sent.describe()
        ));
    }
    own.own_database(name)
}

/// Runs these steps of `transactions.js` in `application` as `user`; fails
/// unless `200` with `{ answers }`.
pub fn run_steps(
    own: &Own,
    user: &User,
    application: &str,
    steps: &[Value],
) -> (Response, Vec<Value>) {
    own.run_body(
        user,
        application,
        &json!({ "steps": steps }),
        crate::transactions::TX_REQUEST_TIMEOUT,
    )
}

/// Where the test of a 100,000-row statement writes down the time its
/// shipment took: `target/t62-100k-shipment.txt` at the repository's root.
pub fn report_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("target")
        .join("t62-100k-shipment.txt")
}

/// The test Postgres's version, as `SHOW server_version` gives it.
pub fn postgres_version(db: &OwnDatabase) -> String {
    db.instance()
        .value("SHOW server_version")
        .unwrap_or_default()
}
