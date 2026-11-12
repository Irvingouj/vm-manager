//! A guest busy loop that never yields is stopped by the CPU limit, well
//! before the wall clock, on every platform the host builds for.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use openworkers_core::{
    DefaultOps, Event, HttpMethod, HttpRequest, OperationsHandle, RequestBody, RuntimeLimits,
    Script, TerminationReason,
};
use openworkers_runtime_v8::Worker;

const CODE: &str = "globalThis.default = { fetch(request) {
    if (new URL(request.url).pathname === '/spin') { while (true) {} }
    return new Response('ok');
} };";

fn get(path: &str) -> (Event, tokio::sync::oneshot::Receiver<openworkers_core::HttpResponse>) {
    Event::fetch(HttpRequest {
        method: HttpMethod::Get,
        url: format!("http://localhost{path}"),
        headers: HashMap::new(),
        body: RequestBody::None,
    })
}

#[tokio::test(flavor = "current_thread")]
async fn a_busy_loop_is_stopped_by_the_cpu_limit() {
    let local = tokio::task::LocalSet::new();

    local
        .run_until(async {
            let ops: OperationsHandle = Arc::new(DefaultOps);
            let limits = RuntimeLimits {
                max_cpu_time_ms: 50,
                max_wall_clock_time_ms: 10_000,
                ..RuntimeLimits::default()
            };
            let mut worker = Worker::new_with_ops(Script::new(CODE), Some(limits), ops)
                .await
                .unwrap();

            let (task, _rx) = get("/spin");
            let start = Instant::now();
            let outcome = worker.exec(task).await;
            let took = start.elapsed();
            eprintln!("/spin -> {outcome:?} after {took:?}");

            assert!(
                matches!(outcome, Err(TerminationReason::CpuTimeLimit)),
                "expected CpuTimeLimit, got {outcome:?} after {took:?}"
            );
            assert!(took < Duration::from_secs(2), "took {took:?}");

            // The worker serves the next request.
            let (task, rx) = get("/");
            worker.exec(task).await.unwrap();
            let body = rx.await.unwrap().body.collect().await.unwrap().unwrap();
            assert_eq!(&body[..], b"ok");
        })
        .await;
}
