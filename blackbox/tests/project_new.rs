//! `vm new <name> [--directory <directory>] [--json]` (CONTRACT.md, "A
//! project: the template, creating one and checking one: Creating a
//! project"; ticket "The project template, and creating and checking a
//! project": one command creates a project from the template). What it
//! needs (nothing but itself), where it writes, what it refuses, and what
//! it prints. What the files hold is in `project_template.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::path::Path;

use blackbox::project::{Tree, help_commands, new_args, stand_ins, template_files};
use blackbox::tool::{Broken, Machine, Run, set_mode};
use serde_json::{Value, json};

/// Fails unless what `vm new --json` printed is the contract's object for
/// a project made in `dir` with this name, and the files it lists are the
/// files there.
fn assert_printed(printed: &Value, name: &str, dir: &Path) {
    let mut keys: Vec<&String> = printed.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(keys, ["files", "name", "next", "project"], "{printed}");
    assert_eq!(printed["name"], json!(name));
    let project = printed["project"].as_str().unwrap_or_default();
    assert!(
        Path::new(project).is_absolute(),
        "expected `project` to be an absolute path, got {printed}"
    );
    assert_eq!(
        fs::canonicalize(project).ok(),
        fs::canonicalize(dir).ok(),
        "expected `project` to be the directory the project was made in ({}), got {printed}",
        dir.display()
    );
    let files: Vec<String> = printed["files"]
        .as_array()
        .unwrap_or_else(|| panic!("expected `files` as an array, got {printed}"))
        .iter()
        .map(|f| f.as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        files,
        template_files(),
        "expected `files` to list every file of a new project, by path relative to it, in order of path"
    );
    assert_eq!(
        Tree::of(dir).files(),
        files,
        "expected `files` to list exactly the files that are there"
    );
    let next: Vec<&str> = printed["next"]
        .as_array()
        .unwrap_or_else(|| panic!("expected `next` as an array, got {printed}"))
        .iter()
        .map(|c| {
            c.as_str()
                .unwrap_or_else(|| panic!("expected each of `next` a command, got {printed}"))
        })
        .collect();
    assert!(
        next.first().is_some_and(|c| c.starts_with("npm install")),
        "expected the first command of `next` to be `npm install`, got {printed}"
    );
}

#[test]
fn vm_new_makes_a_missing_directory_with_its_parents_and_prints_the_project() {
    let machine = Machine::new();
    let dir = machine
        .root()
        .join("work")
        .join("deeper")
        .join("incident-notes");
    let run = machine.run(&new_args("incident-notes", Some(dir.to_str().unwrap())));
    let printed = run.ok_json();
    assert_printed(&printed, "incident-notes", &dir);
}

#[test]
fn without_directory_the_project_is_made_in_dot_slash_name() {
    let machine = Machine::new();
    let printed = machine.run(&new_args("incident-notes", None)).ok_json();
    let dir = machine.cwd().join("incident-notes");
    assert!(dir.join("manifest.ts").is_file());
    assert_printed(&printed, "incident-notes", &dir);
    let mut made: Vec<String> = fs::read_dir(machine.cwd())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    made.sort();
    assert_eq!(made, ["incident-notes"], "expected nothing else made");
}

#[test]
fn a_relative_directory_is_taken_from_the_current_one() {
    let machine = Machine::new();
    let printed = machine
        .run(&new_args("incident-notes", Some("apps/notes")))
        .ok_json();
    assert_printed(
        &printed,
        "incident-notes",
        &machine.cwd().join("apps").join("notes"),
    );
}

#[test]
fn an_empty_directory_is_used() {
    let machine = Machine::new();
    let dir = machine.root().join("empty");
    fs::create_dir_all(&dir).unwrap();
    let printed = machine
        .run(&new_args("incident-notes", Some(dir.to_str().unwrap())))
        .ok_json();
    assert_printed(&printed, "incident-notes", &dir);
}

#[test]
fn a_directory_that_is_not_empty_is_refused_and_nothing_is_written() {
    let machine = Machine::new();
    let with_file = machine.root().join("with-file");
    fs::create_dir_all(&with_file).unwrap();
    fs::write(with_file.join("notes.txt"), "the Builder's own\n").unwrap();
    let with_folder = machine.root().join("with-folder");
    fs::create_dir_all(with_folder.join(".git")).unwrap();
    for dir in [&with_file, &with_folder] {
        let before = Tree::of(machine.root());
        let message = machine
            .run(&new_args("incident-notes", Some(dir.to_str().unwrap())))
            .json_error(1, "directory-not-empty");
        assert!(
            message.contains(dir.to_str().unwrap()) || message.contains("not empty"),
            "expected the message to say which directory is not empty, got {message:?}"
        );
        assert_eq!(
            before.differences(&Tree::of(machine.root())),
            Vec::<String>::new(),
            "expected nothing written"
        );
        machine
            .run(&[
                "new",
                "incident-notes",
                "--directory",
                dir.to_str().unwrap(),
            ])
            .text_error(1);
        assert_eq!(
            before.differences(&Tree::of(machine.root())),
            Vec::<String>::new()
        );
    }
    // The default directory too.
    fs::create_dir_all(machine.cwd().join("incident-notes")).unwrap();
    fs::write(machine.cwd().join("incident-notes/keep"), "").unwrap();
    let before = Tree::of(machine.root());
    machine
        .run(&new_args("incident-notes", None))
        .json_error(1, "directory-not-empty");
    assert_eq!(
        before.differences(&Tree::of(machine.root())),
        Vec::<String>::new()
    );
}

