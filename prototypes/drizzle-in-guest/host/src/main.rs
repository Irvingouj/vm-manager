//! Spike host: a guest bundled with Drizzle reaches Postgres only through
//! host operations (see `db.rs`). Built on the identity spike's host, cut
//! down: no identity header handling, one fixed User.
//!
//! ```text
//! dig-host serve <guest.js> [addr]          serve the guest over HTTP
//! dig-host bench-start <guest.js> [n]        time building a worker (new isolate, script run)
//! dig-host bench-req <guest.js> <path> [n]   time n requests on one warm worker
//! dig-host migrate <conninfo> <folder>       apply drizzle-kit migrations, no Node
//! dig-host snapshot [path]                   write the V8 runtime snapshot
//! ```
//!
//! Resources come from the environment: `DIG_DB_<NAME>=<conninfo>` gives the
//! guest the Resource `<name>` (lower-cased). `DIG_STRICT=1` refuses every
//! statement but one SELECT/INSERT/UPDATE/DELETE. `DIG_CAPTURE=<file>` writes
//! every statement the host saw, as JSON lines, when the server stops (or
//! on `GET /__capture`).

mod db;
mod migrate;
mod pg;

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use openworkers_core::{
    BindingInfo, Event, HttpRequest, InvokeResult, OpFuture, OperationsHandle, OperationsHandler, RequestBody,
    RuntimeLimits, Script, TerminationReason,
};
use openworkers_runtime_v8::Worker;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

struct HostOps {
    current: Mutex<Option<u64>>,
    db: Arc<db::Db>,
}

impl OperationsHandler for HostOps {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn handle_binding_invoke(&self, binding: &str, name: &str, args: Value) -> OpFuture<'_, InvokeResult> {
        // Read in the guest's call (identity spike patch), not when polled later.
        let current = *self.current.lock().unwrap();
        let binding = binding.to_string();
        let name = name.to_string();
        let db = self.db.clone();
        Box::pin(async move {
            if binding != "HOST" {
                return InvokeResult::Error(format!("no binding {binding}"));
            }
            let Some(request_id) = current else {
                return InvokeResult::Error("no request in flight".into());
            };
            let result = match name.as_str() {
                "sql" => db.sql(request_id, args).await,
                "tx.begin" => db.begin(request_id, args).await,
                "tx.commit" => db.end(request_id, args, true).await,
                "tx.rollback" => db.end(request_id, args, false).await,
                "user" => Ok(json!({ "user": "alice", "request_id": request_id })),
                other => Err(format!("no host function HOST.{other}")),
            };
            match result {
                Ok(v) => InvokeResult::Value(v),
                Err(e) => InvokeResult::Error(e),
            }
        })
    }
}

struct AppWorker {
    worker: Worker,
    ops: Arc<HostOps>,
}

struct Served {
    status: Option<u16>,
    body: Option<Vec<u8>>,
    outcome: Result<(), TerminationReason>,
    abandoned_txs: usize,
}

fn script(code: &str) -> Script {
    Script::with_bindings(code.to_string(), None, vec![BindingInfo::invoke("HOST")])
}

fn limits(cpu_ms: u64, wall_ms: u64) -> RuntimeLimits {
    RuntimeLimits { max_cpu_time_ms: cpu_ms, max_wall_clock_time_ms: wall_ms, ..RuntimeLimits::default() }
}

impl AppWorker {
    async fn new(code: &str, limits: RuntimeLimits, db: Arc<db::Db>) -> Result<Self, TerminationReason> {
        let ops = Arc::new(HostOps { current: Mutex::new(None), db });
        let handle: OperationsHandle = ops.clone();
        let worker = Worker::new_with_ops(script(code), Some(limits), handle).await?;
        Ok(Self { worker, ops })
    }

    async fn serve(&mut self, request: HttpRequest) -> Served {
        let request_id = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
        *self.ops.current.lock().unwrap() = Some(request_id);
        let (event, response_rx) = Event::fetch(request);
        let outcome = self.worker.exec(event).await;
        let (status, body) = match response_rx.await {
            Ok(response) => {
                let status = response.status;
                let body = response.body.collect().await.ok().flatten();
                (Some(status), body.map(|b| b.to_vec()))
            }
            Err(_) => (None, None),
        };
        // The request is over: nothing it began may stay open.
        let abandoned_txs = self.ops.db.abandon(request_id).await;
        *self.ops.current.lock().unwrap() = None;
        Served { status, body, outcome, abandoned_txs }
    }
}

