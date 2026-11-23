//! `vibe deploy [--project <directory>] [--allow-data-loss] [--timeout
//! <seconds>] [--json]` (blackbox/CONTRACT.md, "Deploying").
//!
//! Made of what the other commands have: `vibe check`'s sign-in, packages and
//! four checks ([`check`]), `vibe pull` ([`Pull`]), `vibe generate`'s data loss
//! ([`data_loss`]) and journal ([`drizzle`]), the own folder, the time and
//! the signals ([`project_tools`]), and `git` run as the project's tools are
//! ([`Cx::git`]).
//!
//! **Order** (the contract's; each step ends the command when it does not
//! hold): the command line, the project, the sign-in (the kept credential
//! and `GET /v1/resources`); git (on the `PATH`, the project in a
//! repository with a commit, nothing of the project's folder
//! uncommitted); `node` and the packages; the Manifest (the SDK's
//! tooling) and the Application of its name (`not-a-contributor`), with
//! its latest Version's migrations; each
//! declared Resource that is registered and pulled, pulled again (a
//! changed structure written into `resources/<name>/`, the only files the
//! deploy writes in the project); the four checks; data loss (only when the
//! Application has a Version); the latest Version's migrations against the
//! project's (`migrations-diverged`); the bundle. The latest Version, not
//! the serving one: after going back they differ, and the latest's
//! migrations are the ones applied (blackbox/CONTRACT.md, "Deploying after
//! going back"). Only then is anything
//! sent: the Application created when there is none, and the Version.
//!
//! **Endings.** Everything up to the bundle is held to `--timeout` (300 s
//! without it, `deploy-timed-out`) and ended by `SIGINT`, `SIGTERM` and
//! `SIGHUP` (`interrupted`), as for `vibe check`: the future is dropped,
//! which stops every tool's process group, and the own folder is removed
//! after. Sending has its own ends: [`client::ANSWER_TIMEOUT`] to create
//! the Application, [`SEND_TIMEOUT`] for the Version's answer (migrations
//! run before it); past it, or interrupted while sending, the message says
//! the Version may or may not have gone live.
//!
//! **Bounds.** What is sent is at most [`MAX_VERSION_BYTES`] (the API's
//! limit): the bundle is kept to that much of the tooling's output, the
//! migrations are read to that much in all, and the body is measured
//! before anything is sent; past any of these the deploy ends `too-large`,
//! sending nothing. Of `git status`, each record is kept to
//! [`GIT_RECORD_BYTES`], the first [`MAX_FILES`] paths by path order, the
//! rest counted. Of the tooling's build errors, the first
//! [`MAX_BUILD_ERRORS`]. The rest as [`check`] and [`project_tools`] bound
//! them. A definition's second line is read in its first
//! [`DEFINITION_HEAD_BYTES`].
//!
//! **Blocking work**, as for `vibe check`: reading the project's files,
//! writing a changed definition, parsing the added migrations; bounded as
//! above, on the tool's one thread.
//!
//! Text of the installation's and of the project's reaches words only
//! through [`Theirs`].

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use bytes::Bytes;
use hyper::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::check::{self, Found, Manifest, Registry};
use super::child::{Kept as KeptOutput, Lines, Sink};
use super::client::{self, Failure, Sign};
use super::credential_file::{Kept, Place};
use super::data_loss::{self, Destroyed};
use super::drizzle::{self, Journal};
use super::project_tools::{
    Bounded, Cx, DEFAULT_TIMEOUT, Scratch, Signals, TEXT_BYTES, Who, ended, node_is_there,
    own_folder_failed, tool_failed,
};
use super::pull::Pull;
use super::sharing::{entry_words, visible_to_words};
use super::theirs::Theirs;
use super::{
    Done, Ending, Output, get_signed_in, installation, not_success, project_of, read_kept, shaped,
    sign_in_expired, unexpected,
};
use crate::migration_text;
use crate::registry::check_name;
use crate::resources::is_resource_name;
use crate::structure::fingerprint_in;

/// `vibe deploy`.
#[derive(clap::Args)]
pub struct DeployArgs {
    /// The project to deploy; without it, the current directory.
    #[arg(long)]
    pub project: Option<PathBuf>,
    /// Deploy although a migration this Version adds destroys data while the
    /// Application already has a Version. Only when the Builder has clearly
    /// said that this data may be lost.
    #[arg(long)]
    pub allow_data_loss: bool,
    /// The longest the deploy may take on this machine, in seconds (1 to
    /// 3600; 300 without it). Sending the Version has its own end.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=3600))]
    pub timeout: Option<u64>,
    #[command(flatten)]
    pub output: Output,
}

/// How the deploy's messages name it.
const WHO: Who = Who {
    command: "vibe deploy",
    the: "the deploy",
    folder: "vibe-deploy",
};

/// The longest the installation may take to answer a Version, once
/// connected: its migrations run before it answers.
const SEND_TIMEOUT: Duration = Duration::from_secs(120);

/// The most a Version may take: the API's limit on a body.
const MAX_VERSION_BYTES: usize = crate::api::MAX_BODY_BYTES;

/// The uncommitted files named one by one; the rest are counted.
const MAX_FILES: usize = 50;

/// What is kept of one record of `git status -z`.
const GIT_RECORD_BYTES: usize = 16 * 1024;

/// The build errors of the SDK's tooling given; the rest are dropped.
const MAX_BUILD_ERRORS: usize = 50;

/// What is read of a definition for its second line.
const DEFINITION_HEAD_BYTES: u64 = 4 * 1024;

/// Where the deploy is, for the message of one that ran out of time or was
/// interrupted.
#[derive(Clone, Copy)]
enum Stage {
    Check(check::Stage),
    Git,
    Application,
    Pulling,
    ReadingMigrations,
    Bundling,
}

impl Stage {
    fn words(self) -> &'static str {
        match self {
            Self::Check(stage) => stage.words(),
            Self::Git => "asking git whether everything is committed",
            Self::Application => "asking the installation for the Application",
            Self::Pulling => "pulling the declared Resources' structures again",
            Self::ReadingMigrations => "reading the migrations of migrations/",
            Self::Bundling => "bundling the Application (the SDK's tooling running esbuild)",
        }
    }
}

