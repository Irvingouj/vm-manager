//! Postgres downloaded once into the user's own folder (blackbox/CONTRACT.md,
//! "The local loop: What a run does", step 1): zonky's build for this
//! machine, from Maven Central (`https://repo1.maven.org/maven2`) or the
//! mirror `VIBE_LOCAL_POSTGRES_MIRROR` names, checked against the SHA-256 this
//! program carries before anything of it is unpacked, and unpacked beside its
//! final place and renamed.
//!
//! **Fetching.** HTTP/1.1 through hyper's client connection, over TLS
//! (rustls, with ring; the authorities of crate::trust: Mozilla's roots
//! compiled in and those of `VIBE_CA_FILE`) for `https`; plain
//! for an `http` mirror. Bounds: [`CONNECT_TIMEOUT`] to resolve and connect,
//! [`READ_TIMEOUT`] for the answer's head and for each piece of the body,
//! [`DOWNLOAD_TIMEOUT`] for all of it; at most [`MAX_REDIRECTS`] redirects,
//! each to the scheme the base address has or to `https`; the body is
//! counted as it comes and the download stops at [`MAX_ARCHIVE_BYTES`],
//! whatever `Content-Length` says. It is written to a file of the folder as
//! it comes (hashed on the way), so memory holds one piece at a time.
//!
//! **Unpacking.** On a blocking thread: the jar (a zip) holds one `.txz`,
//! read through xz into tar, entry by entry. Every entry's path must stay
//! inside the folder it is unpacked into (no `..`, no absolute path), a link
//! must point inside it too, and only files, folders and links are taken:
//! anything else refuses the whole archive. At most [`MAX_ENTRIES`] entries
//! and [`MAX_UNPACKED_BYTES`] in all. A command that ends meanwhile sets a
//! flag the thread looks at between pieces.
//!
//! What is left half done (the downloaded file, the half-unpacked folder) is
//! removed whatever way the download ends ([`Leftover`]); the half-unpacked
//! folder is removed by whichever of the command and the unpacking thread
//! is last to let go of it, so never while the thread still writes in it.
//! What a command killed outright left (it removed nothing) is removed by
//! the next download once it is [`STALE_AFTER`] old ([`remove_stale`]): a
//! download of its own ends within [`DOWNLOAD_TIMEOUT`] and its unpacking
//! within [`UNPACK_TIMEOUT`], so anything older is no command's at work.

use std::fs::{self, DirBuilder, File};
use std::io::Read;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::header::{CONTENT_LENGTH, HOST, LOCATION, USER_AGENT};
use hyper::{Request, StatusCode, Uri};
use hyper_util::rt::TokioIo;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

/// The version downloaded.
pub(super) const VERSION: &str = "18.6.0";

/// Where it is downloaded from without `VIBE_LOCAL_POSTGRES_MIRROR`.
pub(super) const MAVEN_CENTRAL: &str = "https://repo1.maven.org/maven2";

/// zonky's builds of [`VERSION`] this program has a checksum for: Rust's
/// `(OS, ARCH)`, zonky's name of the platform, and the SHA-256 of the jar,
/// as Maven Central publishes it beside the jar (`<jar>.sha256`).
const ARCHIVES: [(&str, &str, &str, &str); 4] = [
    (
        "linux",
        "x86_64",
        "linux-amd64",
        "008ee189eaa7ca2b58bd01dd950291e638b998392cf982297acd4779ec898ac1",
    ),
    (
        "linux",
        "aarch64",
        "linux-arm64v8",
        "5f54e016723afa6213925a8acb939bac52944b1d348f65c7a20f0cd2928c31be",
    ),
    (
        "macos",
        "x86_64",
        "darwin-amd64",
        "690941008aac9ee360820fde9bb7870a4f3dcac5e9e2f9e45e42333a436ad0f9",
    ),
    (
        "macos",
        "aarch64",
        "darwin-arm64v8",
        "8f6ed6f38195b50a40712ada8b2a12060b6193c1a6977a6eb5cff053bb5a1b89",
    ),
];

/// The largest archive taken; the download stops there.
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;

