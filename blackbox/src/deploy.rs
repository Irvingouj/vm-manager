//! What the tests of "Deploy: a Version built on the Contributor's machine
//! goes live" share (CONTRACT.md, "Deploying"):
//!
//! - a `git` that works on a [`Machine`] whose `HOME` is a folder of the
//!   test's own ([`git_binary`], [`tool_path`]): the first `git` on the
//!   test's `PATH` may be a wrapper that needs the real `HOME`, so the
//!   harness looks for one that answers `git --version` with no `HOME` at
//!   all and puts a link to it in a folder of the machine's own, first on
//!   the `PATH` the tool is given;
//! - `PATH`s that lack `git` or `node` on purpose ([`path_without_git`],
//!   [`path_without_node`]);
//! - a project's repository of its own, made and committed by the harness
//!   with no configuration of the user's or the system's read and the
//!   author and committer given in the environment ([`init_repository`],
//!   [`commit_all`], [`head`], [`git_ok`]), and `git` run as the tool runs
//!   it on the machine ([`machine_git`]);
//! - `vm deploy` run ([`deploy_args`], [`deploy`], [`deploy_with_env`],
//!   [`deploy_in_words`]) and what it printed read with its shape checked
//!   ([`Deployed::of`], [`DataLossAtDeploy::of`]);
//! - a project of the harness's own as a new one stands, under the
//!   repository's `target/` so that its packages resolve, with a name of
//!   the test's choosing, in a repository of its own and committed
//!   ([`deploy_project`]), and one whose code reads a shared database's
//!   pulled table ([`READS_INCIDENTS_TS`]);
//! - the project's tree without `.git` ([`project_tree`]), to say that a
//!   deploy wrote nothing in the project;
//! - the fingerprint a pulled definition's second line gives
//!   ([`fingerprint_in`]).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

use crate::program::run_to_exit;
use crate::project::{Tree, checked_project, manifest_ts};
use crate::sdk::{TempProject, Tooled};
use crate::tool::{Machine, Run};

/// How long a `vm deploy` may take before the harness gives up: above the
/// command's 120 s wait for the installation's answer, below the check's own
/// 300 s plus that.
pub const DEPLOY_TIMEOUT: Duration = Duration::from_secs(300);

/// The author and committer of every commit the harness makes.
pub const AUTHOR: &str = "Harness Builder";
pub const AUTHOR_EMAIL: &str = "builder@example.com";

/// The kinds of `git` the harness looks at, in order: every `git` on the
/// test's `PATH`, then the system's.
fn git_candidates() -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("git"))
                .collect()
        })
        .unwrap_or_default();
    found.push(PathBuf::from("/usr/bin/git"));
    found.push(PathBuf::from("/bin/git"));
    found
}

/// Whether `git` answers `--version` with an empty environment: no `HOME`,
/// no `PATH`.
fn works_without_home(git: &Path) -> bool {
    if !git.is_file() {
        return false;
    }
    let mut command = Command::new(git);
    command.arg("--version").env_clear();
    run_to_exit(command, Duration::from_secs(10))
        .is_ok_and(|f| f.status.is_some_and(|s| s.success()) && f.stdout.starts_with("git "))
}

/// A `git` that does not depend on `HOME`: the first on the test's `PATH`
/// (then `/usr/bin/git`, `/bin/git`) that answers `git --version` in an
/// empty environment. Fails the test when there is none.
pub fn git_binary() -> &'static Path {
    static GIT: OnceLock<PathBuf> = OnceLock::new();
    GIT.get_or_init(|| {
        git_candidates()
            .into_iter()
            .find(|g| works_without_home(g))
            .unwrap_or_else(|| {
                panic!(
                    "the harness needs a `git` that runs without HOME (looked on PATH and at /usr/bin/git, /bin/git); found none"
                )
            })
    })
}

/// The `node` on the test's `PATH`, resolved to its file.
pub fn node_binary() -> PathBuf {
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("node"))
                .find(|n| n.is_file())
        })
        .unwrap_or_else(|| panic!("the harness needs `node` on PATH"))
}

