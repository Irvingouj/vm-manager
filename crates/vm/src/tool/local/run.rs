//! What every local command does, one implementation for all of them
//! (`vm request` alone or answered by `vm dev`, `vm dev`, `vm local
//! reset`): the private Postgres started ([`programs`], [`lock_project`],
//! [`start_server`]); the installation's parts on it ([`Live`]); **the
//! project brought up** ([`bring_up`]: the stand-ins, the Grants of
//! `vm.dev.json`, the build, the local Version and its migrations, the test
//! rows); and **one request served as a test User**, with what it recorded
//! collected ([`serve_and_collect`]).
//!
//! **The same code as an installation.** The request goes through the
//! installation's own [`Ingress`], signed in by [`LocalSignIn`] as the test
//! User put in its extensions; its operations are decided and run by the
//! installation's Operation gateway, recorded in the installation's
//! Operation log and Change history, in the private Postgres.
//!
//! **Collecting.** Once the response is there the request's context has
//! ended (the engine answers after it dropped it). Then, each for at most
//! [`DRAIN_WAIT`]: the operations still under way ([`PlatformGateway::drain`],
//! which takes every one under way: the caller makes sure no other
//! request's run meanwhile, `vm dev` by its lock); the log entry, written
//! by Logs' writer ([`Logs::settled`]); the changes, shipped to the Change
//! history ([`OwnDatabases::ship_and_wait`]).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::header::{HOST, HeaderName, HeaderValue};
use hyper::{Method, Request};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::super::Ending;
use super::super::check::{self, Found, Packages};
use super::super::deploy;
use super::super::project_tools::{Cx, Who, node_is_there, tool_failed};
use super::super::test_users::{self, BUILDER, TestUser, TestUsersFile};
use super::postgres::{
    self, CONNECT_WAIT, HOST as LOOPBACK, Local, PLATFORM_DATABASE, PLATFORM_ROLE, Programs, Server,
};
use super::standins::{self, Standin};
use super::{
    DOMAIN, DRAIN_WAIT, KEPT_VERSIONS, MAX_BODY_BYTES, NO_COMMIT, PLATFORM_WAIT, Progress, Stage,
    control, database_failed, local_state_failed, postgres_failed, rows,
};
use crate::api::{Api, Signing, read_version};
use crate::app_logs::{self, Query, Writer};
use crate::executor;
use crate::gateway::PlatformGateway;
use crate::ingress::{GivenToCode, Ingress, REQUEST_ID_HEADER};
use crate::oplog::OperationLog;
use crate::origin::Public;
use crate::own_db::OwnDatabases;
use crate::platform::{Limits, Platform};
use crate::platform_db::{Connection, DatabaseAddress, DbError, PlatformDatabase, PoolLimits};
use crate::registry::{NewVersion, Registry, Sent};
use crate::resources::{self, Login, NewResource, Resources};
use crate::runner;
use crate::sign_in::{ChosenTestUser, LocalSignIn};
use crate::tool_credentials::{DEFAULT_LIFETIME, ToolCredentials};
use crate::user::User;

/// Where a local command works, and how it names itself.
pub(super) struct Base<'a> {
    pub project: &'a Path,
    /// The command's own folder in `TMPDIR`.
    pub own: &'a Path,
    pub who: &'a Who,
    pub json: bool,
}

/// A line of the command's words on standard error, unless it prints JSON.
pub(super) fn tell(json: bool, line: &str) {
    if !json {
        let mut err = std::io::stderr().lock();
        writeln!(err, "{line}").and_then(|()| err.flush()).ok();
    }
}

/// What a local command holds outside the future that does its work, to let
/// go of after it however it ended: the private Postgres (stopped and
/// waited for), then the project's lock. Each lock here is held only to
/// put in or take out, never across an await.
#[derive(Default)]
pub(super) struct Held {
    server: Mutex<Option<Server>>,
    lock: Mutex<Option<postgres::Lock>>,
}

impl Held {
    fn keep_server(&self, server: Server) {
        *self.server.lock().unwrap_or_else(PoisonError::into_inner) = Some(server);
    }