/// The most a download may take, all of it.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// The longest resolving the name and connecting (TLS included) may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// The longest the answer's head, or any one piece of its body, may take.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// The longest unpacking may take; the thread looks at it between pieces.
const UNPACK_TIMEOUT: Duration = Duration::from_secs(4 * 60);

/// The age past which what a download left is no command's at work: above
/// [`DOWNLOAD_TIMEOUT`] and [`UNPACK_TIMEOUT`] together.
const STALE_AFTER: Duration = Duration::from_secs(15 * 60);

/// Redirects followed.
const MAX_REDIRECTS: usize = 5;

/// What may be unpacked, in all (zonky's largest build is about 200 MiB
/// unpacked).
const MAX_UNPACKED_BYTES: u64 = 1024 * 1024 * 1024;

/// Entries an archive may have (zonky's have about a thousand).
const MAX_ENTRIES: usize = 20_000;

/// zonky's build for this machine: its platform's name and checksum.
pub(super) struct Archive {
    pub platform: &'static str,
    sha256: &'static str,
}

impl Archive {
    /// The build for this machine, if this program knows one.
    pub(super) fn for_this_machine() -> Option<Self> {
        ARCHIVES
            .iter()
            .find(|(os, arch, _, _)| *os == std::env::consts::OS && *arch == std::env::consts::ARCH)
            .map(|(_, _, platform, sha256)| Self { platform, sha256 })
    }

    /// The folder of the folder downloaded into that holds it once
    /// unpacked.
    pub(super) fn folder_name(&self) -> String {
        format!("{VERSION}-{}", self.platform)
    }

    /// The jar's path below a Maven repository's base address.
    pub(super) fn maven_path(&self) -> String {
        let artifact = format!("embedded-postgres-binaries-{}", self.platform);
        format!("io/zonky/test/postgres/{artifact}/{VERSION}/{artifact}-{VERSION}.jar")
    }
}

/// Removes, when dropped, what a download left half done; and tells the
/// unpacking thread to stop.
struct Leftover {
    jar: PathBuf,
    unpacking: Arc<Unpacking>,
}

/// The half-unpacked folder, shared by the command and the unpacking
/// thread: whichever lets go of it last removes it (unless it was renamed
/// into its place). Shared as an `Arc`.
struct Unpacking {
    folder: PathBuf,
    /// Set by the command when it lets go.
    stop: AtomicBool,
    /// Whether the thread has let go (or was never started). Held only
    /// while it is read and set, and while the folder is removed.
    thread_done: std::sync::Mutex<bool>,
}

impl Unpacking {
    /// The thread lets go: it removes the folder if the command already
    /// has.
    fn thread_ends(&self) {
        let mut done = self
            .thread_done
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.stop.load(Ordering::SeqCst) {
            fs::remove_dir_all(&self.folder).ok();
        }
        *done = true;
    }

    /// The command lets go: it removes the folder if the thread already
    /// has (else the thread will, when it ends).
    fn command_ends(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let done = self
            .thread_done
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *done {
            fs::remove_dir_all(&self.folder).ok();
        }
    }
}

impl Drop for Leftover {
    fn drop(&mut self) {
        // Quick calls on files of this run's own; once per download.
        fs::remove_file(&self.jar).ok();
        self.unpacking.command_ends();
    }
}

/// Removes what downloads killed outright left in `folder`: each
/// `.download-*` and `.unpacking-*` older than [`STALE_AFTER`].
fn remove_stale(folder: &Path) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(".download-") && !name.starts_with(".unpacking-") {
            continue;
        }
        let Ok(meta) = entry.path().symlink_metadata() else {
            continue;
        };
        let old = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age > STALE_AFTER);
        if !old {
            continue;
        }
        if meta.is_dir() {
            fs::remove_dir_all(entry.path()).ok();
        } else {
            fs::remove_file(entry.path()).ok();
        }
    }
}

