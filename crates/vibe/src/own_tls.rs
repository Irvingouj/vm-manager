//! The program serves TLS itself (blackbox/CONTRACT.md, "The program serves
//! TLS itself"): with the wildcard certificate and the key the Platform
//! administrator supplies in the files `VIBE_TLS_CERT_FILE` and
//! `VIBE_TLS_KEY_FILE` name, `vibe serve` serves HTTPS only on its
//! listening address.
//!
//! - [`Files::from_environment`] reads the two variables: both or neither.
//! - [`load`] reads the two files and checks them (the checks at start): a
//!   [`Loaded`] pair, or the words of what is wrong, naming the variable and
//!   the path.
//! - [`InUse`] holds the pair new connections are given; it is the server
//!   configuration's certificate resolver. [`server_config`] builds that
//!   configuration once: TLS 1.3 and 1.2, ALPN `http/1.1` alone, no client
//!   certificate.
//! - [`watch`] looks at the two files every [`LOOK_EVERY`] and puts a
//!   changed pair that passes in use; one that does not is logged once.
//!
//! **The key.** It is read into a [`KeyBytes`] (no `Debug`, no `Display`,
//! overwritten when dropped), decoded by rustls's PEM reader and handed to
//! rustls's ring provider, which keeps it as a signing key inside a
//! `CertifiedKey`; no type of this module that holds it prints. No message
//! made here holds a byte of the key's file: a PEM error of that file is
//! never formatted, only said in words of our own.
//!
//! **Bounds.** Each file is opened, looked at (a regular file, at most
//! [`MAX_FILE_BYTES`]) and read through a `take` of one byte more than the
//! bound. The watcher reads them on tokio's blocking pool, one read at a
//! time, waited for at most [`READ_LIMIT`] (a read still running then is
//! waited for at the next look; no second one is started beside it).

use std::fmt;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustls::ServerConfig;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::{PemObject, SectionKind};
use rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs1KeyDer, PrivatePkcs8KeyDer, PrivateSec1KeyDer,
};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::task::JoinHandle;
use x509_parser::prelude::{FromDer, GeneralName, X509Certificate};

use crate::log;

/// The variable that names the certificate file.
pub const CERT_VARIABLE: &str = "VIBE_TLS_CERT_FILE";

/// The variable that names the key file.
pub const KEY_VARIABLE: &str = "VIBE_TLS_KEY_FILE";

/// The largest file taken, of either.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// The most certificates the certificate file may hold: the installation's
/// and its chain.
pub const MAX_CERTIFICATES: usize = 10;

/// How often the two files are looked at.
pub const LOOK_EVERY: Duration = Duration::from_secs(5);

/// The longest a handshake may take, from the connection's accept.
pub const HANDSHAKE_LIMIT: Duration = Duration::from_secs(10);

/// The longest the watcher waits for one read of the two files.
pub const READ_LIMIT: Duration = Duration::from_secs(5);

/// The most of the certificate's names a message lists.
const NAMES_LISTED: usize = 10;

/// The two files' paths, as the two variables name them.
#[derive(Clone, Debug)]
pub struct Files {
    pub cert: PathBuf,
    pub key: PathBuf,
}

impl Files {
    /// The two variables read: `None` when neither is set (an empty value
    /// counts as not set), the two paths when both are, and a message
    /// naming the missing one when only one is.
    pub fn from_environment() -> Result<Option<Self>, String> {
        let read = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
        match (read(CERT_VARIABLE), read(KEY_VARIABLE)) {
            (None, None) => Ok(None),
            (Some(cert), Some(key)) => Ok(Some(Self {
                cert: PathBuf::from(cert),
                key: PathBuf::from(key),
            })),
            (Some(_), None) => Err(one_without_the_other(CERT_VARIABLE, KEY_VARIABLE)),
            (None, Some(_)) => Err(one_without_the_other(KEY_VARIABLE, CERT_VARIABLE)),
        }
    }
}

