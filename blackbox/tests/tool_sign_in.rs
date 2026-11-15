//! Signing the tool in and out, and asking who it is (CONTRACT.md, "The
//! tool: Signing in", "Every command"; ticket "The tool signs in, and lists
//! Resources and Applications": after signing in "who am I" shows the User
//! and their Groups, signing out removes the credential, and an expired or
//! missing credential says to sign in, with its own exit code).
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

use blackbox::api::parse_rfc3339;
use blackbox::records::event;
use blackbox::scenario::{ADMINISTRATORS, Scenario, administrator};
use blackbox::tool::{Machine, NotAnInstallation, bearer, closed_address, sign_in_args};
use blackbox::{DOMAIN, User, Vm, VmOptions};
use serde_json::Value;

const THIRTY_DAYS: i64 = 30 * 24 * 60 * 60;

/// The longest the tool may take to find that nothing listens at a
/// loopback address: far less than its 5 s wait to connect.
const QUICK: Duration = Duration::from_secs(10);

const SIGNED_IN_COMMANDS: [&str; 3] = ["whoami", "resources", "applications"];

fn alice() -> User {
    User::new("alice").in_groups(&["readers", "staff"])
}

fn bob() -> User {
    User::new("bob").in_groups(&["bob-team"])
}

fn vm_with(options: VmOptions) -> Vm {
    Vm::start(
        &options
            .stand_in_sign_in()
            .administrators_group(ADMINISTRATORS),
    )
}

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
        .map(|g| g.as_str().unwrap_or_default().to_owned())
        .collect()
}

fn set_of(user: &User) -> BTreeSet<String> {
    user.groups.iter().cloned().collect()
}

fn now() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
}

fn assert_expires_in(value: &Value, seconds: i64, slack: i64) {
    let at = value
        .as_str()
        .and_then(parse_rfc3339)
        .unwrap_or_else(|| panic!("expected `expires_at` as an RFC 3339 time, got {value}"));
    assert!(
        (at - (now() + seconds)).abs() <= slack,
        "expected `expires_at` {seconds} s from now (within {slack} s), got {value}"
    );
}

/// The `sign-in` arguments as `&str`, with `--json` when asked.
fn args(owned: &[String], json: bool) -> Vec<&str> {
    let mut args: Vec<&str> = owned.iter().map(String::as_str).collect();
    if json {
        args.push("--json");
    }
    args
}

/// Fails unless `message` gives the whole `vm sign-in` command with this
/// installation.
fn assert_says_sign_in_again_to(message: &str, vm_address: &str) {
    for part in ["vm sign-in", "--domain", DOMAIN, "--connect", vm_address] {
        assert!(
            message.contains(part),
            "expected the message to give the whole `vm sign-in` command with the installation it was signed in to ({part:?} missing), got {message:?}"
        );
    }
}

#[test]
fn signing_in_prints_the_user_their_groups_and_the_installation() {
    let vm = vm_with(VmOptions::new());
    let machine = Machine::new();
    let printed = machine.sign_in(&vm, &alice());
    assert_eq!(
        keys(&printed),
        [
            "domain",
            "expires_at",
            "groups",
            "platform_administrator",
            "user"
        ],
        "expected {{ user, groups, platform_administrator, domain, expires_at }}, got {printed}"
    );
    assert_eq!(printed["user"], "alice");
    assert_eq!(groups_set(&printed["groups"]), set_of(&alice()));
    assert_eq!(printed["platform_administrator"], false);
    assert_eq!(printed["domain"], DOMAIN);
    assert_expires_in(&printed["expires_at"], THIRTY_DAYS, 300);

    let who = machine.run(&["whoami", "--json"]).ok_json();
    assert_eq!(
        keys(&who),
        ["domain", "groups", "platform_administrator", "user"],
        "expected {{ user, groups, platform_administrator, domain }}, got {who}"
    );
    assert_eq!(who["user"], "alice");
    assert_eq!(groups_set(&who["groups"]), set_of(&alice()));
    assert_eq!(who["platform_administrator"], false);
    assert_eq!(who["domain"], DOMAIN);

    // In words.
    let other = Machine::new();
    let owned = sign_in_args(vm.address(), &alice());
    let words = other.run(&args(&owned, false)).ok_text();
    let who = other.run(&["whoami"]).ok_text();
    for text in [&words, &who] {
        for fact in ["alice", "readers", "staff", DOMAIN] {
            assert!(
                text.contains(fact),
                "expected {fact:?} in the words, got\n{text}"
            );
        }
    }
}

