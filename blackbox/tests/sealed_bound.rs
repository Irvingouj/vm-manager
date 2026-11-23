//! Sealing at start is bounded (CONTRACT.md, "Resources and Grants in full:
//! At start" and "Bounds"): it is done before `listening` and ends within 15
//! seconds for 1,000 Resources and 1,000 own databases, which are all sealed
//! by then in a platform database from before this ticket.
//!
//! The rows are written directly by the test as the administrator of the
//! platform database, in the clear form of the program before this ticket:
//! Resources over a host nothing listens on (nothing here connects to it) and
//! own databases of Applications that have a row and no database yet (the
//! passwords are read and written, not used, so that none is made). The time
//! is the program's own: from its first log record to its `credentials-sealed`
//! record.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::VmOptions;
use blackbox::sealed::{Dump, Markers, Sealed, Stored, has_hex64_run, old_form_password};
use serde_json::{Value, json};

const HOW_MANY: usize = 1000;

/// The bound of the contract, in milliseconds.
const BOUND_MS: u64 = 15_000;

fn time_of(record: &Value) -> u64 {
    record["time_ms"].as_u64().unwrap()
}

#[test]
fn a_thousand_resources_and_a_thousand_own_databases_are_sealed_within_15_seconds_before_listening()
{
    // A platform database the program has made the tables of.
    let platform = {
        let s = Sealed::start(&VmOptions::new());
        s.platform().clone()
    };
    let stored = Stored::of(&platform);
    let admin = stored.admin();
    let secret = old_form_password();
    let connection = json!({
        "host": "bulk.invalid", "port": 5432, "database": "bulk",
        "read": { "role": "bulk_reader", "password": secret },
        "write": null, "structure": null,
    })
    .to_string();
    admin.batch(&format!(
        "INSERT INTO resources (name, kind, description, owner, contact, connection, created_by)
         SELECT 'bulk_' || g, 'postgres', 'bulk', 'bulk', 'bulk',
                convert_to(replace('{connection}', 'bulk_reader', 'bulk_reader_' || g), 'UTF8'), 'harness'
           FROM generate_series(1, {HOW_MANY}) g;
         INSERT INTO applications (name, created_by)
         SELECT 'bulk-' || g, 'harness' FROM generate_series(1, {HOW_MANY}) g;
         INSERT INTO own_databases (application_id, platform_password, runtime_password, ready, form)
         SELECT a.id, encode(sha256(('p' || g)::bytea), 'hex'), encode(sha256(('r' || g)::bytea), 'hex'), false, 4
           FROM applications a, generate_series(1, {HOW_MANY}) g WHERE a.name = 'bulk-' || g"
    ));
    assert_eq!(
        admin.value("SELECT count(*) FROM own_databases").as_deref(),
        Some(&*HOW_MANY.to_string()),
        "the harness's own step: the rows are written"
    );
    let s = Sealed::start_on(
        &platform,
        blackbox::shared_database::SharedDatabase::create(),
        &VmOptions::new(),
    );
    let records = s.vm.records();
    let first = time_of(&records[0]);
    let sealed = s.logged("credentials-sealed");
    assert_eq!(
        sealed.len(),
        1,
        "expected one `credentials-sealed` record, got {sealed:?}"
    );
    assert_eq!(sealed[0]["resources"], json!(HOW_MANY), "{}", sealed[0]);
    assert_eq!(sealed[0]["own_databases"], json!(HOW_MANY), "{}", sealed[0]);
    let took = time_of(&sealed[0]) - first;
    assert!(
        took <= BOUND_MS,
        "expected sealing to end within {BOUND_MS} ms of the program's first record, it took {took} ms"
    );
    let (sealed_at, listening_at) = s.before_listening("credentials-sealed");
    assert!(
        sealed_at.is_some() && sealed_at < listening_at,
        "expected sealing before `listening` ({sealed_at:?}, {listening_at:?})"
    );
    let dump = Dump::of(&platform);
    let markers = Markers::default().and("a password of the bulk Resources", &secret);
    assert!(
        markers.found_in(&dump.text()).is_empty() && dump.old_form_columns().is_empty(),
        "expected nothing in clear after sealing: {:?}, {:?}",
        markers.found_in(&dump.text()),
        dump.old_form_columns()
    );
    assert!(!has_hex64_run(&dump.table_text("own_databases", false)));
    eprintln!("sealing {HOW_MANY} Resources and {HOW_MANY} own databases took {took} ms");
}

