//! Black-box tests of the `vibe` program, written from the tickets' acceptance
//! criteria against the program from outside. The outside interface they
//! assume is in `CONTRACT.md` beside this crate.
//!
//! This library is the harness every ticket's tests use:
//!
//! - [`program`] gets the binary (`VIBE_BIN`, or a build of `vibe` once per test
//!   binary) and runs it to its exit for start-up checks;
//! - [`postgres`] runs the harness's private Postgres, one per test binary,
//!   and hands each test's program a platform database of its own;
//! - [`Vm`] starts `vibe serve` on a free port against a platform database,
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
//!   server log as the statements each role sent;
//! - [`sdk`] is what the tests of the SDK share: its tooling, the fixture
//!   projects under `fixtures/sdk/` built with it, and the Applications
//!   installed from them;
//! - [`transactions`] is what the raw tests of transactions share;
//! - [`sdk_history`] is what the tests of reading the Change history
//!   through the SDK share;
//! - [`out_of_reach`] is what the tests of keeping the platform's part of
//!   an own database out of a migration's reach share, [`left_behind`]
//!   what those of what a migration leaves behind share, and
//!   [`changed_outside`] what those of what something else changed share;
//! - [`tool`] runs the program as the Builder's command-line tool, on a
//!   temporary directory standing for the Builder's machine;
//! - [`project`] is what the tests of creating and checking a project
//!   share: `vibe new`, `vibe check` and its report, a project of the
//!   harness's own, a tree snapshot, whether a process runs, a tarball;
//! - [`structure`] is what the tests of pulling a Resource's structure
//!   share: a fixture shared database with the common shapes, the
//!   generated link, and a reading of the server log that says whether a
//!   login read only the catalogs;
//! - [`generate`] is what the tests of generating migrations share: `vibe
//!   generate` and what it printed, the entries of `destroys`, a project's
//!   migrations in its journal's order as a Version carries them, and the
//!   own schemas the tests change;
//! - [`deploy`] is what the tests of deploying share: a `git` that works
//!   under a machine's own `HOME`, a project's repository of its own made
//!   and committed, `vibe deploy` and what it printed;
//! - [`logs`] is what the tests of Versions, going back and logs share:
//!   the fixture that prints what it is asked, going back, and reading an
//!   Application's log entries;
//! - [`local`] is what the tests of the local loop share: `vibe request` and
//!   `vibe local ...` on a machine with its own data folder and `TMPDIR`, what
//!   they printed, the local Postgres seen from outside, a mirror that hands
//!   over Postgres's archive, and a project whose handler does what each
//!   path says;
//! - [`oidc`] is the harness's own identity provider, [`browser`] a small
//!   browser with a cookie jar that follows redirects a step at a time, and
//!   [`sessions`] what the tests of sign-in and sessions share;
//! - [`firefox`] is the harness's real browser: Firefox without a window on
//!   a fresh profile of the test's own, driven over Marionette, and ended
//!   with every process it started and its profile whatever happens to the
//!   test; [`walks`] is what the tests that walk the platform's pages in it
//!   share, and [`look`] measures how a page looks in it (colours and
//!   their contrast, sizes, the keyboard's focus, motion);
//! - [`connection`] connects OIDC as an administrator would (the
//!   installation's key, `vibe setup`, `vibe admin connect-oidc`, `PUT
//!   /v1/oidc`), and [`tool_sign_ins`] is what the tests of the tool's
//!   sign-in through the browser share: a verifier and its challenge, the
//!   forms of a page, and the harness's `BROWSER` command;
//! - [`sharing`] is what the tests of Contributors and sharing share: the
//!   entries of the two lists, an Application shared with nobody, and the
//!   `403` of someone it is not shared with. Every other way the harness
//!   makes an Application for a test (the fixtures [`Vm`] installs, the
//!   helpers of [`api`], [`scenario`], [`own_database`], [`sdk`] and
//!   [`transactions`]) shares it with everyone, since those tests are not
//!   about who can open it;
//! - [`lost_answer`] is what the tests of a write whose answer is lost
//!   share: a Postgres of the test's own whose commits the harness can make
//!   wait and let through, and the `host-call` record that says what the
//!   host decided while they wait;
//! - [`dashboard`] is what the tests of the Dashboard share: its address,
//!   requests to it as a User, its pages read into what their `data-vm-...`
//!   attributes say, its forms filled as a person fills them, and the rules
//!   every page of it keeps; [`page`] is the harness's own small reader of
//!   HTML it reads them with;
//! - [`tls`] is what the tests of the tool over HTTPS and of `VIBE_CA_FILE`
//!   share: test certificate authorities, TLS fronts before the program and
//!   before the identity provider, and the harness's own TLS client;
//!   [`own_tls`] is what the tests of the program serving TLS itself
//!   share: keys of each kind under each PEM label, the two files in a
//!   folder of the test's, replaced as an administrator replaces them, the
//!   program started with them on a port the harness chose, and a TLS
//!   client held to one version, a server name or none, and ALPN;
//! - [`install`] is what the install tests share (they need Docker and a
//!   built image, and are run by `scripts/install-gate.sh`): the image, the
//!   files of `deploy/` copied unchanged to a folder of the test's, an
//!   installation started from them with Compose and taken down whatever
//!   happens, and the identity provider on the Docker bridge's address;
//! - [`release`] is what the tests of the first release share: the
//!   package's version, the scripts of `scripts/` run by their interface
//!   with a time limit, a throwaway clone of the repository for a tag or a
//!   `LICENSE`, the notices read into their two parts, and a tool archive
//!   read entry by entry.
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
pub mod browser;
pub mod changed_outside;
pub mod connection;
pub mod dashboard;
pub mod deploy;
pub mod dev;
pub mod expect;
pub mod firefox;
pub mod generate;
pub mod history;
pub mod history_earlier;
pub mod history_kept;
pub mod history_op;
pub mod history_reading;
pub mod history_rewrite;
pub mod history_shipping;
pub mod history_structure;
pub mod http;
pub mod install;
pub mod left_behind;
pub mod local;
pub mod log;
pub mod logs;
pub mod look;
pub mod lost_answer;
pub mod oidc;
pub mod out_of_reach;
pub mod own_database;
pub mod own_tls;
pub mod page;
pub mod ports;
pub mod postgres;
pub mod program;
pub mod project;
pub mod records;
pub mod release;
pub mod scenario;
pub mod sdk;
pub mod sdk_history;
pub mod server_log;
pub mod sessions;
pub mod shared_database;
pub mod sharing;
pub mod sql;
pub mod structure;
pub mod tls;
pub mod tool;
pub mod tool_sign_ins;
pub mod transactions;
pub mod vm;
pub mod walks;

pub use api::{Api, assert_api_error};
pub use http::{Request, Response, User};
pub use postgres::Platform;
pub use records::RequestRecords;
pub use vm::{DOMAIN, FIXTURE_VERSION, INSTALLER, Vm, VmOptions, fixture, fixture_bundle};
