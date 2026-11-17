//! What `vm serve` is started with for OIDC (blackbox/CONTRACT.md, "Sign-in
//! and sessions: Starting with OIDC"), checked.

use std::net::IpAddr;
use std::time::Duration;

use url::{Host, Url};

use super::secret::ClientSecret;

/// The scopes asked for without `--oidc-scopes`.
pub const DEFAULT_SCOPES: &str = "openid profile email offline_access";

/// The claim the Groups are read from without `--oidc-groups-claim`.
pub const DEFAULT_GROUPS_CLAIM: &str = "groups";

/// The platform's registration at the identity provider, and how sessions
/// are kept.
pub struct Settings {
    /// As given: what `iss` and the configuration's `issuer` must equal.
    pub issuer: String,
    pub client_id: String,
    pub client_secret: ClientSecret,
    pub groups_claim: String,
    /// `openid` first, then the others given, each once.
    pub scopes: Vec<String>,
    /// How long the Groups read are used before the provider is asked
    /// again.
    pub renewal: Duration,
    /// How long a session lives from its sign-in.
    pub lifetime: Duration,
}

impl Settings {
    /// The scopes as the authorization request sends them.
    pub fn scope(&self) -> String {
        self.scopes.join(" ")
    }

    /// The address of the provider's configuration.
    pub fn configuration_address(&self) -> Option<Url> {
        let base = self.issuer.trim_end_matches('/');
        Url::parse(&format!("{base}/.well-known/openid-configuration")).ok()
    }

    /// Whether the issuer is a loopback `http` one, whose endpoints may be
    /// `http` too.
    pub fn plain_http_allowed(&self) -> bool {
        Url::parse(&self.issuer).is_ok_and(|u| u.scheme() == "http")
    }
}

/// The scopes asked for: `openid` always, then the given ones in order,
/// each once.
pub fn scopes(given: Option<&str>) -> Vec<String> {
    let mut scopes = vec!["openid".to_owned()];
    for scope in given.unwrap_or(DEFAULT_SCOPES).split_ascii_whitespace() {
        if !scopes.iter().any(|s| s == scope) {
            scopes.push(scope.to_owned());
        }
    }
    scopes
}

/// Whether `issuer` is an issuer the program takes: `https://<host>...`, or
/// `http://` on a loopback host (`127.0.0.1`, `[::1]`, `localhost`); no
/// user, no query, no fragment. The reason when it is not.
pub fn check_issuer(issuer: &str) -> Result<(), &'static str> {
    let url = Url::parse(issuer).map_err(|_| "it is not a URL")?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err("it holds a user or a password");
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("it has a query or a fragment");
    }
    let host = url.host().ok_or("it names no host")?;
    if matches!(&host, Host::Domain(d) if d.is_empty()) {
        return Err("it names no host");
    }
    match url.scheme() {
        "https" => Ok(()),
        "http" => {
            let loopback = match host {
                Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
                Host::Ipv4(ip) => ip == std::net::Ipv4Addr::LOCALHOST,
                Host::Ipv6(ip) => IpAddr::V6(ip) == IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
            };
            if loopback {
                Ok(())
            } else {
                Err(
                    "http:// is taken only for a loopback host (127.0.0.1, [::1], localhost); use https://",
                )
            }
        }
        _ => Err("it is neither https:// nor http:// on a loopback host"),
    }
}
