//! The program serving TLS itself (CONTRACT.md, "The program serves TLS
//! itself"): `vibe serve` given `VIBE_TLS_CERT_FILE` and
//! `VIBE_TLS_KEY_FILE`, with no front of the harness's before it.
//!
//! - [`Key`] is a private key of one of the kinds the contract names (RSA
//!   2048, ECDSA P-256 and P-384, Ed25519) or of one it does not (RSA
//!   1024), written under any of the three PEM labels it names
//!   ([`KeyLabel`]). ring (through rcgen) makes the ECDSA and Ed25519 keys
//!   per test; it cannot make an RSA key, so the two RSA keys are files of
//!   `fixtures/tls/`, made once with `openssl genpkey` (OpenSSL 3.0.2) and
//!   kept: the gate never runs `openssl`. The `RSA PRIVATE KEY` and `EC
//!   PRIVATE KEY` forms are taken out of the PKCS #8 form by the harness's
//!   own small DER reader (the EC one with its curve named, as OpenSSL
//!   writes it). No key is ever printed: a `Debug` shows its kind only, and
//!   every failure message of a program given a key hides the lines of its
//!   file ([`VmOptions::hide`]).
//! - [`Pair`] is what the two files hold: a chain (the certificate first,
//!   issued by a test [`Authority`] for names, a common name and a validity
//!   of the test's choosing) and its key.
//! - [`Place`] is a folder of the test's own holding the two files, named
//!   in the program's environment; a test replaces them there as an
//!   administrator does (in place, by renaming, or by a symbolic link
//!   pointed elsewhere).
//! - [`Serving`] is the program started with a place, on a port the
//!   harness chose and gave as `--public-port` (a browser's addresses then
//!   name the port it listens on), its authority trusted by the harness's
//!   clients ([`VmOptions::reached_over_tls`]).
//! - [`Hello`] and [`shake`] are the harness's own TLS client held to what
//!   a test asks: a server name or none, one TLS version or both, ALPN
//!   protocols, the test's authority trusted or any certificate taken (to
//!   see what is served whatever name was asked); [`Shaken::get`] sends
//!   requests on the connection, one after another.
//! - [`serve_once`] runs `vibe serve` to its end, for the start-up
//!   failures.

use std::fmt;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use rcgen::{
    CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, PublicKeyData, SignatureAlgorithm, date_time_ymd,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, ProtocolVersion, RootCertStore,
    SignatureScheme, StreamOwned,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::postgres::Platform;
use crate::program::{EXIT_TIMEOUT, Finished, run_to_exit, vm_binary};
use crate::tls::{Authority, Files, api_name, pem, text_of};
use crate::vm::{DOMAIN, START_TIMEOUT, TlsClient, Vm, VmOptions, refused_address};

/// The certificate file's variable.
pub const CERT_VAR: &str = "VIBE_TLS_CERT_FILE";
/// The key file's variable.
pub const KEY_VAR: &str = "VIBE_TLS_KEY_FILE";
/// How often the program looks at the two files.
pub const LOOK_EVERY: Duration = Duration::from_secs(5);
/// Within this long of the files' change a new connection gets the new
/// certificate.
pub const TAKEN_WITHIN: Duration = Duration::from_secs(10);
/// A handshake not finished within this long is ended.
pub const HANDSHAKE_LIMIT: Duration = Duration::from_secs(10);
/// The largest file taken.
pub const MIB: usize = 1024 * 1024;
/// The longest the harness's TLS client waits for one thing (connecting,
/// one read).
pub const CLIENT_WAIT: Duration = Duration::from_secs(10);

/// `*.<domain>`: the name the certificate must have.
pub fn wildcard() -> String {
    format!("*.{DOMAIN}")
}

/// The names of the installation's certificate the harness issues:
/// `api.<domain>` and `*.<domain>`, as [`crate::tls::Behind`]'s.
pub fn installation_names() -> Vec<String> {
    vec![api_name(), wildcard()]
}

/// A kind of private key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyKind {
    Rsa2048,
    /// Not a kind the program takes: RSA under 2048 bits.
    Rsa1024,
    EcdsaP256,
    EcdsaP384,
    Ed25519,
}

/// A PEM label a key file may be written under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyLabel {
    /// `PRIVATE KEY`: PKCS #8, any kind.
    Pkcs8,
    /// `RSA PRIVATE KEY`: PKCS #1, RSA only.
    Rsa,
    /// `EC PRIVATE KEY`: SEC 1 with the curve named, ECDSA only.
    Ec,
}

/// A public key as rcgen puts it into a certificate.
struct RawPublic {
    bytes: Vec<u8>,
    alg: &'static SignatureAlgorithm,
}

impl PublicKeyData for RawPublic {
    fn der_bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn algorithm(&self) -> &'static SignatureAlgorithm {
        self.alg
    }
}

/// A private key. Never printed: its `Debug` shows its kind only.
pub struct Key {
    kind: KeyKind,
    pkcs8: Vec<u8>,
    public: RawPublic,
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Key({:?}, hidden)", self.kind)
    }
}

impl Key {
    /// A key of this kind: a fresh one for the kinds ring makes, the
    /// fixture's for RSA.
    pub fn new(kind: KeyKind) -> Self {
        match kind {
            KeyKind::Rsa2048 => Self::rsa_fixture(kind, "rsa-2048.pem"),
            KeyKind::Rsa1024 => Self::rsa_fixture(kind, "rsa-1024.pem"),
            KeyKind::EcdsaP256 => Self::generated(kind, &rcgen::PKCS_ECDSA_P256_SHA256),
            KeyKind::EcdsaP384 => Self::generated(kind, &rcgen::PKCS_ECDSA_P384_SHA384),
            KeyKind::Ed25519 => Self::generated(kind, &rcgen::PKCS_ED25519),
        }
    }

