//! The Operation log (CONTRACT.md, "The Operation log"; ticket "A User's
//! read of a shared database is authorized and logged": every operation,
//! allowed or refused, has an entry with the User, the Application and its
//! Version, the Resource, the statement without its values, rows,
//! duration, outcome and, when refused, the condition; no value from a
//! statement appears in the log; when the entry cannot be written the
//! operation is refused; a Platform administrator can read it).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::postgres::PrivatePostgres;
use blackbox::scenario::{
    Expected, READERS, RESOURCE, Scenario, administrator, assert_entry, assert_refusal,
    assert_rows, lines,
};
use blackbox::shared_database::{find_standalone, token};
use blackbox::{Platform, User, Vm, VmOptions, assert_api_error};
use serde_json::{Value, json};

fn rita() -> User {
    User::new("rita").in_groups(&[READERS])
}

fn ask(sql: &str) -> Value {
    json!({ "resource": RESOURCE, "sql": sql })
}

#[test]
fn no_value_from_a_statement_a_parameter_or_a_row_reaches_the_log() {
    let s = Scenario::ready();
    let t = token();
    let literal = format!("literal-{t}");
    let in_list = format!("in-list-{t}");
    let dollar = format!("dollar-{t}");
    let escaped = format!("escaped-{t}");
    let array = format!("array-{t}");
    let param = format!("param-{t}");
    let refused = format!("refused-{t}");
    let failing = format!("failing-{t}");
    let unparsed = format!("unparsed-{t}");
    let number = "7319004417";
    let negative = "-8812770065";
    let allowed_calls = [
        format!(
            "select id, title from incidents where title <> '{literal}' and id < {number} and id > {negative} and title not in ('{in_list}', 'x') order by id"
        ),
        format!("select $${dollar}$$ as d, E'{escaped}' as e, array['{array}'] as a"),
    ];
    let mut calls: Vec<Value> = allowed_calls.iter().map(|sql| ask(sql)).collect();
    calls.push(json!({
        "resource": RESOURCE,
        "sql": "select title from incidents where title = $1 or id = $2",
        "params": [param, 3],
    }));
    calls.push(ask(&format!("select '{failing}'::int4 as n")));
    calls.push(ask(&format!("selec '{unparsed}' from incidents")));
    calls.push(json!({ "resource": "ghost", "sql": format!("select '{refused}'") }));
    let (response, answers) = s.run(&rita(), "board", &json!({ "calls": calls }));
    // The Application got what it asked for, the row's value included.
    assert!(
        answers[0]["ok"]["rows"]
            .to_string()
            .contains(&s.db.row_marker)
    );
    assert!(
        answers[2]["ok"]["rows"]
            .to_string()
            .contains(&s.db.row_marker)
    );
    assert_eq!(answers[3]["error"]["code"], "22P02", "{}", answers[3]);
    // A User with no Grant, with a value in the statement.
    let (_, denied) = s.query(
        &User::new("nobody"),
        "board",
        &ask(&format!("select '{refused}' from incidents")),
    );
    assert_eq!(denied["refused"]["refusal"]["reason"], "no-grant");

    let entries = s.entries_of(&response);
    assert_eq!(
        entries.len(),
        6,
        "expected six entries:\n{}",
        lines(&entries)
    );
    let statements: Vec<&str> = entries
        .iter()
        .map(|e| e["statement"].as_str().unwrap_or_default())
        .collect();
    assert!(
        statements[0].contains("incidents") && statements[0].contains("title"),
        "expected the statement's structure to be kept, got {:?}",
        statements[0]
    );
    assert_eq!(
        statements[2], "select title from incidents where title = $1 or id = $2",
        "expected a statement with no constants recorded as written"
    );
    assert_eq!(
        statements[4], "<unparsed>",
        "expected a statement that cannot be parsed recorded as <unparsed>"
    );

    let values = [
        literal,
        in_list,
        dollar,
        escaped,
        array,
        param,
        refused,
        failing,
        unparsed,
        s.db.row_marker.clone(),
    ];
    let log_text = serde_json::to_string(&s.log()).unwrap();
    let program_log: Vec<String> = s.vm.lines().into_iter().map(|l| l.raw).collect();
    let mut found = Vec::new();
    for value in &values {
        if log_text.contains(value.as_str()) {
            found.push(format!("{value:?} in the Operation log"));
        }
        for line in &program_log {
            if line.contains(value.as_str()) {
                found.push(format!("{value:?} in the program's log: {line}"));
            }
        }
    }
    for value in [number, negative, negative.trim_start_matches('-')] {
        if find_standalone(&log_text, value) {
            found.push(format!("{value} in the Operation log"));
        }
        for line in &program_log {
            if find_standalone(line, value) {
                found.push(format!("{value} in the program's log: {line}"));
            }
        }
    }
    assert!(
        found.is_empty(),
        "expected no value from a statement, a parameter or a row in any entry or any record, but found:\n{}\nthe entries:\n{}",
        found.join("\n"),
        lines(&s.log())
    );
}

