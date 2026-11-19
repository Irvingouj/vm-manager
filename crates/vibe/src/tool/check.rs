//! `vibe check [--project <directory>] [--timeout <seconds>] [--json]`
//! (blackbox/CONTRACT.md, "Checking a project").
//!
//! Its pieces (the registry, the packages, each of the four checks and the
//! report) are also `vibe deploy`'s, which runs them in its own order: their
//! problems are worded as the check's whichever command runs them, and
//! their endings name the command that runs them.
//!
//! What is looked at first, in order: the command line (clap: `2`), the
//! project (`not-a-project`), the sign-in (the kept credential, then the
//! installation's list of Resources: `4`), `node` (`node-missing`) and the
//! packages (`not-installed`). Then the four checks, one after the other,
//! each reporting every problem it finds: `manifest` (the SDK's tooling),
//! `resources` (left out when the Manifest cannot be read), `migrations`
//! (`drizzle-kit generate` against a copy of `migrations/`, which the
//! check also reads itself: see [`drizzle`]) and `types`
//! (`tsc`). Warnings and notes come from the `resources` check.
//!
//! **Nothing in the project is made, changed or removed.** What the check
//! and its tools write goes in the check's own folder ([`Scratch`]):
//! private (mode `0700`), in `TMPDIR`, made before the first tool runs,
//! every tool's `TMPDIR`, and removed in every ending (passed, failed, out
//! of time, interrupted). `drizzle-kit` writes its journal even when it
//! writes no migration, so it is given a copy of `migrations/` there; `tsc`
//! runs with `--noEmit`, and with `--incremental --tsBuildInfoFile` into
//! the folder, so that a tsconfig's `incremental` or `composite` writes no
//! build information beside it.
//!
//! **Endings.** The whole check is held to `--timeout` (300 s without it),
//! and `SIGINT`, `SIGTERM` and `SIGHUP` end it (`interrupted`): their
//! handlers are in place before the first tool starts. Either way the
//! check's future is dropped, which stops every tool's process group
//! ([`super::child`]), and then the own folder is removed
//! ([`project_tools`]).
//!
//! **Bounds.** What is kept of each tool's output: [`TEXT_BYTES`] of each
//! pipe (the Manifest's JSON: [`MANIFEST_BYTES`]), past which the rest is
//! read and dropped; of `tsc`, the first [`MAX_TYPE_ERRORS`] errors, each
//! line to [`LINE_BYTES`], the rest counted. Of the project's files: the
//! first line of a generated file, the first [`MAX_PULLED`] folders of
//! `resources/`, and `migrations/` as [`drizzle`] bounds it (past which
//! the check ends with `project-file`). A tool's words in a message: as
//! [`Cx::words`] keeps them, without its stack and without the own
//! folder's path.
//!
//! **Blocking file operations.** The check's own reading and copying of
//! the project's files are blocking calls on the tool's one thread, and the
//! timeout does not cut them off: a file system that hangs (a network
//! mount gone) hangs the check. That is accepted here because these are a
//! small project's files on the Builder's own machine, read once each and
//! bounded as above; and moving them to `tokio::fs` would gain nothing,
//! since it runs them on blocking threads that the runtime still waits for
//! when the command ends. Everything that can run long (the tools, the
//! installation) is inside the timeout.
//!
//! Every message is written for a Coding agent to act on, and names the
//! fix. Messages hold text of the project's and of the installation's (a
//! file's name, a contact, a tool's words): with `--json` it is written as
//! JSON writes text; in words every message goes through [`Theirs`], which
//! escapes control characters (the tool's own words have none).

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::child::{Kept, Lines, Sink};
use super::credential_file::Place;
use super::drizzle::{self, Outcome};
use super::project_tools::{
    Bounded, Cx, DEFAULT_TIMEOUT, Scratch, Signals, TEXT_BYTES, Who, ended, node_is_there,
    own_folder_failed, tool_failed,
};
use super::pull::INDEX_FIRST_LINE;
use super::theirs::Theirs;
use super::{Done, Ending, Output, get_signed_in, project_of, read_kept, shaped};
use crate::resources::is_resource_name;
use crate::structure::FIRST_LINE as SCHEMA_FIRST_LINE;

