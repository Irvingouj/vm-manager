//! Two Applications' own databases are separate (CONTRACT.md, "The
//! Application's own database": nothing can name another Application's
//! database, and its roles cannot connect to it; ticket "An Application's
//! own database", fifth acceptance criterion).
//!
//! Two Applications with the same migrations have tables of the same
//! names: each writes its own rows and reads only its own, as either actor,
//! also with requests to both at once. Nothing an operation passes names
//! another database.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::own_database::{ACTORS, Own, own_execute, own_query, standard};
use blackbox::scenario::{assert_malformed, assert_rows, op};
use serde_json::{Value, json};

fn ann() -> User {
    User::new("ann").in_groups(&["staff"])
}

#[test]
fn two_applications_with_the_same_tables_each_see_only_their_own_rows() {
    let own = Own::start();
    let board = own.install("board", &standard());
    let ledger = own.install("ledger", &standard());
    assert_ne!(board.name, ledger.name);
    for app in ["board", "ledger"] {
        let calls: Vec<Value> = ACTORS
            .iter()
            .map(|actor| {
                own_execute(
                    actor,
                    &format!(
                        "insert into items (title) values ('{app} by the {actor}') returning id"
                    ),
                )
            })
            .collect();
        let (_, answers) = own.run(&ann(), app, &calls);
        assert!(answers.iter().all(|a| a["ok"].is_object()), "{answers:?}");
    }
    // Both at once, as both actors, many times.
    let requests: Vec<(User, &str, Value)> = (0..8)
        .map(|i| {
            let app = if i % 2 == 0 { "board" } else { "ledger" };
            let actor = ACTORS[(i / 2) % 2];
            (
                User::new(&format!("u{i}")),
                app,
                json!({
                    "calls": vec![own_query(actor, "select title from items order by id"); 4],
                    "mode": "together",
                }),
            )
        })
        .collect();
    for ((_, app, _), (_, answers)) in requests.iter().zip(own.at_once(&requests)) {
        for answer in answers {
            assert_rows(
                &answer,
                &["title"],
                &json!([
                    [format!("{app} by the user")],
                    [format!("{app} by the application")]
                ]),
                &format!("a read of items in {app:?}"),
            );
        }
    }
    assert_eq!(
        board.column("SELECT title FROM items ORDER BY id"),
        vec!["board by the user", "board by the application"]
    );
    assert_eq!(
        ledger.column("SELECT title FROM items ORDER BY id"),
        vec!["ledger by the user", "ledger by the application"]
    );
    // Each sees its own database's name, and no other.
    for (app, db) in [("board", &board), ("ledger", &ledger)] {
        for actor in ACTORS {
            let (_, answer) = own.one(
                &ann(),
                app,
                &own_query(actor, "select current_database()::text as d"),
            );
            assert_rows(
                &answer,
                &["d"],
                &json!([[db.name]]),
                &format!("{app} as the {actor}"),
            );
        }
    }
}

#[test]
fn nothing_an_operation_passes_names_another_database() {
    let own = Own::start();
    let board = own.install("board", &standard());
    let ledger = own.install("ledger", &standard());
    let before = ledger.fingerprint();
    // Arguments naming the other database are ignored (unknown) or
    // malformed; none reaches it.
    let tries = [
        json!({ "own": true, "as": "user", "sql": "insert into items (title) values ('x') returning id", "database": ledger.name }),
        json!({ "own": true, "as": "application", "sql": "insert into items (title) values ('x') returning id", "application": "ledger" }),
        json!({ "own": true, "as": "application", "sql": "insert into items (title) values ('x') returning id", "own_database": ledger.name }),
    ];
    for args in &tries {
        let (_, answer) = own.one(&ann(), "board", &op("execute", args));
        assert!(
            answer["ok"].is_object(),
            "expected arguments the operation does not have to be ignored, and the insert to go to board's own database, got {answer}"
        );
    }
    let (_, answer) = own.one(
        &ann(),
        "board",
        &op("execute", &json!({ "own": "ledger", "as": "application", "sql": "insert into items (title) values ('x')" })),
    );
    assert_malformed(&answer, "own naming another Application");
    assert_eq!(
        ledger.fingerprint(),
        before,
        "expected nothing done in board to reach ledger's own database"
    );
    assert_eq!(board.column("SELECT count(*) FROM items"), vec!["3"]);
}
