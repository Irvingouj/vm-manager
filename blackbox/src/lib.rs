//! Black-box tests of the `vm` program, written from the tickets' acceptance
//! criteria against the program from outside. The outside interface they
//! assume is in `CONTRACT.md` beside this crate.
//!
//! This library is the harness every ticket's tests use:
//!
//! - [`program`] gets the binary (`VM_BIN`, or a build of `vm` once per test
//!   binary) and runs it to its exit for start-up checks;
//! - [`postgres`] runs the harness's private Postgres, one per test binary,
//!   and hands each test's program a platform database of its own;
//! - [`Vm`] starts `vm serve` on a free port against a platform database,
//!   waits for its `listening` record, installs fixture Applications from
//!   `fixtures/` through the platform's API, and ends the process when
//!   dropped, showing the program's log if the test is failing;
//! - [`api`] is a client for the platform's API as a named [`User`];
//! - [`http`] sends requests as a named [`User`] with full control of the
//!   headers;
//! - [`log`] and [`records`] collect the log records and let a test wait
//!   for the records of one request;
//! - [`shared_database`] makes a shared database for a test to register as
//!   a Resource, and reads what it received from its server's own log;
//!   [`sql`] is the harness's own client for that;
//! - [`scenario`] is what the tests of Resources, Grants, `query` and the
//!   Operation log share;
//! - [`own_database`] is what the tests of an Application's own database
//!   share: migrations, Versions that carry them, operations on the own
//!   database as either actor, and the own database looked at from the
//!   harness's own superuser connection;
//! - [`history`] is what the tests of identity and Change history in the
//!   own database share, and [`server_log`] reads the test Postgres's
//!   server log as the statements each role sent.
//!
//! Every wait has a timeout, so a failing test fails within seconds and
//! never hangs. Each test starts its own program on its own port, so tests
//! run in parallel.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation here is a failing test, not a crash on the serving path"
)]

pub mod api;
pub mod expect;
pub mod history;
pub mod http;
pub mod log;
pub mod own_database;
pub mod postgres;
pub mod program;
pub mod records;
pub mod scenario;
pub mod server_log;
pub mod shared_database;
pub mod sql;
pub mod vm;

pub use api::{Api, assert_api_error};
pub use http::{Request, Response, User};
pub use postgres::Platform;
pub use records::RequestRecords;
pub use vm::{DOMAIN, FIXTURE_VERSION, INSTALLER, Vm, VmOptions, fixture, fixture_bundle};
