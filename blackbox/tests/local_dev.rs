//! `vibe dev`: the command, where it listens, what it prints, how it ends,
//! the help and the instruction file (CONTRACT.md, "The local loop, kept
//! running: the browser, test rows and reset": "The command", "The
//! instruction file"; ticket "Local loop in the browser, with test rows,
//! reset and working offline").
//!
//! A project made by `vibe new` declaring nothing, with the harness's handler
//! (`blackbox/src/local.rs`, `handler`: any path it does not know answers
//! `{ user }`). Each `vibe dev` listens on a port the system gave as free.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use blackbox::dev::{
    Dev, assert_nothing_left, dev_args, has_ipv6_loopback, listeners, run_dev_on_a_free_port,
    spawn_piped_listening,
};
use blackbox::local::{Local, OUTSIDER, local_project, port_open};
use blackbox::project::{
    code_in, help_commands, new_project, paragraphs, squashed, vm_commands_in,
};
use blackbox::sdk::TempProject;
use serde_json::{Value, json};

const NAME: &str = "dev-board";

fn project(local: &Local) -> TempProject {
    local_project(&local.machine, NAME, &[])
}

fn sorted_keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

#[test]
fn it_listens_on_the_loopback_address_only_and_says_so_in_its_first_json_line() {
    let local = Local::new();
    let project = project(&local);
    let dev = Dev::start(&local, &project.dir, NAME);
    let lines = dev.lines();
    let first = &lines[0];
    assert_eq!(
        sorted_keys(first),
        ["application", "event", "port", "url", "version"],
        "expected the first line to be the `listening` event with exactly its fields\n{}",
        dev.stdout()
    );
    assert_eq!(first["event"], json!("listening"));
    assert_eq!(
        first["url"],
        json!(format!("http://{NAME}.localhost:{}/", dev.port)),
        "expected the address of the Application at its own name"
    );
    assert_eq!(first["application"], json!(NAME));
    assert_eq!(first["port"], json!(dev.port));
    assert_eq!(
        first["version"],
        json!(1),
        "expected the first local Version"
    );

    let found = listeners(dev.port);
    assert!(
        found.iter().any(|l| l.address == "127.0.0.1"),
        "expected `vibe dev` to listen on 127.0.0.1:{}, found {found:?}",
        dev.port
    );
    assert!(
        found
            .iter()
            .all(|l| l.address == "127.0.0.1" || l.address == "::1"),
        "expected `vibe dev` to listen on the loopback address only, found {found:?}"
    );
    if has_ipv6_loopback() {
        assert!(
            found.iter().any(|l| l.address == "::1"),
            "expected `vibe dev` to listen on ::1 too, which this machine has; found {found:?}"
        );
        let v6 = SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, dev.port));
        assert!(
            TcpStream::connect_timeout(&v6, Duration::from_secs(2)).is_ok(),
            "expected a connection to [::1]:{} accepted",
            dev.port
        );
    }
    let run = dev.stop("INT");
    run.expect_code(0);
}

#[test]
fn each_request_it_serves_is_a_json_line_with_the_user_method_path_status_and_request_id() {
    let local = Local::new();
    let project = project(&local);
    let dev = Dev::start(&local, &project.dir, NAME);
    let mut browser = dev.browser();
    browser.choose(OUTSIDER);
    let response = browser.get("/somewhere");
    assert_eq!(response.status, 200, "{}", response.describe());
    assert_eq!(response.json(), json!({ "user": OUTSIDER }));
    let id = response.request_id();
    let line = dev.wait_for_line(
        "the `request` line of the request",
        Duration::from_secs(10),
        |l| l["event"] == "request" && l["request_id"] == id.as_str(),
    );
    assert_eq!(
        sorted_keys(&line),
        ["as", "event", "method", "path", "request_id", "status"],
        "expected the `request` line with exactly its fields, got {line}"
    );
    assert_eq!(line["as"], json!(OUTSIDER));
    assert_eq!(line["method"], json!("GET"));
    assert_eq!(line["path"], json!("/somewhere"));
    assert_eq!(line["status"], json!(200));
    dev.stop("TERM").expect_code(0);
}