    pub(super) fn keep_lock(&self, lock: postgres::Lock) {
        *self.lock.lock().unwrap_or_else(PoisonError::into_inner) = Some(lock);
    }

    /// Stops the private Postgres, if it runs, and waits for it. Every
    /// connection to it was dropped before (see the module's text of
    /// `local`).
    pub(super) async fn stop_server(&self) {
        let server = self
            .server
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(server) = server {
            server.stop().await;
        }
    }

    /// Stops the private Postgres, then lets go of the lock.
    pub(super) async fn release(&self) {
        self.stop_server().await;
        drop(
            self.lock
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
    }
}

/// The programs of the private Postgres, found or downloaded.
pub(super) async fn programs(base: &Base<'_>, progress: &Progress) -> Result<Programs, Ending> {
    progress.set(Stage::Programs);
    let json = base.json;
    postgres::programs(base.own, base.who.command, |line| tell(json, line)).await
}

/// `node` and the project's packages, as every run needs them.
pub(super) async fn tools(base: &Base<'_>, progress: &Progress) -> Result<Packages, Ending> {
    let cx = Cx::new(base.project, base.own, base.who);
    progress.set(Stage::Node);
    node_is_there(&cx, base.who).await?;
    progress.set(Stage::Packages);
    packages(base.project, base.who).await
}

/// Makes `.vm/local/`, takes the project's lock (waiting for another local
/// command of the project, as long as the caller lets it), and starts
/// `vm.log` anew. A file that finds a `vm dev` is a killed one's now, and
/// is removed. `Some` instead of the lock: while it waited, a `vm dev` of
/// the project began to listen (one other than `tried`), and the command
/// is to be handed to it.
pub(super) async fn lock_project(
    base: &Base<'_>,
    local: &Local,
    progress: &Progress,
    held: &Held,
    tried: Option<&control::Found>,
) -> Result<Option<control::Found>, Ending> {
    let name = base.who.command;
    local.make().map_err(|why| local_state_failed(&why, name))?;
    progress.set(Stage::Waiting);
    let json = base.json;
    let taken = local
        .lock(
            || {
                tell(
                    json,
                    "Another local command of this project (vm request, vm dev or vm local) is running: waiting for it to end.",
                );
            },
            || control::found_while_waiting(local, tried),
        )
        .await
        .map_err(|why| local_state_failed(&why, name))?;
    let lock = match taken {
        postgres::Taken::Lock(lock) => lock,
        postgres::Taken::Other(found) => return Ok(Some(found)),
    };
    held.keep_lock(lock);
    control::remove_left(local);
    // The parts' own records go to .vm/local/vm.log, not the terminal;
    // started anew by every run, so that it does not grow from run to run
    // (and bounded while one runs: see crate::log).
    if let Ok(file) = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(local.dir.join("vm.log"))
    {
        crate::log::to_file(file);
    }
    Ok(None)
}

/// The private Postgres, running: where it is and its passwords.
pub(super) struct Pg {
    pub local: Local,
    pub state: postgres::State,
    pub port: u16,
}

impl Pg {
    /// The platform's address in it.
    pub(super) fn platform_address(&self) -> DatabaseAddress {
        DatabaseAddress::new(
            LOOPBACK,
            self.port,
            PLATFORM_ROLE,
            self.state.platform.copy(),
            PLATFORM_DATABASE,
        )
    }

