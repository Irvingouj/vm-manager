//! The harness's own downloads (CONTRACT.md, the paragraph that begins "The
//! harness runs a private Postgres of its own"): a download that does not
//! bring the file (the connection fails or is cut, the time limit passes,
//! the status is not 200) is tried again, four tries in all, two, four and
//! eight seconds apart, each failed try said with its address and what
//! happened; after the fourth the harness stops with the last failure and
//! the number of tries. What was fetched but is wrong (a checksum that is
//! not the pinned one, a file larger than the limit) is never tried again.
//!
//! A stand-in HTTP server of the test's own on loopback answers as each
//! case says and counts the requests. The pauses are the caller's: the
//! cases give short ones; the harness's own callers give
//! `DOWNLOAD_PAUSES`, checked here to be two, four and eight seconds.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use blackbox::postgres::{
    Archive, DOWNLOAD_PAUSES, DOWNLOAD_TRIES, MAJOR, archive_of, download, fetch_archive,
};
use sha2::{Digest, Sha256};

/// What the stand-in does with one request.
#[derive(Clone)]
enum Answer {
    /// `404 Not Found`, no body.
    NotFound,
    /// Reads the request and closes the connection without a word.
    Close,
    /// `200 OK` with these bytes.
    File(Vec<u8>),
}

/// How the stand-in chooses its answers.
enum Rule {
    /// The n-th request gets the n-th answer, and every request after the
    /// last gets the last.
    InTurn(Vec<Answer>),
    /// The file at this path (`/` and all), `404` for any other.
    At(String, Vec<u8>),
}

/// The most request paths the stand-in records.
const PATHS_KEPT: usize = 100;

/// A stand-in HTTP server on 127.0.0.1, answering by its [`Rule`], that
/// counts the requests and records the path each asked for.
struct StandIn {
    port: u16,
    requests: Arc<AtomicUsize>,
    paths: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl StandIn {
    fn start(answers: Vec<Answer>) -> Self {
        assert!(!answers.is_empty());
        Self::with_rule(Rule::InTurn(answers))
    }

    /// Serves `bytes` at `path` alone.
    fn serving(path: &str, bytes: &[u8]) -> Self {
        Self::with_rule(Rule::At(path.to_owned(), bytes.to_vec()))
    }

    fn with_rule(rule: Rule) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let paths = Arc::new(Mutex::new(Vec::new()));
        let counted = Arc::clone(&requests);
        let recorded = Arc::clone(&paths);
        let stopping = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let n = counted.fetch_add(1, Ordering::SeqCst);
                        answer_one(stream, |path| {
                            let mut kept = recorded.lock().unwrap();
                            if kept.len() < PATHS_KEPT {
                                kept.push(path.to_owned());
                            }
                            match &rule {
                                Rule::InTurn(answers) => answers[n.min(answers.len() - 1)].clone(),
                                Rule::At(at, bytes) if at == path => Answer::File(bytes.clone()),
                                Rule::At(..) => Answer::NotFound,
                            }
                        });
                    }
                    Err(_) => thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        Self {
            port,
            requests,
            paths,
            stop,
            thread: Some(thread),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}/{path}", self.port)
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    fn paths(&self) -> Vec<String> {
        self.paths.lock().unwrap().clone()
    }
}

impl Drop for StandIn {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            drop(thread.join());
        }
    }
}

fn answer_one(mut stream: TcpStream, choose: impl FnOnce(&str) -> Answer) {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut seen = Vec::new();
    let mut buffer = [0_u8; 1024];
    while !seen.windows(4).any(|w| w == b"\r\n\r\n") && seen.len() < 65536 {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => seen.extend_from_slice(&buffer[..n]),
        }
    }
    let head = String::from_utf8_lossy(&seen);
    let path = head.split_whitespace().nth(1).unwrap_or_default();
    match &choose(path) {
        Answer::NotFound => {
            drop(stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            ));
        }
        Answer::Close => {}
        Answer::File(bytes) => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            );
            drop(stream.write_all(head.as_bytes()));
            drop(stream.write_all(bytes));
        }
    }
    drop(stream.flush());
}

const SHORT: [Duration; 3] = [
    Duration::from_millis(10),
    Duration::from_millis(20),
    Duration::from_millis(40),
];

const FILE: &[u8] = b"the file the harness wanted\n";

fn is_the_file(bytes: &[u8]) -> Result<(), String> {
    if bytes == FILE {
        Ok(())
    } else {
        Err("not the file pinned".to_owned())
    }
}

#[test]
fn the_harness_tries_four_times_two_four_and_eight_seconds_apart() {
    assert_eq!(DOWNLOAD_TRIES, 4);
    assert_eq!(
        DOWNLOAD_PAUSES,
        [
            Duration::from_secs(2),
            Duration::from_secs(4),
            Duration::from_secs(8)
        ]
    );
}

