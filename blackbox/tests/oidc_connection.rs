//! Connecting OIDC (CONTRACT.md, "The tool signs in through the browser,
//! and OIDC is connected from the tool: Connecting OIDC" and "The
//! installation's key"): `vibe admin connect-oidc` with the setup credential
//! or the tool's own, `PUT /v1/oidc` and what it refuses, the provider
//! asked before a connection is taken, the connection taking effect at once,
//! a connection made over an earlier one, `GET /v1/oidc`, `GET
//! /v1/installation`, and no secret anywhere it must not be.
//!
//! Acceptance criterion: "With the setup credential, a Platform
//! administrator connects OIDC and names the administrators' Group from the
//! tool; afterwards the setup credential no longer works."
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use blackbox::api::assert_recent_rfc3339;
use blackbox::connection::{
    Connection, connect_args, connect_with_setup, hide, installation_of, put_oidc,
    setup_credential, shown,
};
use blackbox::oidc::{ConfigFault, Person, Provider};
use blackbox::scenario::ADMINISTRATORS;
use blackbox::sessions::{ADA, GRACE, Installation, ada, grace, oidc_options};
use blackbox::tool::{Machine, bearer, platform_tables_holding, windows_in};
use blackbox::tool_sign_ins::Challenge;
use blackbox::{DOMAIN, Response, User, Vm, VmOptions, assert_api_error};
use serde_json::{Value, json};

/// A Platform administrator at the provider: in [`ADMINISTRATORS`].
const IT_ADMIN: &str = "00u-it-admin";

fn it_admin() -> Person {
    Person::new(IT_ADMIN)
        .named("Ida Admin")
        .email("ida@example.test")
        .in_groups(&[ADMINISTRATORS])
}

/// A program with an installation's key, `honest` installed, no
/// connection, beside a provider with Ada, Grace and the administrator.
fn unconnected() -> Installation {
    Installation::unconnected(
        Provider::start(&[ada(), grace(), it_admin()]),
        &oidc_options().application("honest", "honest.js"),
    )
}

/// As [`unconnected`], connected.
fn connected() -> Installation {
    Installation::start_with(
        &[("honest", "honest.js")],
        &[ada(), grace(), it_admin()],
        &[],
    )
}

fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

fn strings(value: &Value) -> BTreeSet<String> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("expected an array, got {value}"))
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_owned())
        .collect()
}

