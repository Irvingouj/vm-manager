//! Logs, on the platform's API (CONTRACT.md, "Versions, going back, and
//! logs: Logs, on the platform's API"; ticket "Versions: list them, go back
//! to the previous one, read logs": logs show, per request, the User, the
//! Version, what was printed and any uncaught error with its stack; they
//! can be followed and filtered by request; Contributors and Platform
//! administrators can read an Application's logs, nobody else).
//!
//! Each test makes an Application of `fixtures/prints.js`, which prints what
//! the request asks, and reads its entries as they are made.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant};

use blackbox::api::assert_recent_rfc3339;
use blackbox::logs::{
    ENTRY_PROMISED, Page, Print, encode, from_to, go_back, logs, page, post_to, printing,
    prints_bundle, read_after, wait_for_entries, wait_for_entry,
};
use blackbox::own_database::Own;
use blackbox::postgres::{Platform, PrivatePostgres};
use blackbox::records::{event, request_of};
use blackbox::scenario::{ADMINISTRATORS, administrator};
use blackbox::{INSTALLER, Response, User, Vm, VmOptions, assert_api_error};
use serde_json::{Value, json};

/// The Contributor of every Application these tests make.
fn alice() -> User {
    User::new(INSTALLER)
}

/// A User who is not a Contributor, and whose requests the Application
/// serves.
fn bob() -> User {
    User::new("bob").in_groups(&["staff"])
}

/// A program with the stand-in sign-in and a Platform administrators'
/// Group, and `board`: `fixtures/prints.js` marked `v1`, printing
/// `at_load` lines while it loads.
fn board(at_load: usize) -> Own {
    let own = Own::start();
    own.api()
        .create_with_bundle("board", &prints_bundle("v1", at_load));
    own
}

/// A `GET <path>` of `board` as [`bob`], which prints nothing.
fn quiet(vm: &Vm, path: &str) -> Response {
    let response = vm.get_as(&bob(), "board", path);
    response.expect_status(200);
    response
}

/// A `POST <path>` of `board` as [`bob`] that prints these.
fn printed(vm: &Vm, path: &str, prints: &[Print]) -> Response {
    post_to(vm, &bob(), "board", path, &printing(prints, None))
}

/// Every entry of `application` as alice reads it, oldest first (at most
/// 1000).
fn all_entries(vm: &Vm, application: &str) -> Page {
    page(&vm.api_as(&alice()), application, "?limit=1000")
}

/// The entries whose `request` is this request's id.
fn entries_of(page: &Page, response: &Response) -> Vec<Value> {
    let id = response.request_id();
    page.entries
        .iter()
        .filter(|e| e["request"].as_str() == Some(id.as_str()))
        .cloned()
        .collect()
}

#[test]
fn a_contributor_and_a_platform_administrator_read_the_logs_and_nobody_else() {
    let own = board(0);
    let response = quiet(&own.vm, "/");
    let (entry, _) = wait_for_entry(&own.vm.api_as(&alice()), "board", &response.request_id());
    for reader in [alice(), administrator()] {
        let read = page(&own.vm.api_as(&reader), "board", "");
        assert!(
            read.entries.contains(&entry),
            "expected {:?} to read the entry of the request, got {:?}",
            reader.id,
            read.entries
        );
    }
    // Another User, also one the Application served, and also one in a
    // Group named like the administrators' but not it.
    for other in [
        bob(),
        User::new("carol").in_groups(&["platform-admin", "staff"]),
    ] {
        let api = own.vm.api_as(&other);
        assert_api_error(&logs(&api, "board", ""), 403, "not-a-contributor");
        // Before their parameters are looked at.
        assert_api_error(&logs(&api, "board", "?limit=0"), 403, "not-a-contributor");
        assert_api_error(
            &logs(&api, "board", "?after=nonsense"),
            403,
            "not-a-contributor",
        );
    }
    let anonymous = own.vm.api_request("/v1/applications/board/logs").send();
    anonymous.expect_status(401);
    for user in [alice(), bob(), administrator()] {
        assert_api_error(
            &logs(&own.vm.api_as(&user), "nothing-here", ""),
            404,
            "not-found",
        );
    }
}

