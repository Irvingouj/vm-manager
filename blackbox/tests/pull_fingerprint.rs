//! The fingerprint and the sameness of the definition (CONTRACT.md,
//! "Pulling a Resource's structure: The fingerprint", "What is in the
//! structure", "The definition"; ticket "Pull a Resource's structure as a
//! typed Drizzle definition": the same structure always gives the same
//! fingerprint; a changed structure gives a different one).
//!
//! The same structure pulled twice, with the other login, and from a
//! second database built by the same statements in another order (so with
//! other object ids) gives the same fingerprint and the same text. Each
//! kind of change the contract lists gives another fingerprint, and
//! undoing it (building the probe schema again, with new object ids) gives
//! the first one back. What is not in the structure changes neither.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::scenario::{READERS, Scenario};
use blackbox::structure::{
    IN_ANOTHER_ORDER, PROBE, PROBE_AGAIN, STOCK, Stock, assert_fingerprint, body_with_structure,
    build_stock, structure_login, without_second_line,
};
use serde_json::Value;

fn rita() -> User {
    User::new("rita").in_groups(&[READERS])
}

/// (fingerprint, schema_ts) of a pull's answer.
fn both(answer: &Value) -> (String, String) {
    assert_fingerprint(&answer["fingerprint"], "a pull's fingerprint");
    (
        answer["fingerprint"].as_str().unwrap().to_owned(),
        answer["schema_ts"].as_str().unwrap().to_owned(),
    )
}

#[test]
fn the_same_structure_gives_the_same_fingerprint_and_text_whichever_login_and_in_whatever_order_it_was_made()
 {
    let stock = Stock::start();
    stock.register(STOCK);
    stock.register_without_structure("stock_by_reader");
    // A second database, built by the same statements in another order.
    let other = blackbox::shared_database::SharedDatabase::create();
    let other_structure = build_stock(&other, &IN_ANOTHER_ORDER);
    stock.scenario.register_body(
        "stock_reordered",
        &body_with_structure("stock_reordered", &other, &other_structure),
    );
    for name in [STOCK, "stock_by_reader", "stock_reordered"] {
        stock.scenario.grant(name, READERS, "structure");
    }
    let oids = |db: &blackbox::shared_database::SharedDatabase| {
        db.superuser()
            .value("SELECT string_agg(oid::text, ',' ORDER BY relname) FROM pg_class WHERE relname IN ('sites', 'items', 'accounts')")
    };
    assert_ne!(
        oids(stock.db()),
        oids(&other),
        "the harness's own check: the second database's tables must have other object ids"
    );

    let (first, first_ts) = both(&stock.pull_ok(&rita(), STOCK));
    let (again, again_ts) = both(&stock.pull_ok(&rita(), STOCK));
    assert_eq!(first, again, "expected the same fingerprint pulled twice");
    assert_eq!(
        first_ts, again_ts,
        "expected the same text, byte for byte, pulled twice"
    );
    let (by_reader, by_reader_ts) = both(&stock.pull_ok(&rita(), "stock_by_reader"));
    assert_eq!(
        first, by_reader,
        "expected the same fingerprint pulled with the read login as with the structure login"
    );
    assert_eq!(
        without_second_line(&first_ts),
        without_second_line(&by_reader_ts),
        "expected the same text, but for the line that names the Resource, pulled with the other login"
    );
    let (reordered, reordered_ts) = both(&stock.pull_ok(&rita(), "stock_reordered"));
    assert_eq!(
        first, reordered,
        "expected the same fingerprint for the same structure made in another order, with other object ids"
    );
    assert_eq!(
        without_second_line(&first_ts),
        without_second_line(&reordered_ts),
        "expected the same text, byte for byte but for the line that names the Resource, for the same structure made in another order"
    );
}