    fn generated(kind: KeyKind, alg: &'static SignatureAlgorithm) -> Self {
        let pair = KeyPair::generate_for(alg)
            .unwrap_or_else(|e| panic!("ring cannot make a {kind:?} key: {e}"));
        Self {
            kind,
            pkcs8: pair.serialize_der(),
            public: RawPublic {
                bytes: pair.der_bytes().to_vec(),
                alg,
            },
        }
    }

    fn rsa_fixture(kind: KeyKind, file: &str) -> Self {
        let path = fixture_path(file);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let pkcs8 = pem_body(&text, "PRIVATE KEY")
            .unwrap_or_else(|| panic!("{} holds no PRIVATE KEY", path.display()));
        let inner = pkcs8_private_key(&pkcs8)
            .unwrap_or_else(|| panic!("{} is not PKCS #8", path.display()));
        let public =
            rsa_public_of(&inner).unwrap_or_else(|| panic!("{} is not an RSA key", path.display()));
        Self {
            kind,
            pkcs8,
            public: RawPublic {
                bytes: public,
                alg: &rcgen::PKCS_RSA_SHA256,
            },
        }
    }

    pub fn kind(&self) -> KeyKind {
        self.kind
    }

    /// The key in PEM form under this label. Never print it.
    pub fn pem(&self, label: KeyLabel) -> String {
        match label {
            KeyLabel::Pkcs8 => pem("PRIVATE KEY", &self.pkcs8),
            KeyLabel::Rsa => {
                assert!(
                    matches!(self.kind, KeyKind::Rsa2048 | KeyKind::Rsa1024),
                    "the harness's own check: `RSA PRIVATE KEY` is for an RSA key, not {:?}",
                    self.kind
                );
                let inner = pkcs8_private_key(&self.pkcs8)
                    .unwrap_or_else(|| panic!("the harness's own key is not PKCS #8"));
                pem("RSA PRIVATE KEY", &inner)
            }
            KeyLabel::Ec => {
                assert!(
                    matches!(self.kind, KeyKind::EcdsaP256 | KeyKind::EcdsaP384),
                    "the harness's own check: `EC PRIVATE KEY` is for an ECDSA key, not {:?}",
                    self.kind
                );
                let sec1 = sec1_of(&self.pkcs8)
                    .unwrap_or_else(|| panic!("the harness cannot write the key as SEC 1"));
                pem("EC PRIVATE KEY", &sec1)
            }
        }
    }
}

/// The path of a file of `fixtures/tls/`.
pub fn fixture_path(file: &str) -> PathBuf {
    crate::program::manifest_dir()
        .join("fixtures")
        .join("tls")
        .join(file)
}

/// The text of a file of `fixtures/tls/` (an encrypted key, say). Never
/// print it.
pub fn fixture_text(file: &str) -> String {
    let path = fixture_path(file);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The DER of the first PEM section of this label in `text`.
fn pem_body(text: &str, label: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let (_, after) = text.split_once(&begin)?;
    let (body, _) = after.split_once(&end)?;
    let joined: String = body.split_whitespace().collect();
    base64::engine::general_purpose::STANDARD
        .decode(joined)
        .ok()
}

/// One DER element at the start of `bytes`: its tag, its whole length,
/// and where its content is.
fn element(bytes: &[u8]) -> Option<(u8, usize, Range<usize>)> {
    let tag = *bytes.first()?;
    let first = *bytes.get(1)?;
    let (length, header) = if first < 0x80 {
        (usize::from(first), 2)
    } else {
        let n = usize::from(first & 0x7f);
        if n == 0 || n > 4 {
            return None;
        }
        let mut length = 0_usize;
        for byte in bytes.get(2..2 + n)? {
            length = (length << 8) | usize::from(*byte);
        }
        (length, 2 + n)
    };
    let end = header.checked_add(length)?;
    (end <= bytes.len()).then_some((tag, end, header..end))
}

/// One DER element: its tag, the whole element, its content.
type Element = (u8, Vec<u8>, Vec<u8>);

/// The elements of a DER content.
fn children(content: &[u8]) -> Option<Vec<Element>> {
    let mut found = Vec::new();
    let mut at = 0;
    while at < content.len() {
        let rest = content.get(at..)?;
        let (tag, whole, inner) = element(rest)?;
        found.push((tag, rest.get(..whole)?.to_vec(), rest.get(inner)?.to_vec()));
        at += whole;
    }
    Some(found)
}

/// A DER element of this tag and content.
fn encode(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let length = content.len();
    if length < 0x80 {
        out.push(u8::try_from(length).unwrap_or_default());
    } else {
        let bytes: Vec<u8> = length
            .to_be_bytes()
            .into_iter()
            .skip_while(|b| *b == 0)
            .collect();
        out.push(0x80 | u8::try_from(bytes.len()).unwrap_or_default());
        out.extend(bytes);
    }
    out.extend_from_slice(content);
    out
}

/// The sequence's children.
fn sequence(der: &[u8]) -> Option<Vec<Element>> {
    let (tag, _, content) = element(der)?;
    if tag != 0x30 {
        return None;
    }
    children(der.get(content)?)
}

/// The private key inside a PKCS #8 `PrivateKeyInfo` (the octet string's
/// content: PKCS #1 for RSA, SEC 1 for ECDSA).
fn pkcs8_private_key(pkcs8: &[u8]) -> Option<Vec<u8>> {
    let parts = sequence(pkcs8)?;
    let (tag, _, content) = parts.get(2)?;
    (*tag == 0x04).then(|| content.clone())
}

/// The curve's OID element of an ECDSA PKCS #8 key's algorithm.
fn pkcs8_curve(pkcs8: &[u8]) -> Option<Vec<u8>> {
    let parts = sequence(pkcs8)?;
    let (_, algorithm, _) = parts.get(1)?;
    let (_, curve, _) = sequence(algorithm)?.get(1)?.clone();
    Some(curve)
}

/// The SEC 1 `ECPrivateKey` of an ECDSA PKCS #8 key, with its curve named
/// (`[0]`), as OpenSSL writes `EC PRIVATE KEY`.
fn sec1_of(pkcs8: &[u8]) -> Option<Vec<u8>> {
    let inner = pkcs8_private_key(pkcs8)?;
    let parts = sequence(&inner)?;
    if parts.iter().any(|(tag, _, _)| *tag == 0xa0) {
        return Some(inner);
    }
    let curve = pkcs8_curve(pkcs8)?;
    let mut content = Vec::new();
    for (at, (_, whole, _)) in parts.iter().enumerate() {
        content.extend_from_slice(whole);
        if at == 1 {
            content.extend(encode(0xa0, &curve));
        }
    }
    Some(encode(0x30, &content))
}

/// The `RSAPublicKey` (modulus and exponent) of a PKCS #1 `RSAPrivateKey`.
fn rsa_public_of(pkcs1: &[u8]) -> Option<Vec<u8>> {
    let parts = sequence(pkcs1)?;
    let (_, modulus, _) = parts.get(1)?;
    let (_, exponent, _) = parts.get(2)?;
    let mut content = modulus.clone();
    content.extend_from_slice(exponent);
    Some(encode(0x30, &content))
}

/// When a certificate the harness issues is valid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Validity {
    /// From 1975 to 4096 (rcgen's default).
    Now,
    /// 2020 only.
    Expired,
    /// From a day after now, for a year.
    NotYet,
}

