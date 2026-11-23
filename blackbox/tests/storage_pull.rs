//! Pulling a Storage Resource's structure (CONTRACT.md, "Storage: Pulling
//! a Storage Resource, checking and deploying", first two bullets; ticket
//! "Storage: a shared, named file Resource, on a mounted directory now and
//! S3 later"): `GET /v1/resources/<name>/structure` on a Storage Resource
//! answers `{ resource, kind, your_access, contact }` and nothing else,
//! decided as for a database (a Grant at any level; `no-grant` otherwise,
//! with the Resource and its contact), with the Operation log entry of any
//! pull; on a database the answer gains `"kind": "postgres"` and nothing
//! else changes; `vibe pull <name>` writes one file,
//! `resources/<name>/storage.json`, under the rules of the other files a
//! pull writes (not over a file the tool did not write, no link followed,
//! unchanged when nothing changed), copies the Builder's own Grant into
//! `vibe.dev.json`, and prints what a database pull prints with
//! `fingerprint` null and `unsupported` empty.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation here is a failing test"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use blackbox::User;
use blackbox::api::assert_recent_rfc3339;
use blackbox::assert_api_error;
use blackbox::http::Response;
use blackbox::scenario::READERS;
use blackbox::storage::{FILE_READERS, FILE_WRITERS, FILES_CONTACT, MARKETING, Storage};
use blackbox::storage_tool::{
    GENERATED, assert_pull_printed, assert_storage_json, storage_declaring,
};
use blackbox::structure::{FIRST_LINE, STOCK, Stock, assert_fingerprint, pull};
use blackbox::tool::Machine;
use serde_json::{Value, json};

/// A Builder whose Groups hold no Grant.
fn nora() -> User {
    User::new("nora").in_groups(&["elsewhere"])
}

/// A Builder in [`READERS`], granted on the fixture shared database.
fn rita() -> User {
    User::new("rita").in_groups(&[READERS])
}

/// A program with [`MARKETING`] registered and [`FILE_READERS`] granted
/// `read` and [`FILE_WRITERS`] `write` on it.
fn ready() -> Storage {
    let s = Storage::start();
    s.register(MARKETING);
    s.grant(MARKETING, FILE_READERS, "read");
    s.grant(MARKETING, FILE_WRITERS, "write");
    s
}

/// `GET /v1/resources/<name>/structure` on [`MARKETING`] as this User.
fn pull_marketing(s: &Storage, user: &User) -> Response {
    pull(&s.vm.api_as(user), MARKETING)
}

/// A project directory on the machine, declaring [`MARKETING`] at `read`.
fn project(machine: &Machine, name: &str) -> PathBuf {
    let dir = machine.root().join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.ts"),
        storage_declaring(name, &[(MARKETING, "storage", "read")]),
    )
    .unwrap();
    dir
}

/// `vibe pull marketing --project <dir> --json`.
fn pull_command(dir: &Path) -> Vec<&str> {
    vec![
        "pull",
        MARKETING,
        "--project",
        dir.to_str().unwrap(),
        "--json",
    ]
}

/// Fails unless `entry` is the Operation log entry of a structure pull:
/// `operation` `structure`, `application` and `version` null, `statement`
/// and `rows` null, this outcome, and `reason` beside it when refused.
fn assert_structure_entry(
    entry: &Value,
    response: &Response,
    user: &User,
    resource: &str,
    outcome: &str,
    reason: Option<&str>,
) {
    let context = format!("the Operation log entry of the pull {}", response.request());
    assert!(
        entry["id"].is_u64(),
        "{context}: expected a numeric `id`, got {entry}"
    );
    assert_recent_rfc3339(&entry["time"], 120, &context);
    let want = json!({
        "operation": "structure",
        "application": null,
        "version": null,
        "request": response.request_id(),
        "user": user.id,
        "resource": resource,
        "statement": null,
        "outcome": outcome,
        "reason": reason,
        "error": null,
        "rows": null,
    });
    for (key, value) in want.as_object().unwrap() {
        let got = entry.get(key).unwrap_or(&Value::Null);
        assert_eq!(
            got, value,
            "{context}: expected `{key}` {value}, got the entry {entry}"
        );
    }
}