#[test]
fn in_words_it_gives_the_address_and_that_it_runs_until_interrupted_then_a_line_for_each_request() {
    let local = Local::new();
    let project = project(&local);
    let dev = Dev::start_on_a_free_port(&local, &project.dir, NAME, false);
    let words = dev.stdout();
    assert!(
        words.contains(&dev.url()),
        "expected the address in words, got\n{words}"
    );
    let lower = words.to_lowercase();
    assert!(
        lower.contains("interrupt") || lower.contains("ctrl"),
        "expected the words to say that it runs until interrupted, got\n{words}"
    );
    let mut browser = dev.browser();
    browser.choose(OUTSIDER);
    let response = browser.get("/somewhere");
    assert_eq!(response.status, 200, "{}", response.describe());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let words = dev.stdout();
        if words.lines().any(|l| {
            l.contains("GET")
                && l.contains("/somewhere")
                && l.contains("200")
                && l.contains(OUTSIDER)
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "expected one line with the method, the path, the status and the test User, got\n{words}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    dev.stop("INT").expect_code(0);
}

#[test]
fn sigint_sigterm_and_sighup_each_end_it_with_exit_code_0_and_nothing_left_running() {
    let local = Local::new();
    let project = project(&local);
    for signal in ["INT", "TERM", "HUP"] {
        let dev = Dev::start(&local, &project.dir, NAME);
        let port = dev.port;
        let postmaster = dev.postmaster();
        let mut browser = dev.browser();
        browser.choose(OUTSIDER);
        assert_eq!(browser.get("/").status, 200);
        let sent = Instant::now();
        let run = dev.stop(signal);
        assert_eq!(
            run.code,
            Some(0),
            "{signal}: expected `vibe dev` to end with exit code 0\n{}",
            run.describe()
        );
        assert!(
            sent.elapsed() < Duration::from_secs(30),
            "{signal}: expected it to end within seconds of the signal, took {:?}",
            sent.elapsed()
        );
        assert_nothing_left(&local, Some(&postmaster), port, signal);
    }
}

#[test]
fn a_port_that_is_taken_is_port_in_use_naming_the_option() {
    let local = Local::new();
    let project = project(&local);
    let taken = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = taken.local_addr().unwrap().port();
    let args = dev_args(&project.dir, port, true);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    local.watch(&project.dir);
    let run = local.run(&refs, &[]);
    let message = run.json_error(1, "port-in-use");
    assert!(
        message.contains("--port"),
        "expected the message to name --port, got {message:?}"
    );
    drop(taken);
    let left = local.processes_after(Duration::from_secs(5));
    assert!(
        left.is_empty(),
        "expected nothing left running, found {left:#?}"
    );
}

#[test]
fn without_port_it_takes_7480() {
    // Held by the test (or by something else already): either way `vm
    // dev` without `--port` must find 7480 taken. No other test of the
    // harness uses 7480.
    let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 7480)).ok();
    assert!(
        held.is_some() || port_open(7480),
        "the harness's own check: 7480 is neither free to hold nor taken"
    );
    let local = Local::new();
    let project = project(&local);
    local.watch(&project.dir);
    let run = local.run(
        &["dev", "--project", project.dir.to_str().unwrap(), "--json"],
        &[],
    );
    run.json_error(1, "port-in-use");
    drop(held);
}

#[test]
fn a_port_outside_1_to_65535_is_exit_code_2() {
    let local = Local::new();
    let project = project(&local);
    let dir = project.dir.to_str().unwrap();
    for port in ["0", "65536", "-1", "http"] {
        let run = local.run(&["dev", "--project", dir, "--port", port, "--json"], &[]);
        assert_eq!(
            run.code,
            Some(2),
            "expected --port {port} refused on the command line\n{}",
            run.describe()
        );
        assert!(
            run.stderr.contains("--port"),
            "expected the usage error to be about --port {port}\n{}",
            run.describe()
        );
    }
    assert!(
        !project.dir.join(".vibe/local").exists(),
        "expected nothing made for a command line refused"
    );
}

