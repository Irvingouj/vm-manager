//! The local loop without a connection (CONTRACT.md, "The local loop, kept
//! running: the browser, test rows and reset": "Without a connection";
//! ticket "Local loop in the browser, with test rows, reset and working
//! offline": with the network off and structure already pulled, the local
//! loop starts and serves).
//!
//! The project is pulled while the installation runs; then the tool signs
//! out, the installation is stopped, and `VM_LOCAL_POSTGRES_MIRROR` names an
//! address where nothing listens. The last test goes further: every command
//! runs in a network namespace of its own that has the loopback interface
//! and nothing else (`unshare`, as the user, no administrator rights), so
//! that a connection anywhere but the machine itself cannot be made at all.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::process::Command;
use std::time::Duration;

use blackbox::dev::{Dev, free_port};
use blackbox::local::{
    APP, BUILDER, Board, Local, Mirror, Requested, Serving, holds_postgres_programs,
};
use blackbox::program::{run_to_exit, vm_binary};
use blackbox::scenario::Scenario;
use blackbox::sdk::TempProject;
use blackbox::tool::closed_address;
use serde_json::{Value, json};

const ROWS: &str = "INSERT INTO incidents (id, title, open, severity, reported_at) VALUES\n  (1, 'Printer on fire', true, 2, '2026-11-01 09:00:00+00');\n";

const WAIT: [&str; 2] = ["--timeout", "60"];

/// A mirror's base URL at an address where nothing listens.
fn refusing_mirror() -> String {
    format!("http://{}/maven2", closed_address())
}

/// Signs the tool out and stops the installation; the machine and the
/// project, now with no installation, and a mirror that refuses.
fn cut_off(board: Board) -> (Local, TempProject) {
    let Board {
        local,
        project,
        scenario,
    } = board;
    local.machine.run(&["sign-out", "--json"]).ok_json();
    let Scenario { vm, db } = scenario;
    drop(vm);
    drop(db);
    (local.with_mirror(&refusing_mirror()), project)
}

/// `vm dev` serves the browser and the tool's own commands, then stops.
fn assert_dev_serves(local: &Local, project: &TempProject) {
    let dev = Dev::start(local, &project.dir, APP);
    let mut browser = dev.browser();
    browser.choose(BUILDER);
    let page = browser.get("/incidents");
    assert_eq!(page.status, 200, "{}", page.describe());
    assert_eq!(
        page.json()["incidents"],
        json!([{ "id": 1, "title": "Printer on fire" }])
    );
    let requested = Requested::of(&local.request(&project.dir, "/incidents", BUILDER, &WAIT));
    assert_eq!(requested.status(), 200, "{}", requested.describe());
    local.operation_log(&project.dir, &WAIT).ok_json();
    local.change_history(&project.dir, &WAIT).ok_json();
    let reset = local.run(
        &[
            "local",
            "reset",
            "--project",
            project.dir.to_str().unwrap(),
            "--json",
            "--timeout",
            "120",
        ],
        &[],
    );
    reset.ok_json();
    assert_eq!(browser.get("/incidents").status, 200);
    dev.stop("INT").expect_code(0);
}

/// The local commands alone, one after the other.
fn assert_alone_works(local: &Local, project: &TempProject) {
    let requested = local.request_ok(&project.dir, "/incidents", BUILDER, &[]);
    assert_eq!(requested.status(), 200, "{}", requested.describe());
    local.operation_log(&project.dir, &[]).ok_json();
    local.change_history(&project.dir, &[]).ok_json();
    local
        .run(
            &[
                "local",
                "reset",
                "--project",
                project.dir.to_str().unwrap(),
                "--json",
            ],
            &[],
        )
        .ok_json();
}

#[test]
fn signed_out_with_the_installation_stopped_and_the_mirror_refusing_the_local_loop_starts_and_serves()
 {
    let board = Board::start("read", "read");
    board.project.write("test-rows/devops.sql", ROWS);
    let (local, project) = cut_off(board);
    assert_dev_serves(&local, &project);
    assert_alone_works(&local, &project);
    assert_eq!(
        holds_postgres_programs(&local.postgres_folder()),
        Vec::<std::path::PathBuf>::new(),
        "expected nothing downloaded: VM_LOCAL_POSTGRES_BIN names a Postgres"
    );
}

#[test]
fn with_postgres_from_an_earlier_download_the_local_loop_works_with_the_mirror_refusing() {
    let mirror = Mirror::start(Serving::Archive);
    let board = Board::on(
        Local::new().without_bin().with_mirror(&mirror.base_url()),
        APP,
        "read",
        "read",
        "",
    );
    board.project.write("test-rows/devops.sql", ROWS);
    board.request_ok("/incidents", BUILDER, &[]);
    assert!(
        !holds_postgres_programs(&board.local.postgres_folder()).is_empty(),
        "the harness's own check: Postgres was downloaded into the user's folder"
    );
    drop(mirror);
    let (local, project) = cut_off(board);
    assert_dev_serves(&local, &project);
    assert_alone_works(&local, &project);
}