/// The relative paths of every file under `dir`, sorted.
fn every_file(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    collect(dir, dir, &mut found);
    found.sort();
    found
}

fn collect(base: &Path, dir: &Path, found: &mut Vec<String>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        let relative = path
            .strip_prefix(base)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        if path.is_dir() {
            collect(base, &path, found);
        } else {
            found.push(relative);
        }
    }
}

#[test]
fn the_structure_of_a_storage_resource_is_resource_kind_your_access_and_contact_and_nothing_else() {
    let s = ready();
    // A Grant at any level is enough: `read` and `write` both answer.
    for (user, access) in [
        (blackbox::storage::rita(), "read"),
        (blackbox::storage::wanda(), "write"),
    ] {
        let answer = pull_marketing(&s, &user);
        answer.expect_status(200);
        let body = answer.json();
        let mut keys: Vec<&str> = body
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["contact", "kind", "resource", "your_access"],
            "as {user:?}: expected exactly the fields of the contract, no `fingerprint`, `schema_ts`, `standin_sql` or `unsupported`, got {body}"
        );
        assert_eq!(body["resource"], json!(MARKETING), "{body}");
        assert_eq!(body["kind"], json!("storage"), "{body}");
        assert_eq!(body["your_access"], json!(access), "{body}");
        assert_eq!(body["contact"], json!(FILES_CONTACT), "{body}");
    }
}

#[test]
fn a_user_with_no_grant_is_refused_with_the_resource_and_its_contact() {
    let s = ready();
    let answer = pull_marketing(&s, &nora());
    assert_api_error(&answer, 403, "no-grant");
    let error = &answer.json()["error"];
    assert_eq!(
        error["resource"],
        json!(MARKETING),
        "expected `resource` beside the reason, got {error}"
    );
    assert_eq!(
        error["contact"],
        json!(FILES_CONTACT),
        "expected the Resource's `contact` beside the reason, got {error}"
    );
    let message = error["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(FILES_CONTACT),
        "expected the message to name whom to ask ({FILES_CONTACT:?}), got {message:?}"
    );
    let lower = message.to_lowercase();
    assert!(
        lower.contains("build") && (lower.contains("not") || lower.contains("n't")),
        "expected the message to say not to build against the Resource until then, got {message:?}"
    );
}

#[test]
fn every_structure_pull_allowed_or_refused_has_one_operation_log_entry() {
    let s = ready();
    let allowed = pull_marketing(&s, &blackbox::storage::rita());
    allowed.expect_status(200);
    assert_structure_entry(
        &s.entry_of(&allowed),
        &allowed,
        &blackbox::storage::rita(),
        MARKETING,
        "allowed",
        None,
    );
    let refused = pull_marketing(&s, &nora());
    refused.expect_status(403);
    assert_structure_entry(
        &s.entry_of(&refused),
        &refused,
        &nora(),
        MARKETING,
        "refused",
        Some("no-grant"),
    );
}

#[test]
fn on_a_database_the_structure_answer_gains_kind_postgres_and_nothing_else_changes() {
    let stock = Stock::ready();
    let answer = stock.pull_ok(&rita(), STOCK);
    let mut keys: Vec<&str> = answer
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "contact",
            "fingerprint",
            "kind",
            "resource",
            "schema_ts",
            "standin_sql",
            "unsupported",
            "your_access"
        ],
        "expected the answer of a database's pull with `kind` gained and nothing else changed, got {answer}"
    );
    assert_eq!(answer["kind"], json!("postgres"), "{answer}");
    assert_fingerprint(&answer["fingerprint"], "the fingerprint as ever");
    assert!(
        answer["schema_ts"]
            .as_str()
            .unwrap_or_default()
            .starts_with(FIRST_LINE),
        "expected the definition as ever, got {answer}"
    );
    assert_eq!(answer["your_access"], json!("read"), "{answer}");
}

