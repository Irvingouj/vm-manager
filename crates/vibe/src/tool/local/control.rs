//! How the tool's own local commands find a running `vibe dev` and are
//! answered by it (blackbox/CONTRACT.md, "The tool's own commands while it
//! runs").
//!
//! **The file.** `vibe dev` writes `.vibe/local/dev.json` (mode `0600`, made
//! whole under another name then renamed, so that it is never read half
//! written): its port, the Application's name, and a secret of its own run
//! ([`ControlSecret`]: 32 bytes of the system's randomness, 64 lower-case
//! hex digits). It removes the file when it ends.
//!
//! **Believed only while `vibe dev` lives.** `vibe dev` holds the project's lock
//! (`.vibe/local/lock`) as long as it runs, and the kernel lets go of it when
//! the process ends, however it ends. A command believes the file only if
//! the lock is held at that moment ([`running`]); otherwise the file is a
//! killed `vibe dev`'s, the command runs alone, and the first command that
//! takes the lock removes the file ([`remove_left`]).
//!
//! **Found while waiting.** A command that waits for the project's lock
//! reads the file at each try that finds the lock held
//! ([`found_while_waiting`]), without looking at the lock again: a `vibe dev`
//! that was starting writes it once it listens, and the command is then
//! handed to it. So the only look at the lock is the one a command makes
//! before it waits (a shared lock for a moment, never while it holds the
//! lock itself).
//!
//! **The exchange.** The command connects to `127.0.0.1:<port>` only,
//! names the Application in `Host`, and sends `POST /_vm/control/<what>`
//! with the secret in the header [`SECRET_HEADER`] (never in the address)
//! and what it asks as JSON ([`Sent`]). `vibe dev` answers `200` with `{
//! "code", "json", "text" }`: what the command prints and its exit code.
//! Any other answer is not the command's: `404` and `421` (not that
//! process's secret, or the Application renamed since the file was read)
//! leave the command to go on alone, which finds the file again while it
//! waits for the lock; the rest (`503`: stopping, or busy) end it,
//! `failed`.
//! What it sends is at most [`MAX_SENT_BYTES`], what it reads at most
//! [`MAX_ANSWER_BYTES`]; the caller bounds the time.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::header::{CONTENT_TYPE, HOST, HeaderValue};
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use rustix::fs::{FlockOperation, flock};
use serde_json::{Value, json};
use tokio::net::TcpStream;

use super::super::project_tools::Who;
use super::super::{Done, Ending};
use super::postgres::Local;
use super::{Reading, Wanted, hex};

/// The file, in `.vibe/local/`.
const FILE: &str = "dev.json";

/// The header the secret is sent in.
pub(super) const SECRET_HEADER: &str = "x-vm-control";

/// The most a command sends: everything of `vibe request` given on its
/// command line (the path, the headers, a body given with `--data`) and a
/// body read from a file, the headers' values and the body in hex (twice
/// their bytes), with the JSON around them. A command line holds at most 6
/// MiB: Linux gives a program's arguments and environment together at most
/// a quarter of the stack's limit and never more than three quarters of
/// `_STK_LIM` (8 MiB), whatever the limit (`bprm_stack_limits` in
/// `fs/exec.c`). So: 6 MiB of the command line in hex (12 MiB), a body of
/// [`super::MAX_BODY_BYTES`] in hex (2 MiB), and 1 MiB for the rest.
pub(super) const MAX_SENT_BYTES: usize = (6 * 2 + 2 + 1) * 1024 * 1024;

/// The most of an answer read: what `vibe request` prints of a response
/// (of whose body 1 MiB is printed, its headers bounded by the
/// installation's limits on a response), twice (its JSON and its words),
/// escaped as JSON strings, and the rest.
const MAX_ANSWER_BYTES: usize = 32 * 1024 * 1024;

/// The most of the file read.
const MAX_FILE_BYTES: u64 = 64 * 1024;

/// The longest a connection to `vibe dev` may take.
const CONNECT_WAIT: Duration = Duration::from_secs(5);

/// The secret of one `vibe dev`. It does not print: no `Debug`, no
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

