//! The command's name (CONTRACT.md, "The command's name"; ticket "The
//! tool's command is `vibe`"): the usage text and the help of the commands
//! name `vibe`, never `vm`; a new project names `vibe` and `.vibe/` only; a
//! project's `.vm/` and `vm.dev.json` from before are neither read, moved
//! nor removed; a `VM_...` variable is not read.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use blackbox::DOMAIN;
use blackbox::local::{
    Local, Mirror, OUTSIDER, Requested, Serving, archive_path, every_file, holds_postgres_programs,
    local_project,
};
use blackbox::postgres::Platform;
use blackbox::program::{EXIT_TIMEOUT, Finished, run_to_exit, vm_binary};
use blackbox::project::{help_commands, new_project_in_repository};
use blackbox::tool::{Machine, Run};
use serde_json::{Value, json};

/// The commands CONTRACT.md gives the tool, whatever `vibe --help` lists:
/// a word after `vm ` that is one of these, or one the help lists, is the
/// old name telling someone what to run.
const KNOWN_COMMANDS: [&str; 26] = [
    "serve",
    "setup",
    "admin",
    "sign-in",
    "sign-out",
    "whoami",
    "resources",
    "applications",
    "pull",
    "new",
    "check",
    "generate",
    "deploy",
    "versions",
    "go-back",
    "logs",
    "request",
    "local",
    "dev",
    "show",
    "share",
    "unshare",
    "contributors",
    "help",
    "--help",
    "--version",
];

/// The names CONTRACT.md says no output of the program has: the old
/// folder, the old file of test Users, the old log, a `VM_...` variable.
const OLD_NAMES: [&str; 4] = [".vm/", "vm.dev.json", "vm.log", "VM_"];

/// Every place in `text` that names the old command: one of [`OLD_NAMES`],
/// or the word `vm` followed by a space and a command (`commands`, or one
/// of [`KNOWN_COMMANDS`]). Each found place is given with its line.
fn old_names_in(text: &str, commands: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    for line in text.lines() {
        for old in OLD_NAMES {
            if line.contains(old) {
                found.push(format!("{old:?} in {line:?}"));
            }
        }
        for (at, _) in line.match_indices("vm ") {
            let (head, tail) = line.split_at(at);
            let before = head.chars().next_back();
            if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let word: String = tail
                .chars()
                .skip("vm ".len())
                .take_while(|c| c.is_ascii_lowercase() || *c == '-')
                .collect();
            if !word.is_empty()
                && (KNOWN_COMMANDS.contains(&word.as_str()) || commands.contains(&word))
            {
                found.push(format!("`vm {word}` in {line:?}"));
            }
        }
    }
    found
}

/// Fails unless no output of `run` names the old command.
fn assert_no_old_names(run: &Run, commands: &[String], context: &str) {
    let found: Vec<String> = [&run.stdout, &run.stderr]
        .iter()
        .flat_map(|text| old_names_in(text, commands))
        .collect();
    assert!(
        found.is_empty(),
        "{context}: expected no output naming the old command (CONTRACT.md: no output tells anyone to run `vm`, or names `.vm/`, `vm.dev.json`, `vm.log` or a variable `VM_...`), found:\n  {}\n{}",
        found.join("\n  "),
        run.describe()
    );
}

/// The harness's own check of [`old_names_in`].
#[test]
fn the_old_names_are_found_and_the_platforms_own_names_are_not() {
    let commands = vec!["frobnicate".to_owned()];
    for text in [
        "run vm pull devops first",
        "Run `vm sign-in --domain x`.",
        "(vm request / --as outsider)",
        "then vm frobnicate",
        "see .vm/local/postgres.log",
        "the Users of vm.dev.json",
        "kept in vm.log",
        "set VM_LOCAL_POSTGRES_BIN",
        "Usage: vm --help",
    ] {
        assert!(
            !old_names_in(text, &commands).is_empty(),
            "expected {text:?} to name the old command"
        );
    }
    for text in [
        "run vibe pull devops first",
        "see .vibe/local/postgres.log and vibe.dev.json, set VIBE_DATABASE_URL",
        "import from \"@vm/sdk\" (file:.vibe/packages/vm-sdk-1.0.0.tgz)",
        "the schema vm, vm.history, x-vm-request-id, data-vm-serving, VM001, /_vm/test-user",
        "the vm schema holds the history",
        "xvm pull",
    ] {
        assert_eq!(
            old_names_in(text, &commands),
            Vec::<String>::new(),
            "expected {text:?} not to name the old command"
        );
    }
}

