//! What a request, an Application and the program hold of host calls not
//! yet answered (blackbox/CONTRACT.md, "A host call's arguments are
//! bounded"): **one request 16 MiB and 1,024 calls, one Application 64 MiB,
//! the program 256 MiB**, a call counting from the moment it is made until
//! the host has let go of its arguments ("An Application's share of what
//! the program holds").
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
//! Application's share and the program's — in that order, each with no
//! Operation log entry and its `host-call` record `refused` with the
//! reason. A refusal for the Application's share writes the program's
//! `share-full` record (`crate::shares`); one for the program's bound
//! writes none. A call within them holds a [`Guard`] that travels **with
//! its arguments**: into the future that waits for an operation's permit,
//! and into the task that runs the operation, which outlives a request
//! that ended. A call the host answers at once (a refusal, a malformed
//! call, an operation it does not know) never holds one: its guard is
//! dropped before the answer leaves
//! [`Gateway::operate`](super::Gateway::operate).
//!
//! The counters are one small mutex each, taken synchronously: taken and
//! given back on the engine thread that decides the call and on the
//! runtime that ends the operation, never across an await.

use std::sync::{Arc, Mutex, PoisonError};

use crate::shares::{Bytes, Kind, Mentions};

/// One request's calls not yet answered: at most this many bytes.
pub const REQUEST_BYTES: u64 = 16 * 1024 * 1024;

/// One request's calls not yet answered: at most this many.
pub const REQUEST_CALLS: u64 = 1024;

/// The program: at most this many bytes in the calls not yet answered of
/// all requests.
pub const PROGRAM_BYTES: u64 = 256 * 1024 * 1024;

/// Of them, at most this many for one Application ("An Application's share
/// of what the program holds"); exactly that is taken.
pub const PER_APPLICATION_BYTES: u64 = 64 * 1024 * 1024;

/// What one request holds of its calls not yet answered: their bytes and
/// their count.
#[derive(Default)]
pub struct RequestCalls {
    /// `(bytes, calls)`; locked only to take, give back or look, never
    /// across an await.
    held: Mutex<(u64, u64)>,
}

/// What the program holds of the calls not yet answered of all requests,
/// and its 64 MiB for each Application, in one structure behind one lock so
/// that a call is refused for one bound or the other and never takes and
/// gives back a place meanwhile.
pub struct ProgramCalls {
    held: Mutex<Held>,
    mentions: Arc<Mentions>,
}

/// The program's bytes and the bytes each Application holds.
#[derive(Debug)]
struct Held {
    program: u64,
    applications: Bytes,
}

impl ProgramCalls {
    /// The program's 256 MiB, sharing out `mentions` for
    /// `call-arguments`.
    pub fn new(mentions: Arc<Mentions>) -> Self {
        Self {
            held: Mutex::new(Held {
                program: 0,
                applications: Bytes::new(PER_APPLICATION_BYTES),
            }),
            mentions,
        }
    }

    /// Takes `size` bytes of `application`'s share and then of the
    /// program's; `Err` says which had no room. Both are decided under the
    /// one lock, so nothing is taken and given back.
    fn take(&self, application: &Arc<str>, size: u64) -> Result<(), Refused> {
        let refusal = {
            let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
            if !held.applications.would_take(application, size) {
                Some(Refused::Share)
            } else if held.program.saturating_add(size) > PROGRAM_BYTES {
                Some(Refused::Program)
            } else {
                held.program = held.program.saturating_add(size);
                held.applications.take(application, size);
                None
            }
        };
        match refusal {
            None => Ok(()),
            Some(Refused::Share) => {
                // The record is written outside the lock, and only for the
                // share: the Application's own held bytes would pass.
                self.mentions.record(application, Kind::CallArguments);
                Err(Refused::Share)
            }
            Some(Refused::Program) => Err(Refused::Program),
        }
    }

