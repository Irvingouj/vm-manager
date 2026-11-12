//! The platform program (ADR 0015).
//!
//! `vm serve --domain <domain> --listen <ip:port> --application <name>=<path>
//! [--application ...] [--stand-in-sign-in]` serves fixture Applications
//! (blackbox/CONTRACT.md). `vm --version` names the execution core.

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use serde_json::json;
use tokio::net::TcpListener;

use vm::applications::{Applications, check_name};
use vm::gateway::{Gateway, PlatformGateway};
use vm::ingress::Ingress;
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

#[derive(Subcommand)]
enum Command {
    /// Serve Applications, each on <name>.<domain>.
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
    /// A fixture Application, as <name>=<path to one JavaScript file>.
    /// At least one.
    // A value may start with a hyphen, so that a bad name is reported as a
    // bad name.
    #[arg(
        long = "application",
        value_name = "NAME=PATH",
        allow_hyphen_values = true
    )]
    applications: Vec<String>,
    /// Development only: the caller states the User in x-vm-stand-in-* headers.
    #[arg(long)]
    stand_in_sign_in: bool,
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
    if domain.is_empty() || domain.split('.').any(|label| check_name(label).is_err()) {
        return Err(format!(
            "the domain {:?} is not a domain name: use DNS labels joined by dots",
            serve.domain
        ));
    }
    let applications = Applications::from_specs(&serve.applications)?;
    let sign_in: Box<dyn SignIn> = if serve.stand_in_sign_in {
        Box::new(StandInSignIn)
    } else {
        Box::new(NoSignIn)
    };
    let gateway: Arc<dyn Gateway> = Arc::new(PlatformGateway);
    let runner = RequestRunner::start(&runner::Limits::default(), &gateway)
        .map_err(|e| format!("cannot start the engine threads: {e}"))?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start the server's runtime: {e}"))?;
    runtime.block_on(async move {
        let listener = TcpListener::bind(serve.listen)
            .await
            .map_err(|e| format!("cannot listen on {}: {e}", serve.listen))?;
        let address = listener
            .local_addr()
            .map_err(|e| format!("cannot read the listening address: {e}"))?;
        log::record(
            "listening",
            json!({
                "address": address.to_string(),
                "domain": &domain,
                "applications": applications.names(),
                "stand_in_sign_in": sign_in.is_stand_in(),
            }),
        );
        let ingress = Arc::new(Ingress {
            domain,
            applications,
            sign_in,
            runner,
            max_request_body_bytes: 1024 * 1024,
        });
        server::serve(listener, ingress, server::Limits::default()).await;
        Ok(())
    })
}