/// `vibe check`.
#[derive(clap::Args)]
pub struct CheckArgs {
    /// The project to check; without it, the current directory.
    #[arg(long)]
    pub project: Option<PathBuf>,
    /// The longest the whole check may take, in seconds (1 to 3600; 300
    /// without it).
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=3600))]
    pub timeout: Option<u64>,
    #[command(flatten)]
    pub output: Output,
}

/// How the check's messages name it.
const WHO: Who = Who {
    command: "vibe check",
    the: "the check",
    folder: "vibe-check",
};

/// What is kept of the SDK's tooling's answer on the Manifest.
const MANIFEST_BYTES: usize = 1024 * 1024;

/// The type errors reported one by one; the rest are counted.
const MAX_TYPE_ERRORS: usize = 50;

/// What is kept of one line of `tsc`'s output, and of one error's message.
const LINE_BYTES: usize = 16 * 1024;

/// The folders of `resources/` looked at for pulled Resources.
const MAX_PULLED: usize = 1000;

/// Where the check is, for the message of a check that ran out of time.
#[derive(Clone, Copy)]
pub(super) enum Stage {
    SignIn,
    Node,
    Packages,
    Manifest,
    Resources,
    Migrations,
    Types,
}

impl Stage {
    pub(super) fn words(self) -> &'static str {
        match self {
            Self::SignIn => "asking the installation which Resources are registered",
            Self::Node => "asking node its version",
            Self::Packages => "finding the project's packages",
            Self::Manifest => "the manifest check (the SDK's tooling reading manifest.ts)",
            Self::Resources => "the resources check",
            Self::Migrations => {
                "the migrations check (drizzle-kit generate loading src/db/schema.ts)"
            }
            Self::Types => "the types check (tsc on the project)",
        }
    }
}

pub(super) async fn check(args: &CheckArgs) -> Result<Done, Ending> {
    let project = project_of(args.project.as_deref(), "vibe check")?;
    // Node resolves packages from the real path, and the tools are given
    // absolute paths.
    let project = fs::canonicalize(&project).map_err(|error| {
        Ending::new(
            1,
            "project-file",
            format!(
                "the project's folder cannot be read ({error}): make it readable by you, then run vibe check again"
            ),
        )
    })?;
    // In place before the first tool starts: from here on these signals end
    // the check (below), not the program, so what it started is stopped.
    let mut signals = Signals::listen(&WHO)?;
    let own = Scratch::make(&WHO).map_err(|e| own_folder_failed(&e, &WHO))?;
    let limit = args.timeout.map_or(DEFAULT_TIMEOUT, Duration::from_secs);
    let stage = Cell::new(Stage::SignIn);
    // When a signal comes or the time is up, the check's future is dropped
    // inside `within`: every tool's process group is stopped then. The own
    // folder outlives it and is removed after, when nothing writes in it.
    let ended = match signals
        .within(limit, checked(&project, &own.path, &stage))
        .await
    {
        Bounded::Ended(ended) => ended,
        Bounded::TimedOut => Err(Ending::new(
            1,
            "check-timed-out",
            format!(
                "the check did not end within {} s and was stopped, with everything it started; it was in {}. Look at what that runs (src/db/schema.ts and what it imports are run by drizzle-kit), or give more time with --timeout <seconds>, then run vibe check again",
                limit.as_secs(),
                stage.get().words()
            ),
        )),
        Bounded::Interrupted(signal) => Err(interrupted(signal, stage.get())),
    };
    drop(own);
    ended
}

fn interrupted(signal: &str, stage: Stage) -> Ending {
    Ending::new(
        1,
        "interrupted",
        format!(
            "the check was interrupted ({signal}) in {}, and stopped everything it had started; nothing was decided: run vibe check again",
            stage.words()
        ),
    )
}