/// `vibe --help`, which must end with `0`; the commands it lists.
fn usage(machine: &Machine) -> (Run, Vec<String>) {
    let run = machine.run(&["--help"]);
    let text = run.ok_text();
    let commands = help_commands(&text);
    (run, commands)
}

#[test]
fn the_usage_text_and_the_help_of_the_commands_name_vibe_and_never_vm() {
    let machine = Machine::new();
    let (run, commands) = usage(&machine);
    assert!(
        run.stdout.contains("vibe"),
        "expected the usage text to name the command `vibe`\n{}",
        run.describe()
    );
    assert_no_old_names(&run, &commands, "`vibe --help`");

    // The help of every command the usage lists, and of at least these,
    // which tell what to run next or name the project's folder and files.
    let mut asked: Vec<Vec<String>> = Vec::new();
    for command in ["request", "dev", "pull", "check", "serve", "new", "local"]
        .iter()
        .map(|c| (*c).to_owned())
        .chain(commands.iter().filter(|c| *c != "help").cloned())
    {
        if !asked.iter().any(|a| a == &vec![command.clone()]) {
            asked.push(vec![command]);
        }
    }
    for nested in [
        ["local", "reset"],
        ["local", "operation-log"],
        ["local", "change-history"],
        ["admin", "connect-oidc"],
        ["contributors", "add"],
    ] {
        asked.push(nested.iter().map(|w| (*w).to_owned()).collect());
    }
    for command in &asked {
        let mut args: Vec<&str> = command.iter().map(String::as_str).collect();
        args.push("--help");
        let run = machine.run(&args);
        run.expect_code(0);
        assert!(
            !run.stdout.trim().is_empty(),
            "expected the help of `vibe {}` on standard output\n{}",
            command.join(" "),
            run.describe()
        );
        assert_no_old_names(
            &run,
            &commands,
            &format!("`vibe {} --help`", command.join(" ")),
        );
    }

    // A command line that is wrong gives the usage too: exit code 2, and it
    // names `vibe`, never `vm`.
    let wrong = machine.run(&["no-such-command"]);
    wrong.expect_code(2);
    assert_no_old_names(&wrong, &commands, "`vibe no-such-command`");
}

#[test]
fn the_message_of_a_command_that_needs_a_sign_in_says_to_run_vibe_sign_in() {
    let machine = Machine::new();
    let (_, commands) = usage(&machine);
    let run = machine.run(&["whoami", "--json"]);
    let message = run.json_error(4, "not-signed-in");
    assert!(
        message.contains("vibe sign-in"),
        "expected the message to say to run `vibe sign-in`, got {message:?}"
    );
    assert_no_old_names(&run, &commands, "`vibe whoami` signed out");
}

/// The text files of a project, by path relative to it (the SDK's tarball
/// left out: it is the SDK's bytes, not a text of the template).
fn text_files_of(dir: &Path) -> BTreeMap<String, String> {
    let mut files = Vec::new();
    every_file(dir, &mut files);
    files
        .into_iter()
        .filter(|f| f.extension().is_none_or(|e| e != "tgz"))
        .map(|f| {
            let relative = f.strip_prefix(dir).unwrap().to_string_lossy().into_owned();
            let text = fs::read_to_string(&f)
                .unwrap_or_else(|e| panic!("cannot read {} as text: {e}", f.display()));
            (relative, text)
        })
        .collect()
}

