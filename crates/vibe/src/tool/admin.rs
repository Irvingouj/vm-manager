//! `vibe admin connect-oidc` (blackbox/CONTRACT.md, "The tool signs in
//! through the browser, and OIDC is connected from the tool: Connecting
//! OIDC"): `PUT /v1/oidc` with the setup credential (`VIBE_SETUP_CREDENTIAL`,
//! to `--domain`, over HTTPS or at `--connect`) or with the tool's own credential, the
//! client secret from `VIBE_OIDC_CLIENT_SECRET`. Neither secret is printed
//! or kept: each is read from the environment, sent, and dropped.

use bytes::Bytes;
use hyper::{Method, StatusCode};
use serde_json::{Value, json};

use super::client::{self, Installation, Sign};
use super::credential_file::{Place, Secret};
use super::theirs::Theirs;
use super::{
    Done, Ending, Output, Reach, domain, failed, not_success, read_kept, sign_in_expired,
    unexpected,
};
use crate::oidc::secret::ClientSecret;

/// The variable that holds the client secret.
const CLIENT_SECRET_VARIABLE: &str = "VIBE_OIDC_CLIENT_SECRET";

/// The variable that holds the setup credential.
const SETUP_VARIABLE: &str = "VIBE_SETUP_CREDENTIAL";

/// `vibe admin`.
#[derive(clap::Args)]
pub struct AdminArgs {
    #[command(subcommand)]
    pub command: AdminCommand,
}

/// A command for Platform administrators.
#[derive(clap::Subcommand)]
pub enum AdminCommand {
    /// Connect OIDC, with the setup credential or as a Platform
    /// administrator signed in.
    ///
    /// With the setup credential in VIBE_SETUP_CREDENTIAL, --domain says
    /// where the installation is: over HTTPS, with --port and --resolve as
    /// for vibe sign-in, or over plain HTTP on this machine with --connect.
    /// Without it, the command goes to the installation the tool is signed
    /// in to. The client secret is read from VIBE_OIDC_CLIENT_SECRET.
    ConnectOidc(ConnectOidcArgs),
}

/// `vibe admin connect-oidc`.
#[derive(clap::Args)]
pub struct ConnectOidcArgs {
    /// With the setup credential: the installation's domain.
    #[arg(long, value_parser = domain)]
    pub domain: Option<String>,
    /// With the setup credential: how the installation is reached.
    #[command(flatten)]
    pub reach: Reach,
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
    let Some(client_secret) = variable(CLIENT_SECRET_VARIABLE).map(ClientSecret::new) else {
        return Err(Ending::new(
            2,
            "usage",
            format!(
                "{CLIENT_SECRET_VARIABLE} is not set: put the client secret the identity provider gave the platform in it (never on the command line), then run the command again"
            ),
        ));
    };
    // The body that carries the client secret is bytes sent once, in the
    // one request below; it is never printed or kept.
    let mut body = serde_json::Map::new();
    body.insert("issuer".into(), json!(args.issuer));
    body.insert("client_id".into(), json!(args.client_id));
    body.insert("client_secret".into(), json!(client_secret.expose()));
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
    // The setup credential is a Secret from the moment it is read; a value
    // that cannot be one is said so, never shown.
    // What is kept, when the command goes with the tool's own credential:
    // it outlives the match, which lends where it is reached.
    let kept;
    let reply = match variable(SETUP_VARIABLE).map(Secret::new) {
        Some(setup) => {
            let Some(domain) = &args.domain else {
                return Err(Ending::new(
                    2,
                    "usage",
                    format!(
                        "with {SETUP_VARIABLE}, --domain says where the installation is (with --port and --resolve, or --connect for one on this machine): give it, then run the command again"
                    ),
                ));
            };
            let setup = setup.ok_or_else(|| {
                Ending::new(
                    2,
                    "usage",
                    format!(
                        "{SETUP_VARIABLE} does not hold a setup credential (vmsc_...): put the one `vibe setup` printed in it, then run the command again"
                    ),
                )
            })?;
            let at = Installation {
                domain,
                route: args.reach.route(),
            };
            let reply = client::send(
                &at,
                Method::PUT,
                "/v1/oidc",
                Sign::Credential(&setup),
                Some(body),
            )
            .await
            .map_err(|f| failed(at, f))?;
            if reply.status == StatusCode::UNAUTHORIZED {
                return Err(Ending::new(
                    3,
                    "setup-credential-refused",
                    format!(
                        "the installation at {at} ({domain}) refused the setup credential: it is used up, replaced by a later one or expired. Run `vibe setup` again beside the platform database, put the new one in {SETUP_VARIABLE}, then run this command again"
                    ),
                ));
            }
            (reply, at)
        }
        None => {
            if args.domain.is_some() || args.reach.given() {
                return Err(Ending::new(
                    2,
                    "usage",
                    format!(
                        "--domain, --port, --resolve and --connect go with the setup credential ({SETUP_VARIABLE}) only: signed in, the command goes to the installation the tool is signed in to. Leave them out, or set {SETUP_VARIABLE}"
                    ),
                ));
            }
            let place = Place::from_environment().map_err(Ending::credential_file)?;
            kept = read_kept(&place)?;
            let reply = client::send(
                &super::installation(&kept),
                Method::PUT,
                "/v1/oidc",
                Sign::Credential(&kept.credential),
                Some(body),
            )
            .await
            .map_err(|f| failed(kept.at(), f))?;
            if reply.status == StatusCode::UNAUTHORIZED {
                return Err(sign_in_expired(&kept));
            }
            (reply, kept.at())
        }
    };
    let (reply, at) = reply;
    if reply.status != StatusCode::OK {
        return Err(not_success(at, &reply));
    }
    let connection = reply
        .json
        .ok_or_else(|| unexpected(at, "its answer is not JSON"))?;
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
