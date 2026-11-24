//! A write of a Resource whose answer is lost (CONTRACT.md, "Resources and
//! Grants in full: A write whose answer is lost"), with the stand-in
//! sign-in: replacing a Resource's logins and changing its description,
//! owner or contact, each answered `503` `unavailable` while the platform
//! database cannot say what became of it.
//!
//! Two rigs make the write's answer lost:
//!
//! - `blackbox::lost_answer::HeldPostgres`: commits wait, then go through,
//!   so the write **lands** after its `503`.
//! - A proxy (`blackbox::outbound_surface::DbProxy`) between the program and
//!   its platform database that holds the connection of the write from the
//!   write's own bytes on and then drops it: the write **never lands**
//!   (a held commit always lands, so it cannot make this case). The proxy
//!   holds the first chunk that holds a needle: the new `contact` for a
//!   change of the contact, and the Resource's name for a replacement of
//!   logins (whose values are sealed, so no byte of them can be looked
//!   for): for the second the connection held may be the one that takes
//!   the Resource's row lock, so what is tested there is that the write
//!   was never made, not that its commit was lost.
//!
//! Pass B (removal and location) is not here.
//!
//! These tests are slow by nature: each waits for the program's limit on a
//! write (10 s or more) at least once. Those that also send operations
//! while a commit is held wait for the program to stop waiting for their
//! entries, about 20 s more. They run in parallel.
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
    admin_resource, body, connection, connections_of, drop_role, login_json, reader_direct,
    sent_by_login, writer_direct,
};
use blackbox::scenario::{
    CONTACT, Scenario, administrator, assert_error_kind, assert_executed, assert_refusal,
    assert_rows, op, sql_on,
};
use blackbox::server_log::log_everything;
use blackbox::shared_database::{Login, token};
use blackbox::sql::Sql;
use blackbox::{Request, Response, User, Vm, VmOptions, assert_api_error};
use serde_json::{Value, json};

const RESOURCE: &str = "devops";
const CREW: &str = "crew";

/// A User of [`CREW`], which holds `write` on the Resource. One User for
/// each call made while commits are held, so that its `host-call` record
/// can be found.
fn crew(id: &str) -> User {
    User::new(id).in_groups(&[CREW])
}

/// A User no Grant reaches.
fn nora() -> User {
    User::new("nora").in_groups(&["elsewhere"])
}

/// The setting: `devops` registered with a read login and a write login
/// of their own, [`CREW`] granted `write`, and `board` declaring it at
/// `write`.
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

fn marker(what: &str) -> String {
    format!("{what}-{}", token())
}

/// `{ resource, sql }` inserting a row whose body is `marker`.
fn insert(marker: &str) -> Value {
    sql_on(
        RESOURCE,
        &format!("insert into notes (body) values ('{marker}') returning body"),
    )
}

/// `{ resource, sql }` saying which role the statement ran as.
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

/// One call to `board` as `user`, which must be answered `200`.
fn call(vm: &Vm, user: &User, call: &Value, context: &str) -> Value {
    let response = call_request(vm, user, call).send();
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

fn assert_grants_unknown(answer: &Value, context: &str) {
    assert_refusal(
        answer,
        "grants-unknown",
        &json!({ "resource": RESOURCE }),
        context,
    );
}

/// The `no-grant` fields for `user`, who needs `needed`, holds nothing, and
/// is told to ask `contact`.
fn no_grant(user: &User, needed: &str, contact: &str) -> Value {
    json!({
        "resource": RESOURCE,
        "user": user.id,
        "needed": needed,
        "held": null,
        "contact": contact,
    })
}

/// A `PUT` of the connection, waiting as long as a write the program gives
/// up on.
fn lost_put(vm: &Vm, connection: &Value) -> Request {
    vm.api_as(&administrator())
        .request(&format!("/v1/resources/{RESOURCE}/connection"))
        .method("PUT")
        .json(connection)
        .timeout(LOST_WRITE_TIMEOUT)
}

/// A `PATCH` of the Resource, waiting as long as a write the program gives
/// up on.
fn lost_patch(vm: &Vm, fields: &Value) -> Request {
    vm.api_as(&administrator())
        .request(&format!("/v1/resources/{RESOURCE}"))
        .method("PATCH")
        .json(fields)
        .timeout(LOST_WRITE_TIMEOUT)
}

fn assert_lost(answer: Result<Response, String>, what: &str) {
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
        assert_lost(sending.join().unwrap(), what);
    });
    postgres.assert_waiting(waiting, what);
}

