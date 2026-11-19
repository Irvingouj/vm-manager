//! What `vibe serve` checks of its two files at start (CONTRACT.md, "The
//! program serves TLS itself: The two files" and "What is checked at
//! start"; ticket "The program serves TLS itself with a supplied wildcard
//! certificate").
//!
//! Each start-up failure: exit code not `0` within 15 seconds, no
//! `listening` record, a message naming the variable and the file's path
//! and saying why; and nothing of the key's file (none of its body lines)
//! in standard error or standard output. A certificate that is expired or
//! not valid yet is not a failure: it is served, and its client says so.
//!
//! Acceptance criterion: "a certificate that does not cover `*.<domain>`,
//! or a key that does not match, is a start-up failure in words."
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use blackbox::DOMAIN;
use blackbox::own_tls::{
    CERT_VAR, Hello, KEY_VAR, KeyKind, KeyLabel, MIB, Pair, Place, Serving, Validity,
    assert_loaded_record, assert_no_key_in, certificate, fingerprint, fixture_text, hide_keys,
    installation_names, listened, serve_once, shake, wildcard,
};
use blackbox::program::Finished;
use blackbox::tls::{Authority, api_name, pem, text_of};
use blackbox::{Vm, VmOptions};
use serde_json::json;

/// The longest a start-up failure may take.
const WITHIN: Duration = Duration::from_secs(15);

/// A test's authority, the installation's pair, and its files written.
struct Given {
    authority: Authority,
    pair: Pair,
    place: Place,
}

fn given() -> Given {
    let authority = Authority::new("Harness authority of the start-up checks");
    let pair = Pair::installation(&authority);
    let place = Place::new();
    place.write(&pair);
    Given {
        authority,
        pair,
        place,
    }
}

impl Given {
    fn cert(&self) -> String {
        text_of(&self.place.cert)
    }

    fn key(&self) -> String {
        text_of(&self.place.key)
    }

    /// Both variables naming the files.
    fn env(&self) -> Vec<(&'static str, String)> {
        self.place.env().to_vec()
    }
}

/// What the run left, keys hidden, for a message.
fn described(what: &str, finished: &Finished, took: Duration, keys: &[&str]) -> String {
    format!(
        "{what}\nexit: {} after {took:?}\nstandard output:\n{}\nstandard error:\n{}",
        finished.status_text(),
        hide_keys(&finished.stdout, keys),
        hide_keys(&finished.stderr, keys)
    )
}

/// Runs `vibe serve` with these variables and more arguments, and fails
/// unless it is a start-up failure: exit code not `0` within 15 seconds,
/// no `listening` record, every one of `names` (the variable, the path)
/// in what it wrote, at least one of each of `why`'s alternatives (in any
/// letter case), and nothing of `keys`.
fn assert_refused(
    what: &str,
    more: &[&str],
    env: &[(&str, String)],
    names: &[&str],
    why: &[&[&str]],
    keys: &[&str],
) -> String {
    let env: Vec<(&str, &str)> = env.iter().map(|(n, v)| (*n, v.as_str())).collect();
    let (finished, took) = serve_once(more, &env);
    let shown = described(what, &finished, took, keys);
    assert!(
        finished.status.is_some_and(|s| !s.success()) && took < WITHIN,
        "expected a start-up failure (exit code not 0) within {WITHIN:?}\n{shown}"
    );
    assert!(
        !listened(&finished),
        "expected no `listening` record\n{shown}"
    );
    let said = format!("{}\n{}", finished.stderr, finished.stdout);
    for name in names {
        assert!(
            finished.stderr.contains(name),
            "expected the message on standard error to name {name:?}\n{shown}"
        );
    }
    let lower = said.to_lowercase();
    for alternatives in why {
        assert!(
            alternatives
                .iter()
                .any(|a| lower.contains(&a.to_lowercase())),
            "expected the message to say why in words (one of {alternatives:?})\n{shown}"
        );
    }
    assert_no_key_in(
        &[
            ("standard error", &finished.stderr),
            ("standard output", &finished.stdout),
        ],
        keys,
    );
    eprintln!("{what}: refused after {took:?}");
    finished.stderr
}

// --- Both or neither -------------------------------------------------------

#[test]
fn the_certificate_file_without_the_key_file_is_refused_naming_the_key_files_variable() {
    let g = given();
    let key = g.pair.key_pem();
    assert_refused(
        "VIBE_TLS_CERT_FILE alone",
        &[],
        &[(CERT_VAR, g.cert())],
        &[KEY_VAR],
        &[],
        &[&key],
    );
}

