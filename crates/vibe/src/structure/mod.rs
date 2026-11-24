//! The Structure puller (ADR 0003, ADR 0012, ADR 0014; blackbox/CONTRACT.md,
//! "Pulling a Resource's structure"): reads a shared database's catalogs
//! and answers its structure as a Drizzle definition, with a fingerprint.
//!
//! Interface: [`pull`], on the Resource's structure pool (its structure
//! login's, else its read login's: crate::resources), answers a [`Pulled`]
//! or why not ([`Failed`]).
//!
//! - **What is read** ([`catalogs`]): fixed statements of this program's,
//!   with values as parameters, that read `pg_catalog` only, in one
//!   read-only transaction whose `search_path` is empty, so that no name
//!   resolves to an object of the shared database's. Never a row of the
//!   database's own tables, never statistics.
//! - **The structure** ([`Structure`]): every ordinary table, partitioned
//!   table, view and materialized view outside Postgres's own schemas, with
//!   its columns, keys and foreign keys, and the enums its columns use;
//!   nothing else.
//! - **The fingerprint** ([`Structure::fingerprint`]): SHA-256 over a
//!   canonical form of exactly that, which holds no object id and no
//!   creation order (relations, enums and constraints sorted by name,
//!   columns in their order).
//! - **The definition** ([`definition`]): one TypeScript module, the same
//!   bytes for the same structure, into which a name or an expression of
//!   the shared database's enters only as a string or template literal
//!   ([`ts_text`]), and whose identifiers are made by [`naming`].
//! - **Bounds**: [`PULL_TIME`] in all (checkout, connecting, every
//!   statement, building the answer, which is done off the async
//!   workers); [`MAX_BYTES`] on the rows read from
//!   the catalogs, counted as they arrive, and on the definition as it is
//!   written.

mod catalogs;
mod definition;
mod naming;
mod standin;
mod ts_text;

use std::time::Duration;

use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::time::Instant;

use crate::executor::Pool;

pub use definition::FIRST_LINE;
pub use standin::{ident as quoted_ident, literal as quoted_literal};

/// Whether `text` is a fingerprint as a pull gives one: `sha256:` and 64
/// lower-case hex digits.
pub fn is_fingerprint(text: &str) -> bool {
    text.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// The second line of the definition of `resource`'s structure, whose
/// fingerprint is `fingerprint` (without its line end).
pub fn second_line(resource: &str, fingerprint: &str) -> String {
    format!("// Resource: {resource}  Fingerprint: {fingerprint}")
}

/// The fingerprint the second line of a definition of `resource` gives
/// ([`second_line`]), when it gives one.
pub fn fingerprint_in<'a>(definition: &'a str, resource: &str) -> Option<&'a str> {
    let line = definition.lines().nth(1)?;
    let fingerprint = line.strip_prefix(second_line(resource, "").as_str())?;
    is_fingerprint(fingerprint).then_some(fingerprint)
}

/// A pull is held to this in all.
pub const PULL_TIME: Duration = Duration::from_secs(10);

/// The most the rows read from the catalogs may take, and the most the
/// answer may take.
pub const MAX_BYTES: usize = 4 * 1024 * 1024;

/// What a pull answers.
#[derive(Debug)]
pub struct Pulled {
    /// `sha256:` and 64 hex digits.
    pub fingerprint: String,
    /// The definition.
    pub schema_ts: String,
    /// The columns of a type the definition cannot express.
    pub unsupported: Vec<Unsupported>,
    /// The statements that make an empty stand-in of the structure, each
    /// ending with `;` ([`standin`]).
    pub standin_sql: Vec<String>,
    /// The relations not shown because no login of the Resource reaches
    /// them (not in the fingerprint).
    pub hidden: u64,
}

/// A column of a type the definition cannot express: there as a custom
/// type of TypeScript type `unknown`, and listed.
#[derive(Debug, Serialize)]
pub struct Unsupported {
    pub schema: String,
    pub table: String,
    pub column: String,
    /// As Postgres prints it with nothing on the search path.
    #[serde(rename = "type")]
    pub printed: String,
}

/// Why a pull answered no structure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failed {
    /// The shared database could not be reached, refused the login, or
    /// failed otherwise.
    Unavailable,
    /// The pull took longer than [`PULL_TIME`].
    Time,
    /// The structure is larger than [`MAX_BYTES`].
    TooLarge,
}

