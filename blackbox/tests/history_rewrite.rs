//! A rewrite records each row it changed (CONTRACT.md, "A rewrite records
//! each row it changed"; ticket "A migration that rewrites values records
//! each changed row").
//!
//! Every test installs `board` with the tables of
//! `blackbox::history_rewrite::rewrite_tables`, which a User then writes so
//! that each has history, and sends a later Version whose migration writes
//! a table's rows anew. Afterwards `change_history` holds, besides the
//! `column-type` or `table-rewritten` entry, one entry per row whose value
//! differs: `kind` `update` (or `delete` and `insert` when the key
//! changed), `actor` `migration`, the Contributor, the migration's name, no
//! request, `key` after the statement, `old` before it and `new` after it,
//! `changed` by the names after it, in the table's column order. The
//! entries are compared whole, not counted.
//!
//! - When: `ALTER TABLE` itself, `EXECUTE` in a `DO` block, a function and
//!   a procedure; a table the same migration made has none, one an earlier
//!   migration of the Version made has them; a partition's rows under the
//!   partitioned table at the top; an `INHERITS` child under its own name.
//! - What: columns compared; when a value differs; the same row by key;
//!   ids; a refused or failed Version; `history` with the row's key; a
//!   statement that both updates and rewrites.
//! - A table without a primary key: `rewrite-without-key`.
//! - The reading cannot be made to lie: `rewrite-row-security`,
//!   `json-cast`, a virtual generated column (Postgres 18), and whatever a
//!   migration calls in `vm`.
//! - A statement may run more than once: what stays is one run's;
//!   `rewrite-unsteady`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::history::{Change, assert_change, last_history_id};
use blackbox::history_op::changes_of;
use blackbox::history_rewrite::{
    Rewrite, assert_between, assert_refused, assert_row_entries, id_key, migration_update,
    row_entries, structure_after,
};
use blackbox::history_structure::answered_ids;
use blackbox::own_database::{Migration, assert_ok, ident, literal, own_execute};
use blackbox::scenario::lines;
use blackbox::{INSTALLER, User};
use serde_json::{Value, json};

const ACTORS: [&str; 2] = ["user", "application"];

/// Fails unless the later Version left no entry at all after `last`: no row
/// entry and no `structure` entry (a Version refused or failed).
fn assert_nothing_after(r: &Rewrite, last: i64, context: &str) {
    let rows = blackbox::history::history_after(&r.db, last);
    assert!(
        rows.is_empty(),
        "{context}: expected no entry, row or structure; got\n{}",
        lines(&rows)
    );
}

