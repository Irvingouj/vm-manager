//! The tool's own local commands while `vibe dev` runs in the project
//! (CONTRACT.md, "The local loop, kept running: the browser, test rows and
//! reset": "The tool's own commands while it runs"; ticket "Local loop in
//! the browser, with test rows, reset and working offline").
//!
//! A command answered by `vibe dev` starts no Postgres of its own: the one
//! `vibe dev` runs is the same before and after, and the request it sent is
//! among `vibe dev`'s `request` lines. Each command is given `--timeout 60`,
//! so that one that waited for `vibe dev` to end instead would end
//! `request-timed-out`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::net::{Ipv4Addr, TcpListener};
use std::thread;
use std::time::{Duration, Instant};

use blackbox::dev::{Dev, hex64_runs, local_files_but_postgres, local_files_holding};
use blackbox::generate::{Generated, generate};
use blackbox::local::{
    APP, BUILDER, Board, Local, OUTSIDER, Requested, handler, local_project, port_open,
    postmaster_of,
};
use blackbox::project::{NOTES_SCHEMA_TS, is_running};
use blackbox::tool::mode_of;
use serde_json::json;

const WAIT: [&str; 2] = ["--timeout", "60"];

/// A [`Board`] whose own database has `notes`, migrated by `0000_add_notes`.
fn board_with_notes() -> Board {
    let board = Board::start("read", "read");
    board.project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    board.project.write("src/index.ts", &handler(true, true));
    Generated::made(
        &generate(&board.local.machine, board.dir(), &["--name", "add_notes"]),
        board.dir(),
    );
    board
}

/// Fails unless `vibe dev` served the request `id` (a `request` line).
fn assert_served_by(dev: &Dev, id: &str, context: &str) {
    let served = dev.events("request").iter().any(|l| l["request_id"] == id);
    assert!(
        served,
        "{context}: expected the request {id} served by `vibe dev` (one of its `request` lines)\n{}",
        dev.stdout()
    );
}

#[test]
fn vm_request_is_answered_by_it_as_the_user_as_names_whatever_the_browser_chose() {
    let board = Board::start("read", "read");
    let dev = Dev::start(&board.local, board.dir(), APP);
    let postgres = dev.postmaster();
    let mut browser = dev.browser();
    browser.choose(OUTSIDER);
    assert_eq!(browser.get("/incidents").status, 403);
    let requested = Requested::of(&board.request("/incidents", BUILDER, &WAIT));
    assert_eq!(
        requested.json(),
        json!({ "user": BUILDER, "incidents": [] }),
        "expected the request as the User --as names\n{}",
        requested.describe()
    );
    assert_eq!(requested.body["as"], json!(BUILDER));
    assert_eq!(requested.body["application"], json!(APP));
    assert_eq!(requested.version(), 1);
    assert_eq!(requested.operations().len(), 1, "{}", requested.describe());
    assert_eq!(
        requested.body["standins"],
        json!([{ "resource": "devops", "made": false }]),
        "expected every pulled Resource in `standins`, `made` false: the request caused no rebuild"
    );
    assert_eq!(requested.body["migrations_applied"], json!([]));
    assert_eq!(requested.test_rows(), json!([]));
    assert_served_by(&dev, &requested.request_id(), "vibe request");
    let now = dev.postmaster();
    assert_eq!(
        now.pid, postgres.pid,
        "expected no second Postgres: the command was answered by `vibe dev`"
    );
    assert!(is_running(postgres.pid));
    // The browser's choice is its own.
    assert_eq!(browser.get("/incidents").status, 403);
    dev.stop("INT").expect_code(0);
}

#[test]
fn vm_request_while_it_runs_has_the_same_reasons_and_exit_codes_as_alone() {
    let board = Board::start("read", "read");
    let dev = Dev::start(&board.local, board.dir(), APP);
    let run = board.request("/incidents", "nobody-here", &WAIT);
    let message = run.json_error(1, "no-such-test-user");
    assert!(
        message.contains(BUILDER) && message.contains(OUTSIDER),
        "{message}"
    );
    let run = board.request("incidents", BUILDER, &WAIT);
    assert_eq!(run.code, Some(2), "{}", run.describe());
    dev.stop("INT").expect_code(0);
}

