//! Change history of what an Application changes (CONTRACT.md, "Identity
//! and Change history in the own database: Change history"; ticket
//! "Identity in the own database, and Change history", first, second and
//! fifth acceptance criteria).
//!
//! Each row inserted, updated or deleted through `execute`, as the User or
//! as the Application, leaves one row in `vm.history` in the same
//! transaction: the table, the key as it was before (of the new row for an
//! insert; null without a primary key), the kind, the whole row before and
//! after as `to_jsonb` gives it, the columns that differ in the table's
//! order, the User in flight, the Application, the actor, the request's id
//! as in the Operation log, and a time inside the test's window. Many-row
//! statements leave one row per row; cascades and `ON CONFLICT DO UPDATE`
//! are recorded like any change; a change rolled back leaves nothing; the
//! runtime role may read `vm.history` through `query`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant};

use blackbox::User;
use blackbox::history::{
    APP, Change, assert_at_within, assert_change, assert_history_matches_operation_log,
    assert_ids_increase, db_now, history, history_after, history_standard, last_history_id,
    row_json, snapshot,
};
use blackbox::own_database::{ACTORS, Own, OwnDatabase, assert_ok, own_execute, own_query};
use blackbox::scenario::lines;
use serde_json::{Value, json};

fn alice() -> User {
    User::new("alice")
}

fn bob() -> User {
    User::new("bob")
}

/// A program and `board` with [`history_standard`].
fn installed() -> (Own, OwnDatabase) {
    let own = Own::start();
    let db = own.install(APP, &history_standard());
    (own, db)
}

/// Runs one `execute` as `actor` for `user`, failing unless it ran;
/// returns the request's id and the answer.
fn execute_ok(own: &Own, user: &User, actor: &str, sql: &str) -> (String, Value) {
    let (response, answer) = own.one(user, APP, &own_execute(actor, sql));
    assert_ok(&answer, &format!("{sql} as the {actor}"));
    (response.request_id(), answer)
}

/// The User who makes the changes as `actor` in these tests: alice as the
/// User, bob in flight when acting as the Application.
fn user_for(actor: &str) -> User {
    if actor == "user" { alice() } else { bob() }
}

#[test]
fn an_insert_an_update_and_a_delete_each_leave_one_history_row_with_every_column() {
    let (own, db) = installed();
    for actor in ACTORS {
        let user = user_for(actor);
        let before = db_now(&db);
        let last = last_history_id(&db);
        let (inserted, answer) = execute_ok(
            &own,
            &user,
            actor,
            &format!(
                "insert into things (title, size, tags) values ('made as the {actor}', 1, '{{\"colour\": \"red\"}}') returning id"
            ),
        );
        let id = answer["ok"]["rows"][0][0].as_i64().expect("an id");
        let row_after_insert = row_json(&db, "things", &format!("id = {id}"));
        let (updated, _) = execute_ok(
            &own,
            &user,
            actor,
            &format!("update things set size = 2 where id = {id}"),
        );
        let row_after_update = row_json(&db, "things", &format!("id = {id}"));
        let (deleted, _) = execute_ok(
            &own,
            &user,
            actor,
            &format!("delete from things where id = {id}"),
        );
        let after = db_now(&db);
        let rows = history_after(&db, last);
        assert_eq!(
            rows.len(),
            3,
            "expected one history row each for the insert, the update and the delete as the {actor}, got:\n{}",
            lines(&rows)
        );
        let key = json!({ "id": id });
        assert_change(
            &rows[0],
            &Change::by(&user.id, actor, &inserted).insert(key.clone(), row_after_insert.clone()),
            &format!("the insert as the {actor}"),
        );
        assert_change(
            &rows[1],
            &Change::by(&user.id, actor, &updated).update(
                key.clone(),
                row_after_insert,
                row_after_update.clone(),
                json!(["size"]),
            ),
            &format!("the update as the {actor}"),
        );
        assert_change(
            &rows[2],
            &Change::by(&user.id, actor, &deleted).delete(key, row_after_update),
            &format!("the delete as the {actor}"),
        );
        assert_ids_increase(&rows, &format!("the changes as the {actor}"));
        assert_at_within(&db, &rows, &before, &after, &format!("as the {actor}"));
        // The request is the one the Operation log names for the operation.
        for (row, request) in rows.iter().zip([&inserted, &updated, &deleted]) {
            let entries: Vec<Value> = own
                .log()
                .into_iter()
                .filter(|e| e["request"].as_str() == Some(request.as_str()))
                .collect();
            assert_eq!(
                entries.len(),
                1,
                "expected one Operation log entry for request {request}, got:\n{}",
                lines(&entries)
            );
            assert_eq!(
                row["request"], entries[0]["request"],
                "expected the history row's request to be the one in the Operation log"
            );
            assert_eq!(entries[0]["actor"].as_str(), Some(actor));
            assert_eq!(entries[0]["user"].as_str(), Some(user.id.as_str()));
        }
    }
    assert_history_matches_operation_log(&own, &db, "after both actors' changes");
}

