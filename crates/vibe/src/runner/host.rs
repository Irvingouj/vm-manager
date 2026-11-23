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
use openworkers_runtime_v8::runtime::call_args;
use serde_json::{Value, json};

use super::InFlight;
use crate::gateway::{
    Decision, FetchDecision, Gateway, LIMIT, Refusal, UNAVAILABLE, WORD_BYTES, WORD_CALLS,
    WORD_PROGRAM, ok_to_wire,
};
use crate::log;

/// The binding through which Application code calls the platform.
pub const APPLICATION_BINDING: &str = "VM";

/// The binding the platform's own glue (`prelude.js`) uses. Its calls are
/// the platform talking to itself, so they are not `host-call` records.
pub const PLATFORM_BINDING: &str = "__vm_platform";

/// The binding through which the platform's glue makes the Application's
/// `fetch` calls. Its calls are the Application's (they are `host-call`
/// records and Operation log entries), with their own bounds
/// ([`crate::outbound`]) in place of a host call's arguments'.
pub const FETCH_BINDING: &str = "__vm_fetch";

pub struct HostOps {
    pub(super) in_flight: Arc<InFlight>,
    gateway: Arc<dyn Gateway>,
    /// The request is in flight: its event has started and not ended.
    active: AtomicBool,
    /// The request's event has ended: what is printed from now on is in
    /// no entry (crate::app_logs), only in the program's log.
    ended: AtomicBool,
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
            ended: AtomicBool::new(false),
            begun: AtomicBool::new(false),
            failed: AtomicBool::new(false),
        }
    }

    /// The request's event starts.
    pub fn begin(&self) {
        self.active.store(true, Ordering::SeqCst);
    }

    /// The request is over: from now on this context speaks for no one,
    /// every transaction it left open is rolled back, and its open
    /// bodies end — a put still running is given up, the pumps of bodies
    /// nobody reads any more are ended (a body being sent as the response
    /// had its pump taken by the answer, and goes on).
    pub fn end(&self) {
        self.ended.store(true, Ordering::SeqCst);
        self.active.store(false, Ordering::SeqCst);
        self.in_flight.transactions.end();
        self.in_flight.bodies.end();
    }

    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::SeqCst)
    }

    fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    /// Writes an `uncaught` record for this request and marks it failed;
    /// the error goes into the request's log entry too.
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
        self.in_flight.collected.uncaught(error);
    }

    fn host_call(&self, operation: &str, refused: Option<&str>) {
        self.host_call_to(operation, refused, None);
    }

    /// A `host-call` record; `host` is the host of an outbound call, and
    /// no more of its address.
    fn host_call_to(&self, operation: &str, refused: Option<&str>, host: Option<&str>) {
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
        if let (Some(host), Some(object)) = (host, record.as_object_mut()) {
            object.insert("host".into(), host.into());
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

/// `{ error: { kind: "limit", limit: "arguments" } }`: the answer of a
/// call past a bound of this ticket (blackbox/CONTRACT.md, "A host call's
/// arguments are bounded") — its own, which the native decided before
/// anything of the call was copied, or the request's bytes.
fn limit_arguments() -> Value {
    json!({ "error": { "kind": "limit", "limit": "arguments" } })
}

/// `{ error: { kind: "limit", limit: "calls" } }`: the answer of a call
/// that would pass its request's bound on calls.
fn limit_calls() -> Value {
    json!({ "error": { "kind": "limit", "limit": "calls" } })
}

/// `{ error: { kind: "unavailable" } }`: the answer of a call that would
/// pass the program's bound.
fn call_unavailable() -> Value {
    json!({ "error": { "kind": "unavailable" } })
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
        // Decided here, synchronously, while the call is being made. The
        // native hands its decision about the call's arguments here, inside
        // the call (vendor/VENDORED.md, patch 17): it refused a call past
        // the call's own bound before anything of it was copied, and
        // measured one within it.
        let call = call_args::take();
        if binding == PLATFORM_BINDING {
            // The glue's own call: no `host-call` record, whatever it is.
            // The native bounds it like any other: past the bound its
            // arguments are `null` here, so an `uncaught` report of an
            // error too large to carry is still recorded, without its text.
            let result = self.platform_call(name, &args);
            return Box::pin(std::future::ready(result));
        }
        // A call the native refused, before anything of it was copied:
        // answered at once, whatever the operation's name, with or without
        // a request in flight, its arguments never read — the record says
        // `refused` with reason `limit`. A call the host's own precheck
        // refused carries the host's word for which bound; it, too, was
        // decided before anything of it was copied or parsed.
        match call {
            Some(call_args::Call::Refused) => {
                self.host_call(name, Some(LIMIT));
                return Box::pin(std::future::ready(InvokeResult::Value(limit_arguments())));
            }
            Some(call_args::Call::WouldRefuse(word)) => {
                let (answer, reason) = match word {
                    // The words are the gateway's (crate::gateway::calls),
                    // said by its own precheck: which bound would refuse
                    // the call.
                    WORD_CALLS => (limit_calls(), LIMIT),
                    WORD_BYTES => (limit_arguments(), LIMIT),
                    WORD_PROGRAM => (call_unavailable(), UNAVAILABLE),
                    _ => (limit_arguments(), LIMIT),
                };
                self.host_call(name, Some(reason));
                return Box::pin(std::future::ready(InvokeResult::Value(answer)));
            }
            _ => {}
        }
        let active = self.is_active();
        if binding == FETCH_BINDING && name == "fetch" {
            // The call's own bounds are the outbound ones: the arguments
            // are read by the gateway, which holds them to them.
            let FetchDecision {
                decision: Decision { refused, answer },
                host,
            } = self.gateway.fetch(&self.in_flight, active, &args);
            self.host_call_to("fetch", refused, host.as_deref());
            return Box::pin(async move { InvokeResult::Value(answer.await) });
        }
        if binding != APPLICATION_BINDING {
            let refusal = if active {
                Refusal::unknown_operation(name)
            } else {
                Refusal::no_request()
            };
            self.host_call(name, Some(refusal.reason));
            return Box::pin(std::future::ready(InvokeResult::Value(refusal.to_wire())));
        }
        // The call's size, as the native measured it (0 for arguments not
        // given), for the gateway to hold against the request's and the
        // program's bounds. `None` would be a call that did not come
        // through the platform's invoke native — there is no such call;
        // it is answered as past the bound rather than counted as nothing.
        let Decision { refused, answer } = match call {
            Some(call_args::Call::Size(size)) => {
                self.gateway
                    .operate(&self.in_flight, active, name, args, size)
            }
            _ => {
                self.host_call(name, Some(LIMIT));
                return Box::pin(std::future::ready(InvokeResult::Value(limit_arguments())));
            }
        };
        // The gateway reads `active` too: a `query` with no request in
        // flight is refused and still has its Operation log entry.
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
        let level = level_name(&level);
        log::record(
            "printed",
            json!({
                "request": in_flight.map(|f| &*f.request),
                "application": &*self.in_flight.application,
                "user": in_flight.map(|f| f.user.id.as_str()),
                "level": level,
                "message": &message,
            }),
        );
        // Then the message itself, moved, into the request's log entry:
        // printed by the request, or by the script while it was loaded for
        // the request (`loading`). After the request ended, into none.
        if in_flight.is_some() {
            self.in_flight.collected.printed(level, message, false);
        } else if !self.ended.load(Ordering::SeqCst) {
            self.in_flight.collected.printed(level, message, true);
        }
    }
}

#[cfg(test)]
mod tests {
    //! The counting scan of the call's own bound
    //! (vendor/VENDORED.md, patch 17) pinned against a walk of the parsed
    //! `Value`: whatever JSON `serde_json` writes of a value, the scan
    //! counts what the walk counts. These run here, with the host's own
    //! tests, because the vendored crate is excluded from the workspace
    //! and its own test target cannot be built on a machine without the
    //! network.
    use openworkers_runtime_v8::runtime::call_args::{BYTES_A_VALUE, VALUES, count_values};
    use serde_json::{Value, json};
    /// The values of `value`, as the contract counts them: each string,
    /// number, boolean, `null`, array and object, and each key.
    fn walk(value: &Value) -> u64 {
        match value {
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => 1,
            Value::Array(items) => 1 + items.iter().map(walk).sum::<u64>(),
            Value::Object(fields) => {
                1 + fields.iter().map(|(_, field)| 1 + walk(field)).sum::<u64>()
            }
        }
    }

    /// Fails unless the scan counts `value`'s text as the walk counts
    /// `value`.
    fn assert_counted(value: &Value) {
        let text = serde_json::to_string(value).unwrap_or_default();
        assert_eq!(
            count_values(&text),
            walk(value),
            "the scan and the walk disagree on {text}"
        );
    }

    #[test]
    fn the_scan_counts_what_the_walk_counts() {
        // The contract's own example: five values, 160 bytes by them.
        let example = json!({ "resource": "devops", "sql": "select 1" });
        assert_counted(&example);
        assert_eq!(walk(&example), 5);
        // Strings holding the scan's own punctuation, an escaped quote,
        // and a backslash before one.
        assert_counted(&json!([
            "a[1]{b},c",
            "before \" inside",
            "backslash \\\" after",
            "[{},]",
        ]));
        // Nested empty arrays and objects.
        assert_counted(&json!([[], {}, [[], {}], { "a": [] }]));
        // Numbers with exponents, and every other kind.
        assert_counted(&json!([
            1e10,
            -2.5E+3,
            0.5e-1,
            0,
            -0.0,
            true,
            false,
            null,
            "",
            "é",
            "\u{1F600}",
            "\u{FFFD}",
        ]));
        // Keys of every shape the scan must know.
        assert_counted(&json!({
            "k\"y": 1,
            "k\\y": 2,
            "k,y": 3,
            "é": 4,
            "": 5
        }));
        // An empty text counts nothing.
        assert_eq!(count_values(""), 0);
    }

    #[test]
    fn the_scan_never_recurses_however_deep_the_text() {
        // A text nested deeper than any parser's recursion limit: the
        // scan is iterative (it runs inside a V8 callback, where a stack
        // overflow cannot unwind), so it counts it whole.
        let depth = 200_000;
        let mut text = String::with_capacity(2 * depth);
        for _ in 0..depth {
            text.push('[');
        }
        for _ in 0..depth {
            text.push(']');
        }
        assert_eq!(
            count_values(&text),
            u64::try_from(depth).unwrap_or(u64::MAX),
            "one value a container"
        );
    }

    #[test]
    fn the_bounds_are_the_contracts_numbers() {
        // 8 MiB at 32 bytes a value: exactly 262,144 values, one call's
        // most (blackbox/CONTRACT.md, "A host call's arguments are
        // bounded").
        assert_eq!(BYTES_A_VALUE * VALUES, 8_388_608);
    }
}
