//! The statement check (CONTRACT.md, "The statement check"; ticket "Write
//! access, and the database role as the boundary", sixth acceptance
//! criterion). Hygiene, not the boundary: the boundary tests never rely on
//! it; these tests are about the check alone.
//!
//! Allowed: exactly one `SELECT`, `INSERT`, `UPDATE` or `DELETE`, with or
//! without `WITH` and `RETURNING`; every form of those runs (subqueries,
//! joins, casts, parameters), also when it merely mentions a listed name in
//! a string, a comment or an alias. Refused, with its `reason`: several
//! statements, a statement that cannot be parsed, every other kind, a call
//! of a listed function anywhere in the statement, a reference to a listed
//! relation; also when the name is hidden by letter case, quoting, a
//! schema, comments or whitespace. A refusal sends nothing to the shared
//! database (its server log) and has an Operation log entry with outcome
//! `failed`, error `statement`, and no value of the statement in it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::postgres::SUPERUSER;
use blackbox::scenario::{
    Expected, READERS, RESOURCE, Scenario, WRITERS, assert_entry, assert_statement_refused, lines,
    op,
};
use blackbox::shared_database::token;
use serde_json::{Value, json};

fn wanda() -> User {
    User::new("wanda").in_groups(&[READERS, WRITERS])
}

/// A statement the check must refuse with `reason`, sent through
/// `operation`. `%V%` in it is replaced by a value marker that must reach
/// neither the shared database nor the Operation log.
struct Refused {
    operation: &'static str,
    sql: String,
    reason: &'static str,
}

fn refused(operation: &'static str, sql: &str, reason: &'static str) -> Refused {
    Refused {
        operation,
        sql: sql.to_owned(),
        reason,
    }
}

/// Sends every case, twenty to a request, each prefixed by a comment that
/// marks it, and checks the answer, that nothing marked reached the shared
/// database, and the Operation log entry.
fn assert_all_refused(s: &Scenario, cases: &[Refused]) {
    let run = token();
    for (chunk_index, chunk) in cases.chunks(20).enumerate() {
        let sent: Vec<(String, String)> = chunk
            .iter()
            .enumerate()
            .map(|(i, case)| {
                let value = format!("value-{run}-{chunk_index}-{i}");
                let sql = format!(
                    "/* sent-{run}-{chunk_index}-{i} */ {}",
                    case.sql.replace("%V%", &value)
                );
                (value, sql)
            })
            .collect();
        let calls: Vec<Value> = chunk
            .iter()
            .zip(&sent)
            .map(|(case, (_, sql))| {
                op(case.operation, &json!({ "resource": RESOURCE, "sql": sql }))
            })
            .collect();
        let (response, answers) = s.run(&wanda(), "board", &json!({ "calls": calls }));
        let id = response.request_id();
        let entries = s.entries_of(&response);
        for (i, (case, answer)) in chunk.iter().zip(&answers).enumerate() {
            let context = format!("{} of {:?}", case.operation, case.sql);
            assert_statement_refused(answer, case.reason, &context);
            s.db.assert_not_received(&format!("sent-{run}-{chunk_index}-{i} "), &context);
        }
        assert_eq!(
            entries.len(),
            chunk.len(),
            "expected one Operation log entry for each refused statement, got\n{}",
            lines(&entries)
        );
        for ((case, (value, _)), entry) in chunk.iter().zip(&sent).zip(&entries) {
            let context = format!("the entry of {} of {:?}", case.operation, case.sql);
            assert_entry(
                entry,
                &Expected {
                    statement: (case.reason == "unparsed").then_some("<unparsed>"),
                    ..Expected::check_refused(&id, "wanda", "board", case.operation)
                },
                &context,
            );
            assert!(
                !entry.to_string().contains(value.as_str()),
                "{context}: expected no value of the statement in the entry, got {entry}"
            );
        }
    }
}

