//! The platform program (ADR 0015).
//!
//! `VM_DATABASE_URL=postgres://... vm serve --domain <domain> --listen
//! <ip:port> [--stand-in-sign-in [--administrators-group <Group>]]` serves
//! the Applications kept in the platform database, and the platform's API
//! (blackbox/CONTRACT.md); with the installation's key in `VM_SECRET_KEY`,
//! OIDC as the platform database's connection says (crate::oidc).
//! `VM_DATABASE_URL=... vm setup` prints the setup credential (crate::setup).
//! `vm admin connect-oidc`, `vm sign-in`, `vm sign-out`, `vm whoami`, `vm resources`, `vm
//! applications`, `vm pull`, `vm new`, `vm check`, `vm generate`, `vm
//! deploy`, `vm versions`, `vm go-back` and `vm logs` are the Builder's tool
//! (vm::tool). `vm --version` names the execution core.

use std::io::Write;
use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use serde_json::json;
use tokio::net::TcpListener;

use vm::api::{Api, Signing};
use vm::executor;
use vm::ingress::Ingress;
use vm::oidc::key::InstallationKey;
use vm::oidc::{Oidc, OidcParts, connection};
use vm::origin::{Public, Scheme};
use vm::platform::{Limits, Platform};
use vm::platform_db::{self, DatabaseAddress, PlatformDatabase, PoolLimits};
use vm::registry::is_dns_label;
use vm::runner;
use vm::sign_in::{NoSignIn, SignIn, StandInSignIn};
use vm::tool;
use vm::tool_credentials::{DEFAULT_LIFETIME, ToolCredentials, Unchecked};
use vm::{log, server, setup};

#[derive(Parser)]
#[command(
    name = "vm",
    disable_version_flag = true,
    after_help = tool::EXIT_CODES
)]
struct Cli {
    /// Print the version and the execution core, and exit.
    #[arg(short = 'V', long)]
    version: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

/// The longest start-up may spend on the platform database: connecting,
/// creating it, bringing its structure up to date, reading what serves.
const DATABASE_START_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Subcommand)]
enum Command {
    /// Serve Applications, each on <name>.<domain>, and the platform's API
    /// on api.<domain>. The platform database is named by VM_DATABASE_URL,
    /// the installation's key is VM_SECRET_KEY.
    Serve(Serve),
    /// Print the setup credential, good for 24 hours for connecting OIDC
    /// once (vm admin connect-oidc). The platform database is named by
    /// VM_DATABASE_URL.
    Setup(SetupArgs),
    /// Commands for Platform administrators: connect-oidc.
    Admin(tool::AdminArgs),
    /// Sign the tool in to an installation, and keep its credential in your
    /// own configuration folder.
    SignIn(tool::SignInArgs),
    /// End the tool's credential at the installation and remove it from
    /// this machine.
    SignOut(tool::Output),
    /// Who the tool is signed in as: the User, their Groups, the
    /// installation.
    Whoami(tool::Output),
    /// Every registered Resource: its description, your Access level, and
    /// whom to ask.
    Resources(tool::Output),
    /// The Applications you contribute to and those shared with you: name,
    /// description, address, and which is which.
    Applications(tool::Output),
    /// An Application: its address, serving Version, Contributors and whom
    /// it is shared with.
    Show(tool::ShowArgs),
    /// Share the Application: let people, Groups or everyone signed in open
    /// it (--user, --email, --group, --everyone; each as often as needed).
    Share(tool::ChangeArgs<true>),
    /// Stop sharing the Application with people, Groups or everyone.
    Unshare(tool::ChangeArgs<true>),
    /// Change who maintains the Application: vm contributors add|remove.
    Contributors(tool::ContributorsArgs),
    /// Pull a Resource's structure into the project, as a typed Drizzle
    /// definition (resources/<name>/schema.ts) and its link
    /// (resources/<name>/index.ts).
    Pull(tool::PullArgs),
    /// Make a new project from the template: an Application that answers
    /// one page, ready for npm install and vm check.
    New(tool::NewArgs),
    /// Check whether the project will deploy: its Manifest, its Resources,
    /// its migrations and its types, every problem with its fix.
    Check(tool::CheckArgs),
    /// Generate the next migration from src/db/schema.ts into migrations/,
    /// stopping before one that destroys data.
    Generate(tool::GenerateArgs),
    /// Deploy the project: build it on this machine from its last commit,
    /// check it, and send it as the Application's new Version, which goes
    /// live at its address.
    Deploy(tool::DeployArgs),
    /// The Application's Versions: each one's commit, when and by whom it
    /// was deployed, and which one serves.
    Versions(tool::VersionsArgs),
    /// Serve an earlier Version again, at once: the one before the serving
    /// Version, or --to <number>. Migrations are not undone.
    GoBack(tool::GoBackArgs),
    /// What the Application printed and the errors it did not catch,
    /// request by request; --follow prints new entries as they come.
    Logs(tool::LogsArgs),
    /// Run the project on this machine, beside a private Postgres with an
    /// empty stand-in of each pulled Resource, and send it one request as a
    /// test User of vm.dev.json (--as builder, --as outsider, ...).
    Request(tool::RequestArgs),
    /// Keep the project running on this machine and serve it to your
    /// browser at http://<name>.localhost:7480/, as the test User you choose
    /// there; rebuilt when the project changes, until interrupted.
    Dev(tool::DevArgs),
    /// The project's local runs: what they recorded (operation-log,
    /// change-history), and reset, which starts the local databases over.
    Local(tool::LocalArgs),
}

