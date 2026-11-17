//! Connecting OIDC as an administrator would (CONTRACT.md, "The tool signs
//! in through the browser, and OIDC is connected from the tool"): the
//! installation's key (`VM_SECRET_KEY`), the setup credential `vm setup`
//! prints, and the connection made with `vm admin connect-oidc` or `PUT
//! /v1/oidc`.
//!
//! The key, the setup credential and the client secret are types that never
//! print, and [`hide`] and [`masked`] take them (and anything shaped like a
//! credential of the platform's, `vmsc_...` or `vmtc_...`) out of what a
//! failure message shows.

use std::fmt;
use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Value, json};

use crate::http::{Request, Response};
use crate::oidc::{CLIENT_ID, ClientSecret, Provider};
use crate::postgres::Platform;
use crate::program::{Finished, run_vm_to_exit};
use crate::scenario::ADMINISTRATORS;
use crate::tool::{Machine, Run};
use crate::vm::{DOMAIN, Vm};

/// The prefix of a setup credential's text.
pub const SETUP_PREFIX: &str = "vmsc_";
/// The prefix of a tool credential's text.
pub const TOOL_PREFIX: &str = "vmtc_";

/// The installation's key: 32 random bytes in base64. Never printed.
#[derive(Clone)]
pub struct SecretKey(String);

impl SecretKey {
    /// A fresh key, as `openssl rand -base64 32` makes one.
    pub fn fresh() -> Self {
        let mut bytes = [0_u8; 32];
        SystemRandom::new()
            .fill(&mut bytes)
            .unwrap_or_else(|_| panic!("the operating system gave no randomness"));
        Self(STANDARD.encode(bytes))
    }

    /// A value that is no key, for the tests of a malformed one.
    pub fn of(value: &str) -> Self {
        Self(value.to_owned())
    }

    /// The value, for `VM_SECRET_KEY` and for looking for it where it must
    /// not be. Never print it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretKey(<hidden, {} characters>)", self.0.len())
    }
}

/// A setup credential `vm setup` printed. Never printed again.
#[derive(Clone)]
pub struct SetupCredential(String);

impl SetupCredential {
    /// A credential of this text (one a test made up, or took apart).
    pub fn of(value: &str) -> Self {
        Self(value.to_owned())
    }

    /// The value, for `VM_SETUP_CREDENTIAL` and `Authorization`. Never
    /// print it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SetupCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SetupCredential(<hidden, {} characters>)", self.0.len())
    }
}

/// `text` with every run that begins with `vmsc_` or `vmtc_` replaced by
/// its prefix and its length, and each of `secrets` (when not empty)
/// replaced by `<hidden>`.
pub fn masked(text: &str, secrets: &[&str]) -> String {
    let mut out = text.to_owned();
    for secret in secrets.iter().filter(|s| !s.is_empty()) {
        out = out.replace(secret, "<hidden>");
    }
    for prefix in [SETUP_PREFIX, TOOL_PREFIX] {
        let mut result = String::new();
        let mut rest = out.as_str();
        while let Some(at) = rest.find(prefix) {
            let (before, from) = rest.split_at(at);
            result.push_str(before);
            let run = from
                .char_indices()
                .find(|(_, c)| !(c.is_ascii_alphanumeric() || "-_+/=~".contains(*c)))
                .map_or(from.len(), |(i, _)| i);
            result.push_str(&format!("{prefix}<hidden, {} characters>", run));
            rest = from.get(run..).unwrap_or_default();
        }
        result.push_str(rest);
        out = result;
    }
    out
}

/// The run with each of `secrets` and every credential-shaped run taken
/// out of what it printed, for failure messages. Look for secrets in the
/// run before hiding them.
pub fn hide(run: &Run, secrets: &[&str]) -> Run {
    let mut hidden = run.clone();
    hidden.stdout = masked(&run.stdout, secrets);
    hidden.stderr = masked(&run.stderr, secrets);
    hidden
}

