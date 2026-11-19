//! The credential the tool keeps (CONTRACT.md, "The tool: The credential";
//! ticket "The tool signs in, and lists Resources and Applications": the
//! credential is kept in the user's own configuration folder, readable by
//! that user only, and is never printed). Also: it is not taken from the
//! command line or the environment, it is not in the program's log or in
//! the platform database in a usable form, and a credential file that
//! cannot be read or written is `credential-file`, naming the path.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;

use blackbox::scenario::{READERS, Scenario};
use blackbox::tool::{
    ConfigHome, Machine, Run, bearer, mode_of, platform_tables_holding, set_mode, sign_in_args,
    windows_in,
};
use blackbox::{User, Vm, VmOptions};

const SIGNED_IN_COMMANDS: [&str; 3] = ["whoami", "resources", "applications"];

fn alice() -> User {
    User::new("alice").in_groups(&[READERS])
}

fn bob() -> User {
    User::new("bob").in_groups(&["bob-team"])
}

fn vm() -> Vm {
    Vm::start(&VmOptions::new().stand_in_sign_in())
}

/// Fails unless the credential folder is `0700` and its one file `0600`,
/// and returns the file.
fn assert_kept_privately(machine: &Machine) -> std::path::PathBuf {
    let dir = machine.credential_dir();
    assert!(
        dir.is_dir(),
        "expected the credential folder {} to be made, found the machine holding {:?}",
        dir.display(),
        machine.all_files()
    );
    assert_eq!(
        mode_of(&dir),
        0o700,
        "expected the folder {} made with mode 0700, got {:o}",
        dir.display(),
        mode_of(&dir)
    );
    let file = machine.credential_file();
    assert_eq!(
        mode_of(&file),
        0o600,
        "expected the credential file {} with mode 0600, got {:o}",
        file.display(),
        mode_of(&file)
    );
    file
}

#[test]
fn the_credential_is_kept_in_xdg_config_home_when_it_is_absolute() {
    let vm = vm();
    for umask in [None, Some("000"), Some("022")] {
        let mut machine = Machine::new();
        if let Some(umask) = umask {
            machine = machine.umask(umask);
        }
        machine.sign_in(&vm, &alice());
        assert_kept_privately(&machine);
        assert!(
            machine
                .credential_dir()
                .starts_with(machine.absolute_config_home()),
            "the harness's own check"
        );
        assert!(
            !machine.home().join(".config").join("vibe").exists(),
            "expected nothing under $HOME/.config/vibe when XDG_CONFIG_HOME is absolute"
        );
        machine.kept_credential(&vm);
        machine.run(&["whoami", "--json"]).ok_json();
    }
}

#[test]
fn the_credential_is_kept_in_home_config_when_xdg_config_home_is_unset_or_relative() {
    let vm = vm();
    for config_home in [
        ConfigHome::Unset,
        ConfigHome::Relative("relative-config".to_owned()),
    ] {
        let machine = Machine::with_config_home(config_home.clone()).umask("000");
        machine.sign_in(&vm, &alice());
        assert_kept_privately(&machine);
        assert!(
            machine
                .credential_dir()
                .starts_with(machine.home().join(".config")),
            "the harness's own check"
        );
        assert!(
            !machine.cwd().join("relative-config").exists(),
            "expected a relative XDG_CONFIG_HOME not to be used ({config_home:?})"
        );
        assert!(
            !machine.absolute_config_home().join("vibe").exists(),
            "the harness's own check: the absolute configuration home is not told to the tool"
        );
        machine.kept_credential(&vm);
        machine.run(&["whoami", "--json"]).ok_json();
    }
}

#[test]
fn the_file_is_written_0600_whatever_mode_it_had() {
    let vm = vm();
    for mode in [0o644, 0o666, 0o604] {
        let machine = Machine::new();
        machine.sign_in(&vm, &alice());
        let file = assert_kept_privately(&machine);
        set_mode(&file, mode);
        machine.sign_in(&vm, &bob());
        assert_eq!(
            mode_of(&machine.credential_file()),
            0o600,
            "expected the credential written with mode 0600 over a file of mode {mode:o}"
        );
        assert_eq!(machine.run(&["whoami", "--json"]).ok_json()["user"], "bob");
    }
}

