//! `vm sign-in` through the Builder's browser (blackbox/CONTRACT.md, "The
//! tool signs in through the browser, and OIDC is connected from the tool:
//! Signing the tool in").
//!
//! - A **verifier** of 256 random bits is made and kept in memory only; its
//!   **challenge** (`BASE64URL(SHA-256(verifier))`) goes through the
//!   browser.
//! - With a browser: a listener on `127.0.0.1:<a free port>`, and the
//!   address `<sign-in address>/tool?challenge=&port=&machine=` opened with
//!   `BROWSER` (the address its last argument) or the system's own command;
//!   the command is waited for [`BROWSER_WAIT`] at most (one still running
//!   then is left to itself: it is the person's browser). Without one: the
//!   address without a port and the check code
//!   (crate::oidc::tool::check_code) on standard error.
//! - Then `POST /v1/tool-sign-ins` every [`POLL_EVERY`], at once when the
//!   browser reaches the port, each waited for [`POLL_WAIT`] at most, all
//!   within `--timeout`.
//! - The listener takes one connection at a time, reads at most
//!   [`MAX_REQUEST_BYTES`] of it within [`REQUEST_WAIT`], takes only `GET
//!   /?grant=<...>`, answers one small page and closes. It ends with the
//!   command.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use hyper::{Method, StatusCode};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;
use tokio::time::Instant;
use url::{Url, form_urlencoded};

use super::client::{self, Installation, Reply, Sign};
use super::{Ending, failed, not_success, shell_word, unexpected};
use crate::oidc::tool::{check_code, shown_code};

/// How often the installation is asked whether the sign-in was confirmed.
pub const POLL_EVERY: Duration = Duration::from_secs(2);

/// The longest one such question is waited for (and never past the
/// deadline).
pub const POLL_WAIT: Duration = Duration::from_secs(10);

/// The longest the command that opens the browser is waited for.
pub const BROWSER_WAIT: Duration = Duration::from_secs(10);

/// The most read of a request to the tool's port.
pub const MAX_REQUEST_BYTES: usize = 8 * 1024;

/// The longest a request to the tool's port may take to arrive, and its
/// answer to be written.
pub const REQUEST_WAIT: Duration = Duration::from_secs(5);

/// The longest machine name sent, in bytes.
pub const MAX_MACHINE: usize = 64;

/// The longest grant taken from the browser.
const MAX_GRANT: usize = 512;

/// A sign-in through the browser to the installation `at`, whose sign-in
/// address is `sign_in_address`; `no_browser` and `timeout` as given. The
/// answer of `POST /v1/tool-sign-ins` once confirmed (the credential, once).
pub async fn sign_in(
    at: &Installation<'_>,
    sign_in_address: &str,
    no_browser: bool,
    timeout: Duration,
) -> Result<Value, Ending> {
    let deadline = Instant::now() + timeout;
    let base = Url::parse(sign_in_address)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
        .ok_or_else(|| unexpected(at.connect, "the sign-in address it gave is not one"))?;
    let verifier = verifier().ok_or_else(|| {
        Ending::new(
            1,
            "failed",
            "the operating system gave no randomness: run the command again".into(),
        )
    })?;
    let challenge: [u8; 32] = Sha256::digest(verifier.as_bytes()).into();
    let challenge_text = URL_SAFE_NO_PAD.encode(challenge);
    let machine = machine_name();
    let opener = if no_browser { None } else { opener() };
    let listener = match opener {
        Some(_) => Some(
            TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
                .await
                .map_err(|e| {
                    Ending::new(
                        1,
                        "failed",
                        format!("the tool cannot listen on 127.0.0.1 for the browser ({e}): run the command again, or with --no-browser"),
                    )
                })?,
        ),
        None => None,
    };
    let port = match &listener {
        Some(listener) => Some(
            listener
                .local_addr()
                .map_err(|e| {
                    Ending::new(
                        1,
                        "failed",
                        format!("the tool cannot listen on 127.0.0.1 for the browser ({e}): run the command again, or with --no-browser"),
                    )
                })?
                .port(),
        ),
        None => None,
    };
    let address = tool_address(&base, &challenge_text, port, &machine);
    match (opener, port) {
        (Some(opener), Some(_)) => {
            super::say_on_stderr(&format!(
                "Opening your browser to sign the tool in. If it does not open, open this address on this computer:\n{address}\n"
            ));
            open(opener, address.as_str()).await;
        }
        _ => {
            super::say_on_stderr(&format!(
                "Open this address in a browser, on any device, and sign in:\n{address}\nWhen the page asks for it, type this check code: {}\n",
                shown_code(&check_code(&challenge))
            ));
        }
    }
    let mut grant: Option<String> = None;
    let mut next_poll = Instant::now();
    loop {
        let reached = tokio::select! {
            () = tokio::time::sleep_until(deadline) => {
                return Err(not_finished(at, &address, timeout));
            }
            () = tokio::time::sleep_until(next_poll) => None,
            accepted = accept(listener.as_ref()) => accepted,
        };
        match reached {
            None => {
                next_poll = Instant::now() + POLL_EVERY;
                if let Some(made) = poll(at, &verifier, grant.as_deref(), deadline).await? {
                    return Ok(made);
                }
                if Instant::now() >= deadline {
                    return Err(not_finished(at, &address, timeout));
                }
            }
            Some(mut stream) => {
                let Some(given) = read_grant(&mut stream).await else {
                    answer(&mut stream, "404 Not Found", NOT_HERE).await;
                    continue;
                };
                grant = Some(given);
                // Asked at once, before the browser is answered: the page
                // says what came of it.
                let made = poll(at, &verifier, grant.as_deref(), deadline).await;
                match made {
                    Ok(Some(made)) => {
                        answer(&mut stream, "200 OK", SIGNED_IN).await;
                        return Ok(made);
                    }
                    Ok(None) => {
                        answer(&mut stream, "200 OK", NOT_YET).await;
                        next_poll = Instant::now() + POLL_EVERY;
                    }
                    Err(ending) => {
                        answer(&mut stream, "200 OK", FAILED).await;
                        return Err(ending);
                    }
                }
            }
        }
    }
}

