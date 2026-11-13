//! Three roles (CONTRACT.md, "The platform's part of an own database is out
//! of a migration's reach: Three roles"; ticket "The Change history is kept
//! out of a Contributor's reach", sixth acceptance criterion: "The
//! migration role cannot log in between migrations, whatever password it
//! gave itself").
//!
//! After an Application's first Version, on a platform whose role may only
//! create databases and roles:
//!
//! - `<p>_app_<n>_platform` owns the database, the schema `vm` and every
//!   object in it;
//! - `<p>_app_<n>_owner` owns what the migrations made and nothing of the
//!   platform's, and cannot log in once a Version is answered, whether the
//!   Version applied, failed in a migration, or was refused; a password a
//!   migration gave it is of no use;
//! - `<p>_app_<n>_runtime` logs in as before;
//! - the platform's role is still only `CREATEDB` and `CREATEROLE`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::api::assert_api_error;
use blackbox::history::APP;
use blackbox::out_of_reach::{assert_three_roles, base_and, can_log_in, install_seeded, start};
use blackbox::own_database::{Migration, literal, try_login};
use blackbox::shared_database::token;

#[test]
fn after_a_first_version_three_roles_stand_as_the_contract_says() {
    let own = start();
    let db = install_seeded(&own, APP);
    assert_three_roles(&own, &db, "after the first Version");
    // Objects the migrations made are the owner role's.
    let owned = db.owned_by(&db.owner);
    for object in [
        "relation things",
        "relation inventory.stock",
        "relation measures",
        "schema inventory",
    ] {
        assert!(
            owned.iter().any(|o| o == object),
            "expected the owner role to own {object:?}, it owns {owned:?}"
        );
    }
    assert!(
        db.objects_not_owned_by_the_owner().is_empty(),
        "expected the owner role to own every object the migrations made, but not: {:?}",
        db.objects_not_owned_by_the_owner()
    );
}

#[test]
fn the_owner_role_cannot_log_in_after_a_version_applied_failed_or_was_refused() {
    let own = start();
    let db = install_seeded(&own, APP);
    assert!(!can_log_in(&db, &db.owner), "after the first Version");
    let sent = own.send(
        APP,
        &base_and(&[Migration::new(
            "0004_fails",
            &[r#"ALTER TABLE "nowhere" ADD COLUMN "x" integer;"#],
        )]),
    );
    assert_api_error(&sent, 422, "migration-failed");
    assert!(
        !can_log_in(&db, &db.owner),
        "expected the owner role unable to log in after a Version that failed in a migration"
    );
    let sent = own.send(
        APP,
        &base_and(&[Migration::new("0004_commits", &["COMMIT;"])]),
    );
    assert_api_error(&sent, 422, "migration-refused");
    assert!(
        !can_log_in(&db, &db.owner),
        "expected the owner role unable to log in after a refused Version"
    );
    let sent = own.send(
        APP,
        &base_and(&[Migration::new(
            "0004_applies",
            &[r#"CREATE TABLE "later" ("id" integer PRIMARY KEY NOT NULL);"#],
        )]),
    );
    assert_eq!(sent.status, 201, "{}", sent.describe());
    assert_three_roles(&own, &db, "after a second Version");
    // A first Version that fails in its migration leaves no login either.
    own.create("second");
    let sent = own.send(
        "second",
        &[Migration::new(
            "0000_fails",
            &[
                r#"CREATE TABLE "a" ("x" integer);"#,
                r#"CREATE TABLE "a" ("x" integer);"#,
            ],
        )],
    );
    assert_api_error(&sent, 422, "migration-failed");
    if let Some(second) = own.find_own_database("second") {
        assert!(
            !second.role_exists(&second.owner) || !can_log_in(&second, &second.owner),
            "expected the owner role of an own database whose first Version failed unable to log in"
        );
    }
}

#[test]
fn a_password_a_migration_gives_the_owner_role_is_of_no_use() {
    let own = start();
    let db = install_seeded(&own, APP);
    let password = format!("pwned-{}", token());
    let sent = own.send(
        APP,
        &base_and(&[Migration::new(
            "0004_password",
            &[&format!(
                "ALTER ROLE CURRENT_USER PASSWORD {};",
                literal(&password)
            )],
        )]),
    );
    // CONTRACT.md does not say whether Postgres's own permission to change
    // one's own password is taken away; either way the password must not
    // let anyone log in.
    assert!(
        sent.status == 201 || sent.status == 422,
        "expected the Version applied or refused, got\n{}",
        sent.describe()
    );
    let tried = try_login(&db.owner, &password, &db.name);
    assert!(
        tried.is_err(),
        "expected no login as the owner role {} with the password a migration gave it, but it logged in",
        db.owner
    );
    for database in ["postgres", "template1"] {
        assert!(
            try_login(&db.owner, &password, database).is_err(),
            "expected no login as the owner role to {database} either"
        );
    }
    // Nor with a password the harness itself sets (the program's later
    // connections as the owner role do not matter after this).
    let known = db.set_known_password(&db.owner);
    assert!(
        try_login(&db.owner, &known, &db.name).is_err(),
        "expected the owner role unable to log in whatever its password"
    );
}