pub(super) async fn deploy(args: &DeployArgs) -> Result<Done, Ending> {
    let project = project_of(args.project.as_deref(), "vibe deploy")?;
    // Node resolves packages from the real path, and the tools are given
    // absolute paths.
    let project = std::fs::canonicalize(&project).map_err(|error| {
        Ending::new(
            1,
            "project-file",
            format!(
                "the project's folder cannot be read ({error}): make it readable by you, then run vibe deploy again"
            ),
        )
    })?;
    // In place before the first tool starts, and kept until the command
    // ends: a signal ends the work on the machine, and the sending.
    let mut signals = Signals::listen(&WHO)?;
    let own = Scratch::make(&WHO).map_err(|e| own_folder_failed(&e, &WHO))?;
    let limit = args.timeout.map_or(DEFAULT_TIMEOUT, Duration::from_secs);
    let stage = Cell::new(Stage::Check(check::Stage::SignIn));
    let prepared = match signals
        .within(limit, prepared(&project, &own.path, args, &stage))
        .await
    {
        Bounded::Ended(prepared) => prepared,
        Bounded::TimedOut => Err(Ending::new(
            1,
            "deploy-timed-out",
            format!(
                "vibe deploy did not end within {} s and was stopped, with everything it started; it was in {}, and nothing was sent to the installation. Look at what that runs (src/db/schema.ts and what it imports are run by drizzle-kit), or give more time with --timeout <seconds>, then run vibe deploy again",
                limit.as_secs(),
                stage.get().words()
            ),
        )),
        Bounded::Interrupted(signal) => Err(Ending::new(
            1,
            "interrupted",
            format!(
                "vibe deploy was interrupted ({signal}) in {}, and stopped everything it had started; nothing was sent to the installation: run vibe deploy again",
                stage.get().words()
            ),
        )),
    };
    // Nothing writes in the own folder any more.
    drop(own);
    let ready = prepared?;
    // Its own ends are the requests'; this one is above them, so that only
    // a signal ends the sending here.
    let sending = SEND_TIMEOUT
        .saturating_add(client::ANSWER_TIMEOUT)
        .saturating_add(client::CONNECT_TIMEOUT.saturating_mul(2))
        .saturating_add(Duration::from_secs(5));
    // Whether this deploy created the Application and it has no Version
    // that the tool knows of: set by `send`.
    let created = Cell::new(false);
    let sent = match signals.within(sending, send(&ready, &created)).await {
        Bounded::Ended(sent) => sent,
        Bounded::TimedOut => Err(maybe_live(
            "unreachable",
            &format!("it did not answer within {} s", sending.as_secs()),
        )),
        Bounded::Interrupted(signal) => Err(maybe_live(
            "interrupted",
            &format!("vibe deploy was interrupted ({signal}) while sending the Version"),
        )),
    };
    sent.map_err(|ending| {
        if created.get() {
            created_stays(ending, &ready.name)
        } else {
            ending
        }
    })
}

/// An ending after this deploy created the Application and before a
/// Version of it was taken: the Application stays, and the error says so
/// (`"created": true` beside its reason, and a sentence after its message).
fn created_stays(ending: Ending, name: &str) -> Ending {
    let unsure = ending.reason == json!("unreachable") || ending.reason == json!("interrupted");
    let sentence = if unsure {
        format!(
            "This deploy created the Application {name}, which stays: unless the Version went live, {name} now exists with no Version (nothing is at its address), and the next vibe deploy will send its first Version."
        )
    } else {
        format!(
            "This deploy created the Application {name}, which stays: it now exists with no Version (nothing is at its address), and the next vibe deploy will send its first Version."
        )
    };
    ending.amended(&sentence, "created", json!(true))
}

/// An ending while the Version was being sent: it may or may not have gone
/// live.
fn maybe_live(reason: &str, why: &str) -> Ending {
    Ending::new(
        1,
        reason,
        format!(
            "no answer the tool understands came back for the Version ({why}): it may or may not have gone live. Look with vibe applications (the Application's serving Version) before deploying again"
        ),
    )
}

/// The Application as the API answers it.
#[derive(Deserialize)]
struct Application {
    address: String,
    /// Its Contributors, entries as the API answers them.
    contributors: Vec<Value>,
    /// Whom it is shared with: null to someone who is not a Contributor.
    shared_with: Option<Vec<Value>>,
    /// Whether the User is one of its Contributors.
    contributor: bool,
    serving_version: Option<u64>,
    /// The highest-numbered Version, `None` when there is none.
    latest_version: Option<u64>,
    /// Since when and by whom a Platform administrator disabled it; null
    /// when it is not disabled.
    #[serde(default)]
    disabled: Value,
}

/// A Version's migrations as its record has them.
#[derive(Deserialize)]
struct VersionMigrations {
    migrations: Vec<Recorded>,
}

#[derive(Deserialize)]
struct Recorded {
    name: String,
    sha256: String,
}

/// The Application of the Manifest's name, when the installation has one.
struct Existing {
    application: Application,
    /// The latest Version's migrations, when the Application has a
    /// Version: the ones applied to its own database.
    latest: Option<Vec<Recorded>>,
}

/// One migration of `migrations/`, in the journal's order.
pub(super) struct ProjectMigration {
    pub name: String,
    pub sql: String,
}

/// Everything needed to send the Version, made on the machine.
struct Ready {
    kept: Kept,
    name: String,
    description: Option<String>,
    existing: Option<Existing>,
    /// The request's body, at most [`MAX_VERSION_BYTES`].
    body: Bytes,
    git_commit: String,
    added: Vec<String>,
    structures: BTreeMap<String, String>,
    pulled: Vec<Value>,
    warnings: Vec<Value>,
    notes: Vec<Value>,
}

/// How a declared Resource was pulled again.
enum Again {
    /// Pulled: its fingerprint (none for a Storage Resource, which has no
    /// structure), and the files written.
    Pulled {
        fingerprint: Option<String>,
        files: Vec<String>,
    },
    /// Not pulled: the User holds no Grant on it.
    NoGrant,
    /// Not pulled: its shared database did not answer.
    Unavailable,
    /// A Storage Resource not pulled again: its `storage.json` is used as
    /// it is, with a warning of its own.
    StorageNotPulled { no_grant: bool },
    /// Not registered or not pulled: left to the checks.
    Left,
}