#[test]
fn every_allowed_form_runs() {
    let s = Scenario::writable();
    let reads = [
        json!({ "sql": "select id, title from incidents where id = $1", "params": [1] }),
        json!({ "sql": "with open_ones as (select id from incidents where open) select count(*)::int4 as n from open_ones" }),
        json!({ "sql": "with recursive r(n) as (select 1 union all select n + 1 from r where n < 3) select n from r" }),
        json!({ "sql": "select i.id, k.label from incidents i join kinds k on k.label = 'every' where i.id = 1" }),
        json!({ "sql": "select i.id from incidents i left join lateral (select 1 as x) l on true where i.id = $1::int4", "params": ["2"] }),
        json!({ "sql": "select id from incidents where id in (select 1) and exists (select 1 from kinds)" }),
        json!({ "sql": "select (select count(*) from kinds)::int4 as k, '42'::int4 as n, cast('2' as int8) as m, 1::text as t" }),
        json!({ "sql": "select * from (values (1), (2)) v(x) order by x desc limit 1" }),
        json!({ "sql": "select upper(title), coalesce(null, length(title)) from incidents where id = 1" }),
        json!({ "sql": "select id from incidents union select 7 order by 1" }),
        json!({ "sql": "select severity, count(*)::int4 from incidents group by severity having count(*) > 0 order by 1" }),
        // Listed names only in a string, a comment or an alias.
        json!({ "sql": "select 'set_config(''role'', ''none'', false); select 1 from pg_authid' as text_only" }),
        json!({ "sql": "select 1 as one /* set_config('role', 'none', false), pg_stat_activity */" }),
        json!({ "sql": "select 1 as pg_authid, 2 as set_config" }),
        json!({ "sql": "select title from incidents where title like '%query_to_xml%'" }),
        json!({ "sql": "select 1 -- ; drop table incidents\n as one" }),
    ];
    let writes = [
        json!({ "sql": "insert into notes (body) values ($1) returning id, body", "params": ["first"] }),
        json!({ "sql": "insert into notes (body) values ('second')" }),
        json!({ "sql": "with c as (select 'from-cte' as b) insert into notes (body) select b from c returning body" }),
        json!({ "sql": "insert into notes (body) select title from incidents i join kinds k on k.label = 'every' where i.id = 1 returning body" }),
        json!({ "sql": "update notes set body = 'updated' where id in (select id from notes where body = 'from-cte') returning body::text, id::int8" }),
        json!({ "sql": "update incidents set open = not open where id = $1 returning id", "params": [2] }),
        json!({ "sql": "with gone as (delete from notes where body = 'updated' returning id) insert into notes (body) select 'moved-' || id from gone returning body" }),
        json!({ "sql": "delete from notes where body = $1", "params": ["second"] }),
        json!({ "sql": "delete from notes where id = (select min(id) from notes) returning id" }),
        json!({ "sql": "with gone as (delete from notes returning id) select count(*) from gone" }),
        json!({ "sql": "insert into notes (body) values ('pg_authid set_config') returning body as pg_shadow" }),
    ];
    let mut calls: Vec<Value> = Vec::new();
    for read in &reads {
        let mut args = read.clone();
        args["resource"] = json!(RESOURCE);
        calls.push(op("query", &args));
    }
    for write in &writes {
        let mut args = write.clone();
        args["resource"] = json!(RESOURCE);
        calls.push(op("execute", &args));
    }
    let (_, answers) = s.run(&wanda(), "board", &json!({ "calls": calls }));
    let refused: Vec<String> = calls
        .iter()
        .zip(&answers)
        .filter(|(_, answer)| !answer["ok"].is_object())
        .map(|(call, answer)| format!("  {}\n    -> {answer}", call["arguments"]["sql"]))
        .collect();
    assert!(
        refused.is_empty(),
        "expected every allowed form to run, but these did not:\n{}",
        refused.join("\n")
    );
}