#[test]
fn the_key_file_without_the_certificate_file_is_refused_naming_the_certificate_files_variable() {
    let g = given();
    let key = g.pair.key_pem();
    assert_refused(
        "VIBE_TLS_KEY_FILE alone",
        &[],
        &[(KEY_VAR, g.key())],
        &[CERT_VAR],
        &[],
        &[&key],
    );
}

#[test]
fn an_empty_value_counts_as_not_set() {
    let g = given();
    let key = g.pair.key_pem();
    assert_refused(
        "VIBE_TLS_CERT_FILE set, VIBE_TLS_KEY_FILE empty",
        &[],
        &[(CERT_VAR, g.cert()), (KEY_VAR, String::new())],
        &[KEY_VAR],
        &[],
        &[&key],
    );
    // Both empty: neither is set, and the program listens in plain HTTP.
    let vm = Vm::start(
        &VmOptions::new()
            .stand_in_sign_in()
            .env(CERT_VAR, "")
            .env(KEY_VAR, ""),
    );
    assert_eq!(vm.listening()["tls"], json!(false), "{}", vm.listening());
    vm.api_request("/v1/installation").send().expect_status(200);
}

#[test]
fn public_scheme_http_with_the_two_files_is_refused_naming_public_scheme() {
    let g = given();
    let key = g.pair.key_pem();
    assert_refused(
        "--public-scheme http with the two files",
        &["--public-scheme", "http"],
        &g.env(),
        &["--public-scheme"],
        &[],
        &[&key],
    );
}

#[test]
fn public_scheme_https_with_the_two_files_is_taken() {
    let serving = Serving::start_with(
        VmOptions::new()
            .stand_in_sign_in()
            .args_extra(&["--public-scheme", "https"]),
        Pair::installation,
    );
    assert_eq!(serving.vm.listening()["tls"], json!(true));
}

// --- Files that cannot be used --------------------------------------------

#[test]
fn a_certificate_file_that_is_not_there_is_refused() {
    let g = given();
    let key = g.pair.key_pem();
    let missing = g.place.dir().join("not-there.pem");
    let path = text_of(&missing);
    assert_refused(
        "a certificate file that is not there",
        &[],
        &[(CERT_VAR, path.clone()), (KEY_VAR, g.key())],
        &[CERT_VAR, &path],
        &[&[
            "read",
            "not found",
            "no such file",
            "not exist",
            "does not exist",
        ]],
        &[&key],
    );
}

#[test]
fn a_certificate_file_that_cannot_be_read_is_refused() {
    let g = given();
    let key = g.pair.key_pem();
    std::fs::set_permissions(&g.place.cert, std::fs::Permissions::from_mode(0o000)).unwrap();
    assert!(
        std::fs::read(&g.place.cert).is_err(),
        "the harness's own check: the test runs as a user who cannot read a file of mode 000 (not root)"
    );
    let path = g.cert();
    assert_refused(
        "a certificate file that cannot be read",
        &[],
        &g.env(),
        &[CERT_VAR, &path],
        &[&["read", "permission"]],
        &[&key],
    );
}

#[test]
fn a_key_file_that_cannot_be_read_is_refused() {
    let g = given();
    let key = g.pair.key_pem();
    std::fs::set_permissions(&g.place.key, std::fs::Permissions::from_mode(0o000)).unwrap();
    assert!(
        std::fs::read(&g.place.key).is_err(),
        "the harness's own check: the test runs as a user who cannot read a file of mode 000 (not root)"
    );
    let path = g.key();
    assert_refused(
        "a key file that cannot be read",
        &[],
        &g.env(),
        &[KEY_VAR, &path],
        &[&["read", "permission"]],
        &[&key],
    );
}

#[test]
fn an_empty_certificate_file_is_refused() {
    let g = given();
    let key = g.pair.key_pem();
    g.place.write_cert(b"");
    let path = g.cert();
    assert_refused(
        "an empty certificate file",
        &[],
        &g.env(),
        &[CERT_VAR, &path],
        &[&["empty"]],
        &[&key],
    );
}

#[test]
fn an_empty_key_file_is_refused() {
    let g = given();
    g.place.write_key(b"");
    let path = g.key();
    assert_refused(
        "an empty key file",
        &[],
        &g.env(),
        &[KEY_VAR, &path],
        &[&["empty"]],
        &[],
    );
}