/// Downloads `archive` from `base` (a repository's base address) into
/// `folder` (made with mode `0700` if it is not there), and answers the
/// folder its programs are in. The error says what failed, in words.
pub(super) async fn download(
    archive: &Archive,
    base: &str,
    folder: &Path,
) -> Result<PathBuf, String> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(folder)
        .map_err(|e| format!("{} cannot be made ({e})", folder.display()))?;
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).map_err(|e| format!("no randomness for a file name ({e})"))?;
    let tag: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let jar = folder.join(format!(".download-{tag}.jar"));
    let unpacking = folder.join(format!(".unpacking-{tag}"));
    // Quick: a few entries of the folder.
    remove_stale(folder);
    let shared = Arc::new(Unpacking {
        // Paths copied once per download, not on a request's path.
        folder: unpacking.clone(),
        stop: AtomicBool::new(false),
        // No thread yet: the command alone removes the folder.
        thread_done: std::sync::Mutex::new(true),
    });
    let _leftover = Leftover {
        jar: jar.clone(),
        unpacking: Arc::clone(&shared),
    };
    let url = format!("{}/{}", base.trim_end_matches('/'), archive.maven_path());
    let fetched = tokio::time::timeout(DOWNLOAD_TIMEOUT, fetch(&url, &jar))
        .await
        .map_err(|_| {
            format!(
                "the download from {url} did not end within {} minutes",
                DOWNLOAD_TIMEOUT.as_secs() / 60
            )
        })??;
    if fetched != archive.sha256 {
        return Err(format!(
            "what {url} sent has the SHA-256 {fetched}, not the {} this program carries for it: it is not the archive this program runs, and nothing of it was unpacked",
            archive.sha256
        ));
    }
    let from = jar.clone();
    let thread = Arc::clone(&shared);
    *shared
        .thread_done
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
    let deadline = std::time::Instant::now() + UNPACK_TIMEOUT;
    tokio::task::spawn_blocking(move || {
        let unpacked = unpack(&from, &thread.folder, &thread.stop, deadline);
        thread.thread_ends();
        unpacked
    })
    .await
    .map_err(|_| "the archive could not be unpacked (the unpacking stopped)".to_owned())??;
    let bin = unpacking.join("bin");
    for program in ["postgres", "initdb", "pg_ctl"] {
        if !bin.join(program).is_file() {
            return Err(format!(
                "the archive from {url} has no bin/{program}: it is not a build of Postgres"
            ));
        }
    }
    let home = folder.join(archive.folder_name());
    if fs::rename(&unpacking, &home).is_err() && !home.join("bin").join("postgres").is_file() {
        return Err(format!("{} cannot be made", home.display()));
    }
    // Another run that unpacked it first left its own, as good as this
    // one's; this one's is removed with the leftovers.
    Ok(home.join("bin"))
}

/// Fetches `url` (following redirects) into the file `into`, and answers
/// the SHA-256 of what came, in lower-case hex.
async fn fetch(url: &str, into: &Path) -> Result<String, String> {
    let first: Uri = url
        .parse()
        .map_err(|_| format!("{url} is not an address the tool can download from"))?;
    let scheme = first.scheme_str().unwrap_or_default().to_owned();
    if scheme != "https" && scheme != "http" {
        return Err(format!("{url} is neither http:// nor https://"));
    }
    let mut at = first;
    for _ in 0..=MAX_REDIRECTS {
        match get(&at, into).await? {
            Got::Body(sha256) => return Ok(sha256),
            Got::Redirect(location) => {
                let next = resolve(&at, &location)
                    .ok_or_else(|| format!("{at} redirects to an address that cannot be read"))?;
                let to = next.scheme_str().unwrap_or_default();
                if to != "https" && to != scheme {
                    return Err(format!(
                        "{at} redirects to {next}, which is neither https:// nor the mirror's own scheme"
                    ));
                }
                at = next;
            }
        }
    }
    Err(format!("{url} redirects more than {MAX_REDIRECTS} times"))
}

/// What one `GET` came to.
enum Got {
    /// The body is in the file; its SHA-256.
    Body(String),
    Redirect(String),
}

/// A redirect's `Location`, absolute or relative to `from`.
fn resolve(from: &Uri, location: &str) -> Option<Uri> {
    if location.starts_with("https://") || location.starts_with("http://") {
        return location.parse().ok();
    }
    let path = location.starts_with('/').then_some(location)?;
    let authority = from.authority()?.as_str();
    format!("{}://{authority}{path}", from.scheme_str()?)
        .parse()
        .ok()
}