/// An answer as text with every credential-shaped run and each of
/// `secrets` taken out, for failure messages.
pub fn shown(response: &Response, secrets: &[&str]) -> String {
    masked(&response.describe(), secrets)
}

/// Runs `vm setup <args>` with `VM_DATABASE_URL` naming this platform
/// database (or unset), to its exit.
pub fn run_setup(database_url: Option<&str>, args: &[&str]) -> Finished {
    let mut all = vec!["setup"];
    all.extend_from_slice(args);
    run_vm_to_exit(all, database_url)
}

/// `vm setup --json` against this platform database, which must end with
/// `0`; returns the setup credential it printed.
pub fn setup_credential(platform: &Platform) -> SetupCredential {
    let finished = run_setup(Some(platform.url()), &["--json"]);
    let shown = || {
        format!(
            "exit: {}\nstandard output:\n{}\nstandard error:\n{}",
            finished.status_text(),
            masked(&finished.stdout, &[]),
            masked(&finished.stderr, &[])
        )
    };
    assert!(
        finished.status.is_some_and(|s| s.success()),
        "expected `vm setup --json` to end with 0\n{}",
        shown()
    );
    let printed: Value = serde_json::from_str(&finished.stdout).unwrap_or_else(|e| {
        panic!(
            "expected `vm setup --json` to print one JSON object ({e})\n{}",
            shown()
        )
    });
    let credential = printed["setup_credential"].as_str().unwrap_or_else(|| {
        panic!(
            "expected `setup_credential` in what `vm setup --json` printed\n{}",
            shown()
        )
    });
    SetupCredential::of(credential)
}

/// A connection to make: what `vm admin connect-oidc` and `PUT /v1/oidc`
/// take.
#[derive(Clone, Debug)]
pub struct Connection {
    pub issuer: String,
    pub client_id: String,
    client_secret: ClientSecret,
    pub administrators_group: String,
    pub groups_claim: Option<String>,
    pub scopes: Option<String>,
}

impl Connection {
    /// The connection to this provider as the harness registered the
    /// platform there: its issuer, [`CLIENT_ID`], its client secret, and
    /// [`ADMINISTRATORS`] as the administrators' Group.
    pub fn to(provider: &Provider) -> Self {
        Self {
            issuer: provider.issuer().to_owned(),
            client_id: CLIENT_ID.to_owned(),
            client_secret: provider.client_secret(),
            administrators_group: ADMINISTRATORS.to_owned(),
            groups_claim: None,
            scopes: None,
        }
    }

    #[must_use]
    pub fn issuer(mut self, issuer: &str) -> Self {
        issuer.clone_into(&mut self.issuer);
        self
    }

    #[must_use]
    pub fn client_id(mut self, client_id: &str) -> Self {
        client_id.clone_into(&mut self.client_id);
        self
    }

    #[must_use]
    pub fn client_secret(mut self, secret: &str) -> Self {
        self.client_secret = ClientSecret::new(secret);
        self
    }

    #[must_use]
    pub fn administrators_group(mut self, group: &str) -> Self {
        group.clone_into(&mut self.administrators_group);
        self
    }

    #[must_use]
    pub fn groups_claim(mut self, claim: &str) -> Self {
        self.groups_claim = Some(claim.to_owned());
        self
    }

    #[must_use]
    pub fn scopes(mut self, scopes: &str) -> Self {
        self.scopes = Some(scopes.to_owned());
        self
    }

    /// The client secret. Never print it.
    pub fn secret(&self) -> &ClientSecret {
        &self.client_secret
    }

