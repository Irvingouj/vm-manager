//! The list of hosts Applications may call, through the platform's API
//! (CONTRACT.md, "Outbound calls: The list"): empty in a new
//! installation; `PUT` adds and answers every field, once more it says who
//! added it first; `GET` by anyone signed in in the order of `host`;
//! `DELETE` removes; every form of an entry in the contract's table is
//! taken and answered as kept (lower case, `xn--`, without `:443`); an
//! entry that is none of them is `400` `malformed` and adds nothing; at
//! most 1,000 entries (`409` `too-many`); no body is looked at; a change
//! survives a restart; the program's log has a record for a change and none
//! for what changed nothing.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use blackbox::admin::{administrator, other_administrator};
use blackbox::assert_api_error;
use blackbox::outbound_surface::{
    HOSTS, MOST, add, alice, assert_added, assert_deleted, assert_put_answer, bob, change_records,
    delete, entries, entries_as, entries_of, hosts, list, path_of, put, records_of, request, start,
    wait_for_records,
};
use blackbox::tool::bearer;
use serde_json::json;

/// A name of `bytes` bytes made of labels of 63 letters and a last one of
/// what is left.
fn name_of(bytes: usize) -> String {
    let mut labels: Vec<String> = Vec::new();
    let mut left = bytes;
    while left > 0 {
        // A label of 63 and its dot, until the last, which has no dot.
        if left > 64 {
            labels.push("a".repeat(63));
            left -= 64;
        } else {
            labels.push("a".repeat(left));
            left = 0;
        }
    }
    labels.join(".")
}

#[test]
fn a_new_installations_list_is_empty_and_everyone_signed_in_reads_it() {
    let vm = start();
    let carol = alice();
    vm.api_as(&carol)
        .create_application("board")
        .expect_status(201);
    for user in [administrator(), alice(), bob(), other_administrator()] {
        let answer = list(&vm, &user);
        assert!(
            answer.status == 200,
            "expected `200` for {:?} reading the list, got\n{}",
            user.id,
            answer.describe()
        );
        assert_eq!(
            answer.json(),
            json!({ "hosts": [] }),
            "{}: a new installation's list is empty",
            user.id
        );
        blackbox::api::assert_json_content_type(&answer);
    }
    // By a tool credential.
    let (credential, _) = vm.api_as(&bob()).tool_credential();
    let answer = bearer(&vm, HOSTS, &credential).send();
    assert!(
        answer.status == 200 && answer.json() == json!({ "hosts": [] }),
        "expected `200` and an empty list with a tool credential, got\n{}",
        answer.describe()
    );
}

#[test]
fn put_adds_and_answers_every_field_and_again_it_says_who_added_it_first_and_when() {
    let vm = start();
    let first = administrator();
    let body = assert_added(
        &put(&vm, &first, "api.example.com"),
        "api.example.com",
        &first.id,
        "the first PUT",
    );
    let again = put(&vm, &other_administrator(), "api.example.com");
    let second = assert_put_answer(
        &again,
        "api.example.com",
        false,
        &first.id,
        "the second PUT, by another administrator",
    );
    assert_eq!(
        second["added_at"], body["added_at"],
        "an entry already there: the time it was first added"
    );
    let listed = entries(&vm);
    assert_eq!(
        listed,
        vec![json!({
            "host": "api.example.com",
            "added_by": first.id,
            "added_at": body["added_at"],
        })],
        "the list holds the entry once, as first added"
    );
}