/// `text` followed by lines of text outside any PEM section until the
/// whole is `size` bytes.
fn padded(text: &str, size: usize) -> Vec<u8> {
    let mut bytes = text.as_bytes().to_vec();
    let line = b"padding outside any PEM section, which a PEM reader passes over\n";
    while bytes.len() + line.len() <= size {
        bytes.extend_from_slice(line);
    }
    bytes.resize(size, b'\n');
    bytes
}

#[test]
fn a_certificate_file_larger_than_one_mib_is_refused() {
    let g = given();
    let key = g.pair.key_pem();
    g.place.write_cert(&padded(&g.pair.cert_pem(), MIB + 1));
    let path = g.cert();
    assert_refused(
        "a certificate file of 1 MiB and one byte",
        &[],
        &g.env(),
        &[CERT_VAR, &path],
        &[&["mib", "too large", "larger", "1048576", "too big", "size"]],
        &[&key],
    );
}

#[test]
fn a_key_file_larger_than_one_mib_is_refused_showing_nothing_of_it() {
    let g = given();
    let key = g.pair.key_pem();
    g.place.write_key(&padded(&key, MIB + 1));
    let path = g.key();
    assert_refused(
        "a key file of 1 MiB and one byte",
        &[],
        &g.env(),
        &[KEY_VAR, &path],
        &[&["mib", "too large", "larger", "1048576", "too big", "size"]],
        &[&key],
    );
}

#[test]
fn a_certificate_file_of_exactly_one_mib_is_taken() {
    let g = given();
    g.place.write_cert(&padded(&g.pair.cert_pem(), MIB));
    assert_eq!(std::fs::metadata(&g.place.cert).unwrap().len(), 1024 * 1024);
    let key = g.pair.key_pem();
    let options = blackbox::own_tls::options_for(
        VmOptions::new().stand_in_sign_in(),
        &g.place,
        &[&key],
        &g.authority,
        &g.place.dir().join("unused-ca.pem"),
    );
    let vm = blackbox::own_tls::start_on_a_port(&options, true);
    assert_eq!(vm.listening()["tls"], json!(true));
    let shaken = shake(vm.address(), &Hello::trusting(&api_name(), &g.authority)).unwrap();
    assert_eq!(shaken.fingerprint(), g.pair.fingerprint());
}

#[test]
fn a_certificate_file_with_no_certificate_is_refused() {
    let g = given();
    let key = g.pair.key_pem();
    g.place
        .write_cert(b"This file holds text, and no PEM section at all.\n");
    let path = g.cert();
    assert_refused(
        "a certificate file holding no certificate",
        &[],
        &g.env(),
        &[CERT_VAR, &path],
        &[&["certificate"]],
        &[&key],
    );
}

#[test]
fn a_certificate_file_holding_only_the_key_is_refused_showing_nothing_of_it() {
    let g = given();
    let key = g.pair.key_pem();
    // The two files mixed up: the key in both.
    g.place.write_cert(key.as_bytes());
    let path = g.cert();
    assert_refused(
        "a certificate file holding the key and no certificate",
        &[],
        &g.env(),
        &[CERT_VAR, &path],
        &[&["certificate"]],
        &[&key],
    );
}

#[test]
fn a_key_file_with_no_key_is_refused() {
    let g = given();
    g.place.write_key(g.pair.cert_pem().as_bytes());
    let path = g.key();
    assert_refused(
        "a key file holding a certificate and no key",
        &[],
        &g.env(),
        &[KEY_VAR, &path],
        &[&["key"]],
        &[],
    );
}

// --- The certificate file ---------------------------------------------------

/// The installation's pair with `extra` more certificates after its own
/// (the authority's, as a chain would hold it).
fn with_chain_of(g: &Given, extra: usize) -> String {
    let mut text = g.pair.cert_pem();
    for _ in 0..extra {
        text.push_str(&g.authority.pem());
    }
    text
}

#[test]
fn a_certificate_file_with_eleven_certificates_is_refused() {
    let g = given();
    let key = g.pair.key_pem();
    g.place.write_cert(with_chain_of(&g, 10).as_bytes());
    let path = g.cert();
    assert_refused(
        "a certificate file with eleven certificates",
        &[],
        &g.env(),
        &[CERT_VAR, &path],
        &[&["ten", "10", "eleven", "11"]],
        &[&key],
    );
}

