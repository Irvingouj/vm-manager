//! The puller reads catalogs only (CONTRACT.md, "Pulling a Resource's
//! structure: What a pull reads", "The structure login"; ticket "Pull a
//! Resource's structure as a typed Drizzle definition": the Postgres server
//! log shows the puller read catalogs only; a Resource can be registered
//! with an optional structure login, and the puller prefers it).
//!
//! This test binary turns statement logging fully on for the whole
//! instance of shared databases before anything is pulled. Then every
//! statement each login sent is read back from the server log and
//! explained by the harness's superuser (see
//! `blackbox::structure::catalog_reads`): what its plan scans must be in
//! `pg_catalog` and nothing else, none of the statistics; no `reltuples`,
//! `pg_stat_*` or `information_schema` in its text, none of the database's
//! own names written into it as a value; and it runs in a read-only
//! transaction. With a structure login registered, the pull connects as
//! it (the log's `user=`), and never as the read or the write login;
//! without one, as the read login, never as the write login.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::postgres::for_shared_databases;
use blackbox::scenario::READERS;
use blackbox::server_log::log_everything;
use blackbox::structure::{
    STOCK, Stock, catalog_reads, catalog_reads_since, log_mark, own_names, statements_sent_since,
};

fn rita() -> User {
    User::new("rita").in_groups(&[READERS])
}

/// How many statements the shared database's server log shows from `role`
/// after the log held `mark` entries: registering a Resource connects every
/// login and sends the probe on the read and the structure login ("Registering
/// a database, checked"), which is not a pull's.
fn sent_by_role(stock: &Stock, role: &str, mark: usize) -> usize {
    statements_sent_since(stock.db(), role, mark)
}

#[test]
fn a_pull_reads_only_the_catalogs_in_a_read_only_transaction_as_the_structure_login_or_else_the_read_login()
 {
    log_everything(for_shared_databases());
    let stock = Stock::start();
    stock.register(STOCK);
    stock.register_without_structure("stock_by_reader");
    stock.scenario.grant(STOCK, READERS, "structure");
    stock
        .scenario
        .grant("stock_by_reader", READERS, "structure");
    let names = own_names(stock.db());
    assert!(
        names.iter().any(|n| n == "sites") && names.iter().any(|n| n == "close_every_incident"),
        "the harness's own check: expected the database's own names, got {names:?}"
    );
    let db = stock.db();
    // What the registrations sent is in the log by now; what a pull sends
    // follows the mark.
    std::thread::sleep(std::time::Duration::from_millis(500));
    let mark = log_mark(db);
    let (structure, reader, writer) = (
        stock.structure.role.as_str(),
        db.reader.role.as_str(),
        db.writer.role.as_str(),
    );

    // With a structure login: as it, and as no other.
    stock.pull_ok(&rita(), STOCK);
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        sent_by_role(&stock, structure, mark) > 0,
        "expected the pull of a Resource with a structure login to connect as it ({structure}); the server log shows from each login: structure {}, read {}, write {}",
        sent_by_role(&stock, structure, mark),
        sent_by_role(&stock, reader, mark),
        sent_by_role(&stock, writer, mark)
    );
    assert_eq!(
        sent_by_role(&stock, reader, mark),
        0,
        "expected nothing from the read login when the Resource has a structure login"
    );
    let with_structure = catalog_reads_since(db, structure, &names, mark);
    assert!(
        !with_structure.reads.is_empty(),
        "expected the structure login's statements to read something: {with_structure:#?}"
    );
    assert!(
        with_structure.problems.is_empty(),
        "expected the pull to read the catalogs only, in a read-only transaction, but as the structure login:\n{}",
        with_structure.problems.join("\n")
    );

    // Without one: as the read login.
    stock.pull_ok(&rita(), "stock_by_reader");
    std::thread::sleep(std::time::Duration::from_millis(300));
    let with_reader = catalog_reads_since(db, reader, &names, mark);
    assert!(
        !with_reader.reads.is_empty(),
        "expected the pull of a Resource without a structure login to connect as the read login ({reader})"
    );
    assert!(
        with_reader.problems.is_empty(),
        "expected the pull to read the catalogs only, in a read-only transaction, but as the read login:\n{}",
        with_reader.problems.join("\n")
    );
    // What "What a pull reads" says: it looks the Resource's roles up in
    // `pg_roles` and asks `has_table_privilege` and `has_any_column_privilege`.
    for (who, reads) in [
        ("structure", &with_structure.reads),
        ("read", &with_reader.reads),
    ] {
        for word in [
            "pg_roles",
            "has_table_privilege",
            "has_any_column_privilege",
        ] {
            assert!(
                reads
                    .iter()
                    .any(|statement| statement.to_ascii_lowercase().contains(word)),
                "expected the pull as the {who} login to send a statement that uses `{word}`, got:\n{}",
                reads.join("\n")
            );
        }
    }
    // A fixed text of the program's: the same texts whichever login.
    let mut a = with_structure.reads;
    let mut b = with_reader.reads;
    a.sort();
    a.dedup();
    b.sort();
    b.dedup();
    assert_eq!(
        a, b,
        "expected the pull to send the same fixed texts whichever login it uses"
    );

    // Never the write login.
    assert_eq!(
        sent_by_role(&stock, writer, mark),
        0,
        "expected the write login never used by a pull"
    );
}