    /// Whether the Application's share and the program's bound have room
    /// for a call of `size` bytes, taking nothing; a refusal for the share
    /// writes its record. Decided exactly as [`ProgramCalls::take`].
    fn would_hold(&self, application: &Arc<str>, size: u64) -> Option<Refused> {
        let refusal = {
            let held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
            if !held.applications.would_take(application, size) {
                Some(Refused::Share)
            } else if held.program.saturating_add(size) > PROGRAM_BYTES {
                Some(Refused::Program)
            } else {
                None
            }
        };
        if matches!(refusal, Some(Refused::Share)) {
            self.mentions.record(application, Kind::CallArguments);
        }
        refusal
    }

    /// Gives `size` bytes of `application` back: the program's and its
    /// share's, at the moment the call's guard drops.
    fn release(&self, application: &Arc<str>, size: u64) {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        held.program = held.program.saturating_sub(size);
        held.applications.give_back(application, size);
    }
}

/// What taking a call's place came to.
pub enum Took {
    /// The call's place, held until its arguments are let go of.
    Taken(Guard),
    /// The request's bound on calls: answered `limit` `calls`.
    Calls,
    /// The request's bound on bytes: answered `limit` `arguments`.
    Bytes,
    /// The Application's share: answered `unavailable` as the program's
    /// bound is, with the `share-full` record written by the share.
    Share,
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
            Self::Share | Self::Program => WORD_PROGRAM,
        }
    }
}

/// The precheck's word for the request's bound on calls.
pub const WORD_CALLS: &str = "calls";
/// The precheck's word for the request's bound on bytes.
pub const WORD_BYTES: &str = "arguments";
/// The precheck's word for the program's.
pub const WORD_PROGRAM: &str = "unavailable";

/// A call's place among what its request, its Application and the program
/// hold, given back when the host lets go of the call's arguments: the
/// guard travels with them, into the task that runs the operation, so a
/// request that ends first does not free what its operation still reads.
pub struct Guard {
    request: Arc<RequestCalls>,
    program: Arc<ProgramCalls>,
    application: Arc<str>,
    bytes: u64,
}

impl RequestCalls {
    /// Takes one call of `size` bytes against this request's bounds, its
    /// Application's share and the program's — the request's calls first,
    /// then its bytes, then the Application's share and the program's —
    /// or says which bound the call would pass. Nothing is held for a
    /// call that would pass one.
    pub fn take(
        self: &Arc<Self>,
        program: &Arc<ProgramCalls>,
        application: &Arc<str>,
        size: u64,
    ) -> Took {
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
        if let Err(refused) = program.take(application, size) {
            // The Application's share or the program's bound: the place
            // this call had just taken of its request's is given back,
            // for the call is refused.
            self.release(size, 1);
            return match refused {
                Refused::Share => Took::Share,
                Refused::Program => Took::Program,
            };
        }
        Took::Taken(Guard {
            request: Arc::clone(self),
            program: Arc::clone(program),
            application: Arc::clone(application),
            bytes: size,
        })
    }

    /// Gives back `calls` calls of `size` bytes each: what a call held.
    fn release(&self, size: u64, calls: u64) {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        held.0 = held.0.saturating_sub(size);
        held.1 = held.1.saturating_sub(calls);
    }

    /// Whether a call of `size` bytes would be refused by the request's,
    /// the Application's and the program's bounds, and by which word
    /// ([`WORD_CALLS`], [`WORD_BYTES`], [`WORD_PROGRAM`]): decided exactly
    /// as [`RequestCalls::take`] decides it — the request's calls first,
    /// then its bytes, then the Application's share and the program's —
    /// but taking nothing. The engine asks this before a call's arguments
    /// are copied or parsed (vendor/VENDORED.md, patch 17), with the JSON
    /// text's UTF-8 length, so a call whose text alone would pass a bound
    /// is refused without that work; the call's own [`RequestCalls::take`],
    /// a moment later inside the call, is the one that counts.
    pub fn would_refuse(
        self: &Arc<Self>,
        program: Option<&Arc<ProgramCalls>>,
        application: &Arc<str>,
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
            .is_some_and(|program| program.would_hold(application, size).is_some())
            .then_some(WORD_PROGRAM)
    }
}

