//! Who may call what (CONTRACT.md, "Outbound calls: Who may call what"; ADR
//! 0020): the order of the refusals `no-request`, `not-https`,
//! `platform-itself`, `host-not-listed`, `address-not-allowed`,
//! `log-unavailable`; the empty list that refuses everything; a change of
//! the list that takes effect at the next call; the addresses that are never
//! called, whatever the list says.
//!
//! Where a test needs no call to succeed it starts the program without
//! `--outbound-private`, so that it fails on the behaviour and not on the
//! option.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::Duration;

use blackbox::admin;
use blackbox::outbound::{
    APP, Cert, OPEN_LOOPBACK, Origin, Outbound, Reply, assert_fetch_entry, assert_refused,
    assert_resolved, host_call_records, refusal_of,
};
use blackbox::postgres::{Platform, PrivatePostgres};
use blackbox::tls::{Authority, Files, text_of};
use blackbox::vm::DOMAIN;
use serde_json::{Value, json};

fn url(o: &str) -> Value {
    json!({ "url": o })
}

#[test]
fn with_the_list_empty_every_call_is_refused_host_not_listed_and_nothing_leaves() {
    let o = Outbound::start_closed();
    let origin = o.origin();
    let target = origin.url("/path-secret/x?query-secret=1");
    let response = o.call_response(&url(&target));
    let report = o.report(&response);
    assert_refused(
        &report,
        "host-not-listed",
        Some(&origin.host()),
        "an empty list",
    );
    assert_eq!(
        report["rejected"]["isError"], true,
        "expected the rejection to be an Error (without the SDK), got {report}"
    );
    let fix = refusal_of(&report)["fix"].as_str().unwrap();
    assert!(
        fix.contains("vibe admin outbound add"),
        "expected the fix to name `vibe admin outbound add <host>`, got {fix:?}"
    );
    assert!(
        fix.contains("vibe outbound"),
        "expected the fix to say `vibe outbound` shows what is allowed, got {fix:?}"
    );
    origin.assert_untouched("an empty list");

    let entry = o.entry_of(&response);
    assert_fetch_entry(
        &entry,
        "refused",
        Some(&origin.host()),
        Some("host-not-listed"),
        None,
        "the refused call",
    );
    let records = host_call_records(&o.vm, &response);
    assert_eq!(
        records.len(),
        1,
        "expected one `host-call` record, got {records:?}"
    );
    assert_eq!(
        (
            records[0]["outcome"].as_str(),
            records[0]["reason"].as_str()
        ),
        (Some("refused"), Some("host-not-listed")),
        "the program's log: {}",
        records[0]
    );
}

#[test]
fn a_call_with_no_request_in_flight_is_refused_no_request_at_load_and_after_the_request() {
    let o = Outbound::start();
    let origin = o.origin();
    o.allow(&origin.host());
    let at_load: Value =
        o.vm.get_as(&blackbox::outbound::bob(), APP, "/at-load")
            .json();
    assert_refused(
        &at_load["atLoad"],
        "no-request",
        None,
        "a call made while the script loads",
    );
    let started = o.vm.get_as(
        &blackbox::outbound::bob(),
        APP,
        &format!("/after-request?url={}", origin.url("/after")),
    );
    started.expect_status(200);
    thread::sleep(Duration::from_millis(1500));
    origin.assert_untouched("calls with no request in flight");
    // Whatever the program recorded for such calls says refused, `no-request`,
    // and no request.
    for record in o.vm.records() {
        if record["event"] == "host-call" && record["operation"] == "fetch" {
            assert!(
                record["request"].is_null() || record["outcome"] == "refused",
                "expected a `fetch` with no request in flight never allowed, got {record}"
            );
            if record["request"].is_null() {
                assert_eq!(
                    record["reason"], "no-request",
                    "expected reason `no-request`, got {record}"
                );
            }
        }
    }
    o.vm.wait_for_record("a `host-call` of `fetch` refused `no-request`", |r| {
        r["event"] == "host-call"
            && r["operation"] == "fetch"
            && r["outcome"] == "refused"
            && r["reason"] == "no-request"
            && r["request"].is_null()
    });
    for entry in o.entries() {
        if entry["operation"] == "fetch" && entry["request"].is_null() {
            assert_eq!(
                (entry["outcome"].as_str(), entry["reason"].as_str()),
                (Some("refused"), Some("no-request")),
                "expected an entry with no request to be a refusal `no-request`, got {entry}"
            );
        }
    }
}

