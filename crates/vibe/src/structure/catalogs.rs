//! Reading a shared database's structure from its catalogs (blackbox/
//! CONTRACT.md, "What a pull reads").
//!
//! One connection of the Resource's structure pool, one transaction: `BEGIN
//! ISOLATION LEVEL REPEATABLE READ READ ONLY` (one snapshot for every
//! statement, nothing writable), then [`SESSION`], then [`RELATIONS`],
//! [`COLUMNS`], [`ENUMS`] and [`CONSTRAINTS`], then `ROLLBACK` and `DISCARD
//! ALL` as after every transaction of the executor. Each is a fixed text of
//! this program's: the only values in it are written by this program (the
//! kinds of relation, `'pg\_%'`), and the rest are parameters
//! (`information_schema`, the statement timeout, the enums' OIDs, which
//! came back from the catalogs as data). No name of the shared database's
//! is ever part of a statement's text.
//!
//! Every name is qualified with `pg_catalog`, and [`SESSION`] sets the
//! transaction's `search_path` to empty before anything else is read, so
//! that an operator or a type name resolves only in `pg_catalog`
//! (`pg_temp` is never searched for operators or functions, and this
//! session makes nothing in it).
//!
//! What is read: `pg_class`, `pg_namespace`, `pg_attribute`, `pg_type`,
//! `pg_attrdef`, `pg_enum`, `pg_constraint`. Nothing else: no statistics
//! (`pg_statistic`, `pg_stats`, `pg_stat_*`, `reltuples`, `relpages`), no
//! size function, no `information_schema`, no row of a table of the
//! database's own. The functions called are Postgres's own, in
//! `pg_catalog`:
//!
//! - `set_config`, for the transaction's own `search_path` and
//!   `statement_timeout`;
//! - `format_type(type, modifier)`, which prints a type's name from
//!   `pg_type` (with the type's own modifier-output function for a
//!   modifier: built in for Postgres's types, and for any other base type
//!   one only a superuser can install, in C);
//! - `pg_get_expr(adbin, adrelid)`, which prints a default's or a
//!   generated column's stored expression tree as text, never evaluating
//!   it: it looks names up in the catalogs, and a constant in the tree is
//!   printed with its type's output function (built in, or a base type's,
//!   again only a superuser's to install);
//! - `to_json`, `unnest`: built in.
//!
//! Not `pg_get_viewdef` (a view's definition is not in the structure) and
//! not `pg_get_constraintdef` (keys are read from `conkey` and `confkey`).
//!
//! Bounds: every statement's time is cut at the pull's deadline (and the
//! server's `statement_timeout` is set to end just before it); the rows
//! each statement answers are counted as they arrive against what is left
//! of [`super::MAX_BYTES`], so a structure too large ends without being
//! held whole.

use std::collections::BTreeMap;

use serde_json::Value;
use tokio::time::Instant;

use super::{
    Action, Column, Enum, Failed, ForeignKey, Generated, Identity, Key, Kind, MAX_BYTES, Relation,
    Scalar, Structure, Type,
};
use crate::executor::{Failure, Isolation, Level, Limit, Param, Pool, Rows};
use crate::log;

/// Sets what the session reads under, for this transaction only: an empty
/// `search_path`, and a `statement_timeout` (`$1`, in milliseconds) that
/// ends a statement just before the pull's deadline.
const SESSION: &str = "SELECT pg_catalog.set_config('search_path', '', true), \
     pg_catalog.set_config('statement_timeout', $1, true)";

/// Whether the Resource's read login (`$2`) or write login (`$3`, null
/// when it has none) holds a privilege on the relation `c` or one of its
/// columns. The roles are looked up in `pg_roles` first: a role the server
/// no longer has is no row, so the `EXISTS` is false (not NULL, and never
/// everything). `c` is `pg_class`.
macro_rules! can_reach {
    () => {
        "(EXISTS (SELECT 1 FROM pg_catalog.pg_roles r \
                   WHERE r.rolname OPERATOR(pg_catalog.=) $2::pg_catalog.name \
                     AND (pg_catalog.has_table_privilege(r.oid, c.oid, 'SELECT,INSERT,UPDATE,DELETE,TRUNCATE') \
                          OR pg_catalog.has_any_column_privilege(r.oid, c.oid, 'SELECT,INSERT,UPDATE'))) \
          OR EXISTS (SELECT 1 FROM pg_catalog.pg_roles r \
                      WHERE r.rolname OPERATOR(pg_catalog.=) $3::pg_catalog.name \
                        AND (pg_catalog.has_table_privilege(r.oid, c.oid, 'SELECT,INSERT,UPDATE,DELETE,TRUNCATE') \
                             OR pg_catalog.has_any_column_privilege(r.oid, c.oid, 'SELECT,INSERT,UPDATE'))))"
    };
}

