//! What something else changed (CONTRACT.md, "The platform's part of an own
//! database is out of a migration's reach: What a migration leaves behind:
//! What something else changed").
//!
//! The harness's superuser stands for what is not a transaction of the
//! host's. Where it stands for code that ran with the migration role's
//! rights it acts as that role (`SET ROLE <p>_app_<n>_owner`), so that what
//! it makes is the owner's and the capture can be put on it; one variant
//! makes a table as the superuser itself, which the platform may not
//! capture. The variants: the capture of `things` switched off, a row
//! changed, switched on again; left disabled; dropped; a table made; a
//! column's type changed; a table made by the superuser.
//!
//! The platform compares when it first uses the own database after it
//! starts, when a Version is sent (before its migrations), and with a write
//! once the last comparison is 30 s old. So:
//!
//! - (a) changed while the program is stopped: at the first use after the
//!   start, the Operation log has one `capture-check` entry with the fields
//!   the contract gives, the program's log a `capture-changed-outside`
//!   record naming the table, the capture is whole again, the next write
//!   runs and is recorded; a restart and more writes add no second entry;
//! - (b) changed while it runs, then a Version is sent: the same, and the
//!   Version is applied as it would have been (refused `capture-missing`
//!   while a table the platform may not capture is there);
//! - (c) changed while it runs, then writes: once the last comparison is
//!   30 s old, within the next two writes;
//! - ordinary maintenance by a superuser, then a restart or a Version: no
//!   entry, and writes go on.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::cell::Cell;
use std::time::{Duration, Instant};

use blackbox::User;
use blackbox::changed_outside::{
    assert_capture_check_entry, capture_checks, capture_checks_eventually, capture_shape,
};
use blackbox::history::{history_after, last_history_id};
use blackbox::out_of_reach::{
    Answers, ON_DELETE, ON_INSERT, ON_UPDATE, WRITER, assert_answer, base_and, capture_triggers,
    install_seeded, refused, start,
};
use blackbox::own_database::{Migration, Own, OwnDatabase, ident, own_execute};
use blackbox::scenario::lines;
use blackbox::vm::Vm;
use serde_json::Value;

const CAPTURE_MISSING: Answers = &[refused("capture-missing")];

/// What the harness does to an own database outside the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Change {
    /// As the owner: the capture of `things` disabled, a row changed, the
    /// capture enabled again.
    OffChangeOn,
    /// As the owner: the update capture of `things` left disabled.
    LeftDisabled,
    /// As the owner: the update capture of `things` dropped.
    Dropped,
    /// As the owner: a table `public.made_outside`.
    TableMade,
    /// As the owner: `things.size` made `bigint`.
    ColumnType,
    /// As the superuser itself: a table `public.superuser_made`.
    SuperuserTable,
    /// Ordinary maintenance by the superuser: no change.
    Maintenance,
}

impl Change {
    const ALL: [Change; 7] = [
        Change::OffChangeOn,
        Change::LeftDisabled,
        Change::Dropped,
        Change::TableMade,
        Change::ColumnType,
        Change::SuperuserTable,
        Change::Maintenance,
    ];