#[test]
fn each_request_has_one_entry_with_its_user_version_method_path_status_and_what_it_printed() {
    let own = board(0);
    let response = post_to(
        &own.vm,
        &bob(),
        "board",
        "/notes?page=2&sort=new",
        &json!({ "print": [ { "level": "log", "message": "loading notes" },
                            { "level": "warn", "message": "two notes have no title" } ],
                 "status": 201 }),
    );
    response.expect_status(201);
    let api = own.vm.api_as(&alice());
    let (entry, _) = wait_for_entry(&api, "board", &response.request_id());
    assert_eq!(entry["request"], json!(response.request_id()), "{entry}");
    assert_eq!(entry["version"], 1, "{entry}");
    assert_eq!(entry["user"], "bob", "{entry}");
    assert_eq!(entry["method"], "POST", "{entry}");
    assert_eq!(
        entry["path"], "/notes",
        "expected the path without the query: {entry}"
    );
    assert_eq!(entry["status"], 201, "{entry}");
    assert_eq!(
        entry["printed"],
        json!([ { "level": "log", "message": "loading notes" },
                { "level": "warn", "message": "two notes have no title" } ]),
        "{entry}"
    );
    assert_eq!(entry["uncaught"], json!([]), "{entry}");
    assert_eq!(entry["truncated"], false, "{entry}");
    assert_eq!(entry["dropped_before"], 0, "{entry}");
    assert_recent_rfc3339(&entry["time"], 60, "the entry's `time`");
    assert_eq!(
        entries_of(&all_entries(&own.vm, "board"), &response).len(),
        1,
        "expected exactly one entry for the request"
    );
}

#[test]
fn printed_records_are_kept_in_order_with_the_name_of_their_console_method() {
    let own = board(0);
    let prints = [
        Print::new("debug", "first, a debug line"),
        Print::new("error", "then an error line"),
        Print::new("log", "then a log line"),
        Print::new("info", "then an info line"),
        Print::new("warn", "then a warning"),
        Print::new("log", "and a last log line"),
    ];
    let response = printed(&own.vm, "/", &prints);
    response.expect_status(200);
    let (entry, _) = wait_for_entry(&own.vm.api_as(&alice()), "board", &response.request_id());
    let want: Vec<Value> = prints
        .iter()
        .map(|p| json!({ "level": p.level, "message": p.text() }))
        .collect();
    assert_eq!(
        entry["printed"],
        json!(want),
        "expected every line, in order, with its level"
    );
}

#[test]
fn an_uncaught_error_is_in_its_entry_with_its_message_and_stack_as_the_programs_log_has_it() {
    let own = board(0);
    let message = "the notes could not be counted ERR-5d1";
    let response = post_to(
        &own.vm,
        &bob(),
        "board",
        "/count",
        &printing(&[Print::new("log", "counting notes")], Some(message)),
    );
    response.expect_status(500);
    let (entry, _) = wait_for_entry(&own.vm.api_as(&alice()), "board", &response.request_id());
    assert_eq!(entry["status"], 500, "{entry}");
    assert_eq!(
        entry["printed"],
        json!([{ "level": "log", "message": "counting notes" }]),
        "expected what was printed before the error, {entry}"
    );
    let uncaught = entry["uncaught"].as_array().unwrap();
    assert_eq!(uncaught.len(), 1, "expected one uncaught error, {entry}");
    let text = uncaught[0].as_str().unwrap();
    assert!(
        text.contains(message) && text.contains("throwFromTheNamedFunction"),
        "expected the error's message and its stack (naming the function that threw), got {text:?}"
    );
    let records = own.vm.records_of(&response);
    let logged = records.uncaught();
    assert_eq!(logged.len(), 1);
    assert_eq!(
        Some(text),
        logged[0]["error"].as_str(),
        "expected the error as the program's log has it"
    );
}