#[test]
fn what_a_request_while_it_runs_rebuilt_is_in_the_output_of_vm_request() {
    let board = Board::start("read", "read");
    let dev = Dev::start(&board.local, board.dir(), APP);
    board.project.write(
        "test-rows/devops.sql",
        "INSERT INTO incidents (id, title, open, severity, reported_at) VALUES\n  (1, 'Printer on fire', true, 2, '2026-11-01 09:00:00+00');\n",
    );
    let requested = Requested::of(&board.request("/incidents", BUILDER, &WAIT));
    assert_eq!(
        requested.test_rows(),
        json!([{ "file": "test-rows/devops.sql", "rows": 1 }]),
        "expected the test rows the rebuild it caused loaded\n{}",
        requested.describe()
    );
    assert_eq!(
        requested.json()["incidents"],
        json!([{ "id": 1, "title": "Printer on fire" }])
    );
    assert_served_by(&dev, &requested.request_id(), "vibe request");
    dev.stop("INT").expect_code(0);
}

#[test]
fn the_local_operation_log_and_change_history_are_answered_by_it() {
    let board = board_with_notes();
    let dev = Dev::start(&board.local, board.dir(), APP);
    let postgres = dev.postmaster();
    let mut browser = dev.browser();
    browser.choose(BUILDER);
    let request = browser
        .build("POST", "/notes")
        .body("written in the browser");
    let written = browser.send(&request);
    assert_eq!(written.status, 200, "{}", written.describe());
    let id = written.request_id();
    let entries = board.local.operation_log(board.dir(), &WAIT).ok_json()["entries"]
        .as_array()
        .unwrap()
        .clone();
    assert!(
        entries.iter().any(|e| e["request"] == id.as_str()),
        "expected the browser's request in the local Operation log, got {entries:#?}"
    );
    let changes = board.local.change_history(board.dir(), &WAIT).ok_json()["changes"]
        .as_array()
        .unwrap()
        .clone();
    assert!(
        changes
            .iter()
            .any(|c| c["request"] == id.as_str() && c["new"]["body"] == "written in the browser"),
        "expected the browser's write in the local Change history, got {changes:#?}"
    );
    assert_eq!(
        dev.postmaster().pid,
        postgres.pid,
        "expected no second Postgres"
    );
    dev.stop("INT").expect_code(0);
}

/// Requests to `/_vm/control/...` that do not hold the secret, each with
/// the status it must get: `404` at the Application's name, `307` at the
/// loopback addresses (sent on to the Application's name, where it is a
/// `404` again).
fn without_the_secret(dev: &Dev) -> Vec<(blackbox::Request, u16)> {
    let mut requests = Vec::new();
    for path in [
        "/_vm/control/",
        "/_vm/control/request",
        "/_vm/control/reset",
        "/_vm/control/operation-log",
        "/_vm/control/change-history",
        "/_vm/control/x/../reset",
    ] {
        for method in ["GET", "POST"] {
            requests.push((dev.request(path).method(method), 404));
            requests.push((
                dev.request(path)
                    .method(method)
                    .header("Authorization", "Bearer wrong-secret-0123456789abcdef")
                    .header("x-vm-control-secret", "wrong-secret-0123456789abcdef")
                    .header("Cookie", "vm_test_user=builder")
                    .json(&json!({ "path": "/notes", "as": "builder", "secret": "wrong" })),
                404,
            ));
        }
    }
    for host in ["127.0.0.1", "localhost"] {
        requests.push((
            blackbox::Request::new(
                dev.address(),
                "/_vm/control/reset",
                &format!("{host}:{}", dev.port),
            )
            .method("POST"),
            307,
        ));
    }
    requests
}

