//! Part H of docs/research/change-history-out-of-reach.md: what ticket #62
//! (the Change history in the platform database, shipped from an outbox)
//! costs, measured with the real program.
//!
//! Not a test of the suite: `h-cost.sh` copies this file into
//! `blackbox/tests/` for the length of one run and removes it again, so that
//! it drives the real `vm` binary (`VM_BIN`) beside the harness's Postgres
//! through the harness, as the black-box tests do. `H_PARTS` says which
//! parts run: `writes` and `migrations` work with a program from before the
//! ticket; `snapshot`, `after`, `ship`, `reads` and `start` need the ticket's program.
//!
//! It prints lines beginning with `H ` and asserts only that what it times
//! succeeded.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a measurement: a failure is a failed run"
)]

use std::time::{Duration, Instant};

use blackbox::User;
use blackbox::changed_outside::{platform_superuser, stop};
use blackbox::history::{outbox_count, platform_sql};
use blackbox::out_of_reach::{WRITER, base, start};
use blackbox::own_database::{Migration, Own, OwnDatabase, literal, own_execute};
use blackbox::scenario::op;
use serde_json::{Value, json};

fn parts() -> String {
    std::env::var("H_PARTS").unwrap_or_else(|_| "writes,migrations".to_owned())
}

/// Median, 90th percentile and mean of `times`, in milliseconds.
fn summary(times: &[Duration]) -> String {
    let mut ms: Vec<f64> = times.iter().map(|t| t.as_secs_f64() * 1000.0).collect();
    ms.sort_by(f64::total_cmp);
    let at = |q: f64| ms[((ms.len() as f64 - 1.0) * q).round() as usize];
    let mean = ms.iter().sum::<f64>() / ms.len() as f64;
    format!(
        "n={} median={:.3} ms p90={:.3} ms mean={:.3} ms",
        ms.len(),
        at(0.5),
        at(0.9),
        mean
    )
}

/// One `execute` through `application`, timed from the request to its
/// answer.
fn execute(own: &Own, application: &str, sql: &str) -> Duration {
    let call = own_execute("user", sql);
    let started = Instant::now();
    let (_, answers) = own.run_body(
        &User::new(WRITER),
        application,
        &json!({ "calls": [call] }),
        Duration::from_secs(300),
    );
    let took = started.elapsed();
    assert!(answers[0]["ok"].is_object(), "{sql} failed: {}", answers[0]);
    took
}

/// One INSERT of one row of `things`.
fn write(own: &Own, application: &str, id: i64) -> Duration {
    execute(
        own,
        application,
        &format!("insert into things (id, title, size) values ({id}, 'a write', 1)"),
    )
}

/// The time to send `list` (accepted).
fn send(own: &Own, application: &str, list: &[Migration]) -> Duration {
    let started = Instant::now();
    let sent = own.send(application, list);
    let took = started.elapsed();
    assert_eq!(sent.status, 201, "{}", sent.describe());
    took
}

/// One `history` read of `things`, timed from the request to its answer;
/// the number of changes answered.
fn read(own: &Own, application: &str, arguments: &Value) -> (Duration, usize) {
    let mut arguments = arguments.clone();
    arguments["own"] = json!(true);
    arguments["as"] = json!("user");
    arguments["table"] = json!("things");
    let call = op("history", &arguments);
    let started = Instant::now();
    let (_, answer) = own.one(&User::new(WRITER), application, &call);
    let took = started.elapsed();
    let n = answer["ok"]["changes"]
        .as_array()
        .unwrap_or_else(|| panic!("a read failed: {answer}"))
        .len();
    (took, n)
}

/// Waits until `db`'s outbox is empty; how long that took.
fn shipped(db: &OwnDatabase, limit: Duration) -> Duration {
    let started = Instant::now();
    while outbox_count(db) != 0 {
        assert!(started.elapsed() < limit, "{} not shipped within {limit:?}", db.application);
        std::thread::sleep(Duration::from_millis(20));
    }
    started.elapsed()
}

