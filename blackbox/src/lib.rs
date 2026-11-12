//! Black-box tests of the `vm` program, written from the tickets' acceptance
//! criteria against the program from outside. The outside interface they
//! assume is in `CONTRACT.md` beside this crate.
//!
//! Empty for now: the test agent fills it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation here is a failing test, not a crash on the serving path"
)]

#[cfg(test)]
mod tests {
    #[test]
    fn placeholder() {}
}
