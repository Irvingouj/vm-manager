//! The local loop with a Storage Resource (CONTRACT.md, "Storage: The
//! local loop"; ticket "Storage: a shared, named file Resource, on a
//! mounted directory now and S3 later"): a local Storage for each pulled
//! Storage Resource, the folder `.vibe/local/storage/<name>/` made by the
//! first run that needs it with the marks an installation's directory
//! holds, registered under its name with the contact of `storage.json`,
//! removed when it is no longer pulled; the Grants of `vibe.dev.json`
//! decide, and one at `structure` is a note that holds nothing; the
//! outputs gain `"storages"` and hold the Storage operations' entries like
//! any, one line for each in words; what the Application puts there stays
//! from one run to the next and while `vibe dev` runs; a file placed in
//! the folder by hand is a file of the local Storage; `vibe local reset`
//! empties every local Storage folder but keeps it; `vibe dev`'s page
//! `/_vm/test-user` shows the access each test User's Groups give on
//! Storage Resources too.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation here is a failing test"
)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::time::{Duration, Instant};

use blackbox::dev::Dev;
use blackbox::local::{BUILDER, OUTSIDER, dev_json, write_dev_json};
use blackbox::project::Tree;
use blackbox::scenario::{Expected, assert_entry};
use blackbox::storage::{
    FILES_CONTACT, MARKETING, my_uid, place, statement_of, storage_expected, tree,
};
use blackbox::storage_tool::{Files, assert_storages, stale_storage_json};
use serde_json::{Value, json};

/// The Application's answer inside what `vibe request --json` printed.
fn app_body(body: &Value) -> Value {
    let text = body["body"]
        .as_str()
        .unwrap_or_else(|| panic!("expected the Application's answer as text, got {body}"));
    serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("expected the Application's answer as JSON ({e}), got {text:?}"))
}

/// The part of the chooser page from the first mention of `id` to the
/// first mention of another of `ids` after it, or to the end.
fn segment_of<'p>(page: &'p str, id: &str, ids: &[&str]) -> &'p str {
    let start = page
        .find(id)
        .unwrap_or_else(|| panic!("expected {id:?} on the page\n{page}"));
    let end = ids
        .iter()
        .filter(|other| **other != id)
        .filter_map(|other| page.find(other))
        .filter(|at| *at > start)
        .min()
        .unwrap_or(page.len());
    page.get(start..end).unwrap_or(page)
}

/// `vibe local reset --project <dir> --json`, which must end with `0`;
/// what it printed.
fn reset_json(files: &Files) -> Value {
    files.local.watch(files.dir());
    let run = files.local.run(
        &[
            "local",
            "reset",
            "--project",
            files.dir().to_str().unwrap(),
            "--json",
        ],
        &[],
    );
    run.ok_json()
}

/// `vibe local reset --project <dir>` in words, which must end with `0`;
/// what it printed.
fn reset_words(files: &Files) -> String {
    files.local.watch(files.dir());
    files
        .local
        .run(
            &["local", "reset", "--project", files.dir().to_str().unwrap()],
            &[],
        )
        .ok_text()
}

#[test]
fn a_local_storage_is_made_by_the_first_run_and_reported_with_the_operations_like_any() {
    let files = Files::start("files-board", true);
    assert!(
        !files.folder().exists(),
        "the harness's own check: nothing is made before any run"
    );
    let body = files.request("/put?key=run/one.txt", BUILDER, &[]);
    assert_eq!(body["status"], json!(200), "{body}");
    assert_storages(&body, &[(MARKETING, true)], "the first run");
    assert!(
        files.folder().is_dir(),
        "expected the folder of the project's local state made"
    );
    assert!(
        files.folder().join(".vibe-resource").is_file(),
        "expected the mark an installation's directory holds too"
    );
    assert!(
        files.folder().join(".vibe-staging").is_dir(),
        "expected the staging folder an installation's directory holds too"
    );
    assert_eq!(
        fs::read(files.folder().join("run/one.txt")).unwrap(),
        b"stored by the local run".as_slice(),
        "expected the file on the host, with the bytes put"
    );
    let operations = body["operations"].as_array().cloned().unwrap_or_default();
    assert_eq!(operations.len(), 1, "{body}");
    let id = body["request_id"].as_str().unwrap_or_default().to_owned();
    assert_entry(
        &operations[0],
        &storage_expected(
            Expected {
                resource: Some(MARKETING),
                rows: Some(1),
                ..Expected::allowed(&id, BUILDER, "files-board")
            },
            "storage.put",
            &statement_of("put", "run/one.txt"),
        ),
        "the put, in the command's own output",
    );
    // The second run makes nothing and answers the file put.
    let again = files.request("/get?key=run/one.txt", BUILDER, &[]);
    assert_storages(&again, &[(MARKETING, false)], "the second run");
    assert_eq!(
        app_body(&again)["file"]["text"],
        json!("stored by the local run"),
        "{}",
        again
    );
}

