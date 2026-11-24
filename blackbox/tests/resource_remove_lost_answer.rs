//! The rows of "A write whose answer is lost" for removing a Resource and
//! for changing a Storage Resource's location (CONTRACT.md, "Resources and
//! Grants in full: A write whose answer is lost"), with the stand-in
//! sign-in:
//!
//! - a removal held and landing: `not-registered` afterwards, with no
//!   restart, and the old logins never used after the `503`; a removal held
//!   and not landing: the Resource is served afterwards;
//! - a location change held and landing: operations use the new directory
//!   afterwards; held and not landing: the old one, and nothing is left in
//!   the new directory;
//! - meanwhile: `grants-unknown`, the `503`s, nothing sent to the shared
//!   database or the directory;
//! - asking again is safe (`"changed": false`, `404`); a write of a Resource
//!   the program is still finding out is `503` at once;
//!
//! (A write that finds no row is `404` and lets the Resource go: its
//! tests are deterministic and are in `resource_remove`.)
//!
//! Two rigs make the write's answer lost, as in `resource_lost_answer`: a
//! Postgres whose commits are held (the write **lands** after its `503`)
//! and a proxy in front of the platform database that holds the write's
//! connection and then drops it (the write **never lands**). These tests
//! are slow by nature (a write's limit is 10 s or more).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use blackbox::lost_answer::{HeldPostgres, LOST_WRITE_TIMEOUT, SETTLE, host_call_refused, quickly};
use blackbox::outbound_surface::DbProxy;
use blackbox::postgres::{Platform, PrivatePostgres};
use blackbox::resources::{
    admin_resource, body, connection, connections_of, location_body, reader_direct, records_waited,
    sent_by_login, writer_direct,
};
use blackbox::scenario::{
    Scenario, administrator, assert_executed, assert_refusal, assert_rows, op, sql_on,
};
use blackbox::server_log::log_everything;
use blackbox::shared_database::{Login, token};
use blackbox::sql::Sql;
use blackbox::storage::{FILE_WRITERS, FILES, MARKETING, Place, Storage, put, tree};
use blackbox::{Request, Response, User, Vm, VmOptions, assert_api_error};
use serde_json::{Value, json};

const RESOURCE: &str = "devops";
const CREW: &str = "crew";

fn crew(id: &str) -> User {
    User::new(id).in_groups(&[CREW])
}

