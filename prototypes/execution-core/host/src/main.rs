//! A minimal host on the OpenWorkers execution core (openworkers-core +
//! openworkers-runtime-v8), in the style of openworkers-embed but much
//! smaller: one guest script, one engine thread, one warm `Worker`, one invoke
//! binding the guest calls back into.
//!
//! ```text
//! ec-host serve <guest.js> [addr]   serve the guest's fetch handler over HTTP
//! ec-host snapshot [path]           write the V8 runtime snapshot
//! ```
//!
//! The listen address defaults to 127.0.0.1:8787. Environment knobs:
//! `EC_CPU_MS` (CPU limit per request, default 50), `EC_WALL_MS` (wall
//! clock per request, default 10000), `EC_SNAPSHOT` (path of the V8 snapshot
//! to load at start, instead of the path compiled into the runtime).

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use openworkers_core::{
    BindingInfo, Event, HttpRequest, HttpResponse, InvokeResult, OpFuture, OperationsHandle,
    OperationsHandler, RequestBody, RuntimeLimits, Script, TerminationReason,
};
use openworkers_runtime_v8::Worker;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

/// The host side of the guest's `env.HOST` binding.
struct HostOps;

impl OperationsHandler for HostOps {
    fn handle_binding_invoke(
        &self,
        binding: &str,
        name: &str,
        args: Value,
    ) -> OpFuture<'_, InvokeResult> {
        // The binding name is a free string from the guest: check it.
        let answer = match (binding, name) {
            ("HOST", "greet") => {
                let who = args["name"].as_str().unwrap_or("nobody");
                InvokeResult::Value(json!({
                    "message": format!("hello, {who}, from the host"),
                    "os": std::env::consts::OS,
                    "arch": std::env::consts::ARCH,
                }))
            }
            (binding, name) => InvokeResult::Error(format!("no host function {binding}.{name}")),
        };

        Box::pin(async move { answer })
    }
}

/// One request for the engine thread: the request, and where to send the
/// response receiver and the outcome of the event.
struct Job {
    request: HttpRequest,
    response: oneshot::Sender<oneshot::Receiver<HttpResponse>>,
    outcome: oneshot::Sender<Result<(), TerminationReason>>,
}

fn limits() -> RuntimeLimits {
    let env_ms = |name: &str, default: u64| {
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };

    RuntimeLimits {
        max_cpu_time_ms: env_ms("EC_CPU_MS", 50),
        max_wall_clock_time_ms: env_ms("EC_WALL_MS", 10_000),
        ..RuntimeLimits::default()
    }
}

/// The engine thread: V8 work is `!Send`, so it runs on a current-thread
/// runtime inside a `LocalSet`, one event at a time on one warm `Worker`.
fn spawn_engine(code: String) -> mpsc::Sender<Job> {
    let (tx, mut rx) = mpsc::channel::<Job>(64);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    std::thread::Builder::new()
        .name("engine".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("engine runtime");
            let local = tokio::task::LocalSet::new();

            local.block_on(&rt, async move {
                let ops: OperationsHandle = Arc::new(HostOps);
                let script = Script::with_bindings(code, None, vec![BindingInfo::invoke("HOST")]);
                let started = Instant::now();

                let mut worker = match Worker::new_with_ops(script, Some(limits()), ops).await {
                    Ok(worker) => {
                        eprintln!("worker ready in {:?}", started.elapsed());
                        let _ = ready_tx.send(Ok(()));
                        worker
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(format!("{e:?}")));
                        return;
                    }
                };

                while let Some(job) = rx.recv().await {
                    let (event, response_rx) = Event::fetch(job.request);
                    let _ = job.response.send(response_rx);
                    let outcome = worker.exec(event).await;
                    let _ = job.outcome.send(outcome);
                }
            });
        })
        .expect("engine thread");

    match ready_rx.recv() {
        Ok(Ok(())) => tx,
        Ok(Err(e)) => {
            eprintln!("the guest failed to start: {e}");
            std::process::exit(1);
        }
        Err(_) => {
            eprintln!("the engine thread died while starting");
            std::process::exit(1);
        }
    }
}

fn text(status: StatusCode, body: String) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from(body)))
        .unwrap()
}

