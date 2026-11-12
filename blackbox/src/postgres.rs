//! The harness's private Postgres (CONTRACT.md, "How the tests get a
//! Postgres"), and the platform databases the programs under test get on it.
//!
//! The binaries come from the directory `VM_TEST_PG_BIN` names, else from a
//! Postgres found on the machine, else from zonky's prebuilt binaries on
//! Maven Central, downloaded once to a per-user cache and checked against
//! the SHA-256 pinned here and published beside them
//! (`docs/research/local-postgres-without-docker.md`). No Docker, no
//! administrator rights. `VM_TEST_PG_VERSION` (16, 17 or 18; default 18)
//! chooses the version: a Postgres on the machine is used only if it is
//! exactly that version (at least 18 when the variable is unset), and the
//! download is of that version.
//!
//! [`PrivatePostgres::start`] runs `initdb` into a fresh directory under the
//! system temp directory and starts `postgres` on a free 127.0.0.1 port, with
//! a superuser that logs in with a random password. One instance serves all
//! the tests of a test binary ([`shared`]); each test's program gets its own
//! platform database on it ([`Platform::fresh`]).
//!
//! Stopping: a test binary cannot run code when its last test is done, and
//! a `static` is never dropped. So every instance has a guardian, a small
//! `sh` process that waits for its standard input to close and then stops
//! the server and removes the directory. Its standard input is a pipe whose
//! only writer is the test process, so it closes when the test process
//! exits, however it exits: all tests done, a test panicking, Ctrl-C, or a
//! kill. An instance that is dropped stops itself at once.

use std::fmt;
use std::fs;
use std::io::{self, Cursor, Read};
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::program::run_to_exit;

/// The Postgres major version the harness runs when `VM_TEST_PG_VERSION`
/// does not say.
pub const DEFAULT_MAJOR: u32 = 18;

/// The major versions `VM_TEST_PG_VERSION` may choose.
pub const MAJORS: [u32; 3] = [16, 17, 18];

/// The Postgres major version the harness wants: `VM_TEST_PG_VERSION` (16,
/// 17 or 18), else [`DEFAULT_MAJOR`]. Any other value fails the test that
/// asks, saying what is allowed.
pub fn wanted_major() -> u32 {
    match chosen_major() {
        Ok(Some(major)) => major,
        Ok(None) => DEFAULT_MAJOR,
        Err(problem) => panic!("{problem}"),
    }
}

/// The version `VM_TEST_PG_VERSION` chooses, if it is set.
fn chosen_major() -> Result<Option<u32>, String> {
    let Some(value) = std::env::var_os("VM_TEST_PG_VERSION") else {
        return Ok(None);
    };
    let text = value.to_string_lossy();
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    text.parse::<u32>()
        .ok()
        .filter(|major| MAJORS.contains(major))
        .map(Some)
        .ok_or_else(|| {
            format!("VM_TEST_PG_VERSION is {text:?}: it must be one of {MAJORS:?}, or unset for {DEFAULT_MAJOR}")
        })
}

