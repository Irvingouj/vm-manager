//! What the tests of the Storage ticket's surfaces outside the raw
//! interface share (CONTRACT.md, "Storage: Pulling a Storage Resource,
//! checking and deploying", "The tool and the Dashboard", "The local
//! loop"; ticket "Storage: a shared, named file Resource, on a mounted
//! directory now and S3 later"):
//!
//! - [`GENERATED`], the marker inside `resources/<name>/storage.json`, and
//!   [`assert_storage_json`]: the file's four fields, two spaces a level, a
//!   line end;
//! - [`storage_declaring`], a Manifest declaring Resources with their
//!   kinds, and [`stale_storage_json`], a file that looks generated but
//!   says a contact of the past;
//! - [`assert_pull_printed`], what `vibe pull` prints for a Storage
//!   Resource, and [`assert_storages`], the `storages` the local loop's
//!   outputs gain;
//! - [`kind_of`], the `data-vm-kind` of a declared Resource's element on
//!   the Dashboard's page of one Application;
//! - [`storage_handler`] and [`Files`]: the local loop's project, whose
//!   Application reads and writes the files of a pulled Storage Resource
//!   on an installation with that Resource registered and the Builder
//!   granted `write` on it.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::dashboard::Page;
use crate::http::User;
use crate::local::Local;
use crate::project::{manifest_ts, new_project_in_repository};
use crate::sdk::TempProject;
use crate::storage::{FILE_WRITERS, MARKETING, Storage};

/// The `generated` marker of `resources/<name>/storage.json`: a file
/// without it was not written by the tool.
pub const GENERATED: &str = "vibe pull. Do not edit.";

/// A Manifest of `name` declaring these Resources as (name, kind, access
/// level).
pub fn storage_declaring(name: &str, resources: &[(&str, &str, &str)]) -> String {
    let entries: Vec<String> = resources
        .iter()
        .map(|(resource, kind, access)| {
            format!("{resource}: {{ kind: \"{kind}\", access: \"{access}\" }}")
        })
        .collect();
    manifest_ts(name, &format!("{{ {} }}", entries.join(", ")))
}

/// `resources/<name>/storage.json` as a file the tool could have written
/// (its `generated` is the marker's) but that says a contact of the past:
/// what the next pull sees as changed.
pub fn stale_storage_json(resource: &str, contact: &str) -> String {
    let body = json!({
        "generated": GENERATED,
        "resource": resource,
        "kind": "storage",
        "contact": contact,
    });
    format!(
        "{}\n",
        serde_json::to_string_pretty(&body).unwrap_or_default()
    )
}

/// Fails unless `text` is JSON written two spaces a level: every line
/// indented by spaces only, each line after one that opens an object or an
/// array two spaces further in.
pub fn assert_two_spaces_a_level(text: &str, context: &str) {
    let mut previous: Option<(usize, bool)> = None;
    for line in text.lines() {
        let trimmed = line.trim_start_matches(' ');
        let indent = line.len() - trimmed.len();
        assert!(
            !trimmed.starts_with('\t'),
            "{context}: expected no tab, at {line:?}\n{text}"
        );
        if let Some((before, opened)) = previous
            && opened
        {
            let closes_at_once = matches!(line.trim(), "}" | "]" | "}," | "],");
            if !closes_at_once {
                assert_eq!(
                    indent,
                    before + 2,
                    "{context}: expected two spaces a level, at {line:?}\n{text}"
                );
            }
        }
        let trimmed_end = line.trim_end();
        previous = Some((
            indent,
            trimmed_end.ends_with('{') || trimmed_end.ends_with('['),
        ));
    }
}

/// Fails unless the `storage.json` at `path` is the file `vibe pull` writes
/// for `resource` with this contact: exactly the four fields of the
/// contract, two spaces a level, and a line end.
pub fn assert_storage_json(path: &Path, resource: &str, contact: &str, context: &str) {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{context}: cannot read {}: {e}", path.display()));
    let body: Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{context}: expected JSON ({e}):\n{text}"));
    let mut keys: Vec<&str> = body
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["contact", "generated", "kind", "resource"],
        "{context}: expected exactly the four fields of the contract, got\n{text}"
    );
    assert_eq!(body["generated"], json!(GENERATED), "{context}:\n{text}");
    assert_eq!(body["resource"], json!(resource), "{context}:\n{text}");
    assert_eq!(body["kind"], json!("storage"), "{context}:\n{text}");
    assert_eq!(body["contact"], json!(contact), "{context}:\n{text}");
    assert!(
        text.ends_with('\n') && !text.ends_with("\n\n"),
        "{context}: expected one line end after the JSON, got\n{text}"
    );
    assert_two_spaces_a_level(&text, context);
}

