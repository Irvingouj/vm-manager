//! Outbound calls in the local run (CONTRACT.md, "Outbound calls: The local
//! run"): in `vibe dev` and `vibe request` the list is the project's own,
//! `outbound.hosts` of `vibe.dev.json`, with `outbound.private` for the
//! ranges that may be called all the same; without `outbound` nothing may be
//! called, and the refusal's `fix` says where the list is locally and where
//! it is in an installation; a call is really made and shows in `vibe local
//! operation-log` with its `host`; a file whose `outbound` is not that shape
//! is `test-users-file`; `platform-itself` is the local run's own address.
//!
//! Not tested: that `vibe pull` does not write `outbound` and `vibe check`
//! and `vibe deploy` say nothing of it (they need an installation with a
//! Resource; the sentence is a negative the implementation keeps by not
//! doing anything).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::path::Path;

use blackbox::dev::Dev;
use blackbox::local::{Local, OUTSIDER, dev_json, local_project, write_dev_json};
use blackbox::outbound::{Origin, Reply, Setup};
use blackbox::sdk::TempProject;
use serde_json::{Value, json};

const NAME: &str = "outbound-local";

/// A handler that calls the address `?url=` names and says what came.
const HANDLER: &str = r#"import { defineApp, OperationFailed, Refused } from "@vm/sdk";
import manifest from "../manifest.ts";

export default defineApp(manifest, {
  async fetch(request) {
    const url = new URL(request.url);
    const target = url.searchParams.get("url") ?? "";
    try {
      const answer = await fetch(target);
      return Response.json({ status: answer.status, text: await answer.text() });
    } catch (e) {
      if (e instanceof Refused) return Response.json({ refused: { refusal: e.refusal, fix: e.fix } });
      if (e instanceof OperationFailed) return Response.json({ failed: { kind: e.kind, detail: e.detail } });
      return Response.json({ other: String((e as Error).message ?? e) });
    }
  },
});
"#;

fn project(local: &Local) -> TempProject {
    let project = local_project(&local.machine, NAME, &[]);
    project.write("src/index.ts", HANDLER);
    project
}

fn encode(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

/// `outbound` written into the project's `vibe.dev.json`.
fn set_outbound(dir: &Path, outbound: &Value) {
    let mut file = dev_json(dir);
    file["outbound"] = outbound.clone();
    write_dev_json(dir, &file);
}

/// `vibe request /call?url=<target>` as the outsider, with the test
/// authority in `VIBE_CA_FILE`; what the Application answered.
fn call(local: &Local, dir: &Path, setup: &Setup, target: &str) -> Value {
    let ca = setup.ca_file.to_string_lossy().into_owned();
    let run = local.request_with_env(
        dir,
        &format!("/call?url={}", encode(target)),
        OUTSIDER,
        &[],
        &[("VIBE_CA_FILE", ca.as_str())],
    );
    let requested = blackbox::local::Requested::of(&run);
    assert_eq!(requested.status(), 200, "{}", requested.describe());
    requested.json()
}

#[test]
fn without_outbound_in_the_file_nothing_may_be_called_and_the_fix_says_where_the_list_is_locally() {
    let local = Local::new();
    let project = project(&local);
    let setup = Setup::new(&[]);
    let origin = Origin::start(setup.cert().wire());
    let answer = call(&local, &project.dir, &setup, &origin.url("/x"));
    let refusal = &answer["refused"];
    assert_eq!(
        refusal["refusal"]["reason"], "host-not-listed",
        "expected `Refused` `host-not-listed` in the local run, got {answer}"
    );
    assert_eq!(refusal["refusal"]["host"], origin.host(), "{answer}");
    let fix = refusal["fix"].as_str().unwrap_or_default();
    assert!(
        fix.contains("outbound.hosts") && fix.contains("vibe.dev.json"),
        "expected the fix to say that locally the list is `outbound.hosts` in `vibe.dev.json`, got {fix:?}"
    );
    assert!(
        fix.contains("vibe outbound"),
        "expected the fix to say that in an installation it is the Platform administrators' (`vibe outbound`), got {fix:?}"
    );
    origin.assert_untouched("a call with no list");
    // The entry is in the local Operation log with its host.
    let entries = local.operation_log(&project.dir, &[]).ok_json()["entries"].clone();
    let entry = entries
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["operation"] == "fetch")
        .unwrap_or_else(|| {
            panic!("expected a `fetch` entry in the local Operation log, got {entries}")
        });
    assert_eq!(entry["host"], origin.host(), "{entry}");
    assert_eq!(entry["outcome"], "refused", "{entry}");
    assert_eq!(entry["reason"], "host-not-listed", "{entry}");
}

