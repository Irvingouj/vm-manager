//! `vm pull <resource> [--project <directory>] [--json]` (CONTRACT.md,
//! "Pulling a Resource's structure: The tool"; ticket "Pull a Resource's
//! structure as a typed Drizzle definition": pulling twice changes
//! nothing; the generated files say they are generated; a Builder whose
//! Grant is structure-only can pull; a Builder with no Grant is refused,
//! the message names whom to ask and says not to build against the
//! Resource, nothing is written, and the exit code is the refused one; a
//! column type the puller cannot express is reported).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use blackbox::User;
use blackbox::scenario::READERS;
use blackbox::structure::{FIRST_LINE, STOCK, Stock, UNSUPPORTED, assert_no_grant_message, link};
use blackbox::tool::Machine;
use serde_json::{Value, json};

fn rita() -> User {
    User::new("rita").in_groups(&[READERS])
}

fn nora() -> User {
    User::new("nora").in_groups(&["elsewhere"])
}

const MANIFEST: &str = r#"import { defineManifest } from "@vm/sdk";
export default defineManifest({
  name: "stock-tool",
  description: "A project to pull into.",
  resources: { stock: { kind: "postgres", access: "read" } },
});
"#;

/// A project directory on the machine, with a `manifest.ts`.
fn project(machine: &Machine, name: &str) -> PathBuf {
    let dir = machine.root().join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("manifest.ts"), MANIFEST).unwrap();
    dir
}

/// Every file under `dir`, with its bytes and modification time.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>, SystemTime)> {
    let mut found = Vec::new();
    collect(dir, &mut found);
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

fn collect(dir: &Path, found: &mut Vec<(PathBuf, Vec<u8>, SystemTime)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, found);
        } else {
            let meta = fs::metadata(&path).unwrap();
            found.push((
                path.clone(),
                fs::read(&path).unwrap(),
                meta.modified().unwrap(),
            ));
        }
    }
}

fn names(snapshot: &[(PathBuf, Vec<u8>, SystemTime)]) -> Vec<String> {
    snapshot
        .iter()
        .map(|(p, ..)| p.display().to_string())
        .collect()
}

/// `vm pull <resource> --project <dir> [--json]`.
fn pull_args<'a>(resource: &'a str, dir: &'a str, json: bool) -> Vec<&'a str> {
    let mut args = vec!["pull", resource, "--project", dir];
    if json {
        args.push("--json");
    }
    args
}

/// Fails unless `files` lists exactly the three generated files with these
/// `changed` values (`standin.sql` since the ticket on the local loop).
fn assert_files(
    printed: &Value,
    resource: &str,
    schema_changed: bool,
    index_changed: bool,
    standin_changed: bool,
) {
    let mut files: Vec<(String, bool)> = printed["files"]
        .as_array()
        .unwrap_or_else(|| panic!("expected `files` as an array, got {printed}"))
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap_or_default().to_owned(),
                f["changed"]
                    .as_bool()
                    .unwrap_or_else(|| panic!("expected `changed` as a boolean in {f}")),
            )
        })
        .collect();
    files.sort();
    assert_eq!(
        files,
        vec![
            (format!("resources/{resource}/index.ts"), index_changed),
            (format!("resources/{resource}/schema.ts"), schema_changed),
            (format!("resources/{resource}/standin.sql"), standin_changed),
        ],
        "expected the three files, by their paths relative to the project, got {printed}"
    );
}

#[test]
fn pull_writes_the_definition_and_the_link_and_prints_what_it_did() {
    let stock = Stock::ready();
    let api_answer = stock.pull_ok(&rita(), STOCK);
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    let dir = project(&machine, "board");
    let run = machine.run(&pull_args(STOCK, dir.to_str().unwrap(), true));
    let printed = run.ok_json();
    let mut keys: Vec<&String> = printed.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "files",
            "fingerprint",
            "resource",
            "test_users",
            "unsupported",
            "your_access"
        ],
        "{printed}"
    );
    assert_eq!(printed["resource"], json!(STOCK));
    assert_eq!(printed["fingerprint"], api_answer["fingerprint"]);
    assert_eq!(printed["your_access"], json!("read"));
    assert_eq!(
        printed["unsupported"], api_answer["unsupported"],
        "expected the unsupported columns as the API answers them"
    );
    assert_files(&printed, STOCK, true, true, true);
    let schema = fs::read_to_string(dir.join("resources/stock/schema.ts")).unwrap();
    assert_eq!(
        schema,
        api_answer["schema_ts"].as_str().unwrap(),
        "expected resources/stock/schema.ts to be the definition, as answered"
    );
    assert!(schema.starts_with(FIRST_LINE));
    assert_eq!(
        fs::read_to_string(dir.join("resources/stock/index.ts")).unwrap(),
        link(STOCK, false),
        "expected resources/stock/index.ts to be the generated link of \"A project\", without the relations lines"
    );
    // Nothing of the Resource's connection, the structure login included,
    // in what was printed or written.
    let mut texts = run.texts();
    texts.extend(snapshot(&dir).into_iter().map(|(p, bytes, _)| {
        (
            p.display().to_string(),
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    }));
    blackbox::scenario::assert_none_holds(&texts, &stock.markers(), "`vm pull`");
    let written = names(&snapshot(&dir));
    assert_eq!(
        written.len(),
        5,
        "expected the manifest, the three files and vm.dev.json (made by the pull), nothing else, got {written:?}"
    );
}