/// What the script does inside the namespace: each command's output, error
/// and exit code into files of `$out`.
const SCRIPT: &str = r#"set -u
vm="$1"; dir="$2"; port="$3"; out="$4"; app="$5"
exec >"$out/script.log" 2>&1
one() {
  name="$1"; shift
  "$vm" "$@" >"$out/$name.out" 2>"$out/$name.err"
  echo $? >"$out/$name.code"
}
one request-alone request /incidents --as builder --project "$dir" --json
one operation-log-alone local operation-log --project "$dir" --json
one change-history-alone local change-history --project "$dir" --json
one reset-alone local reset --project "$dir" --json
"$vm" dev --project "$dir" --port "$port" --json >"$out/dev.out" 2>"$out/dev.err" &
dev=$!
i=0
while [ $i -lt 2400 ]; do
  grep -q '"listening"' "$out/dev.out" && break
  kill -0 $dev 2>/dev/null || break
  sleep 0.1
  i=$((i + 1))
done
(
  exec 3<>"/dev/tcp/127.0.0.1/$port" || exit 1
  printf 'GET /incidents HTTP/1.1\r\nHost: %s.localhost:%s\r\nCookie: vm_test_user=builder\r\nAccept: text/html\r\nConnection: close\r\n\r\n' "$app" "$port" >&3
  timeout 120 cat <&3
) >"$out/browser.http"
one request-dev request /incidents --as builder --project "$dir" --json --timeout 60
one reset-dev local reset --project "$dir" --json --timeout 120
kill -TERM $dev
wait $dev
echo $? >"$out/dev.code"
"#;

fn read(out: &std::path::Path, name: &str) -> String {
    fs::read_to_string(out.join(name)).unwrap_or_default()
}

fn ok_json_of(out: &std::path::Path, name: &str) -> Value {
    let code = read(out, &format!("{name}.code"));
    let stdout = read(out, &format!("{name}.out"));
    assert_eq!(
        code.trim(),
        "0",
        "expected `{name}` to end with 0 with no network but the loopback\nstandard output:\n{stdout}\nstandard error:\n{}\nscript:\n{}",
        read(out, &format!("{name}.err")),
        read(out, "script.log")
    );
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("`{name}` printed no JSON ({e}): {stdout}"))
}

#[test]
fn with_no_network_but_the_loopback_the_local_commands_and_vm_dev_work() {
    let board = Board::start("read", "read");
    board.project.write("test-rows/devops.sql", ROWS);
    let (local, project) = cut_off(board);
    local.watch(&project.dir);
    let out = local.machine.root().join("offline-out");
    fs::create_dir_all(&out).unwrap();
    let script = local.machine.root().join("offline.sh");
    fs::write(&script, SCRIPT).unwrap();
    let port = free_port();
    let meta = fs::metadata(local.machine.root()).unwrap();
    let mut command = Command::new("unshare");
    command
        .arg("-rn")
        .arg("sh")
        .arg("-c")
        .arg(format!(
            "ip link set lo up && exec unshare -U --map-user={} --map-group={} bash \"$0\" \"$@\"",
            meta.uid(),
            meta.gid()
        ))
        .arg(&script)
        .arg(vm_binary())
        .arg(&project.dir)
        .arg(port.to_string())
        .arg(&out)
        .arg(APP)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", local.machine.home())
        .env("XDG_CONFIG_HOME", local.machine.absolute_config_home())
        .current_dir(local.machine.cwd());
    for (name, value) in local.env() {
        command.env(name, value);
    }
    let finished = run_to_exit(command, Duration::from_secs(600))
        .unwrap_or_else(|e| panic!("cannot run the commands in a namespace of their own: {e}"));
    assert!(
        finished.status.is_some_and(|s| s.success()),
        "the harness's own check: the namespace (unshare -rn) could be made\n{}\n{}\n{}",
        finished.stdout,
        finished.stderr,
        read(&out, "script.log")
    );
    let alone = ok_json_of(&out, "request-alone");
    assert_eq!(
        alone["body"]
            .as_str()
            .map(|b| serde_json::from_str::<Value>(b).unwrap()),
        Some(json!({ "user": BUILDER, "incidents": [{ "id": 1, "title": "Printer on fire" }] })),
        "{alone}"
    );
    ok_json_of(&out, "operation-log-alone");
    ok_json_of(&out, "change-history-alone");
    ok_json_of(&out, "reset-alone");
    let browser = read(&out, "browser.http");
    assert!(
        browser.starts_with("HTTP/1.1 200"),
        "expected `vm dev` to serve the browser with no network but the loopback, got\n{browser}\n`vm dev` printed:\n{}\n{}",
        read(&out, "dev.out"),
        read(&out, "dev.err")
    );
    assert!(browser.contains("Printer on fire"), "{browser}");
    let by_dev = ok_json_of(&out, "request-dev");
    assert_eq!(by_dev["status"], json!(200), "{by_dev}");
    ok_json_of(&out, "reset-dev");
    assert_eq!(
        read(&out, "dev.code").trim(),
        "0",
        "expected `vm dev` to end with 0 on SIGTERM\n{}\n{}",
        read(&out, "dev.out"),
        read(&out, "dev.err")
    );
    let left = local.processes_after(Duration::from_secs(5));
    assert!(
        left.is_empty(),
        "expected nothing left running, found {left:#?}"
    );
}