/// Each kind of change CONTRACT.md lists, made to the probe schema.
const CHANGES: [(&str, &str); 37] = [
    ("a table added", "CREATE TABLE probe.extra (x integer)"),
    ("a table dropped", "DROP TABLE probe.loose"),
    (
        "a table renamed",
        "ALTER TABLE probe.loose RENAME TO loose_renamed",
    ),
    (
        "a table moved to another schema",
        "ALTER TABLE probe.loose SET SCHEMA public",
    ),
    ("a view added", "CREATE VIEW probe.more AS SELECT 1 AS one"),
    ("a view dropped", "DROP VIEW probe.child_notes"),
    (
        "a view renamed",
        "ALTER VIEW probe.child_notes RENAME TO notes_of_children",
    ),
    (
        "a view's columns changed",
        "CREATE OR REPLACE VIEW probe.child_notes AS SELECT id, note, n FROM probe.child",
    ),
    (
        "a materialized view added",
        "CREATE MATERIALIZED VIEW probe.counted AS SELECT count(*) AS n FROM probe.child",
    ),
    (
        "a column added",
        "ALTER TABLE probe.loose ADD COLUMN c integer",
    ),
    ("a column dropped", "ALTER TABLE probe.loose DROP COLUMN b"),
    (
        "a column renamed",
        "ALTER TABLE probe.loose RENAME COLUMN b TO bee",
    ),
    (
        "an enum label added",
        "ALTER TYPE probe.mood ADD VALUE 'tired'",
    ),
    (
        "an enum label added before another",
        "ALTER TYPE probe.mood ADD VALUE 'sleepy' BEFORE 'calm'",
    ),
    (
        "an enum label renamed",
        "ALTER TYPE probe.mood RENAME VALUE 'busy' TO 'hectic'",
    ),
    ("an enum renamed", "ALTER TYPE probe.mood RENAME TO feeling"),
    (
        "a primary key added",
        "ALTER TABLE probe.loose ADD PRIMARY KEY (a)",
    ),
    (
        "a primary key dropped",
        "ALTER TABLE probe.keyed DROP CONSTRAINT keyed_pkey",
    ),
    (
        "a primary key renamed",
        "ALTER TABLE probe.keyed RENAME CONSTRAINT keyed_pkey TO keyed_key_renamed",
    ),
    (
        "a unique constraint added",
        "ALTER TABLE probe.loose ADD CONSTRAINT loose_b_key UNIQUE (b)",
    ),
    (
        "a unique constraint dropped",
        "ALTER TABLE probe.parent DROP CONSTRAINT parent_code_key",
    ),
    (
        "a unique constraint renamed",
        "ALTER TABLE probe.parent RENAME CONSTRAINT parent_code_key TO parent_code_unique",
    ),
    (
        "a foreign key added",
        "ALTER TABLE probe.loose ADD CONSTRAINT loose_a_fkey FOREIGN KEY (a) REFERENCES probe.parent (id)",
    ),
    (
        "a foreign key dropped",
        "ALTER TABLE probe.child DROP CONSTRAINT child_parent_fkey",
    ),
    (
        "a foreign key renamed",
        "ALTER TABLE probe.child RENAME CONSTRAINT child_parent_fkey TO child_parent_fk",
    ),
    (
        "a foreign key's ON DELETE changed",
        "ALTER TABLE probe.child DROP CONSTRAINT child_parent_fkey, ADD CONSTRAINT child_parent_fkey FOREIGN KEY (parent_id) REFERENCES probe.parent (id) ON DELETE RESTRICT ON UPDATE NO ACTION",
    ),
    (
        "a foreign key's ON UPDATE changed",
        "ALTER TABLE probe.child DROP CONSTRAINT child_parent_fkey, ADD CONSTRAINT child_parent_fkey FOREIGN KEY (parent_id) REFERENCES probe.parent (id) ON DELETE CASCADE ON UPDATE CASCADE",
    ),
    (
        "a column's type changed",
        "ALTER TABLE probe.loose ALTER COLUMN b TYPE varchar(20)",
    ),
    (
        "a column made an array",
        "ALTER TABLE probe.loose ALTER COLUMN b TYPE text[] USING ARRAY[b]",
    ),
    (
        "a column made NOT NULL",
        "ALTER TABLE probe.loose ALTER COLUMN b SET NOT NULL",
    ),
    (
        "a column made nullable",
        "ALTER TABLE probe.child ALTER COLUMN parent_id DROP NOT NULL",
    ),
    (
        "a default added",
        "ALTER TABLE probe.loose ALTER COLUMN b SET DEFAULT 'x'",
    ),
    (
        "a default changed",
        "ALTER TABLE probe.child ALTER COLUMN n SET DEFAULT 2",
    ),
    (
        "an identity added",
        "ALTER TABLE probe.loose ALTER COLUMN a ADD GENERATED ALWAYS AS IDENTITY",
    ),
    (
        "an identity changed from by default to always",
        "ALTER TABLE probe.child ALTER COLUMN ident SET GENERATED ALWAYS",
    ),
    (
        "a generation dropped",
        "ALTER TABLE probe.child ALTER COLUMN twice DROP EXPRESSION",
    ),
    (
        "a column moved",
        "DROP TABLE probe.loose; CREATE TABLE probe.loose (b text, a integer NOT NULL)",
    ),
];

