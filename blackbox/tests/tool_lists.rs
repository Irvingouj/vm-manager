//! `vibe resources` and `vibe applications` (CONTRACT.md, "The tool: Listing
//! Resources", "Listing Applications"; ticket "The tool signs in, and lists
//! Resources and Applications": every registered Resource, also those the
//! Builder holds no Grant on, with the description, the Builder's Access
//! level and whom to ask, and no structure; the Applications the Builder
//! contributes to, with name, description and address).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::scenario::{Scenario, administrator, assert_none_holds, resource_body};
use blackbox::shared_database::find_port;
use blackbox::tool::{Machine, Run};
use blackbox::{DOMAIN, User, Vm, VmOptions};
use serde_json::{Value, json};

fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

/// The User most of these tests list as.
fn wren() -> User {
    User::new("wren").in_groups(&["readers", "analysts", "warehouse-team"])
}

/// A Grant: (Group, Access level).
type Grant = Option<(&'static str, &'static str)>;

/// (name, description, owner, contact, the Grant).
const RESOURCES: [(&str, &str, &str, &str, Grant); 4] = [
    (
        "billing",
        "Invoices and payments ledger",
        "The Finance team",
        "Fiona Ledger, #billing-access",
        None,
    ),
    (
        "devops",
        "Incident tracking store",
        "The DevOps team",
        "Dana Ops, #devops-access",
        Some(("readers", "read")),
    ),
    (
        "metrics",
        "Service measurements over time",
        "The SRE team",
        "Sam Gauge, #metrics-access",
        Some(("analysts", "structure")),
    ),
    (
        "warehouse",
        "Stock levels per site",
        "The Logistics team",
        "Wanda Crate, #warehouse-access",
        Some(("warehouse-team", "write")),
    ),
];

/// A program with the four [`RESOURCES`] registered and granted; also a
/// Grant of `read` on `warehouse` to `readers`, below the `write` [`wren`]
/// holds through another Group.
fn four_resources() -> Scenario {
    let s = Scenario::start();
    for (name, description, owner, contact, grant) in RESOURCES {
        let mut body = resource_body(name, &s.db, &s.db.reader);
        body["description"] = json!(description);
        body["owner"] = json!(owner);
        body["contact"] = json!(contact);
        s.register_body(name, &body);
        if let Some((group, access)) = grant {
            s.grant(name, group, access);
        }
    }
    s.grant("warehouse", "readers", "read");
    s
}

#[test]
fn vm_resources_lists_every_resource_with_the_users_access_and_whom_to_ask() {
    let s = four_resources();
    let machine = Machine::new();
    machine.sign_in(&s.vm, &wren());
    let listed = machine.run(&["resources", "--json"]).ok_json();
    let want: Vec<Value> = RESOURCES
        .iter()
        .map(|(name, description, owner, contact, grant)| {
            json!({
                "name": name, "kind": "postgres", "description": description, "owner": owner,
                "contact": contact, "your_access": grant.map(|(_, access)| access),
            })
        })
        .collect();
    assert_eq!(
        listed,
        json!({ "resources": want }),
        "expected every registered Resource, by name, with wren's highest Access level"
    );
    for r in listed["resources"].as_array().unwrap() {
        assert_eq!(
            keys(r),
            [
                "contact",
                "description",
                "kind",
                "name",
                "owner",
                "your_access"
            ],
            "{r}"
        );
    }

    // In words: one block per Resource, in order; each block from its name
    // to the next one's.
    let text = machine.run(&["resources"]).ok_text();
    let mut starts: Vec<usize> = Vec::new();
    for (name, ..) in RESOURCES {
        let at = text
            .find(name)
            .unwrap_or_else(|| panic!("expected {name:?} in the words, got\n{text}"));
        starts.push(at);
    }
    assert!(
        starts.windows(2).all(|w| w[0] < w[1]),
        "expected the Resources by name in the words, got\n{text}"
    );
    for (i, (name, description, owner, contact, grant)) in RESOURCES.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(text.len());
        let block = text.get(starts[i]..end).unwrap();
        for fact in [description, owner, contact] {
            assert!(
                block.contains(fact),
                "expected {fact:?} in {name:?}'s block, got\n{block}"
            );
        }
        let lower = block.to_lowercase();
        match grant {
            None => assert!(
                block.contains(&format!("no access: ask {contact}")),
                "expected {name:?}'s block to say `no access: ask {contact}`, got\n{block}"
            ),
            Some((_, access)) => {
                assert!(
                    !lower.contains("no access"),
                    "expected {name:?}'s block not to say there is no access, got\n{block}"
                );
                assert!(
                    lower.contains(access),
                    "expected {name:?}'s block to say the level {access:?}, got\n{block}"
                );
            }
        }
        if grant.is_some_and(|(_, access)| access == "structure") {
            assert!(
                lower.contains("rows"),
                "expected {name:?}'s block, held at structure, to say that rows cannot be read at that level, got\n{block}"
            );
        }
    }
}