/// The row entries of `rows` made by `migration`.
fn of_migration(rows: &[Value], migration: &str) -> Vec<Value> {
    rows.iter()
        .filter(|r| r["migration"] == migration)
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// The acceptance criteria.
// ---------------------------------------------------------------------------

/// CONTRACT.md: "A row whose value differs is one entry: `kind` `update`,
/// `actor` `migration`, `user_id` the Contributor who sent the Version,
/// `migration` the migration's name, `request` null, `key` ..., `old` ...,
/// `new` ..., `changed` the columns whose values differ"; "A row whose
/// value does not differ has no entry." The ticket's first criterion:
/// `ALTER COLUMN amount TYPE numeric USING 0`.
#[test]
fn using_0_records_an_update_per_row_whose_amount_was_not_0() {
    let mut r = Rewrite::start();
    let last = last_history_id(&r.db);
    let migration = "0004_zero_amounts";
    r.send_ok(&[Migration::new(
        migration,
        &[r#"ALTER TABLE "amounts" ALTER COLUMN "amount" SET DATA TYPE numeric USING 0;"#],
    )]);
    let rows = row_entries(&r.db, last);
    assert_row_entries(
        &rows,
        &[
            migration_update(
                migration,
                "amounts",
                id_key(1),
                json!({ "id": 1, "amount": 5, "note": "five!" }),
                json!({ "id": 1, "amount": 0, "note": "five!" }),
                json!(["amount"]),
            ),
            migration_update(
                migration,
                "amounts",
                id_key(3),
                json!({ "id": 3, "amount": null, "note": "none" }),
                json!({ "id": 3, "amount": 0, "note": "none" }),
                json!(["amount"]),
            ),
        ],
        "amount 5 and amount null made 0: one update each; amount 0 made 0: none",
    );
    let structure = structure_after(&r.db, last);
    assert_eq!(
        structure.len(),
        1,
        "the harness's own check: the column-type entry of #62: {}",
        lines(&structure)
    );
    assert_between(
        &rows,
        None,
        &structure,
        "the rewrite's entries and the column-type entry",
    );
}

/// CONTRACT.md: "A change of type that changes no value records no row and
/// is not refused: `integer` to `bigint`, `varchar(50)` to `text`,
/// `numeric(10,2)` to `numeric(12,4)`, and whatever writes rows anew
/// without changing them (`SET UNLOGGED`, a column added with a volatile
/// default)."
#[test]
fn a_type_change_that_changes_no_value_records_no_row_and_is_not_refused() {
    let mut r = Rewrite::start();
    let last = last_history_id(&r.db);
    let tables = ["wide", "amounts", "other"];
    let before: Vec<Option<String>> = tables.iter().map(|t| r.filenode(t)).collect();
    let rows_before: Vec<Vec<String>> = tables.iter().map(|t| r.rows_of(t)).collect();
    let sent = r.send(&[Migration::new(
        "0004_same_values",
        &[
            r#"ALTER TABLE "wide" ALTER COLUMN "i" SET DATA TYPE bigint;"#,
            r#"ALTER TABLE "wide" ALTER COLUMN "v" SET DATA TYPE text;"#,
            r#"ALTER TABLE "wide" ALTER COLUMN "n" SET DATA TYPE numeric(12, 4);"#,
            r#"ALTER TABLE "amounts" SET UNLOGGED;"#,
            r#"ALTER TABLE "other" ADD COLUMN "r" double precision DEFAULT random();"#,
        ],
    )]);
    assert_eq!(
        sent.status,
        201,
        "a change of type that changes no value is not refused:\n{}",
        sent.describe()
    );
    for ((table, b), rows) in tables.iter().zip(&before).zip(&rows_before) {
        let a = r.filenode(table);
        assert!(
            b.is_some() && *b != a,
            "the harness's own check: Postgres wrote {table} anew (file node {b:?} then {a:?})"
        );
        if *table != "other" {
            assert_eq!(
                r.rows_of(table).len(),
                rows.len(),
                "the harness's own check: {table}"
            );
        }
    }
    let rows = row_entries(&r.db, last);
    assert!(
        rows.is_empty(),
        "integer to bigint, varchar(50) to text, numeric(10,2) to numeric(12,4) (1.50 is 1.5000), SET UNLOGGED, a column added with random(): no row entry; got\n{}",
        lines(&rows)
    );
}

/// CONTRACT.md: "It is seen from the catalogs, not from the statement's
/// text, so it is recorded the same however the statement does it: `ALTER
/// TABLE` itself, dynamic SQL in a `DO` block, a function or a procedure
/// the statement calls." One Version, three migrations, each rewriting
/// `amounts` its own way.
#[test]
fn the_same_rewrite_through_a_do_block_a_function_or_a_procedure_is_recorded_the_same() {
    let mut r = Rewrite::start();
    let last = last_history_id(&r.db);
    let (through_do, through_function, through_procedure) = (
        "0004_through_do",
        "0005_through_function",
        "0006_through_procedure",
    );
    r.send_ok(&[
        Migration::new(
            through_do,
            &[
                r#"DO $d$ BEGIN EXECUTE 'ALTER TABLE "amounts" ALTER COLUMN "amount" SET DATA TYPE numeric USING 0'; END $d$;"#,
            ],
        ),
        Migration::new(
            through_function,
            &[
                r#"CREATE FUNCTION "retype_amounts"() RETURNS void LANGUAGE plpgsql AS $f$
BEGIN
  EXECUTE 'ALTER TABLE "amounts" ALTER COLUMN "amount" SET DATA TYPE integer USING "id"';
END
$f$;"#,
                r#"SELECT "retype_amounts"();"#,
            ],
        ),
        Migration::new(
            through_procedure,
            &[
                r#"CREATE PROCEDURE "scale_amounts"() LANGUAGE plpgsql AS $p$
BEGIN
  EXECUTE 'ALTER TABLE "amounts" ALTER COLUMN "amount" SET DATA TYPE bigint USING "amount" * 10';
END
$p$;"#,
                r#"CALL "scale_amounts"();"#,
            ],
        ),
    ]);
    let rows = row_entries(&r.db, last);
    let row =
        |id: i64, amount: Value, note: &str| json!({ "id": id, "amount": amount, "note": note });
    let notes = [(1, "five!"), (2, "zero"), (3, "none")];
    assert_row_entries(
        &of_migration(&rows, through_do),
        &[
            migration_update(
                through_do,
                "amounts",
                id_key(1),
                row(1, json!(5), "five!"),
                row(1, json!(0), "five!"),
                json!(["amount"]),
            ),
            migration_update(
                through_do,
                "amounts",
                id_key(3),
                row(3, Value::Null, "none"),
                row(3, json!(0), "none"),
                json!(["amount"]),
            ),
        ],
        "EXECUTE in a DO block",
    );
    let wants: Vec<Change<'_>> = notes
        .iter()
        .map(|(id, note)| {
            migration_update(
                through_function,
                "amounts",
                id_key(*id),
                row(*id, json!(0), note),
                row(*id, json!(id), note),
                json!(["amount"]),
            )
        })
        .collect();
    assert_row_entries(
        &of_migration(&rows, through_function),
        &wants,
        "a function the statement calls",
    );
    let wants: Vec<Change<'_>> = notes
        .iter()
        .map(|(id, note)| {
            migration_update(
                through_procedure,
                "amounts",
                id_key(*id),
                row(*id, json!(id), note),
                row(*id, json!(id * 10), note),
                json!(["amount"]),
            )
        })
        .collect();
    assert_row_entries(
        &of_migration(&rows, through_procedure),
        &wants,
        "a procedure the statement calls",
    );
    assert_eq!(rows.len(), 8, "and no other entry: {}", lines(&rows));
}

// ---------------------------------------------------------------------------
// What is recorded.
// ---------------------------------------------------------------------------

/// CONTRACT.md: "Which columns are compared: those the table has both
/// before and after the statement. A column the statement renamed is the
/// same column. A column it added is in `new` and not in `changed`; one it
/// dropped is in `old` and not in `changed`"; `changed` "by their names
/// after the statement, in the table's column order". One `DO` block
/// renames `v` to `v2`, adds `added`, drops `c`, and changes `i` and `v2`.
#[test]
fn columns_compared_are_those_before_and_after_and_a_renamed_one_by_its_new_name() {
    let mut r = Rewrite::start();
    let last = last_history_id(&r.db);
    let migration = "0004_reshape_wide";
    r.send_ok(&[Migration::new(
        migration,
        &[r#"DO $d$
BEGIN
  ALTER TABLE "wide" RENAME COLUMN "v" TO "v2";
  ALTER TABLE "wide" ADD COLUMN "added" text DEFAULT 'new';
  ALTER TABLE "wide" DROP COLUMN "c";
  ALTER TABLE "wide" ALTER COLUMN "i" SET DATA TYPE bigint USING "i" + 1, ALTER COLUMN "v2" SET DATA TYPE text USING upper("v2");
END
$d$;"#],
    )]);
    assert_row_entries(
        &row_entries(&r.db, last),
        &[migration_update(
            migration,
            "wide",
            id_key(1),
            json!({ "id": 1, "i": 12, "v": "vee", "n": 1.5, "c": "c1" }),
            json!({ "id": 1, "i": 13, "v2": "VEE", "n": 1.5, "added": "new" }),
            json!(["i", "v2"]),
        )],
        "row 1: i and v2 (renamed from v) differ, `added` only in new, `c` only in old; row 2 (all null) differs in no compared column: no entry",
    );
}

/// CONTRACT.md: "When a value differs: when its JSON form in `old` and in
/// `new` are different JSON values. ... a number and a string are
/// different (`5` and `\"5\"`), and so are a time without and with its
/// offset. For a `json` or `jsonb` column, JSON `null` and SQL null are
/// different". Four statements on `vals`: `s` text to integer (`'5'` to
/// `5`), `ts` to `timestamptz` with `USING` (so written anew), `jb` and
/// `js` with JSON `null` and SQL null swapped.
#[test]
fn a_value_differs_when_its_json_form_differs() {
    let mut r = Rewrite::start();
    let last = last_history_id(&r.db);
    let migration = "0004_vals";
    r.send_ok(&[Migration::new(
        migration,
        &[
            r#"ALTER TABLE "vals" ALTER COLUMN "s" SET DATA TYPE integer USING "s"::integer;"#,
            r#"ALTER TABLE "vals" ALTER COLUMN "ts" SET DATA TYPE timestamptz USING "ts" AT TIME ZONE 'UTC';"#,
            r#"ALTER TABLE "vals" ALTER COLUMN "jb" SET DATA TYPE jsonb USING CASE WHEN "jb" IS NULL THEN 'null'::jsonb WHEN "jb" = 'null'::jsonb THEN NULL ELSE "jb" END;"#,
            r#"ALTER TABLE "vals" ALTER COLUMN "js" SET DATA TYPE json USING CASE WHEN "js" IS NULL THEN 'null'::json WHEN "js"::text = 'null' THEN NULL ELSE "js" END;"#,
        ],
    )]);
    let rows = row_entries(&r.db, last);
    let by = |column: &str| -> Vec<Value> {
        rows.iter()
            .filter(|e| e["changed"] == json!([column]))
            .cloned()
            .collect()
    };
    let keys = |entries: &[Value]| -> Vec<Value> {
        let mut k: Vec<Value> = entries.iter().map(|e| e["key"].clone()).collect();
        k.sort_by_key(Value::to_string);
        k
    };
    assert_eq!(
        rows.len(),
        7,
        "expected 7 entries (s of rows 1 and 3, ts of row 1, jb and js of rows 2 and 3), got:\n{}",
        lines(&rows)
    );
    for e in &rows {
        assert_eq!(e["kind"], "update", "{e}");
        assert_eq!(e["actor"], "migration", "{e}");
        assert_eq!(e["migration"], migration, "{e}");
        assert_eq!(e["user_id"], INSTALLER, "{e}");
        assert_eq!(e["request"], Value::Null, "{e}");
    }
    let s = by("s");
    assert_eq!(
        keys(&s),
        vec![id_key(1), id_key(3)],
        "'5' to 5 and '7' to 7 differ; null to null does not: {}",
        lines(&rows)
    );
    for e in &s {
        assert!(
            e["old"]["s"].is_string() && e["new"]["s"].is_number(),
            "a string before, a number after: {e}"
        );
    }
    let ts = by("ts");
    assert_eq!(
        keys(&ts),
        vec![id_key(1)],
        "a time without and with its offset differ: {}",
        lines(&rows)
    );
    let (old_ts, new_ts) = (&ts[0]["old"]["ts"], &ts[0]["new"]["ts"]);
    assert!(
        old_ts.as_str().is_some_and(|t| !t.contains('+'))
            && new_ts.as_str().is_some_and(|t| t.ends_with("+00:00")),
        "the time without its offset before and with it after: {}",
        ts[0]
    );
    for column in ["jb", "js"] {
        assert_eq!(
            keys(&by(column)),
            vec![id_key(2), id_key(3)],
            "{column}: JSON null to SQL null (row 2) and SQL null to JSON null (row 3) differ; an object to itself (row 1) does not: {}",
            lines(&rows)
        );
    }
}

/// CONTRACT.md: "The same row before and after is the one with the same
/// primary key: each key column equal as JSON values, or one a number or a
/// boolean and the other the string that reads the same (so a key retyped
/// from `integer` to `text` still pairs). A row before with no row after
/// under its key is an entry of `kind` `delete` with its `old`; a row after
/// with none before is one of `kind` `insert` with its `new` ... A
/// statement that gives every row a new key is so recorded as each row
/// deleted and each inserted."
#[test]
fn the_same_row_is_the_one_with_the_same_key_also_retyped_and_new_keys_are_deletes_and_inserts() {
    let mut r = Rewrite::start();
    let last = last_history_id(&r.db);
    let migration = "0004_keys";
    r.send_ok(&[Migration::new(
        migration,
        &[
            r#"ALTER TABLE "keyed" ALTER COLUMN "id" SET DATA TYPE bigint, ALTER COLUMN "v" SET DATA TYPE text USING "v" || '!';"#,
            r#"ALTER TABLE "keyed" ALTER COLUMN "id" SET DATA TYPE text USING "id"::text;"#,
            r#"ALTER TABLE "keyed" ALTER COLUMN "id" SET DATA TYPE text USING 'k' || "id";"#,
        ],
    )]);
    let by = || Change::by_migration(INSTALLER, migration).on("public", "keyed");
    let mut wants = Vec::new();
    for (id, v) in [(1, "a"), (2, "b")] {
        let text = id.to_string();
        let k = format!("k{id}");
        let bang = format!("{v}!");
        wants.push(by().update(
            id_key(id),
            json!({ "id": id, "v": v }),
            json!({ "id": id, "v": bang }),
            json!(["v"]),
        ));
        wants.push(by().update(
            json!({ "id": text }),
            json!({ "id": id, "v": bang }),
            json!({ "id": text, "v": bang }),
            json!(["id"]),
        ));
        wants.push(by().delete(json!({ "id": text }), json!({ "id": text, "v": bang })));
        wants.push(by().insert(json!({ "id": k }), json!({ "id": k, "v": bang })));
    }
    assert_row_entries(
        &row_entries(&r.db, last),
        &wants,
        "integer to bigint pairs (v changed); integer to text pairs (1 and \"1\" are different values: id changed); new keys: each row deleted and inserted",
    );
}

/// CONTRACT.md: "Their `id`s are greater than those of the rows the same
/// migration changed before the statement, and less than those of the
/// migration's `structure` entries." And: "The row before is the row as it
/// was before the statement" (row 2's note, changed by the migration just
/// before).
#[test]
fn entries_come_after_the_rows_changed_before_the_statement_and_before_the_structure_entries() {
    let mut r = Rewrite::start();
    let last = last_history_id(&r.db);
    let migration = "0004_ordered";
    r.send_ok(&[Migration::new(
        migration,
        &[
            r#"UPDATE "amounts" SET "note" = 'before' WHERE "id" = 2;"#,
            r#"UPDATE "other" SET "v" = 9 WHERE "id" = 1;"#,
            r#"ALTER TABLE "amounts" ALTER COLUMN "amount" SET DATA TYPE bigint USING coalesce("amount", 0) + 1;"#,
            r#"ALTER TABLE "other" DROP COLUMN "v";"#,
        ],
    )]);
    let rows = row_entries(&r.db, last);
    let before: Vec<Value> = rows
        .iter()
        .filter(|e| e["changed"] != json!(["amount"]))
        .cloned()
        .collect();
    let rewrite: Vec<Value> = rows
        .iter()
        .filter(|e| e["changed"] == json!(["amount"]))
        .cloned()
        .collect();
    assert_eq!(
        before.len(),
        2,
        "the harness's own check: the two updates before the statement: {}",
        lines(&rows)
    );
    assert_row_entries(
        &rewrite,
        &[
            migration_update(
                migration,
                "amounts",
                id_key(1),
                json!({ "id": 1, "amount": 5, "note": "five!" }),
                json!({ "id": 1, "amount": 6, "note": "five!" }),
                json!(["amount"]),
            ),
            migration_update(
                migration,
                "amounts",
                id_key(2),
                json!({ "id": 2, "amount": 0, "note": "before" }),
                json!({ "id": 2, "amount": 1, "note": "before" }),
                json!(["amount"]),
            ),
            migration_update(
                migration,
                "amounts",
                id_key(3),
                json!({ "id": 3, "amount": null, "note": "none" }),
                json!({ "id": 3, "amount": 1, "note": "none" }),
                json!(["amount"]),
            ),
        ],
        "every row's amount changed",
    );
    let structure = structure_after(&r.db, last);
    assert_eq!(
        structure.len(),
        2,
        "the harness's own check: column-type of amounts, column-dropped of other: {}",
        lines(&structure)
    );
    let low = before.iter().filter_map(|e| e["id"].as_i64()).max();
    assert_between(
        &rewrite,
        low,
        &structure,
        "after the migration's updates before the statement, before its structure entries",
    );
}

/// CONTRACT.md: "The entries are in the migration's transaction: a Version
/// that is refused or fails leaves none."
#[test]
fn a_version_refused_or_failed_leaves_no_entry_of_its_rewrite() {
    let mut r = Rewrite::start();
    let last = last_history_id(&r.db);
    let amounts = r.rows_of("amounts");
    let rewrite = r#"ALTER TABLE "amounts" ALTER COLUMN "amount" SET DATA TYPE numeric USING 0;"#;
    let refused = r.send(&[Migration::new(
        "0004_refused",
        &[
            rewrite,
            r#"ALTER TABLE "other" ADD CONSTRAINT "other_v_unique" UNIQUE("v") DEFERRABLE;"#,
        ],
    )]);
    blackbox::assert_api_error(&refused, 422, "migration-refused");
    let failed = r.send(&[Migration::new("0004_fails", &[rewrite, r#"SELECT 1 / 0;"#])]);
    blackbox::assert_api_error(&failed, 422, "migration-failed");
    assert_eq!(
        r.rows_of("amounts"),
        amounts,
        "the harness's own check: nothing kept"
    );
    assert_nothing_after(&r, last, "a refused Version, then a failed one");
}

/// CONTRACT.md: "They are read as any change is: `history` with the row's
/// `key` answers them among that row's changes." Row 1 has the rewrite's
/// entry between the column-type entry and its earlier changes; row 2,
/// whose amount was 0, has none.
#[test]
fn a_rewrites_entry_is_read_through_history_with_the_rows_key() {
    let mut r = Rewrite::start();
    let migration = "0004_zero_amounts";
    r.send_ok(&[Migration::new(
        migration,
        &[r#"ALTER TABLE "amounts" ALTER COLUMN "amount" SET DATA TYPE numeric USING 0;"#],
    )]);
    for actor in ACTORS {
        let context = format!("history of amounts {{ id: 1 }} as the {actor}");
        let answer = r.read(actor, "amounts", &json!({ "key": { "id": 1 } }));
        let changes = changes_of(&answer);
        let kinds: Vec<&Value> = changes.iter().map(|c| &c["kind"]).collect();
        assert_eq!(
            kinds,
            vec!["structure", "update", "update", "insert"],
            "{context}: the column-type entry, the rewrite's update, the seed's update and insert, newest first; got\n{}",
            lines(&changes)
        );
        let ids = answered_ids(&changes);
        assert!(
            ids.windows(2).all(|w| w[0] > w[1]),
            "{context}: newest first: {ids:?}"
        );
        let mut got = changes[1].clone();
        let o = got.as_object_mut().unwrap();
        o.remove("id");
        o.remove("at");
        assert_eq!(
            got,
            json!({
                "kind": "update", "key": { "id": 1 },
                "old": [1, "5", "five!"], "new": [1, "0", "five!"], "changed": ["amount"],
                "user_id": INSTALLER, "application": "board", "actor": "migration",
                "request": null, "migration": migration, "structure": null,
            }),
            "{context}: the rewrite's entry"
        );
        let context = format!("history of amounts {{ id: 2 }} as the {actor}");
        let changes = changes_of(&r.read(actor, "amounts", &json!({ "key": { "id": 2 } })));
        let kinds: Vec<&Value> = changes.iter().map(|c| &c["kind"]).collect();
        assert_eq!(
            kinds,
            vec!["structure", "insert"],
            "{context}: amount 0 made 0, no entry of the rewrite; got\n{}",
            lines(&changes)
        );
    }
}

/// CONTRACT.md: "A statement that both changes rows with `UPDATE` and
/// rewrites the table (a `DO` block) has its `UPDATE`s recorded by the
/// capture as always, and the rewrite's entry on top, with `old` the row
/// before the statement."
#[test]
fn a_statement_that_updates_and_rewrites_has_both_entries_the_rewrites_old_before_the_statement() {
    let mut r = Rewrite::start();
    let last = last_history_id(&r.db);
    let migration = "0004_update_and_rewrite";
    r.send_ok(&[Migration::new(
        migration,
        &[r#"DO $d$
BEGIN
  UPDATE "amounts" SET "amount" = "amount" + 1 WHERE "id" = 1;
  ALTER TABLE "amounts" ALTER COLUMN "amount" SET DATA TYPE bigint USING "amount" * 10;
END
$d$;"#],
    )]);
    let rows = row_entries(&r.db, last);
    assert_eq!(
        rows.len(),
        2,
        "row 1: the capture's update and the rewrite's entry; rows 2 (0) and 3 (null) unchanged; got\n{}",
        lines(&rows)
    );
    let find = |amount: i64| -> &Value {
        rows.iter()
            .find(|e| e["new"]["amount"] == json!(amount))
            .unwrap_or_else(|| {
                panic!(
                    "expected an entry whose new amount is {amount}, got\n{}",
                    lines(&rows)
                )
            })
    };
    let old = json!({ "id": 1, "amount": 5, "note": "five!" });
    assert_change(
        find(6),
        &migration_update(
            migration,
            "amounts",
            id_key(1),
            old.clone(),
            json!({ "id": 1, "amount": 6, "note": "five!" }),
            json!(["amount"]),
        ),
        "the UPDATE, recorded by the capture",
    );
    assert_change(
        find(60),
        &migration_update(
            migration,
            "amounts",
            id_key(1),
            old,
            json!({ "id": 1, "amount": 60, "note": "five!" }),
            json!(["amount"]),
        ),
        "the rewrite's entry, `old` the row before the statement",
    );
}

// ---------------------------------------------------------------------------
// When.
// ---------------------------------------------------------------------------

/// CONTRACT.md: "It is recorded for a table that had history when the
/// migration began (it was made by an earlier migration, of this Version
/// or of an earlier one) ... A table the same migration made has no such
/// entries".
#[test]
fn a_table_the_same_migration_made_has_none_one_an_earlier_migration_of_the_version_made_has_them()
{
    let mut r = Rewrite::start();
    let last = last_history_id(&r.db);
    let later = "0005_rewrite_fresh";
    r.send_ok(&[
        Migration::new(
            "0004_make_fresh",
            &[
                r#"CREATE TABLE "fresh" (
	"id" integer PRIMARY KEY NOT NULL,
	"v" integer
);"#,
                r#"INSERT INTO "fresh" VALUES (1, 1), (2, 2);"#,
                r#"ALTER TABLE "fresh" ALTER COLUMN "v" SET DATA TYPE bigint USING "v" + 1;"#,
            ],
        ),
        Migration::new(
            later,
            &[r#"ALTER TABLE "fresh" ALTER COLUMN "v" SET DATA TYPE integer USING "v" * 10;"#],
        ),
    ]);
    assert_row_entries(
        &row_entries(&r.db, last),
        &[
            migration_update(
                later,
                "fresh",
                id_key(1),
                json!({ "id": 1, "v": 2 }),
                json!({ "id": 1, "v": 20 }),
                json!(["v"]),
            ),
            migration_update(
                later,
                "fresh",
                id_key(2),
                json!({ "id": 2, "v": 3 }),
                json!({ "id": 2, "v": 30 }),
                json!(["v"]),
            ),
        ],
        "nothing of what 0004, which made fresh, did; 0005's rewrite recorded",
    );
}

/// CONTRACT.md: "A partitioned table's rows are in its partitions: when a
/// statement writes a partition anew, the rows are recorded under the name
/// of the partitioned table at the top, where the Application reads them."
/// `parted` has the partition `parted_2026` and `parted_2027`, itself
/// partitioned, with `parted_2027_low`.
#[test]
fn a_partitions_rows_are_recorded_under_the_partitioned_table_at_the_top() {
    let mut r = Rewrite::start();
    let last = last_history_id(&r.db);
    let leaves = ["parted_2026", "parted_2027_low"];
    let before: Vec<Option<String>> = leaves.iter().map(|t| r.filenode(t)).collect();
    let migration = "0004_parted";
    r.send_ok(&[Migration::new(
        migration,
        &[r#"ALTER TABLE "parted" ALTER COLUMN "v" SET DATA TYPE bigint USING "v" + 1;"#],
    )]);
    for (leaf, b) in leaves.iter().zip(&before) {
        let a = r.filenode(leaf);
        assert!(
            b.is_some() && *b != a,
            "the harness's own check: Postgres wrote {leaf} anew ({b:?} then {a:?})"
        );
    }
    let key = |id: i64, day: &str| json!({ "id": id, "day": day });
    assert_row_entries(
        &row_entries(&r.db, last),
        &[
            migration_update(
                migration,
                "parted",
                key(1, "2026-03-01"),
                json!({ "id": 1, "day": "2026-03-01", "v": 5 }),
                json!({ "id": 1, "day": "2026-03-01", "v": 6 }),
                json!(["v"]),
            ),
            migration_update(
                migration,
                "parted",
                key(2, "2027-03-01"),
                json!({ "id": 2, "day": "2027-03-01", "v": 7 }),
                json!({ "id": 2, "day": "2027-03-01", "v": 8 }),
                json!(["v"]),
            ),
        ],
        "each row under parted, the table at the top, none under a partition",
    );
}

/// CONTRACT.md: "A table that inherits from another (`INHERITS`) is a
/// table of its own." `child_t` inherits `base_t`; each has a row keyed 1.
/// A type changed on `base_t` is changed on `child_t` too, and Postgres
/// writes both anew: each row is recorded under its own table, and
/// `base_t`'s entries hold only `base_t`'s own row.
#[test]
fn an_inheriting_table_is_a_table_of_its_own() {
    let mut r = Rewrite::start();
    let last = last_history_id(&r.db);
    let before = (r.filenode("base_t"), r.filenode("child_t"));
    let migration = "0004_inherited";
    r.send_ok(&[Migration::new(
        migration,
        &[r#"ALTER TABLE "base_t" ALTER COLUMN "v" SET DATA TYPE bigint USING "v" * 10;"#],
    )]);
    let after = (r.filenode("base_t"), r.filenode("child_t"));
    assert!(
        before.0 != after.0 && before.1 != after.1,
        "the harness's own check: Postgres wrote both anew: {before:?} then {after:?}"
    );
    assert_row_entries(
        &row_entries(&r.db, last),
        &[
            migration_update(
                migration,
                "base_t",
                id_key(1),
                json!({ "id": 1, "v": 1 }),
                json!({ "id": 1, "v": 10 }),
                json!(["v"]),
            ),
            migration_update(
                migration,
                "child_t",
                id_key(1),
                json!({ "id": 1, "v": 2, "extra": "x" }),
                json!({ "id": 1, "v": 20, "extra": "x" }),
                json!(["v"]),
            ),
        ],
        "base_t's own row under base_t, child_t's row under child_t",
    );
}

// ---------------------------------------------------------------------------
// A table without a primary key.
// ---------------------------------------------------------------------------

/// CONTRACT.md: "When a rewrite leaves such a table with other rows than it
/// had ..., the Version is refused: `migration-refused`, rule
/// `rewrite-without-key`, the message names the table and says to give it
/// a primary key in an earlier statement. When the rows are the same,
/// nothing is recorded and the Version applies." And "each row as many
/// times as it is there": `keyless` holds `('d', 1)` twice and `('e', 1)`
/// once; a rewrite that makes one `d` an `e` keeps the same distinct rows
/// and is refused all the same.
#[test]
fn a_table_without_a_key_rewritten_with_other_rows_is_refused_and_with_the_same_applies() {
    let mut r = Rewrite::start();
    let last = last_history_id(&r.db);
    let keyless = r.rows_of("keyless");
    let differ = "0004_keyless_differ";
    let sent = r.send(&[Migration::new(
        differ,
        &[r#"ALTER TABLE "keyless" ALTER COLUMN "b" SET DATA TYPE bigint USING "b" + 1;"#],
    )]);
    assert_refused(
        &sent,
        "rewrite-without-key",
        differ,
        &["keyless", "primary key"],
        "a table without a key whose values changed",
    );
    let counted = "0004_keyless_counted";
    let sent = r.send(&[Migration::new(
        counted,
        &[r#"ALTER TABLE "keyless" ALTER COLUMN "a" SET DATA TYPE text USING CASE WHEN "a" = 'd' AND set_config('rewrite.seen_d', (coalesce(nullif(current_setting('rewrite.seen_d', true), ''), '0')::integer + 1)::text, true) = '1' THEN 'e' ELSE "a" END;"#],
    )]);
    assert_refused(
        &sent,
        "rewrite-without-key",
        counted,
        &["keyless", "primary key"],
        "d, d, e made d, e, e: the same distinct rows, not as many times each",
    );
    assert_eq!(r.rows_of("keyless"), keyless, "nothing kept");
    assert_nothing_after(&r, last, "two refused Versions");
    let before = r.filenode("keyless");
    r.send_ok(&[Migration::new(
        "0004_keyless_same",
        &[r#"ALTER TABLE "keyless" ALTER COLUMN "b" SET DATA TYPE bigint;"#],
    )]);
    assert_ne!(
        before,
        r.filenode("keyless"),
        "the harness's own check: Postgres wrote keyless anew"
    );
    let rows = row_entries(&r.db, last);
    assert!(
        rows.is_empty(),
        "the same rows after: nothing recorded; got\n{}",
        lines(&rows)
    );
}

/// CONTRACT.md: "A table whose primary key is not the same columns before
/// and after the statement is treated so too." One `DO` block moves
/// `pk_moved`'s key from `id` to `code` and rewrites it: with `n` changed,
/// refused; with nothing changed, it applies and nothing is recorded.
#[test]
fn a_primary_key_not_the_same_columns_before_and_after_is_treated_as_no_key() {
    let mut r = Rewrite::start();
    let last = last_history_id(&r.db);
    let moved = |using: &str| {
        format!(
            r#"DO $d$
BEGIN
  ALTER TABLE "pk_moved" DROP CONSTRAINT "pk_moved_pkey";
  ALTER TABLE "pk_moved" ADD CONSTRAINT "pk_moved_code_pk" PRIMARY KEY ("code");
  ALTER TABLE "pk_moved" ALTER COLUMN "n" SET DATA TYPE bigint USING {using};
END
$d$;"#
        )
    };
    let differ = "0004_key_moved_differ";
    let statement = moved(r#""n" + 1"#);
    let sent = r.send(&[Migration::new(differ, &[statement.as_str()])]);
    assert_refused(
        &sent,
        "rewrite-without-key",
        differ,
        &["pk_moved", "primary key"],
        "the key moved from id to code in the statement, n changed",
    );
    assert_nothing_after(&r, last, "the refused Version");
    let statement = moved(r#""n""#);
    r.send_ok(&[Migration::new("0004_key_moved_same", &[statement.as_str()])]);
    let rows = row_entries(&r.db, last);
    assert!(
        rows.is_empty(),
        "the key moved, the rows the same: nothing recorded; got\n{}",
        lines(&rows)
    );
}

// ---------------------------------------------------------------------------
// The reading cannot be made to lie.
// ---------------------------------------------------------------------------

/// CONTRACT.md: "When row security would apply to the migration role's
/// reading of the table, before or after the statement (`FORCE ROW LEVEL
/// SECURITY`, with a policy or without), the Version is refused:
/// `migration-refused`, rule `rewrite-row-security`, the message names the
/// table and says to take `FORCE ROW LEVEL SECURITY` off before the
/// statement and put it back after." Forced by an earlier Version with a
/// policy that hides row 2: refused; then done as the message says (off
/// before, back after): applies, both rows recorded.
#[test]
fn forced_row_security_with_a_policy_that_hides_rows_is_refused_and_off_around_the_statement_is_not()
 {
    let mut r = Rewrite::start();
    r.send_ok(&[Migration::new(
        "0004_force",
        &[
            r#"ALTER TABLE "secured" ENABLE ROW LEVEL SECURITY;"#,
            r#"ALTER TABLE "secured" FORCE ROW LEVEL SECURITY;"#,
            r#"CREATE POLICY "only_one" ON "secured" USING ("id" = 1);"#,
        ],
    )]);
    let last = last_history_id(&r.db);
    let rows = r.rows_of("secured");
    let rewrite = r#"ALTER TABLE "secured" ALTER COLUMN "v" SET DATA TYPE bigint USING "v" + 1;"#;
    let refused = "0005_rewrite_secured";
    let sent = r.send(&[Migration::new(refused, &[rewrite])]);
    assert_refused(
        &sent,
        "rewrite-row-security",
        refused,
        &["secured", "force row level security"],
        "forced row security with a policy that hides a row, from an earlier Version",
    );
    assert_eq!(r.rows_of("secured"), rows, "nothing kept");
    assert_nothing_after(&r, last, "the refused Version");
    let migration = "0005_rewrite_secured_unforced";
    r.send_ok(&[Migration::new(
        migration,
        &[
            r#"ALTER TABLE "secured" NO FORCE ROW LEVEL SECURITY;"#,
            rewrite,
            r#"ALTER TABLE "secured" FORCE ROW LEVEL SECURITY;"#,
        ],
    )]);
    assert_row_entries(
        &row_entries(&r.db, last),
        &[
            migration_update(
                migration,
                "secured",
                id_key(1),
                json!({ "id": 1, "v": 1 }),
                json!({ "id": 1, "v": 2 }),
                json!(["v"]),
            ),
            migration_update(
                migration,
                "secured",
                id_key(2),
                json!({ "id": 2, "v": 2 }),
                json!({ "id": 2, "v": 3 }),
                json!(["v"]),
            ),
        ],
        "FORCE off before the statement and back after: both rows, the one the policy hides included",
    );
}

/// CONTRACT.md, `rewrite-row-security`: forced without a policy, in the
/// same migration before the statement; and forced after the rewrite, by
/// the same `DO` block.
#[test]
fn forced_row_security_without_a_policy_or_forced_after_the_rewrite_is_refused() {
    let mut r = Rewrite::start();
    let last = last_history_id(&r.db);
    let rows = r.rows_of("secured");
    let no_policy = "0004_forced_no_policy";
    let sent = r.send(&[Migration::new(
        no_policy,
        &[
            r#"ALTER TABLE "secured" ENABLE ROW LEVEL SECURITY;"#,
            r#"ALTER TABLE "secured" FORCE ROW LEVEL SECURITY;"#,
            r#"ALTER TABLE "secured" ALTER COLUMN "v" SET DATA TYPE bigint USING "v" + 1;"#,
        ],
    )]);
    assert_refused(
        &sent,
        "rewrite-row-security",
        no_policy,
        &["secured", "force row level security"],
        "forced, no policy (the owner reads no row), before the statement",
    );
    let after = "0004_forced_after";
    let sent = r.send(&[Migration::new(
        after,
        &[r#"DO $d$
BEGIN
  ALTER TABLE "secured" ALTER COLUMN "v" SET DATA TYPE bigint USING "v" + 1;
  ALTER TABLE "secured" ENABLE ROW LEVEL SECURITY;
  ALTER TABLE "secured" FORCE ROW LEVEL SECURITY;
END
$d$;"#],
    )]);
    assert_refused(
        &sent,
        "rewrite-row-security",
        after,
        &["secured", "force row level security"],
        "forced after the rewrite, in the same statement",
    );
    assert_eq!(r.rows_of("secured"), rows, "nothing kept");
    assert_nothing_after(&r, last, "two refused Versions");
}

/// CONTRACT.md: "Row security that is enabled and not forced does not
/// apply to the table's owner, and is not refused." With a policy that
/// would hide row 2 from anyone it applied to, both rows are recorded.
#[test]
fn row_security_enabled_and_not_forced_is_not_refused_and_records_every_row() {
    let mut r = Rewrite::start();
    let last = last_history_id(&r.db);
    let migration = "0004_enabled_not_forced";
    r.send_ok(&[Migration::new(
        migration,
        &[
            r#"ALTER TABLE "secured" ENABLE ROW LEVEL SECURITY;"#,
            r#"CREATE POLICY "only_one" ON "secured" USING ("id" = 1);"#,
            r#"ALTER TABLE "secured" ALTER COLUMN "v" SET DATA TYPE bigint USING "v" + 1;"#,
        ],
    )]);
    assert_row_entries(
        &row_entries(&r.db, last),
        &[
            migration_update(
                migration,
                "secured",
                id_key(1),
                json!({ "id": 1, "v": 1 }),
                json!({ "id": 1, "v": 2 }),
                json!(["v"]),
            ),
            migration_update(
                migration,
                "secured",
                id_key(2),
                json!({ "id": 2, "v": 2 }),
                json!({ "id": 2, "v": 3 }),
                json!(["v"]),
            ),
        ],
        "enabled, not forced: both rows",
    );
}

/// CONTRACT.md: "A cast to `json` or `jsonb` from one of the Application's
/// types whose function is not Postgres's own, when it exists at a
/// rewrite's reading, is `migration-refused` with rule `json-cast`, also
/// when the migration drops it again before its end." The cast's function
/// lies (every `mood` reads `\"sad\"`); `moody`, with a `mood` column, is
/// rewritten while it exists. Once to `json`, once to `jsonb`.
#[test]
fn a_cast_to_json_that_exists_at_a_rewrites_reading_is_refused_also_when_dropped_before_the_end() {
    let mut r = Rewrite::start();
    let last = last_history_id(&r.db);
    let rows = r.rows_of("moody");
    for target in ["json", "jsonb"] {
        let migration = format!("0004_cast_to_{target}");
        let function = format!(
            r#"CREATE FUNCTION "mood_lie"("m" "mood") RETURNS {target} LANGUAGE sql AS $f$ SELECT '"sad"'::{target} $f$;"#
        );
        let cast = format!(r#"CREATE CAST ("mood" AS {target}) WITH FUNCTION "mood_lie"("mood");"#);
        let drop_cast = format!(r#"DROP CAST ("mood" AS {target});"#);
        let sent = r.send(&[Migration::new(
            &migration,
            &[
                function.as_str(),
                cast.as_str(),
                r#"ALTER TABLE "moody" ALTER COLUMN "n" SET DATA TYPE bigint USING "n" + 1;"#,
                drop_cast.as_str(),
                r#"DROP FUNCTION "mood_lie"("mood");"#,
            ],
        )]);
        assert_refused(
            &sent,
            "json-cast",
            &migration,
            &[],
            &format!(
                "a cast to {target} that exists at the rewrite's reading, dropped before the end"
            ),
        );
    }
    assert_eq!(r.rows_of("moody"), rows, "nothing kept");
    assert_nothing_after(&r, last, "two refused Versions");
}

/// CONTRACT.md: "A virtual generated column (Postgres 18) is null in `old`
/// and in `new`, as in the capture, and never in `changed`; the stored
/// columns of such a table are recorded as of any other." One `DO` block
/// changes the virtual column's expression (its value differs when read)
/// and rewrites `s`; then the expression is changed again and the table set
/// unlogged (written anew, no stored value changed). Skipped before
/// Postgres 18.
#[test]
fn a_virtual_generated_column_is_null_in_old_and_new_and_never_in_changed() {
    let major = blackbox::out_of_reach::server_version_num();
    if major < 180_000 {
        eprintln!(
            "SKIPPED: Postgres {major} has no virtual generated columns (CONTRACT.md: Postgres 18)"
        );
        return;
    }
    let mut r = Rewrite::start();
    r.send_ok(&[Migration::new(
        "0004_virtual",
        &[r#"CREATE TABLE "vgen" (
	"id" integer PRIMARY KEY NOT NULL,
	"a" integer NOT NULL,
	"b" integer GENERATED ALWAYS AS ("a" * 2) VIRTUAL,
	"s" integer
);"#],
    )]);
    let (_, answer) = r.own.one(
        &User::new("alice"),
        "board",
        &own_execute(
            "user",
            "insert into vgen (id, a, s) values (1, 1, 1), (2, 2, 2)",
        ),
    );
    assert_ok(&answer, "the harness's own check: rows of vgen");
    let last = last_history_id(&r.db);
    let migration = "0005_rewrite_vgen";
    r.send_ok(&[Migration::new(
        migration,
        &[
            r#"DO $d$
BEGIN
  ALTER TABLE "vgen" ALTER COLUMN "b" SET EXPRESSION AS ("a" * 3);
  ALTER TABLE "vgen" ALTER COLUMN "s" SET DATA TYPE bigint USING "s" + 1;
END
$d$;"#,
            r#"ALTER TABLE "vgen" ALTER COLUMN "b" SET EXPRESSION AS ("a" * 4);"#,
            r#"ALTER TABLE "vgen" SET UNLOGGED;"#,
        ],
    )]);
    assert_row_entries(
        &row_entries(&r.db, last),
        &[
            migration_update(
                migration,
                "vgen",
                id_key(1),
                json!({ "id": 1, "a": 1, "b": null, "s": 1 }),
                json!({ "id": 1, "a": 1, "b": null, "s": 2 }),
                json!(["s"]),
            ),
            migration_update(
                migration,
                "vgen",
                id_key(2),
                json!({ "id": 2, "a": 2, "b": null, "s": 2 }),
                json!({ "id": 2, "a": 2, "b": null, "s": 3 }),
                json!(["s"]),
            ),
        ],
        "s recorded as any column; b null in old and new and never in changed",
    );
}

/// The texts the rewrite's forger feeds to text arguments, by turns.
const FORGED_TEXTS: [&str; 12] = [
    "public",
    "amounts",
    "other",
    "update",
    "migration",
    "mallory-forged",
    "0099_forged",
    "0004_forging_rewrite",
    r#"{"id": 2}"#,
    r#"{"id": 2, "amount": 777, "note": "forged"}"#,
    r#"[{"id": 2, "amount": 777, "note": "forged"}]"#,
    r#"{"kind": "update", "schema_name": "public", "table_name": "amounts", "key": {"id": 2}, "old": {"id": 2, "amount": 0, "note": "zero"}, "new": {"id": 2, "amount": 777, "note": "forged"}, "changed": ["amount"], "user_id": "mallory-forged", "actor": "migration", "migration": "0004_forging_rewrite"}"#,
];

/// The documents it feeds to `jsonb` and `json` arguments, by turns.
const FORGED_DOCS: [&str; 6] = [
    r#"{"id": 2}"#,
    r#"{"id": 2, "amount": 777, "note": "forged"}"#,
    r#"[{"id": 1, "amount": 5, "note": "five!"}, {"id": 2, "amount": 0, "note": "zero"}]"#,
    r#"[{"id": 1, "amount": 999, "note": "forged"}]"#,
    r#"{"kind": "update", "schema_name": "public", "table_name": "amounts", "key": {"id": 2}, "old": {"id": 2, "amount": 0, "note": "zero"}, "new": {"id": 2, "amount": 777, "note": "forged"}, "changed": ["amount"], "user_id": "mallory-forged", "actor": "migration", "migration": "0004_forging_rewrite"}"#,
    r#"{"kind": "delete", "schema_name": "public", "table_name": "other", "key": {"id": 1}, "old": {"id": 1, "v": 1}}"#,
];

fn sql_array(items: &[&str]) -> String {
    format!(
        "ARRAY[{}]::text[]",
        items
            .iter()
            .map(|t| literal(t))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// `0003z_forger`: `forge_rewrite()`, which finds every function and
/// procedure of `vm` in the catalog when it runs and calls each with the
/// forger's arguments by turns (texts, documents, the oids of `amounts` and
/// `other`, made-up keys, all nulls), each call in a subtransaction of its
/// own; returns how many calls Postgres let through.
fn forger() -> Migration {
    let body = format!(
        r#"CREATE FUNCTION "forge_rewrite"() RETURNS integer LANGUAGE plpgsql AS $f$
DECLARE
  f record;
  t oid;
  v text;
  args text[];
  n integer := 0;
  texts text[] := {texts};
  docs text[] := {docs};
BEGIN
  FOR f IN SELECT p.proname::text AS name, p.prokind::text AS kind,
                  ARRAY(SELECT u.t FROM unnest(p.proargtypes::oid[]) WITH ORDINALITY AS u(t, o) ORDER BY u.o) AS types
             FROM pg_proc p JOIN pg_namespace ns ON ns.oid = p.pronamespace
            WHERE ns.nspname = 'vm' AND p.prokind IN ('f', 'p')
  LOOP
    FOR s IN 0..array_length(texts, 1) LOOP
      args := ARRAY[]::text[];
      FOR i IN 1..coalesce(array_length(f.types, 1), 0) LOOP
        t := f.types[i];
        IF s = array_length(texts, 1) THEN
          v := 'NULL::' || format_type(t, NULL);
        ELSIF t IN ('text'::regtype::oid, 'character varying'::regtype::oid, 'name'::regtype::oid, 'bpchar'::regtype::oid) THEN
          v := quote_literal(texts[1 + (s + i - 1) % array_length(texts, 1)]) || '::' || format_type(t, NULL);
        ELSIF t IN ('jsonb'::regtype::oid, 'json'::regtype::oid) THEN
          v := quote_literal(docs[1 + (s + i - 1) % array_length(docs, 1)]) || '::' || format_type(t, NULL);
        ELSIF t IN ('jsonb[]'::regtype::oid) THEN
          v := 'ARRAY[' || quote_literal(docs[1 + (s + i - 1) % array_length(docs, 1)]) || '::jsonb]';
        ELSIF t = 'text[]'::regtype::oid THEN
          v := quote_literal(ARRAY[texts[1 + s % array_length(texts, 1)], 'amount']::text) || '::text[]';
        ELSIF t IN ('integer'::regtype::oid, 'bigint'::regtype::oid, 'oid'::regtype::oid) THEN
          IF s % 2 = 0 THEN
            v := '''public.amounts''::regclass::oid::bigint::' || format_type(t, NULL);
          ELSE
            v := '2::' || format_type(t, NULL);
          END IF;
        ELSIF t = 'smallint'::regtype::oid THEN
          v := '1::smallint';
        ELSIF t = 'regclass'::regtype::oid THEN
          IF s % 2 = 0 THEN v := '''public.amounts''::regclass'; ELSE v := '''public.other''::regclass'; END IF;
        ELSIF t = 'boolean'::regtype::oid THEN
          IF s % 2 = 0 THEN v := 'true'; ELSE v := 'false'; END IF;
        ELSIF t IN ('timestamp with time zone'::regtype::oid, 'timestamp without time zone'::regtype::oid) THEN
          v := 'now()::' || format_type(t, NULL);
        ELSE
          v := 'NULL::' || format_type(t, NULL);
        END IF;
        args := args || v;
      END LOOP;
      BEGIN
        IF f.kind = 'p' THEN
          EXECUTE format('CALL vm.%I(%s)', f.name, array_to_string(args, ', '));
        ELSE
          EXECUTE format('SELECT vm.%I(%s)', f.name, array_to_string(args, ', '));
        END IF;
        n := n + 1;
      EXCEPTION WHEN OTHERS THEN NULL;
      END;
    END LOOP;
  END LOOP;
  RETURN n;
END
$f$;"#,
        texts = sql_array(&FORGED_TEXTS),
        docs = sql_array(&FORGED_DOCS),
    );
    Migration::new("0003z_forger", &[body.as_str()])
}

/// A value of type `ty` for argument `i` of a call in set `set`: the Rust
/// twin of `forge_rewrite()`'s choice.
fn forged_value(ty: &str, i: usize, set: usize) -> String {
    let text = FORGED_TEXTS[(set + i) % FORGED_TEXTS.len()];
    let doc = FORGED_DOCS[(set + i) % FORGED_DOCS.len()];
    match ty {
        "text" | "character varying" | "name" | "character" => format!("{}::{ty}", literal(text)),
        "jsonb" | "json" => format!("{}::{ty}", literal(doc)),
        "jsonb[]" => format!("ARRAY[{}::jsonb]", literal(doc)),
        "text[]" => format!("ARRAY[{}, 'amount']::text[]", literal(text)),
        "integer" | "bigint" | "oid" => {
            if set.is_multiple_of(2) {
                format!("'public.amounts'::regclass::oid::bigint::{ty}")
            } else {
                format!("2::{ty}")
            }
        }
        "smallint" => "1::smallint".to_owned(),
        "regclass" => {
            if set.is_multiple_of(2) {
                "'public.amounts'::regclass".to_owned()
            } else {
                "'public.other'::regclass".to_owned()
            }
        }
        "boolean" => if set.is_multiple_of(2) {
            "true"
        } else {
            "false"
        }
        .to_owned(),
        "timestamp with time zone" | "timestamp without time zone" => format!("now()::{ty}"),
        other => format!("NULL::{other}"),
    }
}

/// Every call of every function and procedure in `vm` the migration role
/// may execute, as the catalog has them now, each a `DO` block that swallows
/// its own failure, so that the migration goes on.
fn static_calls(r: &Rewrite) -> Vec<String> {
    let found = r.db.rows(&format!(
        "SELECT p.proname::text, p.prokind::text,
                coalesce((SELECT string_agg(format_type(t, NULL), '|' ORDER BY o)
                            FROM unnest(p.proargtypes::oid[]) WITH ORDINALITY u(t, o)), '')
           FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
          WHERE n.nspname = 'vm' AND p.prokind IN ('f', 'p')
            AND has_function_privilege({}, p.oid, 'EXECUTE')
          ORDER BY 1, 3",
        literal(&r.db.owner)
    ));
    let mut calls = Vec::new();
    for row in found {
        let text = |i: usize| row.get(i).cloned().flatten().unwrap_or_default();
        let (name, kind, types) = (text(0), text(1), text(2));
        let types: Vec<&str> = types.split('|').filter(|s| !s.is_empty()).collect();
        for set in 0..FORGED_TEXTS.len() {
            let arguments: Vec<String> = types
                .iter()
                .enumerate()
                .map(|(i, ty)| forged_value(ty, i, set))
                .collect();
            let call = if kind == "p" {
                format!("CALL vm.{}({})", ident(&name), arguments.join(", "))
            } else {
                format!("PERFORM vm.{}({})", ident(&name), arguments.join(", "))
            };
            calls.push(format!(
                "DO $c$ BEGIN {call}; EXCEPTION WHEN OTHERS THEN NULL; END $c$;"
            ));
        }
        let nulls: Vec<String> = types.iter().map(|ty| format!("NULL::{ty}")).collect();
        let verb = if kind == "p" { "CALL" } else { "PERFORM" };
        calls.push(format!(
            "DO $c$ BEGIN {verb} vm.{}({}); EXCEPTION WHEN OTHERS THEN NULL; END $c$;",
            ident(&name),
            nulls.join(", ")
        ));
    }
    calls
}

/// CONTRACT.md: "Nothing a migration calls in `vm` changes what is
/// recorded ... Whatever a migration calls, with whatever arguments,
/// before, in or after the statement that rewrites, the history afterwards
/// holds for each rewritten row what it was before the statement and what
/// it is after, and no entry for a row the rewrite did not change." The
/// migration calls `forge_rewrite()` before, inside (the same `DO` block)
/// and after a rewrite of `amounts`, and every function `vm` has that the
/// migration role may execute (from the catalog now), with the forger's
/// arguments, before and after a rewrite of `other`. Afterwards the
/// Version's entries are exactly the rewritten rows, each with what it
/// was and what it is, and the two `column-type` entries; or, if the
/// Version was refused or failed, none.
#[test]
fn nothing_a_migration_calls_in_vm_changes_what_a_rewrite_records() {
    let mut r = Rewrite::start_with(&[forger()]);
    let last = last_history_id(&r.db);
    let calls = static_calls(&r);
    assert!(
        !calls.is_empty(),
        "the harness's own check: the migration role may execute something in vm"
    );
    let migration = "0004_forging_rewrite";
    let mut statements: Vec<String> = vec![
        r#"SELECT "forge_rewrite"();"#.to_owned(),
        r#"DO $d$
BEGIN
  PERFORM "forge_rewrite"();
  ALTER TABLE "amounts" ALTER COLUMN "amount" SET DATA TYPE bigint USING coalesce("amount", 0) + 1;
  PERFORM "forge_rewrite"();
END
$d$;"#
            .to_owned(),
        r#"SELECT "forge_rewrite"();"#.to_owned(),
    ];
    statements.extend(calls.iter().cloned());
    statements.push(
        r#"ALTER TABLE "other" ALTER COLUMN "v" SET DATA TYPE bigint USING "v" + 100;"#.to_owned(),
    );
    statements.extend(calls.iter().cloned());
    let refs: Vec<&str> = statements.iter().map(String::as_str).collect();
    let sent = r.send(&[Migration::new(migration, &refs)]);
    match sent.status {
        201 => {}
        422 => {
            println!(
                "the forging Version was answered 422: {}",
                sent.json()["error"]
            );
            assert_nothing_after(&r, last, "a forging Version refused or failed");
            return;
        }
        other => panic!(
            "expected the forging Version refused or failed (422) or applied (201), got {other}:\n{}",
            sent.describe()
        ),
    }
    let all = blackbox::history::history_after(&r.db, last);
    let rows: Vec<Value> = all
        .iter()
        .filter(|e| e["kind"] != "structure")
        .cloned()
        .collect();
    let row =
        |id: i64, amount: Value, note: &str| json!({ "id": id, "amount": amount, "note": note });
    assert_row_entries(
        &rows,
        &[
            migration_update(
                migration,
                "amounts",
                id_key(1),
                row(1, json!(5), "five!"),
                row(1, json!(6), "five!"),
                json!(["amount"]),
            ),
            migration_update(
                migration,
                "amounts",
                id_key(2),
                row(2, json!(0), "zero"),
                row(2, json!(1), "zero"),
                json!(["amount"]),
            ),
            migration_update(
                migration,
                "amounts",
                id_key(3),
                row(3, Value::Null, "none"),
                row(3, json!(1), "none"),
                json!(["amount"]),
            ),
            migration_update(
                migration,
                "other",
                id_key(1),
                json!({ "id": 1, "v": 1 }),
                json!({ "id": 1, "v": 101 }),
                json!(["v"]),
            ),
        ],
        "every rewritten row with what it was and what it is, nothing else",
    );
    let mut structure: Vec<Value> = all
        .iter()
        .filter(|e| e["kind"] == "structure")
        .map(|e| {
            json!([
                e["table_name"],
                e["structure"],
                e["user_id"],
                e["migration"]
            ])
        })
        .collect();
    structure.sort_by_key(Value::to_string);
    let mut want = vec![
        json!(["amounts", { "change": "column-type", "column": "amount", "from": "integer", "to": "bigint" }, INSTALLER, migration]),
        json!(["other", { "change": "column-type", "column": "v", "from": "integer", "to": "bigint" }, INSTALLER, migration]),
    ];
    want.sort_by_key(Value::to_string);
    assert_eq!(
        structure, want,
        "the structure entries of the two rewrites, nothing else"
    );
}

// ---------------------------------------------------------------------------
// A statement may run more than once.
// ---------------------------------------------------------------------------

/// CONTRACT.md: "What stays of a Version is one run's work: a row a
/// migration inserts before its rewrite is there once, and once in the
/// history." Rows inserted by statements before a rewrite, and by the same
/// `DO` block as one.
#[test]
fn a_row_a_migration_inserts_before_its_rewrite_is_there_once_and_once_in_the_history() {
    let mut r = Rewrite::start();
    let last = last_history_id(&r.db);
    let migration = "0004_insert_then_rewrite";
    r.send_ok(&[Migration::new(
        migration,
        &[
            r#"INSERT INTO "other" VALUES (2, 2);"#,
            r#"INSERT INTO "things" ("title") VALUES ('inserted once');"#,
            r#"ALTER TABLE "amounts" ALTER COLUMN "amount" SET DATA TYPE numeric USING 0;"#,
            r#"DO $d$
BEGIN
  INSERT INTO "other" VALUES (3, 3);
  ALTER TABLE "wide" ALTER COLUMN "i" SET DATA TYPE bigint USING "i" + 1;
END
$d$;"#,
        ],
    )]);
    let other = r.rows_of("other");
    assert_eq!(
        other,
        vec![
            r#"{"v": 1, "id": 1}"#.to_owned(),
            r#"{"v": 2, "id": 2}"#.to_owned(),
            r#"{"v": 3, "id": 3}"#.to_owned(),
        ],
        "each inserted row there once"
    );
    let things =
        r.db.column("SELECT count(*)::text FROM things WHERE title = 'inserted once'");
    assert_eq!(things, vec!["1".to_owned()], "the row of things there once");
    let rows = row_entries(&r.db, last);
    let inserts: Vec<Value> = rows
        .iter()
        .filter(|e| e["kind"] == "insert")
        .cloned()
        .collect();
    let mut keys: Vec<String> = inserts
        .iter()
        .map(|e| {
            format!(
                "{} {}",
                e["table_name"],
                e["new"]["title"]
                    .as_str()
                    .map_or_else(|| e["key"].to_string(), str::to_owned)
            )
        })
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            r#""other" {"id":2}"#.to_owned(),
            r#""other" {"id":3}"#.to_owned(),
            r#""things" inserted once"#.to_owned(),
        ],
        "each inserted row once in the history: {}",
        lines(&rows)
    );
    let updates: Vec<Value> = rows
        .iter()
        .filter(|e| e["kind"] == "update")
        .cloned()
        .collect();
    assert_row_entries(
        &updates,
        &[
            migration_update(
                migration,
                "amounts",
                id_key(1),
                json!({ "id": 1, "amount": 5, "note": "five!" }),
                json!({ "id": 1, "amount": 0, "note": "five!" }),
                json!(["amount"]),
            ),
            migration_update(
                migration,
                "amounts",
                id_key(3),
                json!({ "id": 3, "amount": null, "note": "none" }),
                json!({ "id": 3, "amount": 0, "note": "none" }),
                json!(["amount"]),
            ),
            migration_update(
                migration,
                "wide",
                id_key(1),
                json!({ "id": 1, "i": 12, "v": "vee", "n": 1.5, "c": "c1" }),
                json!({ "id": 1, "i": 13, "v": "vee", "n": 1.5, "c": "c1" }),
                json!(["i"]),
            ),
        ],
        "the rewrites' entries, once each",
    );
}

/// A `DO` block that rewrites `turn_<k>` with `k` the next value of the
/// sequence `turns` modulo `tables`: sequences are not undone, so each run
/// of the statement picks the next table.
fn rewrite_by_turn(tables: usize) -> String {
    format!(
        r#"DO $d$
DECLARE
  k bigint := nextval('"turns"') % {tables};
BEGIN
  EXECUTE format('ALTER TABLE %I ALTER COLUMN "v" SET DATA TYPE bigint USING "v" + 1', 'turn_' || k);
END
$d$;"#
    )
}

/// CONTRACT.md: "At most 5 runs. A Version with a statement that rewrites
/// another table with history each time it runs (so that the platform
/// never has the rows before) is refused: `migration-refused`, rule
/// `rewrite-unsteady`, the message names the migration and the tables."
/// The statement rewrites `turn_1`, `turn_2`, ... in turn, a table it has
/// not rewritten before each time, ten tables round.
#[test]
fn a_statement_that_rewrites_another_table_each_time_it_runs_is_refused_rewrite_unsteady() {
    let mut r = Rewrite::start();
    let last = last_history_id(&r.db);
    let tables: Vec<String> = (0..blackbox::history_rewrite::TURNS)
        .map(|n| format!("turn_{n}"))
        .collect();
    let before: Vec<Vec<String>> = tables.iter().map(|t| r.rows_of(t)).collect();
    let migration = "0004_unsteady";
    let statement = rewrite_by_turn(blackbox::history_rewrite::TURNS);
    let sent = r.send(&[Migration::new(migration, &[statement.as_str()])]);
    assert_refused(
        &sent,
        "rewrite-unsteady",
        migration,
        &[migration, "turn_"],
        "a statement that rewrites another table each time it runs",
    );
    let after: Vec<Vec<String>> = tables.iter().map(|t| r.rows_of(t)).collect();
    assert_eq!(after, before, "nothing kept");
    assert_nothing_after(&r, last, "the refused Version");
    println!(
        "the sequence turns after the refused Version: {:?}",
        r.db.column("SELECT last_value::text FROM turns")
    );
}

/// CONTRACT.md: "It may therefore run a Version's migrations, or some of
/// their statements, more than once, each earlier run undone before the
/// next"; "At most 5 runs". A statement that rewrites `turn_0` and
/// `turn_1` in turn has rewritten, by its third run, only tables it
/// rewrote before: it settles within 5 runs, applies, and what is recorded
/// is the run that stayed: the table whose row changed has its entry, the
/// other none.
#[test]
fn a_statement_that_rewrites_two_tables_in_turn_settles_and_is_recorded_as_the_run_that_stayed() {
    let mut r = Rewrite::start();
    let last = last_history_id(&r.db);
    let migration = "0004_two_in_turn";
    let statement = rewrite_by_turn(2);
    r.send_ok(&[Migration::new(migration, &[statement.as_str()])]);
    let changed: Vec<&str> = ["turn_0", "turn_1"]
        .into_iter()
        .filter(|t| r.rows_of(t) != vec![r#"{"v": 1, "id": 1}"#.to_owned()])
        .collect();
    assert_eq!(
        changed.len(),
        1,
        "the harness's own check: one run stayed, which rewrote one of the two tables: {changed:?}"
    );
    let table = changed[0];
    assert_eq!(
        r.rows_of(table),
        vec![r#"{"v": 2, "id": 1}"#.to_owned()],
        "the harness's own check: {table} rewritten once"
    );
    assert_row_entries(
        &row_entries(&r.db, last),
        &[migration_update(
            migration,
            table,
            id_key(1),
            json!({ "id": 1, "v": 1 }),
            json!({ "id": 1, "v": 2 }),
            json!(["v"]),
        )],
        "the run that stayed: its table's row, once; the other table none",
    );
}
