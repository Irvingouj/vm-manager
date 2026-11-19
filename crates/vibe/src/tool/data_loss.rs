//! What a generated migration destroys (blackbox/CONTRACT.md, "Generating
//! migrations: A migration that destroys data").
//!
//! Decided on the statements as the platform's own Postgres parser reads
//! them ([`crate::migration_text::parsed`]), never on a pattern in the
//! text: `DROP TABLE` in a string or a comment is no statement. A statement
//! destroys data when it is, exactly:
//!
//! - `DROP TABLE` (a `DropStmt` of tables): each table named;
//! - `ALTER TABLE ... DROP COLUMN` (an `AlterTableStmt` on a table, with an
//!   `AT_DropColumn` command): each column named;
//! - `ALTER TABLE ... ALTER COLUMN ... [SET DATA] TYPE` (`AT_AlterColumnType`)
//!   to a type that is not wider ([`is_wider`]) than the column's type in
//!   the last snapshot; a type the snapshot does not have, and every type
//!   not recognised ([`Ty`]), counts as not wider;
//! - `DROP SCHEMA` (a `DropStmt` of schemas): each schema named.
//!
//! Nothing else: a `DISABLE ROW LEVEL SECURITY`, a view dropped, a default
//! dropped or a constraint tightened is in no list. At deploy ([`decide`]
//! with `rows`), besides, what only a migration written by hand has: a
//! `DELETE` (a `DeleteStmt`, its table), at the top of a statement or in
//! its `WITH`, and a `TRUNCATE` (a `TruncateStmt`, each table), as rows
//! lost; an `UPDATE` is not, nor what runs inside a `DO` block or a
//! function.
//!
//! The last snapshot is read for the types of the columns whose type
//! changes and no more ([`snapshot_types`]): its tables one at a time, each
//! dropped once the wanted columns' types are taken from it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use pg_query::NodeEnum;
use pg_query::protobuf::node::Node as N;
use pg_query::protobuf::{AlterTableType, Node, ObjectType, Token, TypeName, a_const};
use serde::Deserialize;
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, Visitor};
use serde_json::{Value, json};

use crate::migration_text::Parsed;

/// A column, by schema, table and name.
pub(super) type ColumnKey = (String, String, String);

/// A type as far as the rules of "wider" go. Every other type is not
/// recognised: no change from or to it is wider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Ty {
    SmallInt,
    Integer,
    BigInt,
    Real,
    Double,
    /// `varchar(n)`, or `varchar` without a length.
    Varchar(Option<i64>),
    /// `char(n)`.
    Char(i64),
    Text,
    /// `numeric(p, s)` (`numeric(p)` is `numeric(p, 0)`), or `numeric`
    /// without precision.
    Numeric(Option<(i64, i64)>),
}

/// Whether a column of type `from` changed to `to` keeps every value it
/// holds, as "Wider" lists it, exactly: `smallint` to `integer` or
/// `bigint`; `integer` to `bigint`; `real` to `double precision`;
/// `varchar(n)` to `varchar(m)` with m ≥ n, to `varchar` or to `text`;
/// `char(n)` to `text`; `numeric(p,s)` to `numeric(p2,s2)` with s2 ≥ s and
/// p2 − s2 ≥ p − s, or to `numeric`.
pub(super) fn is_wider(from: Ty, to: Ty) -> bool {
    match (from, to) {
        (Ty::SmallInt, Ty::Integer | Ty::BigInt)
        | (Ty::Integer, Ty::BigInt)
        | (Ty::Real, Ty::Double)
        | (Ty::Varchar(Some(_)), Ty::Varchar(None) | Ty::Text)
        | (Ty::Char(_), Ty::Text)
        | (Ty::Numeric(Some(_)), Ty::Numeric(None)) => true,
        (Ty::Varchar(Some(n)), Ty::Varchar(Some(m))) => m >= n,
        (Ty::Numeric(Some((p, s))), Ty::Numeric(Some((p2, s2)))) => {
            s2 >= s && p2.saturating_sub(s2) >= p.saturating_sub(s)
        }
        _ => false,
    }
}