#[test]
fn with_relations_the_link_has_the_relations_lines_and_relations_is_left_alone() {
    let stock = Stock::ready();
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    let dir = project(&machine, "board");
    let relations = "// The Builder's own.\nexport {};\n";
    fs::create_dir_all(dir.join("resources/stock")).unwrap();
    fs::write(dir.join("resources/stock/relations.ts"), relations).unwrap();
    machine
        .run(&pull_args(STOCK, dir.to_str().unwrap(), true))
        .ok_json();
    assert_eq!(
        fs::read_to_string(dir.join("resources/stock/index.ts")).unwrap(),
        link(STOCK, true),
        "expected the link with the relations lines when relations.ts is there"
    );
    assert_eq!(
        fs::read_to_string(dir.join("resources/stock/relations.ts")).unwrap(),
        relations,
        "expected relations.ts, the Builder's, left as it was"
    );
    // Once it is gone, the next pull writes the link without them.
    fs::remove_file(dir.join("resources/stock/relations.ts")).unwrap();
    let printed = machine
        .run(&pull_args(STOCK, dir.to_str().unwrap(), true))
        .ok_json();
    assert_files(&printed, STOCK, false, true, false);
    assert_eq!(
        fs::read_to_string(dir.join("resources/stock/index.ts")).unwrap(),
        link(STOCK, false)
    );
}

#[test]
fn without_project_the_current_directory_is_the_project() {
    let stock = Stock::ready();
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    fs::write(machine.cwd().join("manifest.ts"), MANIFEST).unwrap();
    machine.run(&["pull", STOCK, "--json"]).ok_json();
    assert!(
        machine.cwd().join("resources/stock/schema.ts").is_file()
            && machine.cwd().join("resources/stock/index.ts").is_file(),
        "expected the files written in the current directory"
    );
}

#[test]
fn pulling_twice_changes_nothing() {
    let stock = Stock::ready();
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    let dir = project(&machine, "board");
    let first = machine
        .run(&pull_args(STOCK, dir.to_str().unwrap(), true))
        .ok_json();
    assert_files(&first, STOCK, true, true, true);
    let before = snapshot(&dir);
    std::thread::sleep(Duration::from_millis(1100));
    let second = machine
        .run(&pull_args(STOCK, dir.to_str().unwrap(), true))
        .ok_json();
    assert_files(&second, STOCK, false, false, false);
    assert_eq!(second["fingerprint"], first["fingerprint"]);
    let after = snapshot(&dir);
    assert_eq!(
        before, after,
        "expected the second pull of an unchanged structure to write no file: bytes and modification times as they were"
    );
    // In words, too.
    std::thread::sleep(Duration::from_millis(50));
    machine
        .run(&["pull", STOCK, "--project", dir.to_str().unwrap()])
        .ok_text();
    assert_eq!(before, snapshot(&dir));
}

#[test]
fn a_changed_structure_rewrites_the_definition_and_the_standin_only() {
    let stock = Stock::ready();
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    let dir = project(&machine, "board");
    let first = machine
        .run(&pull_args(STOCK, dir.to_str().unwrap(), true))
        .ok_json();
    stock
        .db()
        .superuser()
        .batch("ALTER TABLE sites ADD COLUMN region text");
    let second = machine
        .run(&pull_args(STOCK, dir.to_str().unwrap(), true))
        .ok_json();
    assert_ne!(second["fingerprint"], first["fingerprint"]);
    assert_files(&second, STOCK, true, false, true);
    assert!(
        fs::read_to_string(dir.join("resources/stock/schema.ts"))
            .unwrap()
            .contains("\"region\"")
    );
}

