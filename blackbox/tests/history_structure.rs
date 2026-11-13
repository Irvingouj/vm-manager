//! Changes of structure that change data (CONTRACT.md, "The Change history
//! in the platform database: Changes of structure that change data" and
//! "Through the SDK"; ticket "The Change history lives in the platform
//! database, shipped from an outbox", fifth acceptance criterion: "A
//! migration that changes a column's type with `USING`, drops a column or a
//! table, or replaces a table under its name, leaves a `structure` entry
//! that a reader of any row's history of that table sees").
//!
//! Every test installs `board` with tables a User then writes, so that each
//! has history, and sends a later Version whose migration changes their
//! structure:
//!
//! - each row of the contract's table is one entry of the table's history
//!   in `change_history`: `kind` `structure`, `actor` `migration`, the
//!   Contributor who sent the Version, the migration's name, no request,
//!   `key`, `old`, `new` and `changed` null, and `structure` as the table
//!   gives it, the types as Postgres names them;
//! - its `id` is greater than those of the rows the same migration changed
//!   before it; a Version refused or failed leaves none;
//! - a `history` read answers it among every row's changes, by `id`, newest
//!   first, with a key, without one, and with `limit`;
//! - `table-rewritten`: a column's type "changed" to itself with `USING`,
//!   `CLUSTER`, `SET LOGGED`, a column added with a volatile default; none
//!   when a `column-type` entry of the same migration explains the rewrite;
//! - an enum value renamed: `columns` in the table's column order;
//! - a partition attached or detached: the entry is the parent's, the
//!   partition's own history has none;
//! - a change of structure that changes no data has none; a table has
//!   entries from the end of the migration that made it: none for what that
//!   migration did, one for what a later migration of the same Version
//!   does;
//! - through the SDK (`fixtures/sdk/chronicle`, route `/history/full`) it
//!   is a change of `kind` `"structure"` with `structure` the object (its
//!   column names the database's) and `old`, `new`, `changed` null; a row's
//!   change has `structure` null.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::api::assert_api_error;
use blackbox::history::{
    APP, Change, assert_at_within, assert_change, db_now, history_after, history_where,
    last_history_id,
};
use blackbox::history_op::changes_of;
use blackbox::history_structure::{
    Structure, WRITER, answered_ids, assert_structure_change, assert_structure_row,
    assert_structure_rows, newest_first, send_chronicle, structure_entries,
};
use blackbox::own_database::{Migration, assert_ok, own_execute};
use blackbox::scenario::lines;
use blackbox::sdk::{ann, d};
use blackbox::sdk_history::{
    HistoryCall, SEED_MIGRATION, Want, assert_change as assert_sdk_change,
    assert_ids_are_history_rows, assert_ids_decrease, assert_one_history_entry_under, chronicle,
    id_of,
};
use blackbox::{INSTALLER, Response, User};
use serde_json::{Value, json};

const ACTORS: [&str; 2] = ["user", "application"];

fn column_type(column: &str, from: &str, to: &str) -> Value {
    json!({ "change": "column-type", "column": column, "from": from, "to": to })
}

fn column_dropped(column: &str) -> Value {
    json!({ "change": "column-dropped", "column": column })
}

/// The type of a column of `public.<table>` as Postgres names it.
fn type_of(structure: &Structure, table: &str, column: &str) -> Option<String> {
    structure.db.superuser().value(&format!(
        "SELECT format_type(atttypid, atttypmod) FROM pg_attribute
          WHERE attrelid = 'public.{table}'::regclass AND attname = '{column}' AND NOT attisdropped"
    ))
}

#[test]
fn a_column_type_changed_with_or_without_using_is_an_entry_after_the_rows_changed_before_it() {
    let mut s = Structure::start();
    let last = last_history_id(&s.db);
    let before = db_now(&s.db);
    let migration = "0004_column_types";
    s.send_ok(&[Migration::new(
        migration,
        &[
            r#"UPDATE "things" SET "size" = "size" + 100 WHERE "id" = 1;"#,
            r#"ALTER TABLE "things" ALTER COLUMN "size" SET DATA TYPE bigint;"#,
            r#"ALTER TABLE "priced" ALTER COLUMN "price" SET DATA TYPE numeric(12, 3);"#,
            r#"ALTER TABLE "priced" ALTER COLUMN "code" SET DATA TYPE integer USING "code"::integer;"#,
        ],
    )]);
    let after = db_now(&s.db);
    assert_eq!(
        type_of(&s, "priced", "code").as_deref(),
        Some("integer"),
        "the harness's own check: the Version changed the type"
    );
    let rows = history_after(&s.db, last);
    assert_eq!(
        rows.len(),
        4,
        "expected the migration's update of things and one structure entry per column whose type it changed, got:\n{}",
        lines(&rows)
    );
    assert_change(
        &rows[0],
        &Change::by_migration(INSTALLER, migration)
            .on("public", "things")
            .update(
                json!({ "id": 1 }),
                json!({ "id": 1, "title": "one", "size": 1, "tags": ["a"] }),
                json!({ "id": 1, "title": "one", "size": 101, "tags": ["a"] }),
                json!(["size"]),
            ),
        "the update the migration made before changing the type, recorded first",
    );
    let update_id = rows[0]["id"].as_i64().unwrap();
    let structure: Vec<Value> = rows[1..].to_vec();
    for row in &structure {
        assert!(
            row["id"].as_i64().is_some_and(|id| id > update_id),
            "a structure entry's id is greater than those of the rows the same migration changed before it ({update_id}): {row}"
        );
    }
    assert_structure_rows(
        &structure,
        &[
            ("things", column_type("size", "integer", "bigint")),
            (
                "priced",
                column_type("price", "numeric(10,2)", "numeric(12,3)"),
            ),
            ("priced", column_type("code", "text", "integer")),
        ],
        migration,
        "a column's type changed, without USING and with it",
    );
    assert_at_within(
        &s.db,
        &structure,
        &before,
        &after,
        "the structure entries of the Version",
    );
}

