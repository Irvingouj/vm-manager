//! Run time is held to what a migration is held to (CONTRACT.md, "The
//! platform's part of an own database is out of a migration's reach: What
//! a migration leaves behind: Checked before every commit that could carry
//! a change"; research part F).
//!
//! Every Application here has `fixtures/transactions.js` as its bundle and
//! `left_behind::left_behind` among its first Version's migrations: code
//! that Postgres runs with the migration role's rights while it carries out
//! a foreign key's action (a `BEFORE DELETE` trigger, a `CHECK`, a stored
//! generated column, a default under `SET DEFAULT`), doing what the row
//! that sets it off says. The statement check lets only `SELECT`, `INSERT`,
//! `UPDATE` and `DELETE` through, so the attack is an honest-looking
//! `DELETE` or `UPDATE` whose cascade runs the owner's code.
//!
//! For each attack, on a fresh Application: the answer is `{ error: { kind:
//! "database", code: "VM004" } }` (from the `execute`, or from `tx.commit`);
//! nothing is kept (the whole own database as before, `out_of_reach::
//! untouched`, and the capture triggers' very catalog rows); the Operation
//! log entry of the `execute` or the `commit` is `failed` with `error`
//! `VM004`; after a failed commit the transaction is over; the next honest
//! write runs and is in `vm.history`.
//!
//! A `query` calling the same code fails with `25006` and changes nothing.
//! What reading and writing rows brings with it is not refused and is
//! recorded. A Version sent while writes are being made fails none of them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use blackbox::history::{history_after, last_history_id};
use blackbox::left_behind::{
    assert_vm004, begin_write, capture_catalog, first_version, inside, install, outside, send,
    steps,
};
use blackbox::out_of_reach::{differences, panic_text, server_version_num, start, untouched};
use blackbox::own_database::{Migration, Own};
use blackbox::scenario::lines;
use blackbox::transactions::{commit, execute_in, query_in};
use serde_json::Value;

/// Statements separated by `;;`, run by `obey`: switch the capture of
/// `parents` off, change a row, switch it on again.
const OFF_CHANGE_ON: &str = "SELECT public.each_capture('public.parents', 'ALTER TABLE public.parents DISABLE TRIGGER %I');;UPDATE public.parents SET name = 'changed unseen' WHERE id = 1;;SELECT public.each_capture('public.parents', 'ALTER TABLE public.parents ENABLE TRIGGER %I')";
const OFF_ON: &str = "SELECT public.each_capture('public.parents', 'ALTER TABLE public.parents DISABLE TRIGGER %I');;SELECT public.each_capture('public.parents', 'ALTER TABLE public.parents ENABLE TRIGGER %I')";
const DROP_CAPTURE: &str =
    "SELECT public.each_capture('public.parents', 'DROP TRIGGER %I ON public.parents')";
const RENAME_CAPTURE: &str = "SELECT public.each_capture('public.parents', 'ALTER TRIGGER %1$I ON public.parents RENAME TO %1$s_x')";
const TYPE_USING: &str = "ALTER TABLE public.things ALTER COLUMN size TYPE bigint USING size * 10";
const ADD_COLUMN: &str = "ALTER TABLE public.things ADD COLUMN sneaky text";
const DROP_COLUMN: &str = "ALTER TABLE public.things DROP COLUMN tags";
const CREATE_TABLE: &str = "CREATE TABLE public.sneaky (x integer)";
const DROP_TABLE: &str = "DROP TABLE public.keyless";
const RENAME_TABLE: &str = "ALTER TABLE public.keyless RENAME TO keyless_renamed";
const MOVE_TABLE: &str = "ALTER TABLE public.keyless SET SCHEMA inventory";
const DEFERRED_TRIGGER: &str = "CREATE CONSTRAINT TRIGGER late AFTER UPDATE ON public.parents DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION public.late_fn();;UPDATE public.parents SET name = name WHERE id = 1";
const DEFERRABLE_FK: &str = "ALTER TABLE public.children ADD CONSTRAINT late_fk FOREIGN KEY (parent_id) REFERENCES public.parents (id) DEFERRABLE INITIALLY DEFERRED";
const DETACH_ATTACH: &str = "ALTER TABLE public.measures DETACH PARTITION public.measures_high;;ALTER TABLE public.measures ATTACH PARTITION public.measures_high FOR VALUES FROM (100) TO (200)";
const DETACH: &str = "ALTER TABLE public.measures DETACH PARTITION public.measures_high";
const LOCK_SHARE: &str = "LOCK TABLE public.things IN SHARE MODE";
const ANALYZE: &str = "ANALYZE public.things";
const REFRESH: &str = "REFRESH MATERIALIZED VIEW public.mv";

