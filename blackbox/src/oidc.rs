//! The harness's own OpenID Connect identity provider (CONTRACT.md, "Sign-in
//! and sessions: The harness"): an issuer on `http://127.0.0.1:<port>`,
//! served by a thread of the test, with test people, their Groups, signing
//! keys, and switches a test turns while it runs.
//!
//! It speaks what the program needs and no more:
//!
//! - `GET /.well-known/openid-configuration`, the provider's configuration;
//! - `GET /authorize`, the authorization endpoint: an authorization-code
//!   request is answered `302` to its `redirect_uri` with a `code`, `state`
//!   and `iss`, for the person the browser is signed in as here. The
//!   browser says who that is with the cookie [`PERSON_COOKIE`] on the
//!   provider's host (`Browser::sign_in_at_provider`); without it the
//!   answer is a `200` page asking who you are, which no test fills in;
//! - `POST /token`: the `authorization_code` grant, PKCE `S256` checked
//!   when the request had a `code_challenge`, and the `refresh_token`
//!   grant; the client authenticated with `client_secret_basic` or
//!   `client_secret_post`, whichever the configuration names;
//! - `GET /jwks`, its public keys.
//!
//! ID tokens are signed with RS256 by the RSA key in
//! `fixtures/oidc/rsa-main.pem`, made for this provider with `openssl
//! genpkey` (ring signs RSA but cannot make RSA keys), or, when a test asks,
//! with ES256 by a P-256 key made when the provider starts, or with keys or
//! in ways that must not be taken ([`TokenFault`], [`Key::Other`]).
//!
//! Every request it answers is recorded ([`Recorded`]), so that a test can
//! say which endpoints were visited, with which parameters, and how the
//! client authenticated. Every code and token it issues is kept too, so a
//! test can check that none of them is shown where it must not be.
//!
//! Bounded: at most [`MAX_CONNECTIONS`] connections at once (more are
//! closed at once), [`MAX_RECORDED`] requests recorded and [`MAX_ISSUED`]
//! codes and tokens kept (later ones are counted, not kept), requests read
//! up to [`MAX_REQUEST_BYTES`]. Every read and write has a timeout, and a
//! connection held unanswered ([`Outage::Hang`]) is let go after at most
//! [`HANG_AT_MOST`]. Dropping the [`Provider`] stops its thread and joins
//! every connection's.

use std::collections::HashMap;
use std::fmt;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{self, EcdsaKeyPair, KeyPair, RsaKeyPair};
use serde_json::{Map, Value, json};
use url::Url;

/// The platform's registration at the provider: its client id.
pub const CLIENT_ID: &str = "vm-harness-client";

/// The cookie on the provider's host that says whom the browser is signed in
/// as there: its value is the person's `sub`.
pub const PERSON_COOKIE: &str = "harness-person";

/// The `kid` of the RSA key the provider publishes and signs with.
pub const KID_MAIN: &str = "harness-rsa-main";
/// The `kid` of the second RSA key, published only when a test says so.
pub const KID_OTHER: &str = "harness-rsa-other";
/// The `kid` of the P-256 key, always published.
pub const KID_EC: &str = "harness-ec-p256";

/// How long an ID token lives, from its issue.
pub const ID_TOKEN_SECONDS: i64 = 300;

/// The most connections served at once; one more is closed unanswered.
pub const MAX_CONNECTIONS: usize = 64;
/// The most connections kept in [`Outage::Park`]; one more is closed.
pub const MAX_PARKED: usize = 4096;
/// The most requests recorded; later ones are counted.
pub const MAX_RECORDED: usize = 10_000;
/// The most codes and tokens kept; later ones are counted.
pub const MAX_ISSUED: usize = 10_000;
/// The largest request read, head and body.
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
/// The longest one read or write on a connection may take.
const IO_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest a connection is held unanswered in [`Outage::Hang`].
pub const HANG_AT_MOST: Duration = Duration::from_secs(60);
/// How often the threads look whether they should stop.
const POLL: Duration = Duration::from_millis(10);
/// How much padding [`Provider::oversize_jwks`] adds: more than the 1 MiB
/// the program reads of an answer.
const OVERSIZE_BYTES: usize = 2 * 1024 * 1024;

/// A test person: who the provider says they are.
#[derive(Clone, Debug)]
pub struct Person {
    pub sub: String,
    pub name: Option<String>,
    pub email: Option<String>,
    /// The value of the Groups claim exactly as written into the ID token;
    /// `None` leaves the claim out.
    pub groups: Option<Value>,
    /// Refused at the token endpoint (`invalid_grant`) from now on.
    pub disabled: bool,
}

impl Person {
    /// A person with this `sub`, no name, no email, no Groups (an empty
    /// array).
    pub fn new(sub: &str) -> Self {
        Self {
            sub: sub.to_owned(),
            name: None,
            email: None,
            groups: Some(json!([])),
            disabled: false,
        }
    }

    #[must_use]
    pub fn named(mut self, name: &str) -> Self {
        self.name = Some(name.to_owned());
        self
    }

    #[must_use]
    pub fn email(mut self, email: &str) -> Self {
        self.email = Some(email.to_owned());
        self
    }

    /// The Groups claim as an array of these strings.
    #[must_use]
    pub fn in_groups(mut self, groups: &[&str]) -> Self {
        self.groups = Some(json!(groups));
        self
    }

    /// The Groups claim as this value, whatever its shape; `None` leaves it
    /// out.
    #[must_use]
    pub fn groups_claim(mut self, value: Option<Value>) -> Self {
        self.groups = value;
        self
    }
}

/// Which key an ID token is signed with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// The published RSA key ([`KID_MAIN`]), RS256: the default.
    Main,
    /// The second RSA key ([`KID_OTHER`]), RS256: not in the JWKS unless
    /// [`Provider::publish_other_key`] put it there.
    Other,
    /// The published P-256 key ([`KID_EC`]), ES256.
    Ec,
}