#[test]
fn vibe_pull_writes_one_storage_json_and_prints_as_a_database_with_fingerprint_null() {
    let s = ready();
    let machine = Machine::new();
    machine.sign_in(&s.vm, &blackbox::storage::rita());
    let dir = project(&machine, "pull-board");
    let printed = machine.run(&pull_command(&dir)).ok_json();
    assert_pull_printed(&printed, MARKETING, "read", true, true);
    assert_storage_json(
        &dir.join(format!("resources/{MARKETING}/storage.json")),
        MARKETING,
        FILES_CONTACT,
        "the first pull",
    );
    assert_eq!(
        every_file(&dir),
        [
            "manifest.ts",
            &format!("resources/{MARKETING}/storage.json"),
            "vibe.dev.json"
        ],
        "expected the manifest, the one file of the pull, and vibe.dev.json the pull made, nothing else"
    );
}

#[test]
fn pulling_twice_writes_nothing_and_says_changed_false() {
    let s = ready();
    let machine = Machine::new();
    machine.sign_in(&s.vm, &blackbox::storage::rita());
    let dir = project(&machine, "twice-board");
    machine.run(&pull_command(&dir)).ok_json();
    let before = blackbox::project::Tree::of(&dir);
    // So that a rewrite the contract forbids would be told by the file's
    // modification time, not hidden by the clock's granularity.
    std::thread::sleep(Duration::from_millis(1100));
    let second = machine.run(&pull_command(&dir)).ok_json();
    assert_pull_printed(&second, MARKETING, "read", false, false);
    assert_eq!(
        before.differences(&blackbox::project::Tree::of(&dir)),
        Vec::<String>::new(),
        "expected the second pull of an unchanged Resource to write no file: bytes and modification times as they were"
    );
}

#[test]
fn a_storage_json_the_tool_did_not_write_is_not_generated_and_nothing_is_written() {
    let s = ready();
    let machine = Machine::new();
    machine.sign_in(&s.vm, &blackbox::storage::rita());
    let dir = project(&machine, "hand-board");
    fs::create_dir_all(dir.join(format!("resources/{MARKETING}"))).unwrap();
    let hand_written = format!(
        "{{\n  \"generated\": \"written by hand\",\n  \"resource\": \"{MARKETING}\",\n  \"kind\": \"storage\",\n  \"contact\": \"someone\"\n}}\n"
    );
    fs::write(
        dir.join(format!("resources/{MARKETING}/storage.json")),
        &hand_written,
    )
    .unwrap();
    let before = blackbox::project::Tree::of(&dir);
    let message = machine
        .run(&pull_command(&dir))
        .json_error(1, "not-generated");
    assert!(
        message.contains(&format!("resources/{MARKETING}/storage.json")),
        "expected the message to name the file, got {message:?}"
    );
    assert_eq!(
        fs::read_to_string(dir.join(format!("resources/{MARKETING}/storage.json"))).unwrap(),
        hand_written,
        "expected the hand-written file as it was"
    );
    assert_eq!(
        before.differences(&blackbox::project::Tree::of(&dir)),
        Vec::<String>::new(),
        "expected nothing written: no vibe.dev.json made either"
    );
}

