//! The local run's download of Postgres under `VIBE_CA_FILE` (CONTRACT.md,
//! "The tool reaches an installation over HTTPS: A company's own
//! certificate authority", the bullet "The local run's download of
//! Postgres"; ticket "The tool reaches an installation over HTTPS, and a
//! company's own certificate authority is trusted").
//!
//! The mirror is the harness's own (`Mirror`, zonky's real archive of
//! Postgres 18), behind a TLS front whose certificate for `localhost` is
//! issued under a test authority A; `VIBE_LOCAL_POSTGRES_MIRROR` is
//! `https://localhost:<the front's port>/maven2`, which the tool resolves
//! by the system. A `Redirector` is a plain `http` mirror that sends the
//! archive's address on to the `https` one.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::path::PathBuf;

use blackbox::dev::{dev_args, free_port};
use blackbox::local::{
    Local, Mirror, OUTSIDER, Redirector, Requested, Serving, archive_path, holds_postgres_programs,
    local_project, port_open,
};
use blackbox::tls::{Authority, Files, Front, Handshake, Identity, private_key_pem, text_of};
use blackbox::tool::Run;

/// The archive's mirror behind a TLS front under authority A, and A's
/// certificate in a file.
struct HttpsMirror {
    // The front first, so that it ends before the mirror.
    front: Front,
    mirror: Mirror,
    files: Files,
    ca_file: String,
}

impl HttpsMirror {
    fn start() -> Self {
        Self::start_with(|a, names| a.issue(names))
    }

    /// As [`HttpsMirror::start`], the front's certificate made by `issue`
    /// under A from the names it is for (`localhost`).
    fn start_with(issue: impl FnOnce(&Authority, &[&str]) -> Identity) -> Self {
        let mirror = Mirror::start(Serving::Archive);
        let a = Authority::new("Harness authority A");
        let front = Front::tls(&issue(&a, &["localhost"]), mirror.address());
        let files = Files::new();
        let ca_file = text_of(&files.authorities("authority-a.pem", &[&a]));
        Self {
            front,
            mirror,
            files,
            ca_file,
        }
    }

    /// `https://localhost:<port>/maven2`.
    fn base_url(&self) -> String {
        format!("https://localhost:{}/maven2", self.front.port())
    }

    /// Files `VIBE_CA_FILE` cannot use: one that is not there, one that
    /// holds no certificate (a private key only).
    fn unusable_files(&self) -> Vec<String> {
        vec![
            text_of(&self.files.dir().join("not-there.pem")),
            text_of(
                &self
                    .files
                    .write("key-only.pem", private_key_pem().as_bytes()),
            ),
        ]
    }

    /// Fails unless nothing reached the mirror and no request passed the
    /// front.
    fn assert_nothing_asked(&self, context: &str) {
        assert!(
            self.mirror.requests().is_empty(),
            "{context}: expected nothing to reach the mirror, it was asked {:?}",
            self.mirror.requests()
        );
        assert!(
            self.front.request_lines().is_empty(),
            "{context}: expected no request through the front\n{}",
            self.front.describe()
        );
    }
}

/// A local run of a new project's page as outsider on `local`, with these
/// environment variables.
fn run_page(local: &Local, name: &str, env: &[(&str, &str)]) -> Run {
    let project = local_project(&local.machine, name, &[]);
    local.request_with_env(&project.dir, "/", OUTSIDER, &[], env)
}

fn assert_nothing_unpacked(local: &Local, context: &str) {
    assert_eq!(
        holds_postgres_programs(&local.postgres_folder()),
        Vec::<PathBuf>::new(),
        "{context}: expected nothing unpacked in {}",
        local.postgres_folder().display()
    );
}

/// Fails unless `run` ended `ca-file`, exit code `1`, the message naming
/// `VIBE_CA_FILE` and `path`.
fn assert_ca_file(run: &Run, path: &str) {
    let message = run.json_error(1, "ca-file");
    assert!(
        message.contains("VIBE_CA_FILE") && message.contains(path),
        "expected the message to name VIBE_CA_FILE and the path {path}, got {message:?}"
    );
}

