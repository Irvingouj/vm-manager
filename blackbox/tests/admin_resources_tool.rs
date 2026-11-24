//! The tool's commands for Resources (CONTRACT.md, "Resources and Grants in
//! full: The tool"): `vibe admin resources [<name>]`, `vibe admin resource
//! register` (both kinds), `logins` and `edit` (`location` and `remove` are
//! of a later pass), in words and with `--json`; passwords only from the
//! three environment variables and never printed or kept; what the tool
//! checks before it asks; the exit code of each refusal; `vibe admin --help`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::admin::request_records;
use blackbox::project::help_commands;
use blackbox::resources::{
    Evidence, admin_resource, admin_resources, not_served, reader_of_its_own, writing_login,
};
use blackbox::scenario::{READERS, Scenario, administrator};
use blackbox::sealed::DEVOPS;
use blackbox::storage::{MARKETING, Storage};
use blackbox::tool::{Machine, Run};
use blackbox::{User, Vm};
use serde_json::json;

const READ_PASSWORD: &str = "VIBE_RESOURCE_READ_PASSWORD";
const WRITE_PASSWORD: &str = "VIBE_RESOURCE_WRITE_PASSWORD";
const STRUCTURE_PASSWORD: &str = "VIBE_RESOURCE_STRUCTURE_PASSWORD";

fn signed_in(vm: &Vm, user: &User) -> Machine {
    let machine = Machine::new();
    machine.sign_in(vm, user);
    machine
}

fn admin_run(machine: &Machine, args: &[&str], env: &[(&str, &str)], json: bool) -> Run {
    let mut all = vec!["admin"];
    all.extend_from_slice(args);
    if json {
        all.push("--json");
    }
    machine.run_with_env(&all, env)
}

/// The arguments that register a database over `db` with these roles.
fn register_args(
    name: &str,
    db: &blackbox::shared_database::SharedDatabase,
    read: &str,
) -> Vec<String> {
    vec![
        "resource".to_owned(),
        "register".to_owned(),
        name.to_owned(),
        "--kind".to_owned(),
        "postgres".to_owned(),
        "--description".to_owned(),
        "Incidents of the DevOps team".to_owned(),
        "--owner".to_owned(),
        "The DevOps team".to_owned(),
        "--contact".to_owned(),
        "Dana Ops".to_owned(),
        "--host".to_owned(),
        db.host(),
        "--port".to_owned(),
        db.port().to_string(),
        "--database".to_owned(),
        db.name().to_owned(),
        "--read-role".to_owned(),
        read.to_owned(),
    ]
}

fn refs(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}

#[test]
fn vm_admin_help_names_the_resource_commands_beside_the_others() {
    let help = Machine::new().run(&["admin", "--help"]);
    help.expect_code(0);
    let listed = help_commands(&help.stdout);
    for command in [
        "connect-oidc",
        "applications",
        "disable",
        "enable",
        "operation-log",
        "resources",
        "resource",
    ] {
        assert!(
            listed.iter().any(|c| c == command),
            "expected `vibe admin --help` to name `{command}`, it names {listed:?}\n{}",
            help.describe()
        );
    }
    let resource = Machine::new().run(&["admin", "resource", "--help"]);
    resource.expect_code(0);
    let subcommands = help_commands(&resource.stdout);
    for command in ["register", "logins", "edit"] {
        assert!(
            subcommands.iter().any(|c| c == command),
            "expected `vibe admin resource --help` to name `{command}`, got {subcommands:?}\n{}",
            resource.describe()
        );
    }
    for args in [
        vec!["admin", "resources", "--help"],
        vec!["admin", "resource", "register", "--help"],
        vec!["admin", "resource", "logins", "--help"],
        vec!["admin", "resource", "edit", "--help"],
    ] {
        let run = Machine::new().run(&args);
        run.expect_code(0);
        assert!(run.stdout.contains("--json"), "{}", run.describe());
    }
    let register = Machine::new().run(&["admin", "resource", "register", "--help"]);
    for variable in [READ_PASSWORD, WRITE_PASSWORD, STRUCTURE_PASSWORD] {
        assert!(
            register.stdout.contains(variable),
            "expected the help of `register` to name `{variable}`\n{}",
            register.describe()
        );
    }
    assert!(
        !register.stdout.contains("--read-password"),
        "there is no option for a password"
    );
}

