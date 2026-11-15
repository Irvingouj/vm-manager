//! Names that try to break out of the generated TypeScript (CONTRACT.md,
//! "Pulling a Resource's structure: The definition"): tables, columns and
//! enum labels whose names hold a quote, an apostrophe, a backtick, `${`,
//! `*/`, a backslash, a line end, `</script>`, JavaScript's line and
//! paragraph separators (U+2028, U+2029) or a bidirectional override
//! (U+202E), also in a default's expression. The definition still
//! type-checks, and each name is intact as data: Node, importing the
//! pulled module, finds every table, column and label under its exact name
//! and the default as Postgres prints it; and no raw U+2028, U+2029 or
//! U+202E is in the definition.
//! (A Resource name that would write outside `resources/` is in
//! `pull_tool.rs`.)
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::program::manifest_dir;
use blackbox::scenario::{READERS, Scenario};
use blackbox::sdk::{TSCONFIG, TempProject, node};
use blackbox::structure::{
    HOSTILE_ENUM, HOSTILE_NAMES, NEVER_RAW, body_with_structure, hostile_setup, structure_login,
    type_check,
};
use blackbox::tool::Machine;
use serde_json::Value;

fn rita() -> User {
    User::new("rita").in_groups(&[READERS])
}

const MANIFEST: &str = r#"import { defineManifest } from "@vm/sdk";
export default defineManifest({
  name: "odd-names",
  description: "Names that need escaping.",
  resources: { odd: { kind: "postgres", access: "read" } },
});
"#;

const INDEX: &str = r#"import { defineApp } from "@vm/sdk";
import * as odd from "../resources/odd/schema.ts";
import manifest from "../manifest.ts";

export default defineApp(manifest, {
  async fetch(_request, _ctx) {
    return Response.json({ exports: Object.keys(odd).length });
  },
});
"#;

/// A project to pull `odd` into.
fn odd_project() -> TempProject {
    let project = TempProject::new("odd-names");
    project
        .write("manifest.ts", MANIFEST)
        .write("tsconfig.json", TSCONFIG)
        .write("src/index.ts", INDEX)
        .write("src/db/schema.ts", "export {};\n");
    project
}

/// What `fixtures/structure-names.mts` lists of the module at `schema`.
fn listed(schema: &std::path::Path) -> Value {
    let script = manifest_dir().join("fixtures").join("structure-names.mts");
    let run = node(&[script.to_str().unwrap(), schema.to_str().unwrap()]);
    assert_eq!(run.code, Some(0), "{}", run.describe());
    serde_json::from_str(run.stdout.trim())
        .unwrap_or_else(|e| panic!("expected one JSON object ({e})\n{}", run.describe()))
}

