//! Values round-trip with the types Drizzle declares (CONTRACT.md, "The SDK:
//! Values"): every row of the contract's table, as results, as parameters
//! (in `values(...)`, `set(...)` and `where(...)`) and through
//! `returning()`, in an own database whose TimeZone is America/St_Johns and
//! a shared database whose TimeZone is Asia/Kolkata (neither UTC).
//!
//! The fixture describes each value as it was inside the Application
//! (`fixtures/sdk/board/src/describe.ts`): `typeof`, a `BigInt`'s text, a
//! `Date`'s `toISOString()`, arrays and objects element by element. So a
//! `BigInt` beyond 2^53, a `numeric` with many digits, a timestamp with
//! microseconds, the JSON string `"123"` and JSON `null` are each shown for
//! what they were, not for what JSON makes of them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::BTreeMap;

use blackbox::scenario::lines;
use blackbox::sdk::{
    OWN_TIME_ZONE, SHARED_TIME_ZONE, Sdk, ann, assert_entry_holds_no_parameter, d,
};
use serde_json::{Value, json};

/// The columns of `vals`, as the fixture's Drizzle schema names them.
const COLUMNS: [&str; 22] = [
    "id", "label", "i4", "i2", "big", "bigNum", "num", "r4", "f8", "flag", "t", "vc", "u", "ts",
    "tstz", "tsStr", "tstzStr", "d", "j", "jb", "texts", "ints",
];

/// The text the seeded and written `t` holds.
const TEXT: &str = "héllo ☃ \"quoted\" \\ back";

/// The `texts` array of the seeded and written rows: commas, quotes,
/// backslashes, braces, an empty string, a null, the word NULL, spaces.
fn texts() -> Value {
    d::strings(&[
        Some("a,b"),
        Some("q\"uote"),
        Some("back\\slash"),
        Some("{brace}"),
        Some(""),
        None,
        Some("NULL"),
        Some(" spaced "),
    ])
}

/// A row of `vals` with every column null but these.
fn row(label: &str, set: &[(&str, Value)]) -> BTreeMap<String, Value> {
    let mut row: BTreeMap<String, Value> = COLUMNS
        .iter()
        .filter(|c| **c != "id")
        .map(|c| ((*c).to_owned(), d::null()))
        .collect();
    row.insert("label".to_owned(), d::string(label));
    for (column, value) in set {
        assert!(
            row.contains_key(*column),
            "the test names an unknown column {column}"
        );
        row.insert((*column).to_owned(), value.clone());
    }
    row
}

/// The row of every type, as seeded by the migration (or the shared setup)
/// and read in a database whose `timestamptz` prints as `tstz_text`, its
/// timestamps with microseconds (`.678412`).
fn every(label: &str, tstz_text: &str, date_iso: &str) -> BTreeMap<String, Value> {
    row(
        label,
        &[
            ("i4", d::number("2147483647")),
            ("i2", d::number("-32768")),
            ("big", d::bigint("9007199254740993")),
            ("bigNum", d::number("9007199254740991")),
            (
                "num",
                d::string("12345678901234567890.012345678901234567890"),
            ),
            ("r4", d::number("1.5")),
            ("f8", d::number("3.141592653589793")),
            ("flag", d::boolean(true)),
            ("t", d::string(TEXT)),
            ("vc", d::string("varchar value")),
            ("u", d::string("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11")),
            ("ts", d::date(date_iso)),
            ("tstz", d::date(date_iso)),
            ("tsStr", d::string("2026-01-02 03:04:05.678412")),
            ("tstzStr", d::string(tstz_text)),
            ("d", d::string("2026-01-02")),
            (
                "j",
                d::object(&[
                    ("a", d::array(&[d::number("1"), d::string("x"), d::null()])),
                    ("b", d::string("123")),
                ]),
            ),
            ("jb", d::string("123")),
            ("texts", texts()),
            ("ints", d::numbers(&[Some(1), None, Some(-3)])),
        ],
    )
}

/// The seeded rows of JSON kinds and empty arrays.
fn json_rows() -> Vec<BTreeMap<String, Value>> {
    vec![
        row(
            "json-number",
            &[
                ("j", d::number("123")),
                ("jb", d::number("123")),
                ("texts", d::array(&[])),
                ("ints", d::array(&[])),
            ],
        ),
        row(
            "json-bool",
            &[("j", d::boolean(false)), ("jb", d::boolean(true))],
        ),
        row("json-null", &[]),
        row(
            "json-nested",
            &[
                (
                    "j",
                    d::array(&[d::null(), d::string("123"), d::object(&[])]),
                ),
                (
                    "jb",
                    d::object(&[
                        ("k", d::null()),
                        ("s", d::string("123")),
                        ("e", d::array(&[])),
                    ]),
                ),
            ],
        ),
    ]
}

