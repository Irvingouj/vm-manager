//! Creating and reading Applications through the platform's API
//! (CONTRACT.md, "The platform's API: Applications"): any signed-in User
//! creates one and is its first Contributor; its name is a lower-case DNS
//! label, not one of the platform's own, and unique in the installation; a
//! caller who is not signed in is refused before anything else.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::sync::Barrier;
use std::thread;

use blackbox::api::{assert_json_content_type, assert_recent_rfc3339};
use blackbox::{DOMAIN, Response, User, Vm, VmOptions, assert_api_error};
use serde_json::{Value, json};

const RESERVED: [&str; 6] = ["api", "auth", "dashboard", "git", "admin", "www"];

fn vm() -> Vm {
    Vm::start(&VmOptions::new().stand_in_sign_in())
}

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn bob() -> User {
    User::new("bob")
}

/// Fails unless `application` is the record of a fresh Application `name`
/// created by `creator`.
fn assert_new_application(application: &Value, name: &str, creator: &str) {
    assert_eq!(application["name"], name, "name in {application}");
    assert_eq!(
        application["address"],
        format!("{name}.{DOMAIN}"),
        "expected the address <name>.<domain> in {application}"
    );
    assert_eq!(
        application["contributors"],
        json!([{ "user": creator }]),
        "expected the creator to be the only Contributor, in {application}"
    );
    assert!(
        application
            .get("serving_version")
            .is_some_and(Value::is_null),
        "expected `serving_version: null` before any Version, in {application}"
    );
    assert_eq!(
        application["created_by"], creator,
        "created_by in {application}"
    );
    assert_recent_rfc3339(&application["created_at"], 300, "created_at");
}

#[test]
fn creating_an_application_answers_201_with_the_application() {
    let vm = vm();
    let response = vm.api_as(&alice()).create_application_with(&json!({
        "name": "incident-board",
        "description": "Where incidents are tracked",
    }));
    response.expect_status(201);
    assert_json_content_type(&response);
    let application = response.json();
    assert_new_application(&application, "incident-board", "alice");
    assert_eq!(
        application["description"], "Where incidents are tracked",
        "description in {application}"
    );
}

#[test]
fn the_description_is_optional_and_kept_exactly_as_given() {
    let vm = vm();
    let api = vm.api_as(&alice());
    let without = api.create_application("no-description");
    without.expect_status(201);
    let description = &without.json()["description"];
    assert!(
        description.is_null() || description == "",
        "expected no description to read as null or \"\", got {description}"
    );
    let text = "Zoë's board: 数据, \"quotes\", back\\slash, emoji 🚀, a\nnewline";
    let with =
        api.create_application_with(&json!({ "name": "with-description", "description": text }));
    with.expect_status(201);
    assert_eq!(with.json()["description"], text);
    assert_eq!(api.application("with-description")["description"], text);
}

#[test]
fn every_signed_in_user_reads_the_same_application_as_was_created() {
    let vm = vm();
    let created = vm.api_as(&alice()).create_application("board");
    created.expect_status(201);
    for reader in [alice(), bob(), User::new("carol").in_groups(&["finance"])] {
        let read = vm.api_as(&reader).get_application("board");
        read.expect_status(200);
        assert_json_content_type(&read);
        assert_eq!(
            read.json(),
            created.json(),
            "expected {} to read the same Application as was created",
            reader.id
        );
    }
}

#[test]
fn any_signed_in_user_may_create_an_application_and_is_its_first_contributor() {
    let vm = vm();
    for (n, user) in [
        User::new("alice"),
        User::new("bob").in_groups(&["finance", "staff"]),
        User::new("zoë-数据"),
        User::new(&"u".repeat(256)),
    ]
    .iter()
    .enumerate()
    {
        let name = format!("made-by-{n}");
        let response = vm.api_as(user).create_application(&name);
        response.expect_status(201);
        assert_new_application(&response.json(), &name, &user.id);
    }
}

#[test]
fn names_that_are_lower_case_dns_labels_of_up_to_63_characters_are_accepted() {
    let vm = vm();
    let longest = format!("a{}9", "-b".repeat(30) + "c");
    assert_eq!(longest.len(), 63);
    for name in [
        "a",
        "z9",
        "my-app",
        "a-b-c-d",
        "x--y",
        "apis",
        "my-api",
        "www1",
        "admin-tools",
        longest.as_str(),
    ] {
        let response = vm.api_as(&alice()).create_application(name);
        assert_eq!(
            response.status,
            201,
            "expected the name {name:?} to be accepted, got\n{}",
            response.describe()
        );
        assert_new_application(&response.json(), name, "alice");
    }
}

