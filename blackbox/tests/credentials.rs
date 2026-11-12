//! No answer, no log record and no error message ever holds a Resource's
//! password, host, port, database or role (CONTRACT.md, "Resources and
//! Grants"; ticket "A User's read of a shared database is authorized and
//! logged": no API returns a Resource's credentials).
//!
//! One test does everything a Resource's connection could leak through:
//! registering (also with bodies that are refused), reading Resources and
//! Grants, queries that run, are refused, fail in the database, fail to log
//! in (a wrong password, a database that does not exist, a port nothing
//! listens on), and reading the Operation log. Then it searches every
//! answer it got, and every line the program wrote, for the connection's
//! values: unmistakable markers (the shared database lives on a host and
//! port of its own; see `shared_database`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::scenario::{READERS, Scenario, resource_body};
use blackbox::shared_database::token;
use blackbox::{Response, User};
use serde_json::{Value, json};

fn rita() -> User {
    User::new("rita").in_groups(&[READERS])
}

#[test]
fn no_answer_and_no_log_record_ever_holds_a_resources_connection() {
    let s = Scenario::start();
    let db = &s.db;
    let admin = s.admin();
    let mut answers: Vec<Response> = Vec::new();
    let wrong_password = format!("pw-wrong-{}", token());
    let missing_database = format!("missing_db_{}", token());

    // Registering, as it should be and with bodies that are refused.
    let devops = resource_body("devops", db, &db.reader);
    answers.push(admin.create_resource(&devops));
    answers.push(admin.create_resource(&resource_body("loose", db, &db.careless)));
    let mut wrong = resource_body("wrongpw", db, &db.reader);
    wrong["connection"]["read"]["password"] = json!(wrong_password);
    answers.push(admin.create_resource(&wrong));
    let mut nodb = resource_body("nodb", db, &db.reader);
    nodb["connection"]["database"] = json!(missing_database);
    answers.push(admin.create_resource(&nodb));
    let mut closed = resource_body("closed", db, &db.reader);
    closed["connection"]["port"] = json!(blackbox::postgres::free_port().unwrap());
    answers.push(admin.create_resource(&closed));
    for answer in &answers {
        answer.expect_status(201);
    }
    // Taken, invalid, malformed, by someone who is not an administrator.
    answers.push(admin.create_resource(&devops));
    answers.push(admin.create_resource(&resource_body("Not Valid", db, &db.reader)));
    let mut port_as_text = resource_body("porttext", db, &db.reader);
    port_as_text["connection"]["port"] = json!(db.port().to_string());
    answers.push(admin.create_resource(&port_as_text));
    let twice = devops.to_string().replacen(
        "\"name\":\"devops\"",
        "\"name\":\"twice\",\"name\":\"twice\"",
        1,
    );
    answers.push(admin.post_raw("/v1/resources", &twice));
    let whole = devops.to_string();
    let cut = whole.strip_suffix('}').unwrap();
    answers.push(admin.post_raw("/v1/resources", cut));
    answers.push(s.vm.api_as(&rita()).create_resource(&devops));

    // Reading Resources and Grants.
    for name in ["devops", "loose", "wrongpw", "nodb", "closed", "ghost"] {
        answers.push(s.vm.api_as(&rita()).get_resource(name));
        answers.push(admin.get_resource(name));
        answers.push(admin.grant(name, READERS, "read"));
        answers.push(admin.list_grants(name));
    }
    answers.push(admin.list_resources());
    answers.push(s.vm.api_as(&rita()).list_resources());

    // Queries of every outcome, through an Application that declares all.
    let declared: Vec<(&str, &str)> = ["devops", "loose", "wrongpw", "nodb", "closed"]
        .iter()
        .map(|r| (*r, "read"))
        .collect();
    s.install("board", "query.js", &declared);
    let calls: Vec<Value> = vec![
        json!({ "resource": "devops", "sql": "select id, title from incidents order by id" }),
        json!({ "resource": "loose", "sql": "select count(*)::int4 as n from incidents" }),
        json!({ "resource": "loose", "sql": "insert into incidents values (9, 'x', true, 1, now())" }),
        json!({ "resource": "devops", "sql": "selec 1" }),
        json!({ "resource": "devops", "sql": "select * from staff_only" }),
        json!({ "resource": "wrongpw", "sql": "select 1" }),
        json!({ "resource": "nodb", "sql": "select 1" }),
        json!({ "resource": "closed", "sql": "select 1" }),
        json!({ "resource": "devops", "sql": "select 1", "params": [{ "x": 1 }] }),
        json!({ "resource": "ghost", "sql": "select 1" }),
    ];
    let (response, query_answers) = s.run(&rita(), "board", &json!({ "calls": calls }));
    assert!(
        query_answers[0].get("ok").is_some(),
        "expected the honest query to run, got {}",
        query_answers[0]
    );
    for (i, answer) in query_answers.iter().enumerate().skip(5).take(3) {
        assert!(
            answer.get("ok").is_none(),
            "expected the query {i} through a Resource that cannot log in to fail, got {answer}"
        );
    }
    answers.push(response);
    answers.push(
        s.post(
            &User::new("nobody"),
            "board",
            "/",
            &json!({ "calls": [{ "resource": "devops", "sql": "select 1" }] }),
        )
        .0,
    );

    // The Operation log.
    answers.push(admin.operation_log(None));
    answers.push(s.vm.api_as(&rita()).operation_log(None));

    // Everything the program said.
    let mut texts: Vec<(String, String)> = answers
        .iter()
        .map(|a| (format!("the answer to {}", a.request()), a.describe()))
        .collect();
    texts.extend(
        s.vm.lines()
            .into_iter()
            .map(|l| ("the program's log".to_owned(), l.raw)),
    );
    let mut markers = db.credential_markers();
    markers.push(("a wrong password", wrong_password));
    markers.push(("a database that does not exist", missing_database));
    let mut found = Vec::new();
    for (whence, text) in &texts {
        for (what, marker) in &markers {
            if text.contains(marker.as_str()) {
                found.push(format!("{what} in {whence}:\n    {text}"));
            }
        }
        if blackbox::shared_database::find_port(text, db.port()) {
            found.push(format!("the port {} in {whence}:\n    {text}", db.port()));
        }
    }
    assert!(
        found.is_empty(),
        "expected no answer and no log record to hold a Resource's password, host, port, database or role, but:\n{}",
        found.join("\n")
    );
    if !db.host_is_distinct() {
        eprintln!(
            "the shared database's host is 127.0.0.1 on this machine, which is not searched for"
        );
    }
}
