//! What the tests of "What something else changed" share (CONTRACT.md,
//! "The platform's part of an own database is out of a migration's reach:
//! What a migration leaves behind: What something else changed"): the
//! `capture-check` entries of the Operation log, the shape of the capture,
//! a change made as the owner role outside the host, the platform database
//! reached as the harness's superuser, and a stop and a start of the
//! program on the same platform database.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::out_of_reach::{ON_UPDATE, capture_triggers};
use crate::own_database::{Own, OwnDatabase, ident, literal};
use crate::postgres::{Platform, shared};
use crate::sql::Sql;
use crate::vm::{Vm, VmOptions};

/// The `capture-check` entries of the Operation log for `application`.
pub fn capture_checks(own: &Own, application: &str) -> Vec<Value> {
    own.log()
        .into_iter()
        .filter(|e| e["operation"] == "capture-check" && e["application"] == application)
        .collect()
}

/// The `capture-check` entries, waiting up to 5 s for there to be one.
pub fn capture_checks_eventually(own: &Own, application: &str) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let found = capture_checks(own, application);
        if !found.is_empty() || Instant::now() >= deadline {
            return found;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Fails unless `entry` has the fields CONTRACT.md gives a `capture-check`
/// entry.
pub fn assert_capture_check_entry(entry: &Value, application: &str) {
    let want = [
        ("operation", Value::from("capture-check")),
        ("statement", Value::from("capture-check")),
        ("outcome", Value::from("failed")),
        ("error", Value::from("changed-outside")),
        ("application", Value::from(application)),
        ("own_database", Value::from(true)),
        ("actor", Value::from("platform")),
        ("user", Value::Null),
        ("request", Value::Null),
        ("resource", Value::Null),
        ("transaction", Value::Null),
        ("rows", Value::Null),
    ];
    for (field, value) in want {
        assert_eq!(
            entry.get(field),
            Some(&value),
            "expected the capture-check entry's `{field}` to be {value}, the entry is {entry}"
        );
    }
}

/// The capture triggers by table, events and function, each with whether
/// it is enabled (`O`).
pub fn capture_shape(db: &OwnDatabase) -> Vec<String> {
    db.column(
        "SELECT t.tgrelid::regclass::text || ' type=' || t.tgtype::text || ' ' || t.tgfoid::regprocedure::text
                || ' enabled=' || t.tgenabled::text
           FROM pg_trigger t JOIN pg_proc p ON p.oid = t.tgfoid
          WHERE NOT t.tgisinternal AND p.pronamespace = 'vm'::regnamespace
          ORDER BY 1",
    )
}

/// As the owner role (the superuser's `SET ROLE`), outside the host: the
/// update capture of `things` left disabled.
pub fn leave_update_capture_of_things_disabled(db: &OwnDatabase) {
    let sql = db.superuser();
    let names: Vec<String> = capture_triggers(db)
        .into_iter()
        .filter(|t| t.table == "things" && t.tgtype & ON_UPDATE != 0)
        .map(|t| ident(&t.name))
        .collect();
    assert!(
        !names.is_empty(),
        "the harness's own check: expected `things` to have an update capture trigger"
    );
    sql.batch(&format!("SET ROLE {}", ident(&db.owner)));
    for name in names {
        sql.batch(&format!("ALTER TABLE public.things DISABLE TRIGGER {name}"));
    }
}

/// The platform database, as the harness's superuser.
pub fn platform_superuser(own: &Own) -> Sql {
    Sql::connect(&shared().url(own.vm.platform().name()))
        .unwrap_or_else(|e| panic!("the harness's own connection to the platform database: {e}"))
}

/// What starts the program again on the same platform database.
pub struct Stopped {
    platform: Platform,
    options: VmOptions,
}

impl Stopped {
    /// Starts the program again.
    pub fn start(&self) -> Own {
        Own {
            vm: Vm::start(&self.options.clone().platform(&self.platform)),
        }
    }
}

/// Stops the program (`SIGKILL`).
pub fn stop(own: Own) -> Stopped {
    let Own { vm } = own;
    let stopped = Stopped {
        platform: vm.platform().clone(),
        options: vm.options().clone(),
    };
    drop(vm);
    stopped
}

/// Waits up to `timeout` until a session of the database `database` that
/// is not the asker's matches `condition` (SQL on `pg_stat_activity`);
/// returns whether one did.
pub fn wait_for_session(database: &str, condition: &str, timeout: Duration) -> bool {
    let sql = Sql::connect(&shared().url("postgres"))
        .unwrap_or_else(|e| panic!("the harness's own connection to `postgres`: {e}"));
    let deadline = Instant::now() + timeout;
    loop {
        let found = sql.value(&format!(
            "SELECT count(*)::text FROM pg_stat_activity
              WHERE datname = {} AND pid <> pg_backend_pid() AND ({condition})",
            literal(database)
        ));
        if found.as_deref().is_some_and(|n| n != "0") {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