#[test]
fn a_new_project_names_vibe_and_its_folder_vibe_only() {
    let machine = Machine::new();
    let (_, commands) = usage(&machine);
    let project = new_project_in_repository(&machine, "named");
    let files = text_files_of(&project.dir);
    for required in ["AGENTS.md", "package.json", ".gitignore", "vibe.dev.json"] {
        assert!(
            files.contains_key(required),
            "expected the new project to hold {required}; it holds {:?}",
            files.keys().collect::<Vec<_>>()
        );
    }
    assert!(
        !project.dir.join("vm.dev.json").exists() && !project.dir.join(".vm").exists(),
        "expected no vm.dev.json and no .vm/ in a new project; it holds {:?}",
        files.keys().collect::<Vec<_>>()
    );
    for (path, text) in &files {
        let found = old_names_in(text, &commands);
        assert!(
            found.is_empty(),
            "expected the new project's {path} to name `vibe` and `.vibe/` only, found:\n  {}\n--- {path} ---\n{text}",
            found.join("\n  ")
        );
    }
    let agents = &files["AGENTS.md"];
    for named in ["vibe pull", "vibe check", "vibe request", "vibe.dev.json"] {
        assert!(
            agents.contains(named),
            "expected AGENTS.md to name {named:?}\n{agents}"
        );
    }
    assert!(
        files["package.json"].contains("file:.vibe/packages/"),
        "expected package.json to name the SDK under .vibe/packages/\n{}",
        files["package.json"]
    );
    assert!(
        files[".gitignore"].contains(".vibe/"),
        "expected .gitignore to name the tool's folder .vibe/\n{}",
        files[".gitignore"]
    );
}