#[test]
fn a_table_without_a_key_and_a_composite_key_in_another_schema_are_recorded() {
    let (own, db) = installed();
    for actor in ACTORS {
        let user = user_for(actor);
        let last = last_history_id(&db);
        let tag = format!("by the {actor}");
        let (a, _) = execute_ok(
            &own,
            &user,
            actor,
            &format!("insert into keyless (a, b) values ('{tag}', 1)"),
        );
        let (b, _) = execute_ok(
            &own,
            &user,
            actor,
            &format!("update keyless set b = 2 where a = '{tag}'"),
        );
        let (c, _) = execute_ok(
            &own,
            &user,
            actor,
            &format!("delete from keyless where a = '{tag}'"),
        );
        let sku = format!("sku-{actor}");
        let (d, _) = execute_ok(
            &own,
            &user,
            actor,
            &format!(
                "insert into inventory.stock (sku, warehouse, qty) values ('{sku}', 'north', 5)"
            ),
        );
        let (e, _) = execute_ok(
            &own,
            &user,
            actor,
            &format!("update inventory.stock set qty = 6 where sku = '{sku}'"),
        );
        let (f, _) = execute_ok(
            &own,
            &user,
            actor,
            &format!("update inventory.stock set warehouse = 'south' where sku = '{sku}'"),
        );
        let (g, _) = execute_ok(
            &own,
            &user,
            actor,
            &format!("delete from inventory.stock where sku = '{sku}'"),
        );
        let rows = history_after(&db, last);
        assert_eq!(rows.len(), 7, "{}", lines(&rows));
        let k1 = json!({ "a": tag, "b": 1 });
        let k2 = json!({ "a": tag, "b": 2 });
        assert_change(
            &rows[0],
            &Change::by(&user.id, actor, &a)
                .on("public", "keyless")
                .insert(Value::Null, k1.clone()),
            "keyless insert",
        );
        assert_change(
            &rows[1],
            &Change::by(&user.id, actor, &b)
                .on("public", "keyless")
                .update(Value::Null, k1, k2.clone(), json!(["b"])),
            "keyless update",
        );
        assert_change(
            &rows[2],
            &Change::by(&user.id, actor, &c)
                .on("public", "keyless")
                .delete(Value::Null, k2),
            "keyless delete",
        );
        let north5 = json!({ "sku": sku, "warehouse": "north", "qty": 5 });
        let north6 = json!({ "sku": sku, "warehouse": "north", "qty": 6 });
        let south6 = json!({ "sku": sku, "warehouse": "south", "qty": 6 });
        let north_key = json!({ "sku": sku, "warehouse": "north" });
        let south_key = json!({ "sku": sku, "warehouse": "south" });
        assert_change(
            &rows[3],
            &Change::by(&user.id, actor, &d)
                .on("inventory", "stock")
                .insert(north_key.clone(), north5.clone()),
            "stock insert",
        );
        assert_change(
            &rows[4],
            &Change::by(&user.id, actor, &e)
                .on("inventory", "stock")
                .update(north_key.clone(), north5, north6.clone(), json!(["qty"])),
            "stock update of qty",
        );
        assert_change(
            &rows[5],
            &Change::by(&user.id, actor, &f)
                .on("inventory", "stock")
                .update(north_key, north6, south6.clone(), json!(["warehouse"])),
            "stock update of a key column: the key as it was before",
        );
        assert_change(
            &rows[6],
            &Change::by(&user.id, actor, &g)
                .on("inventory", "stock")
                .delete(south_key, south6),
            "stock delete",
        );
    }
    assert_history_matches_operation_log(&own, &db, "keyless and stock");
}

