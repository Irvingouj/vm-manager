//! What refuses a change of a Storage Resource's location (CONTRACT.md,
//! "Resources and Grants in full: A Storage Resource's location", "Who
//! may"): the directory is checked as at registration, in its order and
//! with its reasons, except that the Resource's own directory is not
//! another Resource's; a refused change leaves nothing in the new directory
//! and changes nothing in the old one; `409` `wrong-kind` for a database,
//! `400` `malformed`, `413` before `403`, `403` `not-an-administrator`,
//! `404`; no answer or record holds the path.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use blackbox::resources::{admin_resource, location_body, put_location, records_waited};
use blackbox::scenario::administrator;
use blackbox::shared_database::token;
use blackbox::storage::{MARKETING, Storage, get, set_mode, terminate, tree};
use blackbox::{Request, Response, User, assert_api_error};
use serde_json::{Value, json};

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn change(s: &Storage, name: &str, directory: &str) -> Response {
    put_location(&s.admin(), name, &location_body(directory))
}

fn location_of(s: &Storage, name: &str) -> Value {
    admin_resource(&s.admin(), name).json()["location"].clone()
}

/// `MARKETING` registered on `root/marketing`, with a file in it.
fn world() -> Storage {
    let s = Storage::start();
    s.register(MARKETING);
    std::fs::write(s.dir(MARKETING).join("kept.txt"), b"kept").unwrap();
    s
}

/// The change of `name` to `directory`, which must be refused with `status`
/// and `reason`, leave the new directory as it was (`look_at`), the old one
/// as it was, and the Resource's location unchanged.
fn assert_refused(
    s: &Storage,
    name: &str,
    directory: &str,
    status: u16,
    reason: &str,
    look_at: &Path,
    what: &str,
) -> String {
    let old = s.dir(name);
    let (before_new, before_old, location) = (tree(look_at), tree(&old), location_of(s, name));
    let answer = change(s, name, directory);
    assert!(
        answer.status == status,
        "{what}: expected `{status} {reason}`, got\n{}",
        answer.describe()
    );
    assert_api_error(&answer, status, reason);
    assert_eq!(
        tree(look_at),
        before_new,
        "{what}: expected a refused change to leave nothing in the new directory"
    );
    assert_eq!(
        tree(&old),
        before_old,
        "{what}: expected a refused change to change nothing in the old directory"
    );
    assert_eq!(
        location_of(s, name),
        location,
        "{what}: expected the location unchanged"
    );
    answer.text()
}