#[test]
fn a_path_that_is_no_directory_is_refused_and_nothing_is_written() {
    let machine = Machine::new();
    let file = machine.root().join("a-file");
    fs::write(&file, "not a directory\n").unwrap();
    let before = Tree::of(machine.root());
    machine
        .run(&new_args("incident-notes", Some(file.to_str().unwrap())))
        .json_error(1, "not-a-directory");
    assert_eq!(
        before.differences(&Tree::of(machine.root())),
        Vec::<String>::new(),
        "expected nothing written"
    );
    machine
        .run(&[
            "new",
            "incident-notes",
            "--directory",
            file.to_str().unwrap(),
        ])
        .text_error(1);
}

#[test]
fn a_file_that_cannot_be_written_is_project_file_and_the_message_names_it() {
    let machine = Machine::new();
    let dir = machine.root().join("read-only");
    fs::create_dir_all(&dir).unwrap();
    set_mode(&dir, 0o500);
    let message = machine
        .run(&new_args("incident-notes", Some(dir.to_str().unwrap())))
        .json_error(1, "project-file");
    assert!(
        template_files()
            .iter()
            .any(|f| message.contains(f.as_str())),
        "expected the message to name the file that cannot be written, got {message:?}"
    );
    set_mode(&dir, 0o700);
}

#[test]
fn the_name_is_an_application_name_as_the_api_takes_one() {
    let machine = Machine::new();
    let long = "a".repeat(63);
    for name in ["a", "9lives", "incident-notes", "1-2-3", long.as_str()] {
        let dir = machine.root().join(format!("ok-{}", name.len()) + name);
        let printed = machine
            .run(&new_args(name, Some(dir.to_str().unwrap())))
            .ok_json();
        assert_eq!(printed["name"], json!(name));
    }
    let too_long = "a".repeat(64);
    let wrong = [
        "",
        "Incident",
        "-notes",
        "notes-",
        "in_notes",
        "in.notes",
        "in notes",
        "../notes",
        "notes/x",
        "/tmp/notes",
        "xn--notes",
        "ünï",
        "notes\n",
        too_long.as_str(),
    ];
    let reserved = ["api", "auth", "dashboard", "git", "admin", "www"];
    for name in wrong.iter().chain(reserved.iter()) {
        for directory in [None, Some("given")] {
            let before = Tree::of(machine.root());
            let run = machine.run(&new_args(name, directory));
            assert_eq!(
                run.code,
                Some(2),
                "expected the name {name:?} to be a wrong command line (exit code 2)\n{}",
                run.describe()
            );
            assert!(!run.stderr.trim().is_empty(), "{}", run.describe());
            assert_eq!(
                before.differences(&Tree::of(machine.root())),
                Vec::<String>::new(),
                "expected nothing written for the name {name:?}"
            );
        }
    }
    assert!(!machine.root().join("notes").exists());
}

#[test]
fn it_needs_no_sign_in_and_runs_no_npm_no_git_and_no_node() {
    let machine = Machine::new();
    let spied = machine.root().join("spied");
    fs::create_dir_all(&spied).unwrap();
    let spy = |name: &str| format!("echo \"$0 $*\" > '{}/{name}'\nexit 1", spied.display());
    let programs: Vec<(String, String)> = ["npm", "npx", "git", "node", "curl", "wget", "tar"]
        .iter()
        .map(|n| ((*n).to_owned(), spy(n)))
        .collect();
    let programs: Vec<(&str, &str)> = programs
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_str()))
        .collect();
    let bin = stand_ins(&machine.root().join("bin"), &programs);
    let dir = machine.root().join("made");
    let run = machine.run_with_env(
        &new_args("incident-notes", Some(dir.to_str().unwrap())),
        &[("PATH", bin.to_str().unwrap())],
    );
    let printed = run.ok_json();
    assert_printed(&printed, "incident-notes", &dir);
    let ran: Vec<String> = fs::read_dir(&spied)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        ran.is_empty(),
        "expected `vm new` to run nothing, it ran {ran:?}"
    );

    // With nothing at all on the PATH.
    let empty = machine.root().join("empty-bin");
    fs::create_dir_all(&empty).unwrap();
    let dir = machine.root().join("made-bare");
    machine
        .run_with_env(
            &new_args("incident-notes", Some(dir.to_str().unwrap())),
            &[("PATH", empty.to_str().unwrap())],
        )
        .ok_json();
    assert_eq!(Tree::of(&dir).files(), template_files());
}

