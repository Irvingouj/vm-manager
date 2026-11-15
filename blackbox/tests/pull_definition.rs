//! The definition a pull answers (CONTRACT.md, "Pulling a Resource's
//! structure: The definition"; ticket "Pull a Resource's structure as a
//! typed Drizzle definition": the generated files say they are generated;
//! a column type the puller cannot express is reported, not silently
//! dropped).
//!
//! Its first two lines, the export names and column keys (camel case, the
//! real name always given, a `_` and a number where names meet, are no
//! identifier or are reserved words), the Drizzle constructs for each
//! shape, the types and their modes, and the unsupported columns present
//! as custom types. That it type-checks and that typed queries type-check
//! against it is `pull_compiles.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::scenario::READERS;
use blackbox::structure::{
    FIRST_LINE, STOCK, Stock, UNSUPPORTED, export_of, is_identifier, key_of, second_line,
};

fn rita() -> User {
    User::new("rita").in_groups(&[READERS])
}

fn pulled() -> String {
    let stock = Stock::ready();
    let answer = stock.pull_ok(&rita(), STOCK);
    answer["schema_ts"].as_str().unwrap().to_owned()
}

/// Fails unless `name` is a `_` and a number added to `base`.
fn assert_numbered(name: &str, base: &str, context: &str) {
    let digits = name
        .strip_prefix(base)
        .and_then(|rest| rest.strip_prefix('_'))
        .unwrap_or_default();
    assert!(
        !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()),
        "{context}: expected {base:?} with a `_` and a number added, got {name:?}"
    );
}