#[test]
fn an_address_that_is_not_https_with_a_user_in_it_or_not_an_address_is_refused_not_https() {
    let o = Outbound::start_closed();
    let origin = o.origin();
    o.allow(&origin.host());
    let port = origin.port();
    let with_address = [
        format!("http://localhost:{port}/plain"),
        format!("HTTP://localhost:{port}/plain"),
        format!("https://user:pw-secret@localhost:{port}/with-user"),
        format!("https://only-user@localhost:{port}/with-user"),
    ];
    for target in &with_address {
        let response = o.call_response(&url(target));
        let report = o.report(&response);
        assert_refused(&report, "not-https", None, target);
        let text = report.to_string();
        assert!(
            !text.contains("pw-secret"),
            "{target}: expected no password in what the code sees, got {report}"
        );
        let entry = o.entry_of(&response);
        assert_eq!(entry["reason"], "not-https", "{target}: {entry}");
        assert_eq!(entry["outcome"], "refused", "{target}: {entry}");
        assert!(
            !entry.to_string().contains("pw-secret"),
            "{target}: expected no password in the entry, got {entry}"
        );
    }
    origin.assert_untouched("a call that is not https");
    for target in [
        "not an address",
        "",
        "localhost",
        "//localhost/x",
        "mailto:a@b.c",
    ] {
        let response = o.call_response(&url(target));
        let report = o.report(&response);
        assert_refused(&report, "not-https", None, &format!("{target:?}"));
        let entries = o.entries_of(&response);
        assert!(
            entries.is_empty()
                || entries
                    .iter()
                    .all(|e| e["host"].is_null() && e["reason"] == "not-https"),
            "{target:?}: expected no entry, or one with no host, for a call with no address, got {entries:?}"
        );
    }
}

#[test]
fn the_installation_itself_is_refused_platform_itself_even_when_listed_on_any_port() {
    let o = Outbound::start_closed();
    for entry in [
        DOMAIN.to_owned(),
        format!("*.{DOMAIN}"),
        format!("api.{DOMAIN}:8443"),
    ] {
        o.allow(&entry);
    }
    let cases = [
        (format!("https://{DOMAIN}/"), DOMAIN.to_owned()),
        (
            format!("https://api.{DOMAIN}/v1/me"),
            format!("api.{DOMAIN}"),
        ),
        (
            format!("https://{APP}.{DOMAIN}:8443/"),
            format!("{APP}.{DOMAIN}:8443"),
        ),
        (
            format!("https://deep.sub.{DOMAIN}:4443/x"),
            format!("deep.sub.{DOMAIN}:4443"),
        ),
        (
            format!("https://api.{DOMAIN}:8443/"),
            format!("api.{DOMAIN}:8443"),
        ),
        (
            format!("https://{}/", DOMAIN.to_uppercase()),
            DOMAIN.to_owned(),
        ),
    ];
    for (target, host) in cases {
        let response = o.call_response(&url(&target));
        let report = o.report(&response);
        assert_refused(&report, "platform-itself", Some(&host), &target);
        let entry = o.entry_of(&response);
        assert_fetch_entry(
            &entry,
            "refused",
            Some(&host),
            Some("platform-itself"),
            None,
            &target,
        );
    }
    // A name that only ends with the domain's letters is not beneath it.
    let report = o.call(&url(&format!("https://not{DOMAIN}/")));
    assert_refused(
        &report,
        "host-not-listed",
        Some(&format!("not{DOMAIN}")),
        "a name that merely ends like the domain",
    );
    // And the order: it is `platform-itself` before `host-not-listed`.
    let report = o.call(&url(&format!("https://unlisted.{DOMAIN}/")));
    assert_refused(
        &report,
        "platform-itself",
        None,
        "a name beneath the domain, listed or not",
    );
}

