//! The Operation gateway: where an operation from Application code enters
//! the host and is decided.
//!
//! The Request runner reads the request in flight synchronously, at the
//! moment the runtime enters the host inside the Application's call, and
//! hands it here. The gateway decides at once, allowed or refused, and the
//! runner logs that decision before anything else happens; an allowed
//! operation then runs as a future that owns what it needs, so it can never
//! look the request in flight up again. A call made with no request in
//! flight is refused by the runner and never reaches the gateway.
//!
//! In this first form the platform has one operation, `whoami`. Later
//! tickets add the Authorization decision, the Operation log and the
//! database executor behind this same interface.

use std::future::Future;
use std::pin::Pin;

use serde_json::{Value, json};

use crate::runner::InFlight;

/// Why an operation was refused, and how to fix it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    /// Which condition failed: a kebab-case word (blackbox/CONTRACT.md,
    /// "Refusal reasons").
    pub reason: &'static str,
    /// One sentence saying how to fix it.
    pub fix: String,
}

/// A call with no request in flight.
pub const NO_REQUEST: &str = "no-request";
/// An operation the host does not know.
pub const UNKNOWN_OPERATION: &str = "unknown-operation";

impl Refusal {
    pub fn no_request() -> Self {
        Self {
            reason: NO_REQUEST,
            fix: "no request in flight: call the platform while handling a request, not at the top level of the script, and not from a timer or a reference kept after the request is over".into(),
        }
    }

    pub fn unknown_operation(operation: &str) -> Self {
        Self {
            reason: UNKNOWN_OPERATION,
            fix: format!("the platform has no operation {operation:?} on env.VM"),
        }
    }

    /// `{ refused: { refusal: { reason }, fix } }`, as Application code
    /// receives it.
    pub fn to_wire(&self) -> Value {
        json!({ "refused": { "refusal": { "reason": self.reason }, "fix": self.fix } })
    }
}

/// `{ ok: value }`, as Application code receives an allowed answer.
pub fn ok_to_wire(value: Value) -> Value {
    // Moved in, not serialised again as `json!` would.
    let mut wire = serde_json::Map::with_capacity(1);
    wire.insert("ok".into(), value);
    Value::Object(wire)
}

/// The rest of an allowed operation.
pub type OperationFuture = Pin<Box<dyn Future<Output = Result<Value, Refusal>> + Send>>;

/// What the gateway decided about one call.
pub enum Decision {
    Allowed(OperationFuture),
    Refused(Refusal),
}

/// Decides and runs the operations Application code asks for.
pub trait Gateway: Send + Sync {
    /// One operation, for the request that was in flight when the call
    /// entered the host. Must decide without waiting.
    fn operate(&self, in_flight: &InFlight, operation: &str, args: &Value) -> Decision;
}

/// The operations of the walking skeleton: `whoami` only.
pub struct PlatformGateway;

impl Gateway for PlatformGateway {
    fn operate(&self, in_flight: &InFlight, operation: &str, _args: &Value) -> Decision {
        match operation {
            // Nothing the Application passes is read: who is asking is the
            // request in flight.
            "whoami" => {
                let answer = json!({
                    "user": in_flight.user.to_json(),
                    "application": &*in_flight.application,
                    "version": in_flight.version,
                    "request": &*in_flight.request,
                });
                Decision::Allowed(Box::pin(std::future::ready(Ok(answer))))
            }
            other => Decision::Refused(Refusal::unknown_operation(other)),
        }
    }
}