/// Per major version and platform: the major, the version of zonky's build
/// that is downloaded, Rust's `(OS, ARCH)`, zonky's name for the platform,
/// and the SHA-256 of its archive as published on Maven Central
/// (`<jar>.sha256`) when this was written. Pinned, so that an archive that
/// changes under the same version is refused rather than run.
const ARCHIVES: &[(u32, &str, &str, &str, &str, &str)] = &[
    (
        18,
        "18.6.0",
        "linux",
        "x86_64",
        "linux-amd64",
        "008ee189eaa7ca2b58bd01dd950291e638b998392cf982297acd4779ec898ac1",
    ),
    (
        18,
        "18.6.0",
        "linux",
        "aarch64",
        "linux-arm64v8",
        "5f54e016723afa6213925a8acb939bac52944b1d348f65c7a20f0cd2928c31be",
    ),
    (
        18,
        "18.6.0",
        "macos",
        "x86_64",
        "darwin-amd64",
        "690941008aac9ee360820fde9bb7870a4f3dcac5e9e2f9e45e42333a436ad0f9",
    ),
    (
        18,
        "18.6.0",
        "macos",
        "aarch64",
        "darwin-arm64v8",
        "8f6ed6f38195b50a40712ada8b2a12060b6193c1a6977a6eb5cff053bb5a1b89",
    ),
    (
        17,
        "17.11.0",
        "linux",
        "x86_64",
        "linux-amd64",
        "0dd7b72b6f335b8ecfb355fa24c5781e8a93edd09880bb77eb52ebbf29b3e96d",
    ),
    (
        17,
        "17.11.0",
        "linux",
        "aarch64",
        "linux-arm64v8",
        "8b042e0ea418b1927d95399207950da0734d27fe01e29503ade3bdc464ad4c2b",
    ),
    (
        17,
        "17.11.0",
        "macos",
        "x86_64",
        "darwin-amd64",
        "d464ff178e9860ba204662ac23fa547504b7fd392392ff2fb92e3fd73b5bdb64",
    ),
    (
        17,
        "17.11.0",
        "macos",
        "aarch64",
        "darwin-arm64v8",
        "a1c2786acb0c398f9b2d76806fc52f5dc8b222cbc8e9383a9b9702084daaf3a5",
    ),
    (
        16,
        "16.15.0",
        "linux",
        "x86_64",
        "linux-amd64",
        "653abc065c682b85d3da50168fb95dc524bd85426cdec9ce3695a550ef431df2",
    ),
    (
        16,
        "16.15.0",
        "linux",
        "aarch64",
        "linux-arm64v8",
        "f846a9989d686b7977d6eca9bc9d8b2b69e3f70c7eb60e032197d9c574a6136c",
    ),
    (
        16,
        "16.15.0",
        "macos",
        "x86_64",
        "darwin-amd64",
        "1229188b99515d2160d5cb6222bc96d9c5f3a248268df3bc2dfe0047e6c233d8",
    ),
    (
        16,
        "16.15.0",
        "macos",
        "aarch64",
        "darwin-arm64v8",
        "65b953905f0a4d46030767a6b44a52360170d69a7985f1b02aa435f2f6256a83",
    ),
];

/// How long the download of the archive may take, all of it.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// The largest archive accepted (zonky's largest, macOS, is 68 MiB).
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
/// How long `initdb` may take.
const INITDB_TIMEOUT: Duration = Duration::from_secs(120);
/// How long the server may take to say it is ready.
const READY_TIMEOUT: Duration = Duration::from_secs(60);
/// How long stopping the server may take.
const STOP_TIMEOUT: Duration = Duration::from_secs(15);
/// How long `postgres --version` may take.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
/// How many free ports are tried before starting is given up.
const PORT_ATTEMPTS: usize = 5;

/// The superuser the harness creates and hands to the program.
pub const SUPERUSER: &str = "vm_test";

/// Where the binaries came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The directory `VM_TEST_PG_BIN` names.
    Variable,
    /// A Postgres installed on the machine.
    OnTheMachine,
    /// The per-user cache the harness downloads to.
    Cache,
}

/// A directory holding `postgres`, `initdb` and `pg_ctl`.
#[derive(Debug)]
pub struct Binaries {
    pub bin: PathBuf,
    pub origin: Origin,
}

impl Binaries {
    pub fn postgres(&self) -> PathBuf {
        self.bin.join(exe("postgres"))
    }

    pub fn initdb(&self) -> PathBuf {
        self.bin.join(exe("initdb"))
    }

    pub fn pg_ctl(&self) -> PathBuf {
        self.bin.join(exe("pg_ctl"))
    }
}

fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

fn has_all_three(bin: &Path) -> bool {
    ["postgres", "initdb", "pg_ctl"]
        .iter()
        .all(|name| bin.join(exe(name)).is_file())
}