impl Failed {
    /// The Operation log entry's `error`.
    pub fn error(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Time => "time",
            Self::TooLarge => "bytes",
        }
    }
}

/// Pulls the structure of `resource` through `pool`, within [`PULL_TIME`].
pub async fn pull(
    pool: &Pool,
    resource: &str,
    read_role: &str,
    write_role: Option<&str>,
) -> Result<Pulled, Failed> {
    let deadline = Instant::now() + PULL_TIME;
    let structure = tokio::time::timeout_at(
        deadline,
        catalogs::read(pool, resource, read_role, write_role, deadline),
    )
    .await
    .map_err(|_| Failed::Time)??;
    // Building is work that does not wait (at the largest structure the
    // bounds let through, tens of milliseconds in a release build, some
    // hundreds in a debug one), so it is done off the async workers, and
    // the pull waits for it within its deadline. The thread is not given
    // up when the wait is (a pull past its time, or its request's future
    // dropped): it stops by itself at the deadline. The name is copied for
    // the thread: a few bytes, once a pull.
    let resource = resource.to_owned();
    let building = tokio::task::spawn_blocking(move || build(&structure, &resource, deadline));
    let built = tokio::time::timeout_at(deadline, building)
        .await
        .map_err(|_| Failed::Time)?
        .map_err(|_| Failed::Unavailable)?;
    // Past the end it is not answered all the same.
    if Instant::now() > deadline {
        return Err(Failed::Time);
    }
    built
}

/// The fingerprint and the definition of `structure`.
fn build(structure: &Structure, resource: &str, deadline: Instant) -> Result<Pulled, Failed> {
    let fingerprint = structure.fingerprint();
    let (schema_ts, unsupported) =
        definition::write(structure, resource, &fingerprint, MAX_BYTES, deadline)?;
    let standin_sql = standin::statements(structure);
    Ok(Pulled {
        fingerprint,
        schema_ts,
        unsupported,
        standin_sql,
        hidden: structure.hidden,
    })
}

/// The structure of a shared database, as the contract lists it: nothing
/// in it is an object id or depends on the order things were made in.
#[derive(Debug)]
struct Structure {
    /// By schema, then name, as Postgres sorts them in the `C` collation
    /// (by their bytes).
    relations: Vec<Relation>,
    /// The enums a listed column uses, by schema, then name.
    enums: Vec<Enum>,
    /// How many tables, views and materialized views outside Postgres's own
    /// schemas (partitions not counted) are not listed because neither the
    /// read nor the write login reaches them. Not in the fingerprint.
    hidden: u64,
}

#[derive(Debug)]
struct Relation {
    schema: String,
    name: String,
    kind: Kind,
    /// In their order.
    columns: Vec<Column>,
    primary_key: Option<Key>,
    /// By name.
    uniques: Vec<Key>,
    /// By name.
    foreign_keys: Vec<ForeignKey>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// Ordinary or partitioned: the definition does not tell them apart.
    Table,
    View,
    MaterializedView,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::View => "view",
            Self::MaterializedView => "materialized view",
        }
    }
}

#[derive(Debug)]
struct Column {
    name: String,
    /// Its type as Postgres prints it with nothing on the search path
    /// (`format_type`): the type, its modifier, and whether it is an array.
    printed: String,
    /// What the definition writes it as.
    ty: Type,
    not_null: bool,
    identity: Identity,
    generated: Generated,
    /// The default's expression, or the generation's when it is
    /// generated, as Postgres prints it (`pg_get_expr`): text, never
    /// evaluated.
    expression: Option<String>,
}

/// A column's type as the definition reads it.
#[derive(Clone, Copy, Debug)]
struct Type {
    scalar: Scalar,
    /// An array of `scalar`, of any number of dimensions.
    array: bool,
    /// The type modifier (`atttypmod`), `-1` when none.
    modifier: i32,
}

