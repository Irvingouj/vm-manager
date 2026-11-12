//! The statement check (blackbox/CONTRACT.md, "The statement check";
//! ADR 0013): before Application SQL is sent to a shared database, it is
//! parsed with Postgres's own parser (`pg_query`) and refused if it is
//! something the platform never needs.
//!
//! Interface: [`check`], a statement in, a verdict out: `Ok(())`, or the
//! [`Refused`] reason and a message for the Coding agent.
//!
//! What is allowed: exactly one statement, a `SELECT` (not `SELECT ...
//! INTO`), `INSERT`, `UPDATE` or `DELETE`, with or without `WITH` and
//! `RETURNING`. What is refused, wherever it is in the parse tree: a
//! statement of any other kind (in a `WITH` too), a call of a listed
//! function ([`FUNCTIONS`], [`FUNCTION_FAMILIES`]), a reference to a
//! listed relation ([`RELATIONS`]).
//!
//! The tree is walked whole, not through a list of the places a call or a
//! relation can be: it is turned into JSON (the parser's own node types
//! serialise themselves) and every object in it is looked at. A call is an
//! object with `funcname` and `funcformat` (a `FuncCall`); a relation is an
//! object with `relname` and `relpersistence` (a `RangeVar`), whether the
//! tree holds it as a node or as a field. Names are compared as the parser
//! leaves them, which is how Postgres resolves them: unquoted names are
//! folded to lower case, quotes are gone, a schema is a separate part. A
//! function's or relation's own name is compared, whatever schema
//! qualifies it (`dblink` lives where its extension was installed). Names
//! inside strings, comments and aliases are not calls or references, and
//! the parser does not report them as such.
//!
//! This is hygiene, not the boundary (docs/research/what-postgres-sees.md):
//! SQL text that the database itself runs (`ts_stat`, a function of the
//! database's own) is not seen here, and a function not on the list may do
//! what a listed one does. What must never happen is held by the login and
//! the transaction (crate::executor).

use serde_json::Value;

use crate::statement::MAX_PARSED_BYTES;

/// Functions refused by name.
pub const FUNCTIONS: [&str; 9] = [
    "set_config",
    "pg_terminate_backend",
    "pg_cancel_backend",
    "pg_notify",
    "pg_reload_conf",
    "pg_read_file",
    "pg_read_binary_file",
    "pg_ls_dir",
    "pg_stat_file",
];

/// Functions refused by the start of their name: `query_to_xml` and its
/// family, `dblink` and its family, the large-object functions.
pub const FUNCTION_FAMILIES: [&str; 3] = ["query_to_xml", "dblink", "lo_"];

/// Relations refused by name.
pub const RELATIONS: [&str; 3] = ["pg_authid", "pg_shadow", "pg_stat_activity"];

/// The statement kinds allowed, as the parser names its nodes.
const KINDS: [&str; 4] = ["SelectStmt", "InsertStmt", "UpdateStmt", "DeleteStmt"];

/// Why the check refused a statement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    SeveralStatements,
    Unparsed,
    KindNotAllowed,
    FunctionNotAllowed,
    RelationNotAllowed,
}

impl Reason {
    /// As Application code receives it.
    pub fn name(self) -> &'static str {
        match self {
            Self::SeveralStatements => "several-statements",
            Self::Unparsed => "unparsed",
            Self::KindNotAllowed => "kind-not-allowed",
            Self::FunctionNotAllowed => "function-not-allowed",
            Self::RelationNotAllowed => "relation-not-allowed",
        }
    }
}

/// A refusal: the reason, and a sentence a Coding agent can act on. The
/// sentence names at most a statement kind, a function or a relation, never
/// a value written in the statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    pub reason: Reason,
    pub message: String,
}

impl Refused {
    fn new(reason: Reason, message: String) -> Self {
        Self { reason, message }
    }
}