/// The Postgres binaries, found or downloaded once per test binary.
pub fn binaries() -> &'static Binaries {
    static BINARIES: OnceLock<Result<Binaries, String>> = OnceLock::new();
    match BINARIES.get_or_init(find_binaries) {
        Ok(binaries) => binaries,
        Err(problem) => panic!("cannot get Postgres binaries for the tests: {problem}"),
    }
}

fn find_binaries() -> Result<Binaries, String> {
    let chosen = chosen_major()?;
    if let Some(dir) = std::env::var_os("VM_TEST_PG_BIN") {
        let bin = PathBuf::from(dir);
        return if has_all_three(&bin) {
            Ok(Binaries {
                bin,
                origin: Origin::Variable,
            })
        } else {
            Err(format!(
                "VM_TEST_PG_BIN names {}, which does not hold postgres, initdb and pg_ctl",
                bin.display()
            ))
        };
    }
    if let Some(bin) = on_the_machine(chosen) {
        return Ok(Binaries {
            bin,
            origin: Origin::OnTheMachine,
        });
    }
    let bin = downloaded(chosen.unwrap_or(DEFAULT_MAJOR))?;
    Ok(Binaries {
        bin,
        origin: Origin::Cache,
    })
}

/// A directory on the machine with all three binaries: of exactly the
/// version `VM_TEST_PG_VERSION` chose, or, when it is unset, of a Postgres
/// at least [`DEFAULT_MAJOR`]. On `PATH`, or where packages and installers
/// put them.
fn on_the_machine(chosen: Option<u32>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    for pattern_dir in [
        "/usr/lib/postgresql",
        "/opt/homebrew/opt",
        "/usr/local/opt",
        "/Applications/Postgres.app/Contents/Versions",
    ] {
        if let Ok(entries) = fs::read_dir(pattern_dir) {
            for entry in entries.flatten() {
                candidates.push(entry.path().join("bin"));
            }
        }
    }
    candidates.push(PathBuf::from("/usr/local/pgsql/bin"));
    candidates
        .into_iter()
        .filter(|bin| has_all_three(bin))
        .find(|bin| {
            major_version(&bin.join(exe("postgres"))).is_some_and(|m| match chosen {
                Some(chosen) => m == chosen,
                None => m >= DEFAULT_MAJOR,
            })
        })
}

/// The major version `postgres --version` reports ("postgres (PostgreSQL) 18.1").
fn major_version(postgres: &Path) -> Option<u32> {
    let mut command = Command::new(postgres);
    command.arg("--version");
    let finished = run_to_exit(command, VERSION_TIMEOUT).ok()?;
    let words = finished.stdout;
    let version = words.split_whitespace().last()?;
    version.split('.').next()?.parse().ok()
}

/// The per-user cache directory the downloaded binaries are kept in.
pub fn cache_dir() -> Result<PathBuf, String> {
    let base = if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()) {
        PathBuf::from(xdg)
    } else if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        if cfg!(target_os = "macos") {
            PathBuf::from(home).join("Library").join("Caches")
        } else {
            PathBuf::from(home).join(".cache")
        }
    } else if let Some(local) = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) {
        PathBuf::from(local)
    } else {
        return Err("no per-user cache directory: set HOME, or set VM_TEST_PG_BIN".to_owned());
    };
    Ok(base.join("vm-blackbox"))
}