/// What is wrong with every ID token issued while it is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenFault {
    /// Signed with [`Key::Other`] but naming [`KID_MAIN`]: a signature
    /// that does not verify with the key it names.
    BadSignature,
    /// A `nonce` other than the one of the authorization request.
    WrongNonce,
    /// An `aud` (and `azp`) other than [`CLIENT_ID`].
    WrongAudience,
    /// An `iss` other than the issuer.
    WrongIssuer,
    /// `exp` this many seconds from now: negative is in the past.
    ExpiresIn(i64),
    /// `alg: none` and no signature.
    AlgNone,
    /// `alg: HS256`, an HMAC with the client secret as its key.
    Hs256WithSecret,
    /// A `sub` other than the person's (`<sub>-someone-else`).
    OtherSubject,
    /// `iat` and `exp` given with a fraction of a second (`...5`): a token
    /// that holds.
    FractionalTimes,
}

/// What the token endpoint's successful answer looks like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenAnswer {
    /// JSON with an `id_token`: the default.
    Normal,
    /// JSON without an `id_token`.
    WithoutIdToken,
    /// `200` with a body that is not JSON.
    NotJson,
    /// JSON of more than 1 MiB.
    Oversize,
}

/// An answer of the token endpoint other than its own: this status, with
/// `{ "error", "error_description" }`, to every token request while set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenStatus {
    pub status: u16,
    pub error: String,
    pub description: String,
}

/// What is wrong with the configuration while it is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigFault {
    /// An `issuer` other than the one the program was given.
    AnotherIssuer,
    /// No `token_endpoint`.
    LacksTokenEndpoint,
    /// More than 1 MiB.
    Oversize,
    /// Answered with this status (and a JSON error) instead of `200`.
    Status(u16),
}

/// Whether the provider answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outage {
    /// It answers.
    None,
    /// Connections are accepted and never answered (let go after
    /// [`HANG_AT_MOST`], or when the outage ends, without an answer).
    Hang,
    /// Connections are accepted and closed at once, unanswered.
    Close,
    /// Connections are accepted and kept without being read or answered,
    /// at no thread each (at most [`MAX_PARKED`]; more are closed), until
    /// the outage ends: then they are closed unanswered. For holding many
    /// calls at once.
    Park,
}

/// One request the provider answered.
#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    /// Names in lower case, in the order received.
    pub headers: Vec<(String, String)>,
    /// The body's `application/x-www-form-urlencoded` fields, if any.
    pub form: Vec<(String, String)>,
    /// At the token endpoint: `client_secret_basic`, `client_secret_post`,
    /// `both` or `none`, as the client presented itself (right or wrong).
    pub client_auth: Option<String>,
    /// At the token endpoint for a code: whether the `code_verifier` matched
    /// the authorization request's `code_challenge` (`None` when there was
    /// no challenge to check, or no such code).
    pub pkce_ok: Option<bool>,
    /// When it was received, for ordering.
    pub at: Instant,
}

impl Recorded {
    pub fn query_param(&self, name: &str) -> Option<&str> {
        find(&self.query, name)
    }

    pub fn form_param(&self, name: &str) -> Option<&str> {
        find(&self.form, name)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        find(&self.headers, &name.to_ascii_lowercase())
    }

    /// The token endpoint's `grant_type`, or `None` for another endpoint.
    pub fn grant_type(&self) -> Option<&str> {
        if self.path == "/token" {
            self.form_param("grant_type")
        } else {
            None
        }
    }
}

fn find<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

/// What an authorization code or a refresh token stands for.
#[derive(Clone, Debug)]
struct Grant {
    sub: String,
    nonce: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    redirect_uri: String,
    scope: String,
    used: bool,
}

struct State {
    people: HashMap<String, Person>,
    codes: HashMap<String, Grant>,
    refresh_tokens: HashMap<String, Grant>,
    issued: Vec<String>,
    issued_dropped: usize,
    recorded: Vec<Recorded>,
    recorded_dropped: usize,
    outage: Outage,
    fault: Option<TokenFault>,
    signing: Key,
    publish_other: bool,
    issue_refresh_tokens: bool,
    groups_claim: String,
    auth_methods: Vec<String>,
    deny: bool,
    token_delay: Duration,
    oversize_jwks: bool,
    token_answer: TokenAnswer,
    token_status: Option<TokenStatus>,
    config_fault: Option<ConfigFault>,
    config_delay: Duration,
    keys_status: Option<u16>,
    unreadable_jwks: bool,
    other_secrets: Vec<String>,
    parked: usize,
}

struct Keys {
    main: RsaKeyPair,
    other: RsaKeyPair,
    ec: EcdsaKeyPair,
}

struct Shared {
    state: Mutex<State>,
    keys: Keys,
    rng: SystemRandom,
    issuer: String,
    secret: String,
    stop: AtomicBool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic while holding the lock can only come from this module's
        // own bookkeeping; what it recorded is still worth showing.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn random(&self) -> String {
        let mut bytes = [0_u8; 32];
        self.rng
            .fill(&mut bytes)
            .unwrap_or_else(|_| panic!("the operating system gave no randomness"));
        URL_SAFE_NO_PAD.encode(bytes)
    }
}

/// The client secret, which never prints.
#[derive(Clone)]
pub struct ClientSecret(String);

impl ClientSecret {
    /// The value, for `VM_OIDC_CLIENT_SECRET` and for looking for it where
    /// it must not be. Never print it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ClientSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ClientSecret(<hidden>)")
    }
}

/// A running identity provider. Dropping it stops it.
pub struct Provider {
    shared: Arc<Shared>,
    address: SocketAddr,
    accepting: Option<JoinHandle<()>>,
}

