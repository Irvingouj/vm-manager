//! A write to the list whose answer is lost (CONTRACT.md, "Outbound
//! calls: The list", last bullet; "A write whose answer is lost"), on a
//! platform database on a Postgres of each test's own whose commits the
//! harness holds ([`HeldPostgres`]): the `PUT` or the `DELETE` is answered
//! `503` `unavailable` within its 10 seconds (and a margin); once the
//! commit goes through, sent again it answers `changed` false, the write
//! having landed. The reads are answered while commits are held.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant};

use blackbox::admin::{self, administrator};
use blackbox::assert_api_error;
use blackbox::lost_answer::{HeldPostgres, QUICK};
use blackbox::outbound_surface::{
    HOSTS, assert_deleted, assert_put_answer, hosts, path_of, request,
};
use blackbox::{Vm, VmOptions};

/// The bound of a write of the list (CONTRACT.md: as the administrators'
/// requests are).
const OWN_BOUND: Duration = Duration::from_secs(10);

/// What a `503` may come after [`OWN_BOUND`]: the request's way there and
/// back on a busy machine.
const MARGIN: Duration = Duration::from_secs(3);

fn on(name: &str) -> (HeldPostgres, Vm) {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform(name);
    let options: VmOptions = admin::options().platform(&platform);
    let vm = Vm::start(&options);
    (postgres, vm)
}

fn lost(vm: &Vm, method: &str, entry: &str) {
    let began = Instant::now();
    let answer = request(vm, Some(&administrator()), method, &path_of(entry))
        .timeout(Duration::from_secs(60))
        .send();
    let took = began.elapsed();
    assert_api_error(&answer, 503, "unavailable");
    assert!(
        took < OWN_BOUND + MARGIN,
        "expected the {method}'s `503` within its own {OWN_BOUND:?} (and {MARGIN:?}), it took {took:?}"
    );
}

#[test]
fn a_put_whose_answer_is_lost_is_503_in_ten_seconds_and_sent_again_says_it_was_added() {
    let (postgres, vm) = on("vm_platform_outbound_lost_put");
    postgres.hold();
    lost(&vm, "PUT", "api.example.com");
    // The read is answered at once while commits are held.
    let began = Instant::now();
    let read = request(&vm, Some(&administrator()), "GET", HOSTS)
        .timeout(Duration::from_secs(30))
        .send();
    assert!(
        began.elapsed() < QUICK + Duration::from_secs(3),
        "expected the list to be read while commits are held, it took {:?}",
        began.elapsed()
    );
    assert!(
        read.status == 200,
        "expected `200` reading the list, got\n{}",
        read.describe()
    );
    postgres.let_through();
    postgres.wait_until_none_waiting("the PUT's commit goes through");
    let answer = request(
        &vm,
        Some(&administrator()),
        "PUT",
        &path_of("api.example.com"),
    )
    .send();
    assert_put_answer(
        &answer,
        "api.example.com",
        false,
        &administrator().id,
        "sent again after the lost answer",
    );
    assert_eq!(hosts(&vm), ["api.example.com"]);
}

#[test]
fn a_delete_whose_answer_is_lost_is_503_in_ten_seconds_and_sent_again_says_it_was_removed() {
    let (postgres, vm) = on("vm_platform_outbound_lost_delete");
    blackbox::outbound_surface::add(&vm, "api.example.com");
    postgres.hold();
    lost(&vm, "DELETE", "api.example.com");
    postgres.let_through();
    postgres.wait_until_none_waiting("the DELETE's commit goes through");
    let answer = request(
        &vm,
        Some(&administrator()),
        "DELETE",
        &path_of("api.example.com"),
    )
    .send();
    assert!(
        answer.status == 200,
        "expected `200` for the DELETE sent again, got\n{}",
        answer.describe()
    );
    assert_deleted(
        &answer,
        "api.example.com",
        false,
        "sent again after the lost answer",
    );
    assert!(hosts(&vm).is_empty(), "the entry is gone");
}