fn one_without_the_other(set: &str, missing: &str) -> String {
    format!(
        "{set} is set and {missing} is not: to serve HTTPS itself the program needs both, {CERT_VARIABLE} naming the certificate file and {KEY_VARIABLE} naming its key's file; set {missing}, or unset {set} to serve plain HTTP behind a reverse proxy"
    )
}

/// The bytes of the key's file. Never printed; overwritten when dropped.
struct KeyBytes(Vec<u8>);

impl Drop for KeyBytes {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

/// What the two files held at one look.
struct Contents {
    cert: Vec<u8>,
    key: KeyBytes,
}

impl Contents {
    /// A digest of both files' bytes, to tell a change: never shown.
    fn digest(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(Sha256::digest(&self.cert));
        hasher.update(Sha256::digest(&self.key.0));
        hasher.finalize().into()
    }
}

/// A pair that passed every check.
pub struct Loaded {
    certified: Arc<CertifiedKey>,
    /// What the two files held: to tell a change, never shown.
    digest: [u8; 32],
    /// The `tls-certificate-loaded` record's fields: nothing of the key.
    record: Value,
}

impl Loaded {
    /// Writes the `tls-certificate-loaded` record.
    pub fn log(&self) {
        log::record("tls-certificate-loaded", self.record.clone());
    }

    /// What the two files held, for [`watch`].
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
}

/// Reads the two files and checks them for `domain` (lower case, no
/// trailing dot): the pair, or the words of the start-up failure.
pub fn load(files: &Files, domain: &str) -> Result<Loaded, String> {
    let read = read_both(files)?;
    check(files, domain, &read)
}

fn read_both(files: &Files) -> Result<Contents, String> {
    let cert = read_file(&files.cert)
        .map_err(|what| about(CERT_VARIABLE, &files.cert, &what, CERT_FIX))?;
    let key = read_file(&files.key)
        .map(KeyBytes)
        .map_err(|what| about(KEY_VARIABLE, &files.key, &what, KEY_FIX))?;
    Ok(Contents { cert, key })
}

const CERT_FIX: &str = "name a file of the installation's certificate for *.<domain> in PEM, its chain after it (at most ten certificates, at most 1 MiB)";
const KEY_FIX: &str = "name the file of the certificate's private key in PEM (PRIVATE KEY, RSA PRIVATE KEY or EC PRIVATE KEY; not encrypted; at most 1 MiB)";

/// `<variable> names <path>, which <what>: <fix>`.
fn about(variable: &str, path: &Path, what: &str, fix: &str) -> String {
    format!("{variable} names {}, which {what}: {fix}", path.display())
}

/// The bytes of the file at `path`, at most [`MAX_FILE_BYTES`]; what is
/// wrong otherwise. The words never hold a byte of the file.
fn read_file(path: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|e| match e.kind() {
        ErrorKind::NotFound => "is not there (no such file)".to_owned(),
        _ => format!("cannot be read ({e})"),
    })?;
    let meta = file
        .metadata()
        .map_err(|e| format!("cannot be read ({e})"))?;
    if !meta.is_file() {
        return Err("is not a file".to_owned());
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err("is larger than 1 MiB".to_owned());
    }
    // Room for the whole file and the one byte that tells it grew.
    let room = usize::try_from(meta.len().saturating_add(1)).unwrap_or(0);
    let mut bytes = Vec::with_capacity(room);
    let mut file = file.take(MAX_FILE_BYTES + 1);
    if let Err(e) = file.read_to_end(&mut bytes) {
        bytes.fill(0);
        return Err(format!("cannot be read ({e})"));
    }
    if u64::try_from(bytes.len()).map_or(true, |n| n > MAX_FILE_BYTES) {
        bytes.fill(0);
        return Err("is larger than 1 MiB".to_owned());
    }
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err("is empty".to_owned());
    }
    Ok(bytes)
}

