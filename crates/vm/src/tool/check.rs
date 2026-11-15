//! `vm check [--project <directory>] [--timeout <seconds>] [--json]`
//! (blackbox/CONTRACT.md, "Checking a project").
//!
//! What is looked at first, in order: the command line (clap: `2`), the
//! project (`not-a-project`), the sign-in (the kept credential, then the
//! installation's list of Resources: `4`), `node` (`node-missing`) and the
//! packages (`not-installed`). Then the four checks, one after the other,
//! each reporting every problem it finds: `manifest` (the SDK's tooling),
//! `resources` (left out when the Manifest cannot be read), `migrations`
//! (`drizzle-kit generate` against a copy of `migrations/`) and `types`
//! (`tsc`). Warnings and notes come from the `resources` check.
//!
//! **Nothing in the project is made, changed or removed.** `drizzle-kit`
//! writes its journal even when it writes no migration, so it is given a
//! copy of `migrations/` in a private folder (mode `0700`) of the system's
//! temporary folder, outside the project; [`Scratch`] removes it in every
//! ending, the timeout included. `tsc` runs with `--noEmit`.
//!
//! **Bounds.** The whole check is held to `--timeout` (300 s without it);
//! past it the check's future is dropped, which stops every tool's process
//! group ([`child`]) and removes the copy. What is kept of each tool's
//! output: [`TEXT_BYTES`] of each pipe (the Manifest's JSON:
//! [`MANIFEST_BYTES`]), past which the rest is read and dropped; of `tsc`,
//! the first [`MAX_TYPE_ERRORS`] errors, each line to [`LINE_BYTES`], the
//! rest counted. Of the project's files: the first line of a generated
//! file, a `package.json` to [`PACKAGE_JSON_BYTES`], the first
//! [`MAX_PULLED`] folders of `resources/`, and a `migrations/` of at most
//! [`MAX_MIGRATION_ENTRIES`] entries and [`MAX_MIGRATION_BYTES`] (past
//! either the check ends with `project-file`).
//!
//! Every message is written for a Coding agent to act on, and names the
//! fix. Messages hold text of the project's and of the installation's (a
//! file's name, a contact, a tool's words): with `--json` it is written as
//! JSON writes text; in words every message goes through [`Theirs`], which
//! escapes control characters (the tool's own words have none).

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, DirBuilder, File};
use std::io::{self, ErrorKind, Read};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::child::{self, Kept, Lines, Sink};
use super::credential_file::Place;
use super::pull::INDEX_FIRST_LINE;
use super::theirs::Theirs;
use super::{Done, Ending, Output, get_signed_in, project_of, read_kept, shaped};
use crate::resources::is_resource_name;
use crate::structure::FIRST_LINE as SCHEMA_FIRST_LINE;

/// `vm check`.
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

/// The check's time without `--timeout`.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// The oldest Node the check runs the project's tools with.
const NODE_MAJOR: u32 = 22;

/// What is kept of a tool's standard output or error.
const TEXT_BYTES: usize = 64 * 1024;

/// What is kept of the SDK's tooling's answer on the Manifest.
const MANIFEST_BYTES: usize = 1024 * 1024;

/// What is read of a package's `package.json`.
const PACKAGE_JSON_BYTES: u64 = 1024 * 1024;

/// The type errors reported one by one; the rest are counted.
const MAX_TYPE_ERRORS: usize = 50;

/// What is kept of one line of `tsc`'s output, and of one error's message.
const LINE_BYTES: usize = 16 * 1024;

/// The folders of `resources/` looked at for pulled Resources.
const MAX_PULLED: usize = 1000;

/// The entries (files and folders) of `migrations/` copied.
const MAX_MIGRATION_ENTRIES: usize = 10_000;

/// The bytes of `migrations/` copied.
const MAX_MIGRATION_BYTES: u64 = 256 * 1024 * 1024;

/// How deep `migrations/` is copied.
const MAX_MIGRATION_DEPTH: usize = 16;

/// The lines of a tool's own words put in a message.
const WORDS_LINES: usize = 12;

/// The characters of a tool's own words put in a message.
const WORDS_CHARS: usize = 2000;

/// How `drizzle-kit generate` is run by hand, for the messages.
const GENERATE: &str =
    "npx drizzle-kit generate --dialect postgresql --schema src/db/schema.ts --out migrations";