#[test]
fn the_control_paths_answer_404_without_the_secret_and_do_nothing() {
    let board = board_with_notes();
    let dev = Dev::start(&board.local, board.dir(), APP);
    let mut browser = dev.browser();
    browser.choose(BUILDER);
    let request = browser.build("POST", "/notes").body("kept");
    assert_eq!(browser.send(&request).status, 200);
    for (request, expected) in without_the_secret(&dev) {
        let response = request.send();
        assert_eq!(
            response.status,
            expected,
            "{request}: expected {expected} without the secret\n{}",
            response.describe()
        );
    }
    assert_eq!(
        browser.get("/notes").json()["notes"],
        json!([{ "id": 1, "body": "kept" }]),
        "expected nothing reset by a request without the secret"
    );
    dev.stop("INT").expect_code(0);
}

#[test]
fn the_file_that_finds_it_is_0600_and_its_secret_is_in_no_output_log_or_page() {
    let board = board_with_notes();
    let dev = Dev::start(&board.local, board.dir(), APP);
    let postgres = dev.postmaster();
    let found = local_files_holding(
        board.dir(),
        Some(&postgres.data),
        dev.port.to_string().as_bytes(),
    );
    assert!(
        !found.is_empty(),
        "expected a file of .vibe/local/ holding the port {} of `vibe dev`",
        dev.port
    );
    for file in &found {
        assert_eq!(
            mode_of(file),
            0o600,
            "expected {} of mode 0600",
            file.display()
        );
    }
    let holding_port: Vec<String> = found
        .iter()
        .flat_map(|f| hex64_runs(&fs::read(f).unwrap()))
        .collect();
    assert!(
        !holding_port.is_empty(),
        "expected a secret of 64 lower-case hex digits in the file that holds the port, {found:?}"
    );
    // Every such run of the files of .vibe/local/ but Postgres's own: the
    // secret, and the private Postgres's passwords when written so.
    let secrets: Vec<String> = local_files_but_postgres(board.dir(), &postgres.data)
        .iter()
        .flat_map(|f| hex64_runs(&fs::read(f).unwrap()))
        .collect();
    // What a run shows: pages, answers, `vibe request`, the records.
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut browser = dev.browser();
    seen.push(("the page".to_owned(), browser.get("/_vm/test-user").text()));
    browser.choose(BUILDER);
    seen.push(("an answer".to_owned(), browser.get("/notes").describe()));
    seen.push((
        "a 404".to_owned(),
        dev.request("/_vm/control/").send().describe(),
    ));
    seen.push((
        "a 421".to_owned(),
        dev.request("/").host("evil.example").send().describe(),
    ));
    let requested = board.request("/echo", BUILDER, &WAIT);
    seen.extend(requested.texts());
    seen.extend(board.local.operation_log(board.dir(), &WAIT).texts());
    seen.extend(board.local.change_history(board.dir(), &WAIT).texts());
    let run = dev.stop("INT");
    run.expect_code(0);
    seen.extend(run.texts());
    for log in ["vibe.log", "postgres.log"] {
        let path = board.dir().join(".vibe/local").join(log);
        seen.push((
            log.to_owned(),
            String::from_utf8_lossy(&fs::read(&path).unwrap_or_default()).into_owned(),
        ));
    }
    for secret in &secrets {
        for (whence, text) in &seen {
            assert!(
                !text.contains(secret.as_str()),
                "expected the secret of the file that finds `vibe dev` in no output, log or page; {whence} holds it"
            );
        }
    }
}

#[test]
fn a_file_left_by_a_vm_dev_killed_outright_is_not_believed() {
    let board = Board::start("read", "read");
    let dev = Dev::start(&board.local, board.dir(), APP);
    let port = dev.port;
    let killed = dev.kill();
    assert_eq!(
        killed.code, None,
        "the harness's own check: killed outright"
    );
    // Something else takes the port the file names: it must not be asked.
    let impostor = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .unwrap_or_else(|e| panic!("the harness's own check: cannot take {port} back: {e}"));
    impostor.set_nonblocking(true).unwrap();
    let requested = Requested::of(&board.request("/incidents", BUILDER, &["--timeout", "120"]));
    assert_eq!(
        requested.json(),
        json!({ "user": BUILDER, "incidents": [] }),
        "expected the command to run alone\n{}",
        requested.describe()
    );
    assert!(
        impostor.accept().is_err(),
        "expected no connection to the port a killed `vibe dev` left in its file"
    );
    drop(impostor);
    assert!(!port_open(port));
    let left = board.local.processes_after(Duration::from_secs(5));
    assert!(
        left.is_empty(),
        "expected nothing left running after the command, found {left:#?}"
    );
    // And a new `vibe dev` starts.
    let again = Dev::start(&board.local, board.dir(), APP);
    again.stop("INT").expect_code(0);
}

