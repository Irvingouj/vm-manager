//! The guest (`guest/attacks.js`) attacks the identity the host attaches.
//! Every test says what the guest tried and checks what the host attached,
//! from the guest's answer and from the host's own audit log.

use std::future::Future;
use std::time::{Duration, Instant};

use idp_host::{AppWorker, Audit, AuditRecord, Served, get, limits};
use serde_json::Value;

const GUEST: &str = include_str!("../../guest/attacks.js");

fn run<F: Future<Output = ()>>(f: F) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&rt, f);
}

async fn worker(audit: &Audit) -> AppWorker {
    AppWorker::new("demo", GUEST, limits(1_000, 10_000), audit.clone())
        .await
        .expect("worker")
}

async fn call(worker: &mut AppWorker, user: &str, path: &str) -> Value {
    call_with(worker, user, path, &[]).await
}

async fn call_with(worker: &mut AppWorker, user: &str, path: &str, headers: &[(&str, &str)]) -> Value {
    let served: Served = worker.serve(user, get(path, headers)).await;
    assert!(served.outcome.is_ok(), "{path}: {:?}", served.outcome);
    let body = served.json();
    eprintln!("{user} {path} [request {}] -> {body}", served.attached.request_id);
    body
}

fn records(audit: &Audit, tag: &str) -> Vec<AuditRecord> {
    audit
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.args["tag"].as_str() == Some(tag))
        .cloned()
        .collect()
}

fn user_of(record: &AuditRecord) -> Option<&str> {
    record.attached.as_ref().map(|a| a.user.as_str())
}

/// The answer of a `whoami` the guest settled: `{ok: Attached}` or `{err}`.
fn ok_user(v: &Value) -> &str {
    v["ok"]["user"].as_str().unwrap_or_else(|| panic!("not an ok whoami: {v}"))
}

#[test]
fn claim_another_user() {
    run(async {
        let audit = Audit::default();
        let mut w = worker(&audit).await;

        // The client forges the identity header in several cases; the host
        // was told the User is alice (in the server, from the one header).
        let headers = call_with(&mut w, "alice", "/headers", &[("X-VM-User", "mallory"), ("x-vm-user", "mallory")]).await;
        let seen = headers["headers"].as_object().unwrap();
        assert!(
            !seen.keys().any(|k| k.eq_ignore_ascii_case("x-vm-user")),
            "the guest saw the identity header: {seen:?}"
        );

        // The guest puts another User in arguments, the native function,
        // a Request it builds, and globals; and tries an outbound fetch.
        let body = call(&mut w, "alice", "/claim-user").await;
        for key in ["args", "native", "request"] {
            assert_eq!(ok_user(&body[key]), "alice", "{key}: {body}");
            assert_eq!(body[key]["ok"]["application"], "demo", "{key}: {body}");
        }
        assert!(body["fetch"]["err"].is_string(), "fetch must fail: {body}");

        // An awaited outbound fetch: the host's egress attaches alice.
        let body = call(&mut w, "alice", "/fetch-whoami?tag=f").await;
        assert_eq!(body["fetch"]["ok"]["attached"]["user"], "alice", "{body}");

        // The host's own log: every call of this request was alice's.
        for r in audit.lock().unwrap().iter().filter(|r| r.attached.is_some()) {
            assert_eq!(user_of(r), Some("alice"), "{r:?}");
        }
    });
}

#[test]
fn claim_another_application() {
    run(async {
        let audit = Audit::default();
        let mut w = worker(&audit).await;
        let body = call(&mut w, "alice", "/claim-app").await;

        assert_eq!(body["envKeys"], serde_json::json!(["HOST"]));
        // Another binding name reaches the host and is refused by it.
        assert_eq!(body["otherBinding"]["err"], "no binding PAYROLL", "{body}");
        assert_eq!(body["otherBindingCase"]["err"], "no binding host", "{body}");
        assert_eq!(body["emptyBinding"]["err"], "no binding ", "{body}");
        // An application in the arguments is ignored.
        assert_eq!(body["argClaim"]["ok"]["application"], "demo", "{body}");
        assert!(
            body["redefineEnv"]["ok"].as_str().unwrap().starts_with("refused"),
            "{body}"
        );

        // A second Application on the same host: its worker attaches its own
        // name, whatever the guest does.
        let mut other = AppWorker::new("payroll", GUEST, limits(1_000, 10_000), audit.clone())
            .await
            .unwrap();
        let body = call(&mut other, "alice", "/claim-app").await;
        assert_eq!(body["argClaim"]["ok"]["application"], "payroll", "{body}");
    });
}

