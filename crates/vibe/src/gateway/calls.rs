//! What a request and the program hold of host calls not yet answered
//! (blackbox/CONTRACT.md, "A host call's arguments are bounded"): **one
//! request 16 MiB and 1,024 calls, the program 256 MiB**, a call counting
//! from the moment it is made until the host has let go of its arguments.
//!
//! [`RequestCalls`] is the one request's, in
//! [`InFlight`](crate::runner::InFlight); [`ProgramCalls`] is the
//! program's, in [`PlatformGateway`](super::PlatformGateway). A call's
//! size against both is decided when the call is made, synchronously,
//! inside the Application's call (the native hands it to the host there,
//! vendor/VENDORED.md, patch 17): its own bound the native has decided
//! already, before anything of the call was copied.
//!
//! A call that would pass a bound is answered at once, never waited for,
//! whatever the operation's name: `limit` `calls` for the request's
//! calls, `limit` `arguments` for its bytes, `unavailable` for the
//! program's — in that order, each with no Operation log entry and its
//! `host-call` record `refused` with the reason. A call within them
//! holds a [`Guard`] that travels **with its arguments**: into the future
//! that waits for an operation's permit, and into the task that runs the
//! operation, which outlives a request that ended. A call the host
//! answers at once (a refusal, a malformed call, an operation it does
//! not know) never holds one: its guard is dropped before the answer
//! leaves [`Gateway::operate`](super::Gateway::operate).
//!
//! The counters are one small mutex each, taken synchronously: taken and
//! given back on the engine thread that decides the call and on the
//! runtime that ends the operation, never across an await.

use std::sync::{Arc, Mutex, PoisonError};

/// One request's calls not yet answered: at most this many bytes.
pub const REQUEST_BYTES: u64 = 16 * 1024 * 1024;

/// One request's calls not yet answered: at most this many.
pub const REQUEST_CALLS: u64 = 1024;

/// The program: at most this many bytes in the calls not yet answered of
/// all requests.
pub const PROGRAM_BYTES: u64 = 256 * 1024 * 1024;

/// What one request holds of its calls not yet answered: their bytes and
/// their count.
#[derive(Default)]
pub struct RequestCalls {
    /// `(bytes, calls)`; locked only to take, give back or look, never
    /// across an await.
    held: Mutex<(u64, u64)>,
}

/// What the program holds of the calls not yet answered of all requests.
#[derive(Default)]
pub struct ProgramCalls {
    bytes: Mutex<u64>,
}

/// What taking a call's place came to.
pub enum Took {
    /// The call's place, held until its arguments are let go of.
    Taken(Guard),
    /// The request's bound on calls: answered `limit` `calls`.
    Calls,
    /// The request's bound on bytes: answered `limit` `arguments`.
    Bytes,
    /// The program's: answered `unavailable`.
    Program,
}

impl Took {
    /// The precheck's word for this refusal, by which the host answers
    /// and records it (vendor/VENDORED.md, patch 17); `""` for a taken
    /// place, which no precheck refuses.
    pub fn word(self) -> &'static str {
        match self {
            Self::Taken(_) => "",
            Self::Calls => WORD_CALLS,
            Self::Bytes => WORD_BYTES,
            Self::Program => WORD_PROGRAM,
        }
    }
}

/// The precheck's word for the request's bound on calls.
pub const WORD_CALLS: &str = "calls";
/// The precheck's word for the request's bound on bytes.
pub const WORD_BYTES: &str = "arguments";
/// The precheck's word for the program's.
pub const WORD_PROGRAM: &str = "unavailable";

/// A call's place among what its request and the program hold, given
/// back when the host lets go of the call's arguments: the guard travels
/// with them, into the task that runs the operation, so a request that
/// ends first does not free what its operation still reads.
pub struct Guard {
    request: Arc<RequestCalls>,
    program: Arc<ProgramCalls>,
    bytes: u64,
}

