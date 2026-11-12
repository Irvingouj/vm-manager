//! Which Version serves (CONTRACT.md, "The platform's API: Versions", and
//! "Responses the program itself gives"): an Application with no Version is
//! `404`; the newest Version serves every request sent once its `201` has
//! been received, while a request already in flight finishes on the Version
//! it started on; earlier Versions are unchanged and still listed; a bundle
//! that fails when it runs is still a Version, and its requests get the
//! fixed `500`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use blackbox::api::{assert_served_by, marked_bundle};
use blackbox::records::{event, request_of};
use blackbox::{Response, User, Vm, VmOptions, assert_api_error, fixture_bundle};
use serde_json::Value;

/// How long the slow request of the in-flight test takes: long enough for
/// a Version to be sent and another request served meanwhile.
const SLOW_MS: u64 = 3000;

/// A request of the steady traffic: when it was sent, and its answer.
type Served = (Instant, Response);

fn vm() -> Vm {
    Vm::start(&VmOptions::new().stand_in_sign_in())
}

fn alice() -> User {
    User::new("alice")
}

fn bob() -> User {
    User::new("bob").in_groups(&["staff"])
}

#[test]
fn an_application_with_no_version_is_404_and_runs_no_code() {
    let vm = vm();
    let api = vm.api_as(&alice());
    api.create_application("board").expect_status(201);
    assert!(api.application("board")["serving_version"].is_null());
    assert_eq!(api.versions("board"), Vec::<Value>::new());
    assert_api_error(&api.get_version("board", 1), 404, "not-found");
    for user in [alice(), bob()] {
        let response = vm.get_as(&user, "board", "/");
        response.expect_status(404);
        let records = vm.records_of(&response);
        records.assert_no_application_code_ran();
        let record = &records.request;
        assert!(
            record["application"].is_null() && record.get("version").is_some_and(Value::is_null),
            "expected `application: null` and `version: null` for a 404, got {record}"
        );
        assert_eq!(record["user"], user.id.as_str(), "user in {record}");
    }
}

#[test]
fn once_the_first_version_is_sent_requests_are_served_by_it() {
    let vm = vm();
    let api = vm.api_as(&alice());
    api.create_application("board").expect_status(201);
    api.send_bundle("board", &marked_bundle("first"))
        .expect_status(201);
    assert_eq!(api.application("board")["serving_version"], 1);
    for user in [alice(), bob(), User::new("carol")] {
        let response = vm.get_as(&user, "board", "/");
        assert_served_by(&vm, &response, "board", "first", 1);
        assert_eq!(response.json()["user"]["id"], user.id.as_str());
    }
}

#[test]
fn the_second_version_takes_over_the_first_is_unchanged_and_both_are_listed() {
    let vm = vm();
    let api = vm.api_as(&alice());
    let first = api.create_with_bundle("board", &marked_bundle("first"));
    let application_before = api.application("board");
    let first_read_before = api.get_version("board", 1);
    first_read_before.expect_status(200);

    let second = api.send_bundle("board", &marked_bundle("second"));
    second.expect_status(201);
    let second = second.json();
    assert_eq!(second["number"], 2, "number in {second}");

    let first_read_after = api.get_version("board", 1);
    first_read_after.expect_status(200);
    assert_eq!(
        first_read_after.text(),
        first_read_before.text(),
        "expected the first Version's record to be byte for byte what it was before the second was sent"
    );
    assert_eq!(
        first_read_after.json(),
        first,
        "and the object its 201 answered"
    );
    assert_eq!(
        api.versions("board"),
        vec![first, second.clone()],
        "both, oldest first"
    );
    assert_eq!(api.get_version("board", 2).json(), second);

    let mut application_after = api.application("board");
    assert_eq!(application_after["serving_version"], 2);
    application_after["serving_version"] = application_before["serving_version"].clone();
    assert_eq!(
        application_after, application_before,
        "expected only serving_version to change in the Application's record"
    );
    assert_served_by(&vm, &vm.get_as(&bob(), "board", "/"), "board", "second", 2);
}

#[test]
fn every_request_sent_after_the_201_is_served_by_the_new_version() {
    let vm = vm();
    let api = vm.api_as(&alice());
    api.create_with_bundle("board", &marked_bundle("v1"));
    for n in 2..=8_u64 {
        let marker = format!("v{n}");
        api.send_bundle("board", &marked_bundle(&marker))
            .expect_status(201);
        let response = vm.get_as(&bob(), "board", "/");
        assert_served_by(&vm, &response, "board", &marker, n);
    }
}

#[test]
fn a_request_in_flight_finishes_on_the_version_it_started_on() {
    let vm = vm();
    let api = vm.api_as(&alice());
    api.create_with_bundle("board", &marked_bundle("first"));
    let token = "in-flight-token-1";
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
        let sent = api.send_bundle("board", &marked_bundle("second"));
        sent.expect_status(201);
        let received_201 = Instant::now();
        let after = vm.get_as(&User::new("carol"), "board", "/");
        assert_served_by(&vm, &after, "board", "second", 2);

        let (slow_response, finished) = in_flight.join().unwrap();
        assert!(
            finished > received_201,
            "the slow request ended before the second Version's 201 was received, so this test showed nothing: raise SLOW_MS"
        );
        assert_eq!(
            request_of(&started),
            Some(slow_response.request_id().as_str()),
            "expected the `started` line to belong to the slow request, got {started}"
        );
        assert_served_by(&vm, &slow_response, "board", "first", 1);
    });
}

