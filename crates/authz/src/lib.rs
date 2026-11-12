//! The Authorization decision (blackbox/CONTRACT.md, "Lower seam: the
//! Authorization decision" and "The Application's own database: The
//! Authorization decision"; ADR 0003): whether an operation may run, decided
//! from the request in flight, who it acts as, whether its target is the
//! Application's own database, and, for a Resource, the serving Version's
//! Declared access, whether the Resource is registered, and the Grants of
//! the User's Groups. It knows nothing of HTTP, Postgres or V8.
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

/// Who an operation acts as: the User of the request in flight, or the
/// Application itself (only ever on its own database).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Actor {
    User,
    Application,
}

/// Everything the decision may depend on.
#[derive(Clone, Copy, Debug)]
pub struct Question<'a> {
    /// Is there a request in flight?
    pub request_in_flight: bool,
    /// Who the operation acts as.
    pub actor: Actor,
    /// Is the target the Application's own database? When true, `declared`,
    /// `registered`, `grants` and `groups` do not matter.
    pub own_database: bool,
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
    NotOwnDatabase,
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
/// The checks are made in the contract's order, and the first that fails
/// refuses: a request in flight; a declaration of the Resource; the
/// Resource registered; declared at the level needed or above; a Grant to
/// one of the User's Groups at the level needed or above. A User holds the
/// highest level any of their Groups is granted; Groups are compared
/// exactly, byte for byte.
pub fn decide(question: &Question<'_>) -> Decision {
    if !question.request_in_flight {
        return Decision::Refuse(Reason::NoRequest);
    }
    let Some(declared) = question.declared else {
        return Decision::Refuse(Reason::NotDeclared);
    };
    if !question.registered {
        return Decision::Refuse(Reason::NotRegistered);
    }
    if declared < question.needed {
        return Decision::Refuse(Reason::DeclaredLower {
            declared,
            needed: question.needed,
        });
    }
    let held = held(question.groups, question.grants);
    match held {
        Some(level) if level >= question.needed => Decision::Allow,
        _ => Decision::Refuse(Reason::NoGrant {
            held,
            needed: question.needed,
        }),
    }
}

/// The highest level any of `groups` is granted, if any.
fn held(groups: &[&str], grants: &[Grant<'_>]) -> Option<AccessLevel> {
    grants
        .iter()
        .filter(|grant| groups.contains(&grant.group))
        .map(|grant| grant.level)
        .max()
}