#[derive(clap::Args)]
struct Serve {
    /// The installation's domain.
    #[arg(long)]
    domain: String,
    /// The address to listen on; port 0 picks a free one.
    #[arg(long)]
    listen: SocketAddr,
    /// Development only: the caller states the User in x-vm-stand-in-* headers.
    #[arg(long)]
    stand_in_sign_in: bool,
    /// With the stand-in only: the Group whose members are Platform
    /// administrators. With OIDC the connection names it (vm admin
    /// connect-oidc).
    #[arg(long)]
    administrators_group: Option<String>,
    /// How long a tool credential lives from its sign-in, in seconds.
    #[arg(long, default_value_t = DEFAULT_LIFETIME.as_secs(),
          value_parser = clap::value_parser!(u64).range(1..=MAX_TOOL_CREDENTIAL_LIFETIME))]
    tool_credential_lifetime: u64,
    /// How a browser reaches the installation: https (the default) or http.
    #[arg(long, value_enum, default_value_t = Scheme::Https)]
    public_scheme: Scheme,
    /// The port a browser reaches the installation at: 443 with https, the
    /// listening port with http, without it.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
    public_port: Option<u16>,
    /// How long the Groups read at sign-in are used before the identity
    /// provider is asked again, in seconds.
    #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u64).range(1..=86_400))]
    session_renewal: u64,
    /// How long a session lives from its sign-in, in seconds.
    #[arg(long, default_value_t = 43_200, value_parser = clap::value_parser!(u64).range(60..=2_592_000))]
    session_lifetime: u64,
}

/// `vm setup`.
#[derive(clap::Args)]
struct SetupArgs {
    /// Print exactly one JSON object on standard output.
    #[arg(long)]
    json: bool,
}