#[test]
fn the_definition_begins_with_the_two_stated_lines() {
    let stock = Stock::ready();
    let answer = stock.pull_ok(&rita(), STOCK);
    let ts = answer["schema_ts"].as_str().unwrap();
    let expected = format!(
        "{FIRST_LINE}\n{}\n",
        second_line(STOCK, answer["fingerprint"].as_str().unwrap())
    );
    assert!(
        ts.starts_with(&expected),
        "expected the definition to begin with\n{expected}got\n{}",
        ts.lines().take(3).collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn tables_views_and_enums_are_exported_in_camel_case_under_their_schemas() {
    let ts = pulled();
    for (call, export) in [
        ("pgTable(\"sites\"", "sites"),
        ("pgTable(\"stock_checks\"", "stockChecks"),
        ("pgTable(\"items\"", "items"),
        ("pgView(\"open_items\"", "openItems"),
        ("pgMaterializedView(\"site_totals\"", "siteTotals"),
        ("pgEnum(\"status\"", "status"),
        ("pgSchema(\"crm\")", "crmSchema"),
        ("crmSchema.table(\"accounts\"", "crmAccounts"),
        ("crmSchema.enum(\"tier\"", "crmTier"),
        ("pgTable(\"readings\"", "readings"),
    ] {
        assert_eq!(
            export_of(&ts, call).as_deref(),
            Some(export),
            "expected `export const {export} = {call}...`, got\n{ts}"
        );
    }
    for (export, column, key) in [
        ("items", "site_id", "siteId"),
        ("items", "label_upper", "labelUpper"),
        ("items", "added_at", "addedAt"),
        ("items", "shelf_life", "shelfLife"),
        ("sites", "opened_on", "openedOn"),
        ("crmAccounts", "home_site", "homeSite"),
        ("movements", "small_serial", "smallSerial"),
        ("siteTotals", "item_count", "itemCount"),
    ] {
        assert_eq!(
            key_of(&ts, export, column).as_deref(),
            Some(key),
            "expected the column {column:?} of {export} under the key {key:?}, with its real name given, got\n{ts}"
        );
    }
}

#[test]
fn names_that_meet_are_no_identifier_or_are_reserved_get_a_number() {
    let ts = pulled();
    // Two tables whose names meet in camel case.
    let snake = export_of(&ts, "pgTable(\"stock_levels\"").unwrap_or_default();
    let camel = export_of(&ts, "pgTable(\"stockLevels\"").unwrap_or_default();
    assert_ne!(snake, camel, "expected two exports, got\n{ts}");
    // `stockLevels` sorts before `stock_levels` in the `C` collation.
    assert_eq!(
        camel, "stockLevels",
        "expected the first of the two by the `C` collation to keep its name"
    );
    assert_numbered(&snake, "stockLevels", "the second of two tables that meet");
    // Two columns of `stock_levels` whose names meet.
    let first = key_of(&ts, &snake, "userId").unwrap_or_default();
    let second = key_of(&ts, &snake, "user_id").unwrap_or_default();
    assert_eq!(first, "userId", "{ts}");
    assert_numbered(&second, "userId", "the second of two columns that meet");
    // A reserved word, as a table and as a column.
    let case = export_of(&ts, "pgTable(\"case\"").unwrap_or_default();
    assert_numbered(&case, "case", "a table named by a reserved word");
    let default = key_of(&ts, &case, "default").unwrap_or_default();
    assert_numbered(&default, "default", "a column named by a reserved word");
    // A name that is no identifier.
    let two_fa = export_of(&ts, "pgTable(\"2fa codes\"").unwrap_or_default();
    assert!(
        is_identifier(&two_fa),
        "expected the table \"2fa codes\" exported under an identifier, got {two_fa:?} in\n{ts}"
    );
    let code = key_of(&ts, &two_fa, "per-user code").unwrap_or_default();
    assert!(
        is_identifier(&code),
        "expected the column \"per-user code\" under an identifier, got {code:?} in\n{ts}"
    );
}

#[test]
fn each_shape_is_written_with_its_drizzle_construct() {
    let ts = pulled();
    for (what, needle) in [
        ("the import of pg-core", "from \"drizzle-orm/pg-core\""),
        ("identity always", ".generatedAlwaysAsIdentity()"),
        ("identity by default", ".generatedByDefaultAsIdentity()"),
        ("a generated column", ".generatedAlwaysAs(sql`"),
        ("a default", ".default(sql`"),
        ("NOT NULL", ".notNull()"),
        ("a primary key of one column", ".primaryKey()"),
        ("a primary key of several columns", "primaryKey({"),
        ("a unique constraint", "unique("),
        ("a foreign key", "foreignKey({"),
        ("ON DELETE CASCADE", "\"cascade\""),
        ("ON UPDATE RESTRICT", "\"restrict\""),
        ("ON DELETE SET NULL", "\"set null\""),
        ("a view", ".existing()"),
        ("an array", ".array()"),
        ("a bigint as a number", "mode: \"number\""),
        ("a timestamp as text", "mode: \"string\""),
        ("a serial", "serial(\"serial_no\""),
        ("a bigserial", "bigserial(\"big_serial\""),
        ("a smallserial", "smallserial(\"small_serial\""),
        ("a custom type", "customType"),
        ("varchar with its length", "varchar(\"code\""),
        ("char with its length", "char(\"sku\""),
        ("numeric with its precision", "numeric(\"price\""),
        ("real", "real(\"weight\""),
        ("double precision", "doublePrecision(\"ratio\""),
        ("smallint", "smallint(\"qty\""),
        ("time", "time(\"opens\""),
        ("interval", "interval(\"shelf_life\""),
        ("uuid", "uuid(\"ref\""),
        ("jsonb", "jsonb(\"attrs\""),
        ("json", "json(\"raw\""),
        ("date", "date(\"opened_on\""),
        ("timestamptz", "timestamp(\"added_at\""),
        ("boolean", "boolean(\"active\""),
    ] {
        assert!(
            ts.contains(needle),
            "expected {what} ({needle}) in the definition, got\n{ts}"
        );
    }
    // Precision and lengths kept.
    let line = |column: &str| {
        ts.lines()
            .find(|l| l.contains(&format!("(\"{column}\"")))
            .unwrap_or_default()
            .to_owned()
    };
    for (column, holds) in [
        ("code", "16"),
        ("sku", "8"),
        ("price", "10"),
        ("price", "2"),
        ("added_at", "3"),
        ("added_at", "withTimezone: true"),
        ("opens_tz", "withTimezone: true"),
        ("big_serial", "mode: \"number\""),
        ("added_at", "mode: \"string\""),
        ("checked_at", "mode: \"string\""),
    ] {
        assert!(
            line(column).contains(holds),
            "expected the column {column:?} written with {holds:?}, got {:?}",
            line(column)
        );
    }
    // Every enum with its labels in order.
    let status = ts
        .lines()
        .find(|l| l.contains("pgEnum(\"status\""))
        .unwrap_or_default();
    let (open, progress, closed) = (
        status.find("\"open\""),
        status.find("\"in progress\""),
        status.find("\"closed\""),
    );
    assert!(
        open.is_some() && open < progress && progress < closed,
        "expected the labels of `status` in order, got {status:?}"
    );
}

#[test]
fn a_column_of_a_type_it_cannot_express_is_in_the_definition_as_a_custom_type() {
    let ts = pulled();
    for (_, table, column, type_word) in UNSUPPORTED {
        let export = if table == "items" { "items" } else { "sites" };
        assert!(
            key_of(&ts, export, column).is_some(),
            "expected the unsupported column {table}.{column} in the definition, not dropped, got\n{ts}"
        );
        assert!(
            ts.contains(type_word),
            "expected the custom type's database type to be the type's name ({type_word}), got\n{ts}"
        );
    }
    assert!(
        ts.contains("unknown"),
        "expected the custom types' TypeScript type to be `unknown`, got\n{ts}"
    );
}