#[test]
fn a_second_vm_dev_in_the_same_project_is_already_running_at_once_giving_the_first_address() {
    let local = Local::new();
    let project = project(&local);
    let first = Dev::start(&local, &project.dir, NAME);
    let began = Instant::now();
    let (_, second) = run_dev_on_a_free_port(&local, &project.dir, true, &[]);
    let message = second.json_error(1, "already-running");
    assert!(
        began.elapsed() < Duration::from_secs(15),
        "expected the second `vibe dev` to end at once, took {:?}",
        began.elapsed()
    );
    assert!(
        message.contains(&format!("{NAME}.localhost:{}", first.port)),
        "expected the message to give the address of the first, {}, got {message:?}",
        first.url()
    );
    // The first goes on serving.
    let mut browser = first.browser();
    browser.choose(OUTSIDER);
    assert_eq!(browser.get("/").status, 200);
    first.stop("INT").expect_code(0);
}

#[test]
fn failures_before_it_listens_are_those_of_vm_request_and_nothing_listens() {
    let local = Local::new();
    let not_a_project = local.machine.root().join("empty-folder");
    fs::create_dir_all(&not_a_project).unwrap();
    let (port, run) = run_dev_on_a_free_port(&local, &not_a_project, true, &[]);
    run.json_error(1, "not-a-project");
    assert!(!port_open(port), "expected nothing listening on {port}");

    let project = project(&local);
    fs::write(project.dir.join("vibe.dev.json"), "{ \"users\": 7 }\n").unwrap();
    local.watch(&project.dir);
    let (port, run) = run_dev_on_a_free_port(&local, &project.dir, true, &[]);
    run.json_error(1, "test-users-file");
    assert!(!port_open(port), "expected nothing listening on {port}");
    let left = local.processes_after(Duration::from_secs(5));
    assert!(
        left.is_empty(),
        "expected nothing left running, found {left:#?}"
    );
}

#[test]
fn a_closed_output_ends_it_with_output_closed_and_nothing_left_running() {
    let local = Local::new();
    let project = project(&local);
    local.watch(&project.dir);
    let (port, running, reader) = spawn_piped_listening(&local, &project.dir);
    let postmaster = blackbox::local::postmaster_of(&project.dir);
    // The reading end goes: the next line it prints finds no reader.
    drop(reader);
    let mut browser = blackbox::dev::Browser::new(
        SocketAddr::from(([127, 0, 0, 1], port)),
        &format!("{NAME}.localhost:{port}"),
    );
    let origin = browser.origin();
    let choose = browser
        .build("POST", "/_vm/test-user")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Origin", origin)
        .body("user=outsider");
    drop(choose.try_send());
    browser.set_cookie("vm_test_user", OUTSIDER);
    for _ in 0..3 {
        drop(browser.build("GET", "/").try_send());
    }
    let run = running.wait(Duration::from_secs(60));
    assert_eq!(
        run.code,
        Some(1),
        "expected a closed output to end `vibe dev` with exit code 1, as `vibe logs --follow` ends\n{}",
        run.describe()
    );
    assert!(
        !run.stderr.contains("panicked"),
        "expected no crash\n{}",
        run.describe()
    );
    assert_nothing_left(&local, postmaster.as_ref(), port, "a closed output");
}

#[test]
fn the_help_lists_dev_and_local_reset_with_their_options() {
    let local = Local::new();
    let help = local.machine.run(&["--help"]);
    help.expect_code(0);
    let commands = help_commands(&help.stdout);
    assert!(
        commands.iter().any(|c| c == "dev"),
        "expected `vibe --help` to list `dev`, got {commands:?}"
    );
    let dev = local.machine.run(&["dev", "--help"]);
    dev.expect_code(0);
    for option in ["--port", "--project", "--json"] {
        assert!(
            dev.stdout.contains(option),
            "expected `vibe dev --help` to name {option}, got\n{}",
            dev.stdout
        );
    }
    let local_help = local.machine.run(&["local", "--help"]);
    local_help.expect_code(0);
    assert!(
        help_commands(&local_help.stdout)
            .iter()
            .any(|c| c == "reset"),
        "expected `vibe local --help` to list `reset`, got\n{}",
        local_help.stdout
    );
    let reset = local.machine.run(&["local", "reset", "--help"]);
    reset.expect_code(0);
    for option in ["--project", "--timeout", "--json"] {
        assert!(
            reset.stdout.contains(option),
            "expected `vibe local reset --help` to name {option}, got\n{}",
            reset.stdout
        );
    }
}