/// A folder `<machine>/<name>` holding links named after `programs`, each to
/// the given file.
fn links_folder(machine: &Machine, name: &str, programs: &[(&str, &Path)]) -> PathBuf {
    let dir = machine.root().join(name);
    fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("cannot make {}: {e}", dir.display()));
    for (program, target) in programs {
        let link = dir.join(program);
        if fs::symlink_metadata(&link).is_err() {
            std::os::unix::fs::symlink(target, &link)
                .unwrap_or_else(|e| panic!("cannot link {}: {e}", link.display()));
        }
    }
    dir
}

/// The `PATH` the tool gets in these tests: a folder of the machine's own
/// with `git` ([`git_binary`]) in it, then the test's own `PATH`.
pub fn tool_path(machine: &Machine) -> String {
    let dir = links_folder(machine, "git-bin", &[("git", git_binary())]);
    let rest = std::env::var("PATH").unwrap_or_default();
    format!("{}:{rest}", dir.display())
}

/// A `PATH` of one folder holding `node` and no `git`.
pub fn path_without_git(machine: &Machine) -> String {
    let node = node_binary();
    links_folder(machine, "only-node", &[("node", &node)])
        .display()
        .to_string()
}

/// A `PATH` of one folder holding `git` and no `node`.
pub fn path_without_node(machine: &Machine) -> String {
    links_folder(machine, "only-git", &[("git", git_binary())])
        .display()
        .to_string()
}

/// Runs the harness's `git` in `dir` with these arguments: no configuration
/// of the user's or the system's read (`GIT_CONFIG_GLOBAL=/dev/null`,
/// `GIT_CONFIG_NOSYSTEM=1`), the author and committer given in the
/// environment, no `HOME`.
pub fn git(dir: &Path, args: &[&str]) -> Tooled {
    let mut command = Command::new(git_binary());
    command
        .current_dir(dir)
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", AUTHOR)
        .env("GIT_AUTHOR_EMAIL", AUTHOR_EMAIL)
        .env("GIT_COMMITTER_NAME", AUTHOR)
        .env("GIT_COMMITTER_EMAIL", AUTHOR_EMAIL);
    let text = format!(
        "(in {}) {} {}",
        dir.display(),
        git_binary().display(),
        args.join(" ")
    );
    let finished = run_to_exit(command, Duration::from_secs(60))
        .unwrap_or_else(|problem| panic!("cannot run {text}: {problem}"));
    Tooled {
        code: finished.status.and_then(|s| s.code()),
        stdout: finished.stdout,
        stderr: finished.stderr,
        command: text,
    }
}

/// As [`git`], which must end with `0`; its standard output.
pub fn git_ok(dir: &Path, args: &[&str]) -> String {
    let ran = git(dir, args);
    assert_eq!(
        ran.code,
        Some(0),
        "the harness's own git failed\n{}",
        ran.describe()
    );
    ran.stdout
}

/// `git` run in `dir` as the tool runs it on `machine`: an environment
/// that holds only the machine's `HOME`, `XDG_CONFIG_HOME` and the
/// [`tool_path`], and `git` found on that `PATH`.
pub fn machine_git(machine: &Machine, dir: &Path, args: &[&str]) -> Tooled {
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        .args(args)
        .env_clear()
        .env("PATH", tool_path(machine))
        .env("HOME", machine.home())
        .env("XDG_CONFIG_HOME", machine.absolute_config_home());
    let text = format!(
        "(in {}, as on the machine) git {}",
        dir.display(),
        args.join(" ")
    );
    let finished = run_to_exit(command, Duration::from_secs(60))
        .unwrap_or_else(|problem| panic!("cannot run {text}: {problem}"));
    Tooled {
        code: finished.status.and_then(|s| s.code()),
        stdout: finished.stdout,
        stderr: finished.stderr,
        command: text,
    }
}

/// `git init` in `dir`, its first branch `main`.
pub fn init_repository(dir: &Path) {
    git_ok(dir, &["init", "-q", "-b", "main"]);
}

/// Every change in `dir`'s repository staged and committed with this
/// message; the new `HEAD`.
pub fn commit_all(dir: &Path, message: &str) -> String {
    git_ok(dir, &["add", "-A"]);
    git_ok(dir, &["commit", "-q", "--allow-empty", "-m", message]);
    head(dir)
}

/// The commit `HEAD` names: 40 hex digits (64 in a SHA-256 repository).
pub fn head(dir: &Path) -> String {
    let text = git_ok(dir, &["rev-parse", "HEAD"]).trim().to_owned();
    assert!(
        (text.len() == 40 || text.len() == 64) && text.bytes().all(|b| b.is_ascii_hexdigit()),
        "the harness's own check: expected HEAD as 40 (or, SHA-256, 64) hex digits, got {text:?}"
    );
    text
}

