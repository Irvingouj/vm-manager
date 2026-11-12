//! The identity label (CONTRACT.md, "The identity label"): the host's first
//! statement in each transaction on a shared database sets the
//! transaction-local settings `vm.user`, `vm.application` and `vm.request`,
//! readable by Application SQL and by the database owner's own triggers.
//! They are the request's own, also with many Users at once; they are gone
//! when the transaction ends, so the next operation on the same connection
//! sees its own; and an Application that overwrites them in its statement
//! changes nothing for anyone else and nothing in the Operation log.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::{BTreeMap, BTreeSet};

use blackbox::User;
use blackbox::scenario::{
    READERS, RESOURCE, Scenario, WRITERS, assert_executed, assert_rows, lines, op, query_sql,
    sql_on,
};
use blackbox::shared_database::{Session, observe_insert, observe_select};
use serde_json::{Value, json};

const READ_LABEL: &str = "select current_setting('vm.user') as u, \
     current_setting('vm.application') as a, current_setting('vm.request') as r";

const WRITE_LABEL: &str = "insert into notes (body) values ('labelled') \
     returning label_user, label_application, label_request, current_setting('vm.user') as u";

/// Fails unless the session's label names this User, Application and
/// request.
fn assert_label(session: &Session, user: &str, application: &str, request: &str, context: &str) {
    assert_eq!(
        (
            session.vm_user.as_deref(),
            session.vm_application.as_deref(),
            session.vm_request.as_deref()
        ),
        (Some(user), Some(application), Some(request)),
        "{context}: expected the label (vm.user, vm.application, vm.request) of the operation's own request, got {session:?}"
    );
}

#[test]
fn the_label_names_the_user_the_application_and_the_request() {
    let s = Scenario::writable();
    let rita = User::new("rita").in_groups(&[READERS]);
    let (response, answer) = s.one(&rita, "viewer", &query_sql(READ_LABEL));
    assert_rows(
        &answer,
        &["u", "a", "r"],
        &json!([["rita", "viewer", response.request_id()]]),
        "the label read by Application SQL in a read",
    );
    let wanda = User::new("wanda").in_groups(&[WRITERS]);
    let (response, answer) = s.execute(&wanda, "board", &sql_on(RESOURCE, WRITE_LABEL));
    let id = response.request_id();
    assert_executed(
        &answer,
        &["label_user", "label_application", "label_request", "u"],
        &json!([["wanda", "board", id, "wanda"]]),
        1,
        "the label read by the owner's trigger and by Application SQL in a write",
    );
    assert_eq!(
        s.db.superuser().batch(
            "select label_user, label_application, label_request from notes where body = 'labelled'"
        ),
        vec![vec![
            Some("wanda".to_owned()),
            Some("board".to_owned()),
            Some(id)
        ]],
        "expected the row as the owner's trigger labelled it, in the shared database"
    );
    // A User id need not be ASCII; it is handed over unchanged.
    let zoe = User::new("zoë ☃").in_groups(&[READERS]);
    let (response, answer) = s.one(&zoe, "viewer", &query_sql(READ_LABEL));
    assert_rows(
        &answer,
        &["u", "a", "r"],
        &json!([["zoë ☃", "viewer", response.request_id()]]),
        "a User id that is not ASCII",
    );
}

#[test]
fn many_users_at_once_each_see_their_own_label() {
    let s = Scenario::writable();
    let requests: Vec<(User, &str, Value)> = (0..16)
        .map(|i| {
            let (user, application, operation, sql) = if i % 2 == 0 {
                (
                    User::new(&format!("reader-{i}")).in_groups(&[READERS]),
                    "viewer",
                    "query",
                    observe_select(50),
                )
            } else {
                (
                    User::new(&format!("writer-{i}")).in_groups(&[WRITERS]),
                    "board",
                    "execute",
                    observe_insert(50),
                )
            };
            (
                user,
                application,
                json!({ "calls": vec![op(operation, &sql_on(RESOURCE, &sql)); 4], "mode": "together" }),
            )
        })
        .collect();
    let results = s.at_once(&requests);
    for ((user, application, _), (response, answers)) in requests.iter().zip(&results) {
        for answer in answers {
            let session = Session::from_answer(answer)
                .unwrap_or_else(|| panic!("expected an observation, got {answer}"));
            assert_label(
                &session,
                &user.id,
                application,
                &response.request_id(),
                &format!("{} in {application}, one of 16 Users at once", user.id),
            );
        }
    }
}

#[test]
fn the_next_operation_on_the_same_connection_sees_its_own_label() {
    let s = Scenario::writable();
    let users = [
        (User::new("ann").in_groups(&[READERS, WRITERS]), "viewer"),
        (User::new("ben").in_groups(&[READERS, WRITERS]), "board"),
    ];
    let mut by_connection: BTreeMap<i64, BTreeSet<String>> = BTreeMap::new();
    for i in 0..12 {
        let (user, application) = &users[i % 2];
        for (operation, sql) in [("query", observe_select(0)), ("execute", observe_insert(0))] {
            let (response, answer) =
                s.one(user, application, &op(operation, &sql_on(RESOURCE, &sql)));
            let session = Session::from_answer(&answer)
                .unwrap_or_else(|| panic!("expected an observation, got {answer}"));
            assert_label(
                &session,
                &user.id,
                application,
                &response.request_id(),
                &format!(
                    "operation {i} ({operation}) by {} in {application}, one after another",
                    user.id
                ),
            );
            by_connection
                .entry(session.pid)
                .or_default()
                .insert(user.id.clone());
        }
    }
    let shared = by_connection
        .values()
        .filter(|users| users.len() > 1)
        .count();
    eprintln!(
        "{shared} of {} connections served both Users one after the other",
        by_connection.len()
    );
}

