//! The Grants of a Group (CONTRACT.md, "Resources and Grants in full: The
//! Grants of a Group"): `GET /v1/admin/grants?group=<group>` answers every
//! Grant the Group holds, by Resource's name, with `kind`, `granted_by` and
//! `granted_at`; a Group that holds none is `[]`; `group` is percent-decoded
//! as the Operation log's parameters are (`+` is a space, `%2B` a plus) and
//! missing, twice or none is `400`; anyone but a Platform administrator is
//! `403` before everything else, whatever the method; `GET` only (`405`);
//! a removed Resource's Grants are gone from it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::api::assert_recent_rfc3339;
use blackbox::resources::{
    assert_utc_time, delete_resource, forget_who_and_when, grants_of_group, grants_of_group_query,
    member_names, revoke,
};
use blackbox::scenario::{ADMINISTRATORS, administrator};
use blackbox::storage::{MARKETING, Storage};
use blackbox::{Request, Response, User, Vm, assert_api_error};
use serde_json::{Value, json};

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

/// Storage `marketing` and databases `zeta`, `alpha`, `devops` (registered
/// in this order, not by name).
fn world() -> Storage {
    let s = Storage::start();
    s.register_postgres("zeta");
    s.register(MARKETING);
    s.register_postgres("alpha");
    s.register_postgres("devops");
    s
}

fn send(vm: &Vm, user: Option<&User>, path: &str, method: &str, body: Option<&str>) -> Response {
    let mut request: Request = vm.api_request(path).method(method);
    if let Some(user) = user {
        request = request.as_user(user);
    }
    if let Some(body) = body {
        request = request
            .header("content-type", "application/json")
            .body(body);
    }
    request.send()
}