    /// The tables the record must name.
    fn tables(self) -> &'static [&'static str] {
        match self {
            Change::OffChangeOn | Change::LeftDisabled | Change::Dropped | Change::ColumnType => {
                &["public.things"]
            }
            Change::TableMade => &["public.made_outside"],
            Change::SuperuserTable => &["public.superuser_made"],
            Change::Maintenance => &[],
        }
    }

    fn uncaptured(self) -> &'static [&'static str] {
        match self {
            Change::SuperuserTable => &["public.superuser_made"],
            _ => &[],
        }
    }

    fn is_change(self) -> bool {
        self != Change::Maintenance
    }

    /// Does it.
    fn make(self, db: &OwnDatabase) {
        let names = |event: i64| -> Vec<String> {
            capture_triggers(db)
                .into_iter()
                .filter(|t| t.table == "things" && t.tgtype & event != 0)
                .map(|t| ident(&t.name))
                .collect()
        };
        let as_owner = |statements: &[String]| {
            let sql = db.superuser();
            sql.batch(&format!("SET ROLE {}", ident(&db.owner)));
            for s in statements {
                sql.batch(s);
            }
        };
        match self {
            Change::OffChangeOn => {
                let all: Vec<String> = capture_triggers(db)
                    .into_iter()
                    .filter(|t| t.table == "things")
                    .map(|t| ident(&t.name))
                    .collect();
                let mut statements: Vec<String> = all
                    .iter()
                    .map(|n| format!("ALTER TABLE public.things DISABLE TRIGGER {n}"))
                    .collect();
                statements.push(
                    "UPDATE public.things SET title = 'changed outside, unseen' WHERE id = 101"
                        .to_owned(),
                );
                statements.extend(
                    all.iter()
                        .map(|n| format!("ALTER TABLE public.things ENABLE TRIGGER {n}")),
                );
                as_owner(&statements);
            }
            Change::LeftDisabled => as_owner(
                &names(ON_UPDATE)
                    .iter()
                    .map(|n| format!("ALTER TABLE public.things DISABLE TRIGGER {n}"))
                    .collect::<Vec<_>>(),
            ),
            Change::Dropped => as_owner(
                &names(ON_UPDATE)
                    .iter()
                    .map(|n| format!("DROP TRIGGER {n} ON public.things"))
                    .collect::<Vec<_>>(),
            ),
            Change::TableMade => as_owner(&[
                "CREATE TABLE public.made_outside (id integer PRIMARY KEY, v text)".to_owned(),
            ]),
            Change::ColumnType => {
                as_owner(&["ALTER TABLE public.things ALTER COLUMN size TYPE bigint".to_owned()])
            }
            Change::SuperuserTable => {
                db.superuser()
                    .batch("CREATE TABLE public.superuser_made (id integer PRIMARY KEY)");
            }
            Change::Maintenance => {
                let sql = db.superuser();
                for statement in [
                    "VACUUM (FREEZE, ANALYZE)",
                    "VACUUM FULL public.things",
                    "VACUUM FULL pg_catalog.pg_trigger",
                    "VACUUM FULL pg_catalog.pg_attribute",
                    "VACUUM FULL pg_catalog.pg_class",
                    "REINDEX TABLE public.things",
                    "CLUSTER public.things USING things_pkey",
                    "ANALYZE",
                ] {
                    sql.batch(statement);
                }
            }
        }
    }
}

/// The `capture-changed-outside` records of `application` in the program's
/// log so far.
fn records(own: &Own, application: &str) -> Vec<Value> {
    own.vm
        .records()
        .into_iter()
        .filter(|r| r["event"] == "capture-changed-outside" && r["application"] == application)
        .collect()
}