#[test]
fn vm_resources_shows_nothing_of_a_resources_connection_or_structure() {
    let s = Scenario::start();
    s.register_writable("devops");
    s.grant("devops", "readers", "write");
    let mut runs: Vec<Run> = Vec::new();
    for user in [
        User::new("rita").in_groups(&["readers"]),
        User::new("nobody"),
        administrator(),
    ] {
        let machine = Machine::new();
        machine.sign_in(&s.vm, &user);
        for args in [vec!["resources", "--json"], vec!["resources"]] {
            let run = machine.run(&args);
            run.expect_code(0);
            runs.push(run);
        }
    }
    let texts: Vec<(String, String)> = runs.iter().flat_map(Run::texts).collect();
    let mut markers = s.db.credential_markers();
    for inside in [
        "incidents",
        "staff_only",
        "reported_at",
        "severity",
        "ticket_numbers",
    ] {
        markers.push(("a name of what is inside the Resource", inside.to_owned()));
    }
    markers.push(("a value from a row", s.db.row_marker.clone()));
    assert_none_holds(
        &texts,
        &markers,
        "`vibe resources` shows no structure and nothing of a Resource's connection",
    );
    for (whence, text) in &texts {
        assert!(
            !find_port(text, s.db.port()),
            "expected the shared database's port in no output, found it in {whence}:\n{text}"
        );
    }
}

#[test]
fn vm_resources_with_none_registered() {
    let s = Scenario::start();
    let machine = Machine::new();
    machine.sign_in(&s.vm, &wren());
    assert_eq!(
        machine.run(&["resources", "--json"]).ok_json(),
        json!({ "resources": [] })
    );
    let text = machine.run(&["resources"]).ok_text().to_lowercase();
    assert!(
        text.contains("administrator") && text.contains("regist"),
        "expected the words to say that none is registered and that a Platform administrator registers them, got\n{text}"
    );
}

