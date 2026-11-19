//! The two files read again when they change (CONTRACT.md, "The program
//! serves TLS itself: Read again when they change"; ticket "The program
//! serves TLS itself with a supplied wildcard certificate": "files, read at
//! start and again when they change, the key never logged").
//!
//! The program looks every 5 seconds; a new connection gets a new pair
//! within 10 seconds of the change, which each test measures (and says on
//! standard error); a connection opened before goes on with the
//! certificate it began with; a pair that does not pass changes nothing and
//! is logged once for that content; the files are found by their path each
//! time (replaced in place, by renaming, or through a symbolic link pointed
//! elsewhere).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use blackbox::own_tls::{
    CANNOT_BE_READ, KeyKind, KeyLabel, Pair, Serving, Shaken, TAKEN_WITHIN, Validity,
    assert_loaded_record, damaged_keys, hide_keys, installation_names, options_for,
    replace_by_renaming, served_fingerprint, shake, start_on_a_port, wait_until_served,
    write_in_place,
};
use blackbox::tls::{Authority, Files, api_name, text_of};
use blackbox::{Vm, VmOptions};
use serde_json::Value;

/// How long a test watches a pair that is not to be taken: more than two
/// looks of the program's.
const WATCH: Duration = Duration::from_secs(12);

/// Sends a request on `open` (a connection opened before the change): it
/// must still be answered, on the certificate it began with.
fn still_served(open: &mut Shaken, old: &str, context: &str) {
    let (status, _) = open
        .get(&api_name(), "/v1/installation")
        .unwrap_or_else(|e| {
            panic!("{context}: expected the connection opened before to go on being served: {e}")
        });
    assert_eq!(status, 200, "{context}");
    assert_eq!(
        open.fingerprint(),
        old,
        "{context}: the connection opened before keeps the certificate it began with"
    );
}

/// Waits until a new connection is served `new`, at most
/// [`TAKEN_WITHIN`] after `since`, keeping `open` busy meanwhile (a
/// request on it every second, so it is never idle); fails otherwise.
/// Returns how long it took, and says it on standard error.
fn wait_for_new(
    serving: &Serving,
    new: &str,
    since: Instant,
    open: &mut Shaken,
    old: &str,
    what: &str,
) -> Duration {
    let mut last_kept = Instant::now();
    loop {
        let left = TAKEN_WITHIN.saturating_sub(since.elapsed());
        let step = left.min(Duration::from_secs(1));
        match wait_until_served(serving.address(), new, Instant::now(), step) {
            Ok(_) => {
                let took = since.elapsed();
                eprintln!(
                    "{what}: a new connection got the new certificate {took:?} after the change"
                );
                assert!(
                    took < TAKEN_WITHIN,
                    "{what}: expected the new certificate within {TAKEN_WITHIN:?} of the change, it took {took:?}"
                );
                return took;
            }
            Err(seen) if since.elapsed() >= TAKEN_WITHIN => panic!(
                "{what}: expected a new connection to get the new certificate within {TAKEN_WITHIN:?} of the change; {seen}\nthe records: {:?}",
                serving.refused()
            ),
            Err(_) => {}
        }
        if last_kept.elapsed() >= Duration::from_secs(1) {
            still_served(open, old, what);
            last_kept = Instant::now();
        }
    }
}

/// A connection opened now, answered once.
fn open_connection(serving: &Serving) -> Shaken {
    let mut open = shake(serving.address(), &serving.hello()).unwrap();
    let (status, _) = open.get(&api_name(), "/v1/installation").unwrap();
    assert_eq!(status, 200);
    open
}

/// Fails unless the records of `tls-certificate-loaded` are `pairs`, in
/// order, each valid now.
fn assert_loaded(serving: &Serving, pairs: &[&Pair]) {
    let loaded = serving.loaded();
    assert_eq!(
        loaded.len(),
        pairs.len(),
        "expected one `tls-certificate-loaded` record at start and one for each replacement taken: {loaded:?}"
    );
    for (record, pair) in loaded.iter().zip(pairs) {
        assert_loaded_record(record, pair, true);
    }
}

/// Fails unless every `tls-certificate-refused` record has a `reason` in
/// words (two words or more).
fn assert_refused_records(records: &[Value]) {
    for record in records {
        let reason = record["reason"].as_str().unwrap_or_default();
        assert!(
            reason.split_whitespace().count() >= 2,
            "expected the `reason` of a `tls-certificate-refused` record in words: {record}"
        );
    }
}