#[test]
fn an_entry_is_unfinished_while_its_statement_runs_and_completed_when_it_ends() {
    let s = Scenario::ready();
    let request =
        s.vm.request("board", "/")
            .as_user(&rita())
            .method("POST")
            .json(&json!({ "calls": [ask("select pg_sleep(3) as slept")] }));
    let began = Instant::now();
    let response = thread::scope(|scope| {
        let running = scope.spawn(|| request.send());
        let entry = wait_for_entry(&s, |e| e["outcome"] == "unfinished", Duration::from_secs(2));
        assert!(
            began.elapsed() < Duration::from_secs(3),
            "expected the entry before the statement ended"
        );
        assert_entry(
            &entry,
            &Expected {
                request: entry["request"].as_str(),
                outcome: "unfinished",
                ran: Some(false),
                ..Expected::allowed("", "rita", "board")
            },
            "the entry while its statement runs",
        );
        running.join().unwrap()
    });
    response.expect_status(200);
    let id = response.request_id();
    let entry = s.entry_of(&response);
    assert_entry(
        &entry,
        &Expected {
            rows: Some(1),
            ..Expected::allowed(&id, "rita", "board")
        },
        "the entry once its statement ended",
    );
    let duration = entry["duration_ms"].as_u64().unwrap();
    assert!(
        (2900..10_000).contains(&duration),
        "expected duration_ms to be about the 3 seconds the statement slept, got {duration}"
    );
}