#[test]
fn a_link_anywhere_under_resources_is_not_written_through() {
    use std::os::unix::fs::symlink;
    // CONTRACT.md: "no link followed", as for the other files a pull
    // writes: `resources`, `resources/<name>` and `storage.json` itself.
    for (case, link_path) in [
        ("the file", format!("resources/{MARKETING}/storage.json")),
        ("the Resource's folder", format!("resources/{MARKETING}")),
        ("resources", "resources".to_owned()),
    ] {
        let s = ready();
        let machine = Machine::new();
        machine.sign_in(&s.vm, &blackbox::storage::rita());
        let dir = project(&machine, "linked-board");
        let outside = machine.root().join("outside");
        fs::create_dir_all(&outside).unwrap();
        match link_path.as_str() {
            "resources" => symlink(&outside, dir.join("resources")).unwrap(),
            path if path == format!("resources/{MARKETING}") => {
                fs::create_dir_all(dir.join("resources")).unwrap();
                symlink(&outside, dir.join(path)).unwrap();
            }
            path => {
                let target = outside.join("storage.json");
                fs::write(&target, format!("{{\"generated\": \"{GENERATED}\"}}\n")).unwrap();
                fs::create_dir_all(dir.join(format!("resources/{MARKETING}"))).unwrap();
                symlink(&target, dir.join(path)).unwrap();
            }
        }
        let before = blackbox::project::Tree::of(machine.root());
        let message = machine
            .run(&pull_command(&dir))
            .json_error(1, "not-a-project");
        assert!(
            message.contains(&link_path),
            "{case}: expected the message to name the link {link_path:?}, got {message:?}"
        );
        assert_eq!(
            before.differences(&blackbox::project::Tree::of(machine.root())),
            Vec::<String>::new(),
            "{case}: expected nothing written or changed, inside the project or outside it"
        );
    }
}

#[test]
fn a_refused_pull_leaves_the_file_an_earlier_pull_wrote_as_it_was() {
    // CONTRACT.md: "nothing is written or changed, also when an earlier
    // pull's files are there".
    let s = ready();
    let machine = Machine::new();
    machine.sign_in(&s.vm, &blackbox::storage::rita());
    let dir = project(&machine, "kept-board");
    machine.run(&pull_command(&dir)).ok_json();
    let file = dir.join(format!("resources/{MARKETING}/storage.json"));
    let written = fs::read_to_string(&file).unwrap();
    let before = blackbox::project::Tree::of(&dir);
    std::thread::sleep(Duration::from_millis(1100));
    machine.sign_in(&s.vm, &nora());
    machine.run(&pull_command(&dir)).json_error(3, "no-grant");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        written,
        "expected the earlier pull's file as it was"
    );
    assert_eq!(
        before.differences(&blackbox::project::Tree::of(&dir)),
        Vec::<String>::new(),
        "expected nothing written or changed by the refused pull: bytes and modification times"
    );
}

#[test]
fn the_pull_in_words_says_the_resource_its_kind_and_the_file_and_no_fingerprint() {
    // CONTRACT.md: "in words it says the Resource, its kind and the file
    // written or unchanged, and no fingerprint".
    let s = ready();
    let machine = Machine::new();
    machine.sign_in(&s.vm, &blackbox::storage::rita());
    let dir = project(&machine, "words-board");
    let words_command = ["pull", MARKETING, "--project", dir.to_str().unwrap()];
    let path = format!("resources/{MARKETING}/storage.json");
    for (case, said) in [
        ("first", &["written", "wrote"][..]),
        ("second", &["unchanged"][..]),
    ] {
        let text = machine.run(&words_command).ok_text();
        let lower = text.to_lowercase();
        for named in [MARKETING, "storage", path.as_str()] {
            assert!(
                text.contains(named),
                "the {case} pull: expected the words to say {named:?}, got\n{text}"
            );
        }
        assert!(
            said.iter().any(|w| lower.contains(w)),
            "the {case} pull: expected the words to say the file was {said:?}, got\n{text}"
        );
        for never in ["fingerprint", "sha256", "null"] {
            assert!(
                !lower.contains(never),
                "the {case} pull: expected no {never:?} in the words of a Storage Resource's pull, got\n{text}"
            );
        }
    }
}