/// A server certificate for `key`, issued by `authority`, with these DNS
/// names and this common name (the first name when `None`).
pub fn certificate(
    authority: &Authority,
    key: &Key,
    names: &[String],
    common_name: Option<&str>,
    validity: Validity,
) -> CertificateDer<'static> {
    let mut params = CertificateParams::new(names.to_vec())
        .unwrap_or_else(|e| panic!("cannot make a certificate's parameters for {names:?}: {e}"));
    let mut dn = DistinguishedName::new();
    let cn = common_name
        .map(str::to_owned)
        .or_else(|| names.first().cloned())
        .unwrap_or_else(|| "no name".to_owned());
    dn.push(DnType::CommonName, cn);
    params.distinguished_name = dn;
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    match validity {
        Validity::Now => {}
        Validity::Expired => {
            params.not_before = date_time_ymd(2020, 1, 1);
            params.not_after = date_time_ymd(2021, 1, 1);
        }
        Validity::NotYet => {
            let since_epoch = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            let day = Duration::from_secs(24 * 60 * 60);
            params.not_before = date_time_ymd(1970, 1, 1) + since_epoch + day;
            params.not_after = date_time_ymd(1970, 1, 1) + since_epoch + day * 366;
        }
    }
    authority.sign(&params, &key.public)
}

/// The SHA-256 of a certificate, 64 lowercase hex digits.
pub fn fingerprint(certificate: &[u8]) -> String {
    Sha256::digest(certificate)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What the two files hold: a chain, the certificate first, and its key
/// under a label. The key is never printed.
pub struct Pair {
    pub chain: Vec<CertificateDer<'static>>,
    pub key: Key,
    pub label: KeyLabel,
    pub names: Vec<String>,
}

impl fmt::Debug for Pair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Pair(names {:?}, {} certificates, {:?} as {:?})",
            self.names,
            self.chain.len(),
            self.key,
            self.label
        )
    }
}

impl Pair {
    /// A certificate for these names under `authority`, with a fresh key
    /// of this kind written under this label.
    pub fn issued(
        authority: &Authority,
        kind: KeyKind,
        label: KeyLabel,
        names: &[String],
        validity: Validity,
    ) -> Self {
        let key = Key::new(kind);
        let chain = vec![certificate(authority, &key, names, None, validity)];
        Self {
            chain,
            key,
            label,
            names: names.to_vec(),
        }
    }

    /// The installation's certificate as an administrator has it: for
    /// `api.<domain>` and `*.<domain>`, valid now, an ECDSA P-256 key in
    /// PKCS #8.
    pub fn installation(authority: &Authority) -> Self {
        Self::issued(
            authority,
            KeyKind::EcdsaP256,
            KeyLabel::Pkcs8,
            &installation_names(),
            Validity::Now,
        )
    }

    /// For these names, the rest as [`Pair::installation`].
    pub fn for_names(authority: &Authority, names: &[String]) -> Self {
        Self::issued(
            authority,
            KeyKind::EcdsaP256,
            KeyLabel::Pkcs8,
            names,
            Validity::Now,
        )
    }

    /// The certificate's fingerprint.
    pub fn fingerprint(&self) -> String {
        fingerprint(self.chain.first().map(AsRef::as_ref).unwrap_or_default())
    }