/// Whether `sql` may be sent to a shared database.
pub fn check(sql: &str) -> Result<(), Refused> {
    let unparsed = || {
        Refused::new(
            Reason::Unparsed,
            "Postgres's parser cannot read the statement: correct its syntax (statements longer than 1 MiB are not read)".into(),
        )
    };
    if sql.len() > MAX_PARSED_BYTES {
        return Err(unparsed());
    }
    let parsed = pg_query::parse(sql, 0).map_err(|_| unparsed())?;
    match parsed.protobuf.stmts.len() {
        0 => {
            return Err(Refused::new(
                Reason::KindNotAllowed,
                "the SQL holds no statement: send one SELECT, INSERT, UPDATE or DELETE".into(),
            ));
        }
        1 => {}
        _ => {
            return Err(Refused::new(
                Reason::SeveralStatements,
                "the SQL holds several statements: send one statement per operation".into(),
            ));
        }
    }
    // The parser's own node types, as JSON, to be walked whole.
    let tree = serde_json::to_value(&parsed.protobuf).map_err(|_| unparsed())?;
    let top = tree
        .get("stmts")
        .and_then(|s| s.get(0))
        .and_then(|s| s.get("stmt"))
        .and_then(|s| s.get("node"))
        .and_then(Value::as_object)
        .and_then(|node| node.keys().next())
        .ok_or_else(unparsed)?;
    if !KINDS.contains(&top.as_str()) {
        return Err(kind_not_allowed(top));
    }
    walk(&tree)
}