fn set_of(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

const CONNECTION_KEYS: [&str; 7] = [
    "administrators_group",
    "client_id",
    "connected_at",
    "connected_by",
    "groups_claim",
    "issuer",
    "scopes",
];

/// Fails unless `value` is the connection to `connection`'s settings with
/// the defaults for what was not given, made by `by`.
fn assert_connection(value: &Value, connection: &Connection, by: &str, context: &str) {
    assert_eq!(keys(value), CONNECTION_KEYS, "{context}: {value}");
    assert_eq!(value["issuer"], connection.issuer.as_str(), "{context}");
    assert_eq!(
        value["client_id"],
        connection.client_id.as_str(),
        "{context}"
    );
    assert_eq!(
        value["administrators_group"],
        connection.administrators_group.as_str(),
        "{context}"
    );
    assert_eq!(
        value["groups_claim"],
        connection.groups_claim.as_deref().unwrap_or("groups"),
        "{context}"
    );
    assert_eq!(value["connected_by"], by, "{context}");
    assert_recent_rfc3339(&value["connected_at"], 120, context);
}

#[test]
fn connecting_from_the_tool_with_the_setup_credential_takes_effect_at_once() {
    let at = unconnected();
    let before = installation_of(&at.vm);
    assert_eq!(before.status, 200, "{}", before.describe());
    assert_eq!(
        before.json(),
        json!({ "domain": DOMAIN, "sign_in": "none", "sign_in_address": null })
    );
    let setup = setup_credential(at.vm.platform());
    let machine = Machine::new();
    let connection = Connection::to(&at.provider);
    let run = connect_with_setup(&machine, &at.vm, &setup, &connection, true);
    let run = hide(&run, &[setup.expose(), connection.secret().expose()]);
    let printed = run.ok_json();
    assert_connection(
        &printed,
        &connection,
        "setup-credential",
        "the tool's answer",
    );
    assert_eq!(
        strings(&printed["scopes"]),
        set_of(&["openid", "profile", "email", "offline_access"]),
        "the default scopes"
    );
    let after = installation_of(&at.vm).json();
    assert_eq!(
        after,
        json!({ "domain": DOMAIN, "sign_in": "oidc", "sign_in_address": at.auth_origin() }),
        "the running program uses the connection at once"
    );
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    assert_eq!(at.user_seen(&mut browser, "honest")["id"], ADA);
}

#[test]
fn afterwards_the_setup_credential_no_longer_works() {
    let at = unconnected();
    let setup = setup_credential(at.vm.platform());
    let machine = Machine::new();
    let connection = Connection::to(&at.provider);
    let secrets = [setup.expose(), connection.secret().expose()];
    hide(
        &connect_with_setup(&machine, &at.vm, &setup, &connection, true),
        &secrets,
    )
    .ok_json();
    let again = hide(
        &connect_with_setup(&machine, &at.vm, &setup, &connection, true),
        &secrets,
    );
    let message = again.json_error(3, "setup-credential-refused");
    assert!(
        message.contains("vibe setup"),
        "expected the message to say to run `vibe setup` again, got {message:?}"
    );
    let in_words = hide(
        &connect_with_setup(&machine, &at.vm, &setup, &connection, false),
        &secrets,
    );
    assert!(in_words.text_error(3).contains("vibe setup"));
    let through_the_api = put_oidc(&at.vm, Some(setup.expose()), &connection.body());
    assert_eq!(
        through_the_api.status,
        401,
        "{}",
        shown(&through_the_api, &secrets)
    );
}

#[test]
fn the_log_says_oidc_connected_and_holds_no_secret() {
    let at = unconnected();
    let setup = setup_credential(at.vm.platform());
    let connection = Connection::to(&at.provider);
    let machine = Machine::new();
    let printed = hide(
        &connect_with_setup(&machine, &at.vm, &setup, &connection, true),
        &[setup.expose(), connection.secret().expose()],
    )
    .ok_json();
    at.assert_logged(
        "oidc-connected",
        &[
            ("issuer", json!(connection.issuer)),
            ("client_id", json!(connection.client_id)),
            ("administrators_group", json!(ADMINISTRATORS)),
            ("by", printed["connected_by"].clone()),
        ],
    );
    at.assert_none_in_log(
        &[
            setup.expose().to_owned(),
            connection.secret().expose().to_owned(),
            at.key().unwrap().expose().to_owned(),
        ],
        "after connecting",
    );
}

#[test]
fn neither_secret_is_printed_nor_kept_on_the_machine() {
    let at = unconnected();
    let connection = Connection::to(&at.provider);
    let machine = Machine::new();
    for json in [false, true] {
        let setup = setup_credential(at.vm.platform());
        let raw = connect_with_setup(&machine, &at.vm, &setup, &connection, json);
        let secrets = [setup.expose(), connection.secret().expose()];
        let run = hide(&raw, &secrets);
        assert_eq!(run.code, Some(0), "{}", run.describe());
        for (what, secret) in [
            ("the setup credential", setup.expose()),
            ("the client secret", connection.secret().expose()),
        ] {
            let found = windows_in(&raw.texts(), secret);
            assert!(
                found.is_empty(),
                "--json {json}: expected {what} printed nowhere, found: {found:?}"
            );
            let files = machine.files_holding(secret);
            assert!(
                files.is_empty(),
                "--json {json}: expected {what} kept nowhere on the Builder's machine, found in {files:?}"
            );
        }
    }
}

#[test]
fn connect_oidc_takes_the_client_secret_from_its_environment_only() {
    let at = unconnected();
    let setup = setup_credential(at.vm.platform());
    let connection = Connection::to(&at.provider);
    let machine = Machine::new();
    let args = connect_args(at.vm.address(), &connection, true);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let without = machine.run_with_env(&refs, &[("VIBE_SETUP_CREDENTIAL", setup.expose())]);
    hide(&without, &[setup.expose()]).expect_code(2);
    let mut on_the_line = refs.clone();
    on_the_line.extend(["--client-secret", "on-the-command-line-Lq0"]);
    let given = machine.run_with_env(
        &on_the_line,
        &[
            ("VIBE_SETUP_CREDENTIAL", setup.expose()),
            ("VIBE_OIDC_CLIENT_SECRET", connection.secret().expose()),
        ],
    );
    hide(&given, &[setup.expose(), connection.secret().expose()]).expect_code(2);
    assert_eq!(
        installation_of(&at.vm).json()["sign_in"],
        "none",
        "nothing was connected"
    );
}

/// The arguments without this option and its value.
fn without_option(args: &[String], option: &str) -> Vec<String> {
    let mut kept = Vec::new();
    let mut skip = false;
    for arg in args {
        if skip {
            skip = false;
        } else if arg == option {
            skip = true;
        } else {
            kept.push(arg.clone());
        }
    }
    kept
}

#[test]
fn with_the_setup_credential_domain_is_required_and_connect_is_loopback_and_alone() {
    let at = unconnected();
    let setup = setup_credential(at.vm.platform());
    let connection = Connection::to(&at.provider);
    let machine = Machine::new();
    let full = connect_args(at.vm.address(), &connection, true);
    let address = at.vm.address().to_string();
    let with = |more: &[&str]| -> Vec<String> {
        let mut args = full.clone();
        args.extend(more.iter().map(|a| (*a).to_owned()));
        args
    };
    let port = at.vm.address().port().to_string();
    let cases: Vec<(&str, Vec<String>)> = vec![
        ("no --domain", without_option(&full, "--domain")),
        (
            "--connect to an address that is not loopback",
            full.iter()
                .map(|a| {
                    if *a == address {
                        "10.0.0.7:80".to_owned()
                    } else {
                        a.clone()
                    }
                })
                .collect(),
        ),
        ("--connect with --port", with(&["--port", &port])),
        (
            "--connect with --resolve",
            with(&["--resolve", "127.0.0.1"]),
        ),
    ];
    let env = [
        ("VIBE_SETUP_CREDENTIAL", setup.expose()),
        ("VIBE_OIDC_CLIENT_SECRET", connection.secret().expose()),
    ];
    for (case, args) in cases {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let run = machine.run_with_env(&refs, &env);
        let run = hide(&run, &[setup.expose(), connection.secret().expose()]);
        assert_eq!(run.code, Some(2), "{case}\n{}", run.describe());
    }
    // Without --connect it is no wrong command line: the installation is
    // reached over HTTPS (here at a port nothing listens on: unreachable).
    let nowhere = blackbox::tool::closed_address().port().to_string();
    let mut https = without_option(&full, "--connect");
    https.extend(["--resolve".to_owned(), "127.0.0.1".to_owned()]);
    https.extend(["--port".to_owned(), nowhere]);
    let refs: Vec<&str> = https.iter().map(String::as_str).collect();
    let run = hide(
        &machine.run_with_env(&refs, &env),
        &[setup.expose(), connection.secret().expose()],
    );
    run.json_error(1, "unreachable");
    // The setup credential is still good: nothing reached the installation.
    assert_eq!(
        installation_of(&at.vm).json()["sign_in"],
        "none",
        "expected nothing connected"
    );
}

#[test]
fn with_the_tool_s_own_credential_domain_port_resolve_and_connect_are_each_exit_code_2() {
    let at = connected();
    let machine = Machine::new();
    at.sign_in_tool(&machine, IT_ADMIN);
    let connection = Connection::to(&at.provider);
    let port = at.vm.address().port().to_string();
    let address = at.vm.address().to_string();
    for (option, value) in [
        ("--domain", DOMAIN),
        ("--port", port.as_str()),
        ("--resolve", "127.0.0.1"),
        ("--connect", address.as_str()),
    ] {
        let mut args = vec!["admin".to_owned(), "connect-oidc".to_owned()];
        args.extend([option.to_owned(), value.to_owned()]);
        args.extend(connection.tool_args());
        args.push("--json".to_owned());
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let run = machine.run_with_env(
            &refs,
            &[("VIBE_OIDC_CLIENT_SECRET", connection.secret().expose())],
        );
        let run = hide(&run, &[connection.secret().expose()]);
        assert_eq!(run.code, Some(2), "{option}\n{}", run.describe());
    }
}

/// `vibe admin connect-oidc` with the tool's own credential: no `--domain`,
/// no `--connect`, no setup credential.
fn connect_as_signed_in(machine: &Machine, connection: &Connection) -> blackbox::tool::Run {
    let mut args = vec!["admin".to_owned(), "connect-oidc".to_owned()];
    args.extend(connection.tool_args());
    args.push("--json".to_owned());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let run = machine.run_with_env(
        &refs,
        &[("VIBE_OIDC_CLIENT_SECRET", connection.secret().expose())],
    );
    hide(&run, &[connection.secret().expose()])
}

#[test]
fn a_platform_administrator_connects_again_with_the_tool_s_own_credential() {
    let at = connected();
    let machine = Machine::new();
    at.sign_in_tool(&machine, IT_ADMIN);
    let connection = Connection::to(&at.provider).groups_claim("groups");
    let printed = connect_as_signed_in(&machine, &connection).ok_json();
    assert_connection(
        &printed,
        &connection,
        IT_ADMIN,
        "connected by an administrator",
    );
    at.assert_logged("oidc-connected", &[("by", json!(IT_ADMIN))]);
}

#[test]
fn connecting_with_the_tool_s_own_credential_is_refused_to_anyone_else() {
    let at = connected();
    let grace_machine = Machine::new();
    at.sign_in_tool(&grace_machine, GRACE);
    let connection = Connection::to(&at.provider).administrators_group("devops");
    connect_as_signed_in(&grace_machine, &connection).json_error(3, "not-an-administrator");
    let nobody = Machine::new();
    connect_as_signed_in(&nobody, &connection).json_error(4, "not-signed-in");
    let me = bearer(&at.vm, "/v1/me", &at.tool_credential_for(GRACE)).send();
    assert_eq!(
        me.json()["platform_administrator"],
        false,
        "the refused connection changed nothing"
    );
}

#[test]
fn put_oidc_answers_the_connection_with_the_groups_claim_and_scopes_given() {
    let at = unconnected();
    let connection = Connection::to(&at.provider)
        .groups_claim("roles")
        .scopes("email profile");
    let answer = at.connect_through_the_api(&connection);
    let secret = connection.secret().expose();
    assert_eq!(answer.status, 200, "{}", shown(&answer, &[secret]));
    assert!(
        !answer.text().contains(secret),
        "the answer never holds the client secret"
    );
    let body = answer.json();
    assert_connection(&body, &connection, "setup-credential", "PUT /v1/oidc");
    assert_eq!(
        strings(&body["scopes"]),
        set_of(&["openid", "email", "profile"]),
        "`openid` is always among the scopes"
    );
}

#[test]
fn an_issuer_that_is_not_https_nor_loopback_http_is_invalid_issuer_and_changes_nothing() {
    let at = unconnected();
    let setup = setup_credential(at.vm.platform());
    let good = Connection::to(&at.provider);
    let secret = good.secret().expose().to_owned();
    for issuer in [
        "http://idp.example.com",
        "http://10.0.0.7:8080",
        "http://192.168.1.20",
        "http://127.0.0.1.nip.io",
        "http://localhost.example.com",
        "ftp://127.0.0.1/",
        "not a url",
        "https://",
        "",
        "https://someone@idp.example.com",
        "https://idp.example.com/?tenant=1",
        "https://idp.example.com/#x",
    ] {
        let answer = put_oidc(
            &at.vm,
            Some(setup.expose()),
            &good.clone().issuer(issuer).body(),
        );
        assert_api_error(&answer, 400, "invalid-issuer");
        assert_eq!(
            installation_of(&at.vm).json()["sign_in"],
            "none",
            "{issuer:?}"
        );
    }
    let taken = put_oidc(&at.vm, Some(setup.expose()), &good.body());
    assert_eq!(
        taken.status,
        200,
        "the refusals did not use the setup credential up\n{}",
        shown(&taken, &[&secret])
    );
}

#[test]
fn a_connection_that_is_not_well_formed_is_malformed_and_changes_nothing() {
    let at = unconnected();
    let setup = setup_credential(at.vm.platform());
    let good = Connection::to(&at.provider);
    let secret = good.secret().expose().to_owned();
    let body = good.body();
    let with = |field: &str, value: Value| {
        let mut changed = body.clone();
        changed[field] = value;
        changed
    };
    let without = |field: &str| {
        let mut changed = body.clone();
        changed.as_object_mut().unwrap().remove(field);
        changed
    };
    let cases: Vec<(&str, Value)> = vec![
        ("an empty client_id", with("client_id", json!(""))),
        (
            "a client_id of 257 bytes",
            with("client_id", json!("c".repeat(257))),
        ),
        (
            "a client_id with a control character",
            with("client_id", json!("vm\u{7}client")),
        ),
        ("an empty client_secret", with("client_secret", json!(""))),
        (
            "a client_secret of 1,025 bytes",
            with("client_secret", json!("s".repeat(1025))),
        ),
        (
            "a client_secret with a control character",
            with("client_secret", json!("se\ncret")),
        ),
        (
            "an empty administrators_group",
            with("administrators_group", json!("")),
        ),
        (
            "an administrators_group of 257 bytes",
            with("administrators_group", json!("g".repeat(257))),
        ),
        (
            "an administrators_group with a control character",
            with("administrators_group", json!("ad\u{1b}mins")),
        ),
        ("an empty groups_claim", with("groups_claim", json!(""))),
        (
            "a client_id that is not a string",
            with("client_id", json!(7)),
        ),
        (
            "scopes that are not one string",
            with("scopes", json!(["openid"])),
        ),
        ("no client_id", without("client_id")),
        ("no client_secret", without("client_secret")),
        ("no administrators_group", without("administrators_group")),
        ("no issuer", without("issuer")),
        ("an issuer that is a number", with("issuer", json!(7))),
        ("an issuer that is null", with("issuer", Value::Null)),
    ];
    for (case, body) in cases {
        let answer = put_oidc(&at.vm, Some(setup.expose()), &body);
        assert_eq!(answer.status, 400, "{case}\n{}", shown(&answer, &[&secret]));
        assert_eq!(
            answer.json()["error"]["reason"],
            "malformed",
            "{case}\n{}",
            shown(&answer, &[&secret])
        );
        assert_eq!(installation_of(&at.vm).json()["sign_in"], "none", "{case}");
    }
    for (case, raw) in [
        ("a body that is not JSON", "{\"issuer\": "),
        ("a JSON array", "[]"),
    ] {
        let answer = at
            .vm
            .api_request("/v1/oidc")
            .method("PUT")
            .bearer(setup.expose())
            .header("content-type", "application/json")
            .body(raw)
            .send();
        assert_eq!(answer.status, 400, "{case}\n{}", shown(&answer, &[&secret]));
        assert_api_error(&answer, 400, "malformed");
        assert_eq!(installation_of(&at.vm).json()["sign_in"], "none", "{case}");
    }
    let taken = put_oidc(&at.vm, Some(setup.expose()), &body);
    assert_eq!(taken.status, 200, "{}", shown(&taken, &[&secret]));
}

#[test]
fn the_longest_values_are_taken() {
    let at = unconnected();
    let connection = Connection::to(&at.provider)
        .client_id(&"c".repeat(256))
        .client_secret(&"s".repeat(1024))
        .administrators_group(&"g".repeat(256));
    let answer = at.connect_through_the_api(&connection);
    assert_eq!(
        answer.status,
        200,
        "a client_id of 256 bytes, a client secret of 1,024 and a Group of 256\n{}",
        shown(&answer, &[connection.secret().expose()])
    );
}

/// Fails unless connecting to `issuer` is `422 identity-provider-unusable`
/// within 14 s and changes nothing; returns the message.
fn assert_unusable(at: &Installation, setup: &str, connection: &Connection, case: &str) -> String {
    let began = Instant::now();
    let answer = put_oidc(&at.vm, Some(setup), &connection.body());
    let took = began.elapsed();
    assert_eq!(
        answer.status,
        422,
        "{case}\n{}",
        shown(&answer, &[connection.secret().expose()])
    );
    assert_api_error(&answer, 422, "identity-provider-unusable");
    assert!(
        took < Duration::from_secs(14),
        "{case}: the provider is asked for at most 10 seconds, the answer took {took:?}"
    );
    assert_eq!(
        installation_of(&at.vm).json()["sign_in"],
        "none",
        "{case}: nothing is changed"
    );
    answer.json()["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn the_provider_is_asked_before_the_connection_is_taken() {
    let at = unconnected();
    let setup = setup_credential(at.vm.platform());
    let good = Connection::to(&at.provider);
    let mut messages: Vec<(String, String)> = Vec::new();
    messages.push((
        "not reached".to_owned(),
        assert_unusable(
            &at,
            setup.expose(),
            &good.clone().issuer(&Provider::unreachable_issuer()),
            "an issuer nothing listens at",
        ),
    ));
    for (case, fault) in [
        ("not JSON", ConfigFault::NotJson),
        ("another issuer", ConfigFault::AnotherIssuer),
        ("an endpoint missing", ConfigFault::LacksTokenEndpoint),
    ] {
        at.provider.config_fault(Some(fault));
        messages.push((
            case.to_owned(),
            assert_unusable(&at, setup.expose(), &good, case),
        ));
    }
    // Read up to 1 MiB: a larger configuration cannot be used either (its
    // message is not asked to differ from the five the contract names).
    at.provider.config_fault(Some(ConfigFault::Oversize));
    assert_unusable(&at, setup.expose(), &good, "larger than 1 MiB");
    at.provider.config_fault(None);
    at.provider.config_delay(Duration::from_secs(20));
    messages.push((
        "late".to_owned(),
        assert_unusable(
            &at,
            setup.expose(),
            &good,
            "a configuration that comes after 10 s",
        ),
    ));
    at.provider.config_delay(Duration::ZERO);
    let distinct: BTreeSet<&String> = messages.iter().map(|(_, m)| m).collect();
    assert_eq!(
        distinct.len(),
        messages.len(),
        "expected the message to say which of these it was, each its own: {messages:?}"
    );
    let taken = put_oidc(&at.vm, Some(setup.expose()), &good.body());
    assert_eq!(
        taken.status,
        200,
        "nothing was changed, the setup credential still connects once the provider answers\n{}",
        shown(&taken, &[good.secret().expose()])
    );
    assert!(
        !at.provider
            .requests_to("/.well-known/openid-configuration")
            .is_empty(),
        "the provider's configuration was asked for"
    );
}

#[test]
fn without_the_installation_s_key_oidc_cannot_be_connected() {
    let provider = Provider::start(&[ada()]);
    let at = Installation::unconnected(
        provider,
        &VmOptions::new()
            .args_extra(&["--public-scheme", "http"])
            .without_secret_key(),
    );
    let setup = setup_credential(at.vm.platform());
    let connection = Connection::to(&at.provider);
    let answer = put_oidc(&at.vm, Some(setup.expose()), &connection.body());
    assert_api_error(&answer, 409, "no-secret-key");
    // Who asks comes first: a made-up setup credential is 401, also with a
    // body that is wrong.
    for (case, body) in [
        ("a good body", connection.body()),
        ("a wrong body", json!({ "issuer": 7 })),
    ] {
        let answer = put_oidc(&at.vm, Some(&made_up_setup_credential()), &body);
        assert_eq!(answer.status, 401, "{case}\n{}", shown(&answer, &[]));
    }
    // The 409 comes before the body: a wrong body is still no-secret-key.
    let wrong = put_oidc(&at.vm, Some(setup.expose()), &json!({ "issuer": 7 }));
    assert_api_error(&wrong, 409, "no-secret-key");
    let machine = Machine::new();
    let setup = setup_credential(at.vm.platform());
    let run = hide(
        &connect_with_setup(&machine, &at.vm, &setup, &connection, true),
        &[setup.expose(), connection.secret().expose()],
    );
    run.json_error(3, "no-secret-key");
    assert_eq!(installation_of(&at.vm).json()["sign_in"], "none");
}

#[test]
fn with_the_stand_in_oidc_cannot_be_connected() {
    let provider = Provider::start(&[ada()]);
    let at = Installation::unconnected(
        provider,
        &oidc_options()
            .stand_in_sign_in()
            .administrators_group(ADMINISTRATORS),
    );
    let connection = Connection::to(&at.provider);
    let answer = at.connect_through_the_api(&connection);
    assert_api_error(&answer, 409, "stand-in-sign-in");
    let as_administrator = at
        .vm
        .api_request("/v1/oidc")
        .method("PUT")
        .as_user(&User::new("it-admin").in_groups(&[ADMINISTRATORS]))
        .json(&connection.body())
        .send();
    assert_api_error(&as_administrator, 409, "stand-in-sign-in");
    // Who asks comes first: a made-up setup credential is 401, someone who
    // is no Platform administrator 403, whatever the body.
    for body in [connection.body(), json!({ "issuer": 7 })] {
        let answer = put_oidc(&at.vm, Some(&made_up_setup_credential()), &body);
        assert_eq!(answer.status, 401, "{body}\n{}", shown(&answer, &[]));
        let not_administrator = at
            .vm
            .api_request("/v1/oidc")
            .method("PUT")
            .as_user(&User::new("someone").in_groups(&["finance"]))
            .json(&body)
            .send();
        assert_api_error(&not_administrator, 403, "not-an-administrator");
    }
    // The 409 comes before the body: a wrong body is still stand-in-sign-in.
    let wrong = at
        .vm
        .api_request("/v1/oidc")
        .method("PUT")
        .as_user(&User::new("it-admin").in_groups(&[ADMINISTRATORS]))
        .json(&json!({ "issuer": 7 }))
        .send();
    assert_api_error(&wrong, 409, "stand-in-sign-in");
    assert_eq!(installation_of(&at.vm).json()["sign_in"], "stand-in");
}

#[test]
fn with_the_stand_in_and_no_key_the_stand_in_is_said_first() {
    let provider = Provider::start(&[ada()]);
    let at = Installation::unconnected(
        provider,
        &VmOptions::new()
            .args_extra(&["--public-scheme", "http"])
            .stand_in_sign_in()
            .administrators_group(ADMINISTRATORS)
            .without_secret_key(),
    );
    let connection = Connection::to(&at.provider);
    let as_administrator = at
        .vm
        .api_request("/v1/oidc")
        .method("PUT")
        .as_user(&User::new("it-admin").in_groups(&[ADMINISTRATORS]))
        .json(&connection.body())
        .send();
    assert_api_error(&as_administrator, 409, "stand-in-sign-in");
    let answer = put_oidc(
        &at.vm,
        Some(&made_up_setup_credential()),
        &connection.body(),
    );
    assert_eq!(answer.status, 401, "{}", shown(&answer, &[]));
}

/// A setup credential of the right form that was never made.
fn made_up_setup_credential() -> String {
    format!("vmsc_{}", Challenge::fresh().challenge)
}

#[test]
fn get_oidc_answers_the_connection_to_a_platform_administrator_only() {
    let at = connected();
    let connection = Connection::to(&at.provider);
    let admin = at.tool_credential_for(IT_ADMIN);
    let answer = bearer(&at.vm, "/v1/oidc", &admin).send();
    let secret = connection.secret().expose();
    assert_eq!(answer.status, 200, "{}", shown(&answer, &[secret]));
    assert!(!answer.text().contains(secret), "never the client secret");
    assert_connection(
        &answer.json(),
        &connection,
        "setup-credential",
        "GET /v1/oidc",
    );
    let grace = at.tool_credential_for(GRACE);
    assert_api_error(
        &bearer(&at.vm, "/v1/oidc", &grace).send(),
        403,
        "not-an-administrator",
    );
    assert_eq!(at.vm.api_request("/v1/oidc").send().status, 401);
}

#[test]
fn get_oidc_without_a_connection_is_404() {
    let vm = Vm::start(
        &VmOptions::new()
            .stand_in_sign_in()
            .administrators_group(ADMINISTRATORS),
    );
    let answer = vm
        .api_request("/v1/oidc")
        .as_user(&User::new("it-admin").in_groups(&[ADMINISTRATORS]))
        .send();
    assert_api_error(&answer, 404, "not-found");
}

fn installation_answer(vm: &Vm, user: Option<&User>) -> Response {
    let request = vm.api_request("/v1/installation");
    match user {
        Some(user) => request.as_user(user),
        None => request,
    }
    .send()
}

#[test]
fn get_installation_says_to_anyone_how_to_sign_in() {
    let none = Vm::start(&oidc_options());
    assert_eq!(
        installation_answer(&none, None).json(),
        json!({ "domain": DOMAIN, "sign_in": "none", "sign_in_address": null })
    );
    let stand_in = Vm::start(&VmOptions::new().stand_in_sign_in());
    for user in [None, Some(&User::new("someone"))] {
        let answer = installation_answer(&stand_in, user);
        assert_eq!(answer.status, 200, "{}", answer.describe());
        assert_eq!(
            answer.json(),
            json!({ "domain": DOMAIN, "sign_in": "stand-in", "sign_in_address": null }),
            "signed in or not"
        );
    }
    let at = connected();
    let want = json!({ "domain": DOMAIN, "sign_in": "oidc", "sign_in_address": at.auth_origin() });
    assert_eq!(installation_answer(&at.vm, None).json(), want);
    let credential = at.tool_credential_for(ADA);
    assert_eq!(
        bearer(&at.vm, "/v1/installation", &credential)
            .send()
            .json(),
        want,
        "signed in or not"
    );
}

/// Ada with a session on honest and a tool credential, Grace with a tool
/// credential.
fn signed_in(at: &Installation) -> (blackbox::browser::Browser, String, String) {
    let mut browser = at.browser_as(ADA);
    at.sign_in(&mut browser, "honest", "/");
    let ada = at.tool_credential_for(ADA);
    let grace = at.tool_credential_for(GRACE);
    (browser, ada, grace)
}

#[test]
fn a_connection_over_an_earlier_one_to_another_issuer_ends_every_session_and_tool_credential() {
    let at = connected();
    let (mut browser, ada_credential, grace_credential) = signed_in(&at);
    let other = Provider::start(&[ada()]);
    at.connect(&Connection::to(&other));
    at.assert_no_session(
        &mut browser,
        "honest",
        "after a connection to another issuer",
    );
    for (who, credential) in [("Ada", &ada_credential), ("Grace", &grace_credential)] {
        assert_eq!(
            bearer(&at.vm, "/v1/me", credential).send().status,
            401,
            "{who}'s tool credential, after a connection to another issuer"
        );
    }
    let mut again = at.browser();
    again.sign_in_at_provider(&other, ADA);
    at.sign_in(&mut again, "honest", "/");
}

#[test]
fn a_connection_over_an_earlier_one_with_another_client_id_ends_every_session_and_tool_credential()
{
    let at = connected();
    let (mut browser, ada_credential, _) = signed_in(&at);
    let second = "vm-harness-client-2";
    at.provider.also_take_client(second);
    at.connect(&Connection::to(&at.provider).client_id(second));
    at.assert_no_session(
        &mut browser,
        "honest",
        "after a connection with another client id",
    );
    assert_eq!(bearer(&at.vm, "/v1/me", &ada_credential).send().status, 401);
    let mut again = at.browser_as(ADA);
    at.sign_in(&mut again, "honest", "/");
}

#[test]
fn a_connection_over_an_earlier_one_with_the_same_issuer_and_client_id_keeps_sessions_and_tool_credentials()
 {
    let at = connected();
    let (mut browser, ada_credential, _) = signed_in(&at);
    at.connect(
        &Connection::to(&at.provider)
            .groups_claim("groups")
            .scopes("openid profile email offline_access"),
    );
    assert_eq!(
        at.user_seen(&mut browser, "honest")["id"],
        ADA,
        "the session stays"
    );
    let me = bearer(&at.vm, "/v1/me", &ada_credential).send();
    assert_eq!(
        me.status,
        200,
        "the tool credential stays\n{}",
        shown(&me, &[])
    );
    assert!(at.logged("session-ended").is_empty());
}

#[test]
fn a_dump_of_the_platform_database_holds_no_secret_in_a_form_that_can_be_used() {
    let at = connected();
    let (_browser, credential, _) = signed_in(&at);
    let platform = at.vm.platform().clone();
    let mut secrets: Vec<(String, String)> = vec![
        (
            "the client secret".to_owned(),
            at.provider.client_secret().expose().to_owned(),
        ),
        (
            "the installation's key".to_owned(),
            at.key().unwrap().expose().to_owned(),
        ),
        ("a tool credential".to_owned(), credential),
    ];
    for (i, token) in at.provider.refresh_tokens_issued().into_iter().enumerate() {
        secrets.push((format!("refresh token {i}"), token));
    }
    assert!(
        secrets.len() >= 5,
        "the harness's own check: a session's and two tool credentials' refresh tokens were issued"
    );
    for (what, secret) in &secrets {
        let holding = platform_tables_holding(&platform, secret);
        assert!(
            holding.is_empty(),
            "expected the platform database to hold {what} in no form that can be used, but these tables hold it: {holding:?}"
        );
    }
}

#[test]
fn a_wrong_client_secret_is_taken_and_shows_at_the_first_sign_in() {
    let provider = Provider::start(&[ada()]);
    let wrong = Connection::to(&provider).client_secret("not-the-client-secret-Rt5");
    let at = Installation::connected(
        provider,
        &oidc_options().application("honest", "honest.js"),
        &wrong,
    );
    let mut browser = at.browser_as(ADA);
    let steps = at.walk_to_callback(&mut browser, "honest", "/");
    let callback = browser.follow(steps.last().unwrap());
    let mut hidden = at.provider.issued();
    hidden.push(at.provider.client_secret().expose().to_owned());
    hidden.push(wrong.secret().expose().to_owned());
    blackbox::sessions::assert_failure_page(
        &callback.response,
        &[400],
        blackbox::sessions::Link::To(&at.app_origin("honest")),
        &hidden,
        "a client secret the provider refuses, at the first sign-in",
    );
}