#[test]
fn an_https_mirror_under_an_authority_of_the_file_downloads_through_the_front() {
    let https = HttpsMirror::start();
    let local = Local::new().without_bin().with_mirror(&https.base_url());
    let run = run_page(&local, "tls-download", &[("VIBE_CA_FILE", &https.ca_file)]);
    Requested::of(&run);
    assert_eq!(https.mirror.requests(), vec![archive_path()]);
    let want = format!("GET {} HTTP/1.1", archive_path());
    assert_eq!(
        https.front.request_lines(),
        vec![want],
        "expected the archive asked for by Maven's layout through the front\n{}",
        https.front.describe()
    );
    assert!(
        https
            .front
            .settled()
            .iter()
            .all(|s| s.handshake == Handshake::Completed
                && s.server_name.as_deref() == Some("localhost")),
        "{}",
        https.front.describe()
    );
    assert!(
        !holds_postgres_programs(&local.postgres_folder()).is_empty(),
        "expected Postgres unpacked"
    );
}

/// Fails unless a download from the `https` mirror with this environment
/// is `postgres-unavailable` naming `VIBE_CA_FILE`, and nothing was asked
/// or unpacked.
fn assert_authority_not_known(env: &[(&str, &str)], context: &str) {
    let https = HttpsMirror::start();
    let local = Local::new().without_bin().with_mirror(&https.base_url());
    let run = run_page(&local, "tls-unknown", env);
    let message = run.json_error(1, "postgres-unavailable");
    assert!(
        message.contains("VIBE_CA_FILE"),
        "{context}: expected the message to name VIBE_CA_FILE, got {message:?}"
    );
    https.assert_nothing_asked(context);
    assert_nothing_unpacked(&local, context);
}

#[test]
fn without_vibe_ca_file_an_https_mirror_under_a_company_authority_is_postgres_unavailable() {
    assert_authority_not_known(&[], "without VIBE_CA_FILE");
}

#[test]
fn with_a_file_of_another_authority_an_https_mirror_is_postgres_unavailable() {
    let files = Files::new();
    let b =
        text_of(&files.authorities("authority-b.pem", &[&Authority::new("Harness authority B")]));
    assert_authority_not_known(&[("VIBE_CA_FILE", &b)], "a file of authority B only");
}

/// Fails unless a download from `base_url` with each unusable file ends
/// `ca-file`, exit `1`, the front saw no connection, and nothing was asked
/// or unpacked.
fn assert_unusable_file_refused(https: &HttpsMirror, base_url: &str) {
    for path in https.unusable_files() {
        let local = Local::new().without_bin().with_mirror(base_url);
        let run = run_page(&local, "tls-ca-file", &[("VIBE_CA_FILE", &path)]);
        assert_ca_file(&run, &path);
        assert_eq!(
            https.front.accepted(),
            0,
            "expected the front to see no connection\n{}",
            https.front.describe()
        );
        https.assert_nothing_asked(&path);
        assert_nothing_unpacked(&local, &path);
    }
}

#[test]
fn an_unusable_ca_file_with_an_https_mirror_is_ca_file_before_anything_is_asked() {
    let https = HttpsMirror::start();
    let base = https.base_url();
    assert_unusable_file_refused(&https, &base);
}

#[test]
fn an_unusable_ca_file_with_a_mirror_written_https_in_capitals_is_ca_file_too() {
    let https = HttpsMirror::start();
    let base = format!("HTTPS://localhost:{}/maven2", https.front.port());
    assert_unusable_file_refused(&https, &base);
}