    /// The address of `database` as its superuser.
    pub(super) fn superuser(&self, database: &str) -> DatabaseAddress {
        self.local
            .superuser_address(&self.state, self.port, database)
    }
}

/// Starts the private Postgres of the project (whose lock the caller
/// holds): a server a killed run left is stopped, the data folder made
/// (emptied and made anew, with new passwords, when `fresh`: a reset), the server started
/// (into `held`), the first run's set-up done.
pub(super) async fn start_server(
    base: &Base<'_>,
    programs: &Programs,
    progress: &Progress,
    held: &Held,
    fresh: bool,
) -> Result<Pg, Ending> {
    let name = base.who.command;
    progress.set(Stage::Starting);
    let local = Local::of(base.project);
    if fresh {
        // A server a killed run left is stopped first, when the state that
        // shows it is ours can be read; then the data goes.
        if let Ok(state) = local.state() {
            local
                .stop_leftover(&state)
                .await
                .map_err(|why| local_state_failed(&why, name))?;
        }
        local
            .empty_data()
            .map_err(|why| local_state_failed(&why, name))?;
        local
            .renew_state()
            .map_err(|why| local_state_failed(&why, name))?;
    }
    let mut state = local
        .state()
        .map_err(|why| local_state_failed(&why, name))?;
    local
        .stop_leftover(&state)
        .await
        .map_err(|why| local_state_failed(&why, name))?;
    local
        .initdb(programs, &state, base.own)
        .await
        .map_err(|why| postgres_failed(&why, name))?;
    let server = Server::start(&local, programs)
        .await
        .map_err(|why| postgres_failed(&why, name))?;
    let port = server.port;
    held.keep_server(server);
    local
        .set_up(&mut state, port)
        .await
        .map_err(|why| postgres_failed(&why, name))?;
    Ok(Pg { local, state, port })
}

/// A pool of the platform database for a local run: a few connections.
fn pool_limits(size: usize) -> PoolLimits {
    PoolLimits {
        size,
        ..PoolLimits::default()
    }
}

/// The installation's parts on the private Postgres, with the Ingress the
/// requests go through.
pub(super) struct Live {
    pub database: Arc<PlatformDatabase>,
    pub own: Arc<OwnDatabases>,
    pub registry: Arc<Registry>,
    pub resources: Arc<Resources>,
    /// Owned here: dropping it ends Logs' writing task.
    writer: Writer,
    pub log: Arc<OperationLog>,
    pub gateway: Arc<PlatformGateway>,
    pub ingress: Ingress,
}

/// How many requests the parts are made for.
pub(super) struct Size {
    /// Connections of the platform database's pool.
    pub pool: usize,
    pub runner: runner::Limits,
}

impl Live {
    pub(super) async fn open(pg: &Pg, size: Size, who: &Who) -> Result<Self, Ending> {
        let Platform {
            database,
            own,
            registry,
            resources,
            logs,
            writer,
            log,
            gateway,
            runner,
        } = Platform::open(
            &pg.platform_address(),
            Limits {
                pool: pool_limits(size.pool),
                executor: executor::Limits {
                    size: 2,
                    ..executor::Limits::default()
                },
                runner: size.runner,
                start: PLATFORM_WAIT,
            },
        )
        .await
        .map_err(|why| postgres_failed(&why, who.command))?;
        // The local run has no sign-in of the tool through the browser, so
        // no credential of it is ever renewed: the renewal is its lifetime.
        let credentials = Arc::new(ToolCredentials::new(
            Arc::clone(&database),
            DEFAULT_LIFETIME,
            DEFAULT_LIFETIME,
        ));
        let ingress = Ingress {
            domain: Arc::from(DOMAIN),
            registry: Arc::clone(&registry),
            api: Api::new(
                Arc::clone(&registry),
                Arc::clone(&resources),
                Arc::clone(&log),
                Arc::clone(&logs),
                Arc::clone(&credentials),
                Arc::from(DOMAIN),
                Signing {
                    stand_in: false,
                    administrators: None,
                    oidc: None,
                    database: Arc::clone(&database),
                    public: Public::local(),
                },
            ),
            sign_in: Box::new(LocalSignIn),
            oidc: None,
            public: Public::local(),
            credentials,
            runner,
            logs,
            max_request_body_bytes: MAX_BODY_BYTES,
        };
        Ok(Self {
            database,
            own,
            registry,
            resources,
            writer,
            log,
            gateway,
            ingress,
        })
    }

