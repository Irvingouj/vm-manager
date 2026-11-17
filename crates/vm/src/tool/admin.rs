//! `vm admin connect-oidc` (blackbox/CONTRACT.md, "The tool signs in
//! through the browser, and OIDC is connected from the tool: Connecting
//! OIDC"): `PUT /v1/oidc` with the setup credential (`VM_SETUP_CREDENTIAL`,
//! to `--domain` at `--connect`) or with the tool's own credential, the
//! client secret from `VM_OIDC_CLIENT_SECRET`. Neither secret is printed
//! or kept: each is read from the environment, sent, and dropped.

use std::net::SocketAddr;

use bytes::Bytes;
use hyper::{Method, StatusCode};
use serde_json::{Value, json};

use super::client::{self, Installation, Sign};
use super::credential_file::{Place, Secret};
use super::theirs::Theirs;
use super::{
    Done, Ending, Output, domain, failed, loopback, not_success, read_kept, sign_in_expired,
    unexpected,
};

/// The variable that holds the client secret.
const CLIENT_SECRET_VARIABLE: &str = "VM_OIDC_CLIENT_SECRET";

/// The variable that holds the setup credential.
const SETUP_VARIABLE: &str = "VM_SETUP_CREDENTIAL";

/// `vm admin`.
#[derive(clap::Args)]
pub struct AdminArgs {
    #[command(subcommand)]
    pub command: AdminCommand,
}

/// A command for Platform administrators.
#[derive(clap::Subcommand)]
pub enum AdminCommand {
    /// Connect OIDC: with the setup credential in VM_SETUP_CREDENTIAL (and
    /// --domain and --connect), or as a Platform administrator signed in;
    /// the client secret in VM_OIDC_CLIENT_SECRET.
    ConnectOidc(ConnectOidcArgs),
}

/// `vm admin connect-oidc`.
#[derive(clap::Args)]
pub struct ConnectOidcArgs {
    /// With the setup credential: the installation's domain.
    #[arg(long, value_parser = domain)]
    pub domain: Option<String>,
    /// With the setup credential: where the installation listens (a
    /// loopback address and port).
    #[arg(long, value_parser = loopback)]
    pub connect: Option<SocketAddr>,
    /// The identity provider's issuer, https://... (http:// only on a
    /// loopback host).
    #[arg(long)]
    pub issuer: String,
    /// The platform's client id at the identity provider.
    #[arg(long)]
    pub client_id: String,
    /// The Group whose members are Platform administrators.
    #[arg(long)]
    pub administrators_group: String,
    /// The ID token's claim that holds the person's Groups (groups without
    /// it).
    #[arg(long)]
    pub groups_claim: Option<String>,
    /// The scopes asked for, separated by spaces; openid is always asked.
    #[arg(long)]
    pub scopes: Option<String>,
    #[command(flatten)]
    pub output: Output,
}

impl AdminArgs {
    pub fn json(&self) -> bool {
        match &self.command {
            AdminCommand::ConnectOidc(args) => args.output.json,
        }
    }
}

pub async fn admin(args: &AdminArgs) -> Result<Done, Ending> {
    match &args.command {
        AdminCommand::ConnectOidc(args) => connect_oidc(args).await,
    }
}

/// A variable of the environment, as UTF-8 and not empty.
fn variable(name: &str) -> Option<String> {
    std::env::var_os(name)
        .and_then(|value| value.into_string().ok())
        .filter(|value| !value.is_empty())
}