/// Every step on the machine, in order; what to send.
async fn prepared(
    project: &Path,
    own: &Path,
    args: &DeployArgs,
    stage: &Cell<Stage>,
) -> Result<Ready, Ending> {
    let place = Place::from_environment().map_err(Ending::credential_file)?;
    let kept = read_kept(&place)?;
    let registry = check::registry(&kept).await?;
    let cx = Cx::new(project, own, &WHO);

    stage.set(Stage::Git);
    let git_commit = committed(&cx).await?;

    stage.set(Stage::Check(check::Stage::Node));
    node_is_there(&cx, &WHO).await?;
    stage.set(Stage::Check(check::Stage::Packages));
    let packages = check::packages(project, &WHO)?;

    let mut found = Found::default();
    stage.set(Stage::Check(check::Stage::Manifest));
    let manifest = check::manifest(&cx, &packages, &mut found, &WHO).await?;
    let existing = match &manifest {
        Some(manifest) => {
            stage.set(Stage::Application);
            application(&kept, manifest).await?
        }
        None => None,
    };

    stage.set(Stage::Pulling);
    let mut again = BTreeMap::new();
    if let Some(manifest) = &manifest {
        for name in manifest.declared.keys() {
            again.insert(
                name.as_str(),
                pull_again(project, name, &kept, &registry).await?,
            );
        }
    }

    if let Some(manifest) = &manifest {
        stage.set(Stage::Check(check::Stage::Resources));
        check::resources(
            project,
            &manifest.declared,
            &manifest.kinds,
            &registry,
            &mut found,
        );
    }
    stage.set(Stage::Check(check::Stage::Migrations));
    check::migrations(&cx, &packages, &mut found, &WHO).await?;
    stage.set(Stage::Check(check::Stage::Types));
    check::types(&cx, &packages, &mut found, &WHO).await?;

    let mut structures = BTreeMap::new();
    let mut pulled = Vec::new();
    let mut changed: Vec<(String, Vec<String>)> = Vec::new();
    let mut storage_changed: BTreeSet<String> = BTreeSet::new();
    for (name, how) in again {
        let files = match how {
            Again::Pulled { fingerprint, files } => {
                if let Some(fingerprint) = fingerprint {
                    structures.insert(name.to_owned(), fingerprint);
                } else {
                    // A Storage Resource's pull has no structure: it is
                    // never in `structures` (blackbox/CONTRACT.md,
                    // "Pulling a Storage Resource, checking and deploying").
                    storage_changed.insert(name.to_owned());
                }
                files
            }
            Again::NoGrant | Again::Unavailable => {
                let unread = match definition_fingerprint(project, name) {
                    Some(fingerprint) => {
                        structures.insert(name.to_owned(), fingerprint);
                        false
                    }
                    None => true,
                };
                not_pulled(name, matches!(how, Again::NoGrant), unread, &mut found);
                Vec::new()
            }
            Again::StorageNotPulled { no_grant } => {
                storage_not_pulled(name, no_grant, &mut found);
                Vec::new()
            }
            Again::Left => Vec::new(),
        };
        pulled.push(json!({ "resource": name, "changed": !files.is_empty(), "files": files }));
        if !files.is_empty() {
            changed.push((name.to_owned(), files));
        }
    }
    for (name, files) in &changed {
        // A Storage Resource's file holds its name and contact, never a
        // structure.
        let said = if storage_changed.contains(name) {
            format!(
                "{} changed since it was pulled: {} updated to it, and this Version was built against it. Commit {}",
                name,
                files.join(" and "),
                files.join(" and ")
            )
        } else {
            format!(
                "The structure of {name} changed since it was pulled: {} updated to it, and this Version was built against the new structure. Commit {}",
                files.join(" and "),
                files.join(" and ")
            )
        };
        found.notes.push(json!({
            "reason": "structure-updated",
            "resource": name,
            "files": files,
            "message": said,
        }));
    }
    if !found.problems.is_empty() {
        // The check's own ending, which a problem always makes.
        return Err(
            match check::report(found, &changed_preface(&changed, &storage_changed)) {
                Err(ending) => ending,
                Ok(_) => tool_failed("vibe deploy", "found problems and no ending", &WHO),
            },
        );
    }
    let Some(manifest) = manifest else {
        // A Manifest that cannot be read is a problem of the check's.
        return Err(tool_failed(
            "the SDK's tooling (manifest)",
            "gave no Manifest and no problem with it",
            &WHO,
        ));
    };

    stage.set(Stage::ReadingMigrations);
    let migrations = project_migrations(project, &WHO)?;
    let latest = existing.as_ref().and_then(|e| e.latest.as_ref());
    let applied: BTreeSet<&str> = latest
        .map(|list| list.iter().map(|m| m.name.as_str()).collect())
        .unwrap_or_default();
    let added: Vec<&ProjectMigration> = migrations
        .iter()
        .filter(|m| !applied.contains(m.name.as_str()))
        .collect();
    if latest.is_some() && !args.allow_data_loss {
        loses_nothing(project, &migrations, &added)?;
    }
    if let Some(latest) = latest {
        continues(latest, &migrations)?;
    }

    stage.set(Stage::Bundling);
    let bundle = bundle(&cx, &packages, &WHO).await?;
    let body = body(
        &bundle,
        &manifest,
        &git_commit,
        &migrations,
        &structures,
        &WHO,
    )?;
    Ok(Ready {
        kept,
        // The names only, once, for what is printed.
        added: added.iter().map(|m| m.name.clone()).collect(),
        name: manifest.name,
        description: manifest.description,
        existing,
        body,
        git_commit,
        structures,
        pulled,
        warnings: found.warnings,
        notes: found.notes,
    })
}

/// What the message of a failed deploy says first when a pull changed
/// files: the structures of the databases among them, and the files of the
/// Storage Resources, whose pull has no structure.
fn changed_preface(changed: &[(String, Vec<String>)], storages: &BTreeSet<String>) -> String {
    if changed.is_empty() {
        return String::new();
    }
    let databases: Vec<&str> = changed
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| !storages.contains(*name))
        .collect();
    let storage: Vec<&str> = changed
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| storages.contains(*name))
        .collect();
    let files: Vec<&str> = changed
        .iter()
        .flat_map(|(_, files)| files.iter().map(String::as_str))
        .collect();
    let mut said = String::new();
    if !databases.is_empty() {
        said.push_str(&format!(
            "The structure of {} changed since it was pulled, and the project was checked against the new structure; ",
            databases.join(", ")
        ));
    }
    if !storage.is_empty() {
        said.push_str(&format!(
            "{} changed since it was pulled; ",
            storage.join(", ")
        ));
    }
    format!("{said}{} updated to it (commit them); ", files.join(", "))
}