#[test]
fn a_request_that_prints_nothing_has_an_entry_with_nothing_printed() {
    let own = board(0);
    let response = quiet(&own.vm, "/quiet");
    let (entry, _) = wait_for_entry(&own.vm.api_as(&alice()), "board", &response.request_id());
    assert_eq!(entry["printed"], json!([]), "{entry}");
    assert_eq!(entry["uncaught"], json!([]), "{entry}");
    assert_eq!(entry["method"], "GET", "{entry}");
    assert_eq!(entry["status"], 200, "{entry}");
}

#[test]
fn a_request_no_application_code_was_given_has_no_entry() {
    let own = board(0);
    // Not signed in, and an Application with no Version: no code ran.
    let anonymous = own.vm.request("board", "/").send();
    anonymous.expect_status(401);
    let api = own.vm.api_as(&alice());
    api.create_application("empty").expect_status(201);
    let none = own.vm.get_as(&bob(), "empty", "/");
    none.expect_status(404);
    let served = quiet(&own.vm, "/after");
    wait_for_entry(&api, "board", &served.request_id());
    let all = all_entries(&own.vm, "board");
    assert!(entries_of(&all, &anonymous).is_empty(), "{:?}", all.entries);
    assert_eq!(page(&api, "empty", "").entries, Vec::<Value>::new());
}

#[test]
fn what_is_printed_while_loading_has_an_entry_of_its_own_with_no_request() {
    let own = board(2);
    let api = own.vm.api_as(&alice());
    let first = quiet(&own.vm, "/");
    wait_for_entry(&api, "board", &first.request_id());
    api.send_bundle("board", &prints_bundle("v2", 1))
        .expect_status(201);
    let second = quiet(&own.vm, "/");
    wait_for_entry(&api, "board", &second.request_id());
    // The loading lines are printed with no request in flight.
    let loading = own
        .vm
        .wait_for_record("a `printed` record of v2 while loading", |r| {
            event(r) == "printed" && r["message"] == "v2 loading 0"
        });
    assert!(request_of(&loading).is_none(), "{loading}");
    let deadline = Instant::now() + Duration::from_secs(10);
    let (v1, v2) = loop {
        let all = all_entries(&own.vm, "board");
        let outside = |marker: &str| -> Vec<Value> {
            all.entries
                .iter()
                .filter(|e| {
                    e["printed"][0]["message"]
                        .as_str()
                        .is_some_and(|m| m.starts_with(&format!("{marker} loading ")))
                })
                .cloned()
                .collect()
        };
        let (v1, v2) = (outside("v1"), outside("v2"));
        if (v1.len() >= 2 && !v2.is_empty()) || Instant::now() > deadline {
            break (v1, v2);
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        v1.len() >= 2 && !v2.is_empty(),
        "expected an entry for each line printed while loading (two of v1, one of v2) within 10 s, got v1 {v1:?} and v2 {v2:?}"
    );
    for (entries, version) in [(&v1, 1), (&v2, 2)] {
        for entry in entries {
            for field in ["request", "user", "method", "path", "status"] {
                assert!(entry[field].is_null(), "expected `{field}` null in {entry}");
            }
            assert_eq!(
                entry["version"], version,
                "expected the Version whose code printed, {entry}"
            );
            assert_eq!(
                entry["printed"].as_array().map(Vec::len),
                Some(1),
                "expected the one record, {entry}"
            );
            assert_eq!(entry["printed"][0]["level"], "log", "{entry}");
        }
    }
    assert!(
        v1.iter()
            .any(|e| e["printed"][0]["message"] == "v1 loading 0")
            && v1
                .iter()
                .any(|e| e["printed"][0]["message"] == "v1 loading 1"),
        "expected both lines of v1 each in an entry, got {v1:?}"
    );
    // The request's own entry holds none of them.
    let (entry, _) = wait_for_entry(&api, "board", &first.request_id());
    assert_eq!(entry["printed"], json!([]), "{entry}");
}

#[test]
fn entries_are_oldest_first_in_the_order_the_requests_ended() {
    let own = board(0);
    let responses: Vec<Response> = (0..5).map(|i| quiet(&own.vm, &format!("/n{i}"))).collect();
    let api = own.vm.api_as(&alice());
    wait_for_entries(&api, "board", &responses.iter().collect::<Vec<_>>());
    let all = all_entries(&own.vm, "board");
    let want: Vec<Option<String>> = responses.iter().map(|r| Some(r.request_id())).collect();
    assert_eq!(all.requests(), want, "expected the entries oldest first");
    let paths: Vec<&str> = all
        .entries
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["/n0", "/n1", "/n2", "/n3", "/n4"]);
}