/// The harness's own check, which holds today: the server log is read, and
/// `catalog_reads` passes a read of the catalogs in a read-only transaction
/// sent with parameters through the extended protocol, and names each of
/// a read of the database's own table, of `pg_stats`, of `reltuples`, of
/// `information_schema`, a name written into the text, and a catalog read
/// outside a read-only transaction.
#[test]
fn harness_the_catalog_check_tells_a_catalog_read_from_every_other() {
    log_everything(for_shared_databases());
    let stock = Stock::start();
    let db = stock.db();
    let names = own_names(db);

    let honest = db.connect_as(&stock.structure);
    honest.batch("BEGIN READ ONLY");
    honest.batch("SET LOCAL statement_timeout = '5s'");
    let rows = honest
        .try_with_params(
            "SELECT c.relname::text, a.attname::text, pg_catalog.format_type(a.atttypid, a.atttypmod)
               FROM pg_catalog.pg_class c
               JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
               JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid
              WHERE n.nspname = $1::text AND a.attnum > 0
              ORDER BY 1, a.attnum",
            &["public"],
        )
        .unwrap();
    assert!(rows > 0);
    honest.batch("COMMIT");
    std::thread::sleep(std::time::Duration::from_millis(300));
    let read = catalog_reads(db, &stock.structure.role, &names);
    assert_eq!(
        read.reads.len(),
        1,
        "expected the one catalog read in the server log: {read:#?}"
    );
    assert!(
        read.problems.is_empty(),
        "expected a read of the catalogs in a read-only transaction to pass: {read:#?}"
    );

    let careless = db.connect_as(&db.reader);
    let wrong = [
        ("SELECT id FROM sites", "public.sites"),
        ("SELECT * FROM pg_catalog.pg_stats", "pg_stat"),
        ("SELECT reltuples FROM pg_catalog.pg_class", "reltuples"),
        (
            "SELECT table_name FROM information_schema.tables",
            "information_schema",
        ),
        (
            "SELECT oid FROM pg_catalog.pg_class WHERE relname = 'items'",
            "\"items\"",
        ),
    ];
    careless.batch("BEGIN READ ONLY");
    for (sql, _) in wrong {
        careless.batch(sql);
    }
    careless.batch("COMMIT");
    careless.batch("SELECT 1 FROM pg_catalog.pg_namespace");
    std::thread::sleep(std::time::Duration::from_millis(300));
    let read = catalog_reads(db, &db.reader.role, &names);
    for (sql, word) in wrong {
        assert!(
            read.problems
                .iter()
                .any(|p| p.contains(sql) && p.contains(word)),
            "expected `{sql}` to be named ({word}), got:\n{}",
            read.problems.join("\n")
        );
    }
    assert!(
        read.problems
            .iter()
            .any(|p| p.contains("not in a read-only transaction") && p.contains("pg_namespace")),
        "expected the read outside a transaction to be named, got:\n{}",
        read.problems.join("\n")
    );
}