#[test]
fn the_grants_of_vibe_dev_json_decide_each_run() {
    let files = Files::start("grants-board", true);
    // The Builder, whose Grant the pull copied, can write.
    let put = files.request("/put?key=kept.txt", BUILDER, &[]);
    assert_eq!(put["status"], json!(200), "{put}");

    // A User whose Groups hold nothing: the refusal an installation gives,
    // with the contact of `storage.json`.
    let refused = files.request("/get?key=kept.txt", OUTSIDER, &[]);
    assert_eq!(refused["status"], json!(403), "{refused}");
    assert_eq!(
        app_body(&refused)["refusal"],
        json!({
            "reason": "no-grant",
            "resource": MARKETING,
            "user": OUTSIDER,
            "needed": "read",
            "held": null,
            "contact": FILES_CONTACT,
        }),
        "expected the refusal an installation gives, the Resource registered with the contact of storage.json"
    );
    assert!(
        app_body(&refused)["fix"]
            .as_str()
            .is_some_and(|f| f.contains(FILES_CONTACT)),
        "expected the refusal's fix to name whom to ask\n{refused}"
    );
    let operations = refused["operations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(operations.len(), 1, "{refused}");
    let id = refused["request_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert_entry(
        &operations[0],
        &storage_expected(
            Expected {
                resource: Some(MARKETING),
                ..Expected::refused(&id, OUTSIDER, "grants-board", "no-grant")
            },
            "storage.get",
            &statement_of("get", "kept.txt"),
        ),
        "the refused read, in the command's own output",
    );

    // A Grant at `read` lets read and refuses write with what is held.
    let mut file = dev_json(files.dir());
    file["users"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "id": "rita", "displayName": "Rita the reader", "email": "rita@example.com", "groups": ["file-readers"] }));
    file["grants"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "group": "file-readers", "resource": MARKETING, "access": "read" }));
    write_dev_json(files.dir(), &file);
    let read = files.request("/get?key=kept.txt", "rita", &[]);
    assert_eq!(read["status"], json!(200), "{read}");
    assert_eq!(
        app_body(&read)["file"]["text"],
        json!("stored by the local run"),
        "{read}"
    );
    let write = files.request("/put?key=mine.txt", "rita", &[]);
    assert_eq!(write["status"], json!(403), "{write}");
    assert_eq!(
        app_body(&write)["refusal"],
        json!({
            "reason": "no-grant",
            "resource": MARKETING,
            "user": "rita",
            "needed": "write",
            "held": "read",
            "contact": FILES_CONTACT,
        }),
        "expected a Grant below what the operation needs to hold exactly what it holds"
    );
}

#[test]
fn a_grant_at_structure_is_a_note_and_holds_nothing() {
    let files = Files::start("structure-grant-board", true);
    let mut file = dev_json(files.dir());
    file["users"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "id": "vera", "displayName": "Vera the viewer", "email": "vera@example.com", "groups": ["viewers"] }));
    file["grants"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "group": "viewers", "resource": MARKETING, "access": "structure" }));
    write_dev_json(files.dir(), &file);
    let body = files.request("/get?key=x.txt", "vera", &[]);
    let notes: Vec<Value> = body["notes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|n| n["reason"] == "grant-level-not-for-kind")
        .collect();
    assert_eq!(notes.len(), 1, "{body}");
    assert_eq!(notes[0]["group"], json!("viewers"), "{}", notes[0]);
    assert_eq!(notes[0]["resource"], json!(MARKETING), "{}", notes[0]);
    // And it holds nothing: vera is refused as a User with no Grant.
    assert_eq!(body["status"], json!(403), "{body}");
    assert_eq!(
        app_body(&body)["refusal"]["reason"],
        json!("no-grant"),
        "{body}"
    );
    assert_eq!(
        app_body(&body)["refusal"]["held"],
        Value::Null,
        "expected the Grant at `structure` to hold nothing, got {body}"
    );
}