/// Where the check is, for the message of a check that ran out of time.
#[derive(Clone, Copy)]
enum Stage {
    SignIn,
    Node,
    Packages,
    Manifest,
    Resources,
    Migrations,
    Types,
}

impl Stage {
    fn words(self) -> &'static str {
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
    let project = project_of(args.project.as_deref(), "vm check")?;
    // Node resolves packages from the real path, and the tools are given
    // absolute paths.
    let project = fs::canonicalize(&project).map_err(|error| {
        Ending::new(
            1,
            "project-file",
            format!(
                "the project's folder cannot be read ({error}): make it readable by you, then run vm check again"
            ),
        )
    })?;
    let limit = args.timeout.map_or(DEFAULT_TIMEOUT, Duration::from_secs);
    let stage = Cell::new(Stage::SignIn);
    match tokio::time::timeout(limit, checked(&project, &stage)).await {
        Ok(ended) => ended,
        Err(_) => Err(Ending::new(
            1,
            "check-timed-out",
            format!(
                "the check did not end within {} s and was stopped, with everything it started; it was in {}. Look at what that runs (src/db/schema.ts and what it imports are run by drizzle-kit), or give more time with --timeout <seconds>, then run vm check again",
                limit.as_secs(),
                stage.get().words()
            ),
        )),
    }
}

/// A Resource as the check reads it from `GET /v1/resources`. Its texts are
/// only ever put in a message or in JSON (see the module's comment).
#[derive(Deserialize)]
struct Registered {
    name: String,
    contact: String,
    your_access: Option<String>,
}

#[derive(Deserialize)]
struct Registry {
    resources: Vec<Registered>,
}

/// The project's packages, as Node resolves them.
struct Packages {
    tooling: PathBuf,
    tsc: PathBuf,
    drizzle_kit: PathBuf,
}

/// What the four checks found.
#[derive(Default)]
struct Found {
    problems: Vec<Value>,
    warnings: Vec<Value>,
    notes: Vec<Value>,
}

async fn checked(project: &Path, stage: &Cell<Stage>) -> Result<Done, Ending> {
    let place = Place::from_environment().map_err(Ending::credential_file)?;
    let kept = read_kept(&place)?;
    let registry: Registry = shaped(kept.connect, get_signed_in(&kept, "/v1/resources").await?)?;

    stage.set(Stage::Node);
    node_is_there(project).await?;
    stage.set(Stage::Packages);
    let packages = packages(project)?;

    let mut found = Found::default();
    stage.set(Stage::Manifest);
    let declared = manifest(project, &packages, &mut found).await?;
    if let Some(declared) = &declared {
        stage.set(Stage::Resources);
        resources(project, declared, &registry, &mut found);
    }
    stage.set(Stage::Migrations);
    migrations(project, &packages, &mut found).await?;
    stage.set(Stage::Types);
    types(project, &packages, &mut found).await?;
    report(found)
}

/// `message` as words: the tool's own, with the project's and the
/// installation's text in it escaped.
fn words(message: &str) -> String {
    Theirs::from(message).to_string()
}

fn message_of(item: &Value) -> &str {
    item.get("message").and_then(Value::as_str).unwrap_or("")
}

fn report(found: Found) -> Result<Done, Ending> {
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
        "the check found {count} problem{s}; the first: {}",
        message_of(first)
    );
    if let Some(object) = body.as_object_mut() {
        object.insert(
            "error".into(),
            json!({ "reason": "check-failed", "message": message }),
        );
    }
    Err(Ending::check_failed(&message, Done { json: body, text }))
}

fn tool_failed(tool: &str, what: &str) -> Ending {
    Ending::new(
        1,
        "tool-failed",
        format!(
            "{tool} {what}: check that the project's packages are installed whole (run npm install), then run vm check again"
        ),
    )
}

