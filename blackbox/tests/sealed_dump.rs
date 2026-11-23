//! A dump of the platform database holds no usable credential (CONTRACT.md,
//! "Resources and Grants in full: What is sealed, and under which key"; ticket
//! "Resources and Grants in full: credentials encrypted, revoking, listing"):
//! a database Resource's host, port, database, roles and passwords, and an
//! own database's role passwords, are kept sealed. The test registers a
//! Resource with recognisable parts, makes an Application with an own
//! database, reads every column of every table of the platform database as
//! the Postgres administrator, and looks for the recognisable strings whole,
//! as hex and as base64, and for the 64 lower-case hex digits an own
//! database's password had before this ticket. (`pg_dump` is not on the
//! machine and not in the harness's Postgres; the same columns are read with
//! SQL.)
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::VmOptions;
use blackbox::own_database::first_tables;
use blackbox::sealed::{Dump, Markers, Sealed, Stored, assert_dump_holds_no_credential};
use blackbox::storage::{MARKETING, Storage};

#[test]
fn a_dump_of_the_platform_database_holds_no_usable_credential() {
    let s = Sealed::start(&VmOptions::new());
    s.with_board();
    // A second Resource with the third login, so that three passwords and
    // three roles are in the database.
    s.register_with("careless", &s.db.careless);
    s.with_shop(&[first_tables()]);
    let (_, answer) = s.read_devops();
    assert!(
        answer["ok"].is_object(),
        "the harness's own check: the Resource is served before it is looked for in a dump, got {answer}"
    );
    let markers = Markers::of(&s.db);
    assert!(
        markers.count() >= 9,
        "the harness's own check: host, port, database, three roles and three passwords"
    );
    assert_dump_holds_no_credential(s.platform(), &markers, "with the key");
}

#[test]
fn the_same_connection_registered_under_two_names_is_stored_as_two_values() {
    // From the contract's AES-256-GCM, a value bound to its row: not a test
    // of a format, a test that nothing about the stored value is the same
    // for the same connection.
    let s = Sealed::start(&VmOptions::new());
    s.register_with("first_one", &s.db.reader);
    s.register_with("second_one", &s.db.reader);
    let stored = Stored::of(s.platform());
    let first = stored.connection("first_one").unwrap();
    let second = stored.connection("second_one").unwrap();
    assert_ne!(
        first, second,
        "expected the same connection registered under two names to be stored as two different values"
    );
}

#[test]
fn a_storage_resource_s_location_is_not_a_credential_and_is_stored_as_before() {
    let s = Storage::start();
    s.register(MARKETING);
    let dump = Dump::of(s.vm.platform());
    let directory = s.dir(MARKETING);
    let text = dump.text();
    assert!(
        text.contains(&*directory.to_string_lossy()),
        "expected the location of a Storage Resource to be stored readable, as before"
    );
}