/// The type a parsed type name is, when it is one of [`Ty`]: a type of
/// `pg_catalog` (as the parser names `integer`, `varchar(20)`, `double
/// precision`, `numeric(10, 2)`, `char(5)` ...) or `text`, with no array
/// bounds, `SETOF` or `%TYPE`, and integer modifiers that fit it.
pub(super) fn ty(name: &TypeName) -> Option<Ty> {
    if name.setof || name.pct_type || !name.array_bounds.is_empty() {
        return None;
    }
    let names: Vec<&str> = name
        .names
        .iter()
        .map(|n| match &n.node {
            Some(N::String(s)) => Some(s.sval.as_str()),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let base = match names.as_slice() {
        ["pg_catalog", base] => *base,
        ["text"] => "text",
        _ => return None,
    };
    let mods: Vec<i64> = name
        .typmods
        .iter()
        .map(|m| match &m.node {
            Some(N::AConst(c)) => match &c.val {
                Some(a_const::Val::Ival(i)) => Some(i64::from(i.ival)),
                _ => None,
            },
            _ => None,
        })
        .collect::<Option<_>>()?;
    Some(match (base, mods.as_slice()) {
        ("int2", []) => Ty::SmallInt,
        ("int4", []) => Ty::Integer,
        ("int8", []) => Ty::BigInt,
        ("float4", []) => Ty::Real,
        ("float8", []) => Ty::Double,
        ("varchar", []) => Ty::Varchar(None),
        ("varchar", [n]) => Ty::Varchar(Some(*n)),
        ("bpchar", [n]) => Ty::Char(*n),
        ("text", []) => Ty::Text,
        ("numeric", []) => Ty::Numeric(None),
        ("numeric", [p]) => Ty::Numeric(Some((*p, 0))),
        ("numeric", [p, s]) => Ty::Numeric(Some((*p, *s))),
        _ => return None,
    })
}

/// The longest type text read from a snapshot.
const MAX_TYPE_TEXT: usize = 200;

/// The type a snapshot writes (`numeric(10, 2)`, `varchar(20)`, `double
/// precision`), read by the same parser as the statements: as the type of
/// `ALTER TABLE t ALTER COLUMN c TYPE <text>`, which must be that one
/// statement with that one command and nothing else. Only letters,
/// digits, spaces, parentheses and commas are taken, so that nothing in
/// the text can be read as anything but a type.
pub(super) fn ty_of_text(text: &str) -> Option<Ty> {
    if text.is_empty()
        || text.len() > MAX_TYPE_TEXT
        || !text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '(' | ')' | ','))
    {
        return None;
    }
    let parsed = pg_query::parse(&format!("ALTER TABLE t ALTER COLUMN c TYPE {text}"), 0).ok()?;
    let [raw] = parsed.protobuf.stmts.as_slice() else {
        return None;
    };
    let Some(NodeEnum::AlterTableStmt(alter)) = raw.stmt.as_ref().and_then(|s| s.node.as_ref())
    else {
        return None;
    };
    let [cmd] = alter.cmds.as_slice() else {
        return None;
    };
    let Some(N::AlterTableCmd(cmd)) = &cmd.node else {
        return None;
    };
    if cmd.subtype() != AlterTableType::AtAlterColumnType {
        return None;
    }
    let Some(N::ColumnDef(def)) = cmd.def.as_ref().and_then(|d| d.node.as_ref()) else {
        return None;
    };
    if def.raw_default.is_some() || def.coll_clause.is_some() {
        return None;
    }
    ty(def.type_name.as_ref()?)
}

/// What a statement destroys, before the types are compared.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Found {
    /// A table dropped: `<schema>.<table>`.
    Table(String),
    /// A column dropped.
    Column {
        schema: String,
        table: String,
        column: String,
    },
    /// A column's type changed: to `to` as the statement writes it, which
    /// is `to_ty` when recognised.
    Type {
        schema: String,
        table: String,
        column: String,
        to: String,
        to_ty: Option<Ty>,
    },
    /// A schema dropped.
    Schema(String),
    /// Rows deleted or truncated: `<schema>.<table>`.
    Rows(String),
}

