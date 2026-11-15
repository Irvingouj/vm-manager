//! Tool credentials on the platform's API (CONTRACT.md, "The tool: On the
//! platform's API"; ticket "The tool signs in, and lists Resources and
//! Applications"): `POST /v1/tool-credentials` makes one for a User signed
//! in otherwise, `Authorization: Bearer` signs in with it on `api.<domain>`
//! and nowhere else, decided by it alone, `DELETE
//! /v1/tool-credentials/current` ends it, `GET /v1/me` says who is signed
//! in, and an ended credential is refused the same whatever ended it. The
//! platform keeps no usable form of it and never logs it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::BTreeSet;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use blackbox::api::{assert_json_content_type, parse_rfc3339, version_body};
use blackbox::postgres::{Platform, PrivatePostgres};
use blackbox::records::event;
use blackbox::scenario::{ADMINISTRATORS, Scenario, administrator, resource_body};
use blackbox::shared_database::token;
use blackbox::tool::{bearer, platform_tables_holding, windows_in};
use blackbox::{Response, User, Vm, VmOptions, assert_api_error};
use serde_json::{Value, json};

const THIRTY_DAYS: i64 = 30 * 24 * 60 * 60;

fn alice() -> User {
    User::new("alice").in_groups(&["readers", "staff"])
}

fn bob() -> User {
    User::new("bob").in_groups(&["bob-team"])
}

fn vm() -> Vm {
    Vm::start(
        &VmOptions::new()
            .stand_in_sign_in()
            .administrators_group(ADMINISTRATORS),
    )
}

/// The keys of a JSON object, sorted.
fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

fn groups_set(value: &Value) -> BTreeSet<String> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("expected an array of Groups, got {value}"))
        .iter()
        .map(|g| {
            g.as_str()
                .unwrap_or_else(|| panic!("expected Groups as strings, got {value}"))
                .to_owned()
        })
        .collect()
}

fn set_of(user: &User) -> BTreeSet<String> {
    user.groups.iter().cloned().collect()
}

fn now() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    i64::try_from(secs).unwrap()
}

/// Fails unless `value` is an RFC 3339 time `seconds` from now, give or
/// take `slack`.
fn assert_expires_in(value: &Value, seconds: i64, slack: i64, context: &str) {
    let text = value
        .as_str()
        .unwrap_or_else(|| panic!("{context}: expected an RFC 3339 time, got {value}"));
    let at = parse_rfc3339(text)
        .unwrap_or_else(|| panic!("{context}: expected an RFC 3339 time, got {text:?}"));
    let want = now() + seconds;
    assert!(
        (at - want).abs() <= slack,
        "{context}: expected a time {seconds} s from now (within {slack} s), got {text:?}"
    );
}

/// The `401` a request gets from someone not signed in at all.
fn not_signed_in(vm: &Vm) -> Response {
    let answer = vm.api_request("/v1/me").send();
    assert_eq!(
        answer.status,
        401,
        "the harness's own check: a request with no sign-in is 401, got\n{}",
        answer.describe()
    );
    answer
}

/// Fails unless `answer` is the very `401` anyone not signed in gets: the
/// same status, body and content type.
fn assert_same_401(answer: &Response, reference: &Response, context: &str) {
    assert_eq!(
        answer.status,
        401,
        "{context}: expected `401`, got\n{}",
        answer.describe()
    );
    assert_eq!(
        answer.text(),
        reference.text(),
        "{context}: expected the same body as for anyone not signed in"
    );
    assert_eq!(
        answer.header("content-type"),
        reference.header("content-type"),
        "{context}: expected the same content type as for anyone not signed in"
    );
}