// --- An interrupted sealing, and a row that cannot be sealed ----------------

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use blackbox::sealed::{Before, DEVOPS, SHOP};
use blackbox::sql::Sql;

/// `HOW_MANY` clear Resources `bulk_<g>` and own databases of Applications
/// `bulk-<g>`, written beside what [`Before`] made. Returns the secret in
/// the Resources' connections.
fn add_bulk(stored: &Stored) {
    let secret = old_form_password();
    let connection = json!({
        "host": "bulk.invalid", "port": 5432, "database": "bulk",
        "read": { "role": "bulk_reader", "password": secret },
        "write": null, "structure": null,
    })
    .to_string();
    stored.admin().batch(&format!(
        "INSERT INTO resources (name, kind, description, owner, contact, connection, created_by)
         SELECT 'bulk_' || g, 'postgres', 'bulk', 'bulk', 'bulk',
                convert_to(replace('{connection}', 'bulk_reader', 'bulk_reader_' || g), 'UTF8'), 'harness'
           FROM generate_series(1, {HOW_MANY}) g;
         INSERT INTO applications (name, created_by)
         SELECT 'bulk-' || g, 'harness' FROM generate_series(1, {HOW_MANY}) g;
         INSERT INTO own_databases (application_id, platform_password, runtime_password, ready, form)
         SELECT a.id, encode(sha256(('p' || g)::bytea), 'hex'), encode(sha256(('r' || g)::bytea), 'hex'), false, 4
           FROM applications a, generate_series(1, {HOW_MANY}) g WHERE a.name = 'bulk-' || g"
    ));
}

/// How the bulk rows stand: (Resources sealed, Resources exactly as they
/// were, Resources neither; own databases sealed, as they were, half-sealed
/// or otherwise changed).
#[derive(Debug, PartialEq, Eq)]
struct Standing {
    resources_sealed: u64,
    resources_as_was: u64,
    resources_other: u64,
    own_sealed: u64,
    own_as_was: u64,
    own_other: u64,
}

fn standing(stored: &Stored) -> Standing {
    let admin = stored.admin();
    let number = |sql: &str| -> u64 { admin.value(sql).unwrap().parse().unwrap() };
    // Exactly as it was: the text still names the host, the secret and the
    // row's own role number; sealed: nothing of them.
    let as_was = "encode(connection, 'escape') LIKE '%bulk.invalid%'
                  AND encode(connection, 'escape') LIKE '%bulk_reader_' || substr(name, 6) || '\"%'";
    let sealed = "encode(connection, 'escape') NOT LIKE '%bulk%' AND encode(connection, 'escape') NOT LIKE '{%'";
    let resources = "FROM resources WHERE name LIKE 'bulk\\_%'";
    let owns = "FROM own_databases o JOIN applications a ON a.id = o.application_id WHERE a.name LIKE 'bulk-%'";
    let hex = "'[0-9a-f]{64}'";
    Standing {
        resources_sealed: number(&format!("SELECT count(*) {resources} AND {sealed}")),
        resources_as_was: number(&format!("SELECT count(*) {resources} AND {as_was}")),
        resources_other: number(&format!(
            "SELECT count(*) {resources} AND NOT ({sealed}) AND NOT ({as_was})"
        )),
        own_sealed: number(&format!(
            "SELECT count(*) {owns} AND o.platform_password !~ {hex} AND o.runtime_password !~ {hex}
              AND o.platform_password IS NOT NULL AND o.runtime_password IS NOT NULL"
        )),
        own_as_was: number(&format!(
            "SELECT count(*) {owns} AND o.platform_password = encode(sha256(('p' || substr(a.name, 6))::bytea), 'hex')
              AND o.runtime_password = encode(sha256(('r' || substr(a.name, 6))::bytea), 'hex')"
        )),
        own_other: number(&format!(
            "SELECT count(*) {owns}
              AND NOT (o.platform_password !~ {hex} AND o.runtime_password !~ {hex}
                       AND o.platform_password IS NOT NULL AND o.runtime_password IS NOT NULL)
              AND NOT (o.platform_password = encode(sha256(('p' || substr(a.name, 6))::bytea), 'hex')
                       AND o.runtime_password = encode(sha256(('r' || substr(a.name, 6))::bytea), 'hex'))"
        )),
    }
}