/// Every check of the contract's "What is checked at start", on what was
/// read.
fn check(files: &Files, domain: &str, read: &Contents) -> Result<Loaded, String> {
    let cert_problem = |what: &str| about(CERT_VARIABLE, &files.cert, what, CERT_FIX);
    let key_problem = |what: &str| about(KEY_VARIABLE, &files.key, what, KEY_FIX);

    // The certificates.
    let mut chain: Vec<CertificateDer<'static>> = Vec::new();
    for section in CertificateDer::pem_slice_iter(&read.cert) {
        // The error is not formatted: the file may hold a key by mistake.
        let certificate = section
            .map_err(|_| cert_problem("is not PEM that can be read (a section is broken)"))?;
        chain.push(certificate);
        if chain.len() > MAX_CERTIFICATES {
            let count = CertificateDer::pem_slice_iter(&read.cert).count();
            return Err(cert_problem(&format!(
                "holds {count} certificates: at most ten are taken (the installation's certificate, then its chain)"
            )));
        }
    }
    let Some(first) = chain.first() else {
        return Err(cert_problem(
            "holds no certificate (a PEM section BEGIN CERTIFICATE)",
        ));
    };
    let parsed = match X509Certificate::from_der(first.as_ref()) {
        Ok(([], parsed)) => parsed,
        Ok(_) => {
            return Err(cert_problem(
                "holds a first certificate that cannot be parsed (bytes follow it)",
            ));
        }
        Err(e) => {
            return Err(cert_problem(&format!(
                "holds a first certificate that cannot be parsed ({e})"
            )));
        }
    };
    let names = dns_names(&parsed).map_err(|e| {
        cert_problem(&format!(
            "holds a first certificate whose names cannot be read ({e})"
        ))
    })?;
    let validity = parsed.validity();
    let not_before = validity.not_before.timestamp();
    let not_after = validity.not_after.timestamp();
    let fingerprint = hex(&Sha256::digest(first.as_ref()));
    drop(parsed);

    // The key.
    let key = private_key(&read.key.0).map_err(|what| key_problem(&what))?;
    let provider = provider();
    // What the key's DER says it is, read before ring takes the key: ring's
    // refusal does not say whether the kind is not taken or the key is
    // damaged.
    let kind = key_kind(&key);
    let signing = provider
        .key_provider
        .load_private_key(key)
        .map_err(|_| match kind {
            KeyKind::NotTaken(what) => key_problem(&format!(
                "holds {what}, a kind of key that is not supported: {KINDS_TAKEN}"
            )),
            KeyKind::Taken | KeyKind::Unreadable => key_problem(KEY_UNREADABLE),
        })?;

    // The names.
    let wildcard = format!("*.{domain}");
    if !names.iter().any(|n| n.eq_ignore_ascii_case(&wildcard)) {
        let listed: Vec<&str> = names
            .iter()
            .take(NAMES_LISTED)
            .map(String::as_str)
            .collect();
        let has = match names.len() {
            0 => "it has no DNS name".to_owned(),
            n if n > NAMES_LISTED => format!(
                "its DNS names are {} and {} more",
                listed.join(", "),
                n - NAMES_LISTED
            ),
            _ => format!("its DNS names are {}", listed.join(", ")),
        };
        return Err(format!(
            "{CERT_VARIABLE} names {}, whose first certificate does not cover {wildcard}: none of its DNS names (subject alternative names; the common name is not looked at) is {wildcard}, and {has}. Name a certificate for {wildcard}, or give the --domain it is for",
            files.cert.display()
        ));
    }

    // The two together.
    let certified = CertifiedKey::new(chain, signing);
    if certified.keys_match().is_err() {
        return Err(format!(
            "the key of {KEY_VARIABLE} ({}) and the certificate of {CERT_VARIABLE} ({}) do not belong together: the key is not the key of the first certificate. Name the key the certificate was made for",
            files.key.display(),
            files.cert.display()
        ));
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0);
    let record = json!({
        "names": names,
        "not_before": rfc3339(not_before),
        "not_after": rfc3339(not_after),
        "fingerprint_sha256": fingerprint,
        "valid_now": not_before <= now && now <= not_after,
    });
    Ok(Loaded {
        certified: Arc::new(certified),
        digest: read.digest(),
        record,
    })
}