#[test]
fn a_directory_without_a_manifest_is_not_a_project() {
    let stock = Stock::ready();
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    let dir = machine.root().join("empty");
    fs::create_dir_all(&dir).unwrap();
    machine
        .run(&pull_args(STOCK, dir.to_str().unwrap(), true))
        .json_error(1, "not-a-project");
    assert!(
        snapshot(&dir).is_empty() && fs::read_dir(&dir).unwrap().next().is_none(),
        "expected nothing written into a directory that is not a project"
    );
    machine
        .run(&pull_args(STOCK, dir.to_str().unwrap(), false))
        .text_error(1);
}

#[test]
fn a_file_the_tool_did_not_write_is_not_overwritten() {
    let stock = Stock::ready();
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    for file in ["schema.ts", "index.ts"] {
        let dir = project(&machine, &format!("hand-{file}"));
        fs::create_dir_all(dir.join("resources/stock")).unwrap();
        let hand_written = "// Written by hand.\nexport const mine = 1;\n";
        fs::write(dir.join("resources/stock").join(file), hand_written).unwrap();
        let before = snapshot(&dir);
        let message = machine
            .run(&pull_args(STOCK, dir.to_str().unwrap(), true))
            .json_error(1, "not-generated");
        assert!(
            message.contains(&format!("resources/stock/{file}")),
            "expected the message to name the file, got {message:?}"
        );
        assert_eq!(
            fs::read_to_string(dir.join("resources/stock").join(file)).unwrap(),
            hand_written,
            "expected the hand-written {file} not overwritten"
        );
        assert_eq!(
            before,
            snapshot(&dir),
            "expected neither file written when {file} was not written by the tool"
        );
    }
}

#[test]
fn without_a_grant_the_pull_is_refused_and_nothing_is_written_or_changed() {
    let stock = Stock::ready();
    let nora_machine = Machine::new();
    nora_machine.sign_in(&stock.scenario.vm, &nora());
    let dir = project(&nora_machine, "board");
    let before = snapshot(&dir);
    let message = nora_machine
        .run(&pull_args(STOCK, dir.to_str().unwrap(), true))
        .json_error(3, "no-grant");
    assert_no_grant_message(&message, "`vm pull` with no Grant");
    let api_refusal = blackbox::structure::pull(&stock.scenario.vm.api_as(&nora()), STOCK).json();
    assert_eq!(
        message,
        api_refusal["error"]["message"].as_str().unwrap(),
        "expected the API's own message"
    );
    assert_eq!(before, snapshot(&dir), "expected nothing written");
    let words = nora_machine
        .run(&pull_args(STOCK, dir.to_str().unwrap(), false))
        .text_error(3);
    assert_no_grant_message(&words, "`vm pull` with no Grant, in words");

    // Also when an earlier pull's files are there.
    let rita_machine = Machine::new();
    rita_machine.sign_in(&stock.scenario.vm, &rita());
    rita_machine
        .run(&pull_args(STOCK, dir.to_str().unwrap(), true))
        .ok_json();
    stock
        .db()
        .superuser()
        .batch("ALTER TABLE sites ADD COLUMN region text");
    let before = snapshot(&dir);
    assert_eq!(
        before.len(),
        5,
        "the manifest, the three files and vm.dev.json"
    );
    std::thread::sleep(Duration::from_millis(50));
    nora_machine
        .run(&pull_args(STOCK, dir.to_str().unwrap(), true))
        .json_error(3, "no-grant");
    assert_eq!(
        before,
        snapshot(&dir),
        "expected an earlier pull's files left as they were by a refused pull"
    );
}

#[test]
fn a_builder_whose_grant_is_structure_only_can_pull() {
    let stock = Stock::start();
    stock.register(STOCK);
    stock.scenario.grant(STOCK, "viewers", "structure");
    let sam = User::new("sam").in_groups(&["viewers"]);
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &sam);
    let dir = project(&machine, "board");
    let printed = machine
        .run(&pull_args(STOCK, dir.to_str().unwrap(), true))
        .ok_json();
    assert_eq!(printed["your_access"], json!("structure"));
    assert!(dir.join("resources/stock/schema.ts").is_file());
}

#[test]
fn a_resource_that_does_not_exist_is_refused_with_the_apis_reason() {
    let stock = Stock::ready();
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    let dir = project(&machine, "board");
    machine
        .run(&pull_args("nothing_here", dir.to_str().unwrap(), true))
        .json_error(3, "not-found");
    assert_eq!(snapshot(&dir).len(), 1, "expected nothing written");
}