/// Starts the program on `platform` with the harness's key by hand, kills
/// it (SIGKILL) `after` the first record of its log, and waits for it.
fn start_and_kill(platform: &blackbox::Platform, after: Duration) {
    let options = VmOptions::new().stand_in_sign_in();
    let mut child = Command::new(blackbox::program::vm_binary())
        .args(options.args())
        .env("VIBE_DATABASE_URL", platform.url())
        .env("VIBE_SECRET_KEY", options.secret_key_value().unwrap())
        .env_remove("VIBE_OIDC_CLIENT_SECRET")
        .env_remove("VIBE_SETUP_CREDENTIAL")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = String::new();
    BufReader::new(child.stderr.take().unwrap())
        .read_line(&mut first)
        .unwrap();
    std::thread::sleep(after);
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn a_sealing_killed_part_way_leaves_each_row_sealed_or_as_it_was_and_the_next_start_finishes_it() {
    let mut mixed_at = None;
    let mut seen: Vec<(u64, Standing)> = Vec::new();
    for delay_ms in [150_u64, 300, 500, 800, 1200, 1800] {
        let before = Before::make();
        let stored = Stored::of(&before.platform);
        add_bulk(&stored);
        start_and_kill(&before.platform, Duration::from_millis(delay_ms));
        let now = standing(&stored);
        assert_eq!(
            (now.resources_other, now.own_other),
            (0, 0),
            "after a kill {delay_ms} ms in: expected every row sealed or exactly as it was, got {now:?}"
        );
        assert_eq!(
            now.resources_sealed + now.resources_as_was,
            u64::try_from(HOW_MANY).unwrap(),
            "after a kill {delay_ms} ms in: no Resource lost, got {now:?}"
        );
        let interrupted =
            now.resources_sealed + now.own_sealed > 0 && now.resources_as_was + now.own_as_was > 0;
        seen.push((delay_ms, now));
        // The next start finishes it.
        let (s, old) = before.start(&VmOptions::new());
        let after = standing(&stored);
        assert_eq!(
            (after.resources_as_was, after.own_as_was),
            (0, 0),
            "after the start that finishes it: no clear value remains, got {after:?}"
        );
        assert_eq!(
            (after.resources_sealed, after.own_sealed),
            (
                u64::try_from(HOW_MANY).unwrap(),
                u64::try_from(HOW_MANY).unwrap()
            ),
            "{after:?}"
        );
        let dump = blackbox::sealed::Dump::of(&old.platform);
        let found = old.markers.found_in(&dump.text());
        let columns = dump.old_form_columns();
        assert!(
            found.is_empty() && columns.is_empty(),
            "expected no recognisable credential after the finishing start (kill at {delay_ms} ms), found {found:?}, 64-hex runs in own_databases columns {columns:?}, where: {:?}",
            dump.where_found(&old.markers)
        );
        assert_eq!(s.read_devops().1["ok"]["rows"], json!([[1]]));
        assert_eq!(
            s.read_shop().1["ok"]["rows"],
            json!([["seeded by 0000"]]),
            "{SHOP}"
        );
        if interrupted {
            mixed_at = Some(delay_ms);
            break;
        }
    }
    eprintln!("kill timings and what each left: {seen:?}");
    assert!(
        mixed_at.is_some(),
        "the harness's own check: no kill timing left rows both sealed and not ({seen:?}); the test could not interrupt the sealing"
    );
}

#[test]
fn a_row_that_cannot_be_sealed_in_time_leaves_the_others_sealed_and_is_not_served() {
    let before = Before::make();
    let stored = Stored::of(&before.platform);
    // The administrator holds the Resource's row by its lock during the start.
    let held: Sql = stored.admin();
    held.batch("BEGIN");
    held.batch(&format!(
        "SELECT id FROM resources WHERE name = '{DEVOPS}' FOR UPDATE"
    ));
    let began = Instant::now();
    let (s, old) = before.start(&VmOptions::new());
    let took = began.elapsed();
    let records = s.vm.records();
    let first = time_of(&records[0]);
    let listening = records.iter().find(|r| r["event"] == "listening").unwrap();
    assert!(
        time_of(listening) - first <= BOUND_MS + 3_000,
        "expected `listening` within 15 s of the first record plus a margin, got {} ms (start took {took:?})",
        time_of(listening) - first
    );
    let incomplete = s.logged("credentials-sealing-incomplete");
    assert_eq!(
        incomplete.len(),
        1,
        "expected one record, got {incomplete:?}"
    );
    assert_eq!(incomplete[0]["resources"], json!(1), "{}", incomplete[0]);
    assert_eq!(
        incomplete[0]["own_databases"],
        json!(0),
        "{}",
        incomplete[0]
    );
    let said: Vec<Value> = s
        .logged("resource-unusable")
        .into_iter()
        .filter(|r| r["resource"] == DEVOPS)
        .collect();
    assert_eq!(said.len(), 1, "expected one record, got {said:?}");
    assert_eq!(said[0]["problem"], "not-sealed", "{}", said[0]);
    let own = stored.own_passwords(SHOP);
    assert_ne!(
        own.platform.as_deref(),
        Some(old.passwords.0.as_str()),
        "expected the other rows (the own database) sealed all the same"
    );
    assert_eq!(
        stored.connection(DEVOPS).unwrap(),
        old.connection,
        "expected the locked row left exactly as it was"
    );
    // While the row is held the Grants on it are not known: the operation is
    // refused `grants-unknown` and nothing is sent to the database.
    let seen = s.db.server_log().len();
    let answer = s.read_devops().1;
    blackbox::scenario::assert_refusal(
        &answer,
        "grants-unknown",
        &json!({ "resource": DEVOPS }),
        "an operation on a Resource whose row another session holds",
    );
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        s.db.server_log().len(),
        seen,
        "nothing sent to the database"
    );
    assert_eq!(s.read_shop().1["ok"]["rows"], json!([["seeded by 0000"]]));
    // Released, without a restart, the Resource is still in clear: not served.
    held.batch("ROLLBACK");
    drop(held);
    let mut answer = Value::Null;
    for _ in 0..10 {
        answer = s.read_devops().1;
        if answer["error"]["kind"] == "unavailable" {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    blackbox::scenario::assert_error_kind(
        &answer,
        "unavailable",
        "a Resource left in clear, its row free again",
    );
    // The next start seals it and serves it.
    let s = s.restarted();
    assert_eq!(
        s.logged("credentials-sealing-incomplete"),
        Vec::<Value>::new()
    );
    assert_ne!(stored.connection(DEVOPS).unwrap(), old.connection);
    assert_eq!(s.logged("resource-unusable"), Vec::<Value>::new());
    assert_eq!(s.read_devops().1["ok"]["rows"], json!([[1]]));
}

#[test]
fn an_own_database_that_cannot_be_sealed_in_time_is_not_sealed_and_served_after_a_later_start() {
    let before = Before::make();
    let stored = Stored::of(&before.platform);
    let held: Sql = stored.admin();
    held.batch("BEGIN");
    held.batch(&format!(
        "SELECT o.application_id FROM own_databases o JOIN applications a ON a.id = o.application_id
          WHERE a.name = '{SHOP}' FOR UPDATE OF o"
    ));
    let (s, old) = before.start(&VmOptions::new());
    let incomplete = s.logged("credentials-sealing-incomplete");
    assert_eq!(
        incomplete.len(),
        1,
        "expected one record, got {incomplete:?}"
    );
    assert_eq!(incomplete[0]["resources"], json!(0), "{}", incomplete[0]);
    assert_eq!(
        incomplete[0]["own_databases"],
        json!(1),
        "{}",
        incomplete[0]
    );
    let said: Vec<Value> = s
        .logged("own-database-unusable")
        .into_iter()
        .filter(|r| r["application"] == SHOP)
        .collect();
    assert_eq!(said.len(), 1, "expected one record, got {said:?}");
    assert_eq!(said[0]["problem"], "not-sealed", "{}", said[0]);
    assert_eq!(
        stored.own_passwords(SHOP).platform.as_deref(),
        Some(old.passwords.0.as_str()),
        "expected the locked row left as it was"
    );
    assert_ne!(
        stored.connection(DEVOPS).unwrap(),
        old.connection,
        "expected the Resource sealed all the same"
    );
    held.batch("ROLLBACK");
    drop(held);
    let s = s.restarted();
    assert_eq!(s.logged("own-database-unusable"), Vec::<Value>::new());
    assert_eq!(s.read_shop().1["ok"]["rows"], json!([["seeded by 0000"]]));
}

#[test]
fn a_request_that_uses_an_own_database_left_in_clear_fails_unavailable_as_an_operation() {
    let before = Before::make();
    let stored = Stored::of(&before.platform);
    let held: Sql = stored.admin();
    held.batch("BEGIN");
    held.batch(&format!(
        "SELECT o.application_id FROM own_databases o JOIN applications a ON a.id = o.application_id
          WHERE a.name = '{SHOP}' FOR UPDATE OF o"
    ));
    let (s, _old) = before.start(&VmOptions::new());
    let answer = s.read_shop().1;
    blackbox::scenario::assert_error_kind(&answer, "unavailable", "an own database left in clear");
}

#[test]
fn when_the_listing_of_resources_is_held_the_record_has_null_for_them_and_a_later_start_seals_them()
{
    use blackbox::outbound_surface::DbProxy;
    let before = Before::make();
    let platform = before.platform.clone();
    let stored = Stored::of(&platform);
    // A proxy between the program and the platform Postgres holds the
    // statement that lists the Resources to seal (found in the server log of
    // a start that seals: `SELECT id FROM resources WHERE kind = 'postgres'
    // ORDER BY id`); the listing of own databases passes.
    let postgres = blackbox::postgres::shared();
    let target = std::net::SocketAddr::new(postgres.host().parse().unwrap(), postgres.port());
    let proxy = DbProxy::start(target);
    proxy.arm("WHERE kind = 'postgres' ORDER BY id");
    let through = blackbox::Platform::on_address(postgres, platform.name(), proxy.address());
    let (s, old) = (
        Sealed::start_on(&through, before.db, &VmOptions::new()),
        before.markers,
    );
    assert_eq!(
        proxy.held(),
        1,
        "the harness's own check: the listing of Resources is held"
    );
    let records = s.vm.records();
    let first = time_of(&records[0]);
    let incomplete = s.logged("credentials-sealing-incomplete");
    assert_eq!(
        incomplete.len(),
        1,
        "expected one record, got {incomplete:?}"
    );
    assert_eq!(incomplete[0]["resources"], Value::Null, "{}", incomplete[0]);
    assert_eq!(
        incomplete[0]["own_databases"],
        json!(0),
        "expected the own databases sealed and counted all the same: {}",
        incomplete[0]
    );
    let took = time_of(&incomplete[0]) - first;
    assert!(
        took <= BOUND_MS + 1_000,
        "expected the sealing to give up within 15 s of the first record, it took {took} ms"
    );
    let sealed = s.logged("credentials-sealed");
    assert_eq!(sealed.len(), 1, "{sealed:?}");
    assert_eq!(sealed[0]["own_databases"], json!(1), "{}", sealed[0]);
    let said: Vec<Value> = s
        .logged("resource-unusable")
        .into_iter()
        .filter(|r| r["resource"] == DEVOPS)
        .collect();
    assert_eq!(said.len(), 1, "expected one record, got {said:?}");
    assert_eq!(said[0]["problem"], "not-sealed", "{}", said[0]);
    blackbox::scenario::assert_error_kind(
        &s.read_devops().1,
        "unavailable",
        "a Resource still in clear",
    );
    assert!(
        stored.connection(DEVOPS).unwrap().starts_with(b"{"),
        "expected the Resource left as it was"
    );
    // Released and started again straight on the platform database.
    proxy.release();
    let blackbox::sealed::Sealed { vm, db } = s;
    drop(vm);
    let s = Sealed::start_on(&platform, db, &VmOptions::new());
    assert_eq!(s.logged("credentials-sealed")[0]["resources"], json!(1));
    assert_eq!(s.logged("resource-unusable"), Vec::<Value>::new());
    assert_eq!(s.read_devops().1["ok"]["rows"], json!([[1]]));
    let _ = old;
}
