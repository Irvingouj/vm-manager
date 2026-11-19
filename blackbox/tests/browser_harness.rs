//! The harness's own real browser (CONTRACT.md, "A real browser in the
//! gate: The browser"): it is found, or every browser test fails in words
//! that say what was looked for and what to do; it starts without a window
//! on a fresh profile of its own; it is ended, with every process it
//! started and its profile's folder, when the test passes, when it fails,
//! and when the test process is killed; and the check that a page is no
//! wider than its window can fail.
//!
//! The checks of a failing or killed test, and of a machine without a
//! browser, run this very test binary again as a child process: a helper
//! test (ignored in the gate, run only so) that holds a browser and fails
//! or waits to be killed, or a browser test with `VIBE_TEST_BROWSER` (and
//! `PATH`) naming no Firefox.
//!
//! Acceptance criteria: "The gate fails with words that say what to
//! install when no browser is found; it never skips the browser tests
//! silently"; "Each browser the tests start is ended by them, also when a
//! test fails; nothing is left running after the gate".
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use blackbox::firefox::{
    self, ENV, Firefox, MIN_VERSION, NO_BROWSER, NOT_ABSOLUTE, Options, Process, WHAT_TO_DO,
    no_wider_than, processes_naming,
};
use blackbox::program::{run_to_exit, write_program};

/// Where a helper test writes what it holds: the profile's folder on the
/// first line, then each process of the browser as `<pid> <start>`.
const RECORD: &str = "VIBE_BLACKBOX_BROWSER_RECORD";

/// How long a child run of this binary may take.
const CHILD_TIMEOUT: Duration = Duration::from_secs(120);

/// How long the end of a browser may take once its test is gone.
const GONE_WITHIN: Duration = Duration::from_secs(30);

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vibe-blackbox-browser-check-{}-{name}",
        std::process::id()
    ));
    drop(fs::remove_dir_all(&dir));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// This test binary run again for one test, with these environment
/// variables (and `--include-ignored` for a helper).
fn this_binary(test: &str, env: &[(&str, &str)]) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .arg(test)
        .arg("--exact")
        .arg("--include-ignored")
        .arg("--test-threads=1")
        .arg("--nocapture")
        .env_remove(RECORD)
        .envs(env.iter().copied());
    command
}

/// Writes the record of `browser` to the file `RECORD` names.
fn write_record(browser: &Firefox) {
    let path = std::env::var_os(RECORD).expect("a helper test is run with its record's path");
    let processes = browser.processes();
    assert!(!processes.is_empty(), "expected the browser's processes");
    let mut text = format!("{}\n", browser.profile().display());
    for p in processes {
        text.push_str(&format!("{} {}\n", p.pid, p.started));
    }
    let partial = PathBuf::from(&path).with_extension("partial");
    fs::write(&partial, text).unwrap();
    fs::rename(&partial, &path).unwrap();
}

/// The record a helper wrote: the profile's folder and the processes.
fn read_record(path: &Path) -> Option<(PathBuf, Vec<Process>)> {
    let text = fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    let profile = PathBuf::from(lines.next()?);
    let processes = lines
        .filter_map(|l| {
            let (pid, started) = l.split_once(' ')?;
            Some(Process {
                pid: pid.parse().ok()?,
                parent: 0,
                group: 0,
                started: started.parse().ok()?,
            })
        })
        .collect();
    Some((profile, processes))
}