#[test]
fn a_declared_storage_resource_that_is_not_pulled_is_not_registered() {
    let files = Files::start("not-pulled-board", false);
    // The Builder's Grant, written by hand: no pull has made the file.
    write_dev_json(
        files.dir(),
        &json!({
            "users": [blackbox::local::outsider_user(), blackbox::local::builder_user()],
            "grants": [{ "group": "builder", "resource": MARKETING, "access": "write" }],
        }),
    );
    assert!(
        !files
            .dir()
            .join(format!("resources/{MARKETING}/storage.json"))
            .exists(),
        "the harness's own check: nothing pulled"
    );
    let body = files.request("/get?key=x.txt", BUILDER, &[]);
    assert_eq!(body["status"], json!(403), "{body}");
    assert_eq!(
        app_body(&body)["refusal"]["reason"],
        json!("not-registered"),
        "expected the operation refused as for a database, got {body}"
    );
    assert_eq!(
        app_body(&body)["refusal"]["resource"],
        json!(MARKETING),
        "{body}"
    );
    assert_storages(&body, &[], "a run with no pulled Storage Resource");
    assert!(
        !files.folder().exists(),
        "expected no local Storage made for a Resource that is not pulled"
    );
}

#[test]
fn one_no_longer_pulled_has_its_folder_removed_by_the_next_run() {
    let files = Files::start("removed-board", true);
    files.request("/put?key=kept.txt", BUILDER, &[]);
    assert!(
        files.folder().is_dir(),
        "the harness's own check: the folder was made"
    );
    fs::remove_file(
        files
            .dir()
            .join(format!("resources/{MARKETING}/storage.json")),
    )
    .unwrap();
    let body = files.request("/get?key=kept.txt", BUILDER, &[]);
    assert_storages(&body, &[], "the run after the pull is gone");
    assert!(
        !files.folder().exists(),
        "expected the folder removed by the next run"
    );
    assert_eq!(
        app_body(&body)["refusal"]["reason"],
        json!("not-registered"),
        "{body}"
    );
}

#[test]
fn what_the_application_puts_stays_from_one_run_to_the_next_and_one_line_in_words() {
    let files = Files::start("stays-board", true);
    files.request("/put?key=kept/notes.txt", BUILDER, &[]);
    let again = files.request("/get?key=kept/notes.txt", BUILDER, &[]);
    assert_eq!(again["status"], json!(200), "{again}");
    assert_eq!(
        app_body(&again)["file"]["text"],
        json!("stored by the local run"),
        "expected what the first run put answered by the second\n{again}"
    );
    // In words, one line for each.
    let words = files.request_in_words("/get?key=kept/notes.txt", BUILDER, &[]);
    for named in [MARKETING, ".vibe/local/storage/marketing"] {
        assert!(
            words.contains(named),
            "expected the words to name {named:?}\n{words}"
        );
    }
}