#[test]
fn a_platform_administrator_is_said_to_be_one() {
    let vm = vm_with(VmOptions::new());
    let machine = Machine::new();
    let printed = machine.sign_in(&vm, &administrator());
    assert_eq!(printed["platform_administrator"], true, "{printed}");
    let who = machine.run(&["whoami", "--json"]).ok_json();
    assert_eq!(who["platform_administrator"], true, "{who}");
    assert_eq!(who["user"], administrator().id.as_str());
}

#[test]
fn a_user_with_no_groups_signs_in() {
    let vm = vm_with(VmOptions::new());
    let machine = Machine::new();
    let printed = machine.sign_in(&vm, &User::new("solo"));
    assert_eq!(printed["groups"], serde_json::json!([]), "{printed}");
    let who = machine.run(&["whoami", "--json"]).ok_json();
    assert_eq!(who["user"], "solo");
    assert_eq!(who["groups"], serde_json::json!([]));
}

#[test]
fn whoami_asks_the_installation() {
    let vm = vm_with(VmOptions::new());
    let machine = Machine::new();
    machine.sign_in(&vm, &alice());
    let before = vm
        .records()
        .iter()
        .filter(|r| event(r) == "request" && r["user"] == "alice")
        .count();
    machine.run(&["whoami", "--json"]).ok_json();
    let count = || {
        vm.records()
            .iter()
            .filter(|r| event(r) == "request" && r["user"] == "alice")
            .count()
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while count() <= before && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        count() > before,
        "expected `vm whoami` to ask the installation: no new `request` record of alice's"
    );
}

#[test]
fn signing_in_again_replaces_the_credential_and_ends_the_earlier_one() {
    let vm = vm_with(VmOptions::new());
    let machine = Machine::new();
    machine.sign_in(&vm, &alice());
    let first = machine.kept_credential(&vm);
    machine.sign_in(&vm, &bob());
    let second = machine.kept_credential(&vm);
    assert_ne!(first, second, "expected a new credential");
    let answer = bearer(&vm, "/v1/me", &first).send();
    assert_eq!(
        answer.status, 401,
        "expected the earlier credential ended at the installation, got status {}",
        answer.status
    );
    assert!(
        machine.files_holding(&first).is_empty(),
        "expected the earlier credential gone from the machine"
    );
    let who = machine.run(&["whoami", "--json"]).ok_json();
    assert_eq!(who["user"], "bob");
    assert_eq!(groups_set(&who["groups"]), set_of(&bob()));
}

#[test]
fn signing_out_ends_the_credential_and_removes_it() {
    let vm = vm_with(VmOptions::new());
    for json in [true, false] {
        let machine = Machine::new();
        machine.sign_in(&vm, &alice());
        let credential = machine.kept_credential(&vm);
        let run = machine.run(&args(&["sign-out".to_owned()], json));
        if json {
            let out = run.ok_json();
            assert_eq!(out["signed_out"], true, "{out}");
            assert_eq!(out["ended_at_installation"], true, "{out}");
        } else {
            run.ok_text();
        }
        let answer = bearer(&vm, "/v1/me", &credential).send();
        assert_eq!(
            answer.status, 401,
            "expected the credential ended at the installation"
        );
        assert!(
            machine.files_holding(&credential).is_empty(),
            "expected the credential removed from the machine"
        );
        machine
            .run(&["whoami", "--json"])
            .json_error(4, "not-signed-in");
        // And again: nothing to sign out of.
        let again = machine.run(&["sign-out", "--json"]).ok_json();
        assert_eq!(again["signed_out"], false, "{again}");
    }
}

#[test]
fn signing_out_with_nothing_to_sign_out_of() {
    let machine = Machine::new();
    let out = machine.run(&["sign-out", "--json"]).ok_json();
    assert_eq!(out["signed_out"], false, "{out}");
    machine.run(&["sign-out"]).expect_code(0);
}

#[test]
fn signing_out_when_the_installation_cannot_be_reached_removes_the_credential_all_the_same() {
    let vm = vm_with(VmOptions::new());
    let machine = Machine::new();
    machine.sign_in(&vm, &alice());
    let credential = machine.kept_credential(&vm);
    drop(vm);
    let run = machine.run(&["sign-out", "--json"]);
    let out = run.ok_json();
    assert_eq!(out["signed_out"], true, "{out}");
    assert_eq!(out["ended_at_installation"], false, "{out}");
    assert!(
        run.elapsed < QUICK,
        "expected sign-out to give up on an installation that refuses the connection at once, it took {:?}",
        run.elapsed
    );
    assert!(
        machine.files_holding(&credential).is_empty(),
        "expected the credential removed from the machine"
    );
    machine
        .run(&["whoami", "--json"])
        .json_error(4, "not-signed-in");
}