    /// The options of `vm admin connect-oidc` that say the connection
    /// (`--issuer`, `--client-id`, `--administrators-group`, and
    /// `--groups-claim` and `--scopes` when given).
    pub fn tool_args(&self) -> Vec<String> {
        let mut args = vec![
            "--issuer".to_owned(),
            self.issuer.clone(),
            "--client-id".to_owned(),
            self.client_id.clone(),
            "--administrators-group".to_owned(),
            self.administrators_group.clone(),
        ];
        if let Some(claim) = &self.groups_claim {
            args.push("--groups-claim".to_owned());
            args.push(claim.clone());
        }
        if let Some(scopes) = &self.scopes {
            args.push("--scopes".to_owned());
            args.push(scopes.clone());
        }
        args
    }

    /// The body of `PUT /v1/oidc`.
    pub fn body(&self) -> Value {
        let mut body = json!({
            "issuer": self.issuer,
            "client_id": self.client_id,
            "client_secret": self.client_secret.expose(),
            "administrators_group": self.administrators_group,
        });
        if let Some(claim) = &self.groups_claim {
            body["groups_claim"] = json!(claim);
        }
        if let Some(scopes) = &self.scopes {
            body["scopes"] = json!(scopes);
        }
        body
    }
}

/// The arguments of `vm admin connect-oidc` with the setup credential, to
/// the program at `address`: `--domain`, `--connect`, the connection's
/// options, and `--json` when asked.
pub fn connect_args(address: SocketAddr, connection: &Connection, json: bool) -> Vec<String> {
    let mut args = vec![
        "admin".to_owned(),
        "connect-oidc".to_owned(),
        "--domain".to_owned(),
        DOMAIN.to_owned(),
        "--connect".to_owned(),
        address.to_string(),
    ];
    args.extend(connection.tool_args());
    if json {
        args.push("--json".to_owned());
    }
    args
}

/// Runs `vm admin connect-oidc` on `machine` with the setup credential in
/// `VM_SETUP_CREDENTIAL` and the client secret in `VM_OIDC_CLIENT_SECRET`,
/// to the program `vm`; `json` adds `--json`. The run is returned as it
/// is: hide it ([`hide`]) before showing it.
pub fn connect_with_setup(
    machine: &Machine,
    vm: &Vm,
    setup: &SetupCredential,
    connection: &Connection,
    json: bool,
) -> Run {
    let args = connect_args(vm.address(), connection, json);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    machine.run_with_env(
        &refs,
        &[
            ("VM_SETUP_CREDENTIAL", setup.expose()),
            ("VM_OIDC_CLIENT_SECRET", connection.secret().expose()),
        ],
    )
}

/// How long a request that connects OIDC is waited for: the platform gives
/// the provider 10 seconds, and answers after that.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// `PUT /v1/oidc` on the program's API with this body, signed in with
/// `Authorization: Bearer <bearer>` (a setup credential or a tool
/// credential), or not signed in.
pub fn put_oidc(vm: &Vm, bearer: Option<&str>, body: &Value) -> Response {
    let request = vm
        .api_request("/v1/oidc")
        .method("PUT")
        .timeout(CONNECT_TIMEOUT)
        .json(body);
    match bearer {
        Some(credential) => request.bearer(credential),
        None => request,
    }
    .send()
}

/// `GET /v1/installation`, not signed in.
pub fn installation_of(vm: &Vm) -> Response {
    vm.api_request("/v1/installation").send()
}

/// A request to the platform's API with `Authorization: Bearer <value>`.
pub fn with_bearer(vm: &Vm, method: &str, target: &str, value: &str) -> Request {
    vm.api_request(target).method(method).bearer(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_and_secrets_are_masked() {
        let text = "a vmsc_AbC-12_x and vmtc_Q, then the-secret.";
        assert_eq!(
            masked(text, &["the-secret"]),
            "a vmsc_<hidden, 13 characters> and vmtc_<hidden, 6 characters>, then <hidden>."
        );
    }

    #[test]
    fn a_fresh_key_is_32_bytes_in_base64() {
        let key = SecretKey::fresh();
        assert_eq!(STANDARD.decode(key.expose()).unwrap().len(), 32);
        assert!(!format!("{key:?}").contains(key.expose()));
    }
}