#[test]
fn a_file_placed_in_the_folder_by_hand_is_a_file_of_the_local_storage() {
    let files = Files::start("hand-file-board", true);
    files.request("/put?key=run.txt", BUILDER, &[]);
    place(&files.folder().join("hand/seed.txt"), b"placed by hand");
    let got = files.request("/get?key=hand/seed.txt", BUILDER, &[]);
    assert_eq!(got["status"], json!(200), "{got}");
    assert_eq!(
        app_body(&got)["file"]["text"],
        json!("placed by hand"),
        "expected the file read back as the Application's own\n{got}"
    );
    let listed = files.request("/list", BUILDER, &[]);
    let keys = app_body(&listed)["keys"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for key in ["hand/seed.txt", "run.txt"] {
        assert!(
            keys.contains(&json!(key)),
            "expected {key:?} listed, got\n{listed}"
        );
    }
}

#[test]
fn vibe_local_reset_empties_every_local_storage_folder_but_keeps_it() {
    let files = Files::start("reset-board", true);
    files.request("/put?key=kept.txt", BUILDER, &[]);
    place(&files.folder().join("hand/seed.txt"), b"placed by hand");
    let reset = reset_json(&files);
    assert_eq!(reset["reset"], json!(true), "{reset}");
    // CONTRACT.md: `made` is true only when this run made the folder
    // because it was not there; a reset empties a folder and keeps it.
    assert_storages(
        &reset,
        &[(MARKETING, false)],
        "the reset of a folder that was there",
    );
    // The folder kept, emptied but for the platform's own names.
    assert!(files.folder().is_dir(), "expected the folder kept");
    assert!(
        files.folder().join(".vibe-resource").is_file(),
        "expected the Resource's mark kept"
    );
    assert!(
        files.folder().join(".vibe-staging").is_dir(),
        "expected the staging folder kept"
    );
    assert!(
        !files.folder().join("kept.txt").exists() && !files.folder().join("hand").exists(),
        "expected the files put and the file placed by hand gone"
    );
    let after = files.request("/get?key=kept.txt", BUILDER, &[]);
    assert_eq!(
        app_body(&after)["file"],
        Value::Null,
        "expected the emptied Storage to answer no file\n{after}"
    );
    // In words, one line for each.
    let words = reset_words(&files);
    for named in [MARKETING, ".vibe/local/storage/marketing"] {
        assert!(
            words.contains(named),
            "expected the words to name {named:?}\n{words}"
        );
    }
    // A folder that is not there when the reset looks is made by it.
    fs::remove_dir_all(files.folder()).unwrap();
    let again = reset_json(&files);
    assert_storages(
        &again,
        &[(MARKETING, true)],
        "the reset of a folder that was not there",
    );
    assert!(
        files.folder().join(".vibe-resource").is_file(),
        "expected the folder the reset made to have the Resource's mark"
    );
}

#[test]
fn while_vm_dev_runs_the_storages_stay_and_the_rebuilt_line_names_them() {
    let files = Files::start("dev-board", true);
    let dev = Dev::start(&files.local, files.dir(), "dev-board");
    let mut browser = dev.browser();
    browser.choose(BUILDER);
    let put = browser.build("GET", "/put?key=kept.txt");
    let put = browser.send(&put);
    assert_eq!(put.status, 200, "{}", put.describe());
    let got = browser.get("/get?key=kept.txt");
    assert_eq!(got.status, 200, "{}", got.describe());
    assert_eq!(
        got.json()["file"]["text"],
        json!("stored by the local run"),
        "expected what was put answered while `vibe dev` runs\n{}",
        got.describe()
    );

    // A change of the project rebuilds; the `rebuilt` line names the
    // storages, and what was put stays.
    let index = files.dir().join("src/index.ts");
    let code = fs::read_to_string(&index).unwrap();
    fs::write(&index, format!("{code}\n// a change to rebuild by\n")).unwrap();
    let again = browser.get("/get?key=kept.txt");
    assert_eq!(again.status, 200, "{}", again.describe());
    assert_eq!(
        again.json()["file"]["text"],
        json!("stored by the local run"),
        "expected what was put before the rebuild still there\n{}",
        again.describe()
    );
    let rebuilt = dev.wait_for_line("the `rebuilt` line", Duration::from_secs(60), |l| {
        l["event"] == "rebuilt"
    });
    assert_storages(&rebuilt, &[(MARKETING, false)], "the rebuilt line");
    dev.stop("INT").expect_code(0);
}

#[test]
fn the_test_user_page_shows_the_access_each_test_users_groups_give_on_each_storage_resource() {
    let files = Files::start("users-board", true);
    files.with_second("archive");
    let mut file = dev_json(files.dir());
    for (id, groups) in [
        ("user-rea", json!(["file-readers"])),
        ("user-wri", json!(["file-writers"])),
        ("user-non", json!([])),
    ] {
        file["users"].as_array_mut().unwrap().push(json!({
            "id": id, "displayName": id, "email": format!("{id}@example.com"), "groups": groups
        }));
    }
    // marketing: readers read, writers write; archive the other way round.
    for (group, resource, access) in [
        ("file-readers", MARKETING, "read"),
        ("file-writers", MARKETING, "write"),
        ("file-readers", "archive", "write"),
        ("file-writers", "archive", "read"),
    ] {
        file["grants"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "group": group, "resource": resource, "access": access }));
    }
    write_dev_json(files.dir(), &file);
    let dev = Dev::start(&files.local, files.dir(), "users-board");
    let mut browser = dev.browser();
    let page = browser.get("/_vm/test-user");
    assert_eq!(page.status, 200, "{}", page.describe());
    let document = blackbox::page::Document::parse(&page.text());
    let plain = document.root().text();
    let ids = ["user-rea", "user-wri", "user-non"];
    let resources = [MARKETING, "archive"];
    for (id, marketing, archive) in [
        ("user-rea", "read", "write"),
        ("user-wri", "write", "read"),
        ("user-non", "none", "none"),
    ] {
        let part = segment_of(&plain, id, &ids);
        for (resource, access) in [(MARKETING, marketing), ("archive", archive)] {
            let row = segment_of(part, resource, &resources);
            assert!(
                row.split(|c: char| !c.is_ascii_alphanumeric())
                    .any(|w| w == access),
                "expected {id}'s access on {resource}, {access}, beside the Resource's name (and not the other's)\n{row}\nin {part}"
            );
            let other = if access == "read" { "write" } else { "read" };
            assert!(
                !row.split(|c: char| !c.is_ascii_alphanumeric())
                    .any(|w| w == other),
                "expected {id}'s row of {resource} not to say {other}\n{row}"
            );
        }
    }
    dev.stop("INT").expect_code(0);
}

