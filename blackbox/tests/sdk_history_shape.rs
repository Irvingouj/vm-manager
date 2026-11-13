//! Changes made by a migration, and the history read in the table's
//! present shape (CONTRACT.md, "Reading the Change history: A change"),
//! with the fixture `fixtures/sdk/chronicle`, whose migrations are:
//!
//! - `0000_chronicle` makes `reshaped (id, keep, gone, before_name)` and
//!   `recast (id, code text)`;
//! - `0001_seed` changes rows of them: so these changes are recorded with
//!   `actor` `migration`, the Contributor who sent the Version, the
//!   migration's name and no request;
//! - `0002_reshape` drops `gone`, adds `added`, renames `before_name` to
//!   `after_name`, and makes `recast.code` an integer (the old value `x1`
//!   does not cast).
//!
//! Read in the present shape, a dropped column is not shown, an added one is
//! null, a renamed one is null under its new name; in `changed`, a column
//! the table no longer has is given by its database name. A value that no
//! longer casts makes the whole read fail with `OperationFailed` of kind
//! `database`, and changes nothing stored.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::INSTALLER;
use blackbox::history::history_where;
use blackbox::scenario::lines;
use blackbox::sdk::{ann, d};
use blackbox::sdk_history::{
    HistoryCall, SEED_MIGRATION, Want, assert_changes, assert_one_history_entry, caught_of,
    chronicle, row_of,
};
use serde_json::{Value, json};

fn reshaped(keep: &str, added: Option<&str>, after: Option<&str>) -> Value {
    let text = |v: Option<&str>| v.map_or_else(d::null, d::string);
    d::object(&[
        ("id", d::number("1")),
        ("keep", d::string(keep)),
        ("added", text(added)),
        ("afterName", text(after)),
    ])
}

#[test]
fn rows_a_migration_changed_are_read_as_the_migrations_in_the_present_shape() {
    let sdk = chronicle();
    let user = ann();
    let by_seed = |want: Want<'static>| want.by_migration(INSTALLER, SEED_MIGRATION);
    let k1 = reshaped("k1", None, None);
    let k2 = reshaped("k2", None, None);
    let seeded = [
        by_seed(Want::update("", "", "", &k2, &k2, &["gone"])),
        by_seed(Want::update("", "", "", &k1, &k2, &["keep"])),
        by_seed(Want::insert("", "", "", &k1)),
    ];
    for actor in ["user", "application"] {
        let context = format!("history(reshaped, {{ id: 1 }}) as the {actor}");
        let (response, changes) = HistoryCall::of("reshaped")
            .key(json!({ "id": 1 }))
            .as_actor(actor)
            .changes(&sdk, &user, &context);
        assert_changes(&changes, &seeded, &context);
        assert_one_history_entry(&sdk, &response, &user, actor, 3, &[], &context);
    }

    let (set, body) = sdk.get_ok(&user, "/reshaped/set?as=user&id=1&added=a&after=x");
    let now = row_of(&body);
    assert_eq!(
        now,
        reshaped("k2", Some("a"), Some("x")),
        "the harness's own check"
    );
    let context = "history(reshaped, { id: 1 }) after a User's change";
    let (_, changes) = HistoryCall::of("reshaped")
        .key(json!({ "id": 1 }))
        .changes(&sdk, &user, context);
    // 0002 renamed `before_name`, it did not empty it: just before this
    // update the row held 0001_seed's `b1` under the new name, and that is
    // the row a select() gave then.
    let before = reshaped("k2", None, Some("b1"));
    let mut wants = vec![Want::update(
        &user.id,
        "user",
        &set.request_id(),
        &before,
        &now,
        &["added", "afterName"],
    )];
    wants.extend(seeded);
    assert_changes(&changes, &wants, context);
}

#[test]
fn a_value_that_no_longer_casts_fails_the_read_and_changes_nothing_stored() {
    let sdk = chronicle();
    let user = ann();
    let stored = || history_where(&sdk.own, "h.table_name = 'recast'");
    let before = stored();
    assert_eq!(
        before.len(),
        2,
        "the harness's own check: 0001_seed's insert and update of recast are recorded:\n{}",
        lines(&before)
    );
    for actor in ["user", "application"] {
        let context = format!("history(recast) as the {actor}");
        let (response, body) = HistoryCall::of("recast").as_actor(actor).send(&sdk, &user);
        let caught = caught_of(&body, &context);
        assert_eq!(
            caught["isOperationFailed"], true,
            "{context}: OperationFailed, got {caught}"
        );
        assert_eq!(caught["kind"], "database", "{context}: {caught}");
        let entries = sdk.entries_of(&response);
        assert_eq!(
            entries.len(),
            1,
            "{context}: one Operation log entry:\n{}",
            lines(&entries)
        );
        assert_eq!(entries[0]["outcome"], "failed", "{context}: {}", entries[0]);
        assert!(
            entries[0]["error"]
                .as_str()
                .is_some_and(|e| e.chars().count() == 5),
            "{context}: the failed entry's `error` is the SQLSTATE: {}",
            entries[0]
        );
        assert_eq!(
            entries[0]["operation"], "query",
            "{context}: {}",
            entries[0]
        );
    }
    assert_eq!(stored(), before, "what is stored is never changed by this");
    // Another table's history still reads.
    let (_, changes) = HistoryCall::of("reshaped").key(json!({ "id": 1 })).changes(
        &sdk,
        &user,
        "reshaped, beside recast",
    );
    assert_eq!(changes.len(), 3, "{}", lines(&changes));
}