#[test]
fn every_other_kind_of_statement_is_refused() {
    let s = Scenario::writable();
    let kinds = [
        "create table made_here (x text default '%V%')",
        "create temp table made_here (x int)",
        "create table stolen as select * from incidents",
        "create view v as select '%V%' as x",
        "create index on incidents (title)",
        "create function f() returns text language sql as $$select '%V%'$$",
        "create role evil",
        "create schema evil",
        "create extension dblink",
        "alter table incidents add column extra int",
        "alter role current_user set work_mem = '64MB'",
        "alter system set work_mem = '64MB'",
        "alter default privileges grant select on tables to public",
        "drop table kinds",
        "drop function close_every_incident()",
        "truncate incidents",
        "set work_mem = '64MB'",
        "set local statement_timeout = 0",
        "set search_path = '%V%'",
        "set role none",
        "set session authorization default",
        "set transaction read write",
        "set session characteristics as transaction read write",
        "reset role",
        "reset all",
        "show work_mem",
        "do $$ begin perform '%V%'; end $$",
        "call no_such_procedure('%V%')",
        "copy incidents to stdout",
        "copy incidents from stdin",
        "copy (select '%V%') to program 'true'",
        "copy incidents to '/tmp/stolen'",
        "explain select 1",
        "explain analyze delete from incidents",
        "prepare p as select '%V%'",
        "execute p",
        "deallocate p",
        "deallocate all",
        "listen channel",
        "notify channel, '%V%'",
        "unlisten *",
        "lock table incidents",
        "begin",
        "start transaction read write",
        "commit",
        "rollback",
        "end",
        "abort",
        "savepoint s",
        "release savepoint s",
        "rollback to savepoint s",
        "prepare transaction '%V%'",
        "commit prepared '%V%'",
        "select '%V%' as x into stolen",
        "select * into temp stolen from incidents",
        "select * into unlogged stolen from incidents where title <> '%V%'",
        "grant select on staff_only to public",
        "revoke select on incidents from public",
        "vacuum incidents",
        "analyze incidents",
        "cluster incidents",
        "reindex table incidents",
        "checkpoint",
        "discard all",
        "discard temp",
        "refresh materialized view m",
        "comment on table incidents is '%V%'",
        "security label on table incidents is '%V%'",
        "load '%V%'",
        "declare c cursor for select '%V%'",
        "fetch c",
        "close c",
        "merge into incidents i using (select 1 as id) s on i.id = s.id when matched then delete",
        "import foreign schema public from server s into public",
        "reassign owned by current_user to public",
    ];
    let mut cases: Vec<Refused> = kinds
        .iter()
        .map(|sql| refused("query", sql, "kind-not-allowed"))
        .collect();
    // The check is the same for a write.
    for sql in [
        "create table made_here (x int)",
        "truncate incidents",
        "copy incidents from stdin",
        "set role none",
        "begin",
        "do $$ begin delete from incidents; end $$",
        "select '%V%' as x into stolen",
        "merge into incidents i using (select 1 as id) s on i.id = s.id when matched then delete",
        "lock table incidents",
        "alter table incidents add column extra int",
        "drop table kinds",
    ] {
        cases.push(refused("execute", sql, "kind-not-allowed"));
    }
    assert_all_refused(&s, &cases);
}

