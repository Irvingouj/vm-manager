//! The rules on a migration's text are checked before any statement of any
//! migration is sent (CONTRACT.md, "The platform's part of an own database
//! is out of a migration's reach: How migrations are sent, and what is
//! refused before anything is sent": "Rules checked on the text, before any
//! statement of any migration is sent").
//!
//! With statement logging fully on, a Version whose first pending migration
//! is harmless and whose second breaks a rule is refused, and the server
//! log shows that no statement of the first one reached the own database
//! from any role but the harness's own superuser. A test binary of its own:
//! it turns logging on for the whole instance its tests share.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::out_of_reach::{
    Answers, assert_answer, base_and, install_seeded, refused, start, untouched,
};
use blackbox::own_database::{Migration, OwnDatabase};
use blackbox::postgres::{SUPERUSER, shared};
use blackbox::server_log::{entries, log_everything};
use blackbox::shared_database::token;

const UNPARSEABLE: Answers = &[refused("unparseable")];
const TRANSACTION_CONTROL: Answers = &[refused("transaction-control")];

/// Fails unless no statement the server log shows sent to `db` by a role
/// other than the harness's superuser holds `text`. (A statement's
/// parameters are left out: the platform may record a migration's text as
/// a value without sending it as a statement.)
fn assert_program_never_sent(db: &OwnDatabase, text: &str, context: &str) {
    let found: Vec<String> = entries(shared())
        .into_iter()
        .filter(|e| {
            e.database == db.name
                && e.user != SUPERUSER
                && (e.text.starts_with("LOG:  statement: ") || e.text.starts_with("LOG:  execute "))
                && e.text.contains(text)
        })
        .map(|e| format!("[{}] {}: {}", e.pid, e.user, e.text))
        .collect();
    assert!(
        found.is_empty(),
        "{context}: expected nothing holding {text:?} sent to {} by the program, but the server log shows:\n{}",
        db.name,
        found.join("\n")
    );
}

#[test]
fn no_statement_of_any_migration_is_sent_when_a_later_one_breaks_a_rule() {
    log_everything(shared());
    let own = start();
    for (i, (rule, hostile)) in [
        ("transaction-control", "COMMIT; BEGIN;"),
        ("unparseable", "CREATE TABLE ( broken;"),
    ]
    .into_iter()
    .enumerate()
    {
        let application = format!("unsent-{i}");
        let db = install_seeded(&own, &application);
        // The harness's own check that the log holds what the program sends.
        let seen = entries(shared()).into_iter().any(|e| {
            e.database == db.name
                && e.user != SUPERUSER
                && e.text.contains("CREATE SEQUENCE \"stash\"")
        });
        assert!(
            seen,
            "the harness's own check: expected the server log to show the first Version's statements sent to {}",
            db.name
        );
        let before = untouched(&db);
        let marker = format!("never-sent-{}", token());
        let select_marker = format!("SELECT '{marker}' AS \"marker\";");
        let pending = [
            Migration::new(
                "0004_harmless",
                &[&select_marker, "SELECT setval('stash', 777);"],
            ),
            Migration::new("0005_hostile", &[hostile]),
        ];
        let sent = own.send(&application, &base_and(&pending));
        let answers = if rule == "unparseable" {
            UNPARSEABLE
        } else {
            TRANSACTION_CONTROL
        };
        assert_answer(&sent, answers, "0005_hostile", None);
        assert_program_never_sent(&db, &marker, &format!("a Version refused as {rule}"));
        assert_program_never_sent(
            &db,
            "setval('stash', 777)",
            &format!("a Version refused as {rule}"),
        );
        assert_eq!(
            untouched(&db),
            before,
            "expected the own database untouched by a Version refused as {rule}"
        );
    }
}