/// Sends `request` with the proxy armed for `needle`: the connection of the
/// write is held (never delivered) from the chunk with the needle on, and
/// the request is answered `503` `unavailable`. The write has not landed
/// and does not, once [`DbProxy::fail`] is called.
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

/// `GET` of the Resource's administrators' view, listing and Grants, and
/// the Resource in the Resources a User may use: each `503` `unavailable`,
/// at once.
fn assert_not_shown_meanwhile(vm: &Vm, what: &str) {
    let admin = vm.api_as(&administrator());
    let member = vm.api_as(&crew("member-api"));
    for (api, path) in [
        (&admin, format!("/v1/resources/{RESOURCE}/grants")),
        (&admin, "/v1/admin/resources".to_owned()),
        (&admin, format!("/v1/admin/resources/{RESOURCE}")),
        (&member, "/v1/resources".to_owned()),
        (&member, format!("/v1/resources/{RESOURCE}")),
    ] {
        let answer = quickly(vm, &api.request(&path), &format!("{what}: GET {path}"));
        assert_api_error(&answer, 503, "unavailable");
    }
}

/// The first call to `board` as a User of its own that is not refused
/// `grants-unknown`, waiting up to [`SETTLE`] for the program to have read
/// what became of the write (which it does once an operation was refused
/// so).
fn settled(vm: &Vm, call_of: &Value, what: &str) -> Value {
    let deadline = Instant::now() + SETTLE;
    loop {
        let user = crew(&format!("settled-{}", token()));
        let answer = call(vm, &user, call_of, what);
        let unknown = answer["refused"]["refusal"]["reason"] == "grants-unknown";
        if !unknown || Instant::now() >= deadline {
            return answer;
        }
        thread::sleep(Duration::from_millis(300));
    }
}