/// Fails unless, within [`GONE_WITHIN`], none of these processes runs, no
/// process names the profile, and the profile's folder is gone.
fn assert_gone(profile: &Path, processes: &[Process], context: &str) {
    let deadline = Instant::now() + GONE_WITHIN;
    loop {
        let running: Vec<u32> = processes
            .iter()
            .filter(|p| p.runs())
            .map(|p| p.pid)
            .collect();
        let naming: Vec<u32> = processes_naming(profile).iter().map(|p| p.pid).collect();
        let left = profile.exists();
        if running.is_empty() && naming.is_empty() && !left {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{context}: expected the browser ended within {GONE_WITHIN:?}, every process it started and its profile; still running of its {}: {running:?}; naming its profile: {naming:?}; its profile {} {}",
            processes.len(),
            profile.display(),
            if left { "is still there" } else { "is gone" }
        );
        thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_browser_starts_on_a_fresh_profile_of_its_own_and_opens_no_page_of_its_own() {
    let mut first = Firefox::start_with(&Options::default());
    let mut second = Firefox::start_with(&Options::default());
    assert_ne!(first.profile(), second.profile());
    for browser in [&mut first, &mut second] {
        let found = browser.found();
        assert!(found.major >= MIN_VERSION, "{found:?}");
        let name = browser.profile().file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with(firefox::PROFILE_PREFIX), "{name}");
        assert_eq!(
            browser.url(),
            "about:blank",
            "expected no page of the browser's own opened at the start"
        );
        assert!(
            browser.processes().iter().any(|p| p.pid == browser.pid()),
            "expected the browser's main process among its processes"
        );
    }
    first.navigate("data:text/html,<p id=x>hello</p>");
    let x = first.the("#x");
    assert_eq!(first.text_of(&x), "hello");
}

#[test]
fn a_browser_is_ended_with_every_process_it_started_and_its_profile_when_the_test_passes() {
    let mut browser = Firefox::start_with(&Options::default());
    browser.navigate("data:text/html,<p>hello</p>");
    let profile = browser.profile().to_path_buf();
    let processes = browser.processes();
    assert!(
        processes.len() > 1,
        "expected the browser's main process and its content processes, got {processes:?}"
    );
    assert!(!processes_naming(&profile).is_empty());
    let began = Instant::now();
    drop(browser);
    eprintln!("ending a browser took {:?}", began.elapsed());
    assert_gone(&profile, &processes, "after the drop");
}

/// Run only as a child of the next test: holds a browser, writes its
/// record, and fails.
#[test]
#[ignore = "a helper: run as a child process by the harness's own checks"]
fn helper_a_test_that_fails_holding_a_browser() {
    let mut browser = Firefox::start_with(&Options::default());
    browser.navigate("data:text/html,<p>failing</p>");
    write_record(&browser);
    panic!("this helper test fails on purpose, holding a browser");
}

#[test]
fn a_browser_is_ended_with_every_process_and_its_profile_when_the_test_fails() {
    let dir = scratch("fails");
    let record = dir.join("record");
    let run = run_to_exit(
        this_binary(
            "helper_a_test_that_fails_holding_a_browser",
            &[(RECORD, record.to_str().unwrap())],
        ),
        CHILD_TIMEOUT,
    )
    .unwrap();
    assert!(
        run.status.is_some_and(|s| !s.success()),
        "expected the helper test to fail\n{}\n{}",
        run.stdout,
        run.stderr
    );
    assert!(
        run.stderr.contains("fails on purpose") || run.stdout.contains("fails on purpose"),
        "expected it to fail where it means to\n{}\n{}",
        run.stdout,
        run.stderr
    );
    let (profile, processes) = read_record(&record).expect("the helper's record");
    assert_gone(&profile, &processes, "after the test failed");
    drop(fs::remove_dir_all(&dir));
}

/// Run only as a child of the next test: holds a browser, writes its
/// record, and waits to be killed.
#[test]
#[ignore = "a helper: run as a child process by the harness's own checks"]
fn helper_a_test_that_holds_a_browser_until_it_is_killed() {
    let mut browser = Firefox::start_with(&Options::default());
    browser.navigate("data:text/html,<p>waiting</p>");
    write_record(&browser);
    thread::sleep(CHILD_TIMEOUT);
}

#[test]
fn a_browser_is_ended_with_every_process_and_its_profile_when_the_test_process_is_killed() {
    let dir = scratch("killed");
    let record = dir.join("record");
    let mut child = this_binary(
        "helper_a_test_that_holds_a_browser_until_it_is_killed",
        &[(RECORD, record.to_str().unwrap())],
    )
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .unwrap();
    let deadline = Instant::now() + CHILD_TIMEOUT;
    let held = loop {
        if let Some(held) = read_record(&record) {
            break held;
        }
        if Instant::now() >= deadline || matches!(child.try_wait(), Ok(Some(_))) {
            drop(child.kill());
            drop(child.wait());
            panic!("expected the helper to hold a browser and write its record");
        }
        thread::sleep(Duration::from_millis(50));
    };
    let (profile, processes) = held;
    assert!(
        processes.iter().all(Process::runs),
        "expected the browser running before the kill"
    );
    // SIGKILL to the test process alone: none of its code runs afterwards.
    child.kill().unwrap();
    child.wait().unwrap();
    assert_gone(&profile, &processes, "after the test process was killed");
    drop(fs::remove_dir_all(&dir));
}

/// A machine without a usable browser: what it is, the environment the
/// browser test is run with, the folder it is run from (`None`: this
/// test's own), and words the failure must hold.
type Case = (
    String,
    Vec<(&'static str, String)>,
    Option<PathBuf>,
    Vec<String>,
);

/// A program at `dir/name` that answers `--version` with `said`.
fn pretender(dir: &Path, name: &str, said: &str) -> PathBuf {
    let path = dir.join(name);
    write_program(&path, format!("#!/bin/sh\necho '{said}'\n").as_bytes()).unwrap();
    path
}

#[test]
fn without_firefox_128_or_later_every_browser_test_fails_saying_what_was_looked_for_and_what_to_do()
{
    let dir = scratch("none");
    let empty = dir.join("empty-path");
    fs::create_dir_all(&empty).unwrap();
    let missing = dir.join("no-such-firefox");
    let old = pretender(&dir, "old-firefox", "Mozilla Firefox 127.0.2");
    let other = pretender(&dir, "chromium", "Chromium 140.0.7339.80 built on Debian");
    // A Firefox 128 that is there, reached from the folder the browser
    // test is run from by a relative path: refused all the same.
    let relative_dir = dir.join("relative");
    fs::create_dir_all(relative_dir.join("bin")).unwrap();
    pretender(
        &relative_dir.join("bin"),
        "firefox",
        "Mozilla Firefox 128.0",
    );
    let cases: Vec<Case> = vec![
        (
            "VIBE_TEST_BROWSER names nothing".to_owned(),
            vec![(ENV, missing.display().to_string())],
            None,
            vec![missing.display().to_string()],
        ),
        (
            "VIBE_TEST_BROWSER names a Firefox older than 128".to_owned(),
            vec![(ENV, old.display().to_string())],
            None,
            vec!["127".to_owned()],
        ),
        (
            "VIBE_TEST_BROWSER names a browser that is not Firefox".to_owned(),
            vec![(ENV, other.display().to_string())],
            None,
            vec!["Chromium".to_owned()],
        ),
        (
            "no VIBE_TEST_BROWSER and no firefox on the PATH".to_owned(),
            vec![(ENV, String::new()), ("PATH", empty.display().to_string())],
            None,
            vec!["`firefox` on the PATH".to_owned()],
        ),
        (
            "VIBE_TEST_BROWSER a relative path to a Firefox 128 that is there".to_owned(),
            vec![(ENV, "bin/firefox".to_owned())],
            Some(relative_dir),
            vec![
                format!("{ENV}=bin/firefox"),
                NOT_ABSOLUTE.to_owned(),
                WHAT_TO_DO.to_owned(),
            ],
        ),
    ];
    for (case, env, from, named) in cases {
        let env: Vec<(&str, &str)> = env.iter().map(|(n, v)| (*n, v.as_str())).collect();
        let began = Instant::now();
        let mut command = this_binary(
            "a_browser_starts_on_a_fresh_profile_of_its_own_and_opens_no_page_of_its_own",
            &env,
        );
        if let Some(from) = &from {
            assert!(
                from.join("bin/firefox").is_file(),
                "the relative path must name a program from the folder the test runs in"
            );
            command.current_dir(from);
        }
        let run = run_to_exit(command, CHILD_TIMEOUT).unwrap();
        let output = format!("{}\n{}", run.stdout, run.stderr);
        assert!(
            run.status.is_some_and(|s| !s.success()),
            "{case}: expected the browser test to fail (never to pass or be skipped without a browser)\n{output}"
        );
        assert!(
            output.contains("1 failed")
                && !output.contains("1 passed")
                && !output.contains("1 ignored"),
            "{case}: expected the browser test to fail, not pass or be skipped\n{output}"
        );
        for words in [
            NO_BROWSER,
            "looked for",
            "Install Firefox 128 or later",
            "set VIBE_TEST_BROWSER",
        ]
        .into_iter()
        .chain(named.iter().map(String::as_str))
        {
            assert!(
                output.contains(words),
                "{case}: expected the failure to say {words:?}\n{output}"
            );
        }
        assert!(
            began.elapsed() < Duration::from_secs(30),
            "{case}: expected the failure within seconds, took {:?}",
            began.elapsed()
        );
    }
    drop(fs::remove_dir_all(&dir));
}

#[test]
fn the_width_check_holds_the_window_to_320_css_pixels_and_fails_for_a_page_wider_than_it() {
    let mut browser = Firefox::start_with(&Options::default());
    browser.navigate(
        "data:text/html,<meta name=viewport content='width=device-width, initial-scale=1'><p>fits</p>",
    );
    let wide_window = no_wider_than(&mut browser, 320);
    assert!(
        wide_window
            .as_ref()
            .is_err_and(|e| e.contains("window.innerWidth")),
        "expected the check to fail while the window is not 320 CSS pixels wide, got {wide_window:?}"
    );
    browser.narrow(320);
    assert_eq!(no_wider_than(&mut browser, 320), Ok(()));
    browser.navigate(
        "data:text/html,<meta name=viewport content='width=device-width, initial-scale=1'><div style='width:400px'>wide</div>",
    );
    let too_wide = no_wider_than(&mut browser, 320);
    eprintln!("the check against a page 400 pixels wide: {too_wide:?}");
    assert!(
        too_wide.as_ref().is_err_and(|e| e.contains("wider")),
        "expected the check to fail for a page wider than its window, got {too_wide:?}"
    );
}

#[test]
fn a_browser_reaches_the_names_it_is_given_on_this_machine_and_nothing_else_of_the_network() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let serving = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut head = [0_u8; 4096];
        let n = stream.read(&mut head).unwrap_or(0);
        let body = "<p id=here>reached</p>";
        drop(write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ));
        String::from_utf8_lossy(&head[..n]).into_owned()
    });
    let mut browser = Firefox::start_with(&Options::reaching(&["auth.vm.test".to_owned()]));
    browser.navigate(&format!("http://auth.vm.test:{port}/"));
    let here = browser.the("#here");
    assert_eq!(browser.text_of(&here), "reached");
    let request = serving.join().unwrap();
    assert!(
        request
            .to_ascii_lowercase()
            .contains(&format!("host: auth.vm.test:{port}")),
        "expected the name answered with this machine's loopback address: {request}"
    );
    // A name it was not given, and an address on the internet, are not
    // reached: they go to the proxy at a port of this machine where nothing
    // listens, and the browser gets no answer from anywhere else.
    for elsewhere in ["http://example.com/", "http://dashboard.vm.test/"] {
        let refused = browser.command(
            "WebDriver:Navigate",
            &serde_json::json!({ "url": elsewhere }),
        );
        assert!(
            refused
                .as_ref()
                .is_err_and(|e| e.message.contains("about:neterror?e=proxyConnectFailure")),
            "expected {elsewhere} not reached, got {refused:?}"
        );
    }
}