#[test]
fn a_column_dropped_is_an_entry() {
    let mut s = Structure::start();
    let last = last_history_id(&s.db);
    let migration = "0004_drop_columns";
    s.send_ok(&[Migration::new(
        migration,
        &[
            r#"ALTER TABLE "things" DROP COLUMN "tags";"#,
            r#"ALTER TABLE "priced" DROP COLUMN "code";"#,
        ],
    )]);
    let rows = history_after(&s.db, last);
    assert_structure_rows(
        &rows,
        &[
            ("things", column_dropped("tags")),
            ("priced", column_dropped("code")),
        ],
        migration,
        "two columns dropped, and nothing else recorded",
    );
}

#[test]
fn a_table_dropped_is_an_entry_after_the_rows_deleted_before_it_and_its_history_stays() {
    let mut s = Structure::start();
    let earlier = history_where(&s.db, "h.table_name = 'gone_table'");
    assert_eq!(earlier.len(), 2, "the harness's own check: {earlier:?}");
    let last = last_history_id(&s.db);
    let migration = "0004_drop_table";
    s.send_ok(&[Migration::new(
        migration,
        &[
            r#"DELETE FROM "gone_table" WHERE "id" = 1;"#,
            r#"DROP TABLE "gone_table";"#,
        ],
    )]);
    let rows = history_after(&s.db, last);
    assert_eq!(
        rows.len(),
        2,
        "expected the delete and then one entry table-dropped, got:\n{}",
        lines(&rows)
    );
    assert_change(
        &rows[0],
        &Change::by_migration(INSTALLER, migration)
            .on("public", "gone_table")
            .delete(json!({ "id": 1 }), json!({ "id": 1, "note": "a" })),
        "the delete the migration made before dropping the table",
    );
    assert_structure_row(
        &rows[1],
        "gone_table",
        migration,
        &json!({ "change": "table-dropped" }),
        "a table dropped, with none under its name",
    );
    assert!(
        rows[1]["id"].as_i64() > rows[0]["id"].as_i64(),
        "the entry's id is greater than the delete's: {}",
        lines(&rows)
    );
    let still: Vec<Value> = history_where(&s.db, &format!("h.id <= {last}"))
        .into_iter()
        .filter(|r| r["table_name"] == "gone_table")
        .collect();
    assert_eq!(
        still, earlier,
        "the dropped table's earlier history stays as it was"
    );
    for actor in ACTORS {
        let answer = s.read(actor, "gone_table", &json!({}));
        assert_eq!(
            answer["error"]["kind"], "database",
            "a table that is not there now: {answer}"
        );
        assert_eq!(
            answer["error"]["code"], "42P01",
            "a table that is not there now: {answer}"
        );
    }
}

#[test]
fn a_table_replaced_under_its_name_either_way_is_an_entry_on_the_name() {
    let mut s = Structure::start();
    let last = last_history_id(&s.db);
    let migration = "0004_replace_tables";
    s.send_ok(&[Migration::new(
        migration,
        &[
            r#"DROP TABLE "swapped";"#,
            r#"CREATE TABLE "swapped" (
	"id" integer PRIMARY KEY NOT NULL,
	"other" text
);"#,
            r#"ALTER TABLE "swapped_away" RENAME TO "swapped_aside";"#,
            r#"CREATE TABLE "swapped_away" (
	"id" integer PRIMARY KEY NOT NULL,
	"label" text
);"#,
        ],
    )]);
    let rows = history_after(&s.db, last);
    assert_structure_rows(
        &rows,
        &[
            ("swapped", json!({ "change": "table-replaced" })),
            ("swapped_away", json!({ "change": "table-replaced" })),
        ],
        migration,
        "a table dropped and another made under its name; a table renamed away and another made under its name (the rename itself no entry)",
    );
    for table in ["swapped", "swapped_away"] {
        let entry = rows.iter().find(|r| r["table_name"] == table).unwrap();
        for actor in ACTORS {
            let context = format!("history of {table} {{ id: 1 }} as the {actor}");
            let answer = s.read(actor, table, &json!({ "key": { "id": 1 } }));
            let changes = changes_of(&answer);
            assert_eq!(
                changes.len(),
                2,
                "{context}: the entry and the earlier insert under the name, got:\n{}",
                lines(&changes)
            );
            assert_structure_change(
                &changes[0],
                migration,
                &json!({ "change": "table-replaced" }),
                &context,
            );
            assert_eq!(
                answered_ids(&changes)[0],
                entry["id"].as_i64().unwrap(),
                "{context}"
            );
            assert_eq!(changes[1]["kind"], "insert", "{context}: {}", changes[1]);
            assert_eq!(changes[1]["user_id"], WRITER, "{context}: {}", changes[1]);
        }
    }
}