#[test]
fn under_steady_traffic_no_request_sent_after_a_201_is_served_by_an_older_version() {
    let vm = vm();
    let api = vm.api_as(&alice());
    api.create_with_bundle("board", &marked_bundle("n1"));
    const SENDERS: usize = 4;
    const MAX_REQUESTS_EACH: usize = 400;
    let stop = AtomicBool::new(false);
    let (served, received): (Vec<Served>, Vec<(u64, Instant)>) = thread::scope(|scope| {
        let traffic: Vec<_> = (0..SENDERS)
            .map(|i| {
                let (vm, stop) = (&vm, &stop);
                scope.spawn(move || {
                    let user = User::new(&format!("reader-{i}"));
                    let mut served = Vec::with_capacity(MAX_REQUESTS_EACH);
                    while !stop.load(Ordering::Relaxed) && served.len() < MAX_REQUESTS_EACH {
                        let sent_at = Instant::now();
                        served.push((sent_at, vm.get_as(&user, "board", "/")));
                    }
                    served
                })
            })
            .collect();
        let mut received = Vec::new();
        for n in 2..=6_u64 {
            thread::sleep(Duration::from_millis(100));
            api.send_bundle("board", &marked_bundle(&format!("n{n}")))
                .expect_status(201);
            received.push((n, Instant::now()));
        }
        thread::sleep(Duration::from_millis(100));
        stop.store(true, Ordering::Relaxed);
        let served = traffic
            .into_iter()
            .flat_map(|t| t.join().unwrap())
            .collect();
        (served, received)
    });
    assert!(
        served.len() > 20,
        "expected steady traffic, got {} requests",
        served.len()
    );
    for (sent_at, response) in &served {
        response.expect_status(200);
        let body = response.json();
        let version = body["whoami"]["ok"]["version"]
            .as_u64()
            .unwrap_or_else(|| panic!("expected whoami to name the Version, got {body}"));
        assert_eq!(
            body["marker"],
            format!("n{version}"),
            "expected the bundle of the Version whoami names, got {body}"
        );
        let newest_before = received
            .iter()
            .filter(|(_, at)| at < sent_at)
            .map(|(n, _)| *n)
            .max()
            .unwrap_or(1);
        assert!(
            version >= newest_before,
            "a request sent after the 201 of Version {newest_before} was served by Version {version}"
        );
    }
}

#[test]
fn a_bundle_that_throws_at_load_is_still_a_version_and_its_requests_get_the_fixed_500() {
    let vm = vm();
    vm.install("errors", &fixture_bundle("errors.js"));
    let api = vm.api_as(&alice());
    let version = api.create_with_bundle("broken", &fixture_bundle("throws-at-load.js"));
    assert_eq!(
        version["number"], 1,
        "expected the failing bundle to be Version 1: {version}"
    );
    assert_eq!(api.versions("broken"), vec![version]);
    assert_eq!(api.application("broken")["serving_version"], 1);

    let response = vm.get_as(&bob(), "broken", "/");
    response.expect_status(500);
    let fixed = vm.get_as(&bob(), "errors", "/throw");
    fixed.expect_status(500);
    assert_eq!(
        response.text(),
        fixed.text(),
        "expected the program's fixed 500 body for a bundle that fails at load"
    );
    let records = vm.records_of(&response);
    assert_eq!(
        records.request["version"], 1,
        "version in {}",
        records.request
    );
    assert_eq!(records.request["application"], "broken");
    assert_eq!(
        records.uncaught().len(),
        1,
        "expected one `uncaught` record: {:?}",
        records.all
    );

    api.send_bundle("broken", &marked_bundle("fixed"))
        .expect_status(201);
    assert_served_by(&vm, &vm.get_as(&bob(), "broken", "/"), "broken", "fixed", 2);
}

#[test]
fn a_bundle_that_is_not_javascript_is_still_a_version_and_its_requests_get_the_fixed_500() {
    let vm = vm();
    let api = vm.api_as(&alice());
    for (n, bundle) in [
        "this is not JavaScript {",
        "",
        "export default { fetch() { return new Response('a module, not a classic script'); } };",
    ]
    .iter()
    .enumerate()
    {
        let name = format!("not-a-script-{n}");
        let version = api.create_with_bundle(&name, bundle);
        assert_eq!(version["number"], 1, "in {version}");
        let response = vm.get_as(&bob(), &name, "/");
        assert_eq!(
            response.status,
            500,
            "expected the fixed 500 for the bundle {bundle:?}, got\n{}",
            response.describe()
        );
        assert_eq!(vm.records_of(&response).request["version"], 1);
    }
}