impl RequestCalls {
    /// Takes one call of `size` bytes against this request's bounds and
    /// the program's — the request's calls first, then its bytes, then
    /// the program's — or says which bound the call would pass. Nothing
    /// is held for a call that would pass one.
    pub fn take(self: &Arc<Self>, program: &Arc<ProgramCalls>, size: u64) -> Took {
        {
            let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
            let calls = held.1.saturating_add(1);
            if calls > REQUEST_CALLS {
                return Took::Calls;
            }
            let bytes = held.0.saturating_add(size);
            if bytes > REQUEST_BYTES {
                return Took::Bytes;
            }
            *held = (bytes, calls);
        }
        if !program.take(size) {
            // The program's bound: the place this call had just taken of
            // its request's is given back, for the call is refused.
            self.release(size, 1);
            return Took::Program;
        }
        Took::Taken(Guard {
            request: Arc::clone(self),
            program: Arc::clone(program),
            bytes: size,
        })
    }

    /// Gives back `calls` calls of `size` bytes each: what a call held.
    fn release(&self, size: u64, calls: u64) {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        held.0 = held.0.saturating_sub(size);
        held.1 = held.1.saturating_sub(calls);
    }

    /// Whether a call of `size` bytes would be refused by the request's
    /// and the program's bounds, and by which word ([`WORD_CALLS`],
    /// [`WORD_BYTES`], [`WORD_PROGRAM`]): decided exactly as
    /// [`RequestCalls::take`] decides it — the request's calls first,
    /// then its bytes, then the program's — but taking nothing. The
    /// engine asks this before a call's arguments are copied or parsed
    /// (vendor/VENDORED.md, patch 17), with the JSON text's UTF-8
    /// length, so a call whose text alone would pass a bound is refused
    /// without that work; the call's own [`RequestCalls::take`], a
    /// moment later inside the call, is the one that counts.
    pub fn would_refuse(
        self: &Arc<Self>,
        program: Option<&Arc<ProgramCalls>>,
        size: u64,
    ) -> Option<&'static str> {
        {
            let held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
            if held.1.saturating_add(1) > REQUEST_CALLS {
                return Some(WORD_CALLS);
            }
            if held.0.saturating_add(size) > REQUEST_BYTES {
                return Some(WORD_BYTES);
            }
        }
        program
            .is_some_and(|program| !program.would_hold(size))
            .then_some(WORD_PROGRAM)
    }
}

impl ProgramCalls {
    /// Takes `size` bytes of the program's bound; `false` when the call
    /// would pass it.
    fn take(&self, size: u64) -> bool {
        let mut bytes = self.bytes.lock().unwrap_or_else(PoisonError::into_inner);
        let taken = bytes.saturating_add(size);
        if taken > PROGRAM_BYTES {
            return false;
        }
        *bytes = taken;
        true
    }

    /// Whether the program's bound has room for a call of `size` bytes,
    /// taking nothing ([`RequestCalls::would_refuse`]).
    fn would_hold(&self, size: u64) -> bool {
        self.bytes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .saturating_add(size)
            <= PROGRAM_BYTES
    }