/// The longest lifetime `--tool-credential-lifetime` takes: ten years.
const MAX_TOOL_CREDENTIAL_LIFETIME: u64 = 10 * 366 * 24 * 60 * 60;

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            // Help and usage errors alike; clap knows which stream and code.
            error.print().ok();
            return if error.use_stderr() {
                ExitCode::from(2)
            } else {
                ExitCode::SUCCESS
            };
        }
    };
    match cli.command {
        _ if cli.version => match version() {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        },
        Some(Command::Serve(serve)) => match run(&serve) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                say_on_stderr(&format!("vm serve: {message}"));
                ExitCode::FAILURE
            }
        },
        Some(Command::Setup(args)) => run_setup(&args),
        Some(Command::Admin(args)) => tool::run(tool::Command::Admin(args)),
        Some(Command::SignIn(args)) => tool::run(tool::Command::SignIn(args)),
        Some(Command::SignOut(output)) => tool::run(tool::Command::SignOut(output)),
        Some(Command::Whoami(output)) => tool::run(tool::Command::Whoami(output)),
        Some(Command::Resources(output)) => tool::run(tool::Command::Resources(output)),
        Some(Command::Applications(output)) => tool::run(tool::Command::Applications(output)),
        Some(Command::Pull(args)) => tool::run(tool::Command::Pull(args)),
        Some(Command::New(args)) => tool::run(tool::Command::New(args)),
        Some(Command::Check(args)) => tool::run(tool::Command::Check(args)),
        Some(Command::Generate(args)) => tool::run(tool::Command::Generate(args)),
        Some(Command::Deploy(args)) => tool::run(tool::Command::Deploy(args)),
        Some(Command::Versions(args)) => tool::run(tool::Command::Versions(args)),
        Some(Command::GoBack(args)) => tool::run(tool::Command::GoBack(args)),
        Some(Command::Logs(args)) => tool::run(tool::Command::Logs(args)),
        Some(Command::Request(args)) => tool::run(tool::Command::Request(args)),
        Some(Command::Dev(args)) => tool::run(tool::Command::Dev(args)),
        Some(Command::Local(args)) => tool::run(tool::Command::Local(args)),
        Some(Command::Show(args)) => tool::run(tool::Command::Show(args)),
        Some(Command::Share(args)) => tool::run(tool::Command::Share(args)),
        Some(Command::Unshare(args)) => tool::run(tool::Command::Unshare(args)),
        Some(Command::Contributors(args)) => tool::run(tool::Command::Contributors(args)),
        None => {
            say_on_stderr(
                "vm: name a command (see vm --help): vm sign-in ..., vm serve ..., or vm --version",
            );
            ExitCode::from(2)
        }
    }
}

/// A line on standard error; when standard error is gone, there is no one
/// to tell, and the program goes on to its exit code.
fn say_on_stderr(line: &str) {
    let mut err = std::io::stderr().lock();
    writeln!(err, "{line}").and_then(|()| err.flush()).ok();
}

/// Written through a handle, so that a closed standard output is an error,
/// not a panic.
fn version() -> std::io::Result<()> {
    let snapshot = match openworkers_runtime_v8::platform::get_snapshot() {
        Some(_) => "present",
        None => "none",
    };
    let mut out = std::io::stdout().lock();
    writeln!(out, "vm {}", env!("CARGO_PKG_VERSION"))?;
    writeln!(
        out,
        "execution core: openworkers-core v0.18.1, openworkers-runtime-v8 v0.18.7 (patched, vendor/VENDORED.md)"
    )?;
    writeln!(out, "V8 startup snapshot: {snapshot}")?;
    out.flush()
}