#[test]
fn the_same_program_makes_the_same_project() {
    let machine = Machine::new();
    let one = machine.root().join("one");
    let two = machine.root().join("elsewhere").join("two");
    machine
        .run(&new_args("incident-notes", Some(one.to_str().unwrap())))
        .ok_json();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let other = Machine::new();
    other
        .run(&new_args("incident-notes", Some(two.to_str().unwrap())))
        .ok_json();
    let bytes = |dir: &Path| -> Vec<(String, Vec<u8>)> {
        Tree::of(dir)
            .files()
            .into_iter()
            .map(|f| {
                let content = fs::read(dir.join(&f)).unwrap();
                (f, content)
            })
            .collect()
    };
    let (first, second) = (bytes(&one), bytes(&two));
    assert_eq!(
        first.iter().map(|(f, _)| f).collect::<Vec<_>>(),
        second.iter().map(|(f, _)| f).collect::<Vec<_>>()
    );
    for ((file, a), (_, b)) in first.iter().zip(&second) {
        assert!(
            a == b,
            "expected {file} the same, byte for byte, in two projects of the same name made at different times in different directories"
        );
    }
}

#[test]
fn without_json_it_says_the_same_in_words() {
    let machine = Machine::new();
    let dir = machine.root().join("worded");
    let words = machine
        .run(&[
            "new",
            "incident-notes",
            "--directory",
            dir.to_str().unwrap(),
        ])
        .ok_text();
    assert!(
        serde_json::from_str::<Value>(&words).is_err(),
        "expected words, got JSON:\n{words}"
    );
    let canonical = fs::canonicalize(&dir).unwrap();
    assert!(
        words.contains(canonical.to_str().unwrap()) || words.contains(dir.to_str().unwrap()),
        "expected the words to name the project's directory, got\n{words}"
    );
    for said in ["npm install", "manifest.ts", "AGENTS.md"] {
        assert!(
            words.contains(said),
            "expected the words to say {said:?}, got\n{words}"
        );
    }
    assert_eq!(Tree::of(&dir).files(), template_files());
}

#[test]
fn vm_help_lists_new_and_check() {
    let machine = Machine::new();
    let help = machine.run(&["--help"]);
    help.expect_code(0);
    let commands = help_commands(&help.stdout);
    for command in ["new", "check"] {
        assert!(
            commands.iter().any(|c| c == command),
            "expected `vm --help` to list `{command}`, got {commands:?}\n{}",
            help.describe()
        );
    }
    for command in ["new", "check"] {
        let own = machine.run(&[command, "--help"]);
        own.expect_code(0);
        assert!(own.stdout.contains("--json"), "{}", own.describe());
    }
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
fn a_wrong_command_line_of_new_or_check_ends_with_2_and_writes_nothing() {
    let machine = Machine::new();
    let cases: [&[&str]; 9] = [
        &["new"],
        &["new", "a", "b"],
        &["new", "notes", "--no-such-option"],
        &["new", "notes", "--directory"],
        &["check", "extra"],
        &["check", "--no-such-option"],
        &["check", "--timeout"],
        &["check", "--project"],
        &["new", "--json"],
    ];
    for case in cases {
        for json in [false, true] {
            let mut args = case.to_vec();
            if json {
                args.push("--json");
            }
            assert_wrong_command_line(&machine.run(&args));
        }
    }
    assert!(
        machine.all_files().is_empty(),
        "expected nothing written, found {:?}",
        machine.all_files()
    );
}

#[test]
fn vm_new_whose_standard_output_is_closed_ends_without_a_crash() {
    let machine = Machine::new();
    for (index, broken) in [
        Broken::StdoutClosed,
        Broken::StdoutReaderGone,
        Broken::StderrClosed,
        Broken::BothClosed,
    ]
    .into_iter()
    .enumerate()
    {
        for json in [false, true] {
            let dir = machine.root().join(format!("closed-{index}-{json}"));
            let mut args = vec![
                "new",
                "incident-notes",
                "--directory",
                dir.to_str().unwrap(),
            ];
            if json {
                args.push("--json");
            }
            let run = machine.run_broken(&args, broken);
            assert!(
                run.code == Some(0) || run.code == Some(1),
                "expected 0 or 1 with {broken:?}, got\n{}",
                run.describe()
            );
            for crash in ["panicked", "RUST_BACKTRACE", "stack backtrace"] {
                assert!(!run.stderr.contains(crash), "{}", run.describe());
            }
        }
    }
    // The control: with nothing broken it ends with 0.
    let dir = machine.root().join("control");
    machine
        .run(&new_args("incident-notes", Some(dir.to_str().unwrap())))
        .ok_json();
}