/// The top folder of the repository `git` finds from `dir`.
pub fn toplevel(dir: &Path) -> PathBuf {
    PathBuf::from(git_ok(dir, &["rev-parse", "--show-toplevel"]).trim())
}

/// A project of the harness's own as a new one stands ([`checked_project`])
/// with the Application name `name`, in a repository of its own, committed.
pub fn deploy_project(label: &str, name: &str) -> TempProject {
    let project = checked_project(label);
    project.write("manifest.ts", &manifest_ts(name, "{}"));
    init_repository(&project.dir);
    commit_all(&project.dir, "A new project");
    project
}

/// The description [`manifest_ts`] gives every Manifest it writes.
pub const MANIFEST_DESCRIPTION: &str = "A project for the tests of vm check.";

/// A handler that reads `incidents.title` of the shared database `devops`
/// through its pulled definition: it type-checks only while that column is
/// in the definition.
pub const READS_INCIDENTS_TS: &str = r#"import { defineApp } from "@vm/sdk";
import manifest from "../manifest.ts";
import { incidents } from "../resources/devops/schema.ts";

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    const rows = await ctx.user().db("devops").select({ title: incidents.title }).from(incidents);
    return Response.json({ user: ctx.user().id, titles: rows.map((r) => r.title) });
  },
});
"#;

/// The Group of [`dana`].
pub const BUILDERS: &str = "builders";

/// The Builder who deploys in these tests.
pub fn dana() -> crate::http::User {
    crate::http::User::new("dana").in_groups(&[BUILDERS])
}

/// A machine signed in to `vm` as [`dana`].
pub fn signed_in(vm: &crate::vm::Vm) -> Machine {
    let machine = Machine::new();
    machine.sign_in(vm, &dana());
    machine
}

/// An ambient module declared for the compiler only: `tsc` finds its types,
/// and there is no such package for a bundler to find.
pub const NOWHERE_DTS: &str =
    "declare module \"vm-harness-nowhere\" {\n  export const greeting: string;\n}\n";

/// A handler that imports [`NOWHERE_DTS`]'s module: it type-checks and does
/// not bundle.
pub const IMPORTS_NOWHERE_TS: &str = r#"import { defineApp } from "@vm/sdk";
import { greeting } from "vm-harness-nowhere";
import manifest from "../manifest.ts";

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    return new Response(`${greeting} ${ctx.user().id}`, { headers: { "content-type": "text/html" } });
  },
});
"#;

/// `notes` with `title` that may not be null: from
/// [`crate::generate::NOTES_WITH_TITLE_SCHEMA_TS`], a change that fails
/// on a row whose `title` is null.
pub const NOTES_WITH_TITLE_REQUIRED_SCHEMA_TS: &str = r#"import { pgTable, serial, text } from "drizzle-orm/pg-core";

export const notes = pgTable("notes", {
  id: serial("id").primaryKey(),
  body: text("body").notNull(),
  title: text("title").notNull(),
});
"#;

/// [`crate::generate::NOTES_WITH_TITLE_SCHEMA_TS`] and a table `tags`.
pub const NOTES_WITH_TITLE_AND_TAGS_SCHEMA_TS: &str = r#"import { pgTable, serial, text } from "drizzle-orm/pg-core";

export const notes = pgTable("notes", {
  id: serial("id").primaryKey(),
  body: text("body").notNull(),
  title: text("title"),
});

export const tags = pgTable("tags", {
  id: serial("id").primaryKey(),
  label: text("label").notNull(),
});
"#;

/// [`crate::generate::NOTES_WITH_TITLE_SCHEMA_TS`] and a column `copy`
/// that may be null: the first step of copying `body`'s values.
pub const NOTES_WITH_TITLE_AND_COPY_SCHEMA_TS: &str = r#"import { pgTable, serial, text } from "drizzle-orm/pg-core";

export const notes = pgTable("notes", {
  id: serial("id").primaryKey(),
  body: text("body").notNull(),
  title: text("title"),
  copy: text("copy"),
});
"#;

/// `vm deploy --project <dir> --json`, and these arguments after.
pub fn deploy_args<'a>(dir: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["deploy", "--project", dir, "--json"];
    args.extend_from_slice(extra);
    args
}

