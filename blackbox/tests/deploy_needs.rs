//! How `vm deploy` ends, what it leaves, and the rules of every command
//! (CONTRACT.md, "Deploying: The command": the project, `node`, the
//! packages, the folder of its own in `TMPDIR`, the process groups and an
//! interruption as for `vm check`; `--timeout`, 300 seconds without it,
//! `deploy-timed-out`; "A project: Checking a project" for what holds as
//! for the check; "The tool: Every command").
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use blackbox::deploy::{
    DEPLOY_TIMEOUT, Deployed, commit_all, dana, deploy, deploy_args, deploy_project,
    deploy_with_env, project_tree, signed_in, tool_path,
};
use blackbox::generate::{listed, own_tmpdir, wait_for_pids};
use blackbox::project::{
    KillOnDrop, pids_in, schema_starting_a_child, still_running_after, type_errors,
};
use blackbox::sdk::TempProject;
use blackbox::tool::{Broken, Machine, Run};
use blackbox::{Vm, VmOptions};

fn vm() -> Vm {
    Vm::start(&VmOptions::new().stand_in_sign_in())
}

/// A committed project named `name` whose `src/db/schema.ts`, when
/// `drizzle-kit` runs it, records its process id and a child's in a file
/// beside the project, and never finishes.
fn hanging(label: &str, name: &str) -> (TempProject, std::path::PathBuf) {
    let project = deploy_project(label, name);
    let pids = project.dir.with_extension("pids");
    project.write("src/db/schema.ts", &schema_starting_a_child(&pids, true));
    commit_all(&project.dir, "a schema that never finishes");
    (project, pids)
}

fn assert_not_created(vm: &Vm, name: &str, context: &str) {
    assert_eq!(
        vm.api_as(&dana()).get_application(name).status,
        404,
        "{context}: expected nothing sent (no Application {name:?} created)"
    );
}

#[test]
fn past_its_timeout_it_ends_deploy_timed_out_leaving_nothing_running_and_nothing_sent() {
    let vm = vm();
    let machine = signed_in(&vm);
    let tmp = own_tmpdir(&machine);
    let (project, pids) = hanging("deploy-timed-out", "slow-board");
    let _cleanup = KillOnDrop(pids.clone());
    let before = project_tree(&project.dir);
    let run = deploy_with_env(
        &machine,
        &project.dir,
        &["--timeout", "3"],
        &[("TMPDIR", tmp.to_str().unwrap())],
    );
    run.json_error(1, "deploy-timed-out");
    assert!(
        run.elapsed < Duration::from_secs(20),
        "expected it to end soon after its 3 s, took {:?}",
        run.elapsed
    );
    let started = pids_in(&pids);
    assert!(
        started.len() >= 2,
        "the harness's own check: expected the schema run by drizzle-kit, recording itself and its child, found {started:?}\n{}",
        run.describe()
    );
    let running = still_running_after(&started, Duration::from_secs(1));
    assert!(
        running.is_empty(),
        "expected nothing it started, nor what that started, still running; running: {running:?}"
    );
    assert_eq!(
        before.differences(&project_tree(&project.dir)),
        Vec::<String>::new()
    );
    assert_eq!(listed(&tmp), Vec::<String>::new(), "own folder removed");
    assert_not_created(&vm, "slow-board", "out of time");
    drop(fs::remove_file(&pids));
}