#[test]
fn an_application_that_overwrites_the_label_changes_nothing_for_anyone_else() {
    let s = Scenario::writable();
    let forge = |local: bool| {
        format!(
            "(select * from ts_stat($vm$select to_tsvector('simple', 'forged ' || \
               set_config('vm.user', 'it-admin', {local}) || ' ' || \
               set_config('vm.application', 'payroll', {local}) || ' ' || \
               set_config('vm.request', 'req-forged', {local}))$vm$) limit 1) forged"
        )
    };
    let forgers = [
        (
            User::new("rita").in_groups(&[READERS]),
            "viewer",
            [
                query_sql(&format!(
                    "select current_setting('vm.user') as u from {}",
                    forge(false)
                )),
                query_sql(&format!(
                    "select current_setting('vm.user') as u from {}",
                    forge(true)
                )),
            ],
        ),
        (
            User::new("wanda").in_groups(&[WRITERS]),
            "board",
            [
                op(
                    "execute",
                    &sql_on(
                        RESOURCE,
                        &format!(
                            "insert into notes (body) select 'forged' from {} returning label_user",
                            forge(false)
                        ),
                    ),
                ),
                op(
                    "execute",
                    &sql_on(
                        RESOURCE,
                        &format!(
                            "insert into notes (body) select 'forged' from {} returning label_user",
                            forge(true)
                        ),
                    ),
                ),
            ],
        ),
    ];
    // Overwritten on as many connections as can be reached at once.
    let bodies: Vec<(User, &str, Value)> = forgers
        .iter()
        .flat_map(|(user, application, calls)| {
            (0..3).map(move |_| {
                (
                    user.clone(),
                    *application,
                    json!({ "calls": [calls[0], calls[1], calls[0], calls[1]], "mode": "together" }),
                )
            })
        })
        .collect();
    let forged = s.at_once(&bodies);
    for ((user, application, _), (response, answers)) in bodies.iter().zip(&forged) {
        for answer in answers {
            assert!(
                answer["ok"].is_object(),
                "expected the overwrite to run (it is the Application's to do in its own transaction), got {answer}"
            );
        }
        // The Operation log names the real User, Application and request.
        let entries = s.entries_of(response);
        assert_eq!(entries.len(), 4, "{}", lines(&entries));
        for entry in &entries {
            assert_eq!(
                (
                    entry["user"].as_str(),
                    entry["application"].as_str(),
                    entry["request"].as_str()
                ),
                (
                    Some(user.id.as_str()),
                    Some(*application),
                    Some(response.request_id().as_str())
                ),
                "expected the Operation log to ignore the overwritten label, got {entry}"
            );
        }
    }
    let log = serde_json::to_string(&s.log()).unwrap();
    for forged_value in ["it-admin", "payroll", "req-forged"] {
        assert!(
            !log.contains(forged_value),
            "expected nothing of the overwritten label in the Operation log, found {forged_value:?}"
        );
    }

    // Everyone else, at once, on every connection: their own label.
    let others = [
        (User::new("bob").in_groups(&[READERS]), "viewer", "query"),
        (User::new("wendy").in_groups(&[WRITERS]), "board", "execute"),
        (User::new("bob").in_groups(&[READERS]), "board", "query"),
        (User::new("wendy").in_groups(&[WRITERS]), "board", "execute"),
    ];
    let bodies: Vec<(User, &str, Value)> = others
        .iter()
        .map(|(user, application, operation)| {
            let sql = if *operation == "execute" {
                observe_insert(100)
            } else {
                observe_select(100)
            };
            (
                user.clone(),
                *application,
                json!({ "calls": vec![op(operation, &sql_on(RESOURCE, &sql)); 8], "mode": "together" }),
            )
        })
        .collect();
    for ((user, application, _), (response, answers)) in bodies.iter().zip(s.at_once(&bodies)) {
        for answer in &answers {
            let session = Session::from_answer(answer)
                .unwrap_or_else(|| panic!("expected an observation, got {answer}"));
            assert_label(
                &session,
                &user.id,
                application,
                &response.request_id(),
                &format!(
                    "{} in {application} after others overwrote the label",
                    user.id
                ),
            );
        }
    }
    // The owner's trigger recorded only real Users for everyone else's rows.
    let labels =
        s.db.superuser()
            .batch("select distinct label_user from notes where body <> 'forged' order by 1");
    assert_eq!(
        labels,
        vec![vec![Some("wendy".to_owned())]],
        "expected the rows written by others to carry their own label"
    );
}