    /// Ends every part: the engine threads (each once its requests ended,
    /// up to [`DRAIN_WAIT`]), the operations under way, Logs' writer once
    /// what is queued is written; then every pool is dropped (its
    /// connections' tasks ended, their sockets closed), before the caller
    /// stops Postgres.
    pub(super) async fn close(self) {
        let Self {
            database,
            own,
            registry,
            resources,
            writer,
            log,
            gateway,
            ingress,
        } = self;
        let Ingress {
            runner, logs, api, ..
        } = ingress;
        drop(api);
        runner.stop(DRAIN_WAIT).await;
        gateway.drain(DRAIN_WAIT).await;
        drop(gateway);
        if let Some(logs) = Arc::into_inner(logs) {
            writer.finish(logs, DRAIN_WAIT).await;
        }
        drop(log);
        drop(resources);
        drop(registry);
        drop(own);
        drop(database);
    }
}

/// What a run says beside its result.
#[derive(Default)]
pub(super) struct Said {
    pub warnings: Vec<Value>,
    pub notes: Vec<Value>,
}

/// What bringing the project up did.
pub(super) struct Brought {
    pub application: String,
    pub version: u64,
    /// Those this bring-up applied.
    pub migrations_applied: Vec<String>,
    /// Every pulled Resource, and whether this bring-up made its stand-in.
    pub standins: Vec<(String, bool)>,
    /// The files of test rows this bring-up loaded, in name order, and the
    /// rows each changed.
    pub test_rows: Vec<(String, u64)>,
    pub said: Said,
}

impl Brought {
    pub(super) fn standins_json(&self) -> Vec<Value> {
        self.standins
            .iter()
            .map(|(resource, made)| json!({ "resource": resource, "made": made }))
            .collect()
    }

    pub(super) fn test_rows_json(&self) -> Vec<Value> {
        self.test_rows
            .iter()
            .map(|(file, rows)| json!({ "file": file, "rows": rows }))
            .collect()
    }

    /// What a later request that caused no bring-up reports of this one:
    /// every pulled Resource, none made, nothing applied or loaded, and the
    /// warnings and notes every bring-up gives again.
    pub(super) fn standing(&self) -> Self {
        let again = |items: &[Value]| -> Vec<Value> {
            items
                .iter()
                .filter(|item| item["reason"] != "stand-in-incomplete")
                // Copied: a few small objects, once per request that
                // caused no bring-up.
                .cloned()
                .collect()
        };
        Self {
            // Copied: the names are printed by the request too.
            application: self.application.clone(),
            version: self.version,
            migrations_applied: Vec::new(),
            standins: self
                .standins
                .iter()
                .map(|(resource, _)| (resource.clone(), false))
                .collect(),
            test_rows: Vec::new(),
            said: Said {
                warnings: again(&self.said.warnings),
                notes: again(&self.said.notes),
            },
        }
    }
}

/// `test-users-file` for a project without `vm.dev.json`.
pub(super) fn no_test_users_file(command: &str) -> Ending {
    Ending::new(
        1,
        "test-users-file",
        format!(
            "The project has no {}: it holds the test Users a request is sent as. Make it as a new project has it (vm new makes one), or run vm pull <resource>, which adds your own; then run {command} again",
            test_users::FILE,
        ),
    )
}

/// `vm.dev.json` of the project, which must be there.
pub(super) fn test_users_of(project: &Path, command: &str) -> Result<TestUsersFile, Ending> {
    test_users::read(project, command)?.ok_or_else(|| no_test_users_file(command))
}

/// Runs `work`, which reads or writes the file system, on a thread of its
/// own and not on the async worker: the one way the local commands do it
/// (`vm dev` serves every connection on the tool's one thread). `None` when
/// that thread ended abnormally.
pub(super) async fn off_thread<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    tokio::task::spawn_blocking(work).await.ok()
}

/// What a local command says when a read of the project's files ended
/// abnormally.
pub(super) fn read_failed(command: &str) -> Ending {
    Ending::new(
        1,
        "failed",
        format!(
            "{command} could not read the project's files (the reading ended abnormally): run {command} again"
        ),
    )
}

/// [`test_users_of`], off the async worker.
pub(super) async fn test_users(project: &Path, who: &Who) -> Result<TestUsersFile, Ending> {
    let command = who.command;
    // Copied: the read owns its path on its own thread; once a read.
    let project = project.to_path_buf();
    off_thread(move || test_users_of(&project, command))
        .await
        .unwrap_or_else(|| Err(read_failed(command)))
}

