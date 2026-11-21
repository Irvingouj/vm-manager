//! Registering a Storage Resource, and Grants on it (CONTRACT.md, "Storage:
//! Registering a Storage Resource", "Grants on it"; ticket "Storage: a
//! shared, named file Resource, on a mounted directory now and S3 later":
//! a Platform administrator registers a Storage Resource on a mounted
//! directory; a directory that cannot be used is refused, saying why).
//!
//! Every refusal of the table, in its order where two apply at once; a
//! refused registration registers nothing and writes nothing; no answer,
//! message or log record holds the directory's path; a registration that
//! passes writes `.vibe-resource` and `.vibe-staging/`; a `.vibe-resource`
//! naming the same Resource is taken.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::os::unix::fs::symlink;
use std::path::Path;

use blackbox::api::{assert_json_content_type, assert_recent_rfc3339};
use blackbox::postgres::Platform;
use blackbox::scenario::administrator;
use blackbox::storage::{
    FILE_READERS, FILE_WRITERS, FILES_CONTACT, MARKETING, Place, Storage, postgres_body, set_mode,
    storage_body, tree,
};
use blackbox::{User, VmOptions, assert_api_error};
use serde_json::{Value, json};

fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

const PUBLIC_FACE: [&str; 7] = [
    "contact",
    "created_at",
    "created_by",
    "description",
    "kind",
    "name",
    "owner",
];

const AS_READ: [&str; 8] = [
    "contact",
    "created_at",
    "created_by",
    "description",
    "kind",
    "name",
    "owner",
    "your_access",
];

/// Fails unless `dir` holds `.vibe-resource` (a regular file) and
/// `.vibe-staging/` (an empty folder), and nothing else.
fn assert_marked(dir: &Path, context: &str) {
    let found = tree(dir);
    let marker = found.iter().any(|e| e.starts_with(".vibe-resource ("));
    assert!(
        marker && found.contains(&".vibe-staging/".to_owned()) && found.len() == 2,
        "{context}: expected the directory to hold `.vibe-resource` (a file) and an empty `.vibe-staging/`, and nothing else; it holds {found:?}"
    );
}