/// Fails unless `actual` (a described row) has exactly the columns of
/// `vals`, `id` a number, and the other columns as `expected` says. Names
/// every column that differs.
fn assert_row(actual: &Value, expected: &BTreeMap<String, Value>, context: &str) {
    let entries = actual["entries"]
        .as_object()
        .unwrap_or_else(|| panic!("{context}: expected a row (an object), got {actual}"));
    let mut keys: Vec<&str> = entries.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut wanted: Vec<&str> = COLUMNS.to_vec();
    wanted.sort_unstable();
    assert_eq!(
        keys, wanted,
        "{context}: expected the row to have exactly the columns of the Drizzle schema, got {actual}"
    );
    assert_eq!(
        entries["id"]["type"], "number",
        "{context}: expected `id` (serial) as a number, got {actual}"
    );
    let differing: Vec<String> = expected
        .iter()
        .filter(|(column, want)| entries.get(column.as_str()) != Some(want))
        .map(|(column, want)| {
            format!(
                "  {column}: expected {want}\n  {}   got {}",
                " ".repeat(column.len()),
                entries.get(column.as_str()).unwrap_or(&Value::Null)
            )
        })
        .collect();
    assert!(
        differing.is_empty(),
        "{context}: these columns are not what Drizzle's types promise:\n{}",
        differing.join("\n")
    );
}

/// Fails unless `rows` (described) are these rows, in order.
fn assert_rows(rows: &Value, expected: &[BTreeMap<String, Value>], context: &str) {
    let rows = rows
        .as_array()
        .unwrap_or_else(|| panic!("{context}: expected a list of rows, got {rows}"));
    assert_eq!(
        rows.len(),
        expected.len(),
        "{context}: expected {} rows, got:\n{}",
        expected.len(),
        lines(rows)
    );
    for (index, (actual, want)) in rows.iter().zip(expected).enumerate() {
        let label = want["label"]["value"].as_str().unwrap_or_default();
        assert_row(actual, want, &format!("{context}, row {index} ({label})"));
    }
}

/// Fails unless the request has one Operation log entry per statement the
/// fixture reported, each `query` for a select and `execute` otherwise (the
/// fixture sends some together, so the order is not compared), and no
/// entry holds a parameter's value.
fn assert_entries_match_statements(sdk: &Sdk, response: &blackbox::Response, body: &Value) {
    let statements = body["statements"].as_array().cloned().unwrap_or_default();
    let entries = sdk.entries_of(response);
    let kind = |sql: &str| {
        let sql = sql.trim_start().to_ascii_lowercase();
        if sql.starts_with("select") || sql.starts_with("with") {
            "query"
        } else {
            "execute"
        }
    };
    let mut wanted: Vec<&str> = statements
        .iter()
        .map(|s| kind(s["sql"].as_str().unwrap_or_default()))
        .collect();
    let mut got: Vec<&str> = entries
        .iter()
        .map(|e| e["operation"].as_str().unwrap_or("<none>"))
        .collect();
    wanted.sort_unstable();
    got.sort_unstable();
    assert_eq!(
        got,
        wanted,
        "expected one Operation log entry per statement Drizzle sent, `query` for each select and `execute` for each other.\nstatements:\n{}entries:\n{}",
        lines(&statements),
        lines(&entries)
    );
    for entry in &entries {
        assert_eq!(
            entry["outcome"], "allowed",
            "expected every statement allowed: {entry}"
        );
        for statement in &statements {
            assert_entry_holds_no_parameter(entry, statement, "a statement of the values route");
        }
    }
}

#[test]
fn the_test_databases_are_set_to_time_zones_other_than_utc() {
    let sdk = Sdk::board();
    let own = sdk.own.superuser().value("SHOW timezone");
    assert_eq!(
        own.as_deref(),
        Some(OWN_TIME_ZONE),
        "the harness's own check: the own database's TimeZone"
    );
    let shared = sdk.scenario.db.superuser().value("SHOW timezone");
    assert_eq!(
        shared.as_deref(),
        Some(SHARED_TIME_ZONE),
        "the harness's own check: the shared database's TimeZone"
    );
}