/// A Resource as the check reads it from `GET /v1/resources`. Its texts are
/// only ever put in a message or in JSON (see the module's comment).
#[derive(Deserialize)]
pub(super) struct Registered {
    name: String,
    contact: String,
    your_access: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct Registry {
    resources: Vec<Registered>,
}

impl Registry {
    /// The Builder's Access level on the registered Resource `name`: `None`
    /// for one not registered, `Some(None)` for no Grant.
    pub(super) fn access_of(&self, name: &str) -> Option<Option<&str>> {
        self.resources
            .iter()
            .find(|r| r.name == name)
            .map(|r| r.your_access.as_deref())
    }
}

/// The registered Resources and what the signed-in Builder holds:
/// `GET /v1/resources`.
pub(super) async fn registry(kept: &super::credential_file::Kept) -> Result<Registry, Ending> {
    shaped(kept.at(), get_signed_in(kept, "/v1/resources").await?)
}

/// The project's packages, as Node resolves them.
pub(super) struct Packages {
    pub tooling: PathBuf,
    tsc: PathBuf,
    drizzle_kit: PathBuf,
}

/// What the four checks found.
#[derive(Default)]
pub(super) struct Found {
    pub problems: Vec<Value>,
    pub warnings: Vec<Value>,
    pub notes: Vec<Value>,
}

/// The Manifest as the SDK's tooling reads it.
pub(super) struct Manifest {
    /// The Application's name, as the tooling took it.
    pub name: String,
    pub description: Option<String>,
    /// The Resources it declares, by name, with the Access level of each.
    pub declared: BTreeMap<String, String>,
    /// `{ "resources": { ... } }`, as the tooling answered it.
    pub declared_access: Value,
}

async fn checked(project: &Path, own: &Path, stage: &Cell<Stage>) -> Result<Done, Ending> {
    let place = Place::from_environment().map_err(Ending::credential_file)?;
    let kept = read_kept(&place)?;
    let registry = registry(&kept).await?;
    let cx = Cx::new(project, own, &WHO);

    stage.set(Stage::Node);
    node_is_there(&cx, &WHO).await?;
    stage.set(Stage::Packages);
    let packages = packages(project, &WHO)?;

    let mut found = Found::default();
    stage.set(Stage::Manifest);
    let manifest = manifest(&cx, &packages, &mut found, &WHO).await?;
    if let Some(manifest) = &manifest {
        stage.set(Stage::Resources);
        resources(project, &manifest.declared, &registry, &mut found);
    }
    stage.set(Stage::Migrations);
    migrations(&cx, &packages, &mut found, &WHO).await?;
    stage.set(Stage::Types);
    types(&cx, &packages, &mut found, &WHO).await?;
    report(found, "")
}

/// `message` as words: the tool's own, with the project's and the
/// installation's text in it escaped.
fn words(message: &str) -> String {
    Theirs::from(message).to_string()
}

fn message_of(item: &Value) -> &str {
    item.get("message").and_then(Value::as_str).unwrap_or("")
}

/// The check's report: `Ok` when there is no problem, else the ending
/// `check-failed`, whose message begins with `preface` (empty, or a
/// sentence of the command's own and a space).
pub(super) fn report(found: Found, preface: &str) -> Result<Done, Ending> {
    let Found {
        problems,
        warnings,
        notes,
    } = found;
    let mut text = String::new();
    let count = problems.len();
    if count == 0 {
        text.push_str("The check passed: no problem.\n");
    } else {
        let s = if count == 1 { "" } else { "s" };
        text.push_str(&format!("The check failed: {count} problem{s}.\n"));
    }
    for (title, items, with_check) in [
        ("Problems", &problems, true),
        ("Warnings", &warnings, false),
        ("Notes", &notes, false),
    ] {
        if items.is_empty() {
            continue;
        }
        text.push_str(&format!("{title}:\n"));
        for item in items {
            let reason = item.get("reason").and_then(Value::as_str).unwrap_or("");
            let check = item.get("check").and_then(Value::as_str);
            match check.filter(|_| with_check) {
                Some(check) => text.push_str(&format!(
                    "  - [{check}] {reason}: {}\n",
                    words(message_of(item))
                )),
                None => text.push_str(&format!("  - {reason}: {}\n", words(message_of(item)))),
            }
        }
    }
    let mut body = json!({
        "ok": count == 0,
        "problems": problems,
        "warnings": warnings,
        "notes": notes,
    });
    let Some(first) = problems.first() else {
        return Ok(Done { json: body, text });
    };
    let s = if count == 1 { "" } else { "s" };
    let message = format!(
        "{preface}the check found {count} problem{s}; the first: {}",
        message_of(first)
    );
    if !preface.is_empty() {
        text = format!("{}\n{text}", words(preface.trim_end()));
    }
    if let Some(object) = body.as_object_mut() {
        object.insert(
            "error".into(),
            json!({ "reason": "check-failed", "message": message }),
        );
    }
    Err(Ending::check_failed(&message, Done { json: body, text }))
}

pub(super) fn packages(project: &Path, who: &Who) -> Result<Packages, Ending> {
    Ok(Packages {
        tooling: super::project_tools::sdk_tooling(project, who)?,
        tsc: super::project_tools::tsc(project, who)?,
        drizzle_kit: super::project_tools::drizzle_kit(project, who)?,
    })
}

/// The `manifest` check: the Manifest as the tooling reads it; none when it
/// cannot be read (and then a problem is in `found`). `who` names the
/// command in its endings.
pub(super) async fn manifest(
    cx: &Cx<'_>,
    packages: &Packages,
    found: &mut Found,
    who: &Who,
) -> Result<Option<Manifest>, Ending> {
    let ran = cx
        .run(
            &[
                packages.tooling.as_os_str(),
                OsStr::new("manifest"),
                cx.project.as_os_str(),
            ],
            Kept::up_to(MANIFEST_BYTES),
            Kept::up_to(TEXT_BYTES),
        )
        .await
        .map_err(|error| tool_failed("node", &format!("cannot be started ({error})"), who))?;
    let answer: Option<Value> = if ran.out.over {
        None
    } else {
        serde_json::from_slice(ran.out.bytes()).ok()
    };
    match (ran.status.code(), answer) {
        (Some(0), Some(mut answer)) => {
            let mut declared = BTreeMap::new();
            if let Some(Value::Object(resources)) = answer.pointer("/declared_access/resources") {
                for (name, resource) in resources {
                    let access = match resource.get("access") {
                        Some(Value::String(access)) => access.as_str(),
                        _ => "read",
                    };
                    declared.insert(name.clone(), access.to_owned());
                }
            }
            let mut text = |key: &str| match answer.get_mut(key).map(Value::take) {
                Some(Value::String(text)) => Some(text),
                _ => None,
            };
            let name = text("name").unwrap_or_default();
            let description = text("description");
            let declared_access = answer
                .get_mut("declared_access")
                .map(Value::take)
                .unwrap_or_else(|| json!({ "resources": {} }));
            Ok(Some(Manifest {
                name,
                description,
                declared,
                declared_access,
            }))
        }
        (Some(2), Some(mut answer)) if answer.get("error").is_some_and(Value::is_object) => {
            let mut error = answer.get_mut("error").map(Value::take).unwrap_or_default();
            let reason = error
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("manifest-invalid")
                .to_owned();
            let message = error.get("message").and_then(Value::as_str).unwrap_or("");
            let line = error.get("line").and_then(Value::as_u64);
            let column = error.get("column").and_then(Value::as_u64);
            let place = match (line, column) {
                (Some(line), Some(column)) => format!("manifest.ts, line {line}, column {column}"),
                (Some(line), None) => format!("manifest.ts, line {line}"),
                _ => "manifest.ts".to_owned(),
            };
            let message =
                format!("{place}: {message} Correct manifest.ts, then run vibe check again.");
            let file = error.get_mut("file").map(Value::take).unwrap_or_default();
            found.problems.push(json!({
                "check": "manifest",
                "reason": reason,
                "message": message,
                "file": file,
                "line": line,
                "column": column,
            }));
            Ok(None)
        }
        _ => Err(tool_failed(
            "the SDK's tooling (manifest)",
            &format!(
                "{} without an answer it understands ({})",
                ended(ran.status),
                cx.words(&ran.err.text(), "")
            ),
            who,
        )),
    }
}

/// How the first line of a file stands.
enum FirstLine {
    Missing,
    Generated,
    Other,
    Unreadable(io::Error),
}

/// Whether the file at `path` begins with `line` (followed by a line end,
/// or nothing): only that much of it is read.
/// Whether the declared Resource `name` is pulled: both its files there,
/// each beginning with the first line `vibe pull` gives it.
pub(super) fn is_pulled(project: &Path, name: &str) -> bool {
    let folder = project.join("resources").join(name);
    matches!(
        first_line(&folder.join("schema.ts"), SCHEMA_FIRST_LINE),
        FirstLine::Generated
    ) && matches!(
        first_line(&folder.join("index.ts"), INDEX_FIRST_LINE),
        FirstLine::Generated
    )
}

fn first_line(path: &Path, line: &str) -> FirstLine {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return FirstLine::Missing,
        Err(error) => return FirstLine::Unreadable(error),
    };
    match file.metadata() {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return FirstLine::Other,
        Err(error) => return FirstLine::Unreadable(error),
    }
    let wanted = line.len().saturating_add(1);
    let mut bytes = Vec::with_capacity(wanted);
    if let Err(error) = file
        .take(u64::try_from(wanted).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
    {
        return FirstLine::Unreadable(error);
    }
    let begins = bytes
        .strip_prefix(line.as_bytes())
        .is_some_and(|rest| rest.is_empty() || rest == b"\n" || rest == b"\r");
    if begins {
        FirstLine::Generated
    } else {
        FirstLine::Other
    }
}

/// An Access level's place among the others: none below `structure`, below
/// `read`, below `write`.
fn rank(level: Option<&str>) -> u8 {
    match level {
        Some("structure") => 1,
        Some("read") => 2,
        Some("write") => 3,
        _ => 0,
    }
}

/// The `resources` check, its warnings, and its notes.
pub(super) fn resources(
    project: &Path,
    declared: &BTreeMap<String, String>,
    registry: &Registry,
    found: &mut Found,
) {
    let registered: BTreeMap<&str, &Registered> = registry
        .resources
        .iter()
        .map(|r| (r.name.as_str(), r))
        .collect();
    for (name, access) in declared {
        let Some(resource) = registered.get(name.as_str()) else {
            let names: Vec<&str> = registered.keys().copied().collect();
            let those = if names.is_empty() {
                "no Resource is registered on this installation".to_owned()
            } else {
                format!("the Resources registered are: {}", names.join(", "))
            };
            found.problems.push(json!({
                "check": "resources",
                "reason": "resource-not-registered",
                "resource": name,
                "message": format!("manifest.ts declares the Resource {name}, and no Resource of that name is registered on the installation ({those}): correct the name in manifest.ts, or ask a Platform administrator to register it, then run vibe check again"),
            }));
            continue;
        };
        if is_resource_name(name) {
            pulled(project, name, found);
        }
        let held = resource.your_access.as_deref();
        if rank(held) < rank(Some(access.as_str())) {
            let holding = match held {
                Some(level) => format!("you hold {name} at {level} only"),
                None => format!("you hold no Grant on {name}"),
            };
            found.warnings.push(json!({
                "reason": "own-grant-below-declared",
                "resource": name,
                "declared": access,
                "held": held,
                "contact": resource.contact,
                "message": format!("manifest.ts declares {name} at {access}, and {holding}: the Application will refuse its operations on {name} at {access} to you, and to every User whose Groups hold no Grant at {access} or above. To use it yourself, ask {} for a Grant at {access}; tell the Builder who will be refused", resource.contact),
            }));
        }
    }

    // Pulled and not declared.
    let Ok(listing) = fs::read_dir(project.join("resources")) else {
        return;
    };
    let mut names = BTreeSet::new();
    for entry in listing.flatten().take(MAX_PULLED) {
        if let Ok(name) = entry.file_name().into_string() {
            names.insert(name);
        }
    }
    for name in names {
        if declared.contains_key(&name) {
            continue;
        }
        let schema = project.join("resources").join(&name).join("schema.ts");
        if matches!(first_line(&schema, SCHEMA_FIRST_LINE), FirstLine::Generated) {
            found.notes.push(json!({
                "reason": "pulled-not-declared",
                "resource": name,
                "message": format!("resources/{name}/ holds a pulled Resource that manifest.ts does not declare: declare {name} in manifest.ts if the Application uses it, or remove the folder resources/{name}/"),
            }));
        }
    }
}

/// That the declared Resource `name` is pulled: both files there, each
/// written by `vibe pull`.
fn pulled(project: &Path, name: &str, found: &mut Found) {
    let mut wrong = Vec::new();
    let mut by_hand = false;
    for (file, line) in [
        ("schema.ts", SCHEMA_FIRST_LINE),
        ("index.ts", INDEX_FIRST_LINE),
    ] {
        let shown = format!("resources/{name}/{file}");
        match first_line(&project.join("resources").join(name).join(file), line) {
            FirstLine::Generated => {}
            FirstLine::Missing => wrong.push(format!("{shown} is not there")),
            FirstLine::Other => {
                by_hand = true;
                wrong.push(format!(
                    "{shown} was not written by vibe pull (it does not begin with `{line}`)"
                ));
            }
            FirstLine::Unreadable(error) => {
                wrong.push(format!("{shown} cannot be read ({error})"));
            }
        }
    }
    if wrong.is_empty() {
        return;
    }
    let fix = if by_hand {
        format!(
            "move the file that vibe pull did not write away first, then run `vibe pull {name}`"
        )
    } else {
        format!("run `vibe pull {name}`")
    };
    found.problems.push(json!({
        "check": "resources",
        "reason": "resource-not-pulled",
        "resource": name,
        "message": format!("manifest.ts declares {name}, and it is not pulled: {}. To pull it, {fix}, then run vibe check again", wrong.join("; ")),
    }));
}
/// The `migrations` check: `drizzle-kit generate` against a copy of
/// `migrations/` in the check's own folder, and the copy read by the check
/// itself ([`drizzle::generate`], which says how it is decided).
/// Its problems are worded as the check's, whichever command runs it; `who`
/// names the command in its endings.
pub(super) async fn migrations(
    cx: &Cx<'_>,
    packages: &Packages,
    found: &mut Found,
    who: &Who,
) -> Result<(), Ending> {
    drizzle::copy_migrations(cx, who)?;
    let outcome = drizzle::generate(cx, &packages.drizzle_kit, &[], who).await?;
    let behind = |message: &str| json!({ "check": "migrations", "reason": "migrations-behind", "message": message });
    match outcome {
        Outcome::Unreadable { what, stray } => found.problems.push(json!({
            "check": "migrations",
            "reason": "migrations-unreadable",
            "message": drizzle::unreadable_message(&what, stray, &WHO),
        })),
        Outcome::Written(_) => found.problems.push(behind(
            "src/db/schema.ts changed since the last migration in migrations/: generating migrations now would write one. Generate it with `vibe generate`, which writes it into migrations/ and stops before one that destroys data (then ask the Builder, as it says), read the SQL it wrote, then run vibe check again",
        )),
        Outcome::InStep => {}
        Outcome::Asked => found.problems.push(behind(
            "src/db/schema.ts changed since the last migration in migrations/, and drizzle-kit has a question about the change (a table or column renamed, or removed and made anew?) that it cannot ask without a terminal, so `vibe generate` writes nothing for it: make the change in steps, each followed by `vibe generate` (add the new column or table, keeping the old one; copy the values in an empty migration made with `vibe generate --empty`; then remove the old one), then run vibe check again",
        )),
        Outcome::SchemaFailed(how) => found.problems.push(json!({
            "check": "migrations",
            "reason": "own-schema-failed",
            "message": drizzle::schema_failed(&how, &WHO),
        })),
    }
    Ok(())
}
/// One error `tsc` gave.
struct TypeError {
    file: Option<String>,
    line: Option<u64>,
    column: Option<u64>,
    message: String,
}

/// `tsc --pretty false`'s output, read as errors: the first
/// [`MAX_TYPE_ERRORS`] kept, the rest counted.
struct TypeErrors {
    lines: Lines,
    read: ReadErrors,
}

/// What [`TypeErrors`] has read so far.
struct ReadErrors {
    kept: Vec<TypeError>,
    more: usize,
    /// Whether the last error seen is the last one kept, so that a line of
    /// its message's continuation is added to it.
    last_kept: bool,
    /// What else was printed, for when nothing could be read as an error.
    other: Kept,
}

impl TypeErrors {
    fn new() -> Self {
        Self {
            lines: Lines::of_at_most(LINE_BYTES),
            read: ReadErrors {
                kept: Vec::new(),
                more: 0,
                last_kept: false,
                other: Kept::up_to(TEXT_BYTES),
            },
        }
    }
}

/// A line of `tsc --pretty false`: `file(line,col): error TSnnnn: message`,
/// or `error TSnnnn: message` with no place.
fn type_error(line: &str) -> Option<TypeError> {
    let (place, rest) = match line.find("): error TS") {
        Some(at) => (
            line.get(..at),
            line.get(at.saturating_add(3)..).unwrap_or_default(),
        ),
        None => (None, line),
    };
    let rest = rest.strip_prefix("error ")?;
    let (code, message) = rest.split_once(": ")?;
    if !code.starts_with("TS") {
        return None;
    }
    let message = format!("{message} ({code})");
    let Some(place) = place else {
        return Some(TypeError {
            file: None,
            line: None,
            column: None,
            message,
        });
    };
    let (file, at) = place.rsplit_once('(')?;
    let (line_number, column) = at.split_once(',')?;
    Some(TypeError {
        file: Some(file.to_owned()),
        line: line_number.trim().parse().ok(),
        column: column.trim().parse().ok(),
        message,
    })
}

impl ReadErrors {
    fn line(&mut self, line: &str) {
        if let Some(error) = type_error(line) {
            if self.kept.len() < MAX_TYPE_ERRORS {
                self.kept.push(error);
                self.last_kept = true;
            } else {
                self.more = self.more.saturating_add(1);
                self.last_kept = false;
            }
        } else if line.starts_with(' ') {
            if self.last_kept
                && let Some(last) = self.kept.last_mut()
                && last.message.len() < LINE_BYTES
            {
                last.message.push(' ');
                last.message.push_str(line.trim());
            }
        } else if !line.trim().is_empty() {
            self.other.take(line.as_bytes());
            self.other.take(b"\n");
        }
    }
}

impl Sink for TypeErrors {
    fn take(&mut self, bytes: &[u8]) {
        let read = &mut self.read;
        self.lines.feed(bytes, |line| read.line(line));
    }