#[test]
fn an_interrupted_deploy_stops_what_it_started_and_ends_interrupted() {
    let vm = vm();
    let machine = signed_in(&vm);
    let tmp = own_tmpdir(&machine);
    for signal in ["TERM", "INT", "HUP"] {
        let name = format!("interrupted-{}", signal.to_lowercase());
        let (project, pids) = hanging(&format!("deploy-{name}"), &name);
        let _cleanup = KillOnDrop(pids.clone());
        let before = project_tree(&project.dir);
        let path = tool_path(&machine);
        let running = machine.spawn_into_files(
            &machine.cwd(),
            &deploy_args(project.dir.to_str().unwrap(), &[]),
            &[("PATH", &path), ("TMPDIR", tmp.to_str().unwrap())],
        );
        let started = wait_for_pids(&pids, 2, Duration::from_secs(60));
        if started.len() < 2 {
            let run = running.wait(Duration::from_secs(5));
            panic!(
                "{signal}: expected the deploy's check to run the schema, which records itself and its child; found {started:?}\n{}",
                run.describe()
            );
        }
        let sent = Instant::now();
        running.signal(signal);
        let run = running.wait(Duration::from_secs(30));
        let waited = sent.elapsed();
        run.json_error(1, "interrupted");
        assert!(
            waited < Duration::from_secs(10),
            "{signal}: expected it to end within a few seconds of the signal, took {waited:?}"
        );
        let left = still_running_after(&started, Duration::from_secs(1));
        assert!(left.is_empty(), "{signal}: still running: {left:?}");
        assert_eq!(
            before.differences(&project_tree(&project.dir)),
            Vec::<String>::new(),
            "{signal}"
        );
        assert_eq!(
            listed(&tmp),
            Vec::<String>::new(),
            "{signal}: own folder removed"
        );
        assert_not_created(&vm, &name, signal);
        drop(fs::remove_file(&pids));
    }
}

#[test]
fn it_writes_in_a_folder_of_its_own_in_tmpdir_and_removes_it_in_every_ending() {
    let vm = vm();
    let machine = signed_in(&vm);
    let tmp = own_tmpdir(&machine);
    let tmp_text = tmp.to_str().unwrap().to_owned();

    // While it runs: one folder, mode 0700.
    let (project, pids) = hanging("deploy-own-folder", "folder-board");
    let _cleanup = KillOnDrop(pids.clone());
    let path = tool_path(&machine);
    let running = machine.spawn_into_files(
        &machine.cwd(),
        &deploy_args(project.dir.to_str().unwrap(), &["--timeout", "20"]),
        &[("PATH", &path), ("TMPDIR", &tmp_text)],
    );
    let started = wait_for_pids(&pids, 2, Duration::from_secs(60));
    if started.len() < 2 {
        let run = running.wait(Duration::from_secs(5));
        panic!(
            "expected the schema run; found {started:?}\n{}",
            run.describe()
        );
    }
    let inside = listed(&tmp);
    assert_eq!(
        inside.len(),
        1,
        "expected one folder of its own in TMPDIR while it runs, got {inside:?}"
    );
    let meta = fs::symlink_metadata(tmp.join(&inside[0])).unwrap();
    assert!(meta.is_dir(), "expected a folder, got {meta:?}");
    assert_eq!(meta.permissions().mode() & 0o7777, 0o700, "its mode");
    running.signal("TERM");
    running
        .wait(Duration::from_secs(30))
        .json_error(1, "interrupted");
    assert_eq!(listed(&tmp), Vec::<String>::new(), "interrupted");
    drop(fs::remove_file(&pids));

    let env = [("TMPDIR", tmp_text.as_str())];
    let ending = |what: &str, run: &Run| {
        assert!(run.code.is_some(), "{what}: {}", run.describe());
        assert_eq!(
            listed(&tmp),
            Vec::<String>::new(),
            "{what}: expected it removed"
        );
    };
    // Deployed.
    let project = deploy_project("deploy-own-folder-ok", "folder-ok-board");
    let run = deploy_with_env(&machine, &project.dir, &[], &env);
    Deployed::of(&run);
    ending("deployed", &run);
    // A check that fails.
    project.write("src/wrong.ts", &type_errors(1));
    commit_all(&project.dir, "a type error");
    let run = deploy_with_env(&machine, &project.dir, &[], &env);
    run.json_error(1, "check-failed");
    ending("a check that fails", &run);
    // Uncommitted changes.
    project.write("notes.txt", "untracked\n");
    let run = deploy_with_env(&machine, &project.dir, &[], &env);
    run.json_error(1, "uncommitted-changes");
    ending("uncommitted changes", &run);
}

#[test]
fn a_timeout_outside_1_to_3600_is_a_wrong_command_line() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = deploy_project("deploy-timeout", "timeout-board");
    let before = project_tree(&project.dir);
    for timeout in [
        "0",
        "3601",
        "-1",
        "1.5",
        "abc",
        "",
        "1e3",
        "99999999999999999999",
    ] {
        let run = deploy(&machine, &project.dir, &["--timeout", timeout]);
        assert_eq!(
            run.code,
            Some(2),
            "expected --timeout {timeout:?} to be a wrong command line\n{}",
            run.describe()
        );
    }
    assert_eq!(
        before.differences(&project_tree(&project.dir)),
        Vec::<String>::new()
    );
    assert_not_created(&vm, "timeout-board", "a wrong --timeout");
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &["--timeout", "3600"]));
    assert_eq!(deployed.version, 1);
}