/// A stand-in site in plain HTTP on the loopback address, for a TLS front
/// to stand before: `GET /` is a page with one form posting to `/sent`
/// and a cookie `__Host-probe` (`Secure; HttpOnly; SameSite=Lax`); `POST
/// /sent` is a page saying it arrived. Each answer closes its connection.
/// It serves at most [`STAND_IN_CONNECTIONS`] connections, each read
/// within a time limit, and ends when its owner stops it.
struct StandIn {
    address: std::net::SocketAddr,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    serving: Option<thread::JoinHandle<()>>,
}

const STAND_IN_CONNECTIONS: usize = 32;

impl StandIn {
    fn start() -> Self {
        use std::io::{Read, Write};
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopping = std::sync::Arc::clone(&stop);
        let serving = thread::spawn(move || {
            let mut served = 0;
            while served < STAND_IN_CONNECTIONS && !stopping.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(20));
                    continue;
                };
                served += 1;
                drop(stream.set_nonblocking(false));
                drop(stream.set_read_timeout(Some(Duration::from_secs(10))));
                let mut head = Vec::new();
                let mut chunk = [0_u8; 4096];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 65_536 {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => head.extend_from_slice(&chunk[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&head).into_owned();
                let (body, cookie) = if text.starts_with("POST /sent ") {
                    (
                        "<!doctype html><title>sent</title><p id=sent>arrived</p>",
                        "",
                    )
                } else {
                    (
                        "<!doctype html><title>form</title><form method=post action=/sent><input type=hidden name=a value=b><button type=submit id=go>Send</button></form>",
                        "Set-Cookie: __Host-probe=1; Path=/; Secure; HttpOnly; SameSite=Lax\r\n",
                    )
                };
                drop(write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n{cookie}Connection: close\r\n\r\n{body}",
                    body.len()
                ));
            }
        });
        Self {
            address,
            stop,
            serving: Some(serving),
        }
    }
}

