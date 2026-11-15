//! `vm logs` (CONTRACT.md, "Versions, going back, and logs: The commands";
//! ticket "Versions: list them, go back to the previous one, read logs":
//! logs show, per request, the User, the Version, what was printed and any
//! uncaught error with its stack; they can be followed live and filtered by
//! request; each command has a JSON form).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::logs::{
    ENTRY_KEYS, Print, encode, page, post_to, printing, prints_bundle, wait_for_entries,
    wait_for_entry,
};
use blackbox::own_database::Own;
use blackbox::tool::{Broken, Machine, Running};
use blackbox::{INSTALLER, Response, User, Vm, VmOptions};
use serde_json::{Value, json};

/// How long a test waits for `vm logs --follow` to print what it expects.
const FOLLOW_WAIT: Duration = Duration::from_secs(15);

/// The Contributor of every Application these tests make.
fn alice() -> User {
    User::new(INSTALLER)
}

/// A User who is not a Contributor.
fn bob() -> User {
    User::new("bob").in_groups(&["staff"])
}

/// `board`, `fixtures/prints.js` marked `v1` printing `at_load` lines
/// while it loads, on a program of its own.
fn board(at_load: usize) -> Own {
    let own = Own::start();
    own.api()
        .create_with_bundle("board", &prints_bundle("v1", at_load));
    own
}

/// A machine signed in as alice.
fn signed_in(vm: &Vm) -> Machine {
    let machine = Machine::new();
    machine.sign_in(vm, &alice());
    machine
}

/// A `POST <path>` of `board` as bob printing one line `message`, whose
/// entry is then readable.
fn one_line(vm: &Vm, path: &str, message: &str) -> Response {
    let response = post_to(
        vm,
        &bob(),
        "board",
        path,
        &printing(&[Print::new("log", message)], None),
    );
    response.expect_status(200);
    wait_for_entry(&vm.api_as(&alice()), "board", &response.request_id());
    response
}

fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

#[test]
fn vm_logs_json_is_the_application_and_its_entries_as_the_api_gives_them() {
    let own = board(0);
    for i in 0..3 {
        one_line(&own.vm, &format!("/n{i}"), &format!("line {i}"));
    }
    let machine = signed_in(&own.vm);
    let printed = machine.run(&["logs", "board", "--json"]).ok_json();
    assert_eq!(
        keys(&printed),
        ["application", "entries", "next"],
        "{printed}"
    );
    assert_eq!(printed["application"], "board");
    let api = page(&own.vm.api_as(&alice()), "board", "");
    assert_eq!(
        printed["entries"],
        json!(api.entries),
        "expected the entries as the API gives them"
    );
    assert_eq!(printed["next"], json!(api.next));
}

