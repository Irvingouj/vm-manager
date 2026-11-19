//! The tool reaches an installation over HTTPS (CONTRACT.md, "The tool
//! reaches an installation over HTTPS: Where the tool connects"; ticket "The
//! tool reaches an installation over HTTPS, and a company's own certificate
//! authority is trusted").
//!
//! The installation is the program behind a TLS front of the harness's
//! (src/tls.rs) whose certificate for `api.<domain>` and `*.<domain>` is
//! issued under a test authority A; the tool is run with `--resolve
//! 127.0.0.1 --port <the front's port>` and `VIBE_CA_FILE` naming A's
//! certificate. Signed in so, every later command reaches the installation
//! the same way from the credential file alone (no option), which the front
//! shows: it counts each command's connections and records the `Host` each
//! request named. Here: the ways that work; the refusals are in
//! `tool_https_refusals.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::connection::{Connection, hide, setup_credential};
use blackbox::deploy::{Deployed, commit_all, dana, deploy_with_env, init_repository};
use blackbox::oidc::Person;
use blackbox::project::new_project_in_repository;
use blackbox::scenario::ADMINISTRATORS;
use blackbox::sessions::{ADA, Installation, ada, oidc_options};
use blackbox::tls::{Behind, Handshake, api_name};
use blackbox::tool::{Machine, Run, Running, bearer, windows_in};
use blackbox::{DOMAIN, INSTALLER, User, Vm, VmOptions};
use serde_json::{Value, json};

/// How long a test waits for `vibe logs --follow` to print what it expects.
const FOLLOW_WAIT: Duration = Duration::from_secs(15);

/// The arguments of `vibe sign-in` over HTTPS through `behind` as this
/// User, with `--json`.
fn sign_in_args(behind: &Behind, user: &User) -> Vec<String> {
    let mut args = vec!["sign-in".to_owned()];
    args.extend(behind.place());
    args.push("--stand-in-user".to_owned());
    args.push(user.id.clone());
    if !user.groups.is_empty() {
        args.push("--stand-in-groups".to_owned());
        args.push(user.groups.join(","));
    }
    args.push("--json".to_owned());
    args
}

/// Runs `vibe <args>` on `machine` with `VIBE_CA_FILE` naming A and nothing
/// else of the installation.
fn run(machine: &Machine, behind: &Behind, args: &[&str]) -> Run {
    let (name, value) = behind.ca_env();
    machine.run_with_env(args, &[(name, value.as_str())])
}

/// Signs `machine` in over HTTPS through `behind` as `user`; the run must
/// end with `0`. Returns what it printed.
fn sign_in(machine: &Machine, behind: &Behind, user: &User) -> Value {
    let args = sign_in_args(behind, user);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let printed = run(machine, behind, &refs);
    hide(&printed, &[]).ok_json()
}

/// Fails unless every request the front passed on named `Host:
/// api.<domain>:<port>` and every connection asked for the server name
/// `api.<domain>`.
fn assert_all_to_the_api(behind: &Behind) {
    let want = format!("{}:{}", api_name(), behind.port());
    let hosts = behind.front.hosts();
    assert!(
        !hosts.is_empty() && hosts.iter().all(|h| *h == want),
        "expected every request through the front to name Host: {want}, got {hosts:?}\n{}",
        behind.front.describe()
    );
    let names: Vec<Option<String>> = behind
        .front
        .settled()
        .iter()
        .map(|s| s.server_name.clone())
        .collect();
    assert!(
        names
            .iter()
            .all(|n| n.as_deref() == Some(api_name().as_str())),
        "expected every connection to ask for the server name {}, got {names:?}",
        api_name()
    );
}

/// Runs `command` (with `--json`) after a sign-in over HTTPS: it must end
/// with `0` having made at least one new connection through the front, all
/// of them completed. Returns what it printed.
fn through_the_front(machine: &Machine, behind: &Behind, command: &[&str]) -> Value {
    let before = behind.front.settled().len();
    let printed = hide(&run(machine, behind, command), &[]);
    let value = printed.ok_json();
    let seen = behind.front.settled();
    let new = seen.get(before..).unwrap_or_default();
    assert!(
        !new.is_empty() && new.iter().all(|s| s.handshake == Handshake::Completed),
        "expected `vibe {}` to reach the installation through the TLS front, from the credential file alone\n{}\n{}",
        command.join(" "),
        printed.describe(),
        behind.front.describe()
    );
    value
}

