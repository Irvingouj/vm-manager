//! `GET /v1/resources/<name>/structure` (CONTRACT.md, "Pulling a Resource's
//! structure": "The structure login", "On the platform's API", "What is in
//! the structure"; ticket "Pull a Resource's structure as a typed Drizzle
//! definition": a Builder whose Grant is structure-only can pull, one with
//! no Grant is refused with a message that names whom to ask and says not
//! to build against the Resource; a Resource can be registered with an
//! optional structure login; a column type the puller cannot express is
//! reported, not silently dropped).
//!
//! The answer's fields, who may pull, the refusals and failures with what
//! each says, the Operation log entry of every pull, the structure login
//! kept out of every answer and log record, and the limits.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant};

use blackbox::postgres::{Platform, PrivatePostgres, free_port};
use blackbox::scenario::{CONTACT, READERS, Scenario, administrator, resource_body};
use blackbox::shared_database::token;
use blackbox::sql::Sql;
use blackbox::structure::{
    FIRST_LINE, STOCK, Stock, UNSUPPORTED, assert_fingerprint, assert_no_grant_error,
    body_with_structure, pull, second_line, structure_login,
};
use blackbox::{Response, User, VmOptions, assert_api_error};
use serde_json::{Value, json};

fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

/// A Builder in [`READERS`].
fn rita() -> User {
    User::new("rita").in_groups(&[READERS])
}

/// A Builder whose Groups hold no Grant.
fn nora() -> User {
    User::new("nora").in_groups(&["elsewhere"])
}

/// The statements the shared database received from these roles.
fn statements_from(stock: &Stock, roles: &[&str]) -> Vec<String> {
    stock
        .db()
        .statements_by_login()
        .into_iter()
        .filter(|(by, _)| roles.contains(&by.as_str()))
        .map(|(_, entry)| entry)
        .collect()
}

/// The Operation log entries of the request this answers.
fn entries_of(scenario: &Scenario, response: &Response) -> Vec<Value> {
    scenario.entries_of(response)
}

/// Fails unless `entry` is the Operation log entry of a pull CONTRACT.md
/// describes: `operation` `structure`, `application` and `version` null,
/// `request` and `user` the request's, `resource`, `statement` and `rows`
/// null, this outcome, reason and error.
fn assert_pull_entry(
    entry: &Value,
    response: &Response,
    user: &User,
    resource: &str,
    outcome: &str,
    reason: Option<&str>,
    error: Option<&str>,
) {
    let context = format!("the Operation log entry of the pull {}", response.request());
    assert!(
        entry["id"].is_u64(),
        "{context}: expected a numeric `id`, got {entry}"
    );
    blackbox::api::assert_recent_rfc3339(&entry["time"], 120, &context);
    let want = json!({
        "operation": "structure",
        "application": null,
        "version": null,
        "request": response.request_id(),
        "user": user.id,
        "resource": resource,
        "statement": null,
        "outcome": outcome,
        "reason": reason,
        "error": error,
        "rows": null,
    });
    for (key, value) in want.as_object().unwrap() {
        let got = entry.get(key).unwrap_or(&Value::Null);
        assert_eq!(
            got, value,
            "{context}: expected `{key}` {value}, got the entry {entry}"
        );
    }
}

