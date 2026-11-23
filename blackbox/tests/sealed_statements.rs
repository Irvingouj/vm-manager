//! No statement the program sends to the platform database carries a
//! credential (CONTRACT.md, "Resources and Grants in full: What is sealed,
//! and under which key"): the values are sealed in the program, before the
//! statement is built, so the parameters of `INSERT INTO resources` and of
//! what writes `own_databases` are sealed bytes. With statement logging fully
//! on in the platform database's server (`server_log::log_everything`), the
//! server log after a registration and an own database's first Version holds
//! none of the recognisable strings, and no run of 64 lower-case hex digits
//! in a statement that names `own_databases`.
//!
//! This binary turns logging on for the instance the platform databases live
//! on, as the other tests of the server log do, and so is a test binary of
//! its own.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::VmOptions;
use blackbox::own_database::first_tables;
use blackbox::postgres::shared;
use blackbox::sealed::{Markers, Sealed, has_hex64_run};
use blackbox::server_log::{entries, log_everything, sent_by};

/// Every entry of the server's log about the platform database `name`:
/// statements, their parameters, durations and errors.
fn log_of(name: &str) -> Vec<String> {
    entries(shared())
        .into_iter()
        .filter(|e| e.database == name)
        .map(|e| e.text)
        .collect()
}

/// The statements the platform's role sent to `name` that name
/// `own_databases`, with their parameters written in.
fn naming_own_databases(platform: &blackbox::Platform) -> Vec<String> {
    sent_by(&entries(shared()), platform.name(), platform.role())
        .into_iter()
        .map(|sent| sent.as_sql())
        .filter(|sql| sql.contains("own_databases"))
        .collect()
}

#[test]
fn nothing_sent_to_the_platform_database_carries_a_credential() {
    log_everything(shared());
    let s = Sealed::start(&VmOptions::new());
    s.with_board();
    s.register_with("careless", &s.db.careless);
    s.with_shop(&[first_tables()]);
    let (_, answer) = s.read_devops();
    assert!(
        answer["ok"].is_object(),
        "the harness's own check: served, got {answer}"
    );
    let name = s.platform().name().to_owned();
    // The log is written by the server as it goes; give the last statements
    // a moment to reach it.
    std::thread::sleep(std::time::Duration::from_millis(500));
    let sent = log_of(&name);
    assert!(
        sent.iter()
            .any(|t| t.contains("INSERT INTO") && t.contains("resources")),
        "the harness's own check: the log shows the registration's INSERT into resources ({} entries)",
        sent.len()
    );
    assert!(
        sent.iter().any(|t| t.contains("own_databases")),
        "the harness's own check: the log shows a statement naming own_databases ({} entries)",
        sent.len()
    );
    let markers = Markers::of(&s.db);
    let mut found: Vec<String> = Vec::new();
    for text in &sent {
        found.extend(markers.found_in(text));
    }
    found.sort();
    found.dedup();
    let own = naming_own_databases(s.platform());
    assert!(
        own.iter()
            .any(|sql| sql.starts_with("INSERT INTO own_databases")),
        "the harness's own check: the statements sent show the INSERT INTO own_databases with its parameters ({} statements name own_databases)",
        own.len()
    );
    let with_a_run = own.iter().filter(|sql| has_hex64_run(sql)).count();
    assert!(
        found.is_empty() && with_a_run == 0,
        "expected no statement sent to the platform database to carry a credential or, naming own_databases, a run of 64 lower-case hex digits (the form an own database's password had before); the server's log shows the credentials {found:?}, and {with_a_run} statements naming own_databases with such a run"
    );
}

#[test]
fn the_statements_of_the_sealing_carry_no_credential_either() {
    log_everything(shared());
    let before = blackbox::sealed::Before::make();
    let name = before.platform.name().to_owned();
    // The preparation (clear rows written back as the administrator) is in
    // the log: only what comes after this point is looked at.
    std::thread::sleep(std::time::Duration::from_millis(700));
    let skip = entries(shared())
        .iter()
        .filter(|e| e.database == name)
        .count();
    let (s, old) = before.start(&VmOptions::new());
    assert_eq!(
        s.logged("credentials-sealed").len(),
        1,
        "the harness's own check: the start sealed"
    );
    std::thread::sleep(std::time::Duration::from_millis(700));
    let all = entries(shared());
    let mine: Vec<_> = all
        .iter()
        .filter(|e| e.database == name)
        .skip(skip)
        .cloned()
        .collect();
    assert!(
        mine.iter()
            .any(|e| e.text.contains("resources") || e.text.contains("own_databases")),
        "the harness's own check: the log shows the sealing's statements ({} entries)",
        mine.len()
    );
    let mut found: Vec<String> = Vec::new();
    for e in &mine {
        found.extend(old.markers.found_in(&e.text));
    }
    found.sort();
    found.dedup();
    let with_run = sent_by(&mine, &name, old.platform.role())
        .into_iter()
        .map(|sent| sent.as_sql())
        .filter(|sql| sql.contains("own_databases") && has_hex64_run(sql))
        .count();
    assert!(
        found.is_empty() && with_run == 0,
        "expected the sealing's statements to carry no credential, but the log shows {found:?} and {with_run} statements naming own_databases with a 64-hex run"
    );
}
