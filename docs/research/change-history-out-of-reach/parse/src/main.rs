//! Splits each migration text with Postgres's own grammar and names each
//! statement's kind, as the host could before sending it.
use pg_query::NodeEnum;

const TEXTS: &[&str] = &[
    "INSERT INTO t VALUES (1); COMMIT; BEGIN; INSERT INTO t VALUES (2)",
    "-- COMMIT;\nSELECT 'COMMIT; ROLLBACK'; /* END; */ SELECT $x$ ; COMMIT; $x$; SELECT \"commit\" FROM t",
    "COMMIT AND CHAIN",
    "END",
    "ABORT",
    "ROLLBACK AND CHAIN",
    "START TRANSACTION",
    "PREPARE TRANSACTION 'x'",
    "COMMIT PREPARED 'x'",
    "SAVEPOINT a; RELEASE SAVEPOINT a; ROLLBACK TO SAVEPOINT a",
    "DO $$ BEGIN COMMIT; END $$",
    "CALL p()",
    "CREATE PROCEDURE p() LANGUAGE plpgsql AS $$ BEGIN COMMIT; END $$",
    "SELECT query_to_xml('ALTER TABLE t DISABLE TRIGGER USER', true, true, '')",
    "CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$ BEGIN EXECUTE convert_from(decode('QUxURVIgVEFCTEU=', 'base64'), 'UTF8') || ' t DISABLE TRIGGER USER'; END $$",
    "ALTER TABLE t DISABLE TRIGGER vm_capture_update",
    "INSERT INTO t VALUES (1);\n--> statement-breakpoint\nCOMMIT;",
];

fn kind(node: Option<&NodeEnum>) -> String {
    match node {
        Some(NodeEnum::TransactionStmt(t)) => format!("TransactionStmt({:?})", t.kind()),
        Some(n) => {
            let debug = format!("{n:?}");
            debug.split('(').next().unwrap_or("?").to_owned()
        }
        None => "?".to_owned(),
    }
}

fn main() {
    for text in TEXTS {
        let shown = text.replace('\n', "\\n");
        match pg_query::split_with_parser(text) {
            Err(e) => println!("{shown}\n    split: error {e}"),
            Ok(parts) => {
                println!("{shown}");
                for part in parts {
                    let k = match pg_query::parse(part, 0) {
                        Ok(r) => r
                            .protobuf
                            .stmts
                            .iter()
                            .map(|s| kind(s.stmt.as_ref().and_then(|s| s.node.as_ref())))
                            .collect::<Vec<_>>()
                            .join(", "),
                        Err(e) => format!("parse error {e}"),
                    };
                    println!("    [{}] {k}", part.trim().replace('\n', "\\n"));
                }
            }
        }
    }
    // What the PL/pgSQL parser sees of the dynamic EXECUTE above.
    let body = "CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$ BEGIN EXECUTE convert_from(decode('QUxURVIgVEFCTEU=', 'base64'), 'UTF8') || ' t DISABLE TRIGGER USER'; END $$";
    match pg_query::parse_plpgsql(body) {
        Ok(v) => {
            let s = v.to_string();
            let at = s.find("PLpgSQL_stmt_dynexecute").unwrap_or(0);
            println!("plpgsql: {}", s.get(at..at.saturating_add(160)).unwrap_or(""));
        }
        Err(e) => println!("plpgsql: error {e}"),
    }
}