/// How many rounds of the test below must start their command inside the
/// window, and how many it may try for them.
const STARTING_HITS: usize = 3;
const STARTING_TRIES: usize = 8;

#[test]
fn a_vm_request_started_while_vm_dev_is_starting_is_answered_as_soon_as_it_listens() {
    // `vibe dev` is starting (it holds the project's lock and does not listen
    // yet) from the moment the project's private Postgres listens, which a
    // run starts only once it holds the lock, until its `listening` line.
    // Each round is a project's first run, so that window holds the making
    // of the local databases and the build. The command is started inside
    // it: after the Postgres was seen, and before `vibe dev` said it listens.
    let local = Local::new();
    let mut hits = 0;
    let mut missed = Vec::new();
    for round in 0..STARTING_TRIES {
        if hits == STARTING_HITS {
            break;
        }
        let name = format!("starting-board-{round}");
        let project = local_project(&local.machine, &name, &[]);
        let mut dev = Dev::spawn_on_a_free_port(&local, &project.dir, &name, true);
        let deadline = Instant::now() + blackbox::local::LOCAL_TIMEOUT;
        while postmaster_of(&project.dir).is_none_or(|p| !is_running(p.pid)) {
            if dev.respawn_if_port_refused() {
                continue;
            }
            assert!(
                !dev.has_ended() && Instant::now() < deadline,
                "round {round}: expected the private Postgres of `vibe dev` to listen while it starts\n{}",
                dev.stdout()
            );
            thread::sleep(Duration::from_millis(10));
        }
        let listened_before = !dev.events("listening").is_empty();
        let began = Instant::now();
        let running = local.spawn_request(&project.dir, "/", OUTSIDER, &WAIT);
        let inside = !listened_before && dev.events("listening").is_empty();
        let run = running.wait(Duration::from_secs(90));
        let requested = Requested::of(&run);
        let took = began.elapsed();
        dev.wait_until_listening();
        if !inside {
            missed.push(round);
            dev.stop("INT").expect_code(0);
            continue;
        }
        hits += 1;
        assert!(
            took < Duration::from_secs(45),
            "round {round}: expected the command answered well inside its 60 seconds, took {took:?}"
        );
        assert_eq!(
            requested.status(),
            200,
            "round {round}\n{}",
            requested.describe()
        );
        let id = requested.request_id();
        assert!(
            dev.events("request")
                .iter()
                .any(|l| l["request_id"] == id.as_str()),
            "round {round}: a command started while `vibe dev` was starting is answered by it as soon as it listens; it printed no `request` line for {id}\n{}",
            dev.stdout()
        );
        dev.stop("INT").expect_code(0);
    }
    println!(
        "the command was started while `vibe dev` was starting in {hits} round(s); missed the window in {:?}",
        missed
    );
    assert!(
        hits >= 1,
        "the harness's own check: in none of {STARTING_TRIES} rounds was the command started while `vibe dev` was starting (missed: {missed:?})"
    );
}