async fn connect_oidc(args: &ConnectOidcArgs) -> Result<Done, Ending> {
    let Some(client_secret) = variable(CLIENT_SECRET_VARIABLE) else {
        return Err(Ending::new(
            2,
            "usage",
            format!(
                "{CLIENT_SECRET_VARIABLE} is not set: put the client secret the identity provider gave the platform in it (never on the command line), then run the command again"
            ),
        ));
    };
    let mut body = serde_json::Map::new();
    body.insert("issuer".into(), json!(args.issuer));
    body.insert("client_id".into(), json!(args.client_id));
    body.insert("client_secret".into(), json!(client_secret));
    body.insert(
        "administrators_group".into(),
        json!(args.administrators_group),
    );
    drop(client_secret);
    if let Some(claim) = &args.groups_claim {
        body.insert("groups_claim".into(), json!(claim));
    }
    if let Some(scopes) = &args.scopes {
        body.insert("scopes".into(), json!(scopes));
    }
    let body = Bytes::from(Value::Object(body).to_string());
    let reply = match variable(SETUP_VARIABLE) {
        Some(setup) => {
            let (Some(domain), Some(connect)) = (&args.domain, args.connect) else {
                return Err(Ending::new(
                    2,
                    "usage",
                    format!(
                        "with {SETUP_VARIABLE}, --domain and --connect say where the installation is: give both, then run the command again"
                    ),
                ));
            };
            let setup = Secret::new(setup).ok_or_else(|| {
                Ending::new(
                    2,
                    "usage",
                    format!(
                        "{SETUP_VARIABLE} does not hold a setup credential (vmsc_...): put the one `vm setup` printed in it, then run the command again"
                    ),
                )
            })?;
            let at = Installation { domain, connect };
            let reply = client::send(
                &at,
                Method::PUT,
                "/v1/oidc",
                Sign::Credential(&setup),
                Some(body),
            )
            .await
            .map_err(|f| failed(connect, f))?;
            if reply.status == StatusCode::UNAUTHORIZED {
                return Err(Ending::new(
                    3,
                    "setup-credential-refused",
                    format!(
                        "the installation at {connect} ({domain}) refused the setup credential: it is used up, replaced by a later one or expired. Run `vm setup` again beside the platform database, put the new one in {SETUP_VARIABLE}, then run this command again"
                    ),
                ));
            }
            (reply, connect)
        }
        None => {
            if args.domain.is_some() || args.connect.is_some() {
                return Err(Ending::new(
                    2,
                    "usage",
                    format!(
                        "--domain and --connect go with the setup credential ({SETUP_VARIABLE}) only: signed in, the command goes to the installation the tool is signed in to. Leave them out, or set {SETUP_VARIABLE}"
                    ),
                ));
            }
            let place = Place::from_environment().map_err(Ending::credential_file)?;
            let kept = read_kept(&place)?;
            let reply = client::send(
                &super::installation(&kept),
                Method::PUT,
                "/v1/oidc",
                Sign::Credential(&kept.credential),
                Some(body),
            )
            .await
            .map_err(|f| failed(kept.connect, f))?;
            if reply.status == StatusCode::UNAUTHORIZED {
                return Err(sign_in_expired(&kept));
            }
            (reply, kept.connect)
        }
    };
    let (reply, connect) = reply;
    if reply.status != StatusCode::OK {
        return Err(not_success(connect, &reply));
    }
    let connection = reply
        .json
        .ok_or_else(|| unexpected(connect, "its answer is not JSON"))?;
    let text_of = |name: &str| {
        Theirs::from(
            connection
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )
    };
    let scopes: Vec<Theirs> = connection
        .get("scopes")
        .and_then(Value::as_array)
        .map(|scopes| {
            scopes
                .iter()
                .filter_map(Value::as_str)
                .map(Theirs::from)
                .collect()
        })
        .unwrap_or_default();
    let mut text = String::new();
    say!(
        text,
        "OIDC is connected: issuer {}, client id {}.",
        text_of("issuer"),
        text_of("client_id")
    );
    say!(
        text,
        "Groups are read from the claim {}; the scopes asked are {}.",
        text_of("groups_claim"),
        super::listed(&scopes, " ")
    );
    say!(
        text,
        "Platform administrators are the members of {}. Connected at {} by {}.",
        text_of("administrators_group"),
        text_of("connected_at"),
        text_of("connected_by")
    );
    Ok(Done {
        json: connection,
        text,
    })
}