const KINDS_TAKEN: &str =
    "an RSA key of 2048 to 4096 bits, an ECDSA key on P-256 or P-384, or an Ed25519 key is taken";
const KEY_UNREADABLE: &str = "holds a key that cannot be read: its PEM section is whole, but what it holds is damaged, not a key of its label (PRIVATE KEY, RSA PRIVATE KEY or EC PRIVATE KEY)";

/// The DNS names among the certificate's subject alternative names.
fn dns_names(certificate: &X509Certificate<'_>) -> Result<Vec<String>, String> {
    let Some(names) = certificate
        .subject_alternative_name()
        .map_err(|e| e.to_string())?
    else {
        return Ok(Vec::new());
    };
    Ok(names
        .value
        .general_names
        .iter()
        .filter_map(|name| match name {
            GeneralName::DNSName(name) => Some((*name).to_owned()),
            _ => None,
        })
        .collect())
}

/// The one private key of the key file's text; what is wrong otherwise, in
/// words that hold nothing of the file.
fn private_key(text: &[u8]) -> Result<PrivateKeyDer<'static>, String> {
    // The labels of the sections, and whether one is encrypted, from the
    // lines that begin a section or are a header of one: never a line of a
    // key's body.
    let mut keys = 0_usize;
    let mut encrypted = false;
    let mut other_kind = false;
    for line in text.split(|b| *b == b'\n' || *b == b'\r') {
        let line = line.trim_ascii();
        if let Some(label) = line
            .strip_prefix(b"-----BEGIN ")
            .and_then(|rest| rest.strip_suffix(b"-----"))
        {
            if label.ends_with(b"PRIVATE KEY") {
                keys += 1;
                match label {
                    b"PRIVATE KEY" | b"RSA PRIVATE KEY" | b"EC PRIVATE KEY" => {}
                    b"ENCRYPTED PRIVATE KEY" => encrypted = true,
                    _ => other_kind = true,
                }
            }
        } else if line.starts_with(b"Proc-Type:") && line.ends_with(b"ENCRYPTED") {
            encrypted = true;
        }
    }
    if keys > 1 {
        return Err(format!(
            "holds more than one key ({keys} PEM sections of a private key): it must hold only one, the certificate's"
        ));
    }
    if encrypted {
        return Err("holds a key that is encrypted: a passphrase is not supported; write the key out unencrypted (for example `openssl pkey -in <encrypted> -out <key>`) and keep the file readable only by the program".to_owned());
    }
    if other_kind {
        return Err(format!(
            "holds a key of a kind that is not supported: {KINDS_TAKEN}"
        ));
    }
    if keys == 0 {
        return Err(
            "holds no private key (a PEM section BEGIN PRIVATE KEY, RSA PRIVATE KEY or EC PRIVATE KEY)"
                .to_owned(),
        );
    }
    for section in <(SectionKind, Vec<u8>)>::pem_slice_iter(text) {
        // The error is not formatted: it may quote the file.
        let Ok((kind, der)) = section else {
            return Err(KEY_BROKEN.to_owned());
        };
        let key = match kind {
            SectionKind::PrivateKey => PrivateKeyDer::from(PrivatePkcs8KeyDer::from(der)),
            SectionKind::RsaPrivateKey => PrivateKeyDer::from(PrivatePkcs1KeyDer::from(der)),
            SectionKind::EcPrivateKey => PrivateKeyDer::from(PrivateSec1KeyDer::from(der)),
            _ => continue,
        };
        return Ok(key);
    }
    Err(KEY_BROKEN.to_owned())
}

const KEY_BROKEN: &str =
    "holds a key that cannot be read: its PEM section is broken (not base64, or never ended)";