/// Every file and folder under `dir`, by path relative to it, with the
/// bytes of each file (`None` for a folder).
fn tree_of(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn walk(root: &Path, dir: &Path, found: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        let entries =
            fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
        for entry in entries {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                found.insert(relative, None);
                walk(root, &path, found);
            } else {
                found.insert(relative, Some(fs::read(&path).unwrap()));
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(dir, dir, &mut found);
    found
}

/// The `vm.dev.json` of a project from before: a test User the run must
/// not know.
fn old_dev_json() -> Value {
    json!({
        "about": "the test Users of the program when it was called vm",
        "users": [
            { "id": "olduser", "displayName": "From before", "groups": ["old"] },
            { "id": "outsider", "displayName": "Outsider (no access)", "groups": [] }
        ],
        "grants": []
    })
}

/// Writes a `.vm/` from before into the project: a `local/` folder whose
/// state is nothing a run could use (if the run took it for its local
/// state it would end `local-state`), and a `vm.dev.json`.
fn write_from_before(dir: &Path) {
    let local = dir.join(".vm").join("local");
    fs::create_dir_all(&local).unwrap();
    fs::write(local.join("state.json"), "not the state of any run {").unwrap();
    // A data folder that is a file: a run that used it could not make it.
    fs::write(local.join("postgres"), "not a data folder").unwrap();
    fs::write(local.join("vm.log"), "a log from before\n").unwrap();
    fs::create_dir_all(dir.join(".vm").join("packages")).unwrap();
    fs::write(
        dir.join(".vm").join("packages").join("vm-sdk-0.0.0.tgz"),
        b"not a tarball",
    )
    .unwrap();
    fs::write(
        dir.join("vm.dev.json"),
        serde_json::to_string_pretty(&old_dev_json()).unwrap() + "\n",
    )
    .unwrap();
}

#[test]
fn a_projects_vm_folder_and_vm_dev_json_from_before_are_neither_read_nor_moved_nor_removed() {
    let local = Local::new();
    let (_, commands) = usage(&local.machine);
    let before = local_project(&local.machine, "frombefore", &[]);
    write_from_before(&before.dir);
    let old_tree = tree_of(&before.dir.join(".vm"));
    let old_file = fs::read(before.dir.join("vm.dev.json")).unwrap();
    // The same project without them, for what a run there gives.
    let neither = local_project(&local.machine, "neither", &[]);

    let run = local.request(&before.dir, "/", OUTSIDER, &[]);
    let there = local.request(&neither.dir, "/", OUTSIDER, &[]);
    let (body, plain) = (Requested::of(&run), Requested::of(&there));
    assert_eq!(body.status(), 200, "{}", body.describe());
    assert_eq!(
        (body.status(), body.json()),
        (plain.status(), plain.json()),
        "expected the run in the project with .vm/ and vm.dev.json from before to answer as in one with neither\n{}\n{}",
        run.describe(),
        there.describe()
    );
    assert_no_old_names(&run, &commands, "the run beside .vm/ and vm.dev.json");
    assert!(
        before.dir.join(".vibe").join("local").is_dir(),
        "expected the run to make .vibe/local/ beside .vm/"
    );
    assert_eq!(
        tree_of(&before.dir.join(".vm")),
        old_tree,
        "expected .vm/ untouched, every file byte for byte, nothing added or removed"
    );
    assert_eq!(
        fs::read(before.dir.join("vm.dev.json")).unwrap(),
        old_file,
        "expected vm.dev.json untouched, byte for byte"
    );
    let log = before.dir.join(".vibe/local/vibe.log");
    let log_text = fs::read_to_string(&log)
        .unwrap_or_else(|e| panic!("expected the run's log at {}: {e}", log.display()));
    let in_log = old_names_in(&log_text, &commands);
    assert!(
        in_log.is_empty(),
        "expected no record of the run's log to name the old command, found:\n  {}",
        in_log.join("\n  ")
    );

    // The test Users of the old file are not known to the run.
    let old_user = local.request(&before.dir, "/", "olduser", &[]);
    let message = old_user.json_error(1, "no-such-test-user");
    assert!(
        message.contains(OUTSIDER),
        "expected the message to name the test Users there are (those of vibe.dev.json), got {message:?}"
    );
    assert_no_old_names(&old_user, &commands, "a run as a User of vm.dev.json");

    // Without vibe.dev.json, the old file does not stand in for it: the run
    // ends as in a project with no file of test Users at all. (CONTRACT.md
    // does not say what a run does with no vibe.dev.json, so the test does
    // not either; it asks as the old file's own User, whom a run that read
    // the old file would serve, and compares.)
    fs::remove_file(before.dir.join("vibe.dev.json")).unwrap();
    fs::remove_file(neither.dir.join("vibe.dev.json")).unwrap();
    let only_old = local.request(&before.dir, "/", "olduser", &[]);
    let none = local.request(&neither.dir, "/", "olduser", &[]);
    assert_eq!(
        (only_old.code, only_old.json()["error"]["reason"].clone()),
        (none.code, none.json()["error"]["reason"].clone()),
        "expected a project with only vm.dev.json to end as one with no file of test Users\n{}\n{}",
        only_old.describe(),
        none.describe()
    );
    assert_ne!(only_old.code, Some(0), "{}", only_old.describe());
    assert_no_old_names(&only_old, &commands, "a run with only vm.dev.json");
    assert_eq!(tree_of(&before.dir.join(".vm")), old_tree);
    assert_eq!(fs::read(before.dir.join("vm.dev.json")).unwrap(), old_file);
}

/// A folder that does not exist, for `VM_LOCAL_POSTGRES_BIN`.
fn nowhere(local: &Local) -> String {
    local
        .machine
        .root()
        .join("no-such-postgres")
        .to_string_lossy()
        .into_owned()
}

#[test]
fn vm_local_postgres_bin_is_not_read_beside_vibe_local_postgres_bin() {
    // The harness gives every local run `VIBE_LOCAL_POSTGRES_BIN` (its own
    // Postgres). With `VM_LOCAL_POSTGRES_BIN` naming a folder that does not
    // exist beside it, a program that read the old variable (instead of the
    // new one, or as well, or before it) would end `postgres-unavailable`;
    // one that does not read it runs as always.
    let local = Local::new();
    let project = local_project(&local.machine, "vmbin", &[]);
    let missing = nowhere(&local);
    let run = local.request_with_env(
        &project.dir,
        "/",
        OUTSIDER,
        &[],
        &[("VM_LOCAL_POSTGRES_BIN", &missing)],
    );
    let requested = Requested::of(&run);
    assert_eq!(requested.status(), 200, "{}", requested.describe());
    assert_eq!(requested.json(), json!({ "user": OUTSIDER }));
}

#[test]
fn vm_local_postgres_bin_alone_is_as_if_no_postgres_were_named() {
    // No `VIBE_LOCAL_POSTGRES_BIN`: the run looks in the user's own folder
    // and, finding nothing there, downloads Postgres (from the harness's
    // mirror), as a first run with neither variable does. Had it read
    // `VM_LOCAL_POSTGRES_BIN` it would have ended `postgres-unavailable`
    // and asked the mirror for nothing.
    let mirror = Mirror::start(Serving::Archive);
    let local = Local::new().without_bin().with_mirror(&mirror.base_url());
    let project = local_project(&local.machine, "vmbinalone", &[]);
    let folder = local.postgres_folder();
    assert!(!folder.exists(), "the harness's own check");
    let missing = nowhere(&local);
    let run = local.request_with_env(
        &project.dir,
        "/",
        OUTSIDER,
        &[],
        &[("VM_LOCAL_POSTGRES_BIN", &missing)],
    );
    let requested = Requested::of(&run);
    assert_eq!(requested.status(), 200, "{}", requested.describe());
    assert_eq!(
        mirror.requests(),
        vec![archive_path()],
        "expected Postgres downloaded once, as with neither variable set\n{}",
        run.describe()
    );
    assert!(
        !holds_postgres_programs(&folder).is_empty(),
        "expected Postgres's programs in the user's own folder {}",
        folder.display()
    );
}

/// Runs `vibe serve` with no `VIBE_DATABASE_URL` and these variables.
fn serve_with(env: &[(&str, &str)]) -> Finished {
    let mut command = Command::new(vm_binary());
    command
        .args([
            "serve",
            "--domain",
            DOMAIN,
            "--listen",
            "127.0.0.1:0",
            "--stand-in-sign-in",
        ])
        .env_remove("VIBE_DATABASE_URL")
        .env_remove("VM_DATABASE_URL");
    for (name, value) in env {
        command.env(name, value);
    }
    run_to_exit(command, EXIT_TIMEOUT)
        .unwrap_or_else(|problem| panic!("cannot run {}: {problem}", vm_binary().display()))
}

#[test]
fn serve_with_vm_database_url_and_no_vibe_database_url_ends_as_with_no_address() {
    // A platform database the program could use: had it read the old
    // variable it would have started and listened (and been killed at the
    // harness's timeout, with no exit code).
    let platform = Platform::fresh();
    let url = platform.url().to_owned();
    let hide = |text: &str| text.replace(&url, "<VM_DATABASE_URL's value>");
    let none = serve_with(&[]);
    let old = serve_with(&[("VM_DATABASE_URL", &url)]);
    let shown = format!(
        "with neither: {}\n{}\nwith VM_DATABASE_URL: {}\n{}",
        none.status_text(),
        none.stderr,
        old.status_text(),
        hide(&old.stderr)
    );
    assert!(
        none.status.is_some_and(|s| !s.success()),
        "the harness's own check: with no address `vibe serve` ends non-zero\n{shown}"
    );
    assert_eq!(
        old.status.map(|s| s.code()),
        none.status.map(|s| s.code()),
        "expected `vibe serve` with VM_DATABASE_URL and no VIBE_DATABASE_URL to end as with no address at all\n{shown}"
    );
    assert!(
        !old.stderr.contains("\"listening\""),
        "expected no `listening` record\n{shown}"
    );
    assert!(
        old.stderr.contains("VIBE_DATABASE_URL"),
        "expected the message to name VIBE_DATABASE_URL\n{shown}"
    );
    assert!(
        !old.stderr.contains("VM_DATABASE_URL") && !old.stdout.contains("VM_DATABASE_URL"),
        "expected no output naming the variable VM_DATABASE_URL\n{shown}"
    );
    assert!(
        !old.stderr.contains(&url) && !old.stdout.contains(&url),
        "expected the address never shown"
    );
}
