//! The rows of "A write whose answer is lost" for Grants (CONTRACT.md,
//! "Resources and Grants in full: A write whose answer is lost", stage
//! three), with the stand-in sign-in:
//!
//! - a revoke held and landing, of the Grant written last on the Resource
//!   and of one that was not: `grants-unknown` meanwhile, the listings
//!   (`GET /v1/admin/grants` too) `503`, `no-grant` afterwards with no
//!   restart and within a bounded poll, the Grant in no listing; a revoke
//!   held and not landing: the operation runs afterwards;
//! - each leaves one `grant-unsettled` record (`asked` `revoke`, `access`
//!   null, `by`) and no `grant-revoked`; a setting held and landing leaves
//!   one (`asked` `set`, its `access`) and no `grant-set`, and `granted_by`
//!   is who asked; a request given up by its sender while its commit is
//!   held leaves the same record;
//! - asking again is safe (`changed` false); a `503` answered before
//!   anything was sent writes no `grant-unsettled`.
//!
//! Two rigs make the write's answer lost, as in `resource_lost_answer`: a
//! Postgres whose commits are held (the write lands after its `503`) and a
//! proxy in front of the platform database that holds the write's
//! connection and drops it (the write never lands). These tests are slow
//! by nature (a write's limit is 10 s or more).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::lost_answer::{HeldPostgres, LOST_WRITE_TIMEOUT, SETTLE, host_call_refused, quickly};
use blackbox::outbound_surface::DbProxy;
use blackbox::postgres::{Platform, PrivatePostgres};
use blackbox::resources::{
    admin_resource, body, connection, encode_segment, grants_of_group, member_names, reader_direct,
    records_waited, revoke, writer_direct,
};
use blackbox::scenario::{
    ADMINISTRATORS, CONTACT, Scenario, administrator, assert_executed, assert_refusal, assert_rows,
    op, sql_on,
};
use blackbox::shared_database::{Login, token};
use blackbox::{Request, Response, User, Vm, VmOptions, assert_api_error};
use serde_json::{Value, json};

const RESOURCE: &str = "devops";
const CREW: &str = "crew";
const OTHERS: &str = "others";

fn crew(id: &str) -> User {
    User::new(id).in_groups(&[CREW])
}

fn other_administrator() -> User {
    User::new("other-admin").in_groups(&[ADMINISTRATORS])
}

/// `devops` over a read and a write login of their own, these Grants given
/// in this order (so that the last one is the Grant written last), and
/// `board` declaring it at `write`.
struct Setting {
    s: Scenario,
    reader: Login,
}

impl Setting {
    fn new(platform: &Platform, grants: &[(&str, &str)]) -> Self {
        let s = Scenario::start_with(&VmOptions::new().platform(platform));
        let reader = reader_direct(&s.db);
        let writer = writer_direct(&s.db);
        s.register_body(
            RESOURCE,
            &body(RESOURCE, &connection(&s.db, &reader, Some(&writer), None)),
        );
        for (group, access) in grants {
            s.grant(RESOURCE, group, access);
        }
        s.install("board", "operations.js", &[(RESOURCE, "write")]);
        Self { s, reader }
    }

    fn vm(&self) -> &Vm {
        &self.s.vm
    }
}

fn marker(what: &str) -> String {
    format!("{what}-{}", token())
}

fn insert(marker: &str) -> Value {
    sql_on(
        RESOURCE,
        &format!("insert into notes (body) values ('{marker}') returning body"),
    )
}

fn who() -> Value {
    sql_on(RESOURCE, "select current_user as who")
}

fn call_request(vm: &Vm, user: &User, call: &Value) -> Request {
    vm.request("board", "/")
        .as_user(user)
        .method("POST")
        .json(&json!({ "calls": [call] }))
        .timeout(LOST_WRITE_TIMEOUT)
}

fn answer_of(vm: &Vm, response: &Response, context: &str) -> Value {
    if response.status != 200 {
        vm.fail(&format!(
            "{context}: expected `200` from the Application with the host's answer, got\n{}",
            response.describe()
        ));
    }
    response.json()["answers"][0].clone()
}

fn call(vm: &Vm, user: &User, call: &Value, context: &str) -> Value {
    let response = call_request(vm, user, call).send();
    answer_of(vm, &response, context)
}

fn lost(answer: Result<Response, String>, what: &str) {
    let answer =
        answer.unwrap_or_else(|e| panic!("{what}: expected `503` `unavailable`, no answer ({e})"));
    assert_api_error(&answer, 503, "unavailable");
}