impl Provider {
    /// Starts a provider on a free loopback port with these people.
    pub fn start(people: &[Person]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .unwrap_or_else(|e| panic!("the identity provider cannot listen: {e}"));
        listener
            .set_nonblocking(true)
            .unwrap_or_else(|e| panic!("the identity provider cannot listen: {e}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|e| panic!("the identity provider has no address: {e}"));
        let rng = SystemRandom::new();
        let keys = Keys {
            main: rsa_key("rsa-main.pem"),
            other: rsa_key("rsa-other.pem"),
            ec: ec_key(&rng),
        };
        let mut secret_bytes = [0_u8; 24];
        rng.fill(&mut secret_bytes)
            .unwrap_or_else(|_| panic!("the operating system gave no randomness"));
        let secret = format!("harness-secret-{}", hex(&secret_bytes));
        let state = State {
            people: people.iter().map(|p| (p.sub.clone(), p.clone())).collect(),
            codes: HashMap::new(),
            refresh_tokens: HashMap::new(),
            issued: Vec::new(),
            issued_dropped: 0,
            recorded: Vec::new(),
            recorded_dropped: 0,
            outage: Outage::None,
            fault: None,
            signing: Key::Main,
            publish_other: false,
            issue_refresh_tokens: true,
            groups_claim: "groups".to_owned(),
            auth_methods: vec![
                "client_secret_basic".to_owned(),
                "client_secret_post".to_owned(),
            ],
            deny: false,
            token_delay: Duration::ZERO,
            oversize_jwks: false,
            token_answer: TokenAnswer::Normal,
            token_status: None,
            config_fault: None,
            config_delay: Duration::ZERO,
            keys_status: None,
            unreadable_jwks: false,
            other_secrets: Vec::new(),
            parked: 0,
        };
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
            keys,
            rng,
            issuer: format!("http://127.0.0.1:{}", address.port()),
            secret,
            stop: AtomicBool::new(false),
        });
        let accepting = {
            let shared = Arc::clone(&shared);
            thread::spawn(move || accept_loop(&listener, &shared))
        };
        Self {
            shared,
            address,
            accepting: Some(accepting),
        }
    }

    /// An issuer URL on a loopback port where nothing listens: a provider
    /// whose connections are refused.
    pub fn unreachable_issuer() -> String {
        let port =
            crate::postgres::free_port().unwrap_or_else(|e| panic!("cannot find a free port: {e}"));
        format!("http://127.0.0.1:{port}")
    }