/// The project's packages (`check::packages`), off the async worker.
pub(super) async fn packages(project: &Path, who: &Who) -> Result<Packages, Ending> {
    let who = *who;
    // Copied: the read owns its path on its own thread; once a read.
    let project = project.to_path_buf();
    off_thread(move || check::packages(&project, &who))
        .await
        .unwrap_or_else(|| Err(read_failed(who.command)))
}

/// The pulled Resources (`standins::pulled`), off the async worker.
pub(super) async fn pulled(project: &Path, who: &Who) -> Result<standins::Pulled, Ending> {
    let command = who.command;
    // Copied: the read owns its path on its own thread; once a read.
    let project = project.to_path_buf();
    off_thread(move || standins::pulled(&project, command))
        .await
        .ok_or_else(|| read_failed(command))
}

/// `no-such-test-user`, naming those there are.
pub(super) fn no_such_test_user(file: &TestUsersFile, id: &str) -> Ending {
    let there: Vec<&str> = file.users.iter().map(|u| u.id.as_str()).collect();
    Ending::theirs(
        1,
        "no-such-test-user",
        &format!(
            "{id} is not a test User of {}; those there are: {}. Send the request as one of them (--as <id>), or add the User to {}",
            test_users::FILE,
            there.join(", "),
            test_users::FILE
        ),
    )
}

/// Brings the project up on the private Postgres `pg` and its parts
/// `live`: the stand-ins of the pulled Resources (made when their
/// structure changed) with the Grants of `file`, the build, the local
/// Version (a new one when the bundle, the Declared access or the
/// migrations differ) with the migrations it adds, and the test rows that
/// changed.
pub(super) async fn bring_up(
    base: &Base<'_>,
    pg: &Pg,
    live: &Live,
    file: &TestUsersFile,
    packages: &Packages,
    progress: &Progress,
) -> Result<Brought, Ending> {
    let who = base.who;
    let cx = Cx::new(base.project, base.own, who);
    let mut said = Said::default();

    progress.set(Stage::Standins);
    let standins = standins_of(base, pg, live, file, &mut said).await?;

    progress.set(Stage::Building);
    let mut found = Found::default();
    let manifest = check::manifest(&cx, packages, &mut found, who).await?;
    let manifest = match manifest {
        Some(manifest) if found.problems.is_empty() => manifest,
        _ => {
            return Err(match check::report(found, "") {
                Err(ending) => ending,
                Ok(_) => tool_failed("the SDK's tooling (manifest)", "gave no Manifest", who),
            });
        }
    };
    said.warnings.append(&mut found.warnings);
    said.notes.append(&mut found.notes);
    let bundle = deploy::bundle(&cx, packages, who).await?;
    let migrations = {
        let who = *who;
        // Copied: the read owns its path on its own thread; once a
        // bring-up.
        let project = base.project.to_path_buf();
        off_thread(move || deploy::project_migrations(&project, &who))
            .await
            .unwrap_or_else(|| Err(read_failed(who.command)))?
    };
    let body = deploy::body(
        &bundle,
        &manifest,
        NO_COMMIT,
        &migrations,
        &BTreeMap::new(),
        who,
    )?;
    drop(bundle);
    drop(migrations);
    let version = read_version(&body).map_err(|why| {
        tool_failed(
            "the SDK's tooling",
            &format!("gave what is not a Version ({why})"),
            who,
        )
    })?;
    drop(body);
    // Copied: a remade own database (test rows) needs them again, after the
    // Version took its own; the project's migrations, once per bring-up.
    let list: Vec<crate::own_db::Migration> = version
        .migrations
        .iter()
        .map(|m| crate::own_db::Migration::new(m.name.clone(), m.sql.clone()))
        .collect();

    progress.set(Stage::Version);
    let (number, mut applied) = local_version(live, &manifest, version, who).await?;

    progress.set(Stage::Rows);
    let loaded = rows::load(
        &rows::Target {
            project: base.project,
            pg,
            live,
            application: &manifest.name,
            migrations: &list,
            who,
        },
        &standins,
        &mut said,
    )
    .await?;
    if let Some(remade) = loaded.remade {
        applied = remade;
    }
    Ok(Brought {
        application: manifest.name,
        version: number,
        migrations_applied: applied,
        standins: standins
            .into_iter()
            .map(|standin| (standin.resource, standin.made))
            .collect(),
        test_rows: loaded.files,
        said,
    })
}