/// That `node` is on the `PATH` and is 22 or later.
async fn node_is_there(project: &Path) -> Result<(), Ending> {
    let missing = |found: &str| {
        Ending::new(
            1,
            "node-missing",
            format!(
                "vm check runs the project's tools with Node {NODE_MAJOR} or later, and {found}: install Node {NODE_MAJOR} or later and put its node on the PATH, then run vm check again"
            ),
        )
    };
    let ran = match child::run(
        project,
        &[OsStr::new("--version")],
        Kept::up_to(TEXT_BYTES),
        Kept::up_to(TEXT_BYTES),
    )
    .await
    {
        Ok(ran) => ran,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Err(missing("there is no node on the PATH"));
        }
        Err(error) => return Err(missing(&format!("node cannot be started ({error})"))),
    };
    let printed = ran.out.text();
    let major = printed
        .trim()
        .strip_prefix('v')
        .and_then(|v| v.split('.').next())
        .and_then(|m| m.parse::<u32>().ok());
    match major {
        Some(major) if ran.status.success() && major >= NODE_MAJOR => Ok(()),
        Some(major) => Err(missing(&format!("the node on the PATH is Node {major}"))),
        None => Err(missing(&format!(
            "the node on the PATH does not give its version ({})",
            words(printed.trim())
        ))),
    }
}

/// The folder of the package `name`, as Node resolves it from `project`:
/// `node_modules/<name>` of the project or of the nearest folder above it
/// that has it.
fn package_dir(project: &Path, name: &str) -> Option<PathBuf> {
    project
        .ancestors()
        .map(|dir| dir.join("node_modules").join(name))
        .find(|dir| dir.join("package.json").is_file())
}

/// The program `bin` names in the `package.json` of `dir`: its `bin` when
/// that is one path, else its entry `command`.
fn bin_of(dir: &Path, command: &str) -> Option<PathBuf> {
    let mut text = String::new();
    File::open(dir.join("package.json"))
        .ok()?
        .take(PACKAGE_JSON_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    let package: Value = serde_json::from_str(&text).ok()?;
    let bin = match package.get("bin")? {
        Value::String(path) => path.as_str(),
        Value::Object(bins) => bins.get(command)?.as_str()?,
        _ => return None,
    };
    let path = dir.join(bin);
    path.is_file().then_some(path)
}

fn packages(project: &Path) -> Result<Packages, Ending> {
    let not_installed = |what: &str| {
        Ending::new(
            1,
            "not-installed",
            format!(
                "the project's packages are not installed ({what} does not resolve from the project as Node resolves it): run npm install in the project, then run vm check again"
            ),
        )
    };
    let tooling = package_dir(project, "@vm/sdk")
        .map(|dir| dir.join("tooling").join("main.js"))
        .filter(|main| main.is_file())
        .ok_or_else(|| not_installed("@vm/sdk"))?;
    let tsc = package_dir(project, "typescript")
        .and_then(|dir| bin_of(&dir, "tsc"))
        .ok_or_else(|| not_installed("typescript"))?;
    let drizzle_kit = package_dir(project, "drizzle-kit")
        .and_then(|dir| bin_of(&dir, "drizzle-kit"))
        .ok_or_else(|| not_installed("drizzle-kit"))?;
    Ok(Packages {
        tooling,
        tsc,
        drizzle_kit,
    })
}

/// How a tool ended, in words.
fn ended(status: std::process::ExitStatus) -> String {
    status.code().map_or_else(
        || "was stopped by a signal".to_owned(),
        |code| format!("ended with {code}"),
    )
}

/// The first lines of a tool's own words: standard error, then standard
/// output, the empty lines left out.
fn tool_words(err: &str, out: &str) -> String {
    let mut lines = Vec::new();
    for line in err.lines().chain(out.lines()) {
        let line = line.trim();
        if !line.is_empty() && lines.len() < WORDS_LINES {
            lines.push(line);
        }
    }
    let joined = lines.join(" / ");
    if joined.chars().count() > WORDS_CHARS {
        let cut: String = joined.chars().take(WORDS_CHARS).collect();
        format!("{cut} ...")
    } else {
        joined
    }
}

/// The `manifest` check: the Resources the Manifest declares, by name, with
/// the Access level of each; none when it cannot be read.
async fn manifest(
    project: &Path,
    packages: &Packages,
    found: &mut Found,
) -> Result<Option<BTreeMap<String, String>>, Ending> {
    let ran = child::run(
        project,
        &[
            packages.tooling.as_os_str(),
            OsStr::new("manifest"),
            project.as_os_str(),
        ],
        Kept::up_to(MANIFEST_BYTES),
        Kept::up_to(TEXT_BYTES),
    )
    .await
    .map_err(|error| tool_failed("node", &format!("cannot be started ({error})")))?;
    let answer: Option<Value> = if ran.out.over {
        None
    } else {
        serde_json::from_slice(ran.out.bytes()).ok()
    };
    match (ran.status.code(), answer) {
        (Some(0), Some(mut answer)) => {
            let mut declared = BTreeMap::new();
            if let Some(Value::Object(resources)) = answer
                .pointer_mut("/declared_access/resources")
                .map(Value::take)
            {
                for (name, resource) in resources {
                    let access = match resource.get("access") {
                        Some(Value::String(access)) => access.as_str(),
                        _ => "read",
                    };
                    let access = access.to_owned();
                    declared.insert(name, access);
                }
            }
            Ok(Some(declared))
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
                format!("{place}: {message} Correct manifest.ts, then run vm check again.");
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
                tool_words(&ran.err.text(), "")
            ),
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
fn resources(
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
                "message": format!("manifest.ts declares the Resource {name}, and no Resource of that name is registered on the installation ({those}): correct the name in manifest.ts, or ask a Platform administrator to register it, then run vm check again"),
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
/// written by `vm pull`.
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
                    "{shown} was not written by vm pull (it does not begin with `{line}`)"
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
        format!("move the file that vm pull did not write away first, then run `vm pull {name}`")
    } else {
        format!("run `vm pull {name}`")
    };
    found.problems.push(json!({
        "check": "resources",
        "reason": "resource-not-pulled",
        "resource": name,
        "message": format!("manifest.ts declares {name}, and it is not pulled: {}. To pull it, {fix}, then run vm check again", wrong.join("; ")),
    }));
}

/// A private folder outside the project, removed when this is dropped.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn make() -> io::Result<Self> {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|e| io::Error::other(e.to_string()))?;
        let hex: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let path = std::env::temp_dir().join(format!("vm-check-{hex}"));
        DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self { path })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).ok();
    }
}