/// Which relations are listed: ordinary and partitioned tables, views and
/// materialized views, not a partition, outside Postgres's own schemas
/// (`$1` is `information_schema`), and one the read or write login reaches.
/// `c` is `pg_class`, `n` its `pg_namespace`.
macro_rules! listed {
    () => {
        "c.relkind IN ('r', 'p', 'v', 'm') AND NOT c.relispartition \
         AND n.nspname OPERATOR(pg_catalog.<>) $1::pg_catalog.name \
         AND n.nspname::pg_catalog.text NOT LIKE 'pg\\_%' \
         AND "
    };
}

/// Each listed relation: its OID (to match the rows of the other
/// statements, never kept), schema, name and kind.
const RELATIONS: &str = concat!(
    "SELECT c.oid::pg_catalog.text, n.nspname::pg_catalog.text, c.relname::pg_catalog.text, \
            c.relkind::pg_catalog.text \
       FROM pg_catalog.pg_class c \
       JOIN pg_catalog.pg_namespace n ON n.oid OPERATOR(pg_catalog.=) c.relnamespace \
      WHERE ",
    listed!(),
    can_reach!()
);

/// Each column of a listed relation: its relation's OID, number, name, type
/// as printed, the type's OID, modifier and kind, the element type's OID
/// and kind when the type is an array, not null, identity, generation, and
/// the default's or generation's expression as text.
const COLUMNS: &str = "SELECT a.attrelid::pg_catalog.text, a.attnum::pg_catalog.text, a.attname::pg_catalog.text, \
            pg_catalog.format_type(a.atttypid, a.atttypmod), \
            a.atttypid::pg_catalog.text, a.atttypmod::pg_catalog.text, t.typtype::pg_catalog.text, \
            e.oid::pg_catalog.text, e.typtype::pg_catalog.text, \
            a.attnotnull::pg_catalog.text, a.attidentity::pg_catalog.text, \
            a.attgenerated::pg_catalog.text, pg_catalog.pg_get_expr(d.adbin, d.adrelid) \
       FROM pg_catalog.pg_attribute a \
       JOIN pg_catalog.pg_class c ON c.oid OPERATOR(pg_catalog.=) a.attrelid \
       JOIN pg_catalog.pg_namespace n ON n.oid OPERATOR(pg_catalog.=) c.relnamespace \
       JOIN pg_catalog.pg_type t ON t.oid OPERATOR(pg_catalog.=) a.atttypid \
       LEFT JOIN pg_catalog.pg_type e ON e.oid OPERATOR(pg_catalog.=) t.typelem \
                                     AND e.typarray OPERATOR(pg_catalog.=) t.oid \
       LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid OPERATOR(pg_catalog.=) a.attrelid \
                                        AND d.adnum OPERATOR(pg_catalog.=) a.attnum \
      WHERE a.attnum OPERATOR(pg_catalog.>) 0 AND NOT a.attisdropped \
        AND c.oid OPERATOR(pg_catalog.=) ANY ($1::pg_catalog.oid[])";

/// The enums of `$1` (their OIDs, as the columns gave them): OID, schema,
/// name, and the labels in order as a JSON array.
const ENUMS: &str = "SELECT t.oid::pg_catalog.text, n.nspname::pg_catalog.text, \
            t.typname::pg_catalog.text, \
            pg_catalog.to_json(ARRAY(SELECT l.enumlabel::pg_catalog.text \
                                       FROM pg_catalog.pg_enum l \
                                      WHERE l.enumtypid OPERATOR(pg_catalog.=) t.oid \
                                      ORDER BY l.enumsortorder))::pg_catalog.text \
       FROM pg_catalog.pg_type t \
       JOIN pg_catalog.pg_namespace n ON n.oid OPERATOR(pg_catalog.=) t.typnamespace \
      WHERE t.oid OPERATOR(pg_catalog.=) ANY ($1::pg_catalog.oid[]) \
        AND t.typtype OPERATOR(pg_catalog.=) 'e'";