#[test]
fn the_credential_is_never_printed_logged_or_kept_usable_by_the_installation() {
    let s = Scenario::start();
    s.register("devops");
    s.grant("devops", READERS, "read");
    s.vm.api_as(&alice())
        .create_application("board")
        .expect_status(201);
    let machine = Machine::new();
    let owned = sign_in_args(s.vm.address(), &alice());
    let sign_in = |json: bool| {
        let mut args: Vec<&str> = owned.iter().map(String::as_str).collect();
        if json {
            args.push("--json");
        }
        machine.run(&args)
    };
    let mut runs: Vec<Run> = Vec::new();
    let mut credentials: Vec<String> = Vec::new();

    for json in [true, false] {
        let run = sign_in(json);
        run.expect_code(0);
        runs.push(run);
        credentials.push(machine.kept_credential(&s.vm));
        for command in SIGNED_IN_COMMANDS {
            let mut args = vec![command];
            if json {
                args.push("--json");
            }
            let run = machine.run(&args);
            run.expect_code(0);
            runs.push(run);
        }
    }
    // Ended elsewhere: every command now ends with `sign-in-expired`.
    let current = credentials.last().unwrap().clone();
    bearer(&s.vm, "/v1/tool-credentials/current", &current)
        .method("DELETE")
        .send()
        .expect_status(204);
    for json in [true, false] {
        for command in SIGNED_IN_COMMANDS.iter().chain(&["sign-out"]) {
            let mut args = vec![*command];
            if json {
                args.push("--json");
            }
            runs.push(machine.run(&args));
        }
    }
    // Signed in again, then signed out.
    runs.push(sign_in(true));
    credentials.push(machine.kept_credential(&s.vm));
    runs.push(machine.run(&["sign-out"]));
    runs.push(sign_in(false));
    credentials.push(machine.kept_credential(&s.vm));
    runs.push(machine.run(&["sign-out", "--json"]));
    runs.push(machine.run(&["whoami"]));
    runs.push(machine.run(&["whoami", "--json"]));

    let mut texts: Vec<(String, String)> = runs.iter().flat_map(Run::texts).collect();
    texts.extend(
        s.vm.lines()
            .into_iter()
            .map(|l| ("the program's log".to_owned(), l.raw)),
    );
    for (n, credential) in credentials.iter().enumerate() {
        let found = windows_in(&texts, credential);
        assert!(
            found.is_empty(),
            "expected the credential of sign-in {n} printed by no command and in no line of the program's log, but:\n{}",
            found.join("\n")
        );
        let tables = platform_tables_holding(s.vm.platform(), credential);
        assert!(
            tables.is_empty(),
            "expected the platform database to hold the credential of sign-in {n} in no usable form, but its text is in {tables:?}"
        );
    }
}

/// Fails unless every command run on `machine` ends with `credential-file`
/// and a message naming `path`.
fn assert_credential_file_error(machine: &Machine, path: &std::path::Path, context: &str) {
    let path = path.display().to_string();
    for command in SIGNED_IN_COMMANDS {
        let message = machine
            .run(&[command, "--json"])
            .json_error(1, "credential-file");
        assert!(
            message.contains(&path),
            "{context}: expected the message to name the path {path:?}, got {message:?}"
        );
        let words = machine.run(&[command]).text_error(1);
        assert!(
            words.contains(&path),
            "{context}: expected the message to name the path {path:?}, got {words:?}"
        );
    }
}

#[test]
fn a_credential_file_that_cannot_be_read_is_credential_file() {
    let vm = vm();
    let machine = Machine::new();
    machine.sign_in(&vm, &alice());
    let file = machine.credential_file();

    set_mode(&file, 0o000);
    if fs::read(&file).is_ok() {
        eprintln!(
            "this test runs with the power to read a file of mode 000 (as root?): the unreadable case is not checked"
        );
    } else {
        assert_credential_file_error(&machine, &file, "a file of mode 000");
    }
    set_mode(&file, 0o600);

    fs::write(&file, b"\x00\xff\xfe{ not a credential \x01\x02 }\xc3\x28")
        .unwrap_or_else(|e| panic!("cannot write {}: {e}", file.display()));
    assert_credential_file_error(&machine, &file, "a file with garbage in it");
}

#[test]
fn a_credential_folder_that_cannot_be_made_is_credential_file() {
    let vm = vm();
    let machine = Machine::new();
    let blocked = machine.credential_dir();
    fs::write(&blocked, b"a file where the folder would be")
        .unwrap_or_else(|e| panic!("cannot write {}: {e}", blocked.display()));
    let owned = sign_in_args(vm.address(), &alice());
    let mut args: Vec<&str> = owned.iter().map(String::as_str).collect();
    let words = machine.run(&args).text_error(1);
    args.push("--json");
    let message = machine.run(&args).json_error(1, "credential-file");
    let path = blocked.display().to_string();
    for text in [&message, &words] {
        assert!(
            text.contains(&path),
            "expected the message to name the path under {path:?}, got {text:?}"
        );
    }
}