/// Fails unless what `vibe pull <resource> --json` printed is what the
/// contract says for a Storage Resource: the fields of a database pull
/// with `fingerprint` null and `unsupported` empty, `files` the one file,
/// and `test_users` the Builder's own Grant.
pub fn assert_pull_printed(
    printed: &Value,
    resource: &str,
    access: &str,
    files_changed: bool,
    test_users_changed: bool,
) {
    let mut keys: Vec<&str> = printed
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    keys.sort_unstable();
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
        "expected the fields of a database pull and no other, got {printed}"
    );
    assert_eq!(printed["resource"], json!(resource), "{printed}");
    assert_eq!(printed["your_access"], json!(access), "{printed}");
    assert_eq!(
        printed["fingerprint"],
        Value::Null,
        "expected `fingerprint` null for a Storage Resource, got {printed}"
    );
    assert_eq!(
        printed["unsupported"],
        json!([]),
        "expected `unsupported` empty for a Storage Resource, got {printed}"
    );
    assert_eq!(
        printed["files"],
        json!([{
            "path": format!("resources/{resource}/storage.json"),
            "changed": files_changed,
        }]),
        "expected the one file a Storage Resource's pull writes, got {printed}"
    );
    assert_eq!(
        printed["test_users"],
        json!({
            "file": "vibe.dev.json",
            "changed": test_users_changed,
            "grant": { "group": "builder", "resource": resource, "access": access },
        }),
        "expected the Builder's own Grant copied into vibe.dev.json, as for a database, got {printed}"
    );
}

/// Fails unless `storages` (of `vibe request --json`, a `vibe dev` `rebuilt`
/// line or `vibe local reset --json`) is exactly these pulled Storage
/// Resources, by name, each `{ resource, directory, made }` with
/// `directory` relative to the project.
pub fn assert_storages(body: &Value, expected: &[(&str, bool)], context: &str) {
    let wanted: Vec<Value> = expected
        .iter()
        .map(|(resource, made)| {
            json!({
                "resource": resource,
                "directory": format!(".vibe/local/storage/{resource}"),
                "made": made,
            })
        })
        .collect();
    assert_eq!(
        body["storages"],
        json!(wanted),
        "{context}: expected `storages` to name every pulled Storage Resource by name, got {body}"
    );
}

/// The `data-vm-kind` of the element `data-vm-resource` names on a page of
/// the Dashboard: `postgres` or `storage`, or `None` when the element or
/// the attribute is not there.
pub fn kind_of(page: &Page, resource: &str) -> Option<String> {
    page.root()
        .with_attr("data-vm-resource")
        .iter()
        .find(|e| e.attr("data-vm-resource") == Some(resource))
        .and_then(|e| e.attr("data-vm-kind"))
        .map(str::to_owned)
}

/// The handler of the local Storage tests: an Application that reads and
/// writes the files of the Storage Resource its Manifest declares at
/// `write` through the SDK. Every route answers JSON:
///
/// - `/put?key=<key>`: a small string put at the key: `{ key, size,
///   contentType }`;
/// - `/get?key=<key>`: `{ file: null | { key, size, contentType, text } }`;
/// - `/head?key=<key>`: `{ info: null | { key, size, contentType } }`;
/// - `/list`: `{ keys, folders, cursor }`;
/// - `/delete?key=<key>`: `{ deleted: <key> }`;
/// - anything else: `{ user }`.
///
/// A refusal is answered `403` with `{ refusal, fix }`, as the local tests'
/// database handler answers it (`blackbox/src/local.rs`, `handler`); an
/// operation that failed without being refused (a key the rules refuse) is
/// answered `400` with `{ failed: <error.kind>, rule }`.
pub fn storage_handler() -> String {
    r#"import { defineApp, OperationFailed, Refused } from "@vm/sdk";
import manifest from "../manifest.ts";

export default defineApp(manifest, {
  async fetch(request, ctx) {
    const url = new URL(request.url);
    const key = url.searchParams.get("key") ?? "a.txt";
    const files = ctx.user().storage("marketing");
    try {
      switch (url.pathname) {
        case "/put": {
          const info = await files.put(key, "stored by the local run");
          return Response.json({ key: info.key, size: info.size, contentType: info.contentType });
        }
        case "/get": {
          const file = await files.get(key);
          return Response.json({ file: file === null ? null : {
            key: file.key,
            size: file.size,
            contentType: file.contentType,
            text: await file.text(),
          } });
        }
        case "/head": {
          const info = await files.head(key);
          return Response.json({ info: info === null ? null : {
            key: info.key,
            size: info.size,
            contentType: info.contentType,
          } });
        }
        case "/list": {
          const list = await files.list();
          return Response.json({ keys: list.files.map((f) => f.key), folders: list.folders, cursor: list.cursor });
        }
        case "/delete": {
          await files.delete(key);
          return Response.json({ deleted: key });
        }
        default:
          return Response.json({ user: ctx.user().id });
      }
    } catch (error) {
      if (error instanceof Refused) {
        return Response.json({ refusal: error.refusal, fix: error.fix }, { status: 403 });
      }
      if (error instanceof OperationFailed) {
        return Response.json({ failed: error.kind, rule: error.detail.rule ?? null }, { status: 400 });
      }
      throw error;
    }
  },
});
"#
    .to_owned()
}