#[test]
fn a_user_signed_in_with_the_stand_in_is_given_a_tool_credential() {
    let vm = vm();
    let api = vm.api_as(&alice());
    let mut seen = BTreeSet::new();
    for answer in [
        api.post_raw("/v1/tool-credentials", ""),
        api.create_tool_credential(),
        vm.api_request("/v1/tool-credentials")
            .method("POST")
            .as_user(&alice())
            .send(),
    ] {
        assert!(
            answer.status == 201,
            "expected `201` making a tool credential, got status {}",
            answer.status
        );
        assert_json_content_type(&answer);
        let body = answer.json();
        assert_eq!(
            keys(&body),
            [
                "credential",
                "expires_at",
                "groups",
                "platform_administrator",
                "user"
            ],
            "expected {{ credential, user, groups, platform_administrator, expires_at }} and nothing else"
        );
        let credential = body["credential"].as_str().unwrap_or_default();
        assert!(
            credential.starts_with("vmtc_"),
            "expected the credential's text to begin with `vmtc_`"
        );
        assert!(
            credential.len() >= "vmtc_".len() + 43,
            "expected a credential of 32 random bytes or more (43 characters or more as text after `vmtc_`), got one of {} characters",
            credential.len()
        );
        assert!(
            seen.insert(credential.to_owned()),
            "expected every credential made to be new"
        );
        assert_eq!(body["user"], "alice");
        assert_eq!(groups_set(&body["groups"]), set_of(&alice()));
        assert_eq!(body["platform_administrator"], false);
        assert_expires_in(&body["expires_at"], THIRTY_DAYS, 300, "expires_at");
    }
    let (_, admin) = vm.api_as(&administrator()).tool_credential();
    assert_eq!(admin["platform_administrator"], true);
    assert_eq!(admin["user"], administrator().id);
}

#[test]
fn get_me_says_who_is_signed_in() {
    let vm = vm();
    for (user, admin) in [
        (alice(), false),
        (administrator(), true),
        (User::new("ünïcode"), false),
    ] {
        let answer = vm.api_as(&user).me();
        answer.expect_status(200);
        assert_json_content_type(&answer);
        let body = answer.json();
        assert_eq!(
            keys(&body),
            ["groups", "platform_administrator", "user"],
            "expected {{ user, groups, platform_administrator }}, got {body}"
        );
        assert_eq!(body["user"], user.id.as_str());
        assert_eq!(groups_set(&body["groups"]), set_of(&user));
        assert_eq!(body["platform_administrator"], admin);
    }
    not_signed_in(&vm);
}

#[test]
fn a_tool_credential_signs_in_to_every_request_of_the_api() {
    let s = Scenario::start();
    let (credential, _) = s.vm.api_as(&alice()).tool_credential();

    let me = bearer(&s.vm, "/v1/me", &credential).send();
    me.expect_status(200);
    let body = me.json();
    assert_eq!(body["user"], "alice");
    assert_eq!(groups_set(&body["groups"]), set_of(&alice()));
    assert_eq!(body["platform_administrator"], false);
    let record = s.vm.records_of(&me).request;
    assert_eq!(
        record["user"], "alice",
        "expected the `request` record to name the User of the credential, got {record}"
    );

    let created = bearer(&s.vm, "/v1/applications", &credential)
        .method("POST")
        .json(&json!({ "name": "board" }))
        .send();
    created.expect_status(201);
    assert_eq!(created.json()["created_by"], "alice");
    bearer(&s.vm, "/v1/applications/board/versions", &credential)
        .method("POST")
        .json(&version_body("globalThis.default = {};"))
        .send()
        .expect_status(201);
    bearer(&s.vm, "/v1/applications/board", &credential)
        .send()
        .expect_status(200);
    bearer(&s.vm, "/v1/resources", &credential)
        .send()
        .expect_status(200);
    // What only a Platform administrator may do, as one, with a tool
    // credential alone.
    assert_api_error(
        &bearer(&s.vm, "/v1/resources", &credential)
            .method("POST")
            .json(&resource_body("devops", &s.db, &s.db.reader))
            .send(),
        403,
        "not-an-administrator",
    );
    let (admin, _) = s.admin().tool_credential();
    bearer(&s.vm, "/v1/resources", &admin)
        .method("POST")
        .json(&resource_body("devops", &s.db, &s.db.reader))
        .send()
        .expect_status(201);
    bearer(&s.vm, "/v1/resources/devops/grants", &admin)
        .method("POST")
        .json(&json!({ "group": "readers", "access": "read" }))
        .send()
        .expect_status(200);
    bearer(&s.vm, "/v1/operation-log", &admin)
        .send()
        .expect_status(200);
}

