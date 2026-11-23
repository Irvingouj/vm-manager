//! What the tests of the outbound list's surfaces share (CONTRACT.md,
//! "Outbound calls": "The list", "The tool", "The Dashboard", "The
//! instruction file"): the list's three requests as a named User, the
//! shape of what they answer, a reading of the list, the entries of the
//! program's log that a change leaves, and a tool signed in as a User.
//!
//! What a call does with the list (the decision, the wire, the Operation
//! log's entries) is the core's tests' (`outbound_*` with `src/outbound.rs`).

use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::admin::{self, administrator, logged};
use crate::api::{assert_json_content_type, assert_recent_rfc3339};
use crate::http::{Request, Response, User};
use crate::logs::encode;
use crate::tool::Machine;
use crate::vm::Vm;

/// The list.
pub const HOSTS: &str = "/v1/outbound-hosts";

/// The most entries the list holds.
pub const MOST: usize = 1000;

/// A program with the stand-in sign-in and the administrators' Group.
pub fn start() -> Vm {
    admin::start()
}

/// A signed-in User who is not a Platform administrator.
pub fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

/// A signed-in User with no Group.
pub fn bob() -> User {
    User::new("bob")
}

/// The address of one entry, the entry percent-encoded as a path segment.
pub fn path_of(entry: &str) -> String {
    format!("{HOSTS}/{}", encode(entry))
}

/// A request to `path` as this User, or as nobody.
pub fn request(vm: &Vm, user: Option<&User>, method: &str, path: &str) -> Request {
    let request = vm.api_request(path).method(method);
    match user {
        Some(user) => request.as_user(user),
        None => request,
    }
}

/// `GET /v1/outbound-hosts` as this User.
pub fn list(vm: &Vm, user: &User) -> Response {
    request(vm, Some(user), "GET", HOSTS).send()
}

/// `PUT /v1/outbound-hosts/<entry>` as this User, the entry encoded.
pub fn put(vm: &Vm, user: &User, entry: &str) -> Response {
    request(vm, Some(user), "PUT", &path_of(entry)).send()
}

/// `DELETE /v1/outbound-hosts/<entry>` as this User, the entry encoded.
pub fn delete(vm: &Vm, user: &User, entry: &str) -> Response {
    request(vm, Some(user), "DELETE", &path_of(entry)).send()
}

