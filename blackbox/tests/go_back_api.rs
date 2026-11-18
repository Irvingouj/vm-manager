//! Going back, on the platform's API (CONTRACT.md, "Versions, going back,
//! and logs: Going back, on the platform's API"; ticket "Versions: list
//! them, go back to the previous one, read logs": going back switches
//! serving to the previous Version at once, and says that migrations are
//! not undone).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::sync::Barrier;
use std::thread;
use std::time::Instant;

use blackbox::api::{
    assert_json_content_type, assert_served_by, marked_bundle, version_body_declaring,
};
use blackbox::expect::assert_refused;
use blackbox::logs::{GONE_BACK_KEYS, from_to, go_back};
use blackbox::own_database::{
    Migration, Own, first_tables, helpers, more_tables, names, own_query,
};
use blackbox::records::{event, request_of};
use blackbox::scenario::{administrator, op};
use blackbox::{INSTALLER, User, Vm, assert_api_error, fixture_bundle};
use serde_json::{Value, json};

/// How long the slow request of the in-flight test takes.
const SLOW_MS: u64 = 3000;

/// The Contributor of every Application these tests make.
fn alice() -> User {
    User::new(INSTALLER)
}

/// A User who is not a Contributor.
fn bob() -> User {
    User::new("bob").in_groups(&["staff"])
}

/// A program with the stand-in sign-in and a Platform administrators'
/// Group.
fn start() -> Own {
    Own::start()
}

/// Creates `name` as [`alice`] with `n` Versions, each `marked_bundle("v<i>")`.
fn with_versions(vm: &Vm, name: &str, n: u64) {
    let api = vm.api_as(&alice());
    api.create_application(name).expect_status(201);
    // Shared with everyone: these tests are not about who can open it.
    api.share_with_everyone(name);
    for i in 1..=n {
        api.send_bundle(name, &marked_bundle(&format!("v{i}")))
            .expect_status(201);
    }
}

/// The Application's record and its Versions, as the API answers them: what
/// a refused go-back must leave as it was.
fn state(vm: &Vm, name: &str) -> (Value, Vec<Value>) {
    let api = vm.api_as(&alice());
    (api.application(name), api.versions(name))
}

fn assert_nothing_changed(vm: &Vm, name: &str, before: &(Value, Vec<Value>), context: &str) {
    assert_eq!(
        &state(vm, name),
        before,
        "{context}: expected a refused go-back to change nothing: the Application's record and its Versions as they were"
    );
}

/// Goes back as [`alice`], which must answer `200`; the answer.
fn gone_back(vm: &Vm, name: &str, from: u64, to: Option<u64>) -> Value {
    let answer = go_back(&vm.api_as(&alice()), name, &from_to(from, to));
    assert!(
        answer.status == 200,
        "expected `200` going back from {from} (to {to:?}) in {name:?}, got\n{}",
        answer.describe()
    );
    assert_json_content_type(&answer);
    answer.json()
}

#[test]
fn going_back_answers_200_with_what_serves_now_and_what_served_before() {
    let own = start();
    with_versions(&own.vm, "board", 3);
    let answer = gone_back(&own.vm, "board", 3, None);
    let mut keys: Vec<&str> = answer
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    keys.sort_unstable();
    assert_eq!(
        keys, GONE_BACK_KEYS,
        "expected exactly the contract's fields, got {answer}"
    );
    assert_eq!(
        answer,
        json!({ "application": "board", "serving_version": 2, "was": 3, "migrations_not_undone": [] }),
        "expected Version 2 to serve in place of 3, and no migration not undone (no Version has one)"
    );
}

#[test]
fn once_the_200_is_received_every_new_request_is_served_by_the_version_gone_back_to() {
    let own = start();
    with_versions(&own.vm, "board", 3);
    gone_back(&own.vm, "board", 3, None);
    assert_eq!(
        own.vm.api_as(&bob()).application("board")["serving_version"],
        2,
        "expected `serving_version` 2 once the 200 was received"
    );
    for user in [alice(), bob(), User::new("carol")] {
        let response = own.vm.get_as(&user, "board", "/");
        assert_served_by(&own.vm, &response, "board", "v2", 2);
    }
    gone_back(&own.vm, "board", 2, None);
    assert_served_by(
        &own.vm,
        &own.vm.get_as(&bob(), "board", "/"),
        "board",
        "v1",
        1,
    );
}