#[test]
fn the_instruction_file_names_vm_dev_and_vm_local_reset_and_says_what_test_rows_are() {
    let local = Local::new();
    let dir = local.machine.root().join("instructed");
    new_project(&local.machine, "instructed", &dir);
    let text = fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    let commands = vm_commands_in(&text);
    assert!(
        commands.iter().any(|c| c == "dev"),
        "expected AGENTS.md to name `vibe dev` in code, got {commands:?}"
    );
    let code = squashed(&code_in(&text).join("\n"));
    assert!(
        code.contains("vibe local reset"),
        "expected AGENTS.md to name `vibe local reset` in code\n{text}"
    );
    let paragraphs: Vec<String> = paragraphs(&text).iter().map(|p| squashed(p)).collect();
    assert!(
        paragraphs.iter().any(|p| p.contains("vibe dev")
            && p.contains(".localhost:7480")
            && p.to_lowercase().contains("browser")),
        "expected a paragraph saying that `vibe dev` serves the project at http://<name>.localhost:7480/ for the Builder's browser\n{text}"
    );
    assert!(
        paragraphs
            .iter()
            .any(|p| p.contains("vibe dev") && p.contains("vibe request")),
        "expected AGENTS.md to say that the agent's own `vibe request` is answered by `vibe dev` while it runs\n{text}"
    );
    assert!(
        paragraphs
            .iter()
            .any(|p| p.contains("test-rows/") && p.contains("own-database.sql")),
        "expected AGENTS.md to say that test rows are SQL files in test-rows/, one a pulled Resource, and own-database.sql\n{text}"
    );
    assert!(
        text.lines().count() <= 120,
        "expected AGENTS.md at most 120 lines, got {}",
        text.lines().count()
    );
    let help = local.machine.run(&["--help"]);
    let listed = help_commands(&help.stdout);
    for command in commands {
        assert!(
            listed.contains(&command),
            "AGENTS.md names `vibe {command}`, which `vibe --help` does not list"
        );
    }
}

#[test]
fn vm_log_is_started_anew_once_past_16_mib_while_vm_dev_runs() {
    let local = Local::new();
    let project = project(&local);
    let dev = Dev::start(&local, &project.dir, NAME);
    let log = project.dir.join(".vibe/local/vibe.log");
    let size = || fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
    let mut browser = dev.browser();
    browser.choose(OUTSIDER);
    // The longest path served, from about 50,000 bytes down.
    let mut length = 50_000;
    loop {
        let path = format!("/{}", "p".repeat(length - 1));
        if browser.get(&path).status == 200 {
            break;
        }
        length /= 2;
        assert!(
            length > 1000,
            "the harness's own check: no long path is served"
        );
    }
    let limit: u64 = 17 * 1024 * 1024;
    let path = format!("/{}", "p".repeat(length - 1));
    // Paths enough for 24 MiB of them, each in at least one record.
    let requests = 24 * 1024 * 1024 / length + 1;
    let mut largest = 0;
    let mut started_anew = false;
    for n in 0..requests {
        let response = browser.get(&path);
        assert_eq!(response.status, 200, "request {n}\n{}", response.describe());
        if n % 10 == 0 {
            let now = size();
            assert!(
                now <= limit,
                "expected vibe.log at most 17 MiB while `vibe dev` runs, it is {now} bytes after {n} requests"
            );
            if now < largest {
                started_anew = true;
            }
            largest = largest.max(now);
        }
    }
    let now = size();
    assert!(
        now <= limit,
        "expected vibe.log at most 17 MiB, it is {now} bytes"
    );
    assert!(
        started_anew || now < largest,
        "the harness's own check: more than 16 MiB was written into vibe.log (largest seen {largest} bytes)"
    );
    dev.stop("INT").expect_code(0);
}