#[test]
fn a_credential_cannot_renew_itself() {
    let vm = vm();
    let (credential, _) = vm.api_as(&alice()).tool_credential();
    for request in [
        bearer(&vm, "/v1/tool-credentials", &credential)
            .method("POST")
            .json(&json!({})),
        bearer(&vm, "/v1/tool-credentials", &credential).method("POST"),
        // Stand-in headers beside it change nothing.
        bearer(&vm, "/v1/tool-credentials", &credential)
            .method("POST")
            .as_user(&alice())
            .json(&json!({})),
    ] {
        let answer = request.send();
        assert!(
            !answer.text().contains(&credential),
            "an answer that is not the 201 must not hold a credential"
        );
        assert_api_error(&answer, 403, "not-with-a-tool-credential");
    }
}

#[test]
fn an_authorization_header_alone_decides_the_sign_in() {
    let vm = vm();
    let reference = not_signed_in(&vm);
    let unknown = format!("{}{}{}{}", token(), token(), token(), token());
    for authorization in [
        format!("Bearer {unknown}"),
        "Bearer ".to_owned(),
        "Bearer".to_owned(),
        "Basic YWxpY2U6c2VjcmV0".to_owned(),
        unknown.clone(),
    ] {
        for (target, method) in [
            ("/v1/me", "GET"),
            ("/v1/resources", "GET"),
            ("/v1/applications", "GET"),
            ("/v1/tool-credentials", "POST"),
        ] {
            for with_stand_in in [false, true] {
                let mut request = vm
                    .api_request(target)
                    .method(method)
                    .header("authorization", &authorization);
                if with_stand_in {
                    request = request.as_user(&alice());
                }
                let answer = request.send();
                assert_same_401(
                    &answer,
                    &reference,
                    &format!(
                        "{method} {target} with `Authorization: {}...` (stand-in headers: {with_stand_in})",
                        authorization.split(' ').next().unwrap_or_default()
                    ),
                );
                let record = vm.records_of(&answer).request;
                assert!(
                    record["user"].is_null(),
                    "expected no User in the `request` record of a 401, got {record}"
                );
            }
        }
    }
    let created = vm
        .api_request("/v1/applications")
        .method("POST")
        .bearer(&unknown)
        .as_user(&alice())
        .json(&json!({ "name": "sneaked" }))
        .send();
    assert_same_401(&created, &reference, "creating with an unknown credential");
    assert_api_error(
        &vm.api_as(&alice()).get_application("sneaked"),
        404,
        "not-found",
    );
}

#[test]
fn malformed_stand_in_headers_are_still_400_beside_a_credential() {
    let vm = vm();
    let (credential, _) = vm.api_as(&alice()).tool_credential();
    for request in [
        bearer(&vm, "/v1/me", &credential)
            .header("x-vm-stand-in-user", "alice")
            .header("x-vm-stand-in-user", "bob"),
        bearer(&vm, "/v1/me", &credential).header("x-vm-stand-in-user", ""),
        bearer(&vm, "/v1/me", &credential)
            .header("x-vm-stand-in-user", "alice")
            .header("x-vm-stand-in-groups", "a")
            .header("x-vm-stand-in-groups", "b"),
        bearer(&vm, "/v1/me", &format!("{}{}", token(), token()))
            .header("x-vm-stand-in-user", "a\u{1}b"),
    ] {
        let answer = request.send();
        assert_eq!(
            answer.status,
            400,
            "expected `400` for {request}, got\n{}",
            answer.describe()
        );
    }
}