#[test]
fn the_answer_holds_the_resource_its_fingerprint_the_users_access_the_definition_and_what_is_unsupported()
 {
    let stock = Stock::ready();
    let answer = stock.pull_ok(&rita(), STOCK);
    assert_eq!(
        keys(&answer),
        [
            "contact",
            "fingerprint",
            "kind",
            "resource",
            "schema_ts",
            "standin_sql",
            "unsupported",
            "your_access"
        ],
        "expected exactly the fields of CONTRACT.md (with `standin_sql` and `contact` since the ticket on the local loop, and `kind` since Storage), got {answer}"
    );
    assert_eq!(answer["resource"], json!(STOCK));
    assert_eq!(answer["your_access"], json!("read"));
    assert_fingerprint(&answer["fingerprint"], "the pull's fingerprint");
    let schema_ts = answer["schema_ts"].as_str().unwrap();
    let mut lines = schema_ts.lines();
    assert_eq!(lines.next(), Some(FIRST_LINE), "{schema_ts}");
    assert_eq!(
        lines.next().map(str::to_owned),
        Some(second_line(STOCK, answer["fingerprint"].as_str().unwrap())),
        "expected the second line to name the Resource and the fingerprint answered, got\n{schema_ts}"
    );

    let unsupported = answer["unsupported"].as_array().unwrap();
    for column in unsupported {
        assert_eq!(
            keys(column),
            ["column", "schema", "table", "type"],
            "expected each unsupported column as {{ schema, table, column, type }}, got {column}"
        );
    }
    let mut got: Vec<(String, String, String)> = unsupported
        .iter()
        .map(|c| {
            (
                c["schema"].as_str().unwrap_or_default().to_owned(),
                c["table"].as_str().unwrap_or_default().to_owned(),
                c["column"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    got.sort();
    let want: Vec<(String, String, String)> = UNSUPPORTED
        .iter()
        .map(|(s, t, c, _)| ((*s).to_owned(), (*t).to_owned(), (*c).to_owned()))
        .collect();
    assert_eq!(
        got, want,
        "expected exactly the columns of a type the puller cannot express to be listed, got {}",
        answer["unsupported"]
    );
    for (_, table, column, type_word) in UNSUPPORTED {
        let listed = unsupported
            .iter()
            .find(|c| c["table"] == json!(table) && c["column"] == json!(column))
            .unwrap();
        assert!(
            listed["type"].as_str() == Some(type_word),
            "expected the type of {table}.{column} as Postgres prints it with nothing on the search path ({type_word}), got {listed}"
        );
        assert!(
            schema_ts.contains(&format!("\"{column}\"")),
            "expected the unsupported column {table}.{column} in the definition all the same, not dropped:\n{schema_ts}"
        );
    }
}

#[test]
fn a_grant_at_any_level_allows_the_pull_and_says_the_level() {
    let stock = Stock::start();
    stock.register(STOCK);
    for (group, level) in [
        ("viewers", "structure"),
        ("readers", "read"),
        ("writers", "write"),
    ] {
        stock.scenario.grant(STOCK, group, level);
    }
    let mut seen: Vec<Value> = Vec::new();
    for (user, level) in [
        (User::new("sam").in_groups(&["viewers"]), "structure"),
        (User::new("rita").in_groups(&["readers"]), "read"),
        (
            User::new("will").in_groups(&["writers", "viewers"]),
            "write",
        ),
    ] {
        let answer = stock.pull_ok(&user, STOCK);
        assert_eq!(
            answer["your_access"],
            json!(level),
            "expected `your_access` to be {:?}'s highest level, got {answer}",
            user.id
        );
        seen.push(answer);
    }
    for answer in &seen[1..] {
        assert_eq!(answer["fingerprint"], seen[0]["fingerprint"]);
        assert_eq!(
            answer["schema_ts"], seen[0]["schema_ts"],
            "expected the same definition whatever the level"
        );
    }
}

#[test]
fn without_a_grant_the_pull_is_refused_with_whom_to_ask_and_nothing_is_sent() {
    let stock = Stock::start();
    stock.register(STOCK);
    stock.scenario.grant(STOCK, READERS, "read");
    let refused = stock.pull(&nora(), STOCK);
    assert_api_error(&refused, 403, "no-grant");
    let body = refused.json();
    assert_no_grant_error(&body, STOCK, "a pull with no Grant");
    assert_eq!(
        keys(&body["error"]),
        ["contact", "message", "reason", "resource"],
        "expected `resource` and `contact` beside `reason` and `message`, got {body}"
    );
    std::thread::sleep(Duration::from_millis(300));
    let sent = statements_from(
        &stock,
        &[
            &stock.structure.role,
            &stock.db().reader.role,
            &stock.db().writer.role,
        ],
    );
    assert!(
        sent.is_empty(),
        "expected nothing sent to the shared database for a refused pull, but its server log shows:\n{}",
        sent.join("\n")
    );
}

#[test]
fn a_resource_that_does_not_exist_is_not_found() {
    let stock = Stock::ready();
    assert_api_error(&stock.pull(&rita(), "nothing_here"), 404, "not-found");
    assert_api_error(
        &stock.pull(&administrator(), "nothing_here"),
        404,
        "not-found",
    );
}

#[test]
fn a_database_that_cannot_be_reached_or_refuses_the_login_is_structure_unavailable() {
    let stock = Stock::start();
    let db = stock.db();
    let missing_database = format!("missing_db_{}", token());
    let wrong_password = format!("pw-wrong-{}", token());
    let mut nodb = body_with_structure("nodb", db, &stock.structure);
    nodb["connection"]["database"] = json!(missing_database);
    let mut wrongpw = body_with_structure("wrongpw", db, &stock.structure);
    wrongpw["connection"]["structure"]["password"] = json!(wrong_password);
    let mut wrongread = resource_body("wrongread", db, &db.reader);
    wrongread["connection"]["read"]["password"] = json!(wrong_password);
    let mut closed = body_with_structure("closed", db, &stock.structure);
    closed["connection"]["port"] = json!(free_port().unwrap());
    for (name, body) in [
        ("nodb", &nodb),
        ("wrongpw", &wrongpw),
        ("wrongread", &wrongread),
        ("closed", &closed),
    ] {
        stock.scenario.register_body(name, body);
        stock.scenario.grant(name, READERS, "structure");
    }
    let mut markers = stock.markers();
    markers.push(("a database that does not exist", missing_database));
    markers.push(("a wrong password", wrong_password));
    for name in ["nodb", "wrongpw", "wrongread", "closed"] {
        let answer = stock.pull(&rita(), name);
        assert_api_error(&answer, 503, "structure-unavailable");
        let text = answer.describe();
        for (what, marker) in &markers {
            assert!(
                !text.contains(marker.as_str()),
                "expected nothing about the connection in the answer for {name:?}, found {what}:\n{text}"
            );
        }
        assert!(
            !blackbox::shared_database::find_port(&text, db.port()),
            "expected the port in no answer, got\n{text}"
        );
        let entries = entries_of(&stock.scenario, &answer);
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_pull_entry(
            &entries[0],
            &answer,
            &rita(),
            name,
            "failed",
            None,
            Some("unavailable"),
        );
    }
}

#[test]
fn every_pull_allowed_refused_or_failed_has_one_operation_log_entry() {
    let stock = Stock::ready();
    let mut missing = body_with_structure("gone", stock.db(), &stock.structure);
    missing["connection"]["database"] = json!(format!("missing_db_{}", token()));
    stock.scenario.register_body("gone", &missing);
    stock.scenario.grant("gone", READERS, "read");

    let allowed = stock.pull(&rita(), STOCK);
    allowed.expect_status(200);
    let refused = stock.pull(&nora(), STOCK);
    assert_api_error(&refused, 403, "no-grant");
    let failed = stock.pull(&rita(), "gone");
    assert_api_error(&failed, 503, "structure-unavailable");

    for (response, user, resource, outcome, reason, error) in [
        (&allowed, rita(), STOCK, "allowed", None, None),
        (&refused, nora(), STOCK, "refused", Some("no-grant"), None),
        (&failed, rita(), "gone", "failed", None, Some("unavailable")),
    ] {
        let entries = entries_of(&stock.scenario, response);
        assert_eq!(
            entries.len(),
            1,
            "expected exactly one Operation log entry for the {outcome} pull, got {entries:?}"
        );
        assert_pull_entry(
            &entries[0],
            response,
            &user,
            resource,
            outcome,
            reason,
            error,
        );
    }
    // A pull of a Resource that does not exist is no operation on one.
    let none = stock.pull(&rita(), "nothing_here");
    assert_api_error(&none, 404, "not-found");
}

#[test]
fn the_structure_login_is_in_no_answer_and_no_log_record() {
    let stock = Stock::start();
    let admin = stock.scenario.admin();
    let mut answers: Vec<Response> = Vec::new();
    answers.push(admin.create_resource(&body_with_structure(STOCK, stock.db(), &stock.structure)));
    answers[0].expect_status(201);
    stock.scenario.grant(STOCK, READERS, "structure");
    let mut wrong = body_with_structure("wrongpw", stock.db(), &stock.structure);
    wrong["connection"]["structure"]["password"] =
        json!(format!("{}-not", stock.structure.password()));
    answers.push(admin.create_resource(&wrong));
    stock.scenario.grant("wrongpw", READERS, "structure");
    // Refused bodies with the structure login in them.
    answers.push(admin.create_resource(&body_with_structure(STOCK, stock.db(), &stock.structure)));
    answers.push(
        stock
            .scenario
            .vm
            .api_as(&rita())
            .create_resource(&body_with_structure("other", stock.db(), &stock.structure)),
    );
    let mut bad = body_with_structure("bad", stock.db(), &stock.structure);
    bad["connection"]["structure"]["password"] = json!(42);
    answers.push(admin.create_resource(&bad));
    answers.push(admin.get_resource(STOCK));
    answers.push(admin.list_resources());
    answers.push(stock.pull(&rita(), STOCK));
    answers.push(stock.pull(&rita(), "wrongpw"));
    answers.push(stock.pull(&nora(), STOCK));
    answers.push(admin.operation_log(None));
    answers[7].expect_status(200);

    let mut texts: Vec<(String, String)> = answers
        .iter()
        .map(|a| (format!("the answer to {}", a.request()), a.describe()))
        .collect();
    texts.extend(
        stock
            .scenario
            .vm
            .lines()
            .into_iter()
            .map(|l| ("the program's log".to_owned(), l.raw)),
    );
    let mut found = Vec::new();
    for (whence, text) in &texts {
        for (what, marker) in stock.markers() {
            if text.contains(marker.as_str()) {
                found.push(format!("{what} in {whence}:\n    {text}"));
            }
        }
    }
    assert!(
        found.is_empty(),
        "expected no answer and no log record to hold any part of a Resource's connection, the structure login included, but:\n{}",
        found.join("\n")
    );
}

#[test]
fn a_structure_login_of_the_wrong_shape_is_malformed() {
    let stock = Stock::start();
    let admin = stock.scenario.admin();
    for (name, structure) in [
        ("text", json!("not a login")),
        ("norole", json!({ "password": "x" })),
        ("nopassword", json!({ "role": "x" })),
        ("numbers", json!({ "role": 1, "password": 2 })),
    ] {
        let mut body = body_with_structure(name, stock.db(), &stock.structure);
        body["connection"]["structure"] = structure.clone();
        assert_api_error(&admin.create_resource(&body), 400, "malformed");
        assert_api_error(&admin.get_resource(name), 404, "not-found");
    }
    // It is optional: a Resource without one is registered as before.
    stock.register_without_structure("plain");
    // And `null` is the same as none.
    let mut body = body_with_structure("nulled", stock.db(), &stock.structure);
    body["connection"]["structure"] = Value::Null;
    admin.create_resource(&body).expect_status(201);
}

#[test]
fn when_the_entry_cannot_be_written_the_pull_is_unavailable_and_nothing_is_sent() {
    let mut platform_postgres = PrivatePostgres::start().unwrap();
    let platform = Platform::on(&platform_postgres, "vm_platform_pull_log_unavailable");
    let scenario = Scenario::start_with(&VmOptions::new().platform(&platform));
    let structure = blackbox::structure::build_stock(&scenario.db, &blackbox::structure::IN_ORDER);
    scenario.register_body(STOCK, &body_with_structure(STOCK, &scenario.db, &structure));
    scenario.grant(STOCK, READERS, "read");
    platform_postgres.stop();
    let answer = pull(&scenario.vm.api_as(&rita()), STOCK);
    assert_api_error(&answer, 503, "unavailable");
    std::thread::sleep(Duration::from_millis(300));
    let sent: Vec<String> = scenario
        .db
        .statements_by_login()
        .into_iter()
        .filter(|(by, _)| {
            [
                &structure.role,
                &scenario.db.reader.role,
                &scenario.db.writer.role,
            ]
            .contains(&by)
        })
        .map(|(_, e)| e)
        .collect();
    assert!(
        sent.is_empty(),
        "expected nothing sent to the shared database when the Operation log entry cannot be written, but:\n{}",
        sent.join("\n")
    );
}

#[test]
fn a_structure_larger_than_4_mib_is_structure_too_large() {
    let stock = Stock::start();
    // 48 tables of 1600 columns with names of 60 characters: more than
    // 4 MiB of catalogs, and of definition, whichever is measured.
    let superuser = stock.db().superuser();
    for t in 0..48 {
        let columns: Vec<String> = (0..1600)
            .map(|c| format!("c{c:04}_{}", "x".repeat(54)))
            .map(|name| format!("{name} text"))
            .collect();
        superuser.batch(&format!(
            "CREATE TABLE wide_{t:02}_{} ({})",
            "w".repeat(40),
            columns.join(", ")
        ));
    }
    stock.register(STOCK);
    stock.scenario.grant(STOCK, READERS, "structure");
    let answer = stock.pull(&rita(), STOCK);
    assert_api_error(&answer, 422, "structure-too-large");
    let entries = entries_of(&stock.scenario, &answer);
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_pull_entry(
        &entries[0],
        &answer,
        &rita(),
        STOCK,
        "failed",
        None,
        Some("bytes"),
    );
}

#[test]
fn a_pull_that_does_not_answer_within_10_seconds_is_structure_unavailable() {
    let stock = Stock::ready();
    // The harness's superuser holds the catalog of constraints locked: a
    // pull that reads keys waits.
    let holder = Sql::connect(&stock.db().server().url(stock.db().name())).unwrap();
    holder.batch("BEGIN; LOCK TABLE pg_catalog.pg_constraint IN ACCESS EXCLUSIVE MODE");
    let began = Instant::now();
    let answer = stock.pull(&rita(), STOCK);
    let took = began.elapsed();
    holder.batch("ROLLBACK");
    assert_api_error(&answer, 503, "structure-unavailable");
    assert!(
        took >= Duration::from_secs(9) && took < Duration::from_secs(20),
        "expected the pull to be ended at its 10 seconds, it took {took:?}"
    );
    let entries = entries_of(&stock.scenario, &answer);
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_pull_entry(
        &entries[0],
        &answer,
        &rita(),
        STOCK,
        "failed",
        None,
        Some("time"),
    );
    // Once the lock is gone, the next pull answers.
    stock.pull_ok(&rita(), STOCK);
}

#[test]
fn every_kind_of_relation_of_every_schema_but_postgress_own_is_in_the_structure() {
    let stock = Stock::ready();
    let answer = stock.pull_ok(&rita(), STOCK);
    let ts = answer["schema_ts"].as_str().unwrap();
    for (what, needle) in [
        ("an ordinary table", "pgTable(\"sites\""),
        ("a table of another schema", "crmSchema.table(\"accounts\""),
        (
            "a table no login of the Resource may read",
            "pgTable(\"restricted\"",
        ),
        ("a partitioned table", "pgTable(\"readings\""),
        ("a view", "pgView(\"open_items\""),
        ("a materialized view", "pgMaterializedView(\"site_totals\""),
        ("the other schema", "pgSchema(\"crm\")"),
        ("an enum", "pgEnum(\"status\""),
        ("an enum of another schema", "crmSchema.enum(\"tier\""),
        (
            "a table of the shared database's own setup",
            "pgTable(\"incidents\"",
        ),
    ] {
        assert!(
            ts.contains(needle),
            "expected {what} in the definition ({needle}), got\n{ts}"
        );
    }
    for (what, needle) in [
        ("a partition", "readings_2025"),
        ("a partition", "readings_2026"),
        ("Postgres's own schema", "pg_catalog"),
        ("Postgres's own schema", "information_schema"),
        ("a table of Postgres's own", "pg_class"),
        ("a value from a row", "North depot"),
        ("a value from a row", stock.db().row_marker.as_str()),
        ("whom to ask", CONTACT),
    ] {
        assert!(
            !ts.contains(needle),
            "expected no {what} ({needle}) in the definition, got\n{ts}"
        );
    }
    let views = ts.matches(".existing()").count();
    assert!(
        views >= 2,
        "expected the view and the materialized view as `.existing()`, got\n{ts}"
    );
}

#[test]
fn a_resource_registered_with_a_structure_login_of_its_own_answers_its_registration() {
    // The registration's answer is the same object as before: nothing of
    // the structure login in it.
    let scenario = Scenario::start();
    let login = structure_login(&scenario.db);
    let body = body_with_structure(STOCK, &scenario.db, &login);
    let created = scenario.register_body(STOCK, &body);
    assert_eq!(
        keys(&created),
        [
            "contact",
            "created_at",
            "created_by",
            "description",
            "kind",
            "name",
            "owner"
        ],
        "{created}"
    );
}

/// The sessions of `role` connected to `database` as (all, waiting on a
/// lock), seen from a connection to the instance's `postgres` database
/// (opened before the lock: a new connection to the locked database would
/// wait on it too).
fn sessions(observer: &Sql, database: &str, role: &str) -> (usize, usize) {
    let row = observer.batch(&format!(
        "SELECT count(*), count(*) FILTER (WHERE wait_event_type = 'Lock') FROM pg_stat_activity
          WHERE datname = '{database}' AND usename = '{role}'"
    ));
    let n = |i: usize| {
        row.first()
            .and_then(|r| r.get(i).cloned().flatten())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    (n(0), n(1))
}

#[test]
fn at_most_two_pulls_of_one_resource_run_at_once_and_its_applications_are_served_meanwhile() {
    // The Resource has no structure login: pulls share the read login
    // with the Application's operations.
    let stock = Stock::start();
    stock.register_without_structure(STOCK);
    stock.scenario.grant(STOCK, READERS, "read");
    stock
        .scenario
        .install("board", "query.js", &[(STOCK, "read")]);
    // Another Resource, over another database: its pulls are not counted.
    let other = blackbox::shared_database::SharedDatabase::create();
    stock.scenario.register_body(
        "elsewhere",
        &resource_body("elsewhere", &other, &other.reader),
    );
    stock.scenario.grant("elsewhere", READERS, "structure");
    let db = stock.db();
    let reader = db.reader.role.clone();

    let query = || {
        stock.scenario.query(
            &rita(),
            "board",
            &blackbox::scenario::sql_on(STOCK, "select 'served-while-pulling' as probe"),
        )
    };
    // The Application has queried once already, as it would have.
    let (_, warm) = query();
    blackbox::scenario::assert_rows(
        &warm,
        &["probe"],
        &json!([["served-while-pulling"]]),
        "before",
    );
    let observer = Sql::connect(&db.server().url("postgres")).unwrap();
    let holder = Sql::connect(&db.server().url(db.name())).unwrap();
    holder.batch("BEGIN; LOCK TABLE pg_catalog.pg_constraint IN ACCESS EXCLUSIVE MODE");
    let api = stock.scenario.vm.api_as(&rita());
    std::thread::scope(|scope| {
        let first = scope.spawn(|| pull(&api, STOCK));
        let second = scope.spawn(|| pull(&api, STOCK));
        // Both waiting on the lock, well within their 10 seconds.
        let deadline = Instant::now() + Duration::from_secs(6);
        while sessions(&observer, db.name(), &reader).1 < 2 {
            assert!(
                Instant::now() < deadline,
                "the harness's own check: expected two pulls waiting on the lock, got (sessions, waiting) {:?}",
                sessions(&observer, db.name(), &reader)
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let (sessions_before, _) = sessions(&observer, db.name(), &reader);

        // A third: answered at once, busy, and nothing sent.
        let began = Instant::now();
        let third = pull(&api, STOCK);
        let took = began.elapsed();
        assert_api_error(&third, 503, "structure-unavailable");
        assert!(
            took < Duration::from_millis(900),
            "expected the third pull answered at once, it took {took:?}"
        );
        let message = third.json()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_lowercase();
        assert!(
            message.contains("again"),
            "expected the message to say to try again shortly, got {message:?}"
        );
        let entries = entries_of(&stock.scenario, &third);
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_pull_entry(
            &entries[0],
            &third,
            &rita(),
            STOCK,
            "failed",
            None,
            Some("busy"),
        );
        std::thread::sleep(Duration::from_millis(300));
        let (sessions_after, waiting_after) = sessions(&observer, db.name(), &reader);
        assert_eq!(
            (sessions_after, waiting_after),
            (sessions_before, 2),
            "expected no third session of the read login for the refused pull"
        );

        // A pull of another Resource is not counted.
        let elsewhere = pull(&api, "elsewhere");
        assert_eq!(
            elsewhere.status,
            200,
            "expected a pull of another Resource allowed while two of this one run, got\n{}",
            elsewhere.describe()
        );

        // The Application's operations on the Resource are served.
        let (_, answer) = query();
        blackbox::scenario::assert_rows(
            &answer,
            &["probe"],
            &json!([["served-while-pulling"]]),
            "a query of the Application while two pulls run",
        );

        holder.batch("ROLLBACK");
        for (which, handle) in [("first", first), ("second", second)] {
            let answer = handle.join().unwrap();
            assert_eq!(
                answer.status,
                200,
                "expected the {which} pull to finish as usual once the lock is gone, got\n{}",
                answer.describe()
            );
        }
    });
    // And a new pull is allowed.
    stock.pull_ok(&rita(), STOCK);
}
