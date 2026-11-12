//! Spike: the host attaches the User and the Application to every operation
//! a guest calls, without the guest supplying either.
//!
//! - The Application is host configuration: the guest a worker was built
//!   for. It is fixed in [`HostOps`] when the worker is built.
//! - The User is a stand-in header ([`USER_HEADER`]) the host reads and
//!   strips before the guest sees the request. Real sign-in is out of scope.
//! - [`AppWorker::serve`] puts User + Application + a request id in the
//!   worker's slot ([`HostOps::current`]) before the event runs, and clears
//!   it when the event (answer, body and waitUntil work) is over.
//! - [`HostOps::handle_binding_invoke`] reads the slot *synchronously*, when
//!   the runtime enters it. With the runtime patch in
//!   `runtime/bindings/fetch.rs` that is always inside the guest's JS call,
//!   so the identity is the one of the request in flight at call time.
//!
//! The invariant it all rests on: one worker (one V8 context, one ops
//! handle) runs at most one event at a time, and guest JS runs only inside
//! an event. See the tests in `tests/`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use openworkers_core::{
    BindingInfo, Event, HttpRequest, InvokeResult, OpFuture, OperationsHandle, OperationsHandler,
    RuntimeLimits, Script, TerminationReason,
};
use openworkers_runtime_v8::Worker;
use serde::Serialize;
use serde_json::{Value, json};

/// The stand-in for sign-in: the host reads this header and strips it.
pub const USER_HEADER: &str = "x-vm-user";

/// The one binding the host gives the guest.
pub const BINDING: &str = "HOST";

/// What the host attaches to an operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Attached {
    pub user: String,
    pub application: String,
    pub request_id: u64,
}

/// One operation as the host saw it: what it attached, and what the guest
/// sent (which the host never uses for identity).
#[derive(Clone, Debug)]
pub struct AuditRecord {
    pub attached: Option<Attached>,
    pub binding: String,
    pub name: String,
    pub args: Value,
}

pub type Audit = Arc<Mutex<Vec<AuditRecord>>>;

static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

pub fn next_request_id() -> u64 {
    NEXT_REQUEST.fetch_add(1, Ordering::Relaxed)
}

/// The host side of `env.HOST`, one per worker (per V8 context).
pub struct HostOps {
    /// From host configuration: which guest this worker runs.
    application: String,
    /// The request in flight on this worker, if any. Set and cleared by the
    /// host only.
    current: Mutex<Option<Attached>>,
    audit: Audit,
}

impl HostOps {
    pub fn new(application: &str, audit: Audit) -> Arc<Self> {
        Arc::new(Self {
            application: application.to_string(),
            current: Mutex::new(None),
            audit,
        })
    }

    pub fn application(&self) -> &str {
        &self.application
    }

    /// Attach a User to this worker for one request; answers what it attached.
    pub fn begin(&self, user: &str) -> Attached {
        let attached = Attached {
            user: user.to_string(),
            application: self.application.clone(),
            request_id: next_request_id(),
        };
        *self.current.lock().unwrap() = Some(attached.clone());
        attached
    }

    pub fn end(&self) {
        *self.current.lock().unwrap() = None;
    }

    pub fn current(&self) -> Option<Attached> {
        self.current.lock().unwrap().clone()
    }
}

impl OperationsHandler for HostOps {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    /// Outbound `fetch`, standing in for an egress the host would attach the
    /// identity to: it answers the identity it attached, and only for
    /// `http://host.test/`. Same rule: read the slot when entered.
    fn handle_fetch(
        &self,
        request: openworkers_core::HttpRequest,
    ) -> OpFuture<'_, Result<openworkers_core::HttpResponse, String>> {
        let attached = self.current();
        self.audit.lock().unwrap().push(AuditRecord {
            attached: attached.clone(),
            binding: "fetch".into(),
            name: request.url.clone(),
            args: json!({ "tag": request.headers.get("x-tag") }),
        });