/// One `GET` of `uri`, its body into `into`.
async fn get(uri: &Uri, into: &Path) -> Result<Got, String> {
    let host = uri
        .host()
        .ok_or_else(|| format!("{uri} names no host"))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    let tls = uri.scheme_str() == Some("https");
    let port = uri.port_u16().unwrap_or(if tls { 443 } else { 80 });
    let connected =
        tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), port)))
            .await
            .map_err(|_| {
                format!(
                    "{uri} did not accept a connection within {} s",
                    CONNECT_TIMEOUT.as_secs()
                )
            })?
            .map_err(|e| format!("{uri} cannot be reached ({e})"))?;
    let path = uri.path_and_query().map_or("/", |p| p.as_str());
    let authority = uri.authority().map_or(host.as_str(), |a| a.as_str());
    let request = Request::get(path)
        .header(HOST, authority)
        .header(USER_AGENT, "vibe")
        .body(Empty::<Bytes>::new())
        .map_err(|_| format!("{uri} cannot be asked for"))?;
    if tls {
        let stream = tokio::time::timeout(CONNECT_TIMEOUT, tls_connect(&host, connected))
            .await
            .map_err(|_| {
                format!(
                    "{uri}: the TLS handshake did not end within {} s",
                    CONNECT_TIMEOUT.as_secs()
                )
            })??;
        exchange(TokioIo::new(stream), request, uri, into).await
    } else {
        exchange(TokioIo::new(connected), request, uri, into).await
    }
}

/// TLS over `stream`, the server checked against the authorities the
/// program trusts (crate::trust).
async fn tls_connect(
    host: &str,
    stream: TcpStream,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, String> {
    let config = crate::trust::client_config()?;
    let name = rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|_| format!("{host} is not a name TLS can check"))?;
    tokio_rustls::TlsConnector::from(config)
        .connect(name, stream)
        .await
        .map_err(|e| format!("the TLS handshake with {host} failed ({e})"))
}

/// The request on one connection, the body of a `200` written into `into`.
async fn exchange<T>(
    io: TokioIo<T>,
    request: Request<Empty<Bytes>>,
    uri: &Uri,
    into: &Path,
) -> Result<Got, String>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let failed = |e: hyper::Error| format!("the download from {uri} failed ({e})");
    let (mut sender, connection) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(failed)?;
    let answer = async {
        let response = tokio::time::timeout(READ_TIMEOUT, sender.send_request(request))
            .await
            .map_err(|_| format!("{uri} did not answer within {} s", READ_TIMEOUT.as_secs()))?
            .map_err(failed)?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| format!("{uri} answered {status} without a Location"))?;
            return Ok(Got::Redirect(location.to_owned()));
        }
        if status != StatusCode::OK {
            return Err(format!("{uri} answered {status}"));
        }
        let announced = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        let mut body = response.into_body();
        let mut file = tokio::fs::File::create(into)
            .await
            .map_err(|e| format!("{} cannot be written ({e})", into.display()))?;
        let mut hash = Sha256::new();
        let mut total: u64 = 0;
        loop {
            let frame = tokio::time::timeout(READ_TIMEOUT, body.frame())
                .await
                .map_err(|_| format!("{uri} sent nothing for {} s", READ_TIMEOUT.as_secs()))?;
            let Some(frame) = frame else { break };
            let frame =
                frame.map_err(|e| format!("the download from {uri} was cut short ({e})"))?;
            let Ok(data) = frame.into_data() else {
                continue;
            };
            total = total.saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
            if total > MAX_ARCHIVE_BYTES {
                return Err(format!(
                    "{uri} sends more than the {} MiB an archive of Postgres may be",
                    MAX_ARCHIVE_BYTES / (1024 * 1024)
                ));
            }
            hash.update(&data);
            file.write_all(&data)
                .await
                .map_err(|e| format!("{} cannot be written ({e})", into.display()))?;
        }
        file.flush()
            .await
            .map_err(|e| format!("{} cannot be written ({e})", into.display()))?;
        if announced.is_some_and(|n| n != total) {
            return Err(format!("the download from {uri} was cut short"));
        }
        Ok(Got::Body(
            hash.finalize().iter().map(|b| format!("{b:02x}")).collect(),
        ))
    };
    tokio::pin!(connection);
    tokio::pin!(answer);
    let mut connection_over = false;
    loop {
        tokio::select! {
            got = &mut answer => return got,
            _ = &mut connection, if !connection_over => connection_over = true,
        }
    }
}