    /// The certificate file's text: the chain in PEM, the certificate
    /// first.
    pub fn cert_pem(&self) -> String {
        self.chain
            .iter()
            .map(|c| pem("CERTIFICATE", c.as_ref()))
            .collect()
    }

    /// The key file's text. Never print it.
    pub fn key_pem(&self) -> String {
        self.key.pem(self.label)
    }
}

/// The lines of a PEM text that hold its body (16 characters or more, no
/// `-----` line, no header): what is searched for in what a program wrote.
pub fn body_lines(pem_text: &str) -> Vec<String> {
    pem_text
        .lines()
        .map(str::trim)
        .filter(|l| l.len() >= 16 && !l.starts_with("-----") && !l.contains(':'))
        .map(str::to_owned)
        .collect()
}

/// `text` with every body line of these key texts replaced by `<hidden>`.
pub fn hide_keys(text: &str, keys: &[&str]) -> String {
    keys.iter()
        .flat_map(|k| body_lines(k))
        .fold(text.to_owned(), |t, l| t.replace(&l, "<hidden>"))
}

/// Fails unless no body line of any of these key texts is in any of these
/// texts (named by where they come from). The line itself is not shown.
pub fn assert_no_key_in(texts: &[(&str, &str)], keys: &[&str]) {
    for (whence, text) in texts {
        for (k, key) in keys.iter().enumerate() {
            let lines = body_lines(key);
            let found = lines.iter().filter(|l| text.contains(l.as_str())).count();
            assert!(
                found == 0,
                "expected nothing of the key's file in {whence}; {found} of the {} body lines of key #{k} are in it (not shown)",
                lines.len()
            );
            assert!(
                !text.contains("PRIVATE KEY-----"),
                "expected no PEM key section in {whence} (not shown)"
            );
        }
    }
}

/// A folder of the test's own holding the two files (`fullchain.pem` and
/// `privkey.pem`), removed when dropped.
pub struct Place {
    files: Files,
    pub cert: PathBuf,
    pub key: PathBuf,
}

impl Default for Place {
    fn default() -> Self {
        Self::new()
    }
}

impl Place {
    pub fn new() -> Self {
        let files = Files::new();
        let cert = files.dir().join("fullchain.pem");
        let key = files.dir().join("privkey.pem");
        Self { files, cert, key }
    }

    pub fn dir(&self) -> &Path {
        self.files.dir()
    }

    /// Writes both files in place, the key first.
    pub fn write(&self, pair: &Pair) {
        write_in_place(&self.key, pair.key_pem().as_bytes());
        write_in_place(&self.cert, pair.cert_pem().as_bytes());
    }

    /// Writes this text as the certificate file.
    pub fn write_cert(&self, text: &[u8]) {
        write_in_place(&self.cert, text);
    }

    /// Writes this text as the key file.
    pub fn write_key(&self, text: &[u8]) {
        write_in_place(&self.key, text);
    }

    /// Writes a file of another name into the folder; returns its path.
    pub fn write_beside(&self, name: &str, text: &[u8]) -> PathBuf {
        self.files.write(name, text)
    }

    /// The two variables naming the files.
    pub fn env(&self) -> [(&'static str, String); 2] {
        [
            (CERT_VAR, text_of(&self.cert)),
            (KEY_VAR, text_of(&self.key)),
        ]
    }
}

/// Writes `bytes` over the file at `path` (truncated and written, not
/// replaced).
pub fn write_in_place(path: &Path, bytes: &[u8]) {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .unwrap_or_else(|e| panic!("cannot open {}: {e}", path.display()));
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
}

/// Writes `bytes` beside `path` under another name and renames it over
/// `path`.
pub fn replace_by_renaming(path: &Path, bytes: &[u8]) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let partial = path.with_file_name(format!(".{name}.new"));
    write_in_place(&partial, bytes);
    std::fs::rename(&partial, path).unwrap_or_else(|e| {
        panic!(
            "cannot rename {} to {}: {e}",
            partial.display(),
            path.display()
        )
    });
}

/// `vibe serve` started on `127.0.0.1:<a port the harness chose>` with
/// these options, `--public-port <that port>` when `public_port`, against
/// a fresh platform database; started again on another port when the one
/// chosen was taken since (at most [`crate::dev::PORT_TRIES`] times).
/// Fixture Applications are not installed: a test makes its own.
pub fn start_on_a_port(options: &VmOptions, public_port: bool) -> Vm {
    let platform = Platform::fresh();
    let mut attempt = 1;
    loop {
        let port = crate::dev::free_port();
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        let mut tried = options.clone().platform(&platform).listen(address);
        if public_port {
            tried = tried.args_extra(&["--public-port", &port.to_string()]);
        }
        match Vm::try_start(&tried, START_TIMEOUT) {
            Ok(vm) => return vm,
            Err(message)
                if attempt < crate::dev::PORT_TRIES && refused_address(address, &message) =>
            {
                crate::dev::say_port_taken("`vibe serve`", port, attempt);
                attempt += 1;
            }
            Err(message) => panic!("{message}"),
        }
    }
}

/// `options` with the two files of `place` in the environment, the key's
/// lines hidden from every failure message, and the harness's clients
/// trusting `authority` (its certificate in `ca_file`).
pub fn options_for(
    options: VmOptions,
    place: &Place,
    key_texts: &[&str],
    authority: &Authority,
    ca_file: &Path,
) -> VmOptions {
    let mut options = place
        .env()
        .iter()
        .fold(options, |o, (name, value)| o.env(name, value));
    for line in key_texts.iter().flat_map(|k| body_lines(k)) {
        options = options.hide(&line);
    }
    options.reached_over_tls(TlsClient {
        roots: vec![authority.der()],
        ca_file: ca_file.to_owned(),
    })
}