/// The sorted keys of a JSON object.
pub fn keys_of(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

/// Fails unless this is the answer to a `PUT` that the contract gives:
/// `200`, JSON, exactly `{ host, added_by, added_at, changed }`, `host`
/// this, `changed` this, `added_by` this, `added_at` a recent RFC 3339
/// time. Returns the body.
pub fn assert_put_answer(
    answer: &Response,
    host: &str,
    changed: bool,
    added_by: &str,
    context: &str,
) -> Value {
    assert!(
        answer.status == 200,
        "{context}: expected `200` for the PUT of {host:?}, got\n{}",
        answer.describe()
    );
    assert_json_content_type(answer);
    let body = answer.json();
    assert_eq!(
        keys_of(&body),
        ["added_at", "added_by", "changed", "host"],
        "{context}: expected exactly {{ host, added_by, added_at, changed }}, got {body}"
    );
    assert_eq!(
        body["host"].as_str(),
        Some(host),
        "{context}: expected `host` {host:?}, got {body}"
    );
    assert_eq!(
        body["changed"].as_bool(),
        Some(changed),
        "{context}: expected `changed` {changed}, got {body}"
    );
    assert_eq!(
        body["added_by"].as_str(),
        Some(added_by),
        "{context}: expected `added_by` {added_by:?}, got {body}"
    );
    assert!(
        crate::api::parse_rfc3339(body["added_at"].as_str().unwrap_or_default()).is_some(),
        "{context}: expected `added_at` an RFC 3339 time, got {body}"
    );
    body
}

/// Fails unless this is the answer to a `PUT` that added `host` just now
/// as `added_by`.
pub fn assert_added(answer: &Response, host: &str, added_by: &str, context: &str) -> Value {
    let body = assert_put_answer(answer, host, true, added_by, context);
    assert_recent_rfc3339(&body["added_at"], 300, &format!("{context}: added_at"));
    body
}

/// Fails unless this is the answer to a `DELETE` that the contract gives:
/// `200`, JSON, exactly `{ host, changed }`.
pub fn assert_deleted(answer: &Response, host: &str, changed: bool, context: &str) {
    assert!(
        answer.status == 200,
        "{context}: expected `200` for the DELETE of {host:?}, got\n{}",
        answer.describe()
    );
    assert_json_content_type(answer);
    let body = answer.json();
    assert_eq!(
        keys_of(&body),
        ["changed", "host"],
        "{context}: expected exactly {{ host, changed }}, got {body}"
    );
    assert_eq!(
        body["host"].as_str(),
        Some(host),
        "{context}: expected `host` {host:?}, got {body}"
    );
    assert_eq!(
        body["changed"].as_bool(),
        Some(changed),
        "{context}: expected `changed` {changed}, got {body}"
    );
}

/// The list as this User reads it: `200`, JSON, exactly `{ hosts }`, each
/// entry exactly `{ host, added_by, added_at }`. Returns the entries.
pub fn entries_as(vm: &Vm, user: &User) -> Vec<Value> {
    let answer = list(vm, user);
    assert!(
        answer.status == 200,
        "expected `200` reading {HOSTS} as {:?}, got\n{}",
        user.id,
        answer.describe()
    );
    entries_of(&answer)
}

/// The entries of an answer to `GET /v1/outbound-hosts`, its shape
/// checked.
pub fn entries_of(answer: &Response) -> Vec<Value> {
    assert_json_content_type(answer);
    let body = answer.json();
    assert_eq!(
        keys_of(&body),
        ["hosts"],
        "expected exactly {{ hosts }} from {HOSTS}, got {body}"
    );
    let hosts = body["hosts"]
        .as_array()
        .unwrap_or_else(|| panic!("expected `hosts` an array, got {body}"));
    for entry in hosts {
        assert_eq!(
            keys_of(entry),
            ["added_at", "added_by", "host"],
            "expected each entry to be exactly {{ host, added_by, added_at }}, got {entry}"
        );
    }
    hosts.clone()
}

/// The list as a Platform administrator reads it.
pub fn entries(vm: &Vm) -> Vec<Value> {
    entries_as(vm, &administrator())
}

/// The `host` of each entry, in the order answered.
pub fn hosts(vm: &Vm) -> Vec<String> {
    entries(vm)
        .iter()
        .map(|e| e["host"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// Adds `entry` as the administrator, which must answer `200`; returns the
/// answer's body.
pub fn add(vm: &Vm, entry: &str) -> Value {
    let answer = put(vm, &administrator(), entry);
    if answer.status != 200 {
        vm.fail(&format!(
            "expected `200` adding {entry:?} as a Platform administrator, got\n{}",
            answer.describe()
        ));
    }
    answer.json()
}

/// The records of the program's log of this event (`outbound-host-added`
/// or `outbound-host-removed`) for this host.
pub fn records_of(vm: &Vm, event: &str, host: &str) -> Vec<Value> {
    logged(vm, event)
        .into_iter()
        .filter(|r| r["host"].as_str() == Some(host))
        .collect()
}

/// Waits until the program's log has at least `count` records of this
/// event for this host, at most five seconds; fails the test otherwise.
/// Returns them.
pub fn wait_for_records(vm: &Vm, event: &str, host: &str, count: usize) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let found = records_of(vm, event, host);
        if found.len() >= count {
            return found;
        }
        if Instant::now() >= deadline {
            vm.fail(&format!(
                "expected {count} `{event}` record(s) for {host:?} in the program's log within 5 seconds, found {}",
                found.len()
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// How many records of `outbound-host-added` and `outbound-host-removed`
/// the program's log has.
pub fn change_records(vm: &Vm) -> usize {
    logged(vm, "outbound-host-added").len() + logged(vm, "outbound-host-removed").len()
}

/// A tool of a machine of its own, signed in as this User.
pub fn signed_in(vm: &Vm, user: &User) -> Machine {
    let machine = Machine::new();
    machine.sign_in(vm, user);
    machine
}

/// How many `request` records the program's log has: what tells whether a
/// command asked the installation anything.
pub fn asked(vm: &Vm) -> usize {
    admin::request_records(vm)
}

/// A TCP proxy between a program and its Postgres, which can hold the
/// bytes one connection sends towards the database (neither delivered nor
/// refused) while every other connection, and new ones, pass.
///
/// [`DbProxy::arm`] says: hold the first connection that, from now on,
/// sends a chunk holding the word `delete` (any case), from that chunk on.
/// [`DbProxy::release`] delivers what was held, in order, and then lets
/// the connection end as its client ended it (the server sees what the
/// client sent, then the end).
pub struct DbProxy {
    address: std::net::SocketAddr,
    state: std::sync::Arc<ProxyState>,
}

#[derive(Default)]
struct ProxyState {
    needle: std::sync::Mutex<Option<Vec<u8>>>,
    first_bytes: std::sync::Mutex<Vec<String>>,
    held: std::sync::atomic::AtomicUsize,
    released: std::sync::atomic::AtomicBool,
    failed: std::sync::atomic::AtomicBool,
    stopped: std::sync::atomic::AtomicBool,
}

impl DbProxy {
    /// Listens on a port of 127.0.0.1 and forwards to `target`.
    pub fn start(target: std::net::SocketAddr) -> Self {
        use std::net::TcpListener;
        use std::sync::Arc;
        let listener = TcpListener::bind("127.0.0.1:0")
            .unwrap_or_else(|e| panic!("the harness's proxy cannot listen: {e}"));
        listener
            .set_nonblocking(true)
            .unwrap_or_else(|e| panic!("the harness's proxy: {e}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|e| panic!("the harness's proxy: {e}"));
        let state = Arc::new(ProxyState::default());
        let shared = Arc::clone(&state);
        thread::spawn(move || {
            while !shared.stopped.load(std::sync::atomic::Ordering::SeqCst) {
                match listener.accept() {
                    Ok((client, _)) => {
                        let Ok(server) = std::net::TcpStream::connect(target) else {
                            continue;
                        };
                        proxy_pair(client, server, &shared);
                    }
                    Err(_) => thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        Self { address, state }
    }

    /// Where a program connects.
    pub fn address(&self) -> std::net::SocketAddr {
        self.address
    }

    /// Holds the first connection that sends a chunk holding these bytes,
    /// from that chunk on.
    pub fn arm(&self, needle: &str) {
        if let Ok(mut slot) = self.state.needle.lock() {
            *slot = Some(needle.as_bytes().to_vec());
        }
    }

    /// The first bytes (at most 120, lossy) of each chunk that was held.
    pub fn held_chunks(&self) -> Vec<String> {
        self.state
            .first_bytes
            .lock()
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// How many connections are being held.
    pub fn held(&self) -> usize {
        self.state.held.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Lets the held connection fail: what was held is never delivered, and
    /// both ends of the connection to the database are closed.
    pub fn fail(&self) {
        self.state
            .failed
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Delivers what was held.
    pub fn release(&self) {
        self.state
            .released
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Drop for DbProxy {
    fn drop(&mut self) {
        self.state
            .stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.state
            .released
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Whether `bytes` hold the armed needle; if they do the proxy is disarmed
/// and the chunk's first bytes are kept.
fn take_needle(state: &ProxyState, bytes: &[u8]) -> bool {
    let Ok(mut slot) = state.needle.lock() else {
        return false;
    };
    let found = slot
        .as_ref()
        .is_some_and(|n| !n.is_empty() && bytes.windows(n.len()).any(|w| w == n.as_slice()));
    if found {
        *slot = None;
        if let Ok(mut seen) = state.first_bytes.lock() {
            seen.push(String::from_utf8_lossy(&bytes[..bytes.len().min(120)]).into_owned());
        }
    }
    found
}

fn proxy_pair(
    client: std::net::TcpStream,
    server: std::net::TcpStream,
    state: &std::sync::Arc<ProxyState>,
) {
    use std::io::{Read, Write};
    use std::net::Shutdown;
    use std::sync::Arc;
    use std::sync::atomic::Ordering::SeqCst;
    let (Ok(mut client_in), Ok(mut server_out)) = (client.try_clone(), server.try_clone()) else {
        return;
    };
    let (mut server_in, mut client_out) = (server, client);
    // Server to client: plain.
    thread::spawn(move || {
        let mut chunk = [0_u8; 16 * 1024];
        while let Ok(n) = server_in.read(&mut chunk) {
            if n == 0 || client_out.write_all(&chunk[..n]).is_err() {
                break;
            }
        }
        drop(client_out.shutdown(Shutdown::Both));
    });
    // Client to server: held when armed and a DELETE goes by.
    let state = Arc::clone(state);
    thread::spawn(move || {
        drop(client_in.set_read_timeout(Some(Duration::from_millis(50))));
        let mut chunk = [0_u8; 16 * 1024];
        let mut held: Option<Vec<u8>> = None;
        let mut ended = false;
        loop {
            if held.is_some() && state.failed.load(SeqCst) {
                state.held.fetch_sub(1, SeqCst);
                drop(server_out.shutdown(Shutdown::Both));
                return;
            }
            if let Some(buffer) = held.as_mut() {
                if state.released.load(SeqCst) {
                    drop(server_out.write_all(buffer));
                    held = None;
                    state.held.fetch_sub(1, SeqCst);
                    if ended {
                        break;
                    }
                    continue;
                }
                if ended {
                    thread::sleep(Duration::from_millis(20));
                    continue;
                }
            }
            match client_in.read(&mut chunk) {
                Ok(0) => {
                    ended = true;
                    if held.is_none() {
                        break;
                    }
                }
                Ok(n) => {
                    let bytes = &chunk[..n];
                    if let Some(buffer) = held.as_mut() {
                        if let Ok(mut seen) = state.first_bytes.lock() {
                            seen.push(String::from_utf8_lossy(&bytes[..n.min(120)]).into_owned());
                        }
                        buffer.extend_from_slice(bytes);
                    } else if take_needle(&state, bytes) {
                        state.held.fetch_add(1, SeqCst);
                        held = Some(bytes.to_vec());
                    } else if server_out.write_all(bytes).is_err() {
                        break;
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => {
                    ended = true;
                    if held.is_none() {
                        break;
                    }
                }
            }
        }
        drop(server_out.shutdown(Shutdown::Write));
    });
}
