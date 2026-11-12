//! The spike host over HTTP, and the cost measurements.
//!
//! ```text
//! idp-host serve <guest.js> [addr] [application]   serve the guest
//! idp-host bench <guest.js> <mode> [n]              time n requests
//! idp-host snapshot [path]                          write the V8 snapshot
//! ```
//!
//! `serve`: the User is the `x-vm-user` header (a stand-in for sign-in; a
//! request without exactly one is refused, and the header never reaches the
//! guest). The Application is the third argument (default `demo`), never
//! read from the request. One engine thread runs a pool of warm workers of
//! the Application; each worker serves one request at a time, and a request
//! that finds none free gets a new one (a worker per in-flight request).
//!
//! `bench` modes: `warm` (one worker, reused), `fresh-isolate` (a new
//! `Worker`, so a new isolate and context, per request), `pool-warm` and
//! `pool-fresh-context` (`execute_pinned`, contexts cached or never cached,
//! so a new context per request on a reused isolate).

use std::cell::RefCell;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use idp_host::{AppWorker, Audit, HostOps, USER_HEADER, get, limits, script};
use openworkers_core::{Event, HttpRequest, OperationsHandle, RequestBody};
use tokio::sync::{mpsc, oneshot};

struct Job {
    user: String,
    request: HttpRequest,
    answer: oneshot::Sender<idp_host::Served>,
}

fn spawn_engine(code: String, application: String) -> mpsc::Sender<Job> {
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
                let audit: Audit = Arc::default();
                let free: Rc<RefCell<Vec<AppWorker>>> = Rc::default();
                let limits = limits(env_ms("EC_CPU_MS", 50), env_ms("EC_WALL_MS", 10_000));

                match AppWorker::new(&application, &code, limits.clone(), audit.clone()).await {
                    Ok(worker) => {
                        free.borrow_mut().push(worker);
                        let _ = ready_tx.send(Ok(()));
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(format!("{e:?}")));
                        return;
                    }
                }

                while let Some(job) = rx.recv().await {
                    let free = free.clone();
                    let (code, application, audit, limits) =
                        (code.clone(), application.clone(), audit.clone(), limits.clone());

                    tokio::task::spawn_local(async move {
                        // A free warm worker, or a new one: never two
                        // requests on one worker.
                        let taken = free.borrow_mut().pop();
                        let mut worker = match taken {
                            Some(worker) => worker,
                            None => match AppWorker::new(&application, &code, limits, audit).await {
                                Ok(worker) => {
                                    eprintln!("new worker (all busy)");
                                    worker
                                }
                                Err(e) => {
                                    eprintln!("cannot build a worker: {e:?}");
                                    return;
                                }
                            },
                        };
                        let served = worker.serve(&job.user, job.request).await;
                        let healthy = served.outcome.is_ok();
                        let _ = job.answer.send(served);
                        if healthy {
                            free.borrow_mut().push(worker);
                        }
                    });
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

fn env_ms(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
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
    // The stand-in for sign-in: exactly one x-vm-user header.
    let users: Vec<_> = req.headers().get_all(USER_HEADER).iter().collect();
    let user = match users.as_slice() {
        [one] => match one.to_str() {
            Ok(user) if !user.is_empty() => user.to_string(),
            _ => return Ok(text(StatusCode::BAD_REQUEST, "bad user header\n".into())),
        },
        [] => return Ok(text(StatusCode::UNAUTHORIZED, "no user\n".into())),
        _ => return Ok(text(StatusCode::BAD_REQUEST, "more than one user header\n".into())),
    };

    let method = req.method().as_str().parse().unwrap_or_default();
    let host = req
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost")
        .to_string();
    let url = format!("http://{host}{}", req.uri());
    let headers = req
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

    let (answer, answer_rx) = oneshot::channel();
    let job = Job {
        user,
        request: HttpRequest {
            method,
            url: url.clone(),
            headers,
            body,
        },
        answer,
    };
    if engine.send(job).await.is_err() {
        return Ok(text(StatusCode::SERVICE_UNAVAILABLE, "engine gone\n".into()));
    }
    let Ok(served) = answer_rx.await else {
        return Ok(text(StatusCode::SERVICE_UNAVAILABLE, "engine gone\n".into()));
    };

    eprintln!(
        "{url} user={} app={} request={} -> {:?} {:?}",
        served.attached.user, served.attached.application, served.attached.request_id, served.status, served.outcome
    );

    Ok(match (served.status, served.body) {
        (Some(status), Some(body)) => Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body)))
            .unwrap(),
        _ => text(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("guest stopped: {:?}\n", served.outcome),
        ),
    })
}