#[test]
fn an_enum_value_renamed_is_an_entry_on_every_table_with_a_column_of_it() {
    let mut s = Structure::start();
    // A table whose enum columns are not in the order of their names, made
    // by a Version of its own and given history.
    s.send_ok(&[Migration::new(
        "0004_backwards_moods",
        &[r#"CREATE TABLE "backwards_moods" (
	"id" integer PRIMARY KEY NOT NULL,
	"z" "mood"[],
	"m" text,
	"a" "mood"
);"#],
    )]);
    let (_, answer) = s.own.one(
        &User::new(WRITER),
        APP,
        &own_execute(
            "user",
            "insert into backwards_moods (id, z, m, a) values (1, '{sad}', 'm', 'happy')",
        ),
    );
    assert_ok(&answer, "the harness's own check: a row of backwards_moods");
    let last = last_history_id(&s.db);
    let migration = "0005_rename_mood";
    s.send_ok(&[Migration::new(
        migration,
        &[r#"ALTER TYPE "public"."mood" RENAME VALUE 'sad' TO 'blue';"#],
    )]);
    let renamed = |columns: &[&str]| json!({ "change": "enum-value-renamed", "type": "public.mood", "from": "sad", "to": "blue", "columns": columns });
    let rows = history_after(&s.db, last);
    assert_structure_rows(
        &rows,
        &[
            ("moods", renamed(&["mood"])),
            ("mood_lists", renamed(&["moods"])),
            ("two_moods", renamed(&["a", "b"])),
            ("backwards_moods", renamed(&["z", "a"])),
        ],
        migration,
        "an enum's value renamed: one entry on each table with a column of it, also as an array, `columns` in the table's column order; none on a table without",
    );
}

#[test]
fn a_partition_attached_or_detached_is_an_entry_on_the_partitioned_table() {
    let mut s = Structure::start();
    let last = last_history_id(&s.db);
    let migration = "0004_partitions";
    s.send_ok(&[Migration::new(
        migration,
        &[
            r#"ALTER TABLE "measures" ATTACH PARTITION "measures_2027" FOR VALUES FROM ('2027-01-01') TO ('2028-01-01');"#,
            r#"ALTER TABLE "measures" DETACH PARTITION "measures_2026";"#,
        ],
    )]);
    // CONTRACT.md: "The entry is the parent's; the partition's own history
    // has none": these two, and nothing on measures_2026 or measures_2027.
    let rows = structure_entries(&s.db, last);
    assert_structure_rows(
        &rows,
        &[
            (
                "measures",
                json!({ "change": "partition-attached", "partition": "public.measures_2027" }),
            ),
            (
                "measures",
                json!({ "change": "partition-detached", "partition": "public.measures_2026" }),
            ),
        ],
        migration,
        "a partition attached to the table and one detached from it, and no entry on either partition",
    );
    for table in ["measures_2026", "measures_2027"] {
        for actor in ACTORS {
            let changes = changes_of(&s.read(actor, table, &json!({})));
            assert!(
                changes.iter().all(|c| c["kind"] != "structure"),
                "history of the partition {table} as the {actor}: no structure entry; got\n{}",
                lines(&changes)
            );
        }
    }
}

#[test]
fn a_structure_entry_is_in_every_rows_history_in_the_order_of_id() {
    let mut s = Structure::start();
    let last = last_history_id(&s.db);
    let migration = "0004_drop_tags";
    s.send_ok(&[Migration::new(
        migration,
        &[r#"ALTER TABLE "things" DROP COLUMN "tags";"#],
    )]);
    let dropped = column_dropped("tags");
    let entries = structure_entries(&s.db, last);
    assert_structure_rows(
        &entries,
        &[("things", dropped.clone())],
        migration,
        "the column dropped",
    );
    let entry_id = entries[0]["id"].as_i64().unwrap();
    // A change after the entry, to row 2.
    let (_, answer) = s.own.one(
        &User::new(WRITER),
        APP,
        &own_execute("user", "update things set size = 200 where id = 2"),
    );
    assert_ok(
        &answer,
        "the harness's own check: an update after the Version",
    );
    let all = history_where(&s.db, "h.table_name = 'things'");
    assert!(
        all.iter()
            .any(|r| r["kind"] == "update" && r["id"].as_i64() > Some(entry_id)),
        "the harness's own check: the update after the Version is recorded: {}",
        lines(&all)
    );
    for actor in ACTORS {
        for id in [1, 2, 3] {
            let context = format!("history of things {{ id: {id} }} as the {actor}");
            let answer = s.read(actor, "things", &json!({ "key": { "id": id } }));
            let changes = changes_of(&answer);
            let expected: Vec<Value> = all
                .iter()
                .filter(|r| r["key"] == json!({ "id": id }) || r["kind"] == "structure")
                .cloned()
                .collect();
            assert_eq!(
                answered_ids(&changes),
                newest_first(&expected),
                "{context}: expected the row's changes and the structure entry, by id, newest first; got:\n{}",
                lines(&changes)
            );
            let found: Vec<&Value> = changes
                .iter()
                .filter(|c| c["kind"] == "structure")
                .collect();
            assert_eq!(found.len(), 1, "{context}: {}", lines(&changes));
            assert_structure_change(found[0], migration, &dropped, &context);
        }
        let context = format!("history of things as the {actor}, every row");
        let changes = changes_of(&s.read(actor, "things", &json!({})));
        assert_eq!(
            answered_ids(&changes),
            newest_first(&all),
            "{context}: {}",
            lines(&changes)
        );
        let context = format!("history of things {{ id: 1 }}, limit 1, as the {actor}");
        let changes =
            changes_of(&s.read(actor, "things", &json!({ "key": { "id": 1 }, "limit": 1 })));
        assert_eq!(
            changes.len(),
            1,
            "{context}: limit counts the structure entry too: {}",
            lines(&changes)
        );
        assert_structure_change(&changes[0], migration, &dropped, &context);
        for key in [json!({ "id": 99 }), json!({ "title": "one" })] {
            let context = format!("history of things {key} as the {actor}");
            let changes = changes_of(&s.read(actor, "things", &json!({ "key": key })));
            assert_eq!(
                changes.len(),
                1,
                "{context}: a key of no row, or not the primary key, answers the structure entries only: {}",
                lines(&changes)
            );
            assert_structure_change(&changes[0], migration, &dropped, &context);
        }
    }
}

#[test]
fn a_version_refused_or_failed_leaves_no_structure_entry() {
    let mut s = Structure::start();
    let last = last_history_id(&s.db);
    let refused = s.send(&[Migration::new(
        "0004_refused",
        &[
            r#"UPDATE "things" SET "size" = "size" + 1 WHERE "id" = 1;"#,
            r#"ALTER TABLE "things" ALTER COLUMN "size" SET DATA TYPE bigint;"#,
            r#"ALTER TABLE "priced" ADD CONSTRAINT "priced_code_unique" UNIQUE("code") DEFERRABLE;"#,
        ],
    )]);
    assert_api_error(&refused, 422, "migration-refused");
    let failed = s.send(&[Migration::new(
        "0004_fails",
        &[
            r#"ALTER TABLE "things" DROP COLUMN "tags";"#,
            r#"DROP TABLE "gone_table";"#,
            r#"SELECT 1 / 0;"#,
        ],
    )]);
    assert_api_error(&failed, 422, "migration-failed");
    assert_eq!(
        type_of(&s, "things", "size").as_deref(),
        Some("integer"),
        "the harness's own check: nothing of the refused Version is kept"
    );
    assert!(
        type_of(&s, "things", "tags").is_some(),
        "the harness's own check: nothing of the failed Version is kept"
    );
    let rows = history_after(&s.db, last);
    assert!(
        rows.is_empty(),
        "a Version refused or failed leaves no entry, structure or row: got\n{}",
        lines(&rows)
    );
    // The same change in a Version that is accepted: one entry, its own.
    let migration = "0004_drop_tags";
    s.send_ok(&[Migration::new(
        migration,
        &[r#"ALTER TABLE "things" DROP COLUMN "tags";"#],
    )]);
    assert_structure_rows(
        &history_after(&s.db, last),
        &[("things", column_dropped("tags"))],
        migration,
        "the accepted Version after the refused and the failed one",
    );
}

#[test]
fn changes_of_structure_that_change_no_data_have_no_entry() {
    let mut s = Structure::start();
    let last = last_history_id(&s.db);
    let renamed_before = history_where(&s.db, "h.table_name = 'renamed_t'");
    assert_eq!(
        renamed_before.len(),
        2,
        "the harness's own check: {renamed_before:?}"
    );
    s.send_ok(&[Migration::new(
        "0004_no_data_changed",
        &[
            r#"ALTER TABLE "plain" RENAME COLUMN "a" TO "a2";"#,
            r#"ALTER TABLE "plain" ADD COLUMN "c" text;"#,
            r#"ALTER TABLE "plain" ALTER COLUMN "b" SET DEFAULT 5;"#,
            r#"ALTER TABLE "plain" ALTER COLUMN "b" SET NOT NULL;"#,
            r#"ALTER TABLE "plain" ADD CONSTRAINT "plain_b_check" CHECK ("b" >= 0);"#,
            r#"ALTER TABLE "plain" ADD CONSTRAINT "plain_a2_unique" UNIQUE("a2");"#,
            r#"CREATE INDEX "plain_b_idx" ON "plain" USING btree ("b");"#,
            r#"ALTER TABLE "renamed_t" RENAME TO "renamed_t2";"#,
        ],
    )]);
    let rows = history_after(&s.db, last);
    assert!(
        rows.is_empty(),
        "a column or a table renamed, a column added, a default, a constraint, an index: no entry; got\n{}",
        lines(&rows)
    );
    assert_eq!(
        history_where(&s.db, "h.table_name = 'renamed_t'"),
        renamed_before,
        "a renamed table's earlier changes stay under its old name"
    );
    for actor in ACTORS {
        let answer = s.read(actor, "renamed_t2", &json!({}));
        assert_eq!(
            changes_of(&answer),
            Vec::<Value>::new(),
            "history of the table under its new name, as the {actor}: its earlier changes are under the old name"
        );
        let answer = s.read(actor, "renamed_t", &json!({}));
        assert_eq!(
            answer["error"]["code"], "42P01",
            "history under the old name, which no table has now, as the {actor}: {answer}"
        );
        let answer = s.read(actor, "plain", &json!({ "key": { "id": 1 } }));
        let changes = changes_of(&answer);
        assert!(
            changes.len() == 2 && changes.iter().all(|c| c["kind"] != "structure"),
            "history of plain {{ id: 1 }} as the {actor}: the insert and the update, no structure entry; got\n{}",
            lines(&changes)
        );
    }
}

#[test]
fn a_table_has_entries_from_the_end_of_the_migration_that_made_it() {
    let mut s = Structure::start();
    let last = last_history_id(&s.db);
    s.send_ok(&[
        Migration::new(
            "0004_make_fresh",
            &[
                r#"CREATE TABLE "fresh" (
	"id" integer PRIMARY KEY NOT NULL,
	"a" integer,
	"b" text,
	"c" text
);"#,
                r#"INSERT INTO "fresh" VALUES (1, 1, 'x', 'c'), (2, 2, 'y', 'c');"#,
                r#"ALTER TABLE "fresh" ALTER COLUMN "a" SET DATA TYPE bigint;"#,
                r#"ALTER TABLE "fresh" DROP COLUMN "c";"#,
            ],
        ),
        Migration::new(
            "0005_fill_fresh",
            &[r#"INSERT INTO "fresh" VALUES (3, 3, 'z');"#],
        ),
        Migration::new(
            "0006_reshape",
            &[
                r#"ALTER TABLE "fresh" ALTER COLUMN "a" SET DATA TYPE numeric(12,2);"#,
                r#"ALTER TABLE "fresh" DROP COLUMN "b";"#,
                r#"ALTER TABLE "things" DROP COLUMN "tags";"#,
            ],
        ),
    ]);
    let rows = history_after(&s.db, last);
    let fresh_rows: Vec<&Value> = rows
        .iter()
        .filter(|r| r["table_name"] == "fresh" && r["kind"] != "structure")
        .collect();
    assert!(
        fresh_rows.len() == 1
            && fresh_rows[0]["kind"] == "insert"
            && fresh_rows[0]["migration"] == "0005_fill_fresh",
        "fresh: nothing of what 0004, which made it, did (its inserts, its type change, its drop); 0005's insert recorded; got\n{}",
        lines(&rows)
    );
    let structure: Vec<Value> = rows
        .iter()
        .filter(|r| r["kind"] == "structure")
        .cloned()
        .collect();
    assert_structure_rows(
        &structure,
        &[
            ("fresh", column_type("a", "bigint", "numeric(12,2)")),
            ("fresh", column_dropped("b")),
            ("things", column_dropped("tags")),
        ],
        "0006_reshape",
        "0006, a later migration of the Version that made fresh, leaves entries on it, as on a table older than the Version; 0004, which made it, none",
    );
    let insert_id = fresh_rows[0]["id"].as_i64().unwrap();
    assert!(
        structure.iter().all(|r| r["id"].as_i64() > Some(insert_id)),
        "0006's entries come after 0005's insert: {}",
        lines(&rows)
    );
}

/// The file node of `public.<table>`: Postgres gives a table a new one
/// when it writes its rows anew.
fn filenode(structure: &Structure, table: &str) -> Option<String> {
    structure.db.superuser().value(&format!(
        "SELECT pg_relation_filenode('public.{table}')::text"
    ))
}

/// CONTRACT.md: `table-rewritten` when "Postgres wrote the table's rows
/// anew and no `column-type` entry of the same migration shows why: a
/// column's type \"changed\" to the type it had, with `USING` ..., and
/// also ... `CLUSTER`, `SET LOGGED`, a column added with a volatile
/// default". One table each, in one migration; and a table whose rewrite a
/// `column-type` entry of the same migration explains has that entry only.
#[test]
fn a_table_rewritten_with_no_column_type_entry_to_say_why_is_an_entry() {
    let mut s = Structure::start();
    // A table made unlogged by a Version of its own, given history, so that
    // a later one can set it logged.
    s.send_ok(&[Migration::new(
        "0004_unlogged",
        &[r#"CREATE UNLOGGED TABLE "scratch" (
	"id" integer PRIMARY KEY NOT NULL,
	"v" text
);"#],
    )]);
    let (_, answer) = s.own.one(
        &User::new(WRITER),
        APP,
        &own_execute("user", "insert into scratch (id, v) values (1, 's')"),
    );
    assert_ok(&answer, "the harness's own check: a row of scratch");
    let last = last_history_id(&s.db);
    let tables = ["priced", "plain", "scratch", "no_moods", "swapped"];
    let before: Vec<Option<String>> = tables.iter().map(|t| filenode(&s, t)).collect();
    let migration = "0005_rewrites";
    s.send_ok(&[Migration::new(
        migration,
        &[
            r#"ALTER TABLE "priced" ALTER COLUMN "code" SET DATA TYPE text USING upper("code");"#,
            r#"CLUSTER "plain" USING "plain_pkey";"#,
            r#"ALTER TABLE "scratch" SET LOGGED;"#,
            r#"ALTER TABLE "no_moods" ADD COLUMN "r" double precision DEFAULT random();"#,
            r#"ALTER TABLE "swapped" ALTER COLUMN "label" SET DATA TYPE integer USING length("label");"#,
        ],
    )]);
    let after: Vec<Option<String>> = tables.iter().map(|t| filenode(&s, t)).collect();
    for ((table, b), a) in tables.iter().zip(&before).zip(&after) {
        assert!(
            b.is_some() && b != a,
            "the harness's own check: Postgres wrote {table} anew (its file node {b:?} then {a:?})"
        );
    }
    let rewritten = || json!({ "change": "table-rewritten" });
    assert_structure_rows(
        &history_after(&s.db, last),
        &[
            ("priced", rewritten()),
            ("plain", rewritten()),
            ("scratch", rewritten()),
            ("no_moods", rewritten()),
            ("swapped", column_type("label", "text", "integer")),
        ],
        migration,
        "a type changed to itself with USING, CLUSTER, SET LOGGED, a column added with a volatile default: table-rewritten; a type changed: column-type and no table-rewritten",
    );
    for actor in ACTORS {
        let context = format!("history of priced {{ id: 1 }} as the {actor}");
        let changes = changes_of(&s.read(actor, "priced", &json!({ "key": { "id": 1 } })));
        assert!(
            !changes.is_empty(),
            "{context}: expected the entry and the insert, got nothing"
        );
        assert_structure_change(&changes[0], migration, &rewritten(), &context);
    }
}

#[test]
fn through_the_sdk_a_structure_entry_is_a_change_of_kind_structure() {
    let sdk = chronicle();
    let user = ann();
    let full =
        |actor: &str, key: Option<Value>, options: Option<Value>| -> (Response, Vec<Value>) {
            let mut path = format!("/history/full?as={actor}&table=reshaped");
            if let Some(key) = key {
                path.push_str(&format!(
                    "&key={}",
                    blackbox::sdk_history::encode(&key.to_string())
                ));
            }
            if let Some(options) = options {
                path.push_str(&format!(
                    "&options={}",
                    blackbox::sdk_history::encode(&options.to_string())
                ));
            }
            let (response, body) = sdk.get_ok(&user, &path);
            let changes = body["changes"].as_array().cloned().unwrap_or_else(|| {
                panic!("expected history(...) to resolve to changes, got {body}")
            });
            (response, changes)
        };
    // The first Version: 0002_reshape dropped `gone` of `reshaped`, which
    // 0000 of the same Version made: an entry (a later migration of the
    // same Version), newest; then 0001_seed's three changes, each with
    // `structure` null.
    let gone = d::object(&[
        ("change", d::string("column-dropped")),
        ("column", d::string("gone")),
    ]);
    let entry_of = |migration: &'static str| Want {
        kind: "structure",
        ..Want::insert("", "", "", &d::null()).by_migration(INSTALLER, migration)
    };
    for actor in ACTORS {
        let context =
            format!("history(reshaped, {{ id: 1 }}) as the {actor} after the first Version");
        let (_, changes) = full(actor, Some(json!({ "id": 1 })), None);
        assert_eq!(
            changes.len(),
            4,
            "{context}: 0002_reshape's entry and 0001_seed's three changes; got:\n{}",
            lines(&changes)
        );
        assert_sdk_change(&changes[0], &entry_of("0002_reshape"), &context);
        assert_eq!(changes[0]["structure"], gone, "{context}: {}", changes[0]);
        for change in &changes[1..] {
            assert_eq!(
                change["structure"],
                d::null(),
                "{context}: a row's change has `structure` null: {change}"
            );
        }
        for change in &changes {
            assert!(
                change["properties"]
                    .as_array()
                    .is_some_and(|p| p.contains(&json!("structure"))),
                "{context}: a change has the property `structure`: {change}"
            );
        }
    }
    // A later Version retypes `keep` and `after_name` (`afterName` in the
    // Application's schema): the entries name the database's columns.
    let migration = "0003_recast_keep";
    send_chronicle(
        &sdk,
        &[Migration::new(
            migration,
            &[
                r#"ALTER TABLE "reshaped" ALTER COLUMN "keep" SET DATA TYPE varchar(20);"#,
                r#"ALTER TABLE "reshaped" ALTER COLUMN "after_name" SET DATA TYPE varchar(30);"#,
            ],
        )],
    );
    let keep = d::object(&[
        ("change", d::string("column-type")),
        ("column", d::string("keep")),
        ("from", d::string("text")),
        ("to", d::string("character varying(20)")),
    ]);
    let after_name = d::object(&[
        ("change", d::string("column-type")),
        ("column", d::string("after_name")),
        ("from", d::string("text")),
        ("to", d::string("character varying(30)")),
    ]);
    let k1 = d::object(&[
        ("id", d::number("1")),
        ("keep", d::string("k1")),
        ("added", d::null()),
        ("afterName", d::null()),
    ]);
    let k2 = d::object(&[
        ("id", d::number("1")),
        ("keep", d::string("k2")),
        ("added", d::null()),
        ("afterName", d::null()),
    ]);
    let by_seed = |want: Want<'static>| want.by_migration(INSTALLER, SEED_MIGRATION);
    let seeded = [
        by_seed(Want::update("", "", "", &k2, &k2, &["gone"])),
        by_seed(Want::update("", "", "", &k1, &k2, &["keep"])),
        by_seed(Want::insert("", "", "", &k1)),
    ];
    let structure_want = entry_of(migration);
    // The two entries of 0003, in either order (CONTRACT.md does not order
    // the entries of one migration).
    let assert_newest_two = |changes: &[Value], context: &str| {
        let mut got: Vec<String> = changes[..2]
            .iter()
            .map(|c| {
                assert_sdk_change(c, &structure_want, context);
                c["structure"].to_string()
            })
            .collect();
        got.sort();
        let mut want = vec![keep.to_string(), after_name.to_string()];
        want.sort();
        assert_eq!(
            got, want,
            "{context}: the two entries of {migration}, columns by their names in the database"
        );
    };
    for actor in ACTORS {
        let context = format!("history(reshaped, {{ id: 1 }}) as the {actor}");
        let (response, changes) = full(actor, Some(json!({ "id": 1 })), None);
        assert_eq!(
            changes.len(),
            6,
            "{context}: {migration}'s two entries, 0002_reshape's entry and 0001_seed's three changes; got:\n{}",
            lines(&changes)
        );
        assert_newest_two(&changes, &context);
        assert_sdk_change(&changes[2], &entry_of("0002_reshape"), &context);
        assert_eq!(changes[2]["structure"], gone, "{context}: {}", changes[2]);
        for (change, want) in changes[3..].iter().zip(&seeded) {
            assert_sdk_change(change, want, &context);
            assert_eq!(change["structure"], d::null(), "{context}: {change}");
        }
        assert_ids_decrease(&changes, &context);
        assert_ids_are_history_rows(&sdk.own, &changes, "public", "reshaped", &context);
        // The entry says the Version serving: the second, sent above.
        assert_one_history_entry_under(&sdk, &response, &user, actor, 6, &[], 2, &context);

        let context = format!("history(reshaped, {{ id: 1 }}, {{ limit: 1 }}) as the {actor}");
        let (_, changes) = full(actor, Some(json!({ "id": 1 })), Some(json!({ "limit": 1 })));
        assert_eq!(changes.len(), 1, "{context}: {}", lines(&changes));
        assert_sdk_change(&changes[0], &structure_want, &context);
        assert!(
            changes[0]["structure"] == keep || changes[0]["structure"] == after_name,
            "{context}: one of {migration}'s entries: {}",
            changes[0]
        );

        let context = format!("history(reshaped) as the {actor}");
        let (_, changes) = full(actor, None, None);
        assert_eq!(changes.len(), 6, "{context}: {}", lines(&changes));
        assert_newest_two(&changes, &context);
    }
    // The existing route still answers the entry, by its kind.
    let (_, changes) = HistoryCall::of("reshaped").key(json!({ "id": 1 })).changes(
        &sdk,
        &user,
        "history(reshaped, { id: 1 }) through /history",
    );
    assert_eq!(changes.len(), 6, "{}", lines(&changes));
    assert_eq!(changes[0]["kind"], d::string("structure"));
    assert!(id_of(&changes[0]) > id_of(&changes[1]));
}

/// CONTRACT.md, `table-rewritten`: "A partitioned table has this entry when
/// any of its partitions was written anew (its rows' changes are recorded
/// under its name), and so has that partition"; `partition-detached`: "a
/// partition was detached from it, or dropped"; `partition-attached`: "one
/// the migration makes with `CREATE TABLE ... PARTITION OF` brings no rows
/// and has no entry". One Version, three migrations:
///
/// - `0004_cluster_partition`: `CLUSTER measures_2026`: `table-rewritten`
///   on `measures` and on `measures_2026`;
/// - `0005_new_partition`: `CREATE TABLE measures_2028 PARTITION OF
///   measures`: no entry;
/// - `0006_drop_partition`: `DROP TABLE measures_2026`:
///   `partition-detached` of `public.measures_2026` on `measures`, and none
///   in the partition's own history.
#[test]
fn partitions_rewritten_made_and_dropped() {
    let mut s = Structure::start();
    let last = last_history_id(&s.db);
    s.send_ok(&[
        Migration::new(
            "0004_cluster_partition",
            &[r#"CLUSTER "measures_2026" USING "measures_2026_pkey";"#],
        ),
        Migration::new(
            "0005_new_partition",
            &[r#"CREATE TABLE "measures_2028" PARTITION OF "measures" FOR VALUES FROM ('2028-01-01') TO ('2029-01-01');"#],
        ),
        Migration::new(
            "0006_drop_partition",
            &[r#"DROP TABLE "measures_2026";"#],
        ),
    ]);
    let entries = structure_entries(&s.db, last);
    let of = |migration: &str| -> Vec<Value> {
        entries
            .iter()
            .filter(|r| r["migration"] == migration)
            .cloned()
            .collect()
    };
    assert_structure_rows(
        &of("0004_cluster_partition"),
        &[
            ("measures", json!({ "change": "table-rewritten" })),
            ("measures_2026", json!({ "change": "table-rewritten" })),
        ],
        "0004_cluster_partition",
        "a partition written anew: table-rewritten on the partitioned table and on the partition",
    );
    assert_structure_rows(
        &of("0005_new_partition"),
        &[],
        "0005_new_partition",
        "a partition made with CREATE TABLE ... PARTITION OF: no entry",
    );
    assert_structure_rows(
        &of("0006_drop_partition"),
        &[(
            "measures",
            json!({ "change": "partition-detached", "partition": "public.measures_2026" }),
        )],
        "0006_drop_partition",
        "a partition dropped: partition-detached on the partitioned table, nothing on the partition",
    );
    assert_eq!(
        entries.len(),
        3,
        "expected these three entries and no other: {}",
        lines(&entries)
    );
}

/// CONTRACT.md, `enum-value-renamed`: "on every table with a column of that
/// type, of an array of it, or of a domain over either". A later Version
/// makes the domains `mood_d` over `mood` and `moods_d` over `mood[]`, and
/// `dom_moods (id, x mood_d, n text, y moods_d)` with a row; the next one
/// renames a value of `mood`: `dom_moods` has the entry, `columns` `["x",
/// "y"]`.
#[test]
fn an_enum_value_renamed_is_an_entry_on_a_table_with_a_domain_over_it() {
    let mut s = Structure::start();
    s.send_ok(&[Migration::new(
        "0004_domains",
        &[
            r#"CREATE DOMAIN "mood_d" AS "mood";"#,
            r#"CREATE DOMAIN "moods_d" AS "mood"[];"#,
            r#"CREATE TABLE "dom_moods" (
	"id" integer PRIMARY KEY NOT NULL,
	"x" "mood_d",
	"n" text,
	"y" "moods_d"
);"#,
        ],
    )]);
    let (_, answer) = s.own.one(
        &User::new(WRITER),
        APP,
        &own_execute(
            "user",
            "insert into dom_moods (id, x, n, y) values (1, 'sad', 'n', '{sad,happy}')",
        ),
    );
    assert_ok(&answer, "the harness's own check: a row of dom_moods");
    let last = last_history_id(&s.db);
    let migration = "0005_rename_mood";
    s.send_ok(&[Migration::new(
        migration,
        &[r#"ALTER TYPE "public"."mood" RENAME VALUE 'sad' TO 'blue';"#],
    )]);
    let renamed = |columns: &[&str]| json!({ "change": "enum-value-renamed", "type": "public.mood", "from": "sad", "to": "blue", "columns": columns });
    assert_structure_rows(
        &history_after(&s.db, last),
        &[
            ("moods", renamed(&["mood"])),
            ("mood_lists", renamed(&["moods"])),
            ("two_moods", renamed(&["a", "b"])),
            ("dom_moods", renamed(&["x", "y"])),
        ],
        migration,
        "an enum's value renamed: also on a table whose columns are of a domain over it and over an array of it",
    );
}

/// CONTRACT.md: "A change of structure that changes no data has no entry:
/// ... a table made to inherit from another (`INHERIT`: each table's rows
/// stay its own)". `plain (id, a, b)` made to inherit from `renamed_t (id,
/// a)`: no entry, and each table's history read as before.
#[test]
fn a_table_made_to_inherit_from_another_has_no_entry() {
    let mut s = Structure::start();
    let last = last_history_id(&s.db);
    s.send_ok(&[Migration::new(
        "0004_inherit",
        &[r#"ALTER TABLE "plain" INHERIT "renamed_t";"#],
    )]);
    let rows = history_after(&s.db, last);
    assert!(
        rows.is_empty(),
        "a table made to inherit from another: no entry; got\n{}",
        lines(&rows)
    );
    for (table, count) in [("plain", 2), ("renamed_t", 2)] {
        let changes = changes_of(&s.read("user", table, &json!({ "key": { "id": 1 } })));
        assert!(
            changes.len() == count && changes.iter().all(|c| c["kind"] != "structure"),
            "history of {table} {{ id: 1 }}: its own two changes, no structure entry; got\n{}",
            lines(&changes)
        );
    }
}