/// zonky's binaries of this major version for this platform, downloaded
/// and unpacked into the cache unless they are there already. Returns their
/// `bin` directory.
fn downloaded(major: u32) -> Result<PathBuf, String> {
    let (zonky_version, platform, sha256) = ARCHIVES
        .iter()
        .find(|(m, _, os, arch, _, _)| {
            *m == major && *os == std::env::consts::OS && *arch == std::env::consts::ARCH
        })
        .map(|(_, version, _, _, platform, sha256)| (*version, *platform, *sha256))
        .ok_or_else(|| {
            format!(
                "no prebuilt Postgres {major} is known for {} {}: install Postgres {major} or set VM_TEST_PG_BIN",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        })?;
    let cache = cache_dir()?;
    let home = cache.join(format!("postgres-{zonky_version}-{platform}"));
    // The directory is only ever created by a rename of a complete unpack,
    // so if it is there, it is whole.
    if has_all_three(&home.join("bin")) {
        return Ok(home.join("bin"));
    }
    fs::create_dir_all(&cache)
        .map_err(|e| format!("cannot create the cache {}: {e}", cache.display()))?;
    let artifact = format!("embedded-postgres-binaries-{platform}");
    let url = format!(
        "https://repo1.maven.org/maven2/io/zonky/test/postgres/{artifact}/{zonky_version}/{artifact}-{zonky_version}.jar"
    );
    eprintln!(
        "blackbox: downloading Postgres {zonky_version} once, from {url}, to {}",
        home.display()
    );
    let began = Instant::now();
    let published = fetch(&format!("{url}.sha256"), 1024)?;
    let published = String::from_utf8_lossy(&published);
    let published = published.split_whitespace().next().unwrap_or_default();
    if !published.eq_ignore_ascii_case(sha256) {
        return Err(format!(
            "the SHA-256 published for {url} is {published:?}, not the {sha256} pinned in the harness: refusing it"
        ));
    }
    let archive = fetch(&url, MAX_ARCHIVE_BYTES)?;
    eprintln!(
        "blackbox: downloaded {} bytes of Postgres {zonky_version} in {:?}",
        archive.len(),
        began.elapsed()
    );
    let actual = hex(&Sha256::digest(&archive));
    if actual != sha256 {
        return Err(format!(
            "the archive downloaded from {url} has SHA-256 {actual}, not the published {sha256}: refusing it"
        ));
    }
    let unpacking = cache.join(format!(
        ".postgres-{zonky_version}-{platform}.unpacking-{}-{}",
        std::process::id(),
        unique()
    ));
    let unpacked = unpack(&archive, &unpacking).and_then(|()| {
        if has_all_three(&unpacking.join("bin")) {
            Ok(())
        } else {
            Err(format!(
                "the archive from {url} holds no bin/postgres, bin/initdb and bin/pg_ctl"
            ))
        }
    });
    if let Err(problem) = unpacked {
        drop(fs::remove_dir_all(&unpacking));
        return Err(problem);
    }
    if fs::rename(&unpacking, &home).is_err() {
        // Another process unpacked it first; theirs is as good as ours.
        drop(fs::remove_dir_all(&unpacking));
    }
    if has_all_three(&home.join("bin")) {
        eprintln!(
            "blackbox: Postgres {zonky_version} downloaded and unpacked in {:?}",
            began.elapsed()
        );
        Ok(home.join("bin"))
    } else {
        Err(format!(
            "cannot put the unpacked Postgres at {}",
            home.display()
        ))
    }
}

fn fetch(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(DOWNLOAD_TIMEOUT))
        .build()
        .into();
    agent
        .get(url)
        .call()
        .map_err(|e| format!("cannot download {url}: {e}"))?
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|e| format!("cannot download {url}: {e}"))
}