/// An attack: honest steps that set it up (each must run), then the steps
/// of one request, one of whose answers must be `VM004`.
struct Attack {
    name: String,
    setup: Vec<Value>,
    attack: Vec<Value>,
    /// The step whose answer is `VM004`.
    failing: usize,
    /// The Operation log entry that must be `failed` with `VM004`:
    /// `execute` or `commit`.
    entry: &'static str,
    /// The last step uses the transaction after its failed commit and must
    /// be answered `no-transaction`.
    then_over: bool,
    /// The lowest `server_version_num` it can run on.
    since: u32,
}

/// Through the `BEFORE DELETE` trigger on `ca`: a row of `ca` whose action
/// is `action`, then an `execute` outside any transaction deleting its
/// parent.
fn through_a_cascade(name: &str, action: &str) -> Attack {
    Attack {
        name: format!("cascade-trigger/{name}"),
        setup: vec![outside(
            "execute",
            "insert into ca (id, p, action) values (1, 1, $1)",
            &[action],
        )],
        attack: vec![outside("execute", "delete from pa where id = 1", &[])],
        failing: 0,
        entry: "execute",
        then_over: false,
        since: 0,
    }
}

/// The same in a transaction: begin, delete, commit, then a statement on
/// the transaction that is over.
fn through_a_cascade_in_a_transaction(name: &str, action: &str) -> Attack {
    Attack {
        name: format!("cascade-trigger-in-a-transaction/{name}"),
        setup: vec![outside(
            "execute",
            "insert into ca (id, p, action) values (2, 2, $1)",
            &[action],
        )],
        attack: vec![
            begin_write("t", None),
            inside("t", "delete from pa where id = 2", &[]),
            commit("t"),
            execute_in("t", "select 1"),
        ],
        failing: 2,
        entry: "commit",
        then_over: true,
        since: 0,
    }
}