/// What the statements of a migration may destroy, each with the index of
/// its statement, in the order of the statements.
pub(super) fn found(sql: &str, statements: &[Parsed]) -> Vec<(usize, Found)> {
    let mut found = Vec::new();
    for (index, statement) in statements.iter().enumerate() {
        match &statement.node {
            Some(NodeEnum::DropStmt(drop)) => {
                let kind = drop.remove_type();
                for object in &drop.objects {
                    let names = name_parts(object);
                    if kind == ObjectType::ObjectTable
                        && let Some((schema, table)) = qualified(&names)
                    {
                        found.push((index, Found::Table(format!("{schema}.{table}"))));
                    } else if kind == ObjectType::ObjectSchema
                        && let [schema] = names.as_slice()
                    {
                        found.push((index, Found::Schema((*schema).to_owned())));
                    }
                }
            }
            Some(NodeEnum::AlterTableStmt(alter)) if alter.objtype() == ObjectType::ObjectTable => {
                let Some(relation) = &alter.relation else {
                    continue;
                };
                let schema = schema_or_public(&relation.schemaname);
                for cmd in &alter.cmds {
                    let Some(N::AlterTableCmd(cmd)) = &cmd.node else {
                        continue;
                    };
                    match cmd.subtype() {
                        AlterTableType::AtDropColumn => found.push((
                            index,
                            Found::Column {
                                schema: schema.to_owned(),
                                table: relation.relname.clone(),
                                column: cmd.name.clone(),
                            },
                        )),
                        AlterTableType::AtAlterColumnType => {
                            let type_name = match cmd.def.as_ref().and_then(|d| d.node.as_ref()) {
                                Some(N::ColumnDef(def)) => def.type_name.as_ref(),
                                _ => None,
                            };
                            found.push((
                                index,
                                Found::Type {
                                    schema: schema.to_owned(),
                                    table: relation.relname.clone(),
                                    column: cmd.name.clone(),
                                    to: type_name
                                        .and_then(|t| type_text(sql, statement, t))
                                        .unwrap_or_else(|| statement_text(sql, statement)),
                                    to_ty: type_name.and_then(ty),
                                },
                            ));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    found
}

/// What the statements of a migration delete or truncate, each with the
/// index of its statement, in the order of the statements and within one
/// in the order of its text: a `TRUNCATE` at the top of a statement, and a
/// `DELETE` at its top or in its `WITH` (nested `WITH`s too), found on the
/// parsed nodes.
fn rows_found(statements: &[Parsed]) -> Vec<(usize, Found)> {
    let mut found = Vec::new();
    let table = |relation: &pg_query::protobuf::RangeVar| {
        (
            relation.location,
            Found::Rows(format!(
                "{}.{}",
                schema_or_public(&relation.schemaname),
                relation.relname
            )),
        )
    };
    for (index, statement) in statements.iter().enumerate() {
        let mut here = Vec::new();
        if let Some(NodeEnum::TruncateStmt(truncate)) = &statement.node {
            for relation in &truncate.relations {
                if let Some(N::RangeVar(relation)) = &relation.node {
                    here.push(table(relation));
                }
            }
        }
        // The statement and the queries of its `WITH`s. Bounded by the
        // statement's tree, which the parser bounds.
        let mut pending: Vec<&NodeEnum> = statement.node.iter().collect();
        while let Some(node) = pending.pop() {
            let with = match node {
                NodeEnum::DeleteStmt(delete) => {
                    if let Some(relation) = &delete.relation {
                        here.push(table(relation));
                    }
                    delete.with_clause.as_ref()
                }
                NodeEnum::SelectStmt(select) => select.with_clause.as_ref(),
                NodeEnum::InsertStmt(insert) => insert.with_clause.as_ref(),
                NodeEnum::UpdateStmt(update) => update.with_clause.as_ref(),
                NodeEnum::MergeStmt(merge) => merge.with_clause.as_ref(),
                _ => None,
            };
            for cte in with.iter().flat_map(|with| &with.ctes) {
                if let Some(N::CommonTableExpr(cte)) = &cte.node
                    && let Some(query) = cte.ctequery.as_ref().and_then(|q| q.node.as_ref())
                {
                    pending.push(query);
                }
            }
        }
        here.sort_by_key(|(at, _)| *at);
        found.extend(here.into_iter().map(|(_, rows)| (index, rows)));
    }
    found
}

/// What one migration destroys, decided on its text.
pub(super) struct Decision {
    /// Its statements, as the migration has them.
    pub statements: Vec<String>,
    /// What each statement destroys, by the statement's index, in order.
    pub destroyed: Vec<(usize, Destroyed)>,
}

/// What the migration `text`, whose statements are `statements`, destroys:
/// [`found`], and with `rows` also what it deletes or truncates; a change
/// of type compared with the types of the snapshot before it, which
/// `previous_snapshot` reads (only when a type changes; none when there is
/// none).
pub(super) fn decide(
    text: &str,
    statements: &[Parsed],
    previous_snapshot: impl FnOnce() -> Option<Vec<u8>>,
    rows: bool,
) -> Decision {
    let mut all = found(text, statements);
    if rows {
        all.extend(rows_found(statements));
        // Stable: the order of the statements, and within one its own.
        all.sort_by_key(|(index, _)| *index);
    }
    let wanted: Vec<(&str, &str, &str)> = all
        .iter()
        .filter_map(|(_, f)| match f {
            Found::Type {
                schema,
                table,
                column,
                ..
            } => Some((schema.as_str(), table.as_str(), column.as_str())),
            _ => None,
        })
        .collect();
    // The last snapshot before this migration: the types it had.
    let from = if wanted.is_empty() {
        BTreeMap::new()
    } else {
        previous_snapshot()
            .map(|bytes| snapshot_types(&bytes, wanted.iter().copied()))
            .unwrap_or_default()
    };
    let destroyed = destroyed(all, &from);
    let statements = statements.iter().map(|s| statement_text(text, s)).collect();
    Decision {
        statements,
        destroyed,
    }
}

/// What would be lost, in words: each thing, joined.
pub(super) fn lost(destroyed: &[&Destroyed]) -> String {
    destroyed
        .iter()
        .map(|d| d.words.as_str())
        .collect::<Vec<_>>()
        .join("; ")
}

/// What to do about data that would be lost (`lost`, in words): the words
/// `vm generate` and `vm deploy` stop with, `command` being the one to run
/// again.
pub(super) fn ask_the_builder(lost: &str, command: &str) -> String {
    format!(
        "Applied, it would lose: {lost}. Deploying an older Version later does not bring it back. Ask the Builder, telling them exactly what would be lost, unless the Builder has already clearly said that this data may be lost; then run the command again with --allow-data-loss ({command} --allow-data-loss)"
    )
}

/// The destroying statements among `statements`, each once, in order.
pub(super) fn destroying<'a>(
    destroyed: &[(usize, Destroyed)],
    statements: &'a [String],
) -> Vec<&'a str> {
    let mut indexes: Vec<usize> = destroyed.iter().map(|(i, _)| *i).collect();
    indexes.dedup();
    indexes
        .iter()
        .filter_map(|i| statements.get(*i).map(String::as_str))
        .collect()
}

/// A statement in words: each of its lines indented, escaped as any text
/// of the project's.
pub(super) fn push_statement(text: &mut String, statement: &str) {
    for line in statement.lines() {
        text.push_str("    ");
        text.push_str(&super::theirs::Theirs::from(line.replace('\t', "  ").as_str()).to_string());
        text.push('\n');
    }
}

/// `public` for a name given without its schema.
fn schema_or_public(schema: &str) -> &str {
    if schema.is_empty() { "public" } else { schema }
}

/// The names of a dropped object: a list of names (`schema`, `table`), or
/// one name.
fn name_parts(object: &Node) -> Vec<&str> {
    match &object.node {
        Some(N::List(list)) => list
            .items
            .iter()
            .filter_map(|item| match &item.node {
                Some(N::String(s)) => Some(s.sval.as_str()),
                _ => None,
            })
            .collect(),
        Some(N::String(s)) => vec![s.sval.as_str()],
        _ => Vec::new(),
    }
}

/// A table's schema and name from its names: `table`, `schema.table` or
/// `catalog.schema.table`.
fn qualified<'a>(names: &[&'a str]) -> Option<(&'a str, &'a str)> {
    match names {
        [table] => Some(("public", table)),
        [schema, table] | [_, schema, table] => Some((schema, table)),
        _ => None,
    }
}

/// The statement as the migration has it: without the white space around
/// it, with the semicolon that ends it.
pub(super) fn statement_text(sql: &str, statement: &Parsed) -> String {
    let text = sql.get(statement.range.clone()).unwrap_or_default().trim();
    let ended = sql
        .get(statement.range.end..)
        .is_some_and(|rest| rest.trim_start().starts_with(';'));
    if !text.ends_with(';') && ended {
        format!("{text};")
    } else {
        text.to_owned()
    }
}

/// The type of an `ALTER COLUMN ... TYPE` as the statement writes it: its
/// tokens from where the parser says it begins up to what ends it (a
/// comma, a semicolon, `USING` or `COLLATE`, outside parentheses and
/// brackets).
fn type_text(sql: &str, statement: &Parsed, name: &TypeName) -> Option<String> {
    let at = statement
        .part_start
        .checked_add(usize::try_from(name.location).ok()?)?;
    let region = sql.get(at..statement.range.end)?;
    let scanned = pg_query::scan(region).ok()?;
    let mut depth = 0_u32;
    let mut end = None;
    for token in &scanned.tokens {
        let kind = token.token();
        match kind {
            Token::Ascii40 | Token::Ascii91 => depth = depth.saturating_add(1),
            Token::Ascii41 | Token::Ascii93 => depth = depth.saturating_sub(1),
            Token::Ascii44 | Token::Ascii59 | Token::Using | Token::Collate if depth == 0 => break,
            _ => {}
        }
        end = usize::try_from(token.end).ok();
    }
    Some(region.get(..end?)?.trim().to_owned())
}

/// A column's type as a snapshot has it.
#[derive(Deserialize)]
struct SnapshotColumn {
    name: String,
    #[serde(rename = "type")]
    ty: String,
    /// Set for a type of the project's own (an enum), which is never one
    /// of [`Ty`] whatever its name.
    #[serde(rename = "typeSchema", default)]
    type_schema: Option<IgnoredAny>,
}

#[derive(Deserialize)]
struct SnapshotTable {
    name: String,
    /// Empty for `public`.
    #[serde(default)]
    schema: String,
    #[serde(default)]
    columns: BTreeMap<String, SnapshotColumn>,
}

/// The types, as the snapshot `bytes` writes them, of the columns
/// `wanted`; a column it does not have, or a snapshot it cannot read, gives
/// none. A column whose type is the project's own (with a `typeSchema`) is
/// given as no type.
pub(super) fn snapshot_types<'w>(
    bytes: &[u8],
    wanted: impl IntoIterator<Item = (&'w str, &'w str, &'w str)>,
) -> BTreeMap<ColumnKey, String> {
    let mut by_table: Wanted<'_> = BTreeMap::new();
    for (schema, table, column) in wanted {
        by_table.entry((schema, table)).or_default().insert(column);
    }
    let mut types = BTreeMap::new();
    let mut reader = serde_json::Deserializer::from_slice(bytes);
    let read = Snapshot {
        wanted: &by_table,
        types: &mut types,
    }
    .deserialize(&mut reader);
    if read.is_err() {
        types.clear();
    }
    types
}

/// The columns wanted, by schema and table.
type Wanted<'a> = BTreeMap<(&'a str, &'a str), BTreeSet<&'a str>>;

/// The snapshot's top object: only `tables` is read.
struct Snapshot<'a> {
    wanted: &'a Wanted<'a>,
    types: &'a mut BTreeMap<ColumnKey, String>,
}

impl<'de> DeserializeSeed<'de> for Snapshot<'_> {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for Snapshot<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a drizzle-kit snapshot")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let Self { wanted, types } = self;
        while let Some(key) = map.next_key::<String>()? {
            if key == "tables" {
                map.next_value_seed(Tables {
                    wanted,
                    types: &mut *types,
                })?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

/// The snapshot's `tables`, read one table at a time.
struct Tables<'a> {
    wanted: &'a Wanted<'a>,
    types: &'a mut BTreeMap<ColumnKey, String>,
}

impl<'de> DeserializeSeed<'de> for Tables<'_> {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for Tables<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the tables of a drizzle-kit snapshot")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let Self { wanted, types } = self;
        while map.next_key::<IgnoredAny>()?.is_some() {
            let table: SnapshotTable = map.next_value()?;
            let schema = schema_or_public(&table.schema);
            let Some(columns) = wanted.get(&(schema, table.name.as_str())) else {
                continue;
            };
            for column in table.columns.into_values() {
                if columns.contains(column.name.as_str()) && column.type_schema.is_none() {
                    // Owned for the key: only the few columns wanted.
                    let key = (schema.to_owned(), table.name.clone(), column.name);
                    types.insert(key, column.ty);
                }
            }
        }
        Ok(())
    }
}