#[test]
fn a_pair_replaced_by_renaming_is_taken_within_ten_seconds_and_an_open_connection_goes_on() {
    let serving = Serving::start();
    let old = serving.pair.fingerprint();
    let mut open = open_connection(&serving);
    let new = Pair::installation(&serving.authority);
    replace_by_renaming(&serving.place.key, new.key_pem().as_bytes());
    replace_by_renaming(&serving.place.cert, new.cert_pem().as_bytes());
    let changed = Instant::now();
    wait_for_new(
        &serving,
        &new.fingerprint(),
        changed,
        &mut open,
        &old,
        "renamed",
    );
    still_served(&mut open, &old, "after the replacement");
    let mut fresh = shake(serving.address(), &serving.hello()).unwrap_or_else(|e| {
        panic!("expected the new certificate trusted under the same authority: {e}")
    });
    assert_eq!(fresh.fingerprint(), new.fingerprint());
    assert_eq!(fresh.get(&api_name(), "/v1/installation").unwrap().0, 200);
    assert_loaded(&serving, &[&serving.pair, &new]);
    assert!(serving.refused().is_empty(), "{:?}", serving.refused());
    serving.assert_no_key_logged(&[&new.key_pem()]);
}

#[test]
fn written_in_place_the_certificate_before_its_key_changes_nothing_and_is_refused_once() {
    let serving = Serving::start();
    let old = serving.pair.fingerprint();
    let mut open = open_connection(&serving);
    let new = Pair::installation(&serving.authority);
    // The certificate first; the key not yet.
    write_in_place(&serving.place.cert, new.cert_pem().as_bytes());
    let began = Instant::now();
    let mut looked = 0;
    while began.elapsed() < WATCH {
        let seen = served_fingerprint(serving.address());
        assert_eq!(
            seen.as_deref(),
            Ok(old.as_str()),
            "expected the certificate in use to go on being served while the key does not belong to the new certificate ({:?} after it was written)",
            began.elapsed()
        );
        looked += 1;
        still_served(&mut open, &old, "between the certificate and its key");
        thread::sleep(Duration::from_millis(500));
    }
    let refused = serving.refused();
    eprintln!(
        "the certificate alone for {WATCH:?}: {looked} new connections all served the old one; {} `tls-certificate-refused` records",
        refused.len()
    );
    assert_eq!(
        refused.len(),
        1,
        "expected the pair that does not pass to be logged once for that content, not at every look: {refused:?}"
    );
    assert_refused_records(&refused);
    // Then the key.
    write_in_place(&serving.place.key, new.key_pem().as_bytes());
    let changed = Instant::now();
    wait_for_new(
        &serving,
        &new.fingerprint(),
        changed,
        &mut open,
        &old,
        "written in place",
    );
    assert_loaded(&serving, &[&serving.pair, &new]);
    serving.assert_no_key_logged(&[&new.key_pem()]);
}

#[test]
fn the_certificate_written_a_second_before_its_key_is_taken_once_both_are_there() {
    let serving = Serving::start();
    let old = serving.pair.fingerprint();
    let mut open = open_connection(&serving);
    let new = Pair::installation(&serving.authority);
    write_in_place(&serving.place.cert, new.cert_pem().as_bytes());
    thread::sleep(Duration::from_secs(1));
    write_in_place(&serving.place.key, new.key_pem().as_bytes());
    let changed = Instant::now();
    wait_for_new(
        &serving,
        &new.fingerprint(),
        changed,
        &mut open,
        &old,
        "a second apart",
    );
    serving.assert_no_key_logged(&[&new.key_pem()]);
}