/// The program serving TLS itself with the pair it was started with.
pub struct Serving {
    // The program first: it ends before its files go.
    pub vm: Vm,
    pub authority: Authority,
    pub place: Place,
    pub pair: Pair,
    ca: Files,
    ca_file: PathBuf,
}

impl Serving {
    /// The installation's pair ([`Pair::installation`]) under a fresh
    /// authority, the program started with the stand-in sign-in.
    pub fn start() -> Self {
        Self::start_with(VmOptions::new().stand_in_sign_in(), Pair::installation)
    }

    /// The pair `pair_of` makes under a fresh authority, the program
    /// started with these options (and `--public-port` its port).
    pub fn start_with(options: VmOptions, pair_of: impl FnOnce(&Authority) -> Pair) -> Self {
        let authority = Authority::new("Harness authority of the program's own TLS");
        let pair = pair_of(&authority);
        let place = Place::new();
        place.write(&pair);
        let ca = Files::new();
        let ca_file = ca.authorities("own-tls-authority.pem", &[&authority]);
        let key = pair.key_pem();
        let options = options_for(options, &place, &[&key], &authority, &ca_file);
        let vm = start_on_a_port(&options, true);
        Self {
            vm,
            authority,
            place,
            pair,
            ca,
            ca_file,
        }
    }

    pub fn address(&self) -> SocketAddr {
        self.vm.address()
    }

    pub fn port(&self) -> u16 {
        self.vm.address().port()
    }

    /// The file holding the authority's certificate, for `VIBE_CA_FILE`.
    pub fn ca_file(&self) -> &Path {
        &self.ca_file
    }

    /// `VIBE_CA_FILE` naming the authority's file.
    pub fn ca_env(&self) -> (&'static str, String) {
        ("VIBE_CA_FILE", text_of(&self.ca_file))
    }

    /// The folder of [`Serving::ca_file`].
    pub fn ca_dir(&self) -> &Path {
        self.ca.dir()
    }

    /// A hello for `api.<domain>`, trusting the authority.
    pub fn hello(&self) -> Hello {
        Hello::trusting(&api_name(), &self.authority)
    }

    /// The fingerprint of the certificate a new connection is served now
    /// (any certificate taken), or why there was none.
    pub fn served(&self) -> Result<String, String> {
        served_fingerprint(self.address())
    }

    /// Fails unless the program's log holds nothing of these keys' files
    /// (the one it was started with is always among them).
    pub fn assert_no_key_logged(&self, more: &[&str]) {
        let lines: String = self
            .vm
            .lines()
            .iter()
            .map(|l| format!("{}\n", l.raw))
            .collect();
        let key = self.pair.key_pem();
        let mut keys: Vec<&str> = vec![&key];
        keys.extend_from_slice(more);
        assert_no_key_in(&[("the program's log", &lines)], &keys);
    }

    /// The `tls-certificate-loaded` records so far.
    pub fn loaded(&self) -> Vec<Value> {
        records_of(&self.vm, "tls-certificate-loaded")
    }

    /// The `tls-certificate-refused` records so far.
    pub fn refused(&self) -> Vec<Value> {
        records_of(&self.vm, "tls-certificate-refused")
    }
}

/// The records of this `event` so far.
pub fn records_of(vm: &Vm, event: &str) -> Vec<Value> {
    vm.records()
        .into_iter()
        .filter(|r| r["event"] == event)
        .collect()
}

/// Whether `text` is an RFC 3339 date and time (`2026-11-19T10:00:00Z`,
/// with fractions of a second or an offset).
pub fn is_rfc3339(text: &str) -> bool {
    let bytes = text.as_bytes();
    let digits = |r: Range<usize>| {
        bytes
            .get(r)
            .is_some_and(|d| d.iter().all(u8::is_ascii_digit))
    };
    let at = |i: usize, c: u8| bytes.get(i) == Some(&c);
    let head = digits(0..4)
        && at(4, b'-')
        && digits(5..7)
        && at(7, b'-')
        && digits(8..10)
        && (at(10, b'T') || at(10, b't'))
        && digits(11..13)
        && at(13, b':')
        && digits(14..16)
        && at(16, b':')
        && digits(17..19);
    if !head {
        return false;
    }
    let mut rest = text.get(19..).unwrap_or_default();
    if let Some(fraction) = rest.strip_prefix('.') {
        let n = fraction.chars().take_while(char::is_ascii_digit).count();
        if n == 0 {
            return false;
        }
        rest = fraction.get(n..).unwrap_or_default();
    }
    rest == "Z"
        || rest == "z"
        || (rest.len() == 6
            && (rest.starts_with('+') || rest.starts_with('-'))
            && rest.get(3..4) == Some(":")
            && rest
                .get(1..3)
                .is_some_and(|d| d.chars().all(|c| c.is_ascii_digit()))
            && rest
                .get(4..6)
                .is_some_and(|d| d.chars().all(|c| c.is_ascii_digit())))
}