impl Drop for StandIn {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(serving) = self.serving.take() {
            drop(serving.join());
        }
    }
}

/// The name of the stand-in site, one of the installation's domain.
const SITE: &str = "form.vm.test";

#[test]
fn a_browser_not_given_the_authority_refuses_the_front_and_shows_its_own_certificate_error() {
    let stand_in = StandIn::start();
    let authority = blackbox::tls::Authority::new("Harness authority of a browser check");
    let front = blackbox::tls::Front::tls(&authority.issue(&[SITE]), stand_in.address);
    let mut browser = Firefox::start_with(&Options::reaching(&[SITE.to_owned()]));
    let address = format!("https://{SITE}:{}/", front.port());
    let refused = browser.command("WebDriver:Navigate", &serde_json::json!({ "url": address }));
    eprintln!("Marionette answered the navigation with: {refused:?}");
    assert!(
        refused
            .as_ref()
            .is_err_and(|e| e.error == "insecure certificate"),
        "expected the navigation refused as an insecure certificate, got {refused:?}"
    );
    let document = browser.script("return document.documentURI;", &[]);
    let text = browser.text();
    eprintln!(
        "the page shown: {} (document {document})\n--- its text ---\n{text}\n---",
        browser.url()
    );
    assert!(
        document
            .as_str()
            .is_some_and(|d| d.starts_with("about:certerror")),
        "expected the document shown to be the browser's own certificate error, got {document}"
    );
    let seen = front.connections();
    assert!(
        !seen.is_empty()
            && seen.iter().all(
                |s| s.received_bytes == 0 && s.handshake != blackbox::tls::Handshake::Completed
            ),
        "expected no handshake completed and nothing sent past it: {seen:?}"
    );
    eprintln!("the front saw: {seen:?}");
}