/// The primary keys, unique constraints and foreign keys of the listed
/// relations (not the copies Postgres makes for partitions): the
/// relation's OID, name, kind, the columns' names in order as a JSON
/// array, and for a foreign key the schema and name of the table referred
/// to, its columns likewise, and the actions on update and on delete.
const CONSTRAINTS: &str = "SELECT k.conrelid::pg_catalog.text, k.conname::pg_catalog.text, k.contype::pg_catalog.text, \
            pg_catalog.to_json(ARRAY(SELECT a.attname::pg_catalog.text \
                 FROM pg_catalog.unnest(k.conkey) WITH ORDINALITY u(attnum, i) \
                 JOIN pg_catalog.pg_attribute a ON a.attrelid OPERATOR(pg_catalog.=) k.conrelid \
                                               AND a.attnum OPERATOR(pg_catalog.=) u.attnum \
                ORDER BY u.i))::pg_catalog.text, \
            fn.nspname::pg_catalog.text, fc.relname::pg_catalog.text, \
            pg_catalog.to_json(ARRAY(SELECT a.attname::pg_catalog.text \
                 FROM pg_catalog.unnest(k.confkey) WITH ORDINALITY u(attnum, i) \
                 JOIN pg_catalog.pg_attribute a ON a.attrelid OPERATOR(pg_catalog.=) k.confrelid \
                                               AND a.attnum OPERATOR(pg_catalog.=) u.attnum \
                ORDER BY u.i))::pg_catalog.text, \
            k.confupdtype::pg_catalog.text, k.confdeltype::pg_catalog.text, \
            k.confrelid::pg_catalog.text, COALESCE(fc.relispartition::pg_catalog.text, 'false') \
       FROM pg_catalog.pg_constraint k \
       JOIN pg_catalog.pg_class c ON c.oid OPERATOR(pg_catalog.=) k.conrelid \
       JOIN pg_catalog.pg_namespace n ON n.oid OPERATOR(pg_catalog.=) c.relnamespace \
       LEFT JOIN pg_catalog.pg_class fc ON fc.oid OPERATOR(pg_catalog.=) k.confrelid \
       LEFT JOIN pg_catalog.pg_namespace fn ON fn.oid OPERATOR(pg_catalog.=) fc.relnamespace \
      WHERE k.contype OPERATOR(pg_catalog.=) ANY ('{p,u,f}'::pg_catalog.\"char\"[]) \
        AND k.conparentid OPERATOR(pg_catalog.=) 0::pg_catalog.oid \
        AND c.oid OPERATOR(pg_catalog.=) ANY ($1::pg_catalog.oid[])";

/// How many relations outside Postgres's own schemas (partitions not
/// counted) the logins reach nothing of: not listed, and only counted.
const HIDDEN: &str = concat!(
    "SELECT pg_catalog.count(*)::pg_catalog.text \
       FROM pg_catalog.pg_class c \
       JOIN pg_catalog.pg_namespace n ON n.oid OPERATOR(pg_catalog.=) c.relnamespace \
      WHERE c.relkind IN ('r', 'p', 'v', 'm') AND NOT c.relispartition \
        AND n.nspname OPERATOR(pg_catalog.<>) $1::pg_catalog.name \
        AND n.nspname::pg_catalog.text NOT LIKE 'pg\\_%' \
        AND NOT ",
    can_reach!()
);

/// The `statement_timeout` ends a statement this long before the pull's
/// deadline, so that Postgres ends it rather than the program.
const SERVER_MARGIN_MS: u128 = 250;

/// Reads the structure through `pool`, its time cut at `deadline`. The
/// caller holds the pull to the deadline as a whole.
pub(super) async fn read(
    pool: &Pool,
    resource: &str,
    read_role: &str,
    write_role: Option<&str>,
    deadline: Instant,
) -> Result<Structure, Failed> {
    let mut held = pool
        .checkout(Level::Read)
        .await
        .map_err(|_| Failed::Unavailable)?;
    held.begin_unlabelled(Some(Isolation::RepeatableRead))
        .await
        .map_err(|_| Failed::Unavailable)?;
    let mut reading = Reading {
        held: &mut held,
        deadline,
        left: MAX_BYTES,
        resource,
        read_role,
        write_role,
    };
    let read = reading.structure().await;
    // Read-only: nothing to keep. Back to the pool only shown clean.
    held.end(false).await.ok();
    read
}