    /// `http://127.0.0.1:<port>`, as `--oidc-issuer` takes it and as the
    /// `iss` of its tokens.
    pub fn issuer(&self) -> &str {
        &self.shared.issuer
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// The client secret the platform must present, for
    /// `VM_OIDC_CLIENT_SECRET`.
    pub fn client_secret(&self) -> ClientSecret {
        ClientSecret(self.shared.secret.clone())
    }

    /// Adds a person, or replaces the one with the same `sub`.
    pub fn add_person(&self, person: &Person) {
        self.shared
            .lock()
            .people
            .insert(person.sub.clone(), person.clone());
    }

    fn with_person(&self, sub: &str, change: impl FnOnce(&mut Person)) {
        let mut state = self.shared.lock();
        let person = state
            .people
            .get_mut(sub)
            .unwrap_or_else(|| panic!("the identity provider has no person {sub:?}"));
        change(person);
    }

    /// Changes a person's Groups (an array of these strings) for the tokens
    /// issued from now on.
    pub fn set_groups(&self, sub: &str, groups: &[&str]) {
        self.with_person(sub, |p| p.groups = Some(json!(groups)));
    }

    /// Changes a person's name for the tokens issued from now on.
    pub fn set_name(&self, sub: &str, name: Option<&str>) {
        self.with_person(sub, |p| p.name = name.map(str::to_owned));
    }

    /// Changes a person's email for the tokens issued from now on.
    pub fn set_email(&self, sub: &str, email: Option<&str>) {
        self.with_person(sub, |p| p.email = email.map(str::to_owned));
    }

    /// Disables a person: the refresh grant answers `invalid_grant` for
    /// them from now on, as does exchanging a code.
    pub fn disable(&self, sub: &str) {
        self.with_person(sub, |p| p.disabled = true);
    }

    pub fn set_outage(&self, outage: Outage) {
        self.shared.lock().outage = outage;
    }

    /// Every ID token issued from now on has this fault, until cleared with
    /// `None`.
    pub fn set_fault(&self, fault: Option<TokenFault>) {
        self.shared.lock().fault = fault;
    }

    /// The key ID tokens are signed with from now on.
    pub fn sign_with(&self, key: Key) {
        self.shared.lock().signing = key;
    }

    /// Whether the JWKS holds [`Key::Other`] too.
    pub fn publish_other_key(&self, published: bool) {
        self.shared.lock().publish_other = published;
    }

    /// Whether the token endpoint issues refresh tokens (it does unless
    /// told otherwise).
    pub fn issue_refresh_tokens(&self, issue: bool) {
        self.shared.lock().issue_refresh_tokens = issue;
    }

    /// The name of the claim the Groups are written under (`groups` unless
    /// told otherwise).
    pub fn groups_claim_name(&self, name: &str) {
        name.clone_into(&mut self.shared.lock().groups_claim);
    }

    /// The client authentications the configuration names, in
    /// `token_endpoint_auth_methods_supported`, and the only ones the token
    /// endpoint takes.
    pub fn token_auth_methods(&self, methods: &[&str]) {
        self.shared.lock().auth_methods = methods.iter().map(|m| (*m).to_owned()).collect();
    }

    /// Whether the authorization endpoint answers every request with
    /// `error=access_denied`.
    pub fn deny(&self, deny: bool) {
        self.shared.lock().deny = deny;
    }

    /// How long the token endpoint waits before answering.
    pub fn token_delay(&self, delay: Duration) {
        self.shared.lock().token_delay = delay;
    }

    /// Whether the JWKS is padded to more than 1 MiB.
    pub fn oversize_jwks(&self, oversize: bool) {
        self.shared.lock().oversize_jwks = oversize;
    }

    /// Every token request is answered with this status (and error) while
    /// it is set.
    pub fn token_status(&self, status: Option<TokenStatus>) {
        self.shared.lock().token_status = status;
    }

    /// What is wrong with the configuration while it is set.
    pub fn config_fault(&self, fault: Option<ConfigFault>) {
        self.shared.lock().config_fault = fault;
    }

    /// How long the configuration waits before answering.
    pub fn config_delay(&self, delay: Duration) {
        self.shared.lock().config_delay = delay;
    }

    /// The JWKS is answered with this status (and a JSON error) instead of
    /// `200` while it is set.
    pub fn keys_status(&self, status: Option<u16>) {
        self.shared.lock().keys_status = status;
    }

    /// How many connections [`Outage::Park`] holds now.
    pub fn parked(&self) -> usize {
        self.shared.lock().parked
    }

    /// Whether the JWKS is JSON that is no JWKS (`keys` not a list).
    pub fn unreadable_jwks(&self, unreadable: bool) {
        self.shared.lock().unreadable_jwks = unreadable;
    }

    /// Takes this client secret too, beside its own: a client secret
    /// changed at the provider while the old one still works.
    pub fn also_take_secret(&self, secret: &str) {
        self.shared.lock().other_secrets.push(secret.to_owned());
    }

    /// What the token endpoint's successful answers look like from now on.
    pub fn token_answer(&self, answer: TokenAnswer) {
        self.shared.lock().token_answer = answer;
    }

    /// Every request answered so far, in order. Fails if more were made
    /// than could be recorded.
    pub fn requests(&self) -> Vec<Recorded> {
        let state = self.shared.lock();
        assert!(
            state.recorded_dropped == 0,
            "the identity provider got more than {MAX_RECORDED} requests and recorded only the first"
        );
        state.recorded.clone()
    }

    /// The requests answered so far to this path.
    pub fn requests_to(&self, path: &str) -> Vec<Recorded> {
        self.requests()
            .into_iter()
            .filter(|r| r.path == path)
            .collect()
    }

    /// The token endpoint's requests of this grant type.
    pub fn grants(&self, grant_type: &str) -> Vec<Recorded> {
        self.requests()
            .into_iter()
            .filter(|r| r.grant_type() == Some(grant_type))
            .collect()
    }

    /// Every code and token issued so far (authorization codes, ID tokens,
    /// access tokens, refresh tokens).
    pub fn issued(&self) -> Vec<String> {
        let state = self.shared.lock();
        assert!(
            state.issued_dropped == 0,
            "the identity provider issued more than {MAX_ISSUED} codes and tokens and kept only the first"
        );
        state.issued.clone()
    }

    /// The people as they are now.
    pub fn person(&self, sub: &str) -> Person {
        self.shared
            .lock()
            .people
            .get(sub)
            .cloned()
            .unwrap_or_else(|| panic!("the identity provider has no person {sub:?}"))
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(accepting) = self.accepting.take() {
            drop(accepting.join());
        }
    }
}

fn rsa_key(file: &str) -> RsaKeyPair {
    let path = crate::program::manifest_dir()
        .join("fixtures")
        .join("oidc")
        .join(file);
    let pem = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read the provider's key {}: {e}", path.display()));
    let body: String = pem
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<Vec<_>>()
        .concat();
    let der = STANDARD
        .decode(body.trim())
        .unwrap_or_else(|e| panic!("the provider's key {} is not PEM: {e}", path.display()));
    RsaKeyPair::from_pkcs8(&der).unwrap_or_else(|e| {
        panic!(
            "the provider's key {} is not an RSA PKCS#8 key: {e}",
            path.display()
        )
    })
}

fn ec_key(rng: &SystemRandom) -> EcdsaKeyPair {
    let alg = &signature::ECDSA_P256_SHA256_FIXED_SIGNING;
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(alg, rng)
        .unwrap_or_else(|_| panic!("cannot make a P-256 key"));
    EcdsaKeyPair::from_pkcs8(alg, pkcs8.as_ref(), rng)
        .unwrap_or_else(|e| panic!("cannot read the P-256 key just made: {e}"))
}

/// Lower-case hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn now_secs() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    i64::try_from(secs).unwrap_or(i64::MAX)
}