/// Makes and readies the stand-ins, then makes them the Resources of the
/// run, with the Grants of `vm.dev.json`: those of the file, all of them
/// and no other.
async fn standins_of(
    base: &Base<'_>,
    pg: &Pg,
    live: &Live,
    file: &TestUsersFile,
    said: &mut Said,
) -> Result<Vec<Standin>, Ending> {
    let name = base.who.command;
    let pulled = pulled(base.project, base.who).await?;
    said.warnings.extend(pulled.warnings);
    let address = pg.superuser("postgres");
    let ready = {
        let connection = Connection::open(&address.config(CONNECT_WAIT), CONNECT_WAIT)
            .await
            .map_err(|e| database_failed(&e, name))?;
        let mut ready = Vec::with_capacity(pulled.files.len());
        for file in &pulled.files {
            ready.push(
                standins::ready(file, connection.client(), &address, &mut said.warnings)
                    .await
                    .map_err(|why| postgres_failed(&why, name))?,
            );
        }
        let kept: Vec<&str> = ready.iter().map(|s| s.database.as_str()).collect();
        standins::remove_others(connection.client(), &kept)
            .await
            .map_err(|why| postgres_failed(&why, name))?;
        ready
    };
    let resources = &live.resources;
    for resource in resources.list() {
        resources
            .unregister(&resource.name)
            .await
            .map_err(|e| database_failed(&e, name))?;
    }
    for standin in &ready {
        // Copied: the stand-in is kept (its write login loads its test
        // rows) and the Resource owns what it is registered with; once a
        // bring-up.
        let connection = resources::Connection::new(
            LOOPBACK.to_owned(),
            pg.port,
            standin.database.clone(),
            Login::new(standin.read.0.clone(), standin.read.1.copy()),
            Some(Login::new(standin.write.0.clone(), standin.write.1.copy())),
            None,
        );
        resources
            .register(
                NewResource {
                    description: format!("The local stand-in of {}", standin.resource),
                    owner: "the local run".to_owned(),
                    contact: standin.contact.clone(),
                    connection,
                    name: standin.resource.clone(),
                },
                BUILDER,
            )
            .await
            .map_err(|e| database_failed(&e, name))?;
    }
    for grant in &file.grants {
        let level = resources::level_of(&grant.access);
        match (resources.get(&grant.resource), level) {
            (Some(_), Some(level)) => {
                resources
                    .grant(&grant.resource, &grant.group, level)
                    .await
                    .map_err(|e| database_failed(&e, name))?;
            }
            _ => said.notes.push(json!({
                "reason": "grant-without-resource",
                "resource": grant.resource,
                "group": grant.group,
                "message": format!(
                    "{} grants {} on {}, which has no stand-in here (it is not pulled): the Grant does nothing in the local run. Run vm pull {} to have it",
                    test_users::FILE, grant.group, grant.resource, grant.resource
                ),
            })),
        }
    }
    Ok(ready)
}