/// A `vibe dev` found running: where it listens and its secret.
pub(super) struct Found {
    port: u16,
    application: String,
    secret: ControlSecret,
}

impl Found {
    /// Its address: `http://<application>.localhost:<port>/`.
    pub(super) fn address(&self) -> String {
        format!("http://{}.localhost:{}/", self.application, self.port)
    }

    /// Whether `other` is the same file of the same `vibe dev`: a rename
    /// writes it again with another name.
    pub(super) fn same(&self, other: &Self) -> bool {
        self.port == other.port
            && self.application == other.application
            && self.secret.matches(other.secret.0.as_bytes())
    }
}

/// Writes the file of the `vibe dev` that holds the project's lock.
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
        format!("the file that finds vibe dev cannot be written ({error})")
    })
}

/// Removes the file: `vibe dev` ends, or a command that took the project's
/// lock found one a killed `vibe dev` left.
pub(super) fn remove_left(local: &Local) {
    fs::remove_file(local.dir.join(FILE)).ok();
}

/// The `vibe dev` running in the project of `local`, if one does: its file,
/// believed only while the project's lock is held.
pub(super) fn running(local: &Local) -> Option<Found> {
    let found = read(local)?;
    let lock = fs::File::open(local.dir.join("lock")).ok()?;
    match flock(&lock, FlockOperation::NonBlockingLockShared) {
        // Nobody holds it: the file is a killed `vibe dev`'s.
        Ok(()) => None,
        Err(rustix::io::Errno::WOULDBLOCK) => Some(found),
        Err(_) => None,
    }
}

/// The address of a running `vibe dev`, as its file gives it.
pub(super) fn address_of(local: &Local) -> Option<String> {
    running(local).map(|found| found.address())
}

/// The `vibe dev` of the file, for a command that waits for the project's
/// lock and has just found it held: no look at the lock (what holds it
/// removed any file a killed `vibe dev` left when it took it, and a `vibe dev`
/// writes its own only once it listens). Not one `tried` already, which
/// did not answer.
pub(super) fn found_while_waiting(local: &Local, tried: Option<&Found>) -> Option<Found> {
    let found = read(local)?;
    match tried {
        Some(tried) if tried.same(&found) => None,
        _ => Some(found),
    }
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

/// What a command asks `vibe dev`.
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

/// What `vibe request` sends: the request, its headers' values and its body
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

/// What `vibe local operation-log` and `change-history` send.
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

/// Hands the command to the `vibe dev` `found`, and answers what it printed:
/// `None` when it did not answer it (the caller runs the command alone,
/// which hands it over again should it find a `vibe dev` while it waits for
/// the project's lock): it could not be reached; `404`, the secret of the
/// file is not that process's; `421`, the Application's name in the file
/// is no longer its own (renamed since the file was read).
pub(super) async fn forward(found: &Found, sent: &Sent, who: &Who) -> Option<Result<Done, Ending>> {
    match exchange(found, sent).await {
        Ok((StatusCode::OK, body)) => Some(answered(&body, who)),
        Ok((StatusCode::NOT_FOUND | StatusCode::MISDIRECTED_REQUEST, _)) | Err(()) => None,
        Ok((status, _)) => Some(Err(not_now(status, who))),
    }
}

/// An answer of `vibe dev` that is not the command's: it is stopping, or busy
/// for longer than the command may wait.
fn not_now(status: StatusCode, who: &Who) -> Ending {
    let command = who.command;
    Ending::new(
        1,
        "failed",
        format!(
            "vibe dev runs in this project and could not do {command} now (it answered {status}: it is stopping, or busy for longer than {command} may wait). Run {command} again"
        ),
    )
}

/// What `vibe dev` sent back, as the command's result.
fn answered(body: &[u8], who: &Who) -> Result<Done, Ending> {
    let value: Value = serde_json::from_slice(body).map_err(|_| {
        Ending::new(
            1,
            "failed",
            format!(
                "vibe dev runs in this project and answered {} with what is not the command's output. Run {} again",
                who.command, who.command
            ),
        )
    })?;
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

/// One `POST` to the `vibe dev` `found`; its status and body. `Err` when it
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