#[test]
fn changed_names_the_differing_columns_in_the_tables_order_and_is_empty_when_none_differs() {
    let (own, db) = installed();
    let actor = "user";
    let (_, answer) = execute_ok(
        &own,
        &alice(),
        actor,
        "insert into things (title, size, tags) values ('first', 1, '[1]') returning id",
    );
    let id = answer["ok"]["rows"][0][0].as_i64().unwrap();
    let last = last_history_id(&db);
    let r0 = row_json(&db, "things", &format!("id = {id}"));
    // SET names tags before title: `changed` follows the table's order.
    let (both, _) = execute_ok(
        &own,
        &alice(),
        actor,
        &format!("update things set tags = '{{\"b\": 2}}', title = 'renamed' where id = {id}"),
    );
    let r1 = row_json(&db, "things", &format!("id = {id}"));
    let (same, _) = execute_ok(
        &own,
        &alice(),
        actor,
        &format!("update things set title = title, size = size where id = {id}"),
    );
    let (rekeyed, _) = execute_ok(
        &own,
        &alice(),
        actor,
        &format!("update things set id = 900000 where id = {id}"),
    );
    let r2 = row_json(&db, "things", "id = 900000");
    let rows = history_after(&db, last);
    assert_eq!(rows.len(), 3, "{}", lines(&rows));
    let key = json!({ "id": id });
    assert_change(
        &rows[0],
        &Change::by("alice", actor, &both).update(
            key.clone(),
            r0,
            r1.clone(),
            json!(["title", "tags"]),
        ),
        "an update of two columns",
    );
    assert_change(
        &rows[1],
        &Change::by("alice", actor, &same).update(key.clone(), r1.clone(), r1.clone(), json!([])),
        "an update that changes no value",
    );
    assert_change(
        &rows[2],
        &Change::by("alice", actor, &rekeyed).update(key, r1, r2, json!(["id"])),
        "an update of the primary key: the key as it was before",
    );
}