/// The warning for a declared Storage Resource that could not be pulled
/// again: the project's `storage.json` is used as it is. It says nothing of
/// a structure or of `schema.ts`, which the Resource has none of.
fn storage_not_pulled(name: &str, no_grant: bool, found: &mut Found) {
    let why = if no_grant {
        "you hold no Grant on it"
    } else {
        "its pull did not answer"
    };
    found.warnings.push(json!({
        "reason": "storage-not-pulled",
        "resource": name,
        "message": format!(
            "The Storage Resource {name} could not be pulled again ({why}), so resources/{name}/storage.json was used as it is. Pull it again with `vibe pull {name}` when the installation takes it, and deploy again"
        ),
    }));
}

/// The warning for a declared database Resource that could not be pulled
/// again: added to the check's `own-grant-below-declared` for it when the
/// User holds no Grant, else `structure-not-pulled`.
fn not_pulled(name: &str, no_grant: bool, unread: bool, found: &mut Found) {
    let why = if no_grant {
        "you hold no Grant on it"
    } else {
        "its shared database did not answer"
    };
    let fingerprint = if unread {
        " Its fingerprint could not be read from the definition's second line, so the Version records none for it."
    } else {
        ""
    };
    let said = format!(
        "The structure of {name} could not be pulled again ({why}), so the definition in the project, resources/{name}/schema.ts, was used as it is.{fingerprint}"
    );
    if no_grant
        && let Some(warning) = found.warnings.iter_mut().find(|w| {
            w.get("reason") == Some(&json!("own-grant-below-declared"))
                && w.get("resource") == Some(&json!(name))
        })
        && let Some(Value::String(message)) = warning.get_mut("message")
    {
        message.push_str(". ");
        message.push_str(&said);
        return;
    }
    found.warnings.push(json!({
        "reason": "structure-not-pulled",
        "resource": name,
        "message": format!("{said} Pull it again with `vibe pull {name}` when it answers, and deploy again to build against its structure as it is then"),
    }));
}

/// Step 5 for one declared Resource: pulled again when it is registered and
/// pulled; a refusal for want of a Grant, or a shared database that does
/// not answer, leaves the definition as it is. A Storage Resource's pull
/// writes `storage.json`, never a structure; one that is not pulled is left
/// to the checks.
async fn pull_again(
    project: &Path,
    name: &str,
    kept: &Kept,
    registry: &Registry,
) -> Result<Again, Ending> {
    let Some(kind) = registry.kind_of(name) else {
        return Ok(Again::Left);
    };
    let storage = kind == "storage";
    if !is_resource_name(name)
        || registry.access_of(name).is_none()
        || if storage {
            !check::is_storage_pulled(project, name)
        } else {
            !check::is_pulled(project, name)
        }
    {
        return Ok(Again::Left);
    }
    let pull = Pull::prepare(project, name, "vibe deploy", true)?;
    match pull.fetch(kept).await {
        Ok(pulled) => Ok(Again::Pulled {
            fingerprint: pulled.fingerprint,
            files: pulled
                .files
                .into_iter()
                .filter_map(|(shown, written)| written.then_some(shown))
                .collect(),
        }),
        // A Storage Resource not pulled again uses the `storage.json` of the
        // project, with a warning of its own: nothing of a structure, which
        // it has none of.
        Err(ending) if storage && ending.reason == json!("no-grant") => {
            Ok(Again::StorageNotPulled { no_grant: true })
        }
        Err(ending) if storage && ending.reason == json!("structure-unavailable") => {
            Ok(Again::StorageNotPulled { no_grant: false })
        }
        Err(ending) if ending.reason == json!("no-grant") => Ok(Again::NoGrant),
        Err(ending) if ending.reason == json!("structure-unavailable") => Ok(Again::Unavailable),
        Err(ending) => Err(ending),
    }
}

/// The fingerprint the second line of `resources/<name>/schema.ts` gives.
fn definition_fingerprint(project: &Path, name: &str) -> Option<String> {
    let mut head = Vec::new();
    File::open(project.join("resources").join(name).join("schema.ts"))
        .ok()?
        .take(DEFINITION_HEAD_BYTES)
        .read_to_end(&mut head)
        .ok()?;
    fingerprint_in(&String::from_utf8_lossy(&head), name).map(str::to_owned)
}

/// Step 4: the Application of the Manifest's name, when there is one, of
/// which the User must be a Contributor; and its latest Version's
/// migrations, which are the ones applied (also after going back, when
/// another Version serves).
async fn application(kept: &Kept, manifest: &Manifest) -> Result<Option<Existing>, Ending> {
    let name = &manifest.name;
    if check_name(name).is_err() {
        return Err(tool_failed(
            "the SDK's tooling (manifest)",
            "gave a name the platform does not take",
            &WHO,
        ));
    }
    let reply = client::send(
        &installation(kept),
        Method::GET,
        &format!("/v1/applications/{name}"),
        Sign::Credential(&kept.credential),
        None,
    )
    .await
    .map_err(|f| super::failed(kept.at(), f))?;
    let application: Application = match reply.status {
        StatusCode::OK => shaped(
            kept.at(),
            reply
                .json
                .ok_or_else(|| unexpected(kept.at(), "its answer is not JSON"))?,
        )?,
        StatusCode::NOT_FOUND => return Ok(None),
        StatusCode::UNAUTHORIZED => return Err(sign_in_expired(kept)),
        // It exists, and the User cannot open it: none of its Contributors
        // is named, as the API names none.
        StatusCode::FORBIDDEN if refusal_is(&reply, "not-shared") => {
            return Err(Ending::new(
                3,
                "not-a-contributor",
                format!(
                    "An Application named {name} exists on the installation and is not shared with you, so you cannot deploy to it. Choose another `name` in manifest.ts (it becomes the address), commit, then run vibe deploy again; or, if that Application is meant to be this project, ask whoever maintains it to make you a Contributor. Nothing was built or sent"
                ),
            ));
        }
        _ => return Err(not_success(kept.at(), &reply)),
    };
    if !application.contributor {
        let names: Vec<String> = application
            .contributors
            .iter()
            .map(|entry| entry_words(kept, entry))
            .collect();
        // The words of entries are escaped already ([`entry_words`]).
        return Err(Ending::new(
            3,
            "not-a-contributor",
            format!(
                "The installation already has an Application named {name}, and you are not one of its Contributors ({}), so you cannot deploy to it. Ask one of them to make you a Contributor, or choose another `name` in manifest.ts (it becomes the address), commit, then run vibe deploy again; nothing was built or sent",
                names.join(", ")
            ),
        ));
    }
    // That one Version, read by itself: never the list of them.
    let latest = match application.latest_version {
        Some(number) => {
            let version: VersionMigrations = shaped(
                kept.at(),
                get_signed_in(kept, &format!("/v1/applications/{name}/versions/{number}")).await?,
            )?;
            Some(version.migrations)
        }
        None => None,
    };
    Ok(Some(Existing {
        application,
        latest,
    }))
}