/// alice's request keeps `env`, the binding, a detached `invoke`, a closure
/// and a pending promise, and arms timers; bob's request comes `gap` later.
async fn stash_then_use(gap: Duration) -> (Value, Value, Audit) {
    let audit = Audit::default();
    let mut w = worker(&audit).await;

    let stash = call(&mut w, "alice", "/stash?tag=a").await;
    tokio::time::sleep(gap).await;
    let used = call(&mut w, "bob", "/use-stash?tag=b").await;
    // Let anything still pending land, then look again.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let later = call(&mut w, "carol", "/results").await;
    eprintln!("stash: {stash}\nused: {used}\nlater: {later}");
    (used, later, audit)
}

fn check_stash(used: &Value, later: &Value, audit: &Audit) {
    // Handles alice left behind, used in bob's request: the host attaches
    // the request in flight, bob's. Nothing of alice's identity travels.
    for key in ["closure", "detachedInvoke", "oldEnv"] {
        assert_eq!(ok_user(&used[key]), "bob", "{key}: {used}");
    }
    // The timer and waitUntil work after alice's answer ran inside her event.
    assert_eq!(ok_user(&used["timerResult"]), "alice", "{used}");
    assert_eq!(ok_user(&used["waitUntilResult"]), "alice", "{used}");
    // A pending op and a timer that outlive alice's event never run JS again.
    assert_eq!(used["pendingResult"], "not run", "{used}");
    assert_eq!(used["lateTimerResult"], "not run", "{used}");
    assert_eq!(later["pendingResult"], "not run", "{later}");
    assert_eq!(later["lateTimerResult"], "not run", "{later}");

    // The host log: every call made in alice's request (tags a:*) was
    // attached to alice; none ran later under anyone else.
    for r in audit.lock().unwrap().iter() {
        let tag = r.args["tag"].as_str().unwrap_or("");
        if tag == "a" || (tag.starts_with("a:") && tag != "a:closure") {
            assert_eq!(user_of(r), Some("alice"), "{r:?}");
        }
    }
    assert!(records(audit, "a:pending").is_empty());
    assert!(records(audit, "a:late-timer").is_empty());
    // The closure alice made ran in bob's request, attached to bob.
    let closure = records(audit, "a:closure");
    assert_eq!(closure.len(), 1);
    assert_eq!(user_of(&closure[0]), Some("bob"));
}

#[test]
fn handles_kept_after_the_request_next_request_at_once() {
    run(async {
        let (used, later, audit) = stash_then_use(Duration::ZERO).await;
        check_stash(&used, &later, &audit);
    });
}

#[test]
fn handles_kept_after_the_request_next_request_after_the_timers() {
    run(async {
        // bob comes after alice's pending op and late timer are due.
        let (used, later, audit) = stash_then_use(Duration::from_millis(500)).await;
        check_stash(&used, &later, &audit);
    });
}

#[test]
fn no_request_in_flight_at_load_and_between_requests() {
    run(async {
        let audit = Audit::default();
        let mut w = worker(&audit).await;

        // The top-level call at load: no identity attached.
        let load = records(&audit, "load");
        assert_eq!(load.len(), 1, "the load-time call reached the host");
        assert_eq!(load[0].attached, None);

        // alice arms a 10 ms timer and leaves; nothing runs for 100 ms.
        call(&mut w, "alice", "/timer-between").await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let results = call(&mut w, "bob", "/results").await;
        assert_eq!(results["loadResult"]["err"], "no request in flight: the host attaches no identity");
        assert_eq!(results["betweenResult"], "not run", "{results}");
        assert!(records(&audit, "between").is_empty());

        // The host itself runs the guest's callbacks between requests
        // (Worker::process_callbacks): the timer's call gets no identity.
        call(&mut w, "alice", "/timer-between").await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        w.worker().process_callbacks();
        tokio::time::sleep(Duration::from_millis(20)).await;
        w.worker().process_callbacks();
        let between = records(&audit, "between");
        eprintln!("between after process_callbacks: {between:?}");
        for r in &between {
            assert_eq!(r.attached, None, "{r:?}");
        }
    });
}