fn value(sql: &blackbox::sql::Sql, query: &str) -> String {
    sql.value(query).unwrap_or_default()
}

#[test]
fn h_cost() {
    let parts = parts();
    let own = start();
    let app = "cost";
    let db = own.install(app, &base());
    let mut list = base();
    let mut next = 4;
    let mut push = |list: &mut Vec<Migration>, statements: &[&str]| {
        list.push(Migration::new(&format!("{next:04}_h"), statements));
        next += 1;
    };

    if parts.contains("writes") {
        for i in 0..30 {
            write(&own, app, 100_000 + i);
        }
        let times: Vec<Duration> = (0..400).map(|i| write(&own, app, 200_000 + i)).collect();
        println!("H write (one INSERT through execute, request to answer): {}", summary(&times));
    }

    if parts.contains("migrations") {
        let mut tables = 0_usize;
        let mut made = 0;
        for target in [10_usize, 100, 500] {
            let statements: Vec<String> = (tables..target)
                .map(|_| {
                    made += 1;
                    format!(
                        r#"CREATE TABLE "h_{made}" ("id" integer PRIMARY KEY NOT NULL, "a" text, "b" text, "c" integer, "d" integer, "e" timestamp with time zone, "f" jsonb, "g" boolean, "h" numeric, "i" text);"#
                    )
                })
                .collect();
            let refs: Vec<&str> = statements.iter().map(String::as_str).collect();
            push(&mut list, &refs);
            send(&own, app, &list);
            tables = target;
            db.superuser().batch("ANALYZE");
            let mut plain = Vec::new();
            let mut retype = Vec::new();
            for round in 0..5 {
                push(&mut list, &["SELECT 1;"]);
                plain.push(send(&own, app, &list));
                let to = if round % 2 == 0 { "bigint" } else { "integer" };
                let statement =
                    format!(r#"ALTER TABLE "h_1" ALTER COLUMN "c" SET DATA TYPE {to};"#);
                push(&mut list, &[statement.as_str()]);
                retype.push(send(&own, app, &list));
            }
            println!(
                "H tables={target}: a Version with one migration of `SELECT 1` {} ; with one migration retyping a column {}",
                summary(&plain),
                summary(&retype)
            );
            if parts.contains("snapshot") {
                let session = db.superuser();
                session.batch("SELECT vm.structure_snapshot()");
                let started = Instant::now();
                for _ in 0..50 {
                    session.batch("SELECT vm.structure_snapshot()");
                }
                let snapshot = started.elapsed() / 50;
                let started = Instant::now();
                for _ in 0..50 {
                    session.batch("SELECT 1");
                }
                let alone = started.elapsed() / 50;
                let size = value(&session, "SELECT octet_length(vm.structure_snapshot()::text)::text");
                println!(
                    "H tables={target}: vm.structure_snapshot() {:.2} ms a call ({size} bytes as text), a round trip alone {:.2} ms",
                    snapshot.as_secs_f64() * 1000.0,
                    alone.as_secs_f64() * 1000.0
                );
            }
        }
    }

    if parts.contains("after") {
        // Does a write wait for the shipment of the one before it? A write
        // of 10,000 rows, and at once a write of one row, ten times.
        let mut after_big = Vec::new();
        let mut quiet = Vec::new();
        let mut big = Vec::new();
        for round in 0..10_i64 {
            shipped(&db, Duration::from_secs(60));
            quiet.push(write(&own, app, 300_000 + round));
            shipped(&db, Duration::from_secs(60));
            let first = 400_000 + round * 20_000;
            big.push(execute(
                &own,
                app,
                &format!(
                    "insert into things (id, title, size) select g, 'many', 1 from generate_series({first}, {}) g",
                    first + 9_999
                ),
            ));
            let left = outbox_count(&db);
            after_big.push(write(&own, app, 310_000 + round));
            println!("H next-write round {round}: outbox held {left} rows when the next write was sent");
        }
        println!("H a write of 10,000 rows: {}", summary(&big));
        println!("H a write of one row with the outbox empty: {}", summary(&quiet));
        println!(
            "H a write of one row sent at once after the write of 10,000 was answered (its shipment under way): {}",
            summary(&after_big)
        );
    }

    if parts.contains("ship") {
        shipped(&db, Duration::from_secs(60));
        let answered = execute(
            &own,
            app,
            "insert into things (id, title, size) select g, 'hundred-k-' || g, 1 from generate_series(1000001, 1100000) g",
        );
        let took = shipped(&db, Duration::from_secs(600));
        let platform = platform_sql(&db);
        let n = value(
            &platform,
            &format!(
                "SELECT count(*)::text FROM change_history WHERE application = {} AND new->>'title' LIKE 'hundred-k-%'",
                literal(app)
            ),
        );
        let batches = value(
            &platform,
            &format!(
                "SELECT count(DISTINCT xmin::text)::text FROM change_history WHERE application = {} AND new->>'title' LIKE 'hundred-k-%'",
                literal(app)
            ),
        );
        println!(
            "H 100,000 rows in one execute: answered after {:.3} s; outbox empty {:.3} s after the answer; {n} changes in change_history, in {batches} transactions",
            answered.as_secs_f64(),
            took.as_secs_f64()
        );
    }

    if parts.contains("reads") {
        let reads = "reads";
        let rdb = own.install(reads, &base());
        execute(
            &own,
            reads,
            "insert into things (id, title, size) select g, 'r', 0 from generate_series(1, 10) g",
        );
        for i in 1..100 {
            execute(&own, reads, &format!("update things set size = {i}"));
        }
        shipped(&rdb, Duration::from_secs(60));
        let platform = platform_sql(&rdb);
        let measure = |label: &str| {
            for (what, arguments) in [
                ("no key", json!({ "limit": 100 })),
                ("key {id: 7}", json!({ "limit": 100, "key": { "id": 7 } })),
                ("key {id: \"7\"}", json!({ "limit": 100, "key": { "id": "7" } })),
            ] {
                for _ in 0..5 {
                    read(&own, reads, &arguments);
                }
                let mut times = Vec::new();
                let mut n = 0;
                for _ in 0..50 {
                    let (t, got) = read(&own, reads, &arguments);
                    times.push(t);
                    n = got;
                }
                println!("H read {label}, {what}: {n} changes answered; {}", summary(&times));
            }
        };
        let kept = value(
            &platform,
            &format!("SELECT count(*)::text FROM change_history WHERE application = {}", literal(reads)),
        );
        measure(&format!("with {kept} changes of the Application kept"));
        // 999,000 more, made by the harness's superuser from the ones kept:
        // the same table, 9,990 other keys, ids above every real one (no
        // write follows, so no id of the outbox meets them).
        platform.batch(&format!(
            "INSERT INTO change_history (application, id, at, schema_name, table_name, kind, key, old, new, changed,
                                         user_id, actor, request, migration, structure)
             SELECT h.application, 10000000 + g * 1000 + (h.id % 1000), h.at, h.schema_name, h.table_name, h.kind,
                    jsonb_build_object('id', 1000 + g * 10 + (h.key->>'id')::int),
                    h.old || jsonb_build_object('id', 1000 + g * 10 + (h.key->>'id')::int),
                    h.new || jsonb_build_object('id', 1000 + g * 10 + (h.key->>'id')::int),
                    h.changed, h.user_id, h.actor, h.request, h.migration, h.structure
               FROM change_history h CROSS JOIN generate_series(1, 999) g
              WHERE h.application = {app} AND h.table_name = 'things';
             ANALYZE change_history",
            app = literal(reads)
        ));
        let kept = value(
            &platform,
            &format!("SELECT count(*)::text FROM change_history WHERE application = {}", literal(reads)),
        );
        measure(&format!("with {kept} changes of the Application kept"));
        // The keyed read's plan: crates/vm/src/own_db/history.rs's `ROW`
        // statement, copied here with its parameters written in.
        let plan = platform.batch(&format!(
            "EXPLAIN (ANALYZE, BUFFERS, COSTS OFF, TIMING OFF)
             SELECT y.id FROM (SELECT x.*, sum(x.size) OVER (ORDER BY x.id DESC) AS running
               FROM (SELECT h.id, coalesce(octet_length(h.old::text), 0) + coalesce(octet_length(h.new::text), 0) AS size
                       FROM (SELECT * FROM (
                               (SELECT * FROM change_history
                                 WHERE application = {app} AND schema_name = 'public' AND table_name = 'things'
                                   AND id < 9223372036854775807
                                   AND jsonb_hash_extended(key, 0) = jsonb_hash_extended('{{\"id\": 7}}'::jsonb, 0)
                                   AND key = '{{\"id\": 7}}'::jsonb
                                 ORDER BY id DESC LIMIT 100)
                               UNION ALL
                               (SELECT * FROM change_history
                                 WHERE application = {app} AND schema_name = 'public' AND table_name = 'things'
                                   AND id < 9223372036854775807 AND kind = 'structure'
                                 ORDER BY id DESC LIMIT 100)) u
                             ORDER BY id DESC LIMIT 100) h) x) y
              WHERE y.running - y.size <= 8388608 ORDER BY y.id DESC",
            app = literal(reads)
        ));
        println!("H the keyed read's plan with {kept} changes kept:");
        for row in plan {
            println!("H   {}", row.into_iter().flatten().collect::<Vec<_>>().join(" "));
        }
        drop(rdb);
    }

    if parts.contains("start") {
        // 50 own databases, each with an outbox of 1,000 changes to ship
        // when the program starts: written while the harness holds a lock on
        // change_history (so that no shipment gets through), then the
        // program is killed, the lock released, and the program started.
        let mut dbs = Vec::new();
        for i in 0..50 {
            let name = format!("s{i:02}");
            dbs.push(own.install(&name, &base()));
        }
        let lock = platform_superuser(&own);
        lock.batch("BEGIN; LOCK TABLE change_history IN ACCESS EXCLUSIVE MODE");
        for (i, db) in dbs.iter().enumerate() {
            execute(
                &own,
                &db.application,
                "insert into things (id, title, size) select g, 'at-start', 1 from generate_series(1, 1000) g",
            );
            let _ = i;
        }
        let stopped = stop(own);
        lock.batch("ROLLBACK");
        let waiting: i64 = dbs.iter().map(outbox_count).sum();
        let started = Instant::now();
        let own = stopped.start();
        let listening = started.elapsed();
        let first_write = write(&own, &dbs[0].application, 900_001);
        let first_read = read(&own, &dbs[49].application, &json!({ "limit": 1 })).0;
        let mut empty_after = Duration::ZERO;
        loop {
            let left: i64 = dbs.iter().map(outbox_count).sum();
            if left == 0 {
                empty_after = started.elapsed().max(empty_after);
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(600), "{left} left");
            std::thread::sleep(Duration::from_millis(50));
        }
        let platform = platform_superuser(&own);
        let shipped = value(
            &platform,
            "SELECT count(*)::text FROM change_history WHERE application LIKE 's%' AND new->>'title' = 'at-start'",
        );
        let not_shipped = own
            .vm
            .records()
            .iter()
            .filter(|r| r["event"] == "history-not-shipped")
            .count();
        println!(
            "H start with 50 own databases, {waiting} changes in their outboxes: listening after {:.3} s; a write sent then answered in {:.3} ms, a read of another Application's history in {:.3} ms; every outbox empty {:.3} s after the start; {shipped} of them in change_history; {not_shipped} history-not-shipped records after the restart",
            listening.as_secs_f64(),
            first_write.as_secs_f64() * 1000.0,
            first_read.as_secs_f64() * 1000.0,
            empty_after.as_secs_f64()
        );
    }
    drop(db);
}