/// More changes of the same kinds, also listed: kept apart only to keep
/// each table short.
const MORE_CHANGES: [(&str, &str); 4] = [
    (
        "a default dropped",
        "ALTER TABLE probe.child ALTER COLUMN n DROP DEFAULT",
    ),
    (
        "an identity dropped",
        "ALTER TABLE probe.child ALTER COLUMN ident DROP IDENTITY",
    ),
    (
        "a generation's expression changed",
        "ALTER TABLE probe.child DROP COLUMN twice, ADD COLUMN twice integer GENERATED ALWAYS AS (n * 3) STORED",
    ),
    (
        "an enum used by a column added",
        "CREATE TYPE probe.size AS ENUM ('s', 'm'); ALTER TABLE probe.loose ADD COLUMN size probe.size",
    ),
];

#[test]
fn each_kind_of_change_gives_another_fingerprint_and_undoing_it_gives_the_first_back() {
    let scenario = Scenario::start();
    scenario.db.superuser().batch(PROBE);
    let login = structure_login(&scenario.db);
    scenario.register_body(
        "probed",
        &body_with_structure("probed", &scenario.db, &login),
    );
    scenario.grant("probed", READERS, "structure");
    let api = scenario.vm.api_as(&rita());
    let pulled = || {
        let answer = blackbox::structure::pull(&api, "probed");
        if answer.status != 200 {
            scenario.vm.fail(&format!(
                "expected `200` pulling, got\n{}",
                answer.describe()
            ));
        }
        both(&answer.json())
    };
    let (base, base_ts) = pulled();
    let mut wrong = Vec::new();
    for (what, change) in CHANGES.iter().chain(MORE_CHANGES.iter()) {
        scenario.db.superuser().batch(change);
        let (changed, _) = pulled();
        if changed == base {
            wrong.push(format!(
                "  {what} ({change}): the fingerprint stayed {base}"
            ));
        }
        scenario.db.superuser().batch(PROBE_AGAIN);
        scenario.db.superuser().batch(PROBE);
        let (undone, undone_ts) = pulled();
        if undone != base || undone_ts != base_ts {
            wrong.push(format!(
                "  after undoing {what} (the probe schema built again, with new object ids): expected the first fingerprint and text back, got {undone}"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "expected each change of the structure to give another fingerprint:\n{}",
        wrong.join("\n")
    );
}

/// What CONTRACT.md says is not in the structure, applied one after
/// another. `%READER%` is the read login.
const NOT_IN_THE_STRUCTURE: [(&str, &str); 16] = [
    (
        "an index",
        "CREATE INDEX child_note_idx ON probe.child (note)",
    ),
    (
        "a unique index that backs no constraint",
        "CREATE UNIQUE INDEX loose_b_idx ON probe.loose (b)",
    ),
    (
        "a check constraint",
        "ALTER TABLE probe.child ADD CONSTRAINT child_n_positive CHECK (n > 0)",
    ),
    (
        "a comment on a table",
        "COMMENT ON TABLE probe.child IS 'children of parents'",
    ),
    (
        "a comment on a column",
        "COMMENT ON COLUMN probe.child.note IS 'free text'",
    ),
    (
        "a privilege",
        "GRANT SELECT, INSERT ON probe.child TO %READER%; GRANT USAGE ON SCHEMA probe TO %READER%",
    ),
    (
        "a trigger and its function",
        "CREATE FUNCTION probe.touch() RETURNS trigger LANGUAGE plpgsql AS $f$ BEGIN RETURN NEW; END $f$; \
         CREATE TRIGGER child_touch BEFORE UPDATE ON probe.child FOR EACH ROW EXECUTE FUNCTION probe.touch()",
    ),
    (
        "a function",
        "CREATE FUNCTION probe.answer() RETURNS integer LANGUAGE sql AS $f$ SELECT 42 $f$",
    ),
    (
        "row security",
        "ALTER TABLE probe.child ENABLE ROW LEVEL SECURITY; CREATE POLICY child_mine ON probe.child USING (true)",
    ),
    ("a sequence", "CREATE SEQUENCE probe.counter"),
    ("an owner", "ALTER TABLE probe.loose OWNER TO %READER%"),
    (
        "storage",
        "ALTER TABLE probe.child SET (fillfactor = 70); ALTER TABLE probe.child ALTER COLUMN note SET STORAGE EXTERNAL",
    ),
    (
        "an enum no listed column uses",
        "CREATE TYPE probe.unused AS ENUM ('x', 'y')",
    ),
    (
        "rows",
        "INSERT INTO probe.parent VALUES (1, 'one'), (2, 'two'); INSERT INTO probe.child (id, parent_id, note) VALUES (1, 1, 'a'), (2, 2, 'b')",
    ),
    ("statistics", "ANALYZE probe.parent; ANALYZE probe.child"),
    (
        "a value of a sequence",
        "SELECT nextval('probe.counter'); SELECT nextval('probe.child_ident_seq')",
    ),
];

#[test]
fn what_is_not_in_the_structure_changes_neither_the_definition_nor_the_fingerprint() {
    let scenario = Scenario::start();
    scenario.db.superuser().batch(PROBE);
    let login = structure_login(&scenario.db);
    scenario.register_body(
        "probed",
        &body_with_structure("probed", &scenario.db, &login),
    );
    scenario.grant("probed", READERS, "structure");
    let api = scenario.vm.api_as(&rita());
    let pulled = || {
        let answer = blackbox::structure::pull(&api, "probed");
        if answer.status != 200 {
            scenario.vm.fail(&format!(
                "expected `200` pulling, got\n{}",
                answer.describe()
            ));
        }
        both(&answer.json())
    };
    let (base, base_ts) = pulled();
    let mut wrong = Vec::new();
    for (what, change) in NOT_IN_THE_STRUCTURE {
        scenario
            .db
            .superuser()
            .batch(&change.replace("%READER%", &scenario.db.reader.role));
        let (after, after_ts) = pulled();
        if after != base {
            wrong.push(format!("  {what}: the fingerprint changed to {after}"));
        }
        if after_ts != base_ts {
            wrong.push(format!("  {what}: the definition changed"));
        }
        for word in [
            "children of parents",
            "free text",
            "child_note_idx",
            "child_n_positive",
            "child_touch",
            "counter",
            "unused",
        ] {
            if after_ts.contains(word) && !base_ts.contains(word) {
                wrong.push(format!("  {what}: the definition holds {word:?}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "expected what is not in the structure to change neither the definition nor the fingerprint:\n{}",
        wrong.join("\n")
    );
}
