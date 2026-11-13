//! The operation `history` through the raw host interface (CONTRACT.md,
//! "The Change history in the platform database: Reading: the operation
//! `history`"), as the tests of ticket #62 send it through a bundle that
//! passes operations through (`operations.js`, `transactions.js`).

use serde_json::{Value, json};

use crate::scenario::op;

/// `history` of `schema`.`table` of the own database as `actor`, with
/// `extra` (an object: `key`, `limit`, `before`, `values`, or anything a
/// test wants to send) merged into the arguments.
pub fn history_call(actor: &str, schema: &str, table: &str, extra: &Value) -> Value {
    let mut arguments = json!({ "own": true, "as": actor, "schema": schema, "table": table });
    if let (Some(into), Some(from)) = (arguments.as_object_mut(), extra.as_object()) {
        for (k, v) in from {
            into.insert(k.clone(), v.clone());
        }
    }
    op("history", &arguments)
}

/// A `history` call with exactly these arguments.
pub fn history_with(arguments: &Value) -> Value {
    op("history", arguments)
}

/// The changes of an `ok` answer of `history`; fails otherwise.
pub fn changes_of(answer: &Value) -> Vec<Value> {
    answer["ok"]["changes"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| {
            panic!("expected an `ok` answer of `history` with `changes`, got {answer}")
        })
}