#[test]
fn a_name_that_is_no_resource_name_is_a_wrong_command_line_and_writes_nothing_anywhere() {
    let stock = Stock::ready();
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    let dir = project(&machine, "board");
    // A name that is one pulls, and one that is a name but no Resource's is
    // the API's refusal: the command line was right.
    machine
        .run(&pull_args(STOCK, dir.to_str().unwrap(), true))
        .ok_json();
    machine
        .run(&pull_args("nothing_here", dir.to_str().unwrap(), true))
        .json_error(3, "not-found");
    let before_machine = snapshot(machine.root());
    for name in [
        "..",
        "../stock",
        "stock/../../outside",
        "a/b",
        "/tmp/stock",
        "Stock",
        "stock-tool",
        "1stock",
        "",
        "st\nock",
        &"s".repeat(64),
    ] {
        let run = machine.run(&pull_args(name, dir.to_str().unwrap(), true));
        run.expect_code(2);
        assert_eq!(
            before_machine,
            snapshot(machine.root()),
            "expected nothing written anywhere for the name {name:?}"
        );
    }
    assert!(!machine.root().join("outside").exists());
}

#[test]
fn not_signed_in_is_exit_code_4() {
    let stock = Stock::ready();
    let machine = Machine::new();
    let dir = project(&machine, "board");
    machine
        .run(&pull_args(STOCK, dir.to_str().unwrap(), true))
        .json_error(4, "not-signed-in");
    assert_eq!(snapshot(&dir).len(), 1);
    drop(stock);
}

#[test]
fn without_json_each_unsupported_column_is_one_line_saying_it_is_typed_unknown() {
    let stock = Stock::ready();
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    let dir = project(&machine, "board");
    let words = machine
        .run(&pull_args(STOCK, dir.to_str().unwrap(), false))
        .ok_text();
    for (_, table, column, type_word) in UNSUPPORTED {
        assert!(
            words.lines().any(|l| l.contains(table)
                && l.contains(column)
                && l.contains(type_word)
                && l.contains("unknown")),
            "expected one line naming {table}, {column} and its type ({type_word}) and saying it is typed `unknown`, got\n{words}"
        );
    }
    assert!(
        words.contains("resources/stock/schema.ts"),
        "expected the words to name the files written, got\n{words}"
    );
}

#[test]
fn the_tool_writes_through_no_symbolic_link() {
    use std::os::unix::fs::symlink;
    let stock = Stock::ready();
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    let generated = |file: &str| {
        if file == "schema.ts" {
            format!("{FIRST_LINE}\n// outside the project\n")
        } else {
            "// Generated. Do not edit.\n// outside the project\n".to_owned()
        }
    };
    for (case, link_path) in [
        ("resources", "resources"),
        ("the resource's folder", "resources/stock"),
        ("schema.ts", "resources/stock/schema.ts"),
        ("index.ts", "resources/stock/index.ts"),
    ] {
        let dir = project(
            &machine,
            &format!("linked-{}", case.replace([' ', '\''], "-")),
        );
        let outside = machine
            .root()
            .join(format!("outside-{}", case.replace([' ', '\''], "-")));
        fs::create_dir_all(&outside).unwrap();
        match link_path {
            "resources" => symlink(&outside, dir.join("resources")).unwrap(),
            "resources/stock" => {
                fs::create_dir_all(dir.join("resources")).unwrap();
                symlink(&outside, dir.join("resources/stock")).unwrap();
            }
            file => {
                let name = file.rsplit('/').next().unwrap();
                let target = outside.join(name);
                fs::write(&target, generated(name)).unwrap();
                fs::create_dir_all(dir.join("resources/stock")).unwrap();
                symlink(&target, dir.join(file)).unwrap();
            }
        }
        let before = snapshot(machine.root());
        let message = machine
            .run(&pull_args(STOCK, dir.to_str().unwrap(), true))
            .json_error(1, "not-a-project");
        assert!(
            message.contains(link_path),
            "{case}: expected the message to name the link {link_path:?}, got {message:?}"
        );
        assert_eq!(
            before,
            snapshot(machine.root()),
            "{case}: expected nothing written or changed, inside the project or outside"
        );
        let outside_files = snapshot(&outside);
        assert!(
            outside_files.iter().all(|(_, bytes, _)| String::from_utf8_lossy(bytes).contains("outside the project")),
            "{case}: expected nothing written outside the project: {:?}",
            names(&outside_files)
        );
    }
}