#[test]
fn global_state_is_shared_by_users_in_a_reused_context() {
    run(async {
        let audit = Audit::default();

        // Warm reuse: bob reads what alice left in globalThis.
        let mut w = worker(&audit).await;
        call(&mut w, "alice", "/global-write?v=alice-secret").await;
        let read = call(&mut w, "bob", "/global-read").await;
        assert_eq!(read["read"], "alice-secret", "expected the leak in a reused context");

        // A fresh worker (isolate + context) per request: nothing to read.
        let mut a = worker(&audit).await;
        call(&mut a, "alice", "/global-write?v=alice-secret").await;
        drop(a);
        let mut b = worker(&audit).await;
        let read = call(&mut b, "bob", "/global-read").await;
        assert_eq!(read["read"], Value::Null);
    });
}

#[test]
fn work_queued_by_one_user_runs_as_the_next() {
    run(async {
        let audit = Audit::default();
        let mut w = worker(&audit).await;
        call(&mut w, "alice", "/queue-push?tag=alice").await;
        let drained = call(&mut w, "bob", "/queue-drain").await;
        // The call happens in bob's request, so the host attaches bob: the
        // host attributes by the request in flight, and guest globals can
        // carry alice's intent into it.
        assert_eq!(ok_user(&drained["results"][0]), "bob", "{drained}");
        assert_eq!(user_of(&records(&audit, "alice:queued")[0]), Some("bob"));
    });
}

#[test]
fn interleaved_requests_on_two_workers() {
    run(async {
        let audit = Audit::default();
        let mut wa = worker(&audit).await;
        let mut wb = worker(&audit).await;

        let start = Instant::now();
        let (a, b) = tokio::join!(
            wa.serve("alice", get("/interleave?tag=alice", &[])),
            wb.serve("bob", get("/interleave?tag=bob", &[])),
        );
        let took = start.elapsed();
        let (a, b) = (a.json(), b.json());
        eprintln!("alice: {a}\nbob: {b}\nboth in {took:?}");

        for (body, user) in [(&a, "alice"), (&b, "bob")] {
            for key in ["before", "afterHostOp", "afterTimer"] {
                assert_eq!(ok_user(&body[key]), user, "{key}: {body}");
            }
            assert_eq!(body["slept"]["ok"]["attached"]["user"], user, "{body}");
        }
        // Each takes >= 250 ms (150 host op + 100 timer): together < 500 ms
        // means they were in flight at once.
        assert!(took < Duration::from_millis(450), "not concurrent: {took:?}");

        for r in audit.lock().unwrap().iter() {
            let tag = r.args["tag"].as_str().unwrap_or("");
            if let Some(user) = tag.split(':').next().filter(|u| *u == "alice" || *u == "bob") {
                assert_eq!(user_of(r), Some(user), "{r:?}");
            }
        }
    });
}

/// Past the runtime's inline budget (16 calls per turn) a call is handed to
/// the scheduler. The host must still attach the request the JS call was
/// made in. alice's answer leaves before her 40 calls settle, and bob's
/// request starts at once, with no yield in between.
#[test]
fn many_calls_past_the_inline_budget() {
    run(async {
        let audit = Audit::default();
        let mut w = worker(&audit).await;

        call(&mut w, "alice", "/flood?tag=alice&n=40").await;
        // One yield: the runtime's event loop takes alice's leftover calls
        // off its queue before bob's request starts.
        tokio::task::yield_now().await;
        call(&mut w, "bob", "/flood?tag=bob&n=40").await;
        tokio::task::yield_now().await;
        call(&mut w, "erin", "/whoami?tag=erin").await;
        // dave's request ends, then the engine is idle for a while.
        call(&mut w, "dave", "/flood?tag=dave&n=40").await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        call(&mut w, "carol", "/flood-await?tag=carol&n=40").await;

        for tag in ["alice", "bob", "dave", "carol"] {
            let rs = records(&audit, tag);
            let mut by: std::collections::BTreeMap<String, usize> = Default::default();
            for r in &rs {
                *by.entry(format!("{:?}", user_of(r))).or_default() += 1;
            }
            eprintln!("calls made in {tag}'s request: {} reached the host, attached: {by:?}", rs.len());
        }
        for tag in ["alice", "bob", "dave", "carol"] {
            let rs = records(&audit, tag);
            assert_eq!(rs.len(), 40, "{tag}: every call reached the host");
            for r in &rs {
                assert_eq!(user_of(r), Some(tag), "{tag}: {r:?}");
            }
        }
    });
}

