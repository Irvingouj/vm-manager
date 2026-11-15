//! Part J of docs/research/change-history-out-of-reach.md: what ticket #63
//! (a rewrite records each row it changed) costs, measured with the real
//! program.
//!
//! Not a test of the suite: `j-cost.sh` copies this file into
//! `blackbox/tests/` for the length of one run and removes it again, so that
//! it drives the real `vm` binary (`VM_BIN`) beside the harness's Postgres
//! through the harness, as the black-box tests do. `J_PARTS` says which
//! parts run (`statements`, `rewrite`, `breakdown`); `breakdown` needs the
//! ticket's program. `J_ROWS` is the rewritten table's size (100,000 by
//! default), `J_ROUNDS` how many Versions of each kind `rewrite` times (3
//! by default).
//!
//! It prints lines beginning with `J ` and asserts only that what it times
//! succeeded.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a measurement: a failure is a failed run"
)]

use std::time::{Duration, Instant};

use blackbox::history::{outbox_count, platform_sql};
use blackbox::out_of_reach::{base, start};
use blackbox::own_database::{Migration, Own, OwnDatabase, literal};

fn parts() -> String {
    std::env::var("J_PARTS").unwrap_or_else(|_| "statements,rewrite".to_owned())
}

fn rows() -> u64 {
    std::env::var("J_ROWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000)
}

fn rounds() -> usize {
    std::env::var("J_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3)
}

fn ms(t: Duration) -> f64 {
    t.as_secs_f64() * 1000.0
}

fn median(times: &[Duration]) -> Duration {
    let mut sorted = times.to_vec();
    sorted.sort();
    sorted[sorted.len() / 2]
}

fn list_ms(times: &[Duration]) -> String {
    times
        .iter()
        .map(|t| format!("{:.0}", ms(*t)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The time to send `list`, and its answer's status.
fn send(own: &Own, application: &str, list: &[Migration]) -> (Duration, u16) {
    let started = Instant::now();
    let sent = own.send(application, list);
    let took = started.elapsed();
    (took, sent.status)
}

fn send_ok(own: &Own, application: &str, list: &[Migration]) -> Duration {
    let started = Instant::now();
    let sent = own.send(application, list);
    let took = started.elapsed();
    assert_eq!(sent.status, 201, "{}", sent.describe());
    took
}

/// Waits until `db`'s outbox is empty, then vacuums what the last Version
/// left, so that no Version measured pays for the one before.
fn settle(db: &OwnDatabase) {
    shipped(db, Duration::from_secs(900));
    db.superuser().batch("VACUUM vm.outbox, big");
}

/// Waits until `db`'s outbox is empty.
fn shipped(db: &OwnDatabase, limit: Duration) {
    let started = Instant::now();
    while outbox_count(db) != 0 {
        assert!(started.elapsed() < limit, "{} not shipped within {limit:?}", db.application);
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A migration of `count` statements, each making one partition of
/// `table` (ten values each, from `from` on).
fn making(table: &str, from: usize, count: usize) -> Vec<String> {
    (from..from + count)
        .map(|i| {
            format!(
                r#"CREATE TABLE "{table}_{i}" PARTITION OF "{table}" FOR VALUES FROM ({}) TO ({});"#,
                i * 10,
                i * 10 + 10
            )
        })
        .collect()
}

/// A Version whose migration makes partitions, one a statement, under a
/// partitioned table with history (a) that had none, (b) that had 1,000;
/// beside it a Version of as many `SELECT 1`; and in (b) what one
/// `SELECT 1` costs. Medians of `J_ROUNDS`.
fn partitions(own: &Own, base: &[Migration]) {
    let mut next = 4;
    let mut list = base.to_vec();
    let mut push = |list: &mut Vec<Migration>, statements: &[String]| {
        let refs: Vec<&str> = statements.iter().map(String::as_str).collect();
        list.push(Migration::new(&format!("{next:04}_p"), &refs));
        next += 1;
    };
    let times = |list: &mut Vec<Migration>, push: &mut dyn FnMut(&mut Vec<Migration>, &[String]), app: &str, before: &dyn Fn(usize) -> Vec<String>, statements: &dyn Fn(usize) -> Vec<String>| {
        let mut taken = Vec::new();
        for round in 0..rounds() {
            let ready = before(round);
            if !ready.is_empty() {
                push(list, &ready);
                send_ok(own, app, list);
            }
            push(list, &statements(round));
            taken.push(send_ok(own, app, list));
        }
        taken
    };
    let ddl = |t: &str| format!(r#"CREATE TABLE "{t}" ("id" integer NOT NULL, "v" text, PRIMARY KEY ("id")) PARTITION BY RANGE ("id");"#);

    // (a): each round a partitioned table of its own, made empty by the
    // Version before.
    let app = "parts-a";
    let _db = own.install(app, &list);
    for count in [100_usize, 200] {
        let ones = times(&mut list, &mut push, app, &|_| Vec::new(), &|_| vec!["SELECT 1;".to_owned(); count]);
        let made = times(
            &mut list,
            &mut push,
            app,
            &|round| vec![ddl(&format!("a{count}_{round}"))],
            &|round| making(&format!("a{count}_{round}"), 0, count),
        );
        println!(
            "J partitions (a), none before: {count} `SELECT 1` median {:.0} ms [{}]; {count} partitions made median {:.0} ms [{}]; a partition {:.2} ms more than a `SELECT 1`",
            ms(median(&ones)),
            list_ms(&ones),
            ms(median(&made)),
            list_ms(&made),
            (ms(median(&made)) - ms(median(&ones))) / count as f64
        );
    }

    // (b): one partitioned table with 1,000 partitions; each round makes
    // `count` more, and the Version after drops them again.
    let app = "parts-b";
    list = base.to_vec();
    let _db = own.install(app, &list);
    push(&mut list, &[ddl("b")]);
    push(
        &mut list,
        &[format!(
            r#"DO $p$ BEGIN FOR i IN 0..999 LOOP EXECUTE format('CREATE TABLE %I PARTITION OF "b" FOR VALUES FROM (%s) TO (%s)', 'b_' || i, i * 10, i * 10 + 10); END LOOP; END $p$;"#
        )],
    );
    send_ok(own, app, &list);
    let mut one = Vec::new();
    let mut more = Vec::new();
    for _ in 0..rounds().max(5) {
        push(&mut list, &["SELECT 1;".to_owned()]);
        one.push(send_ok(own, app, &list));
        push(&mut list, &vec!["SELECT 1;".to_owned(); 301]);
        more.push(send_ok(own, app, &list));
    }
    println!(
        "J partitions (b), 1000 before: a Version of one `SELECT 1` median {:.1} ms [{}]; of 301 median {:.1} ms [{}]; a statement {:.3} ms",
        ms(median(&one)),
        list_ms(&one),
        ms(median(&more)),
        list_ms(&more),
        (ms(median(&more)) - ms(median(&one))) / 300.0
    );
    for count in [100_usize, 200] {
        let ones = times(&mut list, &mut push, app, &|_| Vec::new(), &|_| vec!["SELECT 1;".to_owned(); count]);
        let made = times(
            &mut list,
            &mut push,
            app,
            &|round| {
                if round == 0 {
                    Vec::new()
                } else {
                    vec![format!(
                        r#"DO $p$ BEGIN FOR i IN 1000..{} LOOP EXECUTE format('DROP TABLE %I', 'b_' || i); END LOOP; END $p$;"#,
                        999 + count
                    )]
                }
            },
            &|_| making("b", 1000, count),
        );
        println!(
            "J partitions (b), 1000 before: {count} `SELECT 1` median {:.0} ms [{}]; {count} partitions made median {:.0} ms [{}]; a partition {:.2} ms more than a `SELECT 1`",
            ms(median(&ones)),
            list_ms(&ones),
            ms(median(&made)),
            list_ms(&made),
            (ms(median(&made)) - ms(median(&ones))) / count as f64
        );
        // Back to 1,000 for what follows.
        push(
            &mut list,
            &[format!(
                r#"DO $p$ BEGIN FOR i IN 1000..{} LOOP EXECUTE format('DROP TABLE %I', 'b_' || i); END LOOP; END $p$;"#,
                999 + count
            )],
        );
        send_ok(own, app, &list);
    }
}

#[test]
fn j_cost() {
    let parts = parts();
    let own = start();
    let mut list = base();
    let mut next = 4;
    let mut push = |list: &mut Vec<Migration>, statements: &[&str]| {
        list.push(Migration::new(&format!("{next:04}_j"), statements));
        next += 1;
    };

    if parts.contains("statements") {
        // A statement's cost in a Version that rewrites nothing: a Version
        // whose last migration is 301 `SELECT 1` against one whose last
        // migration is one, over 300; medians of five.
        let app = "statements";
        let _db = own.install(app, &list);
        let mut tables = 0_usize;
        let mut made = 0;
        for target in [10_usize, 500] {
            let statements: Vec<String> = (tables..target)
                .map(|_| {
                    made += 1;
                    format!(
                        r#"CREATE TABLE "j_{made}" ("id" integer PRIMARY KEY NOT NULL, "a" text, "b" text, "c" integer, "d" integer, "e" timestamp with time zone, "f" jsonb, "g" boolean, "h" numeric, "i" text);"#
                    )
                })
                .collect();
            let refs: Vec<&str> = statements.iter().map(String::as_str).collect();
            push(&mut list, &refs);
            send_ok(&own, app, &list);
            tables = target;
            let many: Vec<&str> = vec!["SELECT 1;"; 301];
            let mut one = Vec::new();
            let mut more = Vec::new();
            let mut migrations = Vec::new();
            for _ in 0..5 {
                push(&mut list, &["SELECT 1;"]);
                one.push(send_ok(&own, app, &list));
                push(&mut list, &many);
                more.push(send_ok(&own, app, &list));
                for _ in 0..21 {
                    push(&mut list, &["SELECT 1;"]);
                }
                migrations.push(send_ok(&own, app, &list));
            }
            let per = (ms(median(&more)) - ms(median(&one))) / 300.0;
            let per_migration = (ms(median(&migrations)) - ms(median(&one))) / 20.0;
            println!(
                "J tables={target}: a Version with one migration of one `SELECT 1` median {:.1} ms [{}]; of 301 median {:.1} ms [{}]; a statement {per:.3} ms",
                ms(median(&one)),
                list_ms(&one),
                ms(median(&more)),
                list_ms(&more)
            );
            println!(
                "J tables={target}: a Version with 21 migrations of one `SELECT 1` median {:.1} ms [{}]; a migration {per_migration:.3} ms",
                ms(median(&migrations)),
                list_ms(&migrations)
            );
        }
    }

    if parts.contains("partitions") {
        partitions(&own, &list);
    }

    if parts.contains("rewrite") || parts.contains("breakdown") {
        let n = rows();
        let app = "rewrite";
        let db = own.install(app, &list);
        let make = format!(
            r#"CREATE TABLE "big" ("id" integer PRIMARY KEY NOT NULL, "amount" integer NOT NULL, "note" text NOT NULL, "at" timestamp with time zone NOT NULL DEFAULT '2026-01-01T00:00:00Z', "w1" varchar(50) DEFAULT 'w', "w2" varchar(50) DEFAULT 'w', "w3" varchar(50) DEFAULT 'w', "w4" varchar(50) DEFAULT 'w', "w5" varchar(50) DEFAULT 'w');"#
        );
        let fill = format!(
            r#"INSERT INTO "big" ("id", "amount", "note") SELECT g, g % 1000, 'a note of row ' || g FROM generate_series(1, {n}) g;"#
        );
        push(&mut list, &[make.as_str()]);
        push(&mut list, &[fill.as_str()]);
        let filled = send_ok(&own, app, &list);
        println!("J rows={n}: the table filled by a migration in {:.0} ms", ms(filled));
        settle(&db);
        db.superuser().batch("ANALYZE big");
        let kinds: [(&str, fn(&str) -> String); 3] = [
            ("every row changes (ALTER ... TYPE ... USING amount + 1)", |to| {
                format!(r#"ALTER TABLE "big" ALTER COLUMN "amount" SET DATA TYPE {to} USING "amount" + 1;"#)
            }),
            ("no row changes (ALTER ... TYPE)", |to| {
                format!(r#"ALTER TABLE "big" ALTER COLUMN "amount" SET DATA TYPE {to};"#)
            }),
            ("every row changes, the statement in a DO block", |to| {
                format!(
                    r#"DO $j$ BEGIN EXECUTE 'ALTER TABLE "big" ALTER COLUMN "amount" SET DATA TYPE {to} USING "amount" + 1'; END $j$;"#
                )
            }),
        ];
        let mut bigint = true;
        if parts.contains("rewrite") {
            for (what, statement) in &kinds {
                let mut times = Vec::new();
                for _ in 0..rounds() {
                    let to = if bigint { "bigint" } else { "integer" };
                    bigint = !bigint;
                    let text = statement(to);
                    push(&mut list, &[text.as_str()]);
                    let (took, status) = send(&own, app, &list);
                    assert_eq!(status, 201, "{what}");
                    times.push(took);
                    settle(&db);
                }
                println!(
                    "J rows={n}, {what}: a Version median {:.0} ms [{}]",
                    ms(median(&times)),
                    list_ms(&times)
                );
            }
            // A change of type Postgres makes in the catalog alone: each
            // round another column, `varchar(50)` to `text`.
            let mut times = Vec::new();
            for w in 1..=rounds().min(5) {
                let text = format!(r#"ALTER TABLE "big" ALTER COLUMN "w{w}" SET DATA TYPE text;"#);
                push(&mut list, &[text.as_str()]);
                let (took, status) = send(&own, app, &list);
                assert_eq!(status, 201, "varchar(50) to text");
                times.push(took);
                settle(&db);
            }
            println!(
                "J rows={n}, varchar(50) to text (no rewrite): a Version median {:.0} ms [{}]",
                ms(median(&times)),
                list_ms(&times)
            );
        }
        if parts.contains("breakdown") {
            // The vm functions' own times, from Postgres's function
            // statistics, in one more Version of each kind.
            let su = db.superuser();
            su.batch(&format!(
                "ALTER DATABASE {} SET track_functions = 'all'",
                blackbox::own_database::ident(&db.name)
            ));
            for (what, statement) in &kinds {
                let to = if bigint { "bigint" } else { "integer" };
                bigint = !bigint;
                let text = statement(to);
                push(&mut list, &[text.as_str()]);
                settle(&db);
                su.batch("SELECT pg_stat_reset()");
                let took = send_ok(&own, app, &list);
                // The migration's sessions have ended: their counts are in.
                std::thread::sleep(Duration::from_millis(1500));
                let stats = su.batch(
                    "SELECT funcname, calls::text, round(total_time::numeric, 1)::text, round(self_time::numeric, 1)::text \
                     FROM pg_stat_user_functions WHERE schemaname = 'vm' AND (funcname LIKE 'rewrite%' OR funcname = 'capture_guard') ORDER BY funcname",
                );
                shipped(&db, Duration::from_secs(900));
                let entries = platform_sql(&db)
                    .value(&format!(
                        "SELECT count(*)::text FROM change_history WHERE application = {} AND migration = {} AND kind <> 'structure'",
                        literal(&db.application),
                        literal(&list.last().unwrap().name)
                    ))
                    .unwrap_or_default();
                println!("J rows={n}, {what}, with function statistics: a Version {:.0} ms; rows' entries {entries}", ms(took));
                for row in stats {
                    let cell = |i: usize| row[i].clone().unwrap_or_default();
                    println!(
                        "J     {}: calls {}, total {} ms, self {} ms",
                        cell(0),
                        cell(1),
                        cell(2),
                        cell(3)
                    );
                }
            }
            su.batch(&format!(
                "ALTER DATABASE {} RESET track_functions",
                blackbox::own_database::ident(&db.name)
            ));
        }
    }
}
