//! Identity in a transaction on the own database (CONTRACT.md,
//! "Transactions: Identity, the Operation log and the Change history";
//! ticket "Transactions through the SDK", criterion 5, "changes in the own
//! database carry identity into the Change history"):
//!
//! - the identity is attached when the transaction begins and holds for
//!   all of it: `vm.identity()` answers the same in every statement;
//! - every change made in the transaction is in the Change history with
//!   the User in flight, the Application, the actor and the request;
//! - a transaction rolled back leaves no history row.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::history::{Change, assert_change, assert_ids_increase, history_where};
use blackbox::shared_database::token;
use blackbox::transactions::{
    APP, Target, Txs, assert_began, assert_ended, begin, commit, execute_in, in_tx_with, query_in,
    rollback, wanda,
};
use serde_json::json;

const IDENTITY: &str = "select user_id, application, actor, request from vm.identity()";

#[test]
fn the_identity_holds_for_the_whole_transaction_and_every_change_is_in_the_history() {
    let t = Txs::start();
    let user = wanda();
    for actor in ["user", "application"] {
        let target = Target::Own(actor);
        let marker = format!("identity-{actor}-{}", token());
        let (response, answers) = t.run(
            &user,
            APP,
            &[
                begin(target, "write", "t"),
                query_in("t", IDENTITY),
                in_tx_with(
                    "execute",
                    "t",
                    "insert into things (title, size) values ($1, 1) returning id",
                    &json!([marker]),
                ),
                query_in("t", IDENTITY),
                in_tx_with(
                    "execute",
                    "t",
                    "update things set size = 2 where title = $1",
                    &json!([marker]),
                ),
                query_in("t", IDENTITY),
                in_tx_with(
                    "execute",
                    "t",
                    "delete from things where title = $1",
                    &json!([marker]),
                ),
                commit("t"),
            ],
        );
        let context = format!("as the {actor}");
        assert_began(&answers[0], &context);
        let request = response.request_id();
        let expected = json!([[user.id, APP, actor, request]]);
        for i in [1, 3, 5] {
            assert_eq!(
                answers[i]["ok"]["rows"], expected,
                "{context}: vm.identity() in statement {i} of the transaction must be the identity attached at the begin"
            );
        }
        assert_ended(&answers[7], &context);
        let rows = history_where(&t.own, &format!("h.request = '{request}'"));
        assert_eq!(
            rows.len(),
            3,
            "{context}: expected the insert, the update and the delete in the Change history, got {rows:#?}"
        );
        assert_ids_increase(&rows, &context);
        let id = rows[0]["key"]["id"].clone();
        let row = |size: i64| json!({ "id": id, "title": marker, "size": size, "tags": null });
        let by = Change::by(&user.id, actor, &request);
        assert_change(
            &rows[0],
            &by.clone().insert(json!({ "id": id }), row(1)),
            &context,
        );
        assert_change(
            &rows[1],
            &by.clone()
                .update(json!({ "id": id }), row(1), row(2), json!(["size"])),
            &context,
        );
        assert_change(&rows[2], &by.delete(json!({ "id": id }), row(2)), &context);
    }
}

#[test]
fn a_read_transaction_on_the_own_database_has_the_identity_and_a_rolled_back_one_no_history() {
    let t = Txs::start();
    let user = wanda();
    let marker = format!("rolled-identity-{}", token());
    let (response, answers) = t.run(
        &user,
        APP,
        &[
            begin(Target::Own("application"), "read", "r"),
            query_in("r", IDENTITY),
            commit("r"),
            begin(Target::Own("user"), "write", "w"),
            execute_in(
                "w",
                &format!("insert into things (title) values ('{marker}')"),
            ),
            rollback("w"),
        ],
    );
    assert_began(&answers[0], "read");
    assert_eq!(
        answers[1]["ok"]["rows"],
        json!([[user.id, APP, "application", response.request_id()]]),
        "vm.identity() in a read transaction"
    );
    assert_ended(&answers[2], "commit");
    assert_began(&answers[3], "write");
    assert_ended(&answers[5], "rollback");
    let rows = history_where(&t.own, &format!("h.request = '{}'", response.request_id()));
    assert!(
        rows.is_empty(),
        "a change rolled back leaves no history row, got {rows:#?}"
    );
}