#[test]
fn a_host_is_allowed_only_as_the_entry_says_its_port_and_its_name_exactly_or_one_wildcard() {
    let o = Outbound::start();
    let listed = o.origin();
    let other_port = o.origin();
    o.allow(&listed.host());
    o.allow("*.example.invalid");
    o.allow("*.ported.invalid:8443");

    // The same name on another port is another host.
    let report = o.call(&url(&other_port.url("/")));
    assert_refused(
        &report,
        "host-not-listed",
        Some(&other_port.host()),
        "another port of a listed host",
    );
    other_port.assert_untouched("another port of a listed host");

    // An entry on port 443 does not allow a port; a port does not allow 443.
    let report = o.call(&url("https://localhost/x"));
    assert_refused(
        &report,
        "host-not-listed",
        Some("localhost"),
        "port 443 when only a port is listed",
    );

    // A name beneath a listed name is not the listed name.
    let report = o.call(&url(&format!("https://sub.localhost:{}/", listed.port())));
    assert_refused(
        &report,
        "host-not-listed",
        Some(&format!("sub.localhost:{}", listed.port())),
        "a name beneath a listed name",
    );
    listed.assert_untouched("a name beneath a listed name");

    // A wildcard allows what is beneath it (the call goes on, and fails
    // later: the name does not resolve), and not its own base.
    let report = o.call(&url("https://a.b.example.invalid/x"));
    assert!(
        report["rejected"]["refused"].is_null(),
        "expected a name beneath `*.example.invalid` not refused, got {report}"
    );
    assert_failure_unreachable(&report, "a name beneath a wildcard");
    let report = o.call(&url("https://example.invalid/x"));
    assert_refused(
        &report,
        "host-not-listed",
        Some("example.invalid"),
        "the wildcard's own base",
    );
    // A wildcard has a port of its own.
    let report = o.call(&url("https://x.ported.invalid/x"));
    assert_refused(
        &report,
        "host-not-listed",
        Some("x.ported.invalid"),
        "a wildcard listed on 8443, called on 443",
    );
    let report = o.call(&url("https://x.ported.invalid:8443/x"));
    assert!(
        report["rejected"]["refused"].is_null(),
        "expected a name beneath `*.ported.invalid:8443` on 8443 not refused, got {report}"
    );

    // Letter case does not matter, and the entry shows the host in lower case.
    let upper = format!("HTTPS://LOCALHOST:{}/Path", listed.port());
    let response = o.call_response(&url(&upper));
    let report = o.report(&response);
    assert_resolved(&report, 200, "a listed host written in capitals");
    let entry = o.entry_of(&response);
    assert_eq!(entry["host"], json!(listed.host()), "{entry}");
    assert_eq!(listed.the_request().path(), "/Path");
}

fn assert_failure_unreachable(report: &Value, context: &str) {
    assert_eq!(
        report["rejected"]["failure"]["kind"], "unreachable",
        "{context}: expected the failure `unreachable`, got {report}"
    );
}

