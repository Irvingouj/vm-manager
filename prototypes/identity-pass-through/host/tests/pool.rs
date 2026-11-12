//! The same seam through the runtime's isolate pool (`execute_pinned`), the
//! production path upstream: warm contexts are cached and reused, and an
//! isolate can serve two requests at once (two contexts, turns on the V8
//! Locker).
//!
//! The pool config is process-wide (a OnceLock), so every test here shares
//! it: one isolate per thread, two requests at once per isolate.

use std::future::Future;
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use idp_host::{Audit, HostOps, get, limits, script};
use openworkers_core::{Event, OperationsHandle};
use openworkers_runtime_v8::{PinnedExecuteRequest, PinnedPoolConfig, execute_pinned, get_local_pool_stats, init_pinned_pool};
use serde_json::Value;

const GUEST: &str = include_str!("../../guest/attacks.js");

fn run<F: Future<Output = ()>>(f: F) {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        init_pinned_pool(PinnedPoolConfig {
            max_per_thread: 1,
            max_per_owner: None,
            max_concurrent_per_isolate: 2,
            max_cached_contexts: 8,
            overcommit: false,
            max_context_reuses: 1_000,
            limits: limits(1_000, 10_000),
        });
    });
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&rt, f);
}

/// How the host hands identity to a pooled context.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Build a new ops handle with the request's User and pass it in the
    /// request. On a warm hit the pool uses the cached context's ops instead.
    NaivePerRequestOps,
    /// Also set the User on the cached ops in `on_warm_hit`, and clear the
    /// slot of whichever ops served once the event is over.
    SlotOnTheContextOps,
}

struct Pinned {
    body: Value,
    warm: bool,
}

async fn pinned(audit: &Audit, worker_id: &str, user: &str, path: &str, mode: Mode) -> Pinned {
    let ops = HostOps::new("demo", audit.clone());
    ops.begin(user);

    // Which ops served: the new one (cold) or the cached one (warm).
    let served_by: Arc<Mutex<Option<OperationsHandle>>> = Arc::default();
    let on_warm_hit: Option<openworkers_runtime_v8::WarmHitCallback> = {
        let served_by = served_by.clone();
        let user = user.to_string();
        Some(Box::new(move |cached: &OperationsHandle| {
            if mode == Mode::SlotOnTheContextOps {
                let host = cached.as_any().downcast_ref::<HostOps>().expect("our ops");
                host.begin(&user);
            }
            *served_by.lock().unwrap() = Some(cached.clone());
        }))
    };

    let (event, rx) = Event::fetch(get(path, &[]));
    let handle: OperationsHandle = ops.clone();
    let result = execute_pinned(PinnedExecuteRequest {
        owner_id: "demo".into(),
        worker_id: worker_id.into(),
        version: 1,
        script: script(GUEST),
        ops: handle,
        task: event,
        on_warm_hit,
        env_updated_at: None,
        abort: None,
        on_report: None,
    })
    .await;

    let cached = served_by.lock().unwrap().take();
    let warm = cached.is_some();
    if mode == Mode::SlotOnTheContextOps {
        ops.end();
        if let Some(cached) = cached {
            cached.as_any().downcast_ref::<HostOps>().unwrap().end();
        }
    }

    result.unwrap_or_else(|e| panic!("{path}: {e:?}"));
    let response = rx.await.expect("a response");
    let body = response.body.collect().await.unwrap().unwrap_or_default();
    let body: Value = serde_json::from_slice(&body).unwrap();
    eprintln!("{user} {path} warm={warm} -> {body}");
    Pinned { body, warm }
}

#[test]
fn naive_per_request_ops_are_ignored_on_a_warm_hit() {
    run(async {
        let audit = Audit::default();
        let a = pinned(&audit, "naive", "alice", "/whoami", Mode::NaivePerRequestOps).await;
        let b = pinned(&audit, "naive", "bob", "/whoami", Mode::NaivePerRequestOps).await;
        assert!(!a.warm && b.warm);
        assert_eq!(a.body["whoami"]["ok"]["user"], "alice");
        // bob's request ran with alice's identity: the pool kept the ops of
        // the request that built the context (pool.rs, warm hit path).
        assert_eq!(b.body["whoami"]["ok"]["user"], "alice", "the footgun is gone? {}", b.body);
    });
}

#[test]
fn slot_on_the_context_ops_follows_each_request() {
    run(async {
        let audit = Audit::default();
        for (i, user) in ["alice", "bob", "alice", "carol", "bob"].iter().enumerate() {
            let p = pinned(&audit, "slot", user, "/whoami", Mode::SlotOnTheContextOps).await;
            assert_eq!(p.warm, i > 0);
            assert_eq!(p.body["whoami"]["ok"]["user"], *user, "{}", p.body);
        }
    });
}

#[test]
fn two_requests_at_once_on_one_isolate() {
    run(async {
        let audit = Audit::default();
        let start = Instant::now();
        let (a, b) = tokio::join!(
            pinned(&audit, "interleave", "alice", "/interleave?tag=alice", Mode::SlotOnTheContextOps),
            pinned(&audit, "interleave", "bob", "/interleave?tag=bob", Mode::SlotOnTheContextOps),
        );
        let took = start.elapsed();
        eprintln!("both in {took:?}; local pool: {:?}", get_local_pool_stats());

        // One isolate on this thread served both.
        assert_eq!(get_local_pool_stats().unwrap().total, 1);
        assert!(took < Duration::from_millis(495), "not concurrent: {took:?}");
        for (p, user) in [(&a, "alice"), (&b, "bob")] {
            for key in ["before", "afterHostOp", "afterTimer"] {
                assert_eq!(p.body[key]["ok"]["user"], user, "{key}: {}", p.body);
            }
        }

        // Then both contexts are cached; four more pairs reuse them.
        for round in 0..4 {
            let (a, b) = tokio::join!(
                pinned(&audit, "interleave", "bob", "/interleave?tag=bob", Mode::SlotOnTheContextOps),
                pinned(&audit, "interleave", "alice", "/interleave?tag=alice", Mode::SlotOnTheContextOps),
            );
            assert!(a.warm && b.warm, "round {round}");
            for (p, user) in [(&a, "bob"), (&b, "alice")] {
                for key in ["before", "afterHostOp", "afterTimer"] {
                    assert_eq!(p.body[key]["ok"]["user"], user, "{key}: {}", p.body);
                }
            }
        }
    });
}

/// Keying the warm context by Application *and* User: a User never gets a
/// context another User's request ran in, so globals do not cross Users.
#[test]
fn a_context_per_application_and_user() {
    run(async {
        let audit = Audit::default();
        let key = |user: &str| format!("demo:{user}");
        pinned(&audit, &key("alice"), "alice", "/global-write?v=alice-secret", Mode::SlotOnTheContextOps).await;
        let bob = pinned(&audit, &key("bob"), "bob", "/global-read", Mode::SlotOnTheContextOps).await;
        assert_eq!(bob.body["read"], Value::Null);
        let alice = pinned(&audit, &key("alice"), "alice", "/global-read", Mode::SlotOnTheContextOps).await;
        assert!(alice.warm);
        assert_eq!(alice.body["read"], "alice-secret");
        // Both contexts live on the one isolate of this thread.
        assert_eq!(get_local_pool_stats().unwrap().total, 1);
    });
}