fn accept_loop(listener: &TcpListener, shared: &Arc<Shared>) {
    let mut connections: Vec<JoinHandle<()>> = Vec::new();
    let mut parked: Vec<TcpStream> = Vec::new();
    while !shared.stop.load(Ordering::SeqCst) {
        let parking = shared.lock().outage == Outage::Park;
        if !parking && !parked.is_empty() {
            for stream in parked.drain(..) {
                drop(stream.shutdown(Shutdown::Both));
            }
            shared.lock().parked = 0;
        }
        match listener.accept() {
            Ok((stream, _)) if parking => {
                if parked.len() < MAX_PARKED {
                    parked.push(stream);
                    shared.lock().parked = parked.len();
                } else {
                    drop(stream.shutdown(Shutdown::Both));
                }
            }
            Ok((stream, _)) => {
                connections.retain(|c| !c.is_finished());
                if connections.len() >= MAX_CONNECTIONS {
                    drop(stream.shutdown(Shutdown::Both));
                    continue;
                }
                let shared = Arc::clone(shared);
                connections.push(thread::spawn(move || serve_connection(stream, &shared)));
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => thread::sleep(POLL),
            Err(_) => thread::sleep(POLL),
        }
    }
    for connection in connections {
        drop(connection.join());
    }
}

/// A request as read off the wire.
struct Incoming {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Incoming {
    fn header(&self, name: &str) -> Option<&str> {
        find(&self.headers, name)
    }
}

/// An answer to write.
struct Answer {
    status: u16,
    content_type: &'static str,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Answer {
    fn json(status: u16, value: &Value) -> Self {
        Self {
            status,
            content_type: "application/json",
            headers: Vec::new(),
            body: value.to_string().into_bytes(),
        }
    }

    fn html(status: u16, text: &str) -> Self {
        Self {
            status,
            content_type: "text/html; charset=utf-8",
            headers: Vec::new(),
            body: format!("<!doctype html><title>Harness identity provider</title><p>{text}</p>")
                .into_bytes(),
        }
    }

    fn redirect(location: &str) -> Self {
        Self {
            status: 302,
            content_type: "text/plain",
            headers: vec![("Location".to_owned(), location.to_owned())],
            body: Vec::new(),
        }
    }
}

fn serve_connection(mut stream: TcpStream, shared: &Shared) {
    let outage = shared.lock().outage;
    match outage {
        Outage::Close => {
            drop(stream.shutdown(Shutdown::Both));
            return;
        }
        Outage::Hang => {
            // The request is read and recorded, so that a test can count
            // the calls it got no answer to; then nothing is answered.
            drop(stream.set_read_timeout(Some(IO_TIMEOUT)));
            if let Some(incoming) = read_request(&mut stream) {
                let recorded = record_of(&incoming);
                keep_recorded(shared, recorded);
            }
            let began = Instant::now();
            while !shared.stop.load(Ordering::SeqCst)
                && shared.lock().outage == Outage::Hang
                && began.elapsed() < HANG_AT_MOST
            {
                thread::sleep(POLL);
            }
            drop(stream.shutdown(Shutdown::Both));
            return;
        }
        Outage::None | Outage::Park => {}
    }
    drop(stream.set_read_timeout(Some(IO_TIMEOUT)));
    drop(stream.set_write_timeout(Some(IO_TIMEOUT)));
    let Some(incoming) = read_request(&mut stream) else {
        return;
    };
    let answer = answer(&incoming, shared);
    write_answer(&mut stream, &answer);
}

fn read_request(stream: &mut TcpStream) -> Option<Incoming> {
    let mut raw = Vec::new();
    let mut chunk = [0_u8; 16 * 1024];
    let head_end = loop {
        if let Some(at) = position(&raw, b"\r\n\r\n") {
            break at;
        }
        if raw.len() > MAX_REQUEST_BYTES {
            return None;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => raw.extend_from_slice(chunk.get(..n)?),
        }
    };
    let head = String::from_utf8_lossy(raw.get(..head_end)?).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_owned();
    let target = request_line.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let mut body = raw.get(head_end + 4..)?.to_vec();
    let chunked = find(&headers, "transfer-encoding")
        .is_some_and(|te| te.to_ascii_lowercase().contains("chunked"));
    if chunked {
        loop {
            if let Some(decoded) = decode_chunked(&body) {
                body = decoded;
                break;
            }
            if body.len() > MAX_REQUEST_BYTES {
                return None;
            }
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return None,
                Ok(n) => body.extend_from_slice(chunk.get(..n)?),
            }
        }
    } else {
        let length: usize = find(&headers, "content-length").map_or(Some(0), |l| l.parse().ok())?;
        if length > MAX_REQUEST_BYTES {
            return None;
        }
        while body.len() < length {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return None,
                Ok(n) => body.extend_from_slice(chunk.get(..n)?),
            }
        }
        body.truncate(length);
    }
    Some(Incoming {
        method,
        target,
        headers,
        body,
    })
}

/// A complete chunked body decoded, or `None` while it is incomplete.
fn decode_chunked(mut rest: &[u8]) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let line_end = position(rest, b"\r\n")?;
        let size_line = String::from_utf8_lossy(rest.get(..line_end)?).into_owned();
        let size = usize::from_str_radix(size_line.split(';').next()?.trim(), 16).ok()?;
        rest = rest.get(line_end + 2..)?;
        if size == 0 {
            return Some(body);
        }
        body.extend_from_slice(rest.get(..size)?);
        rest = rest.get(size + 2..)?;
    }
}

