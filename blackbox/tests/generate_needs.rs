//! What `vm generate` needs, what it leaves, and how it ends (CONTRACT.md,
//! "Generating migrations: The command", "What stops it before drizzle-kit
//! is asked", "The instruction file and the check"; "A project: Checking a
//! project" for what holds as for `vm check`; "The tool: Every command").
//! It needs no sign-in and asks the installation nothing; it needs a
//! project, Node 22 or later and `drizzle-kit` installed; it writes in a
//! folder of its own in `TMPDIR` and nothing in the project unless a
//! migration is generated; it has an end, stops what it started, and ends
//! `interrupted` on a signal.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use blackbox::generate::{
    GENERATE_TIMEOUT, Generated, NOTES_WITH_TITLE_SCHEMA_TS, assert_unchanged, generate,
    generate_args, generate_in_words, generate_with_env, listed, own_tmpdir, project_with,
    wait_for_pids,
};
use blackbox::project::{
    KillOnDrop, NODE_20, NOTES_AND_TAGS_SCHEMA_TS, NOTES_RENAMED_SCHEMA_TS, NOTES_SCHEMA_TS,
    Report, Tree, check, checked_project, code_in, help_commands, new_project, pids_in,
    schema_starting_a_child, stack_line_in, stand_ins, still_running_after, write_checked,
};
use blackbox::sdk::repository_root;
use blackbox::tool::{Broken, Machine, Run};
use blackbox::{User, Vm, VmOptions};