/// What a key's DER says it is. Nothing of the key is kept: only the kind.
enum KeyKind {
    /// A kind ring takes (whether ring then takes this key or not).
    Taken,
    /// A kind that is not taken, in words: "an ECDSA key on P-521".
    NotTaken(&'static str),
    /// The DER is not a key of its label.
    Unreadable,
}

/// The OIDs, as their DER contents.
const OID_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
const OID_EC: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const OID_ED25519: &[u8] = &[0x2b, 0x65, 0x70];
const OID_ED448: &[u8] = &[0x2b, 0x65, 0x71];
const OID_DSA: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x38, 0x04, 0x01];
const OID_P256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
const OID_P384: &[u8] = &[0x2b, 0x81, 0x04, 0x00, 0x22];
const OID_P521: &[u8] = &[0x2b, 0x81, 0x04, 0x00, 0x23];
const OID_SECP256K1: &[u8] = &[0x2b, 0x81, 0x04, 0x00, 0x0a];

/// DER tags read here.
const SEQUENCE: u8 = 0x30;
const INTEGER: u8 = 0x02;
const OCTET_STRING: u8 = 0x04;
const OID: u8 = 0x06;
const NULL: u8 = 0x05;
/// SEC 1's `[0] parameters`, constructed.
const CONTEXT_0: u8 = 0xa0;

/// The key's kind from its DER, by its label: PKCS #8 (`PRIVATE KEY`),
/// PKCS #1 (`RSA PRIVATE KEY`) or SEC 1 (`EC PRIVATE KEY`).
fn key_kind(key: &PrivateKeyDer<'_>) -> KeyKind {
    let kind = match key {
        PrivateKeyDer::Pkcs8(k) => pkcs8_kind(k.secret_pkcs8_der()),
        PrivateKeyDer::Pkcs1(k) => rsa_kind(k.secret_pkcs1_der()),
        PrivateKeyDer::Sec1(k) => sec1_kind(k.secret_sec1_der()),
        _ => None,
    };
    kind.unwrap_or(KeyKind::Unreadable)
}

/// PKCS #8: SEQUENCE { INTEGER version, SEQUENCE { OID algorithm,
/// parameters }, OCTET STRING privateKey, ... }.
fn pkcs8_kind(der: &[u8]) -> Option<KeyKind> {
    let mut info = Der::whole(der, SEQUENCE)?;
    info.next(INTEGER)?;
    let mut algorithm = Der::new(info.next(SEQUENCE)?);
    let oid = algorithm.next(OID)?;
    let private = info.next(OCTET_STRING)?;
    Some(match oid {
        OID_RSA => {
            // parameters NULL, or absent.
            if !algorithm.is_empty() {
                algorithm.next(NULL)?;
            }
            rsa_kind(private)?
        }
        OID_EC => {
            let curve = algorithm.next(OID)?;
            let kind = curve_kind(curve);
            if matches!(kind, KeyKind::Taken) {
                // The key itself must be a SEC 1 structure.
                sec1_kind(private)?;
            }
            kind
        }
        OID_ED25519 => {
            // CurvePrivateKey ::= OCTET STRING of 32 bytes.
            let seed = Der::whole(private, OCTET_STRING)?.rest;
            if seed.len() != 32 {
                return None;
            }
            KeyKind::Taken
        }
        OID_ED448 => KeyKind::NotTaken("an Ed448 key"),
        OID_DSA => KeyKind::NotTaken("a DSA key"),
        _ => KeyKind::NotTaken("a key of an algorithm that is not RSA, ECDSA or Ed25519"),
    })
}

/// PKCS #1: SEQUENCE { INTEGER version, INTEGER modulus, ... }.
fn rsa_kind(der: &[u8]) -> Option<KeyKind> {
    let mut key = Der::whole(der, SEQUENCE)?;
    key.next(INTEGER)?;
    let modulus = key.next(INTEGER)?;
    // Its other seven integers must be there.
    for _ in 0..7 {
        key.next(INTEGER)?;
    }
    // The modulus's length in bits: its bytes without the sign's zero.
    let digits = modulus.get(modulus.iter().position(|b| *b != 0)?..)?;
    let top = digits.first()?;
    let bits = (digits.len() - 1) * 8 + (8 - usize::try_from(top.leading_zeros()).ok()?);
    Some(if bits < 2048 {
        KeyKind::NotTaken("an RSA key of fewer than 2048 bits")
    } else if bits > 4096 {
        KeyKind::NotTaken("an RSA key of more than 4096 bits")
    } else {
        KeyKind::Taken
    })
}