#[test]
fn a_certificate_file_with_ten_certificates_is_taken_and_the_chain_presented() {
    let g = given();
    g.place.write_cert(with_chain_of(&g, 9).as_bytes());
    let key = g.pair.key_pem();
    let options = blackbox::own_tls::options_for(
        VmOptions::new().stand_in_sign_in(),
        &g.place,
        &[&key],
        &g.authority,
        &g.place.dir().join("unused-ca.pem"),
    );
    let vm = blackbox::own_tls::start_on_a_port(&options, true);
    let shaken = shake(vm.address(), &Hello::trusting(&api_name(), &g.authority))
        .unwrap_or_else(|e| panic!("expected a certificate file of ten certificates served: {e}"));
    assert_eq!(shaken.fingerprint(), g.pair.fingerprint());
    assert_eq!(
        shaken.chain.len(),
        10,
        "expected the program to present the whole chain of the file, ten certificates"
    );
}

#[test]
fn a_first_certificate_that_cannot_be_parsed_is_refused() {
    let g = given();
    let key = g.pair.key_pem();
    let mut text = pem(
        "CERTIFICATE",
        b"these bytes are no certificate, in DER or otherwise",
    );
    text.push_str(&g.pair.cert_pem());
    g.place.write_cert(text.as_bytes());
    let path = g.cert();
    assert_refused(
        "a certificate file whose first certificate cannot be parsed",
        &[],
        &g.env(),
        &[CERT_VAR, &path],
        &[&["certificate"]],
        &[&key],
    );
}

// --- The key file -----------------------------------------------------------

#[test]
fn a_key_file_with_two_keys_is_refused() {
    let g = given();
    let key = g.pair.key_pem();
    let other = Pair::installation(&g.authority).key_pem();
    g.place.write_key(format!("{key}{other}").as_bytes());
    let path = g.key();
    assert_refused(
        "a key file with two keys",
        &[],
        &g.env(),
        &[KEY_VAR, &path],
        &[&[
            "more than one",
            "two",
            "one key",
            "2 keys",
            "several",
            "only one",
        ]],
        &[&key, &other],
    );
}

#[test]
fn an_encrypted_pkcs8_key_is_refused_saying_a_passphrase_is_not_supported() {
    let g = given();
    let encrypted = fixture_text("p256-encrypted-pkcs8.pem");
    g.place.write_key(encrypted.as_bytes());
    let path = g.key();
    assert_refused(
        "an encrypted key (ENCRYPTED PRIVATE KEY)",
        &[],
        &g.env(),
        &[KEY_VAR, &path],
        &[&["passphrase"]],
        &[&encrypted],
    );
}

#[test]
fn an_encrypted_traditional_key_is_refused_saying_a_passphrase_is_not_supported() {
    let g = given();
    let encrypted = fixture_text("p256-encrypted-traditional.pem");
    g.place.write_key(encrypted.as_bytes());
    let path = g.key();
    assert_refused(
        "an encrypted key (EC PRIVATE KEY with Proc-Type: 4,ENCRYPTED)",
        &[],
        &g.env(),
        &[KEY_VAR, &path],
        &[&["passphrase"]],
        &[&encrypted],
    );
}

#[test]
fn an_rsa_key_of_1024_bits_is_refused_as_a_kind_not_taken() {
    let g = given();
    let small = Pair::issued(
        &g.authority,
        KeyKind::Rsa1024,
        KeyLabel::Pkcs8,
        &installation_names(),
        Validity::Now,
    );
    g.place.write(&small);
    let key = small.key_pem();
    let path = g.key();
    assert_refused(
        "an RSA key of 1024 bits, with its certificate",
        &[],
        &g.env(),
        &[KEY_VAR, &path],
        &[&[
            "1024",
            "2048",
            "kind",
            "not supported",
            "unsupported",
            "too small",
            "too short",
        ]],
        &[&key],
    );
}

#[test]
fn a_key_that_is_not_the_certificates_is_refused_naming_both_files() {
    let g = given();
    let other = Pair::installation(&g.authority);
    let key = other.key_pem();
    g.place.write_key(key.as_bytes());
    let (cert, key_path) = (g.cert(), g.key());
    assert_refused(
        "a key that is not the certificate's",
        &[],
        &g.env(),
        &[&cert, &key_path],
        &[&["belong", "match", "pair", "not the key"]],
        &[&key, &g.pair.key_pem()],
    );
}

// --- The names ---------------------------------------------------------------