/// How much of `migrations/` is left to copy.
struct Budget {
    entries: usize,
    bytes: u64,
}

fn too_large() -> io::Error {
    io::Error::other(format!(
        "it holds more than {MAX_MIGRATION_ENTRIES} files and folders, more than {} MiB, or folders more than {MAX_MIGRATION_DEPTH} deep",
        MAX_MIGRATION_BYTES / (1024 * 1024)
    ))
}

/// Copies the folder `from` into `to` (made): its files and folders, a
/// link to a file as the file, a link to a folder not at all.
fn copy_tree(from: &Path, to: &Path, budget: &mut Budget, depth: usize) -> io::Result<()> {
    if depth > MAX_MIGRATION_DEPTH {
        return Err(too_large());
    }
    fs::create_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        budget.entries = budget.entries.checked_sub(1).ok_or_else(too_large)?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&source, &target, budget, depth.saturating_add(1))?;
        } else if let Ok(meta) = fs::metadata(&source)
            && meta.is_file()
        {
            budget.bytes = budget.bytes.checked_sub(meta.len()).ok_or_else(too_large)?;
            fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

/// The path of `to` relative to `from`, both absolute: up to the root,
/// then down.
fn from_to(from: &Path, to: &Path) -> PathBuf {
    let mut path = PathBuf::new();
    for component in from.components() {
        if matches!(component, Component::Normal(_)) {
            path.push("..");
        }
    }
    for component in to.components() {
        if let Component::Normal(name) = component {
            path.push(name);
        }
    }
    path
}

/// The names of the `.sql` files in `dir`.
fn sql_files(dir: &Path) -> BTreeSet<String> {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.ends_with(".sql"))
                .take(MAX_MIGRATION_ENTRIES)
                .collect()
        })
        .unwrap_or_default()
}