/// SEC 1: SEQUENCE { INTEGER 1, OCTET STRING privateKey, [0] OID curve
/// (optional), [1] publicKey (optional) }.
fn sec1_kind(der: &[u8]) -> Option<KeyKind> {
    let mut key = Der::whole(der, SEQUENCE)?;
    key.next(INTEGER)?;
    key.next(OCTET_STRING)?;
    if key.is_empty() {
        // No curve named: within PKCS #8 the algorithm names it.
        return Some(KeyKind::Taken);
    }
    match key.peek() {
        Some(CONTEXT_0) => {
            let curve = Der::whole(key.next(CONTEXT_0)?, OID)?.rest;
            Some(curve_kind(curve))
        }
        _ => Some(KeyKind::Taken),
    }
}

fn curve_kind(oid: &[u8]) -> KeyKind {
    match oid {
        OID_P256 | OID_P384 => KeyKind::Taken,
        OID_P521 => KeyKind::NotTaken("an ECDSA key on P-521"),
        OID_SECP256K1 => KeyKind::NotTaken("an ECDSA key on secp256k1"),
        _ => KeyKind::NotTaken("an ECDSA key on a curve that is not P-256 or P-384"),
    }
}

/// A reader of DER elements, one after the other, with no recursion: each
/// caller goes one level down by hand. It never indexes; a length past the
/// end, or a form DER does not allow, is `None`.
struct Der<'a> {
    rest: &'a [u8],
}

impl<'a> Der<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { rest: bytes }
    }

    /// The contents of the one element `tag` that is the whole of `bytes`.
    fn whole(bytes: &'a [u8], tag: u8) -> Option<Self> {
        let mut outer = Self::new(bytes);
        let inner = outer.next(tag)?;
        outer.is_empty().then(|| Self::new(inner))
    }

    fn is_empty(&self) -> bool {
        self.rest.is_empty()
    }

    fn peek(&self) -> Option<u8> {
        self.rest.first().copied()
    }

    /// The contents of the next element, which must be of `tag`.
    fn next(&mut self, tag: u8) -> Option<&'a [u8]> {
        let (&found, rest) = self.rest.split_first()?;
        if found != tag {
            return None;
        }
        let (&first, rest) = rest.split_first()?;
        let (length, rest) = if first < 0x80 {
            (usize::from(first), rest)
        } else {
            // Long form: one to four bytes of length (never indefinite).
            let count = usize::from(first & 0x7f);
            if count == 0 || count > 4 {
                return None;
            }
            let (bytes, rest) = rest.split_at_checked(count)?;
            let length = bytes
                .iter()
                .fold(0_usize, |n, b| (n << 8) | usize::from(*b));
            (length, rest)
        };
        let (contents, rest) = rest.split_at_checked(length)?;
        self.rest = rest;
        Some(contents)
    }
}

/// The cryptography of the program's TLS: ring's.
fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Lower-case hex.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(text, "{byte:02x}").ok();
    }
    text
}

/// Seconds since the Unix epoch as RFC 3339 in UTC (`2026-11-19T10:00:00Z`).
fn rfc3339(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let of_day = seconds.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        of_day / 3600,
        of_day % 3600 / 60,
        of_day % 60
    )
}

/// The pair new connections are given. The lock is taken only to clone or
/// to replace the `Arc`, never across an await.
pub struct InUse {
    current: RwLock<Arc<CertifiedKey>>,
}

impl fmt::Debug for InUse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Nothing of the key, nor of the certificate.
        f.write_str("InUse")
    }
}

