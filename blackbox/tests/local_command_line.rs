//! `vm request`'s command line, the test Users file, what is looked at
//! first, the help and the instruction file (CONTRACT.md, "The local loop:
//! one request as a test User": "The test Users file", "The command", "The
//! instruction file"; ticket "Local loop: run the Application on the
//! Builder's machine and send a request as a test User": the command has a
//! JSON form; the local Operation log and Change history can be read with
//! the tool).
//!
//! Most projects here declare no Resource: a project made by `vm new` with
//! the harness's handler (`/echo` answers the request as it came).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::path::Path;

use blackbox::deploy::path_without_node;
use blackbox::local::{
    BUILDER, Local, OUTSIDER, Requested, dev_json, local_project, outsider_user, write_dev_json,
};
use blackbox::project::{
    NODE_20, Tree, code_in, help_commands, new_project, paragraphs, squashed, stand_ins,
    vm_commands_in,
};
use blackbox::tool::Run;
use serde_json::{Value, json};

/// A project made by `vm new` (under the repository's `target/`) declaring
/// nothing, with the harness's handler.
fn echo_project(local: &Local) -> blackbox::sdk::TempProject {
    local_project(&local.machine, "echo-board", &[])
}

/// What `/echo` answered.
fn echoed(requested: &Requested) -> Value {
    assert_eq!(requested.status(), 200, "{}", requested.describe());
    requested.json()
}

fn no_local_state(dir: &Path) -> bool {
    !dir.join(".vm/local").exists()
}

#[test]
fn the_method_is_get_without_a_body_and_post_with_one_and_any_token_is_taken() {
    let local = Local::new();
    let project = echo_project(&local);
    let get = local.request_ok(&project.dir, "/echo?a=1&b=two", OUTSIDER, &[]);
    let got = echoed(&get);
    assert_eq!(got["method"], json!("GET"), "{}", get.describe());
    assert_eq!(
        got["search"],
        json!("?a=1&b=two"),
        "expected the query handed on"
    );
    assert_eq!(get.body["method"], json!("GET"));

    let post = local.request_ok(&project.dir, "/echo", OUTSIDER, &["--data", "hello there"]);
    let posted = echoed(&post);
    assert_eq!(posted["method"], json!("POST"), "{}", post.describe());
    assert_eq!(posted["body"], json!("hello there"));
    assert_eq!(post.body["method"], json!("POST"));

    let patch = local.request_ok(
        &project.dir,
        "/echo",
        OUTSIDER,
        &["--method", "PATCH", "--data", "x"],
    );
    assert_eq!(echoed(&patch)["method"], json!("PATCH"));
    let purge = local.request_ok(&project.dir, "/echo", OUTSIDER, &["--method", "PURGE"]);
    assert_eq!(
        echoed(&purge)["method"],
        json!("PURGE"),
        "expected any token taken as the method"
    );
}

#[test]
fn headers_may_be_given_several_times_and_a_data_file_is_sent_as_it_is() {
    let local = Local::new();
    let project = echo_project(&local);
    let file = local.machine.root().join("body.txt");
    let bytes = "first line\nsecond\twith a tab\n";
    fs::write(&file, bytes).unwrap();
    let requested = local.request_ok(
        &project.dir,
        "/echo",
        OUTSIDER,
        &[
            "--header",
            "x-one: a",
            "--header",
            "X-Two: b c",
            "--data-file",
            file.to_str().unwrap(),
        ],
    );
    let got = echoed(&requested);
    assert_eq!(
        got["headers"]["x-one"],
        json!("a"),
        "{}",
        requested.describe()
    );
    assert_eq!(
        got["headers"]["x-two"],
        json!("b c"),
        "{}",
        requested.describe()
    );
    assert_eq!(
        got["body"],
        json!(bytes),
        "expected the file's bytes as the body"
    );
    assert_eq!(got["method"], json!("POST"));
}