fn utf8(dir: &Path) -> &str {
    dir.to_str().unwrap_or_else(|| panic!("not UTF-8: {dir:?}"))
}

/// Runs `vm deploy --project <dir> --json <extra>` on this machine, with
/// [`tool_path`] as its `PATH`.
pub fn deploy(machine: &Machine, dir: &Path, extra: &[&str]) -> Run {
    deploy_with_env(machine, dir, extra, &[])
}

/// As [`deploy`], with these environment variables added (a `PATH` given
/// here replaces [`tool_path`]).
pub fn deploy_with_env(machine: &Machine, dir: &Path, extra: &[&str], env: &[(&str, &str)]) -> Run {
    let path = tool_path(machine);
    let mut all: Vec<(&str, &str)> = vec![("PATH", path.as_str())];
    all.extend_from_slice(env);
    machine.run_into_files(&deploy_args(utf8(dir), extra), &all, DEPLOY_TIMEOUT)
}

/// Runs `vm deploy --project <dir> <extra>` (in words) on this machine.
pub fn deploy_in_words(machine: &Machine, dir: &Path, extra: &[&str]) -> Run {
    let path = tool_path(machine);
    let mut args = vec!["deploy", "--project", utf8(dir)];
    args.extend_from_slice(extra);
    machine.run_into_files(&args, &[("PATH", path.as_str())], DEPLOY_TIMEOUT)
}

/// What `vm deploy --json` printed when it ended with `0`.
#[derive(Clone, Debug)]
pub struct Deployed {
    pub run: Run,
    pub body: Value,
    pub application: String,
    pub version: u64,
    pub created: bool,
    pub git_commit: String,
    pub migrations_applied: Vec<String>,
    pub structures: Value,
    pub pulled: Vec<Value>,
    /// `visible_to.contributors`: entries (CONTRACT.md, "Contributors and
    /// sharing: The tool").
    pub contributors: Vec<Value>,
    /// `visible_to.shared_with`: entries.
    pub shared_with: Vec<Value>,
    pub warnings: Vec<Value>,
    pub notes: Vec<Value>,
}

/// The fields of what `vm deploy --json` prints, sorted.
pub const DEPLOYED_KEYS: [&str; 11] = [
    "address",
    "application",
    "created",
    "git_commit",
    "migrations_applied",
    "notes",
    "pulled",
    "structures",
    "version",
    "visible_to",
    "warnings",
];