/// Which bound refused a call whose request's bounds it passed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refused {
    /// The Application's share.
    Share,
    /// The program's bound.
    Program,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.request.release(self.bytes, 1);
        self.program.release(&self.application, self.bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str) -> Arc<str> {
        Arc::from(name)
    }

    fn program_calls() -> Arc<ProgramCalls> {
        Arc::new(ProgramCalls::new(Arc::new(Mentions::default())))
    }

    /// Which bound a call of `size` bytes would pass, or `""` when it is
    /// taken (its guard dropped with the call: the place is given back).
    fn would_pass(
        request: &Arc<RequestCalls>,
        program: &Arc<ProgramCalls>,
        application: &Arc<str>,
        size: u64,
    ) -> &'static str {
        match request.take(program, application, size) {
            Took::Taken(_) => "",
            Took::Calls => "calls",
            Took::Bytes => "bytes",
            Took::Share => "share",
            Took::Program => "program",
        }
    }

    #[test]
    fn a_request_holds_at_most_16_mib_and_1024_calls() {
        let request = Arc::new(RequestCalls::default());
        let program = program_calls();
        // 1,024 calls of 16 KiB: both bounds exactly, none over.
        let guards: Vec<Guard> = (0..REQUEST_CALLS)
            .map(
                |n| match request.take(&program, &app("a"), REQUEST_BYTES / REQUEST_CALLS) {
                    Took::Taken(guard) => guard,
                    Took::Calls => panic!("call {} of the 1,024 was `limit` `calls`", n + 1),
                    Took::Bytes => panic!("call {} of the 1,024 was `limit` `arguments`", n + 1),
                    Took::Share | Took::Program => {
                        panic!("call {} of the 1,024 was `unavailable`", n + 1)
                    }
                },
            )
            .collect();
        assert_eq!(
            would_pass(&request, &program, &app("a"), 0),
            "calls",
            "the 1,025th call of a request, even of no bytes, is `limit` `calls`"
        );
        drop(guards);
        assert_eq!(
            would_pass(&request, &program, &app("a"), 0),
            "",
            "the places are given back when the guards drop"
        );
        // 16 MiB by two calls: a third of one byte is over, and a call of
        // no bytes still fits, as `whoami` must.
        let guards: Vec<Guard> = (0..2)
            .map(
                |_| match request.take(&program, &app("a"), REQUEST_BYTES / 2) {
                    Took::Taken(guard) => guard,
                    _ => panic!("half the request's bytes"),
                },
            )
            .collect();
        assert_eq!(
            would_pass(&request, &program, &app("a"), 1),
            "bytes",
            "a call one byte over the request's 16 MiB is `limit` `arguments`"
        );
        assert_eq!(would_pass(&request, &program, &app("a"), 0), "");
        drop(guards);
        assert_eq!(
            would_pass(&request, &program, &app("a"), REQUEST_BYTES),
            "",
            "the whole request's bytes are given back when the guards drop"
        );
    }

    #[test]
    fn the_program_holds_at_most_256_mib_whichever_request_holds_it() {
        let program = program_calls();
        // 16 requests of 16 MiB each fill the program exactly.
        let guards: Vec<Guard> = (0..16)
            .flat_map(|n| {
                let program = Arc::clone(&program);
                let request = Arc::new(RequestCalls::default());
                let application: Arc<str> = Arc::from(format!("app-{n}"));
                (0..4).map(
                    move |_| match request.take(&program, &application, 4 * 1024 * 1024) {
                        Took::Taken(guard) => guard,
                        _ => panic!("a holder's call, request {n}"),
                    },
                )
            })
            .collect();
        let request = Arc::new(RequestCalls::default());
        assert_eq!(
            would_pass(&request, &program, &app("probe"), 1),
            "program",
            "a call one byte over the program's 256 MiB is `unavailable`, whichever request it is of"
        );
        assert_eq!(
            would_pass(&request, &program, &app("probe"), 0),
            "",
            "a call of no bytes fits, as `whoami` must"
        );
        drop(guards);
        assert_eq!(
            would_pass(&request, &program, &app("probe"), 4 * 1024 * 1024),
            "",
            "the program's places are given back when the guards drop"
        );
    }

    #[test]
    fn nothing_is_held_for_a_call_that_would_pass_a_bound() {
        let request = Arc::new(RequestCalls::default());
        let program = program_calls();
        // The program full: a call that would pass it leaves the
        // request's places as they were — a call of no bytes after it
        // still fits, so it held nothing of its request's.
        let holders: Vec<Guard> = (0..16)
            .flat_map(|n| {
                let program = Arc::clone(&program);
                let other = Arc::new(RequestCalls::default());
                let application: Arc<str> = Arc::from(format!("app-{n}"));
                (0..4).map(
                    move |_| match other.take(&program, &application, 4 * 1024 * 1024) {
                        Took::Taken(guard) => guard,
                        _ => panic!("a holder's call"),
                    },
                )
            })
            .collect();
        assert_eq!(
            would_pass(&request, &program, &app("probe"), 1024),
            "program"
        );
        assert_eq!(would_pass(&request, &program, &app("probe"), 0), "");
        drop(holders);
        assert_eq!(would_pass(&request, &program, &app("probe"), 1024), "");
    }

    #[test]
    fn an_application_at_its_share_is_refused_by_the_share_and_writes_one_record() {
        let mentions = Arc::new(Mentions::default());
        let program = Arc::new(ProgramCalls::new(Arc::clone(&mentions)));
        let a = app("a");
        // A holds exactly its 64 MiB in four calls of 16 MiB.
        let holders: Vec<Guard> = (0..4)
            .map(
                |_| match Arc::new(RequestCalls::default()).take(&program, &a, 16 * 1024 * 1024) {
                    Took::Taken(guard) => guard,
                    _ => panic!("a call of A within its share"),
                },
            )
            .collect();
        let request = Arc::new(RequestCalls::default());
        assert!(
            matches!(request.take(&program, &a, 1), Took::Share),
            "A's next byte passes its share, though the program has room"
        );
        assert_eq!(
            mentions.remembered(),
            1,
            "the share refusal writes one record"
        );
        // A second refusal within the minute writes none; another
        // Application is served.
        assert!(matches!(
            Arc::new(RequestCalls::default()).take(&program, &a, 1),
            Took::Share
        ));
        assert_eq!(mentions.remembered(), 1);
        assert!(matches!(
            Arc::new(RequestCalls::default()).take(&program, &app("b"), 1),
            Took::Taken(_)
        ));
        drop(holders);
    }

    #[test]
    fn a_call_past_the_program_within_the_share_is_the_programs_and_writes_no_record() {
        let mentions = Arc::new(Mentions::default());
        let program = Arc::new(ProgramCalls::new(Arc::clone(&mentions)));
        // Four Applications at their 64 MiB exactly fill the program's
        // 256 MiB.
        let mut holders = Vec::new();
        for n in 0..4 {
            let application: Arc<str> = Arc::from(format!("app-{n}"));
            for _ in 0..4 {
                let request = Arc::new(RequestCalls::default());
                match request.take(&program, &application, 16 * 1024 * 1024) {
                    Took::Taken(guard) => holders.push(guard),
                    _ => panic!("a holder's call within the program"),
                }
            }
        }
        let probe = app("probe");
        let request = Arc::new(RequestCalls::default());
        assert!(
            matches!(request.take(&program, &probe, 1), Took::Program),
            "the program's 256 MiB are full and the probe is within its share"
        );
        assert_eq!(
            mentions.remembered(),
            0,
            "a refusal for the program's bound writes no `share-full`"
        );
        assert_eq!(
            request.would_refuse(Some(&program), &probe, 1),
            Some(WORD_PROGRAM),
            "the precheck decides as `take` does"
        );
        assert_eq!(
            mentions.remembered(),
            0,
            "and writes no `share-full` either"
        );
        drop(holders);
    }
}