#[test]
fn without_after_the_newest_limit_entries_are_answered_oldest_first() {
    let own = board(0);
    let api = own.vm.api_as(&alice());
    let responses: Vec<Response> = (0..105)
        .map(|i| quiet(&own.vm, &format!("/n{i}")))
        .collect();
    wait_for_entry(&api, "board", &responses[104].request_id());
    let ids: Vec<Option<String>> = responses.iter().map(|r| Some(r.request_id())).collect();
    let two = page(&api, "board", "?limit=2");
    assert_eq!(
        two.requests(),
        ids[103..].to_vec(),
        "?limit=2: the newest two, oldest first"
    );
    assert_eq!(
        two.next,
        two.ids()[1],
        "expected `next` the cursor of the last entry answered"
    );
    let default = page(&api, "board", "");
    assert_eq!(
        default.requests(),
        ids[5..].to_vec(),
        "without `limit`: the newest 100"
    );
    let one = page(&api, "board", "?limit=1");
    assert_eq!(one.requests(), ids[104..].to_vec());
    let all = page(&api, "board", "?limit=1000");
    assert_eq!(all.requests(), ids, "?limit=1000: all 105");
}

#[test]
fn after_answers_the_oldest_limit_entries_after_that_one() {
    let own = board(0);
    let api = own.vm.api_as(&alice());
    let responses: Vec<Response> = (0..5).map(|i| quiet(&own.vm, &format!("/n{i}"))).collect();
    wait_for_entries(&api, "board", &responses.iter().collect::<Vec<_>>());
    let all = page(&api, "board", "");
    let ids = all.ids();
    assert_eq!(ids.len(), 5);
    let after = |query: &str| page(&api, "board", query);
    let rest = after(&format!("?after={}", encode(&ids[1])));
    assert_eq!(
        rest.ids(),
        ids[2..].to_vec(),
        "after the second: the third to the fifth"
    );
    assert_eq!(rest.next, ids[4]);
    let two = after(&format!("?after={}&limit=2", encode(&ids[1])));
    assert_eq!(
        two.ids(),
        ids[2..4].to_vec(),
        "the oldest two after the second"
    );
    assert_eq!(
        two.next, ids[3],
        "expected `next` the cursor of the last entry answered"
    );
    let none = after(&format!("?after={}", encode(&ids[4])));
    assert_eq!(none.entries, Vec::<Value>::new());
    assert_eq!(
        none.next, ids[4],
        "with no entry answered, `next` is `after` as given"
    );
}

#[test]
fn reading_with_after_next_again_and_again_gives_every_entry_once_from_an_empty_log() {
    let own = board(0);
    let api = own.vm.api_as(&alice());
    let empty = page(&api, "board", "");
    assert_eq!(empty.entries, Vec::<Value>::new(), "no request yet");
    // From an empty log, `next` is a cursor after which only later entries
    // come; reading after it now answers none, and the same cursor.
    let (none, cursor) = read_after(&api, "board", &empty.next, 2);
    assert!(none.is_empty());
    let mut cursor = cursor;
    let mut made: Vec<Option<String>> = Vec::new();
    let mut read: Vec<Option<String>> = Vec::new();
    for round in 1..=4 {
        let responses: Vec<Response> = (0..round)
            .map(|i| quiet(&own.vm, &format!("/r{round}-{i}")))
            .collect();
        wait_for_entries(&api, "board", &responses.iter().collect::<Vec<_>>());
        made.extend(responses.iter().map(|r| Some(r.request_id())));
        let (got, next) = read_after(&api, "board", &cursor, 2);
        read.extend(got.iter().map(|e| e["request"].as_str().map(str::to_owned)));
        cursor = next;
    }
    assert_eq!(
        read, made,
        "expected every entry exactly once, in order, following with after=<next>"
    );
}