fn get(path: &str) -> HttpRequest {
    HttpRequest {
        method: openworkers_core::HttpMethod::Get,
        url: format!("http://localhost{path}"),
        headers: HashMap::new(),
        body: RequestBody::None,
    }
}

fn env_ms(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

async fn resources() -> db::Resources {
    let mut map = HashMap::new();
    for (k, v) in std::env::vars() {
        if let Some(name) = k.strip_prefix("DIG_DB_") {
            let r = db::Resource::new(&v).await.unwrap_or_else(|e| {
                eprintln!("Resource {name}: {e}");
                std::process::exit(1);
            });
            map.insert(name.to_lowercase(), Arc::new(r));
        }
    }
    Arc::new(map)
}

fn new_db(resources: db::Resources) -> Arc<db::Db> {
    let strict = std::env::var("DIG_STRICT").map(|v| v == "1").unwrap_or(false);
    Arc::new(db::Db::new(resources, Arc::default(), strict))
}

fn write_capture(db: &db::Db) {
    if let Ok(path) = std::env::var("DIG_CAPTURE") {
        let lines: Vec<String> = db.capture.lock().unwrap().iter().map(|v| v.to_string()).collect();
        let _ = std::fs::write(&path, lines.join("\n") + "\n");
    }
}

struct Job {
    request: HttpRequest,
    answer: oneshot::Sender<Served>,
}

/// One engine thread with a pool of warm workers, as in the identity spike.
fn spawn_engine(code: String) -> (mpsc::Sender<Job>, Arc<db::Db>) {
    let (tx, mut rx) = mpsc::channel::<Job>(64);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<Arc<db::Db>, String>>();

    std::thread::Builder::new()
        .name("engine".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let db = new_db(resources().await);
                let free: std::rc::Rc<std::cell::RefCell<Vec<AppWorker>>> = Default::default();
                let limits = limits(env_ms("EC_CPU_MS", 500), env_ms("EC_WALL_MS", 10_000));
                let start = Instant::now();
                match AppWorker::new(&code, limits.clone(), db.clone()).await {
                    Ok(w) => {
                        eprintln!("first worker ready in {:?}", start.elapsed());
                        free.borrow_mut().push(w);
                        let _ = ready_tx.send(Ok(db.clone()));
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(format!("{e:?}")));
                        return;
                    }
                }
                while let Some(job) = rx.recv().await {
                    let (free, code, limits, db) = (free.clone(), code.clone(), limits.clone(), db.clone());
                    tokio::task::spawn_local(async move {
                        let taken = free.borrow_mut().pop();
                        let mut worker = match taken {
                            Some(w) => w,
                            None => match AppWorker::new(&code, limits, db).await {
                                Ok(w) => w,
                                Err(e) => {
                                    eprintln!("cannot build a worker: {e:?}");
                                    return;
                                }
                            },
                        };
                        let served = worker.serve(job.request).await;
                        let healthy = served.outcome.is_ok();
                        let _ = job.answer.send(served);
                        if healthy {
                            free.borrow_mut().push(worker);
                        }
                    });
                }
            });
        })
        .unwrap();

    match ready_rx.recv() {
        Ok(Ok(db)) => (tx, db),
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

async fn handle(
    engine: mpsc::Sender<Job>,
    db: Arc<db::Db>,
    req: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let respond = |status: StatusCode, body: String| {
        Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body)))
            .unwrap()
    };
    if req.uri().path() == "/__capture" {
        write_capture(&db);
        let all: Vec<Value> = db.capture.lock().unwrap().clone();
        return Ok(respond(StatusCode::OK, Value::Array(all).to_string()));
    }
    let url = format!("http://localhost{}", req.uri());
    let (answer, answer_rx) = oneshot::channel();
    let job = Job { request: get(req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/")), answer };
    if engine.send(job).await.is_err() {
        return Ok(respond(StatusCode::SERVICE_UNAVAILABLE, "\"engine gone\"".into()));
    }
    let Ok(served) = answer_rx.await else {
        return Ok(respond(StatusCode::SERVICE_UNAVAILABLE, "\"engine gone\"".into()));
    };
    eprintln!("{url} -> {:?} {:?} abandoned_txs={}", served.status, served.outcome, served.abandoned_txs);
    Ok(match (served.status, served.body) {
        (Some(status), Some(body)) => Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .header("x-abandoned-txs", served.abandoned_txs.to_string())
            .body(Full::new(Bytes::from(body)))
            .unwrap(),
        _ => respond(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "guest_stopped": format!("{:?}", served.outcome) }).to_string(),
        ),
    })
}