impl InUse {
    pub fn new(loaded: &Loaded) -> Arc<Self> {
        Arc::new(Self {
            current: RwLock::new(Arc::clone(&loaded.certified)),
        })
    }

    fn get(&self) -> Arc<CertifiedKey> {
        Arc::clone(&self.current.read().unwrap_or_else(PoisonError::into_inner))
    }

    fn put(&self, loaded: &Loaded) {
        *self.current.write().unwrap_or_else(PoisonError::into_inner) =
            Arc::clone(&loaded.certified);
    }
}

impl ResolvesServerCert for InUse {
    fn resolve(&self, _: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        // The same certificate whatever server name is asked for.
        Some(self.get())
    }
}

/// The server configuration, built once: TLS 1.3 and 1.2, ALPN `http/1.1`
/// alone, no client certificate, the certificate of `in_use`.
pub fn server_config(in_use: &Arc<InUse>) -> Result<Arc<ServerConfig>, String> {
    let resolver: Arc<dyn ResolvesServerCert> = Arc::<InUse>::clone(in_use);
    let mut config = ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(|e| format!("TLS cannot be set up ({e})"))?
        .with_no_client_auth()
        .with_cert_resolver(resolver);
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// What one look at the two files found.
enum Look {
    /// What they hold is what the last look found.
    Same,
    /// They hold something else: the pair, or why it is not taken.
    Changed {
        digest: [u8; 32],
        outcome: Result<Loaded, String>,
    },
}

/// One look, on a blocking thread: the files read, and checked when what
/// they hold is not `seen`.
fn look(files: &Files, domain: &str, seen: [u8; 32]) -> Look {
    let (digest, outcome) = match read_both(files) {
        Ok(contents) => {
            let digest = contents.digest();
            if digest == seen {
                return Look::Same;
            }
            (digest, check(files, domain, &contents))
        }
        Err(message) => {
            // A file that cannot be read is said once for that reason.
            let digest: [u8; 32] = Sha256::digest(message.as_bytes()).into();
            if digest == seen {
                return Look::Same;
            }
            (digest, Err(message))
        }
    };
    Look::Changed { digest, outcome }
}

/// Looks at the two files every [`LOOK_EVERY`] until dropped (its owner
/// holds its task's handle and ends it): a changed pair that passes is put
/// in use and logged; one that does not is logged once for that content.
/// `first` is the pair loaded at start, in use already.
pub async fn watch(files: Files, domain: Arc<str>, in_use: Arc<InUse>, first: [u8; 32]) {
    let files = Arc::new(files);
    // What the files held at the last look, and what the pair in use was
    // made of.
    let mut seen = first;
    let mut in_use_digest = first;
    // The one look running (waited for at most READ_LIMIT at a time; one
    // still running then is waited for again at the next tick, and no
    // second is started beside it).
    let mut looking: Option<JoinHandle<Look>> = None;
    let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + LOOK_EVERY, LOOK_EVERY);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        let handle = looking.get_or_insert_with(|| {
            let files = Arc::clone(&files);
            let domain = Arc::clone(&domain);
            tokio::task::spawn_blocking(move || look(&files, &domain, seen))
        });
        let Ok(joined) = tokio::time::timeout(READ_LIMIT, handle).await else {
            continue;
        };
        looking = None;
        // A look that ended without an answer is made again at the next tick.
        let Ok(Look::Changed { digest, outcome }) = joined else {
            continue;
        };
        seen = digest;
        if digest == in_use_digest {
            // Back to what is in use.
            continue;
        }
        match outcome {
            Ok(loaded) => {
                in_use.put(&loaded);
                in_use_digest = digest;
                loaded.log();
            }
            Err(reason) => {
                log::record("tls-certificate-refused", json!({ "reason": reason }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::rfc3339;

    #[test]
    fn rfc3339_of_known_instants() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_795_000_000), "2026-11-18T11:06:40Z");
        assert_eq!(rfc3339(-1), "1969-12-31T23:59:59Z");
    }
}
