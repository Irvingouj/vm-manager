//! The platform program (ADR 0015). In this first form it serves fixture
//! Applications, each on its own subdomain (ADR 0011), to signed-in Users.
//!
//! The parts, each behind a small interface:
//!
//! - [`ingress`]: from an HTTP request to an Application and a User, and
//!   from the Application's answer back to an HTTP response.
//! - [`sign_in`]: who is this request's User. For now only a development
//!   stand-in; real sessions fill the same seam later.
//! - [`runner`]: the Request runner. Runs an Application's
//!   `fetch(request, ctx)` in a fresh context, with the request in flight
//!   held by the host.
//! - [`gateway`]: where every operation from Application code enters the
//!   host, and is decided.
//! - [`applications`]: the Applications this program serves, named at
//!   start-up.
//! - [`platform_db`]: the platform database: connecting (and creating it),
//!   its structure in numbered steps, a bounded pool.
//! - [`log`]: the program's log, one JSON record per line.
//! - [`server`]: the listening socket and the connections.

pub mod applications;
pub mod gateway;
pub mod ingress;
pub mod log;
pub mod platform_db;
pub mod runner;
pub mod server;
pub mod sign_in;
pub mod user;