fn wait_for_entry(s: &Scenario, matches: impl Fn(&Value) -> bool, within: Duration) -> Value {
    let deadline = Instant::now() + within;
    loop {
        if let Some(entry) = s.log().into_iter().find(|e| matches(e)) {
            return entry;
        }
        if Instant::now() >= deadline {
            s.vm.fail(&format!(
                "expected an Operation log entry within {within:?}; the log:\n{}",
                lines(&s.log())
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn an_entry_stays_unfinished_when_the_program_dies_while_its_statement_runs() {
    let s = Scenario::ready();
    let request =
        s.vm.request("board", "/")
            .as_user(&rita())
            .method("POST")
            .json(&json!({ "calls": [ask("select pg_sleep(3) as slept")] }));
    let sender = thread::spawn(move || drop(request.try_send()));
    let entry = wait_for_entry(&s, |e| e["outcome"] == "unfinished", Duration::from_secs(3));
    let Scenario { vm, db } = s;
    let options = vm.options().clone().platform(vm.platform());
    drop(vm);
    sender.join().unwrap();
    let vm = Vm::start(&options);
    let s = Scenario { vm, db };
    let after: Vec<Value> = s
        .log()
        .into_iter()
        .filter(|e| e["id"] == entry["id"])
        .collect();
    assert_eq!(after.len(), 1, "expected the entry kept: {after:?}");
    assert_eq!(
        after[0]["outcome"], "unfinished",
        "expected the entry to stay unfinished after the program died, got {}",
        after[0]
    );
    assert!(
        after[0]["rows"].is_null() && after[0]["duration_ms"].is_null(),
        "{}",
        after[0]
    );
}

#[test]
fn entries_are_oldest_first_and_limit_gives_the_newest() {
    let s = Scenario::ready();
    for n in 0..5 {
        s.query(&rita(), "board", &ask(&format!("select {n} as n")));
    }
    let all = s.log();
    assert_eq!(all.len(), 5, "{}", lines(&all));
    let ids: Vec<u64> = all.iter().map(|e| e["id"].as_u64().unwrap()).collect();
    assert!(
        ids.windows(2).all(|w| w[0] < w[1]),
        "expected entries oldest first, ids rising, got {ids:?}"
    );
    for (limit, want) in [
        ("3", &all[2..]),
        ("1", &all[4..]),
        ("5", &all[..]),
        ("1000", &all[..]),
    ] {
        let answer = s.admin().operation_log(Some(limit));
        answer.expect_status(200);
        assert_eq!(
            answer.json()["entries"].as_array().unwrap().as_slice(),
            want,
            "expected ?limit={limit} to give the newest {limit}, oldest first"
        );
    }
    for limit in ["0", "1001", "-1", "abc"] {
        let answer = s.admin().operation_log(Some(limit));
        assert_eq!(
            answer.status,
            400,
            "expected `400` for ?limit={limit:?} (1 to 1000), got\n{}",
            answer.describe()
        );
    }
}

#[test]
fn without_a_limit_the_newest_100_are_given() {
    let s = Scenario::ready();
    // 105 refused queries: cheap entries.
    let calls: Vec<Value> = (0..105)
        .map(|_| json!({ "resource": "ghost", "sql": "select 1" }))
        .collect();
    s.run(&rita(), "board", &json!({ "calls": calls }));
    let all = s.log();
    assert_eq!(all.len(), 105, "expected 105 entries");
    let answer = s.admin().operation_log(None);
    answer.expect_status(200);
    assert_eq!(
        answer.json()["entries"].as_array().unwrap().as_slice(),
        &all[5..],
        "expected the newest 100, oldest first"
    );
}

#[test]
fn only_a_platform_administrator_reads_the_operation_log() {
    let s = Scenario::ready();
    s.query(&rita(), "board", &ask("select 1 as one"));
    for user in [
        rita(),
        User::new("carol").in_groups(&["Platform-Admins"]),
        User::new(&administrator().id),
    ] {
        assert_api_error(
            &s.vm.api_as(&user).operation_log(None),
            403,
            "not-an-administrator",
        );
        assert_api_error(
            &s.vm.api_as(&user).operation_log(Some("5")),
            403,
            "not-an-administrator",
        );
    }
    let anonymous = s.vm.api_request("/v1/operation-log").send();
    assert_eq!(anonymous.status, 401, "{}", anonymous.describe());
    assert_eq!(s.log().len(), 1);
}

#[test]
fn a_restart_keeps_resources_grants_and_the_operation_log() {
    let s = Scenario::ready();
    s.register("billing");
    s.grant("billing", "finance", "structure");
    s.grant(RESOURCE, "writers", "write");
    s.query(&rita(), "board", &ask("select 1 as one"));
    s.query(&User::new("nobody"), "board", &ask("select 2 as two"));
    let before = (
        s.admin().list_resources().json(),
        s.admin().list_grants(RESOURCE).json(),
        s.admin().list_grants("billing").json(),
        s.log(),
    );
    let Scenario { vm, db } = s;
    let s = Scenario {
        vm: vm.restart(),
        db,
    };
    let after = (
        s.admin().list_resources().json(),
        s.admin().list_grants(RESOURCE).json(),
        s.admin().list_grants("billing").json(),
        s.log(),
    );
    assert_eq!(
        before, after,
        "expected Resources, Grants and the Operation log as they were"
    );
    let (response, answer) = s.query(&rita(), "board", &ask("select 3 as three"));
    assert_rows(&answer, &["three"], &json!([[3]]), "after the restart");
    let newest = s.entry_of(&response);
    let last_before = before.3.last().unwrap()["id"].as_u64().unwrap();
    assert!(
        newest["id"].as_u64().unwrap() > last_before,
        "expected entry ids to go on rising after a restart, got {newest} after {last_before}"
    );
}

#[test]
fn when_the_entry_cannot_be_written_the_query_is_refused_and_nothing_is_sent() {
    // The platform database on a Postgres of this test's own, the shared
    // database on another: stopping the first leaves the second serving.
    let mut platform_postgres = PrivatePostgres::start().unwrap();
    let platform = Platform::on(&platform_postgres, "vm_platform_log_unavailable");
    let s = Scenario::start_with(&VmOptions::new().platform(&platform));
    s.register(RESOURCE);
    s.grant(RESOURCE, READERS, "read");
    s.install("board", "query.js", &[(RESOURCE, "read")]);
    let (_, answer) = s.query(&rita(), "board", &ask("select 'before-the-stop' as probe"));
    assert_rows(
        &answer,
        &["probe"],
        &json!([["before-the-stop"]]),
        "while all is well",
    );
    s.db.assert_received("before-the-stop", "the query while all is well");

    platform_postgres.stop();
    let marker = format!("after-the-stop-{}", token());
    let response =
        s.vm.request("board", "/")
            .as_user(&rita())
            .method("POST")
            .json(&json!({ "calls": [ask(&format!("select '{marker}' as probe"))] }))
            .timeout(Duration::from_secs(30))
            .send();
    assert_eq!(
        response.status,
        200,
        "expected the Application to answer, with the host's refusal, got\n{}",
        response.describe()
    );
    assert_refusal(
        &response.json()["answers"][0],
        "log-unavailable",
        &json!({ "resource": RESOURCE }),
        "a query whose Operation log entry cannot be written",
    );
    thread::sleep(Duration::from_millis(500));
    s.db.assert_not_received(&marker, "a query whose entry could not be written");
}