fn position(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn write_answer(stream: &mut TcpStream, answer: &Answer) {
    let reason = match answer.status {
        200 => "OK",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Other",
    };
    let mut wire = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        answer.status,
        answer.content_type,
        answer.body.len()
    )
    .into_bytes();
    for (name, value) in &answer.headers {
        wire.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    wire.extend_from_slice(b"\r\n");
    wire.extend_from_slice(&answer.body);
    drop(stream.write_all(&wire));
    drop(stream.flush());
    drop(stream.shutdown(Shutdown::Write));
}

fn record_of(incoming: &Incoming) -> Recorded {
    let parsed = Url::parse(&format!("http://provider{}", incoming.target)).ok();
    let path = parsed
        .as_ref()
        .map_or_else(|| incoming.target.clone(), |u| u.path().to_owned());
    let query: Vec<(String, String)> = parsed
        .as_ref()
        .map(|u| u.query_pairs().into_owned().collect())
        .unwrap_or_default();
    let is_form = incoming
        .header("content-type")
        .is_some_and(|c| c.starts_with("application/x-www-form-urlencoded"));
    let form: Vec<(String, String)> = if is_form {
        url::form_urlencoded::parse(&incoming.body)
            .into_owned()
            .collect()
    } else {
        Vec::new()
    };
    Recorded {
        method: incoming.method.clone(),
        path,
        query,
        headers: incoming.headers.clone(),
        form,
        client_auth: None,
        pkce_ok: None,
        at: Instant::now(),
    }
}

fn keep_recorded(shared: &Shared, recorded: Recorded) {
    let mut state = shared.lock();
    if state.recorded.len() < MAX_RECORDED {
        state.recorded.push(recorded);
    } else {
        state.recorded_dropped += 1;
    }
}

fn answer(incoming: &Incoming, shared: &Shared) -> Answer {
    let mut recorded = record_of(incoming);
    let path = recorded.path.clone();
    let answer = match (incoming.method.as_str(), path.as_str()) {
        ("GET", "/.well-known/openid-configuration") => configuration(shared),
        ("GET", "/jwks") => jwks(shared),
        ("GET", "/authorize") => authorize(&recorded, shared),
        ("POST", "/token") => token(&mut recorded, shared),
        _ => Answer::json(404, &json!({ "error": "not_found" })),
    };
    keep_recorded(shared, recorded);
    answer
}

fn configuration(shared: &Shared) -> Answer {
    let issuer = &shared.issuer;
    let (methods, fault, delay) = {
        let state = shared.lock();
        (
            state.auth_methods.clone(),
            state.config_fault,
            state.config_delay,
        )
    };
    if !delay.is_zero() {
        thread::sleep(delay);
    }
    if let Some(ConfigFault::Status(status)) = fault {
        return Answer::json(status, &json!({ "error": "harness_status" }));
    }
    let mut body = json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{issuer}/authorize"),
            "token_endpoint": format!("{issuer}/token"),
            "jwks_uri": format!("{issuer}/jwks"),
            "response_types_supported": ["code"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256", "ES256"],
            "token_endpoint_auth_methods_supported": methods,
            "code_challenge_methods_supported": ["S256"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "scopes_supported": ["openid", "profile", "email", "offline_access"],
            "claims_supported": ["sub", "iss", "aud", "exp", "iat", "nonce", "name", "email", "groups"],
    });
    match fault {
        Some(ConfigFault::AnotherIssuer) => {
            body["issuer"] = json!(format!("{issuer}/someone-else"));
        }
        Some(ConfigFault::LacksTokenEndpoint) => {
            if let Some(fields) = body.as_object_mut() {
                fields.remove("token_endpoint");
            }
        }
        Some(ConfigFault::Oversize) => {
            body["padding"] = Value::String("x".repeat(OVERSIZE_BYTES));
        }
        Some(ConfigFault::Status(_)) | None => {}
    }
    Answer::json(200, &body)
}

fn rsa_jwk(key: &RsaKeyPair, kid: &str) -> Value {
    let components: ring::rsa::PublicKeyComponents<Vec<u8>> = key.public().into();
    json!({
        "kty": "RSA",
        "use": "sig",
        "alg": "RS256",
        "kid": kid,
        "n": URL_SAFE_NO_PAD.encode(&components.n),
        "e": URL_SAFE_NO_PAD.encode(&components.e),
    })
}

fn ec_jwk(key: &EcdsaKeyPair) -> Value {
    // An uncompressed point: 0x04, then x and y, 32 bytes each.
    let point = key.public_key().as_ref();
    let x = point.get(1..33).unwrap_or_default();
    let y = point.get(33..65).unwrap_or_default();
    json!({
        "kty": "EC",
        "use": "sig",
        "alg": "ES256",
        "crv": "P-256",
        "kid": KID_EC,
        "x": URL_SAFE_NO_PAD.encode(x),
        "y": URL_SAFE_NO_PAD.encode(y),
    })
}

fn jwks(shared: &Shared) -> Answer {
    let (publish_other, oversize, unreadable, status) = {
        let state = shared.lock();
        (
            state.publish_other,
            state.oversize_jwks,
            state.unreadable_jwks,
            state.keys_status,
        )
    };
    if let Some(status) = status {
        return Answer::json(status, &json!({ "error": "harness_status" }));
    }
    if unreadable {
        return Answer::json(200, &json!({ "keys": "these are not keys" }));
    }
    let mut keys = vec![
        rsa_jwk(&shared.keys.main, KID_MAIN),
        ec_jwk(&shared.keys.ec),
    ];
    if publish_other {
        keys.push(rsa_jwk(&shared.keys.other, KID_OTHER));
    }
    let mut body = json!({ "keys": keys });
    if oversize {
        body["padding"] = Value::String("x".repeat(OVERSIZE_BYTES));
    }
    Answer::json(200, &body)
}

fn person_of(cookie_header: Option<&str>) -> Option<String> {
    cookie_header?.split(';').find_map(|pair| {
        let (name, value) = pair.trim().split_once('=')?;
        (name == PERSON_COOKIE).then(|| value.to_owned())
    })
}

fn authorize(recorded: &Recorded, shared: &Shared) -> Answer {
    let param = |name: &str| recorded.query_param(name);
    if param("response_type") != Some("code") {
        return Answer::html(400, "unsupported response_type");
    }
    if param("client_id") != Some(CLIENT_ID) {
        return Answer::html(400, "unknown client_id");
    }
    let Some(redirect_uri) = param("redirect_uri").filter(|r| !r.is_empty()) else {
        return Answer::html(400, "no redirect_uri");
    };
    let Ok(mut back) = Url::parse(redirect_uri) else {
        return Answer::html(400, "redirect_uri is not a URL");
    };
    let mut state = shared.lock();
    if state.deny {
        {
            let mut pairs = back.query_pairs_mut();
            pairs.append_pair("error", "access_denied");
            pairs.append_pair("error_description", "the harness denied this sign-in");
            if let Some(s) = param("state") {
                pairs.append_pair("state", s);
            }
        }
        return Answer::redirect(back.as_str());
    }
    let person = person_of(recorded.header("cookie"))
        .and_then(|sub| state.people.get(&sub).map(|p| p.sub.clone()));
    let Some(sub) = person else {
        return Answer::html(
            200,
            "Who are you? (The browser is signed in as no one at the harness's provider.)",
        );
    };
    let code = shared.random();
    state.codes.insert(
        code.clone(),
        Grant {
            sub,
            nonce: param("nonce").map(str::to_owned),
            code_challenge: param("code_challenge").map(str::to_owned),
            code_challenge_method: param("code_challenge_method").map(str::to_owned),
            redirect_uri: redirect_uri.to_owned(),
            scope: param("scope").unwrap_or_default().to_owned(),
            used: false,
        },
    );
    keep_issued(&mut state, &code);
    {
        let mut pairs = back.query_pairs_mut();
        pairs.append_pair("code", &code);
        if let Some(s) = param("state") {
            pairs.append_pair("state", s);
        }
        pairs.append_pair("iss", &shared.issuer);
    }
    Answer::redirect(back.as_str())
}

fn keep_issued(state: &mut State, value: &str) {
    if state.issued.len() < MAX_ISSUED {
        state.issued.push(value.to_owned());
    } else {
        state.issued_dropped += 1;
    }
}

fn token_error(status: u16, error: &str) -> Answer {
    Answer::json(status, &json!({ "error": error }))
}

/// How the client presented itself: (method, client id, secret).
fn client_auth(recorded: &Recorded) -> (String, Option<String>, Option<String>) {
    let basic = recorded
        .header("authorization")
        .and_then(|a| {
            a.strip_prefix("Basic ")
                .or_else(|| a.strip_prefix("basic "))
        })
        .and_then(|b| STANDARD.decode(b.trim()).ok())
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|pair| {
            let (id, secret) = pair.split_once(':')?;
            let decode = |s: &str| {
                url::form_urlencoded::parse(format!("v={s}").as_bytes())
                    .next()
                    .map(|(_, v)| v.into_owned())
            };
            Some((decode(id)?, decode(secret)?))
        });
    let posted_secret = recorded.form_param("client_secret").map(str::to_owned);
    match (basic, posted_secret) {
        (Some(_), Some(_)) => ("both".to_owned(), None, None),
        (Some((id, secret)), None) => ("client_secret_basic".to_owned(), Some(id), Some(secret)),
        (None, Some(secret)) => (
            "client_secret_post".to_owned(),
            recorded.form_param("client_id").map(str::to_owned),
            Some(secret),
        ),
        (None, None) => ("none".to_owned(), None, None),
    }
}

