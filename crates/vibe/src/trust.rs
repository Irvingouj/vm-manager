//! The certificate authorities the program trusts for the HTTPS connections
//! it makes itself (blackbox/CONTRACT.md, "The tool reaches an installation
//! over HTTPS: A company's own certificate authority"): the public ones
//! built into it (webpki-roots) and those of the PEM file `VIBE_CA_FILE`
//! names, beside them.
//!
//! [`client_config`] is the one place the trust store is built, for all
//! three that use it: the tool to the installation
//! (crate::tool::client), the platform to the identity provider
//! (crate::oidc::http), the local run's download of Postgres. It is built
//! once in a process, the first time it is asked for, and kept: `vibe serve`
//! and `vibe setup` ask for it at start, so a file that cannot be used is a
//! start-up failure and a changed file counts from the next start.
//!
//! **Bounds.** The file is opened, looked at (a regular file, at most
//! [`MAX_FILE_BYTES`]) and read through a `take` of one byte more than the
//! bound, so no more than that is ever read. Only `CERTIFICATE` sections
//! are taken from it; a private key in it is skipped, never read as a key.
//!
//! The file is not a secret: its path may be in a message.

use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::Path;
use std::sync::{Arc, OnceLock};

use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use rustls::{CertificateError, ClientConfig, RootCertStore};

/// The variable that names the file.
pub const VARIABLE: &str = "VIBE_CA_FILE";

/// The largest file taken.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// What was built, once in a process.
static BUILT: OnceLock<Result<Arc<ClientConfig>, String>> = OnceLock::new();

/// The TLS client configuration of every HTTPS connection the program
/// makes: ring, TLS 1.2 or later, the built-in authorities and those of
/// `VIBE_CA_FILE`. The error is a message for a person that names
/// `VIBE_CA_FILE` and the path.
pub fn client_config() -> Result<Arc<ClientConfig>, String> {
    match BUILT.get_or_init(build) {
        Ok(config) => Ok(Arc::clone(config)),
        Err(message) => Err(message.clone()),
    }
}

fn build() -> Result<Arc<ClientConfig>, String> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(path) = std::env::var_os(VARIABLE).filter(|v| !v.is_empty()) {
        let path = Path::new(&path);
        let problem = |what: &str| {
            format!(
                "{VARIABLE} names {}, which {what}: name a file of certificates in PEM form (at most 1 MiB), or unset {VARIABLE}",
                path.display()
            )
        };
        for certificate in read_certificates(path).map_err(|what| problem(&what))? {
            roots
                .add(certificate)
                .map_err(|e| problem(&format!("holds a certificate that cannot be used ({e})")))?;
        }
    }
    let config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| format!("TLS cannot be set up ({e})"))?
            .with_root_certificates(roots)
            .with_no_client_auth();
    Ok(Arc::new(config))
}

/// The certificates of the file at `path`; what is wrong with it otherwise.
fn read_certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>, String> {
    let file = File::open(path).map_err(|e| match e.kind() {
        ErrorKind::NotFound => "is not there".to_owned(),
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
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot be read ({e})"))?;
    if u64::try_from(bytes.len()).map_or(true, |n| n > MAX_FILE_BYTES) {
        return Err("is larger than 1 MiB".to_owned());
    }
    let mut certificates = Vec::new();
    for section in CertificateDer::pem_slice_iter(&bytes) {
        let certificate = section.map_err(|e| format!("is not PEM that can be read ({e})"))?;
        certificates.push(certificate);
    }
    if certificates.is_empty() {
        return Err("holds no certificate (a PEM section BEGIN CERTIFICATE)".to_owned());
    }
    Ok(certificates)
}

/// Why a server's certificate was not taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// It is not for the name asked for.
    OtherName,
    /// Its authority is none the program trusts.
    UnknownAuthority,
    /// Its time is over.
    Expired,
    /// Its time has not begun.
    NotValidYet,
    /// Anything else about the certificate.
    Other,
}

/// What a failed TLS handshake (`error`, as tokio-rustls gives it) says of
/// the server's certificate, read from rustls's own error by its type:
/// `None` when the handshake failed for another reason.
pub fn refusal(error: &std::io::Error) -> Option<Refusal> {
    let tls = error.get_ref()?.downcast_ref::<rustls::Error>()?;
    let rustls::Error::InvalidCertificate(certificate) = tls else {
        return None;
    };
    Some(match certificate {
        CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. } => {
            Refusal::OtherName
        }
        CertificateError::UnknownIssuer => Refusal::UnknownAuthority,
        CertificateError::Expired | CertificateError::ExpiredContext { .. } => Refusal::Expired,
        CertificateError::NotValidYet | CertificateError::NotValidYetContext { .. } => {
            Refusal::NotValidYet
        }
        _ => Refusal::Other,
    })
}