fn run(serve: &Serve) -> Result<(), String> {
    let domain = serve.domain.trim_end_matches('.').to_ascii_lowercase();
    if domain.is_empty() || !domain.split('.').all(is_dns_label) {
        return Err(format!(
            "the domain {:?} is not a domain name: use DNS labels joined by dots",
            serve.domain
        ));
    }
    if serve.administrators_group.is_some() && !serve.stand_in_sign_in {
        return Err(
            "--administrators-group belongs to the stand-in (--stand-in-sign-in): with OIDC, the Group whose members are Platform administrators is named when OIDC is connected, with `vm admin connect-oidc --administrators-group <Group>`"
                .to_owned(),
        );
    }
    if serve.stand_in_sign_in && !serve.listen.ip().is_loopback() {
        return Err(format!(
            "--stand-in-sign-in is for tests and the local run only: it needs a loopback --listen address, not {}",
            serve.listen
        ));
    }
    let key = InstallationKey::from_environment()?;
    let address = database_address()?;
    let sign_in: Box<dyn SignIn> = if serve.stand_in_sign_in {
        Box::new(StandInSignIn)
    } else {
        Box::new(NoSignIn)
    };
    let administrators: Option<Arc<str>> = serve.administrators_group.as_deref().map(Arc::from);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start the server's runtime: {e}"))?;
    runtime.block_on(async move {
        let limits = Limits {
            pool: PoolLimits::default(),
            executor: executor::Limits::default(),
            runner: runner::Limits::default(),
            start: DATABASE_START_TIMEOUT,
        };
        let Platform {
            database,
            registry,
            resources,
            logs,
            writer,
            log,
            runner,
            ..
        } = Platform::open(&address, limits).await?;
        let stored = tokio::time::timeout(DATABASE_START_TIMEOUT, connection::read(&database))
            .await
            .map_err(|_| "the platform database did not answer in time".to_owned())?
            .map_err(|e| format!("the OIDC connection cannot be read: {e}"))?;
        if serve.stand_in_sign_in && stored.is_some() {
            return Err(
                "--stand-in-sign-in cannot be switched on: this platform database holds an OIDC connection, and the stand-in cannot be on together with OIDC (it is for tests and the local run only)"
                    .to_owned(),
            );
        }
        let renewal = Duration::from_secs(serve.session_renewal);
        let credentials = Arc::new(ToolCredentials::new(
            Arc::clone(&database),
            Duration::from_secs(serve.tool_credential_lifetime),
            renewal,
            if serve.stand_in_sign_in {
                Unchecked::Taken
            } else {
                Unchecked::Refused
            },
        ));
        let domain: Arc<str> = Arc::from(domain);
        let listener = TcpListener::bind(serve.listen)
            .await
            .map_err(|e| format!("cannot listen on {}: {e}", serve.listen))?;
        let address = listener
            .local_addr()
            .map_err(|e| format!("cannot read the listening address: {e}"))?;
        let public = Public::new(
            serve.public_scheme,
            serve.public_port.unwrap_or(match serve.public_scheme {
                Scheme::Https => 443,
                Scheme::Http => address.port(),
            }),
        );
        let oidc = match key.as_ref().filter(|_| !serve.stand_in_sign_in) {
            Some(key) => Some(Arc::new(Oidc::new(
                key,
                OidcParts {
                    database: Arc::clone(&database),
                    credentials: Arc::clone(&credentials),
                    registry: Arc::clone(&registry),
                    domain: Arc::clone(&domain),
                    public,
                    renewal,
                    lifetime: Duration::from_secs(serve.session_lifetime),
                },
            )?)),
            None => None,
        };
        if let Some(stored) = &stored {
            let unusable = match &oidc {
                None => Some("no-secret-key"),
                Some(oidc) if !oidc.take_stored(stored)? => Some("another-secret-key"),
                Some(_) => None,
            };
            if let Some(reason) = unusable {
                log::record(
                    "oidc-connection-unusable",
                    json!({
                        "reason": reason,
                        "message": "the OIDC connection in the platform database cannot be used with this installation's key: connect OIDC again (vm setup, then vm admin connect-oidc)",
                    }),
                );
            }
        }
        let connected = oidc.as_ref().is_some_and(|o| o.connected().is_some());
        let sign_in_name = match (connected, sign_in.is_stand_in()) {
            (true, _) => "oidc",
            (false, true) => "stand-in",
            (false, false) => "none",
        };
        let serving = registry.serving_names();
        let serving: Vec<&str> = serving.iter().map(AsRef::as_ref).collect();
        log::record(
            "listening",
            json!({
                "address": address.to_string(),
                "domain": &*domain,
                "applications": serving,
                "stand_in_sign_in": sign_in.is_stand_in(),
                "sign_in": sign_in_name,
            }),
        );
        let ingress = Arc::new(Ingress {
            api: Api::new(
                Arc::clone(&registry),
                resources,
                log,
                Arc::clone(&logs),
                Arc::clone(&credentials),
                Arc::clone(&domain),
                Signing {
                    stand_in: serve.stand_in_sign_in,
                    administrators,
                    oidc: oidc.as_ref().map(Arc::clone),
                    database: Arc::clone(&database),
                    public,
                },
            ),
            domain,
            registry,
            sign_in,
            oidc,
            public,
            credentials,
            runner,
            logs,
            max_request_body_bytes: 1024 * 1024,
            decides_opening: true,
        });
        server::serve(listener, ingress, server::Limits::default()).await;
        drop(writer);
        Ok(())
    })
}