#[test]
fn many_rows_cascades_and_on_conflict_are_each_recorded_under_the_same_identity() {
    let (own, db) = installed();
    // Many rows in one statement, as the Application.
    let last = last_history_id(&db);
    let (many, _) = execute_ok(
        &own,
        &bob(),
        "application",
        "insert into things (title) select 'many ' || n from generate_series(1, 3) n",
    );
    let rows = history_after(&db, last);
    assert_eq!(rows.len(), 3, "one row per inserted row:\n{}", lines(&rows));
    for row in &rows {
        let id = row["new"]["id"].as_i64().unwrap();
        assert_change(
            row,
            &Change::by("bob", "application", &many).insert(
                json!({ "id": id }),
                row_json(&db, "things", &format!("id = {id}")),
            ),
            "a row of a many-row insert",
        );
    }
    assert_ids_increase(&rows, "a many-row insert");
    let last = last_history_id(&db);
    let (bumped, answer) = execute_ok(
        &own,
        &bob(),
        "application",
        "update things set size = coalesce(size, 0) + 1 where title like 'many %'",
    );
    assert_eq!(answer["ok"]["rows_affected"], 3);
    let rows = history_after(&db, last);
    assert_eq!(rows.len(), 3, "one row per updated row:\n{}", lines(&rows));
    assert!(
        rows.iter().all(|r| r["kind"] == "update"
            && r["changed"] == json!(["size"])
            && r["request"] == json!(bumped)
            && r["user_id"] == "bob"
            && r["actor"] == "application"),
        "{}",
        lines(&rows)
    );
    // A cascading delete: the parent and its child, both recorded as the
    // User's change. Both rows were put in by the migration that made the
    // tables, so neither was recorded until now.
    let parent = row_json(&db, "parents", "id = 1");
    let child = row_json(&db, "children", "id = 10");
    assert!(parent.is_object() && child.is_object());
    let last = last_history_id(&db);
    let (cascade, _) = execute_ok(&own, &alice(), "user", "delete from parents where id = 1");
    let rows = history_after(&db, last);
    assert_eq!(
        rows.len(),
        2,
        "expected the delete and the delete it cascaded to:\n{}",
        lines(&rows)
    );
    let of = |table: &str| {
        rows.iter()
            .find(|r| r["table_name"] == table)
            .unwrap_or_else(|| panic!("no history row for {table}:\n{}", lines(&rows)))
    };
    assert_change(
        of("parents"),
        &Change::by("alice", "user", &cascade)
            .on("public", "parents")
            .delete(json!({ "id": 1 }), parent),
        "the deleted parent",
    );
    assert_change(
        of("children"),
        &Change::by("alice", "user", &cascade)
            .on("public", "children")
            .delete(json!({ "id": 10 }), child),
        "the child deleted by the cascade",
    );
    // ON CONFLICT DO UPDATE: first an insert, then an update.
    let upsert = "insert into counters (name, n) values ('hits', 1) on conflict (name) do update set n = counters.n + 1";
    let last = last_history_id(&db);
    let (first, _) = execute_ok(&own, &alice(), "application", upsert);
    let (second, _) = execute_ok(&own, &alice(), "application", upsert);
    let rows = history_after(&db, last);
    assert_eq!(rows.len(), 2, "{}", lines(&rows));
    assert_change(
        &rows[0],
        &Change::by("alice", "application", &first)
            .on("public", "counters")
            .insert(json!({ "name": "hits" }), json!({ "name": "hits", "n": 1 })),
        "the upsert's insert",
    );
    assert_change(
        &rows[1],
        &Change::by("alice", "application", &second)
            .on("public", "counters")
            .update(
                json!({ "name": "hits" }),
                json!({ "name": "hits", "n": 1 }),
                json!({ "name": "hits", "n": 2 }),
                json!(["n"]),
            ),
        "the upsert's update",
    );
    assert_history_matches_operation_log(&own, &db, "many rows, cascade, upsert");
}

#[test]
fn a_change_that_is_rolled_back_or_fails_leaves_no_history_row() {
    let (own, db) = installed();
    execute_ok(
        &own,
        &alice(),
        "user",
        "insert into things (title, size) values ('kept', 1)",
    );
    let before = snapshot(&db);
    for actor in ACTORS {
        let failing = [
            (
                "execute",
                "insert into things (title) values ('rolled back 1'), (null)",
            ),
            (
                "execute",
                "with i as (insert into things (title) values ('rolled back 2') returning id) select 1 / (id - id) from i",
            ),
            (
                "execute",
                "update things set title = 'rolled back 3' where title = 'kept' returning 1 / (size - size)",
            ),
            (
                "execute",
                "with d as (delete from parents where id = 1 returning id) select 1 / (id - id) from d",
            ),
            (
                "execute",
                "insert into things (title) select 'rolled back 4' from generate_series(1, 10001) returning id",
            ),
            (
                "execute",
                "insert into counters (name, n) values ('c', 1), ('c', 2)",
            ),
            (
                "query",
                "insert into things (title) values ('through a read')",
            ),
            ("query", "delete from parents"),
        ];
        let calls: Vec<Value> = failing
            .iter()
            .map(|(operation, sql)| {
                if *operation == "query" {
                    own_query(actor, sql)
                } else {
                    own_execute(actor, sql)
                }
            })
            .collect();
        let (_, answers) = own.run(&user_for(actor), APP, &calls);
        for ((_, sql), answer) in failing.iter().zip(&answers) {
            assert!(
                answer.get("ok").is_none() && answer["error"].is_object(),
                "expected {sql:?} as the {actor} to fail, got {answer}"
            );
        }
        assert_eq!(
            snapshot(&db),
            before,
            "expected neither a row nor a history row left by a change that failed, as the {actor}"
        );
    }
}