#[test]
fn hostile_names_do_not_break_out_of_the_definition_and_stay_intact_as_data() {
    let scenario = Scenario::start();
    scenario.db.superuser().batch(&hostile_setup());
    let login = structure_login(&scenario.db);
    scenario.register_body("odd", &body_with_structure("odd", &scenario.db, &login));
    scenario.grant("odd", READERS, "structure");
    let project = odd_project();
    let machine = Machine::new();
    machine.sign_in(&scenario.vm, &rita());
    machine
        .run(&[
            "pull",
            "odd",
            "--project",
            project.dir.to_str().unwrap(),
            "--json",
        ])
        .ok_json();
    let schema = project.path("resources/odd/schema.ts");
    let checked = type_check(&project.dir);
    assert_eq!(
        checked.code,
        Some(0),
        "expected the definition of hostile names to type-check\n{}\nthe definition:\n{}",
        checked.describe(),
        std::fs::read_to_string(&schema).unwrap_or_default()
    );

    let text = std::fs::read_to_string(&schema).unwrap();
    for c in NEVER_RAW {
        assert!(
            !text.contains(c),
            "expected U+{:04X} never raw in the definition (escaped instead), but it is",
            u32::from(c)
        );
    }
    let found = listed(&schema);
    // The default's expression intact as data: as Postgres prints it.
    let printed = scenario
        .db
        .superuser()
        .value(&format!(
            "SELECT pg_get_expr(d.adbin, d.adrelid) FROM pg_attrdef d JOIN pg_attribute a
               ON a.attrelid = d.adrelid AND a.attnum = d.adnum
              WHERE d.adrelid = {}::regclass AND a.attname = 'note'",
            blackbox::structure::literal(&blackbox::structure::ident(HOSTILE_NAMES[0]))
        ))
        .unwrap();
    let tables = found["tables"].as_array().unwrap();
    let mut wrong = Vec::new();
    for table in HOSTILE_NAMES {
        let Some(t) = tables.iter().find(|t| t["name"].as_str() == Some(table)) else {
            wrong.push(format!("  no table named {table:?}"));
            continue;
        };
        let columns: Vec<&str> = t["columns"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        if t["defaults"]["note"].as_str() != Some(printed.as_str()) {
            wrong.push(format!(
                "  the default of {table:?}.note is {}, expected the expression as Postgres prints it, {printed:?}",
                t["defaults"]["note"]
            ));
        }
        let mut want: Vec<&str> = vec!["id"];
        want.extend(HOSTILE_NAMES);
        want.push("mood");
        want.push("note");
        if columns != want {
            wrong.push(format!(
                "  the table {table:?} has the columns {columns:?}, expected {want:?}"
            ));
        }
    }
    let enums = found["enums"].as_array().unwrap();
    match enums
        .iter()
        .find(|e| e["name"].as_str() == Some(HOSTILE_ENUM))
    {
        Some(e) => {
            let labels: Vec<&str> = e["labels"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect();
            if labels != HOSTILE_NAMES {
                wrong.push(format!(
                    "  the enum has the labels {labels:?}, expected {HOSTILE_NAMES:?}"
                ));
            }
        }
        None => wrong.push(format!("  no enum named {HOSTILE_ENUM:?} in {found}")),
    }
    assert!(
        wrong.is_empty(),
        "expected every hostile name intact as data in the pulled module:\n{}\nfound {found}",
        wrong.join("\n")
    );
}

/// The harness's own check, which holds today: the hostile structure is
/// made as intended (each name exactly, as Postgres keeps it), and the
/// listing script reads a hand-written module with such names.
#[test]
fn harness_the_hostile_structure_is_made_and_listed_as_intended() {
    let scenario = Scenario::start();
    scenario.db.superuser().batch(&hostile_setup());
    let rows = scenario.db.superuser().batch(&format!(
        "SELECT c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
          WHERE n.nspname = 'public' AND c.relkind = 'r' AND c.relname = ANY (ARRAY[{}]) ORDER BY 1",
        HOSTILE_NAMES
            .iter()
            .map(|n| blackbox::structure::literal(n))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    assert_eq!(rows.len(), HOSTILE_NAMES.len(), "{rows:?}");
    let labels = scenario
        .db
        .superuser()
        .batch(&format!(
            "SELECT enumlabel FROM pg_enum WHERE enumtypid = {}::regtype ORDER BY enumsortorder",
            blackbox::structure::literal(&blackbox::structure::ident(HOSTILE_ENUM))
        ))
        .into_iter()
        .filter_map(|r| r.into_iter().next().flatten())
        .collect::<Vec<_>>();
    assert_eq!(labels, HOSTILE_NAMES);

    let project = odd_project();
    let written = HOSTILE_NAMES
        .iter()
        .map(|n| serde_json::to_string(n).unwrap())
        .collect::<Vec<_>>();
    project.write(
        "resources/odd/schema.ts",
        &format!(
            "import {{ pgEnum, pgTable, text }} from \"drizzle-orm/pg-core\";\nexport const mood = pgEnum({}, [{}]);\nexport const t = pgTable({}, {{ c: text({}) }});\n",
            serde_json::to_string(HOSTILE_ENUM).unwrap(),
            written.join(", "),
            written[0],
            written[3]
        ),
    );
    let found = listed(&project.path("resources/odd/schema.ts"));
    assert_eq!(found["tables"][0]["name"].as_str(), Some(HOSTILE_NAMES[0]));
    assert_eq!(
        found["tables"][0]["columns"][0].as_str(),
        Some(HOSTILE_NAMES[3])
    );
    assert_eq!(found["enums"][0]["name"].as_str(), Some(HOSTILE_ENUM));
}