#[test]
fn a_command_line_the_tool_refuses_is_exit_code_2_and_nothing_is_made() {
    let local = Local::new();
    let project = echo_project(&local);
    let big = local.machine.root().join("big.txt");
    fs::write(&big, vec![b'a'; 1024 * 1024 + 1]).unwrap();
    let small = local.machine.root().join("small.txt");
    fs::write(&small, "small").unwrap();
    let dir = project.dir.to_str().unwrap();
    let cases: Vec<(&str, Vec<&str>)> = vec![
        (
            "a path that does not begin with /",
            vec!["request", "echo", "--as", OUTSIDER, "--project", dir],
        ),
        (
            "a header without a colon",
            vec![
                "request",
                "/echo",
                "--as",
                OUTSIDER,
                "--project",
                dir,
                "--header",
                "no colon",
            ],
        ),
        (
            "Host",
            vec![
                "request",
                "/echo",
                "--as",
                OUTSIDER,
                "--project",
                dir,
                "--header",
                "Host: elsewhere.example",
            ],
        ),
        (
            "Content-Length",
            vec![
                "request",
                "/echo",
                "--as",
                OUTSIDER,
                "--project",
                dir,
                "--header",
                "content-length: 3",
            ],
        ),
        (
            "the sign-in's own User header",
            vec![
                "request",
                "/echo",
                "--as",
                OUTSIDER,
                "--project",
                dir,
                "--header",
                "x-vm-stand-in-user: someone",
            ],
        ),
        (
            "the sign-in's own Groups header",
            vec![
                "request",
                "/echo",
                "--as",
                OUTSIDER,
                "--project",
                dir,
                "--header",
                "X-VM-Stand-In-Groups: admins",
            ],
        ),
        (
            "both --data and --data-file",
            vec![
                "request",
                "/echo",
                "--as",
                OUTSIDER,
                "--project",
                dir,
                "--data",
                "x",
                "--data-file",
                small.to_str().unwrap(),
            ],
        ),
        (
            "a body over 1 MiB",
            vec![
                "request",
                "/echo",
                "--as",
                OUTSIDER,
                "--project",
                dir,
                "--data-file",
                big.to_str().unwrap(),
            ],
        ),
        ("no --as", vec!["request", "/echo", "--project", dir]),
        (
            "a timeout of 0",
            vec![
                "request",
                "/echo",
                "--as",
                OUTSIDER,
                "--project",
                dir,
                "--timeout",
                "0",
            ],
        ),
        (
            "a timeout over 3600",
            vec![
                "request",
                "/echo",
                "--as",
                OUTSIDER,
                "--project",
                dir,
                "--timeout",
                "3601",
            ],
        ),
        (
            "a timeout that is no number",
            vec![
                "request",
                "/echo",
                "--as",
                OUTSIDER,
                "--project",
                dir,
                "--timeout",
                "soon",
            ],
        ),
    ];
    // The same command line, right, is taken: what follows is refused for
    // the one thing wrong in it.
    let right = local.request_ok(&project.dir, "/echo", OUTSIDER, &["--timeout", "3600"]);
    assert_eq!(right.status(), 200, "{}", right.describe());
    fs::remove_dir_all(project.dir.join(".vm/local")).unwrap();
    let before = Tree::of(&project.dir);
    for (case, args) in cases {
        let mut with_json = args.clone();
        with_json.push("--json");
        local.run(&with_json, &[]).expect_code(2);
        assert_eq!(
            before.differences(&Tree::of(&project.dir)),
            Vec::<String>::new(),
            "{case}: expected nothing made in the project"
        );
        assert!(no_local_state(&project.dir), "{case}");
    }
    assert!(
        !local.postgres_folder().exists() && local.processes().is_empty(),
        "expected nothing started or made for a wrong command line"
    );
}

#[test]
fn a_user_that_is_not_in_the_file_is_no_such_test_user_naming_those_that_are() {
    let local = Local::new();
    let project = echo_project(&local);
    let mut file = dev_json(&project.dir);
    file["users"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "id": "carol", "groups": ["ops"] }));
    write_dev_json(&project.dir, &file);
    let run = local.request(&project.dir, "/echo", "dana", &[]);
    let message = run.json_error(1, "no-such-test-user");
    assert!(
        message.contains(OUTSIDER) && message.contains("carol"),
        "expected the message to name the test Users there are, got {message:?}"
    );
    assert!(no_local_state(&project.dir));
    // In words: the message on standard error.
    let words = local
        .request_in_words(&project.dir, "/echo", "dana", &[])
        .text_error(1);
    assert!(words.contains("carol"), "{words}");
}