/// What `git status --porcelain=v1 -z` names, read as it comes: the first
/// [`MAX_FILES`] paths by path order, relative to the project, and how many
/// there are in all.
struct Uncommitted {
    records: Lines,
    read: Named,
}

struct Named {
    /// The project's folder relative to the repository's top, as git gives
    /// it (`app/`, or empty).
    prefix: String,
    first: BTreeSet<String>,
    count: usize,
}

impl Named {
    fn record(&mut self, record: &str) {
        // `XY <path>`.
        let Some(path) = record.get(3..) else {
            return;
        };
        let path = path.strip_prefix(self.prefix.as_str()).unwrap_or(path);
        self.count = self.count.saturating_add(1);
        self.first.insert(path.to_owned());
        if self.first.len() > MAX_FILES {
            self.first.pop_last();
        }
    }
}

impl Sink for Uncommitted {
    fn take(&mut self, bytes: &[u8]) {
        let read = &mut self.read;
        self.records.feed(bytes, |record| read.record(record));
    }

    fn end(&mut self) {
        let read = &mut self.read;
        self.records.finish(|record| read.record(record));
    }
}

/// `git <args>` in the project: how it ended, its standard output, and its
/// words; `git-missing` when there is no `git` on the `PATH`.
async fn git(cx: &Cx<'_>, args: &[&str]) -> Result<(bool, String, String), Ending> {
    let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    let ran = run_git(cx, &args, KeptOutput::up_to(TEXT_BYTES)).await?;
    let out = ran.out.text();
    let words = cx.words(&ran.err.text(), "");
    Ok((ran.status.success(), out, words))
}

async fn run_git<O: Sink>(
    cx: &Cx<'_>,
    args: &[&OsStr],
    out: O,
) -> Result<super::child::Ran<O, KeptOutput>, Ending> {
    cx.git(args, out, KeptOutput::up_to(TEXT_BYTES))
        .await
        .map_err(|error| {
            let found = if error.kind() == ErrorKind::NotFound {
                "there is no git on the PATH".to_owned()
            } else {
                format!("git cannot be started ({error})")
            };
            Ending::new(
                1,
                "git-missing",
                format!(
                    "vibe deploy needs git, to send the commit the project is at and to see that everything is committed, and {found}: install git and put it on the PATH, then run vibe deploy again"
                ),
            )
        })
}

/// Step 2: the project is in a repository with a commit, and nothing of
/// its folder is uncommitted; the commit `HEAD` names.
async fn committed(cx: &Cx<'_>) -> Result<String, Ending> {
    let not_a_repository = |why: &str| {
        Ending::theirs(
            1,
            "not-a-repository",
            &format!(
                "the project is not in a Git repository with a commit ({why}): a deploy sends the commit the project is at. Make it one (git init, then commit everything), then run vibe deploy again"
            ),
        )
    };
    let (ok, prefix, words) = git(cx, &["rev-parse", "--show-prefix"]).await?;
    if !ok {
        return Err(not_a_repository(&format!("git says: {words}")));
    }
    let prefix = prefix.strip_suffix('\n').unwrap_or(&prefix).to_owned();
    let (ok, head, _) = git(cx, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]).await?;
    if !ok {
        return Err(not_a_repository("the repository has no commit yet"));
    }
    let head = head.trim().to_owned();
    if head.len() != 40 || !head.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Ending::new(
            1,
            "not-a-repository",
            "the repository's commits are not named by 40 hex digits (a SHA-256 repository?), and the platform records a commit by them: deploy from a repository with SHA-1 commits"
                .into(),
        ));
    }
    let status = [
        "--no-optional-locks",
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
        "--no-renames",
        "--",
        ".",
    ];
    let args: Vec<&OsStr> = status.iter().map(OsStr::new).collect();
    let sink = Uncommitted {
        records: Lines::records_of_at_most(GIT_RECORD_BYTES),
        read: Named {
            prefix,
            first: BTreeSet::new(),
            count: 0,
        },
    };
    let ran = run_git(cx, &args, sink).await?;
    if !ran.status.success() {
        return Err(Ending::theirs(
            1,
            "failed",
            &format!(
                "git status {} ({}): look at what git says, then run vibe deploy again",
                ended(ran.status),
                cx.words(&ran.err.text(), "")
            ),
        ));
    }
    let named = ran.out.read;
    if named.count > 0 {
        return Err(uncommitted(&named));
    }
    Ok(head.to_ascii_lowercase())
}

fn uncommitted(named: &Named) -> Ending {
    let files: Vec<&str> = named.first.iter().map(String::as_str).collect();
    let more = named.count.saturating_sub(files.len());
    let and_more = if more > 0 {
        format!(", and {more} more")
    } else {
        String::new()
    };
    let message = format!(
        "files of the project are not committed (changed, staged, removed or untracked): {}{and_more}. A deploy sends the commit the project is at, so everything must be committed: commit them (or add what is not part of the Application to .gitignore), then run vibe deploy again; nothing was sent",
        files.join(", ")
    );
    let mut error = Map::new();
    error.insert("reason".into(), json!("uncommitted-changes"));
    error.insert("message".into(), json!(message));
    error.insert("files".into(), json!(files));
    if more > 0 {
        error.insert("more".into(), json!(more));
    }
    Ending::detailed(
        1,
        "uncommitted-changes",
        &message,
        Done {
            json: json!({ "error": error }),
            text: format!("{}\n", Theirs::from(message.as_str())),
        },
    )
}