/// Fails unless the run ended with `2`, said why on standard error, and
/// printed on standard output at most the error object.
fn assert_wrong_command_line(run: &Run) {
    run.expect_code(2);
    assert!(!run.stderr.trim().is_empty(), "{}", run.describe());
    if !run.stdout.trim().is_empty() {
        assert!(
            run.json()["error"]["reason"].is_string(),
            "{}",
            run.describe()
        );
    }
}

#[test]
fn a_wrong_command_line_ends_with_2_and_sends_nothing() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = deploy_project("deploy-wrong-line", "wrong-line-board");
    let dir = project.dir.to_str().unwrap();
    let path = tool_path(&machine);
    let cases: [&[&str]; 6] = [
        &["deploy", "--project", dir, "extra"],
        &["deploy", "--project", dir, "--no-such-option"],
        &["deploy", "--project", dir, "--timeout"],
        &["deploy", "--project"],
        &["deploy", "--project", dir, "--allow-data-loss=yes"],
        &["deploy", "--project", dir, "--name", "x"],
    ];
    for case in cases {
        for json in [false, true] {
            let mut args = case.to_vec();
            if json {
                args.push("--json");
            }
            assert_wrong_command_line(&machine.run_into_files(
                &args,
                &[("PATH", &path)],
                DEPLOY_TIMEOUT,
            ));
        }
    }
    assert_not_created(&vm, "wrong-line-board", "a wrong command line");
}

#[test]
fn not_signed_in_is_exit_code_4_and_the_message_says_to_sign_in() {
    let machine = Machine::new();
    let project = deploy_project("deploy-not-signed-in", "unsigned-board");
    let message = deploy(&machine, &project.dir, &[]).json_error(4, "not-signed-in");
    assert!(message.contains("vm sign-in"), "{message:?}");
    let path = tool_path(&machine);
    machine
        .run_into_files(
            &["deploy", "--project", project.dir.to_str().unwrap()],
            &[("PATH", &path)],
            DEPLOY_TIMEOUT,
        )
        .text_error(4);
}

#[test]
fn without_project_the_current_directory_is_deployed() {
    let vm = vm();
    let machine = signed_in(&vm);
    let project = deploy_project("deploy-cwd", "cwd-board");
    let path = tool_path(&machine);
    let run = machine.run_into_files_in(
        &project.dir,
        &["deploy", "--json"],
        &[("PATH", &path)],
        DEPLOY_TIMEOUT,
    );
    let deployed = Deployed::of(&run);
    assert_eq!(deployed.application, "cwd-board");
}

#[test]
fn vm_deploy_whose_standard_output_or_error_is_closed_ends_without_a_crash() {
    let vm = vm();
    let machine = signed_in(&vm);
    let empty = machine.root().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let unsigned = Machine::new();
    for broken in [
        Broken::StdoutClosed,
        Broken::StdoutReaderGone,
        Broken::StderrClosed,
        Broken::BothClosed,
    ] {
        for (who, args, want) in [
            (
                &machine,
                vec!["deploy", "--project", empty.to_str().unwrap()],
                1,
            ),
            (
                &machine,
                vec!["deploy", "--project", empty.to_str().unwrap(), "--json"],
                1,
            ),
            (
                &unsigned,
                vec!["deploy", "--project", empty.to_str().unwrap()],
                1,
            ),
        ] {
            let run = who.run_broken(&args, broken);
            assert!(
                run.code == Some(want) || run.code == Some(1),
                "expected {want} or 1 with {broken:?}, got\n{}",
                run.describe()
            );
            for crash in ["panicked", "RUST_BACKTRACE", "stack backtrace"] {
                assert!(!run.stderr.contains(crash), "{}", run.describe());
            }
        }
    }
    // The control: with nothing broken, it is not-a-project (and not an
    // unknown command).
    deploy(&machine, &empty, &[]).json_error(1, "not-a-project");
}