#[test]
fn a_call_of_a_listed_function_is_refused_wherever_it_is_and_however_it_is_written() {
    let s = Scenario::writable();
    let functions = [
        "set_config('work_mem', '%V%', false)",
        "pg_terminate_backend(0)",
        "pg_cancel_backend(0)",
        "pg_notify('channel', '%V%')",
        "pg_reload_conf()",
        "query_to_xml('select ''%V%''', true, true, '')",
        "query_to_xmlschema('select 1', true, true, '')",
        "query_to_xml_and_xmlschema('select 1', true, true, '')",
        "dblink('dbname=x', 'select ''%V%''')",
        "dblink_exec('dbname=x', 'select 1')",
        "dblink_connect('dbname=x')",
        "lo_import('/etc/hostname')",
        "lo_export(1, '/tmp/%V%')",
        "lo_from_bytea(0, '%V%')",
        "lo_create(0)",
        "lo_get(1)",
        "lo_put(1, 0, 'x')",
        "lo_unlink(1)",
        "pg_read_file('postgresql.conf')",
        "pg_read_binary_file('postgresql.conf')",
        "pg_ls_dir('.')",
        "pg_stat_file('postgresql.conf')",
    ];
    let mut cases: Vec<Refused> = functions
        .iter()
        .map(|call| {
            refused(
                "query",
                &format!("select {call} as x"),
                "function-not-allowed",
            )
        })
        .collect();
    let set = "set_config('search_path', '%V%', false)";
    let places = [
        format!("select title from incidents where {set} is not null"),
        format!("select * from (select {set} as x) s"),
        format!("with c as (select {set} as x) select x from c"),
        format!("select (select {set}) as x"),
        format!("select upper({set}) as x"),
        format!("select case when id = 1 then {set} end from incidents"),
        format!("select coalesce(null, {set}) as x"),
        format!("select 1 from incidents i join kinds k on {set} is not null"),
        format!("select id from incidents order by {set}"),
        format!("select severity from incidents group by severity having max({set}) is not null"),
        format!("select * from (values ({set})) v(x)"),
        format!("select * from {set}"),
        format!("select * from incidents i, lateral (select {set} as x) l"),
        format!("select exists (select {set}) as x"),
        format!("select id from incidents where id in (select length({set}))"),
        format!("select array[{set}] as x"),
        format!("select row({set}) as x"),
    ];
    for sql in &places {
        cases.push(refused("query", sql, "function-not-allowed"));
    }
    for sql in [
        format!("insert into notes (body) values ({set})"),
        format!("insert into notes (body) select {set}"),
        format!("insert into notes (body) values ('x') returning {set}"),
        format!("update notes set body = {set}"),
        format!("update notes set body = 'x' where {set} is not null returning id"),
        format!("update notes set body = 'x' returning {set}"),
        format!("delete from notes where body = {set}"),
        format!("delete from notes returning {set}"),
        format!("with c as (select {set} as x) insert into notes (body) select x from c"),
        "select pg_terminate_backend(pid) from (select 0 as pid) p".to_owned(),
    ] {
        cases.push(refused("execute", &sql, "function-not-allowed"));
    }
    // Hiding the name.
    for hidden in [
        "SET_CONFIG('work_mem', '%V%', false)",
        "Set_Config('work_mem', '%V%', false)",
        "\"set_config\"('work_mem', '%V%', false)",
        "pg_catalog.set_config('work_mem', '%V%', false)",
        "PG_CATALOG.SET_CONFIG('work_mem', '%V%', false)",
        "\"pg_catalog\".\"set_config\"('work_mem', '%V%', false)",
        "pg_catalog . set_config ('work_mem', '%V%', false)",
        "set_config/* hidden */('work_mem', '%V%', false)",
        "pg_catalog./* hidden */set_config('work_mem', '%V%', false)",
        "set_config\n('work_mem', '%V%', false)",
        "set_config\t\r\n ('work_mem', '%V%', false)",
        "set_config -- hidden\n('work_mem', '%V%', false)",
        "PG_TERMINATE_BACKEND(0)",
        "pg_catalog.\"pg_terminate_backend\"(0)",
        "Pg_Notify('channel', '%V%')",
        "\"pg_catalog\".\"pg_read_file\"('postgresql.conf')",
        "LO_FROM_BYTEA(0, '%V%')",
        "pg_catalog.lo_import('/etc/hostname')",
        "QUERY_TO_XML('select 1', true, true, '')",
        "pg_catalog.query_to_xml('select 1', true, true, '')",
    ] {
        cases.push(refused(
            "query",
            &format!("select {hidden} as x"),
            "function-not-allowed",
        ));
    }
    cases.push(refused(
        "query",
        "/* set_config */ select pg_catalog.set_config('work_mem', '%V%', false) as x",
        "function-not-allowed",
    ));
    assert_all_refused(&s, &cases);
}