#[test]
fn a_listed_name_that_resolves_to_a_loopback_address_is_refused_address_not_allowed() {
    // Started without `--outbound-private`: nothing of the kinds is opened.
    let o = Outbound::start_closed();
    let origin = o.origin();
    o.allow(&origin.host());
    let response = o.call_response(&url(&origin.url("/")));
    let report = o.report(&response);
    assert_refused(
        &report,
        "address-not-allowed",
        Some(&origin.host()),
        "a listed name that resolves to loopback",
    );
    let fix = refusal_of(&report)["fix"].as_str().unwrap().to_lowercase();
    assert!(
        fix.contains("network") || fix.contains("machine"),
        "expected the fix to say the platform does not call addresses inside the company's network or the machine itself, got {fix:?}"
    );
    assert!(
        fix.contains("list"),
        "expected the fix to say it is so whatever the list says, got {fix:?}"
    );
    origin.assert_untouched("a listed name that resolves to loopback");
    let entry = o.entry_of(&response);
    assert_fetch_entry(
        &entry,
        "refused",
        Some(&origin.host()),
        Some("address-not-allowed"),
        None,
        "the refused call",
    );
}

#[test]
fn every_kind_of_address_that_is_never_called_is_refused_though_listed() {
    let o = Outbound::start_closed();
    let never = [
        "127.0.0.1",
        "127.255.255.254",
        "10.0.0.1",
        "10.255.255.255",
        "172.16.0.1",
        "172.31.255.255",
        "192.168.0.1",
        "192.168.255.255",
        "169.254.169.254",
        "169.254.0.1",
        "100.64.0.1",
        "100.127.255.255",
        "0.0.0.0",
        "0.1.2.3",
        "224.0.0.1",
        "239.255.255.255",
        "240.0.0.1",
        "255.255.255.255",
        "192.0.0.8",
        "192.0.2.1",
        "198.18.0.1",
        "198.19.255.255",
        "198.51.100.1",
        "203.0.113.7",
        "[::1]",
        "[::]",
        "[fc00::1]",
        "[fd12:3456:789a::1]",
        "[fe80::1]",
        "[febf::1]",
        "[ff02::1]",
        "[2001:db8::7]",
        "[2001:db8:ffff::1]",
        // An IPv6 address that carries an IPv4 one is judged by that one.
        "[::ffff:7f00:1]",
        "[::ffff:a00:1]",
        "[::ffff:a9fe:a9fe]",
        "[64:ff9b::7f00:1]",
        "[64:ff9b::a00:1]",
        "[2002:7f00:1::]",
        "[2002:a9fe:a9fe::]",
    ];
    for address in never {
        for port in ["", ":8443"] {
            let host = format!("{address}{port}");
            o.allow(&host);
            let response = o.call_response(&url(&format!("https://{host}/x?y=1")));
            let report = o.report(&response);
            assert_refused(
                &report,
                "address-not-allowed",
                Some(&host),
                &format!("{host} (listed)"),
            );
            let entry = o.entry_of(&response);
            assert_fetch_entry(
                &entry,
                "refused",
                Some(&host),
                Some("address-not-allowed"),
                None,
                &host,
            );
        }
    }
}

#[test]
fn an_address_the_operator_did_not_open_is_refused_though_listed_and_nothing_connects_there() {
    // `--outbound-private` opens 127.0.0.1 and ::1 only; 127.0.0.2 is
    // loopback too, and a server there sees nothing.
    let o = Outbound::start();
    let authority_cert = o.cert();
    let there = Origin::start_at("127.0.0.2".parse().unwrap(), authority_cert.wire());
    let host = format!("127.0.0.2:{}", there.port());
    o.allow(&host);
    let report = o.call(&url(&format!("https://{host}/")));
    assert_refused(
        &report,
        "address-not-allowed",
        Some(&host),
        "an address not opened",
    );
    there.assert_untouched("an address the operator did not open");
}

#[test]
fn an_address_the_operator_opened_is_called_by_its_address_when_listed() {
    let o = Outbound::start();
    let origin = o.origin();
    let host = format!("127.0.0.1:{}", origin.port());
    let report = o.call(&url(&format!("https://{host}/by-address")));
    assert_refused(
        &report,
        "host-not-listed",
        Some(&host),
        "an address not listed",
    );
    o.allow(&host);
    let report = o.call(&url(&format!("https://{host}/by-address")));
    assert_resolved(&report, 200, "an address listed and opened");
    assert_eq!(origin.the_request().path(), "/by-address");
}