/// One pull's transaction, as it reads.
struct Reading<'h, 'p> {
    held: &'h mut crate::executor::Held<'p>,
    deadline: Instant,
    /// What is left of [`MAX_BYTES`] for the rows still to come.
    left: usize,
    resource: &'h str,
    read_role: &'h str,
    write_role: Option<&'h str>,
}

impl Reading<'_, '_> {
    async fn structure(&mut self) -> Result<Structure, Failed> {
        let left_ms = self
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .saturating_sub(SERVER_MARGIN_MS)
            .max(1);
        self.run(SESSION, &[Param::Text(left_ms.to_string())])
            .await?;
        let schemas = [
            Param::Text("information_schema".to_owned()),
            Param::Text(self.read_role.to_owned()),
            match self.write_role {
                Some(role) => Param::Text(role.to_owned()),
                None => Param::Null,
            },
        ];
        let relations = self.run(RELATIONS, &schemas).await?;
        let mut built = Built::relations(&relations)?;
        let listed = format!("{{{}}}", built.oids().join(","));
        let columns = self.run(COLUMNS, &[Param::Text(listed.clone())]).await?;
        let enum_oids = built.columns(&columns)?;
        let oids: Vec<String> = enum_oids.iter().map(u32::to_string).collect();
        let enums = self
            .run(ENUMS, &[Param::Text(format!("{{{}}}", oids.join(",")))])
            .await?;
        built.enums(&enums)?;
        let constraints = self.run(CONSTRAINTS, &[Param::Text(listed)]).await?;
        built.constraints(&constraints)?;
        let hidden = self.run(HIDDEN, &schemas).await?;
        let hidden = hidden
            .rows
            .first()
            .map(|row| number::<u64>(row, 0))
            .transpose()?
            .unwrap_or(0);
        Ok(built.finish(hidden))
    }

    /// Runs one statement within the deadline and what is left of the
    /// bytes.
    async fn run(&mut self, sql: &str, params: &[Param]) -> Result<Rows, Failed> {
        let outcome = self
            .held
            .run_bounded(sql, params, self.deadline, self.left)
            .await;
        match outcome.result {
            Ok(rows) => {
                self.left = self.left.saturating_sub(taken(&rows));
                Ok(rows)
            }
            Err(Failure::Limit(Limit::Bytes | Limit::Rows)) => Err(Failed::TooLarge),
            Err(Failure::Limit(Limit::Time)) => Err(Failed::Time),
            // query_canceled: the statement_timeout set above.
            Err(Failure::Database { code, .. }) if code == "57014" => Err(Failed::Time),
            Err(Failure::Database { code, .. }) => {
                log::record(
                    "structure-unavailable",
                    serde_json::json!({ "resource": self.resource, "code": code }),
                );
                Err(Failed::Unavailable)
            }
            Err(Failure::Unavailable) => Err(Failed::Unavailable),
        }
    }
}

/// What the rows took as they arrived: each row's message body, its count
/// of values and each value's length and bytes (the executor counts the
/// same).
fn taken(rows: &Rows) -> usize {
    rows.rows
        .iter()
        .map(|row| {
            let values = row.as_array().map_or(&[][..], Vec::as_slice);
            values.iter().fold(2_usize, |sum, value| {
                sum.saturating_add(4)
                    .saturating_add(value.as_str().map_or(0, str::len))
            })
        })
        .fold(0_usize, usize::saturating_add)
}

/// The catalogs answered something this program does not read: a newer
/// Postgres, or a bug. Unavailable, said once in the program's log.
fn unreadable(what: &str) -> Failed {
    log::record(
        "structure-unreadable",
        serde_json::json!({ "problem": what }),
    );
    Failed::Unavailable
}

/// The text in column `at` of `row`, `None` for SQL null.
fn cell(row: &Value, at: usize) -> Result<Option<&str>, Failed> {
    match row.as_array().and_then(|values| values.get(at)) {
        Some(Value::String(text)) => Ok(Some(text)),
        Some(Value::Null) => Ok(None),
        _ => Err(unreadable("a row of another shape")),
    }
}