#[test]
fn each_needs_a_sign_in() {
    let machine = Machine::new();
    for args in [
        vec!["resources"],
        vec!["resources", "devops"],
        vec!["resource", "logins", "devops", "--no-write"],
        vec!["resource", "edit", "devops", "--contact", "x"],
    ] {
        admin_run(&machine, &args, &[], true).json_error(4, "not-signed-in");
    }
    let s = Scenario::start();
    let mut args = register_args("devops", &s.db, &s.db.reader.role);
    args.push("--json".to_owned());
    let run = machine.run_with_env(
        &[&["admin"][..], &refs(&args)].concat(),
        &[(READ_PASSWORD, s.db.reader.password())],
    );
    run.json_error(4, "not-signed-in");
    assert!(
        !run.stdout.contains(s.db.reader.password())
            && !run.stderr.contains(s.db.reader.password())
    );
}

#[test]
fn what_the_tool_checks_before_it_asks_is_exit_code_2_and_asks_nothing() {
    let s = Scenario::start();
    let machine = signed_in(&s.vm, &administrator());
    let dir = machine.root().to_string_lossy().into_owned();
    let env = [
        (READ_PASSWORD, "pw-read"),
        (WRITE_PASSWORD, "pw-write"),
        (STRUCTURE_PASSWORD, "pw-structure"),
    ];
    let good = |extra: &[&str]| -> Vec<String> {
        let mut args = register_args("devops", &s.db, "reader");
        args.extend(extra.iter().map(|a| (*a).to_owned()));
        args
    };
    // The commands well formed are asked and answered: what is refused
    // below is refused for what is wrong in it.
    admin_run(
        &machine,
        &["resources", "--limit", "100", "--after", "a"],
        &[],
        true,
    )
    .ok_json();
    let mut fine = register_args("fine", &s.db, &s.db.reader.role);
    fine.push("--json".to_owned());
    machine
        .run_with_env(
            &[&["admin"][..], &refs(&fine)].concat(),
            &[(READ_PASSWORD, s.db.reader.password())],
        )
        .ok_json();
    // Each is the command made wrong in one way.
    let cases: Vec<(&str, Vec<String>)> = vec![
        (
            "a name no Resource could have",
            vec!["resources".into(), "Not_A_Name".into()],
        ),
        (
            "an --after no Resource could have",
            vec!["resources".into(), "--after".into(), "Not-A-Name".into()],
        ),
        (
            "--limit 0",
            vec!["resources".into(), "--limit".into(), "0".into()],
        ),
        (
            "--limit 101",
            vec!["resources".into(), "--limit".into(), "101".into()],
        ),
        (
            "--limit text",
            vec!["resources".into(), "--limit".into(), "many".into()],
        ),
        (
            "--limit twice",
            vec![
                "resources".into(),
                "--limit".into(),
                "1".into(),
                "--limit".into(),
                "2".into(),
            ],
        ),
        (
            "--after twice",
            vec![
                "resources".into(),
                "--after".into(),
                "a".into(),
                "--after".into(),
                "b".into(),
            ],
        ),
        ("a register name no Resource could have", {
            let mut a = good(&[]);
            a[2] = "Not Valid".into();
            a
        }),
        ("a port of 0", good(&["--port", "0"])),
        ("a port of 65536", good(&["--port", "65536"])),
        ("a port that is text", good(&["--port", "five"])),
        ("a kind that is neither", {
            let mut a = good(&[]);
            a[4] = "s3".into();
            a
        }),
        (
            "--directory with a database",
            good(&["--directory", "/srv/files"]),
        ),
        (
            "--write-role given twice",
            good(&["--write-role", "a", "--write-role", "b"]),
        ),
        (
            "an option that does not exist",
            good(&["--read-password", "hunter2-not-printed"]),
        ),
        ("a database without --host", {
            let mut a = good(&[]);
            let at = a.iter().position(|x| x == "--host").unwrap();
            a.drain(at..at + 2);
            a
        }),
        ("a database without --port", {
            let mut a = good(&[]);
            let at = a.iter().position(|x| x == "--port").unwrap();
            a.drain(at..at + 2);
            a
        }),
        ("a database without --database", {
            let mut a = good(&[]);
            let at = a.iter().position(|x| x == "--database").unwrap();
            a.drain(at..at + 2);
            a
        }),
        ("a database without --read-role", {
            let mut a = good(&[]);
            let at = a.iter().position(|x| x == "--read-role").unwrap();
            a.drain(at..at + 2);
            a
        }),
        ("a register without --description", {
            let mut a = good(&[]);
            let at = a.iter().position(|x| x == "--description").unwrap();
            a.drain(at..at + 2);
            a
        }),
        ("a register without --owner", {
            let mut a = good(&[]);
            let at = a.iter().position(|x| x == "--owner").unwrap();
            a.drain(at..at + 2);
            a
        }),
        ("a register without --contact", {
            let mut a = good(&[]);
            let at = a.iter().position(|x| x == "--contact").unwrap();
            a.drain(at..at + 2);
            a
        }),
        (
            "a storage register with a relative --directory",
            storage_args("marketing", "relative/dir"),
        ),
        ("a storage register with --host", {
            let mut a = storage_args("marketing", &dir);
            a.extend(["--host".to_owned(), "db".to_owned()]);
            a
        }),
        ("a storage register with --port", {
            let mut a = storage_args("marketing", &dir);
            a.extend(["--port".to_owned(), "5432".to_owned()]);
            a
        }),
        ("a storage register with --database", {
            let mut a = storage_args("marketing", &dir);
            a.extend(["--database".to_owned(), "d".to_owned()]);
            a
        }),
        ("a storage register with --read-role", {
            let mut a = storage_args("marketing", &dir);
            a.extend(["--read-role".to_owned(), "r".to_owned()]);
            a
        }),
        ("a storage register without --directory", {
            let mut a = storage_args("marketing", &dir);
            a.truncate(a.len() - 2);
            a
        }),
        (
            "logins with no option",
            vec!["resource".into(), "logins".into(), "devops".into()],
        ),
        (
            "logins with --write-role and --no-write",
            vec![
                "resource".into(),
                "logins".into(),
                "devops".into(),
                "--write-role".into(),
                "w".into(),
                "--no-write".into(),
            ],
        ),
        (
            "logins with --structure-role and --no-structure",
            vec![
                "resource".into(),
                "logins".into(),
                "devops".into(),
                "--structure-role".into(),
                "w".into(),
                "--no-structure".into(),
            ],
        ),
        (
            "logins with a port of 70000",
            vec![
                "resource".into(),
                "logins".into(),
                "devops".into(),
                "--port".into(),
                "70000".into(),
            ],
        ),
        (
            "logins with a name no Resource could have",
            vec![
                "resource".into(),
                "logins".into(),
                "Not_A_Name".into(),
                "--no-write".into(),
            ],
        ),
        (
            "logins with an option given twice",
            vec![
                "resource".into(),
                "logins".into(),
                "devops".into(),
                "--host".into(),
                "a".into(),
                "--host".into(),
                "b".into(),
            ],
        ),
        (
            "edit with none of its three",
            vec!["resource".into(), "edit".into(), "devops".into()],
        ),
        (
            "edit with a name no Resource could have",
            vec![
                "resource".into(),
                "edit".into(),
                "Not_A_Name".into(),
                "--contact".into(),
                "x".into(),
            ],
        ),
        (
            "edit with --contact twice",
            vec![
                "resource".into(),
                "edit".into(),
                "devops".into(),
                "--contact".into(),
                "a".into(),
                "--contact".into(),
                "b".into(),
            ],
        ),
    ];
    for (what, args) in &cases {
        let before = request_records(&s.vm);
        let all: Vec<&str> = std::iter::once("admin")
            .chain(args.iter().map(String::as_str))
            .collect();
        let run = machine.run_with_env(&all, &env);
        assert_eq!(
            run.code,
            Some(2),
            "expected exit code 2 for {what}: `vibe {}`\n{}",
            all.join(" "),
            run.describe()
        );
        assert!(
            !run.stderr.trim().is_empty() && run.stdout.trim().is_empty(),
            "{what}: expected the usage text on standard error and nothing on standard output\n{}",
            run.describe()
        );
        for secret in ["hunter2-not-printed", "pw-read", "pw-write", "pw-structure"] {
            assert!(
                !run.stderr.contains(secret) && !run.stdout.contains(secret),
                "{what}: no password in what the tool printed"
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(
            request_records(&s.vm),
            before,
            "{what}: expected nothing asked of the installation"
        );
    }
    s.admin().get_resource("devops").expect_status(404);
}

fn storage_args(name: &str, directory: &str) -> Vec<String> {
    vec![
        "resource".to_owned(),
        "register".to_owned(),
        name.to_owned(),
        "--kind".to_owned(),
        "storage".to_owned(),
        "--description".to_owned(),
        "Campaign files".to_owned(),
        "--owner".to_owned(),
        "Marketing".to_owned(),
        "--contact".to_owned(),
        "Pat Files".to_owned(),
        "--directory".to_owned(),
        directory.to_owned(),
    ]
}

#[test]
fn a_role_without_its_password_variable_is_exit_code_2_naming_the_variable() {
    let s = Scenario::start();
    let db = &s.db;
    let machine = signed_in(&s.vm, &administrator());
    let before = request_records(&s.vm);
    let base = register_args("devops", db, &db.reader.role);
    let with = |extra: &[&str]| -> Vec<String> {
        let mut a = base.clone();
        a.extend(extra.iter().map(|x| (*x).to_owned()));
        a
    };
    /// What is given, what is in the environment, and the variable the
    /// message must name.
    type Case<'a> = (&'a str, Vec<String>, Vec<(&'a str, &'a str)>, &'a str);
    let cases: Vec<Case<'_>> = vec![
        ("no read variable", base.clone(), vec![], READ_PASSWORD),
        (
            "an empty read variable",
            base.clone(),
            vec![(READ_PASSWORD, "")],
            READ_PASSWORD,
        ),
        (
            "no write variable",
            with(&["--write-role", "w"]),
            vec![(READ_PASSWORD, db.reader.password())],
            WRITE_PASSWORD,
        ),
        (
            "an empty write variable",
            with(&["--write-role", "w"]),
            vec![(READ_PASSWORD, db.reader.password()), (WRITE_PASSWORD, "")],
            WRITE_PASSWORD,
        ),
        (
            "no structure variable",
            with(&["--structure-role", "s"]),
            vec![(READ_PASSWORD, db.reader.password())],
            STRUCTURE_PASSWORD,
        ),
        (
            "an empty structure variable",
            with(&["--structure-role", "s"]),
            vec![
                (READ_PASSWORD, db.reader.password()),
                (STRUCTURE_PASSWORD, ""),
            ],
            STRUCTURE_PASSWORD,
        ),
    ];
    for (what, args, env, variable) in cases {
        let run = admin_run(&machine, &refs(&args), &env, false);
        run.expect_code(2);
        assert!(
            run.stderr.contains(variable),
            "{what}: expected the message to name `{variable}`\n{}",
            run.describe()
        );
        for secret in [db.reader.password(), db.reader.role.as_str(), db.name()] {
            assert!(
                !run.stderr.contains(secret) && !run.stdout.contains(secret),
                "{what}: never a value in the message"
            );
        }
    }
    // `logins` the same: a role without its variable.
    let run = admin_run(
        &machine,
        &["resource", "logins", "devops", "--read-role", "r"],
        &[],
        false,
    );
    run.expect_code(2);
    assert!(run.stderr.contains(READ_PASSWORD), "{}", run.describe());
    let run = admin_run(
        &machine,
        &["resource", "logins", "devops", "--write-role", "w"],
        &[(READ_PASSWORD, "set-but-not-for-this")],
        false,
    );
    run.expect_code(2);
    assert!(run.stderr.contains(WRITE_PASSWORD), "{}", run.describe());
    assert_eq!(request_records(&s.vm), before, "nothing asked");
    s.admin().get_resource("devops").expect_status(404);
}

#[test]
fn passwords_come_from_the_environment_and_are_never_printed_or_kept() {
    let s = Scenario::start();
    let db = &s.db;
    let machine = signed_in(&s.vm, &administrator());
    let structure = blackbox::resources::login_holding_nothing(db);
    let mut args = register_args("devops", db, &db.reader.role);
    args.extend([
        "--write-role".to_owned(),
        db.writer.role.clone(),
        "--structure-role".to_owned(),
        structure.role.clone(),
    ]);
    let env = [
        (READ_PASSWORD, db.reader.password()),
        (WRITE_PASSWORD, db.writer.password()),
        (STRUCTURE_PASSWORD, structure.password()),
        // A variable set without its role is not looked at.
        ("VIBE_RESOURCE_UNRELATED", "x"),
    ];
    let run = admin_run(&machine, &refs(&args), &env, true);
    let printed = run.ok_json();
    assert_eq!(printed["name"], "devops", "{}", run.describe());
    assert_eq!(printed["warnings"], json!([]));
    for secret in [
        db.reader.password(),
        db.writer.password(),
        structure.password(),
        db.reader.role.as_str(),
        db.writer.role.as_str(),
        structure.role.as_str(),
        db.name(),
    ] {
        assert!(
            !run.stdout.contains(secret) && !run.stderr.contains(secret),
            "no login, database or password in what the tool printed"
        );
        assert!(
            machine.files_holding(secret).is_empty(),
            "nothing of it kept on the Builder's machine"
        );
    }
    // The registration is the API's: three logins.
    let view = admin_resource(&s.admin(), "devops").json();
    assert_eq!(
        view["logins"],
        json!({ "read": true, "write": true, "structure": true })
    );
    // A variable set for a role that was not given is not looked at, and a
    // write role later: only what is given is read.
    let second = register_args("second", db, &db.reader.role);
    admin_run(
        &machine,
        &refs(&second),
        &[
            (READ_PASSWORD, db.reader.password()),
            (WRITE_PASSWORD, ""),
            (STRUCTURE_PASSWORD, ""),
        ],
        true,
    )
    .ok_json();
    assert_eq!(
        admin_resource(&s.admin(), "second").json()["logins"],
        json!({ "read": true, "write": false, "structure": false })
    );
}

#[test]
fn register_in_words_says_it_is_registered_each_warning_and_that_no_group_has_access() {
    let s = Scenario::start();
    let db = &s.db;
    let machine = signed_in(&s.vm, &administrator());
    let careless = writing_login(db, &[Evidence::Table]);
    let args = register_args("devops", db, &careless.role);
    let run = admin_run(
        &machine,
        &refs(&args),
        &[(READ_PASSWORD, careless.password())],
        false,
    );
    let words = run.ok_text();
    let lower = words.to_lowercase();
    assert!(
        lower.contains("registered") && lower.contains("connect"),
        "expected words that it is registered and every login connected\n{}",
        run.describe()
    );
    assert!(
        words.lines().any(
            |l| l.starts_with("warning: ") && l.to_lowercase().contains("read login can write")
        ),
        "expected `warning: the read login can write (a table)` and the like, on a line of its own\n{}",
        run.describe()
    );
    assert!(
        lower.contains("no group")
            && words.contains("vibe admin grant devops")
            && words.contains("--group")
            && words.contains("--access"),
        "expected that no Group has access until a Grant is given, with the command `vibe admin grant devops --group <group> --access <level>`\n{}",
        run.describe()
    );
    for secret in [careless.password(), careless.role.as_str(), db.name()] {
        assert!(!words.contains(secret) && !run.stderr.contains(secret));
    }
    // A clean registration has no warning line.
    let clean = register_args("clean", db, &db.reader.role);
    let words = admin_run(
        &machine,
        &refs(&clean),
        &[(READ_PASSWORD, db.reader.password())],
        false,
    )
    .ok_text();
    assert!(!words.lines().any(|l| l.starts_with("warning:")), "{words}");
    // The same with --json: the API's answer, the Resource and `warnings`.
    let json_args = register_args("devops2", db, &careless.role);
    let printed = admin_run(
        &machine,
        &refs(&json_args),
        &[(READ_PASSWORD, careless.password())],
        true,
    )
    .ok_json();
    assert_eq!(printed["name"], "devops2");
    assert_eq!(printed["warnings"][0]["reason"], "read-login-can-write");
    assert_eq!(printed["warnings"][0]["evidence"], json!(["table"]));
}

#[test]
fn a_login_that_failed_is_a_line_each_and_exit_code_3() {
    let s = Scenario::start();
    let db = &s.db;
    let machine = signed_in(&s.vm, &administrator());
    let mut args = register_args("devops", db, &db.reader.role);
    args.extend(["--write-role".to_owned(), db.writer.role.clone()]);
    let env = [
        (READ_PASSWORD, "wrong-read"),
        (WRITE_PASSWORD, "wrong-write"),
    ];
    let run = admin_run(&machine, &refs(&args), &env, false);
    let said = run.text_error(3);
    let lower = said.to_lowercase();
    assert!(
        lower.contains("read") && lower.contains("write") && !lower.contains("structure"),
        "expected one line for each login that failed\n{}",
        run.describe()
    );
    assert!(
        said.lines()
            .filter(
                |l| l.to_lowercase().contains("refused") || l.to_lowercase().contains("password")
            )
            .count()
            >= 2,
        "expected each line to say why\n{said}"
    );
    for secret in [
        "wrong-read",
        "wrong-write",
        db.reader.role.as_str(),
        db.name(),
    ] {
        assert!(!said.contains(secret) && !run.stdout.contains(secret));
    }
    let run = admin_run(&machine, &refs(&args), &env, true);
    run.json_error(3, "login-failed");
    s.admin().get_resource("devops").expect_status(404);
    // Other refusals of the API end with 3 too.
    let ok = register_args("devops", db, &db.reader.role);
    admin_run(
        &machine,
        &refs(&ok),
        &[(READ_PASSWORD, db.reader.password())],
        true,
    )
    .ok_json();
    admin_run(
        &machine,
        &refs(&ok),
        &[(READ_PASSWORD, db.reader.password())],
        true,
    )
    .json_error(3, "name-taken");
    admin_run(
        &machine,
        &["resource", "edit", "ghost", "--contact", "x"],
        &[],
        true,
    )
    .json_error(3, "not-found");
    admin_run(
        &machine,
        &["resource", "logins", "ghost", "--no-write"],
        &[],
        true,
    )
    .json_error(3, "not-found");
    // Someone who is not an administrator.
    let alice = signed_in(&s.vm, &User::new("alice").in_groups(&["staff"]));
    admin_run(&alice, &["resources"], &[], true).json_error(3, "not-an-administrator");
    admin_run(
        &alice,
        &["resource", "edit", "devops", "--contact", "x"],
        &[],
        true,
    )
    .json_error(3, "not-an-administrator");
    admin_run(
        &alice,
        &refs(&ok),
        &[(READ_PASSWORD, db.reader.password())],
        true,
    )
    .json_error(3, "not-an-administrator");
}

#[test]
fn register_a_storage_resource_with_a_directory() {
    let s = Storage::start();
    let dir = s.dir(MARKETING);
    std::fs::create_dir_all(&dir).unwrap();
    let machine = signed_in(&s.vm, &administrator());
    let args = storage_args(MARKETING, &dir.to_string_lossy());
    let printed = admin_run(&machine, &refs(&args), &[], true).ok_json();
    assert_eq!(printed["kind"], "storage", "{printed}");
    assert_eq!(printed["warnings"], json!([]));
    let again = admin_run(&machine, &refs(&args), &[], true);
    again.json_error(3, "name-taken");
    // Words, for another directory.
    let other = s.dir("other");
    std::fs::create_dir_all(&other).unwrap();
    let words = admin_run(
        &machine,
        &refs(&storage_args("other", &other.to_string_lossy())),
        &[],
        false,
    )
    .ok_text();
    assert!(words.to_lowercase().contains("registered"), "{words}");
    assert!(
        !words.contains(&*other.to_string_lossy()),
        "the directory is not printed back by a registration"
    );
    // A directory that is not there is the registration's refusal, `3`.
    let gone = s.dir("gone");
    admin_run(
        &machine,
        &refs(&storage_args("gone", &gone.to_string_lossy())),
        &[],
        true,
    )
    .json_error(3, "directory-missing");
}

#[test]
fn resources_prints_the_apis_page_and_in_words_one_block_each() {
    let s = Scenario::start();
    let db = &s.db;
    let structure = blackbox::resources::login_holding_nothing(db);
    s.register_body(
        "alpha",
        &blackbox::resources::body(
            "alpha",
            &blackbox::resources::connection(db, &db.reader, Some(&db.writer), Some(&structure)),
        ),
    );
    s.register("beta");
    s.register("gamma");
    s.grant("alpha", READERS, "read");
    s.grant("alpha", "ops", "write");
    for n in 0..201 {
        s.grant("beta", &format!("group{n:03}"), "read");
    }
    let machine = signed_in(&s.vm, &administrator());
    let api = s.admin();
    let printed = admin_run(&machine, &["resources"], &[], true).ok_json();
    assert_eq!(
        printed,
        admin_resources(&api, "").json(),
        "--json: the API's answer as it is"
    );
    let one = admin_run(&machine, &["resources", "alpha"], &[], true).ok_json();
    assert_eq!(one, admin_resource(&api, "alpha").json());
    let words = admin_run(&machine, &["resources"], &[], false).ok_text();
    assert!(
        words.contains("alpha (postgres)") && words.contains("beta (postgres)"),
        "{words}"
    );
    assert!(
        words.contains("The alpha team's incident database") || words.contains("alpha"),
        "{words}"
    );
    assert!(words.contains("The DevOps team"), "the owner: {words}");
    assert!(
        words.contains(blackbox::scenario::CONTACT),
        "whom to ask: {words}"
    );
    assert!(words.contains("logins: read, write, structure"), "{words}");
    assert!(words.contains("logins: read"), "{words}");
    for line in [format!("{READERS}  read"), "ops  write".to_owned()] {
        assert!(
            words.lines().any(|l| l.trim() == line),
            "expected a Grant on a line `<group>  <access>`: {line:?}\n{words}"
        );
    }
    assert!(
        words.to_lowercase().contains("1 more") || words.contains("1 more"),
        "expected how many more Grants when `grants_more` is not 0\n{words}"
    );
    assert!(
        words.to_lowercase().contains("no group"),
        "expected that no Group holds a Grant where none does (gamma has none)\n{words}"
    );
    for secret in [
        db.name(),
        db.reader.role.as_str(),
        db.reader.password(),
        db.writer.role.as_str(),
    ] {
        assert!(
            !words.contains(secret),
            "never a login, a host or a password"
        );
    }
    // A page of one and the line with the whole command for the next.
    let page = admin_run(&machine, &["resources", "--limit", "1"], &[], false).ok_text();
    assert!(
        page.contains("vibe admin resources --after alpha"),
        "expected a last line giving the whole command with `--after`\n{page}"
    );
    let last = admin_run(&machine, &["resources", "--after", "alpha"], &[], false).ok_text();
    assert!(
        last.contains("beta") && last.contains("gamma") && !last.contains("--after"),
        "no line for a next page when there is none\n{last}"
    );
    // The words of the single Resource.
    let single = admin_run(&machine, &["resources", "beta"], &[], false).ok_text();
    assert!(
        single.contains("beta (postgres)") && !single.contains("alpha"),
        "{single}"
    );
}

#[test]
fn with_no_resource_it_says_none_is_registered_and_how() {
    let s = Scenario::start();
    let machine = signed_in(&s.vm, &administrator());
    let printed = admin_run(&machine, &["resources"], &[], true).ok_json();
    assert_eq!(printed, json!({ "resources": [], "next": null }));
    let words = admin_run(&machine, &["resources"], &[], false).ok_text();
    assert!(
        words.to_lowercase().contains("no resource")
            && words.contains("vibe admin resource register"),
        "{words}"
    );
    admin_run(&machine, &["resources", "ghost"], &[], true).json_error(3, "not-found");
}

#[test]
fn a_resource_that_is_not_served_says_what_to_do() {
    for (problem, remedy) in [
        ("no-secret-key", "VIBE_SECRET_KEY"),
        ("does-not-open", "vibe admin resource logins"),
        ("not-sealed", "vibe admin resource logins"),
    ] {
        let s = not_served(problem);
        let machine = signed_in(&s.vm, &administrator());
        let began = std::time::Instant::now();
        let words = loop {
            let run = admin_run(&machine, &["resources", DEVOPS], &[], false);
            if run.code == Some(0) {
                break run.stdout;
            }
            // While the Resource's Grants are not yet known the answer is
            // `1` (`unavailable`); it settles within seconds.
            assert!(
                run.code == Some(1) && began.elapsed() < std::time::Duration::from_secs(20),
                "{problem}: expected the Resource to be listed\n{}",
                run.describe()
            );
            std::thread::sleep(std::time::Duration::from_millis(500));
        };
        assert!(
            words.to_lowercase().contains("not usable")
                || words.to_lowercase().contains("not served"),
            "{problem}: expected that it is not usable\n{words}"
        );
        assert!(words.contains(problem), "{problem}: expected why\n{words}");
        assert!(
            words.contains(remedy),
            "{problem}: expected what to do (`{remedy}`)\n{words}"
        );
        assert!(
            !words.contains("logins: read"),
            "{problem}: its logins cannot be read\n{words}"
        );
    }
}

#[test]
fn logins_replaces_prints_the_apis_answer_and_in_words_what_follows() {
    let s = Scenario::ready();
    let db = &s.db;
    let machine = signed_in(&s.vm, &administrator());
    let new_read = reader_of_its_own(db);
    let careless = writing_login(db, &[Evidence::Schema]);
    // --json: the API's answer (the Resource and `warnings`).
    let printed = admin_run(
        &machine,
        &[
            "resource",
            "logins",
            "devops",
            "--read-role",
            &new_read.role,
        ],
        &[(READ_PASSWORD, new_read.password())],
        true,
    )
    .ok_json();
    assert_eq!(printed["name"], "devops");
    assert_eq!(printed["warnings"], json!([]));
    assert_eq!(
        printed["logins"],
        admin_resource(&s.admin(), "devops").json()["logins"]
    );
    // In words, with a warning.
    let run = admin_run(
        &machine,
        &[
            "resource",
            "logins",
            "devops",
            "--read-role",
            &careless.role,
        ],
        &[(READ_PASSWORD, careless.password())],
        false,
    );
    let words = run.ok_text();
    let lower = words.to_lowercase();
    assert!(
        words.lines().any(|l| l.starts_with("warning: ")),
        "the warnings are said as `register` says them\n{words}"
    );
    assert!(
        lower.contains("next operation") && lower.contains("every application"),
        "expected that the new logins hold from the next operation on, in every Application\n{words}"
    );
    assert!(
        lower.contains("not deployed")
            || lower.contains("no application is deployed")
            || lower.contains("deployed again"),
        "expected that no Application is deployed again\n{words}"
    );
    // The removal flags.
    let removed = admin_run(
        &machine,
        &[
            "resource",
            "logins",
            "devops",
            "--no-write",
            "--no-structure",
        ],
        &[],
        true,
    )
    .ok_json();
    assert_eq!(
        removed["logins"],
        json!({ "read": true, "write": false, "structure": false })
    );
    // An address change: every login is connected, and a wrong one is `3`.
    let wrong = admin_run(
        &machine,
        &[
            "resource",
            "logins",
            "devops",
            "--database",
            "no_such_database_x",
        ],
        &[],
        true,
    );
    wrong.json_error(3, "login-failed");
    for secret in [careless.password(), new_read.password(), db.name()] {
        assert!(machine.files_holding(secret).is_empty());
    }
}

#[test]
fn logins_for_a_resource_that_was_not_served_says_it_is_served_now() {
    let s = not_served("does-not-open");
    let db = &s.db;
    let machine = signed_in(&s.vm, &administrator());
    let port = db.port().to_string();
    let args = [
        "resource",
        "logins",
        DEVOPS,
        "--host",
        &db.host(),
        "--port",
        &port,
        "--database",
        db.name(),
        "--read-role",
        &db.reader.role,
    ];
    let words = admin_run(
        &machine,
        &args,
        &[(READ_PASSWORD, db.reader.password())],
        false,
    )
    .ok_text();
    assert!(
        words.to_lowercase().contains("served now") || words.to_lowercase().contains("is served"),
        "expected that the Resource is served now\n{words}"
    );
    let (_, read) = s.read_devops();
    assert_eq!(read["ok"]["rows"], json!([[1]]), "{read}");
    // And without the key: the refusal is `3`, `no-secret-key`.
    let keyless = not_served("no-secret-key");
    let machine = signed_in(&keyless.vm, &administrator());
    let args = ["resource", "logins", DEVOPS, "--no-write"];
    admin_run(&machine, &args, &[], true).json_error(3, "no-secret-key");
    // A connection that is not whole: `connection-incomplete`.
    let partial = not_served("does-not-open");
    let machine = signed_in(&partial.vm, &administrator());
    admin_run(
        &machine,
        &[
            "resource",
            "logins",
            DEVOPS,
            "--read-role",
            &partial.db.reader.role,
        ],
        &[(READ_PASSWORD, partial.db.reader.password())],
        true,
    )
    .json_error(3, "connection-incomplete");
}

#[test]
fn edit_prints_the_resource_and_in_words_what_changed() {
    let s = Scenario::ready();
    let machine = signed_in(&s.vm, &administrator());
    let printed = admin_run(
        &machine,
        &[
            "resource",
            "edit",
            "devops",
            "--contact",
            "Sam Ops",
            "--owner",
            "The Platform team",
        ],
        &[],
        true,
    )
    .ok_json();
    assert_eq!(printed, admin_resource(&s.admin(), "devops").json());
    assert_eq!(printed["contact"], "Sam Ops");
    let words = admin_run(
        &machine,
        &[
            "resource",
            "edit",
            "devops",
            "--description",
            "A new description",
        ],
        &[],
        false,
    )
    .ok_text();
    assert!(
        words.to_lowercase().contains("description") && words.contains("devops"),
        "expected what changed\n{words}"
    );
    assert!(
        !words.contains("Sam Ops"),
        "only what changed is said\n{words}"
    );
    admin_run(
        &machine,
        &[
            "resource",
            "edit",
            "devops",
            "--contact",
            "x".repeat(300).as_str(),
        ],
        &[],
        true,
    )
    .json_error(3, "malformed");
}