#[test]
fn the_first_run_after_the_pull_makes_the_folder_whether_or_not_the_request_touches_it() {
    let files = Files::start("untouched-board", true);
    assert!(!files.folder().exists(), "the harness's own check");
    // `/` is the handler's default route: it never touches the Storage.
    let first = files.request("/", BUILDER, &[]);
    assert_eq!(first["status"], json!(200), "{first}");
    assert_storages(&first, &[(MARKETING, true)], "the first run, untouching");
    assert!(
        files.folder().join(".vibe-resource").is_file()
            && files.folder().join(".vibe-staging").is_dir(),
        "expected the folder made, with its marks, by a run that did not touch it"
    );
    let second = files.request("/", BUILDER, &[]);
    assert_storages(&second, &[(MARKETING, false)], "the second run");
}

#[test]
fn storages_are_every_pulled_resource_in_the_order_of_their_names_one_line_for_each() {
    let files = Files::start("two-board", true);
    files.with_second("archive");
    let first = files.request("/", BUILDER, &[]);
    assert_storages(
        &first,
        &[("archive", true), (MARKETING, true)],
        "the first run: by name, not in the order of declaring or of pulling",
    );
    let second = files.request("/", BUILDER, &[]);
    assert_storages(
        &second,
        &[("archive", false), (MARKETING, false)],
        "the second run",
    );
    let lines_of = |words: &str, context: &str| {
        let lines: Vec<&str> = words
            .lines()
            .filter(|l| l.contains(".vibe/local/storage/"))
            .collect();
        assert_eq!(
            lines.len(),
            2,
            "{context}: expected one line for each pulled Storage Resource\n{words}"
        );
        assert!(
            lines[0].contains("archive")
                && lines[0].contains(".vibe/local/storage/archive")
                && !lines[0].contains(MARKETING),
            "{context}: expected the first line to be archive's alone\n{words}"
        );
        assert!(
            lines[1].contains(MARKETING)
                && lines[1].contains(".vibe/local/storage/marketing")
                && !lines[1].contains("archive"),
            "{context}: expected the second line to be marketing's alone\n{words}"
        );
    };
    lines_of(
        &files.request_in_words("/", BUILDER, &[]),
        "vibe request in words",
    );
    let reset = reset_json(&files);
    assert_storages(
        &reset,
        &[("archive", false), (MARKETING, false)],
        "the reset",
    );
    lines_of(&reset_words(&files), "vibe local reset in words");
}

#[test]
fn a_key_the_rules_refuse_is_refused_in_the_local_storage_too() {
    let files = Files::start("keys-board", true);
    files.request("/put?key=ok/one.txt", BUILDER, &[]);
    let before = tree(&files.folder());
    let outside_of_folder = files.folder().join("../escaped.txt");
    let long = vec!["a".repeat(100); 11].join("/");
    for (key, rule) in [
        ("../escaped.txt", "dot-segment"),
        ("a/./b", "dot-segment"),
        ("/lead", "empty-segment"),
        ("a//b", "empty-segment"),
        (".vibe-mine", "reserved"),
        ("a/.Vibe-x", "reserved"),
        (long.as_str(), "too-long"),
    ] {
        for operation in ["put", "get"] {
            let body = files.request(&format!("/{operation}?key={key}"), BUILDER, &[]);
            assert_eq!(body["status"], json!(400), "{operation} {key:?}: {body}");
            assert_eq!(
                app_body(&body),
                json!({ "failed": "invalid-key", "rule": rule }),
                "{operation} {key:?}: expected the rule the installation names, got {body}"
            );
        }
    }
    assert_eq!(
        tree(&files.folder()),
        before,
        "expected nothing written or removed in the folder by refused keys"
    );
    assert!(
        !outside_of_folder.exists(),
        "expected nothing written beside the folder through `..`"
    );
}