#[test]
fn request_answers_only_that_requests_entry() {
    let own = board(0);
    let api = own.vm.api_as(&alice());
    api.create_with_bundle("other", &prints_bundle("other", 0));
    let mine = quiet(&own.vm, "/mine");
    let also = quiet(&own.vm, "/also");
    let theirs = own.vm.get_as(&bob(), "other", "/theirs");
    theirs.expect_status(200);
    let (entry, _) = wait_for_entry(&api, "board", &mine.request_id());
    wait_for_entry(&api, "board", &also.request_id());
    wait_for_entry(&api, "other", &theirs.request_id());
    let only = page(
        &api,
        "board",
        &format!("?request={}", encode(&mine.request_id())),
    );
    assert_eq!(only.entries, vec![entry.clone()]);
    let with_limit = page(
        &api,
        "board",
        &format!("?request={}&limit=1", encode(&mine.request_id())),
    );
    assert_eq!(with_limit.entries, vec![entry.clone()]);
    let after_itself = page(
        &api,
        "board",
        &format!(
            "?request={}&after={}",
            encode(&mine.request_id()),
            encode(entry["id"].as_str().unwrap())
        ),
    );
    assert_eq!(
        after_itself.entries,
        Vec::<Value>::new(),
        "with `after` its own cursor"
    );
    let other_application = page(
        &api,
        "board",
        &format!("?request={}", encode(&theirs.request_id())),
    );
    assert_eq!(
        other_application.entries,
        Vec::<Value>::new(),
        "expected nothing for another Application's request"
    );
    let unknown = page(&api, "board", "?request=no-such-request");
    assert_eq!(unknown.entries, Vec::<Value>::new());
}

#[test]
fn malformed_parameters_are_400_and_unknown_ones_are_ignored() {
    let own = board(0);
    let api = own.vm.api_as(&alice());
    let response = quiet(&own.vm, "/");
    let (entry, _) = wait_for_entry(&api, "board", &response.request_id());
    let cursor = encode(entry["id"].as_str().unwrap());
    let request = encode(&response.request_id());
    let malformed = [
        "?limit=0".to_owned(),
        "?limit=1001".to_owned(),
        "?limit=-1".to_owned(),
        "?limit=1.5".to_owned(),
        "?limit=ten".to_owned(),
        "?limit=".to_owned(),
        "?after=not-a-cursor".to_owned(),
        format!("?after={cursor}x"),
        "?limit=5&limit=5".to_owned(),
        format!("?after={cursor}&after={cursor}"),
        format!("?request={request}&request={request}"),
    ];
    for query in &malformed {
        assert_api_error(&logs(&api, "board", query), 400, "malformed");
    }
    for query in ["?limit=1", "?limit=1000", "?colour=blue"] {
        page(&api, "board", query);
    }
    let with_unknown = page(&api, "board", "?since=yesterday&limit=1");
    assert_eq!(with_unknown.entries, vec![entry]);
}

#[test]
fn an_entry_can_be_read_within_2_seconds_of_its_response() {
    let own = board(0);
    let api = own.vm.api_as(&alice());
    for i in 0..5 {
        let response = printed(&own.vm, &format!("/n{i}"), &[Print::new("log", "a line")]);
        response.expect_status(200);
        let (_, waited) = wait_for_entry(&api, "board", &response.request_id());
        // The poll asks every 50 ms; a quarter of a second covers it and
        // the reading itself.
        assert!(
            waited <= ENTRY_PROMISED + Duration::from_millis(250),
            "expected the entry of request {i} readable within {ENTRY_PROMISED:?} of its response, it took {waited:?}"
        );
    }
}