impl Deployed {
    /// Fails unless the run ended with `0` and printed the object of
    /// CONTRACT.md's "What it prints", every field there with its type and
    /// no other: `address` is `<application>.<domain>`, `git_commit` 40 hex
    /// digits, `visible_to` `{ "contributors": [ <entry> ... ],
    /// "shared_with": [ <entry> ... ] }` (CONTRACT.md, "Contributors and
    /// sharing: The tool"), each of
    /// `warnings` and `notes` a `reason` and a `message`, `pulled` by
    /// resource name.
    pub fn of(run: &Run) -> Self {
        let body = run.ok_json();
        let describe = || run.describe();
        let mut keys: Vec<&str> = body
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        keys.sort_unstable();
        assert_eq!(
            keys,
            DEPLOYED_KEYS,
            "expected exactly the fields of the contract's object\n{}",
            describe()
        );
        let application = body["application"]
            .as_str()
            .unwrap_or_else(|| panic!("expected `application` a string\n{}", describe()))
            .to_owned();
        assert_eq!(
            body["address"].as_str(),
            Some(format!("{application}.{}", crate::vm::DOMAIN).as_str()),
            "expected `address` <name>.<domain>\n{}",
            describe()
        );
        let version = body["version"]
            .as_u64()
            .unwrap_or_else(|| panic!("expected `version` a number\n{}", describe()));
        let created = body["created"]
            .as_bool()
            .unwrap_or_else(|| panic!("expected `created` a boolean\n{}", describe()));
        let git_commit = body["git_commit"].as_str().unwrap_or_default().to_owned();
        assert!(
            git_commit.len() == 40 && git_commit.bytes().all(|b| b.is_ascii_hexdigit()),
            "expected `git_commit` 40 hex digits\n{}",
            describe()
        );
        let strings = |key: &str| -> Vec<String> {
            body[key]
                .as_array()
                .unwrap_or_else(|| panic!("expected `{key}` an array\n{}", describe()))
                .iter()
                .map(|v| {
                    v.as_str()
                        .unwrap_or_else(|| {
                            panic!("expected each of `{key}` a string\n{}", describe())
                        })
                        .to_owned()
                })
                .collect()
        };
        let migrations_applied = strings("migrations_applied");
        let structures = body["structures"].clone();
        assert!(
            structures.is_object(),
            "expected `structures` an object\n{}",
            describe()
        );
        let pulled = body["pulled"]
            .as_array()
            .unwrap_or_else(|| panic!("expected `pulled` an array\n{}", describe()))
            .clone();
        for item in &pulled {
            let mut inner: Vec<&str> = item
                .as_object()
                .map(|o| o.keys().map(String::as_str).collect())
                .unwrap_or_default();
            inner.sort_unstable();
            assert_eq!(
                inner,
                ["changed", "files", "resource"],
                "expected each of `pulled` to be {{ resource, changed, files }}\n{}",
                describe()
            );
        }
        let names: Vec<&str> = pulled
            .iter()
            .map(|p| p["resource"].as_str().unwrap_or_default())
            .collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "expected `pulled` by name\n{}", describe());
        let visible_to = &body["visible_to"];
        let mut visible_keys: Vec<&str> = visible_to
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        visible_keys.sort_unstable();
        assert_eq!(
            visible_keys,
            ["contributors", "shared_with"],
            "expected `visible_to` to be {{ contributors, shared_with }}\n{}",
            describe()
        );
        let entries = |key: &str| -> Vec<Value> {
            let items = visible_to[key]
                .as_array()
                .unwrap_or_else(|| panic!("expected `visible_to.{key}` an array\n{}", describe()))
                .clone();
            for item in &items {
                assert!(
                    crate::sharing::entry_key(item).is_some(),
                    "expected each of `visible_to.{key}` to be an entry ({{ user }}, {{ group }} or {{ everyone: true }}), got {item}\n{}",
                    describe()
                );
            }
            items
        };
        let contributors = entries("contributors");
        let shared_with = entries("shared_with");
        let list = |key: &str| -> Vec<Value> {
            let items = body[key]
                .as_array()
                .unwrap_or_else(|| panic!("expected `{key}` an array\n{}", describe()))
                .clone();
            for item in &items {
                for field in ["reason", "message"] {
                    assert!(
                        item[field].as_str().is_some_and(|s| !s.trim().is_empty()),
                        "expected each of `{key}` to have a `{field}`, got {item}\n{}",
                        describe()
                    );
                }
            }
            items
        };
        let warnings = list("warnings");
        let notes = list("notes");
        Self {
            run: run.clone(),
            body: body.clone(),
            application,
            version,
            created,
            git_commit,
            migrations_applied,
            structures,
            pulled,
            contributors,
            shared_with,
            warnings,
            notes,
        }
    }

    /// The warnings with this reason.
    pub fn warnings_for(&self, reason: &str) -> Vec<Value> {
        self.warnings
            .iter()
            .filter(|w| w["reason"] == reason)
            .cloned()
            .collect()
    }

    /// The notes with this reason.
    pub fn notes_for(&self, reason: &str) -> Vec<Value> {
        self.notes
            .iter()
            .filter(|w| w["reason"] == reason)
            .cloned()
            .collect()
    }

    /// The entry of `pulled` for this Resource; fails unless there is one.
    pub fn pulled_of(&self, resource: &str) -> Value {
        self.pulled
            .iter()
            .find(|p| p["resource"] == resource)
            .cloned()
            .unwrap_or_else(|| panic!("expected {resource:?} in `pulled`\n{}", self.run.describe()))
    }
}

/// Runs `vm deploy --json` on the project in `dir`, which must end with `0`.
pub fn deploy_ok(machine: &Machine, dir: &Path) -> Deployed {
    Deployed::of(&deploy(machine, dir, &[]))
}

/// One migration a `data-loss` of `vm deploy` stops for: its name, what it
/// destroys and its destroying statements.
#[derive(Clone, Debug)]
pub struct Destroying {
    pub migration: String,
    pub destroys: Vec<Value>,
    pub statements: Vec<String>,
}