#[test]
fn a_change_of_the_list_takes_effect_at_the_next_call_both_ways_with_nothing_deployed_again() {
    let o = Outbound::start();
    let origin = o.origin();
    origin.always(Reply::ok(b"hello"));
    let version_before =
        o.vm.api_as(&blackbox::outbound::bob())
            .get_application(APP)
            .json();

    assert_refused(
        &o.call(&url(&origin.url("/"))),
        "host-not-listed",
        None,
        "before it is listed",
    );
    o.allow(&origin.host());
    assert_resolved(
        &o.call(&url(&origin.url("/"))),
        200,
        "right after it is listed",
    );
    o.disallow(&origin.host());
    assert_refused(
        &o.call(&url(&origin.url("/"))),
        "host-not-listed",
        None,
        "right after it is removed",
    );
    o.allow(&origin.host());
    assert_resolved(&o.call(&url(&origin.url("/"))), 200, "listed again");
    assert_eq!(
        origin.requests().len(),
        2,
        "expected exactly the two allowed calls to arrive"
    );
    let version_after =
        o.vm.api_as(&blackbox::outbound::bob())
            .get_application(APP)
            .json();
    assert_eq!(
        version_before, version_after,
        "expected nothing deployed again between the changes"
    );
}

#[test]
fn a_call_under_way_is_not_cut_by_removing_its_host_from_the_list() {
    let o = Outbound::start();
    let origin = o.origin();
    origin.always(Reply::ok(b"late").after(Duration::from_secs(3)));
    o.allow(&origin.host());
    let report = thread::scope(|scope| {
        let call = scope.spawn(|| o.call(&url(&origin.url("/slow"))));
        origin.wait_requests(1, Duration::from_secs(10));
        o.disallow(&origin.host());
        call.join().unwrap()
    });
    assert_resolved(&report, 200, "a call under way when its host was removed");
    assert_eq!(report["resolved"]["text"], "late", "{report}");
    assert_refused(
        &o.call(&url(&origin.url("/again"))),
        "host-not-listed",
        None,
        "the next call",
    );
}

#[test]
fn when_the_entry_cannot_be_written_a_listed_call_is_refused_log_unavailable_and_nothing_leaves() {
    let mut postgres = PrivatePostgres::start().unwrap();
    let platform = Platform::on(&postgres, "vm_platform_outbound_log_unavailable");
    let authority = Authority::new("outbound test authority");
    let files = Files::new();
    let ca_file = files.authorities("ca.pem", &[&authority]);
    let options = admin::options()
        .platform(&platform)
        .args_extra(&OPEN_LOOPBACK)
        .env("VIBE_CA_FILE", &text_of(&ca_file));
    let o = Outbound::started(&options, authority, files, ca_file, &[]);
    let origin = Origin::start(Cert::localhost(&o.authority).wire());
    o.allow(&origin.host());
    assert_resolved(
        &o.call(&url(&origin.url("/before"))),
        200,
        "while all is well",
    );

    postgres.stop();
    let report = o.call(&url(&origin.url("/after")));
    assert_refused(
        &report,
        "log-unavailable",
        None,
        "a listed call whose entry cannot be written",
    );
    assert_eq!(
        origin.requests().len(),
        1,
        "expected nothing to leave when the entry cannot be written, got {:?}",
        origin
            .requests()
            .iter()
            .map(|r| r.describe())
            .collect::<Vec<_>>()
    );
    // An earlier reason in the order is kept.
    let report = o.call(&url("https://unlisted.example.invalid/"));
    assert_refused(
        &report,
        "host-not-listed",
        None,
        "a call refused for an earlier reason keeps it",
    );
}