#[test]
fn a_sign_in_with_resolve_and_port_signs_in_over_https_through_the_front() {
    let vm = Vm::with_stand_in(&[]);
    let behind = Behind::before(vm.address());
    let machine = Machine::new();
    let printed = sign_in(&machine, &behind, &dana());
    assert_eq!(printed["user"], "dana", "{printed}");
    assert_eq!(printed["groups"], json!(["builders"]), "{printed}");
    assert_eq!(printed["domain"], DOMAIN, "{printed}");
    assert!(
        behind.front.completed() >= 1,
        "expected the sign-in to go through the TLS front\n{}",
        behind.front.describe()
    );
    assert_all_to_the_api(&behind);
    assert!(
        behind
            .front
            .request_lines()
            .iter()
            .any(|l| l.starts_with("POST /v1/tool-credentials ")),
        "expected the credential to be asked for through the front\n{}",
        behind.front.describe()
    );
    // The credential it keeps is one the installation knows.
    let credential = machine.kept_credential(&vm);
    bearer(&vm, "/v1/me", &credential).send().expect_status(200);
}

#[test]
fn whoami_and_applications_reach_the_installation_over_https_from_the_credential_file_alone() {
    let vm = Vm::with_stand_in(&[("board", "honest.js")]);
    let behind = Behind::before(vm.address());
    let machine = Machine::new();
    sign_in(&machine, &behind, &User::new(INSTALLER));
    let me = through_the_front(&machine, &behind, &["whoami", "--json"]);
    assert_eq!(me["user"], INSTALLER, "{me}");
    assert_eq!(me["domain"], DOMAIN, "{me}");
    let listed = through_the_front(&machine, &behind, &["applications", "--json"]);
    assert_eq!(
        listed["applications"][0]["name"], "board",
        "expected the fixture Application in the list\n{listed}"
    );
    assert_all_to_the_api(&behind);
}

#[test]
fn sign_out_over_https_ends_the_credential_at_the_installation() {
    let vm = Vm::with_stand_in(&[]);
    let behind = Behind::before(vm.address());
    let machine = Machine::new();
    sign_in(&machine, &behind, &dana());
    let credential = machine.kept_credential(&vm);
    let out = through_the_front(&machine, &behind, &["sign-out", "--json"]);
    assert_eq!(
        out,
        json!({ "signed_out": true, "ended_at_installation": true }),
        "expected the sign-out to be ended at the installation over HTTPS"
    );
    bearer(&vm, "/v1/me", &credential).send().expect_status(401);
    assert!(
        behind
            .front
            .request_lines()
            .iter()
            .any(|l| l.starts_with("DELETE /v1/tool-credentials/current ")),
        "expected the credential to be ended through the front\n{}",
        behind.front.describe()
    );
    assert!(machine.credential_files().is_empty());
}

#[test]
fn a_deploy_of_the_template_project_goes_over_https_from_the_credential_file_alone() {
    let vm = Vm::start(&VmOptions::new().stand_in_sign_in());
    let behind = Behind::before(vm.address());
    let machine = Machine::new();
    sign_in(&machine, &behind, &dana());
    let name = "over-https";
    let project = new_project_in_repository(&machine, name);
    init_repository(&project.dir);
    commit_all(&project.dir, "A new project");
    let before = behind.front.settled().len();
    let (ca_name, ca_value) = behind.ca_env();
    let run = deploy_with_env(&machine, &project.dir, &[], &[(ca_name, ca_value.as_str())]);
    let deployed = Deployed::of(&run);
    assert_eq!(deployed.body["application"], name);
    assert_eq!(deployed.body["version"], 1);
    let seen = behind.front.settled();
    assert!(
        seen.len() > before
            && seen[before..]
                .iter()
                .all(|s| s.handshake == Handshake::Completed),
        "expected the deploy to go through the TLS front\n{}",
        behind.front.describe()
    );
    assert_all_to_the_api(&behind);
    assert_eq!(
        vm.api_as(&dana()).application(name)["serving_version"],
        json!(1),
        "expected the Version sent over HTTPS to serve"
    );
}