#[test]
fn an_administrator_registers_a_storage_resource_and_everyone_reads_its_face_without_its_location()
{
    let s = Storage::start();
    let dir = s.dir(MARKETING);
    std::fs::create_dir_all(&dir).unwrap();
    let body = storage_body(MARKETING, &dir.to_string_lossy());
    let created = s.admin().create_resource(&body);
    created.expect_status(201);
    assert_json_content_type(&created);
    let resource = created.json();
    assert_eq!(
        keys(&resource),
        PUBLIC_FACE,
        "expected {{ name, kind, description, owner, contact, created_by, created_at }} and nothing of `location`, got {resource}"
    );
    assert_eq!(resource["name"], MARKETING);
    assert_eq!(resource["kind"], "storage");
    assert_eq!(resource["description"], body["description"]);
    assert_eq!(resource["owner"], body["owner"]);
    assert_eq!(resource["contact"], FILES_CONTACT);
    assert_eq!(resource["created_by"], administrator().id);
    assert_recent_rfc3339(&resource["created_at"], 120, "the Resource's created_at");
    assert_marked(&dir, "after the registration");

    s.grant(MARKETING, FILE_READERS, "read");
    s.grant(MARKETING, FILE_WRITERS, "write");
    for (reader, access) in [
        (User::new("bob"), Value::Null),
        (User::new("rita").in_groups(&[FILE_READERS]), json!("read")),
        (
            User::new("wanda").in_groups(&[FILE_READERS, FILE_WRITERS]),
            json!("write"),
        ),
        (administrator(), Value::Null),
    ] {
        let one = s.vm.api_as(&reader).get_resource(MARKETING);
        one.expect_status(200);
        let mut one = one.json();
        assert_eq!(keys(&one), AS_READ, "{one}");
        assert_eq!(
            one["your_access"], access,
            "expected `your_access` {access} for {:?} (the highest level of their Groups' Grants), got {one}",
            reader.id
        );
        let listed = s.vm.api_as(&reader).list_resources().json();
        let in_list = listed["resources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == MARKETING)
            .cloned()
            .unwrap_or_else(|| panic!("expected {MARKETING} in the list, got {listed}"));
        assert_eq!(in_list, one, "expected the list to show it as GET does");
        one.as_object_mut().unwrap().remove("your_access");
        assert_eq!(one, resource);
    }
    let dir_text = dir.to_string_lossy().into_owned();
    for target in ["/v1/resources", &format!("/v1/resources/{MARKETING}")] {
        let text = s.admin().get(target).text();
        assert!(
            !text.contains(&dir_text) && !text.contains("location"),
            "expected nothing of the location in {target}, got {text}"
        );
    }
}

#[test]
fn a_grant_on_storage_is_read_or_write_and_structure_is_malformed() {
    let s = Storage::start();
    s.register(MARKETING);
    for access in ["read", "write"] {
        let answer = s.admin().grant(MARKETING, "team", access);
        answer.expect_status(200);
        assert_eq!(
            answer.json(),
            json!({ "resource": MARKETING, "group": "team", "access": access })
        );
    }
    let refused = s.admin().grant(MARKETING, "others", "structure");
    assert_api_error(&refused, 400, "malformed");
    let message = refused.json()["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        message.contains("read") && message.contains("write"),
        "expected the message to say that a Storage Resource has two levels, `read` and `write`, got {message:?}"
    );
    let grants = s.admin().list_grants(MARKETING);
    grants.expect_status(200);
    assert_eq!(
        grants.json(),
        json!({ "grants": [ { "group": "team", "access": "write" } ] }),
        "expected the refused Grant not written, and the list as for a database"
    );
    // A User holds the highest level of their Groups' Grants.
    s.grant(MARKETING, "readers-too", "read");
    let both = User::new("both").in_groups(&["readers-too", "team"]);
    assert_eq!(
        s.vm.api_as(&both).get_resource(MARKETING).json()["your_access"],
        "write"
    );
}

#[test]
fn a_kind_or_a_location_of_the_wrong_shape_is_malformed() {
    let s = Storage::start();
    let dir = s.dir(MARKETING);
    std::fs::create_dir_all(&dir).unwrap();
    let directory = dir.to_string_lossy().into_owned();
    let good = storage_body(MARKETING, &directory);
    let connection = postgres_body("x")["connection"].clone();
    let mut cases: Vec<(&str, Value)> = Vec::new();
    let mut with = |what: &'static str, change: &dyn Fn(&mut Value)| {
        let mut body = good.clone();
        change(&mut body);
        cases.push((what, body));
    };
    with("kind s3", &|b| b["kind"] = json!("s3"));
    with("kind missing", &|b| {
        b.as_object_mut().unwrap().remove("kind");
    });
    with("kind a number", &|b| b["kind"] = json!(1));
    with("storage with a connection beside its location", &|b| {
        b["connection"] = connection.clone();
    });
    with("storage with a connection and no location", &|b| {
        b["connection"] = connection.clone();
        b.as_object_mut().unwrap().remove("location");
    });
    with("storage with neither", &|b| {
        b.as_object_mut().unwrap().remove("location");
    });
    with("postgres with a location", &|b| {
        b["kind"] = json!("postgres")
    });
    with("postgres with both", &|b| {
        b["kind"] = json!("postgres");
        b["connection"] = connection.clone();
    });
    with("backend s3", &|b| b["location"]["backend"] = json!("s3"));
    with("backend missing", &|b| {
        b["location"].as_object_mut().unwrap().remove("backend");
    });
    with("directory a number", &|b| {
        b["location"]["directory"] = json!(5)
    });
    with("directory missing", &|b| {
        b["location"].as_object_mut().unwrap().remove("directory");
    });
    with("location a string", &|b| b["location"] = json!(directory));
    for (what, body) in &cases {
        let answer = s.admin().create_resource(body);
        assert!(
            answer.status == 400,
            "{what}: expected `400 malformed`, got\n{}",
            answer.describe()
        );
        assert_api_error(&answer, 400, "malformed");
    }
    s.admin().get_resource(MARKETING).expect_status(404);
    assert!(
        tree(&dir).is_empty(),
        "expected nothing written by a malformed registration, found {:?}",
        tree(&dir)
    );
}

