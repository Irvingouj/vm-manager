//! The harness's own Postgres (CONTRACT.md, "How the tests get a Postgres";
//! the last acceptance criterion of "Applications and their Versions live in
//! the platform database"): started with no Docker and no administrator
//! rights, on loopback, from binaries kept between runs, and gone when it is
//! stopped. These tests check the harness, not the program.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::net::TcpStream;
use std::time::{Duration, Instant};

use blackbox::postgres::{self, Origin, PrivatePostgres};

#[test]
fn a_private_postgres_starts_on_loopback_and_is_gone_once_dropped() {
    let began = Instant::now();
    let mut postgres = PrivatePostgres::start().unwrap();
    let took = began.elapsed();
    eprintln!(
        "a private Postgres from {:?} at {}: initdb {:?}, start {:?}, in all {took:?}",
        postgres.binaries().origin,
        postgres.binaries().bin.display(),
        postgres.initdb_took,
        postgres.start_took,
    );
    let port = postgres.port();
    let root = postgres.root().to_path_buf();
    assert!(postgres.is_running(), "expected the server to be running");
    assert!(
        root.starts_with("/dev/shm") || root.starts_with(std::env::temp_dir()),
        "expected the data in memory (/dev/shm) or under the system temp directory, got {}",
        root.display()
    );
    TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(5))
        .unwrap_or_else(|e| {
            panic!("expected Postgres to accept connections on 127.0.0.1:{port}: {e}")
        });
    assert!(
        !format!("{:?} {:?}", postgres, postgres.password()).contains(postgres.password().expose()),
        "expected the harness never to print the superuser's password"
    );
    drop(postgres);
    assert!(
        !root.exists(),
        "expected {} to be removed once the Postgres was dropped",
        root.display()
    );
    assert!(
        TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(2)).is_err(),
        "expected nothing to listen on 127.0.0.1:{port} once the Postgres was dropped"
    );
}

#[test]
fn downloaded_binaries_are_kept_in_a_per_user_cache() {
    let binaries = postgres::binaries();
    if binaries.origin != Origin::Cache {
        eprintln!(
            "the binaries came from {:?} ({}), not the cache: nothing to check",
            binaries.origin,
            binaries.bin.display()
        );
        return;
    }
    let cache = postgres::cache_dir().unwrap();
    assert!(
        binaries.bin.starts_with(&cache),
        "expected the downloaded binaries in the per-user cache {}, got {}",
        cache.display(),
        binaries.bin.display()
    );
    assert!(
        !binaries.bin.starts_with(std::env::temp_dir()),
        "expected the cache outside the system temp directory, so that it is kept between runs"
    );
}

#[test]
fn the_shared_postgres_serves_the_platform_databases_of_this_test_binary() {
    let shared = postgres::shared();
    let first = blackbox::Platform::fresh();
    let second = blackbox::Platform::fresh();
    assert_ne!(first.name(), second.name());
    assert!(
        first
            .url()
            .contains(&format!("127.0.0.1:{}/", shared.port())),
        "expected the platform database on the shared Postgres, got {first:?}"
    );
    assert!(
        !format!("{first:?}").contains(shared.password().expose()),
        "expected a Platform's Debug not to show the password"
    );
}

#[test]
fn the_postgres_runs_the_version_asked_for() {
    let shared = postgres::shared();
    let sql = blackbox::sql::Sql::connect(&shared.url("postgres")).unwrap();
    let number: u32 = sql
        .value("show server_version_num")
        .and_then(|v| v.parse().ok())
        .expect("expected a numeric server_version_num");
    let binaries = shared.binaries();
    eprintln!(
        "the shared Postgres is {} (server_version_num {number}) from {:?} at {}: initdb {:?}, start {:?}",
        sql.value("show server_version").unwrap_or_default(),
        binaries.origin,
        binaries.bin.display(),
        shared.initdb_took,
        shared.start_took,
    );
    if binaries.origin == Origin::Variable {
        eprintln!("VM_TEST_PG_BIN chose the binaries: their version is the caller's choice");
        return;
    }
    assert_eq!(
        number / 10_000,
        postgres::wanted_major(),
        "expected the Postgres major version VM_TEST_PG_VERSION asks for (or {} when unset)",
        postgres::DEFAULT_MAJOR
    );
}