/// The migrations of `migrations/`, in the journal's order (none without a
/// journal), read to [`MAX_VERSION_BYTES`] in all.
pub(super) fn project_migrations(
    project: &Path,
    who: &Who,
) -> Result<Vec<ProjectMigration>, Ending> {
    let folder = project.join("migrations");
    let unreadable = |what: String| {
        Ending::theirs(
            1,
            "migrations-unreadable",
            &format!(
                "{what}: the migrations cannot be read. Bring migrations/ back as it was last committed (migrations are never edited by hand), then run {} again",
                who.command
            ),
        )
    };
    let journal = match File::open(folder.join("meta").join("_journal.json")) {
        Ok(file) => drizzle::journal_bytes(file)
            .map_err(|e| {
                unreadable(format!(
                    "migrations/meta/_journal.json cannot be read ({e})"
                ))
            })?
            .ok_or_else(|| unreadable("migrations/meta/_journal.json is too large".into()))?,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(unreadable(format!(
                "migrations/meta/_journal.json cannot be read ({error})"
            )));
        }
    };
    let journal: Journal = serde_json::from_slice(&journal).map_err(|e| {
        unreadable(format!(
            "migrations/meta/_journal.json is not a journal ({e})"
        ))
    })?;
    let mut left = u64::try_from(MAX_VERSION_BYTES).unwrap_or(u64::MAX);
    let mut migrations = Vec::with_capacity(journal.entries.len());
    for entry in journal.entries {
        let shown = format!("migrations/{}.sql", entry.tag);
        let bytes = drizzle::read_bounded(&folder.join(format!("{}.sql", entry.tag)), left)
            .map_err(|e| unreadable(format!("{shown} cannot be read ({e})")))?
            .ok_or_else(|| too_large(who))?;
        left = left.saturating_sub(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        let sql = String::from_utf8(bytes)
            .map_err(|_| unreadable(format!("{shown} is not UTF-8 text")))?;
        migrations.push(ProjectMigration {
            name: entry.tag,
            sql,
        });
    }
    Ok(migrations)
}

fn too_large(who: &Who) -> Ending {
    let mib = MAX_VERSION_BYTES / (1024 * 1024);
    let message = if who.command == WHO.command {
        format!(
            "the Version (its bundle and its migrations together) is larger than the {mib} MiB the installation takes, so nothing was sent: make the bundle smaller (fewer or smaller packages imported), then run vibe deploy again"
        )
    } else {
        format!(
            "the project's Version (its bundle and its migrations together) is larger than the {mib} MiB an installation takes, so the local run does not run it either: make the bundle smaller (fewer or smaller packages imported, less text in the code), then run {} again",
            who.command
        )
    };
    Ending::new(1, "too-large", message)
}

/// Step 7: no migration this Version adds destroys data, as
/// [`data_loss::decide`] decides it with rows; else `data-loss`.
fn loses_nothing(
    project: &Path,
    migrations: &[ProjectMigration],
    added: &[&ProjectMigration],
) -> Result<(), Ending> {
    let meta = project.join("migrations").join("meta");
    let mut stopping = Vec::new();
    for migration in added {
        let statements = migration_text::parsed(&migration.sql).map_err(|refused| {
            Ending::theirs(
                1,
                "migration-unparseable",
                &format!(
                    "migrations/{}.sql cannot be read by Postgres's parser, from line {}{}: correct it (if you wrote it by hand) or bring migrations/ back as it was last committed, then run vibe deploy again; nothing was sent",
                    migration.name,
                    refused.line,
                    refused.words.map(|w| format!(" ({w})")).unwrap_or_default()
                ),
            )
        })?;
        let previous = migrations
            .iter()
            .position(|m| m.name == migration.name)
            .and_then(|at| at.checked_sub(1))
            .and_then(|at| migrations.get(at));
        let decision = data_loss::decide(
            &migration.sql,
            &statements,
            || {
                let number = drizzle::number_of(&previous?.name)?;
                drizzle::read_bounded(
                    &meta.join(format!("{number}_snapshot.json")),
                    drizzle::MAX_SNAPSHOT_BYTES,
                )
                .ok()
                .flatten()
            },
            true,
        );
        if !decision.destroyed.is_empty() {
            stopping.push((migration.name.as_str(), decision));
        }
    }
    if stopping.is_empty() {
        return Ok(());
    }
    let mut lost = Vec::new();
    let mut items = Vec::new();
    let mut text = String::new();
    for (name, decision) in &stopping {
        let all: Vec<&Destroyed> = decision.destroyed.iter().map(|(_, d)| d).collect();
        lost.push(format!("{name}: {}", data_loss::lost(&all)));
        let statements = data_loss::destroying(&decision.destroyed, &decision.statements);
        text.push_str(&format!(
            "The statements of {} that destroy it:\n",
            Theirs::from(*name)
        ));
        for statement in &statements {
            data_loss::push_statement(&mut text, statement);
        }
        items.push(json!({
            "migration": name,
            "destroys": all.iter().map(|d| &d.json).collect::<Vec<_>>(),
            "statements": statements,
        }));
    }
    let message = format!(
        "A migration this Version adds destroys data, and the Application already has a Version, so the rows are there now: this is asked again at deploy, even when vibe generate was allowed to write it, and nothing was sent. {}",
        data_loss::ask_the_builder(&lost.join("; "), "vibe deploy")
    );
    let text = format!("{}\n{text}", Theirs::from(message.as_str()));
    Err(Ending::detailed(
        3,
        "data-loss",
        &message,
        Done {
            json: json!({ "error": {
                "reason": "data-loss",
                "message": message,
                "migrations": items,
            } }),
            text,
        },
    ))
}