fn lose_held(postgres: &HeldPostgres, request: Request, what: &str) {
    postgres.hold();
    let waiting = postgres.waiting() + 1;
    thread::scope(|scope| {
        let sending = scope.spawn(move || request.try_send());
        postgres.wait_for_waiting(waiting, what);
        lost(sending.join().unwrap(), what);
    });
    postgres.assert_waiting(waiting, what);
}

fn lose_dropped(proxy: &DbProxy, needle: &str, request: &Request, what: &str) {
    proxy.arm(needle);
    let began = Instant::now();
    let answer = request
        .try_send()
        .unwrap_or_else(|e| panic!("{what}: expected `503` `unavailable`, no answer ({e})"));
    assert_api_error(&answer, 503, "unavailable");
    assert!(
        began.elapsed() < Duration::from_secs(40),
        "{what}: expected the `503` within the program's own limit, it took {:?}",
        began.elapsed()
    );
    assert_eq!(
        proxy.held(),
        1,
        "{what}: the harness's own check: expected the proxy to hold the write's connection ({needle:?} in a chunk), it held {}; first bytes: {:?}",
        proxy.held(),
        proxy.held_chunks()
    );
}

fn given_up(request: Request, wait: Duration, what: &str) {
    let answer = request.timeout(wait).try_send();
    assert!(
        answer.is_err(),
        "the harness's own check, {what}: expected no answer within {wait:?} (the write is held), got {:?}",
        answer.map(|a| a.status)
    );
}

fn give_up_held(postgres: &HeldPostgres, request: Request, what: &str) {
    postgres.hold();
    let waiting = postgres.waiting() + 1;
    thread::scope(|scope| {
        let sending = scope.spawn(move || given_up(request, Duration::from_secs(4), what));
        postgres.wait_for_waiting(waiting, what);
        sending.join().unwrap();
    });
    postgres.assert_waiting(waiting, what);
}

fn behind_a_proxy(name: &str) -> (PrivatePostgres, DbProxy, Platform) {
    let postgres = PrivatePostgres::start().expect("a Postgres of the test's own");
    let target = format!("{}:{}", postgres.host(), postgres.port())
        .parse()
        .expect("the Postgres's address");
    let proxy = DbProxy::start(target);
    let platform = Platform::on_address(&postgres, name, proxy.address());
    (postgres, proxy, platform)
}

fn lost_revoke(vm: &Vm, group: &str) -> Request {
    vm.api_as(&administrator())
        .request(&format!(
            "/v1/resources/{RESOURCE}/grants/{}",
            encode_segment(group)
        ))
        .method("DELETE")
        .timeout(LOST_WRITE_TIMEOUT)
}

fn lost_set(vm: &Vm, by: &User, group: &str, access: &str) -> Request {
    vm.api_as(by)
        .request(&format!("/v1/resources/{RESOURCE}/grants"))
        .method("POST")
        .json(&json!({ "group": group, "access": access }))
        .timeout(LOST_WRITE_TIMEOUT)
}

/// `GET` of what shows the Grants: each `503` `unavailable`, at once.
fn assert_not_shown_meanwhile(vm: &Vm, group: &str, what: &str) {
    let admin = vm.api_as(&administrator());
    let member = vm.api_as(&User::new("member-api").in_groups(&[group]));
    for (api, path) in [
        (&admin, format!("/v1/resources/{RESOURCE}/grants")),
        (&admin, "/v1/admin/resources".to_owned()),
        (&admin, format!("/v1/admin/resources/{RESOURCE}")),
        (
            &admin,
            format!("/v1/admin/grants?group={}", encode_segment(group)),
        ),
        (&member, "/v1/resources".to_owned()),
        (&member, format!("/v1/resources/{RESOURCE}")),
    ] {
        let answer = quickly(vm, &api.request(&path), &format!("{what}: GET {path}"));
        assert_api_error(&answer, 503, "unavailable");
    }
}

/// The first call as a User of its own that is not refused
/// `grants-unknown`, waiting up to [`SETTLE`].
fn settled(vm: &Vm, group: &str, call_of: &Value, what: &str) -> Value {
    let deadline = Instant::now() + SETTLE;
    loop {
        let user = User::new(&format!("settled-{}", token())).in_groups(&[group]);
        let answer = call(vm, &user, call_of, what);
        let unknown = answer["refused"]["refusal"]["reason"] == "grants-unknown";
        if !unknown || Instant::now() >= deadline {
            return answer;
        }
        thread::sleep(Duration::from_millis(300));
    }
}