/// Waits until `running` has printed a complete line holding `needle`, at
/// most [`FOLLOW_WAIT`].
fn wait_for_line(running: &mut Running, needle: &str) -> String {
    let deadline = Instant::now() + FOLLOW_WAIT;
    loop {
        let out = running.stdout_so_far();
        if out.lines().any(|l| l.contains(needle)) && out.ends_with('\n') {
            return out;
        }
        assert!(
            !running.has_ended(),
            "expected `vibe logs --follow` to go on running while waiting for {needle:?}; it ended, having printed\n{out}\nstandard error:\n{}",
            running.stderr_so_far()
        );
        assert!(
            Instant::now() < deadline,
            "expected `vibe logs --follow` to print {needle:?} within {FOLLOW_WAIT:?}, it printed\n{out}\nstandard error:\n{}",
            running.stderr_so_far()
        );
        thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn logs_follow_over_https_receives_an_entry_made_after_it_started() {
    let vm = Vm::with_stand_in(&[("board", "honest.js")]);
    let behind = Behind::before(vm.address());
    let machine = Machine::new();
    sign_in(&machine, &behind, &User::new(INSTALLER));
    let before = behind.front.settled().len();
    let completed_before = behind.front.completed();
    let (ca_name, ca_value) = behind.ca_env();
    let mut running = machine.spawn_into_files(
        &machine.cwd(),
        &["logs", "board", "--follow", "--json"],
        &[(ca_name, ca_value.as_str())],
    );
    // Once it has asked through the front (and printed the newest, none),
    // an entry is made.
    let deadline = Instant::now() + FOLLOW_WAIT;
    while behind.front.completed() <= completed_before && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
    }
    thread::sleep(Duration::from_millis(1500));
    let response = vm.get_as(&User::new("bob"), "board", "/after-it-started");
    response.expect_status(200);
    let request = response.request_id();
    let out = wait_for_line(&mut running, &request);
    running.signal("INT");
    let ended = running.wait(Duration::from_secs(10));
    ended.expect_code(0);
    let line: Value = out
        .lines()
        .find(|l| l.contains(&request))
        .map(|l| serde_json::from_str(l).unwrap())
        .unwrap();
    assert_eq!(line["application"], "board", "{line}");
    assert_eq!(line["path"], "/after-it-started", "{line}");
    let seen = behind.front.settled();
    assert!(
        seen.len() > before
            && seen[before..]
                .iter()
                .all(|s| s.handshake == Handshake::Completed),
        "expected `vibe logs --follow` to ask through the TLS front\n{}",
        behind.front.describe()
    );
    assert_all_to_the_api(&behind);
}

#[test]
fn the_sign_in_expired_message_gives_the_sign_in_with_resolve_and_port() {
    let vm = Vm::start(
        &VmOptions::new()
            .stand_in_sign_in()
            .tool_credential_lifetime(2),
    );
    let behind = Behind::before(vm.address());
    let machine = Machine::new();
    sign_in(&machine, &behind, &dana());
    thread::sleep(Duration::from_millis(3500));
    let message = run(&machine, &behind, &["whoami", "--json"]).json_error(4, "sign-in-expired");
    for part in [
        "vibe sign-in".to_owned(),
        format!("--domain {DOMAIN}"),
        "--resolve 127.0.0.1".to_owned(),
        format!("--port {}", behind.port()),
    ] {
        assert!(
            message.contains(&part),
            "expected the message to give the sign-in with the options it was made with ({part:?}), got {message:?}"
        );
    }
    assert!(
        !message.contains("--connect"),
        "expected no --connect in the command given, got {message:?}"
    );
}

#[test]
fn the_tool_signs_in_through_the_browser_over_https() {
    let at = Installation::start(&[]);
    let behind = Behind::before(at.vm.address());
    let machine = Machine::new();
    let mut args = vec!["sign-in".to_owned()];
    args.extend(behind.place());
    args.push("--json".to_owned());
    let (ca_name, ca_value) = behind.ca_env();
    let printed = at.sign_in_tool_with(&machine, ADA, &args, &[(ca_name, ca_value.as_str())]);
    assert_eq!(printed["user"], ADA, "{printed}");
    assert!(
        behind
            .front
            .request_lines()
            .iter()
            .any(|l| l.starts_with("POST /v1/tool-sign-ins ")),
        "expected the tool to take its credential through the TLS front\n{}",
        behind.front.describe()
    );
    let me = through_the_front(&machine, &behind, &["whoami", "--json"]);
    assert_eq!(me["user"], ADA, "{me}");
    assert_all_to_the_api(&behind);
}

#[test]
fn connect_oidc_with_the_setup_credential_goes_over_https() {
    let at = Installation::unconnected(blackbox::oidc::Provider::start(&[ada()]), &oidc_options());
    let behind = Behind::before(at.vm.address());
    let connection = Connection::to(&at.provider);
    let setup = setup_credential(at.vm.platform());
    let mut args = vec![
        "admin".to_owned(),
        "connect-oidc".to_owned(),
        "--domain".to_owned(),
        DOMAIN.to_owned(),
        "--port".to_owned(),
        behind.port().to_string(),
        "--resolve".to_owned(),
        "127.0.0.1".to_owned(),
    ];
    args.extend(connection.tool_args());
    args.push("--json".to_owned());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let machine = Machine::new();
    let (ca_name, ca_value) = behind.ca_env();
    let ran = machine.run_with_env(
        &refs,
        &[
            ("VIBE_SETUP_CREDENTIAL", setup.expose()),
            ("VIBE_OIDC_CLIENT_SECRET", connection.secret().expose()),
            (ca_name, ca_value.as_str()),
        ],
    );
    let hidden = hide(&ran, &[setup.expose(), connection.secret().expose()]);
    let printed = hidden.ok_json();
    assert_eq!(printed["connected_by"], "setup-credential", "{printed}");
    assert_eq!(printed["issuer"], at.provider.issuer(), "{printed}");
    assert!(
        behind
            .front
            .request_lines()
            .iter()
            .any(|l| l.starts_with("PUT /v1/oidc ")),
        "expected the connection to be made through the TLS front\n{}",
        behind.front.describe()
    );
    assert_all_to_the_api(&behind);
    assert!(
        windows_in(&hidden.texts(), setup.expose()).is_empty(),
        "the setup credential was printed"
    );
}

#[test]
fn connect_oidc_with_the_tools_own_credential_goes_over_https_from_the_credential_file() {
    let admin = Person::new("00u-admin-1")
        .named("Ida Admin")
        .email("ida@example.test")
        .in_groups(&[ADMINISTRATORS]);
    let at = Installation::start_with(&[], &[admin], &[]);
    let behind = Behind::before(at.vm.address());
    let machine = Machine::new();
    let mut args = vec!["sign-in".to_owned()];
    args.extend(behind.place());
    args.push("--json".to_owned());
    let (ca_name, ca_value) = behind.ca_env();
    at.sign_in_tool_with(
        &machine,
        "00u-admin-1",
        &args,
        &[(ca_name, ca_value.as_str())],
    );
    let connection = Connection::to(&at.provider).administrators_group("platform-admins-2");
    let mut command = vec!["admin".to_owned(), "connect-oidc".to_owned()];
    command.extend(connection.tool_args());
    command.push("--json".to_owned());
    let refs: Vec<&str> = command.iter().map(String::as_str).collect();
    let before = behind.front.settled().len();
    let ran = machine.run_with_env(
        &refs,
        &[
            ("VIBE_OIDC_CLIENT_SECRET", connection.secret().expose()),
            (ca_name, ca_value.as_str()),
        ],
    );
    let printed = hide(&ran, &[connection.secret().expose()]).ok_json();
    assert_eq!(printed["connected_by"], "00u-admin-1", "{printed}");
    assert_eq!(printed["administrators_group"], "platform-admins-2");
    let seen = behind.front.settled();
    assert!(
        seen.len() > before
            && behind
                .front
                .request_lines()
                .iter()
                .any(|l| l.starts_with("PUT /v1/oidc ")),
        "expected the connection to be made through the TLS front\n{}",
        behind.front.describe()
    );
    assert_all_to_the_api(&behind);
}