#[test]
fn the_runtime_role_reads_vm_history_through_query_and_its_columns_have_the_contracts_types() {
    let (own, db) = installed();
    execute_ok(
        &own,
        &alice(),
        "user",
        "insert into things (title) values ('to be read')",
    );
    execute_ok(&own, &bob(), "application", "update things set size = 5");
    let types = db.rows(
        "SELECT column_name::text, data_type::text FROM information_schema.columns
          WHERE table_schema = 'vm' AND table_name = 'history' ORDER BY ordinal_position",
    );
    let types: Vec<(String, String)> = types
        .into_iter()
        .map(|r| (r[0].clone().unwrap(), r[1].clone().unwrap()))
        .collect();
    for (column, ty) in [
        ("id", "bigint"),
        ("at", "timestamp with time zone"),
        ("schema_name", "text"),
        ("table_name", "text"),
        ("kind", "text"),
        ("key", "jsonb"),
        ("old", "jsonb"),
        ("new", "jsonb"),
        ("changed", "jsonb"),
        ("user_id", "text"),
        ("application", "text"),
        ("actor", "text"),
        ("request", "text"),
        ("migration", "text"),
    ] {
        assert!(
            types.contains(&(column.to_owned(), ty.to_owned())),
            "expected vm.history to have a column {column} of type {ty}; its columns are {types:?}"
        );
    }
    let expected: Vec<Value> = history(&db)
        .iter()
        .map(|r| {
            json!([
                r["id"].to_string(),
                r["kind"],
                r["table_name"],
                r["user_id"],
                r["actor"],
                r["request"]
            ])
        })
        .collect();
    assert_eq!(expected.len(), 2);
    for actor in ACTORS {
        let (_, answer) = own.one(
            &alice(),
            APP,
            &own_query(
                actor,
                "select id::text as id, kind, table_name, user_id, actor, request from vm.history order by id",
            ),
        );
        assert_eq!(
            answer["ok"]["rows"],
            Value::Array(expected.clone()),
            "expected the Application to read its own history through query as the {actor}, got {answer}"
        );
    }
}

#[test]
fn a_bulk_write_records_every_row() {
    let (own, db) = installed();
    let last = last_history_id(&db);
    let began = Instant::now();
    let (request, answer) = execute_ok(
        &own,
        &alice(),
        "user",
        "insert into things (title) select 'bulk ' || n from generate_series(1, 5000) n",
    );
    let took = began.elapsed();
    assert_eq!(answer["ok"]["rows_affected"], 5000);
    let counted = db
        .superuser()
        .value(&format!(
            "SELECT count(*)::text || ' ' || count(DISTINCT key)::text FROM vm.history
              WHERE id > {last} AND kind = 'insert' AND request = '{request}' AND user_id = 'alice' AND actor = 'user'"
        ))
        .unwrap();
    assert_eq!(
        counted, "5000 5000",
        "expected one history row per row of a 5000-row insert"
    );
    // Not an assertion of the contract: the time is printed for the
    // ticket's last acceptance criterion (the cost of a bulk write).
    eprintln!("a 5000-row insert through execute, with its history, took {took:?}");
    assert!(took < Duration::from_secs(10), "{took:?}");
}