#[test]
fn it_answers_each_grant_of_the_group_by_resource_with_kind_who_and_when() {
    let s = world();
    let admin = s.admin();
    for (resource, group, access) in [
        ("zeta", "sre", "read"),
        ("devops", "sre", "write"),
        (MARKETING, "sre", "read"),
        ("alpha", "sre", "structure"),
        ("alpha", "other", "write"),
        ("devops", "srex", "read"),
        ("zeta", "SRE", "read"),
    ] {
        s.grant(resource, group, access);
    }
    // One of them set by another Platform administrator.
    s.vm.api_as(&User::new("other-admin").in_groups(&[ADMINISTRATORS]))
        .grant("zeta", "sre", "write")
        .expect_status(200);
    let answer = grants_of_group(&admin, "sre");
    answer.expect_status(200);
    blackbox::api::assert_json_content_type(&answer);
    let body = answer.json();
    assert_eq!(member_names(&body), ["grants", "group"], "{body}");
    assert_eq!(body["group"], json!("sre"));
    let grants = body["grants"].as_array().unwrap();
    let seen: Vec<(&str, &str, &str)> = grants
        .iter()
        .map(|g| {
            (
                g["resource"].as_str().unwrap(),
                g["kind"].as_str().unwrap(),
                g["access"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("alpha", "postgres", "structure"),
            ("devops", "postgres", "write"),
            (MARKETING, "storage", "read"),
            ("zeta", "postgres", "write"),
        ],
        "every Grant of the Group and no other Group's, by Resource's name"
    );
    for g in grants {
        assert_eq!(
            member_names(g),
            ["access", "granted_at", "granted_by", "kind", "resource"],
            "{g}"
        );
        assert_utc_time(&g["granted_at"], "granted_at");
        assert_recent_rfc3339(&g["granted_at"], 60, "granted_at");
    }
    assert_eq!(grants[0]["granted_by"], json!(administrator().id));
    assert_eq!(grants[3]["granted_by"], json!("other-admin"));
    // The same Grants are in the Resource's own listing.
    let listed = admin.list_grants("zeta").json();
    let theirs = listed["grants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["group"] == "sre")
        .unwrap();
    assert_eq!(theirs["granted_at"], grants[3]["granted_at"]);
    assert_eq!(theirs["granted_by"], grants[3]["granted_by"]);
}

#[test]
fn a_group_that_holds_none_is_200_with_an_empty_list() {
    let s = world();
    s.grant("alpha", "sre", "read");
    let answer = grants_of_group(&s.admin(), "nobody-we-know");
    answer.expect_status(200);
    assert_eq!(
        answer.json(),
        json!({ "group": "nobody-we-know", "grants": [] })
    );
}

#[test]
fn a_grant_that_is_revoked_or_whose_resource_is_removed_is_gone_from_it() {
    let s = world();
    for resource in ["alpha", "devops", "zeta"] {
        s.grant(resource, "sre", "read");
    }
    let admin = s.admin();
    revoke(&admin, "devops", "sre").expect_status(200);
    let names = |admin: &blackbox::Api<'_>| -> Vec<String> {
        grants_of_group(admin, "sre").json()["grants"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["resource"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(names(&admin), ["alpha", "zeta"]);
    delete_resource(&admin, "alpha").expect_status(200);
    assert_eq!(
        names(&admin),
        ["zeta"],
        "a removed Resource's Grants are gone from it"
    );
    // Registered again under the name, it holds none.
    s.register_postgres("alpha");
    assert_eq!(names(&admin), ["zeta"]);
}

#[test]
fn plus_is_a_space_and_percent_2b_a_plus_and_other_parameters_are_ignored() {
    let s = world();
    s.grant("alpha", "dev ops", "read");
    s.grant("devops", "dev+ops", "write");
    s.grant("zeta", "dev/ops%", "read");
    let admin = s.admin();
    let by_space = grants_of_group_query(&admin, "?group=dev+ops");
    by_space.expect_status(200);
    assert_eq!(by_space.json()["group"], json!("dev ops"));
    assert_eq!(by_space.json()["grants"][0]["resource"], json!("alpha"));
    assert_eq!(by_space.json()["grants"].as_array().unwrap().len(), 1);
    let by_percent_space = grants_of_group_query(&admin, "?group=dev%20ops");
    assert_eq!(by_percent_space.json(), by_space.json());
    let by_plus = grants_of_group_query(&admin, "?group=dev%2Bops");
    assert_eq!(by_plus.json()["group"], json!("dev+ops"));
    assert_eq!(by_plus.json()["grants"][0]["resource"], json!("devops"));
    let by_slash = grants_of_group_query(&admin, "?group=dev%2Fops%25");
    assert_eq!(by_slash.json()["grants"][0]["resource"], json!("zeta"));
    let with_others = grants_of_group_query(&admin, "?other=1&group=dev%2Bops&limit=0");
    with_others.expect_status(200);
    assert_eq!(with_others.json(), by_plus.json());
}

#[test]
fn a_group_that_is_missing_twice_or_none_is_400_malformed() {
    let s = world();
    s.grant("alpha", "sre", "read");
    let admin = s.admin();
    let long = format!("?group={}", "g".repeat(257));
    let wide = format!("?group={}", "%C3%A9".repeat(129));
    for (what, query) in [
        ("missing", ""),
        ("missing, with another parameter", "?other=1"),
        ("given twice", "?group=sre&group=sre"),
        ("given twice, different", "?group=sre&group=alpha"),
        ("empty", "?group="),
        ("over 256 bytes", long.as_str()),
        ("over 256 bytes of two-byte letters", wide.as_str()),
        ("a control character", "?group=a%01b"),
        ("a NUL", "?group=a%00b"),
        ("not UTF-8", "?group=%FF"),
    ] {
        let answer = grants_of_group_query(&admin, query);
        assert_eq!(
            answer.status,
            400,
            "expected `400` for a `group` that is {what}, got\n{}",
            answer.describe()
        );
        assert_api_error(&answer, 400, "malformed");
    }
    let at = grants_of_group_query(&admin, &format!("?group={}", "g".repeat(256)));
    at.expect_status(200);
}

#[test]
fn it_is_for_platform_administrators_and_takes_get_only() {
    let s = world();
    s.grant("alpha", "sre", "read");
    let queries = [
        "",
        "?group=sre",
        "?group=",
        "?group=a&group=b",
        "?group=%FF",
    ];
    let mut bodies: Vec<Vec<u8>> = Vec::new();
    for user in [
        alice(),
        User::new("mallory").in_groups(&["Platform-Admins"]),
        User::new(&administrator().id),
    ] {
        for method in ["GET", "POST", "PUT", "PATCH", "DELETE"] {
            for query in queries {
                let answer = send(
                    &s.vm,
                    Some(&user),
                    &format!("/v1/admin/grants{query}"),
                    method,
                    Some("{}"),
                );
                assert_api_error(&answer, 403, "not-an-administrator");
                bodies.push(answer.body().to_vec());
            }
        }
    }
    assert!(
        bodies.windows(2).all(|w| w[0] == w[1]),
        "the same refusal for every request"
    );
    for query in ["", "?group=sre"] {
        let answer = send(
            &s.vm,
            None,
            &format!("/v1/admin/grants{query}"),
            "GET",
            None,
        );
        assert_eq!(answer.status, 401, "{}", answer.describe());
    }
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        for query in ["?group=sre", ""] {
            let answer = send(
                &s.vm,
                Some(&administrator()),
                &format!("/v1/admin/grants{query}"),
                method,
                Some("{}"),
            );
            assert_api_error(&answer, 405, "method-not-allowed");
        }
    }
    // Nothing was changed by the requests that were refused.
    let still = grants_of_group(&s.admin(), "sre").json();
    assert_eq!(still["grants"].as_array().unwrap().len(), 1, "{still}");
}

#[test]
fn a_grant_from_before_this_stage_has_no_who_and_no_when_here_too() {
    let s = world();
    s.grant("alpha", "sre", "read");
    forget_who_and_when(&s.vm);
    let Storage { vm, place } = s;
    let s = Storage {
        vm: blackbox::storage::terminate(vm).start(),
        place,
    };
    let answer = grants_of_group(&s.admin(), "sre");
    answer.expect_status(200);
    let grant: Value = answer.json()["grants"][0].clone();
    assert_eq!(grant["resource"], json!("alpha"));
    assert_eq!(grant["granted_by"], Value::Null, "{grant}");
    assert_eq!(grant["granted_at"], Value::Null, "{grant}");
}

#[test]
fn past_64_kib_is_413_for_everyone_and_exactly_64_kib_is_not() {
    let s = world();
    let over = " ".repeat(64 * 1024 + 1);
    let at = " ".repeat(64 * 1024);
    let path = "/v1/admin/grants?group=sre";
    for user in [alice(), administrator()] {
        let answer = send(&s.vm, Some(&user), path, "GET", Some(&over));
        assert_api_error(&answer, 413, "too-large");
    }
    assert_api_error(
        &send(&s.vm, Some(&alice()), path, "GET", Some(&at)),
        403,
        "not-an-administrator",
    );
    let answer = send(&s.vm, Some(&administrator()), path, "GET", Some(&at));
    assert_ne!(answer.status, 413, "{}", answer.describe());
}