fn token(recorded: &mut Recorded, shared: &Shared) -> Answer {
    let delay = shared.lock().token_delay;
    if !delay.is_zero() {
        thread::sleep(delay);
    }
    let (method, id, secret) = client_auth(recorded);
    recorded.client_auth = Some(method.clone());
    let mut state = shared.lock();
    if let Some(status) = &state.token_status {
        return Answer::json(
            status.status,
            &json!({ "error": status.error, "error_description": status.description }),
        );
    }
    let secret_taken = secret
        .as_deref()
        .is_some_and(|s| s == shared.secret || state.other_secrets.iter().any(|o| o == s));
    let authenticated =
        state.auth_methods.contains(&method) && id.as_deref() == Some(CLIENT_ID) && secret_taken;
    if !authenticated {
        return token_error(401, "invalid_client");
    }
    let grant = match recorded.form_param("grant_type") {
        Some("authorization_code") => {
            let code = recorded.form_param("code").unwrap_or_default();
            let Some(grant) = state.codes.get_mut(code) else {
                return token_error(400, "invalid_grant");
            };
            if grant.used {
                return token_error(400, "invalid_grant");
            }
            grant.used = true;
            if recorded.form_param("redirect_uri") != Some(grant.redirect_uri.as_str()) {
                return token_error(400, "invalid_grant");
            }
            if let Some(challenge) = &grant.code_challenge {
                let ok = grant.code_challenge_method.as_deref() == Some("S256")
                    && recorded.form_param("code_verifier").is_some_and(|v| {
                        let digest = ring::digest::digest(&ring::digest::SHA256, v.as_bytes());
                        URL_SAFE_NO_PAD.encode(digest.as_ref()) == *challenge
                    });
                recorded.pkce_ok = Some(ok);
                if !ok {
                    return token_error(400, "invalid_grant");
                }
            }
            grant.clone()
        }
        Some("refresh_token") => {
            let presented = recorded.form_param("refresh_token").unwrap_or_default();
            // Refresh tokens are one use each: a renewal that brings a new
            // one uses up the one presented, and a used one is refused.
            let Some(grant) = state.refresh_tokens.get(presented).cloned() else {
                return token_error(400, "invalid_grant");
            };
            if state.issue_refresh_tokens {
                state.refresh_tokens.remove(presented);
            }
            grant
        }
        _ => return token_error(400, "unsupported_grant_type"),
    };
    let Some(person) = state.people.get(&grant.sub).cloned() else {
        return token_error(400, "invalid_grant");
    };
    if person.disabled {
        return token_error(400, "invalid_grant");
    }
    // A renewal's ID token carries no `nonce` (OIDC Core 12.2 allows
    // either; the platform must not ask for one).
    let nonce = if recorded.form_param("grant_type") == Some("refresh_token") {
        None
    } else {
        grant.nonce.as_deref()
    };
    let id_token = id_token(shared, &state, &person, nonce);
    let access_token = shared.random();
    keep_issued(&mut state, &id_token);
    keep_issued(&mut state, &access_token);
    let mut body = json!({
        "access_token": access_token,
        "token_type": "Bearer",
        "expires_in": ID_TOKEN_SECONDS,
        "id_token": id_token,
        "scope": grant.scope,
    });
    if state.issue_refresh_tokens {
        let refresh_token = shared.random();
        keep_issued(&mut state, &refresh_token);
        state.refresh_tokens.insert(refresh_token.clone(), grant);
        body["refresh_token"] = Value::String(refresh_token);
    }
    match state.token_answer {
        TokenAnswer::Normal => Answer::json(200, &body),
        TokenAnswer::WithoutIdToken => {
            if let Some(fields) = body.as_object_mut() {
                fields.remove("id_token");
            }
            Answer::json(200, &body)
        }
        TokenAnswer::Oversize => {
            body["padding"] = Value::String("x".repeat(OVERSIZE_BYTES));
            Answer::json(200, &body)
        }
        TokenAnswer::NotJson => Answer {
            status: 200,
            content_type: "application/json",
            headers: Vec::new(),
            body: b"<html>this is not JSON</html>".to_vec(),
        },
    }
}

