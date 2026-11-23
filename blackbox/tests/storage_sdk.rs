//! Storage through the SDK (CONTRACT.md, "Storage: Through the SDK";
//! ticket: an Application that declares it reads, writes, lists and deletes
//! files through the SDK as the request's User). The fixture project
//! `fixtures/sdk/files` is built with the SDK's tooling and installed with
//! the Declared access its `manifest` printed.
//!
//! What each call answers and that it is exactly one host operation;
//! refusals as `Refused` with the kinds of a mismatch, failures as
//! `OperationFailed`, and what escapes `fetch`; arguments of the wrong type
//! a `TypeError` with nothing sent; a body put and read back with the
//! readers, and an upload of the request's own body; hostile keys forced
//! past the types. The body as a stream, every kind a put takes and what
//! is refused, and the readers each once, are in
//! `blackbox/tests/storage_stream_*.rs`. The compile errors are the type
//! tests of `blackbox/sdk-types/projects/storage/`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::http::Response;
use blackbox::sdk::{SDK_REQUEST_TIMEOUT, built, install};
use blackbox::storage::{
    FILE_READERS, FILE_WRITERS, FILES_CONTACT, MARKETING, Storage, assert_host_calls, b64, mtime,
    nobody, rita, tree, wanda,
};
use serde_json::{Value, json};

const APP: &str = "sdk-files";

/// A body past the old 8 MiB bound: any size the file bound allows goes
/// through the readers whole.
const EIGHT_MIB: usize = 8 * 1024 * 1024;