#[test]
fn vm_logs_request_after_and_limit_are_the_apis() {
    let own = board(0);
    let responses: Vec<Response> = (0..4)
        .map(|i| one_line(&own.vm, &format!("/n{i}"), &format!("line {i}")))
        .collect();
    let api = own.vm.api_as(&alice());
    let machine = signed_in(&own.vm);
    let all = page(&api, "board", "");
    let first = all.ids()[0].clone();
    let id = responses[2].request_id();
    for (args, query) in [
        (vec!["--limit", "2"], "?limit=2".to_owned()),
        (
            vec!["--after", first.as_str()],
            format!("?after={}", encode(&first)),
        ),
        (
            vec!["--after", first.as_str(), "--limit", "1"],
            format!("?after={}&limit=1", encode(&first)),
        ),
        (
            vec!["--request", id.as_str()],
            format!("?request={}", encode(&id)),
        ),
    ] {
        let mut command = vec!["logs", "board", "--json"];
        command.extend(args.iter().copied());
        let printed = machine.run(&command).ok_json();
        let want = page(&api, "board", &query);
        assert_eq!(
            (&printed["entries"], &printed["next"]),
            (&json!(want.entries), &json!(want.next)),
            "expected `vm {}` to print what the API answers {query}",
            command.join(" ")
        );
    }
    // `--after <next>` of an earlier run prints what came since.
    let earlier = machine.run(&["logs", "board", "--json"]).ok_json();
    let next = earlier["next"].as_str().unwrap().to_owned();
    let later = one_line(&own.vm, "/later", "a later line");
    let since = machine
        .run(&["logs", "board", "--after", &next, "--json"])
        .ok_json();
    let requests: Vec<&str> = since["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["request"].as_str().unwrap())
        .collect();
    assert_eq!(
        requests,
        [later.request_id()],
        "expected only what came since"
    );
}

#[test]
fn vm_logs_refusals_and_wrong_command_lines() {
    let own = board(0);
    one_line(&own.vm, "/", "a line");
    let machine = signed_in(&own.vm);
    for limit in ["0", "1001", "-1", "ten"] {
        machine
            .run(&["logs", "board", "--limit", limit, "--json"])
            .expect_code(2);
    }
    machine
        .run(&["logs", "board", "--after", "not-a-cursor", "--json"])
        .json_error(3, "malformed");
    machine
        .run(&["logs", "nothing-here", "--json"])
        .json_error(3, "not-found");
    machine.run(&["logs", "--json"]).expect_code(2);
    let other = Machine::new();
    other.sign_in(&own.vm, &bob());
    other
        .run(&["logs", "board", "--json"])
        .json_error(3, "not-a-contributor");
    other.run(&["logs", "board"]).text_error(3);
    Machine::new()
        .run(&["logs", "board", "--json"])
        .json_error(4, "not-signed-in");
    Machine::new().run(&["logs", "board"]).text_error(4);
}

#[test]
fn vm_logs_in_words_gives_each_requests_facts_its_printed_lines_and_its_error_with_its_stack() {
    let own = board(0);
    let message = "the notes could not be counted ERR-77a";
    let response = post_to(
        &own.vm,
        &bob(),
        "board",
        "/count?x=1",
        &printing(
            &[Print::new("warn", "counting the notes now")],
            Some(message),
        ),
    );
    response.expect_status(500);
    let (entry, _) = wait_for_entry(&own.vm.api_as(&alice()), "board", &response.request_id());
    let text = signed_in(&own.vm).run(&["logs", "board"]).ok_text();
    let id = response.request_id();
    let first: Vec<&str> = text.lines().filter(|l| l.contains(&id)).collect();
    assert_eq!(
        first.len(),
        1,
        "expected one line with the request's id, got\n{text}"
    );
    let first = first[0];
    let date: String = entry["time"].as_str().unwrap().chars().take(10).collect();
    let date = date.as_str();
    for (what, wanted) in [
        ("the date of its time", date),
        ("the User", "bob"),
        ("the method", "POST"),
        ("the path", "/count"),
        ("the status", "500"),
    ] {
        assert!(
            first.contains(wanted),
            "expected the entry's first line to hold {what} ({wanted:?}), got {first:?}"
        );
    }
    assert!(
        first
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|w| w == "1"),
        "expected the first line to hold the Version, 1, got {first:?}"
    );
    let lines: Vec<&str> = text.lines().collect();
    let at = lines.iter().position(|l| l.contains(&id)).unwrap();
    let rest = &lines[at + 1..];
    let printed_line = rest
        .iter()
        .find(|l| l.contains("counting the notes now"))
        .unwrap_or_else(|| panic!("expected the printed line after the first, got\n{text}"));
    assert!(
        printed_line.contains("warn"),
        "expected the printed line with its level, got {printed_line:?}"
    );
    let error_line = rest
        .iter()
        .find(|l| l.contains(message))
        .unwrap_or_else(|| panic!("expected the uncaught error's message, got\n{text}"));
    let stack_line = rest
        .iter()
        .find(|l| l.contains("throwFromTheNamedFunction"))
        .unwrap_or_else(|| panic!("expected the error's stack, got\n{text}"));
    for line in [error_line, stack_line] {
        assert!(
            line.starts_with(char::is_whitespace),
            "expected the uncaught error indented, got {line:?}"
        );
    }
}

#[test]
fn vm_logs_in_words_says_when_an_entry_was_printed_outside_a_request_or_cut_and_when_there_is_none()
{
    let own = board(0);
    let machine = signed_in(&own.vm);
    let none = machine.run(&["logs", "board"]).ok_text().to_lowercase();
    assert!(
        none.contains("none") || none.contains("no entr") || none.contains("no log"),
        "expected the words to say there is no entry yet, got\n{none}"
    );
    let api = own.vm.api_as(&alice());
    api.send_bundle("board", &prints_bundle("v2", 1))
        .expect_status(201);
    let long = post_to(
        &own.vm,
        &bob(),
        "board",
        "/long",
        &printing(&[Print::new("log", "y").repeated(9000)], None),
    );
    let (entry, _) = wait_for_entry(&api, "board", &long.request_id());
    assert_eq!(entry["truncated"], true, "the harness's own check");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !page(&api, "board", "?limit=1000")
        .entries
        .iter()
        .any(|e| e["request"].is_null())
    {
        assert!(
            Instant::now() < deadline,
            "expected an entry printed while loading within 10 s"
        );
        thread::sleep(Duration::from_millis(100));
    }
    let text = machine.run(&["logs", "board"]).ok_text();
    let lower = text.to_lowercase();
    assert!(
        text.contains("v2 loading 0"),
        "expected the line printed while loading, got\n{text}"
    );
    assert!(
        lower.contains("outside")
            || lower.contains("no request")
            || lower.contains("while loading"),
        "expected the words to say an entry was printed outside a request, got\n{text}"
    );
    assert!(
        lower.contains("cut") || lower.contains("truncated"),
        "expected the words to say an entry was cut, got\n{text}"
    );
}