/// The local Version: the latest one when the bundle, the Declared access
/// and the migrations are its own, else a new one sent as a deploy sends
/// it. Its number, and the migrations this sending applied.
async fn local_version(
    live: &Live,
    manifest: &check::Manifest,
    version: NewVersion,
    who: &Who,
) -> Result<(u64, Vec<String>), Ending> {
    let name = who.command;
    let failed = |e: DbError| database_failed(&e, name);
    let registry = &live.registry;
    let latest = match registry.application(&manifest.name).await.map_err(failed)? {
        None => {
            registry
                .create_application(&manifest.name, manifest.description.as_deref(), BUILDER)
                .await
                .map_err(failed)?;
            None
        }
        Some(record) => match record.latest_version {
            Some(number) => registry
                .version(&manifest.name, number)
                .await
                .map_err(failed)?,
            None => None,
        },
    };
    let ours: Vec<Value> = version
        .migrations
        .iter()
        .map(|m| json!({ "name": m.name, "sha256": m.sha256 }))
        .collect();
    let bundle_sha256 = super::hex(&Sha256::digest(version.bundle.as_bytes()));
    let declared = serde_json::to_value(&version.declared_access).ok();
    if let Some(latest) = &latest
        && latest.bundle_sha256 == bundle_sha256
        && Some(&latest.declared_access) == declared.as_ref()
        && latest.migrations.as_array() == Some(&ours)
    {
        return Ok((latest.number, Vec::new()));
    }
    let before: BTreeSet<&str> = latest
        .as_ref()
        .and_then(|l| l.migrations.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|m| m.get("name").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    let added: Vec<String> = version
        .migrations
        .iter()
        .filter(|m| !before.contains(m.name.as_str()))
        .map(|m| m.name.clone())
        .collect();
    let diverged_at = latest.as_ref().and_then(|l| {
        let theirs = l.migrations.as_array()?;
        theirs
            .iter()
            .enumerate()
            .find(|(at, recorded)| ours.get(*at) != Some(*recorded))
            .and_then(|(_, recorded)| recorded.get("name").and_then(Value::as_str))
            .map(str::to_owned)
    });
    match registry
        .send_version(&manifest.name, version, BUILDER)
        .await
        .map_err(failed)?
    {
        Sent::Sent(record) => {
            registry
                .forget_versions(&manifest.name, KEPT_VERSIONS)
                .await
                .map_err(failed)?;
            Ok((record.number, added))
        }
        Sent::Diverged => {
            let migration = diverged_at.unwrap_or_default();
            Err(Ending::theirs(
                1,
                "local-migrations-diverged",
                &format!(
                    "migrations/ no longer begins with what the local own database has: it has the migration {migration}, which migrations/ does not have as it was (a migration not yet deployed was generated again). Removing the folder .vm/local/ (or vm local reset) starts the local databases anew (their test rows are loaded again); then run {name} again"
                ),
            ))
        }
        Sent::MigrationFailed {
            migration,
            code,
            message,
            ..
        } => Err(migration_failed(&migration, &code, &message, name)),
        Sent::MigrationRefused {
            migration,
            rule,
            message,
            ..
        } => Err(migration_refused(&migration, rule, &message, name)),
        Sent::OwnDatabaseUnavailable { step, code } => {
            Err(own_database_unavailable(step, code.as_deref(), name))
        }
        Sent::Busy | Sent::NoSuchApplication | Sent::NotAContributor => {
            Err(tool_failed(name, "could not make the local Version", who))
        }
    }
}

pub(super) fn migration_failed(migration: &str, code: &str, message: &str, name: &str) -> Ending {
    let said = format!(
        "The migration {migration} failed in the local own database: {message} (SQLSTATE {code}). Nothing was changed: the local own database is as it was. Correct src/db/schema.ts or the migration while it is not deployed, then run {name} again"
    );
    deploy::detailed_refusal(
        "migration-failed",
        &said,
        &[
            ("migration", migration),
            ("code", code),
            ("database_message", message),
        ],
    )
}

pub(super) fn migration_refused(migration: &str, rule: &str, message: &str, name: &str) -> Ending {
    let said = format!(
        "The migration {migration} breaks the platform's rule {rule}: {message}. Nothing was applied, locally as at a deploy. Correct the migration while it is not deployed, then run {name} again"
    );
    deploy::detailed_refusal(
        "migration-refused",
        &said,
        &[("migration", migration), ("rule", rule)],
    )
}

pub(super) fn own_database_unavailable(step: &str, code: Option<&str>, name: &str) -> Ending {
    postgres_failed(
        &format!(
            "the local own database could not be made ready ({step}{})",
            code.map(|c| format!(", SQLSTATE {c}")).unwrap_or_default()
        ),
        name,
    )
}

/// The request a local command sends.
pub(super) struct Asked<'a> {
    /// `<path>`, with its query.
    pub path: &'a str,
    pub user: &'a TestUser,
    pub method: Method,
    pub headers: &'a [(HeaderName, HeaderValue)],
    pub body: Bytes,
}

