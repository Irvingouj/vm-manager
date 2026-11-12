//! The platform program (ADR 0015). It keeps Applications and their
//! Versions in the platform database, takes them in through the platform's
//! API, and serves each Application's serving Version on its own subdomain
//! (ADR 0011) to signed-in Users.
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
//! - [`resources`]: the Resources (shared databases) and their Grants,
//!   held in memory in step with the platform database.
//! - [`executor`]: the Database executor: bounded pools of connections to
//!   shared databases and the limits of one statement.
//! - [`statement`]: a statement without its values, for the Operation log.
//! - [`check`]: the statement check, Application SQL's hygiene before it
//!   is sent.
//! - [`oplog`]: the Operation log, in the platform database.
//! - [`own_db`]: the Own-database provisioner and migrator: each
//!   Application's own database, its two roles, its migrations, and the
//!   runtime role's pool.
//! - [`platform_db`]: the platform database: connecting (and creating it),
//!   its structure in numbered steps, a bounded pool.
//! - [`registry`]: the Application registry: Applications, Contributors,
//!   Versions, and the serving Version of each, held in memory too.
//! - [`api`]: the platform's API at `api.<domain>`: HTTP in, registry
//!   calls, JSON out.
//! - [`log`]: the program's log, one JSON record per line.
//! - [`server`]: the listening socket and the connections.

pub mod api;
pub mod check;
pub mod executor;
pub mod gateway;
pub mod ingress;
pub mod log;
pub mod oplog;
pub mod own_db;
pub mod platform_db;
pub mod registry;
pub mod resources;
pub mod runner;
pub mod server;
pub mod sign_in;
pub mod statement;
pub mod user;