#[test]
fn a_replacement_for_another_domain_is_refused_and_the_old_certificate_stays() {
    let serving = Serving::start();
    let old = serving.pair.fingerprint();
    let other = Pair::for_names(
        &serving.authority,
        &[
            "*.another.example".to_owned(),
            "api.another.example".to_owned(),
        ],
    );
    replace_by_renaming(&serving.place.key, other.key_pem().as_bytes());
    replace_by_renaming(&serving.place.cert, other.cert_pem().as_bytes());
    let began = Instant::now();
    while began.elapsed() < WATCH {
        assert_eq!(
            served_fingerprint(serving.address()).as_deref(),
            Ok(old.as_str()),
            "expected the certificate in use to stay when its replacement is for another domain"
        );
        thread::sleep(Duration::from_millis(500));
    }
    let refused = serving.refused();
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_refused_records(&refused);
    let reason = refused[0]["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains(&format!("*.{}", blackbox::DOMAIN)),
        "expected the reason to name the name the certificate lacks, *.{}: {reason:?}",
        blackbox::DOMAIN
    );
    assert_loaded(&serving, &[&serving.pair]);
    serving.assert_no_key_logged(&[&other.key_pem()]);
    // When the files change again they are looked at again.
    let good = Pair::installation(&serving.authority);
    replace_by_renaming(&serving.place.key, good.key_pem().as_bytes());
    replace_by_renaming(&serving.place.cert, good.cert_pem().as_bytes());
    let changed = Instant::now();
    let mut open = open_connection(&serving);
    wait_for_new(
        &serving,
        &good.fingerprint(),
        changed,
        &mut open,
        &old,
        "after a refusal",
    );
}

/// Watches for [`WATCH`]: every new connection gets `old`, and `open`
/// goes on with it.
fn old_stays(serving: &Serving, old: &str, open: &mut Shaken, context: &str) {
    let began = Instant::now();
    while began.elapsed() < WATCH {
        assert_eq!(
            served_fingerprint(serving.address()).as_deref(),
            Ok(old),
            "{context}: expected the certificate in use to go on being served ({:?} after)",
            began.elapsed()
        );
        still_served(open, old, context);
        thread::sleep(Duration::from_millis(500));
    }
}

#[test]
fn a_damaged_key_written_under_the_running_program_changes_nothing_and_is_refused_once_as_unreadable()
 {
    let serving = Serving::start();
    let old = serving.pair.fingerprint();
    let mut open = open_connection(&serving);
    let (what, damaged) = damaged_keys(KeyLabel::Pkcs8).into_iter().next().unwrap();
    write_in_place(&serving.place.key, damaged.as_bytes());
    old_stays(
        &serving,
        &old,
        &mut open,
        &format!("{what} written as the key"),
    );
    let refused = serving.refused();
    assert_eq!(
        refused.len(),
        1,
        "expected the damaged key logged once for that content: {}",
        hide_keys(&format!("{refused:?}"), &[&damaged])
    );
    assert_refused_records(&refused);
    let reason = refused[0]["reason"].as_str().unwrap_or_default();
    let lower = reason.to_lowercase();
    assert!(
        CANNOT_BE_READ.iter().any(|w| lower.contains(w)),
        "expected the reason to say the key cannot be read (one of {CANNOT_BE_READ:?}): {}",
        hide_keys(reason, &[&damaged])
    );
    for wrong in ["not supported", "unsupported"] {
        assert!(
            !lower.contains(wrong),
            "expected the reason not to call the damaged key a kind {wrong:?}: {}",
            hide_keys(reason, &[&damaged])
        );
    }
    assert_loaded(&serving, &[&serving.pair]);
    serving.assert_no_key_logged(&[&damaged]);
    // The right key written back: the pair in use. Nothing more is
    // refused; it is served still, and is either not logged again or
    // logged loaded as the same pair.
    write_in_place(&serving.place.key, serving.pair.key_pem().as_bytes());
    old_stays(&serving, &old, &mut open, "the right key written back");
    let refused = serving.refused();
    assert_eq!(
        refused.len(),
        1,
        "expected nothing more refused once the right key is back: {}",
        hide_keys(&format!("{refused:?}"), &[&damaged])
    );
    let loaded = serving.loaded();
    assert!(
        loaded.len() <= 2,
        "expected the pair in use logged loaded at most once more: {loaded:?}"
    );
    for record in &loaded {
        assert_loaded_record(record, &serving.pair, true);
    }
    eprintln!(
        "a damaged key, then the right one back: 1 refused, {} loaded records",
        loaded.len()
    );
    serving.assert_no_key_logged(&[&damaged]);
}