/// Names that are not a lower-case DNS label of 1 to 63 characters.
fn invalid_names() -> Vec<String> {
    let mut names: Vec<String> = [
        "",
        "Board",
        "UPPER",
        "has_underscore",
        "-leading-hyphen",
        "trailing-hyphen-",
        "-",
        "has.dot",
        "trailing-dot.",
        ".leading-dot",
        "has space",
        " leading-space",
        "trailing-space ",
        "tab\there",
        "new\nline",
        "nul\u{0}byte",
        "café",
        "zoë",
        "数据",
        "a\u{200b}b",
        "emoji-🚀",
        "a/b",
        "a?b",
        "a#b",
        "a%2Fb",
        "..",
        "*",
        "a:b",
        "a@b",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    names.push("a".repeat(64));
    names.push(format!("{}-{}", "a".repeat(40), "b".repeat(30)));
    names.push("x".repeat(10_000));
    names
}

#[test]
fn names_that_are_not_lower_case_dns_labels_are_400_invalid_name() {
    let vm = vm();
    let api = vm.api_as(&alice());
    for name in invalid_names() {
        let response = api.create_application(&name);
        let shown: String = name.chars().take(80).collect();
        assert!(
            response.status == 400,
            "expected `400 invalid-name` for the name {shown:?}, got\n{}",
            response.describe()
        );
        assert_api_error(&response, 400, "invalid-name");
    }
    // Nothing was made of them under a cleaned-up name either.
    for name in [
        "board",
        "upper",
        "has-underscore",
        "leading-hyphen",
        "trailing-hyphen",
        "cafe",
        "has",
        "a",
    ] {
        assert_api_error(&api.get_application(name), 404, "not-found");
    }
}

#[test]
fn the_platforms_own_names_are_400_reserved_name() {
    let vm = vm();
    for name in RESERVED {
        assert_api_error(
            &vm.api_as(&alice()).create_application(name),
            400,
            "reserved-name",
        );
        assert_api_error(&vm.api_as(&alice()).get_application(name), 404, "not-found");
    }
    // `api.<domain>` is still the platform's API.
    let still_api = vm.api_as(&bob()).get_application("nothing-here");
    assert_api_error(&still_api, 404, "not-found");
}

#[test]
fn look_alikes_of_the_platforms_own_names_are_refused_and_never_take_their_place() {
    let vm = vm();
    let api = vm.api_as(&alice());
    for name in [
        "API",
        "Api",
        "WWW",
        "Admin",
        "api.",
        "www.",
        " api",
        "api ",
        "аpi",
        "ｗｗｗ",
        "api.vm.test",
        "admin\u{0}",
        "git\n",
    ] {
        let response = api.create_application(name);
        let reason = serde_json::from_slice::<Value>(response.body())
            .ok()
            .and_then(|b| b["error"]["reason"].as_str().map(str::to_owned));
        assert!(
            response.status == 400
                && matches!(reason.as_deref(), Some("invalid-name" | "reserved-name")),
            "expected the look-alike {name:?} to be refused `400` (invalid-name or reserved-name), got\n{}",
            response.describe()
        );
    }
    for name in RESERVED {
        assert_api_error(&api.get_application(name), 404, "not-found");
    }
    let still_api = vm
        .api_request("/v1/applications/nothing-here")
        .as_user(&bob())
        .send();
    assert_api_error(&still_api, 404, "not-found");
}

#[test]
fn a_taken_name_is_409_name_taken_and_the_application_is_unchanged() {
    let vm = vm();
    let created = vm.api_as(&alice()).create_application("board");
    created.expect_status(201);
    for user in [bob(), alice(), User::new("Alice")] {
        assert_api_error(
            &vm.api_as(&user).create_application("board"),
            409,
            "name-taken",
        );
        let again = vm.api_as(&user).create_application_with(&json!({
            "name": "board",
            "description": "taken over",
        }));
        assert_api_error(&again, 409, "name-taken");
    }
    assert_eq!(
        vm.api_as(&bob()).application("board"),
        created.json(),
        "expected the Application to be unchanged by the refused attempts"
    );
}

#[test]
fn when_users_race_for_a_name_exactly_one_wins() {
    let vm = vm();
    const RACERS: usize = 16;
    for round in 0..4 {
        let name = format!("contested-{round}");
        let start = Barrier::new(RACERS);
        let answers: Vec<(String, Response)> = thread::scope(|scope| {
            let handles: Vec<_> = (0..RACERS)
                .map(|i| {
                    let (vm, start, name) = (&vm, &start, &name);
                    scope.spawn(move || {
                        let user = User::new(&format!("racer-{i}"));
                        let request = vm
                            .api_as(&user)
                            .request("/v1/applications")
                            .method("POST")
                            .json(&json!({ "name": name }));
                        start.wait();
                        (user.id, request.send())
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let winners: Vec<&String> = answers
            .iter()
            .filter(|(_, r)| r.status == 201)
            .map(|(user, _)| user)
            .collect();
        assert_eq!(
            winners.len(),
            1,
            "expected exactly one of {RACERS} Users racing for {name:?} to win, got winners {winners:?}"
        );
        for (_, response) in answers.iter().filter(|(_, r)| r.status != 201) {
            assert_api_error(response, 409, "name-taken");
        }
        let application = vm.api_as(&bob()).application(&name);
        assert_new_application(&application, &name, winners[0]);
    }
}

#[test]
fn not_signed_in_is_401_on_the_api_before_anything_else() {
    let vm = vm();
    let unsigned_401 = vm.request("anything", "/").send();
    unsigned_401.expect_status(401);
    let requests = [
        vm.api_request("/v1/applications")
            .method("POST")
            .json(&json!({ "name": "board" })),
        vm.api_request("/v1/applications")
            .method("POST")
            .body("{ not json"),
        vm.api_request("/v1/applications")
            .method("POST")
            .json(&json!({ "name": "api" })),
        vm.api_request("/v1/applications")
            .method("POST")
            .json(&json!({ "name": "Not A Name" })),
        vm.api_request("/v1/applications/board"),
        vm.api_request("/v1/applications/board/versions")
            .method("POST")
            .json(&blackbox::api::version_body("x")),
        vm.api_request("/v1/no-such-path"),
        vm.api_request("/v1/applications")
            .method("POST")
            .body(vec![b' '; 16 * 1024 * 1024 + 1])
            .timeout(std::time::Duration::from_secs(60)),
    ];
    for request in &requests {
        let response = request.send();
        response.expect_status(401);
        assert_eq!(
            response.text(),
            unsigned_401.text(),
            "expected the API's 401 to be the program's own 401, the same whatever was asked for"
        );
        let record = vm.records_of(&response).request;
        assert!(
            record["user"].is_null() && record["application"].is_null(),
            "expected a 401's `request` record to name no User and no Application, got {record}"
        );
    }
    assert_api_error(
        &vm.api_as(&alice()).get_application("board"),
        404,
        "not-found",
    );
}

#[test]
fn a_malformed_sign_in_or_no_host_on_the_api_is_the_programs_own_400() {
    let vm = vm();
    let twice = vm
        .api_request("/v1/applications")
        .method("POST")
        .header("x-vm-stand-in-user", "alice")
        .header("x-vm-stand-in-user", "mallory")
        .json(&json!({ "name": "board" }));
    let response = twice.send();
    response.expect_status(400);
    assert!(
        response
            .header("content-type")
            .is_some_and(|t| t.starts_with("text/plain")),
        "expected the program's own text/plain 400 for a malformed sign-in, got\n{}",
        response.describe()
    );
    let no_host = vm
        .api_request("/v1/applications")
        .method("POST")
        .without_host()
        .as_user(&alice())
        .json(&json!({ "name": "board" }));
    no_host.send().expect_status(400);
    assert_api_error(
        &vm.api_as(&alice()).get_application("board"),
        404,
        "not-found",
    );
}

#[test]
fn with_the_stand_in_off_the_api_is_401_and_creates_nothing() {
    let off = Vm::start(&VmOptions::new());
    let response = off
        .api_request("/v1/applications")
        .method("POST")
        .as_user(&alice())
        .json(&json!({ "name": "board" }))
        .send();
    response.expect_status(401);
    let platform = off.platform().clone();
    drop(off);
    let on = Vm::start_on(&platform);
    assert_api_error(
        &on.api_as(&alice()).get_application("board"),
        404,
        "not-found",
    );
}