#[test]
fn a_message_longer_than_8192_bytes_is_cut_at_a_character_boundary() {
    let own = board(0);
    // 1 + 2 * 5000 bytes: a cut at exactly 8,192 bytes would split an `é`.
    let long = format!("a{}", "é".repeat(5000));
    let exact = "b".repeat(8192);
    let response = printed(
        &own.vm,
        "/",
        &[Print::new("log", &long), Print::new("info", &exact)],
    );
    response.expect_status(200);
    let (entry, _) = wait_for_entry(&own.vm.api_as(&alice()), "board", &response.request_id());
    let kept = entry["printed"][0]["message"].as_str().unwrap();
    assert!(
        kept.len() <= 8192 && kept.len() >= 8191 && long.starts_with(kept),
        "expected the message cut to at most 8,192 bytes, at a character boundary, the beginning of what was printed; got {} bytes",
        kept.len()
    );
    assert_eq!(
        entry["printed"][1]["message"].as_str(),
        Some(exact.as_str()),
        "expected a message of exactly 8,192 bytes kept whole"
    );
    assert_eq!(entry["truncated"], true, "{}", entry["truncated"]);
    let records = own.vm.records_of(&response);
    assert_eq!(
        records.printed()[0]["message"].as_str(),
        Some(long.as_str()),
        "expected the program's log to keep the whole message"
    );
}

#[test]
fn an_entry_within_every_bound_is_not_truncated() {
    let own = board(0);
    let exact = Print::new("log", "b").repeated(8192);
    let prints: Vec<Print> = (0..200)
        .map(|i| Print::new("log", &format!("line {i}")))
        .collect();
    let response = printed(&own.vm, "/", &[exact]);
    let (entry, _) = wait_for_entry(&own.vm.api_as(&alice()), "board", &response.request_id());
    assert_eq!(
        entry["truncated"], false,
        "a message of exactly 8,192 bytes"
    );
    let response = printed(&own.vm, "/", &prints);
    let (entry, _) = wait_for_entry(&own.vm.api_as(&alice()), "board", &response.request_id());
    assert_eq!(entry["printed"].as_array().map(Vec::len), Some(200));
    assert_eq!(entry["truncated"], false, "exactly 200 printed records");
}

#[test]
fn an_uncaught_error_longer_than_16384_bytes_is_cut_at_a_character_boundary() {
    let own = board(0);
    let long = format!("a{}", "é".repeat(10_000));
    let response = post_to(&own.vm, &bob(), "board", "/", &printing(&[], Some(&long)));
    response.expect_status(500);
    let (entry, _) = wait_for_entry(&own.vm.api_as(&alice()), "board", &response.request_id());
    let kept = entry["uncaught"][0].as_str().unwrap();
    let records = own.vm.records_of(&response);
    let logged = records.uncaught()[0]["error"].as_str().unwrap().to_owned();
    assert!(
        logged.len() > 16_384,
        "the harness's own check: expected the program's log to keep the whole error, got {} bytes",
        logged.len()
    );
    assert!(
        kept.len() <= 16_384 && kept.len() >= 16_383 && logged.starts_with(kept),
        "expected the error cut to at most 16,384 bytes at a character boundary, the beginning of the program's log's; got {} bytes",
        kept.len()
    );
    assert_eq!(entry["truncated"], true);
}