/// 256 random bits in base64url.
fn verifier() -> Option<String> {
    let mut bytes = [0_u8; 32];
    SystemRandom::new().fill(&mut bytes).ok()?;
    Some(URL_SAFE_NO_PAD.encode(bytes))
}

/// The machine's host name, at most [`MAX_MACHINE`] bytes, without control
/// characters; `machine` when the system gives none.
fn machine_name() -> String {
    let uname = rustix::system::uname();
    let name = uname.nodename().to_string_lossy();
    let mut kept = String::new();
    for c in name.chars().filter(|c| !c.is_control()) {
        if kept.len() + c.len_utf8() > MAX_MACHINE {
            break;
        }
        kept.push(c);
    }
    if kept.trim().is_empty() {
        "machine".to_owned()
    } else {
        kept
    }
}

/// The command that opens the browser, without the address: the words of
/// `BROWSER` when it is set, else the system's own when there is a display.
fn opener() -> Option<Vec<String>> {
    if let Some(browser) = std::env::var_os("BROWSER") {
        let words: Vec<String> = browser
            .to_string_lossy()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        if !words.is_empty() {
            return Some(words);
        }
    }
    if cfg!(target_os = "macos") {
        return Some(vec!["open".to_owned()]);
    }
    let display = ["DISPLAY", "WAYLAND_DISPLAY"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|v| !v.is_empty()));
    display.then(|| vec!["xdg-open".to_owned()])
}

/// Runs the opener with the address as its last argument, waiting
/// [`BROWSER_WAIT`] at most. Its failure is not the sign-in's: the address
/// is on standard error too.
async fn open(opener: Vec<String>, address: &str) {
    let mut words = opener.into_iter();
    let Some(program) = words.next() else {
        return;
    };
    let child = Command::new(program)
        .args(words)
        .arg(address)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match child {
        Ok(mut child) => {
            if tokio::time::timeout(BROWSER_WAIT, child.wait())
                .await
                .is_err()
            {
                super::say_on_stderr(
                    "The command that opens the browser has not ended; open the address above yourself if no browser opened.",
                );
            }
        }
        Err(error) => {
            super::say_on_stderr(&format!(
                "The browser could not be opened ({error}): open the address above yourself."
            ));
        }
    }
}

/// `<sign-in address>/tool?challenge=&port=&machine=`.
fn tool_address(base: &Url, challenge: &str, port: Option<u16>, machine: &str) -> Url {
    let mut address = base.clone();
    address.set_path("/tool");
    address.set_fragment(None);
    let mut query = form_urlencoded::Serializer::new(String::new());
    query.append_pair("challenge", challenge);
    if let Some(port) = port {
        query.append_pair("port", &port.to_string());
    }
    query.append_pair("machine", machine);
    address.set_query(Some(&query.finish()));
    address
}

/// The next connection to the listener; never, without one.
async fn accept(listener: Option<&TcpListener>) -> Option<TcpStream> {
    match listener {
        Some(listener) => match listener.accept().await {
            Ok((stream, _)) => Some(stream),
            // Not a browser that can be answered: wait on.
            Err(_) => std::future::pending().await,
        },
        None => std::future::pending().await,
    }
}