#[test]
fn with_hosts_and_private_a_call_is_made_and_shows_in_the_local_operation_log_with_its_host() {
    let local = Local::new();
    let project = project(&local);
    let setup = Setup::new(&[]);
    let origin = Origin::start(setup.cert().wire());
    origin.always(Reply::ok(b"from the outside"));
    set_outbound(
        &project.dir,
        &json!({ "hosts": [origin.host()], "private": ["127.0.0.1/32", "::1/128"] }),
    );
    let answer = call(&local, &project.dir, &setup, &origin.url("/real?a=1"));
    assert_eq!(answer["status"], 200, "{answer}");
    assert_eq!(answer["text"], "from the outside", "{answer}");
    let seen = origin.the_request();
    assert_eq!(seen.target, "/real?a=1");
    assert_eq!(seen.header("user-agent"), Some("vibe"));
    let entries = local.operation_log(&project.dir, &[]).ok_json()["entries"].clone();
    let fetches: Vec<&Value> = entries
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["operation"] == "fetch")
        .collect();
    assert_eq!(fetches.len(), 1, "{entries}");
    assert_eq!(fetches[0]["host"], origin.host(), "{}", fetches[0]);
    assert_eq!(fetches[0]["outcome"], "allowed", "{}", fetches[0]);
    // The text of `local operation-log` shows `→ <host>`.
    let words = local.run(
        &[
            "local",
            "operation-log",
            "--project",
            project.dir.to_str().unwrap(),
        ],
        &[],
    );
    assert!(
        words
            .stdout
            .contains(&format!("\u{2192} {}", origin.host())),
        "expected `→ <host>` in the words of `vibe local operation-log`, got\n{}",
        words.describe()
    );
}

#[test]
fn listed_but_not_opened_a_loopback_address_is_refused_in_the_local_run_too() {
    let local = Local::new();
    let project = project(&local);
    let setup = Setup::new(&[]);
    let origin = Origin::start(setup.cert().wire());
    set_outbound(&project.dir, &json!({ "hosts": [origin.host()] }));
    let answer = call(&local, &project.dir, &setup, &origin.url("/x"));
    assert_eq!(
        answer["refused"]["refusal"]["reason"], "address-not-allowed",
        "expected a listed name that resolves to loopback refused in the local run, got {answer}"
    );
    origin.assert_untouched("an address not opened locally");
}

#[test]
fn an_outbound_that_is_not_the_shape_is_test_users_file_and_nothing_runs() {
    let local = Local::new();
    let project = project(&local);
    let too_many_hosts: Vec<String> = (0..1001).map(|i| format!("h{i}.example.com")).collect();
    let too_many_ranges: Vec<String> = (0..17).map(|i| format!("10.0.{i}.0/24")).collect();
    let bad = [
        json!("nope"),
        json!(7),
        json!([]),
        json!({ "hosts": "api.example.com" }),
        json!({ "hosts": [7] }),
        json!({ "hosts": ["*.com"] }),
        json!({ "hosts": ["not a host"] }),
        json!({ "hosts": ["api.example.com:0"] }),
        json!({ "hosts": ["api.example.com:65536"] }),
        json!({ "private": "10.0.0.0/8" }),
        json!({ "private": ["not a range"] }),
        json!({ "private": ["10.0.0.0/33"] }),
        json!({ "hosts": too_many_hosts }),
        json!({ "private": too_many_ranges }),
    ];
    for outbound in bad {
        set_outbound(&project.dir, &outbound);
        let run = local.request(&project.dir, "/", OUTSIDER, &[]);
        run.json_error(1, "test-users-file");
        let text = run.describe();
        assert!(
            text.contains("outbound"),
            "expected the message to say which part of the file is wrong ({}), got\n{text}",
            outbound.to_string().chars().take(60).collect::<String>()
        );
    }
    // Both optional, and empty is the shape.
    set_outbound(&project.dir, &json!({}));
    let run = local.request(&project.dir, "/", OUTSIDER, &[]);
    assert_eq!(run.code, Some(0), "{}", run.describe());
    set_outbound(&project.dir, &json!({ "hosts": [], "private": [] }));
    let run = local.request(&project.dir, "/", OUTSIDER, &[]);
    assert_eq!(run.code, Some(0), "{}", run.describe());
}

#[test]
fn the_local_runs_own_address_is_refused_platform_itself_though_listed_and_opened() {
    let local = Local::new();
    let project = project(&local);
    // `vibe dev` listens on a port the harness chooses; list the
    // Application's own address and open the loopback, so that only
    // `platform-itself` stops the call.
    let port = blackbox::dev::free_port();
    let own = format!("{NAME}.localhost:{port}");
    set_outbound(
        &project.dir,
        &json!({
            "hosts": [own, format!("localhost:{port}"), format!("127.0.0.1:{port}")],
            "private": ["127.0.0.1/32", "::1/128"],
        }),
    );
    let mut dev = Dev::spawn(&local, &project.dir, NAME, port, true);
    dev.wait_until_listening();
    let own = dev.host();
    let mut browser = dev.browser();
    browser.choose(OUTSIDER);
    let response = browser.get(&format!(
        "/call?url={}",
        encode(&format!("https://{own}/x"))
    ));
    assert_eq!(response.status, 200, "{}", response.describe());
    let answer = response.json();
    assert_eq!(
        answer["refused"]["refusal"]["reason"], "platform-itself",
        "expected the local run's own address refused `platform-itself`, got {answer}"
    );
    dev.stop("INT").expect_code(0);
}
