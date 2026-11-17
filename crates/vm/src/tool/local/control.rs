//! How the tool's own local commands find a running `vm dev` and are
//! answered by it (blackbox/CONTRACT.md, "The tool's own commands while it
//! runs").
//!
//! **The file.** `vm dev` writes `.vm/local/dev.json` (mode `0600`, made
//! whole under another name then renamed, so that it is never read half
//! written): its port, the Application's name, and a secret of its own run
//! ([`ControlSecret`]: 32 bytes of the system's randomness, 64 lower-case
//! hex digits). It removes the file when it ends.
//!
//! **Believed only while `vm dev` lives.** `vm dev` holds the project's lock
//! (`.vm/local/lock`) as long as it runs, and the kernel lets go of it when
//! the process ends, however it ends. A command believes the file only if
//! the lock is held at that moment ([`running`]); otherwise the file is a
//! killed `vm dev`'s, the command runs alone, and the first command that
//! takes the lock removes the file ([`remove_left`]).
//!
//! **The exchange.** The command connects to `127.0.0.1:<port>` only,
//! names the Application in `Host`, and sends `POST /_vm/control/<what>`
//! with the secret in the header [`SECRET_HEADER`] (never in the address)
//! and what it asks as JSON ([`Sent`]). `vm dev` answers `200` with `{
//! "code", "json", "text" }`: what the command prints and its exit code.
//! What it sends is at most [`MAX_SENT_BYTES`], what it reads at most
//! [`MAX_ANSWER_BYTES`]; the caller bounds the time.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::header::{CONTENT_TYPE, HOST, HeaderValue};
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use rustix::fs::{FlockOperation, flock};
use serde_json::{Value, json};
use tokio::net::TcpStream;

use super::super::project_tools::{Who, tool_failed};
use super::super::{Done, Ending};
use super::postgres::Local;
use super::{Reading, Wanted, hex};

/// The file, in `.vm/local/`.
const FILE: &str = "dev.json";

/// The header the secret is sent in.
pub(super) const SECRET_HEADER: &str = "x-vm-control";

/// The most a command sends: a request's body (1 MiB) and its headers, in
/// hex.
pub(super) const MAX_SENT_BYTES: usize = 4 * 1024 * 1024;

/// The most of an answer read: what `vm request` prints of a response of 1
/// MiB, twice (its JSON and its words), and the rest.
const MAX_ANSWER_BYTES: usize = 16 * 1024 * 1024;

/// The most of the file read.
const MAX_FILE_BYTES: u64 = 64 * 1024;

/// The longest a connection to `vm dev` may take.
const CONNECT_WAIT: Duration = Duration::from_secs(5);

/// The secret of one `vm dev`. It does not print: no `Debug`, no
/// `Display`; it leaves only through [`ControlSecret::matches`] and the
/// file.
pub(super) struct ControlSecret(String);

impl ControlSecret {
    /// 32 bytes of the system's randomness, as 64 lower-case hex digits.
    pub(super) fn generate() -> Result<Self, String> {
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).map_err(|e| format!("the system gave no randomness ({e})"))?;
        Ok(Self(hex(&random)))
    }

    /// Whether `given` is the secret, in a time that does not depend on
    /// where they differ.
    pub(super) fn matches(&self, given: &[u8]) -> bool {
        let ours = self.0.as_bytes();
        if given.len() != ours.len() {
            return false;
        }
        ours.iter()
            .zip(given)
            .fold(0u8, |differ, (a, b)| differ | (a ^ b))
            == 0
    }
}

/// A `vm dev` found running: where it listens and its secret.
pub(super) struct Found {
    port: u16,
    application: String,
    secret: ControlSecret,
}

/// Writes the file of the `vm dev` that holds the project's lock.
pub(super) fn write(
    local: &Local,
    port: u16,
    application: &str,
    secret: &ControlSecret,
) -> Result<(), String> {
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).map_err(|e| format!("the system gave no randomness ({e})"))?;
    let partial = local.dir.join(format!("{FILE}.{}", hex(&random)));
    let text = json!({ "port": port, "application": application, "secret": secret.0 }).to_string();
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&partial)
        .and_then(|mut file| {
            file.write_all(text.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&partial, local.dir.join(FILE)));
    written.map_err(|error| {
        fs::remove_file(&partial).ok();
        format!("the file that finds vm dev cannot be written ({error})")
    })
}

/// Removes the file: `vm dev` ends, or a command that took the project's
/// lock found one a killed `vm dev` left.
pub(super) fn remove_left(local: &Local) {
    fs::remove_file(local.dir.join(FILE)).ok();
}

/// The `vm dev` running in the project of `local`, if one does: its file,
/// believed only while the project's lock is held.
pub(super) fn running(local: &Local) -> Option<Found> {
    let found = read(local)?;
    let lock = fs::File::open(local.dir.join("lock")).ok()?;
    match flock(&lock, FlockOperation::NonBlockingLockShared) {
        // Nobody holds it: the file is a killed `vm dev`'s.
        Ok(()) => None,
        Err(rustix::io::Errno::WOULDBLOCK) => Some(found),
        Err(_) => None,
    }
}