#[test]
fn a_directory_outside_the_storage_root_is_refused_in_every_form() {
    let s = world();
    let root = s.root();
    let r = root.to_string_lossy().into_owned();
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::create_dir_all(root.join("other")).unwrap();
    let prefixed = s.place.base.join("root-other");
    std::fs::create_dir_all(&prefixed).unwrap();
    let cases = [
        ("relative".to_owned(), "a relative path"),
        ("root/other".to_owned(), "a relative path to a real one"),
        (r.clone(), "the storage root itself"),
        (format!("{r}/"), "the storage root, with a slash"),
        (
            s.place.outside().to_string_lossy().into_owned(),
            "a sibling of the root",
        ),
        (
            prefixed.to_string_lossy().into_owned(),
            "a path the root's text is a prefix of",
        ),
        ("/etc".to_owned(), "/etc"),
        ("/".to_owned(), "/"),
        (format!("{r}/../outside"), "a `..` leading out"),
        (format!("{r}/a/../other"), "a `..` that stays inside"),
        (format!("{r}/./other"), "a `.` in it"),
        (format!("{r}//other"), "an empty segment"),
        (format!("{r}/other/."), "a `.` at its end"),
        (format!("{r}/other/.."), "a `..` at its end"),
        (format!("{r}/other//"), "two slashes at its end"),
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

/// "A Storage Resource's location": the directory is taken as a
/// registration takes it: one `/` at its end is dropped (the Resource's own
/// directory written so is the same directory), and no more.
#[test]
fn the_own_directory_with_one_slash_at_its_end_is_the_same_and_with_two_is_outside_the_root() {
    let s = world();
    let own = s.dir(MARKETING).to_string_lossy().into_owned();
    let same = change(&s, MARKETING, &format!("{own}/"));
    assert_eq!(same.status, 200, "{}", same.describe());
    assert_eq!(same.json()["changed"], json!(false), "{}", same.describe());
    assert_refused(
        &s,
        MARKETING,
        &format!("{own}//"),
        422,
        "outside-storage-root",
        &s.place.base,
        "the Resource's own directory with two slashes at its end",
    );
}

/// "Registering a Storage Resource": a segment that is one of the program's
/// own names (`.vibe-resource`, `.vibe-staging`) is not taken, at a change
/// of location as at a registration, whatever the segment is under.
#[test]
fn a_directory_with_one_of_the_programs_own_names_as_a_segment_is_outside_the_storage_root() {
    let s = world();
    let root = s.root();
    let own = s.dir(MARKETING);
    for made in [
        root.join("x/.vibe-staging"),
        own.join(".vibe-staging/y"),
        root.join(".vibe-resource/z"),
    ] {
        std::fs::create_dir_all(made).unwrap();
    }
    for (directory, what) in [
        (
            root.join("x/.vibe-staging"),
            "`.vibe-staging` under another directory",
        ),
        (
            own.join(".vibe-staging"),
            "the Resource's own `.vibe-staging`",
        ),
        (
            own.join(".vibe-staging/y"),
            "beneath the Resource's own `.vibe-staging`",
        ),
        (root.join(".vibe-resource/z"), "beneath a `.vibe-resource`"),
    ] {
        assert_refused(
            &s,
            MARKETING,
            &directory.to_string_lossy(),
            422,
            "outside-storage-root",
            &s.place.base,
            what,
        );
    }
}

#[test]
fn each_refusal_of_the_directory_is_its_own() {
    let s = world();
    let root = s.root();
    let real = root.join("real");
    std::fs::create_dir_all(real.join("inner")).unwrap();
    symlink(&real, root.join("link")).unwrap();
    symlink(s.place.outside(), root.join("out")).unwrap();
    for (directory, what) in [
        (root.join("link"), "a link to a directory inside the root"),
        (
            root.join("link").join("inner"),
            "a component that is a link",
        ),
        (root.join("out"), "a link leading out of the root"),
    ] {
        assert_refused(
            &s,
            MARKETING,
            &directory.to_string_lossy(),
            422,
            "directory-link",
            &s.place.base,
            what,
        );
    }
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
    if blackbox::storage::my_uid() != 0 {
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
}

#[test]
fn another_resources_directory_inside_one_or_holding_one_is_in_use() {
    let s = world();
    let root = s.root();
    let beta = root.join("teams").join("beta");
    std::fs::create_dir_all(beta.join("sub")).unwrap();
    s.register_on("beta", &beta);
    for (directory, what) in [
        (beta.clone(), "the directory of another Resource"),
        (beta.join("sub"), "a directory inside another Resource's"),
        (root.join("teams"), "a directory holding another Resource's"),
    ] {
        assert_refused(
            &s,
            MARKETING,
            &directory.to_string_lossy(),
            409,
            "directory-in-use",
            &s.place.base,
            what,
        );
    }
    // A `.vibe-resource` naming another Resource, copied.
    let copied = root.join("copied");
    std::fs::create_dir_all(&copied).unwrap();
    std::fs::copy(beta.join(".vibe-resource"), copied.join(".vibe-resource")).unwrap();
    assert_refused(
        &s,
        MARKETING,
        &copied.to_string_lossy(),
        409,
        "directory-in-use",
        &s.place.base,
        "a directory whose `.vibe-resource` names another Resource",
    );
    // The other way: beta's own change to marketing's directory.
    let answer = change(&s, "beta", &s.dir(MARKETING).to_string_lossy());
    assert_api_error(&answer, 409, "directory-in-use");
}

#[test]
fn refusals_come_in_the_tables_order() {
    let s = world();
    let root = s.root();
    let r = root.to_string_lossy().into_owned();
    assert_refused(
        &s,
        MARKETING,
        &format!("{r}/../nowhere"),
        422,
        "outside-storage-root",
        &s.place.base,
        "outside and missing",
    );
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
    if blackbox::storage::my_uid() != 0 {
        let beta = root.join("beta");
        std::fs::create_dir_all(&beta).unwrap();
        s.register_on("beta", &beta);
        set_mode(&beta, 0o555);
        assert_refused(
            &s,
            MARKETING,
            &beta.to_string_lossy(),
            422,
            "directory-not-writable",
            &s.place.base,
            "in use and not writable",
        );
        set_mode(&beta, 0o755);
    }
}

#[test]
fn a_program_started_without_a_storage_root_refuses_a_change_as_it_refuses_a_registration() {
    let s = world();
    let Storage { vm, place } = s;
    let stopped = terminate(vm);
    let s = Storage {
        vm: stopped.start_with_root(None),
        place,
    };
    let new = s.root().join("new");
    std::fs::create_dir_all(&new).unwrap();
    let before = tree(&new);
    let answer = change(&s, MARKETING, &new.to_string_lossy());
    assert_api_error(&answer, 422, "storage-not-configured");
    assert_eq!(tree(&new), before, "nothing written");
    assert_api_error(
        &s.try_register("beta", &new.to_string_lossy()),
        422,
        "storage-not-configured",
    );
}

#[test]
fn a_refused_change_leaves_the_old_location_serving() {
    let s = Storage::ready();
    s.put_ok("a.txt", "kept", None);
    let answer = change(&s, MARKETING, "/etc");
    assert_api_error(&answer, 422, "outside-storage-root");
    let seen = s.as_writer(&get(MARKETING, "a.txt"));
    assert_eq!(seen["ok"]["size"], json!(4), "{seen}");
    s.put_ok("b.txt", "still the old directory", None);
    assert!(s.dir(MARKETING).join("b.txt").is_file());
    let records = records_waited(&s.vm, "resource-location-changed", 0);
    assert!(
        records.is_empty(),
        "no record for a refused change: {records:?}"
    );
}

#[test]
fn a_database_resource_is_wrong_kind() {
    let s = Storage::start();
    s.register_postgres("devops");
    let answer = change(&s, "devops", &s.dir("x").to_string_lossy());
    assert_api_error(&answer, 409, "wrong-kind");
    let message = answer.json()["error"]["message"]
        .as_str()
        .unwrap()
        .to_lowercase();
    assert!(
        message.contains("storage") || message.contains("database"),
        "expected the message to say which kind it is, got {message:?}"
    );
    // Nothing of the platform's own was written in the directory.
    assert_eq!(answer.status, 409);
}

fn send(s: &Storage, user: Option<&User>, name: &str, body: Option<&str>) -> Response {
    let mut request: Request =
        s.vm.api_request(&format!("/v1/resources/{name}/location"))
            .method("PUT");
    if let Some(user) = user {
        request = request.as_user(user);
    }
    if let Some(body) = body {
        request = request
            .header("content-type", "application/json")
            .body(body);
    }
    request.send()
}

#[test]
fn a_body_that_is_not_a_location_is_malformed_whatever_the_directory() {
    let s = world();
    let admin = administrator();
    let dir = s.root().join("elsewhere");
    std::fs::create_dir_all(&dir).unwrap();
    let d = dir.to_string_lossy();
    for (what, body) in [
        ("no body", String::new()),
        ("not JSON", "not json".to_owned()),
        ("an array", "[]".to_owned()),
        ("a string", "\"x\"".to_owned()),
        ("an empty object", "{}".to_owned()),
        (
            "no directory",
            json!({ "backend": "directory" }).to_string(),
        ),
        ("no backend", json!({ "directory": d }).to_string()),
        (
            "another backend",
            json!({ "backend": "s3", "directory": d }).to_string(),
        ),
        (
            "a backend that is not a string",
            json!({ "backend": 1, "directory": d }).to_string(),
        ),
        (
            "a directory that is not a string",
            json!({ "backend": "directory", "directory": 7 }).to_string(),
        ),
        (
            "a directory that is null",
            json!({ "backend": "directory", "directory": null }).to_string(),
        ),
        (
            "a directory that is a list",
            json!({ "backend": "directory", "directory": [d] }).to_string(),
        ),
    ] {
        let answer = send(&s, Some(&admin), MARKETING, Some(&body));
        assert_api_error(&answer, 400, "malformed");
        assert!(
            !answer.text().contains("elsewhere"),
            "{what}: no part of what was sent in the message"
        );
    }
    assert_eq!(
        location_of(&s, MARKETING)["directory"],
        json!(s.dir(MARKETING).to_string_lossy()),
        "nothing changed"
    );
    // A body at its bound is taken, one past it is `413`, whoever sends it.
    let at = format!(
        "{}{}",
        json!({ "backend": "directory", "directory": d }),
        " ".repeat(
            64 * 1024
                - json!({ "backend": "directory", "directory": d })
                    .to_string()
                    .len()
        )
    );
    assert_eq!(at.len(), 64 * 1024);
    let ok = send(&s, Some(&admin), MARKETING, Some(&at));
    assert_eq!(ok.status, 200, "{}", ok.describe());
    let over = " ".repeat(64 * 1024 + 1);
    assert_api_error(
        &send(&s, Some(&admin), MARKETING, Some(&over)),
        413,
        "too-large",
    );
    assert_api_error(
        &send(&s, Some(&alice()), MARKETING, Some(&over)),
        413,
        "too-large",
    );
}

#[test]
fn only_a_platform_administrator_and_the_order_of_what_refuses() {
    let s = world();
    let dir = s.root().join("elsewhere");
    std::fs::create_dir_all(&dir).unwrap();
    let good = json!({ "backend": "directory", "directory": dir.to_string_lossy() }).to_string();
    let before = location_of(&s, MARKETING);
    let first = send(&s, Some(&alice()), MARKETING, Some(&good));
    assert_api_error(&first, 403, "not-an-administrator");
    // The same answer whatever the body and whatever the Resource.
    for (name, body) in [
        (MARKETING, "not json"),
        (MARKETING, "{}"),
        ("ghost", good.as_str()),
        ("Not_A_Name", good.as_str()),
    ] {
        let answer = send(&s, Some(&alice()), name, Some(body));
        assert_api_error(&answer, 403, "not-an-administrator");
        assert_eq!(answer.body(), first.body(), "{name}");
    }
    assert_eq!(location_of(&s, MARKETING), before, "nothing changed");
    // The Resource's owner is refused as anyone else.
    let owner = User::new("The Marketing team");
    assert_api_error(
        &send(&s, Some(&owner), MARKETING, Some(&good)),
        403,
        "not-an-administrator",
    );
    // An administrator: a Resource nobody has is `404`, a name no Resource
    // could have too.
    let admin = administrator();
    assert_api_error(
        &send(&s, Some(&admin), "ghost", Some(&good)),
        404,
        "not-found",
    );
    assert_api_error(
        &send(&s, Some(&admin), "Not_A_Name", Some(&good)),
        404,
        "not-found",
    );
    assert_eq!(
        records_waited(&s.vm, "resource-location-changed", 0).len(),
        0,
        "no record for a refused request"
    );
}

#[test]
fn no_refusal_holds_the_directorys_path() {
    let s = world();
    let root = s.root();
    let t = token();
    let names: Vec<String> = ["missing", "file", "link", "outside"]
        .iter()
        .map(|n| format!("{n}-{t}"))
        .collect();
    std::fs::write(root.join(&names[1]), b"x").unwrap();
    symlink(root.join("anywhere"), root.join(&names[2])).unwrap();
    let mut texts: Vec<String> = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let path: PathBuf = if i == 3 {
            PathBuf::from(format!("/etc/{name}"))
        } else {
            root.join(name)
        };
        texts.push(change(&s, MARKETING, &path.to_string_lossy()).text());
    }
    texts.push(s.admin().get("/v1/operation-log").text());
    blackbox::storage::assert_nowhere(&s.vm, &texts, &t, "the directory's path", "the refusals");
}