/// The text in column `at`, which is never null.
fn text(row: &Value, at: usize) -> Result<&str, Failed> {
    cell(row, at)?.ok_or_else(|| unreadable("a null where there is none"))
}

fn number<T: std::str::FromStr>(row: &Value, at: usize) -> Result<T, Failed> {
    text(row, at)?
        .parse()
        .map_err(|_| unreadable("a number that is none"))
}

/// A JSON array of texts in column `at`.
fn texts(row: &Value, at: usize) -> Result<Vec<String>, Failed> {
    serde_json::from_str(text(row, at)?).map_err(|_| unreadable("a list that is none"))
}

/// The structure as it is put together from the rows.
struct Built {
    /// Sorted by schema, then name.
    relations: Vec<Relation>,
    /// A relation's OID to its index in `relations`.
    by_oid: BTreeMap<u32, usize>,
    /// Each relation's columns as read, by number, before they are put in
    /// order: (number, column, its enum's OID if it is of an enum).
    columns: Vec<Vec<(i32, Column, Option<u32>)>>,
    /// The enums by OID; sorted into [`Structure::enums`] at the end.
    enums: BTreeMap<u32, Enum>,
}

impl Built {
    fn relations(rows: &Rows) -> Result<Self, Failed> {
        let mut read: Vec<(u32, Relation)> = Vec::with_capacity(rows.rows.len());
        for row in &rows.rows {
            let kind = match text(row, 3)? {
                "r" | "p" => Kind::Table,
                "v" => Kind::View,
                "m" => Kind::MaterializedView,
                _ => return Err(unreadable("a relation of another kind")),
            };
            read.push((
                number(row, 0)?,
                Relation {
                    schema: text(row, 1)?.to_owned(),
                    name: text(row, 2)?.to_owned(),
                    kind,
                    columns: Vec::new(),
                    primary_key: None,
                    uniques: Vec::new(),
                    foreign_keys: Vec::new(),
                },
            ));
        }
        read.sort_by(|(_, a), (_, b)| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
        let by_oid = read
            .iter()
            .enumerate()
            .map(|(index, (oid, _))| (*oid, index))
            .collect();
        let columns = read.iter().map(|_| Vec::new()).collect();
        Ok(Self {
            relations: read.into_iter().map(|(_, r)| r).collect(),
            by_oid,
            columns,
            enums: BTreeMap::new(),
        })
    }

    /// The OIDs of the listed relations, sorted.
    fn oids(&self) -> Vec<String> {
        self.by_oid.keys().map(u32::to_string).collect()
    }

    /// Takes the columns; answers the OIDs of the enums they use.
    fn columns(&mut self, rows: &Rows) -> Result<Vec<u32>, Failed> {
        let mut enums: Vec<u32> = Vec::new();
        for row in &rows.rows {
            let relation: u32 = number(row, 0)?;
            // A relation made between the two statements' snapshots cannot
            // be, in one repeatable-read transaction; skipped all the same.
            let Some(&index) = self.by_oid.get(&relation) else {
                continue;
            };
            let type_oid: u32 = number(row, 4)?;
            let modifier: i32 = number(row, 5)?;
            let (scalar_oid, scalar_kind, array) = match cell(row, 7)? {
                Some(element) => (
                    element
                        .parse()
                        .map_err(|_| unreadable("a number that is none"))?,
                    cell(row, 8)?.unwrap_or_default(),
                    true,
                ),
                None => (type_oid, text(row, 6)?, false),
            };
            let enum_oid = (scalar_kind == "e").then_some(scalar_oid);
            if let Some(oid) = enum_oid
                && !enums.contains(&oid)
            {
                enums.push(oid);
            }
            let identity = match text(row, 10)? {
                "" => Identity::None,
                "a" => Identity::Always,
                "d" => Identity::ByDefault,
                _ => return Err(unreadable("an identity of another kind")),
            };
            let generated = match text(row, 11)? {
                "" => Generated::None,
                "s" => Generated::Stored,
                "v" => Generated::Virtual,
                _ => return Err(unreadable("a generation of another kind")),
            };
            let column = Column {
                name: text(row, 2)?.to_owned(),
                printed: text(row, 3)?.to_owned(),
                ty: Type {
                    // The enum's index is known once the enums are read.
                    scalar: Scalar::Oid(scalar_oid),
                    array,
                    modifier,
                },
                not_null: matches!(text(row, 9)?, "true" | "t"),
                identity,
                generated,
                expression: cell(row, 12)?.map(str::to_owned),
            };
            if let Some(list) = self.columns.get_mut(index) {
                list.push((number(row, 1)?, column, enum_oid));
            }
        }
        enums.sort_unstable();
        Ok(enums)
    }

    fn enums(&mut self, rows: &Rows) -> Result<(), Failed> {
        for row in &rows.rows {
            self.enums.insert(
                number(row, 0)?,
                Enum {
                    schema: text(row, 1)?.to_owned(),
                    name: text(row, 2)?.to_owned(),
                    labels: texts(row, 3)?,
                },
            );
        }
        Ok(())
    }

    fn constraints(&mut self, rows: &Rows) -> Result<(), Failed> {
        for row in &rows.rows {
            let relation: u32 = number(row, 0)?;
            let kind = text(row, 2)?;
            if kind == "f" {
                // A foreign key to a relation no login reaches is left out
                // of the definition and the fingerprint. A partition is
                // kept as before (it is not listed itself).
                let confrelid: u32 = number(row, 9)?;
                let partition = matches!(cell(row, 10)?, Some("true" | "t"));
                if !partition && !self.by_oid.contains_key(&confrelid) {
                    continue;
                }
            }
            let Some(found) = self
                .by_oid
                .get(&relation)
                .and_then(|&index| self.relations.get_mut(index))
            else {
                continue;
            };
            let name = text(row, 1)?.to_owned();
            let columns = texts(row, 3)?;
            match kind {
                "p" => found.primary_key = Some(Key { name, columns }),
                "u" => found.uniques.push(Key { name, columns }),
                "f" => found.foreign_keys.push(ForeignKey {
                    name,
                    columns,
                    schema: cell(row, 4)?.unwrap_or_default().to_owned(),
                    table: cell(row, 5)?.unwrap_or_default().to_owned(),
                    referred: texts(row, 6)?,
                    on_update: action(text(row, 7)?)?,
                    on_delete: action(text(row, 8)?)?,
                }),
                _ => return Err(unreadable("a constraint of another kind")),
            }
        }
        Ok(())
    }

    /// Puts it in the order the contract gives: enums by schema then name,
    /// columns by number, constraints by name.
    fn finish(self, hidden: u64) -> Structure {
        let mut enums: Vec<(u32, Enum)> = self.enums.into_iter().collect();
        enums.sort_by(|(_, a), (_, b)| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
        let index_of: BTreeMap<u32, usize> = enums
            .iter()
            .enumerate()
            .map(|(index, (oid, _))| (*oid, index))
            .collect();
        let mut relations = self.relations;
        for (relation, mut columns) in relations.iter_mut().zip(self.columns) {
            columns.sort_by_key(|(number, ..)| *number);
            relation.columns = columns
                .into_iter()
                .map(|(_, mut column, enum_oid)| {
                    if let Some(index) = enum_oid.and_then(|oid| index_of.get(&oid)) {
                        column.ty.scalar = Scalar::Enum(*index);
                    } else if enum_oid.is_some() {
                        // An enum the second statement did not find: dropped
                        // meanwhile; written as a type it cannot express.
                        column.ty.scalar = Scalar::Oid(0);
                    }
                    column
                })
                .collect();
            relation
                .uniques
                .sort_by(|a, b| (&a.name, &a.columns).cmp(&(&b.name, &b.columns)));
            relation.foreign_keys.sort_by(|a, b| {
                (&a.name, &a.columns, &a.schema, &a.table, &a.referred).cmp(&(
                    &b.name,
                    &b.columns,
                    &b.schema,
                    &b.table,
                    &b.referred,
                ))
            });
        }
        Structure {
            relations,
            enums: enums.into_iter().map(|(_, e)| e).collect(),
            hidden,
        }
    }
}

fn action(code: &str) -> Result<Action, Failed> {
    match code {
        "a" => Ok(Action::Nothing),
        "r" => Ok(Action::Restrict),
        "c" => Ok(Action::Cascade),
        "n" => Ok(Action::SetNull),
        "d" => Ok(Action::SetDefault),
        _ => Err(unreadable("a foreign key action of another kind")),
    }
}