    /// Gives `size` bytes back.
    fn release(&self, size: u64) {
        let mut bytes = self.bytes.lock().unwrap_or_else(PoisonError::into_inner);
        *bytes = bytes.saturating_sub(size);
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.request.release(self.bytes, 1);
        self.program.release(self.bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Which bound a call of `size` bytes would pass, or `""` when it is
    /// taken (its guard dropped with the call: the place is given back).
    fn would_pass(
        request: &Arc<RequestCalls>,
        program: &Arc<ProgramCalls>,
        size: u64,
    ) -> &'static str {
        match request.take(program, size) {
            Took::Taken(_) => "",
            Took::Calls => "calls",
            Took::Bytes => "bytes",
            Took::Program => "program",
        }
    }

    #[test]
    fn a_request_holds_at_most_16_mib_and_1024_calls() {
        let request = Arc::new(RequestCalls::default());
        let program = Arc::new(ProgramCalls::default());
        // 1,024 calls of 16 KiB: both bounds exactly, none over.
        let guards: Vec<Guard> = (0..REQUEST_CALLS)
            .map(
                |n| match request.take(&program, REQUEST_BYTES / REQUEST_CALLS) {
                    Took::Taken(guard) => guard,
                    Took::Calls => panic!("call {} of the 1,024 was `limit` `calls`", n + 1),
                    Took::Bytes => panic!("call {} of the 1,024 was `limit` `arguments`", n + 1),
                    Took::Program => panic!("call {} of the 1,024 was `unavailable`", n + 1),
                },
            )
            .collect();
        assert_eq!(
            would_pass(&request, &program, 0),
            "calls",
            "the 1,025th call of a request, even of no bytes, is `limit` `calls`"
        );
        drop(guards);
        assert_eq!(
            would_pass(&request, &program, 0),
            "",
            "the places are given back when the guards drop"
        );
        // 16 MiB by two calls: a third of one byte is over, and a call of
        // no bytes still fits, as `whoami` must.
        let guards: Vec<Guard> = (0..2)
            .map(|_| match request.take(&program, REQUEST_BYTES / 2) {
                Took::Taken(guard) => guard,
                _ => panic!("half the request's bytes"),
            })
            .collect();
        assert_eq!(
            would_pass(&request, &program, 1),
            "bytes",
            "a call one byte over the request's 16 MiB is `limit` `arguments`"
        );
        assert_eq!(would_pass(&request, &program, 0), "");
        drop(guards);
        assert_eq!(
            would_pass(&request, &program, REQUEST_BYTES),
            "",
            "the whole request's bytes are given back when the guards drop"
        );
    }

    #[test]
    fn the_program_holds_at_most_256_mib_whichever_request_holds_it() {
        let program = Arc::new(ProgramCalls::default());
        // 16 requests of 16 MiB each fill the program exactly.
        let guards: Vec<Guard> = (0..16)
            .flat_map(|n| {
                let program = Arc::clone(&program);
                let request = Arc::new(RequestCalls::default());
                (0..4).map(move |_| match request.take(&program, 4 * 1024 * 1024) {
                    Took::Taken(guard) => guard,
                    _ => panic!("a holder's call, request {n}"),
                })
            })
            .collect();
        let request = Arc::new(RequestCalls::default());
        assert_eq!(
            would_pass(&request, &program, 1),
            "program",
            "a call one byte over the program's 256 MiB is `unavailable`, whichever request it is of"
        );
        assert_eq!(
            would_pass(&request, &program, 0),
            "",
            "a call of no bytes fits, as `whoami` must"
        );
        drop(guards);
        assert_eq!(
            would_pass(&request, &program, 4 * 1024 * 1024),
            "",
            "the program's places are given back when the guards drop"
        );
    }

    #[test]
    fn nothing_is_held_for_a_call_that_would_pass_a_bound() {
        let request = Arc::new(RequestCalls::default());
        let program = Arc::new(ProgramCalls::default());
        // The program full: a call that would pass it leaves the
        // request's places as they were — a call of no bytes after it
        // still fits, so it held nothing of its request's.
        let holders: Vec<Guard> = (0..16)
            .flat_map(|_| {
                let program = Arc::clone(&program);
                let other = Arc::new(RequestCalls::default());
                (0..4).map(move |_| match other.take(&program, 4 * 1024 * 1024) {
                    Took::Taken(guard) => guard,
                    _ => panic!("a holder's call"),
                })
            })
            .collect();
        assert_eq!(would_pass(&request, &program, 1024), "program");
        assert_eq!(would_pass(&request, &program, 0), "");
        drop(holders);
        assert_eq!(would_pass(&request, &program, 1024), "");
    }
}