#[test]
fn a_request_in_flight_finishes_on_the_version_it_started_on() {
    let own = start();
    let vm = &own.vm;
    with_versions(vm, "board", 2);
    let token = "go-back-in-flight-1";
    let slow = vm
        .request("board", &format!("/slow?ms={SLOW_MS}&token={token}"))
        .as_user(&bob());
    thread::scope(|scope| {
        let in_flight = scope.spawn(|| {
            let response = slow.send();
            (response, Instant::now())
        });
        let started = vm.wait_for_record("the slow request to have started", |r| {
            event(r) == "printed" && r["message"].as_str().is_some_and(|m| m.contains(token))
        });
        gone_back(vm, "board", 2, None);
        let received_200 = Instant::now();
        assert_served_by(
            vm,
            &vm.get_as(&User::new("carol"), "board", "/"),
            "board",
            "v1",
            1,
        );
        let (slow_response, finished) = in_flight.join().unwrap();
        assert!(
            finished > received_200,
            "the slow request ended before the go-back's 200 was received, so this test showed nothing: raise SLOW_MS"
        );
        assert_eq!(
            request_of(&started),
            Some(slow_response.request_id().as_str())
        );
        assert_served_by(vm, &slow_response, "board", "v2", 2);
    });
}

#[test]
fn the_version_gone_back_to_serves_with_its_own_declared_access() {
    // Version 1 declares `devops` (no Resource of that name is registered),
    // Version 2 declares nothing: a query on `devops` is refused
    // `not-registered` by the first and `not-declared` by the second.
    let own = start();
    let api = own.api();
    api.create_application("board").expect_status(201);
    // Shared with everyone: this test is not about who can open it.
    api.share_with_everyone("board");
    let bundle = fixture_bundle("operations.js");
    api.send_version(
        "board",
        &version_body_declaring(&bundle, &[("devops", "read")]),
    )
    .expect_status(201);
    api.send_version("board", &version_body_declaring(&bundle, &[]))
        .expect_status(201);
    let call = op("query", &json!({ "resource": "devops", "sql": "select 1" }));
    let (_, answer) = own.one(&bob(), "board", &call);
    assert_refused(
        &answer,
        "not-declared",
        "under Version 2, which declares nothing",
    );
    gone_back(&own.vm, "board", 2, None);
    let (_, answer) = own.one(&bob(), "board", &call);
    assert_refused(
        &answer,
        "not-registered",
        "under Version 1 gone back to, which declares `devops`",
    );
}

#[test]
fn to_names_an_earlier_version_to_go_back_to() {
    let own = start();
    with_versions(&own.vm, "board", 3);
    let answer = gone_back(&own.vm, "board", 3, Some(1));
    assert_eq!(answer["serving_version"], 1, "{answer}");
    assert_eq!(answer["was"], 3, "{answer}");
    assert_served_by(
        &own.vm,
        &own.vm.get_as(&bob(), "board", "/"),
        "board",
        "v1",
        1,
    );
}

#[test]
fn to_left_out_or_null_is_the_version_before_from() {
    let own = start();
    with_versions(&own.vm, "board", 3);
    let answer = go_back(
        &own.vm.api_as(&alice()),
        "board",
        &json!({ "from": 3, "to": null }),
    );
    answer.expect_status(200);
    assert_eq!(answer.json()["serving_version"], 2, "`to` null");
    let answer = gone_back(&own.vm, "board", 2, None);
    assert_eq!(answer["serving_version"], 1, "`to` left out");
}