fn run_attacks(own: &Own, prefix: &str, attacks: Vec<Attack>) {
    let version = server_version_num();
    let mut failures = Vec::new();
    let total = attacks.len();
    for (i, attack) in attacks.into_iter().enumerate() {
        if version < attack.since {
            eprintln!(
                "SKIPPED {}: needs server_version_num {} or later",
                attack.name, attack.since
            );
            continue;
        }
        let application = format!("{prefix}-{i}");
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let db = install(own, &application);
            if !attack.setup.is_empty() {
                let (_, answers) = steps(own, &application, &attack.setup);
                for answer in &answers {
                    assert!(
                        answer["ok"].is_object(),
                        "the harness's own setup: expected each step to run, got {answer}"
                    );
                }
            }
            let before = untouched(&db);
            let catalog = capture_catalog(&db);
            let (response, answers) = steps(own, &application, &attack.attack);
            let answer = answers.get(attack.failing).cloned().unwrap_or(Value::Null);
            assert_vm004(&answer, "the attack");
            if attack.then_over {
                let last = answers.last().cloned().unwrap_or(Value::Null);
                assert_eq!(
                    last["error"]["kind"], "no-transaction",
                    "expected the transaction over after its failed commit, got {last}"
                );
            }
            let after = untouched(&db);
            assert!(
                after == before,
                "expected nothing kept, but these differ:\n{}",
                differences(&before, &after)
            );
            assert_eq!(
                capture_catalog(&db),
                catalog,
                "expected the capture triggers' very catalog rows (object id, row version, enabling) as before"
            );
            let entries = own.entries_of(&response);
            let of_entry: Vec<&Value> = entries
                .iter()
                .filter(|e| e["operation"] == attack.entry)
                .collect();
            assert!(
                of_entry.len() == 1
                    && of_entry[0]["outcome"] == "failed"
                    && of_entry[0]["error"] == "VM004",
                "expected one Operation log entry of the {} `failed` with `error` `VM004`, got:\n{}",
                attack.entry,
                lines(&entries)
            );
            let last = last_history_id(&db);
            let (_, honest) = steps(
                own,
                &application,
                &[outside(
                    "execute",
                    "insert into things (id, title) values (900, 'honest after')",
                    &[],
                )],
            );
            assert!(
                honest[0]["ok"].is_object(),
                "expected the next honest write to run, got {}",
                honest[0]
            );
            let rows = history_after(&db, last);
            assert!(
                rows.iter()
                    .any(|r| r["table_name"] == "things" && r["new"]["id"] == 900),
                "expected the next honest write in vm.history, got:\n{}",
                lines(&rows)
            );
        }));
        if let Err(payload) = outcome {
            let why: String = panic_text(payload.as_ref()).chars().take(2500).collect();
            failures.push(format!(
                "--- {} (Application {application}) ---\n{why}",
                attack.name
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {total} attacks at run time were not answered VM004 with nothing kept:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The owner's code, through the cascade trigger, does each change the
/// contract lists (and the locks it adds).
#[test]
fn code_a_migration_left_cannot_change_the_structure_or_the_capture_through_a_cascade() {
    let own = start();
    let attacks = vec![
        through_a_cascade("disable-change-enable", OFF_CHANGE_ON),
        through_a_cascade("disable-enable-alone", OFF_ON),
        through_a_cascade("drop-a-capture-trigger", DROP_CAPTURE),
        through_a_cascade("rename-a-capture-trigger", RENAME_CAPTURE),
        through_a_cascade("column-type-with-using", TYPE_USING),
        through_a_cascade("add-a-column", ADD_COLUMN),
        through_a_cascade("drop-a-column", DROP_COLUMN),
        through_a_cascade("create-a-table", CREATE_TABLE),
        through_a_cascade("drop-a-table", DROP_TABLE),
        through_a_cascade("rename-a-table", RENAME_TABLE),
        through_a_cascade("move-a-table-to-another-schema", MOVE_TABLE),
        through_a_cascade("deferred-constraint-trigger-queued", DEFERRED_TRIGGER),
        through_a_cascade("deferrable-foreign-key", DEFERRABLE_FK),
        through_a_cascade("detach-and-attach-a-partition", DETACH_ATTACH),
        through_a_cascade("detach-a-partition", DETACH),
        through_a_cascade("lock-share-mode", LOCK_SHARE),
        through_a_cascade("analyze", ANALYZE),
        through_a_cascade("refresh-a-materialized-view", REFRESH),
    ];
    run_attacks(&own, "cascade", attacks);
}

/// The other ways Postgres runs the owner's code in a foreign key's action,
/// the same attack in transactions, and what the runtime role's own
/// functions may not do.
#[test]
fn other_vectors_transactions_and_the_runtime_role_s_functions_are_held_alike() {
    let own = start();
    let mut attacks = vec![
        through_a_cascade_in_a_transaction("disable-change-enable", OFF_CHANGE_ON),
        through_a_cascade_in_a_transaction("create-a-table", CREATE_TABLE),
        // The tampering first, honest statements after: all rolled back.
        Attack {
            name: "tamper-first-honest-after".to_owned(),
            setup: vec![outside(
                "execute",
                "insert into ca (id, p, action) values (3, 3, $1)",
                &[OFF_CHANGE_ON],
            )],
            attack: vec![
                begin_write("t", None),
                inside("t", "delete from pa where id = 3", &[]),
                inside(
                    "t",
                    "insert into things (id, title) values (801, 'honest in the same transaction')",
                    &[],
                ),
                inside(
                    "t",
                    "insert into counters (name, n) values ('honest in the same transaction', 1)",
                    &[],
                ),
                commit("t"),
                execute_in("t", "select 1"),
            ],
            failing: 4,
            entry: "commit",
            then_over: true,
            since: 0,
        },
        // A CHECK on the child of ON UPDATE CASCADE.
        Attack {
            name: "check-in-an-update-cascade".to_owned(),
            setup: vec![outside(
                "execute",
                "insert into cb (id, p, action) values (1, 1, $1)",
                &[OFF_CHANGE_ON],
            )],
            attack: vec![outside(
                "execute",
                "update pb set id = 1001 where id = 1",
                &[],
            )],
            failing: 0,
            entry: "execute",
            then_over: false,
            since: 0,
        },
        // A stored generated column there.
        Attack {
            name: "stored-generated-column-in-an-update-cascade".to_owned(),
            setup: vec![outside(
                "execute",
                "insert into cc (id, p, action) values (1, 1, $1)",
                &[CREATE_TABLE],
            )],
            attack: vec![outside(
                "execute",
                "update pc set id = 1001 where id = 1",
                &[],
            )],
            failing: 0,
            entry: "execute",
            then_over: false,
            since: 0,
        },
        // A default under ON DELETE SET DEFAULT, obeying `orders`, in one
        // transaction with the order.
        Attack {
            name: "default-under-set-default".to_owned(),
            setup: vec![outside(
                "execute",
                "insert into cd (id, p) values (1, 1)",
                &[],
            )],
            attack: vec![
                begin_write("t", None),
                inside(
                    "t",
                    "insert into orders (id, action) values (1, $1)",
                    &[OFF_CHANGE_ON],
                ),
                inside("t", "delete from pd where id = 1", &[]),
                commit("t"),
            ],
            failing: 3,
            entry: "commit",
            then_over: false,
            since: 0,
        },
        // The runtime role's own call of a function of the Application's:
        // a lock stronger than ROW EXCLUSIVE.
        Attack {
            name: "runtime-function-lock-share-mode".to_owned(),
            setup: Vec::new(),
            attack: vec![outside("execute", "select public.obey($1)", &[LOCK_SHARE])],
            failing: 0,
            entry: "execute",
            then_over: false,
            since: 0,
        },
        // A refresh by the runtime role, granted MAINTAIN (17 and later).
        Attack {
            name: "runtime-function-refresh-with-maintain".to_owned(),
            setup: Vec::new(),
            attack: vec![outside("execute", "select public.obey($1)", &[REFRESH])],
            failing: 0,
            entry: "execute",
            then_over: false,
            since: 170_000,
        },
        // A table the runtime role itself makes where a migration let it
        // (CONTRACT.md: "whoever comes to own what it made").
        Attack {
            name: "runtime-made-table".to_owned(),
            setup: Vec::new(),
            attack: vec![outside(
                "execute",
                "select public.obey($1)",
                &["CREATE TABLE scratch.loose (x integer)"],
            )],
            failing: 0,
            entry: "execute",
            then_over: false,
            since: 0,
        },
        Attack {
            name: "runtime-made-table-in-a-transaction".to_owned(),
            setup: Vec::new(),
            attack: vec![
                begin_write("t", None),
                inside(
                    "t",
                    "select public.obey($1)",
                    &[
                        "CREATE TABLE scratch.loose (x integer);;INSERT INTO scratch.loose VALUES (1)",
                    ],
                ),
                commit("t"),
                execute_in("t", "select 1"),
            ],
            failing: 2,
            entry: "commit",
            then_over: true,
            since: 0,
        },
    ];
    // The isolation levels do not change it.
    for level in ["repeatable read", "serializable"] {
        attacks.push(Attack {
            name: format!("cascade-in-a-{level}-transaction"),
            setup: vec![outside(
                "execute",
                "insert into ca (id, p, action) values (4, 4, $1)",
                &[OFF_CHANGE_ON],
            )],
            attack: vec![
                begin_write("t", Some(level)),
                inside("t", "delete from pa where id = 4", &[]),
                commit("t"),
            ],
            failing: 2,
            entry: "commit",
            then_over: false,
            since: 0,
        });
    }
    run_attacks(&own, "vectors", attacks);
}

/// research F3: a cursor `WITH HOLD` declared from inside the owner's code
/// would run its query at the commit, after any check. CONTRACT.md: the
/// host closes every cursor first; its query never runs. Here the cursor's
/// query would delete `pa` row 9, whose child's trigger switches the capture
/// of `parents` off, changes a row and switches it on again.
#[test]
fn a_cursor_with_hold_declared_by_code_a_migration_left_never_runs_its_query() {
    let own = start();
    let application = "cursor";
    let db = install(&own, application);
    let parents = db.column("SELECT to_jsonb(p)::text FROM parents p ORDER BY id");
    let catalog = capture_catalog(&db);
    for (i, through) in ["execute", "transaction"].iter().enumerate() {
        let id = i + 5;
        let declare = format!("DECLARE held_{id} CURSOR WITH HOLD FOR SELECT public.cascade_now()");
        let (_, set) = steps(
            &own,
            application,
            &[outside(
                "execute",
                &format!("insert into ca (id, p, action) values ({id}, {id}, $1)"),
                &[&declare],
            )],
        );
        assert!(
            set[0]["ok"].is_object(),
            "the harness's own setup: {}",
            set[0]
        );
        let attack = if *through == "execute" {
            vec![outside(
                "execute",
                &format!("delete from pa where id = {id}"),
                &[],
            )]
        } else {
            vec![
                begin_write("t", None),
                inside("t", &format!("delete from pa where id = {id}"), &[]),
                commit("t"),
            ]
        };
        let (_, answers) = steps(&own, application, &attack);
        let answer = answers.last().cloned().unwrap_or(Value::Null);
        assert!(
            answer["ok"].is_object() || answer["error"]["code"] == "VM004",
            "through {through}: expected the statement to run with its cursor closed (or VM004), got {answer}"
        );
        assert_eq!(
            db.column("SELECT to_jsonb(p)::text FROM parents p ORDER BY id"),
            parents,
            "through {through}: expected the cursor's query never to run: `parents` changed"
        );
        assert_eq!(
            db.column("SELECT id::text FROM pa WHERE id = 9"),
            vec!["9".to_owned()],
            "through {through}: expected the cursor's query never to run: `pa` row 9 was deleted"
        );
        assert_eq!(
            capture_catalog(&db),
            catalog,
            "through {through}: expected the capture triggers' catalog rows as before"
        );
    }
}

/// CONTRACT.md: "A `query` outside such a transaction is in a read-only
/// transaction, where Postgres itself refuses such a change (`25006`)."
#[test]
fn a_query_calling_the_same_code_fails_read_only_and_changes_nothing() {
    let own = start();
    let application = "reads";
    let db = install(&own, application);
    let before = untouched(&db);
    let catalog = capture_catalog(&db);
    let mut wrong = Vec::new();
    // Not a lock alone: Postgres lets `LOCK TABLE` in any mode, and
    // `ANALYZE`, run in a read-only transaction; they change nothing.
    let mut cases = vec![
        (
            "a function switching the capture off",
            "select public.obey($1)",
            vec![OFF_CHANGE_ON],
        ),
        (
            "a function making a table",
            "select public.obey($1)",
            vec![CREATE_TABLE],
        ),
        (
            "a function setting off the cascade",
            "select public.cascade_now()",
            vec![],
        ),
    ];
    if server_version_num() >= 170_000 {
        cases.push((
            "a function refreshing a materialized view (MAINTAIN granted)",
            "select public.obey($1)",
            vec![REFRESH],
        ));
    }
    for (label, sql, params) in cases {
        let (_, answers) = steps(&own, application, &[outside("query", sql, &params)]);
        let answer = &answers[0];
        if !(answer["error"]["kind"] == "database" && answer["error"]["code"] == "25006") {
            wrong.push(format!("  {label}: {answer}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "expected each to fail with {{ error: {{ kind: \"database\", code: \"25006\" }} }}, got:\n{}",
        wrong.join("\n")
    );
    let after = untouched(&db);
    assert!(
        after == before,
        "expected nothing changed:\n{}",
        differences(&before, &after)
    );
    assert_eq!(capture_catalog(&db), catalog);
}

/// What reading and writing rows brings with it is as before, and is
/// recorded (CONTRACT.md: "a foreign key's cascade with the Application's
/// own triggers and constraints, functions that read and write rows,
/// `SELECT ... FOR UPDATE`, sequences, `ON CONFLICT`, any isolation level").
#[test]
fn honest_run_time_is_not_refused_and_is_recorded() {
    let own = start();
    let application = "honest";
    let db = install(&own, application);
    let catalog = capture_catalog(&db);
    // (label, steps of one request, the tables whose change must be
    // recorded by it)
    let cases: Vec<(&str, Vec<Value>, Vec<&str>)> = vec![
        (
            "a cascade whose trigger writes a row",
            vec![
                outside(
                    "execute",
                    "insert into ca (id, p, action) values (11, 11, $1)",
                    &["INSERT INTO public.counters (name, n) VALUES ('by a cascade', 1)"],
                ),
                outside("execute", "delete from pa where id = 11", &[]),
            ],
            vec!["pa", "ca", "counters"],
        ),
        (
            "a cascade through a CHECK",
            vec![
                outside(
                    "execute",
                    "insert into cb (id, p, action) values (12, 12, null)",
                    &[],
                ),
                outside("execute", "update pb set id = 1012 where id = 12", &[]),
            ],
            vec!["pb", "cb"],
        ),
        (
            "a cascade through a stored generated column",
            vec![
                outside(
                    "execute",
                    "insert into cc (id, p, action) values (13, 13, null)",
                    &[],
                ),
                outside("execute", "update pc set id = 1013 where id = 13", &[]),
            ],
            vec!["pc", "cc"],
        ),
        (
            "a function that writes rows",
            vec![outside(
                "execute",
                "select public.obey($1)",
                &["INSERT INTO public.things (id, title) VALUES (701, 'by a function')"],
            )],
            vec!["things"],
        ),
        (
            "a lock in ROW EXCLUSIVE mode inside a function",
            vec![outside(
                "execute",
                "select public.obey($1)",
                &[
                    "LOCK TABLE public.things IN ROW EXCLUSIVE MODE;;INSERT INTO public.things (id, title) VALUES (702, 'after a lock')",
                ],
            )],
            vec!["things"],
        ),
        (
            "a sequence",
            vec![outside(
                "execute",
                "insert into things (title) values ('numbered by its sequence')",
                &[],
            )],
            vec!["things"],
        ),
        (
            "ON CONFLICT",
            vec![
                outside(
                    "execute",
                    "insert into counters (name, n) values ('conflicting', 1) on conflict (name) do update set n = counters.n + 1",
                    &[],
                ),
                outside(
                    "execute",
                    "insert into counters (name, n) values ('conflicting', 1) on conflict (name) do update set n = counters.n + 1",
                    &[],
                ),
            ],
            vec!["counters"],
        ),
        (
            "SELECT FOR UPDATE and FOR KEY SHARE in a transaction",
            vec![
                begin_write("t", None),
                query_in("t", "select * from parents where id = 1 for update"),
                query_in("t", "select * from parents where id = 1 for key share"),
                inside(
                    "t",
                    "update parents set name = 'locked and updated' where id = 1",
                    &[],
                ),
                commit("t"),
            ],
            vec!["parents"],
        ),
        (
            "SELECT FOR UPDATE through execute",
            vec![outside("execute", "select * from things for update", &[])],
            vec![],
        ),
        (
            "a write to a partitioned table",
            vec![outside(
                "execute",
                "insert into measures (id, k, v) values (7, 150, 'into a partition')",
                &[],
            )],
            // Recorded under the partitioned table's name.
            vec!["measures"],
        ),
        (
            "a change of structure tried and undone inside the owner's code",
            vec![
                outside(
                    "execute",
                    "insert into ca (id, p, action) values (14, 14, $1)",
                    &[
                        "SELECT public.try_then_undo('ALTER TABLE public.things ADD COLUMN undone integer')",
                    ],
                ),
                outside("execute", "delete from pa where id = 14", &[]),
            ],
            vec!["pa", "ca"],
        ),
        (
            "a table tried and undone inside the runtime role's function",
            vec![outside(
                "execute",
                "select public.try_then_undo($1)",
                &[
                    "CREATE TABLE scratch.undone (x integer);;INSERT INTO public.things (id, title) VALUES (703, 'undone too')",
                ],
            )],
            vec![],
        ),
    ];
    let mut cases = cases;
    for (i, level) in ["read committed", "repeatable read", "serializable"]
        .into_iter()
        .enumerate()
    {
        let label: &'static str = Box::leak(format!("a {level} transaction").into_boxed_str());
        let insert: &'static str = Box::leak(
            format!(
                "insert into things (id, title) values ({}, 'in {level}')",
                710 + i
            )
            .into_boxed_str(),
        );
        cases.push((
            label,
            vec![
                begin_write("t", Some(level)),
                inside("t", insert, &[]),
                commit("t"),
            ],
            vec!["things"],
        ));
    }
    let mut wrong = Vec::new();
    for (label, request, tables) in cases {
        let last = last_history_id(&db);
        let (_, answers) = steps(&own, application, &request);
        let failed: Vec<&Value> = answers.iter().filter(|a| !a["ok"].is_object()).collect();
        if !failed.is_empty() {
            wrong.push(format!(
                "  {label}: expected every step to run, got {failed:?}"
            ));
            continue;
        }
        let rows = history_after(&db, last);
        for table in tables {
            if !rows.iter().any(|r| r["table_name"] == table) {
                wrong.push(format!(
                    "  {label}: expected a change of {table} recorded, got:\n{}",
                    lines(&rows)
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "honest run time was refused or not recorded:\n{}",
        wrong.join("\n")
    );
    assert!(
        db.column("SELECT count(*)::text FROM things WHERE id = 703") == vec!["0".to_owned()],
        "the harness's own check: what try_then_undo did is undone"
    );
    assert_eq!(
        capture_catalog(&db),
        catalog,
        "expected the capture triggers' catalog rows untouched by honest run time"
    );
}

/// CONTRACT.md: "A Version sent while writes are being made does not make
/// them fail: ... none is answered `VM004` for the Version's own change."
#[test]
fn a_version_sent_while_writes_are_made_fails_none_of_them() {
    let own = start();
    let application = "busy";
    install(&own, application);
    let stop = AtomicBool::new(false);
    let mut list = first_version();
    let (sent, answers) = std::thread::scope(|scope| {
        let writer = scope.spawn(|| {
            let mut answers = Vec::new();
            let mut id = 2000;
            let deadline = Instant::now() + Duration::from_secs(60);
            while !stop.load(Ordering::SeqCst) && Instant::now() < deadline {
                id += 1;
                let (_, a) = steps(
                    &own,
                    application,
                    &[outside(
                        "execute",
                        &format!("insert into things (id, title) values ({id}, 'while a Version is sent')"),
                        &[],
                    )],
                );
                answers.push(a.into_iter().next().unwrap_or(Value::Null));
            }
            answers
        });
        std::thread::sleep(Duration::from_millis(500));
        let mut sent = Vec::new();
        list.push(Migration::new(
            "0006_a_table",
            &[r#"CREATE TABLE "added_while_busy" ("id" integer PRIMARY KEY NOT NULL, "v" text);"#],
        ));
        sent.push(send(&own, application, &list));
        list.push(Migration::new(
            "0007_a_column",
            &[r#"ALTER TABLE "things" ADD COLUMN "added_while_busy" text;"#],
        ));
        sent.push(send(&own, application, &list));
        std::thread::sleep(Duration::from_millis(500));
        stop.store(true, Ordering::SeqCst);
        (sent, writer.join().unwrap())
    });
    for s in &sent {
        assert_eq!(s.status, 201, "{}", s.describe());
    }
    let refused: Vec<&Value> = answers.iter().filter(|a| !a["ok"].is_object()).collect();
    assert!(
        answers.len() >= 3,
        "the harness's own check: expected writes during the Versions, made {}",
        answers.len()
    );
    assert!(
        refused.is_empty(),
        "expected none of the {} writes made while Versions were sent to fail (none answered VM004), got:\n{refused:#?}",
        answers.len()
    );
}