/// What came of a request, and what it recorded.
pub(super) struct Served {
    pub parts: hyper::http::response::Parts,
    pub body: Bytes,
    pub request_id: String,
    /// Its operations' entries in the local Operation log, oldest first.
    pub operations: Vec<Value>,
    pub printed: Value,
    pub uncaught: Value,
    /// `records-incomplete`, for each part that did not come in time.
    pub warnings: Vec<Value>,
    /// Whether the Application's code was given it.
    pub given_to_code: bool,
}

/// The warning of a part of what the request recorded that did not come
/// in time.
fn incomplete(part: &str, what: &str) -> Value {
    json!({
        "reason": "records-incomplete",
        "part": part,
        "message": format!("{what}. The response above is the Application's all the same."),
    })
}

/// Hands `asked` to the Application `application` through the
/// installation's Ingress as its test User, and collects what the request
/// recorded (see the module's text). The caller makes sure no other
/// request runs meanwhile.
pub(super) async fn serve_and_collect(
    live: &Live,
    application: &str,
    asked: &Asked<'_>,
    progress: &Progress,
    who: &Who,
) -> Result<Served, Ending> {
    let name = who.command;
    progress.set(Stage::Request);
    let mut request = Request::builder()
        .method(asked.method.clone())
        .uri(asked.path)
        .header(HOST, format!("{application}.{DOMAIN}"));
    for (header, value) in asked.headers {
        request = request.header(header, value);
    }
    let mut request = request
        // A cheap copy: `Bytes` is shared.
        .body(Full::new(asked.body.clone()))
        .map_err(|e| tool_failed(name, &format!("cannot make the request ({e})"), who))?;
    request.extensions_mut().insert(ChosenTestUser(User {
        // Copied: the request owns its User; once a request.
        id: asked.user.id.clone(),
        groups: asked.user.groups.clone(),
    }));
    let response = live.ingress.handle(request).await;
    let given_to_code = response.extensions().get::<GivenToCode>().is_some();
    let (parts, body) = response.into_parts();
    let body = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(never) => match never {},
    };
    // A response to HEAD has no body, as on the wire.
    let body = if asked.method == Method::HEAD {
        Bytes::new()
    } else {
        body
    };
    let request_id = parts
        .headers
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();

    progress.set(Stage::Recording);
    let mut warnings = Vec::new();
    if !live.gateway.drain(DRAIN_WAIT).await {
        warnings.push(incomplete(
            "operations",
            &format!(
                "the request's operations did not all end within {} seconds of its response: the operations below may lack some, and their entries in the local Operation log may come later or not at all",
                DRAIN_WAIT.as_secs()
            ),
        ));
    }
    if live.own.has_own_database(application) {
        let shipped = tokio::time::timeout(DRAIN_WAIT, live.own.ship_and_wait(application)).await;
        if shipped != Ok(true) {
            warnings.push(incomplete(
                "changes",
                &format!(
                    "the changes the request made in the own database did not reach the local Change history within {} seconds: vm local change-history may lack them until a later run ships them",
                    DRAIN_WAIT.as_secs()
                ),
            ));
        }
    }
    if !live.ingress.logs.settled(DRAIN_WAIT).await {
        warnings.push(incomplete(
            "log",
            &format!(
                "the request's log entry was not written within {} seconds: printed and uncaught may lack what the code printed",
                DRAIN_WAIT.as_secs()
            ),
        ));
    }
    let (operations, entry) = if request_id.is_empty() {
        (Vec::new(), None)
    } else {
        let operations = live
            .log
            .of_request(&request_id)
            .await
            .map_err(|e| database_failed(&e, name))?;
        let entry = app_logs::read(
            &live.database,
            application,
            &Query {
                limit: 1,
                after: None,
                // Copied: the query owns its text; once a request.
                request: Some(request_id.clone()),
            },
        )
        .await
        .map_err(|e| database_failed(&e, name))?
        .entries
        .first()
        .and_then(|entry| serde_json::to_value(entry).ok());
        (operations, entry)
    };
    let part = |key: &str| {
        entry
            .as_ref()
            .and_then(|e| e.get(key))
            .cloned()
            .unwrap_or_else(|| json!([]))
    };
    Ok(Served {
        printed: part("printed"),
        uncaught: part("uncaught"),
        parts,
        body,
        request_id,
        operations,
        warnings,
        given_to_code,
    })
}