#[test]
fn a_credential_of_one_user_never_answers_as_the_stand_in_beside_it() {
    let s = Scenario::start();
    let (credential, _) = s.vm.api_as(&alice()).tool_credential();
    for beside in [
        bob(),
        administrator(),
        User::new("alice").in_groups(&[ADMINISTRATORS]),
    ] {
        let me = bearer(&s.vm, "/v1/me", &credential).as_user(&beside).send();
        me.expect_status(200);
        let body = me.json();
        assert_eq!(
            body["user"], "alice",
            "expected the credential's User, whatever stand-in headers come with it, got {body}"
        );
        assert_eq!(
            groups_set(&body["groups"]),
            set_of(&alice()),
            "expected the Groups of the moment the credential was made, got {body}"
        );
        assert_eq!(body["platform_administrator"], false);
        assert_api_error(
            &bearer(&s.vm, "/v1/resources", &credential)
                .as_user(&beside)
                .method("POST")
                .json(&resource_body("devops", &s.db, &s.db.reader))
                .send(),
            403,
            "not-an-administrator",
        );
    }
    let created = bearer(&s.vm, "/v1/applications", &credential)
        .as_user(&bob())
        .method("POST")
        .json(&json!({ "name": "whose" }))
        .send();
    created.expect_status(201);
    assert_eq!(created.json()["created_by"], "alice");
    assert_eq!(created.json()["contributors"], json!([{ "user": "alice" }]));
    let record = s.vm.records_of(&created).request;
    assert_eq!(record["user"], "alice", "{record}");
}

#[test]
fn a_tool_credential_is_not_a_sign_in_on_an_applications_address() {
    let vm = Vm::start(
        &VmOptions::new()
            .stand_in_sign_in()
            .application("board", "which-version.js"),
    );
    let (credential, _) = vm.api_as(&alice()).tool_credential();
    let alone = vm.request("board", "/").bearer(&credential).send();
    assert_eq!(
        alone.status,
        401,
        "expected `401` on an Application's address with a tool credential alone, got\n{}",
        alone.describe()
    );
    let records = vm.records_of(&alone);
    assert!(
        records.request["user"].is_null(),
        "expected no User in the 401's record, got {}",
        records.request
    );
    let beside = vm
        .request("board", "/")
        .bearer(&credential)
        .as_user(&bob())
        .send();
    beside.expect_status(200);
    let body = beside.json();
    assert_eq!(
        body["user"]["id"], "bob",
        "expected the stand-in's User on an Application's address, the tool credential counting for nothing, got {body}"
    );
}

#[test]
fn ending_the_current_credential() {
    let vm = vm();
    let reference = not_signed_in(&vm);
    let api = vm.api_as(&alice());
    let (first, _) = api.tool_credential();
    let (second, _) = api.tool_credential();
    let ended = bearer(&vm, "/v1/tool-credentials/current", &first)
        .method("DELETE")
        .send();
    assert_eq!(
        ended.status, 204,
        "expected `204`, got status {}",
        ended.status
    );
    assert!(
        !ended.text().contains(&first),
        "the 204 must not hold the credential"
    );
    assert_same_401(
        &bearer(&vm, "/v1/me", &first).send(),
        &reference,
        "the ended credential",
    );
    assert_same_401(
        &bearer(&vm, "/v1/tool-credentials/current", &first)
            .method("DELETE")
            .send(),
        &reference,
        "ending the ended credential again",
    );
    // The other credential of the same User is untouched.
    bearer(&vm, "/v1/me", &second).send().expect_status(200);
    // Without a credential, or signed in otherwise.
    for answer in [
        api.request("/v1/tool-credentials/current")
            .method("DELETE")
            .send(),
        vm.api_as(&administrator())
            .request("/v1/tool-credentials/current")
            .method("DELETE")
            .send(),
    ] {
        assert_api_error(&answer, 404, "not-found");
    }
    assert_same_401(
        &vm.api_request("/v1/tool-credentials/current")
            .method("DELETE")
            .send(),
        &reference,
        "ending with no sign-in at all",
    );
    bearer(&vm, "/v1/me", &second).send().expect_status(200);
}