/// Sends the request `make` builds again and again until it is answered
/// something but `503` `unavailable` (allowed for a short while: the
/// program is finding the Resource out), within 5 s. A `409` at any point
/// fails; so does no answer but `503` in 5 s.
fn asked_again(vm: &Vm, make: &dyn Fn() -> Request, what: &str) -> Response {
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
            let _ = vm;
            return answer;
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn contact_of(vm: &Vm) -> String {
    let answer = admin_resource(&vm.api_as(&administrator()), RESOURCE);
    answer.json()["contact"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

// ---------------------------------------------------------------------------
// a. A replacement that removes the write login, its commit held, landing
// ---------------------------------------------------------------------------

/// Takes 40 s or more: the program's limit on the write, then on the
/// entries of the operations made meanwhile.
#[test]
fn a_replacement_removing_the_write_login_whose_answer_is_lost_never_uses_that_login_on_a_guess() {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_lost_logins_remove");
    let t = Setting::new(&platform);
    let vm = t.vm();
    let db = &t.s.db;
    log_everything(db.server());

    // The harness's own check: the write login writes before.
    let first = marker("before");
    let answer = call(
        vm,
        &crew("crew-before"),
        &op("execute", &insert(&first)),
        "before the replacement",
    );
    assert_executed(
        &answer,
        &["body"],
        &json!([[first]]),
        1,
        "the harness's own check: the write login writes before the replacement",
    );

    lose_held(
        &postgres,
        lost_put(vm, &json!({ "write": null })),
        "the replacement that removes the write login",
    );
    // Whatever the old write login did until now is behind us; from now on
    // it sends nothing and connects to nothing.
    thread::sleep(Duration::from_millis(1500));
    let sent_at_503 = sent_by_login(db, &t.writer).len();
    let connected_at_503 = connections_of(db, &t.writer);

    assert_not_shown_meanwhile(vm, "while the replacement's commit is held");
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
                let request = call_request(vm, user, c);
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
    for (answer, (user, _)) in answers.into_iter().zip(&calls) {
        let response = answer.unwrap_or_else(|e| {
            vm.fail(&format!(
                "{:?}'s call made meanwhile: no answer ({e})",
                user.id
            ))
        });
        assert_grants_unknown(
            &answer_of(vm, &response, &format!("{:?}'s call meanwhile", user.id)),
            &format!(
                "{:?}'s call, meanwhile, on the Resource whose logins are being replaced",
                user.id
            ),
        );
    }
    assert!(
        !pulled.text().contains("schema_ts"),
        "expected no structure for a pull meanwhile, got\n{}",
        pulled.describe()
    );
    assert_api_error(&pulled, 503, "unavailable");
    db.assert_not_received(
        &write,
        "an execute made while the logins' replacement is unknown",
    );
    assert_eq!(
        (
            sent_by_login(db, &t.writer).len(),
            connections_of(db, &t.writer)
        ),
        (sent_at_503, connected_at_503),
        "expected the old write login to send no statement and make no connection after the `503` of the replacement that removes it"
    );

    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let after = settled(
        vm,
        &op("execute", &insert(&marker("after"))),
        "after the replacement landed",
    );
    assert_error_kind(
        &after,
        "read-only-resource",
        "an execute once the replacement that removes the write login has landed, with no restart",
    );
    assert_eq!(
        (
            sent_by_login(db, &t.writer).len(),
            connections_of(db, &t.writer)
        ),
        (sent_at_503, connected_at_503),
        "expected the old write login never to be used after the `503`, also once the replacement landed"
    );
}

// ---------------------------------------------------------------------------
// b. A read-login replacement held and not landing
// ---------------------------------------------------------------------------

/// A Postgres of the test's own with a proxy in front of the platform
/// database: the program reaches the platform database through it.
fn behind_a_proxy(name: &str) -> (PrivatePostgres, DbProxy, Platform) {
    let postgres = PrivatePostgres::start().expect("a Postgres of the test's own");
    let target = format!("{}:{}", postgres.host(), postgres.port())
        .parse()
        .expect("the Postgres's address");
    let proxy = DbProxy::start(target);
    let platform = Platform::on_address(&postgres, name, proxy.address());
    (postgres, proxy, platform)
}

/// Takes about 10 s for the write's limit, and up to 15 s more for the
/// program to find out.
#[test]
fn a_read_login_replacement_that_never_lands_leaves_the_old_login_serving() {
    let (_postgres, proxy, platform) = behind_a_proxy("vm_platform_lost_read_dropped");
    let t = Setting::new(&platform);
    let vm = t.vm();
    let db = &t.s.db;
    let new_reader = reader_direct(db);
    let listing = admin_resource(&vm.api_as(&administrator()), RESOURCE).json();

    lose_dropped(
        &proxy,
        RESOURCE,
        &lost_put(vm, &json!({ "read": login_json(&new_reader) })),
        "the replacement of the read login whose connection is held",
    );
    // No "meanwhile" answer is asked of here: with the write's connection
    // held before it took the row's lock, nothing on the platform database
    // is on its way that a reading could be earlier than.
    // The write is never made: its connection is dropped.
    proxy.fail();

    let answer = settled(
        vm,
        &op("query", &who()),
        "once the replacement did not land",
    );
    assert_rows(
        &answer,
        &["who"],
        &json!([[t.reader.role]]),
        "a read once the replacement did not land: the old read login serves",
    );
    assert_eq!(
        admin_resource(&vm.api_as(&administrator()), RESOURCE).json(),
        listing,
        "expected the Resource's listing unchanged once the replacement did not land"
    );
}

// ---------------------------------------------------------------------------
// c. A read-login replacement held and landing
// ---------------------------------------------------------------------------

/// Takes about 10 s for the write's limit.
#[test]
fn a_read_login_replacement_that_lands_after_its_503_is_served_by_the_new_login_with_no_restart() {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_lost_read_landed");
    let t = Setting::new(&platform);
    let vm = t.vm();
    let db = &t.s.db;
    let new_reader = reader_direct(db);

    // Before: the old read login serves.
    let answer = call(vm, &crew("before-read"), &op("query", &who()), "before");
    assert_rows(
        &answer,
        &["who"],
        &json!([[t.reader.role]]),
        "the harness's own check: the old read login serves before",
    );

    lose_held(
        &postgres,
        lost_put(vm, &json!({ "read": login_json(&new_reader) })),
        "the replacement of the read login",
    );
    // Only the new login can serve from here on.
    drop_role(db, &t.reader);

    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let answer = settled(vm, &op("query", &who()), "once the replacement landed");
    assert_rows(
        &answer,
        &["who"],
        &json!([[new_reader.role]]),
        "a read once the replacement landed, its old role gone: the new login serves, with no restart",
    );
}

// ---------------------------------------------------------------------------
// d. A change of the contact
// ---------------------------------------------------------------------------

const NEW_CONTACT: &str = "Rae Ops, #new-ops on chat";

/// Takes 10 s or more.
#[test]
fn a_contact_change_that_lands_after_its_503_is_named_in_the_next_no_grant() {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_lost_contact_landed");
    let t = Setting::new(&platform);
    let vm = t.vm();

    let before = call(
        vm,
        &nora(),
        &op("query", &sql_on(RESOURCE, "select 1 as one")),
        "before the change",
    );
    assert_refusal(
        &before,
        "no-grant",
        &no_grant(&nora(), "read", CONTACT),
        "the harness's own check: the old contact before the change",
    );

    lose_held(
        &postgres,
        lost_patch(vm, &json!({ "contact": NEW_CONTACT })),
        "the change of the contact",
    );
    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let answer = {
        let deadline = Instant::now() + SETTLE;
        loop {
            let user = User::new(&format!("nora-{}", token())).in_groups(&["elsewhere"]);
            let answer = call(
                vm,
                &user,
                &op("query", &sql_on(RESOURCE, "select 1 as one")),
                "once the change landed",
            );
            if answer["refused"]["refusal"]["reason"] != "grants-unknown"
                || Instant::now() >= deadline
            {
                break (user, answer);
            }
            thread::sleep(Duration::from_millis(300));
        }
    };
    assert_refusal(
        &answer.1,
        "no-grant",
        &no_grant(&answer.0, "read", NEW_CONTACT),
        "a refusal once the change of the contact landed: the new contact is named, with no restart",
    );
    assert_eq!(contact_of(vm), NEW_CONTACT);
}

/// Takes about 10 s for the write's limit, and up to 15 s more.
#[test]
fn a_contact_change_that_never_lands_leaves_the_old_contact_in_the_next_no_grant() {
    let (_postgres, proxy, platform) = behind_a_proxy("vm_platform_lost_contact_dropped");
    let t = Setting::new(&platform);
    let vm = t.vm();

    lose_dropped(
        &proxy,
        NEW_CONTACT,
        &lost_patch(vm, &json!({ "contact": NEW_CONTACT })),
        "the change of the contact whose connection is held",
    );
    proxy.fail();
    let user = nora();
    let deadline = Instant::now() + SETTLE;
    let answer = loop {
        let user = User::new(&format!("{}-{}", user.id, token())).in_groups(&["elsewhere"]);
        let answer = call(
            vm,
            &user,
            &op("query", &sql_on(RESOURCE, "select 1 as one")),
            "once the change did not land",
        );
        if answer["refused"]["refusal"]["reason"] != "grants-unknown" || Instant::now() >= deadline
        {
            break (user, answer);
        }
        thread::sleep(Duration::from_millis(300));
    };
    assert_refusal(
        &answer.1,
        "no-grant",
        &no_grant(&answer.0, "read", CONTACT),
        "a refusal once the change of the contact did not land: the old contact is named",
    );
    assert_eq!(contact_of(vm), CONTACT);
}

// ---------------------------------------------------------------------------
// e. Asking again is safe
// ---------------------------------------------------------------------------

/// Takes 10 s or more.
#[test]
fn the_same_replacement_asked_again_after_its_503_answers_200() {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_lost_again_put");
    let t = Setting::new(&platform);
    let vm = t.vm();

    lose_held(
        &postgres,
        lost_put(vm, &json!({ "write": null })),
        "the replacement that removes the write login",
    );
    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let again = asked_again(
        vm,
        &|| lost_put(vm, &json!({ "write": null })),
        "the same replacement asked again",
    );
    assert_eq!(
        again.status,
        200,
        "expected the same replacement asked again to answer `200` (the same members) within 5 s, got\n{}",
        again.describe()
    );
    let answer = settled(
        vm,
        &op("execute", &insert(&marker("again"))),
        "after asking again",
    );
    assert_error_kind(
        &answer,
        "read-only-resource",
        "an execute after the replacement was asked again",
    );
}

/// Takes 10 s or more.
#[test]
fn the_same_change_asked_again_after_its_503_is_the_same() {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_lost_again_patch");
    let t = Setting::new(&platform);
    let vm = t.vm();
    let fields = json!({ "contact": NEW_CONTACT, "owner": "The new team" });

    lose_held(
        &postgres,
        lost_patch(vm, &fields),
        "the change of the contact and the owner",
    );
    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let again = asked_again(
        vm,
        &|| lost_patch(vm, &fields),
        "the same change asked again",
    );
    assert_eq!(
        again.status,
        200,
        "expected the same change asked again to answer `200` within 5 s, got\n{}",
        again.describe()
    );
    let view = admin_resource(&vm.api_as(&administrator()), RESOURCE).json();
    assert_eq!(
        (view["contact"].as_str(), view["owner"].as_str()),
        (Some(NEW_CONTACT), Some("The new team")),
        "expected the Resource to hold the change asked twice, got {view}"
    );
}

// ---------------------------------------------------------------------------
// f. A write waits for the row's lock of a write whose answer was lost
// ---------------------------------------------------------------------------

/// Takes 20 s or more: the program's limit on the first write.
#[test]
fn a_write_behind_the_programs_own_lost_write_is_503_at_once_and_200_once_it_landed_and_is_found_out()
 {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_lost_row_lock");
    let t = Setting::new(&platform);
    let vm = t.vm();

    lose_held(
        &postgres,
        lost_patch(vm, &json!({ "contact": "First Contact, #first" })),
        "the first change of the contact",
    );
    let began = Instant::now();
    let second = lost_patch(vm, &json!({ "contact": NEW_CONTACT }))
        .try_send()
        .unwrap_or_else(|e| panic!("the second change: expected `503`, no answer ({e})"));
    let took = began.elapsed();
    assert_api_error(&second, 503, "unavailable");
    assert!(
        took < Duration::from_secs(2),
        "expected the write behind the program's own lost write to be answered `503` at once (nothing written), it took {took:?}"
    );
    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let again = asked_again(
        vm,
        &|| lost_patch(vm, &json!({ "contact": NEW_CONTACT })),
        "the second change asked again",
    );
    assert_eq!(
        again.status,
        200,
        "expected the same write asked again, the lost one landed and found out, to answer `200` within 5 s, got\n{}",
        again.describe()
    );
    assert_eq!(contact_of(vm), NEW_CONTACT);
}

/// Takes 10 s or more: a write waits for the row's lock held by another
/// transaction (here the platform database's administrator) and is `503`.
#[test]
fn a_write_of_the_row_while_another_transaction_holds_it_is_503_after_10_seconds_and_asked_again_is_200()
 {
    let postgres = PrivatePostgres::start().expect("a Postgres of the test's own");
    let name = "vm_platform_lost_row_held";
    let platform = Platform::on(&postgres, name);
    let t = Setting::new(&platform);
    let vm = t.vm();

    let holder = Sql::connect(&postgres.url(name)).expect("the harness's own connection");
    holder.batch("BEGIN");
    holder.batch(&format!(
        "SELECT id FROM resources WHERE name = '{RESOURCE}' FOR UPDATE"
    ));
    let began = Instant::now();
    let answer = lost_patch(vm, &json!({ "contact": NEW_CONTACT }))
        .try_send()
        .unwrap_or_else(|e| panic!("the change: expected `503`, no answer ({e})"));
    let took = began.elapsed();
    assert_api_error(&answer, 503, "unavailable");
    assert!(
        took >= Duration::from_secs(9) && took < Duration::from_secs(15),
        "expected the write to wait for the row's lock for 10 s (\"Bounds and costs\"), it answered after {took:?}"
    );
    holder.batch("ROLLBACK");
    drop(holder);
    // Asked again, the lock released: `503` is allowed while the program
    // finds out ("try again shortly"), then `200`.
    let deadline = Instant::now() + SETTLE;
    let again = loop {
        let again = lost_patch(vm, &json!({ "contact": NEW_CONTACT })).send();
        if again.status != 503 || Instant::now() >= deadline {
            break again;
        }
        thread::sleep(Duration::from_millis(300));
    };
    assert_eq!(
        again.status,
        200,
        "expected the change asked again, the lock released, to answer `200` within {SETTLE:?}, got\n{}",
        again.describe()
    );
    assert_eq!(contact_of(vm), NEW_CONTACT);
}

// ---------------------------------------------------------------------------
// g. The bounds of the earlier section still hold meanwhile
// ---------------------------------------------------------------------------

/// Takes 10 s or more.
#[test]
fn a_request_to_the_application_is_answered_at_once_while_a_write_of_its_resource_is_lost() {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_lost_logins_bounds");
    let t = Setting::new(&platform);
    let vm = t.vm();

    lose_held(
        &postgres,
        lost_put(vm, &json!({ "write": null })),
        "the replacement that removes the write login",
    );
    let user = crew("visitor-meanwhile");
    let request = vm
        .request("board", "/")
        .as_user(&user)
        .method("POST")
        .json(&json!({ "calls": [] }));
    let answer = quickly(
        vm,
        &request,
        "a request to an Application that declares the Resource whose logins' replacement is unknown",
    );
    assert_eq!(
        answer.status,
        200,
        "expected the Application to be served as ever, got\n{}",
        answer.describe()
    );
    // The other routes of the API are answered at once too.
    let answer = quickly(
        vm,
        &vm.api_as(&administrator()).request("/v1/applications"),
        "GET the Applications while the Resource's logins are unknown",
    );
    assert_eq!(answer.status, 200, "{}", answer.describe());
}

// ---------------------------------------------------------------------------
// h. A request given up mid-write
// ---------------------------------------------------------------------------

/// Sends `request` with its client's patience cut to `patience`: the client
/// closes the connection with no answer read.
fn given_up(request: Request, patience: Duration) {
    let answer = request.timeout(patience).try_send();
    assert!(
        answer.is_err(),
        "the harness's own check: expected the client to give up with no answer, got {answer:?}"
    );
}

/// The logins of the Resource as the administrators' view says.
fn logins_shown(vm: &Vm) -> Value {
    admin_resource(&vm.api_as(&administrator()), RESOURCE).json()["logins"].clone()
}

/// Takes about 15 s.
#[test]
fn a_replacement_given_up_mid_write_that_lands_is_never_served_by_the_old_write_login() {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_given_up_put");
    let t = Setting::new(&platform);
    let vm = t.vm();
    let db = &t.s.db;
    log_everything(db.server());
    let first = marker("before");
    call(
        vm,
        &crew("given-up-before"),
        &op("execute", &insert(&first)),
        "before",
    );

    postgres.hold();
    let waiting = postgres.waiting() + 1;
    thread::scope(|scope| {
        let sending = scope.spawn(|| {
            given_up(
                lost_put(vm, &json!({ "write": null })),
                Duration::from_secs(4),
            )
        });
        postgres.wait_for_waiting(waiting, "the replacement given up");
        sending.join().unwrap();
    });
    postgres.assert_waiting(waiting, "after the client gave up");
    thread::sleep(Duration::from_millis(1500));
    let sent = sent_by_login(db, &t.writer).len();
    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");

    // From the landing on: never a success; settles on `read-only-resource`.
    let landed = Instant::now();
    let mut seen = Vec::new();
    let end = loop {
        let user = crew(&format!("given-up-{}", token()));
        let m = marker("after-given-up");
        let answer = call(
            vm,
            &user,
            &op("execute", &insert(&m)),
            "after the commit landed",
        );
        assert!(
            answer.get("ok").is_none(),
            "expected no success of an execute after the replacement removing the write login landed, got {answer}"
        );
        let reason = answer["refused"]["refusal"]["reason"]
            .as_str()
            .or(answer["error"]["kind"].as_str())
            .unwrap_or("?")
            .to_owned();
        seen.push(reason.clone());
        if reason == "read-only-resource" || landed.elapsed() > Duration::from_secs(8) {
            break reason;
        }
        thread::sleep(Duration::from_millis(300));
    };
    assert_eq!(
        end, "read-only-resource",
        "expected `read-only-resource` within a few seconds with no restart, answers seen: {seen:?}"
    );
    assert_eq!(
        sent_by_login(db, &t.writer).len(),
        sent,
        "expected the old write login to send nothing after the commit landed"
    );
    assert_eq!(
        logins_shown(vm)["write"],
        json!(false),
        "expected the Resource's view to show no write login"
    );
}

/// Takes about 15 s.
#[test]
fn a_contact_change_given_up_mid_write_that_lands_is_named_within_a_few_seconds() {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_given_up_patch");
    let t = Setting::new(&platform);
    let vm = t.vm();

    postgres.hold();
    let waiting = postgres.waiting() + 1;
    thread::scope(|scope| {
        let sending = scope.spawn(|| {
            given_up(
                lost_patch(vm, &json!({ "contact": NEW_CONTACT })),
                Duration::from_secs(4),
            )
        });
        postgres.wait_for_waiting(waiting, "the change given up");
        sending.join().unwrap();
    });
    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let landed = Instant::now();
    let (user, answer) = loop {
        let user = User::new(&format!("nora-{}", token())).in_groups(&["elsewhere"]);
        let answer = call(
            vm,
            &user,
            &op("query", &sql_on(RESOURCE, "select 1 as one")),
            "after the change landed",
        );
        let named = answer["refused"]["refusal"]["contact"] == json!(NEW_CONTACT);
        if named || landed.elapsed() > Duration::from_secs(8) {
            break (user, answer);
        }
        thread::sleep(Duration::from_millis(300));
    };
    assert_refusal(
        &answer,
        "no-grant",
        &no_grant(&user, "read", NEW_CONTACT),
        "a refusal within a few seconds of a given-up change landing: the new contact is named, with no restart",
    );
}

/// Takes about 20 s.
#[test]
fn a_replacement_given_up_mid_write_that_never_lands_leaves_the_old_logins_serving() {
    let (_postgres, proxy, platform) = behind_a_proxy("vm_platform_given_up_dropped");
    let t = Setting::new(&platform);
    let vm = t.vm();

    proxy.arm(RESOURCE);
    given_up(
        lost_put(vm, &json!({ "write": null })),
        Duration::from_secs(4),
    );
    assert_eq!(
        proxy.held(),
        1,
        "the harness's own check: expected the proxy to hold the write's connection; first bytes: {:?}",
        proxy.held_chunks()
    );
    proxy.fail();
    let landed = Instant::now();
    let end = loop {
        let user = crew(&format!("given-up-{}", token()));
        let m = marker("never-landed");
        let answer = call(
            vm,
            &user,
            &op("execute", &insert(&m)),
            "after the connection was dropped",
        );
        if answer.get("ok").is_some() || landed.elapsed() > Duration::from_secs(8) {
            break answer;
        }
        thread::sleep(Duration::from_millis(300));
    };
    assert_executed(
        &end,
        &["body"],
        &json!([[end["ok"]["rows"][0][0].clone()]]),
        1,
        "an execute within a few seconds of the dropped write: the old write login serves again",
    );
    assert_eq!(logins_shown(vm)["write"], json!(true));
}