/// Fails unless `record` is a `tls-certificate-loaded` record of `pair`:
/// its names (those of the certificate), RFC 3339 dates, the fingerprint
/// of its first certificate, and `valid_now`; and nothing else of note.
pub fn assert_loaded_record(record: &Value, pair: &Pair, valid_now: bool) {
    assert_eq!(record["event"], "tls-certificate-loaded", "{record}");
    let mut names: Vec<String> = record["names"]
        .as_array()
        .unwrap_or_else(|| panic!("expected `names` to be a list: {record}"))
        .iter()
        .map(|n| n.as_str().unwrap_or_default().to_owned())
        .collect();
    names.sort();
    let mut want = pair.names.clone();
    want.sort();
    assert_eq!(
        names, want,
        "expected the record's names to be the certificate's DNS names: {record}"
    );
    for field in ["not_before", "not_after"] {
        let value = record[field].as_str().unwrap_or_default();
        assert!(
            is_rfc3339(value),
            "expected `{field}` to be an RFC 3339 date and time, got {value:?}: {record}"
        );
    }
    let print = record["fingerprint_sha256"].as_str().unwrap_or_default();
    assert!(
        print.len() == 64 && print.chars().all(|c| c.is_ascii_hexdigit()),
        "expected `fingerprint_sha256` to be 64 hex digits: {record}"
    );
    assert_eq!(
        print.to_ascii_lowercase(),
        pair.fingerprint(),
        "expected the fingerprint of the first certificate: {record}"
    );
    assert_eq!(
        record["valid_now"],
        Value::Bool(valid_now),
        "expected `valid_now` {valid_now}: {record}"
    );
}

/// Which TLS versions the harness's client offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Versions {
    Both,
    Tls13,
    Tls12,
}

/// What the harness's client trusts.
#[derive(Clone, Debug)]
pub enum Trust {
    /// These authorities alone, the name checked.
    Roots(Vec<CertificateDer<'static>>),
    /// Any certificate, unchecked but for the handshake's signature: to see
    /// what is served.
    Anything,
}

/// How the harness's client begins a handshake.
#[derive(Clone, Debug)]
pub struct Hello {
    /// The server name asked for; `None` asks for none (no SNI).
    pub server_name: Option<String>,
    pub trust: Trust,
    pub versions: Versions,
    /// The ALPN protocols offered, in order; none when empty.
    pub alpn: Vec<Vec<u8>>,
}

impl Hello {
    /// Asks for `name`, trusts `authority` alone, both versions, offers
    /// `http/1.1`.
    pub fn trusting(name: &str, authority: &Authority) -> Self {
        Self {
            server_name: Some(name.to_owned()),
            trust: Trust::Roots(vec![authority.der()]),
            versions: Versions::Both,
            alpn: vec![b"http/1.1".to_vec()],
        }
    }

    /// Asks for `name` (or none), takes any certificate.
    pub fn any(name: Option<&str>) -> Self {
        Self {
            server_name: name.map(str::to_owned),
            trust: Trust::Anything,
            versions: Versions::Both,
            alpn: Vec::new(),
        }
    }

    #[must_use]
    pub fn versions(mut self, versions: Versions) -> Self {
        self.versions = versions;
        self
    }

    #[must_use]
    pub fn alpn(mut self, protocols: &[&[u8]]) -> Self {
        self.alpn = protocols.iter().map(|p| p.to_vec()).collect();
        self
    }
}

/// Takes any certificate; checks the handshake's signatures with ring's
/// algorithms.
#[derive(Debug)]
struct TakeAnything(rustls::crypto::WebPkiSupportedAlgorithms);

impl ServerCertVerifier for TakeAnything {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_schemes()
    }
}

/// Has no client certificate, and records whether the server asked for
/// one (rustls asks it only on a `CertificateRequest`).
#[derive(Debug, Default)]
struct AskedForCertificate(std::sync::atomic::AtomicBool);

impl rustls::client::ResolvesClientCert for AskedForCertificate {
    fn resolve(
        &self,
        _root_hint_subjects: &[&[u8]],
        _sigschemes: &[SignatureScheme],
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        None
    }

    fn has_certs(&self) -> bool {
        false
    }
}

fn client_config(hello: &Hello, asked: &Arc<AskedForCertificate>) -> Result<ClientConfig, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let algorithms = provider.signature_verification_algorithms;
    let builder = ClientConfig::builder_with_provider(Arc::clone(&provider));
    let builder = match hello.versions {
        Versions::Both => {
            builder.with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        }
        Versions::Tls13 => builder.with_protocol_versions(&[&rustls::version::TLS13]),
        Versions::Tls12 => builder.with_protocol_versions(&[&rustls::version::TLS12]),
    }
    .map_err(|e| format!("the harness's TLS versions: {e}"))?;
    let mut config = match &hello.trust {
        Trust::Roots(roots) => {
            let mut store = RootCertStore::empty();
            for root in roots {
                store
                    .add(root.clone())
                    .map_err(|e| format!("cannot trust a root: {e}"))?;
            }
            builder
                .with_root_certificates(store)
                .with_client_cert_resolver(
                    Arc::clone(asked) as Arc<dyn rustls::client::ResolvesClientCert>
                )
        }
        Trust::Anything => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(TakeAnything(algorithms)))
            .with_client_cert_resolver(
                Arc::clone(asked) as Arc<dyn rustls::client::ResolvesClientCert>
            ),
    };
    config.alpn_protocols.clone_from(&hello.alpn);
    Ok(config)
}