#[test]
fn an_expired_replacement_is_taken_and_logged_as_not_valid_now() {
    let serving = Serving::start();
    let old = serving.pair.fingerprint();
    let mut open = open_connection(&serving);
    let expired = Pair::issued(
        &serving.authority,
        KeyKind::EcdsaP256,
        KeyLabel::Pkcs8,
        &installation_names(),
        Validity::Expired,
    );
    replace_by_renaming(&serving.place.key, expired.key_pem().as_bytes());
    replace_by_renaming(&serving.place.cert, expired.cert_pem().as_bytes());
    let changed = Instant::now();
    wait_for_new(
        &serving,
        &expired.fingerprint(),
        changed,
        &mut open,
        &old,
        "expired",
    );
    let loaded = serving.loaded();
    assert_eq!(loaded.len(), 2, "{loaded:?}");
    assert_loaded_record(&loaded[1], &expired, false);
}

/// The two files as an orchestrator mounts a secret: `fullchain.pem` and
/// `privkey.pem` are links into `..data`, itself a link to a folder of
/// the version; a new version is a new folder, and `..data` is pointed at
/// it by renaming a new link over it.
struct Linked {
    files: Files,
}

impl Linked {
    fn new(pair: &Pair) -> Self {
        let linked = Self {
            files: Files::new(),
        };
        let dir = linked.files.dir();
        linked.version("v1", pair);
        std::os::unix::fs::symlink("v1", dir.join("..data")).unwrap();
        for name in ["fullchain.pem", "privkey.pem"] {
            std::os::unix::fs::symlink(format!("..data/{name}"), dir.join(name)).unwrap();
        }
        linked
    }

    fn version(&self, name: &str, pair: &Pair) {
        let folder = self.files.dir().join(name);
        std::fs::create_dir(&folder).unwrap();
        write_in_place(&folder.join("fullchain.pem"), pair.cert_pem().as_bytes());
        write_in_place(&folder.join("privkey.pem"), pair.key_pem().as_bytes());
    }

    /// Points `..data` at a new folder holding `pair`.
    fn point_at(&self, name: &str, pair: &Pair) {
        self.version(name, pair);
        let dir = self.files.dir();
        std::os::unix::fs::symlink(name, dir.join("..data_tmp")).unwrap();
        std::fs::rename(dir.join("..data_tmp"), dir.join("..data")).unwrap();
    }

    fn path(&self, name: &str) -> String {
        text_of(&self.files.dir().join(name))
    }

    fn dir(&self) -> &Path {
        self.files.dir()
    }
}

#[test]
fn a_symbolic_link_pointed_at_another_folder_is_followed_within_ten_seconds() {
    let authority = Authority::new("Harness authority of a mounted secret");
    let first = Pair::installation(&authority);
    let linked = Linked::new(&first);
    let place = blackbox::own_tls::Place::new();
    let key = first.key_pem();
    let options = options_for(
        VmOptions::new().stand_in_sign_in(),
        &place,
        &[&key],
        &authority,
        &linked.dir().join("unused-ca.pem"),
    )
    .env(blackbox::own_tls::CERT_VAR, &linked.path("fullchain.pem"))
    .env(blackbox::own_tls::KEY_VAR, &linked.path("privkey.pem"));
    let vm: Vm = start_on_a_port(&options, true);
    let hello = blackbox::own_tls::Hello::trusting(&api_name(), &authority);
    let mut open = shake(vm.address(), &hello).unwrap();
    assert_eq!(open.fingerprint(), first.fingerprint());
    let second = Pair::installation(&authority);
    linked.point_at("v2", &second);
    let changed = Instant::now();
    let mut last_kept = Instant::now();
    loop {
        if served_fingerprint(vm.address()).as_deref() == Ok(second.fingerprint().as_str()) {
            break;
        }
        assert!(
            changed.elapsed() < TAKEN_WITHIN,
            "expected the certificate behind the link pointed elsewhere within {TAKEN_WITHIN:?}"
        );
        if last_kept.elapsed() >= Duration::from_secs(1) {
            assert_eq!(open.get(&api_name(), "/v1/installation").unwrap().0, 200);
            last_kept = Instant::now();
        }
        thread::sleep(Duration::from_millis(100));
    }
    let took = changed.elapsed();
    eprintln!("a link pointed elsewhere: a new connection got the new certificate {took:?} after");
    assert_eq!(open.get(&api_name(), "/v1/installation").unwrap().0, 200);
    assert_eq!(open.fingerprint(), first.fingerprint());
    let lines: String = vm.lines().iter().map(|l| format!("{}\n", l.raw)).collect();
    blackbox::own_tls::assert_no_key_in(
        &[("the program's log", &lines)],
        &[&key, &second.key_pem()],
    );
    drop(place);
}