    fn end(&mut self) {
        let read = &mut self.read;
        self.lines.finish(|line| read.line(line));
    }
}

/// The `types` check: `tsc --noEmit` with the project's own
/// `tsconfig.json`. `who` names the command in its endings.
pub(super) async fn types(
    cx: &Cx<'_>,
    packages: &Packages,
    found: &mut Found,
    who: &Who,
) -> Result<(), Ending> {
    // Build information, which `incremental` or `composite` in the
    // project's tsconfig.json would have tsc write beside it, goes to the
    // check's own folder: these two flags are taken whatever the tsconfig
    // says.
    let build_information = cx.own.join("tsc.tsbuildinfo");
    let ran = cx
        .run(
            &[
                packages.tsc.as_os_str(),
                OsStr::new("--noEmit"),
                OsStr::new("-p"),
                OsStr::new("tsconfig.json"),
                OsStr::new("--pretty"),
                OsStr::new("false"),
                OsStr::new("--incremental"),
                OsStr::new("--tsBuildInfoFile"),
                build_information.as_os_str(),
            ],
            TypeErrors::new(),
            Kept::up_to(TEXT_BYTES),
        )
        .await
        .map_err(|error| tool_failed("node", &format!("cannot be started ({error})"), who))?;
    let errors = ran.out.read;
    for error in &errors.kept {
        let message = match (&error.file, error.line, error.column) {
            (Some(file), Some(line), Some(column)) => format!(
                "{file}, line {line}, column {column}: {}. Correct {file} at line {line}, then run vibe check again",
                error.message.trim_end_matches('.')
            ),
            _ => format!(
                "{}. The compiler gives this with no place in a file: correct what it names, then run vibe check again",
                error.message.trim_end_matches('.')
            ),
        };
        found.problems.push(json!({
            "check": "types",
            "reason": "type-error",
            "message": message,
            "file": error.file,
            "line": error.line,
            "column": error.column,
        }));
    }
    if errors.more > 0 {
        let more = errors.more;
        let s = if more == 1 { "" } else { "s" };
        found.problems.push(json!({
            "check": "types",
            "reason": "type-error",
            "message": format!("and {more} more type error{s} after these {MAX_TYPE_ERRORS}: correct these first, then run vibe check again to see the rest"),
            "more": more,
        }));
    }
    if errors.kept.is_empty() && !ran.status.success() {
        let mut problem = Map::new();
        problem.insert("check".into(), json!("types"));
        problem.insert("reason".into(), json!("type-error"));
        problem.insert(
            "message".into(),
            json!(format!(
                "tsc {} and gave no error it could place ({}): correct what it names, then run vibe check again",
                ended(ran.status),
                cx.words(&ran.err.text(), &errors.other.text())
            )),
        );
        for field in ["file", "line", "column"] {
            problem.insert(field.into(), Value::Null);
        }
        found.problems.push(Value::Object(problem));
    }
    Ok(())
}