async fn handle(
    engine: mpsc::Sender<Job>,
    req: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let started = Instant::now();
    let method_name = req.method().as_str().to_string();
    let method = method_name.parse().unwrap_or_default();
    let host = req
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost")
        .to_string();
    let url = format!("http://{host}{}", req.uri());
    let headers: HashMap<String, String> = req
        .headers()
        .iter()
        .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
        .collect();
    let body = match req.into_body().collect().await {
        Ok(b) => b.to_bytes(),
        Err(e) => return Ok(text(StatusCode::BAD_REQUEST, format!("bad body: {e}\n"))),
    };
    let body = if body.is_empty() {
        RequestBody::None
    } else {
        RequestBody::Bytes(body)
    };

    let (response_tx, response_rx) = oneshot::channel();
    let (outcome_tx, outcome_rx) = oneshot::channel();
    let job = Job {
        request: HttpRequest {
            method,
            url: url.clone(),
            headers,
            body,
        },
        response: response_tx,
        outcome: outcome_tx,
    };

    if engine.send(job).await.is_err() {
        return Ok(text(StatusCode::SERVICE_UNAVAILABLE, "engine gone\n".into()));
    }

    let Ok(response_rx) = response_rx.await else {
        return Ok(text(StatusCode::SERVICE_UNAVAILABLE, "engine gone\n".into()));
    };

    let answer = match response_rx.await {
        Ok(response) => match response.body.collect().await {
            Ok(body) => {
                let mut out = Response::builder().status(response.status);
                for (k, v) in &response.headers {
                    out = out.header(k, v);
                }
                out.body(Full::new(body.unwrap_or_default()))
                    .unwrap_or_else(|e| text(StatusCode::BAD_GATEWAY, format!("{e}\n")))
            }
            Err(e) => text(StatusCode::BAD_GATEWAY, format!("body failed: {e}\n")),
        },
        // No response: the event ended without one. Its outcome says why.
        Err(_) => {
            let reason = match outcome_rx.await {
                Ok(Err(reason)) => format!("{reason:?}"),
                Ok(Ok(())) => "no response".into(),
                Err(_) => "engine gone".into(),
            };
            let status = match reason.as_str() {
                "CpuTimeLimit" | "WallClockTimeout" | "MemoryLimit" => {
                    StatusCode::SERVICE_UNAVAILABLE
                }
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            text(status, format!("guest stopped: {reason}\n"))
        }
    };

    eprintln!(
        "{} {} -> {} in {:?}",
        method_name,
        url,
        answer.status(),
        started.elapsed()
    );
    Ok(answer)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(String::as_str) {
        Some("snapshot") => {
            let path = args
                .get(2)
                .cloned()
                .unwrap_or_else(|| env!("RUNTIME_SNAPSHOT_PATH").to_string());
            let started = Instant::now();
            let snapshot = openworkers_runtime_v8::snapshot::create_runtime_snapshot()
                .unwrap_or_else(|e| {
                    eprintln!("snapshot failed: {e}");
                    std::process::exit(1);
                });
            std::fs::write(&path, &snapshot.output).unwrap_or_else(|e| {
                eprintln!("cannot write {path}: {e}");
                std::process::exit(1);
            });
            eprintln!(
                "snapshot: {} bytes to {path} in {:?}",
                snapshot.output.len(),
                started.elapsed()
            );
        }
        Some("serve") if args.len() >= 3 => {
            let code = std::fs::read_to_string(&args[2]).unwrap_or_else(|e| {
                eprintln!("cannot read {}: {e}", args[2]);
                std::process::exit(1);
            });
            let addr: SocketAddr = args
                .get(3)
                .map(String::as_str)
                .unwrap_or("127.0.0.1:8787")
                .parse()
                .expect("listen address");
            serve(code, addr);
        }
        _ => {
            eprintln!("usage: ec-host serve <guest.js> [addr] | ec-host snapshot [path]");
            std::process::exit(2);
        }
    }
}

/// `EC_SNAPSHOT=<path>` gives the runtime its snapshot at run time (patched
/// runtime API); otherwise the runtime reads the compile-time path.
fn choose_snapshot() {
    match std::env::var("EC_SNAPSHOT") {
        Ok(path) => match std::fs::read(&path) {
            Ok(bytes) => {
                let len = bytes.len();
                let set = openworkers_runtime_v8::platform::set_snapshot(Box::leak(
                    bytes.into_boxed_slice(),
                ));
                eprintln!("snapshot: {len} bytes from EC_SNAPSHOT={path} (set: {set})");
            }
            Err(e) => eprintln!("snapshot: cannot read EC_SNAPSHOT={path}: {e}; none used"),
        },
        Err(_) => {
            let path = env!("RUNTIME_SNAPSHOT_PATH");
            match std::fs::metadata(path) {
                Ok(m) if m.len() > 0 => eprintln!("snapshot: {} bytes at compiled-in {path}", m.len()),
                Ok(_) => eprintln!("snapshot: compiled-in {path} is empty; none used"),
                Err(e) => eprintln!("snapshot: compiled-in {path}: {e}; none used"),
            }
        }
    }
}

fn serve(code: String, addr: SocketAddr) {
    eprintln!(
        "ec-host on {} {}; snapshot path compiled in: {}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        env!("RUNTIME_SNAPSHOT_PATH")
    );
    choose_snapshot();
    let engine = spawn_engine(code);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("http runtime");

    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .unwrap_or_else(|e| {
                eprintln!("cannot listen on {addr}: {e}");
                std::process::exit(1);
            });
        eprintln!("listening on http://{addr}");

        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let engine = engine.clone();
            tokio::spawn(async move {
                let service = service_fn(move |req| handle(engine.clone(), req));
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
}