#[test]
fn what_is_looked_at_first_is_the_command_line_then_the_project_then_the_file_then_the_user() {
    let local = Local::new();
    // Not a project, and a wrong command line: the command line first.
    let empty = local.machine.root().join("empty");
    fs::create_dir_all(&empty).unwrap();
    fs::write(empty.join("vm.dev.json"), "not JSON").unwrap();
    local
        .run(
            &[
                "request",
                "no-slash",
                "--as",
                "nobody",
                "--project",
                empty.to_str().unwrap(),
                "--json",
            ],
            &[],
        )
        .expect_code(2);
    // Not a project, with a file that cannot be read: the project first.
    local
        .request(&empty, "/echo", "nobody", &[])
        .json_error(1, "not-a-project");
    // A project whose file cannot be read, and a User not in it: the file.
    let project = echo_project(&local);
    let good = fs::read_to_string(project.dir.join("vm.dev.json")).unwrap();
    fs::write(project.dir.join("vm.dev.json"), "not JSON").unwrap();
    local
        .request(&project.dir, "/echo", "nobody", &[])
        .json_error(1, "test-users-file");
    fs::write(project.dir.join("vm.dev.json"), good).unwrap();
    // A User not in the file and no node: the User first.
    let no_node = path_without_node(&local.machine);
    local
        .request_with_env(&project.dir, "/echo", "nobody", &[], &[("PATH", &no_node)])
        .json_error(1, "no-such-test-user");
    // A User in the file and no node: node.
    local
        .request_with_env(&project.dir, "/echo", OUTSIDER, &[], &[("PATH", &no_node)])
        .json_error(1, "node-missing");
    assert!(no_local_state(&project.dir));
}

#[test]
fn node_older_than_22_is_node_missing_and_packages_not_installed_is_not_installed() {
    let local = Local::new();
    // A project outside the repository: its packages are not installed.
    let dir = local.machine.root().join("uninstalled");
    new_project(&local.machine, "uninstalled", &dir);
    let old = stand_ins(&local.machine.root().join("node-20"), &[("node", NODE_20)]);
    local
        .request_with_env(&dir, "/", OUTSIDER, &[], &[("PATH", old.to_str().unwrap())])
        .json_error(1, "node-missing");
    let message = local
        .request(&dir, "/", OUTSIDER, &[])
        .json_error(1, "not-installed");
    assert!(
        message.contains("npm install"),
        "expected the message to say to run npm install, got {message:?}"
    );
    assert!(no_local_state(&dir));
}

/// A file of one User and one Grant, as `vm.dev.json` may be, to be broken
/// one way at a time.
fn good_file() -> Value {
    json!({
        "about": "test Users",
        "users": [
            outsider_user(),
            { "id": "carol", "displayName": "Carol", "email": "carol@example.com", "groups": ["ops"] }
        ],
        "grants": [ { "group": "ops", "resource": "devops", "access": "read" } ]
    })
}