#[test]
fn migrations_not_undone_are_the_latest_versions_that_the_version_gone_back_to_lacks() {
    let own = start();
    own.create("notes");
    own.send_ok("notes", &[first_tables()]);
    own.send_ok("notes", &[first_tables(), helpers()]);
    own.send_ok("notes", &[first_tables(), helpers(), more_tables()]);
    let db = own.own_database("notes");

    let answer = gone_back(&own.vm, "notes", 3, None);
    assert_eq!(
        answer["migrations_not_undone"],
        json!(["0002_more_tables"]),
        "going back from 3 to 2: the migration Version 3 has and Version 2 has not"
    );
    // Version 2 serves; the latest is still Version 3. Going back to 1
    // names what the latest has and Version 1 has not, in their order.
    let answer = gone_back(&own.vm, "notes", 2, None);
    assert_eq!(
        answer["migrations_not_undone"],
        json!(["0001_helpers", "0002_more_tables"]),
        "going back from 2 to 1: the migrations the latest Version (3) has and Version 1 has not, in order"
    );

    // The own database stays as the latest Version's migrations left it,
    // and Version 1's code runs against it: it reads the newer table.
    assert!(db.has_table("extras"), "expected `extras` still there");
    assert_eq!(
        db.column("SELECT label FROM seeded ORDER BY id"),
        ["seeded by 0000", "seeded by 0002"],
        "expected the rows of the migrations not undone still there"
    );
    assert_eq!(own.serving(&bob(), "notes"), 1);
    let (_, read) = own.one(
        &bob(),
        "notes",
        &own_query("application", "SELECT count(*) AS n FROM extras"),
    );
    assert!(
        read["ok"].is_object(),
        "expected Version 1's code to read the table a migration it lacks made, got {read}"
    );
}

