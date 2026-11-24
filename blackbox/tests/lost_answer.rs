//! A write whose answer is lost (CONTRACT.md, "A write whose answer is
//! lost", and the row `grants-unknown` of "Operations on a shared database:
//! Who is allowed"), with the stand-in sign-in, on a platform database on a
//! Postgres of each test's own whose commits the harness can hold
//! (`blackbox::lost_answer`, which says how).
//!
//! Each test sends its writes while commits are held; the program gives up
//! on each after its limit for an operation on the platform database and
//! answers `503` `unavailable`, its commit still on its way. From that
//! answer on, the program never decides by what it held: an Application's
//! address is `503` with `retry-after`, at once; an operation on the
//! Resource is refused `grants-unknown` (the refusals checked before it
//! given as ever), never sent; the API's answers that would show the
//! Grants are `503`. Once commits go through and the program has read what
//! became of the write, all is as the database has it, with no restart:
//! the Version the database says serves, the Grant the database has, the
//! Resource registered. The same for a program started while such writes
//! are on their way.
//!
//! While commits are held the Operation log entry of an operation cannot
//! be written either: the host's decision is seen in its `host-call`
//! record, written at the moment of the call, and the operation is
//! answered with its own refusal once the program stops waiting for the
//! entry, commits still held. Once commits go through, the tests wait until
//! no commit is waiting and ask nothing meanwhile: the first request to an
//! Application, and the first operation on a Resource, must already be
//! decided by what the database has ("When the program finds out").
//!
//! These tests are slow by nature: each waits once for the program's 20 s
//! limit (the writes of a test are sent together and wait together), then
//! for the program to stop waiting for the entries of the operations made
//! meanwhile (they are sent together too). Each takes about half a minute;
//! they run in parallel.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::Duration;

use blackbox::api::{marked_bundle, version_body};
use blackbox::lost_answer::{
    HeldPostgres, LOST_WRITE_TIMEOUT, assert_503_retry, host_call_of, host_call_refused, quickly,
};
use blackbox::scenario::{
    CONTACT, Scenario, administrator, assert_executed, assert_refusal, assert_rows, op,
    resource_body, sql_on,
};
use blackbox::shared_database::{SharedDatabase, token};
use blackbox::{INSTALLER, Request, Response, User, Vm, VmOptions, assert_api_error};
use serde_json::{Value, json};

/// The Group every User of these tests is in.
const CREW: &str = "crew";

/// A Resource whose Grant to [`CREW`] is lowered from `write` to `read`.
const LOWERED: &str = "lowered";
/// A Resource whose Grant to [`CREW`] is raised from `read` to `write`.
const RAISED: &str = "raised";
/// A Resource nobody changes: decided as ever throughout.
const UNCHANGED: &str = "unchanged";
/// A Resource registered while commits are held.
const LATE: &str = "late";

/// A User of [`CREW`]. Each call made while commits are held is made by a
/// User of its own, so that its `host-call` record can be found.
fn crew(id: &str) -> User {
    User::new(id).in_groups(&[CREW])
}

/// Someone who opens the Applications (shared with everyone).
fn visitor() -> User {
    User::new("visitor")
}

/// `{ resource, sql }` inserting a row whose body is `marker` into
/// `notes`, returning it: a write, with its marker in the statement's text
/// so that the shared database's log shows whether it was sent.
fn insert(resource: &str, marker: &str) -> Value {
    sql_on(
        resource,
        &format!("insert into notes (body) values ('{marker}') returning body"),
    )
}

/// `{ resource, sql }` of a read.
fn select_one(resource: &str) -> Value {
    sql_on(resource, "select 1 as one")
}

/// A fresh marker for a write.
fn marker(what: &str) -> String {
    format!("{what}-{}", token())
}

/// `POST /` of `fixtures/operations.js` on `application` as `user` with one
/// call, waiting as long as a write the program gives up on.
fn call_request(vm: &Vm, user: &User, application: &str, call: &Value) -> Request {
    vm.request(application, "/")
        .as_user(user)
        .method("POST")
        .json(&json!({ "calls": [call] }))
        .timeout(LOST_WRITE_TIMEOUT)
}

/// What the host answered the one call of a request to
/// `fixtures/operations.js`, which must be answered `200`.
fn answer_of(vm: &Vm, response: &Response, context: &str) -> Value {
    if response.status != 200 {
        vm.fail(&format!(
            "{context}: expected `200` from the Application with the host's answer, got\n{}",
            response.describe()
        ));
    }
    response.json()["answers"][0].clone()
}