#[test]
fn a_reference_to_a_listed_relation_is_refused_wherever_it_is_and_however_it_is_written() {
    let s = Scenario::writable();
    let mut cases: Vec<Refused> = Vec::new();
    for relation in ["pg_authid", "pg_shadow", "pg_stat_activity"] {
        for sql in [
            format!("select * from {relation}"),
            format!("select * from pg_catalog.{relation}"),
            format!("select * from \"{relation}\""),
            format!("select * from \"pg_catalog\".\"{relation}\""),
            format!("select * from {}", relation.to_uppercase()),
            format!("select * from PG_CATALOG.{}", relation.to_uppercase()),
            format!("select * from pg_catalog . {relation}"),
            format!("select * from pg_catalog./* hidden */{relation}"),
            format!("select * from\n{relation}\nwhere '%V%' <> ''"),
            format!("select 1 from incidents i join {relation} a on true"),
            format!("select (select count(*) from {relation}) as n"),
            format!("with a as (select * from {relation}) select 1 from a"),
            format!("select 1 where exists (select 1 from {relation})"),
            format!("select id from incidents where id in (select 1 from {relation})"),
            format!("select 1 from incidents, lateral (select * from {relation}) l"),
            format!("select 1 from incidents union all select 1 from {relation}"),
        ] {
            cases.push(refused("query", &sql, "relation-not-allowed"));
        }
        for sql in [
            format!("update notes set body = (select count(*) from {relation})::text"),
            format!("delete from notes where body in (select '%V%' from {relation})"),
            format!("insert into notes (body) select 'x' from {relation}"),
            format!(
                "insert into notes (body) values ('x') returning (select count(*) from {relation})"
            ),
            format!(
                "with a as (select * from {relation}) insert into notes (body) select 'x' from a"
            ),
        ] {
            cases.push(refused("execute", &sql, "relation-not-allowed"));
        }
    }
    assert_all_refused(&s, &cases);
}

#[test]
fn several_statements_and_unparseable_ones_are_refused() {
    let s = Scenario::writable();
    let mut cases: Vec<Refused> = [
        "select 1; select '%V%'",
        "select 1;select 2",
        "select 1;; select 2",
        "select 1 /* ; */; select 2",
        "select ';' as s; select 2",
        "select 1;\nselect 2",
        "select title from incidents; update incidents set title = '%V%'",
        "update incidents set open = false; delete from incidents",
    ]
    .iter()
    .map(|sql| refused("query", sql, "several-statements"))
    .collect();
    for sql in [
        "insert into notes (body) values ('%V%'); delete from notes",
        "delete from notes; insert into notes (body) values ('x')",
        "update incidents set open = false; select 1",
    ] {
        cases.push(refused("execute", sql, "several-statements"));
    }
    for sql in [
        "selec '%V%' from incidents",
        "select from where",
        "select '%V%",
        "select (1",
        "insert into",
        "update incidents set where id = 1",
        "delete incidents",
        "select $$%V%",
        "select 1 from incidents where",
    ] {
        cases.push(refused("query", sql, "unparsed"));
    }
    for sql in [
        "insert into notes (body) value ('%V%')",
        "update notes set body = 'x' were id = 1",
    ] {
        cases.push(refused("execute", sql, "unparsed"));
    }
    assert_all_refused(&s, &cases);
    // A mix of a refused kind and another statement is refused too, for
    // whichever reason the check meets first.
    let marker = format!("mixed-{}", token());
    for sql in [
        format!("select '{marker}'; drop table kinds"),
        format!("set role {SUPERUSER}; select '{marker}'"),
    ] {
        let (_, answer) = s.one(
            &wanda(),
            "board",
            &op("query", &json!({ "resource": RESOURCE, "sql": sql })),
        );
        assert_eq!(
            answer["error"]["kind"], "statement",
            "expected {sql:?} refused by the statement check, got {answer}"
        );
    }
    s.db.assert_not_received(&marker, "a mix of statements");
}