/// The grant of `GET /?grant=<...>`, read within [`REQUEST_WAIT`] and
/// [`MAX_REQUEST_BYTES`]; `None` for anything else.
async fn read_grant(stream: &mut TcpStream) -> Option<String> {
    let mut read = Vec::new();
    let mut chunk = [0_u8; 1024];
    let head = tokio::time::timeout(REQUEST_WAIT, async {
        loop {
            let n = stream.read(&mut chunk).await.ok()?;
            if n == 0 {
                return None;
            }
            read.extend_from_slice(chunk.get(..n)?);
            // The whole head is read before the answer, so that nothing
            // the browser sent is left unread when the socket closes (which
            // would reset the connection under the answer).
            if read.windows(4).any(|w| w == b"\r\n\r\n") {
                let end = read.windows(2).position(|w| w == b"\r\n")?;
                return read.get(..end).map(<[u8]>::to_vec);
            }
            if read.len() >= MAX_REQUEST_BYTES {
                return None;
            }
        }
    })
    .await
    .ok()??;
    let line = std::str::from_utf8(&head).ok()?;
    let mut parts = line.split(' ');
    let (Some("GET"), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    if !version.starts_with("HTTP/1.") {
        return None;
    }
    let query = target.strip_prefix("/?")?;
    let mut grant = None;
    for (name, value) in form_urlencoded::parse(query.as_bytes()) {
        if name == "grant" {
            if grant.is_some() {
                return None;
            }
            grant = Some(value.into_owned());
        }
    }
    grant.filter(|g| {
        !g.is_empty()
            && g.len() <= MAX_GRANT
            && g.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}

const SIGNED_IN: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Signed in</title></head><body><h1>The tool is signed in</h1><p>You can close this page and go back to the tool.</p></body></html>";

const NOT_YET: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Not signed in</title></head><body><h1>The tool is not signed in</h1><p>This answer is not for the sign-in the tool is waiting for. Run vm sign-in again if it has ended.</p></body></html>";

const FAILED: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Not signed in</title></head><body><h1>The tool could not finish signing in</h1><p>See what the tool says, then run vm sign-in again.</p></body></html>";

const NOT_HERE: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Not here</title></head><body><p>Nothing is here.</p></body></html>";

/// Writes one small page and closes, within [`REQUEST_WAIT`]; a browser
/// that does not take it is not waited for.
async fn answer(stream: &mut TcpStream, status: &str, page: &str) {
    let text = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{page}",
        page.len()
    );
    let written = tokio::time::timeout(REQUEST_WAIT, async {
        stream.write_all(text.as_bytes()).await?;
        stream.flush().await?;
        stream.shutdown().await
    })
    .await;
    drop(written);
}

/// `POST /v1/tool-sign-ins`: the answer on `201`, `None` while it is not
/// confirmed. Never waited for past `deadline`.
async fn poll(
    at: &Installation<'_>,
    verifier: &str,
    grant: Option<&str>,
    deadline: Instant,
) -> Result<Option<Value>, Ending> {
    let body = match grant {
        Some(grant) => json!({ "verifier": verifier, "grant": grant }),
        None => json!({ "verifier": verifier }),
    };
    let wait = deadline
        .saturating_duration_since(Instant::now())
        .min(POLL_WAIT);
    if wait.is_zero() {
        return Ok(None);
    }
    let reply = client::send_waiting(
        at,
        Method::POST,
        "/v1/tool-sign-ins",
        Sign::Anyone,
        Some(Bytes::from(body.to_string())),
        wait,
    )
    .await;
    let reply: Reply = match reply {
        Ok(reply) => reply,
        // Late: asked again at the next poll, or ended at the deadline.
        Err(client::Failure::Unreachable(_)) if Instant::now() >= deadline => return Ok(None),
        Err(failure) => return Err(failed(at.connect, failure)),
    };
    match reply.status {
        StatusCode::CREATED => reply
            .json
            .map(Some)
            .ok_or_else(|| unexpected(at.connect, "its answer is not JSON")),
        StatusCode::CONFLICT
            if reply
                .json
                .as_ref()
                .and_then(|b| b.pointer("/error/reason"))
                .and_then(Value::as_str)
                == Some("sign-in-not-confirmed") =>
        {
            Ok(None)
        }
        _ => Err(not_success(at.connect, &reply)),
    }
}

/// `sign-in-not-finished`: the address again and the command to run.
fn not_finished(at: &Installation<'_>, address: &Url, timeout: Duration) -> Ending {
    Ending::new(
        3,
        "sign-in-not-finished",
        format!(
            "the sign-in was not finished within {} seconds: nobody confirmed it at {address} (nothing was kept). When you are ready, run `vm sign-in --domain {} --connect {}` again and answer in the browser",
            timeout.as_secs(),
            shell_word(at.domain),
            at.connect
        ),
    )
}
