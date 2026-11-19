//! The kinds of key the program takes, under each PEM label the contract
//! names (CONTRACT.md, "The program serves TLS itself: The two files"): an
//! RSA key of 2048 bits (`PRIVATE KEY` and `RSA PRIVATE KEY`), ECDSA on
//! P-256 and P-384 (`PRIVATE KEY` and `EC PRIVATE KEY`), and Ed25519
//! (`PRIVATE KEY`). For each, the program starts with a certificate for its
//! key, serves it over TLS 1.3 and over TLS 1.2, and logs it loaded; the
//! key is nowhere in its log.
//!
//! RSA keys of 3072 and 4096 bits are taken too (the bounds are 2048 to
//! 4096), and key files as the usual tools write them: lines of text
//! before the PEM section, an `EC PARAMETERS` section before an `EC
//! PRIVATE KEY`, CR LF line ends (in the certificate file too), and an
//! Ed25519 key without its public key.
//!
//! The ECDSA and Ed25519 keys are made per test by ring; the RSA keys are
//! the harness's fixtures (`fixtures/tls/rsa-*.pem`, test keys only: ring
//! makes no RSA key, and making one at test time is slow).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::VmOptions;
use blackbox::own_tls::{
    KeyKind, KeyLabel, Pair, Serving, Validity, Versions, Written, assert_loaded_record,
    installation_names, shake,
};
use blackbox::tls::api_name;
use rustls::ProtocolVersion;
use rustls::pki_types::pem::PemObject;

fn served_with(kind: KeyKind, label: KeyLabel) {
    served_written(kind, label, Written::Plain);
}

fn served_written(kind: KeyKind, label: KeyLabel, written: Written) {
    let serving = Serving::start_with(VmOptions::new().stand_in_sign_in(), |a| {
        Pair::issued(a, kind, label, &installation_names(), Validity::Now).written(written)
    });
    let first = format!("{label:?} written {written:?}");
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

#[test]
fn an_rsa_4096_key_as_private_key_is_taken() {
    served_with(KeyKind::Rsa4096, KeyLabel::Pkcs8);
}

#[test]
fn an_rsa_4096_key_as_rsa_private_key_is_taken() {
    served_with(KeyKind::Rsa4096, KeyLabel::Rsa);
}

#[test]
fn an_rsa_3072_key_as_private_key_is_taken() {
    served_with(KeyKind::Rsa3072, KeyLabel::Pkcs8);
}

#[test]
fn a_key_file_with_bag_attributes_before_its_pem_section_is_taken() {
    served_written(KeyKind::EcdsaP256, KeyLabel::Pkcs8, Written::BagAttributes);
}

#[test]
fn an_ec_parameters_section_before_the_ec_private_key_is_taken() {
    served_written(KeyKind::EcdsaP384, KeyLabel::Ec, Written::EcParametersFirst);
}

#[test]
fn both_files_with_cr_lf_line_ends_are_taken() {
    served_written(KeyKind::EcdsaP256, KeyLabel::Ec, Written::CrLf);
}

#[test]
fn an_ed25519_key_without_its_public_key_as_openssl_writes_it_is_taken() {
    served_written(KeyKind::Ed25519, KeyLabel::Pkcs8, Written::WithoutPublicKey);
}

/// The harness's own check of the two Ed25519 forms: the one it writes
/// plainly holds the public key (PKCS #8 version 2, as ring makes it), the
/// other does not (version 1, as `openssl genpkey` writes it).
#[test]
fn the_harness_writes_ed25519_with_its_public_key_and_without() {
    use blackbox::own_tls::Key;
    let key = Key::new(KeyKind::Ed25519);
    assert!(
        key.pkcs8_holds_public_key(),
        "the plain form holds the public key"
    );
    let without = key.pem_without_public_key();
    let der = rustls::pki_types::PrivateKeyDer::from_pem_slice(without.as_bytes()).unwrap();
    // 0x30 0x2e: 48 bytes in all, as OpenSSL writes an Ed25519 key.
    assert_eq!(
        der.secret_der().len(),
        48,
        "the form without the public key"
    );
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
        (KeyKind::Rsa3072, KeyLabel::Pkcs8, "PRIVATE KEY"),
        (KeyKind::Rsa4096, KeyLabel::Pkcs8, "PRIVATE KEY"),
        (KeyKind::Rsa4096, KeyLabel::Rsa, "RSA PRIVATE KEY"),
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
    // The written forms: rustls finds the key in each and it is the
    // certificate's.
    for (kind, label, written) in [
        (KeyKind::EcdsaP256, KeyLabel::Pkcs8, Written::BagAttributes),
        (KeyKind::EcdsaP384, KeyLabel::Ec, Written::EcParametersFirst),
        (KeyKind::EcdsaP256, KeyLabel::Ec, Written::CrLf),
        (KeyKind::Ed25519, KeyLabel::Pkcs8, Written::WithoutPublicKey),
    ] {
        let pair = Pair::issued(
            &authority,
            kind,
            label,
            &installation_names(),
            Validity::Now,
        )
        .written(written);
        let key = rustls::pki_types::PrivateKeyDer::from_pem_slice(pair.key_pem().as_bytes())
            .unwrap_or_else(|e| panic!("{written:?}: no key rustls finds: {e:?}"));
        let chain: Vec<rustls::pki_types::CertificateDer<'static>> =
            rustls::pki_types::CertificateDer::pem_slice_iter(pair.cert_pem().as_bytes())
                .collect::<Result<_, _>>()
                .unwrap_or_else(|e| panic!("{written:?}: the certificate file: {e:?}"));
        assert_eq!(chain, pair.chain, "{written:?}: the certificate file");
        let provider = rustls::crypto::ring::default_provider();
        let signing = provider
            .key_provider
            .load_private_key(key)
            .unwrap_or_else(|e| panic!("{written:?}: rustls cannot use it: {e}"));
        rustls::sign::CertifiedKey::new(pair.chain.clone(), signing)
            .keys_match()
            .unwrap_or_else(|e| panic!("{written:?}: not the certificate's key: {e}"));
    }
    // The text before the PEM section, the parameters and the CR LF are
    // there as written.
    let authority2 = &authority;
    let text = |kind, label, written| {
        Pair::issued(
            authority2,
            kind,
            label,
            &installation_names(),
            Validity::Now,
        )
        .written(written)
        .key_pem()
    };
    assert!(
        text(KeyKind::EcdsaP256, KeyLabel::Pkcs8, Written::BagAttributes)
            .starts_with("Bag Attributes\n    localKeyID: 01 02 03 \nKey Attributes: <No Attributes>\n-----BEGIN PRIVATE KEY-----\n")
    );
    assert!(
        text(KeyKind::EcdsaP384, KeyLabel::Ec, Written::EcParametersFirst)
            .starts_with("-----BEGIN EC PARAMETERS-----\nBgUrgQQAIg==\n-----END EC PARAMETERS-----\n-----BEGIN EC PRIVATE KEY-----\n")
    );
    let crlf = text(KeyKind::EcdsaP256, KeyLabel::Ec, Written::CrLf);
    assert!(crlf.contains("\r\n") && !crlf.replace("\r\n", "").contains('\n'));
}