fn encode(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

/// The program with `marketing` and `archive` (Storage) and `devops` (a
/// database never reached) registered; file-writers granted `write` on
/// both Storage Resources, file-readers `read` on `marketing`; and
/// `fixtures/sdk/files` installed.
fn files() -> Storage {
    let built = built("files");
    let s = Storage::start();
    s.register(MARKETING);
    s.register("archive");
    s.register_postgres("devops");
    s.grant(MARKETING, FILE_WRITERS, "write");
    s.grant(MARKETING, FILE_READERS, "read");
    s.grant("archive", FILE_WRITERS, "write");
    install(&s.vm, &built);
    s
}

fn send(s: &Storage, user: &User, method: &str, target: &str, body: &[u8]) -> Response {
    s.vm.request(APP, target)
        .as_user(user)
        .method(method)
        .header("content-type", "application/octet-stream")
        .body(body)
        .timeout(SDK_REQUEST_TIMEOUT)
        .send()
}

fn ok_json(s: &Storage, user: &User, method: &str, target: &str, body: &[u8]) -> (Response, Value) {
    let response = send(s, user, method, target, body);
    if response.status != 200 {
        s.vm.fail(&format!(
            "expected `200` with JSON from {APP} for {method} {target} as {:?}, got\n{}",
            user.id,
            response.describe()
        ));
    }
    let json = response.json();
    (response, json)
}

/// Fails unless `info` describes a `FileInfo` with these values and a
/// `lastModified` that is a `Date` of whole seconds.
fn assert_info(info: &Value, key: &str, size: usize, content_type: &str, context: &str) {
    assert_eq!(info["key"], json!(key), "{context}: {info}");
    assert_eq!(
        info["size"].as_u64(),
        u64::try_from(size).ok(),
        "{context}: {info}"
    );
    assert_eq!(
        info["contentType"],
        json!(content_type),
        "{context}: {info}"
    );
    assert_eq!(
        info["isDate"], true,
        "{context}: expected `lastModified` a Date, got {info}"
    );
    let time = info["lastModified"].as_str().unwrap_or_default();
    assert!(
        time.ends_with(".000Z"),
        "{context}: expected `lastModified` of whole seconds, got {info}"
    );
}

#[test]
fn put_get_head_list_and_delete_through_the_sdk_each_one_host_operation() {
    let s = files();
    let bytes: Vec<u8> = b"%PDF \xff\xfe not UTF-8 ".to_vec();
    let (response, put) = ok_json(&s, &wanda(), "POST", "/put?key=reports/q3.pdf", &bytes);
    assert_info(
        &put["info"],
        "reports/q3.pdf",
        bytes.len(),
        "application/pdf",
        "put of bytes",
    );
    assert_host_calls(&s.vm, &response, &["storage.put"], "a put");
    assert_eq!(
        s.entries_eventually(&response, 1).len(),
        1,
        "one entry for one put"
    );
    let file = s.dir(MARKETING).join("reports/q3.pdf");
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    let seconds =
        blackbox::api::parse_rfc3339(put["info"]["lastModified"].as_str().unwrap()).unwrap();
    assert_eq!(
        seconds,
        mtime(&file),
        "expected `lastModified` the host's time of last change"
    );

    let (response, got) = ok_json(&s, &rita(), "GET", "/get?key=reports/q3.pdf", b"");
    assert_host_calls(&s.vm, &response, &["storage.get"], "a get");
    let f = &got["file"];
    assert_info(
        &f["info"],
        "reports/q3.pdf",
        bytes.len(),
        "application/pdf",
        "get",
    );
    assert_eq!(
        f["isStream"], true,
        "expected `body` a ReadableStream fed by the host: {got}"
    );
    assert_eq!(
        f["bodyUsedAfter"], true,
        "expected `bodyUsed` true once the body was read: {got}"
    );
    assert_eq!(
        f["bytesB64"],
        json!(b64(&bytes)),
        "expected the bytes put: {got}"
    );
    let (_, text) = ok_json(&s, &rita(), "GET", "/get-text?key=reports/q3.pdf", b"");
    assert!(
        text["text"].as_str().unwrap().contains('\u{fffd}'),
        "expected text() to decode malformed bytes as U+FFFD: {text}"
    );
    let (response, headed) = ok_json(&s, &rita(), "GET", "/head?key=reports/q3.pdf", b"");
    assert_host_calls(&s.vm, &response, &["storage.head"], "a head");
    assert_eq!(headed["info"], f["info"], "head and get agree");
    let (_, none) = ok_json(&s, &rita(), "GET", "/get?key=reports/none.pdf", b"");
    assert_eq!(
        none["file"],
        Value::Null,
        "expected get of no file to be null"
    );
    let (_, none) = ok_json(&s, &rita(), "GET", "/head?key=reports/none.pdf", b"");
    assert_eq!(
        none["info"],
        Value::Null,
        "expected head of no file to be null"
    );

    let (_, text) = ok_json(
        &s,
        &wanda(),
        "POST",
        "/put?key=notes/a.txt&as=text",
        "héllo ☃".as_bytes(),
    );
    assert_info(
        &text["info"],
        "notes/a.txt",
        "héllo ☃".len(),
        "text/plain; charset=utf-8",
        "a string",
    );
    assert_eq!(
        std::fs::read(s.dir(MARKETING).join("notes/a.txt")).unwrap(),
        "héllo ☃".as_bytes()
    );
    let (_, buffer) = ok_json(
        &s,
        &wanda(),
        "POST",
        "/put?key=notes/b.bin&as=buffer&type=application/x-given",
        b"\x00\x01\x02",
    );
    assert_info(
        &buffer["info"],
        "notes/b.bin",
        3,
        "application/x-given",
        "an ArrayBuffer",
    );

    let (response, listed) = ok_json(&s, &rita(), "GET", "/list?delimiter=/", b"");
    assert_host_calls(&s.vm, &response, &["storage.list"], "a list");
    assert_eq!(listed["folders"], json!(["notes/", "reports/"]), "{listed}");
    assert_eq!(listed["files"], json!([]), "{listed}");
    assert_eq!(listed["cursor"], Value::Null);
    // Pages of `limit` 1 through the SDK, with the cursor until it is
    // null (a page may be short: at most `limit`).
    let mut paged = Vec::new();
    let mut target = "/list?prefix=notes/&limit=1".to_owned();
    for pages in 1.. {
        let (_, page) = ok_json(&s, &rita(), "GET", &target, b"");
        let files = page["files"].as_array().unwrap();
        assert!(files.len() <= 1, "at most `limit`: {page}");
        paged.extend(files.iter().map(|f| f["key"].as_str().unwrap().to_owned()));
        let Some(cursor) = page["cursor"].as_str() else {
            break;
        };
        assert!(pages < 100, "the pages do not end: {paged:?}");
        target = format!("/list?prefix=notes/&limit=1&cursor={}", encode(cursor));
    }
    assert_eq!(paged, ["notes/a.txt", "notes/b.bin"], "the pages together");
    let (_, all) = ok_json(&s, &rita(), "GET", "/list-all", b"");
    assert_eq!(
        all["files"].as_array().unwrap().len(),
        3,
        "a list with no options: {all}"
    );

    let (response, deleted) = ok_json(&s, &wanda(), "GET", "/delete?key=notes/a.txt", b"");
    assert_host_calls(&s.vm, &response, &["storage.delete"], "a delete");
    assert_eq!(
        deleted["result"], "undefined",
        "expected delete to resolve to nothing"
    );
    assert!(!s.dir(MARKETING).join("notes/a.txt").exists());

    let served = send(&s, &rita(), "GET", "/serve?key=reports/q3.pdf", b"");
    assert_eq!(served.status, 200, "{}", served.describe());
    assert_eq!(
        served.body(),
        bytes.as_slice(),
        "expected the file's bytes served"
    );
    assert_eq!(served.header("content-type"), Some("application/pdf"));
}

/// What `/caught?case=<case>` answered as `who`.
fn caught(s: &Storage, who: &User, case: &str) -> Value {
    ok_json(
        s,
        who,
        "GET",
        &format!("/caught?case={case}&key=reports/q3.pdf"),
        b"",
    )
    .1
}

fn assert_refused(answer: &Value, refusal: &Value, context: &str) {
    let error = &answer["error"];
    assert_eq!(
        error["isRefused"], true,
        "{context}: expected a `Refused`, got {answer}"
    );
    assert_eq!(error["name"], "Refused", "{context}: {answer}");
    assert_eq!(error["isError"], true, "{context}: {answer}");
    assert_eq!(
        &error["refusal"], refusal,
        "{context}: expected the refusal {refusal}, got {answer}"
    );
    let fix = error["fix"].as_str().unwrap_or_default();
    assert!(fix.len() >= 10, "{context}: expected a fix, got {answer}");
    assert_eq!(
        error["message"],
        json!(format!("{}: {fix}", refusal["reason"].as_str().unwrap())),
        "{context}: expected the message `<reason>: <fix>`"
    );
}

#[test]
fn refusals_reject_with_refused_and_a_mismatch_carries_the_kinds() {
    let s = files();
    let before = tree(&s.place.base);
    assert_refused(
        &caught(&s, &wanda(), "put-through-read"),
        &json!({ "reason": "declared-lower", "resource": "archive", "declared": "read", "needed": "write" }),
        "a put forced through a handle declared at read",
    );
    assert_refused(
        &caught(&s, &wanda(), "delete-through-read"),
        &json!({ "reason": "declared-lower", "resource": "archive", "declared": "read", "needed": "write" }),
        "a delete forced through a handle declared at read",
    );
    assert_refused(
        &caught(&s, &wanda(), "undeclared"),
        &json!({ "reason": "not-declared", "resource": "payroll", "kind": "storage" }),
        "storage() of a name not declared",
    );
    assert_refused(
        &caught(&s, &wanda(), "declared-postgres"),
        &json!({ "reason": "not-declared", "resource": "devops", "kind": "storage", "declared_kind": "postgres" }),
        "storage() of a name declared postgres",
    );
    assert_refused(
        &caught(&s, &wanda(), "db-on-storage"),
        &json!({ "reason": "not-declared", "resource": MARKETING, "kind": "postgres", "declared_kind": "storage" }),
        "db() of a name declared storage",
    );
    assert_refused(
        &caught(&s, &wanda(), "app-storage"),
        &json!({ "reason": "not-own-database", "resource": MARKETING }),
        "ctx.app().storage()",
    );
    assert_refused(
        &caught(&s, &nobody(), "get"),
        &json!({ "reason": "no-grant", "resource": MARKETING, "user": "nobody", "needed": "read", "held": null, "contact": FILES_CONTACT }),
        "a get by a User with nothing",
    );
    assert_refused(
        &caught(&s, &rita(), "put"),
        &json!({ "reason": "no-grant", "resource": MARKETING, "user": "rita", "needed": "write", "held": "read", "contact": FILES_CONTACT }),
        "a put by a reader",
    );
    assert_eq!(
        tree(&s.place.base),
        before,
        "expected a refused call to touch nothing"
    );
}

#[test]
fn a_failure_rejects_with_operation_failed_and_what_escapes_fetch_is_a_403_or_the_fixed_500() {
    let s = files();
    let failed = caught(&s, &wanda(), "invalid-key");
    let error = &failed["error"];
    assert_eq!(
        error["isOperationFailed"], true,
        "expected an `OperationFailed`, got {failed}"
    );
    assert_eq!(error["isError"], true, "{failed}");
    assert_eq!(error["kind"], "invalid-key", "{failed}");
    assert_eq!(
        error["detail"]["kind"], "invalid-key",
        "expected the host's whole error: {failed}"
    );
    assert_eq!(error["detail"]["rule"], "dot-segment", "{failed}");

    let escaped = send(&s, &wanda(), "GET", "/escape?case=refused", b"");
    assert_eq!(escaped.status, 403, "{}", escaped.describe());
    assert!(
        escaped
            .header("content-type")
            .is_some_and(|t| t.starts_with("application/json")),
        "{}",
        escaped.describe()
    );
    let body = escaped.json();
    assert_eq!(
        body["refusal"],
        json!({ "reason": "declared-lower", "resource": "archive", "declared": "read", "needed": "write" }),
        "expected the 403's body {{ refusal, fix }}, got {body}"
    );
    assert!(
        body["fix"].as_str().is_some_and(|f| !f.is_empty()),
        "{body}"
    );
    let failed = send(&s, &wanda(), "GET", "/escape?case=failed", b"");
    assert_eq!(failed.status, 500, "{}", failed.describe());
    assert!(
        !failed.text().contains("invalid-key") && !failed.text().contains("dot-segment"),
        "expected the fixed 500, holding nothing of the error: {}",
        failed.describe()
    );
}

#[test]
fn arguments_of_the_wrong_type_are_a_type_error_and_nothing_is_sent() {
    let s = files();
    for case in [
        "body-number",
        "body-object",
        "body-missing",
        "key-number",
        "key-missing",
        "delete-key-object",
        "put-options-number",
        "content-type-number",
        "list-options-string",
        "list-prefix-number",
    ] {
        let (response, answer) = ok_json(
            &s,
            &wanda(),
            "GET",
            &format!("/type-error?case={case}"),
            b"",
        );
        assert_eq!(
            answer["error"]["isTypeError"], true,
            "{case}: expected a TypeError, got {answer}"
        );
        assert_host_calls(
            &s.vm,
            &response,
            &[],
            &format!("{case}: nothing sent to the host"),
        );
        assert!(s.entries_of(&response).is_empty(), "{case}: no entry");
    }
}

#[test]
fn a_body_goes_through_the_sdk_and_through_an_upload_of_the_requests_own_body() {
    let s = files();
    let (_, big) = ok_json(
        &s,
        &wanda(),
        "GET",
        &format!("/big?key=big/eight.bin&size={EIGHT_MIB}"),
        b"",
    );
    assert_eq!(
        big["same"], true,
        "expected 8 MiB put and read back whole through `bytes()`: {big}"
    );
    assert_info(
        &big["put"],
        "big/eight.bin",
        EIGHT_MIB,
        "application/octet-stream",
        "8 MiB put",
    );
    let upload = vec![0x5a_u8; EIGHT_MIB];
    let (response, up) = ok_json(&s, &wanda(), "POST", "/upload?key=big/upload.bin", &upload);
    assert_info(
        &up["info"],
        "big/upload.bin",
        EIGHT_MIB,
        "application/octet-stream",
        "an upload of the request's own body",
    );
    assert_host_calls(
        &s.vm,
        &response,
        &["storage.put"],
        "put(key, request.body) is one host operation",
    );
    let on_disk = std::fs::read(s.dir(MARKETING).join("big/upload.bin")).unwrap();
    assert!(on_disk == upload, "expected the upload's bytes on the disk");
}

#[test]
fn hostile_keys_forced_past_the_types_end_at_the_host() {
    let s = files();
    let before = tree(&s.place.base);
    let (response, body) = ok_json(&s, &wanda(), "GET", "/hostile", b"");
    let rules = [
        ("dotdot-secret", "dot-segment", Some("dot-segment")),
        ("absolute", "empty-segment", Some("empty-segment")),
        ("dotdot", "dot-segment", None),
        ("nul", "character", Some("character")),
        ("backslash", "character", Some("character")),
        ("marker", "reserved", Some("reserved")),
        ("not-nfc", "not-nfc", Some("not-nfc")),
    ];
    for (label, rule, list_rule) in rules {
        for operation in ["head", "get", "put", "delete"] {
            let answer = &body["answers"][label][operation];
            assert_eq!(
                (
                    answer["error"]["kind"].as_str(),
                    answer["error"]["detail"]["rule"].as_str()
                ),
                (Some("invalid-key"), Some(rule)),
                "{operation} of {label}: expected OperationFailed invalid-key {rule}, got {answer}"
            );
        }
        let listed = &body["answers"][label]["list"];
        match list_rule {
            Some(rule) => assert_eq!(
                listed["error"]["detail"]["rule"].as_str(),
                Some(rule),
                "list with the prefix {label}: {listed}"
            ),
            None => assert!(
                listed.get("resolved").is_some(),
                "list with the prefix {label}: {listed}"
            ),
        }
    }
    assert_eq!(
        s.entries_eventually(&response, rules.len() * 5).len(),
        rules.len() * 5
    );
    assert_eq!(
        tree(&s.place.base),
        before,
        "expected nothing made anywhere"
    );
}
