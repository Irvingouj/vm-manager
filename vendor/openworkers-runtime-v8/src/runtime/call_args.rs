//! The bound on one host call's arguments (vendor/VENDORED.md, patch 17).
//!
//! `__nativeBindingInvoke` reads a call's arguments as `JSON.stringify`
//! reads them (patch 5). The host bounds what a call may carry
//! (blackbox/CONTRACT.md, "A host call's arguments are bounded"): at most
//! [`TEXT_BYTES`] of JSON text as UTF-8, or, when that is more,
//! [`BYTES_A_VALUE`] bytes for every value in the arguments — each string,
//! number, boolean, `null`, array and object, and each key of an object.
//! The bound is decided before anything of the call is copied to the
//! host's side: the JSON text is a V8 string, whose UTF-8 length is asked
//! without copying it; only a text within the bound is copied, scanned
//! for its values ([`count_values`]) and parsed. A call past the bound is
//! not parsed at all.
//!
//! What was decided travels to the host through [`set`] and [`take`]: the
//! host is entered inside the guest's call (patch 2), so what is set here
//! before the call's future is first polled is read there, on the same
//! thread, and by nothing else. A guest cannot write it: no JavaScript
//! reaches it.

use std::cell::Cell;

/// The most bytes of JSON text one call's arguments may be, as UTF-8.
/// Exactly that is taken.
pub const TEXT_BYTES: usize = 8 * 1024 * 1024;

/// What each value of a call's arguments counts for.
pub const BYTES_A_VALUE: u64 = 32;

/// The most values one call's arguments may hold: [`TEXT_BYTES`] at
/// [`BYTES_A_VALUE`] a value. Exactly that is taken.
pub const VALUES: u64 = 262_144;

/// What the native decided about one call's arguments.
#[derive(Clone, Copy, Debug)]
pub enum Call {
    /// The call is past its own bound: refused before anything of it was
    /// copied. The host answers `{ error: { kind: "limit", limit:
    /// "arguments" } }` and writes its `host-call` record; the arguments
    /// themselves never reach it.
    Refused,
    /// The host's precheck says its own bounds would refuse the call, and
    /// by its word for which — asked before anything of the call was
    /// copied or parsed, so a call that cannot be held costs the host
    /// nothing but the length of its text. The host answers and records
    /// by its word.
    WouldRefuse(&'static str),
    /// The call is within its bound. Its arguments are of this many bytes
    /// — the JSON text's UTF-8 length, or [`BYTES_A_VALUE`] a value when
    /// that is more — for the request's and the program's bounds, which
    /// the host decides.
    Size(u64),
}

/// What the host installs to be asked before a call's arguments are
/// copied or parsed: the call's binding name and the UTF-8 length of its
/// JSON text — always within the call's own bound. The host's word for
/// the bound that would refuse the call, or `None` when none would (or
/// may, or the call is not one the host bounds at all): the word is the
/// host's alone, and so is deciding what it means.
pub type Precheck = std::sync::Arc<dyn Fn(&str, u64) -> Option<&'static str> + Send + Sync>;

thread_local! {
    /// What the call being made on this thread decided. Set by
    /// `__nativeBindingInvoke` before the call's future is first polled,
    /// taken by the host inside that poll, cleared after it.
    static DECIDED: Cell<Option<Call>> = const { Cell::new(None) };
}

/// Notes what the call being made on this thread decided, for the host to
/// read inside the call.
pub fn set(call: Call) {
    DECIDED.with(|cell| cell.set(Some(call)));
}

/// What the call being made decided, taken: read once, by the host inside
/// the call; `None` when no call is being made.
pub fn take() -> Option<Call> {
    DECIDED.with(|cell| cell.take())
}

/// Forgets what the last call decided. The native calls this after the
/// call's first poll, whatever came of it, so that nothing of one call is
/// read for another.
pub fn clear() {
    DECIDED.with(|cell| cell.set(None));
}

/// Counts the values in the JSON text `text`: each string, number,
/// boolean and `null`, each array and each object, and each key of an
/// object — as the host counts a call's arguments
/// (blackbox/CONTRACT.md, "A host call's arguments are bounded").
///
/// A scan, not a parse: it knows a string and its escapes, so a string
/// holding `[`, `{`, `,` or an escaped quote is one value, and the text is
/// never built into anything. Iterative, never recursive: the text may
/// nest deeply, and this runs inside a V8 callback, where a stack
/// overflow cannot unwind. Text that is not JSON is stepped over, not
/// refused — the parse behind the scan is what says whether the text is
/// JSON at all.
pub fn count_values(text: &str) -> u64 {
    let bytes = text.as_bytes();
    // Where the next token begins.
    let mut at = 0_usize;
    let mut values = 0_u64;
    while let Some(&byte) = bytes.get(at) {
        match byte {
            // The whitespace between tokens.
            b' ' | b'\t' | b'\n' | b'\r' => at += 1,
            // A container: one value itself, its contents their own.
            b'{' | b'[' => {
                values += 1;
                at += 1;
            }
            // The end of a container.
            b'}' | b']' => at += 1,
            // A string: an object's key or one value.
            b'"' => {
                skip_string(bytes, &mut at);
                values += 1;
            }
            // A number.
            b'-' | b'0'..=b'9' => {
                skip_number(bytes, &mut at);
                values += 1;
            }
            // `true`, `false` and `null`, each one value.
            b't' if matches!(bytes.get(at..at + 4), Some(b"true")) => {
                at += 4;
                values += 1;
            }
            b'f' if matches!(bytes.get(at..at + 5), Some(b"false")) => {
                at += 5;
                values += 1;
            }
            b'n' if matches!(bytes.get(at..at + 4), Some(b"null")) => {
                at += 4;
                values += 1;
            }
            // The `,` and `:` between tokens, and anything the JSON the
            // scan is given does not hold.
            _ => at += 1,
        }
    }
    values
}

/// Steps `at` past a JSON string: its opening quote, its contents with
/// their escapes, and its closing quote. An escaped quote (`\"`) does not
/// end the string; `\uXXXX` is its four hex digits.
///
/// The contents are stepped with `memchr`, without a look at any other
/// byte: a call's text is mostly its strings' contents, and the scan of
/// it is on the request's own CPU budget.
fn skip_string(bytes: &[u8], at: &mut usize) {
    // Past the opening quote.
    *at += 1;
    loop {
        let Some(rest) = bytes.get(*at..) else {
            return;
        };
        let Some(step) = memchr::memchr2(b'"', b'\\', rest) else {
            // A string that never closes: the parse behind the scan says
            // the text is not JSON.
            *at = bytes.len();
            return;
        };
        if rest.get(step) == Some(&b'"') {
            *at += step + 1;
            return;
        }
        // The escape's backslash: past it and the byte it escapes. On
        // text that ends mid-escape this steps past the end, which the
        // next round's `get` takes for the string's end.
        *at += step + 2;
        if bytes.get(*at - 1) == Some(&b'u') {
            *at += 4;
        }
    }
}

/// Steps `at` past a JSON number: `-?digits(.digits)?([eE][+-]?digits)?`.
/// A number's own characters are stepped as a set, so an exponent's `e`
/// and `+` are taken with the rest.
fn skip_number(bytes: &[u8], at: &mut usize) {
    if bytes.get(*at) == Some(&b'-') {
        *at += 1;
    }
    while bytes
        .get(*at)
        .is_some_and(|byte| matches!(byte, b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-'))
    {
        *at += 1;
    }
}