/// One call, sent and answered now.
fn call(vm: &Vm, user: &User, application: &str, call: &Value, context: &str) -> Value {
    let response = call_request(vm, user, application, call).send();
    answer_of(vm, &response, context)
}

/// Fails unless `answer` is the `grants-unknown` refusal for `resource`, as
/// the Application's code sees it: the reason, the Resource, and a `fix`
/// that says to try again shortly.
fn assert_grants_unknown(answer: &Value, resource: &str, context: &str) {
    assert_refusal(
        answer,
        "grants-unknown",
        &json!({ "resource": resource }),
        context,
    );
    let fix = answer["refused"]["fix"].as_str().unwrap_or_default();
    assert!(
        fix.to_lowercase().contains("again"),
        "{context}: expected the `fix` of `grants-unknown` to say to try again shortly, got {fix:?}"
    );
}

/// The `no-grant` fields for a User of [`CREW`] who needs `needed` and
/// holds `held`.
fn no_grant(resource: &str, user: &User, needed: &str, held: Option<&str>) -> Value {
    json!({
        "resource": resource,
        "user": user.id,
        "needed": needed,
        "held": held,
        "contact": CONTACT,
    })
}

/// The Grants on `resource` as the administrator reads them.
fn grants(vm: &Vm, resource: &str) -> Value {
    let answer = vm.api_as(&administrator()).list_grants(resource);
    if answer.status != 200 {
        vm.fail(&format!(
            "expected `200` listing the Grants on {resource:?}, got\n{}",
            answer.describe()
        ));
    }
    blackbox::resources::without_who_and_when(&answer.json()["grants"])
}

/// `[{ group: CREW, access }]`.
fn crew_holds(access: &str) -> Value {
    json!([{ "group": CREW, "access": access }])
}

/// A `POST` of the platform's API as `user`, waiting as long as a write the
/// program gives up on.
fn lost_write(vm: &Vm, user: &User, target: &str, body: &Value) -> Request {
    vm.api_as(user)
        .request(target)
        .method("POST")
        .json(body)
        .timeout(LOST_WRITE_TIMEOUT)
}

/// The answer of a write sent while commits are held, which the program
/// gives up on: `503` `unavailable`.
fn assert_lost(answer: Result<Response, String>, what: &str) {
    let answer =
        answer.unwrap_or_else(|e| panic!("{what}: expected `503` `unavailable`, no answer ({e})"));
    assert_api_error(&answer, 503, "unavailable");
}

/// Fails unless none of `markers` reached the shared database.
fn assert_none_sent(db: &SharedDatabase, markers: &[String], context: &str) {
    for marker in markers {
        db.assert_not_received(marker, context);
    }
}