#[test]
fn the_local_storage_is_registered_with_the_contact_of_storage_json() {
    let files = Files::start("contact-board", true);
    let file = files
        .dir()
        .join(format!("resources/{MARKETING}/storage.json"));
    fs::write(&file, stale_storage_json(MARKETING, "Zed Elsewhere, #zed")).unwrap();
    let refused = files.request("/get?key=x.txt", OUTSIDER, &[]);
    assert_eq!(refused["status"], json!(403), "{refused}");
    assert_eq!(
        app_body(&refused)["refusal"]["contact"],
        json!("Zed Elsewhere, #zed"),
        "expected the contact of storage.json, not the installation's, got {refused}"
    );
    assert!(
        app_body(&refused)["fix"]
            .as_str()
            .is_some_and(|f| f.contains("Zed Elsewhere, #zed")),
        "expected the fix to name the contact of storage.json\n{refused}"
    );
}

/// A folder outside the project holding folders and files of its own: the
/// target of the links of [`a_link_in_the_local_state_is_not_followed`].
fn target_of_links(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let folder = root.join("link-targets/folder");
    fs::create_dir_all(folder.join("inner/deep")).unwrap();
    fs::write(folder.join("top.txt"), "kept\n").unwrap();
    fs::write(folder.join("inner/deep/file.txt"), "kept too\n").unwrap();
    fs::write(folder.join(".hidden"), "kept as well\n").unwrap();
    let file = root.join("link-targets/file.txt");
    fs::write(&file, "a file, kept\n").unwrap();
    (folder, file)
}

#[test]
fn a_link_in_the_local_state_is_not_followed_by_a_run_or_a_reset() {
    let files = Files::start("links-board", true);
    let (folder_target, file_target) = target_of_links(files.local.machine.root());
    let targets = folder_target.parent().unwrap().to_path_buf();
    for link in [
        ".vibe",
        ".vibe/local",
        ".vibe/local/storage",
        ".vibe/local/storage/marketing",
    ] {
        for (kind, target) in [("a folder", &folder_target), ("a file", &file_target)] {
            let context = format!("{link} a link to {kind}");
            let state = files.dir().join(".vibe");
            match fs::symlink_metadata(&state) {
                Ok(m) if m.file_type().is_symlink() => fs::remove_file(&state).unwrap(),
                Ok(_) => fs::remove_dir_all(&state).unwrap(),
                Err(_) => {}
            }
            let at = files.dir().join(link);
            fs::create_dir_all(at.parent().unwrap()).unwrap();
            symlink(target, &at).unwrap();
            let before = Tree::of(&targets);
            for command in ["request", "reset"] {
                let run = if command == "request" {
                    files.local.request(files.dir(), "/", BUILDER, &[])
                } else {
                    files.local.watch(files.dir());
                    files.local.run(
                        &[
                            "local",
                            "reset",
                            "--project",
                            files.dir().to_str().unwrap(),
                            "--json",
                        ],
                        &[],
                    )
                };
                let message = run.json_error(1, "local-state");
                assert!(
                    message.contains(link) && message.to_lowercase().contains("remov"),
                    "{context}, {command}: expected the message to name the link {link:?} and say to remove it, got {message:?}"
                );
                let lower = message.to_lowercase();
                assert!(
                    !lower.contains("anew") && !lower.contains("start the local databases"),
                    "{context}, {command}: expected the message not to say the local databases are started anew, got {message:?}"
                );
                assert_eq!(
                    before.differences(&Tree::of(&targets)),
                    Vec::<String>::new(),
                    "{context}, {command}: expected what the link points to untouched"
                );
                assert!(
                    fs::symlink_metadata(&at).is_ok_and(|m| m.file_type().is_symlink()),
                    "{context}, {command}: expected the link still there"
                );
            }
        }
    }
}

/// Makes `resources/marketing/storage.json` something that is there and
/// is not a file the tool wrote and a run can read.
fn spoil(files: &Files, how: &str) {
    let file = files
        .dir()
        .join(format!("resources/{MARKETING}/storage.json"));
    let good = fs::read_to_string(&file).unwrap();
    match how {
        "unreadable" => fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap(),
        "not generated" => fs::write(
            &file,
            good.replace("vibe pull. Do not edit.", "written by hand"),
        )
        .unwrap(),
        "not JSON" => fs::write(&file, "this is not JSON at all\n").unwrap(),
        "too large" => {
            // The tool's own file, valid JSON, and a long extra member:
            // 100 KiB, past the 64 KiB no file of the tool's is.
            let mut body: Value = serde_json::from_str(&good).unwrap();
            body["padding"] = json!("x".repeat(100 * 1024));
            fs::write(
                &file,
                format!("{}\n", serde_json::to_string_pretty(&body).unwrap()),
            )
            .unwrap();
        }
        "a link" => {
            let outside = files.local.machine.root().join("linked-storage-json");
            fs::create_dir_all(&outside).unwrap();
            let target = outside.join("storage.json");
            fs::write(&target, &good).unwrap();
            fs::remove_file(&file).unwrap();
            symlink(&target, &file).unwrap();
        }
        other => panic!("unknown way to spoil: {other}"),
    }
}