/// Looks at every object in `tree`, outermost first.
fn walk(tree: &Value) -> Result<(), Refused> {
    // Bounded by the tree, which is bounded by the statement's length and
    // the depth the parser's output may have.
    let mut pending = vec![tree];
    while let Some(value) = pending.pop() {
        match value {
            Value::Object(object) => {
                if let Some(name) = function_name(object) {
                    refuse_function(name)?;
                }
                if let Some(name) = object.get("relname").and_then(Value::as_str)
                    && object.contains_key("relpersistence")
                {
                    refuse_relation(name)?;
                }
                for (key, child) in object {
                    // A node's kind is its key: `SelectStmt`, `MergeStmt`.
                    if key.ends_with("Stmt") && key.starts_with(|c: char| c.is_ascii_uppercase()) {
                        if !KINDS.contains(&key.as_str()) {
                            return Err(kind_not_allowed(key));
                        }
                        if key == "SelectStmt"
                            && child.get("into_clause").is_some_and(|into| !into.is_null())
                        {
                            return Err(kind_not_allowed("SelectInto"));
                        }
                    }
                    pending.push(child);
                }
            }
            Value::Array(items) => pending.extend(items),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    Ok(())
}

/// The name a call calls, if `object` is a call: the last part of its
/// `funcname` (the parts before it are the schema).
fn function_name(object: &serde_json::Map<String, Value>) -> Option<&str> {
    if !object.contains_key("funcformat") {
        return None;
    }
    object
        .get("funcname")?
        .as_array()?
        .last()?
        .get("node")?
        .get("String")?
        .get("sval")?
        .as_str()
}

fn refuse_function(name: &str) -> Result<(), Refused> {
    let listed = FUNCTIONS.contains(&name)
        || FUNCTION_FAMILIES
            .iter()
            .any(|family| name.starts_with(family));
    if listed {
        return Err(Refused::new(
            Reason::FunctionNotAllowed,
            format!(
                "the statement calls {name}, which Application SQL may not call on a shared database: remove the call"
            ),
        ));
    }
    Ok(())
}

fn refuse_relation(name: &str) -> Result<(), Refused> {
    if RELATIONS.contains(&name) {
        return Err(Refused::new(
            Reason::RelationNotAllowed,
            format!(
                "the statement reads {name}, which Application SQL may not read on a shared database: remove the reference"
            ),
        ));
    }
    Ok(())
}

/// The refusal of a statement of the parser's kind `kind`.
fn kind_not_allowed(kind: &str) -> Refused {
    let kind = match kind {
        "SelectInto" => "SELECT ... INTO".to_owned(),
        other => {
            let kind = other.strip_suffix("Stmt").unwrap_or(other);
            kind.to_owned()
        }
    };
    Refused::new(
        Reason::KindNotAllowed,
        format!(
            "a statement of the kind {kind} is not allowed: Application SQL is one SELECT, INSERT, UPDATE or DELETE (with or without WITH and RETURNING)"
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason(sql: &str) -> Option<Reason> {
        check(sql).err().map(|refused| refused.reason)
    }

    #[test]
    fn the_four_kinds_run_in_their_forms() {
        for sql in [
            "select 1",
            "select id from incidents where id = $1",
            "with a as (select 1) select * from a",
            "with d as (delete from t returning id) insert into u select id from d returning id",
            "insert into t (a) values ($1) on conflict do nothing returning a",
            "update t set a = 1 from u where u.id = t.id returning t.a",
            "delete from t using u where u.id = t.id",
            "values (1), (2)",
            "table incidents",
            "select * from incidents for update",
            "select 'set_config(1) from pg_authid' as x /* pg_shadow */ -- lo_import",
            "select 1 as pg_authid, 2 as set_config from incidents pg_shadow",
            "select \"SET_CONFIG\"('a', 'b', false)",
        ] {
            assert_eq!(check(sql), Ok(()), "{sql}");
        }
    }

    #[test]
    fn several_statements_and_unparsed_ones_are_refused() {
        assert_eq!(
            reason("select 1; select 2"),
            Some(Reason::SeveralStatements)
        );
        assert_eq!(
            reason("select 1;; select 2"),
            Some(Reason::SeveralStatements)
        );
        assert_eq!(
            reason("select ';'; drop table t"),
            Some(Reason::SeveralStatements)
        );
        assert_eq!(reason("selec 1"), Some(Reason::Unparsed));
        assert_eq!(reason("select 'open"), Some(Reason::Unparsed));
        assert_eq!(reason(""), Some(Reason::KindNotAllowed));
        assert_eq!(
            reason("/* only */ -- comments"),
            Some(Reason::KindNotAllowed)
        );
        let long = format!("select {}1", "1 + ".repeat(MAX_PARSED_BYTES / 4));
        assert_eq!(reason(&long), Some(Reason::Unparsed));
    }

    #[test]
    fn other_kinds_are_refused_also_inside_with() {
        for sql in [
            "create table t (x int)",
            "set role none",
            "reset all",
            "begin",
            "commit",
            "copy t to stdout",
            "explain select 1",
            "do $$ begin end $$",
            "select 1 into t",
            "select * into temp t from u",
            "merge into t using u on t.id = u.id when matched then delete",
            "with m as (merge into t using u on t.id = u.id when matched then delete returning t.id) select * from m",
            "discard all",
            "listen c",
            "prepare p as select 1",
        ] {
            assert_eq!(reason(sql), Some(Reason::KindNotAllowed), "{sql}");
        }
    }

    #[test]
    fn listed_functions_are_refused_wherever_and_however_written() {
        for sql in [
            "select set_config('a', 'b', false)",
            "select SET_CONFIG('a', 'b', false)",
            "select \"set_config\"('a', 'b', false)",
            "select PG_CATALOG.set_config('a', 'b', false)",
            "select pg_catalog . /* x */ set_config ('a', 'b', false)",
            "select * from set_config('a', 'b', false)",
            "select * from (values (set_config('a', 'b', false))) v(x)",
            "select array[set_config('a', 'b', false)], row(set_config('a', 'b', false))",
            "select case when true then set_config('a', 'b', false) end",
            "select 1 from t order by pg_notify('c', 'p')",
            "select x from t group by x having max(pg_cancel_backend(0)) is not null",
            "insert into t (a) values ('x') returning pg_terminate_backend(0)",
            "update t set a = pg_read_file('f')",
            "delete from t where a = pg_ls_dir('.')",
            "with c as (select pg_stat_file('f')) select * from c",
            "insert into t (a) values ('x') on conflict (a) do update set a = pg_reload_conf()::text",
            "select query_to_xml_and_xmlschema('select 1', true, true, '')",
            "select public.dblink_exec('c', 'select 1')",
            "select lo_from_bytea(0, 'x')",
            "select pg_read_binary_file('f')",
            "select exists (select 1 from t where pg_notify('c', 'p') is null)",
        ] {
            assert_eq!(reason(sql), Some(Reason::FunctionNotAllowed), "{sql}");
        }
        let refused = check("select pg_catalog.set_config('secret-value', 'x', false)")
            .err()
            .map(|r| r.message)
            .unwrap_or_default();
        assert!(refused.contains("set_config"), "{refused}");
        assert!(!refused.contains("secret-value"), "{refused}");
    }

    #[test]
    fn listed_relations_are_refused_wherever_and_however_written() {
        for sql in [
            "select * from pg_authid",
            "select * from PG_CATALOG.PG_SHADOW",
            "select * from \"pg_catalog\".\"pg_stat_activity\"",
            "table pg_authid",
            "select 1 from t join pg_authid a on true",
            "select (select count(*) from pg_shadow)",
            "select 1 from t union all select 1 from pg_stat_activity",
            "insert into t select 'x' from pg_authid",
            "update t set a = 'x' from pg_authid",
            "delete from t using pg_stat_activity a where a.pid = t.id",
            "insert into t (a) values ('x') returning (select count(*) from pg_shadow)",
        ] {
            assert_eq!(reason(sql), Some(Reason::RelationNotAllowed), "{sql}");
        }
    }
}
