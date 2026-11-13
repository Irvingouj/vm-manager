//! The platform program (ADR 0015).
//!
//! `VM_DATABASE_URL=postgres://... vm serve --domain <domain> --listen
//! <ip:port> [--stand-in-sign-in] [--administrators-group <Group>]` serves the Applications kept in the
//! platform database, and the platform's API (blackbox/CONTRACT.md).
//! `vm --version` names the execution core.

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use serde_json::json;
use tokio::net::TcpListener;

use vm::api::Api;
use vm::executor;
use vm::gateway::{Gateway, PlatformGateway};
use vm::ingress::Ingress;
use vm::oplog::OperationLog;
use vm::own_db::OwnDatabases;
use vm::platform_db::{self, DatabaseAddress, PlatformDatabase, PoolLimits};
use vm::registry::{Registry, is_dns_label};
use vm::resources::Resources;
use vm::runner::{self, RequestRunner};
use vm::sign_in::{NoSignIn, SignIn, StandInSignIn};
use vm::{log, server};

#[derive(Parser)]
#[command(name = "vm", disable_version_flag = true)]
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
    /// on api.<domain>. The platform database is named by VM_DATABASE_URL.
    Serve(Serve),
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
    /// The Group whose members are Platform administrators. Without it no
    /// one is.
    #[arg(long)]
    administrators_group: Option<String>,
}

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
        _ if cli.version => {
            version();
            ExitCode::SUCCESS
        }
        Some(Command::Serve(serve)) => match run(&serve) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("vm serve: {message}");
                ExitCode::FAILURE
            }
        },
        None => {
            eprintln!("vm: name a command: vm serve ..., or vm --version");
            ExitCode::from(2)
        }
    }
}

fn version() {
    println!("vm {}", env!("CARGO_PKG_VERSION"));
    println!(
        "execution core: openworkers-core v0.18.1, openworkers-runtime-v8 v0.18.7 (patched, vendor/VENDORED.md)"
    );
    let snapshot = match openworkers_runtime_v8::platform::get_snapshot() {
        Some(_) => "present",
        None => "none",
    };
    println!("V8 startup snapshot: {snapshot}");
}

fn run(serve: &Serve) -> Result<(), String> {
    let domain = serve.domain.trim_end_matches('.').to_ascii_lowercase();
    if domain.is_empty() || !domain.split('.').all(is_dns_label) {
        return Err(format!(
            "the domain {:?} is not a domain name: use DNS labels joined by dots",
            serve.domain
        ));
    }
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
        let started = tokio::time::timeout(DATABASE_START_TIMEOUT, async {
            let database = Arc::new(PlatformDatabase::open(&address, &PoolLimits::default()).await?);
            let unreadable = |e: platform_db::DbError| {
                format!(
                    "cannot read the Applications and Resources from the platform database \"{}\": {e}",
                    address.database()
                )
            };
            let own = Arc::new(
                OwnDatabases::load(Arc::clone(&database), &address)
                    .await
                    .map_err(unreadable)?,
            );
            let registry = Registry::load(Arc::clone(&database), Arc::clone(&own))
                .await
                .map_err(unreadable)?;
            let resources = Resources::load(Arc::clone(&database), executor::Limits::default())
                .await
                .map_err(unreadable)?;
            Ok::<_, String>((database, own, registry, resources))
        })
        .await;
        let (database, own, registry, resources) = match started {
            Ok(started) => started?,
            Err(_) => {
                return Err(format!(
                    "the platform database \"{}\" on Postgres at {} was not ready within {} s",
                    address.database(),
                    address.server(),
                    DATABASE_START_TIMEOUT.as_secs()
                ));
            }
        };
        // Before serving: every own database in this program's form, its
        // migration role closed (each within its own time limits).
        own.bring_to_form().await;
        let registry = Arc::new(registry);
        let resources = Arc::new(resources);
        let log = Arc::new(OperationLog::new(database));
        let gateway: Arc<dyn Gateway> = Arc::new(PlatformGateway::new(
            Arc::clone(&resources),
            own,
            Arc::clone(&log),
            tokio::runtime::Handle::current(),
        ));
        let runner = RequestRunner::start(&runner::Limits::default(), &gateway)
            .map_err(|e| format!("cannot start the engine threads: {e}"))?;
        let domain: Arc<str> = Arc::from(domain);
        let listener = TcpListener::bind(serve.listen)
            .await
            .map_err(|e| format!("cannot listen on {}: {e}", serve.listen))?;
        let address = listener
            .local_addr()
            .map_err(|e| format!("cannot read the listening address: {e}"))?;
        let serving = registry.serving_names();
        let serving: Vec<&str> = serving.iter().map(AsRef::as_ref).collect();
        log::record(
            "listening",
            json!({
                "address": address.to_string(),
                "domain": &*domain,
                "applications": serving,
                "stand_in_sign_in": sign_in.is_stand_in(),
            }),
        );
        let ingress = Arc::new(Ingress {
            api: Api::new(
                Arc::clone(&registry),
                resources,
                log,
                Arc::clone(&domain),
                administrators,
            ),
            domain,
            registry,
            sign_in,
            runner,
            max_request_body_bytes: 1024 * 1024,
        });
        server::serve(listener, ingress, server::Limits::default()).await;
        Ok(())
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
