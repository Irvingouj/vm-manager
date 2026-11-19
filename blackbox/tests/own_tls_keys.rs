//! The kinds of key the program takes, under each PEM label the contract
//! names (CONTRACT.md, "The program serves TLS itself: The two files"): an
//! RSA key of 2048 bits (`PRIVATE KEY` and `RSA PRIVATE KEY`), ECDSA on
//! P-256 and P-384 (`PRIVATE KEY` and `EC PRIVATE KEY`), and Ed25519
//! (`PRIVATE KEY`). For each, the program starts with a certificate for its
//! key, serves it over TLS 1.3 and over TLS 1.2, and logs it loaded; the
//! key is nowhere in its log.
//!
//! The ECDSA and Ed25519 keys are made per test by ring; the RSA key is the
//! harness's fixture (`fixtures/tls/rsa-2048.pem`: ring makes no RSA key).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::VmOptions;
use blackbox::own_tls::{
    KeyKind, KeyLabel, Pair, Serving, Validity, Versions, assert_loaded_record, installation_names,
    shake,
};
use blackbox::tls::api_name;
use rustls::ProtocolVersion;
use rustls::pki_types::pem::PemObject;

fn served_with(kind: KeyKind, label: KeyLabel) {
    let serving = Serving::start_with(VmOptions::new().stand_in_sign_in(), |a| {
        Pair::issued(a, kind, label, &installation_names(), Validity::Now)
    });
    let key = serving.pair.key_pem();
    let first = key.lines().next().unwrap_or_default().to_owned();
    for (versions, want) in [
        (Versions::Tls13, ProtocolVersion::TLSv1_3),
        (Versions::Tls12, ProtocolVersion::TLSv1_2),
    ] {
        let mut shaken = shake(serving.address(), &serving.hello().versions(versions))
            .unwrap_or_else(|e| {
                panic!(
                    "expected a {want:?} handshake with a {kind:?} key written as `{first}`: {e}"
                )
            });
        assert_eq!(shaken.version, Some(want));
        assert_eq!(
            shaken.fingerprint(),
            serving.pair.fingerprint(),
            "expected the certificate for the {kind:?} key"
        );
        let (status, _) = shaken.get(&api_name(), "/v1/installation").unwrap();
        assert_eq!(status, 200);
    }
    let loaded = serving.loaded();
    assert_eq!(loaded.len(), 1, "{loaded:?}");
    assert_loaded_record(&loaded[0], &serving.pair, true);
    serving.assert_no_key_logged(&[]);
}

#[test]
fn an_rsa_2048_key_as_private_key_is_taken() {
    served_with(KeyKind::Rsa2048, KeyLabel::Pkcs8);
}

#[test]
fn an_rsa_2048_key_as_rsa_private_key_is_taken() {
    served_with(KeyKind::Rsa2048, KeyLabel::Rsa);
}

#[test]
fn an_ecdsa_p256_key_as_private_key_is_taken() {
    served_with(KeyKind::EcdsaP256, KeyLabel::Pkcs8);
}

#[test]
fn an_ecdsa_p256_key_as_ec_private_key_is_taken() {
    served_with(KeyKind::EcdsaP256, KeyLabel::Ec);
}

#[test]
fn an_ecdsa_p384_key_as_private_key_is_taken() {
    served_with(KeyKind::EcdsaP384, KeyLabel::Pkcs8);
}

#[test]
fn an_ecdsa_p384_key_as_ec_private_key_is_taken() {
    served_with(KeyKind::EcdsaP384, KeyLabel::Ec);
}

#[test]
fn an_ed25519_key_as_private_key_is_taken() {
    served_with(KeyKind::Ed25519, KeyLabel::Pkcs8);
}

/// The harness's own check of the keys it writes: each kind under each of
/// its labels is a key rustls itself takes for a server, matching its
/// certificate (so that a refusal by the program is the program's).
#[test]
fn the_harness_writes_each_key_as_a_server_library_reads_it() {
    let authority = blackbox::tls::Authority::new("Harness check");
    for (kind, label, pem_label) in [
        (KeyKind::Rsa2048, KeyLabel::Pkcs8, "PRIVATE KEY"),
        (KeyKind::Rsa2048, KeyLabel::Rsa, "RSA PRIVATE KEY"),
        (KeyKind::EcdsaP256, KeyLabel::Pkcs8, "PRIVATE KEY"),
        (KeyKind::EcdsaP256, KeyLabel::Ec, "EC PRIVATE KEY"),
        (KeyKind::EcdsaP384, KeyLabel::Pkcs8, "PRIVATE KEY"),
        (KeyKind::EcdsaP384, KeyLabel::Ec, "EC PRIVATE KEY"),
        (KeyKind::Ed25519, KeyLabel::Pkcs8, "PRIVATE KEY"),
    ] {
        let pair = Pair::issued(
            &authority,
            kind,
            label,
            &installation_names(),
            Validity::Now,
        );
        let text = pair.key_pem();
        assert!(
            text.starts_with(&format!("-----BEGIN {pem_label}-----\n")),
            "{kind:?} as {label:?}: the label"
        );
        let key = rustls::pki_types::PrivateKeyDer::from_pem_slice(text.as_bytes())
            .unwrap_or_else(|e| panic!("{kind:?} as {label:?}: not a PEM key rustls reads: {e:?}"));
        let provider = rustls::crypto::ring::default_provider();
        let signing = provider
            .key_provider
            .load_private_key(key)
            .unwrap_or_else(|e| panic!("{kind:?} as {label:?}: rustls cannot use it: {e}"));
        let certified = rustls::sign::CertifiedKey::new(pair.chain.clone(), signing);
        certified.keys_match().unwrap_or_else(|e| {
            panic!("{kind:?} as {label:?}: the key is not its certificate's: {e}")
        });
    }
}