/// The bidirectional marks and overrides the contract names.
fn is_bidi(c: char) -> bool {
    matches!(u32::from(c), 0x061C | 0x200E | 0x200F | 0x202A..=0x202E | 0x2066..=0x2069)
}

#[test]
fn vm_logs_in_words_lets_no_control_character_reach_the_terminal_and_indents_a_message_of_several_lines()
 {
    let own = board(0);
    let coloured = "red \u{1b}[31malert\u{7} \u{202e}reversed";
    let lines = "first line of the note\nsecond line of the note";
    let response = post_to(
        &own.vm,
        &bob(),
        "board",
        "/",
        &printing(
            &[Print::new("log", coloured), Print::new("log", lines)],
            None,
        ),
    );
    response.expect_status(200);
    wait_for_entry(&own.vm.api_as(&alice()), "board", &response.request_id());
    let machine = signed_in(&own.vm);
    let text = machine.run(&["logs", "board"]).ok_text();
    let raw: Vec<String> = text
        .chars()
        .filter(|c| (c.is_control() && *c != '\n') || is_bidi(*c))
        .map(|c| format!("U+{:04X}", u32::from(c)))
        .collect();
    assert!(
        raw.is_empty(),
        "expected no raw control character but line ends, found {raw:?} in\n{}",
        text.escape_debug()
    );
    assert!(
        text.contains("\\u{1b}"),
        "expected the escape character printed as \\u{{1b}}, got\n{}",
        text.escape_debug()
    );
    let second = text
        .lines()
        .find(|l| l.contains("second line of the note"))
        .unwrap_or_else(|| panic!("expected the second line of the message, got\n{text}"));
    assert!(
        second.starts_with(char::is_whitespace) && !second.contains("first line of the note"),
        "expected the line end inside the message to begin a new, indented line, got {second:?}"
    );
    let run = machine.run(&["logs", "board", "--json"]);
    let printed = run.ok_json();
    assert!(
        !run.stdout
            .chars()
            .any(|c| (c.is_control() && c != '\n') || is_bidi(c)),
        "expected --json to hold no raw control character, got\n{}",
        run.stdout.escape_debug()
    );
    assert_eq!(
        printed["entries"][0]["printed"],
        json!([{ "level": "log", "message": coloured }, { "level": "log", "message": lines }]),
        "expected --json to parse to the original text"
    );
}