#[derive(Clone, Copy, Debug)]
enum Scalar {
    /// A type by its OID; only Postgres's own are expressed, by their OIDs,
    /// which are the same on every server.
    Oid(u32),
    /// An enum of [`Structure::enums`], by its index there.
    Enum(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Identity {
    None,
    Always,
    ByDefault,
}

impl Identity {
    fn name(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Always => "always",
            Self::ByDefault => "by default",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Generated {
    None,
    Stored,
    /// Postgres 18's virtual generated columns.
    Virtual,
}

impl Generated {
    fn name(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Stored => "stored",
            Self::Virtual => "virtual",
        }
    }
}

/// A primary key or a unique constraint.
#[derive(Debug)]
struct Key {
    name: String,
    /// Column names, in the constraint's order.
    columns: Vec<String>,
}

#[derive(Debug)]
struct ForeignKey {
    name: String,
    columns: Vec<String>,
    /// The table referred to.
    schema: String,
    table: String,
    referred: Vec<String>,
    on_update: Action,
    on_delete: Action,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Nothing,
    Restrict,
    Cascade,
    SetNull,
    SetDefault,
}

impl Action {
    /// As Drizzle writes it.
    fn name(self) -> &'static str {
        match self {
            Self::Nothing => "no action",
            Self::Restrict => "restrict",
            Self::Cascade => "cascade",
            Self::SetNull => "set null",
            Self::SetDefault => "set default",
        }
    }
}

#[derive(Debug)]
struct Enum {
    schema: String,
    name: String,
    /// In their order.
    labels: Vec<String>,
}

impl Structure {
    /// `sha256:` and the hex SHA-256 of the canonical form: one JSON array,
    /// `["vm structure 1", relations, enums]`, where each relation is
    /// `[kind, schema, name, columns, primary key or null, uniques,
    /// foreign keys]`, each column `[name, type as printed, not null,
    /// identity, generation, expression or null]`, each key `[name,
    /// columns]`, each foreign key `[name, columns, schema, table, columns
    /// referred to, on update, on delete]`, and each enum `[schema, name,
    /// labels]`; every list in the order of [`Structure`]. JSON writes each
    /// text unambiguously, so two structures have one form only if they are
    /// the same.
    fn fingerprint(&self) -> String {
        fn key(k: &Key) -> (&str, &[String]) {
            (k.name.as_str(), k.columns.as_slice())
        }
        let relations: Vec<_> = self
            .relations
            .iter()
            .map(|r| {
                let columns: Vec<_> = r
                    .columns
                    .iter()
                    .map(|c| {
                        (
                            c.name.as_str(),
                            c.printed.as_str(),
                            c.not_null,
                            c.identity.name(),
                            c.generated.name(),
                            c.expression.as_deref(),
                        )
                    })
                    .collect();
                let uniques: Vec<_> = r.uniques.iter().map(key).collect();
                let foreign_keys: Vec<_> = r
                    .foreign_keys
                    .iter()
                    .map(|f| {
                        (
                            f.name.as_str(),
                            f.columns.as_slice(),
                            f.schema.as_str(),
                            f.table.as_str(),
                            f.referred.as_slice(),
                            f.on_update.name(),
                            f.on_delete.name(),
                        )
                    })
                    .collect();
                (
                    r.kind.name(),
                    r.schema.as_str(),
                    r.name.as_str(),
                    columns,
                    r.primary_key.as_ref().map(key),
                    uniques,
                    foreign_keys,
                )
            })
            .collect();
        let enums: Vec<_> = self
            .enums
            .iter()
            .map(|e| (e.schema.as_str(), e.name.as_str(), e.labels.as_slice()))
            .collect();
        // Written straight into the digest, never held whole.
        let mut hashing = Hashing(Sha256::new());
        // Only a map with keys that are not strings fails, and there is
        // none: tuples and lists of texts, booleans and nulls.
        serde_json::to_writer(&mut hashing, &("vm structure 1", relations, enums)).ok();
        let digest = hashing.0.finalize();
        let mut text = String::with_capacity(71);
        text.push_str("sha256:");
        for byte in digest {
            text.push(hex_digit(byte >> 4));
            text.push(hex_digit(byte & 0x0f));
        }
        text
    }
}

/// Feeds what is written to a SHA-256.
struct Hashing(Sha256);

impl std::io::Write for Hashing {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn hex_digit(nibble: u8) -> char {
    char::from_digit(u32::from(nibble), 16).unwrap_or('0')
}