#[test]
fn vm_applications_lists_those_the_user_contributes_to() {
    let vm = Vm::start(&VmOptions::new().stand_in_sign_in());
    let alice = User::new("alice").in_groups(&["staff"]);
    let as_alice = vm.api_as(&alice);
    as_alice
        .create_application_with(
            &json!({ "name": "zeta-board", "description": "Where incidents are triaged" }),
        )
        .expect_status(201);
    as_alice
        .create_application_with(
            &json!({ "name": "alpha-tool", "description": "Rota of who is on call" }),
        )
        .expect_status(201);
    as_alice
        .send_bundle("zeta-board", "globalThis.default = {};")
        .expect_status(201);
    vm.api_as(&User::new("bob"))
        .create_application_with(&json!({ "name": "bobs-app", "description": "Bob's own" }))
        .expect_status(201);

    let machine = Machine::new();
    machine.sign_in(&vm, &alice);
    let listed = machine.run(&["applications", "--json"]).ok_json();
    assert_eq!(
        listed,
        json!({ "applications": [
            { "name": "alpha-tool", "description": "Rota of who is on call",
              "address": format!("alpha-tool.{DOMAIN}"), "contributor": true, "serving_version": null,
              "disabled": false },
            { "name": "zeta-board", "description": "Where incidents are triaged",
              "address": format!("zeta-board.{DOMAIN}"), "contributor": true, "serving_version": 1,
              "disabled": false },
        ] }),
        "expected alice's Applications by name, and not bob's"
    );
    assert_eq!(
        listed,
        as_alice.list_applications().json(),
        "expected the tool to print what GET /v1/applications answers"
    );

    let text = machine.run(&["applications"]).ok_text();
    let alpha = text
        .find("alpha-tool")
        .unwrap_or_else(|| panic!("expected alpha-tool in the words, got\n{text}"));
    let zeta = text
        .find("zeta-board")
        .unwrap_or_else(|| panic!("expected zeta-board in the words, got\n{text}"));
    assert!(
        alpha < zeta,
        "expected the Applications by name, got\n{text}"
    );
    for fact in [
        "Rota of who is on call",
        "Where incidents are triaged",
        &format!("alpha-tool.{DOMAIN}"),
        &format!("zeta-board.{DOMAIN}"),
    ] {
        assert!(
            text.contains(fact),
            "expected {fact:?} in the words, got\n{text}"
        );
    }
    assert!(
        !text.contains("bobs-app") && !text.contains("Bob's own"),
        "expected bob's Application not listed, got\n{text}"
    );
}

#[test]
fn vm_applications_with_none() {
    let vm = Vm::start(&VmOptions::new().stand_in_sign_in());
    vm.api_as(&User::new("bob"))
        .create_application("bobs-app")
        .expect_status(201);
    let machine = Machine::new();
    machine.sign_in(&vm, &User::new("carol").in_groups(&["staff"]));
    assert_eq!(
        machine.run(&["applications", "--json"]).ok_json(),
        json!({ "applications": [] })
    );
    let text = machine.run(&["applications"]).ok_text();
    assert!(
        text.to_lowercase().contains("application") && !text.contains("bobs-app"),
        "expected the words to say carol contributes to no Application yet, got\n{text}"
    );
}

/// The code points written as `\u{<hex>}` in `text`, in any letter case
/// and with or without leading zeros.
fn escapes_in(text: &str) -> Vec<u32> {
    text.split("\\u{")
        .skip(1)
        .filter_map(|rest| {
            let hex = rest.split('}').next()?;
            u32::from_str_radix(hex, 16).ok()
        })
        .collect()
}

/// The bidirectional marks and overrides the contract names.
fn is_bidi(c: char) -> bool {
    matches!(u32::from(c), 0x061C | 0x200E | 0x200F | 0x202A..=0x202E | 0x2066..=0x2069)
}

/// Fails unless `text` holds no control character but its own line ends,
/// and no bidirectional mark or override.
fn assert_no_raw_control(text: &str, context: &str) {
    let raw: Vec<String> = text
        .chars()
        .filter(|c| (c.is_control() && *c != '\n') || is_bidi(*c))
        .map(|c| format!("U+{:04X}", u32::from(c)))
        .collect();
    assert!(
        raw.is_empty(),
        "{context}: expected no raw control character but line ends, found {raw:?} in\n{}",
        text.escape_debug()
    );
}