/// A finished handshake with the program: what it presented and agreed
/// to, and the connection, on which requests can be sent.
pub struct Shaken {
    pub chain: Vec<CertificateDer<'static>>,
    pub version: Option<ProtocolVersion>,
    pub alpn: Option<Vec<u8>>,
    /// Whether the server asked for a client certificate.
    pub asked_for_client_certificate: bool,
    stream: StreamOwned<ClientConnection, TcpStream>,
}

impl fmt::Debug for Shaken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Shaken(version {:?}, alpn {:?}, {} certificates)",
            self.version,
            self.alpn.as_deref().map(String::from_utf8_lossy),
            self.chain.len()
        )
    }
}

impl Shaken {
    /// The fingerprint of the certificate presented.
    pub fn fingerprint(&self) -> String {
        fingerprint(self.chain.first().map(AsRef::as_ref).unwrap_or_default())
    }

    /// Sends `GET <target>` with `Host: <host>` on this connection (kept
    /// open) and reads the one answer: its status and body.
    pub fn get(&mut self, host: &str, target: &str) -> Result<(u16, Vec<u8>), String> {
        let request = format!("GET {target} HTTP/1.1\r\nHost: {host}\r\n\r\n");
        self.stream
            .write_all(request.as_bytes())
            .and_then(|()| self.stream.flush())
            .map_err(|e| format!("cannot send the request: {e}"))?;
        read_answer(&mut self.stream)
    }
}

/// Reads one HTTP/1.1 answer (its head, then a body framed by
/// `Content-Length` or chunked) from `stream`.
fn read_answer(stream: &mut impl Read) -> Result<(u16, Vec<u8>), String> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    let head_end = loop {
        if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        if bytes.len() > 64 * 1024 {
            return Err("an answer's head longer than 64 KiB".to_owned());
        }
        match stream.read(&mut chunk) {
            Ok(0) => {
                return Err(format!(
                    "the connection ended after {} bytes of a head",
                    bytes.len()
                ));
            }
            Ok(n) => bytes.extend_from_slice(chunk.get(..n).unwrap_or_default()),
            Err(e) => return Err(format!("reading the answer: {e}")),
        }
    };
    let head = String::from_utf8_lossy(bytes.get(..head_end).unwrap_or_default()).into_owned();
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("not an HTTP answer: {:?}", head.lines().next()))?;
    let header = |name: &str| {
        head.lines().skip(1).find_map(|l| {
            let (n, v) = l.split_once(':')?;
            n.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim().to_owned())
        })
    };
    let mut body = bytes.get(head_end..).unwrap_or_default().to_vec();
    let read_more = |stream: &mut dyn Read, body: &mut Vec<u8>| -> Result<(), String> {
        let mut chunk = [0_u8; 4096];
        match stream.read(&mut chunk) {
            Ok(0) => Err("the connection ended inside a body".to_owned()),
            Ok(n) => {
                body.extend_from_slice(chunk.get(..n).unwrap_or_default());
                Ok(())
            }
            Err(e) => Err(format!("reading a body: {e}")),
        }
    };
    if header("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        while !body.windows(5).any(|w| w == b"0\r\n\r\n") {
            if body.len() > 16 * 1024 * 1024 {
                return Err("a body over 16 MiB".to_owned());
            }
            read_more(stream, &mut body)?;
        }
        return Ok((status, body));
    }
    let length = header("content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    if length > 16 * 1024 * 1024 {
        return Err("a body over 16 MiB".to_owned());
    }
    while body.len() < length {
        read_more(stream, &mut body)?;
    }
    body.truncate(length);
    Ok((status, body))
}

/// A handshake with `address` as `hello` says; `Err` with the client's own
/// words when it fails. Each wait (connecting, each read) is held to
/// [`CLIENT_WAIT`].
pub fn shake(address: SocketAddr, hello: &Hello) -> Result<Shaken, String> {
    let asked = Arc::new(AskedForCertificate::default());
    let config = client_config(hello, &asked)?;
    let name = match &hello.server_name {
        Some(name) => ServerName::try_from(name.clone())
            .map_err(|e| format!("not a server name {name:?}: {e}"))?,
        // An address as the name: rustls then sends no SNI.
        None => ServerName::IpAddress(address.ip().into()),
    };
    let connection = ClientConnection::new(Arc::new(config), name)
        .map_err(|e| format!("cannot begin TLS: {e}"))?;
    let socket = TcpStream::connect_timeout(&address, CLIENT_WAIT)
        .map_err(|e| format!("cannot connect to {address}: {e}"))?;
    socket
        .set_read_timeout(Some(CLIENT_WAIT))
        .and_then(|()| socket.set_write_timeout(Some(CLIENT_WAIT)))
        .map_err(|e| format!("cannot set timeouts: {e}"))?;
    let mut stream = StreamOwned::new(connection, socket);
    let deadline = Instant::now() + CLIENT_WAIT;
    while stream.conn.is_handshaking() {
        if Instant::now() >= deadline {
            return Err(format!("the handshake did not end within {CLIENT_WAIT:?}"));
        }
        stream
            .conn
            .complete_io(&mut stream.sock)
            .map_err(|e| format!("TLS: {e}"))?;
    }
    let chain = stream
        .conn
        .peer_certificates()
        .map(|c| c.iter().map(|d| d.clone().into_owned()).collect())
        .unwrap_or_default();
    let version = stream.conn.protocol_version();
    let alpn = stream.conn.alpn_protocol().map(<[u8]>::to_vec);
    Ok(Shaken {
        chain,
        version,
        alpn,
        asked_for_client_certificate: asked.0.load(std::sync::atomic::Ordering::SeqCst),
        stream,
    })
}

