//! Black-box tests of the `vm` program, written from the tickets' acceptance
//! criteria against the program from outside. The outside interface they
//! assume is in `CONTRACT.md` beside this crate.
//!
//! This library is the harness every ticket's tests use:
//!
//! - [`program`] gets the binary (`VM_BIN`, or a build of `vm` once per test
//!   binary) and runs it to its exit for start-up checks;
//! - [`Vm`] starts `vm serve` on a free port with fixture Applications from
//!   `fixtures/`, waits for its `listening` record and ends the process
//!   when dropped, showing the program's log if the test is failing;
//! - [`http`] sends requests as a named [`User`] with full control of the
//!   headers;
//! - [`log`] and [`records`] collect the log records and let a test wait
//!   for the records of one request.
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

pub mod expect;
pub mod http;
pub mod log;
pub mod program;
pub mod records;
pub mod vm;

pub use http::{Request, Response, User};
pub use records::RequestRecords;
pub use vm::{DOMAIN, Vm, VmOptions, fixture};