/// The `migrations` check: `drizzle-kit generate` against a copy of
/// `migrations/`, outside the project.
async fn migrations(project: &Path, packages: &Packages, found: &mut Found) -> Result<(), Ending> {
    let scratch_failed = |error: io::Error| {
        Ending::new(
            1,
            "failed",
            format!(
                "vm check cannot make its private folder in the system's temporary folder ({error}): make room there or set TMPDIR to a folder you can write, then run vm check again"
            ),
        )
    };
    let scratch = Scratch::make().map_err(scratch_failed)?;
    let out = scratch.path.join("migrations");
    let own = project.join("migrations");
    if own.is_dir() {
        let mut budget = Budget {
            entries: MAX_MIGRATION_ENTRIES,
            bytes: MAX_MIGRATION_BYTES,
        };
        copy_tree(&own, &out, &mut budget, 0).map_err(|error| {
            Ending::new(
                1,
                "project-file",
                format!(
                    "migrations/ cannot be copied for the check ({error}): make it readable by you and keep it to the project's migrations, then run vm check again"
                ),
            )
        })?;
    } else {
        fs::create_dir(&out).map_err(scratch_failed)?;
    }
    let before = sql_files(&out);
    // drizzle-kit puts `./` in front of the folder it is given, so it is
    // given the copy's path relative to the project.
    let out_from_project = fs::canonicalize(&out)
        .map(|out| from_to(project, &out))
        .map_err(scratch_failed)?;
    let ran = child::run(
        project,
        &[
            packages.drizzle_kit.as_os_str(),
            OsStr::new("generate"),
            OsStr::new("--dialect"),
            OsStr::new("postgresql"),
            OsStr::new("--schema"),
            OsStr::new("src/db/schema.ts"),
            OsStr::new("--out"),
            out_from_project.as_os_str(),
        ],
        Kept::up_to(TEXT_BYTES),
        Kept::up_to(TEXT_BYTES),
    )
    .await
    .map_err(|error| tool_failed("node", &format!("cannot be started ({error})")))?;
    let written = sql_files(&out).difference(&before).count();
    let (out_text, err_text) = (ran.out.text(), ran.err.text());
    let said = |words: &str| out_text.contains(words) || err_text.contains(words);
    let behind = |message: String| json!({ "check": "migrations", "reason": "migrations-behind", "message": message });
    if written > 0 {
        found.problems.push(behind(format!(
            "src/db/schema.ts changed since the last migration in migrations/: generating migrations now would write one. Generate it with `{GENERATE}`, read the SQL it wrote (ask the Builder before one that destroys data), then run vm check again"
        )));
    } else if said("No schema changes") {
        // In step.
    } else if said("Interactive prompts require a TTY") {
        found.problems.push(behind(format!(
            "src/db/schema.ts changed since the last migration in migrations/, and drizzle-kit has a question about the change (a table or column renamed, or created anew?) that only a terminal can answer: run `{GENERATE}` in a terminal and answer it, or make the change in steps (add the new column, copy the values, remove the old one), then run vm check again"
        )));
    } else {
        found.problems.push(json!({
            "check": "migrations",
            "reason": "own-schema-failed",
            "message": format!(
                "drizzle-kit cannot load src/db/schema.ts (it {}: {}): correct src/db/schema.ts (it must be there, load without throwing, and import only what is there), then run vm check again",
                ended(ran.status),
                tool_words(&err_text, &out_text)
            ),
        }));
    }
    drop(scratch);
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
/// `tsconfig.json`.
async fn types(project: &Path, packages: &Packages, found: &mut Found) -> Result<(), Ending> {
    let ran = child::run(
        project,
        &[
            packages.tsc.as_os_str(),
            OsStr::new("--noEmit"),
            OsStr::new("-p"),
            OsStr::new("tsconfig.json"),
            OsStr::new("--pretty"),
            OsStr::new("false"),
        ],
        TypeErrors::new(),
        Kept::up_to(TEXT_BYTES),
    )
    .await
    .map_err(|error| tool_failed("node", &format!("cannot be started ({error})")))?;
    let errors = ran.out.read;
    for error in &errors.kept {
        let message = match (&error.file, error.line, error.column) {
            (Some(file), Some(line), Some(column)) => format!(
                "{file}, line {line}, column {column}: {}. Correct {file} at line {line}, then run vm check again",
                error.message.trim_end_matches('.')
            ),
            _ => format!(
                "{}. The compiler gives this with no place in a file: correct what it names, then run vm check again",
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
            "message": format!("and {more} more type error{s} after these {MAX_TYPE_ERRORS}: correct these first, then run vm check again to see the rest"),
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
                "tsc {} and gave no error it could place ({}): correct what it names, then run vm check again",
                ended(ran.status),
                tool_words(&ran.err.text(), &errors.other.text())
            )),
        );
        for field in ["file", "line", "column"] {
            problem.insert(field.into(), Value::Null);
        }
        found.problems.push(Value::Object(problem));
    }
    Ok(())
}