/// The fingerprint of the certificate a new connection to `address` is
/// served (asking for `api.<domain>`, any certificate taken).
pub fn served_fingerprint(address: SocketAddr) -> Result<String, String> {
    shake(address, &Hello::any(Some(&api_name()))).map(|s| s.fingerprint())
}

/// Polls new connections to `address` every 100 ms until one is served
/// the certificate of `fingerprint`, at most `within` after `since`;
/// returns how long after `since` it was, or what was served last.
pub fn wait_until_served(
    address: SocketAddr,
    fingerprint: &str,
    since: Instant,
    within: Duration,
) -> Result<Duration, String> {
    loop {
        let seen = served_fingerprint(address);
        if seen.as_deref() == Ok(fingerprint) {
            return Ok(since.elapsed());
        }
        if since.elapsed() >= within {
            return Err(format!(
                "after {:?} a new connection was served {seen:?}",
                since.elapsed()
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// How a plain connection ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ended {
    /// The program closed it (a read gave the end, or it was reset).
    Closed(Duration),
    /// Still open when the harness stopped waiting.
    Open,
}

/// Connects to `address` without TLS, sends `bytes`, and reads until the
/// program closes the connection, at most `wait`: what it sent back (the
/// first MiB) and how it ended.
pub fn plain_exchange(address: SocketAddr, bytes: &[u8], wait: Duration) -> (Vec<u8>, Ended) {
    let began = Instant::now();
    let mut socket = TcpStream::connect_timeout(&address, CLIENT_WAIT)
        .unwrap_or_else(|e| panic!("cannot connect to {address}: {e}"));
    socket
        .set_write_timeout(Some(CLIENT_WAIT))
        .unwrap_or_else(|e| panic!("cannot set a timeout: {e}"));
    if !bytes.is_empty() {
        socket
            .write_all(bytes)
            .unwrap_or_else(|e| panic!("cannot send to {address}: {e}"));
    }
    let mut got = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let left = wait.saturating_sub(began.elapsed());
        if left.is_zero() {
            return (got, Ended::Open);
        }
        socket
            .set_read_timeout(Some(
                left.min(Duration::from_millis(250))
                    .max(Duration::from_millis(1)),
            ))
            .unwrap_or_else(|e| panic!("cannot set a timeout: {e}"));
        match socket.read(&mut chunk) {
            Ok(0) => return (got, Ended::Closed(began.elapsed())),
            Ok(n) => {
                // At most 1 MiB is kept; the rest is read and let go.
                let room = MIB.saturating_sub(got.len());
                got.extend_from_slice(chunk.get(..n.min(room)).unwrap_or_default());
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => return (got, Ended::Closed(began.elapsed())),
        }
    }
}

/// `vibe serve --domain <domain> --listen 127.0.0.1:0` and these more
/// arguments, with a fresh platform database and these variables (no
/// other TLS variable, no installation's key), run to its end (killed at
/// [`EXIT_TIMEOUT`]); with how long it took.
pub fn serve_once(more: &[&str], env: &[(&str, &str)]) -> (Finished, Duration) {
    let platform = Platform::fresh();
    let mut command = Command::new(vm_binary());
    command
        .args(["serve", "--domain", DOMAIN, "--listen", "127.0.0.1:0"])
        .args(more)
        .env("VIBE_DATABASE_URL", platform.url())
        .env_remove(CERT_VAR)
        .env_remove(KEY_VAR)
        .env_remove("VIBE_CA_FILE")
        .env_remove("VIBE_SECRET_KEY")
        .env_remove("VIBE_SETUP_CREDENTIAL")
        .env_remove("VIBE_OIDC_CLIENT_SECRET")
        .envs(env.iter().copied());
    let began = Instant::now();
    let finished = run_to_exit(command, EXIT_TIMEOUT)
        .unwrap_or_else(|problem| panic!("cannot run {}: {problem}", vm_binary().display()));
    (finished, began.elapsed())
}

/// Whether the program wrote a `listening` record.
pub fn listened(finished: &Finished) -> bool {
    finished
        .stderr
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .any(|r| r["event"] == "listening")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_dates_are_told_from_other_text() {
        for good in [
            "2026-11-19T10:00:00Z",
            "2026-11-19T10:00:00.123Z",
            "2026-11-19T10:00:00+01:00",
            "4096-01-01T00:00:00Z",
        ] {
            assert!(is_rfc3339(good), "{good}");
        }
        for bad in [
            "2026-11-19",
            "2026-11-19 10:00:00Z",
            "2026-11-19T10:00:00",
            "1763546400",
            "",
        ] {
            assert!(!is_rfc3339(bad), "{bad}");
        }
    }

    #[test]
    fn der_lengths_are_written_and_read_back() {
        for n in [0_usize, 1, 127, 128, 255, 256, 70_000] {
            let content = vec![7_u8; n];
            let der = encode(0x04, &content);
            let (tag, whole, inner) = element(&der).unwrap();
            assert_eq!(
                (tag, whole, der.get(inner).unwrap()),
                (0x04, der.len(), &content[..])
            );
        }
    }

    #[test]
    fn body_lines_leave_out_the_armour_and_headers() {
        let text = "-----BEGIN EC PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: AES-256-CBC,00\n\nABCDEFGHIJKLMNOPQRSTUVWX\nshort\n-----END EC PRIVATE KEY-----\n";
        assert_eq!(body_lines(text), vec!["ABCDEFGHIJKLMNOPQRSTUVWX"]);
    }
}