#[test]
fn a_404_on_the_first_tries_is_tried_again_and_the_file_had() {
    let stand_in = StandIn::start(vec![
        Answer::NotFound,
        Answer::NotFound,
        Answer::File(FILE.to_vec()),
    ]);
    let got = download(&stand_in.url("a.jar"), 1024, &SHORT, is_the_file);
    assert_eq!(got.as_deref(), Ok(FILE), "{got:?}");
    assert_eq!(
        stand_in.requests(),
        3,
        "expected two refused tries, then the file"
    );
}

#[test]
fn a_connection_closed_without_an_answer_is_tried_again_and_the_file_had() {
    let stand_in = StandIn::start(vec![
        Answer::Close,
        Answer::Close,
        Answer::Close,
        Answer::File(FILE.to_vec()),
    ]);
    let got = download(&stand_in.url("a.jar"), 1024, &SHORT, is_the_file);
    assert_eq!(got.as_deref(), Ok(FILE), "{got:?}");
    assert_eq!(
        stand_in.requests(),
        4,
        "expected three cut tries, then the file"
    );
}

#[test]
fn always_404_stops_after_four_tries_saying_the_address_the_failure_and_the_tries() {
    let stand_in = StandIn::start(vec![Answer::NotFound]);
    let url = stand_in.url("missing.jar");
    let began = Instant::now();
    let got = download(&url, 1024, &SHORT, is_the_file);
    let words = got.expect_err("expected the download to fail");
    assert_eq!(stand_in.requests(), 4, "expected exactly four tries");
    assert!(words.contains(&url), "expected the address in {words:?}");
    assert!(
        words.contains("404"),
        "expected the last failure in {words:?}"
    );
    assert!(
        words.contains("4 tries"),
        "expected the number of tries in {words:?}"
    );
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "expected the caller's short pauses, took {:?}",
        began.elapsed()
    );
}

#[test]
fn a_file_with_the_wrong_sum_is_asked_for_once() {
    let stand_in = StandIn::start(vec![Answer::File(b"another file\n".to_vec())]);
    let url = stand_in.url("a.jar");
    let got = download(&url, 1024, &SHORT, is_the_file);
    let words = got.expect_err("expected the wrong file refused");
    assert!(words.contains("not the file pinned"), "{words:?}");
    assert_eq!(
        stand_in.requests(),
        1,
        "expected a wrong file never asked for again"
    );
}

#[test]
fn a_file_larger_than_the_limit_is_asked_for_once() {
    let stand_in = StandIn::start(vec![Answer::File(vec![b'x'; 4096])]);
    let url = stand_in.url("a.jar");
    let got = download(&url, 1024, &SHORT, |_| Ok(()));
    let words = got.expect_err("expected the file over the limit refused");
    assert!(words.contains(&url), "{words:?}");
    assert_eq!(
        stand_in.requests(),
        1,
        "expected a file over the limit never asked for again"
    );
}

/// The harness's archive of Postgres 18 for this machine, its path as
/// Maven lays it out, but pinned to the sum of `bytes`: a small file of the
/// test's own stands in for it.
fn pinned_to(bytes: &[u8]) -> Archive {
    let mut archive = archive_of(MAJOR).expect("the harness knows an archive for this machine");
    archive.sha256 = format!("{:x}", Sha256::digest(bytes));
    archive
}

#[test]
fn the_postgres_download_asks_for_the_archive_and_nothing_else() {
    let bytes = b"a small file standing in for the archive\n";
    let archive = pinned_to(bytes);
    let stand_in = StandIn::serving(&format!("/{}", archive.maven_path), bytes);
    let base = format!("http://127.0.0.1:{}", stand_in.port);
    let got = fetch_archive(&base, &archive, &SHORT);
    assert_eq!(got.as_deref(), Ok(&bytes[..]), "{got:?}");
    assert_eq!(
        stand_in.paths(),
        vec![format!("/{}", archive.maven_path)],
        "expected one request, for the archive alone (no .sha256)"
    );
    assert_eq!(stand_in.requests(), 1);
}

#[test]
fn a_postgres_archive_whose_sum_is_not_the_pinned_one_is_refused_after_one_request() {
    let archive = pinned_to(b"the archive that was pinned\n");
    let served = b"another archive\n";
    let found = format!("{:x}", Sha256::digest(served));
    let stand_in = StandIn::serving(&format!("/{}", archive.maven_path), served);
    let base = format!("http://127.0.0.1:{}", stand_in.port);
    let words = fetch_archive(&base, &archive, &SHORT).expect_err("expected the archive refused");
    assert!(
        words.contains(&archive.sha256),
        "expected the pinned sum in {words:?}"
    );
    assert!(
        words.contains(&found),
        "expected the sum found in {words:?}"
    );
    assert!(words.contains("pinned"), "{words:?}");
    assert!(!words.contains("published"), "{words:?}");
    assert_eq!(
        stand_in.paths(),
        vec![format!("/{}", archive.maven_path)],
        "expected one request, for the archive alone"
    );
}