#[test]
fn a_storage_resource_takes_a_names_rules_as_a_database_does() {
    let s = Storage::start();
    s.register_postgres("devops");
    let dir = s.dir("files");
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["Marketing", "1files", "files-x", "", &"f".repeat(64)] {
        let answer = s.try_register(name, &dir.to_string_lossy());
        assert_api_error(&answer, 400, "invalid-name");
    }
    let taken = s.try_register("devops", &dir.to_string_lossy());
    assert_api_error(&taken, 409, "name-taken");
    s.register_on("files", &dir);
    let other = s.dir("other");
    std::fs::create_dir_all(&other).unwrap();
    let again = s.try_register("files", &other.to_string_lossy());
    assert_api_error(&again, 409, "name-taken");
    assert!(
        tree(&other).is_empty(),
        "expected nothing written for a name taken, found {:?}",
        tree(&other)
    );
    let database = s.admin().create_resource(&postgres_body("files"));
    assert_api_error(&database, 409, "name-taken");
}

/// The registration of `name` on `directory`, which must be refused with
/// `status` and `reason`, register nothing and leave `look_at` as it was.
fn assert_refused(
    s: &Storage,
    name: &str,
    directory: &str,
    status: u16,
    reason: &str,
    look_at: &Path,
    what: &str,
) -> String {
    let before = tree(look_at);
    let answer = s.try_register(name, directory);
    assert!(
        answer.status == status,
        "{what}: expected `{status} {reason}`, got\n{}",
        answer.describe()
    );
    assert_api_error(&answer, status, reason);
    s.admin().get_resource(name).expect_status(404);
    assert_eq!(
        tree(look_at),
        before,
        "{what}: expected nothing written by a refused registration"
    );
    answer.text()
}

#[test]
fn a_directory_outside_the_storage_root_is_refused_in_every_form() {
    let s = Storage::start();
    let root = s.root();
    let r = root.to_string_lossy().into_owned();
    std::fs::create_dir_all(root.join("marketing")).unwrap();
    std::fs::create_dir_all(root.join("a")).unwrap();
    let sibling = s.place.outside();
    let prefixed = s.place.base.join("root-other");
    std::fs::create_dir_all(&prefixed).unwrap();
    let cases = [
        ("relative".to_owned(), "a relative path"),
        ("root/marketing".to_owned(), "a relative path"),
        (r.clone(), "the storage root itself"),
        (format!("{r}/"), "the storage root itself, with a slash"),
        (
            sibling.to_string_lossy().into_owned(),
            "a sibling of the root",
        ),
        (
            prefixed.to_string_lossy().into_owned(),
            "a path the root's text is a prefix of",
        ),
        ("/etc".to_owned(), "/etc"),
        ("/".to_owned(), "/"),
        (format!("{r}/../outside"), "a `..` leading out"),
        (format!("{r}/a/../marketing"), "a `..` that stays inside"),
        (format!("{r}/./marketing"), "a `.` in it"),
        (format!("{r}//marketing"), "an empty segment"),
        (format!("{r}/marketing/."), "a `.` at its end"),
        (format!("{r}/marketing/.."), "a `..` at its end"),
        (format!("{r}/marketing//"), "two slashes at its end"),
    ];
    for (directory, what) in &cases {
        assert_refused(
            &s,
            MARKETING,
            directory,
            422,
            "outside-storage-root",
            &s.place.base,
            what,
        );
    }
}