#[test]
fn an_ended_credential_is_refused_alike_whether_expired_signed_out_or_never_made() {
    let vm = Vm::start(
        &VmOptions::new()
            .stand_in_sign_in()
            .tool_credential_lifetime(2),
    );
    let reference = not_signed_in(&vm);
    let api = vm.api_as(&alice());
    let (expiring, made) = api.tool_credential();
    assert_expires_in(
        &made["expires_at"],
        2,
        60,
        "expires_at with a lifetime of 2 s",
    );
    bearer(&vm, "/v1/me", &expiring).send().expect_status(200);
    let (signed_out, _) = api.tool_credential();
    bearer(&vm, "/v1/tool-credentials/current", &signed_out)
        .method("DELETE")
        .send()
        .expect_status(204);
    let never = format!("{}{}{}{}", token(), token(), token(), token());
    thread::sleep(Duration::from_millis(3500));
    for (what, credential) in [
        ("expired", &expiring),
        ("signed out", &signed_out),
        ("never made", &never),
    ] {
        for target in ["/v1/me", "/v1/resources", "/v1/applications"] {
            assert_same_401(
                &bearer(&vm, target, credential).send(),
                &reference,
                &format!("GET {target} with a credential {what}"),
            );
        }
    }
}

#[test]
fn the_credential_is_never_logged_and_kept_only_in_a_form_that_cannot_be_used() {
    let vm = vm();
    let api = vm.api_as(&alice());
    let (credential, _) = api.tool_credential();
    let mut answers = vec![
        bearer(&vm, "/v1/me", &credential).send(),
        bearer(&vm, "/v1/resources", &credential).send(),
        bearer(&vm, "/v1/applications", &credential).send(),
        bearer(&vm, "/v1/tool-credentials", &credential)
            .method("POST")
            .send(),
        bearer(&vm, "/v1/no-such-path", &credential).send(),
    ];
    assert_eq!(answers[0].status, 200, "{}", answers[0].describe());
    let (ended, _) = api.tool_credential();
    answers.push(
        bearer(&vm, "/v1/tool-credentials/current", &ended)
            .method("DELETE")
            .send(),
    );
    answers.push(bearer(&vm, "/v1/me", &ended).send());

    let mut texts: Vec<(String, String)> = answers
        .iter()
        .map(|a| (format!("the answer to {}", a.request()), a.text()))
        .collect();
    texts.extend(
        vm.lines()
            .into_iter()
            .map(|l| ("the program's log".to_owned(), l.raw)),
    );
    assert!(
        vm.records().iter().any(|r| event(r) == "request"),
        "the harness's own check: the log has request records"
    );
    for (what, secret) in [
        ("the credential", &credential),
        ("the ended credential", &ended),
    ] {
        let found = windows_in(&texts, secret);
        assert!(
            found.is_empty(),
            "expected no answer but the 201 and no log line to hold {what}, but:\n{}",
            found.join("\n")
        );
        let tables = platform_tables_holding(vm.platform(), secret);
        assert!(
            tables.is_empty(),
            "expected the platform database to hold {what} in no usable form (a digest), but its text is in {tables:?}"
        );
    }
}

#[test]
fn the_tool_credentials_path_has_only_its_methods() {
    let vm = vm();
    assert_api_error(
        &vm.api_as(&alice()).get("/v1/tool-credentials"),
        405,
        "method-not-allowed",
    );
    assert_api_error(
        &vm.api_as(&alice())
            .request("/v1/me")
            .method("POST")
            .json(&json!({}))
            .send(),
        405,
        "method-not-allowed",
    );
}

