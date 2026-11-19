//! The harness's own checks of its TLS client for the program serving TLS
//! itself (src/own_tls.rs), run against a TLS front of the harness's
//! before a program in plain HTTP, so that they pass before the program
//! serves TLS: a handshake held to one version, the certificate seen,
//! requests one after another on one connection, a connection with no
//! server name, a certificate replaced and waited for, and a connection
//! left without a handshake timed by how it ended.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant};

use blackbox::own_tls::{
    Ended, Hello, Versions, fingerprint, plain_exchange, served_fingerprint, shake,
    wait_until_served,
};
use blackbox::tls::{Authority, FRONT_WAIT, Front, api_name};
use blackbox::{DOMAIN, Vm};
use rustls::ProtocolVersion;

#[test]
fn the_client_is_held_to_one_version_and_sends_requests_one_after_another_on_one_connection() {
    let vm = Vm::with_stand_in(&[]);
    let authority = Authority::new("Harness check A");
    let identity = authority.issue(&[&api_name(), &format!("*.{DOMAIN}")]);
    let front = Front::tls(&identity, vm.address());
    for (versions, want) in [
        (Versions::Tls13, ProtocolVersion::TLSv1_3),
        (Versions::Tls12, ProtocolVersion::TLSv1_2),
        (Versions::Both, ProtocolVersion::TLSv1_3),
    ] {
        let mut shaken = shake(
            front.address(),
            &Hello::trusting(&api_name(), &authority).versions(versions),
        )
        .unwrap();
        assert_eq!(shaken.version, Some(want));
        assert!(!shaken.asked_for_client_certificate);
        assert_eq!(
            shaken.fingerprint(),
            fingerprint(identity.chain()[0].as_ref())
        );
        for _ in 0..3 {
            let (status, body) = shaken.get(&api_name(), "/v1/installation").unwrap();
            assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
            assert!(String::from_utf8_lossy(&body).contains(DOMAIN));
        }
    }
    // Another authority: refused by the checking client, taken by the one
    // that takes anything.
    let stranger = Authority::new("Harness check B");
    let refused = shake(front.address(), &Hello::trusting(&api_name(), &stranger)).unwrap_err();
    assert!(
        refused.contains("UnknownIssuer") || refused.to_lowercase().contains("issuer"),
        "{refused}"
    );
    assert!(shake(front.address(), &Hello::any(Some("another.example"))).is_ok());
    // No server name asked for.
    shake(front.address(), &Hello::any(None)).unwrap();
    let names: Vec<Option<String>> = front
        .settled()
        .iter()
        .map(|s| s.server_name.clone())
        .collect();
    assert_eq!(names.last(), Some(&None), "{names:?}");
}

#[test]
fn a_replaced_certificate_is_waited_for_and_a_silent_connection_timed() {
    let vm = Vm::with_stand_in(&[]);
    let authority = Authority::new("Harness check C");
    let first = authority.issue(&[&api_name()]);
    let front = Front::tls(&first, vm.address());
    assert_eq!(
        served_fingerprint(front.address()).unwrap(),
        fingerprint(first.chain()[0].as_ref())
    );
    let second = authority.issue(&[&api_name()]);
    let wanted = fingerprint(second.chain()[0].as_ref());
    let since = Instant::now();
    assert!(
        wait_until_served(front.address(), &wanted, since, Duration::from_millis(300)).is_err()
    );
    front.replace_identity(&second);
    wait_until_served(front.address(), &wanted, since, Duration::from_secs(5)).unwrap();
    // A ClientHello begun and never finished: the front waits FRONT_WAIT
    // for the rest, then closes.
    let begun = [
        0x16, 0x03, 0x01, 0x02, 0x00, 0x01, 0x00, 0x01, 0xfc, 0x03, 0x03,
    ];
    let (got, ended) = plain_exchange(front.address(), &begun, Duration::from_secs(20));
    assert!(got.is_empty());
    let Ended::Closed(after) = ended else {
        panic!("expected the front to close a silent connection");
    };
    assert!(
        after >= FRONT_WAIT.saturating_sub(Duration::from_millis(500))
            && after < FRONT_WAIT + Duration::from_secs(3),
        "{after:?}"
    );
    // Plain HTTP to a TLS front: no HTTP answer, closed.
    let (got, ended) = plain_exchange(
        front.address(),
        b"GET / HTTP/1.1\r\nHost: api.vm.test\r\n\r\n",
        Duration::from_secs(15),
    );
    assert!(!got.starts_with(b"HTTP/"));
    assert!(matches!(ended, Ended::Closed(_)), "{ended:?}");
}

/// A server of the test's own that asks for a client certificate (and
/// takes a client without one): the client says it was asked.
#[test]
fn the_client_tells_whether_a_server_asked_for_a_client_certificate() {
    use rustls::pki_types::PrivateKeyDer;
    use rustls::pki_types::pem::PemObject;
    use rustls::server::WebPkiClientVerifier;
    use rustls::{RootCertStore, ServerConfig, ServerConnection, StreamOwned};
    use std::net::TcpListener;
    use std::sync::Arc;

    let authority = Authority::new("Harness check D");
    let identity = authority.issue(&[&api_name()]);
    let key = PrivateKeyDer::from_pem_slice(identity.key_pem().as_bytes()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = RootCertStore::empty();
    roots.add(authority.der()).unwrap();
    let verifier =
        WebPkiClientVerifier::builder_with_provider(Arc::new(roots), Arc::clone(&provider))
            .allow_unauthenticated()
            .build()
            .unwrap();
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_client_cert_verifier(verifier)
        .with_single_cert(identity.chain().to_vec(), key)
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("no client came: {e}"),
            }
        };
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut stream = StreamOwned::new(ServerConnection::new(Arc::new(config)).unwrap(), socket);
        while stream.conn.is_handshaking() {
            if stream.conn.complete_io(&mut stream.sock).is_err() {
                break;
            }
        }
    });
    let shaken = shake(address, &Hello::trusting(&api_name(), &authority)).unwrap();
    assert!(shaken.asked_for_client_certificate);
    drop(shaken);
    server.join().unwrap();
}