#[test]
fn a_form_of_an_https_page_submitted_by_a_click_arrives_with_the_browsers_origin_and_sec_fetch_headers()
 {
    let stand_in = StandIn::start();
    let authority = blackbox::tls::Authority::new("Harness authority of a browser check");
    let front = blackbox::tls::Front::tls(&authority.issue(&[SITE]), stand_in.address);
    let mut browser = Firefox::start_with(
        &Options::reaching(&[SITE.to_owned()]).trusting(authority.der().as_ref()),
    );
    let origin = format!("https://{SITE}:{}", front.port());
    browser.navigate(&format!("{origin}/"));
    let go = browser.the("#go");
    browser.click(&go);
    browser.wait_for("the page the form led to", Duration::from_secs(30), |f| {
        f.find("#sent")
    });
    let requests = front.requests();
    let posted = requests
        .iter()
        .find(|r| r.line.starts_with("POST /sent "))
        .unwrap_or_else(|| panic!("expected the form's POST through the front: {requests:?}"));
    let header = |name: &str| posted.header_values(name);
    for (name, _) in posted
        .headers
        .iter()
        .filter(|(n, _)| n.to_ascii_lowercase().starts_with("sec-fetch-"))
    {
        eprintln!("the form's request: {name}: {}", header(name).join(", "));
    }
    eprintln!(
        "the form's request: origin: {}",
        header("origin").join(", ")
    );
    let names: Vec<String> = header("cookie")
        .iter()
        .flat_map(|c| {
            c.split(';')
                .map(|p| p.trim().split('=').next().unwrap_or("").to_owned())
                .collect::<Vec<_>>()
        })
        .collect();
    eprintln!("cookie names sent: {names:?}");
    assert_eq!(header("origin"), [origin], "{posted:?}");
    assert_eq!(header("sec-fetch-site"), ["same-origin"]);
    assert_eq!(header("sec-fetch-mode"), ["navigate"]);
    assert_eq!(header("sec-fetch-dest"), ["document"]);
    assert_eq!(
        names,
        ["__Host-probe"],
        "expected the Secure cookie sent back"
    );
}
