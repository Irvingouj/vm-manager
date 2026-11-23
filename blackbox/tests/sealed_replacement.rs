//! A replacement of an own database's passwords that fails (CONTRACT.md,
//! "Resources and Grants in full: At start": "A replacement that fails ... is
//! tried again at a later use, at most once in 10 seconds for one own
//! database; the use that found it failing is `unavailable`"). The platform
//! database refuses the write: the test's administrator puts a trigger on
//! `own_databases` that raises an error for every insert and update, and
//! takes it away again. The attempts are counted in the platform Postgres's
//! server log (statement logging fully on, so a failed statement is there).
//!
//! This binary turns logging on for the instance the platform databases live
//! on, as `sealed_statements` does, and is a test binary of its own.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant};

use blackbox::VmOptions;
use blackbox::connection::SecretKey;
use blackbox::postgres::shared;
use blackbox::scenario::assert_error_kind;
use blackbox::sealed::{SHOP, Sealed, Stored};
use blackbox::server_log::{entries, log_everything, sent_by};
use serde_json::{Value, json};

/// How many statements the program sent to the platform database that write
/// the own databases' passwords, after the first `skip` entries.
fn attempts(platform: &blackbox::Platform, skip: usize) -> usize {
    let all = entries(shared());
    let mine: Vec<_> = all.into_iter().skip(skip).collect();
    sent_by(&mine, platform.name(), platform.role())
        .into_iter()
        .map(|sent| sent.sql)
        .filter(|sql| {
            sql.contains("own_databases")
                && sql.contains("password")
                && (sql.trim_start().starts_with("UPDATE")
                    || sql.trim_start().starts_with("INSERT"))
        })
        .count()
}

#[test]
fn a_replacement_that_fails_is_tried_at_most_once_in_ten_seconds_and_each_use_is_unavailable() {
    log_everything(shared());
    let s = Sealed::start(&VmOptions::new());
    s.with_shop(&[blackbox::own_database::first_tables()]);
    let platform = s.platform().clone();
    let stored = Stored::of(&platform);
    let admin = stored.admin();
    admin.batch(
        "CREATE FUNCTION refuse_the_write() RETURNS trigger LANGUAGE plpgsql AS
           $$ BEGIN RAISE EXCEPTION 'refused by the test'; END $$;
         CREATE TRIGGER refuse BEFORE INSERT OR UPDATE ON own_databases
           FOR EACH ROW EXECUTE FUNCTION refuse_the_write()",
    );
    let s = s.restarted_with(&VmOptions::new().env("VIBE_SECRET_KEY", SecretKey::fresh().expose()));
    std::thread::sleep(Duration::from_millis(700));
    let skip = entries(shared()).len();
    let began = Instant::now();
    let mut answers: Vec<Value> = Vec::new();
    while began.elapsed() < Duration::from_secs(8) {
        answers.push(s.read_shop().1);
        std::thread::sleep(Duration::from_millis(900));
    }
    for (at, answer) in answers.iter().enumerate() {
        assert_error_kind(
            answer,
            "unavailable",
            &format!("use {at} of a failing replacement"),
        );
    }
    std::thread::sleep(Duration::from_millis(500));
    let tried = attempts(&platform, skip);
    assert_eq!(
        tried,
        1,
        "expected exactly one attempt to write the new passwords in about 8 seconds of {} uses, counted {tried} statements in the platform database's server log",
        answers.len()
    );
    assert_eq!(
        s.logged("own-database-passwords-replaced"),
        Vec::<Value>::new(),
        "expected no record of a replacement that failed"
    );
    // The cause removed, a later use (after the 10 seconds) succeeds.
    admin.batch("DROP TRIGGER refuse ON own_databases");
    let waited = began.elapsed();
    if waited < Duration::from_secs(11) {
        std::thread::sleep(Duration::from_secs(11) - waited);
    }
    let (_, answer) = s.read_shop();
    assert_eq!(
        answer["ok"]["rows"],
        json!([["seeded by 0000"]]),
        "{answer}"
    );
    let replaced = s.logged("own-database-passwords-replaced");
    assert_eq!(replaced.len(), 1, "expected one record, got {replaced:?}");
    assert_eq!(replaced[0]["application"], SHOP, "{}", replaced[0]);
    assert_eq!(replaced[0]["reason"], "does-not-open", "{}", replaced[0]);
    assert_eq!(s.read_shop().1["ok"]["rows"], json!([["seeded by 0000"]]));
    assert_eq!(s.logged("own-database-passwords-replaced").len(), 1);
}