fn id_token(shared: &Shared, state: &State, person: &Person, nonce: Option<&str>) -> String {
    let now = now_secs();
    let fault = state.fault;
    let mut claims = Map::new();
    let issuer = if fault == Some(TokenFault::WrongIssuer) {
        format!("{}/someone-else", shared.issuer)
    } else {
        shared.issuer.clone()
    };
    let audience = if fault == Some(TokenFault::WrongAudience) {
        "another-client"
    } else {
        CLIENT_ID
    };
    let sub = if fault == Some(TokenFault::OtherSubject) {
        format!("{}-someone-else", person.sub)
    } else {
        person.sub.clone()
    };
    let expires_in = match fault {
        Some(TokenFault::ExpiresIn(seconds)) => seconds,
        _ => ID_TOKEN_SECONDS,
    };
    claims.insert("iss".to_owned(), json!(issuer));
    claims.insert("sub".to_owned(), json!(sub));
    claims.insert("aud".to_owned(), json!(audience));
    claims.insert("azp".to_owned(), json!(audience));
    claims.insert("iat".to_owned(), json!(now.min(now + expires_in - 1)));
    claims.insert("auth_time".to_owned(), json!(now));
    claims.insert("exp".to_owned(), json!(now + expires_in));
    if fault == Some(TokenFault::FractionalTimes) {
        // Halves of a second, `1794923733.5`, written as JSON numbers.
        let half = |whole: i64| {
            serde_json::from_str::<Value>(&format!("{whole}.5")).unwrap_or(Value::Null)
        };
        claims.insert("iat".to_owned(), half(now - 1));
        claims.insert("exp".to_owned(), half(now + expires_in));
    }
    if let Some(nonce) = nonce {
        let nonce = if fault == Some(TokenFault::WrongNonce) {
            format!("{nonce}-not-it")
        } else {
            nonce.to_owned()
        };
        claims.insert("nonce".to_owned(), json!(nonce));
    }
    if let Some(name) = &person.name {
        claims.insert("name".to_owned(), json!(name));
    }
    if let Some(email) = &person.email {
        claims.insert("email".to_owned(), json!(email));
    }
    if let Some(groups) = &person.groups {
        claims.insert(state.groups_claim.clone(), groups.clone());
    }
    let claims = Value::Object(claims);
    match fault {
        Some(TokenFault::AlgNone) => {
            let header = json!({ "alg": "none", "typ": "JWT" });
            format!("{}.", signing_input(&header, &claims))
        }
        Some(TokenFault::Hs256WithSecret) => {
            let header = json!({ "alg": "HS256", "typ": "JWT", "kid": KID_MAIN });
            let input = signing_input(&header, &claims);
            let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, shared.secret.as_bytes());
            let tag = ring::hmac::sign(&key, input.as_bytes());
            format!("{input}.{}", URL_SAFE_NO_PAD.encode(tag.as_ref()))
        }
        Some(TokenFault::BadSignature) => sign_rsa(shared, &shared.keys.other, KID_MAIN, &claims),
        _ => match state.signing {
            Key::Main => sign_rsa(shared, &shared.keys.main, KID_MAIN, &claims),
            Key::Other => sign_rsa(shared, &shared.keys.other, KID_OTHER, &claims),
            Key::Ec => {
                let header = json!({ "alg": "ES256", "typ": "JWT", "kid": KID_EC });
                let input = signing_input(&header, &claims);
                let signature = shared
                    .keys
                    .ec
                    .sign(&shared.rng, input.as_bytes())
                    .unwrap_or_else(|_| panic!("cannot sign with the P-256 key"));
                format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.as_ref()))
            }
        },
    }
}

fn signing_input(header: &Value, claims: &Value) -> String {
    format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.to_string()),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    )
}

fn sign_rsa(shared: &Shared, key: &RsaKeyPair, kid: &str, claims: &Value) -> String {
    let header = json!({ "alg": "RS256", "typ": "JWT", "kid": kid });
    let input = signing_input(&header, claims);
    let mut signature = vec![0_u8; key.public().modulus_len()];
    key.sign(
        &signature::RSA_PKCS1_SHA256,
        &shared.rng,
        input.as_bytes(),
        &mut signature,
    )
    .unwrap_or_else(|_| panic!("cannot sign with the RSA key"));
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature))
}

/// The claims of a JWT, read without checking anything: for the harness's
/// own tests and for messages.
pub fn claims_of(jwt: &str) -> Option<Value> {
    let payload = jwt.split('.').nth(1)?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()
}

/// The header of a JWT, read without checking anything.
pub fn header_of(jwt: &str) -> Option<Value> {
    let header = jwt.split('.').next()?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(header).ok()?).ok()
}

/// Whether a JWT's RS256 signature verifies with the RSA key in this JWK:
/// for the harness's own tests.
pub fn rs256_verifies(jwt: &str, jwk: &Value) -> bool {
    let mut parts = jwt.rsplitn(2, '.');
    let (Some(signature), Some(input)) = (parts.next(), parts.next()) else {
        return false;
    };
    let decode = |v: &Value| v.as_str().and_then(|s| URL_SAFE_NO_PAD.decode(s).ok());
    let (Some(n), Some(e), Ok(signature)) = (
        decode(&jwk["n"]),
        decode(&jwk["e"]),
        URL_SAFE_NO_PAD.decode(signature),
    ) else {
        return false;
    };
    ring::rsa::PublicKeyComponents { n, e }
        .verify(
            &signature::RSA_PKCS1_2048_8192_SHA256,
            input.as_bytes(),
            &signature,
        )
        .is_ok()
}

/// Whether a JWT's ES256 signature verifies with the P-256 key in this JWK.
pub fn es256_verifies(jwt: &str, jwk: &Value) -> bool {
    let mut parts = jwt.rsplitn(2, '.');
    let (Some(signature), Some(input)) = (parts.next(), parts.next()) else {
        return false;
    };
    let decode = |v: &Value| v.as_str().and_then(|s| URL_SAFE_NO_PAD.decode(s).ok());
    let (Some(x), Some(y), Ok(signature)) = (
        decode(&jwk["x"]),
        decode(&jwk["y"]),
        URL_SAFE_NO_PAD.decode(signature),
    ) else {
        return false;
    };
    let mut point = vec![4_u8];
    point.extend_from_slice(&x);
    point.extend_from_slice(&y);
    signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, point)
        .verify(input.as_bytes(), &signature)
        .is_ok()
}

/// `BASE64URL(SHA-256(verifier))`: the `S256` code challenge of a verifier.
pub fn s256(verifier: &str) -> String {
    URL_SAFE_NO_PAD
        .encode(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()).as_ref())
}