/// Unpacks the jar `jar` into `into` (see the module's text).
fn unpack(
    jar: &Path,
    into: &Path,
    stop: &AtomicBool,
    deadline: std::time::Instant,
) -> Result<(), String> {
    let refused = |why: &str| format!("the archive is refused: {why}");
    let file = File::open(jar).map_err(|e| format!("{} cannot be read ({e})", jar.display()))?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| refused(&format!("it is not a jar ({e})")))?;
    let name = zip
        .file_names()
        .filter_map(Result::ok)
        .find(|n| n.ends_with(".txz"))
        .map(|n| n.into_owned())
        .ok_or_else(|| refused("it holds no .txz"))?;
    let txz = zip
        .by_name(&name)
        .map_err(|e| refused(&format!("{name} cannot be read ({e})")))?;
    DirBuilder::new()
        .mode(0o700)
        .create(into)
        .map_err(|e| format!("{} cannot be made ({e})", into.display()))?;
    // Bounds the bytes that come out of xz, whatever the headers say.
    let xz = liblzma::read::XzDecoder::new(txz).take(MAX_UNPACKED_BYTES);
    let mut tar = tar::Archive::new(xz);
    let entries = tar
        .entries()
        .map_err(|e| refused(&format!("its tar cannot be read ({e})")))?;
    let mut count = 0usize;
    let mut total: u64 = 0;
    for entry in entries {
        if stop.load(Ordering::SeqCst) {
            return Err("the unpacking was stopped".to_owned());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "the unpacking did not end within {} minutes",
                UNPACK_TIMEOUT.as_secs() / 60
            ));
        }
        let mut entry = entry.map_err(|e| refused(&format!("its tar cannot be read ({e})")))?;
        count = count.saturating_add(1);
        total = total.saturating_add(entry.header().size().unwrap_or(u64::MAX));
        if count > MAX_ENTRIES || total > MAX_UNPACKED_BYTES {
            return Err(refused("it unpacks to more than this program takes"));
        }
        let path = entry
            .path()
            .map_err(|e| refused(&format!("an entry's name cannot be read ({e})")))?
            .into_owned();
        let Some(depth) = depth_of(&path) else {
            return Err(refused(&format!("{} leaves the folder", path.display())));
        };
        let kind = entry.header().entry_type();
        if kind.is_symlink() || kind.is_hard_link() {
            let target = entry
                .link_name()
                .map_err(|e| refused(&format!("a link cannot be read ({e})")))?
                .ok_or_else(|| refused("a link without a target"))?
                .into_owned();
            // A symbolic link is read from its own folder; a hard link
            // names an entry of the archive.
            let inside = if kind.is_symlink() {
                stays_inside(depth.saturating_sub(1), &target)
            } else {
                depth_of(&target).is_some()
            };
            if !inside {
                return Err(refused(&format!(
                    "the link {} points outside the folder",
                    path.display()
                )));
            }
        } else if !kind.is_file() && !kind.is_dir() {
            return Err(refused(&format!(
                "{} is neither a file, a folder nor a link",
                path.display()
            )));
        }
        entry
            .unpack_in(into)
            .map_err(|e| format!("{} cannot be unpacked ({e})", path.display()))?;
    }
    Ok(())
}

/// How many folders deep `path` is, if it is relative and never goes up.
fn depth_of(path: &Path) -> Option<usize> {
    let mut depth = 0usize;
    for component in path.components() {
        match component {
            Component::Normal(_) => depth = depth.saturating_add(1),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(depth)
}

/// Whether `target`, read from a folder `depth` folders deep, stays inside.
fn stays_inside(depth: usize, target: &Path) -> bool {
    let mut at = depth;
    for component in target.components() {
        match component {
            Component::Normal(_) => at = at.saturating_add(1),
            Component::CurDir => {}
            Component::ParentDir => match at.checked_sub(1) {
                Some(up) => at = up,
                None => return false,
            },
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}