#[test]
fn an_installation_without_the_stand_in_refuses_the_sign_in() {
    let vm = Vm::start(&VmOptions::new());
    let machine = Machine::new();
    let owned = sign_in_args(vm.address(), &alice());
    machine
        .run(&args(&owned, true))
        .json_error(3, "sign-in-refused");
    machine.run(&args(&owned, false)).text_error(3);
    assert!(
        machine.all_files().is_empty(),
        "expected nothing kept, found {:?}",
        machine.all_files()
    );
    machine
        .run(&["whoami", "--json"])
        .json_error(4, "not-signed-in");
}

#[test]
fn a_missing_credential_says_to_sign_in() {
    let machine = Machine::new();
    for command in SIGNED_IN_COMMANDS {
        let message = machine
            .run(&[command, "--json"])
            .json_error(4, "not-signed-in");
        assert!(
            message.contains("vm sign-in"),
            "expected the message to say to run `vm sign-in`, got {message:?}"
        );
        let words = machine.run(&[command]).text_error(4);
        assert!(words.contains("vm sign-in"), "{words:?}");
    }
}

#[test]
fn an_expired_credential_says_to_sign_in_again_to_the_same_installation() {
    let s = Scenario::start_with(&VmOptions::new().tool_credential_lifetime(2));
    let machine = Machine::new();
    let printed = machine.sign_in(&s.vm, &alice());
    assert_expires_in(&printed["expires_at"], 2, 60);
    machine.run(&["whoami", "--json"]).ok_json();
    thread::sleep(Duration::from_millis(3500));
    let address = s.vm.address().to_string();
    for command in SIGNED_IN_COMMANDS {
        let message = machine
            .run(&[command, "--json"])
            .json_error(4, "sign-in-expired");
        assert_says_sign_in_again_to(&message, &address);
        let words = machine.run(&[command]).text_error(4);
        assert_says_sign_in_again_to(&words, &address);
    }
    // Signing in again mends it.
    machine.sign_in(&s.vm, &alice());
    machine.run(&["whoami", "--json"]).ok_json();
}

#[test]
fn a_credential_signed_out_elsewhere_is_expired_to_the_tool() {
    let vm = vm_with(VmOptions::new());
    let machine = Machine::new();
    machine.sign_in(&vm, &alice());
    let credential = machine.kept_credential(&vm);
    bearer(&vm, "/v1/tool-credentials/current", &credential)
        .method("DELETE")
        .send()
        .expect_status(204);
    let address = vm.address().to_string();
    for command in SIGNED_IN_COMMANDS {
        let message = machine
            .run(&[command, "--json"])
            .json_error(4, "sign-in-expired");
        assert_says_sign_in_again_to(&message, &address);
    }
    let out = machine.run(&["sign-out", "--json"]).ok_json();
    assert_eq!(out["signed_out"], true, "{out}");
    machine
        .run(&["whoami", "--json"])
        .json_error(4, "not-signed-in");
}

#[test]
fn an_installation_that_is_not_there_is_unreachable() {
    let machine = Machine::new();
    let nowhere = closed_address();
    let owned = sign_in_args(nowhere, &alice());
    let run = machine.run(&args(&owned, true));
    run.json_error(1, "unreachable");
    assert!(run.elapsed < QUICK, "took {:?}", run.elapsed);
    machine.run(&args(&owned, false)).text_error(1);
    assert!(
        machine.all_files().is_empty(),
        "expected nothing kept, found {:?}",
        machine.all_files()
    );

    let vm = vm_with(VmOptions::new());
    machine.sign_in(&vm, &alice());
    drop(vm);
    for command in SIGNED_IN_COMMANDS {
        let run = machine.run(&[command, "--json"]);
        run.json_error(1, "unreachable");
        assert!(run.elapsed < QUICK, "took {:?}", run.elapsed);
        machine.run(&[command]).text_error(1);
    }
}

#[test]
fn something_that_is_not_an_installation_is_an_unexpected_answer() {
    let other = NotAnInstallation::start();
    let machine = Machine::new();
    let owned = sign_in_args(other.address(), &alice());
    machine
        .run(&args(&owned, true))
        .json_error(1, "unexpected-answer");
    machine.run(&args(&owned, false)).text_error(1);
    assert!(
        machine.all_files().is_empty(),
        "expected nothing kept, found {:?}",
        machine.all_files()
    );
}