#[test]
fn a_second_vm_dev_started_while_the_first_is_starting_ends_already_running_without_waiting() {
    let local = Local::new();
    let project = local_project(&local.machine, "twice-board", &[]);
    let mut first = Dev::spawn_on_a_free_port(&local, &project.dir, "twice-board", true);
    let mut second = Dev::spawn_on_a_free_port(&local, &project.dir, "twice-board", true);
    let deadline = Instant::now() + blackbox::local::LOCAL_TIMEOUT;
    // Until one ends, or one says it listens; then the other has 15 seconds.
    let mut listened: Option<Instant> = None;
    let (ended, mut running) = loop {
        if first.respawn_if_port_refused() || second.respawn_if_port_refused() {
            continue;
        }
        if second.has_ended() {
            break (second, first);
        }
        if first.has_ended() {
            break (first, second);
        }
        if listened.is_none()
            && (!first.events("listening").is_empty() || !second.events("listening").is_empty())
        {
            listened = Some(Instant::now());
        }
        if let Some(at) = listened {
            assert!(
                at.elapsed() < Duration::from_secs(15),
                "expected the other `vibe dev` to end already-running as soon as the first listens, not to wait for it to end"
            );
        }
        assert!(
            Instant::now() < deadline,
            "expected one of the two `vibe dev` to listen and the other to end already-running"
        );
        thread::sleep(Duration::from_millis(50));
    };
    let run = ended.wait_end(Duration::from_secs(5));
    run.json_error(1, "already-running");
    running.wait_until_listening();
    running.stop("INT").expect_code(0);
}

/// `vibe request /echo --as outsider` with 12 headers of 100,000 bytes and a
/// `--data-file` of exactly 1 MiB, in the project `dir`.
fn large_request(local: &Local, dir: &std::path::Path) -> blackbox::tool::Run {
    let data = local.machine.root().join("one-mebibyte.txt");
    fs::write(&data, "d".repeat(1_048_576)).unwrap();
    let mut extra: Vec<String> = Vec::new();
    for n in 0..12 {
        extra.push("--header".to_owned());
        extra.push(format!("x-large-{n}: {}", "h".repeat(100_000)));
    }
    extra.push("--data-file".to_owned());
    extra.push(data.to_str().unwrap().to_owned());
    extra.push("--timeout".to_owned());
    extra.push("120".to_owned());
    let refs: Vec<&str> = extra.iter().map(String::as_str).collect();
    local.request(dir, "/echo", OUTSIDER, &refs)
}

#[test]
fn a_large_vm_request_is_answered_by_vm_dev_as_it_is_alone() {
    let local = Local::new();
    let project = local_project(&local.machine, "large-request-board", &[]);
    let alone = Requested::of(&large_request(&local, &project.dir));
    let dev = Dev::start(&local, &project.dir, "large-request-board");
    let run = large_request(&local, &project.dir);
    assert_eq!(
        run.code,
        Some(0),
        "expected the same large request answered by `vibe dev`\n{}",
        run.describe()
    );
    let served = Requested::of(&run);
    assert_eq!(
        served.status(),
        alone.status(),
        "expected the status it has alone"
    );
    assert_served_by(&dev, &served.request_id(), "the large vibe request");
    dev.stop("INT").expect_code(0);
}

/// This test takes about 35 seconds: the handler sleeps 33 seconds, past
/// the 30 an installation gives a request.
#[test]
fn the_code_is_given_the_same_time_alone_and_under_vm_dev() {
    let local = Local::new();
    let alone_project = local_project(&local.machine, "time-alone-board", &[]);
    let dev_project = local_project(&local.machine, "time-dev-board", &[]);
    let dev = Dev::start(&local, &dev_project.dir, "time-dev-board");
    let args = ["--timeout", "120"];
    let (alone, under_dev) = thread::scope(|scope| {
        let alone =
            scope.spawn(|| local.request(&alone_project.dir, "/slow?ms=33000", OUTSIDER, &args));
        let under_dev =
            scope.spawn(|| local.request(&dev_project.dir, "/slow?ms=33000", OUTSIDER, &args));
        (alone.join().unwrap(), under_dev.join().unwrap())
    });
    let alone = Requested::of(&alone);
    let under_dev = Requested::of(&under_dev);
    assert_ne!(
        alone.status(),
        200,
        "expected the handler stopped at the 30 seconds an installation gives, not its own 200 after 33\n{}",
        alone.describe()
    );
    assert_eq!(
        under_dev.status(),
        alone.status(),
        "expected the same answer under `vibe dev` as alone\n{}",
        under_dev.describe()
    );
    dev.stop("INT").expect_code(0);
}