/// What `vm deploy --json` printed when it stopped for data that would be
/// lost.
#[derive(Clone, Debug)]
pub struct DataLossAtDeploy {
    pub run: Run,
    pub message: String,
    pub migrations: Vec<Destroying>,
}

impl DataLossAtDeploy {
    /// Fails unless the run ended with `3`, reason `data-loss`, and the
    /// error is exactly `{ reason, message, migrations: [ { migration,
    /// destroys, statements } ] }` (CONTRACT.md, "Deploying", step 7), each
    /// item those three fields and no other.
    pub fn of(run: &Run) -> Self {
        let message = run.json_error(3, "data-loss");
        let body = run.json();
        let error = &body["error"];
        let mut keys: Vec<&str> = error
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["message", "migrations", "reason"],
            "expected the error to be {{ reason, message, migrations }}\n{}",
            run.describe()
        );
        let read = |item: &Value| -> Destroying {
            let mut inner: Vec<&str> = item
                .as_object()
                .map(|o| o.keys().map(String::as_str).collect())
                .unwrap_or_default();
            inner.sort_unstable();
            assert_eq!(
                inner,
                ["destroys", "migration", "statements"],
                "expected each of `migrations` to be {{ migration, destroys, statements }}, got {item}"
            );
            let migration = item["migration"]
                .as_str()
                .unwrap_or_else(|| panic!("expected `migration` a string in {item}"))
                .to_owned();
            let destroys = item["destroys"]
                .as_array()
                .unwrap_or_else(|| panic!("expected `destroys` an array in {item}"))
                .clone();
            let statements = item["statements"]
                .as_array()
                .unwrap_or_else(|| panic!("expected `statements` an array in {item}"))
                .iter()
                .map(|s| {
                    s.as_str()
                        .unwrap_or_else(|| panic!("expected each statement a string in {item}"))
                        .to_owned()
                })
                .collect();
            Destroying {
                migration,
                destroys,
                statements,
            }
        };
        let migrations: Vec<Destroying> = error["migrations"]
            .as_array()
            .unwrap_or_else(|| panic!("expected `migrations` an array\n{}", run.describe()))
            .iter()
            .map(read)
            .collect();
        Self {
            run: run.clone(),
            message,
            migrations,
        }
    }
}

/// Every file, folder and link of the project in `dir` but `.git` and what
/// is inside it (git's own, which `git status` may refresh).
pub fn project_tree(dir: &Path) -> Tree {
    let mut tree = Tree::of_skipping(dir, &[".git"]);
    tree.0.retain(|path, _| path != ".git");
    tree
}

/// Fails unless nothing in the project `dir` (but `.git`) differs from
/// `before` except under these paths (`resources/devops`, say).
pub fn assert_project_unchanged_but(before: &Tree, dir: &Path, allowed: &[&str], context: &str) {
    let differences: Vec<String> = before
        .differences(&project_tree(dir))
        .into_iter()
        .filter(|line| {
            !allowed.iter().any(|prefix| {
                line.starts_with(&format!("{prefix}/")) || line.starts_with(&format!("{prefix}:"))
            })
        })
        .collect();
    assert_eq!(
        differences,
        Vec::<String>::new(),
        "{context}: expected nothing written in the project{}",
        if allowed.is_empty() {
            String::new()
        } else {
            format!(" but under {allowed:?}")
        }
    );
}

/// The fingerprint the second line of a pulled definition gives
/// (`// Resource: <name>  Fingerprint: sha256:<64 hex>`).
pub fn fingerprint_in(schema_ts: &str) -> Option<String> {
    let line = schema_ts.lines().nth(1)?;
    let at = line.find("Fingerprint: ")?;
    line.get(at + "Fingerprint: ".len()..)
        .map(|f| f.trim().to_owned())
}

/// The `git status --porcelain` of `dir` as the harness's `git` sees it:
/// empty when everything is committed.
pub fn status(dir: &Path) -> String {
    git_ok(dir, &["status", "--porcelain"])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fingerprint_is_read_from_the_second_line() {
        let ts = format!(
            "// Generated by vm pull. Do not edit.\n// Resource: devops  Fingerprint: sha256:{}\nexport {{}};\n",
            "ab".repeat(32)
        );
        assert_eq!(
            fingerprint_in(&ts),
            Some(format!("sha256:{}", "ab".repeat(32)))
        );
        assert_eq!(fingerprint_in("one line\n"), None);
    }
}