/// The local Storage tests' world: an installation with [`MARKETING`]
/// registered on a directory of its own and [`FILE_WRITERS`] granted
/// `write` on it; a Builder's [`Local`] machine signed in to it as dana
/// (in [`FILE_WRITERS`]); and a project made by `vibe new`, named `name`,
/// declaring [`MARKETING`] at `write`, whose handler ([`storage_handler`])
/// reads and writes its files, pulled when `pulled` is true.
pub struct Files {
    pub name: String,
    pub local: Local,
    pub project: TempProject,
    pub storage: Storage,
}

impl Files {
    pub fn start(name: &str, pulled: bool) -> Self {
        let storage = Storage::start();
        storage.register(MARKETING);
        storage.grant(MARKETING, FILE_WRITERS, "write");
        let local = Local::new();
        local
            .machine
            .sign_in(&storage.vm, &User::new("dana").in_groups(&[FILE_WRITERS]));
        let project = new_project_in_repository(&local.machine, name);
        project.write(
            "manifest.ts",
            &storage_declaring(name, &[(MARKETING, "storage", "write")]),
        );
        project.write("src/index.ts", &storage_handler());
        if pulled {
            local.pull_ok(&project.dir, MARKETING);
        }
        Self {
            name: name.to_owned(),
            local,
            project,
            storage,
        }
    }

    /// Registers a second Storage Resource `second` on a directory of its
    /// own, grants [`FILE_WRITERS`] `write` on it, has the Manifest declare
    /// it beside [`MARKETING`] (the Manifest lists [`MARKETING`] first, so
    /// that the order of the names is not the order of declaring), and
    /// pulls it.
    pub fn with_second(&self, second: &str) {
        self.storage.register(second);
        self.storage.grant(second, FILE_WRITERS, "write");
        self.project.write(
            "manifest.ts",
            &storage_declaring(
                &self.name,
                &[
                    (MARKETING, "storage", "write"),
                    (second, "storage", "write"),
                ],
            ),
        );
        self.local.pull_ok(&self.project.dir, second);
    }

    /// The project's local Storage folder of `resource`.
    pub fn folder_of(&self, resource: &str) -> PathBuf {
        self.dir()
            .join(".vibe")
            .join("local")
            .join("storage")
            .join(resource)
    }

    pub fn dir(&self) -> &Path {
        &self.project.dir
    }

    /// The project's local Storage folder of [`MARKETING`].
    pub fn folder(&self) -> PathBuf {
        self.dir()
            .join(".vibe")
            .join("local")
            .join("storage")
            .join(MARKETING)
    }

    /// `vibe request <path> --as <user> --json <extra>`, which must end
    /// with `0`; what it printed.
    pub fn request(&self, path: &str, user: &str, extra: &[&str]) -> Value {
        self.local.request(self.dir(), path, user, extra).ok_json()
    }

    /// `vibe request <path> --as <user> <extra>` in words, which must end
    /// with `0`; what it printed.
    pub fn request_in_words(&self, path: &str, user: &str, extra: &[&str]) -> String {
        self.local
            .request_in_words(self.dir(), path, user, extra)
            .ok_text()
    }
}