#[test]
fn each_malformed_test_users_file_is_test_users_file_naming_what_is_wrong() {
    let local = Local::new();
    let project = echo_project(&local);
    let long_id = "u".repeat(257);
    let mut cases: Vec<(&str, String, &str)> = vec![
        ("not JSON", "{ \"users\": ".to_owned(), "vm.dev.json"),
        ("not an object", "[]".to_owned(), "vm.dev.json"),
    ];
    let mut broken = |case: &'static str, word: &'static str, change: &dyn Fn(&mut Value)| {
        let mut file = good_file();
        change(&mut file);
        cases.push((case, serde_json::to_string_pretty(&file).unwrap(), word));
    };
    broken("users missing", "users", &|f| {
        f.as_object_mut().unwrap().remove("users");
    });
    broken("users not a list", "users", &|f| f["users"] = json!({}));
    broken("a User without an id", "id", &|f| {
        f["users"][1].as_object_mut().unwrap().remove("id");
    });
    broken("an empty id", "id", &|f| f["users"][1]["id"] = json!(""));
    broken("an id of 257 bytes", "id", &|f| {
        f["users"][1]["id"] = json!(long_id.clone());
    });
    broken("an id with a control character", "id", &|f| {
        f["users"][1]["id"] = json!("car\u{7}ol");
    });
    broken("two Users of one id", "carol", &|f| {
        let copy = f["users"][1].clone();
        f["users"].as_array_mut().unwrap().push(copy);
    });
    broken("a User without groups", "groups", &|f| {
        f["users"][1].as_object_mut().unwrap().remove("groups");
    });
    broken("groups not a list", "groups", &|f| {
        f["users"][1]["groups"] = json!("ops");
    });
    broken("a Group that is not text", "groups", &|f| {
        f["users"][1]["groups"] = json!([5]);
    });
    broken("an empty Group", "group", &|f| {
        f["users"][1]["groups"] = json!([""]);
    });
    broken("a displayName that is not text", "displayName", &|f| {
        f["users"][1]["displayName"] = json!(5);
    });
    broken("an email that is not text", "email", &|f| {
        f["users"][1]["email"] = json!(true);
    });
    broken("grants not a list", "grants", &|f| {
        f["grants"] = json!("ops")
    });
    broken("a Grant without a group", "group", &|f| {
        f["grants"][0].as_object_mut().unwrap().remove("group");
    });
    broken("a Grant at an unknown level", "access", &|f| {
        f["grants"][0]["access"] = json!("admin");
    });
    broken("a Grant on what is no Resource's name", "resource", &|f| {
        f["grants"][0]["resource"] = json!("Not A Name");
    });
    broken("two Grants of one Group on one Resource", "ops", &|f| {
        f["grants"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "group": "ops", "resource": "devops", "access": "write" }));
    });
    // The good file itself is taken, with keys the tool does not know.
    let mut taken = good_file();
    taken["remarks"] = json!("not the tool's");
    taken["users"][1]["team"] = json!("night");
    write_dev_json(&project.dir, &taken);
    let ran = local.request_ok(&project.dir, "/", "carol", &[]);
    assert_eq!(ran.json(), json!({ "user": "carol" }), "{}", ran.describe());

    for (case, text, word) in cases {
        fs::write(project.dir.join("vm.dev.json"), &text).unwrap();
        let message = local
            .request(&project.dir, "/", OUTSIDER, &[])
            .json_error(1, "test-users-file");
        assert!(
            message.contains(word),
            "{case}: expected the message to name what is wrong ({word:?}), got {message:?}\nthe file:\n{text}"
        );
    }
}

#[test]
fn a_grant_on_a_resource_that_is_not_pulled_is_a_note_and_does_nothing() {
    let local = Local::new();
    let project = echo_project(&local);
    let mut file = dev_json(&project.dir);
    file["grants"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "group": "ops", "resource": "payroll", "access": "read" }));
    write_dev_json(&project.dir, &file);
    let requested = local.request_ok(&project.dir, "/", OUTSIDER, &[]);
    let notes = requested.notes_for("grant-without-resource");
    assert_eq!(
        notes.len(),
        1,
        "expected a note for the Grant on a Resource that is not pulled\n{}",
        requested.describe()
    );
    assert!(
        notes[0].to_string().contains("payroll"),
        "expected the note to name the Resource, got {}",
        notes[0]
    );
}