/// Waits until what `running` printed satisfies `done`, at most
/// [`FOLLOW_WAIT`]; fails saying what it waited for.
fn wait_for_output(running: &mut Running, what: &str, done: impl Fn(&str) -> bool) -> String {
    let deadline = Instant::now() + FOLLOW_WAIT;
    loop {
        let out = running.stdout_so_far();
        if done(&out) {
            return out;
        }
        assert!(
            !running.has_ended(),
            "expected `vm logs --follow` to go on running while waiting for {what}; it ended, having printed\n{out}"
        );
        assert!(
            Instant::now() < deadline,
            "expected `vm logs --follow` to print {what} within {FOLLOW_WAIT:?}, it printed\n{out}"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

fn complete_lines(out: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = out.split('\n').collect();
    lines.pop();
    lines
}

#[test]
fn vm_logs_follow_json_prints_one_line_for_each_entry_as_it_is_made_until_interrupted() {
    let own = board(0);
    let old: Vec<Response> = (0..2)
        .map(|i| one_line(&own.vm, &format!("/old{i}"), "old"))
        .collect();
    let machine = signed_in(&own.vm);
    let mut running = machine.spawn_into_files(
        &machine.cwd(),
        &["logs", "board", "--follow", "--json"],
        &[],
    );
    wait_for_output(&mut running, "the two entries there already", |o| {
        complete_lines(o).len() >= 2
    });
    let new: Vec<Response> = (0..3)
        .map(|i| one_line(&own.vm, &format!("/new{i}"), "new"))
        .collect();
    wait_for_output(&mut running, "the three new entries", |o| {
        complete_lines(o).len() >= 5
    });
    running.signal("INT");
    let run = running.wait(Duration::from_secs(10));
    run.expect_code(0);
    let lines = complete_lines(&run.stdout);
    let objects: Vec<Value> = lines
        .iter()
        .map(|l| {
            serde_json::from_str(l)
                .unwrap_or_else(|e| panic!("expected each line one JSON object ({e}), got {l:?}"))
        })
        .collect();
    let mut want_keys: Vec<String> = ENTRY_KEYS.iter().map(|k| (*k).to_owned()).collect();
    want_keys.push("application".to_owned());
    want_keys.sort();
    for object in &objects {
        assert_eq!(
            keys(object),
            want_keys,
            "expected the entry with `application` beside its fields, got {object}"
        );
        assert_eq!(object["application"], "board");
    }
    let requests: Vec<String> = objects
        .iter()
        .map(|o| o["request"].as_str().unwrap().to_owned())
        .collect();
    let want: Vec<String> = old
        .iter()
        .chain(new.iter())
        .map(Response::request_id)
        .collect();
    assert_eq!(requests, want, "expected each entry once, oldest first");
    let api = own.vm.api_as(&alice());
    let entries = wait_for_entries(&api, "board", &new.iter().collect::<Vec<_>>());
    for (object, entry) in objects[2..].iter().zip(&entries) {
        let mut without = object.clone();
        without.as_object_mut().unwrap().remove("application");
        assert_eq!(&without, entry, "expected the entry as the API gives it");
    }
}

#[test]
fn vm_logs_follow_ends_with_0_on_sigterm_and_sighup_too() {
    let own = board(0);
    one_line(&own.vm, "/", "a line to print first");
    let machine = signed_in(&own.vm);
    for signal in ["TERM", "HUP", "INT"] {
        let mut running =
            machine.spawn_into_files(&machine.cwd(), &["logs", "board", "--follow"], &[]);
        wait_for_output(&mut running, "the entry there already", |o| {
            o.contains("a line to print first")
        });
        running.signal(signal);
        let run = running.wait(Duration::from_secs(10));
        assert_eq!(
            run.code,
            Some(0),
            "expected `vm logs --follow` to end with 0 on SIG{signal}\n{}",
            run.describe()
        );
    }
}

#[test]
fn vm_logs_follow_ends_with_unreachable_when_the_installation_stops_answering() {
    let own = board(0);
    one_line(&own.vm, "/", "printed before the stop");
    let machine = signed_in(&own.vm);
    let mut running = machine.spawn_into_files(
        &machine.cwd(),
        &["logs", "board", "--follow", "--json"],
        &[],
    );
    wait_for_output(&mut running, "the entry there already", |o| {
        !complete_lines(o).is_empty()
    });
    drop(own);
    let run = running.wait(Duration::from_secs(60));
    run.expect_code(1);
    let lines = complete_lines(&run.stdout);
    assert!(
        lines
            .first()
            .is_some_and(|l| l.contains("printed before the stop")),
        "expected what it printed before kept, got\n{}",
        run.describe()
    );
    let last: Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    assert_eq!(
        last["error"]["reason"],
        "unreachable",
        "expected the run to end with the error `unreachable`\n{}",
        run.describe()
    );
}

#[test]
fn vm_logs_follow_ends_with_4_when_the_sign_in_ends() {
    let own = Own::start_with(&VmOptions::new().tool_credential_lifetime(6));
    own.api()
        .create_with_bundle("board", &prints_bundle("v1", 0));
    one_line(&own.vm, "/", "a line");
    let machine = signed_in(&own.vm);
    let mut running = machine.spawn_into_files(&machine.cwd(), &["logs", "board", "--follow"], &[]);
    wait_for_output(&mut running, "the entry there already", |o| {
        o.contains("a line")
    });
    let run = running.wait(Duration::from_secs(40));
    assert_eq!(
        run.code,
        Some(4),
        "expected `vm logs --follow` to end with 4 once its credential expired (6 s)\n{}",
        run.describe()
    );
}

#[test]
fn a_closed_standard_output_ends_vm_logs_without_a_crash() {
    let own = board(0);
    for i in 0..3 {
        one_line(&own.vm, &format!("/n{i}"), "a line");
    }
    let machine = signed_in(&own.vm);
    for broken in [Broken::StdoutClosed, Broken::StdoutReaderGone] {
        for args in [
            vec!["logs", "board", "--follow"],
            vec!["logs", "board", "--follow", "--json"],
            vec!["logs", "board"],
        ] {
            let run = machine.run_broken(&args, broken);
            assert!(
                matches!(run.code, Some(0 | 1)),
                "expected `vm {}` with standard output {broken:?} to end by itself with 0 or 1\n{}",
                args.join(" "),
                run.describe()
            );
            let lower = run.stderr.to_lowercase();
            assert!(
                !lower.contains("panic") && !lower.contains("backtrace"),
                "expected nothing but an ordinary message on standard error\n{}",
                run.describe()
            );
        }
    }
}