#[test]
fn without_json_no_control_character_of_the_installations_reaches_the_terminal() {
    let s = Scenario::start();
    let escape = "\u{1b}[2J\u{1b}[31mred\u{7}";
    let description = format!("Invoices{escape}\nfake line\r\tend");
    let owner = "Finance\u{1b}]0;title\u{7}team";
    let contact = "Ask\u{8}\u{8}\u{8}Fiona\u{7f}\u{9b}31m";
    let mut body = resource_body("billing", &s.db, &s.db.reader);
    body["description"] = json!(description);
    body["owner"] = json!(owner);
    body["contact"] = json!(contact);
    s.register_body("billing", &body);
    let alice = User::new("alice");
    let app_description = format!("Board{escape}\nsecond line");
    s.vm.api_as(&alice)
        .create_application_with(&json!({ "name": "board", "description": app_description }))
        .expect_status(201);
    let machine = Machine::new();
    machine.sign_in(&s.vm, &alice);

    let words = machine.run(&["resources"]).ok_text();
    assert_no_raw_control(&words, "`vibe resources`");
    assert!(
        words.contains("\\u{1b}"),
        "expected the escape character printed as \\u{{1b}}, got\n{}",
        words.escape_debug()
    );
    assert!(
        words.contains("\\u{a}") || words.contains("\\u{0a}"),
        "expected the description's line end printed as \\u{{a}}, got\n{}",
        words.escape_debug()
    );
    let words = machine.run(&["applications"]).ok_text();
    assert_no_raw_control(&words, "`vibe applications`");
    assert!(words.contains("\\u{1b}"), "{}", words.escape_debug());

    let listed = machine.run(&["resources", "--json"]).ok_json();
    let r = &listed["resources"][0];
    assert_eq!(r["description"], json!(description));
    assert_eq!(r["owner"], json!(owner));
    assert_eq!(r["contact"], json!(contact));
    let run = machine.run(&["resources", "--json"]);
    assert_no_raw_control(&run.stdout, "`vibe resources --json`");
    let listed = machine.run(&["applications", "--json"]).ok_json();
    assert_eq!(
        listed["applications"][0]["description"],
        json!(app_description)
    );
}

#[test]
fn bidirectional_marks_and_overrides_are_escaped_too() {
    let s = Scenario::start();
    let marks: Vec<char> = [0x061C_u32, 0x200E, 0x200F]
        .into_iter()
        .chain(0x202A..=0x202E)
        .chain(0x2066..=0x2069)
        .map(|c| char::from_u32(c).unwrap())
        .collect();
    let all: String = marks.iter().collect();
    let description = format!("Ledger \u{202e}gnp.exe{all} end");
    let owner = "Finance \u{2067}team\u{2069}";
    let contact = "Fiona \u{200f}\u{061c}#billing";
    let mut body = resource_body("billing", &s.db, &s.db.reader);
    body["description"] = json!(description);
    body["owner"] = json!(owner);
    body["contact"] = json!(contact);
    s.register_body("billing", &body);
    let alice = User::new("alice");
    let app_description = format!("Board \u{202d}{all}");
    s.vm.api_as(&alice)
        .create_application_with(&json!({ "name": "board", "description": app_description }))
        .expect_status(201);
    let machine = Machine::new();
    machine.sign_in(&s.vm, &alice);

    for command in ["resources", "applications"] {
        let words = machine.run(&[command]).ok_text();
        assert_no_raw_control(&words, &format!("`vibe {command}`"));
        let escaped = escapes_in(&words);
        assert!(
            escaped.contains(&0x202e) || escaped.contains(&0x202d),
            "expected an override printed as \\u{{<hex>}} by `vibe {command}`, got\n{}",
            words.escape_debug()
        );
        let run = machine.run(&[command, "--json"]);
        let listed = run.ok_json();
        assert_no_raw_control(&run.stdout, &format!("`vibe {command} --json`"));
        assert!(
            run.stdout.to_lowercase().contains("\\u202"),
            "expected the overrides written as JSON escapes (\\u202e) by `vibe {command} --json`, got\n{}",
            run.stdout.escape_debug()
        );
        if command == "resources" {
            let r = &listed["resources"][0];
            assert_eq!(r["description"], json!(description));
            assert_eq!(r["owner"], json!(owner));
            assert_eq!(r["contact"], json!(contact));
        } else {
            assert_eq!(
                listed["applications"][0]["description"],
                json!(app_description)
            );
        }
    }
    let words = machine.run(&["resources"]).ok_text();
    let escaped = escapes_in(&words);
    for mark in &marks {
        assert!(
            escaped.contains(&u32::from(*mark)),
            "expected U+{:04X} printed as \\u{{<hex>}} in the words of `vibe resources`, got\n{}",
            u32::from(*mark),
            words.escape_debug()
        );
    }
}