/// Fails unless a certificate for `names` (and `common_name`) is refused,
/// the message naming `*.<domain>` and each of the names it has.
fn assert_names_refused(what: &str, names: &[String], common_name: Option<&str>) -> String {
    let g = given();
    let key = g.pair.key_pem();
    let leaf = certificate(&g.authority, &g.pair.key, names, common_name, Validity::Now);
    g.place
        .write_cert(pem("CERTIFICATE", leaf.as_ref()).as_bytes());
    let path = g.cert();
    let star = wildcard();
    let mut wanted: Vec<&str> = vec![CERT_VAR, &path, &star];
    if names.len() <= 10 {
        wanted.extend(names.iter().map(String::as_str));
    }
    assert_refused(what, &[], &g.env(), &wanted, &[], &[&key])
}

#[test]
fn a_certificate_for_another_domain_is_refused_naming_the_wildcard_and_its_names() {
    assert_names_refused(
        "a certificate for *.another.example",
        &["*.another.example".to_owned(), "another.example".to_owned()],
        None,
    );
}

#[test]
fn a_certificate_for_the_domain_and_the_api_alone_is_refused() {
    assert_names_refused(
        "a certificate for <domain> and api.<domain> alone",
        &[DOMAIN.to_owned(), api_name()],
        None,
    );
}

#[test]
fn a_certificate_whose_common_name_alone_is_the_wildcard_is_refused() {
    assert_names_refused(
        "a certificate whose common name is *.<domain>, its DNS names another",
        &[format!("dashboard.{DOMAIN}")],
        Some(&wildcard()),
    );
}

#[test]
fn a_certificate_with_many_names_none_the_wildcard_lists_at_most_ten_of_them() {
    let names: Vec<String> = (1..=12).map(|n| format!("name-{n:02}.{DOMAIN}")).collect();
    let said = assert_names_refused(
        "a certificate with twelve names, none *.<domain>",
        &names,
        None,
    );
    let listed = names.iter().filter(|n| said.contains(n.as_str())).count();
    assert!(
        (1..=10).contains(&listed),
        "expected the message to list the certificate's names, at most ten of them; it lists {listed} of the twelve:\n{said}"
    );
}

// --- Expired and not valid yet: served ------------------------------------

/// The program started with a certificate of this validity: it starts, it
/// serves it (a checking client refuses it in words holding `client_says`),
/// and its `tls-certificate-loaded` record says `"valid_now": false`.
fn served_though_not_valid_now(validity: Validity, client_says: &str) {
    let serving = Serving::start_with(VmOptions::new().stand_in_sign_in(), |a| {
        Pair::issued(
            a,
            KeyKind::EcdsaP256,
            KeyLabel::Pkcs8,
            &installation_names(),
            validity,
        )
    });
    assert_eq!(serving.vm.listening()["tls"], json!(true));
    let refused = shake(serving.address(), &serving.hello())
        .expect_err("expected a client that checks the certificate to refuse it");
    assert!(
        refused.to_lowercase().contains(client_says),
        "expected the client's own error to say {client_says:?}, got {refused:?}"
    );
    let served = shake(serving.address(), &Hello::any(Some(&api_name()))).unwrap();
    assert_eq!(
        served.fingerprint(),
        serving.pair.fingerprint(),
        "expected the certificate served all the same"
    );
    let loaded = serving.loaded();
    assert!(
        !loaded.is_empty(),
        "expected a `tls-certificate-loaded` record at start"
    );
    assert_loaded_record(&loaded[0], &serving.pair, false);
    serving.assert_no_key_logged(&[]);
}

#[test]
fn an_expired_certificate_is_served_and_logged_as_not_valid_now() {
    served_though_not_valid_now(Validity::Expired, "expired");
}

#[test]
fn a_certificate_not_valid_yet_is_served_and_logged_as_not_valid_now() {
    served_though_not_valid_now(Validity::NotYet, "not valid yet");
}

#[test]
fn a_certificate_valid_now_is_logged_at_start_with_the_contracts_fields() {
    let serving = Serving::start();
    let loaded = serving.loaded();
    assert_eq!(
        loaded.len(),
        1,
        "expected one `tls-certificate-loaded` record at start: {loaded:?}"
    );
    assert_loaded_record(&loaded[0], &serving.pair, true);
    assert_eq!(
        fingerprint(serving.pair.chain[0].as_ref()),
        serving.pair.fingerprint()
    );
    serving.assert_no_key_logged(&[]);
}