fn serve(code: String, addr: SocketAddr, application: String) {
    let engine = spawn_engine(code, application.clone());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("http runtime");

    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap_or_else(|e| {
            eprintln!("cannot listen on {addr}: {e}");
            std::process::exit(1);
        });
        eprintln!("idp-host: application {application} on http://{addr}");

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

fn choose_snapshot() {
    if let Ok(path) = std::env::var("EC_SNAPSHOT") {
        match std::fs::read(&path) {
            Ok(bytes) => {
                let len = bytes.len();
                let set = openworkers_runtime_v8::platform::set_snapshot(Box::leak(bytes.into_boxed_slice()));
                eprintln!("snapshot: {len} bytes from EC_SNAPSHOT={path} (set: {set})");
            }
            Err(e) => eprintln!("snapshot: cannot read {path}: {e}"),
        }
    } else {
        let path = env!("RUNTIME_SNAPSHOT_PATH");
        let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        eprintln!("snapshot: compiled-in {path}, {len} bytes (0 = none)");
    }
}

fn stats(label: &str, mut times: Vec<Duration>) {
    times.sort();
    let n = times.len();
    let total: Duration = times.iter().sum();
    let pct = |p: f64| times[((n as f64 - 1.0) * p).round() as usize];
    println!(
        "{label}: n={n} mean={:?} p50={:?} p90={:?} p99={:?} max={:?}",
        total / n as u32,
        pct(0.5),
        pct(0.9),
        pct(0.99),
        times[n - 1]
    );
}

/// Times `n` requests of `/whoami` (the guest calls the host once) in `mode`.
fn bench(code: String, mode: &str, n: usize) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    let audit: Audit = Arc::default();
    let limits = limits(1_000, 10_000);

    local.block_on(&rt, async move {
        let users = ["alice", "bob"];
        let mut times = Vec::with_capacity(n);
        let check = |served: &idp_host::Served, user: &str| {
            let body = served.json();
            assert_eq!(body["whoami"]["ok"]["user"], user, "{body}");
        };

        match mode {
            "warm" => {
                let mut worker = AppWorker::new("demo", &code, limits, audit).await.unwrap();
                for i in 0..n {
                    let user = users[i % 2];
                    let start = Instant::now();
                    let served = worker.serve(user, get("/whoami", &[])).await;
                    times.push(start.elapsed());
                    check(&served, user);
                }
            }
            "fresh-isolate" => {
                for i in 0..n {
                    let user = users[i % 2];
                    let start = Instant::now();
                    let mut worker = AppWorker::new("demo", &code, limits.clone(), audit.clone())
                        .await
                        .unwrap();
                    let served = worker.serve(user, get("/whoami", &[])).await;
                    drop(worker);
                    times.push(start.elapsed());
                    check(&served, user);
                }
            }
            "pool-warm" | "pool-fresh-context" => {
                let cache = if mode == "pool-warm" { 4 } else { 0 };
                openworkers_runtime_v8::init_pinned_pool(openworkers_runtime_v8::PinnedPoolConfig {
                    max_per_thread: 4,
                    max_per_owner: None,
                    max_concurrent_per_isolate: 1,
                    max_cached_contexts: cache,
                    overcommit: true,
                    max_context_reuses: u32::MAX,
                    limits,
                });
                for i in 0..n {
                    let user = users[i % 2];
                    let start = Instant::now();
                    let ops = HostOps::new("demo", audit.clone());
                    ops.begin(user);
                    let (event, rx) = Event::fetch(get("/whoami", &[]));
                    let user_owned = user.to_string();
                    let handle: OperationsHandle = ops.clone();
                    let result = openworkers_runtime_v8::execute_pinned(
                        openworkers_runtime_v8::PinnedExecuteRequest {
                            owner_id: "demo".into(),
                            worker_id: "demo".into(),
                            version: 1,
                            script: script(&code),
                            ops: handle,
                            task: event,
                            on_warm_hit: Some(Box::new(move |cached: &OperationsHandle| {
                                let cached = cached.as_any().downcast_ref::<HostOps>().unwrap();
                                cached.begin(&user_owned);
                            })),
                            env_updated_at: None,
                            abort: None,
                            on_report: None,
                        },
                    )
                    .await;
                    let response = rx.await.unwrap();
                    let body = response.body.collect().await.unwrap().unwrap();
                    times.push(start.elapsed());
                    result.unwrap();
                    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(body["whoami"]["ok"]["user"], user, "{body}");
                }
            }
            other => panic!("unknown mode {other}"),
        }

        stats(mode, times);
    });
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let read = |path: &str| {
        std::fs::read_to_string(path).unwrap_or_else(|e| {
            eprintln!("cannot read {path}: {e}");
            std::process::exit(1);
        })
    };

    match args.get(1).map(String::as_str) {
        Some("snapshot") => {
            let path = args
                .get(2)
                .cloned()
                .unwrap_or_else(|| env!("RUNTIME_SNAPSHOT_PATH").to_string());
            let snapshot = openworkers_runtime_v8::snapshot::create_runtime_snapshot()
                .unwrap_or_else(|e| {
                    eprintln!("snapshot failed: {e}");
                    std::process::exit(1);
                });
            std::fs::write(&path, &snapshot.output).expect("write snapshot");
            eprintln!("snapshot: {} bytes to {path}", snapshot.output.len());
        }
        Some("serve") if args.len() >= 3 => {
            choose_snapshot();
            let addr: SocketAddr = args
                .get(3)
                .map(String::as_str)
                .unwrap_or("127.0.0.1:8787")
                .parse()
                .expect("listen address");
            let application = args.get(4).cloned().unwrap_or_else(|| "demo".into());
            serve(read(&args[2]), addr, application);
        }
        Some("bench") if args.len() >= 4 => {
            choose_snapshot();
            let n = args.get(4).and_then(|n| n.parse().ok()).unwrap_or(200);
            bench(read(&args[2]), &args[3], n);
        }
        _ => {
            eprintln!(
                "usage: idp-host serve <guest.js> [addr] [application] | bench <guest.js> <mode> [n] | snapshot [path]"
            );
            std::process::exit(2);
        }
    }
}