fn serve(code: String, addr: SocketAddr) {
    let (engine, db) = spawn_engine(code);
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap_or_else(|e| {
            eprintln!("cannot listen on {addr}: {e}");
            std::process::exit(1);
        });
        eprintln!("dig-host on http://{addr}");
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let (engine, db) = (engine.clone(), db.clone());
            tokio::spawn(async move {
                let service = service_fn(move |req| handle(engine.clone(), db.clone(), req));
                let _ = http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
            });
        }
    });
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

fn local<F: std::future::Future<Output = ()>>(f: F) {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    tokio::task::LocalSet::new().block_on(&rt, f);
}

fn bench_start(code: String, n: usize) {
    local(async move {
        let db = new_db(resources().await);
        let mut ready = Vec::new();
        let mut first = Vec::new();
        for _ in 0..n {
            let start = Instant::now();
            let mut w = AppWorker::new(&code, limits(5_000, 10_000), db.clone()).await.unwrap();
            ready.push(start.elapsed());
            let start = Instant::now();
            let served = w.serve(get("/noop")).await;
            first.push(start.elapsed());
            assert_eq!(served.status, Some(200), "{:?}", served.outcome);
        }
        stats("worker ready (new isolate + script run)", ready);
        stats("first request /noop on the new worker", first);
    });
}

fn bench_req(code: String, path: String, n: usize) {
    local(async move {
        let db = new_db(resources().await);
        let mut w = AppWorker::new(&code, limits(5_000, 10_000), db.clone()).await.unwrap();
        for _ in 0..20 {
            w.serve(get(&path)).await;
        }
        let mut times = Vec::new();
        for _ in 0..n {
            let start = Instant::now();
            let served = w.serve(get(&path)).await;
            times.push(start.elapsed());
            assert_eq!(served.status, Some(200), "{:?} {:?}", served.outcome, served.body.map(String::from_utf8));
        }
        stats(&format!("warm {path}"), times);
    });
}

fn choose_snapshot() {
    let path = env!("RUNTIME_SNAPSHOT_PATH");
    let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    eprintln!("snapshot: compiled-in {path}, {len} bytes (0 = none)");
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
            let path = args.get(2).cloned().unwrap_or_else(|| env!("RUNTIME_SNAPSHOT_PATH").to_string());
            let snapshot = openworkers_runtime_v8::snapshot::create_runtime_snapshot().expect("snapshot");
            std::fs::write(&path, &snapshot.output).expect("write snapshot");
            eprintln!("snapshot: {} bytes to {path}", snapshot.output.len());
        }
        Some("serve") if args.len() >= 3 => {
            choose_snapshot();
            let addr: SocketAddr = args.get(3).map(String::as_str).unwrap_or("127.0.0.1:8792").parse().unwrap();
            serve(read(&args[2]), addr);
        }
        Some("bench-start") if args.len() >= 3 => {
            choose_snapshot();
            bench_start(read(&args[2]), args.get(3).and_then(|n| n.parse().ok()).unwrap_or(30));
        }
        Some("bench-req") if args.len() >= 4 => {
            choose_snapshot();
            bench_req(read(&args[2]), args[3].clone(), args.get(4).and_then(|n| n.parse().ok()).unwrap_or(300));
        }
        Some("migrate") if args.len() >= 4 => {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            if let Err(e) = rt.block_on(migrate::apply(&args[2], std::path::Path::new(&args[3]))) {
                eprintln!("migrate failed: {e}");
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!("usage: see the header of host/src/main.rs");
            std::process::exit(2);
        }
    }
}