#[test]
fn an_http_mirror_that_redirects_to_https_with_an_unusable_file_is_ca_file_at_the_redirect() {
    let https = HttpsMirror::start();
    let redirector = Redirector::start(&https.base_url());
    for path in https.unusable_files() {
        let local = Local::new()
            .without_bin()
            .with_mirror(&redirector.base_url());
        let before = redirector.requests().len();
        let run = run_page(&local, "tls-redirect-bad", &[("VIBE_CA_FILE", &path)]);
        assert_ca_file(&run, &path);
        assert_eq!(
            redirector.requests().get(before..).unwrap_or_default(),
            [archive_path()],
            "expected the plain mirror asked once, for the archive"
        );
        assert!(
            https
                .front
                .settled()
                .iter()
                .all(|s| s.handshake != Handshake::Completed),
            "expected no handshake with the https mirror\n{}",
            https.front.describe()
        );
        https.assert_nothing_asked(&path);
        assert_nothing_unpacked(&local, &path);
    }
}

#[test]
fn an_http_mirror_that_redirects_to_https_downloads_with_the_good_file() {
    let https = HttpsMirror::start();
    let redirector = Redirector::start(&https.base_url());
    let local = Local::new()
        .without_bin()
        .with_mirror(&redirector.base_url());
    let run = run_page(
        &local,
        "tls-redirect-good",
        &[("VIBE_CA_FILE", &https.ca_file)],
    );
    Requested::of(&run);
    assert_eq!(redirector.requests(), vec![archive_path()]);
    assert_eq!(https.mirror.requests(), vec![archive_path()]);
    assert!(
        !holds_postgres_programs(&local.postgres_folder()).is_empty(),
        "expected Postgres unpacked"
    );
}

#[test]
fn a_plain_http_mirror_never_looks_at_the_ca_file() {
    let https = HttpsMirror::start();
    let plain = Mirror::start(Serving::Archive);
    let path = https.unusable_files().remove(1);
    let local = Local::new().without_bin().with_mirror(&plain.base_url());
    let run = run_page(&local, "tls-plain", &[("VIBE_CA_FILE", &path)]);
    Requested::of(&run);
    assert_eq!(plain.requests(), vec![archive_path()]);
}

#[test]
fn vibe_dev_with_an_https_mirror_and_an_unusable_file_is_ca_file_and_listens_nowhere() {
    let https = HttpsMirror::start();
    let local = Local::new().without_bin().with_mirror(&https.base_url());
    let project = local_project(&local.machine, "tls-dev", &[]);
    let path = https.unusable_files().remove(0);
    let port = free_port();
    let args = dev_args(&project.dir, port, true);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    local.watch(&project.dir);
    let run = local.run(&refs, &[("VIBE_CA_FILE", &path)]);
    assert_ca_file(&run, &path);
    assert!(!port_open(port), "expected nothing listening on {port}");
    assert_eq!(https.front.accepted(), 0, "{}", https.front.describe());
    https.assert_nothing_asked("vibe dev");
}

/// Fails unless a download from the `https` mirror whose certificate
/// `issue` makes under A, with A in `VIBE_CA_FILE` (so that this is the one
/// fault), is `postgres-unavailable`, exit `1`, the message saying `words`,
/// nothing asked of the mirror and nothing unpacked.
fn assert_certificate_fault(issue: impl FnOnce(&Authority, &[&str]) -> Identity, words: &str) {
    let https = HttpsMirror::start_with(issue);
    let local = Local::new().without_bin().with_mirror(&https.base_url());
    let run = run_page(&local, "tls-fault", &[("VIBE_CA_FILE", &https.ca_file)]);
    let message = run.json_error(1, "postgres-unavailable");
    assert!(
        message.to_lowercase().contains(words),
        "expected the message to say {words:?}, got {message:?}"
    );
    https.assert_nothing_asked(words);
    assert_nothing_unpacked(&local, words);
}

#[test]
fn a_mirror_certificate_for_another_name_is_postgres_unavailable_saying_so() {
    assert_certificate_fault(|a, _| a.issue(&["mirror.other.test"]), "another name");
}

#[test]
fn an_expired_mirror_certificate_is_postgres_unavailable_saying_so() {
    assert_certificate_fault(|a, names| a.issue_expired(names), "expired");
}

#[test]
fn a_mirror_certificate_not_valid_yet_is_postgres_unavailable_saying_so() {
    assert_certificate_fault(|a, names| a.issue_not_valid_yet(names), "not valid yet");
}
