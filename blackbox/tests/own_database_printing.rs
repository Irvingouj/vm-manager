//! The own database's role passwords never appear in an answer, a log
//! record or an error message (CONTRACT.md, "The Application's own
//! database: What it looks like in Postgres").
//!
//! The passwords are the platform's own, so the harness cannot search for
//! them directly. It does what it can from outside, after making the
//! program do everything that touches the own database (Versions with
//! migrations that apply, fail, diverge or are malformed; operations of
//! every outcome as both actors; a restart):
//!
//! - no answer and no line of the program's log holds a connection address
//!   (`postgres://`, `postgresql://`) or a `password` beside the name of
//!   one of these roles;
//! - none holds any part of the roles' stored SCRAM verifiers (salt, stored
//!   key, server key), read from `pg_authid` by the harness's superuser;
//! - no word of any answer or log line is a password: each is tried as the
//!   password of both roles, logging in to the own database. Words are
//!   split at white space, quotes, brackets and `:`, `@`, `/`, `,`, `;`,
//!   `?`, `&`, `=`; a password that holds one of those and was printed
//!   whole would be missed.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::BTreeSet;

use blackbox::own_database::{
    ACTORS, Migration, Own, OwnDatabase, literal, more_tables, own_execute, own_query, standard,
    try_login, version_body_with_raw_migrations,
};
use blackbox::{Response, User};
use serde_json::json;

fn ann() -> User {
    User::new("ann").in_groups(&["staff"])
}

/// The parts of `role`'s stored SCRAM verifier
/// (`SCRAM-SHA-256$<iterations>:<salt>$<stored key>:<server key>`) that are
/// long enough to search for.
fn verifier_parts(db: &OwnDatabase, role: &str) -> Vec<String> {
    let verifier = db
        .instance()
        .value(&format!(
            "SELECT rolpassword FROM pg_authid WHERE rolname = {}",
            literal(role)
        ))
        .unwrap_or_default();
    verifier
        .split(['$', ':'])
        .filter(|part| part.len() >= 16)
        .map(str::to_owned)
        .collect()
}

/// Every word of `text` worth trying as a password.
fn words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '"' | '\''
                    | '`'
                    | '{'
                    | '}'
                    | '['
                    | ']'
                    | '('
                    | ')'
                    | '<'
                    | '>'
                    | ':'
                    | '@'
                    | '/'
                    | ','
                    | ';'
                    | '?'
                    | '&'
                    | '='
                    | '\\'
            )
    })
    .filter(|w| (8..=200).contains(&w.len()))
    .map(str::to_owned)
    .collect()
}

#[test]
fn no_answer_and_no_log_record_holds_the_own_database_s_passwords() {
    let own = Own::start();
    let mut answers: Vec<Response> = Vec::new();
    own.create("board");
    answers.push(own.send("board", &standard()));
    let mut list = standard();
    list.push(more_tables());
    answers.push(own.send("board", &list));
    let mut broken = list.clone();
    broken.push(Migration::new(
        "0003_broken",
        &[r#"ALTER TABLE "nowhere" ADD COLUMN "x" int;"#],
    ));
    answers.push(own.send("board", &broken));
    answers.push(own.send("board", &standard()));
    answers.push(own.api().send_version(
        "board",
        &version_body_with_raw_migrations(&json!([{ "name": "bad name" }])),
    ));
    own.create("failing");
    answers.push(own.send(
        "failing",
        &[Migration::new(
            "0000_fails",
            &[
                r#"CREATE TABLE "a" (x int);"#,
                r#"CREATE TABLE "a" (x int);"#,
            ],
        )],
    ));
    let db = own.own_database("board");
    for actor in ACTORS {
        let (response, _) = own.run(
            &ann(),
            "board",
            &[
                own_query(actor, "select title from items"),
                own_execute(actor, "insert into items (title) values ('x') returning id"),
                own_query(actor, "select * from missing_table"),
                own_query(actor, "insert into items (title) values ('through a read')"),
                own_execute(actor, "select run_sql('create table sneaky (x int)')"),
                own_execute(actor, "select run_sql('set role none')"),
                own_query(actor, "create table sneaky (x int)"),
                own_query(actor, "select n from generate_series(1, 10001) n"),
                own_query(
                    actor,
                    "select current_user, session_user, current_database()",
                ),
            ],
        );
        answers.push(response);
    }
    answers.push(
        own.vm
            .api_as(&blackbox::scenario::administrator())
            .operation_log(None),
    );
    answers.push(own.api().get_version("board", 1));
    answers.push(own.api().list_versions("board"));
    let Own { vm } = own;
    let mut texts: Vec<(String, String)> = answers
        .iter()
        .map(|a| (format!("the answer to {}", a.request()), a.describe()))
        .collect();
    texts.extend(
        vm.lines()
            .into_iter()
            .map(|l| ("the program's log".to_owned(), l.raw)),
    );
    let vm = vm.restart();
    texts.extend(
        vm.lines()
            .into_iter()
            .map(|l| ("the restarted program's log".to_owned(), l.raw)),
    );

    let roles = [db.owner.clone(), db.runtime.clone()];
    let mut found = Vec::new();
    // Connection addresses and passwords beside these roles.
    for (whence, text) in &texts {
        let lower = text.to_ascii_lowercase();
        if lower.contains("postgres://") || lower.contains("postgresql://") {
            found.push(format!("a connection address in {whence}: {text}"));
        }
        for role in &roles {
            if text.contains(role.as_str()) && lower.contains("password") {
                found.push(format!(
                    "the role {role} beside a password in {whence}: {text}"
                ));
            }
        }
    }
    // The stored verifiers.
    for role in &roles {
        for part in verifier_parts(&db, role) {
            for (whence, text) in &texts {
                if text.contains(&part) {
                    found.push(format!(
                        "part of {role}'s stored SCRAM verifier in {whence}: {text}"
                    ));
                }
            }
        }
    }
    // Every word, tried as each role's password.
    let candidates: BTreeSet<String> = texts.iter().flat_map(|(_, text)| words(text)).collect();
    let candidates: Vec<&String> = candidates.iter().collect();
    eprintln!(
        "trying {} words as the own database's passwords",
        candidates.len()
    );
    let logged_in: Vec<String> = std::thread::scope(|scope| {
        let chunks: Vec<&[&String]> = candidates
            .chunks(candidates.len().div_ceil(8).max(1))
            .collect();
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                let roles = &roles;
                let db = &db;
                scope.spawn(move || {
                    let mut hits = Vec::new();
                    for word in chunk {
                        for role in roles {
                            if try_login(role, word, &db.name).is_ok() {
                                hits.push(format!(
                                    "{role}'s password, printed as the word {word:?}"
                                ));
                            }
                        }
                    }
                    hits
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });
    found.extend(logged_in);
    assert!(
        found.is_empty(),
        "expected no answer and no log record to hold the own database's passwords, but:\n{}",
        found.join("\n")
    );
    // The roles the words were tried against are there.
    for role in &roles {
        assert!(db.role_exists(role), "the role {role} is gone");
    }
}
