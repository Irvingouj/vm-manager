//! The pulled definition compiles, typed queries type-check against it, and
//! a Version built from it reads the fixture database (CONTRACT.md,
//! "Pulling a Resource's structure: The definition", "The tool"; "The SDK:
//! A project"; ticket "Pull a Resource's structure as a typed Drizzle
//! definition": pulling a fixture database with the common column types,
//! keys, foreign keys and enums writes a definition that compiles, and
//! typed queries through `ctx.user().db(name)` type-check against it).
//!
//! The fixture project `fixtures/sdk/stockroom` holds a hand-written
//! definition of the part of the fixture database it uses (so that it
//! compiles in `npm test` before anything is pulled), application code
//! (`src/index.ts`) and type checks (`src/typed.ts`: selects with typed
//! columns, a join through a foreign key, inserts with and without the
//! required columns, an enum's labels as a union, the unsupported columns
//! typed `unknown`). Each test copies it into a temporary project under the
//! repository's `target/`, removes the hand-written `resources/stock/`,
//! pulls with `vm pull` into the copy, and runs `tsc` over the copy.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;

use blackbox::User;
use blackbox::scenario::READERS;
use blackbox::sdk::{SDK_REQUEST_TIMEOUT, build, fixture_project, install};
use blackbox::structure::{STOCK, Stock, copy_project, type_check};
use blackbox::tool::Machine;
use serde_json::{Value, json};

const PROJECT: &str = "stockroom";

fn rita() -> User {
    User::new("rita").in_groups(&[READERS])
}

/// Sends a project built from `dir` as a Version, then `GET`s each route as
/// [`rita`] and returns the bodies.
fn run_routes(stock: &Stock, dir: &std::path::Path) -> Vec<(String, Value)> {
    let built = build(dir).unwrap_or_else(|problem| panic!("{problem}"));
    install(&stock.scenario.vm, &built);
    ["/items", "/open", "/accounts", "/unsupported"]
        .iter()
        .map(|route| {
            let response = stock
                .scenario
                .vm
                .request(&built.name, route)
                .as_user(&rita())
                .timeout(SDK_REQUEST_TIMEOUT)
                .send();
            if response.status != 200 {
                stock.scenario.vm.fail(&format!(
                    "expected `200` from {route}, got\n{}",
                    response.describe()
                ));
            }
            ((*route).to_owned(), response.json())
        })
        .collect()
}

/// What the routes answer over the fixture's rows.
fn expected_rows() -> Vec<(String, Value)> {
    vec![
        (
            "/items".to_owned(),
            json!({ "rows": [
                { "id": 1, "site": "north", "label": "Bolts", "status": "open", "price": "1.50" },
                { "id": 2, "site": "south", "label": "Nuts", "status": "in progress", "price": null },
                { "id": 3, "site": "north", "label": "Gears", "status": "closed", "price": "12.00" },
            ] }),
        ),
        (
            "/open".to_owned(),
            json!({ "rows": [ { "id": 1, "status": "open" }, { "id": 2, "status": "in progress" } ] }),
        ),
        (
            "/accounts".to_owned(),
            json!({ "rows": [ { "name": "Acme", "tier": "gold" }, { "name": "Globex", "tier": "bronze" } ] }),
        ),
        (
            "/unsupported".to_owned(),
            json!({ "rows": [ { "id": 1, "area": "((0,0),(1,0),(1,1))" }, { "id": 2, "area": null } ] }),
        ),
    ]
}

#[test]
fn the_pulled_definition_compiles_and_typed_queries_type_check_against_it() {
    let stock = Stock::ready();
    let project = copy_project(PROJECT);
    fs::remove_dir_all(project.path("resources/stock")).unwrap();
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    machine
        .run(&[
            "pull",
            STOCK,
            "--project",
            project.dir.to_str().unwrap(),
            "--json",
        ])
        .ok_json();
    let checked = type_check(&project.dir);
    assert_eq!(
        checked.code,
        Some(0),
        "expected the project, with the pulled definition, to type-check under its tsconfig.json (application code and src/typed.ts included)\n{}\nthe pulled definition:\n{}",
        checked.describe(),
        fs::read_to_string(project.path("resources/stock/schema.ts")).unwrap_or_default()
    );
}

#[test]
fn a_version_built_from_the_pulled_definition_reads_the_fixture_database() {
    let stock = Stock::ready();
    let project = copy_project(PROJECT);
    fs::remove_dir_all(project.path("resources/stock")).unwrap();
    let machine = Machine::new();
    machine.sign_in(&stock.scenario.vm, &rita());
    machine
        .run(&[
            "pull",
            STOCK,
            "--project",
            project.dir.to_str().unwrap(),
            "--json",
        ])
        .ok_json();
    assert_eq!(run_routes(&stock, &project.dir), expected_rows());
}

/// The harness's own check, which holds today: the fixture project, with
/// its hand-written definition of the same fixture database, type-checks;
/// without its `@ts-expect-error` lines it does not (each check bites);
/// and built and sent as a Version, it reads the fixture database as the
/// tests above expect.
#[test]
fn harness_the_hand_written_definition_type_checks_and_reads_the_fixture_database() {
    let checked = type_check(&fixture_project(PROJECT));
    assert_eq!(checked.code, Some(0), "{}", checked.describe());

    let project = copy_project(PROJECT);
    let typed = fs::read_to_string(project.path("src/typed.ts")).unwrap();
    let directive = |l: &str| l.trim_start().starts_with("// @ts-expect-error");
    let expectations = typed.lines().filter(|l| directive(l)).count();
    assert_eq!(expectations, 3);
    let without: String = typed
        .lines()
        .filter(|l| !directive(l))
        .map(|l| format!("{l}\n"))
        .collect();
    fs::write(project.path("src/typed.ts"), without).unwrap();
    let failing = type_check(&project.dir);
    assert_ne!(failing.code, Some(0), "{}", failing.describe());
    assert_eq!(
        failing.stdout.matches("src/typed.ts(").count(),
        expectations,
        "expected one error for each `@ts-expect-error` removed\n{}",
        failing.describe()
    );

    let wrong = copy_project(PROJECT);
    let typed = fs::read_to_string(wrong.path("src/typed.ts")).unwrap();
    fs::write(
        wrong.path("src/typed.ts"),
        typed.replace(
            "Equal<typeof site.area, unknown>",
            "Equal<typeof site.area, string | null>",
        ),
    )
    .unwrap();
    let failing = type_check(&wrong.dir);
    assert_ne!(
        failing.code,
        Some(0),
        "expected the check that a column is typed `unknown` to bite\n{}",
        failing.describe()
    );

    let stock = Stock::ready();
    assert_eq!(
        run_routes(&stock, &fixture_project(PROJECT)),
        expected_rows()
    );
}