#[test]
fn the_builders_own_grant_is_copied_into_vibe_dev_json_and_replaced_when_it_changes() {
    let s = ready();
    let machine = Machine::new();
    machine.sign_in(&s.vm, &blackbox::storage::rita());
    let dir = project(&machine, "grant-board");
    machine.run(&pull_command(&dir)).ok_json();
    let file = blackbox::local::dev_json(&dir);
    assert_eq!(
        file["grants"],
        json!([{ "group": "builder", "resource": MARKETING, "access": "read" }]),
        "expected the Builder's own Grant copied, as for a database, got {file}"
    );
    assert!(
        file["users"]
            .as_array()
            .is_some_and(|users| users.contains(&blackbox::local::builder_user())),
        "expected the builder test User added, as for a database, got {file}"
    );

    // Another Builder, whose Grant is at `write`, pulls into the same
    // project: the Grant that Group had on the Resource is replaced.
    machine.sign_in(&s.vm, &blackbox::storage::wanda());
    let printed = machine.run(&pull_command(&dir)).ok_json();
    assert_pull_printed(&printed, MARKETING, "write", false, true);
    assert_eq!(
        blackbox::local::dev_json(&dir)["grants"],
        json!([{ "group": "builder", "resource": MARKETING, "access": "write" }]),
        "expected the Grant replaced, not a second one beside it"
    );
}

#[test]
fn a_pull_without_a_grant_is_refused_and_writes_nothing() {
    let s = ready();
    let machine = Machine::new();
    machine.sign_in(&s.vm, &nora());
    let dir = project(&machine, "refused-board");
    let before = blackbox::project::Tree::of(&dir);
    let message = machine.run(&pull_command(&dir)).json_error(3, "no-grant");
    assert!(
        message.contains(FILES_CONTACT),
        "expected the refusal's message to name whom to ask ({FILES_CONTACT:?}), got {message:?}"
    );
    assert_eq!(
        before.differences(&blackbox::project::Tree::of(&dir)),
        Vec::<String>::new(),
        "expected nothing written or changed by a refused pull"
    );
}

/// Plants `case` under `resources/<resource>/` of the project in `dir`:
/// what a pull checks of the project before it asks the platform anything.
/// Returns the reason the pull must end with.
fn plant(dir: &Path, resource: &str, case: &str, outside: &Path) -> &'static str {
    use std::os::unix::fs::symlink;
    let folder = dir.join(format!("resources/{resource}"));
    fs::create_dir_all(dir.join("resources")).unwrap();
    match case {
        "schema.ts by hand" | "index.ts by hand" | "storage.json by hand" => {
            fs::create_dir_all(&folder).unwrap();
            let name = case.split(' ').next().unwrap();
            let text = if name == "storage.json" {
                "{\n  \"generated\": \"mine\"\n}\n"
            } else {
                "// Written by hand.\nexport {};\n"
            };
            fs::write(folder.join(name), text).unwrap();
            "not-generated"
        }
        "a link as storage.json" => {
            fs::create_dir_all(&folder).unwrap();
            let target = outside.join("storage.json");
            fs::write(&target, format!("{{\"generated\": \"{GENERATED}\"}}\n")).unwrap();
            symlink(&target, folder.join("storage.json")).unwrap();
            "not-a-project"
        }
        "a link as the Resource's folder" => {
            symlink(outside, &folder).unwrap();
            "not-a-project"
        }
        other => panic!("unknown case {other}"),
    }
}

const PLANTED: [&str; 5] = [
    "schema.ts by hand",
    "index.ts by hand",
    "storage.json by hand",
    "a link as storage.json",
    "a link as the Resource's folder",
];

/// Plants each of [`PLANTED`] under `resources/<resource>/` of a project
/// and pulls as each of these Machines: the pull must end with the
/// planted case's reason, write nothing, and (the caller reads the
/// Operation log) ask the platform nothing.
fn pull_ends_on_the_project(kind: &str, resource: &str, machines: &[(&str, Machine)]) {
    for (who, machine) in machines {
        for (n, case) in PLANTED.iter().enumerate() {
            let name = format!("order-{n}");
            let dir = machine.root().join(&name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("manifest.ts"),
                storage_declaring(&name, &[(resource, "storage", "read")]),
            )
            .unwrap();
            let outside = machine.root().join(format!("outside-{n}"));
            fs::create_dir_all(&outside).unwrap();
            let reason = plant(&dir, resource, case, &outside);
            let before = blackbox::project::Tree::of(machine.root());
            let run = machine.run(&[
                "pull",
                resource,
                "--project",
                dir.to_str().unwrap(),
                "--json",
            ]);
            run.json_error(1, reason);
            assert_eq!(
                before.differences(&blackbox::project::Tree::of(machine.root())),
                Vec::<String>::new(),
                "{kind}, {who}, {case}: expected nothing written or changed"
            );
        }
    }
}