/// Step 8: the latest Version's migrations are the beginning of the
/// project's, by name and by the SHA-256 of their text.
fn continues(latest: &[Recorded], migrations: &[ProjectMigration]) -> Result<(), Ending> {
    for (at, recorded) in latest.iter().enumerate() {
        let ours = migrations.get(at);
        let why = match ours {
            None => format!(
                "the Application's latest Version has the migration {}, which migrations/ does not have",
                recorded.name
            ),
            Some(ours) if ours.name != recorded.name => format!(
                "the latest Version's migration number {} is {}, and the project's is {}",
                at.saturating_add(1),
                recorded.name,
                ours.name
            ),
            Some(ours) if hex(&Sha256::digest(ours.sql.as_bytes())) != recorded.sha256 => {
                format!(
                    "the migration {} differs from the one the latest Version has (its text changed since it was deployed)",
                    recorded.name
                )
            }
            Some(_) => continue,
        };
        return Err(Ending::theirs(
            1,
            "migrations-diverged",
            &format!(
                "The project's migrations do not continue the Application's latest Version's (its migrations are the ones applied, also after going back): {why}. Bring migrations/ in line with the repository's history (a deployed migration is never changed or removed; another Contributor's migrations may not be pulled into your copy yet: git pull), then run vibe deploy again; nothing was sent"
            ),
        ));
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The bundle: the SDK's tooling's `bundle`, kept to [`MAX_VERSION_BYTES`].
pub(super) async fn bundle(
    cx: &Cx<'_>,
    packages: &check::Packages,
    who: &Who,
) -> Result<String, Ending> {
    let ran = cx
        .run(
            &[
                packages.tooling.as_os_str(),
                OsStr::new("bundle"),
                cx.project.as_os_str(),
            ],
            KeptOutput::up_to(MAX_VERSION_BYTES),
            KeptOutput::up_to(TEXT_BYTES),
        )
        .await
        .map_err(|error| tool_failed("node", &format!("cannot be started ({error})"), who))?;
    match ran.status.code() {
        Some(0) if ran.out.over => Err(too_large(who)),
        Some(0) => String::from_utf8(ran.out.into_bytes()).map_err(|_| {
            tool_failed(
                "the SDK's tooling (bundle)",
                "wrote a bundle that is not UTF-8",
                who,
            )
        }),
        Some(2) => {
            let answer: Option<Value> = serde_json::from_slice(ran.out.bytes()).ok();
            let Some(mut error) = answer
                .and_then(|mut a| a.get_mut("error").map(Value::take))
                .filter(Value::is_object)
            else {
                return Err(tool_failed(
                    "the SDK's tooling (bundle)",
                    "ended with 2 without an answer it understands",
                    who,
                ));
            };
            let mut errors = match error.get_mut("errors").map(Value::take) {
                Some(Value::Array(errors)) => errors,
                _ => Vec::new(),
            };
            errors.truncate(MAX_BUILD_ERRORS);
            let said = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("The project does not build");
            // A deploy checked the types and sends nothing; the local run
            // builds what is there, committed or not.
            let next = if who.command == WHO.command {
                "It type-checks, and esbuild cannot bundle it: correct what the errors name (an import of a package that is not installed, or of one that runs only on Node), commit, then run vibe deploy again; nothing was sent".to_owned()
            } else {
                format!(
                    "esbuild cannot bundle it: correct what the errors name (an import of a package that is not installed, or of one that runs only on Node), then run {} again",
                    who.command
                )
            };
            let message = format!("{said}. {next}");
            Err(Ending::detailed(
                1,
                "build-failed",
                &message,
                Done {
                    json: json!({ "error": {
                        "reason": "build-failed",
                        "message": message,
                        "errors": errors,
                    } }),
                    text: format!("{}\n", Theirs::from(message.as_str())),
                },
            ))
        }
        _ => Err(tool_failed(
            "the SDK's tooling (bundle)",
            &format!(
                "{} without a bundle ({})",
                ended(ran.status),
                cx.words(&ran.err.text(), "")
            ),
            who,
        )),
    }
}

/// The Version's body, at most [`MAX_VERSION_BYTES`].
pub(super) fn body(
    bundle: &str,
    manifest: &Manifest,
    git_commit: &str,
    migrations: &[ProjectMigration],
    structures: &BTreeMap<String, String>,
    who: &Who,
) -> Result<Bytes, Ending> {
    /// The body of `POST .../versions`, borrowing what it holds.
    #[derive(Serialize)]
    struct Version<'a> {
        bundle: &'a str,
        declared_access: &'a Value,
        git_commit: &'a str,
        migrations: Vec<Migration<'a>>,
        structures: &'a BTreeMap<String, String>,
    }
    #[derive(Serialize)]
    struct Migration<'a> {
        name: &'a str,
        sql: &'a str,
    }
    // The one copy of what is sent: the bundle and the migrations, written
    // once, as the request's body.
    let body = serde_json::to_vec(&Version {
        bundle,
        declared_access: &manifest.declared_access,
        git_commit,
        migrations: migrations
            .iter()
            .map(|m| Migration {
                name: &m.name,
                sql: &m.sql,
            })
            .collect(),
        structures,
    })
    .map_err(|_| tool_failed(who.command, "cannot write the Version as JSON", who))?;
    if body.len() > MAX_VERSION_BYTES {
        return Err(too_large(who));
    }
    Ok(Bytes::from(body))
}

/// The Application created: `POST /v1/applications`.
async fn create(ready: &Ready) -> Result<Application, Ending> {
    let kept = &ready.kept;
    let body = serde_json::to_vec(&json!({ "name": ready.name, "description": ready.description }))
        .map_err(|_| tool_failed("vibe deploy", "cannot write the Application as JSON", &WHO))?;
    let reply = client::send(
        &installation(kept),
        Method::POST,
        "/v1/applications",
        Sign::Credential(&kept.credential),
        Some(Bytes::from(body)),
    )
    .await
    .map_err(|f| super::failed(kept.at(), f))?;
    match reply.status {
        StatusCode::CREATED => shaped(
            kept.at(),
            reply
                .json
                .ok_or_else(|| unexpected(kept.at(), "its answer is not JSON"))?,
        ),
        StatusCode::UNAUTHORIZED => Err(sign_in_expired(kept)),
        _ => Err(not_success(kept.at(), &reply)),
    }
}

/// Whether `reply` is the API's refusal with this `reason`.
fn refusal_is(reply: &client::Reply, reason: &str) -> bool {
    reply
        .json
        .as_ref()
        .and_then(|body| body.get("error"))
        .and_then(|error| error.get("reason"))
        .and_then(Value::as_str)
        == Some(reason)
}

/// What `POST .../versions` answers.
#[derive(Deserialize)]
struct SentVersion {
    number: u64,
}

