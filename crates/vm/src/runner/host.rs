//! The host side of one request's context: what the runtime calls when
//! Application code prints, calls `env.VM.invoke`, or fetches.
//!
//! One [`HostOps`] belongs to one context, and one context to one request,
//! so the request it may speak for is fixed when it is made. It speaks for
//! that request only between [`HostOps::begin`] and [`HostOps::end`], which
//! the runner calls around the request's event: before (the script's top
//! level) and after (code left running), it speaks for no one.
//!
//! Every call is decided and logged at the moment Application code makes
//! it: the patched runtime enters the host inline, inside the call
//! (vendor/VENDORED.md, patch 2).

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use openworkers_core::{
    HttpRequest, HttpResponse, InvokeResult, LogLevel, OpFuture, OperationsHandler,
};
use serde_json::{Value, json};

use super::InFlight;
use crate::gateway::{Decision, Gateway, Refusal, ok_to_wire};
use crate::log;

/// The binding through which Application code calls the platform.
pub const APPLICATION_BINDING: &str = "VM";

/// The binding the platform's own glue (`prelude.js`) uses. Its calls are
/// the platform talking to itself, so they are not `host-call` records.
pub const PLATFORM_BINDING: &str = "__vm_platform";

pub struct HostOps {
    in_flight: Arc<InFlight>,
    gateway: Arc<dyn Gateway>,
    /// The request is in flight: its event has started and not ended.
    active: AtomicBool,
    /// The glue has asked whose request this is.
    begun: AtomicBool,
    /// The Application threw, rejected or answered with something that is
    /// not a Response; the User gets the platform's fixed 500.
    failed: AtomicBool,
}

impl HostOps {
    pub fn new(in_flight: Arc<InFlight>, gateway: Arc<dyn Gateway>) -> Self {
        Self {
            in_flight,
            gateway,
            active: AtomicBool::new(false),
            begun: AtomicBool::new(false),
            failed: AtomicBool::new(false),
        }
    }

    /// The request's event starts.
    pub fn begin(&self) {
        self.active.store(true, Ordering::SeqCst);
    }

    /// The request is over: from now on this context speaks for no one,
    /// and every transaction it left open is rolled back.
    pub fn end(&self) {
        self.active.store(false, Ordering::SeqCst);
        self.in_flight.transactions.end();
    }

    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::SeqCst)
    }

    fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    /// Writes an `uncaught` record for this request and marks it failed.
    pub fn uncaught(&self, error: &str) {
        self.failed.store(true, Ordering::SeqCst);
        log::record(
            "uncaught",
            json!({
                "request": &*self.in_flight.request,
                "application": &*self.in_flight.application,
                "user": &self.in_flight.user.id,
                "error": error,
            }),
        );
    }

    fn host_call(&self, operation: &str, refused: Option<&str>) {
        let in_flight = self.is_active().then_some(&*self.in_flight);
        let mut record = json!({
            "request": in_flight.map(|f| &*f.request),
            "application": &*self.in_flight.application,
            "user": in_flight.map(|f| f.user.id.as_str()),
            "operation": operation,
            "outcome": if refused.is_some() { "refused" } else { "allowed" },
        });
        if let (Some(reason), Some(object)) = (refused, record.as_object_mut()) {
            object.insert("reason".into(), reason.into());
        }
        log::record("host-call", record);
    }

    /// A call from the platform's glue. Not a `host-call`: the Application
    /// did not ask for it.
    fn platform_call(&self, name: &str, args: &Value) -> InvokeResult {
        if !self.is_active() {
            return InvokeResult::Value(Refusal::no_request().to_wire());
        }
        match name {
            // Once per request: the glue keeps the answer for the request.
            // A second ask is not the glue's, so it is refused and logged.
            "begin" if !self.begun.swap(true, Ordering::SeqCst) => {
                InvokeResult::Value(ok_to_wire(json!({ "user": self.in_flight.user.to_json() })))
            }
            "uncaught" => {
                let error = args
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("a thrown value that cannot be shown");
                self.uncaught(error);
                InvokeResult::Value(ok_to_wire(Value::Null))
            }
            other => {
                let refusal = Refusal::unknown_operation(other);
                self.host_call(other, Some(refusal.reason));
                InvokeResult::Value(refusal.to_wire())
            }
        }
    }
}

fn level_name(level: &LogLevel) -> &'static str {
    match level {
        LogLevel::Error => "error",
        LogLevel::Warn => "warn",
        LogLevel::Info => "info",
        LogLevel::Log => "log",
        LogLevel::Debug => "debug",
        LogLevel::Trace => "trace",
    }
}

impl OperationsHandler for HostOps {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn handle_binding_invoke(
        &self,
        binding: &str,
        name: &str,
        args: Value,
    ) -> OpFuture<'_, InvokeResult> {
        // Decided here, synchronously, while the call is being made.
        if binding == PLATFORM_BINDING {
            let result = self.platform_call(name, &args);
            return Box::pin(std::future::ready(result));
        }
        let active = self.is_active();
        if binding != APPLICATION_BINDING {
            let refusal = if active {
                Refusal::unknown_operation(name)
            } else {
                Refusal::no_request()
            };
            self.host_call(name, Some(refusal.reason));
            return Box::pin(std::future::ready(InvokeResult::Value(refusal.to_wire())));
        }
        // The gateway reads `active` too: a `query` with no request in
        // flight is refused and still has its Operation log entry.
        let Decision { refused, answer } =
            self.gateway.operate(&self.in_flight, active, name, &args);
        self.host_call(name, refused);
        Box::pin(async move { InvokeResult::Value(answer.await) })
    }

    fn handle_fetch(&self, _request: HttpRequest) -> OpFuture<'_, Result<HttpResponse, String>> {
        Box::pin(std::future::ready(Err(
            "outbound fetch is not available to Applications on this platform".to_owned(),
        )))
    }

    fn handle_log(&self, level: LogLevel, message: String) {
        let in_flight = self.is_active().then_some(&*self.in_flight);
        log::record(
            "printed",
            json!({
                "request": in_flight.map(|f| &*f.request),
                "application": &*self.in_flight.application,
                "user": in_flight.map(|f| f.user.id.as_str()),
                "level": level_name(&level),
                "message": message,
            }),
        );
    }
}