#[test]
fn the_list_is_answered_to_everyone_signed_in_alike_in_the_order_of_host() {
    let vm = start();
    for entry in [
        "c.example.com",
        "a.example.com",
        "b.example.org",
        "a.example.net",
    ] {
        add(&vm, entry);
    }
    let want = [
        "a.example.com",
        "a.example.net",
        "b.example.org",
        "c.example.com",
    ];
    let reference = entries(&vm);
    let got: Vec<&str> = reference
        .iter()
        .map(|e| e["host"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(got, want, "the order of `host`");
    for user in [alice(), bob(), other_administrator()] {
        assert_eq!(
            entries_as(&vm, &user),
            reference,
            "{}: the same list as the administrator reads",
            user.id
        );
    }
    let (credential, _) = vm.api_as(&alice()).tool_credential();
    let answer = bearer(&vm, HOSTS, &credential).send();
    assert!(
        answer.status == 200,
        "expected `200` with a tool credential, got\n{}",
        answer.describe()
    );
    assert_eq!(entries_of(&answer), reference, "with a tool credential");
    let (credential, _) = vm.api_as(&administrator()).tool_credential();
    let answer = bearer(&vm, HOSTS, &credential).send();
    assert_eq!(
        entries_of(&answer),
        reference,
        "with an administrator's tool credential"
    );
}

#[test]
fn delete_removes_and_says_whether_it_changed_anything() {
    let vm = start();
    let admin = administrator();
    add(&vm, "api.example.com");
    add(&vm, "other.example.com");
    assert_deleted(
        &delete(&vm, &admin, "api.example.com"),
        "api.example.com",
        true,
        "the first DELETE",
    );
    assert_eq!(hosts(&vm), ["other.example.com"], "only the other is left");
    assert_deleted(
        &delete(&vm, &other_administrator(), "api.example.com"),
        "api.example.com",
        false,
        "the second DELETE",
    );
    assert_deleted(
        &delete(&vm, &admin, "never.example.com"),
        "never.example.com",
        false,
        "an entry that was never there",
    );
    assert_eq!(hosts(&vm), ["other.example.com"]);
    // It can be added again, as a new entry.
    assert_added(
        &put(&vm, &admin, "api.example.com"),
        "api.example.com",
        &admin.id,
        "added again",
    );
}

#[test]
fn every_form_of_an_entry_is_taken_and_answered_as_kept() {
    let vm = start();
    let admin = administrator();
    // (what is given, what is kept).
    let table = [
        ("api.example.com", "api.example.com"),
        ("api.example.com:8443", "api.example.com:8443"),
        ("*.example.com", "*.example.com"),
        ("*.example.com:8443", "*.example.com:8443"),
        ("203.0.113.7", "203.0.113.7"),
        ("203.0.113.7:8443", "203.0.113.7:8443"),
        ("[2001:db8::7]", "[2001:db8::7]"),
        ("[2001:db8::7]:8443", "[2001:db8::7]:8443"),
        ("Mixed.Example.ORG", "mixed.example.org"),
        ("bücher.example", "xn--bcher-kva.example"),
        ("a-b.example.net", "a-b.example.net"),
        ("1st.example.net", "1st.example.net"),
        ("*.co.uk", "*.co.uk"),
        ("edge.example.com:1", "edge.example.com:1"),
        ("edge.example.com:65535", "edge.example.com:65535"),
        ("localhost", "localhost"),
    ];
    for (given, kept) in table {
        assert_added(
            &put(&vm, &admin, given),
            kept,
            &admin.id,
            &format!("PUT {given:?}"),
        );
    }
    let mut want: Vec<String> = table.iter().map(|(_, kept)| (*kept).to_owned()).collect();
    want.sort();
    assert_eq!(
        hosts(&vm),
        want,
        "every entry once, as kept, in the order of `host` (byte order)"
    );
    // The same entries in other forms are the same entries.
    let aliases = [
        ("API.EXAMPLE.COM", "api.example.com"),
        ("api.example.com:443", "api.example.com"),
        ("*.example.com:443", "*.example.com"),
        ("203.0.113.7:443", "203.0.113.7"),
        ("[2001:db8::7]:443", "[2001:db8::7]"),
        ("xn--bcher-kva.example", "xn--bcher-kva.example"),
        ("XN--BCHER-KVA.EXAMPLE", "xn--bcher-kva.example"),
        ("MIXED.example.org", "mixed.example.org"),
    ];
    for (given, kept) in aliases {
        let answer = put(&vm, &other_administrator(), given);
        assert_put_answer(
            &answer,
            kept,
            false,
            &admin.id,
            &format!("PUT {given:?} (the same entry)"),
        );
    }
    assert_eq!(hosts(&vm), want, "no entry was added by an alias");
    // A host on port 443 is written without it, also in the path as it is
    // written in the contract's table.
    let raw = request(
        &vm,
        Some(&admin),
        "PUT",
        "/v1/outbound-hosts/api.example.com:443",
    )
    .send();
    assert_put_answer(
        &raw,
        "api.example.com",
        false,
        &admin.id,
        "PUT .../api.example.com:443 (not encoded)",
    );
    let raw = request(
        &vm,
        Some(&admin),
        "PUT",
        "/v1/outbound-hosts/api.example.com:8443",
    )
    .send();
    assert_put_answer(
        &raw,
        "api.example.com:8443",
        false,
        &admin.id,
        "PUT .../api.example.com:8443 (not encoded)",
    );
    let raw = request(&vm, Some(&admin), "PUT", "/v1/outbound-hosts/*.example.com").send();
    assert_put_answer(
        &raw,
        "*.example.com",
        false,
        &admin.id,
        "PUT .../*.example.com (not encoded)",
    );
}

#[test]
fn a_name_and_the_same_name_on_a_port_are_two_entries_and_neither_allows_the_other() {
    let vm = start();
    add(&vm, "api.example.com");
    add(&vm, "api.example.com:8443");
    add(&vm, "api.example.com:8444");
    assert_eq!(
        hosts(&vm),
        [
            "api.example.com",
            "api.example.com:8443",
            "api.example.com:8444"
        ]
    );
    assert_deleted(
        &delete(&vm, &administrator(), "api.example.com:8443"),
        "api.example.com:8443",
        true,
        "deleting one of the three",
    );
    assert_eq!(hosts(&vm), ["api.example.com", "api.example.com:8444"]);
}

#[test]
fn a_name_at_its_limits_is_taken_as_a_name_one_byte_longer_is_not() {
    let vm = start();
    let admin = administrator();
    let longest = name_of(253);
    assert_eq!(longest.len(), 253, "the harness's own check");
    assert_added(
        &put(&vm, &admin, &longest),
        &longest,
        &admin.id,
        "a name of 253 bytes",
    );
    let label = format!("{}.example.com", "b".repeat(63));
    assert_added(
        &put(&vm, &admin, &label),
        &label,
        &admin.id,
        "a label of 63 letters",
    );
    let one = "x.example.com";
    assert_added(
        &put(&vm, &admin, one),
        one,
        &admin.id,
        "a label of 1 letter",
    );
    let too_long = name_of(254);
    assert_eq!(too_long.len(), 254, "the harness's own check");
    assert_api_error(&put(&vm, &admin, &too_long), 400, "malformed");
    assert_eq!(hosts(&vm).len(), 3, "the 254 bytes was not added");
}

#[test]
fn delete_takes_the_forms_put_takes() {
    let vm = start();
    let admin = administrator();
    for entry in [
        "api.example.com",
        "*.example.com:8443",
        "[2001:db8::7]",
        "bücher.example",
    ] {
        add(&vm, entry);
    }
    for (given, kept) in [
        ("API.Example.COM:443", "api.example.com"),
        ("*.example.com:8443", "*.example.com:8443"),
        ("[2001:db8::7]:443", "[2001:db8::7]"),
        ("xn--bcher-kva.example", "xn--bcher-kva.example"),
    ] {
        assert_deleted(
            &delete(&vm, &admin, given),
            kept,
            true,
            &format!("DELETE {given:?}"),
        );
    }
    assert!(hosts(&vm).is_empty(), "every entry was removed");
}

/// Each: (what is given, why it is not an entry).
fn malformed() -> Vec<(String, &'static str)> {
    let long_label = format!("{}.example.com", "a".repeat(64));
    let long_name = name_of(254);
    let entries: Vec<(String, &'static str)> = vec![
        (String::new(), "empty"),
        ("-a.example.com".into(), "a label that begins with -"),
        ("a-.example.com".into(), "a label that ends with -"),
        (
            "a.-b.example.com".into(),
            "a second label that begins with -",
        ),
        (long_label, "a label of 64"),
        (long_name, "a name of 254 bytes"),
        ("*.com".into(), "a wildcard over a top-level name"),
        ("*".into(), "a wildcard alone"),
        ("a.*.example.com".into(), "a wildcard in the middle"),
        ("*example.com".into(), "a wildcard without the dot"),
        ("*.*.example.com".into(), "two wildcards"),
        ("a..example.com".into(), "an empty label"),
        (".example.com".into(), "a leading dot"),
        ("a_b.example.com".into(), "an underscore"),
        ("api.example.com:0".into(), "port 0"),
        ("api.example.com:65536".into(), "port 65536"),
        ("api.example.com:abc".into(), "a port that is not a number"),
        ("api.example.com:".into(), "an empty port"),
        ("api.example.com:-1".into(), "a negative port"),
        ("api.example.com:443:443".into(), "two ports"),
        ("[2001:db8::7]:0".into(), "an address with port 0"),
        ("203.0.113.7:65536".into(), "an address with port 65536"),
        ("https://api.example.com".into(), "a scheme"),
        ("http://api.example.com".into(), "another scheme"),
        ("api.example.com/v1".into(), "a path"),
        ("api.example.com/".into(), "a slash"),
        ("api.example.com?x=1".into(), "a query"),
        ("user@api.example.com".into(), "a user"),
        ("user:pw@api.example.com".into(), "a user and a password"),
        ("api example.com".into(), "a space"),
        (" api.example.com".into(), "a leading space"),
        ("api.example.com ".into(), "a trailing space"),
        ("*[2001:db8::7]".into(), "an address with a wildcard"),
        ("*.[2001:db8::7]".into(), "an address with a wildcard label"),
        (
            "2001:db8::7:8443".into(),
            "an IPv6 address without brackets, with a port",
        ),
        ("2001:db8::7".into(), "an IPv6 address without brackets"),
        ("[2001:db8::7".into(), "an unclosed bracket"),
        ("[not-an-address]".into(), "brackets that hold no address"),
        ("[203.0.113.7]".into(), "brackets around an IPv4 address"),
        ("a\tb.example.com".into(), "a tab"),
    ];
    entries
}

#[test]
fn an_entry_that_is_none_of_the_forms_is_malformed_and_adds_nothing() {
    let vm = start();
    let admin = administrator();
    for (entry, why) in malformed() {
        // An entry the harness sends as the contract says, percent-encoded.
        let answer = if entry.is_empty() {
            request(&vm, Some(&admin), "PUT", &format!("{HOSTS}/")).send()
        } else {
            put(&vm, &admin, &entry)
        };
        assert!(
            answer.status == 400,
            "expected `400` `malformed` for {entry:?} ({why}), got\n{}",
            answer.describe()
        );
        assert_api_error(&answer, 400, "malformed");
        let message = answer.json()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_lowercase();
        assert!(
            message.contains("entry")
                && (message.contains("host")
                    || message.contains("name")
                    || message.contains("address")),
            "expected the message to say what an entry is, for {entry:?} ({why}), got {message:?}"
        );
        // And DELETE refuses it alike.
        let answer = if entry.is_empty() {
            request(&vm, Some(&admin), "DELETE", &format!("{HOSTS}/")).send()
        } else {
            delete(&vm, &admin, &entry)
        };
        assert_api_error(&answer, 400, "malformed");
    }
    assert!(
        hosts(&vm).is_empty(),
        "nothing was added by an entry that is not one"
    );
    assert_eq!(change_records(&vm), 0, "and no change was logged");
}

#[test]
fn a_thousand_entries_the_1001st_is_too_many_and_removing_one_makes_room() {
    let vm = start();
    let admin = administrator();
    for n in 0..MOST {
        let entry = format!("h{n:04}.example.com");
        let answer = put(&vm, &admin, &entry);
        assert!(
            answer.status == 200,
            "expected `200` adding entry {} of {MOST}, got\n{}",
            n + 1,
            answer.describe()
        );
    }
    assert_eq!(entries(&vm).len(), MOST);
    let over = put(&vm, &admin, "one-too-many.example.com");
    assert_api_error(&over, 409, "too-many");
    assert_eq!(entries(&vm).len(), MOST, "the 1,001st was not added");
    // One that is there is not a 1,001st.
    assert_put_answer(
        &put(&vm, &admin, "h0000.example.com"),
        "h0000.example.com",
        false,
        &admin.id,
        "an entry already there, with the list full",
    );
    assert_deleted(
        &delete(&vm, &admin, "h0500.example.com"),
        "h0500.example.com",
        true,
        "removing one",
    );
    assert_added(
        &put(&vm, &admin, "one-too-many.example.com"),
        "one-too-many.example.com",
        &admin.id,
        "with room made",
    );
    assert_eq!(entries(&vm).len(), MOST);
    assert_api_error(&put(&vm, &admin, "another.example.com"), 409, "too-many");
}

#[test]
fn no_body_is_looked_at() {
    let vm = start();
    let admin = administrator();
    for (n, (content_type, body)) in [
        ("application/json", "not json".to_owned()),
        ("application/json", "[1, 2".to_owned()),
        ("text/plain", "x".repeat(64 * 1024)),
        (
            "application/json",
            json!({ "host": "elsewhere.example.com" }).to_string(),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let entry = format!("body{n}.example.com");
        let answer = request(&vm, Some(&admin), "PUT", &path_of(&entry))
            .header("content-type", content_type)
            .body(body)
            .timeout(Duration::from_secs(30))
            .send();
        assert_put_answer(&answer, &entry, true, &admin.id, &format!("body {n}"));
    }
    assert_eq!(
        hosts(&vm),
        [
            "body0.example.com",
            "body1.example.com",
            "body2.example.com",
            "body3.example.com"
        ],
        "the entry is the address's, whatever the body says"
    );
}

#[test]
fn the_list_is_kept_in_the_platform_database_and_survives_a_restart() {
    let vm = start();
    let admin = administrator();
    let first = assert_added(
        &put(&vm, &admin, "api.example.com"),
        "api.example.com",
        &admin.id,
        "added",
    );
    add(&vm, "*.example.org:8443");
    add(&vm, "gone.example.com");
    assert_deleted(
        &delete(&vm, &admin, "gone.example.com"),
        "gone.example.com",
        true,
        "removed",
    );
    let before = entries(&vm);
    assert_eq!(before.len(), 2);
    let vm = vm.restart();
    assert_eq!(
        entries(&vm),
        before,
        "after a restart against the same platform database: the same entries, who added them and when"
    );
    let again = put(&vm, &other_administrator(), "api.example.com");
    let again = assert_put_answer(
        &again,
        "api.example.com",
        false,
        &admin.id,
        "after the restart",
    );
    assert_eq!(again["added_at"], first["added_at"]);
}

#[test]
fn a_change_has_a_record_in_the_programs_log_and_what_changed_nothing_has_none() {
    let vm = start();
    let admin = administrator();
    let other = other_administrator();
    put(&vm, &admin, "api.example.com").expect_status(200);
    let added = wait_for_records(&vm, "outbound-host-added", "api.example.com", 1);
    assert_eq!(
        added[0]["by"].as_str(),
        Some(admin.id.as_str()),
        "the record names who added it, got {}",
        added[0]
    );
    // Said again, by another: nothing changed.
    put(&vm, &other, "API.example.com:443").expect_status(200);
    // A change that follows, so that the log has been given time.
    put(&vm, &other, "second.example.com").expect_status(200);
    wait_for_records(&vm, "outbound-host-added", "second.example.com", 1);
    assert_eq!(
        records_of(&vm, "outbound-host-added", "api.example.com").len(),
        1,
        "no record for a PUT that changed nothing"
    );
    delete(&vm, &other, "api.example.com").expect_status(200);
    let removed = wait_for_records(&vm, "outbound-host-removed", "api.example.com", 1);
    assert_eq!(
        removed[0]["by"].as_str(),
        Some(other.id.as_str()),
        "the record names who removed it, got {}",
        removed[0]
    );
    delete(&vm, &admin, "api.example.com").expect_status(200);
    delete(&vm, &admin, "never.example.com").expect_status(200);
    // A change that follows.
    delete(&vm, &admin, "second.example.com").expect_status(200);
    wait_for_records(&vm, "outbound-host-removed", "second.example.com", 1);
    assert_eq!(
        records_of(&vm, "outbound-host-removed", "api.example.com").len(),
        1,
        "no record for a DELETE that changed nothing"
    );
    assert!(
        records_of(&vm, "outbound-host-removed", "never.example.com").is_empty(),
        "no record for an entry that was never there"
    );
    assert_eq!(change_records(&vm), 4, "two additions and two removals");
    for record in blackbox::admin::logged(&vm, "outbound-host-added") {
        assert!(
            record["host"].is_string() && record["by"].is_string(),
            "the record has `host` and `by`, got {record}"
        );
    }
}

/// Names whose last label is a number: not names, and not taken.
const NUMERIC: [&str; 9] = [
    "127.1",
    "0x7f.0.0.1",
    "2130706433",
    "1.2.3",
    "010.0.0.1",
    "203.0.113.010",
    "example.123",
    "example.0x1f",
    "*.example.123",
];

#[test]
fn a_name_whose_last_label_is_a_number_is_malformed_for_put_and_delete_and_adds_nothing() {
    let vm = start();
    let admin = administrator();
    for entry in NUMERIC {
        for method in ["PUT", "DELETE"] {
            let answer = request(&vm, Some(&admin), method, &path_of(entry)).send();
            assert!(
                answer.status == 400,
                "expected `400` `malformed` for {method} {entry:?} (a number is not a name), got\n{}",
                answer.describe()
            );
            assert_api_error(&answer, 400, "malformed");
        }
    }
    assert!(hosts(&vm).is_empty(), "nothing was added");
    assert_eq!(change_records(&vm), 0);
}

#[test]
fn names_that_only_look_like_numbers_and_an_address_in_its_usual_form_are_taken() {
    let vm = start();
    let admin = administrator();
    for entry in [
        "203.0.113.7",
        "1.example.com",
        "123.example.com",
        "example.com1",
        "x0x1f.example.org",
    ] {
        assert_added(
            &put(&vm, &admin, entry),
            entry,
            &admin.id,
            &format!("PUT {entry:?}"),
        );
    }
}

#[test]
fn an_entry_is_one_path_segment() {
    let vm = start();
    let admin = administrator();
    let answer = request(&vm, Some(&admin), "PUT", "/v1/outbound-hosts/a/b").send();
    assert!(
        answer.status == 404 || answer.status == 400,
        "expected `404` or `400` for PUT /v1/outbound-hosts/a/b, got\n{}",
        answer.describe()
    );
    let answer = request(&vm, Some(&admin), "PUT", "/v1/outbound-hosts/a%2Fb").send();
    assert_api_error(&answer, 400, "malformed");
    assert!(hosts(&vm).is_empty(), "nothing was added");
}