#[test]
fn more_than_200_printed_records_keep_the_first_200() {
    let own = board(0);
    let prints: Vec<Print> = (0..250)
        .map(|i| Print::new("log", &format!("line {i}")))
        .collect();
    let response = printed(&own.vm, "/", &prints);
    response.expect_status(200);
    let (entry, _) = wait_for_entry(&own.vm.api_as(&alice()), "board", &response.request_id());
    let messages: Vec<&str> = entry["printed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["message"].as_str().unwrap())
        .collect();
    let want: Vec<String> = (0..200).map(|i| format!("line {i}")).collect();
    assert_eq!(messages, want, "expected the first 200, in order");
    assert_eq!(entry["truncated"], true);
    assert_eq!(
        own.vm.records_of(&response).printed().len(),
        250,
        "expected the program's log to keep every record"
    );
}

#[test]
fn more_than_64_kib_of_messages_keeps_what_came_first() {
    let own = board(0);
    let prints: Vec<Print> = (0..20)
        .map(|i| Print::new("log", &format!("{i:02}{}", "x".repeat(4998))))
        .collect();
    let response = printed(&own.vm, "/", &prints);
    response.expect_status(200);
    let (entry, _) = wait_for_entry(&own.vm.api_as(&alice()), "board", &response.request_id());
    let kept: Vec<&str> = entry["printed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["message"].as_str().unwrap())
        .collect();
    let total: usize = kept.iter().map(|m| m.len()).sum();
    assert!(
        total <= 64 * 1024,
        "expected at most 64 KiB of messages kept, got {total} bytes"
    );
    assert!(
        kept.len() >= 13,
        "expected the 13 whole messages that fit in 64 KiB kept, got {}",
        kept.len()
    );
    for (i, message) in kept.iter().enumerate() {
        let sent = prints[i].text();
        assert!(
            sent.starts_with(message),
            "expected the kept records to be the first ones, in order: record {i} is not the beginning of what was printed {i}th"
        );
        if i < 13 {
            assert_eq!(message.len(), sent.len(), "expected record {i} whole");
        }
    }
    assert_eq!(entry["truncated"], true);
    let logged = own.vm.records_of(&response);
    assert_eq!(
        logged.printed().len(),
        20,
        "expected the program's log to keep every record"
    );
    assert!(
        logged
            .printed()
            .iter()
            .all(|r| r["message"].as_str().is_some_and(|m| m.len() == 5000))
    );
}

#[test]
fn entries_stay_when_a_version_is_gone_back_from() {
    let own = board(0);
    let api = own.vm.api_as(&alice());
    api.send_bundle("board", &prints_bundle("v2", 0))
        .expect_status(201);
    let on_two = printed(&own.vm, "/", &[Print::new("log", "served by two")]);
    let (entry, _) = wait_for_entry(&api, "board", &on_two.request_id());
    assert_eq!(entry["version"], 2);
    go_back(&api, "board", &from_to(2, None)).expect_status(200);
    let on_one = printed(&own.vm, "/", &[Print::new("log", "served by one")]);
    let (later, _) = wait_for_entry(&api, "board", &on_one.request_id());
    assert_eq!(later["version"], 1, "{later}");
    let all = all_entries(&own.vm, "board");
    assert_eq!(
        entries_of(&all, &on_two),
        vec![entry],
        "expected the entry of Version 2 still there, as it was"
    );
}

#[test]
fn a_request_is_answered_the_same_when_its_entry_cannot_be_written() {
    // The platform database on a Postgres of this test's own: stopping it
    // leaves the program serving from what it holds in memory.
    let mut postgres = PrivatePostgres::start().unwrap();
    let platform = Platform::on(&postgres, "vm_platform_logs_unwritable");
    let vm = Vm::start(
        &VmOptions::new()
            .stand_in_sign_in()
            .administrators_group(ADMINISTRATORS)
            .platform(&platform),
    );
    vm.api_as(&alice())
        .create_with_bundle("board", &prints_bundle("v1", 0));
    let body = printing(&[Print::new("log", "a line")], None);
    let before = post_to(&vm, &bob(), "board", "/", &body);
    before.expect_status(200);
    wait_for_entry(&vm.api_as(&alice()), "board", &before.request_id());

    postgres.stop();
    for _ in 0..3 {
        let began = Instant::now();
        let after = vm
            .request("board", "/")
            .as_user(&bob())
            .method("POST")
            .json(&body)
            .timeout(Duration::from_secs(30))
            .send();
        let took = began.elapsed();
        assert_eq!(
            (after.status, after.body()),
            (before.status, before.body()),
            "expected the request answered as before, though its entry cannot be written"
        );
        assert!(
            took < Duration::from_secs(2),
            "expected no request to wait for its entry, this one took {took:?}"
        );
    }
}

#[test]
fn any_other_method_on_the_path_is_405() {
    let own = board(0);
    let api = own.vm.api_as(&alice());
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        let answer = api
            .request("/v1/applications/board/logs")
            .method(method)
            .send();
        assert_api_error(&answer, 405, "method-not-allowed");
    }
}