#[test]
fn a_user_holds_at_most_sixteen_live_credentials() {
    let vm = vm();
    let as_bob = vm.api_as(&bob());
    let bobs: Vec<String> = (0..16).map(|_| as_bob.tool_credential().0).collect();
    let as_alice = vm.api_as(&alice());
    let mut alices: Vec<String> = (0..16).map(|_| as_alice.tool_credential().0).collect();
    for credential in alices.iter().chain(&bobs) {
        bearer(&vm, "/v1/me", credential).send().expect_status(200);
    }
    // The seventeenth ends the one made longest ago, and only that one.
    alices.push(as_alice.tool_credential().0);
    let reference = not_signed_in(&vm);
    assert_same_401(
        &bearer(&vm, "/v1/me", &alices[0]).send(),
        &reference,
        "alice's first credential after her seventeenth was made",
    );
    for (n, credential) in alices.iter().enumerate().skip(1) {
        let me = bearer(&vm, "/v1/me", credential).send();
        assert_eq!(
            me.status, 200,
            "expected alice's credential {n} (of 0 to 16) to sign in still, got status {}",
            me.status
        );
    }
    for (n, credential) in bobs.iter().enumerate() {
        let me = bearer(&vm, "/v1/me", credential).send();
        assert_eq!(
            me.status, 200,
            "expected bob's credential {n} untouched by alice's, got status {}",
            me.status
        );
        assert_eq!(me.json()["user"], "bob");
    }
    // An eighteenth ends the next oldest.
    alices.push(as_alice.tool_credential().0);
    assert_same_401(
        &bearer(&vm, "/v1/me", &alices[1]).send(),
        &reference,
        "alice's second credential after her eighteenth was made",
    );
    bearer(&vm, "/v1/me", &alices[2]).send().expect_status(200);
}

/// The headers `fixtures/honest.js` says it was handed, by lower-case name.
fn headers_handed(answer: &Response) -> serde_json::Map<String, Value> {
    answer.expect_status(200);
    let body = answer.json();
    body["headers"]
        .as_object()
        .unwrap_or_else(|| {
            panic!("expected the fixture to answer the headers it was handed, got {body}")
        })
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
        .collect()
}

#[test]
fn an_authorization_header_holding_a_tool_credential_is_never_handed_to_an_application() {
    let vm = Vm::start(
        &VmOptions::new()
            .stand_in_sign_in()
            .application("board", "honest.js"),
    );
    let (credential, _) = vm.api_as(&alice()).tool_credential();
    for scheme in ["Bearer", "bearer", "BEARER", "bEaReR"] {
        let answer = vm
            .request("board", "/")
            .as_user(&bob())
            .header("authorization", format!("{scheme} {credential}"))
            .send();
        let headers = headers_handed(&answer);
        assert!(
            !headers.contains_key("authorization"),
            "expected `Authorization: {scheme} <a tool credential>` not handed to the Application; it was handed {:?}",
            headers.keys().collect::<Vec<_>>()
        );
        let text = answer.text();
        assert!(
            windows_in(
                &[("the Application's answer".to_owned(), text)],
                &credential
            )
            .is_empty(),
            "expected no part of the credential to reach the Application"
        );
        assert_eq!(answer.json()["user"]["id"], "bob");
    }
    // Any other `Authorization` header is handed on as before.
    for other in [
        "Basic YWxpY2U6c2VjcmV0".to_owned(),
        "Bearer something-else-entirely".to_owned(),
        format!("Token {}", token()),
    ] {
        let headers = headers_handed(
            &vm.request("board", "/")
                .as_user(&bob())
                .header("authorization", &other)
                .send(),
        );
        assert_eq!(
            headers.get("authorization").and_then(Value::as_str),
            Some(other.as_str()),
            "expected `Authorization: {other}` handed on to the Application, got {headers:?}"
        );
    }
}

#[test]
fn a_credential_while_the_platform_database_cannot_be_reached_is_503_not_401() {
    let mut postgres = PrivatePostgres::start().unwrap();
    let platform = Platform::on(&postgres, "vm_platform_tool_unavailable");
    let vm = Vm::start(&VmOptions::new().stand_in_sign_in().platform(&platform));
    let (credential, _) = vm.api_as(&alice()).tool_credential();
    bearer(&vm, "/v1/me", &credential).send().expect_status(200);
    postgres.stop();
    for target in ["/v1/me", "/v1/resources", "/v1/applications"] {
        let answer = bearer(&vm, target, &credential)
            .timeout(Duration::from_secs(40))
            .send();
        assert_api_error(&answer, 503, "unavailable");
    }
}