/// Unpacks zonky's jar: a zip holding one `.txz`, a tar compressed with xz.
fn unpack(archive: &[u8], into: &Path) -> Result<(), String> {
    let mut jar = zip::ZipArchive::new(Cursor::new(archive))
        .map_err(|e| format!("the downloaded archive is not a zip: {e}"))?;
    let name = jar
        .file_names()
        .filter_map(Result::ok)
        .find(|name| name.ends_with(".txz"))
        .map(|name| name.into_owned())
        .ok_or("the downloaded archive holds no .txz")?;
    let entry = jar
        .by_name(&name)
        .map_err(|e| format!("cannot read {name} in the archive: {e}"))?;
    let mut txz = Vec::new();
    entry
        .take(MAX_ARCHIVE_BYTES)
        .read_to_end(&mut txz)
        .map_err(|e| format!("cannot read {name} in the archive: {e}"))?;
    fs::create_dir_all(into).map_err(|e| format!("cannot create {}: {e}", into.display()))?;
    let mut tar = tar::Archive::new(liblzma::read::XzDecoder::new(Cursor::new(txz)));
    tar.set_preserve_permissions(true);
    tar.unpack(into)
        .map_err(|e| format!("cannot unpack {name} into {}: {e}", into.display()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A number no other call in this process returns, mixed with the clock so
/// that names differ between processes too.
fn unique() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    format!("{n}-{nanos}")
}

/// The superuser's password: random per instance, and never printed by the
/// harness (AGENTS.md, Rust rule 9). Tests read it with [`Password::expose`]
/// to check that the program never prints it either.
pub struct Password(String);

impl Password {
    fn random() -> Self {
        use std::hash::{BuildHasher, Hasher};
        let mut text = String::from("pw");
        for round in 0..3_u64 {
            let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
            hasher.write_u64(round);
            hasher.write(unique().as_bytes());
            hasher.write_u32(std::process::id());
            text.push_str(&format!("{:016x}", hasher.finish()));
        }
        Self(text)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(<not shown>)")
    }
}

/// The process that stops the server and removes its directory once the
/// test process is gone (see the module's documentation).
struct Guardian {
    /// Closing this tells the guardian to clean up.
    stdin: Option<ChildStdin>,
    child: Child,
}

/// The guardian's script: ignore the signals a Ctrl-C or a group kill sends,
/// wait for standard input to close, then stop the server and remove the
/// directory. Paths are passed as arguments, never pasted into the script.
const GUARDIAN_SCRIPT: &str = r#"trap '' INT TERM HUP
while read -r _ignored; do :; done
"$1" stop -D "$2" -m immediate -w -t 15 >/dev/null 2>&1
rm -rf -- "$3""#;

impl Guardian {
    fn spawn(pg_ctl: &Path, data: &Path, root: &Path) -> Result<Self, String> {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(GUARDIAN_SCRIPT)
            .arg("vm-blackbox-postgres-guardian")
            .arg(pg_ctl)
            .arg(data)
            .arg(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start the guardian of the test Postgres (sh): {e}"))?;
        let stdin = child.stdin.take();
        Ok(Self { stdin, child })
    }

    /// Tells the guardian to clean up now, and waits for it a bounded time.
    fn release(&mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + STOP_TIMEOUT;
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        drop(self.child.kill());
        drop(self.child.wait());
    }
}

/// A Postgres server of the harness's own.
pub struct PrivatePostgres {
    root: PathBuf,
    data: PathBuf,
    port: u16,
    /// A second loopback address it also listens on, if any.
    also_on: Option<Ipv4Addr>,
    password: Password,
    binaries: &'static Binaries,
    server: Option<Child>,
    guardian: Option<Guardian>,
    /// How long `initdb` and the start took, for the report.
    pub initdb_took: Duration,
    pub start_took: Duration,
}

impl fmt::Debug for PrivatePostgres {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrivatePostgres")
            .field("root", &self.root)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl PrivatePostgres {
    /// Runs `initdb` into a new directory under the system temp directory
    /// and starts the server on a free 127.0.0.1 port. It listens on no
    /// Unix socket and only on loopback.
    pub fn start() -> Result<Self, String> {
        Self::start_also_on(None)
    }

    /// As [`PrivatePostgres::start`], listening also on this second loopback
    /// address (on Linux all of 127.0.0.0/8 is loopback).
    pub fn start_also_on(also_on: Option<Ipv4Addr>) -> Result<Self, String> {
        let binaries = binaries();
        let root = std::env::temp_dir().join(format!(
            "vm-blackbox-pg-{}-{}",
            std::process::id(),
            unique()
        ));
        create_private_dir(&root)?;
        let data = root.join("data");
        let mut postgres = Self {
            root,
            data,
            port: 0,
            also_on,
            password: Password::random(),
            binaries,
            server: None,
            guardian: None,
            initdb_took: Duration::ZERO,
            start_took: Duration::ZERO,
        };
        // From here on, dropping `postgres` cleans up whatever was made.
        postgres.guardian = Some(Guardian::spawn(
            &binaries.pg_ctl(),
            &postgres.data,
            &postgres.root,
        )?);
        postgres.initdb()?;
        postgres.serve()?;
        Ok(postgres)
    }

    fn initdb(&mut self) -> Result<(), String> {
        let password_file = self.root.join("superuser-password");
        write_private_file(&password_file, self.password.expose())?;
        let began = Instant::now();
        let mut command = Command::new(self.binaries.initdb());
        command
            .arg("-D")
            .arg(&self.data)
            .arg("-U")
            .arg(SUPERUSER)
            .arg(format!("--pwfile={}", password_file.display()))
            .args([
                "-A",
                "scram-sha-256",
                "-E",
                "UTF8",
                "--no-locale",
                "--no-sync",
            ]);
        let finished = run_to_exit(command, INITDB_TIMEOUT)?;
        drop(fs::remove_file(&password_file));
        self.initdb_took = began.elapsed();
        if finished.status.is_some_and(|s| s.success()) {
            Ok(())
        } else {
            Err(format!(
                "initdb failed ({}):\n{}\n{}",
                finished.status_text(),
                finished.stdout,
                finished.stderr
            ))
        }
    }

    fn serve(&mut self) -> Result<(), String> {
        let log_path = self.root.join("postgres.log");
        let began = Instant::now();
        let mut last_problem = String::new();
        for _ in 0..PORT_ATTEMPTS {
            let port = free_port()?;
            let log = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .map_err(|e| format!("cannot open {}: {e}", log_path.display()))?;
            let log_too = log
                .try_clone()
                .map_err(|e| format!("cannot open {}: {e}", log_path.display()))?;
            let listen = match self.also_on {
                Some(other) => format!("listen_addresses=127.0.0.1,{other}"),
                None => "listen_addresses=127.0.0.1".to_owned(),
            };
            let server = Command::new(self.binaries.postgres())
                .arg("-D")
                .arg(&self.data)
                .arg("-p")
                .arg(port.to_string())
                .arg("-c")
                .arg(listen)
                .args([
                    // Each line of the server's log says which database and
                    // role it is about, so that a test can read what one
                    // database received (statements are logged only for
                    // databases that ask for it: see `shared_database`).
                    "-c",
                    "log_line_prefix=%m [%p] db=%d user=%u: ",
                    "-c",
                    "unix_socket_directories=",
                    "-c",
                    "max_connections=500",
                    "-c",
                    "shared_buffers=64MB",
                    // A throwaway cluster: speed over durability.
                    "-c",
                    "fsync=off",
                    "-c",
                    "synchronous_commit=off",
                    "-c",
                    "full_page_writes=off",
                ])
                .stdin(Stdio::null())
                .stdout(log)
                .stderr(log_too)
                .spawn()
                .map_err(|e| format!("cannot start {}: {e}", self.binaries.postgres().display()))?;
            self.server = Some(server);
            match self.wait_until_ready() {
                Ok(()) => {
                    self.port = port;
                    self.start_took = began.elapsed();
                    return Ok(());
                }
                Err(problem) => {
                    self.stop_server();
                    let log = fs::read_to_string(&log_path).unwrap_or_default();
                    last_problem = format!("{problem}\nits log:\n{log}");
                    if !log.contains("could not bind") && !log.contains("in use") {
                        break;
                    }
                }
            }
        }
        Err(format!("cannot start the test Postgres: {last_problem}"))
    }

    /// Waits until `postmaster.pid` says the server is ready.
    fn wait_until_ready(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + READY_TIMEOUT;
        let pid_file = self.data.join("postmaster.pid");
        loop {
            let status = fs::read_to_string(&pid_file).unwrap_or_default();
            if status.lines().nth(7).is_some_and(|l| l.trim() == "ready") {
                return Ok(());
            }
            if let Some(server) = self.server.as_mut()
                && let Ok(Some(exit)) = server.try_wait()
            {
                return Err(format!("postgres exited ({exit}) before it was ready"));
            }
            if Instant::now() >= deadline {
                return Err(format!("postgres was not ready within {READY_TIMEOUT:?}"));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Stops the server now, as abruptly as a crash (`pg_ctl stop -m
    /// immediate`): to show what the program does when its Postgres goes
    /// away.
    pub fn stop(&mut self) {
        self.stop_server();
    }

    fn stop_server(&mut self) {
        let Some(mut server) = self.server.take() else {
            return;
        };
        let mut command = Command::new(self.binaries.pg_ctl());
        command
            .arg("stop")
            .arg("-D")
            .arg(&self.data)
            .args(["-m", "immediate", "-w", "-t", "15"]);
        drop(run_to_exit(command, STOP_TIMEOUT));
        let deadline = Instant::now() + STOP_TIMEOUT;
        while Instant::now() < deadline {
            if let Ok(Some(_)) = server.try_wait() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        drop(server.kill());
        drop(server.wait());
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The address a client connects to: the second loopback address when
    /// there is one, else 127.0.0.1.
    pub fn host(&self) -> String {
        self.also_on
            .map_or_else(|| "127.0.0.1".to_owned(), |a| a.to_string())
    }

    /// Everything the server has written to its log so far.
    pub fn server_log(&self) -> String {
        let path = self.root.join("postgres.log");
        let bytes = fs::read(&path).unwrap_or_default();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    pub fn password(&self) -> &Password {
        &self.password
    }

    pub fn binaries(&self) -> &'static Binaries {
        self.binaries
    }

    /// The directory under the system temp directory that holds the data.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The superuser's connection address to `database`.
    pub fn url(&self, database: &str) -> String {
        self.url_as(SUPERUSER, self.password.expose(), database)
    }

    /// A connection address to `database` as `role` with `password`.
    pub fn url_as(&self, role: &str, password: &str, database: &str) -> String {
        format!(
            "postgres://{role}:{password}@{}:{}/{database}",
            self.host(),
            self.port
        )
    }

    /// Whether the server is still running.
    pub fn is_running(&mut self) -> bool {
        self.server
            .as_mut()
            .is_some_and(|s| matches!(s.try_wait(), Ok(None)))
    }
}

impl Drop for PrivatePostgres {
    fn drop(&mut self) {
        self.stop_server();
        drop(fs::remove_dir_all(&self.root));
        if let Some(mut guardian) = self.guardian.take() {
            guardian.release();
        }
    }
}

fn create_private_dir(path: &Path) -> Result<(), String> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))
}

fn write_private_file(path: &Path, content: &str) -> Result<(), String> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    io::Write::write_all(&mut file, content.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// A 127.0.0.1 port nothing listens on at this moment.
pub fn free_port() -> Result<u16, String> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .map_err(|e| format!("cannot find a free port: {e}"))
}

/// The instance all tests of this test binary share. It is never dropped;
/// its guardian stops it when the test process exits.
pub fn shared() -> &'static PrivatePostgres {
    match SHARED.get_or_init(PrivatePostgres::start) {
        Ok(postgres) => postgres,
        Err(problem) => panic!("{problem}"),
    }
}

static SHARED: OnceLock<Result<PrivatePostgres, String>> = OnceLock::new();

/// The instance of [`shared`] if it has been started, without starting
/// it: for failure messages.
pub fn shared_if_started() -> Option<&'static PrivatePostgres> {
    SHARED.get().and_then(|started| started.as_ref().ok())
}

/// The last `lines` lines of the shared instance's server log, or a note
/// saying why there are none: for failure messages.
pub fn shared_log_tail(lines: usize) -> String {
    let Some(postgres) = shared_if_started() else {
        return "(the test Postgres was not started)\n".to_owned();
    };
    let log = postgres.server_log();
    let all: Vec<&str> = log.lines().collect();
    let from = all.len().saturating_sub(lines);
    let mut tail = String::new();
    for line in all.get(from..).unwrap_or_default() {
        tail.push_str(line);
        tail.push('\n');
    }
    if tail.is_empty() {
        tail.push_str("(empty)\n");
    }
    tail
}

/// A second loopback address that is not 127.0.0.1, if this machine has one
/// (Linux routes all of 127.0.0.0/8 to loopback; macOS by default does not).
/// The instance shared databases live on listens there too, so that their
/// host is a value nothing else the program handles contains.
pub const DISTINCT_LOOPBACK: Ipv4Addr = Ipv4Addr::new(127, 71, 43, 29);

fn distinct_loopback() -> Option<Ipv4Addr> {
    TcpListener::bind((DISTINCT_LOOPBACK, 0))
        .ok()
        .map(|_| DISTINCT_LOOPBACK)
}

/// A second instance for this test binary, where the shared databases the
/// tests register as Resources live: a different port from the platform
/// databases, and on Linux a different host, so that neither appears in
/// anything the program says unless it leaked a Resource's connection.
/// Never dropped; its guardian stops it when the test process exits.
pub fn for_shared_databases() -> &'static PrivatePostgres {
    static OTHER: OnceLock<Result<PrivatePostgres, String>> = OnceLock::new();
    match OTHER.get_or_init(|| PrivatePostgres::start_also_on(distinct_loopback())) {
        Ok(postgres) => postgres,
        Err(problem) => panic!("{problem}"),
    }
}

/// A platform database on the shared Postgres: the name a program is given
/// in `VM_DATABASE_URL`. A fresh one does not exist until the program
/// creates it.
#[derive(Clone)]
pub struct Platform {
    name: String,
    url: String,
    role: String,
}

impl Platform {
    /// A name no other test of this test binary uses.
    pub fn fresh() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self::named(&format!(
            "vm_platform_{}",
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    /// This name, on the shared Postgres, as its superuser.
    pub fn named(name: &str) -> Self {
        Self::on(shared(), name)
    }

    /// This name, on another of the harness's instances, as its superuser.
    pub fn on(postgres: &PrivatePostgres, name: &str) -> Self {
        Self {
            name: name.to_owned(),
            url: postgres.url(name),
            role: SUPERUSER.to_owned(),
        }
    }

    /// A fresh name on the shared Postgres, for a role of its own that is
    /// not a superuser and may only create databases and roles (`LOGIN
    /// CREATEDB CREATEROLE`): what CONTRACT.md says `VM_DATABASE_URL`'s role
    /// needs ("The Application's own database: What it looks like in
    /// Postgres"). The platform database does not exist until the program
    /// creates it.
    pub fn fresh_with_limited_role() -> Self {
        let fresh = Self::fresh();
        let token = unique().replace('-', "_");
        let role = format!("vm_limited_{token}");
        let password = format!("pw-limited-{}", Password::random().expose());
        let superuser = crate::sql::Sql::connect(&shared().url("postgres"))
            .unwrap_or_else(|e| panic!("cannot reach the test Postgres: {e}"));
        superuser.batch(&format!(
            "CREATE ROLE {role} LOGIN CREATEDB CREATEROLE NOSUPERUSER PASSWORD '{password}'"
        ));
        Self {
            url: shared().url_as(&role, &password, &fresh.name),
            name: fresh.name,
            role,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The role `VM_DATABASE_URL` names: the superuser, or the limited role
    /// of [`Platform::fresh_with_limited_role`].
    pub fn role(&self) -> &str {
        &self.role
    }

    /// The value for `VM_DATABASE_URL`. It holds the password: never print it.
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl fmt::Debug for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Platform({:?})", self.name)
    }
}