#[test]
fn versions_are_not_removed_or_renumbered_and_the_next_continues_the_latest() {
    let own = start();
    own.create("notes");
    own.send_ok("notes", &[first_tables()]);
    own.send_ok("notes", &[first_tables(), helpers()]);
    let latest = vec![first_tables(), helpers(), more_tables()];
    own.send_ok("notes", &latest);
    let api = own.api();
    let before: Vec<String> = (1..=3)
        .map(|n| api.get_version("notes", n).expect_status(200).text())
        .collect();

    gone_back(&own.vm, "notes", 3, Some(1));
    let after: Vec<String> = (1..=3)
        .map(|n| api.get_version("notes", n).expect_status(200).text())
        .collect();
    assert_eq!(
        after, before,
        "expected every Version still there, byte for byte, the one gone back from too"
    );
    assert_eq!(own.version_numbers("notes"), [1, 2, 3]);

    // Version 1's migrations do not continue the latest Version's.
    let diverged = own.send("notes", &[first_tables()]);
    assert_api_error(&diverged, 409, "migrations-diverged");
    assert_eq!(own.version_numbers("notes"), [1, 2, 3]);
    assert_eq!(api.application("notes")["serving_version"], 1);

    // The latest's, and one more: numbered after the latest, and only the
    // new migration is applied (applying the others again would fail).
    let added = Migration::new(
        "0003_after_going_back",
        &[r#"CREATE TABLE "later" ("id" serial PRIMARY KEY NOT NULL);"#],
    );
    let mut next = latest;
    next.push(added);
    let sent = own.send_ok("notes", &next);
    assert_eq!(
        sent["number"], 4,
        "expected the next Version numbered after the latest, got {sent}"
    );
    assert_eq!(api.application("notes")["serving_version"], 4);
    assert!(own.own_database("notes").has_table("later"));
    assert_eq!(
        names(&next).len(),
        sent["migrations"].as_array().map_or(0, Vec::len),
        "expected the Version to carry the whole list"
    );
}

#[test]
fn only_backwards_to_a_version_below_from() {
    let own = start();
    with_versions(&own.vm, "board", 3);
    let api = own.vm.api_as(&alice());
    let before = state(&own.vm, "board");
    for to in [3_u64, 4, 9] {
        let answer = go_back(&api, "board", &from_to(3, Some(to)));
        assert_api_error(&answer, 400, "malformed");
        assert_nothing_changed(&own.vm, "board", &before, &format!("`to` {to} from 3"));
    }
    // Gone back to 1, Version 3 cannot be served again by going "back".
    gone_back(&own.vm, "board", 3, Some(1));
    let before = state(&own.vm, "board");
    assert_api_error(
        &go_back(&api, "board", &from_to(1, Some(3))),
        400,
        "malformed",
    );
    assert_api_error(
        &go_back(&api, "board", &from_to(1, Some(2))),
        400,
        "malformed",
    );
    assert_nothing_changed(&own.vm, "board", &before, "going forward");
}

#[test]
fn a_body_that_is_not_the_one_asked_for_is_malformed_and_changes_nothing() {
    let own = start();
    with_versions(&own.vm, "board", 3);
    let api = own.vm.api_as(&alice());
    let before = state(&own.vm, "board");
    let bodies = [
        json!({}),
        json!({ "to": 2 }),
        json!({ "from": null }),
        json!({ "from": 0 }),
        json!({ "from": -3 }),
        json!({ "from": "3" }),
        json!({ "from": 3.5 }),
        json!({ "from": true }),
        json!({ "from": 3, "to": 0 }),
        json!({ "from": 3, "to": -1 }),
        json!({ "from": 3, "to": "2" }),
        json!({ "from": 3, "to": 1.5 }),
        json!([3]),
        json!(3),
    ];
    for body in &bodies {
        assert_api_error(&go_back(&api, "board", body), 400, "malformed");
        assert_nothing_changed(&own.vm, "board", &before, &format!("the body {body}"));
    }
    for raw in ["", "not json", "{\"from\": 3", "{\"from\": 3, \"from\": 3}"] {
        let answer = api.post_raw("/v1/applications/board/go-back", raw);
        assert_api_error(&answer, 400, "malformed");
        assert_nothing_changed(&own.vm, "board", &before, &format!("the body {raw:?}"));
    }
    // Fields the request does not have are ignored.
    let answer = go_back(
        &api,
        "board",
        &json!({ "from": 3, "reason": "a bad deploy" }),
    );
    answer.expect_status(200);
}

#[test]
fn no_such_application_is_not_found_whoever_asks() {
    let own = start();
    for user in [alice(), bob(), administrator()] {
        let api = own.vm.api_as(&user);
        assert_api_error(
            &go_back(&api, "nothing-here", &from_to(2, None)),
            404,
            "not-found",
        );
        assert_api_error(&go_back(&api, "nothing-here", &json!({})), 404, "not-found");
    }
}

#[test]
fn someone_who_is_not_a_contributor_is_refused_before_their_body_is_looked_at() {
    let own = start();
    with_versions(&own.vm, "board", 3);
    let before = state(&own.vm, "board");
    // A Platform administrator who is not a Contributor may read logs, but
    // going back is a Contributor's.
    for user in [bob(), administrator()] {
        let api = own.vm.api_as(&user);
        for body in [
            from_to(3, None),
            json!({}),
            json!({ "from": "x" }),
            from_to(2, None),
        ] {
            assert_api_error(&go_back(&api, "board", &body), 403, "not-a-contributor");
        }
        assert_api_error(
            &api.post_raw("/v1/applications/board/go-back", "not json"),
            403,
            "not-a-contributor",
        );
    }
    assert_nothing_changed(&own.vm, "board", &before, "going back as someone else");
}

#[test]
fn from_that_does_not_serve_is_serving_changed_and_names_the_one_that_serves() {
    let own = start();
    with_versions(&own.vm, "board", 3);
    let api = own.vm.api_as(&alice());
    let before = state(&own.vm, "board");
    for from in [2_u64, 1, 4, 99] {
        let answer = go_back(&api, "board", &from_to(from, None));
        assert_api_error(&answer, 409, "serving-changed");
        let message = answer.json()["error"]["message"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            message.contains('3'),
            "expected the message to name Version 3, the one that serves, got {message:?}"
        );
        assert_nothing_changed(&own.vm, "board", &before, &format!("`from` {from}"));
    }
    // `malformed` comes first: `to` not below `from`.
    assert_api_error(
        &go_back(&api, "board", &from_to(2, Some(2))),
        400,
        "malformed",
    );
    // And `serving-changed` before `no-previous-version`: from 1 while 3
    // serves is the first.
    assert_api_error(
        &go_back(&api, "board", &from_to(1, None)),
        409,
        "serving-changed",
    );
}

#[test]
fn an_application_with_no_version_is_serving_changed() {
    let own = start();
    let api = own.vm.api_as(&alice());
    api.create_application("empty").expect_status(201);
    let before = state(&own.vm, "empty");
    for from in [1_u64, 2] {
        assert_api_error(
            &go_back(&api, "empty", &from_to(from, None)),
            409,
            "serving-changed",
        );
    }
    assert_nothing_changed(&own.vm, "empty", &before, "with no Version");
}

#[test]
fn version_1_serving_has_no_previous_version() {
    let own = start();
    with_versions(&own.vm, "single", 1);
    let api = own.vm.api_as(&alice());
    let before = state(&own.vm, "single");
    let answer = go_back(&api, "single", &from_to(1, None));
    assert_api_error(&answer, 409, "no-previous-version");
    assert_nothing_changed(&own.vm, "single", &before, "Version 1 serving");
    // Also when Version 1 serves because it was gone back to.
    with_versions(&own.vm, "board", 2);
    gone_back(&own.vm, "board", 2, None);
    let before = state(&own.vm, "board");
    assert_api_error(
        &go_back(&api, "board", &from_to(1, None)),
        409,
        "no-previous-version",
    );
    assert_nothing_changed(&own.vm, "board", &before, "Version 1 gone back to");
}

#[test]
fn two_go_backs_from_the_same_version_at_once_one_is_taken_and_the_other_is_serving_changed() {
    let own = start();
    with_versions(&own.vm, "board", 3);
    let barrier = Barrier::new(2);
    let answers: Vec<(u64, blackbox::Response)> = thread::scope(|scope| {
        let handles: Vec<_> = [2_u64, 1]
            .into_iter()
            .map(|to| {
                let barrier = &barrier;
                let vm = &own.vm;
                scope.spawn(move || {
                    let api = vm.api_as(&alice());
                    barrier.wait();
                    (to, go_back(&api, "board", &from_to(3, Some(to))))
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let taken: Vec<&(u64, blackbox::Response)> =
        answers.iter().filter(|(_, a)| a.status == 200).collect();
    let refused: Vec<&(u64, blackbox::Response)> =
        answers.iter().filter(|(_, a)| a.status != 200).collect();
    assert_eq!(
        (taken.len(), refused.len()),
        (1, 1),
        "expected one 200 and one refusal, got\n{}\n{}",
        answers[0].1.describe(),
        answers[1].1.describe()
    );
    assert_api_error(&refused[0].1, 409, "serving-changed");
    let winner = taken[0].0;
    assert_eq!(taken[0].1.json()["serving_version"], winner);
    assert_eq!(
        own.vm.api_as(&alice()).application("board")["serving_version"],
        winner,
        "expected the Version the taken go-back named to serve"
    );
}

#[test]
fn a_go_back_is_one_went_back_record_in_the_programs_log() {
    let own = start();
    with_versions(&own.vm, "board", 3);
    gone_back(&own.vm, "board", 3, Some(1));
    let record = own
        .vm
        .wait_for_record("a `went-back` record", |r| event(r) == "went-back");
    assert_eq!(record["application"], "board", "{record}");
    assert_eq!(record["user"], INSTALLER, "{record}");
    assert_eq!(record["from"], 3, "{record}");
    assert_eq!(record["to"], 1, "{record}");
    // A refused one leaves none.
    let api = own.vm.api_as(&alice());
    assert_api_error(
        &go_back(&api, "board", &from_to(3, None)),
        409,
        "serving-changed",
    );
    own.vm.get_as(&bob(), "board", "/").expect_status(200);
    let count = own
        .vm
        .records()
        .iter()
        .filter(|r| event(r) == "went-back")
        .count();
    assert_eq!(
        count, 1,
        "expected one `went-back` record, for the go-back taken"
    );
}

#[test]
fn any_other_method_on_the_path_is_405() {
    let own = start();
    with_versions(&own.vm, "board", 2);
    let api = own.vm.api_as(&alice());
    for method in ["GET", "PUT", "PATCH", "DELETE"] {
        let answer = api
            .request("/v1/applications/board/go-back")
            .method(method)
            .send();
        assert_api_error(&answer, 405, "method-not-allowed");
    }
    assert_eq!(api.application("board")["serving_version"], 2);
}

#[test]
fn someone_not_signed_in_is_401_and_nothing_changes() {
    let own = start();
    with_versions(&own.vm, "board", 2);
    let answer = own
        .vm
        .api_request("/v1/applications/board/go-back")
        .method("POST")
        .json(&from_to(2, None))
        .send();
    answer.expect_status(401);
    assert_eq!(
        own.vm.api_as(&alice()).application("board")["serving_version"],
        2
    );
}