#[test]
fn tampering_with_the_runtime_glue() {
    run(async {
        let audit = Audit::default();
        let mut w = worker(&audit).await;

        let tamper = call(&mut w, "alice", "/tamper").await;
        assert_eq!(ok_user(&tamper["whoami"]), "alice", "{tamper}");

        // bob's request on the tampered context: still bob, and alice's
        // timer is still cleared by the reset.
        let check = call(&mut w, "bob", "/tamper-check").await;
        assert_eq!(ok_user(&check["whoami"]), "bob", "{check}");
        assert_eq!(check["tamperTimerResult"], "not run", "{check}");
        tokio::time::sleep(Duration::from_millis(100)).await;
        let check = call(&mut w, "carol", "/tamper-check").await;
        assert_eq!(ok_user(&check["whoami"]), "carol", "{check}");
        assert_eq!(check["tamperTimerResult"], "not run", "{check}");

        let records = audit.lock().unwrap();
        for r in records.iter().filter(|r| r.args["tag"] == "tamper-timer") {
            panic!("alice's timer ran later: {r:?}");
        }
    });
}

/// The same, at scale and with the runtime's tasks on other threads: alice
/// makes `n` calls (none awaited) and answers; bob's request follows after
/// k yields. Every call alice made must reach the host attached to alice.
/// Without the runtime patch, on a multi-thread runtime, thousands were
/// attached to bob.
#[test]
fn calls_past_the_budget_current_thread_runtime() {
    calls_past_the_budget(false, "flood", 2000);
}

#[test]
fn calls_past_the_budget_multi_thread_runtime() {
    calls_past_the_budget(true, "flood", 2000);
}

/// The same with outbound `fetch`, which the runtime hands to the host only
/// from a scheduler task (no inline poll, patched or not).
#[test]
fn unawaited_fetches_current_thread_runtime() {
    calls_past_the_budget(false, "fetch-flood", 200);
}

#[test]
fn unawaited_fetches_multi_thread_runtime() {
    calls_past_the_budget(true, "fetch-flood", 200);
}

fn calls_past_the_budget(multi: bool, route: &str, n: usize) {
    let n: usize = std::env::var("FLOOD_N").ok().and_then(|v| v.parse().ok()).unwrap_or(n);
    let rt = if multi {
        tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().unwrap()
    } else {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    };
    let mut failures = vec![];
    tokio::task::LocalSet::new().block_on(&rt, async {
        for k in 0..4 {
            let audit = Audit::default();
            let mut w = worker(&audit).await;
            for round in 0..10 {
                let tag = format!("a{round}");
                call(&mut w, "alice", &format!("/{route}?tag={tag}&n={n}")).await;
                for _ in 0..k {
                    tokio::task::yield_now().await;
                }
                call(&mut w, "bob", "/whoami?tag=b").await;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            let mut by: std::collections::BTreeMap<String, usize> = Default::default();
            for r in audit.lock().unwrap().iter().filter(|r| r.args["tag"].as_str().is_some_and(|t| t.starts_with('a'))) {
                *by.entry(format!("{:?}", user_of(r))).or_default() += 1;
            }
            let line = format!("{route} multi={multi} k={k}: {} calls by alice, at the host: {by:?}", 10 * n);
            eprintln!("{line}");
            if by.len() != 1 || by.get("Some(\"alice\")") != Some(&(10 * n)) {
                failures.push(line);
            }
        }
    });
    assert!(failures.is_empty(), "calls not attached to alice:\n{}", failures.join("\n"));
}