/// One thing a migration destroys: as `destroys` lists it, and in words.
pub(super) struct Destroyed {
    pub json: Value,
    pub words: String,
}

/// What `found` destroys, with the types the last snapshot has (`from`),
/// each with the index of its statement: every drop, and every change of
/// type that is not wider.
pub(super) fn destroyed(
    found: Vec<(usize, Found)>,
    from: &BTreeMap<ColumnKey, String>,
) -> Vec<(usize, Destroyed)> {
    let mut destroyed = Vec::new();
    for (index, found) in found {
        let one = match found {
            Found::Table(table) => Destroyed {
                words: format!("the table {table} and every row in it"),
                json: json!({ "what": "table", "table": table }),
            },
            Found::Column {
                schema,
                table,
                column,
            } => {
                let table = format!("{schema}.{table}");
                Destroyed {
                    words: format!(
                        "the column {column} of the table {table} and every value in it"
                    ),
                    json: json!({ "what": "column", "table": table, "column": column }),
                }
            }
            Found::Type {
                schema,
                table,
                column,
                to,
                to_ty,
            } => {
                let key = (schema, table, column);
                let old = from.get(&key);
                let wider = match (old.and_then(|t| ty_of_text(t)), to_ty) {
                    (Some(old), Some(new)) => is_wider(old, new),
                    _ => false,
                };
                if wider {
                    continue;
                }
                let (schema, table, column) = key;
                let table = format!("{schema}.{table}");
                let was = old.map_or_else(
                    || "its type now is not in the last snapshot".to_owned(),
                    |old| format!("it is {old} now"),
                );
                Destroyed {
                    words: format!(
                        "the values of the column {column} of the table {table} that do not fit its new type {to} ({was}, and {to} is not wider: such values are cut, or the migration fails)"
                    ),
                    json: json!({ "what": "type", "table": table, "column": column, "from": old, "to": to }),
                }
            }
            Found::Schema(schema) => Destroyed {
                words: format!("the schema {schema} and everything in it"),
                json: json!({ "what": "schema", "schema": schema }),
            },
            Found::Rows(table) => Destroyed {
                words: format!("rows of the table {table} (it deletes or truncates them)"),
                json: json!({ "what": "rows", "table": table }),
            },
        };
        destroyed.push((index, one));
    }
    destroyed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration_text::parsed;

    fn t(text: &str) -> Ty {
        ty_of_text(text).unwrap_or_else(|| panic!("not recognised: {text}"))
    }

    fn wider(from: &str, to: &str) -> bool {
        is_wider(t(from), t(to))
    }

    #[test]
    fn types_are_recognised_as_the_snapshot_and_the_statement_write_them() {
        for (text, want) in [
            ("smallint", Ty::SmallInt),
            ("integer", Ty::Integer),
            ("INT", Ty::Integer),
            ("bigint", Ty::BigInt),
            ("real", Ty::Real),
            ("double precision", Ty::Double),
            ("DOUBLE PRECISION", Ty::Double),
            ("varchar(20)", Ty::Varchar(Some(20))),
            ("VARCHAR(20)", Ty::Varchar(Some(20))),
            ("character varying(20)", Ty::Varchar(Some(20))),
            ("varchar", Ty::Varchar(None)),
            ("character varying", Ty::Varchar(None)),
            ("char(5)", Ty::Char(5)),
            ("character(5)", Ty::Char(5)),
            ("text", Ty::Text),
            ("TEXT", Ty::Text),
            ("numeric(10, 2)", Ty::Numeric(Some((10, 2)))),
            ("numeric(10,2)", Ty::Numeric(Some((10, 2)))),
            ("NUMERIC(8, 2)", Ty::Numeric(Some((8, 2)))),
            ("decimal(10, 2)", Ty::Numeric(Some((10, 2)))),
            ("numeric(10)", Ty::Numeric(Some((10, 0)))),
            ("numeric", Ty::Numeric(None)),
        ] {
            assert_eq!(ty_of_text(text), Some(want), "{text}");
        }
    }

    #[test]
    fn what_is_not_recognised_is_none() {
        for text in [
            "",
            "serial",
            "bigserial",
            "int4",
            "int8",
            "integer[]",
            "text[]",
            "timestamp",
            "uuid",
            "jsonb",
            "mood",
            "\"integer\"",
            "public.text",
            "integer USING 1",
            "integer, DROP COLUMN x",
            "integer; DROP TABLE x",
            "integer -- x",
            "varchar(20) COLLATE \"C\"",
            "bit(3)",
            "interval",
        ] {
            assert_eq!(ty_of_text(text), None, "{text:?}");
        }
        assert_eq!(ty_of_text(&format!("varchar({})", "9".repeat(300))), None);
    }

    #[test]
    fn every_row_of_wider_is_wider() {
        for (from, to) in [
            ("smallint", "integer"),
            ("smallint", "bigint"),
            ("integer", "bigint"),
            ("real", "double precision"),
            ("varchar(20)", "varchar(50)"),
            ("varchar(20)", "varchar(20)"),
            ("varchar(20)", "varchar"),
            ("varchar(20)", "character varying"),
            ("varchar(20)", "text"),
            ("char(5)", "text"),
            ("numeric(10, 2)", "numeric(12, 3)"),
            ("numeric(10, 2)", "numeric(11, 3)"),
            ("numeric(10, 2)", "numeric(10, 2)"),
            ("numeric(10)", "numeric(12, 2)"),
            ("numeric(10, 2)", "numeric"),
        ] {
            assert!(wider(from, to), "{from} to {to}");
        }
    }

    #[test]
    fn every_other_change_is_not_wider() {
        for (from, to) in [
            ("integer", "smallint"),
            ("bigint", "integer"),
            ("bigint", "smallint"),
            ("integer", "integer"),
            ("smallint", "real"),
            ("integer", "double precision"),
            ("integer", "numeric"),
            ("integer", "text"),
            ("double precision", "real"),
            ("real", "numeric"),
            ("text", "varchar(10)"),
            ("text", "varchar"),
            ("text", "char(5)"),
            ("varchar(50)", "varchar(20)"),
            ("varchar", "varchar(20)"),
            ("varchar", "text"),
            ("varchar(20)", "char(20)"),
            ("char(5)", "char(10)"),
            ("char(5)", "varchar(10)"),
            ("char(5)", "varchar"),
            ("numeric(10, 2)", "numeric(8, 2)"),
            ("numeric(10, 2)", "numeric(10, 3)"),
            ("numeric(10, 2)", "numeric(10, 1)"),
            ("numeric(10, 2)", "numeric(9, 0)"),
            ("numeric(10, 2)", "integer"),
            ("numeric", "numeric(10, 2)"),
            ("numeric(10, 2)", "double precision"),
        ] {
            assert!(!wider(from, to), "{from} to {to}");
        }
    }

    fn found_in(sql: &str) -> Vec<(usize, Found)> {
        found(sql, &parsed(sql).unwrap())
    }

    #[test]
    fn drops_are_found_on_the_parsed_statements() {
        let sql = "ALTER TABLE \"tags\" DISABLE ROW LEVEL SECURITY;--> statement-breakpoint\nDROP TABLE \"tags\" CASCADE;--> statement-breakpoint\nDROP TABLE \"archive\".\"old\" CASCADE;--> statement-breakpoint\nDROP SCHEMA \"archive\";\n--> statement-breakpoint\nALTER TABLE \"notes\" DROP COLUMN \"title\";";
        assert_eq!(
            found_in(sql),
            [
                (1, Found::Table("public.tags".into())),
                (2, Found::Table("archive.old".into())),
                (3, Found::Schema("archive".into())),
                (
                    4,
                    Found::Column {
                        schema: "public".into(),
                        table: "notes".into(),
                        column: "title".into()
                    }
                ),
            ]
        );
        assert_eq!(
            found_in("DROP TABLE a, s.b; ALTER TABLE ONLY x DROP COLUMN p, DROP COLUMN q"),
            [
                (0, Found::Table("public.a".into())),
                (0, Found::Table("s.b".into())),
                (
                    1,
                    Found::Column {
                        schema: "public".into(),
                        table: "x".into(),
                        column: "p".into()
                    }
                ),
                (
                    1,
                    Found::Column {
                        schema: "public".into(),
                        table: "x".into(),
                        column: "q".into()
                    }
                ),
            ]
        );
    }

    #[test]
    fn the_words_elsewhere_destroy_nothing() {
        for sql in [
            "ALTER TABLE \"notes\" ADD COLUMN \"hint\" text DEFAULT 'DROP TABLE notes; ALTER TABLE notes DROP COLUMN body';",
            "-- DROP TABLE notes;\nSELECT 1;",
            "/* DROP SCHEMA x */ CREATE TABLE \"drop\" (\"column\" int);",
            "DROP VIEW v; DROP INDEX i; ALTER TABLE t ALTER COLUMN c DROP DEFAULT; ALTER TABLE t ALTER COLUMN c SET NOT NULL; ALTER TABLE t DROP CONSTRAINT k",
            "DROP TYPE mood; ALTER TABLE t RENAME COLUMN a TO b",
            "CREATE FUNCTION f() RETURNS void LANGUAGE sql AS $$ DROP TABLE x $$",
        ] {
            assert_eq!(found_in(sql), [], "{sql}");
        }
    }

    #[test]
    fn a_change_of_type_is_found_with_its_type_as_written() {
        let sql = "ALTER TABLE \"notes\" ALTER COLUMN \"v\" SET DATA TYPE numeric(8, 2);\n--> statement-breakpoint\nALTER TABLE \"s\".\"t\" ALTER COLUMN \"c\" TYPE character varying(20) USING c::text, ALTER COLUMN \"d\" TYPE int[];";
        assert_eq!(
            found_in(sql),
            [
                (
                    0,
                    Found::Type {
                        schema: "public".into(),
                        table: "notes".into(),
                        column: "v".into(),
                        to: "numeric(8, 2)".into(),
                        to_ty: Some(Ty::Numeric(Some((8, 2)))),
                    }
                ),
                (
                    1,
                    Found::Type {
                        schema: "s".into(),
                        table: "t".into(),
                        column: "c".into(),
                        to: "character varying(20)".into(),
                        to_ty: Some(Ty::Varchar(Some(20))),
                    }
                ),
                (
                    1,
                    Found::Type {
                        schema: "s".into(),
                        table: "t".into(),
                        column: "d".into(),
                        to: "int[]".into(),
                        to_ty: None,
                    }
                ),
            ]
        );
    }

    fn borrowed(wanted: &BTreeSet<ColumnKey>) -> impl Iterator<Item = (&str, &str, &str)> {
        wanted
            .iter()
            .map(|(s, t, c)| (s.as_str(), t.as_str(), c.as_str()))
    }

    fn key(table: &str, column: &str) -> ColumnKey {
        ("public".into(), table.into(), column.into())
    }

    #[test]
    fn the_snapshot_gives_the_wanted_types_only() {
        let snapshot = br#"{ "id": "x", "tables": {
            "public.notes": { "name": "notes", "schema": "", "columns": {
                "v": { "name": "v", "type": "numeric(10, 2)", "primaryKey": false },
                "w": { "name": "w", "type": "integer" },
                "m": { "name": "m", "type": "text", "typeSchema": "public" } } },
            "archive.old": { "name": "old", "schema": "archive", "columns": {
                "v": { "name": "v", "type": "bigint" } } } },
            "enums": {} }"#;
        let wanted: BTreeSet<ColumnKey> = [
            key("notes", "v"),
            key("notes", "m"),
            key("notes", "gone"),
            ("archive".into(), "old".into(), "v".into()),
        ]
        .into();
        let types = snapshot_types(snapshot, borrowed(&wanted));
        assert_eq!(
            types,
            [
                (key("notes", "v"), "numeric(10, 2)".to_owned()),
                (
                    ("archive".into(), "old".into(), "v".into()),
                    "bigint".to_owned()
                ),
            ]
            .into()
        );
        assert!(snapshot_types(b"{ not json", borrowed(&wanted)).is_empty());
        assert!(snapshot_types(b"[]", borrowed(&wanted)).is_empty());
    }

    #[test]
    fn a_change_of_type_destroys_unless_wider_than_the_snapshot_has_it() {
        let sql = "ALTER TABLE \"notes\" ALTER COLUMN \"v\" SET DATA TYPE numeric(12, 3);ALTER TABLE \"notes\" ALTER COLUMN \"w\" SET DATA TYPE smallint;ALTER TABLE \"notes\" ALTER COLUMN \"gone\" SET DATA TYPE bigint;";
        let from: BTreeMap<ColumnKey, String> = [
            (key("notes", "v"), "numeric(10, 2)".to_owned()),
            (key("notes", "w"), "integer".to_owned()),
        ]
        .into();
        let got: Vec<(usize, Value)> = destroyed(found_in(sql), &from)
            .into_iter()
            .map(|(i, d)| (i, d.json))
            .collect();
        assert_eq!(
            got,
            [
                (
                    1,
                    json!({ "what": "type", "table": "public.notes", "column": "w", "from": "integer", "to": "smallint" })
                ),
                (
                    2,
                    json!({ "what": "type", "table": "public.notes", "column": "gone", "from": null, "to": "bigint" })
                ),
            ]
        );
    }

    #[test]
    fn a_statement_is_given_with_its_semicolon_and_without_the_breakpoint() {
        let sql = "DROP TABLE \"archive\".\"old\" CASCADE;--> statement-breakpoint\nDROP SCHEMA \"archive\";\n";
        let statements = parsed(sql).unwrap();
        let texts: Vec<String> = statements.iter().map(|s| statement_text(sql, s)).collect();
        assert_eq!(
            texts,
            [
                "DROP TABLE \"archive\".\"old\" CASCADE;",
                "DROP SCHEMA \"archive\";"
            ]
        );
        let sql = "SELECT 1; SELECT 2";
        let statements = parsed(sql).unwrap();
        let texts: Vec<String> = statements.iter().map(|s| statement_text(sql, s)).collect();
        assert_eq!(texts, ["SELECT 1;", "SELECT 2"]);
    }
}
