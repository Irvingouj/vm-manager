//! A platform database from before this ticket is sealed at the first start
//! with the key (CONTRACT.md, "Resources and Grants in full: At start"): each
//! Resource whose stored connection is in clear and each own database with a
//! password in clear is sealed in place, one at a time; the program writes
//! `credentials-sealed` with the numbers sealed and
//! `credentials-sealed-old-copies` saying that earlier copies remain, both
//! before `listening`; afterwards no column holds a clear value; what was
//! served is served as before; starting again changes nothing and seals
//! nothing twice.
//!
//! The platform database "from before" is made by running the program,
//! stopping it, and writing the clear form the program before this ticket
//! kept back into the rows (`sealed::Before`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::VmOptions;
use blackbox::sealed::{
    Before, DEVOPS, Old, SHOP, Sealed, Stored, assert_dump_holds_no_credential,
};
use serde_json::json;

fn sealed_at_the_first_start() -> (Sealed, Old) {
    Before::make().start(&VmOptions::new())
}

#[test]
fn the_harness_s_platform_database_from_before_holds_every_clear_value() {
    // The harness's own check, which must hold on any program: the state the
    // other tests start from really is clear, so that a pass means the
    // program sealed it.
    let before = Before::make();
    let stored = Stored::of(&before.platform);
    assert_eq!(stored.connection(DEVOPS).unwrap(), before.connection);
    let passwords = stored.own_passwords(SHOP);
    assert_eq!(
        passwords.platform.as_deref(),
        Some(before.passwords.0.as_str())
    );
    assert_eq!(
        passwords.runtime.as_deref(),
        Some(before.passwords.1.as_str())
    );
    let dump = blackbox::sealed::Dump::of(&before.platform);
    assert!(
        !before.markers.found_in(&dump.text()).is_empty(),
        "the harness's own check: the clear state holds what the dump test looks for"
    );
}

#[test]
fn at_the_first_start_with_the_key_each_clear_value_is_sealed_and_two_records_say_so() {
    let (s, old) = sealed_at_the_first_start();
    let sealed = s.logged("credentials-sealed");
    assert_eq!(
        sealed.len(),
        1,
        "expected one `credentials-sealed` record, got {sealed:?}"
    );
    assert_eq!(sealed[0]["resources"], json!(1), "{}", sealed[0]);
    assert_eq!(sealed[0]["own_databases"], json!(1), "{}", sealed[0]);
    let copies = s.logged("credentials-sealed-old-copies");
    assert_eq!(
        copies.len(),
        1,
        "expected one `credentials-sealed-old-copies` record, got {copies:?}"
    );
    let message = copies[0]["message"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    for word in ["backup", "old files", "login", "replace"] {
        assert!(
            message.contains(word),
            "expected the message to say earlier copies remain in backups and in the database server's old files and that logins of shared databases should be changed there and replaced here (the word {word:?}), got {message:?}"
        );
    }
    let (sealed_at, listening_at) = s.before_listening("credentials-sealed");
    let (copies_at, _) = s.before_listening("credentials-sealed-old-copies");
    assert!(
        sealed_at.is_some() && sealed_at < listening_at,
        "expected `credentials-sealed` before `listening` ({sealed_at:?}, {listening_at:?})"
    );
    assert!(
        copies_at.is_some() && copies_at < listening_at,
        "expected `credentials-sealed-old-copies` before `listening` ({copies_at:?}, {listening_at:?})"
    );
    assert!(
        !old.is_still_clear(),
        "expected the stored connection and passwords to be no longer what the program before wrote"
    );
}

#[test]
fn after_the_first_start_a_dump_holds_no_clear_value() {
    let (s, old) = sealed_at_the_first_start();
    assert_dump_holds_no_credential(
        s.platform(),
        &old.markers,
        "after the first start with the key",
    );
}

#[test]
fn what_was_sealed_is_served_as_before() {
    let (s, _old) = sealed_at_the_first_start();
    let (_, answer) = s.read_devops();
    assert_eq!(
        answer["ok"]["rows"],
        json!([[1]]),
        "expected the sealed Resource to be served, got {answer}"
    );
    let (_, answer) = s.read_shop();
    assert_eq!(
        answer["ok"]["rows"],
        json!([["seeded by 0000"]]),
        "expected the own database with sealed passwords to be served with its data, got {answer}"
    );
}

#[test]
fn starting_again_changes_nothing_and_seals_nothing_twice() {
    let (s, old) = sealed_at_the_first_start();
    assert!(
        !old.is_still_clear(),
        "expected the first start to have sealed the stored values (so that starting again has something to leave alone)"
    );
    let stored = Stored::of(&old.platform);
    let connection = stored.connection(DEVOPS).unwrap();
    let passwords = stored.own_passwords(SHOP);
    let s = s.restarted();
    assert_eq!(
        s.logged("credentials-sealed"),
        Vec::<serde_json::Value>::new(),
        "expected no second sealing"
    );
    assert_eq!(
        s.logged("credentials-sealed-old-copies"),
        Vec::<serde_json::Value>::new(),
        "expected no second record of old copies"
    );
    assert_eq!(
        stored.connection(DEVOPS).unwrap(),
        connection,
        "the stored connection"
    );
    assert_eq!(
        stored.own_passwords(SHOP),
        passwords,
        "the stored passwords"
    );
    let (_, answer) = s.read_devops();
    assert_eq!(answer["ok"]["rows"], json!([[1]]), "{answer}");
}

#[test]
fn a_start_with_the_key_of_a_database_that_is_sealed_already_says_nothing_of_it() {
    // A platform database made by this program, not from before: nothing to
    // seal, no record of sealing, no record of a Resource unusable.
    let s = Sealed::start(&VmOptions::new());
    s.with_board();
    s.with_shop(&[blackbox::own_database::first_tables()]);
    let s = s.restarted();
    for event in [
        "credentials-sealed",
        "credentials-sealed-old-copies",
        "resource-unusable",
        "own-database-unusable",
        "own-database-passwords-replaced",
    ] {
        assert_eq!(
            s.logged(event),
            Vec::<serde_json::Value>::new(),
            "expected no `{event}` record"
        );
    }
    assert_eq!(s.read_devops().1["ok"]["rows"], json!([[1]]));
}
