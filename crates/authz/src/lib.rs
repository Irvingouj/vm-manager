//! The Authorization decision (blackbox/CONTRACT.md, "Lower seam: the
//! Authorization decision"; ADR 0003): whether an operation on a Resource may
//! run, decided from the request in flight, the serving Version's Declared
//! access, whether the Resource is registered, and the Grants of the User's
//! Groups. It knows nothing of HTTP, Postgres or V8.
//!
//! The interface is the contract's, exactly. Its tests are in `tests/` and
//! were written before the decision itself.

/// How much of a Resource a Grant opens. Each level includes the one before:
/// `Structure` < `Read` < `Write`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AccessLevel {
    Structure,
    Read,
    Write,
}

/// A Platform administrator's statement that a Group may reach the Resource
/// at a level.
#[derive(Clone, Copy, Debug)]
pub struct Grant<'a> {
    pub group: &'a str,
    pub level: AccessLevel,
}

/// Everything the decision may depend on.
#[derive(Clone, Copy, Debug)]
pub struct Question<'a> {
    /// Is there a request in flight?
    pub request_in_flight: bool,
    /// The Groups of the User whose request it is.
    pub groups: &'a [&'a str],
    /// What the serving Version declares for the Resource asked for, if anything.
    pub declared: Option<AccessLevel>,
    /// Is a Resource of that name registered?
    pub registered: bool,
    /// The Grants on that Resource.
    pub grants: &'a [Grant<'a>],
    /// The level the operation needs.
    pub needed: AccessLevel,
}

/// The answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Refuse(Reason),
}

/// Why an operation is refused, in the order the checks are made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    NoRequest,
    NotDeclared,
    NotRegistered,
    DeclaredLower {
        declared: AccessLevel,
        needed: AccessLevel,
    },
    NoGrant {
        held: Option<AccessLevel>,
        needed: AccessLevel,
    },
}

/// Decides whether the operation in `question` may run.
///
/// PLACEHOLDER, not the decision: it refuses everything as if no request
/// were in flight, so that the tests in `tests/` can be seen failing before
/// the decision is written. The implementation replaces this body.
pub fn decide(_question: &Question<'_>) -> Decision {
    Decision::Refuse(Reason::NoRequest)
}