#[test]
fn each_refusal_of_the_directory_is_its_own() {
    let s = Storage::start();
    let root = s.root();
    // directory-link: the directory itself, and a component beneath the root.
    let real = root.join("real");
    std::fs::create_dir_all(real.join("inner")).unwrap();
    symlink(real, root.join("link")).unwrap();
    assert_refused(
        &s,
        MARKETING,
        &root.join("link").to_string_lossy(),
        422,
        "directory-link",
        &s.place.base,
        "a link to a directory inside the root",
    );
    assert_refused(
        &s,
        MARKETING,
        &root.join("link").join("inner").to_string_lossy(),
        422,
        "directory-link",
        &s.place.base,
        "a component that is a link",
    );
    symlink(s.place.outside(), root.join("out")).unwrap();
    assert_refused(
        &s,
        MARKETING,
        &root.join("out").to_string_lossy(),
        422,
        "directory-link",
        &s.place.base,
        "a link leading out of the root",
    );
    // directory-missing.
    assert_refused(
        &s,
        MARKETING,
        &root.join("not-there").to_string_lossy(),
        422,
        "directory-missing",
        &s.place.base,
        "a directory that is not there",
    );
    assert!(
        !root.join("not-there").exists(),
        "expected the program not to make it"
    );
    // not-a-directory.
    std::fs::write(root.join("file"), b"a file").unwrap();
    assert_refused(
        &s,
        MARKETING,
        &root.join("file").to_string_lossy(),
        422,
        "not-a-directory",
        &s.place.base,
        "a file",
    );
    // directory-not-writable.
    let locked = root.join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    set_mode(&locked, 0o555);
    assert_refused(
        &s,
        MARKETING,
        &locked.to_string_lossy(),
        422,
        "directory-not-writable",
        &s.place.base,
        "a directory the program cannot write in",
    );
    set_mode(&locked, 0o755);
}

#[test]
fn a_directory_of_another_resource_inside_one_or_holding_one_is_in_use() {
    let s = Storage::start();
    let root = s.root();
    let a = root.join("teams").join("a");
    std::fs::create_dir_all(a.join("sub")).unwrap();
    s.register_on("alpha", &a);
    for (directory, what) in [
        (a.clone(), "the directory of another Resource"),
        (a.join("sub"), "a directory inside another Resource's"),
        (root.join("teams"), "a directory holding another Resource's"),
    ] {
        assert_refused(
            &s,
            "beta",
            &directory.to_string_lossy(),
            409,
            "directory-in-use",
            &s.place.base,
            what,
        );
    }
    // A `.vibe-resource` naming another Resource: copied from alpha's.
    let copied = root.join("copied");
    std::fs::create_dir_all(&copied).unwrap();
    std::fs::copy(a.join(".vibe-resource"), copied.join(".vibe-resource")).unwrap();
    assert_refused(
        &s,
        "beta",
        &copied.to_string_lossy(),
        409,
        "directory-in-use",
        &s.place.base,
        "a directory whose `.vibe-resource` names another Resource",
    );
}

#[test]
fn refusals_come_in_the_tables_order() {
    let s = Storage::start();
    let root = s.root();
    let r = root.to_string_lossy().into_owned();
    // outside-storage-root before directory-missing.
    assert_refused(
        &s,
        MARKETING,
        &format!("{r}/../nowhere"),
        422,
        "outside-storage-root",
        &s.place.base,
        "outside and missing",
    );
    // directory-link before directory-missing: a link to nothing.
    symlink(root.join("nothing"), root.join("dangling")).unwrap();
    assert_refused(
        &s,
        MARKETING,
        &root.join("dangling").to_string_lossy(),
        422,
        "directory-link",
        &s.place.base,
        "a link to nothing",
    );
    // directory-not-writable before directory-in-use: another Resource's
    // directory, made read-only.
    let a = root.join("a");
    std::fs::create_dir_all(&a).unwrap();
    s.register_on("alpha", &a);
    set_mode(&a, 0o555);
    assert_refused(
        &s,
        "beta",
        &a.to_string_lossy(),
        422,
        "directory-not-writable",
        &s.place.base,
        "in use and not writable",
    );
    set_mode(&a, 0o755);
    // invalid-name and name-taken before the directory is looked at.
    let taken = s.try_register("alpha", &format!("{r}/../nowhere"));
    assert_api_error(&taken, 409, "name-taken");
    let invalid = s.try_register("Bad Name", "relative");
    assert_api_error(&invalid, 400, "invalid-name");
}

