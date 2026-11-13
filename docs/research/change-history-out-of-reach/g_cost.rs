//! Part G of docs/research/change-history-out-of-reach.md: what the second
//! slice of ticket #57 costs, measured with the real program.
//!
//! Not a test of the suite: `g-cost.sh` copies this file into
//! `blackbox/tests/` for the length of one run and removes it again, so that
//! it drives the real `vm` binary (`VM_BIN`) beside the harness's Postgres
//! through the harness, as the black-box tests do. `G_PARTS` says which
//! parts run: `writes`, `migrations` (both work with a program from before
//! the slice), `functions` (needs the slice's functions in `vm`).
//!
//! It prints lines beginning with `G ` and asserts only that what it times
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
use blackbox::out_of_reach::{WRITER, base, install_seeded, start};
use blackbox::own_database::{Migration, Own, OwnDatabase, ident, own_execute};

const APPLICATION: &str = "cost";

fn parts() -> String {
    std::env::var("G_PARTS").unwrap_or_else(|_| "writes,migrations,functions".to_owned())
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

/// One `execute` of one INSERT through the Application, timed from the
/// request to its answer.
fn write(own: &Own, id: i64) -> Duration {
    let call = own_execute(
        "user",
        &format!("insert into things (id, title) values ({id}, 'a write')"),
    );
    let started = Instant::now();
    let (_, answer) = own.one(&User::new(WRITER), APPLICATION, &call);
    let took = started.elapsed();
    assert!(answer["ok"].is_object(), "a write failed: {answer}");
    took
}

/// The time to send `list` (accepted).
fn send(own: &Own, list: &[Migration]) -> Duration {
    let started = Instant::now();
    let sent = own.send(APPLICATION, list);
    let took = started.elapsed();
    assert_eq!(sent.status, 201, "{}", sent.describe());
    took
}

/// `sql` run `n` times as the superuser in the own database, after `setup`.
fn repeat(db: &OwnDatabase, setup: &str, sql: &str, n: usize) -> Duration {
    let session = db.superuser();
    if !setup.is_empty() {
        session.batch(setup);
    }
    session.batch(sql);
    let started = Instant::now();
    for _ in 0..n {
        session.batch(sql);
    }
    started.elapsed() / u32::try_from(n).unwrap()
}

#[test]
fn g_cost() {
    let parts = parts();
    let own = start();
    let db = install_seeded(&own, APPLICATION);
    let mut list = base();
    let mut next = 4;
    let mut push = |list: &mut Vec<Migration>, statements: &[&str]| {
        list.push(Migration::new(&format!("{next:04}_g"), statements));
        next += 1;
    };

    if parts.contains("writes") {
        for i in 0..30 {
            write(&own, 100_000 + i);
        }
        let times: Vec<Duration> = (0..400).map(|i| write(&own, 200_000 + i)).collect();
        println!("G write (one INSERT through execute, request to answer): {}", summary(&times));
    }

    if parts.contains("migrations") {
        // A migration of one `SELECT 1`, and one of 301: the difference over
        // 300 is what one more statement of a migration costs, as sent.
        let one: Vec<&str> = vec!["SELECT 1;"];
        let many: Vec<&str> = vec!["SELECT 1;"; 301];
        let mut short = Vec::new();
        let mut long = Vec::new();
        for _ in 0..5 {
            push(&mut list, &one);
            short.push(send(&own, &list));
            push(&mut list, &many);
            long.push(send(&own, &list));
        }
        short.sort();
        long.sort();
        let per = long[2].saturating_sub(short[2]) / 300;
        println!(
            "G migration: a Version with 1 statement {} ; with 301 statements {} ; per statement (medians) {:.3} ms",
            summary(&short),
            summary(&long),
            per.as_secs_f64() * 1000.0
        );
    }

    if parts.contains("functions") {
        let guard = repeat(&db, "", "SELECT vm.capture_guard()", 2000);
        let alone = repeat(&db, "", "SELECT 1", 2000);
        println!(
            "G capture_guard: {:.1} us a call, round trip alone {:.1} us, so {:.1} us more per captured statement",
            guard.as_secs_f64() * 1e6,
            alone.as_secs_f64() * 1e6,
            guard.saturating_sub(alone).as_secs_f64() * 1e6
        );
        let check = repeat(
            &db,
            "",
            "BEGIN; SELECT vm.before_commit(); COMMIT",
            2000,
        );
        let bare = repeat(&db, "", "BEGIN; SELECT 1; COMMIT", 2000);
        println!(
            "G before_commit: {:.1} us a transaction with it, {:.1} us with SELECT 1 instead, so {:.1} us more",
            check.as_secs_f64() * 1e6,
            bare.as_secs_f64() * 1e6,
            check.saturating_sub(bare).as_secs_f64() * 1e6
        );
        let setup = format!("SET ROLE {}", ident(&db.owner));
        let mut tables: usize = db
            .superuser()
            .value(
                "SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                  WHERE c.relkind IN ('r', 'p') AND n.nspname NOT IN ('vm', 'information_schema')
                    AND n.nspname NOT LIKE 'pg\\_%'",
            )
            .unwrap()
            .parse()
            .unwrap();
        let mut made = 0;
        for target in [10_usize, 100, 500] {
            if tables < target {
                let statements: Vec<String> = (tables..target)
                    .map(|_| {
                        made += 1;
                        format!(
                            r#"CREATE TABLE "g_{made}" ("id" integer PRIMARY KEY NOT NULL, "a" text, "b" text, "c" integer, "d" integer, "e" timestamp with time zone, "f" jsonb, "g" boolean, "h" numeric, "i" text);"#
                        )
                    })
                    .collect();
                let refs: Vec<&str> = statements.iter().map(String::as_str).collect();
                push(&mut list, &refs);
                send(&own, &list);
                tables = target;
            }
            db.superuser().batch("ANALYZE");
            let state = repeat(&db, &setup, "SELECT count(*) FROM vm.capture_state()", 50);
            let problems = repeat(&db, &setup, "SELECT count(*) FROM vm.capture_problems()", 50);
            let structure = repeat(&db, &setup, "SELECT count(*) FROM vm.structure_state()", 50);
            let alone = repeat(&db, &setup, "SELECT 1", 50);
            let none_new: Vec<Duration> = (0..5).map(|_| send(&own, &list)).collect();
            println!(
                "G tables={target}: capture_state {:.2} ms, capture_problems {:.2} ms, structure_state {:.2} ms (round trip alone {:.2} ms); a Version with nothing to apply (the stored comparison included) {}",
                state.as_secs_f64() * 1000.0,
                problems.as_secs_f64() * 1000.0,
                structure.as_secs_f64() * 1000.0,
                alone.as_secs_f64() * 1000.0,
                summary(&none_new)
            );
        }
    }
    drop(db);
}