#[test]
fn every_value_type_round_trips_through_drizzle_in_the_own_database() {
    let sdk = Sdk::board();
    let (response, body) = sdk.get_ok(&ann(), "/values/own?as=application");
    let tstz_text = "2026-01-01 23:34:05.678412-03:30";

    // Results: rows the migration put in, read in America/St_Johns.
    let mut seeded = vec![
        every("every", tstz_text, "2026-01-02T03:04:05.678Z"),
        row("nulls", &[]),
    ];
    seeded.extend(json_rows());
    assert_rows(&body["seeded"], &seeded, "the seeded rows, read");

    // Parameters in values(...), and what returning() gives: equal to what
    // was written. The Dates were written to the millisecond.
    let written = every("written", tstz_text, "2026-01-02T03:04:05.678Z");
    assert_rows(
        &body["inserted"],
        std::slice::from_ref(&written),
        "the row of every type, inserted and given back by returning()",
    );
    let empty = row(
        "written-empty",
        &[
            ("texts", d::array(&[])),
            ("ints", d::array(&[])),
            ("jb", d::array(&[])),
            ("j", d::object(&[])),
        ],
    );
    assert_rows(
        &body["emptyArrays"],
        std::slice::from_ref(&empty),
        "empty arrays and an empty JSON array and object, inserted",
    );
    let nulls = row("written-nulls", &[]);
    assert_rows(
        &body["writtenNulls"],
        std::slice::from_ref(&nulls),
        "a null in every column, inserted",
    );
    assert_rows(
        &body["readBack"],
        &[written.clone(), empty, nulls],
        "the written rows, read back with a select",
    );

    // Parameters in where(...): each value written finds the rows holding it.
    let both = json!(["every", "written"]);
    let mut expected_where: BTreeMap<&str, Value> = [
        "i4", "i2", "big", "bigNum", "num", "r4", "f8", "flag", "t", "vc", "u", "tsStr", "tstzStr",
        "d", "jb", "texts", "ints",
    ]
    .iter()
    .map(|c| (*c, both.clone()))
    .collect();
    // The seeded timestamps hold microseconds, a Date only milliseconds.
    expected_where.insert("ts", json!(["written"]));
    expected_where.insert("tstz", json!(["written"]));
    let got_where: BTreeMap<&str, Value> = body["where"]
        .as_object()
        .unwrap_or_else(|| panic!("expected `where` in {body}"))
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect();
    let differing: Vec<String> = expected_where
        .iter()
        .filter(|(k, v)| got_where.get(*k) != Some(*v))
        .map(|(k, v)| {
            format!(
                "  where {k} = <written value>: expected {v}, got {:?}",
                got_where.get(*k)
            )
        })
        .collect();
    assert!(
        differing.is_empty() && got_where.len() == expected_where.len(),
        "expected each written value, used as a parameter in where(), to find the rows that hold it (a JSON string \"123\" not the number 123, a BigInt exactly, an array element by element):\n{}\nall: {}",
        differing.join("\n"),
        body["where"]
    );

    // Parameters in set(...), and returning() of update and delete.
    let mut updated = written;
    for (column, value) in [
        ("big", d::bigint("-9223372036854775808")),
        ("bigNum", d::number("-9007199254740991")),
        ("num", d::string("-0.000000000000000000001000")),
        ("tstz", d::date("2030-06-07T08:09:10.123Z")),
        (
            "jb",
            d::array(&[
                d::string("x"),
                d::string("123"),
                d::null(),
                d::object(&[("k", d::null())]),
            ]),
        ),
        (
            "texts",
            d::strings(&[
                Some(","),
                Some("\""),
                Some("\\"),
                Some("{}"),
                Some(""),
                None,
            ]),
        ),
        ("ints", d::array(&[])),
    ] {
        updated.insert(column.to_owned(), value);
    }
    assert_rows(
        &body["updated"],
        std::slice::from_ref(&updated),
        "the row updated with set(...) and given back by returning()",
    );
    assert_rows(
        &body["deleted"],
        std::slice::from_ref(&updated),
        "the row deleted and given back by returning()",
    );

    assert_entries_match_statements(&sdk, &response, &body);
}

#[test]
fn every_value_type_reads_as_drizzle_declares_from_a_shared_database_in_another_time_zone() {
    let sdk = Sdk::board();
    let (response, body) = sdk.get_ok(&ann(), "/values/shared");
    let mut expected = vec![
        every(
            "every",
            "2026-01-02 08:34:05.678412+05:30",
            "2026-01-02T03:04:05.678Z",
        ),
        row("nulls", &[]),
    ];
    expected.extend(json_rows());
    assert_rows(
        &body["rows"],
        &expected,
        "the shared database's rows, read in Asia/Kolkata",
    );
    let only_every = json!(["every"]);
    let expected_where: BTreeMap<&str, Value> = [
        "big", "bigNum", "num", "tsStr", "tstzStr", "d", "jb", "texts", "ints", "u", "t",
    ]
    .iter()
    .map(|c| (*c, only_every.clone()))
    .collect();
    let got_where: BTreeMap<&str, Value> = body["where"]
        .as_object()
        .unwrap_or_else(|| panic!("expected `where` in {body}"))
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect();
    assert_eq!(
        got_where, expected_where,
        "expected each value used as a parameter in where() on the shared database to find the row that holds it"
    );
    assert_entries_match_statements(&sdk, &response, &body);
    for entry in sdk.entries_of(&response) {
        assert_eq!(
            entry["resource"], "devops",
            "expected every statement on devops: {entry}"
        );
    }
}