#[test]
fn a_storage_json_that_is_there_and_not_usable_is_a_warning_and_the_folder_stays() {
    for how in [
        "unreadable",
        "not generated",
        "not JSON",
        "too large",
        "a link",
    ] {
        if how == "unreadable" && my_uid() == 0 {
            // Mode 000 does not stop root.
            continue;
        }
        let files = Files::start("unusable-board", true);
        files.request("/put?key=kept.txt", BUILDER, &[]);
        place(&files.folder().join("hand/seed.txt"), b"placed by hand");
        let before = tree(&files.folder());
        spoil(&files, how);
        let body = files.request("/get?key=hand/seed.txt", BUILDER, &[]);
        assert_eq!(
            app_body(&body)["refusal"]["reason"],
            json!("not-registered"),
            "{how}: expected the Resource not registered for that run, got {body}"
        );
        let warnings = body["warnings"].as_array().cloned().unwrap_or_default();
        let unusable: Vec<&Value> = warnings
            .iter()
            .filter(|w| w["reason"] == "storage-file-unusable")
            .collect();
        assert!(
            unusable.len() == 1
                && unusable[0].to_string().contains("storage.json")
                && unusable[0].to_string().contains(MARKETING),
            "{how}: expected one warning `storage-file-unusable` naming the file and the Resource, got {body}"
        );
        assert_eq!(
            tree(&files.folder()),
            before,
            "{how}: expected the folder, with the file placed in it, left as it is"
        );
        assert_eq!(
            fs::read(files.folder().join("hand/seed.txt")).unwrap(),
            b"placed by hand".as_slice(),
            "{how}: expected the placed file as it was"
        );
        // `vibe local reset` leaves that Resource's folder as it is too.
        let reset = reset_json(&files);
        assert_eq!(reset["reset"], json!(true), "{how}: {reset}");
        assert_eq!(
            tree(&files.folder()),
            before,
            "{how}: expected the folder, with the file placed in it, left as it is by `vibe local reset`"
        );
        assert_eq!(
            fs::read(files.folder().join("hand/seed.txt")).unwrap(),
            b"placed by hand".as_slice(),
            "{how}: expected the placed file as it was after the reset"
        );
        // Put right again, the Resource is registered and the file is there.
        let file = files
            .dir()
            .join(format!("resources/{MARKETING}/storage.json"));
        if how == "unreadable" {
            fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        } else {
            fs::remove_file(&file).unwrap();
            fs::write(&file, stale_storage_json(MARKETING, FILES_CONTACT)).unwrap();
        }
        let again = files.request("/get?key=hand/seed.txt", BUILDER, &[]);
        assert_eq!(
            app_body(&again)["file"]["text"],
            json!("placed by hand"),
            "{how}: expected the file there once storage.json is the tool's again\n{again}"
        );
        assert_storages(&again, &[(MARKETING, false)], "the run after");
    }
}

#[test]
fn a_resource_with_both_kinds_of_pulled_files_is_taken_as_the_kind_the_manifest_declares() {
    let files = Files::start("both-board", true);
    // A database's pulled files beside `storage.json`, each the tool's.
    let folder = files.dir().join(format!("resources/{MARKETING}"));
    fs::write(
        folder.join("schema.ts"),
        "// Generated by vibe pull. Do not edit.\n// Resource: marketing  Fingerprint: sha256:0000000000000000000000000000000000000000000000000000000000000000\nexport {};\n",
    )
    .unwrap();
    fs::write(
        folder.join("index.ts"),
        "// Generated. Do not edit.\nexport {};\n",
    )
    .unwrap();
    // The Manifest declares `marketing` as `storage`: the run takes it as
    // one, and its operations work.
    let put = files.request("/put?key=both.txt", BUILDER, &[]);
    assert_eq!(put["status"], json!(200), "{put}");
    assert_storages(
        &put,
        &[(MARKETING, true)],
        "both kinds of files, declared storage",
    );
    assert_eq!(
        put["standins"],
        json!([]),
        "expected no stand-in made for a Resource declared `storage`, got {put}"
    );
}