#[test]
fn the_help_lists_the_commands_and_their_options() {
    let local = Local::new();
    let help = local.machine.run(&["--help"]);
    help.expect_code(0);
    let commands = help_commands(&help.stdout);
    for command in ["request", "local"] {
        assert!(
            commands.iter().any(|c| c == command),
            "expected `vm --help` to list {command:?}, got {commands:?}"
        );
    }
    let request = local.machine.run(&["request", "--help"]);
    request.expect_code(0);
    for option in [
        "--as",
        "--method",
        "--header",
        "--data",
        "--data-file",
        "--project",
        "--timeout",
        "--json",
    ] {
        assert!(
            request.stdout.contains(option),
            "expected `vm request --help` to name {option}, got\n{}",
            request.stdout
        );
    }
    let local_help = local.machine.run(&["local", "--help"]);
    local_help.expect_code(0);
    let subcommands = help_commands(&local_help.stdout);
    for command in ["operation-log", "change-history"] {
        assert!(
            subcommands.iter().any(|c| c == command),
            "expected `vm local --help` to list {command:?}, got\n{}",
            local_help.stdout
        );
    }
    for (sub, options) in [
        (
            "operation-log",
            &["--limit", "--project", "--timeout", "--json"][..],
        ),
        (
            "change-history",
            &["--table", "--limit", "--project", "--timeout", "--json"][..],
        ),
    ] {
        let run = local.machine.run(&["local", sub, "--help"]);
        run.expect_code(0);
        for option in options {
            assert!(
                run.stdout.contains(option),
                "expected `vm local {sub} --help` to name {option}, got\n{}",
                run.stdout
            );
        }
    }
}

#[test]
fn the_instruction_file_tells_the_agent_to_try_the_application_as_builder_and_as_outsider() {
    let local = Local::new();
    let dir = local.machine.root().join("instructed");
    new_project(&local.machine, "instructed", &dir);
    let text = fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    let commands = vm_commands_in(&text);
    for command in ["request", "local"] {
        assert!(
            commands.iter().any(|c| c == command),
            "expected AGENTS.md to name `vm {command}` in code, got {commands:?}"
        );
    }
    let code = code_in(&text).join("\n");
    for named in [
        "vm request",
        "vm local operation-log",
        "vm local change-history",
    ] {
        assert!(
            squashed(&code).contains(named),
            "expected AGENTS.md to name `{named}` in code\n{text}"
        );
    }
    let flat = squashed(&text);
    assert!(
        flat.contains("--as"),
        "expected AGENTS.md to show `vm request <path> --as <user>`\n{text}"
    );
    assert!(
        paragraphs(&text).iter().any(|p| {
            let p = squashed(p);
            p.contains(BUILDER) && p.contains(OUTSIDER) && p.contains("vm request")
        }),
        "expected one paragraph to say to try the Application with vm request as builder and as outsider\n{text}"
    );
    assert!(
        paragraphs(&text).iter().any(|p| {
            let p = squashed(p);
            p.contains("vm.dev.json") && p.contains("vm pull") && p.contains("Grant")
        }),
        "expected AGENTS.md to say the Grants of vm.dev.json decide and vm pull copies the Builder's own there\n{text}"
    );
    let lower = flat.to_lowercase();
    assert!(
        lower.contains("empty") && lower.contains("stand-in"),
        "expected AGENTS.md to say the Resources are empty stand-ins with the real structure\n{text}"
    );
    assert!(
        text.lines().count() <= 120,
        "expected AGENTS.md at most 120 lines, got {}",
        text.lines().count()
    );
    // Every command it names is one `vm --help` lists.
    let help = local.machine.run(&["--help"]);
    let listed = help_commands(&help.stdout);
    for command in commands {
        assert!(
            listed.contains(&command),
            "AGENTS.md names `vm {command}`, which `vm --help` does not list"
        );
    }
}

#[test]
fn the_local_reading_commands_refuse_a_limit_outside_1_to_1000() {
    let local = Local::new();
    let project = echo_project(&local);
    let dir = project.dir.to_str().unwrap();
    for sub in ["operation-log", "change-history"] {
        for limit in ["1", "1000"] {
            local
                .run(
                    &["local", sub, "--project", dir, "--limit", limit, "--json"],
                    &[],
                )
                .ok_json();
        }
        for limit in ["0", "1001", "-1", "many"] {
            let run: Run = local.run(
                &["local", sub, "--project", dir, "--limit", limit, "--json"],
                &[],
            );
            run.expect_code(2);
        }
    }
    assert!(no_local_state(&project.dir));
}