fn utf8(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// A folder outside the repository holding the checked project and, in its
/// `node_modules`, links to the root's packages named: what resolves from
/// it is what is linked.
fn linked_project(machine: &Machine, label: &str, packages: &[&str]) -> std::path::PathBuf {
    let dir = machine.root().join(label);
    write_checked(&dir);
    for package in packages {
        let link = dir.join("node_modules").join(package);
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(repository_root().join("node_modules").join(package), &link)
            .unwrap();
    }
    dir
}

#[test]
fn a_directory_without_a_manifest_is_not_a_project_and_the_current_one_is_the_default() {
    let machine = Machine::new();
    let empty = machine.root().join("empty");
    fs::create_dir_all(&empty).unwrap();
    generate(&machine, &empty, &[]).json_error(1, "not-a-project");
    generate_in_words(&machine, &empty, &[]).text_error(1);
    // Without --project: the current directory, which has no manifest.ts.
    machine
        .run_into_files(&["generate", "--json"], &[], GENERATE_TIMEOUT)
        .json_error(1, "not-a-project");
    assert!(machine.cwd().read_dir().unwrap().next().is_none());
    assert!(empty.read_dir().unwrap().next().is_none());

    // Run in a project, without --project: that project.
    let project = project_with("needs-cwd", NOTES_SCHEMA_TS);
    project.write("src/db/schema.ts", NOTES_WITH_TITLE_SCHEMA_TS);
    let run =
        machine.run_into_files_in(&project.dir, &["generate", "--json"], &[], GENERATE_TIMEOUT);
    Generated::made(&run, &project.dir);
}

#[test]
fn no_node_on_the_path_or_one_older_than_22_is_node_missing() {
    let machine = Machine::new();
    let project = project_with("needs-node", NOTES_SCHEMA_TS);
    project.write("src/db/schema.ts", NOTES_WITH_TITLE_SCHEMA_TS);
    std::thread::sleep(Duration::from_millis(50));
    let before = Tree::of(&project.dir);
    let empty = stand_ins(&machine.root().join("no-node"), &[]);
    let old = stand_ins(&machine.root().join("node-20"), &[("node", NODE_20)]);
    for (case, path) in [("no node", &empty), ("Node 20", &old)] {
        let env = [("PATH", utf8(path))];
        let message =
            generate_with_env(&machine, &project.dir, &[], &env).json_error(1, "node-missing");
        assert!(
            message.contains("Node") && message.contains("22"),
            "{case}: expected the message to say Node 22 or later is needed, got {message:?}"
        );
        machine
            .run_into_files(
                &["generate", "--project", utf8(&project.dir)],
                &env,
                GENERATE_TIMEOUT,
            )
            .text_error(1);
    }
    assert_unchanged(&before, &project.dir, "node-missing");
}

#[test]
fn a_project_whose_drizzle_kit_does_not_resolve_is_not_installed() {
    let machine = Machine::new();
    // Outside the repository: nothing resolves. The schema has a table, so
    // there would be a migration to generate.
    let bare = linked_project(&machine, "bare", &[]);
    fs::write(bare.join("src/db/schema.ts"), NOTES_SCHEMA_TS).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let before = Tree::of(&bare);
    let message = generate(&machine, &bare, &[]).json_error(1, "not-installed");
    assert!(
        message.contains("npm install"),
        "expected the message to say to run `npm install`, got {message:?}"
    );
    generate_in_words(&machine, &bare, &[]).text_error(1);
    assert_unchanged(&before, &bare, "not-installed");
}

/// Only `drizzle-kit` is needed (and what the schema imports): with
/// `@vm/sdk` and `typescript` not resolving, it generates.
#[test]
fn only_drizzle_kit_is_needed() {
    let machine = Machine::new();
    let dir = linked_project(&machine, "only-drizzle", &["drizzle-kit", "drizzle-orm"]);
    fs::write(dir.join("src/db/schema.ts"), NOTES_SCHEMA_TS).unwrap();
    Generated::made(&generate(&machine, &dir, &[]), &dir);
}

#[test]
fn an_own_schema_that_throws_is_own_schema_failed_with_the_tools_words_and_nothing_written() {
    let machine = Machine::new();
    let tmp = own_tmpdir(&machine);
    let project = project_with("needs-schema-throws", NOTES_SCHEMA_TS);
    project.write(
        "src/db/schema.ts",
        "throw new Error(\"the schema refuses to load\");\nexport {};\n",
    );
    std::thread::sleep(Duration::from_millis(50));
    let before = Tree::of(&project.dir);
    let env = [("TMPDIR", utf8(&tmp))];
    let message =
        generate_with_env(&machine, &project.dir, &[], &env).json_error(1, "own-schema-failed");
    assert!(
        message.contains("the schema refuses to load"),
        "expected drizzle-kit's own words, got {message:?}"
    );
    assert!(
        message.contains("src/db/schema.ts") && message.to_lowercase().contains("correct"),
        "expected the message to say to correct src/db/schema.ts, got {message:?}"
    );
    assert_words_not_stack(&message, &tmp);
    let words = generate_in_words(&machine, &project.dir, &[]).text_error(1);
    assert_words_not_stack(&words, &tmp);
    assert_unchanged(&before, &project.dir, "own-schema-failed");
    assert_eq!(listed(&tmp), Vec::<String>::new(), "own folder removed");
}

#[test]
fn a_journal_that_is_not_json_is_migrations_unreadable_and_nothing_written() {
    let machine = Machine::new();
    let tmp = own_tmpdir(&machine);
    let project = project_with("needs-journal", NOTES_SCHEMA_TS);
    project.write("migrations/meta/_journal.json", "{ not json\n");
    project.write("src/db/schema.ts", NOTES_WITH_TITLE_SCHEMA_TS);
    std::thread::sleep(Duration::from_millis(50));
    let before = Tree::of(&project.dir);
    let env = [("TMPDIR", utf8(&tmp))];
    let message =
        generate_with_env(&machine, &project.dir, &[], &env).json_error(1, "migrations-unreadable");
    let lower = message.to_lowercase();
    assert!(
        message.contains("Expected property name"),
        "expected drizzle-kit's own words, got {message:?}"
    );
    assert!(
        lower.contains("by hand") && lower.contains("committed"),
        "expected the message to say migrations are never edited by hand and to bring the folder back as it was last committed, got {message:?}"
    );
    assert_words_not_stack(&message, &tmp);
    generate_in_words(&machine, &project.dir, &[]).text_error(1);
    assert_unchanged(&before, &project.dir, "migrations-unreadable");
}

/// A migration the journal names whose `.sql` file is gone: the journal and
/// the files do not agree, and the message names the file as the project's.
#[test]
fn a_migration_whose_sql_file_is_gone_is_migrations_unreadable() {
    let machine = Machine::new();
    let project = project_with("needs-sql-gone", NOTES_SCHEMA_TS);
    let gone = blackbox::project::sql_files(&project.dir).remove(0);
    fs::remove_file(project.path(&format!("migrations/{gone}"))).unwrap();
    project.write("src/db/schema.ts", NOTES_WITH_TITLE_SCHEMA_TS);
    std::thread::sleep(Duration::from_millis(50));
    let before = Tree::of(&project.dir);
    let message = generate(&machine, &project.dir, &[]).json_error(1, "migrations-unreadable");
    assert!(
        message.contains(&format!("migrations/{gone}")),
        "expected the message to name migrations/{gone}, got {message:?}"
    );
    assert_unchanged(&before, &project.dir, "migrations-unreadable");
}

/// Fails unless a message with a tool's words holds its error and not its
/// stack: no line of a stack trace and no path of the command's own folder
/// (which is in `tmp`).
fn assert_words_not_stack(message: &str, tmp: &Path) {
    assert_eq!(
        stack_line_in(message),
        None,
        "expected the tool's error without its stack, got {message:?}"
    );
    assert!(
        !message.contains(utf8(tmp)),
        "expected no path of the command's own folder, got {message:?}"
    );
}

#[test]
fn the_command_line_is_looked_at_first_then_the_project_then_node_then_the_packages() {
    let machine = Machine::new();
    let empty = machine.root().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let no_node = stand_ins(&machine.root().join("no-node"), &[]);
    let no_node = [("PATH", utf8(&no_node))];

    // The command line first: 2, on a directory that is no project.
    let run = machine.run_into_files(
        &generate_args(utf8(&empty), &["--timeout", "0"]),
        &no_node,
        GENERATE_TIMEOUT,
    );
    assert_eq!(run.code, Some(2), "{}", run.describe());
    // The project before node.
    generate_with_env(&machine, &empty, &[], &no_node).json_error(1, "not-a-project");
    // node before the packages.
    let bare = linked_project(&machine, "bare", &[]);
    generate_with_env(&machine, &bare, &[], &no_node).json_error(1, "node-missing");
    generate(&machine, &bare, &[]).json_error(1, "not-installed");
}

#[test]
fn it_needs_no_sign_in_and_asks_the_installation_nothing() {
    // Signed in to an installation that is then gone: a command that asked
    // it anything would end `unreachable`.
    let machine = Machine::new();
    {
        let vm = Vm::start(&VmOptions::new().stand_in_sign_in());
        machine.sign_in(&vm, &User::new("bea").in_groups(&["builders"]));
    }
    let project = project_with("needs-no-installation", NOTES_SCHEMA_TS);
    project.write("src/db/schema.ts", NOTES_WITH_TITLE_SCHEMA_TS);
    Generated::made(&generate(&machine, &project.dir, &[]), &project.dir);
    Generated::nothing(&generate(&machine, &project.dir, &[]));

    // And never signed in.
    let never = Machine::new();
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    generate(&never, &project.dir, &[]).json_error(3, "data-loss");
}

#[test]
fn past_its_timeout_it_ends_generate_timed_out_leaving_nothing_running_or_written() {
    let machine = Machine::new();
    let tmp = own_tmpdir(&machine);
    let project = checked_project("needs-timed-out");
    let pids = project.dir.with_extension("pids");
    let _cleanup = KillOnDrop(pids.clone());
    project.write("src/db/schema.ts", &schema_starting_a_child(&pids, true));
    std::thread::sleep(Duration::from_millis(50));
    let before = Tree::of(&project.dir);
    let run = machine.run_into_files(
        &generate_args(utf8(&project.dir), &["--timeout", "3"]),
        &[("TMPDIR", utf8(&tmp))],
        Duration::from_secs(60),
    );
    run.json_error(1, "generate-timed-out");
    assert!(
        run.elapsed < Duration::from_secs(15),
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
    assert_unchanged(&before, &project.dir, "out of time");
    assert_eq!(
        listed(&tmp),
        Vec::<String>::new(),
        "out of time: own folder removed"
    );
    drop(fs::remove_file(&pids));
}

#[test]
fn an_interrupted_generate_stops_what_it_started_and_ends_interrupted() {
    let machine = Machine::new();
    let tmp = own_tmpdir(&machine);
    for signal in ["TERM", "INT", "HUP"] {
        let project = checked_project(&format!("needs-interrupted-{signal}"));
        let pids = project.dir.with_extension("pids");
        let _cleanup = KillOnDrop(pids.clone());
        project.write("src/db/schema.ts", &schema_starting_a_child(&pids, true));
        std::thread::sleep(Duration::from_millis(50));
        let before = Tree::of(&project.dir);
        let running = machine.spawn_into_files(
            &machine.cwd(),
            &generate_args(utf8(&project.dir), &[]),
            &[("TMPDIR", utf8(&tmp))],
        );
        let started = wait_for_pids(&pids, 2, Duration::from_secs(60));
        if started.len() < 2 {
            let run = running.wait(Duration::from_secs(5));
            panic!(
                "{signal}: expected drizzle-kit to run the schema, which records itself and its child; found {started:?}\n{}",
                run.describe()
            );
        }
        let sent = std::time::Instant::now();
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
        assert_unchanged(&before, &project.dir, signal);
        assert_eq!(
            listed(&tmp),
            Vec::<String>::new(),
            "{signal}: own folder removed"
        );
        drop(fs::remove_file(&pids));
    }
}

#[test]
fn it_writes_in_a_folder_of_its_own_in_tmpdir_and_removes_it_in_every_ending() {
    let machine = Machine::new();
    let tmp = own_tmpdir(&machine);
    let env = [("TMPDIR", utf8(&tmp))];

    // While it runs: one folder, mode 0700.
    let project = checked_project("needs-own-folder");
    let pids = project.dir.with_extension("pids");
    let _cleanup = KillOnDrop(pids.clone());
    project.write("src/db/schema.ts", &schema_starting_a_child(&pids, true));
    let running = machine.spawn_into_files(
        &machine.cwd(),
        &generate_args(utf8(&project.dir), &["--timeout", "10"]),
        &env,
    );
    let started = wait_for_pids(&pids, 2, Duration::from_secs(60));
    if started.len() < 2 {
        let run = running.wait(Duration::from_secs(5));
        panic!(
            "expected drizzle-kit to run the schema, which records itself and its child; found {started:?}\n{}",
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
    running
        .wait(Duration::from_secs(60))
        .json_error(1, "generate-timed-out");
    assert_eq!(listed(&tmp), Vec::<String>::new(), "out of time");
    drop(fs::remove_file(&pids));

    // Every other ending.
    let project = project_with("needs-own-folder-endings", NOTES_AND_TAGS_SCHEMA_TS);
    let dir = &project.dir;
    let ending = |what: &str, run: &Run| {
        assert!(run.code.is_some(), "{what}: {}", run.describe());
        assert_eq!(
            listed(&tmp),
            Vec::<String>::new(),
            "{what}: expected it removed"
        );
    };
    let run = generate_with_env(&machine, dir, &[], &env);
    Generated::nothing(&run);
    ending("nothing to generate", &run);
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    let run = generate_with_env(&machine, dir, &[], &env);
    run.json_error(3, "data-loss");
    ending("data loss", &run);
    project.write(
        "src/db/schema.ts",
        &NOTES_AND_TAGS_SCHEMA_TS.replace("body: text(\"body\")", "text: text(\"text\")"),
    );
    let run = generate_with_env(&machine, dir, &[], &env);
    run.json_error(1, "rename-not-generated");
    ending("the question", &run);
    project.write("src/db/schema.ts", "throw new Error(\"no\");\n");
    let run = generate_with_env(&machine, dir, &[], &env);
    run.json_error(1, "own-schema-failed");
    ending("a failure", &run);
    project.write(
        "src/db/schema.ts",
        &format!(
            "{NOTES_AND_TAGS_SCHEMA_TS}\nexport const more = pgTable(\"more\", {{ id: serial(\"id\").primaryKey() }});\n"
        ),
    );
    let run = generate_with_env(&machine, dir, &[], &env);
    Generated::made(&run, dir);
    ending("a migration generated", &run);
}

#[test]
fn a_timeout_outside_1_to_3600_is_a_wrong_command_line() {
    let machine = Machine::new();
    let project = project_with("needs-timeout", NOTES_SCHEMA_TS);
    std::thread::sleep(Duration::from_millis(50));
    let before = Tree::of(&project.dir);
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
        let run = machine.run_into_files(
            &generate_args(utf8(&project.dir), &["--timeout", timeout]),
            &[],
            GENERATE_TIMEOUT,
        );
        assert_eq!(
            run.code,
            Some(2),
            "expected --timeout {timeout:?} to be a wrong command line\n{}",
            run.describe()
        );
    }
    assert_unchanged(&before, &project.dir, "a wrong --timeout");
    Generated::nothing(&generate(&machine, &project.dir, &["--timeout", "3600"]));
    Generated::nothing(&generate(&machine, &project.dir, &["--timeout", "1"]));
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
fn a_wrong_command_line_ends_with_2_and_writes_nothing() {
    let machine = Machine::new();
    let project = project_with("needs-wrong-line", NOTES_SCHEMA_TS);
    project.write("src/db/schema.ts", NOTES_WITH_TITLE_SCHEMA_TS);
    std::thread::sleep(Duration::from_millis(50));
    let before = Tree::of(&project.dir);
    let dir = utf8(&project.dir);
    let cases: [&[&str]; 6] = [
        &["generate", "--project", dir, "extra"],
        &["generate", "--project", dir, "--no-such-option"],
        &["generate", "--project", dir, "--timeout"],
        &["generate", "--project", dir, "--name"],
        &["generate", "--project"],
        &["generate", "--project", dir, "--allow-data-loss=yes"],
    ];
    for case in cases {
        for json in [false, true] {
            let mut args = case.to_vec();
            if json {
                args.push("--json");
            }
            assert_wrong_command_line(&machine.run_into_files(&args, &[], GENERATE_TIMEOUT));
        }
    }
    assert_unchanged(&before, &project.dir, "a wrong command line");
    // The control: the right one generates.
    Generated::made(&generate(&machine, &project.dir, &[]), &project.dir);
}

#[test]
fn vm_generate_whose_standard_output_or_error_is_closed_ends_without_a_crash() {
    let machine = Machine::new();
    let empty = machine.root().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let project = project_with("needs-closed", NOTES_SCHEMA_TS);
    let dir = utf8(&project.dir);
    for broken in [
        Broken::StdoutClosed,
        Broken::StdoutReaderGone,
        Broken::StderrClosed,
        Broken::BothClosed,
    ] {
        for (args, want) in [
            (vec!["generate", "--project", dir], 0),
            (vec!["generate", "--project", dir, "--json"], 0),
            (vec!["generate", "--project", utf8(&empty)], 1),
            (vec!["generate", "--project", utf8(&empty), "--json"], 1),
        ] {
            let run = machine.run_broken(&args, broken);
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
    // The control: with nothing broken.
    Generated::nothing(&generate(&machine, &project.dir, &[]));
}

#[test]
fn vm_help_lists_generate_and_its_own_help_names_its_options() {
    let machine = Machine::new();
    let help = machine.run(&["--help"]);
    help.expect_code(0);
    let commands = help_commands(&help.stdout);
    assert!(
        commands.iter().any(|c| c == "generate"),
        "expected `vm --help` to list `generate`, got {commands:?}"
    );
    let own = machine.run(&["generate", "--help"]);
    own.expect_code(0);
    for option in [
        "--project",
        "--name",
        "--allow-data-loss",
        "--empty",
        "--timeout",
        "--json",
    ] {
        assert!(
            own.stdout.contains(option),
            "expected {option}\n{}",
            own.describe()
        );
    }
}

#[test]
fn agents_md_names_vm_generate_and_no_longer_says_to_run_drizzle_kit_by_hand() {
    let machine = Machine::new();
    let dir = machine.root().join("new");
    new_project(&machine, "incident-notes", &dir);
    let text = fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    let code = code_in(&text);
    assert!(
        code.iter().any(|c| c
            .split_whitespace()
            .collect::<Vec<_>>()
            .starts_with(&["vm", "generate"])),
        "expected AGENTS.md to name `vm generate` in a code span\n{text}"
    );
    for piece in &code {
        assert!(
            !piece.contains("drizzle-kit generate"),
            "expected AGENTS.md no longer to tell the agent to run drizzle-kit generate by hand, it has `{piece}`"
        );
    }
    let help = machine.run(&["--help"]);
    let listed_commands = help_commands(&help.stdout);
    assert!(listed_commands.iter().any(|c| c == "generate"));
}

#[test]
fn the_checks_migrations_behind_message_gives_vm_generate() {
    let vm = Vm::start(&VmOptions::new().stand_in_sign_in());
    let machine = Machine::new();
    machine.sign_in(&vm, &User::new("bea").in_groups(&["builders"]));
    let project = project_with("needs-check-message", NOTES_SCHEMA_TS);
    project.write("src/db/schema.ts", NOTES_RENAMED_SCHEMA_TS);
    let report = Report::failed(&check(&machine, &project.dir));
    let problem = report.problem("migrations", "migrations-behind");
    let message = problem["message"].as_str().unwrap();
    assert!(
        message.contains("vm generate"),
        "expected the message to give the command `vm generate`, got {message:?}"
    );
}