/// Sends the request `make` builds until it is answered something but `503`
/// (allowed for a short while: the program is finding the Resource out),
/// within 5 s. A `409` at any point fails.
fn asked_again(make: &dyn Fn() -> Request, what: &str) -> Response {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let answer = make().send();
        assert_ne!(
            answer.status,
            409,
            "{what}: expected never `409` because of the program's own earlier write, got\n{}",
            answer.describe()
        );
        if answer.status != 503 || Instant::now() >= deadline {
            return answer;
        }
        thread::sleep(Duration::from_millis(250));
    }
}

/// The groups of the Grants the administrators' listing shows now.
fn groups_listed(vm: &Vm) -> Vec<String> {
    let answer = vm.api_as(&administrator()).list_grants(RESOURCE);
    answer.expect_status(200);
    answer.json()["grants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["group"].as_str().unwrap().to_owned())
        .collect()
}

/// Exactly one `grant-unsettled` record, as the contract gives it.
fn assert_unsettled(vm: &Vm, group: &str, asked: &str, access: &Value, by: &str, what: &str) {
    let records = records_waited(vm, "grant-unsettled", 1);
    assert_eq!(
        records.len(),
        1,
        "{what}: one `grant-unsettled`: {records:?}"
    );
    let record = &records[0];
    assert_eq!(
        member_names(record),
        [
            "access", "asked", "by", "event", "group", "resource", "time_ms"
        ],
        "{what}: {record}"
    );
    assert_eq!(record["resource"], json!(RESOURCE), "{what}");
    assert_eq!(record["group"], json!(group), "{what}");
    assert_eq!(record["asked"], json!(asked), "{what}");
    assert_eq!(&record["access"], access, "{what}");
    assert_eq!(record["by"], json!(by), "{what}");
}

/// A revoke held and landing: the whole of the rows for it, for the Grant
/// of `CREW` among these, in this order.
fn a_revoke_held_and_landing(label: &str, grants: &[(&str, &str)]) {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform(&format!("vm_platform_lost_revoke_landed_{label}"));
    let t = Setting::new(&platform, grants);
    let vm = t.vm();
    let first = marker("before");
    let answer = call(
        vm,
        &crew("crew-before"),
        &op("execute", &insert(&first)),
        "before the revoke",
    );
    assert_executed(
        &answer,
        &["body"],
        &json!([[first]]),
        1,
        "the harness's own check: the Group's Grant works before the revoke",
    );

    lose_held(&postgres, lost_revoke(vm, CREW), "the revoke of the Grant");
    thread::sleep(Duration::from_millis(1500));
    assert_not_shown_meanwhile(vm, CREW, "while the revoke's commit is held");
    let write = marker("meanwhile");
    let user = crew("meanwhile-execute");
    let request = call_request(vm, &user, &op("execute", &insert(&write)));
    let (decided, answer) = thread::scope(|scope| {
        let sending = scope.spawn(move || request.try_send());
        let decided = host_call_refused(vm, "meanwhile-execute", "grants-unknown");
        (decided, sending.join().unwrap())
    });
    decided.unwrap_or_else(|e| panic!("{e}"));
    assert_refusal(
        &answer_of(vm, &answer.unwrap(), "a call meanwhile"),
        "grants-unknown",
        &json!({ "resource": RESOURCE }),
        "a write, meanwhile, on a Resource whose Grant is being revoked: never allowed on a guess",
    );
    t.s.db
        .assert_not_received(&write, "an execute made while the revoke is unknown");

    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let after = settled(
        vm,
        CREW,
        &op("query", &who()),
        "after the revoke landed, with no restart",
    );
    assert_refusal(
        &after,
        "no-grant",
        &json!({ "resource": RESOURCE, "user": after["refused"]["refusal"]["user"], "needed": "read", "held": null, "contact": CONTACT }),
        "a read once the revoke landed",
    );
    let groups = groups_listed(vm);
    assert!(!groups.contains(&CREW.to_owned()), "{groups:?}");
    let by_group = grants_of_group(&vm.api_as(&administrator()), CREW);
    by_group.expect_status(200);
    assert_eq!(by_group.json()["grants"], json!([]), "in no listing");
    let one = admin_resource(&vm.api_as(&administrator()), RESOURCE).json();
    assert!(
        one["grants"]
            .as_array()
            .unwrap()
            .iter()
            .all(|g| g["group"] != CREW),
        "{one}"
    );
    // The other Group's Grant is untouched.
    let kept = call(
        vm,
        &User::new("kept").in_groups(&[OTHERS]),
        &op("query", &who()),
        "another Group's Grant",
    );
    assert_rows(
        &kept,
        &["who"],
        &json!([[t.reader.role]]),
        "another Group's Grant after the revoke landed",
    );
    assert_unsettled(
        vm,
        CREW,
        "revoke",
        &Value::Null,
        &administrator().id,
        "a revoke that landed",
    );
    assert_eq!(
        records_waited(vm, "grant-revoked", 0).len(),
        0,
        "no `grant-revoked`: the program does not know that it was made"
    );
    // Asking again is safe.
    let again = asked_again(&|| lost_revoke(vm, CREW), "the revoke asked again");
    again.expect_status(200);
    assert_eq!(
        again.json(),
        json!({ "resource": RESOURCE, "group": CREW, "changed": false, "was": null })
    );
}

/// Takes 40 s or more.
#[test]
fn a_revoke_of_the_grant_written_last_never_serves_on_a_guess_and_lands_with_no_restart() {
    a_revoke_held_and_landing("last", &[(OTHERS, "read"), (CREW, "write")]);
}

/// Takes 40 s or more.
#[test]
fn a_revoke_of_a_grant_that_was_not_written_last_lands_with_no_restart_all_the_same() {
    a_revoke_held_and_landing("not_last", &[(CREW, "write"), (OTHERS, "read")]);
}

/// Takes about 10 s for the write's limit, and up to 15 s more.
#[test]
fn a_revoke_that_never_lands_leaves_the_grant_and_the_operation_runs() {
    let (_postgres, proxy, platform) = behind_a_proxy("vm_platform_lost_revoke_dropped");
    let t = Setting::new(&platform, &[(OTHERS, "read"), (CREW, "write")]);
    let vm = t.vm();
    let group = format!("gone-{}", token());
    t.s.grant(RESOURCE, &group, "write");
    let before = vm.api_as(&administrator()).list_grants(RESOURCE).json();
    lose_dropped(
        &proxy,
        &group,
        &lost_revoke(vm, &group),
        "the revoke whose connection is held",
    );
    proxy.fail();
    let user_group = group.clone();
    let write = marker("after");
    let answer = settled(
        vm,
        &user_group,
        &op("execute", &insert(&write)),
        "once the revoke did not land",
    );
    assert_executed(
        &answer,
        &["body"],
        &json!([[write]]),
        1,
        "a write once the revoke did not land: the Grant is in force",
    );
    assert_eq!(
        vm.api_as(&administrator()).list_grants(RESOURCE).json(),
        before,
        "expected the Grants as they were"
    );
    assert_unsettled(
        vm,
        &group,
        "revoke",
        &Value::Null,
        &administrator().id,
        "a revoke that did not land",
    );
    assert_eq!(records_waited(vm, "grant-revoked", 0).len(), 0);
}

/// Takes 40 s or more.
#[test]
fn a_setting_that_lands_after_its_503_leaves_one_unsettled_record_and_the_asker_in_granted_by() {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_lost_set_landed");
    let t = Setting::new(&platform, &[(OTHERS, "read")]);
    let vm = t.vm();
    let new_group = "newcomers";
    let asker = other_administrator();
    lose_held(
        &postgres,
        lost_set(vm, &asker, new_group, "write"),
        "the setting of a Grant",
    );
    thread::sleep(Duration::from_millis(1500));
    assert_not_shown_meanwhile(vm, new_group, "while the setting's commit is held");
    let user = User::new("meanwhile-new").in_groups(&[new_group]);
    let write = marker("meanwhile");
    let request = call_request(vm, &user, &op("execute", &insert(&write)));
    let (decided, answer) = thread::scope(|scope| {
        let sending = scope.spawn(move || request.try_send());
        let decided = host_call_refused(vm, "meanwhile-new", "grants-unknown");
        (decided, sending.join().unwrap())
    });
    decided.unwrap_or_else(|e| panic!("{e}"));
    assert_refusal(
        &answer_of(vm, &answer.unwrap(), "a call meanwhile"),
        "grants-unknown",
        &json!({ "resource": RESOURCE }),
        "a write meanwhile, on a Resource whose Grant is being set",
    );

    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let marker_after = marker("after");
    let after = settled(
        vm,
        new_group,
        &op("execute", &insert(&marker_after)),
        "after the setting landed",
    );
    assert_executed(
        &after,
        &["body"],
        &json!([[marker_after]]),
        1,
        "a write once the setting landed, with no restart",
    );
    let listed = vm.api_as(&administrator()).list_grants(RESOURCE).json();
    let entry = listed["grants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["group"] == new_group)
        .unwrap_or_else(|| panic!("expected the Grant in the list, got {listed}"))
        .clone();
    assert_eq!(entry["access"], json!("write"));
    assert_eq!(
        entry["granted_by"],
        json!(asker.id),
        "`granted_by` is who asked, though the answer was lost: {entry}"
    );
    assert_unsettled(
        vm,
        new_group,
        "set",
        &json!("write"),
        &asker.id,
        "a setting that landed",
    );
    assert_eq!(
        records_waited(vm, "grant-set", 1).len(),
        1,
        "the one `grant-set` is the earlier setting of the harness's own, none for the lost one"
    );
    // Asking again is safe: the Grant is the same Grant.
    let again = asked_again(
        &|| lost_set(vm, &asker, new_group, "write"),
        "the setting asked again",
    );
    again.expect_status(200);
    assert_eq!(
        again.json(),
        json!({ "resource": RESOURCE, "group": new_group, "access": "write", "changed": false, "was": "write" })
    );
}

/// Takes 40 s or more.
#[test]
fn a_revoke_and_a_setting_given_up_by_their_sender_leave_the_same_record() {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_given_up_grants");
    let t = Setting::new(&platform, &[(OTHERS, "read"), (CREW, "write")]);
    let vm = t.vm();
    give_up_held(
        &postgres,
        lost_revoke(vm, CREW),
        "the revoke, given up by its sender",
    );
    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let after = settled(
        vm,
        CREW,
        &op("query", &who()),
        "once the revoke that was given up landed",
    );
    assert_eq!(
        after["refused"]["refusal"]["reason"],
        json!("no-grant"),
        "{after}"
    );
    assert_unsettled(
        vm,
        CREW,
        "revoke",
        &Value::Null,
        &administrator().id,
        "a revoke given up",
    );

    // A setting given up: the same.
    let asker = other_administrator();
    give_up_held(
        &postgres,
        lost_set(vm, &asker, "late", "read"),
        "the setting, given up by its sender",
    );
    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let late = settled(
        vm,
        "late",
        &op("query", &who()),
        "once the setting that was given up landed",
    );
    assert_rows(
        &late,
        &["who"],
        &json!([[t.reader.role]]),
        "a read once the setting that was given up landed",
    );
    let records = records_waited(vm, "grant-unsettled", 2);
    assert_eq!(records.len(), 2, "{records:?}");
    let set = records.iter().find(|r| r["asked"] == "set").unwrap();
    assert_eq!(set["group"], json!("late"));
    assert_eq!(set["access"], json!("read"));
    assert_eq!(set["by"], json!(asker.id));
    assert_eq!(records_waited(vm, "grant-revoked", 0).len(), 0);
}

/// Takes 30 s or more.
#[test]
fn a_write_behind_the_programs_own_lost_write_is_503_at_once_and_writes_no_record() {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_lost_grant_again");
    let t = Setting::new(&platform, &[(OTHERS, "read"), (CREW, "write")]);
    let vm = t.vm();
    lose_held(
        &postgres,
        lost_revoke(vm, CREW),
        "the first revoke of the Grant",
    );
    let began = Instant::now();
    let second = lost_set(vm, &administrator(), "behind", "read")
        .try_send()
        .unwrap_or_else(|e| {
            panic!("the setting behind the revoke: expected `503`, no answer ({e})")
        });
    assert_api_error(&second, 503, "unavailable");
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "expected a write of a Resource the program is finding out to be answered `503` at once (nothing written), it took {:?}",
        began.elapsed()
    );
    let third = lost_revoke(vm, OTHERS).try_send().unwrap();
    assert_api_error(&third, 503, "unavailable");
    assert_unsettled(
        vm,
        CREW,
        "revoke",
        &Value::Null,
        &administrator().id,
        "only the first write may have been made: a `503` answered before anything was sent writes none",
    );

    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    // The two that were answered at once were never written.
    let asked = asked_again(
        &|| lost_set(vm, &administrator(), "behind", "read"),
        "the setting that was refused at once, asked again",
    );
    asked.expect_status(200);
    assert_eq!(
        asked.json()["was"],
        Value::Null,
        "it was never written: {}",
        asked.describe()
    );
    assert_eq!(asked.json()["changed"], json!(true));
    let groups = groups_listed(vm);
    assert_eq!(
        groups,
        ["behind", OTHERS],
        "the first revoke landed, the third never did"
    );
    assert_eq!(
        revoke(&vm.api_as(&administrator()), RESOURCE, OTHERS).status,
        200
    );
}