/// Fails unless every one of `checks` passed, listing those that did not.
fn assert_all(checks: &[Result<(), String>], context: &str) {
    let failed: Vec<&String> = checks.iter().filter_map(|c| c.as_ref().err()).collect();
    assert!(
        failed.is_empty(),
        "{context}:\n{}",
        failed
            .iter()
            .map(|f| format!("  {f}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// `Ok` when the `host-call` record of `user`'s call says `allowed`.
fn host_call_allowed(vm: &Vm, user: &str) -> Result<(), String> {
    match host_call_of(vm, user) {
        Some(record) if record["outcome"].as_str() == Some("allowed") => Ok(()),
        Some(record) => Err(format!(
            "expected the host to allow {user:?}'s call, as ever, its `host-call` record says {record}"
        )),
        None => Err(format!(
            "expected a `host-call` record for {user:?}'s call, there is none"
        )),
    }
}

// ---------------------------------------------------------------------------
// Grants
// ---------------------------------------------------------------------------

/// Once the lowered Grant's write is settled (no commit waiting), the
/// first write on [`LOWERED`] as `user`: refused `no-grant` with `held`
/// `read`, never `grants-unknown` ("When the program finds out") and never
/// allowed by the Grant as it was. Returns its marker (which may not reach
/// the shared database).
fn first_on_lowered(vm: &Vm, user: &User, context: &str) -> String {
    let m = marker("lowered-settled-first");
    let answer = call(
        vm,
        user,
        "board",
        &op("execute", &insert(LOWERED, &m)),
        context,
    );
    assert_refusal(
        &answer,
        "no-grant",
        &no_grant(LOWERED, user, "write", Some("read")),
        &format!(
            "{context}: the first write on {LOWERED:?} once its Grant's write is settled, decided by the Grant the database has"
        ),
    );
    m
}

/// Once the raised Grant's write is settled, the first write on
/// [`RAISED`] as `user`: allowed.
fn first_on_raised(vm: &Vm, user: &User, context: &str) {
    let m = marker("raised-settled-first");
    let answer = call(
        vm,
        user,
        "board",
        &op("execute", &insert(RAISED, &m)),
        context,
    );
    assert_executed(
        &answer,
        &["body"],
        &json!([[m]]),
        1,
        &format!(
            "{context}: the first write on {RAISED:?} once its Grant's write is settled, decided by the Grant the database has"
        ),
    );
}

#[test]
fn a_grant_changed_whose_answer_is_lost_never_leaves_the_old_level_deciding() {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_lost_grants");
    let s = Scenario::start_with(&VmOptions::new().platform(&platform));
    for resource in [LOWERED, RAISED, UNCHANGED] {
        s.register_writable(resource);
    }
    s.grant(LOWERED, CREW, "write");
    s.grant(RAISED, CREW, "read");
    s.grant(UNCHANGED, CREW, "write");
    s.install(
        "board",
        "operations.js",
        &[(LOWERED, "write"), (RAISED, "write"), (UNCHANGED, "write")],
    );
    s.install("viewer", "operations.js", &[(LOWERED, "read")]);
    s.install("outsider", "operations.js", &[]);
    let vm = &s.vm;

    // The harness's own check of the setting: crew writes on `lowered`.
    let first = marker("lowered-before");
    let answer = call(
        vm,
        &crew("crew-before"),
        "board",
        &op("execute", &insert(LOWERED, &first)),
        "before the change",
    );
    assert_executed(
        &answer,
        &["body"],
        &json!([[first]]),
        1,
        "the harness's own check: crew writes on `lowered` before its Grant is lowered",
    );

    postgres.hold();
    let lower = lost_write(
        vm,
        &administrator(),
        &format!("/v1/resources/{LOWERED}/grants"),
        &json!({ "group": CREW, "access": "read" }),
    );
    let raise = lost_write(
        vm,
        &administrator(),
        &format!("/v1/resources/{RAISED}/grants"),
        &json!({ "group": CREW, "access": "write" }),
    );
    let (lowered, raised) = thread::scope(|scope| {
        let lowered = scope.spawn(move || lower.try_send());
        postgres.wait_for_waiting(1, "the Grant lowered");
        let raised = scope.spawn(move || raise.try_send());
        postgres.wait_for_waiting(2, "the Grant raised");
        (lowered.join().unwrap(), raised.join().unwrap())
    });
    assert_lost(lowered, "the Grant lowered while commits are held");
    assert_lost(raised, "the Grant raised while commits are held");
    postgres.assert_waiting(2, "after the Grants' `503`");

    // Meanwhile, the API does not show the Grants of either Resource, at
    // once; the untouched Resource is answered as ever.
    let admin = vm.api_as(&administrator());
    let member = vm.api_as(&crew("crew-api"));
    for resource in [LOWERED, RAISED] {
        let context = format!("GET the Grants on {resource:?} while its Grant cannot be told");
        let answer = quickly(
            vm,
            &admin.request(&format!("/v1/resources/{resource}/grants")),
            &context,
        );
        assert_api_error(&answer, 503, "unavailable");
        let context = format!("GET the Resource {resource:?} while its Grant cannot be told");
        let answer = quickly(
            vm,
            &member.request(&format!("/v1/resources/{resource}")),
            &context,
        );
        assert_api_error(&answer, 503, "unavailable");
    }
    let answer = quickly(
        vm,
        &member.request("/v1/resources"),
        "GET the Resources while two Grants cannot be told",
    );
    assert_api_error(&answer, 503, "unavailable");
    let answer = quickly(
        vm,
        &admin.request(&format!("/v1/resources/{UNCHANGED}/grants")),
        "GET the Grants on the untouched Resource meanwhile",
    );
    assert_eq!(
        (
            answer.status,
            blackbox::resources::without_who_and_when(&answer.json()["grants"]),
        ),
        (200, crew_holds("write")),
        "the untouched Resource's Grants are answered as ever meanwhile, got\n{}",
        answer.describe()
    );
    let answer = quickly(
        vm,
        &member.request(&format!("/v1/resources/{UNCHANGED}")),
        "GET the untouched Resource meanwhile",
    );
    assert_eq!(
        (answer.status, answer.json()["your_access"].clone()),
        (200, json!("write")),
        "the untouched Resource is answered as ever meanwhile, got\n{}",
        answer.describe()
    );

    // Meanwhile, operations: each by a User of its own, sent together.
    let lowered_write = marker("lowered-meanwhile");
    let raised_write = marker("raised-meanwhile");
    let viewer_write = marker("viewer-meanwhile");
    let unchanged_write = marker("unchanged-meanwhile");
    let calls: Vec<(User, &str, Value)> = vec![
        (
            crew("crew-lowered-write"),
            "board",
            op("execute", &insert(LOWERED, &lowered_write)),
        ),
        (
            crew("crew-lowered-read"),
            "board",
            op("query", &select_one(LOWERED)),
        ),
        (
            crew("crew-raised-write"),
            "board",
            op("execute", &insert(RAISED, &raised_write)),
        ),
        (
            crew("crew-viewer-write"),
            "viewer",
            op("execute", &insert(LOWERED, &viewer_write)),
        ),
        (
            crew("crew-outsider-read"),
            "outsider",
            op("query", &select_one(LOWERED)),
        ),
        (
            crew("crew-unchanged-write"),
            "board",
            op("execute", &insert(UNCHANGED, &unchanged_write)),
        ),
    ];
    let (decided, answers) = thread::scope(|scope| {
        let sending: Vec<_> = calls
            .iter()
            .map(|(user, application, c)| {
                let request = call_request(vm, user, application, c);
                scope.spawn(move || request.try_send())
            })
            .collect();
        let decided = vec![
            host_call_refused(vm, "crew-lowered-write", "grants-unknown"),
            host_call_refused(vm, "crew-lowered-read", "grants-unknown"),
            host_call_refused(vm, "crew-raised-write", "grants-unknown"),
            host_call_refused(vm, "crew-viewer-write", "declared-lower"),
            host_call_refused(vm, "crew-outsider-read", "not-declared"),
            host_call_allowed(vm, "crew-unchanged-write"),
        ];
        // The operations' entries wait with every other commit, and
        // still do when the program stops waiting for them: each operation
        // is answered then, commits still held, with its own refusal.
        let answers: Vec<Result<Response, String>> =
            sending.into_iter().map(|h| h.join().unwrap()).collect();
        (decided, answers)
    });
    assert_all(
        &decided,
        "while the Grants on `lowered` and `raised` cannot be told, the host decided",
    );
    let answers: Vec<Value> = answers
        .into_iter()
        .zip(&calls)
        .map(|(answer, (user, _, _))| {
            let response = answer.unwrap_or_else(|e| {
                vm.fail(&format!(
                    "{:?}'s call made meanwhile: no answer within {LOST_WRITE_TIMEOUT:?}, commits still held ({e})",
                    user.id
                ))
            });
            answer_of(
                vm,
                &response,
                &format!("{:?}'s call made meanwhile", user.id),
            )
        })
        .collect();
    assert!(
        postgres.waiting() > 0,
        "the harness's own check: expected commits still held when the operations made meanwhile were answered"
    );
    assert_grants_unknown(
        &answers[0],
        LOWERED,
        "a write on `lowered` meanwhile, answered while its entry cannot be written",
    );
    assert_grants_unknown(
        &answers[1],
        LOWERED,
        "a read on `lowered` meanwhile, answered while its entry cannot be written",
    );
    assert_grants_unknown(
        &answers[2],
        RAISED,
        "a write on `raised` meanwhile, answered while its entry cannot be written",
    );
    assert_refusal(
        &answers[3],
        "declared-lower",
        &json!({ "resource": LOWERED, "declared": "read", "needed": "write" }),
        "a write on `lowered` by an Application declaring it at `read`, meanwhile",
    );
    assert_refusal(
        &answers[4],
        "not-declared",
        &json!({ "resource": LOWERED }),
        "a read on `lowered` by an Application declaring nothing, meanwhile",
    );
    // Allowed as ever (its `host-call` record), and like any allowed
    // operation whose entry cannot be written, refused `log-unavailable`.
    assert_refusal(
        &answers[5],
        "log-unavailable",
        &json!({ "resource": UNCHANGED }),
        "a write on the untouched Resource meanwhile, its entry not written",
    );
    let mut refused = vec![lowered_write, raised_write, viewer_write, unchanged_write];
    assert_none_sent(&s.db, &refused, "operations made meanwhile");

    // Commits go through; once every write has landed, and with nothing
    // asked of the program about these Resources meanwhile, the first
    // operation on each is decided by the Grant the database has.
    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    let settled = crew("crew-settled");
    refused.push(first_on_lowered(vm, &settled, "once commits go through"));
    first_on_raised(vm, &settled, "once commits go through");
    let m = marker("unchanged-settled");
    let answer = call(
        vm,
        &settled,
        "board",
        &op("execute", &insert(UNCHANGED, &m)),
        "the untouched Resource, settled",
    );
    assert_executed(
        &answer,
        &["body"],
        &json!([[m]]),
        1,
        "a write on the untouched Resource, decided as ever",
    );
    assert_eq!(
        grants(vm, LOWERED),
        crew_holds("read"),
        "the lowered Grant, read through the API once settled"
    );
    assert_eq!(
        grants(vm, RAISED),
        crew_holds("write"),
        "the raised Grant, read through the API once settled"
    );
    let resource = member.get_resource(LOWERED);
    assert_eq!(
        (resource.status, resource.json()["your_access"].clone()),
        (200, json!("read")),
        "`lowered` as crew reads it once settled, got\n{}",
        resource.describe()
    );
    for round in 0..3 {
        let context = format!("settled, round {round}");
        let user = crew("crew-settled");
        let m = marker("lowered-settled");
        let answer = call(
            vm,
            &user,
            "board",
            &op("execute", &insert(LOWERED, &m)),
            &context,
        );
        refused.push(m);
        assert_refusal(
            &answer,
            "no-grant",
            &no_grant(LOWERED, &user, "write", Some("read")),
            &context,
        );
        let answer = call(
            vm,
            &user,
            "board",
            &op("query", &select_one(LOWERED)),
            &context,
        );
        assert_rows(&answer, &["one"], &json!([[1]]), &context);
        let m = marker("raised-settled");
        let answer = call(
            vm,
            &user,
            "board",
            &op("execute", &insert(RAISED, &m)),
            &context,
        );
        assert_executed(&answer, &["body"], &json!([[m]]), 1, &context);
    }
    assert_none_sent(
        &s.db,
        &refused,
        "every write refused on `lowered` and `raised`",
    );
}

// ---------------------------------------------------------------------------
// The serving Version
// ---------------------------------------------------------------------------

/// The marker of Version `number` of `application`.
fn version_marker(application: &str, number: u64) -> String {
    format!("{application}-v{number}")
}

/// Creates `application` as [`INSTALLER`], shares it with everyone, and
/// sends `versions` Versions of `fixtures/which-version.js`, each marked
/// with [`version_marker`].
fn create_with_versions(vm: &Vm, application: &str, versions: u64) {
    let api = vm.api_as(&User::new(INSTALLER));
    api.create_application(application).expect_status(201);
    api.share_with_everyone(application);
    for number in 1..=versions {
        let sent = api.send_version(
            application,
            &version_body(&marked_bundle(&version_marker(application, number))),
        );
        if sent.status != 201 {
            vm.fail(&format!(
                "expected `201` sending Version {number} of {application:?}, got\n{}",
                sent.describe()
            ));
        }
    }
}

/// `GET /` of `application` as the visitor.
fn visit(vm: &Vm, application: &str) -> Request {
    vm.request(application, "/")
        .as_user(&visitor())
        .timeout(Duration::from_secs(10))
}

/// The marker of the bundle that answered, if one did.
fn served_marker(response: &Response) -> Option<String> {
    if response.status != 200 {
        return None;
    }
    serde_json::from_slice::<Value>(response.body())
        .ok()
        .and_then(|b| b["marker"].as_str().map(str::to_owned))
}

/// `serving_version` of `application` as its Contributor reads it.
fn serving_version(vm: &Vm, application: &str) -> u64 {
    let answer = vm
        .api_as(&User::new(INSTALLER))
        .get_application(application);
    if answer.status != 200 {
        vm.fail(&format!(
            "expected `200` reading {application:?}, got\n{}",
            answer.describe()
        ));
    }
    answer.json()["serving_version"]
        .as_u64()
        .unwrap_or_else(|| {
            vm.fail(&format!(
                "expected {application:?} to have a serving Version"
            ))
        })
}

/// Fails unless the request was answered by Version `number` of
/// `application`'s bundle.
fn assert_served(response: &Response, application: &str, number: u64, context: &str) {
    assert_eq!(
        served_marker(response).as_deref(),
        Some(version_marker(application, number).as_str()),
        "{context}: expected {application:?} served by Version {number}, got\n{}",
        response.describe()
    );
}

/// Once the write is settled (no commit waiting): the **first** request to
/// `application` is served, never by Version `never` (the one that served
/// before the write), and by the Version the API says serves; so are the
/// next ones.
fn served_once_settled(vm: &Vm, application: &str, never: u64, context: &str) {
    let first = quickly(vm, &visit(vm, application), context);
    assert_ne!(
        served_marker(&first).as_deref(),
        Some(version_marker(application, never).as_str()),
        "{context}: {application:?} was served by Version {never}, which served before the write whose answer was lost"
    );
    assert_eq!(
        first.status,
        200,
        "{context}: expected the first request to {application:?} once the write is settled served, got\n{}",
        first.describe()
    );
    let serving = serving_version(vm, application);
    assert_served(
        &first,
        application,
        serving,
        &format!(
            "{context}, the first request: the API says Version {serving} serves, so must the address"
        ),
    );
    for round in 0..5 {
        let response = quickly(vm, &visit(vm, application), context);
        assert_served(
            &response,
            application,
            serving,
            &format!(
                "{context}, round {round}: the API says Version {serving} serves, so must the address"
            ),
        );
    }
}

#[test]
fn a_go_back_or_a_version_whose_answer_is_lost_never_leaves_another_version_serving() {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_lost_versions");
    let vm = Vm::start(&VmOptions::new().stand_in_sign_in().platform(&platform));
    create_with_versions(&vm, "board", 2);
    create_with_versions(&vm, "ledger", 2);
    create_with_versions(&vm, "other", 1);
    for (application, number) in [("board", 2), ("ledger", 2), ("other", 1)] {
        let response = visit(&vm, application).send();
        assert_served(
            &response,
            application,
            number,
            "the harness's own check, before",
        );
    }

    postgres.hold();
    let installer = User::new(INSTALLER);
    let go_back = lost_write(
        &vm,
        &installer,
        "/v1/applications/board/go-back",
        &json!({ "from": 2, "to": 1 }),
    );
    let send = lost_write(
        &vm,
        &installer,
        "/v1/applications/ledger/versions",
        &version_body(&marked_bundle(&version_marker("ledger", 3))),
    );
    let (went_back, sent) = thread::scope(|scope| {
        let went_back = scope.spawn(move || go_back.try_send());
        postgres.wait_for_waiting(1, "the go-back");
        let sent = scope.spawn(move || send.try_send());
        postgres.wait_for_waiting(2, "the Version sent");
        (went_back.join().unwrap(), sent.join().unwrap())
    });
    assert_lost(went_back, "the go-back while commits are held");
    assert_lost(sent, "the Version sent while commits are held");
    postgres.assert_waiting(2, "after the `503` of the go-back and of the Version");

    // Meanwhile: neither Application is served on a guess; the other is
    // served as ever. All at once.
    for round in 0..5 {
        for application in ["board", "ledger"] {
            let context =
                format!("{application:?} while its serving Version cannot be told, round {round}");
            let response = quickly(&vm, &visit(&vm, application), &context);
            assert_503_retry(&response, &context);
        }
        let context = format!("the other Application meanwhile, round {round}");
        let response = quickly(&vm, &visit(&vm, "other"), &context);
        assert_served(&response, "other", 1, &context);
        thread::sleep(Duration::from_millis(300));
    }

    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    served_once_settled(&vm, "board", 2, "the go-back, once commits go through");
    served_once_settled(
        &vm,
        "ledger",
        2,
        "the Version sent, once commits go through",
    );
    assert_eq!(
        serving_version(&vm, "board"),
        1,
        "the go-back was made after all"
    );
    assert_eq!(
        serving_version(&vm, "ledger"),
        3,
        "the Version sent was made after all"
    );
    assert_served(
        &visit(&vm, "other").send(),
        "other",
        1,
        "the other Application, settled",
    );
}

// ---------------------------------------------------------------------------
// A Resource registered
// ---------------------------------------------------------------------------

#[test]
fn a_resource_registered_whose_answer_is_lost_is_known_without_a_restart() {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_lost_resource");
    let s = Scenario::start_with(&VmOptions::new().platform(&platform));
    s.install("board", "operations.js", &[(LATE, "read")]);
    let vm = &s.vm;
    let answer = call(
        vm,
        &crew("crew-before"),
        "board",
        &op("query", &select_one(LATE)),
        "before",
    );
    assert_refusal(
        &answer,
        "not-registered",
        &json!({ "resource": LATE }),
        "the harness's own check: before it is registered",
    );

    postgres.hold();
    let register = lost_write(
        vm,
        &administrator(),
        "/v1/resources",
        &resource_body(LATE, &s.db, &s.db.reader),
    );
    let registered = thread::scope(|scope| {
        let registered = scope.spawn(move || register.try_send());
        postgres.wait_for_waiting(1, "the Resource registered");
        registered.join().unwrap()
    });
    assert_lost(registered, "the Resource registered while commits are held");
    postgres.assert_waiting(1, "after the registration's `503`");

    // Meanwhile the program does not know it, as before the request.
    let (decided, meanwhile) = thread::scope(|scope| {
        let request = call_request(
            vm,
            &crew("crew-meanwhile"),
            "board",
            &op("query", &select_one(LATE)),
        );
        let sending = scope.spawn(move || request.try_send());
        let decided = host_call_refused(vm, "crew-meanwhile", "not-registered");
        postgres.let_through();
        (decided, sending.join().unwrap())
    });
    assert_all(&[decided], "while the registration is on its way");
    let meanwhile = meanwhile.unwrap_or_else(|e| vm.fail(&format!("no answer meanwhile ({e})")));
    assert_refusal(
        &answer_of(vm, &meanwhile, "meanwhile"),
        "not-registered",
        &json!({ "resource": LATE }),
        "a read on `late` while its registration is on its way",
    );

    // Once commits go through: registering it again is `name-taken`, and
    // from that answer on it is there for operations, decided by its
    // Grants (none yet), without a restart.
    let again = s
        .admin()
        .create_resource(&resource_body(LATE, &s.db, &s.db.reader));
    assert_api_error(&again, 409, "name-taken");
    let user = crew("crew-after");
    let answer = call(vm, &user, "board", &op("query", &select_one(LATE)), "after");
    assert_refusal(
        &answer,
        "no-grant",
        &no_grant(LATE, &user, "read", None),
        "a read on `late` once registering it again answered `name-taken`: known, no Grant yet",
    );
    let resource = s.admin().get_resource(LATE);
    assert_eq!(
        resource.status,
        200,
        "`late` read through the API once settled, got\n{}",
        resource.describe()
    );
    s.grant(LATE, CREW, "read");
    let answer = call(
        vm,
        &user,
        "board",
        &op("query", &select_one(LATE)),
        "granted",
    );
    assert_rows(
        &answer,
        &["one"],
        &json!([[1]]),
        "a read on `late` once crew is granted `read`",
    );
}

// ---------------------------------------------------------------------------
// A start while writes are on their way
// ---------------------------------------------------------------------------

/// Once commits go through, for a program started meanwhile or after: the
/// first request to `site` is served by the Version the database says serves and never by
/// Version 2 (gone back from).
fn settle_site(vm: &Vm, context: &str) {
    served_once_settled(vm, "site", 2, context);
    assert_eq!(
        serving_version(vm, "site"),
        1,
        "{context}: the go-back was made after all"
    );
}

/// Once commits go through, for a program started meanwhile or after: the
/// first write on [`LOWERED`] is refused `no-grant` (never `grants-unknown`,
/// never allowed by the Grant as it was). Returns the markers of the writes
/// refused.
fn settle_grant(vm: &Vm, context: &str) -> Vec<String> {
    let refused = vec![first_on_lowered(vm, &crew("crew-settled"), context)];
    assert_eq!(
        grants(vm, LOWERED),
        crew_holds("read"),
        "{context}: the lowered Grant, read through the API"
    );
    refused
}

#[test]
fn a_program_started_while_writes_are_on_their_way_never_decides_by_what_it_read_then() {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_lost_at_start");
    let s = Scenario::start_with(&VmOptions::new().platform(&platform));
    let options = s.vm.options().clone().platform(&platform);
    s.register_writable(LOWERED);
    s.grant(LOWERED, CREW, "write");
    s.install("board", "operations.js", &[(LOWERED, "write")]);
    // `board` makes the operations; `site` is the Application whose
    // serving Version is changed.
    let Scenario { vm, db } = s;
    create_with_versions(&vm, "site", 2);
    assert_served(
        &visit(&vm, "site").send(),
        "site",
        2,
        "the harness's own check, before",
    );
    let first = marker("lowered-before");
    let answer = call(
        &vm,
        &crew("crew-before"),
        "board",
        &op("execute", &insert(LOWERED, &first)),
        "before",
    );
    assert_executed(
        &answer,
        &["body"],
        &json!([[first]]),
        1,
        "the harness's own check: crew writes on `lowered` before its Grant is lowered",
    );

    postgres.hold();
    let lower = lost_write(
        &vm,
        &administrator(),
        &format!("/v1/resources/{LOWERED}/grants"),
        &json!({ "group": CREW, "access": "read" }),
    );
    let go_back = lost_write(
        &vm,
        &User::new(INSTALLER),
        "/v1/applications/site/go-back",
        &json!({ "from": 2, "to": 1 }),
    );
    // Both are sent; once both commits wait, the program is killed.
    thread::scope(|scope| {
        let lowering = scope.spawn(move || lower.try_send());
        postgres.wait_for_waiting(1, "the Grant lowered");
        let going_back = scope.spawn(move || go_back.try_send());
        postgres.wait_for_waiting(2, "the go-back");
        drop(vm);
        // Whatever they got (most likely no answer at all).
        drop(lowering.join());
        drop(going_back.join());
    });
    postgres.assert_waiting(2, "after the program was stopped");

    let mut refused = Vec::new();
    match Vm::try_start(&options, Duration::from_secs(20)) {
        Ok(vm) => {
            // Started while both writes are on their way: neither the
            // serving Version nor the Grants it read then decide.
            for round in 0..5 {
                let context =
                    format!("`site`, started while the go-back is on its way, round {round}");
                let response = quickly(&vm, &visit(&vm, "site"), &context);
                assert_ne!(
                    served_marker(&response).as_deref(),
                    Some(version_marker("site", 2).as_str()),
                    "{context}: served by Version 2, read while the go-back from it was on its way"
                );
                assert_503_retry(&response, &context);
                thread::sleep(Duration::from_millis(300));
            }
            let meanwhile = marker("lowered-at-start");
            let (decided, answer) = thread::scope(|scope| {
                let request = call_request(
                    &vm,
                    &crew("crew-at-start"),
                    "board",
                    &op("execute", &insert(LOWERED, &meanwhile)),
                );
                let sending = scope.spawn(move || request.try_send());
                let decided = host_call_refused(&vm, "crew-at-start", "grants-unknown");
                // Answered while commits are still held, once the program
                // stops waiting for its entry.
                (decided, sending.join().unwrap())
            });
            refused.push(meanwhile);
            postgres.assert_waiting(
                2,
                "when the operation made meanwhile was answered, before commits go through",
            );
            let answer = answer.unwrap_or_else(|e| {
                vm.fail(&format!(
                    "no answer within {LOST_WRITE_TIMEOUT:?}, commits still held ({e})"
                ))
            });
            let answer = answer_of(&vm, &answer, "started meanwhile");
            postgres.let_through();
            postgres.wait_until_none_waiting("once commits go through");
            // The serving Version first, so that its fault is seen apart
            // from the Grant's.
            settle_site(&vm, "started meanwhile, once commits go through");
            assert_all(
                &[decided],
                "started while the lowered Grant was on its way, the host decided",
            );
            assert_grants_unknown(
                &answer,
                LOWERED,
                "a write on `lowered` by a program started while its Grant was on its way, answered while its entry cannot be written",
            );
            refused.extend(settle_grant(
                &vm,
                "started meanwhile, once commits go through",
            ));
        }
        Err(_) => {
            // It did not start while the commits waited: allowed. Once
            // commits go through, a program starts and decides by what the
            // database has.
            postgres.let_through();
            postgres.wait_until_none_waiting("once commits go through");
            let vm = Vm::start(&options);
            settle_site(&vm, "started after commits go through");
            refused.extend(settle_grant(&vm, "started after commits go through"));
        }
    }
    assert_none_sent(&db, &refused, "every write refused on `lowered`");
}
