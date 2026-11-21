//! A Storage Resource in the Manifest, the Declared access and a Version
//! (CONTRACT.md, "Storage: The Manifest, Declared access and a Version"):
//! `POST /v1/applications/<name>/versions` takes `kind: "storage"` at
//! `read` or `write` and shows it as sent; `storage` at `structure`, a kind
//! that is neither, and `structures` naming a Resource declared `storage`
//! are `400 malformed`; the SDK's tooling prints the Declared access of a
//! Manifest declaring Storage and answers `manifest-invalid` for any other
//! kind or level, naming the two kinds.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::VmOptions;
use blackbox::admin::listed;
use blackbox::sdk::TempProject;
use blackbox::storage::{Place, Storage, version_body_with_kinds};
use blackbox::vm::INSTALLER;
use blackbox::{User, assert_api_error};
use serde_json::{Value, json};

#[test]
fn a_version_declaring_storage_at_read_or_write_is_taken_and_shown_as_sent() {
    // A Version declaring Storage needs no storage root: nothing is
    // registered.
    let s = Storage::start_on(Place::new("versions"), None, &VmOptions::new());
    let api = s.vm.api_as(&User::new(INSTALLER));
    api.create_application("files").expect_status(201);
    let body = version_body_with_kinds(
        "globalThis.default = { fetch() { return new Response('ok'); } };",
        &[
            ("marketing", "storage", "write"),
            ("archive", "storage", "read"),
            ("devops", "postgres", "read"),
        ],
    );
    let sent = api.send_version("files", &body);
    sent.expect_status(201);
    let declared = body["declared_access"].clone();
    assert_eq!(
        sent.json()["declared_access"],
        declared,
        "expected the Version's Declared access as sent"
    );
    assert_eq!(
        api.get_version("files", 1).json()["declared_access"],
        declared,
        "expected GET of the Version to show it as sent"
    );
    assert_eq!(
        listed(&s.vm, "files")["declared_access"],
        declared,
        "expected GET /v1/admin/applications to show it as sent"
    );
}

#[test]
fn storage_at_structure_a_kind_that_is_neither_and_structures_naming_storage_are_malformed() {
    // A Version declaring Storage needs no storage root: nothing is
    // registered.
    let s = Storage::start_on(Place::new("versions"), None, &VmOptions::new());
    let api = s.vm.api_as(&User::new(INSTALLER));
    api.create_application("files").expect_status(201);
    let bundle = "globalThis.default = { fetch() { return new Response('ok'); } };";
    let mut cases: Vec<(&str, Value)> = vec![
        (
            "storage at structure",
            version_body_with_kinds(bundle, &[("marketing", "storage", "structure")]),
        ),
        (
            "kind s3",
            version_body_with_kinds(bundle, &[("marketing", "s3", "read")]),
        ),
        (
            "kind Storage",
            version_body_with_kinds(bundle, &[("marketing", "Storage", "read")]),
        ),
        (
            "storage at admin",
            version_body_with_kinds(bundle, &[("marketing", "storage", "admin")]),
        ),
    ];
    let mut with_structures = version_body_with_kinds(bundle, &[("marketing", "storage", "read")]);
    with_structures["structures"] = json!({ "marketing": format!("sha256:{}", "a".repeat(64)) });
    cases.push((
        "structures naming a Resource declared storage",
        with_structures,
    ));
    for (what, body) in &cases {
        let answer = api.send_version("files", body);
        assert!(
            answer.status == 400,
            "{what}: expected `400 malformed`, got\n{}",
            answer.describe()
        );
        assert_api_error(&answer, 400, "malformed");
    }
    assert!(
        api.versions("files").is_empty(),
        "expected no Version made by a malformed body"
    );
    // A database declared beside it may still carry its structure.
    let mut mixed = version_body_with_kinds(
        bundle,
        &[
            ("marketing", "storage", "read"),
            ("devops", "postgres", "read"),
        ],
    );
    mixed["structures"] = json!({ "devops": format!("sha256:{}", "b".repeat(64)) });
    api.send_version("files", &mixed).expect_status(201);
}

const HEAD: &str = "import { defineManifest } from \"@vm/sdk\";\n";

fn project(resources: &str) -> TempProject {
    let p = TempProject::new("storage-manifest");
    p.write(
        "manifest.ts",
        &format!(
            "{HEAD}export default defineManifest({{\n  name: \"files\",\n  description: \"Files.\",\n  resources: {resources},\n}});\n"
        ),
    );
    p
}

#[test]
fn the_tooling_prints_a_manifest_declaring_storage() {
    let p = project(
        "{\n    devops: { kind: \"postgres\", access: \"read\" },\n    marketing: { kind: \"storage\", access: \"write\" },\n    archive: { kind: \"storage\", access: \"read\" },\n  }",
    );
    let run = p.manifest();
    assert_eq!(run.code, Some(0), "{}", run.describe());
    assert_eq!(
        run.json().unwrap_or_default(),
        json!({
            "name": "files",
            "description": "Files.",
            "declared_access": { "resources": {
                "devops": { "kind": "postgres", "access": "read" },
                "marketing": { "kind": "storage", "access": "write" },
                "archive": { "kind": "storage", "access": "read" },
            } },
        }),
        "{}",
        run.describe()
    );
}

#[test]
fn any_other_kind_or_level_is_manifest_invalid_naming_the_two_kinds() {
    for (resources, what) in [
        (
            "{ marketing: { kind: \"storage\", access: \"structure\" } }",
            "storage at structure",
        ),
        (
            "{ marketing: { kind: \"storage\", access: \"admin\" } }",
            "storage at admin",
        ),
        (
            "{ marketing: { kind: \"s3\", access: \"read\" } }",
            "kind s3",
        ),
        (
            "{ marketing: { kind: \"files\", access: \"write\" } }",
            "kind files",
        ),
    ] {
        let p = project(resources);
        let run = p.manifest();
        assert_eq!(run.code, Some(2), "{what}: {}", run.describe());
        let error = run.json().unwrap_or_default()["error"].clone();
        assert_eq!(
            error["reason"],
            "manifest-invalid",
            "{what}: {}",
            run.describe()
        );
        if what.starts_with("kind") {
            let message = error["message"].as_str().unwrap_or_default();
            assert!(
                message.contains("postgres") && message.contains("storage"),
                "{what}: expected the message to name the two kinds, got {message:?}"
            );
        }
    }
}