#[test]
fn the_credential_is_not_taken_from_the_environment_or_the_command_line() {
    let s = Scenario::start();
    let (bobs, _) = s.vm.api_as(&bob()).tool_credential();
    let names = [
        "VIBE_CREDENTIAL",
        "VIBE_TOOL_CREDENTIAL",
        "VIBE_TOKEN",
        "VIBE_API_TOKEN",
        "VIBE_AUTH_TOKEN",
        "VIBE_ACCESS_TOKEN",
        "VIBE_BEARER_TOKEN",
        "VIBE_SESSION",
        "TOKEN",
    ];
    let env: Vec<(&str, &str)> = names.iter().map(|n| (*n, bobs.as_str())).collect();
    let machine = Machine::new();

    // Not signed in: another User's valid credential in the environment
    // does not sign the tool in.
    for command in ["whoami", "resources", "applications"] {
        let run = machine.run_with_env(&[command, "--json"], &env);
        run.json_error(4, "not-signed-in");
        let found = windows_in(&run.texts(), &bobs);
        assert!(found.is_empty(), "{}", found.join("\n"));
    }

    // Signed in as alice: the environment and extra arguments change
    // nothing about who the tool is.
    machine.sign_in(&s.vm, &alice());
    let who = machine.run_with_env(&["whoami", "--json"], &env).ok_json();
    assert_eq!(who["user"], "alice", "{who}");
    for extra in [
        vec!["--credential", bobs.as_str()],
        vec!["--token", bobs.as_str()],
        vec!["--stand-in-user", "bob"],
        vec!["--stand-in-groups", "platform-admins"],
    ] {
        let mut args = vec!["whoami", "--json"];
        args.extend(extra.iter().copied());
        let run = machine.run(&args);
        match run.code {
            Some(2) => {}
            Some(0) => {
                let who = run.json();
                assert_eq!(
                    who["user"],
                    "alice",
                    "expected `vibe whoami {}` to stay alice, got {who}",
                    extra.first().unwrap()
                );
                assert_eq!(who["platform_administrator"], false, "{who}");
            }
            _ => panic!(
                "expected `vibe whoami` with an extra {:?} to be a wrong command line (2) or to change nothing (0), got exit code {:?}",
                extra.first().unwrap(),
                run.code
            ),
        }
    }
    // What the tool keeps is still alice's own.
    let kept = machine.kept_credential(&s.vm);
    assert_ne!(kept, bobs);
}

#[test]
fn a_sign_in_whose_credential_cannot_be_kept_leaves_no_credential_alive() {
    let vm = vm();
    let machine = Machine::new();
    let dir = machine.credential_dir();
    fs::create_dir(&dir).unwrap_or_else(|e| panic!("cannot make {}: {e}", dir.display()));
    set_mode(&dir, 0o500);
    if fs::write(dir.join("probe"), b"x").is_ok() {
        drop(fs::remove_file(dir.join("probe")));
        eprintln!(
            "this test runs with the power to write into a folder of mode 0500 (as root?): not checked"
        );
        return;
    }
    let owned = sign_in_args(vm.address(), &alice());
    let mut args: Vec<&str> = owned.iter().map(String::as_str).collect();
    args.push("--json");
    let message = machine.run(&args).json_error(1, "credential-file");
    let path = dir.display().to_string();
    assert!(
        message.contains(&path),
        "expected the message to name the path under {path:?}, got {message:?}"
    );
    args.pop();
    machine.run(&args).text_error(1);
    set_mode(&dir, 0o700);
    assert!(
        machine.credential_files().is_empty(),
        "expected nothing kept, found {:?}",
        machine.credential_files()
    );

    // What an outsider sees of it: every credential made for alice was
    // ended at the installation (each `POST /v1/tool-credentials` answered
    // `201` is followed by a `DELETE /v1/tool-credentials/current` of hers
    // answered `204`).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let records = vm.records();
        let of_alice = |method: &str, path: &str, status: u64| {
            records
                .iter()
                .filter(|r| {
                    blackbox::records::event(r) == "request"
                        && r["user"] == "alice"
                        && r["method"] == method
                        && r["path"] == path
                        && r["status"].as_u64() == Some(status)
                })
                .count()
        };
        let made = of_alice("POST", "/v1/tool-credentials", 201);
        let ended = of_alice("DELETE", "/v1/tool-credentials/current", 204);
        if made == ended {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "expected every credential made for alice ended at the installation: {made} made, {ended} ended"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // And the machine can sign in once the folder can be written.
    machine.sign_in(&vm, &alice());
}