#[test]
fn the_rebuilt_line_in_words_has_one_line_for_each_storage() {
    let files = Files::start("dev-words-board", true);
    let dev = Dev::start_on_a_free_port(&files.local, files.dir(), "dev-words-board", false);
    let mut browser = dev.browser();
    browser.choose(BUILDER);
    let seen = dev.stdout().len();
    let index = files.dir().join("src/index.ts");
    let code = fs::read_to_string(&index).unwrap();
    fs::write(&index, format!("{code}\n// a change to rebuild by\n")).unwrap();
    let got = browser.get("/");
    assert_eq!(got.status, 200, "{}", got.describe());
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let words = dev.stdout();
        let after = words.get(seen..).unwrap_or_default();
        if after.lines().any(|l| {
            l.contains(MARKETING)
                && l.contains(".vibe/local/storage/marketing")
                && !l.contains("GET")
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "expected a line for the Storage Resource in the words of the rebuild, got\n{after}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    dev.stop("INT").expect_code(0);
}

#[test]
fn a_storage_json_just_under_64_kib_is_the_tools_own_for_the_local_run() {
    let files = Files::start("long-contact-board", true);
    let contact = "c".repeat(65_000);
    let file = files
        .dir()
        .join(format!("resources/{MARKETING}/storage.json"));
    fs::write(&file, stale_storage_json(MARKETING, &contact)).unwrap();
    assert!(
        fs::metadata(&file).unwrap().len() < 65_536,
        "the harness's own check: under 64 KiB"
    );
    let refused = files.request("/get?key=x.txt", OUTSIDER, &[]);
    assert_eq!(refused["status"], json!(403), "{refused}");
    let refusal = &app_body(&refused)["refusal"];
    assert_eq!(refusal["reason"], json!("no-grant"), "{refused}");
    assert_eq!(
        refusal["contact"],
        json!(contact),
        "expected the file the tool's own: registered with its contact"
    );
    assert!(
        refused["warnings"]
            .as_array()
            .is_none_or(|w| w.iter().all(|w| w["reason"] != "storage-file-unusable")),
        "expected no warning that the file is unusable\n{refused}"
    );
}

#[test]
fn the_test_user_page_takes_the_kind_the_manifest_declares_when_both_kinds_of_files_are_there() {
    // CONTRACT.md: "taken as the kind the Manifest declares for `<name>`,
    // by the local run and by the page `/_vm/test-user` alike". A Grant at
    // `structure` holds nothing on a Storage Resource and is the level of
    // a database that rows cannot be read at: the page says which.
    let files = Files::start("page-both-board", true);
    let folder = files.dir().join(format!("resources/{MARKETING}"));
    fs::write(
        folder.join("schema.ts"),
        "// Generated by vibe pull. Do not edit.\n// Resource: marketing  Fingerprint: sha256:0000000000000000000000000000000000000000000000000000000000000000\nexport {};\n",
    )
    .unwrap();
    fs::write(
        folder.join("index.ts"),
        "// Generated. Do not edit.\nexport {};\n",
    )
    .unwrap();
    let mut file = dev_json(files.dir());
    file["users"].as_array_mut().unwrap().push(json!({
        "id": "user-str", "displayName": "user-str", "email": "user-str@example.com", "groups": ["viewers"]
    }));
    file["grants"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "group": "viewers", "resource": MARKETING, "access": "structure" }));
    write_dev_json(files.dir(), &file);
    let dev = Dev::start(&files.local, files.dir(), "page-both-board");
    let mut browser = dev.browser();
    let page = browser.get("/_vm/test-user");
    assert_eq!(page.status, 200, "{}", page.describe());
    let document = blackbox::page::Document::parse(&page.text());
    let plain = document.root().text();
    let ids = ["user-str", "builder", "outsider"];
    let part = segment_of(&plain, "user-str", &ids);
    let row = segment_of(part, MARKETING, &[MARKETING]);
    let words: Vec<&str> = row.split(|c: char| !c.is_ascii_alphanumeric()).collect();
    assert!(
        words.contains(&"none") && !words.contains(&"structure"),
        "expected the page to take marketing as a Storage Resource (its Manifest says `storage`): a Grant at `structure` gives `none`, got\n{row}"
    );
    dev.stop("INT").expect_code(0);
}