/// `vm setup`: the platform database brought up to date, then the setup
/// credential made and printed once. Failures are those of a start-up:
/// exit code 1, a message that never shows a password.
fn run_setup(args: &SetupArgs) -> ExitCode {
    match setup_credential() {
        Ok(made) => {
            let written = if args.json {
                let line = json!({
                    "setup_credential": made.credential.expose(),
                    "expires_at": made.expires_at,
                });
                write_out(&format!("{line}\n"))
            } else {
                say_on_stderr(&format!(
                    "The setup credential, good until {} for connecting OIDC once:\n\
                     put it in VM_SETUP_CREDENTIAL and the client secret in VM_OIDC_CLIENT_SECRET, then run\n\
                     vm admin connect-oidc --domain <domain> --connect <ip:port> --issuer <URL> --client-id <id> --administrators-group <Group>\n\
                     A second vm setup ends this credential.",
                    made.expires_at
                ));
                write_out(&format!("{}\n", made.credential.expose()))
            };
            if written.is_ok() {
                ExitCode::SUCCESS
            } else {
                say_on_stderr(
                    "vm setup: the setup credential could not be written out: run vm setup again",
                );
                ExitCode::FAILURE
            }
        }
        Err(message) => {
            say_on_stderr(&format!("vm setup: {message}"));
            ExitCode::FAILURE
        }
    }
}

fn write_out(text: &str) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    out.write_all(text.as_bytes())?;
    out.flush()
}

/// The platform database opened (and brought up to date) and the setup
/// credential made, within [`DATABASE_START_TIMEOUT`].
fn setup_credential() -> Result<setup::Made, String> {
    let address = database_address()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start the runtime: {e}"))?;
    runtime.block_on(async move {
        let made = tokio::time::timeout(DATABASE_START_TIMEOUT, async {
            let database = PlatformDatabase::open(&address, &PoolLimits::default()).await?;
            setup::make(&database)
                .await
                .map_err(|e| format!("the setup credential cannot be kept: {e}"))
        })
        .await
        .map_err(|_| {
            format!(
                "the platform database did not answer within {} seconds: check {} and that Postgres is running",
                DATABASE_START_TIMEOUT.as_secs(),
                platform_db::VARIABLE
            )
        })??;
        made.ok_or_else(|| "the operating system gave no randomness: run vm setup again".to_owned())
    })
}

/// `VM_DATABASE_URL`, read. The message never repeats its value, which
/// holds a password.
fn database_address() -> Result<DatabaseAddress, String> {
    let variable = platform_db::VARIABLE;
    let Some(value) = std::env::var_os(variable) else {
        return Err(format!(
            "{variable} is not set: set it to {}",
            platform_db::FORM
        ));
    };
    let value = value
        .into_string()
        .map_err(|_| format!("{variable} is not UTF-8: set it to {}", platform_db::FORM))?;
    DatabaseAddress::parse(&value).map_err(|problem| {
        format!(
            "{variable} cannot be read: {problem}. Set it to {}",
            platform_db::FORM
        )
    })
}