fn assert_no_structure_entry(log: &[Value], what: &str) {
    let pulls: Vec<&Value> = log
        .iter()
        .filter(|e| e["operation"] == "structure")
        .collect();
    assert!(
        pulls.is_empty(),
        "expected no request made for a pull of {what} that ends on the project, got the Operation log entries {pulls:?}"
    );
}

fn machine_as(vm: &blackbox::vm::Vm, user: Option<&User>) -> Machine {
    let m = Machine::new();
    if let Some(user) = user {
        m.sign_in(vm, user);
    }
    m
}

#[test]
fn a_pull_of_a_storage_resource_checks_the_project_before_it_asks_the_platform_anything() {
    // CONTRACT.md: checked "before it asks the platform anything ... for
    // every pull, whatever the Resource turns out to be ... and no request
    // made", also when the Builder holds no Grant or is not signed in.
    let s = ready();
    let machines = [
        (
            "with a Grant",
            machine_as(&s.vm, Some(&blackbox::storage::rita())),
        ),
        ("without a Grant", machine_as(&s.vm, Some(&nora()))),
        ("not signed in", machine_as(&s.vm, None)),
    ];
    pull_ends_on_the_project("a Storage Resource", MARKETING, &machines);
    assert_no_structure_entry(&s.log(), "a Storage Resource");
}

#[test]
fn a_pull_of_a_database_checks_the_project_before_it_asks_the_platform_anything() {
    let stock = Stock::ready();
    let vm = &stock.scenario.vm;
    let machines = [
        ("with a Grant", machine_as(vm, Some(&rita()))),
        ("without a Grant", machine_as(vm, Some(&nora()))),
        ("not signed in", machine_as(vm, None)),
    ];
    pull_ends_on_the_project("a database", STOCK, &machines);
    assert_no_structure_entry(&stock.scenario.log(), "a database");
}

/// 17 MiB, past what any file the tool wrote can be.
const TOO_LARGE: usize = 17 * 1024 * 1024;

#[test]
fn a_storage_json_too_large_to_be_the_tools_is_not_generated_and_left_as_it_was() {
    let s = ready();
    let machine = Machine::new();
    machine.sign_in(&s.vm, &blackbox::storage::rita());
    for (case, content) in [
        ("17 MiB of anything", vec![b'x'; TOO_LARGE]),
        ("a start like the tool's file, going on", {
            let mut bytes = storage_json_of_the_past().into_bytes();
            bytes.extend(std::iter::repeat_n(b' ', TOO_LARGE));
            bytes
        }),
    ] {
        let dir = project(&machine, "large-board");
        fs::create_dir_all(dir.join(format!("resources/{MARKETING}"))).unwrap();
        let file = dir.join(format!("resources/{MARKETING}/storage.json"));
        fs::write(&file, &content).unwrap();
        let before = blackbox::project::Tree::of(&dir);
        let message = machine
            .run(&pull_command(&dir))
            .json_error(1, "not-generated");
        assert!(
            message.contains(&format!("resources/{MARKETING}/storage.json")),
            "{case}: expected the message to name the file, got {message:?}"
        );
        assert!(
            fs::read(&file).unwrap() == content,
            "{case}: expected the file byte for byte as it was"
        );
        assert_eq!(
            before.differences(&blackbox::project::Tree::of(&dir)),
            Vec::<String>::new(),
            "{case}: expected nothing written or changed"
        );
    }
}

/// A `storage.json` that looks the tool's, as a pull would write it.
fn storage_json_of_the_past() -> String {
    blackbox::storage_tool::stale_storage_json(MARKETING, FILES_CONTACT)
}