fn names_of(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Fails unless the record names these tables and these uncaptured ones.
fn assert_record(own: &Own, application: &str, change: Change) {
    let record = own.vm.wait_for_record(
        &format!("a `capture-changed-outside` record for {application:?}"),
        |r| r["event"] == "capture-changed-outside" && r["application"] == application,
    );
    let tables = names_of(&record["tables"]);
    for table in change.tables() {
        assert!(
            tables.iter().any(|t| t == table),
            "expected the record's `tables` to name {table}, got {record}"
        );
    }
    assert!(
        record["uncaptured"].is_array(),
        "expected the record's `uncaptured` to be an array, got {record}"
    );
    let mut uncaptured = names_of(&record["uncaptured"]);
    uncaptured.sort();
    assert_eq!(
        uncaptured,
        change.uncaptured(),
        "expected the record's `uncaptured`, got {record}"
    );
}

/// Fails unless the capture is whole again: as `shape` was before, and a
/// table made outside the host captured on insert, update and delete.
fn assert_capture_whole(db: &OwnDatabase, shape: &[String], change: Change) {
    let now = capture_shape(db);
    if change == Change::TableMade {
        let made: Vec<_> = capture_triggers(db)
            .into_iter()
            .filter(|t| t.table == "made_outside")
            .collect();
        for event in [ON_INSERT, ON_UPDATE, ON_DELETE] {
            assert!(
                made.iter().any(|t| t.tgtype & event != 0),
                "expected the table made outside the host given its capture (event {event}), its capture triggers are {made:#?}"
            );
        }
    }
    let kept: Vec<&String> = now
        .iter()
        // `next` is the table the test's own Version makes in (b): its
        // capture is new, not a difference.
        .filter(|l| !l.starts_with("made_outside ") && !l.starts_with("next "))
        .collect();
    let before: Vec<&String> = shape.iter().collect();
    assert_eq!(
        kept, before,
        "expected the capture whole again: every capture trigger there and enabled as before"
    );
}

/// A counter of the ids the writes use.
struct Writes {
    next: Cell<i64>,
}

impl Writes {
    fn new() -> Self {
        Self {
            next: Cell::new(5000),
        }
    }

    /// One write through the Application.
    fn write(&self, own: &Own, application: &str) -> (i64, Value) {
        let id = self.next.get() + 1;
        self.next.set(id);
        let (_, answer) = own.one(
            &User::new(WRITER),
            application,
            &own_execute(
                "user",
                &format!("insert into things (id, title) values ({id}, 'a write')"),
            ),
        );
        (id, answer)
    }

    /// A write that must run and be recorded.
    fn recorded(&self, own: &Own, db: &OwnDatabase, application: &str, context: &str) {
        let last = last_history_id(db);
        let (id, answer) = self.write(own, application);
        assert!(
            answer["ok"].is_object(),
            "{context}: expected the write to run, got {answer}"
        );
        let rows = history_after(db, last);
        assert!(
            rows.iter()
                .any(|r| r["table_name"] == "things" && r["new"]["id"] == id),
            "{context}: expected the write recorded, got:\n{}",
            lines(&rows)
        );
    }
}

fn application_of(change: Change, prefix: &str) -> String {
    format!("{prefix}-{change:?}").to_lowercase()
}

/// (a): changed while the program is stopped.
#[test]
fn a_change_made_while_the_program_was_stopped_is_found_at_the_first_use_and_said_once() {
    let own = start();
    let writes = Writes::new();
    let installed: Vec<(Change, String, OwnDatabase, Vec<String>)> = Change::ALL
        .iter()
        .map(|&change| {
            let application = application_of(change, "stopped");
            let db = install_seeded(&own, &application);
            let shape = capture_shape(&db);
            (change, application, db, shape)
        })
        .collect();
    let Own { vm } = own;
    let platform = vm.platform().clone();
    let options = vm.options().clone();
    drop(vm);
    for (change, _, db, _) in &installed {
        change.make(db);
    }
    let own = Own {
        vm: Vm::start(&options.platform(&platform)),
    };
    let mut wrong = Vec::new();
    for (change, application, db, shape) in &installed {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // The first use: a write, which may itself be refused.
            let _ = writes.write(&own, application);
            let entries = capture_checks_eventually(&own, application);
            if change.is_change() {
                assert_eq!(
                    entries.len(),
                    1,
                    "expected one capture-check entry at the first use after the start, got:\n{}",
                    lines(&entries)
                );
                assert_capture_check_entry(&entries[0], application);
                assert_record(&own, application, *change);
                if *change != Change::SuperuserTable {
                    assert_capture_whole(db, shape, *change);
                }
            } else {
                assert!(
                    entries.is_empty(),
                    "expected no capture-check entry after ordinary maintenance, got:\n{}",
                    lines(&entries)
                );
            }
            writes.recorded(&own, db, application, "the write after the first use");
        }));
        if let Err(payload) = outcome {
            wrong.push(format!(
                "--- {change:?} ({application}) ---\n{}",
                blackbox::out_of_reach::panic_text(payload.as_ref())
                    .chars()
                    .take(2000)
                    .collect::<String>()
            ));
        }
    }
    // Said once: not again at the next writes, nor after a restart.
    let Own { vm } = own;
    let own = Own { vm: vm.restart() };
    for (change, application, _, _) in &installed {
        let _ = writes.write(&own, application);
        let _ = writes.write(&own, application);
        let expected = usize::from(change.is_change());
        let entries = capture_checks(&own, application);
        if entries.len() != expected {
            wrong.push(format!(
                "--- {change:?} ({application}) after a restart ---\nexpected {expected} capture-check entr(y/ies) in all, got:\n{}",
                lines(&entries)
            ));
        }
        if !records(&own, application).is_empty() {
            wrong.push(format!(
                "--- {change:?} ({application}) after a restart ---\nexpected no second `capture-changed-outside` record, got {:?}",
                records(&own, application)
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "what something else changed while the program was stopped:\n{}",
        wrong.join("\n")
    );
}

/// (b): changed while it runs, then a Version is sent.
#[test]
fn a_change_found_when_a_version_is_sent_is_said_and_the_version_applies_as_it_would_have() {
    let own = start();
    let writes = Writes::new();
    let mut wrong = Vec::new();
    for change in Change::ALL {
        let application = application_of(change, "sent");
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let db = install_seeded(&own, &application);
            let shape = capture_shape(&db);
            change.make(&db);
            let list = base_and(&[Migration::new(
                "0004_next",
                &[r#"CREATE TABLE "next" ("id" integer PRIMARY KEY NOT NULL, "v" text);"#],
            )]);
            let sent = own.send(&application, &list);
            let entries = capture_checks_eventually(&own, &application);
            if change == Change::SuperuserTable {
                assert_answer(
                    &sent,
                    CAPTURE_MISSING,
                    blackbox::out_of_reach::ANY_MIGRATION,
                    Some("superuser_made"),
                );
            } else {
                assert_eq!(
                    sent.status,
                    201,
                    "expected the Version applied as it would have been, got\n{}",
                    sent.describe()
                );
            }
            if change.is_change() {
                assert_eq!(
                    entries.len(),
                    1,
                    "expected one capture-check entry when the Version was sent, got:\n{}",
                    lines(&entries)
                );
                assert_capture_check_entry(&entries[0], &application);
                assert_record(&own, &application, change);
                if change != Change::SuperuserTable {
                    assert_capture_whole(&db, &shape, change);
                }
            } else {
                assert!(
                    entries.is_empty(),
                    "expected no capture-check entry after ordinary maintenance and a Version, got:\n{}",
                    lines(&entries)
                );
            }
            // The next write runs and is recorded; a superuser-made table
            // dropped, a Version applies again.
            let _ = writes.write(&own, &application);
            writes.recorded(&own, &db, &application, "a write after the Version");
            if change == Change::SuperuserTable {
                // CONTRACT.md: "a Version with none to apply is accepted".
                let none_new = own.send(&application, &blackbox::out_of_reach::base());
                assert_eq!(
                    none_new.status,
                    201,
                    "expected a Version with no migrations to apply accepted while the superuser's table is there, got\n{}",
                    none_new.describe()
                );
                db.superuser().batch("DROP TABLE public.superuser_made");
                let sent = own.send(&application, &list);
                assert_eq!(
                    sent.status,
                    201,
                    "expected a Version applied once the superuser's table is gone, got\n{}",
                    sent.describe()
                );
            }
        }));
        if let Err(payload) = outcome {
            wrong.push(format!(
                "--- {change:?} ({application}) ---\n{}",
                blackbox::out_of_reach::panic_text(payload.as_ref())
                    .chars()
                    .take(2000)
                    .collect::<String>()
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "what something else changed, found when a Version was sent:\n{}",
        wrong.join("\n")
    );
}

/// (c): changed while it runs, then writes. The last comparison is the
/// first Version; 31 s later, within the next two writes.
#[test]
fn a_change_made_while_the_program_runs_is_found_by_a_write_once_the_last_comparison_is_30_seconds_old()
 {
    let own = start();
    let writes = Writes::new();
    let application = "running";
    let db = install_seeded(&own, application);
    let compared = Instant::now();
    let shape = capture_shape(&db);
    Change::LeftDisabled.make(&db);
    let wait = Duration::from_secs(31).saturating_sub(compared.elapsed());
    std::thread::sleep(wait);
    let _ = writes.write(&own, application);
    let _ = writes.write(&own, application);
    let entries = capture_checks_eventually(&own, application);
    assert_eq!(
        entries.len(),
        1,
        "expected one capture-check entry within two writes once the last comparison was 30 s old, got:\n{}",
        lines(&entries)
    );
    assert_capture_check_entry(&entries[0], application);
    assert_record(&own, application, Change::LeftDisabled);
    assert_capture_whole(&db, &shape, Change::LeftDisabled);
    writes.recorded(
        &own,
        &db,
        application,
        "a write after the capture was put back",
    );
}