/// The address of a running `vm dev`, as its file gives it.
pub(super) fn address_of(local: &Local) -> Option<String> {
    let found = running(local)?;
    Some(format!(
        "http://{}.localhost:{}/",
        found.application, found.port
    ))
}

fn read(local: &Local) -> Option<Found> {
    let file = match fs::File::open(local.dir.join(FILE)) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return None,
        Err(_) => return None,
    };
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES).read_to_end(&mut bytes).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let port = u16::try_from(value.get("port")?.as_u64()?).ok()?;
    let application = value.get("application")?.as_str()?.to_owned();
    let secret = value.get("secret")?.as_str()?;
    if secret.len() != 64 || !secret.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(Found {
        port,
        application,
        secret: ControlSecret(secret.to_owned()),
    })
}

/// What a command asks `vm dev`.
pub(super) enum Sent {
    Request(Value),
    Reading(Value),
    Reset,
}

impl Sent {
    fn path(&self) -> &'static str {
        match self {
            Self::Request(_) => "/_vm/control/request",
            Self::Reading(_) => "/_vm/control/read",
            Self::Reset => "/_vm/control/reset",
        }
    }

    fn body(&self) -> String {
        match self {
            Self::Request(value) | Self::Reading(value) => value.to_string(),
            Self::Reset => "{}".to_owned(),
        }
    }
}

/// What `vm request` sends: the request, its headers' values and its body
/// in hex (they are bytes).
pub(super) fn request_body(wanted: &Wanted<'_>) -> Value {
    let headers: Vec<Value> = wanted
        .headers
        .iter()
        .map(|(name, value)| json!([name.as_str(), hex(value.as_bytes())]))
        .collect();
    json!({
        "path": wanted.path,
        "as": wanted.user,
        "method": wanted.method.as_str(),
        "headers": headers,
        "body": hex(&wanted.body),
    })
}

/// What `vm local operation-log` and `change-history` send.
pub(super) fn reading_body(reading: &Reading) -> Value {
    match reading {
        Reading::OperationLog { limit } => json!({ "read": "operation-log", "limit": limit }),
        Reading::ChangeHistory { limit, table } => json!({
            "read": "change-history",
            "limit": limit,
            "table": table.as_ref().map(|(schema, name)| json!([schema, name])),
        }),
    }
}

/// Hands the command to the `vm dev` `found`, and answers what it printed:
/// `None` when it could not be reached (the caller runs the command alone).
/// When `vm dev` says the Application's name in the file is no longer its
/// own (`421`), the file is read once more.
pub(super) async fn forward(
    project: &Path,
    found: Found,
    sent: Sent,
    who: &Who,
) -> Option<Result<Done, Ending>> {
    let mut found = found;
    for _ in 0..2 {
        match exchange(&found, &sent).await {
            Ok((StatusCode::OK, body)) => return Some(answered(&body, who)),
            Ok((StatusCode::MISDIRECTED_REQUEST, _)) => {
                found = running(&Local::of(project))?;
            }
            Ok((status, _)) => {
                return Some(Err(tool_failed(
                    "vm dev",
                    &format!("answered {status} when asked to do the command"),
                    who,
                )));
            }
            Err(()) => return None,
        }
    }
    None
}

/// What `vm dev` sent back, as the command's result.
fn answered(body: &[u8], who: &Who) -> Result<Done, Ending> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| tool_failed("vm dev", "answered what is not the command's output", who))?;
    let code = value
        .get("code")
        .and_then(Value::as_u64)
        .and_then(|c| u8::try_from(c).ok())
        .unwrap_or(1);
    let done = Done {
        json: value.get("json").cloned().unwrap_or(Value::Null),
        text: value
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    };
    if code == 0 {
        Ok(done)
    } else {
        Err(Ending::from_printed(code, done))
    }
}

/// One `POST` to the `vm dev` `found`; its status and body. `Err` when it
/// could not be reached or its answer could not be read.
async fn exchange(found: &Found, sent: &Sent) -> Result<(StatusCode, Bytes), ()> {
    let stream = tokio::time::timeout(
        CONNECT_WAIT,
        TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, found.port)),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(stream))
            .await
            .map_err(|_| ())?;
    let mut secret = HeaderValue::from_str(&found.secret.0).map_err(|_| ())?;
    // Never shown, should the request ever be printed.
    secret.set_sensitive(true);
    let request = Request::builder()
        .method(Method::POST)
        .uri(sent.path())
        .header(
            HOST,
            format!("{}.localhost:{}", found.application, found.port),
        )
        .header(SECRET_HEADER, secret)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(sent.body())))
        .map_err(|_| ())?;
    let exchange = async {
        let response = sender.send_request(request).await.map_err(|_| ())?;
        let status = response.status();
        let body = Limited::new(response.into_body(), MAX_ANSWER_BYTES)
            .collect()
            .await
            .map_err(|_| ())?
            .to_bytes();
        Ok::<_, ()>((status, body))
    };
    // The connection is driven here, beside the exchange, and dropped with
    // it: no task of its own.
    tokio::pin!(connection);
    tokio::pin!(exchange);
    let mut driven = false;
    loop {
        tokio::select! {
            biased;
            done = &mut exchange => return done,
            _ = &mut connection, if !driven => driven = true,
        }
    }
}