        Box::pin(async move {
            if !request.url.starts_with("http://host.test/") {
                return Err("fetch: only http://host.test/ in this spike".into());
            }
            let body = json!({ "attached": attached }).to_string();
            Ok(openworkers_core::HttpResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: openworkers_core::ResponseBody::Bytes(body.into()),
            })
        })
    }

    fn handle_binding_invoke(
        &self,
        binding: &str,
        name: &str,
        args: Value,
    ) -> OpFuture<'_, InvokeResult> {
        // Read now, in the guest's call, not when the future completes.
        let attached = self.current();

        self.audit.lock().unwrap().push(AuditRecord {
            attached: attached.clone(),
            binding: binding.to_string(),
            name: name.to_string(),
            args: args.clone(),
        });

        // The binding name is a free string from the guest: it names nothing
        // but the one binding, and never an Application.
        if binding != BINDING {
            let message = format!("no binding {binding}");
            return Box::pin(async move { InvokeResult::Error(message) });
        }

        let Some(attached) = attached else {
            return Box::pin(async {
                InvokeResult::Error("no request in flight: the host attaches no identity".into())
            });
        };

        match name {
            "whoami" => Box::pin(async move { InvokeResult::Value(json!(attached)) }),
            // An operation that is pending for a while, then answers with the
            // identity it was called under.
            "sleep" => {
                let ms = args["ms"].as_u64().unwrap_or(10).min(5_000);
                Box::pin(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                    InvokeResult::Value(json!({ "slept_ms": ms, "attached": attached }))
                })
            }
            other => {
                let message = format!("no host function {BINDING}.{other}");
                Box::pin(async move { InvokeResult::Error(message) })
            }
        }
    }
}

/// The script with the one binding the host gives every guest.
pub fn script(code: &str) -> Script {
    Script::with_bindings(code.to_string(), None, vec![BindingInfo::invoke(BINDING)])
}

pub fn limits(cpu_ms: u64, wall_ms: u64) -> RuntimeLimits {
    RuntimeLimits {
        max_cpu_time_ms: cpu_ms,
        max_wall_clock_time_ms: wall_ms,
        ..RuntimeLimits::default()
    }
}

/// Removes every copy of the identity header, whatever its case.
pub fn strip_identity(headers: &mut HashMap<String, String>) {
    headers.retain(|k, _| !k.eq_ignore_ascii_case(USER_HEADER));
}

/// The outcome of one request on a worker.
#[derive(Debug)]
pub struct Served {
    pub attached: Attached,
    pub status: Option<u16>,
    pub body: Option<Vec<u8>>,
    pub outcome: Result<(), TerminationReason>,
}

impl Served {
    pub fn json(&self) -> Value {
        let body = self.body.as_deref().unwrap_or_default();
        serde_json::from_slice(body).unwrap_or_else(|_| {
            json!({ "raw": String::from_utf8_lossy(body), "outcome": format!("{:?}", self.outcome) })
        })
    }
}

/// Clears the slot when the event is over, also when its future is dropped.
struct Clear<'a>(&'a HostOps);

impl Drop for Clear<'_> {
    fn drop(&mut self) {
        self.0.end();
    }
}

/// One warm worker of one Application. `serve` takes `&mut self`, like
/// `Worker::exec`: one request at a time.
pub struct AppWorker {
    worker: Worker,
    ops: Arc<HostOps>,
}

impl AppWorker {
    pub async fn new(
        application: &str,
        code: &str,
        limits: RuntimeLimits,
        audit: Audit,
    ) -> Result<Self, TerminationReason> {
        let ops = HostOps::new(application, audit);
        let handle: OperationsHandle = ops.clone();
        let worker = Worker::new_with_ops(script(code), Some(limits), handle).await?;
        Ok(Self { worker, ops })
    }

    pub fn ops(&self) -> &Arc<HostOps> {
        &self.ops
    }

    pub fn worker(&mut self) -> &mut Worker {
        &mut self.worker
    }

    /// Runs one request as `user`: strip the identity header, attach, run the
    /// event to its end (waitUntil included), detach.
    pub async fn serve(&mut self, user: &str, mut request: HttpRequest) -> Served {
        strip_identity(&mut request.headers);

        let attached = self.ops.begin(user);
        let _clear = Clear(&self.ops);

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

        Served {
            attached,
            status,
            body,
            outcome,
        }
    }
}

/// A GET request with the given headers.
pub fn get(path: &str, headers: &[(&str, &str)]) -> HttpRequest {
    HttpRequest {
        method: openworkers_core::HttpMethod::Get,
        url: format!("http://localhost{path}"),
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        body: openworkers_core::RequestBody::None,
    }
}