#[test]
fn no_answer_message_or_log_record_holds_the_directorys_path() {
    let s = Storage::start();
    let root = s.root();
    let token = blackbox::shared_database::token();
    let names: Vec<String> = ["ok", "missing", "file", "locked", "link"]
        .iter()
        .map(|n| format!("{n}-{token}"))
        .collect();
    std::fs::create_dir_all(root.join(&names[0])).unwrap();
    std::fs::write(root.join(&names[2]), b"x").unwrap();
    std::fs::create_dir_all(root.join(&names[3])).unwrap();
    set_mode(&root.join(&names[3]), 0o555);
    symlink(root.join(&names[0]), root.join(&names[4])).unwrap();
    let mut texts = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let answer = s.try_register(&format!("r{i}"), &root.join(name).to_string_lossy());
        texts.push(answer.text());
    }
    let outside = s.try_register("outside", &format!("/etc/{token}"));
    texts.push(outside.text());
    texts.push(s.admin().get("/v1/resources").text());
    texts.push(s.admin().get("/v1/resources/r0").text());
    texts.push(s.admin().get("/v1/operation-log").text());
    set_mode(&root.join(&names[3]), 0o755);
    blackbox::storage::assert_nowhere(
        &s.vm,
        &texts,
        &token,
        "the directory's path",
        "the registrations",
    );
}

#[test]
fn a_vibe_resource_naming_the_same_resource_is_taken() {
    // A registration whose answer was lost, asked again after the Resource
    // was removed from memory: the program's own `.vibe-resource` naming
    // this same Resource is taken. Seen from outside as a second
    // installation (a platform database of its own) on the same directory.
    let place = Place::new("again");
    let dir = place.root().join(MARKETING);
    std::fs::create_dir_all(&dir).unwrap();
    let root = place.root();
    let first = Storage::start_on(place, Some(&root), &VmOptions::new());
    first.register_on(MARKETING, &dir);
    let marker = std::fs::read(dir.join(".vibe-resource")).unwrap();
    let Storage { vm, place } = first;
    drop(vm);
    let second = Storage::start_on(
        place,
        Some(&root),
        &VmOptions::new().platform(&Platform::fresh()),
    );
    second.register_on(MARKETING, &dir);
    assert_marked(&dir, "after the second registration");
    let other = second.try_register("other", &dir.to_string_lossy());
    assert_api_error(&other, 409, "directory-in-use");
    assert_eq!(
        std::fs::read(dir.join(".vibe-resource")).unwrap(),
        marker,
        "expected the marker of the same Resource kept as it was"
    );
}

#[test]
fn one_slash_at_the_end_of_the_directory_is_dropped() {
    let s = Storage::start();
    let dir = s.dir(MARKETING);
    std::fs::create_dir_all(&dir).unwrap();
    let answer = s.try_register(MARKETING, &format!("{}/", dir.to_string_lossy()));
    assert!(
        answer.status == 201,
        "expected `<root>/marketing/` taken as `<root>/marketing`, got\n{}",
        answer.describe()
    );
    assert_marked(&dir, "after a registration with one slash at the end");
    let again = s.try_register("other", &dir.to_string_lossy());
    assert_api_error(&again, 409, "directory-in-use");
}