fn filer(id: &str) -> User {
    User::new(id).in_groups(&[FILE_WRITERS])
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

fn call_request(vm: &Vm, application: &str, user: &User, call: &Value) -> Request {
    vm.request(application, "/")
        .as_user(user)
        .method("POST")
        .json(&json!({ "calls": [call] }))
        .timeout(LOST_WRITE_TIMEOUT)
}

/// One call to `application` as `user`, which must be answered `200`.
fn call(vm: &Vm, application: &str, user: &User, call: &Value, context: &str) -> Value {
    let response = call_request(vm, application, user, call).send();
    answer_of(vm, &response, context)
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

fn lost(answer: Result<Response, String>, what: &str) {
    let answer =
        answer.unwrap_or_else(|e| panic!("{what}: expected `503` `unavailable`, no answer ({e})"));
    assert_api_error(&answer, 503, "unavailable");
}

/// Holds commits and sends `request`: answered `503` `unavailable` with its
/// commit waiting.
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

/// Sends `request` with the proxy armed for `needle`: the write's
/// connection is held from the chunk with the needle on, and the request is
/// answered `503` `unavailable`.
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

fn behind_a_proxy(name: &str) -> (PrivatePostgres, DbProxy, Platform) {
    let postgres = PrivatePostgres::start().expect("a Postgres of the test's own");
    let target = format!("{}:{}", postgres.host(), postgres.port())
        .parse()
        .expect("the Postgres's address");
    let proxy = DbProxy::start(target);
    let platform = Platform::on_address(&postgres, name, proxy.address());
    (postgres, proxy, platform)
}

/// `GET` of what shows the Resource: each `503` `unavailable`, at once.
fn assert_not_shown_meanwhile(vm: &Vm, resource: &str, group: &str, what: &str) {
    let admin = vm.api_as(&administrator());
    let member = vm.api_as(&User::new("member-api").in_groups(&[group]));
    for (api, path) in [
        (&admin, format!("/v1/resources/{resource}/grants")),
        (&admin, "/v1/admin/resources".to_owned()),
        (&admin, format!("/v1/admin/resources/{resource}")),
        (&member, "/v1/resources".to_owned()),
        (&member, format!("/v1/resources/{resource}")),
    ] {
        let answer = quickly(vm, &api.request(&path), &format!("{what}: GET {path}"));
        assert_api_error(&answer, 503, "unavailable");
    }
}

/// The first call as a User of its own that is not refused
/// `grants-unknown`, waiting up to [`SETTLE`].
fn settled(vm: &Vm, application: &str, group: &str, call_of: &Value, what: &str) -> Value {
    let deadline = Instant::now() + SETTLE;
    loop {
        let user = User::new(&format!("settled-{}", token())).in_groups(&[group]);
        let answer = call(vm, application, &user, call_of, what);
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

fn lost_delete(vm: &Vm, name: &str) -> Request {
    vm.api_as(&administrator())
        .request(&format!("/v1/resources/{name}"))
        .method("DELETE")
        .timeout(LOST_WRITE_TIMEOUT)
}

fn lost_location(vm: &Vm, name: &str, directory: &str) -> Request {
    vm.api_as(&administrator())
        .request(&format!("/v1/resources/{name}/location"))
        .method("PUT")
        .json(&location_body(directory))
        .timeout(LOST_WRITE_TIMEOUT)
}

// ---------------------------------------------------------------------------
// A database Resource
// ---------------------------------------------------------------------------

/// `devops` registered with a read and a write login of their own, [`CREW`]
/// granted `write`, and `board` declaring it at `write`.
struct Setting {
    s: Scenario,
    reader: Login,
    writer: Login,
}

impl Setting {
    fn new(platform: &Platform) -> Self {
        let s = Scenario::start_with(&VmOptions::new().platform(platform));
        let reader = reader_direct(&s.db);
        let writer = writer_direct(&s.db);
        s.register_body(
            RESOURCE,
            &body(RESOURCE, &connection(&s.db, &reader, Some(&writer), None)),
        );
        s.grant(RESOURCE, CREW, "write");
        s.install("board", "operations.js", &[(RESOURCE, "write")]);
        Self { s, reader, writer }
    }

    fn vm(&self) -> &Vm {
        &self.s.vm
    }
}

/// Takes 40 s or more: the program's limit on the write, then on the
/// entries of the operations made meanwhile.
#[test]
fn a_removal_whose_answer_is_lost_never_serves_on_a_guess_and_lands_as_not_registered_with_no_restart()
 {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_lost_remove_landed");
    let t = Setting::new(&platform);
    let (vm, db) = (t.vm(), &t.s.db);
    log_everything(db.server());
    let first = marker("before");
    let answer = call(
        vm,
        "board",
        &crew("crew-before"),
        &op("execute", &insert(&first)),
        "before the removal",
    );
    assert_executed(
        &answer,
        &["body"],
        &json!([[first]]),
        1,
        "the harness's own check: the Resource serves before the removal",
    );

    lose_held(
        &postgres,
        lost_delete(vm, RESOURCE),
        "the removal of the Resource",
    );
    thread::sleep(Duration::from_millis(1500));
    let sent = (
        sent_by_login(db, &t.reader).len(),
        sent_by_login(db, &t.writer).len(),
        connections_of(db, &t.reader),
        connections_of(db, &t.writer),
    );
    assert_not_shown_meanwhile(vm, RESOURCE, CREW, "while the removal's commit is held");
    let member = vm.api_as(&crew("member-pull"));
    let write = marker("meanwhile-execute");
    let calls: Vec<(User, Value)> = vec![
        (crew("meanwhile-execute"), op("execute", &insert(&write))),
        (crew("meanwhile-query"), op("query", &who())),
    ];
    let (decided, answers, pulled) = thread::scope(|scope| {
        let sending: Vec<_> = calls
            .iter()
            .map(|(user, c)| {
                let request = call_request(vm, "board", user, c);
                scope.spawn(move || request.try_send())
            })
            .collect();
        let pulling = scope.spawn(|| blackbox::structure::pull(&member, RESOURCE));
        let decided = vec![
            host_call_refused(vm, "meanwhile-execute", "grants-unknown"),
            host_call_refused(vm, "meanwhile-query", "grants-unknown"),
        ];
        let answers: Vec<Result<Response, String>> =
            sending.into_iter().map(|h| h.join().unwrap()).collect();
        (decided, answers, pulling.join().unwrap())
    });
    for d in decided {
        d.unwrap_or_else(|e| panic!("{e}"));
    }
    for answer in answers {
        let response = answer.unwrap_or_else(|e| vm.fail(&format!("a call meanwhile: {e}")));
        assert_refusal(
            &answer_of(vm, &response, "a call meanwhile"),
            "grants-unknown",
            &json!({ "resource": RESOURCE }),
            "a call, meanwhile, on a Resource being removed",
        );
    }
    assert_api_error(&pulled, 503, "unavailable");
    db.assert_not_received(&write, "an execute made while the removal is unknown");
    assert_eq!(
        (
            sent_by_login(db, &t.reader).len(),
            sent_by_login(db, &t.writer).len(),
            connections_of(db, &t.reader),
            connections_of(db, &t.writer)
        ),
        sent,
        "expected nothing sent to the shared database and no connection made while the removal is unknown"
    );

    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let after = settled(
        vm,
        "board",
        CREW,
        &op("query", &who()),
        "after the removal landed",
    );
    assert_refusal(
        &after,
        "not-registered",
        &json!({ "resource": RESOURCE }),
        "a query once the removal has landed, with no restart",
    );
    assert_api_error(
        &admin_resource(&vm.api_as(&administrator()), RESOURCE),
        404,
        "not-found",
    );
    assert_eq!(
        (
            sent_by_login(db, &t.reader).len(),
            sent_by_login(db, &t.writer).len(),
            connections_of(db, &t.reader),
            connections_of(db, &t.writer)
        ),
        sent,
        "expected the old logins never used after the `503`, also once the removal landed"
    );
    assert_eq!(
        records_waited(vm, "resource-removed", 1).len(),
        1,
        "the record of the removal"
    );
}

/// Takes about 10 s for the write's limit, and up to 15 s more.
#[test]
fn a_removal_that_never_lands_leaves_the_resource_served() {
    let (_postgres, proxy, platform) = behind_a_proxy("vm_platform_lost_remove_dropped");
    let t = Setting::new(&platform);
    let vm = t.vm();
    let view = admin_resource(&vm.api_as(&administrator()), RESOURCE).json();
    lose_dropped(
        &proxy,
        RESOURCE,
        &lost_delete(vm, RESOURCE),
        "the removal whose connection is held",
    );
    proxy.fail();
    let answer = settled(
        vm,
        "board",
        CREW,
        &op("query", &who()),
        "once the removal did not land",
    );
    assert_rows(
        &answer,
        &["who"],
        &json!([[t.reader.role]]),
        "a read once the removal did not land: the Resource is served as before",
    );
    assert_eq!(
        admin_resource(&vm.api_as(&administrator()), RESOURCE).json(),
        view,
        "expected the Resource, its Grants and its logins as they were"
    );
}

/// Takes 20 s or more: the program's limit on the first write.
#[test]
fn a_write_behind_the_programs_own_lost_removal_is_503_at_once_and_asked_again_is_404() {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_lost_remove_again");
    let t = Setting::new(&platform);
    let vm = t.vm();
    lose_held(
        &postgres,
        lost_delete(vm, RESOURCE),
        "the first removal of the Resource",
    );
    let began = Instant::now();
    let second = vm
        .api_as(&administrator())
        .request(&format!("/v1/resources/{RESOURCE}"))
        .method("PATCH")
        .json(&json!({ "contact": "Someone Else" }))
        .timeout(LOST_WRITE_TIMEOUT)
        .try_send()
        .unwrap_or_else(|e| {
            panic!("the change behind the removal: expected `503`, no answer ({e})")
        });
    assert_api_error(&second, 503, "unavailable");
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "expected a write of a Resource the program is finding out to be answered `503` at once (nothing written), it took {:?}",
        began.elapsed()
    );
    let again = lost_delete(vm, RESOURCE).try_send().unwrap();
    assert_api_error(&again, 503, "unavailable");
    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let asked = asked_again(&|| lost_delete(vm, RESOURCE), "the removal asked again");
    assert_api_error(&asked, 404, "not-found");
}

// ---------------------------------------------------------------------------
// A Storage Resource's location
// ---------------------------------------------------------------------------

/// [`MARKETING`] on `root/marketing`, [`FILE_WRITERS`] granted `write`,
/// [`FILES`] declaring it at `write`, on this platform database.
fn storage_on(platform: &Platform) -> Storage {
    let place = Place::new("lost");
    let root = place.root();
    let s = Storage::start_on(place, Some(&root), &VmOptions::new().platform(platform));
    s.register(MARKETING);
    s.grant(MARKETING, FILE_WRITERS, "write");
    s.install(FILES, "operations.js", &[(MARKETING, "storage", "write")]);
    s
}

fn new_directory(s: &Storage, token: &str) -> PathBuf {
    let dir = s.root().join(format!("moved-{token}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn put_call(key: &str) -> Value {
    put(MARKETING, key, "content", None)
}

/// Takes 40 s or more.
#[test]
fn a_location_change_whose_answer_is_lost_sends_nothing_to_the_directory_and_lands_with_no_restart()
{
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_lost_location_landed");
    let s = storage_on(&platform);
    let vm = &s.vm;
    let old = s.dir(MARKETING);
    let new = new_directory(&s, &token());
    let key = marker("meanwhile");

    lose_held(
        &postgres,
        lost_location(vm, MARKETING, &new.to_string_lossy()),
        "the change of the location",
    );
    assert_not_shown_meanwhile(
        vm,
        MARKETING,
        FILE_WRITERS,
        "while the change's commit is held",
    );
    let user = filer("meanwhile-put");
    let request = call_request(vm, FILES, &user, &put_call(&format!("{key}.txt")));
    let (decided, answer) = thread::scope(|scope| {
        let sending = scope.spawn(move || request.try_send());
        let decided = host_call_refused(vm, "meanwhile-put", "grants-unknown");
        (decided, sending.join().unwrap())
    });
    decided.unwrap_or_else(|e| panic!("{e}"));
    assert_refusal(
        &answer_of(vm, &answer.unwrap(), "a put meanwhile"),
        "grants-unknown",
        &json!({ "resource": MARKETING }),
        "a put meanwhile, on a Resource whose location is being changed",
    );
    for dir in [&old, &new] {
        assert!(
            !tree(dir).iter().any(|e| e.starts_with(&key)),
            "expected nothing sent to a directory meanwhile; {dir:?} holds {:?}",
            tree(dir)
        );
    }

    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let after = settled(
        vm,
        FILES,
        FILE_WRITERS,
        &put_call("after.txt"),
        "after the change landed",
    );
    assert!(after["ok"].is_object(), "{after}");
    assert!(
        new.join("after.txt").is_file(),
        "expected the put to use the new directory once the change landed, with no restart: {:?}",
        tree(&new)
    );
    assert!(!old.join("after.txt").exists());
    assert_eq!(
        admin_resource(&vm.api_as(&administrator()), MARKETING).json()["location"]["directory"],
        json!(new.to_string_lossy())
    );
    assert_eq!(records_waited(vm, "resource-location-changed", 1).len(), 1);
}

/// Takes about 10 s for the write's limit, and up to 15 s more.
#[test]
fn a_location_change_that_never_lands_leaves_the_old_directory_and_nothing_in_the_new_one() {
    let (_postgres, proxy, platform) = behind_a_proxy("vm_platform_lost_location_dropped");
    let s = storage_on(&platform);
    let vm = &s.vm;
    let old = s.dir(MARKETING);
    let t = token();
    let new = new_directory(&s, &t);
    lose_dropped(
        &proxy,
        &format!("moved-{t}"),
        &lost_location(vm, MARKETING, &new.to_string_lossy()),
        "the change of the location whose connection is held",
    );
    proxy.fail();
    let answer = settled(
        vm,
        FILES,
        FILE_WRITERS,
        &put_call("after.txt"),
        "once the change did not land",
    );
    assert!(answer["ok"].is_object(), "{answer}");
    assert!(
        old.join("after.txt").is_file(),
        "expected the old directory to serve once the change did not land: {:?}",
        tree(&old)
    );
    assert!(!new.join("after.txt").exists());
    // A change that is given up leaves nothing in the new directory.
    let clean = blackbox::storage::eventually(SETTLE, || {
        !tree(&new)
            .iter()
            .any(|e| e.starts_with(".vibe-resource") || e.starts_with(".vibe-staging"))
    });
    assert!(
        clean,
        "expected a change that never landed to leave nothing in the new directory: {:?}",
        tree(&new)
    );
    assert!(
        old.join(".vibe-resource").exists(),
        "the old directory keeps its marker"
    );
    assert_eq!(
        admin_resource(&vm.api_as(&administrator()), MARKETING).json()["location"]["directory"],
        json!(old.to_string_lossy())
    );
}

/// Takes 10 s or more.
#[test]
fn a_location_change_asked_again_after_its_503_is_200_changed_false_and_another_is_not_blocked() {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_lost_location_again");
    let s = storage_on(&platform);
    let vm = &s.vm;
    let new = new_directory(&s, &token());
    let other = new_directory(&s, &token());
    lose_held(
        &postgres,
        lost_location(vm, MARKETING, &new.to_string_lossy()),
        "the first change of the location",
    );
    // A write of a Resource the program is finding out: `503` at once.
    let began = Instant::now();
    let second = lost_location(vm, MARKETING, &other.to_string_lossy())
        .try_send()
        .unwrap();
    assert_api_error(&second, 503, "unavailable");
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "expected `503` at once, it took {:?}",
        began.elapsed()
    );
    assert!(
        !tree(&other).iter().any(|e| e.starts_with(".vibe-resource")),
        "expected nothing written for a write that was refused at once"
    );
    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let again = asked_again(
        &|| lost_location(vm, MARKETING, &new.to_string_lossy()),
        "the same change asked again",
    );
    assert_eq!(again.status, 200, "{}", again.describe());
    assert_eq!(
        again.json()["changed"],
        json!(false),
        "{}",
        again.describe()
    );
    // Another change goes through, with no `409` for the program's own
    // earlier write.
    let moved = asked_again(
        &|| lost_location(vm, MARKETING, &other.to_string_lossy()),
        "another change after the lost one",
    );
    assert_eq!(moved.status, 200, "{}", moved.describe());
    assert_eq!(moved.json()["changed"], json!(true));
}

// ---------------------------------------------------------------------------
// What the program does after an answer, it does when it finds out
// ("A write whose answer is lost": "What the program does after an answer...",
// "A request its sender gives up")
// ---------------------------------------------------------------------------

/// What `dir` holds but the program's own `.vibe-resource` and `.vibe-staging`.
fn files_of(dir: &std::path::Path) -> Vec<String> {
    tree(dir)
        .into_iter()
        .filter(|e| !e.starts_with(".vibe-resource") && !e.starts_with(".vibe-staging"))
        .collect()
}

fn the_programs_names_in(dir: &std::path::Path) -> Vec<String> {
    tree(dir)
        .into_iter()
        .filter(|e| e.starts_with(".vibe-resource") || e.starts_with(".vibe-staging"))
        .collect()
}

/// Waits up to [`SETTLE`] for `dir` to hold none of the program's names.
fn wait_cleaned(dir: &std::path::Path, what: &str) {
    let clean = blackbox::storage::eventually(SETTLE, || the_programs_names_in(dir).is_empty());
    assert!(
        clean,
        "{what}: expected the directory to hold no `.vibe-resource` and no empty `.vibe-staging/` once the program found out, it holds {:?}",
        tree(dir)
    );
}

/// The record `event`, exactly once, naming the Resource and who asked.
fn assert_one_record(vm: &Vm, event: &str, what: &str) {
    let records = records_waited(vm, event, 1);
    // A later record would be late: give it a moment, then count.
    thread::sleep(Duration::from_millis(500));
    let records: Vec<Value> = if records.len() == 1 {
        records_waited(vm, event, 1)
    } else {
        records
    };
    assert_eq!(
        records.len(),
        1,
        "{what}: expected exactly one `{event}` record, got {records:?}"
    );
    assert_eq!(
        records[0]["resource"],
        json!(MARKETING),
        "{what}: {records:?}"
    );
    assert_eq!(
        records[0]["by"],
        json!(administrator().id),
        "{what}: expected the record to name who asked, got {records:?}"
    );
}

/// Takes 40 s or more.
#[test]
fn a_location_change_found_to_have_landed_cleans_the_old_directory_and_writes_its_record_once() {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_found_location_landed");
    let s = storage_on(&platform);
    let vm = &s.vm;
    let old = s.dir(MARKETING);
    std::fs::write(old.join("kept.txt"), b"kept").unwrap();
    std::fs::create_dir_all(old.join("folder")).unwrap();
    std::fs::write(old.join("folder/b.txt"), b"also kept").unwrap();
    let files = files_of(&old);
    let new = new_directory(&s, &token());
    lose_held(
        &postgres,
        lost_location(vm, MARKETING, &new.to_string_lossy()),
        "the change of the location",
    );
    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let after = settled(
        vm,
        FILES,
        FILE_WRITERS,
        &put_call("found-out.txt"),
        "once the change landed",
    );
    assert!(after["ok"].is_object(), "{after}");
    wait_cleaned(&old, "the old directory of a change that landed");
    assert_eq!(
        files_of(&old),
        files,
        "expected the old directory's files untouched"
    );
    assert_one_record(
        vm,
        "resource-location-changed",
        "a change found to have landed",
    );
    s.register_on("another", &old);
}

/// Takes 40 s or more.
#[test]
fn a_removal_of_a_storage_resource_found_to_have_landed_cleans_its_directory_and_writes_its_record_once()
 {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_found_remove_landed");
    let s = storage_on(&platform);
    let vm = &s.vm;
    let dir = s.dir(MARKETING);
    std::fs::write(dir.join("kept.txt"), b"kept").unwrap();
    let files = files_of(&dir);
    lose_held(
        &postgres,
        lost_delete(vm, MARKETING),
        "the removal of the Storage Resource",
    );
    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let after = settled(
        vm,
        FILES,
        FILE_WRITERS,
        &put_call("after.txt"),
        "once the removal landed",
    );
    assert_refusal(
        &after,
        "not-registered",
        &json!({ "kind": "storage", "resource": MARKETING }),
        "a put once the removal landed",
    );
    wait_cleaned(&dir, "the directory of a removal that landed");
    assert_eq!(files_of(&dir), files, "expected the files untouched");
    assert_one_record(vm, "resource-removed", "a removal found to have landed");
    s.register_on("another", &dir);
}

/// Takes about 10 s for the write's limit, and up to 15 s more.
#[test]
fn a_location_change_found_not_to_have_landed_removes_only_what_it_made_in_the_new_directory() {
    let (_postgres, proxy, platform) = behind_a_proxy("vm_platform_found_location_kept");
    let s = storage_on(&platform);
    let vm = &s.vm;
    let old = s.dir(MARKETING);
    let t = token();
    let new = new_directory(&s, &t);
    // The new directory already held, before the request, a `.vibe-resource`
    // naming this same Resource and a `.vibe-staging/`.
    std::fs::copy(old.join(".vibe-resource"), new.join(".vibe-resource")).unwrap();
    std::fs::create_dir_all(new.join(".vibe-staging")).unwrap();
    let before = tree(&new);
    lose_dropped(
        &proxy,
        &format!("moved-{t}"),
        &lost_location(vm, MARKETING, &new.to_string_lossy()),
        "the change of the location whose connection is held",
    );
    proxy.fail();
    let answer = settled(
        vm,
        FILES,
        FILE_WRITERS,
        &put_call("after.txt"),
        "once the change did not land",
    );
    assert!(answer["ok"].is_object(), "{answer}");
    assert!(old.join("after.txt").is_file(), "{:?}", tree(&old));
    // Tried once, in the second the reading has left: give it that.
    thread::sleep(Duration::from_secs(3));
    assert_eq!(
        tree(&new),
        before,
        "expected a change found not to have landed to leave what was in the new directory before it: the `.vibe-resource` naming this Resource and the `.vibe-staging/`"
    );
}

/// A request sent and given up on after `wait`: the client's connection is
/// closed while the write is on its way.
fn given_up(request: Request, wait: Duration, what: &str) {
    let answer = request.timeout(wait).try_send();
    assert!(
        answer.is_err(),
        "the harness's own check, {what}: expected no answer within {wait:?} (the write is held), got {:?}",
        answer.map(|a| a.status)
    );
}

/// Holds commits, sends `request`, gives up on it once its commit waits.
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

/// Takes 30 s or more.
#[test]
fn a_location_change_given_up_while_its_commit_is_held_and_then_landing_ends_on_the_new_directory()
{
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_given_up_location_landed");
    let s = storage_on(&platform);
    let vm = &s.vm;
    let old = s.dir(MARKETING);
    std::fs::write(old.join("kept.txt"), b"kept").unwrap();
    let files = files_of(&old);
    let t = token();
    let new = new_directory(&s, &t);
    give_up_held(
        &postgres,
        lost_location(vm, MARKETING, &new.to_string_lossy()),
        "the change of the location, given up",
    );
    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let after = settled(
        vm,
        FILES,
        FILE_WRITERS,
        &put_call("after.txt"),
        "once the change that was given up landed",
    );
    assert!(after["ok"].is_object(), "{after}");
    assert!(
        new.join("after.txt").is_file(),
        "expected the Resource on the new directory: {:?}",
        tree(&new)
    );
    assert_eq!(
        admin_resource(&vm.api_as(&administrator()), MARKETING).json()["location"]["directory"],
        json!(new.to_string_lossy())
    );
    assert_one_record(
        vm,
        "resource-location-changed",
        "a change given up, then landed",
    );
    wait_cleaned(
        &old,
        "the old directory of a change that was given up and landed",
    );
    assert_eq!(files_of(&old), files);
    // The new directory is not held for the change any longer: another
    // Resource can be registered beside it.
    let beside = s.root().join(format!("beside-{t}"));
    std::fs::create_dir_all(&beside).unwrap();
    s.register_on("beside", &beside);
}

/// Takes about 10 s for the write's limit, and up to 15 s more.
#[test]
fn a_location_change_given_up_while_its_connection_is_held_and_never_landing_leaves_nothing_in_the_new_directory()
 {
    let (_postgres, proxy, platform) = behind_a_proxy("vm_platform_given_up_location_dropped");
    let s = storage_on(&platform);
    let vm = &s.vm;
    let old = s.dir(MARKETING);
    let t = token();
    let new = new_directory(&s, &t);
    proxy.arm(&format!("moved-{t}"));
    given_up(
        lost_location(vm, MARKETING, &new.to_string_lossy()),
        Duration::from_secs(4),
        "the change of the location, given up",
    );
    assert_eq!(
        proxy.held(),
        1,
        "the harness's own check: expected the write's connection held, first bytes {:?}",
        proxy.held_chunks()
    );
    proxy.fail();
    let answer = settled(
        vm,
        FILES,
        FILE_WRITERS,
        &put_call("after.txt"),
        "once the change that was given up did not land",
    );
    assert!(answer["ok"].is_object(), "{answer}");
    assert!(old.join("after.txt").is_file(), "{:?}", tree(&old));
    assert!(!new.join("after.txt").exists());
    let clean = blackbox::storage::eventually(SETTLE, || the_programs_names_in(&new).is_empty());
    assert!(
        clean,
        "expected nothing of the change in the new directory: {:?}",
        tree(&new)
    );
    assert_eq!(
        admin_resource(&vm.api_as(&administrator()), MARKETING).json()["location"]["directory"],
        json!(old.to_string_lossy())
    );
    let beside = s.root().join(format!("beside-{t}"));
    std::fs::create_dir_all(&beside).unwrap();
    s.register_on("beside", &beside);
}

/// Takes 30 s or more.
#[test]
fn a_removal_given_up_while_its_commit_is_held_and_then_landing_is_not_registered_with_one_record()
{
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_given_up_remove_landed");
    let s = storage_on(&platform);
    let vm = &s.vm;
    let dir = s.dir(MARKETING);
    give_up_held(
        &postgres,
        lost_delete(vm, MARKETING),
        "the removal, given up",
    );
    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let after = settled(
        vm,
        FILES,
        FILE_WRITERS,
        &put_call("after.txt"),
        "once the removal that was given up landed",
    );
    assert_refusal(
        &after,
        "not-registered",
        &json!({ "kind": "storage", "resource": MARKETING }),
        "a put once the removal that was given up landed",
    );
    assert_one_record(vm, "resource-removed", "a removal given up, then landed");
    wait_cleaned(
        &dir,
        "the directory of a removal that was given up and landed",
    );
}

/// Takes about 10 s for the write's limit, and up to 15 s more.
#[test]
fn a_removal_that_did_not_land_forgets_who_asked_for_it() {
    let (postgres, proxy, platform) = behind_a_proxy("vm_platform_forgotten_remover");
    let t = Setting::new(&platform);
    let vm = t.vm();
    lose_dropped(
        &proxy,
        RESOURCE,
        &lost_delete(vm, RESOURCE),
        "the removal whose connection is held",
    );
    proxy.fail();
    let answer = settled(
        vm,
        "board",
        CREW,
        &op("query", &who()),
        "once the removal did not land",
    );
    assert_rows(
        &answer,
        &["who"],
        &json!([[t.reader.role]]),
        "the Resource is served again",
    );
    // Then its row is deleted behind the program's back, and a write finds
    // no row.
    let direct = Sql::connect(&postgres.url("vm_platform_forgotten_remover"))
        .expect("the harness's own connection");
    let id = direct
        .value(&format!(
            "SELECT id::text FROM resources WHERE name = '{RESOURCE}'"
        ))
        .expect("the row");
    let holders = direct.batch(
        "SELECT c.conrelid::regclass::text, a.attname::text
           FROM pg_constraint c
           JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = c.conkey[1]
          WHERE c.contype = 'f' AND c.confrelid = 'resources'::regclass",
    );
    for holder in holders {
        if let (Some(Some(table)), Some(Some(column))) = (holder.first(), holder.get(1)) {
            direct.batch(&format!(
                "DELETE FROM {table} WHERE {column}::text = '{id}'"
            ));
        }
    }
    direct.batch(&format!("DELETE FROM resources WHERE id::text = '{id}'"));
    let patched = asked_again(
        &|| {
            vm.api_as(&administrator())
                .request(&format!("/v1/resources/{RESOURCE}"))
                .method("PATCH")
                .json(&json!({ "contact": "Someone Else" }))
                .timeout(LOST_WRITE_TIMEOUT)
        },
        "a change of a Resource whose row is gone",
    );
    assert_api_error(&patched, 404, "not-found");
    let named: Vec<Value> = records_waited(vm, "resource-removed", 0)
        .into_iter()
        .filter(|r| r["by"] == json!(administrator().id))
        .collect();
    assert!(
        named.is_empty(),
        "expected no `resource-removed` record naming who asked for a removal that did not land, got {named:?}"
    );
}

/// Takes 40 s or more.
#[test]
fn a_directory_the_program_had_not_opened_yet_is_opened_for_the_cleaning_of_its_removal() {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_found_location_then_removed");
    let s = storage_on(&platform);
    let vm = &s.vm;
    let t = token();
    let new = new_directory(&s, &t);
    std::fs::write(new.join("kept.txt"), b"kept").unwrap();
    lose_held(
        &postgres,
        lost_location(vm, MARKETING, &new.to_string_lossy()),
        "the change of the location",
    );
    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let after = settled(
        vm,
        FILES,
        FILE_WRITERS,
        &put_call("after.txt"),
        "once the change landed",
    );
    assert!(after["ok"].is_object(), "{after}");
    let removed = asked_again(&|| lost_delete(vm, MARKETING), "the removal");
    assert_eq!(removed.status, 200, "{}", removed.describe());
    assert_eq!(
        removed.json()["directory_cleaned"],
        json!(true),
        "{}",
        removed.describe()
    );
    assert!(
        !new.join(".vibe-resource").exists(),
        "expected the new directory's `.vibe-resource` gone: {:?}",
        tree(&new)
    );
    assert!(new.join("kept.txt").is_file() && new.join("after.txt").is_file());
    s.register_on("another", &new);
}