/// The Application created when there is none, then the Version sent.
async fn send(ready: &Ready, creating: &Cell<bool>) -> Result<Done, Ending> {
    let kept = &ready.kept;
    let (application, created) = match &ready.existing {
        Some(_) => (None, false),
        None => (Some(create(ready).await?), true),
    };
    creating.set(created);
    let application = match (&application, &ready.existing) {
        (Some(made), _) => made,
        (None, Some(existing)) => &existing.application,
        (None, None) => return Err(tool_failed("vibe deploy", "has no Application", &WHO)),
    };
    let reply = client::send_waiting(
        &installation(kept),
        Method::POST,
        &format!("/v1/applications/{}/versions", ready.name),
        Sign::Credential(&kept.credential),
        // A cheap clone: `Bytes` shares the one body.
        Some(Bytes::clone(&ready.body)),
        SEND_TIMEOUT,
    )
    .await
    .map_err(|failure| match failure {
        Failure::Unreachable(why) | Failure::Unexpected(why) => maybe_live("unreachable", &why),
        // The handshake failed, or the authorities could not be read:
        // nothing of the Version was sent.
        refused @ (Failure::Certificate(_) | Failure::CaFile(_)) => {
            super::failed(installation(kept), refused)
        }
    })?;
    let serving = ready
        .existing
        .as_ref()
        .and_then(|existing| existing.application.serving_version);
    match reply.status {
        StatusCode::CREATED => creating.set(false),
        StatusCode::UNAUTHORIZED => return Err(sign_in_expired(kept)),
        _ => return Err(refused(kept, &reply, serving)),
    }
    let Some(sent) = reply
        .json
        .and_then(|body| serde_json::from_value::<SentVersion>(body).ok())
    else {
        return Err(Ending::new(
            1,
            "unexpected",
            format!(
                "the Version was taken and went live, but the installation's answer could not be read for its number: look with vibe applications ({}), and tell the Builder",
                ready.name
            ),
        ));
    };
    Ok(deployed(ready, application, created, sent.number))
}

/// What the installation answered instead of the Version.
fn refused(kept: &Kept, reply: &client::Reply, serving: Option<u64>) -> Ending {
    let error = reply.json.as_ref().and_then(|body| body.get("error"));
    let field = |name: &str| error.and_then(|e| e.get(name)).and_then(Value::as_str);
    match (reply.status, field("reason")) {
        (StatusCode::UNPROCESSABLE_ENTITY, Some("migration-failed")) => {
            let migration = field("migration").unwrap_or_default();
            let code = field("code").unwrap_or_default();
            let database = field("database_message").unwrap_or_default();
            let serving = serving
                .map(|n| format!(" Version {n} goes on serving."))
                .unwrap_or_default();
            let message = format!(
                "The migration {migration} failed in the Application's own database: {database} (SQLSTATE {code}). Nothing was changed: the Application's own database is as it was.{serving} The rows already there may not fit what the migration does (a column made NOT NULL over rows without a value, say): tell the Builder, correct src/db/schema.ts or the migration while it is not deployed, commit, then run vibe deploy again"
            );
            detailed_refusal(
                "migration-failed",
                &message,
                &[
                    ("migration", migration),
                    ("code", code),
                    ("database_message", database),
                ],
            )
        }
        (StatusCode::UNPROCESSABLE_ENTITY, Some("migration-refused")) => {
            let migration = field("migration").unwrap_or_default();
            let rule = field("rule").unwrap_or_default();
            let said = field("message").unwrap_or_default();
            let ended = if said.ends_with('.') { "" } else { "." };
            let message = format!(
                "The installation refused the Version: {said}{ended} Correct the migration while it is not deployed, commit, then run vibe deploy again"
            );
            detailed_refusal(
                "migration-refused",
                &message,
                &[("migration", migration), ("rule", rule)],
            )
        }
        (StatusCode::CONFLICT | StatusCode::PAYLOAD_TOO_LARGE, Some(_)) => {
            // Not a person's to decide: a failure of the Version's own.
            let mut ending = not_success(kept.at(), reply);
            ending.code = 1;
            ending
        }
        _ => not_success(kept.at(), reply),
    }
}

/// A refusal of the installation's with fields of its own beside the
/// reason and the message.
pub(super) fn detailed_refusal(reason: &str, message: &str, fields: &[(&str, &str)]) -> Ending {
    let mut error = Map::new();
    error.insert("reason".into(), json!(reason));
    error.insert("message".into(), json!(message));
    for (name, value) in fields {
        error.insert((*name).into(), json!(value));
    }
    Ending::detailed(
        1,
        reason,
        message,
        Done {
            json: json!({ "error": error }),
            text: format!("{}\n", Theirs::from(message)),
        },
    )
}

/// What a deploy that went live prints.
fn deployed(ready: &Ready, application: &Application, created: bool, number: u64) -> Done {
    // A deploy is by a Contributor, to whom it is always answered.
    let shared_with = application.shared_with.as_deref().unwrap_or(&[]);
    let words = |text: &str| Theirs::from(text).to_string();
    let disabled = application.disabled.is_object();
    let mut text = if disabled {
        // Never that it is served: it is not, until it is enabled.
        format!(
            "Deployed {} as Version {number}: a Platform administrator has disabled the Application, so {} answers that it is disabled until a Platform administrator enables it",
            words(&ready.name),
            words(&application.address)
        )
    } else {
        format!(
            "Deployed {}: it is live at {} as Version {number}",
            words(&ready.name),
            words(&application.address)
        )
    };
    text.push_str(if created {
        ", in an Application this deploy created.\n"
    } else {
        ".\n"
    });
    let disabled_warning = disabled.then(|| super::disabled_warning(&ready.name));
    let warnings: Vec<&Value> = ready.warnings.iter().chain(&disabled_warning).collect();
    if ready.added.is_empty() {
        text.push_str("It applied no migration.\n");
    } else {
        text.push_str(&format!(
            "It applied the migrations {}.\n",
            words(&ready.added.join(", "))
        ));
    }
    text.push_str(&visible_to_words(
        &ready.kept,
        &ready.name,
        &Theirs::from(application.address.as_str()),
        &application.contributors,
        Some(shared_with),
    ));
    let notes: Vec<&Value> = ready.notes.iter().collect();
    for (title, items) in [("Warnings", &warnings), ("Notes", &notes)] {
        if items.is_empty() {
            continue;
        }
        text.push_str(&format!("{title}:\n"));
        for item in items {
            let reason = item.get("reason").and_then(Value::as_str).unwrap_or("");
            let message = item.get("message").and_then(Value::as_str).unwrap_or("");
            text.push_str(&format!("  - {reason}: {}\n", words(message)));
        }
    }
    Done {
        json: json!({
            "application": ready.name,
            "address": application.address,
            "disabled": application.disabled,
            "version": number,
            "created": created,
            "git_commit": ready.git_commit,
            "migrations_applied": ready.added,
            "structures": ready.structures,
            "pulled": ready.pulled,
            "visible_to": {
                "contributors": application.contributors,
                "shared_with": shared_with,
            },
            "warnings": warnings,
            "notes": ready.notes,
        }),
        text,
    }
}
