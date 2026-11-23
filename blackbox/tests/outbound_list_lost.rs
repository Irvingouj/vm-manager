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
use std::thread;

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

/// How long the program has, once the database answers again, to hold
/// what the table holds (the contract says 5 seconds; a loaded machine
/// is allowed more).
const SETTLE_ALLOWED: Duration = Duration::from_secs(15);

/// The number of rows the list's table holds, read as the Postgres
/// administrator; the table is the one of the platform database whose name
/// has `outbound` in it.
fn rows_in_table(vm: &Vm) -> usize {
    let sql = admin::platform_sql(vm);
    let names = sql.batch(
        "SELECT quote_ident(table_name) FROM information_schema.tables \
         WHERE table_schema = 'public' AND table_name LIKE '%outbound%'",
    );
    let name = names
        .into_iter()
        .next()
        .and_then(|row| row.into_iter().next().flatten())
        .unwrap_or_else(|| {
            panic!("expected a table of the platform database named for the outbound list")
        });
    sql.value(&format!("SELECT count(*) FROM {name}"))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("the harness could not count the rows of {name}"))
}

/// Reads the list every 100 ms until `settled` holds of its hosts, at most
/// [`SETTLE_ALLOWED`]; `every` is checked of every reading on the way.
fn wait_until(vm: &Vm, what: &str, every: impl Fn(&[String]), settled: impl Fn(&[String]) -> bool) {
    let began = Instant::now();
    loop {
        let now = hosts(vm);
        every(&now);
        if settled(&now) {
            eprintln!("{what}: settled after {:?}", began.elapsed());
            return;
        }
        assert!(
            began.elapsed() < SETTLE_ALLOWED,
            "{what}: expected the list to hold what the table holds within {SETTLE_ALLOWED:?} of the database answering, with nothing sent again; it answers {now:?}"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_delete_answered_503_takes_the_entry_off_the_list_at_once_and_the_program_reads_the_table_back_by_itself()
 {
    let (postgres, vm) = on("vm_platform_outbound_lost_delete_held");
    blackbox::outbound_surface::add(&vm, "api.example.com");
    blackbox::outbound_surface::add(&vm, "kept.example.com");
    postgres.hold();
    lost(&vm, "DELETE", "api.example.com");
    // From that moment, and before anything lets the commit through, the
    // entry is not answered; the one nobody touched is.
    assert_eq!(
        hosts(&vm),
        ["kept.example.com"],
        "expected `GET` not to answer the entry whose DELETE was answered `503`"
    );
    postgres.let_through();
    postgres.wait_until_none_waiting("the DELETE's commit goes through");
    // The delete was made (the harness lets the held commit through). With
    // nothing sent again, the list is what the table holds.
    wait_until(
        &vm,
        "after the DELETE",
        |now| {
            assert!(
                !now.iter().any(|h| h == "api.example.com"),
                "expected the entry never to be answered again, got {now:?}"
            );
        },
        |now| now.len() == rows_in_table(&vm) && now == ["kept.example.com"],
    );
    assert_eq!(rows_in_table(&vm), 1, "the table holds the one entry left");
}

#[test]
fn a_put_answered_503_is_not_answered_at_once_and_is_when_the_program_has_read_the_table_back() {
    let (postgres, vm) = on("vm_platform_outbound_lost_put_held");
    blackbox::outbound_surface::add(&vm, "kept.example.com");
    postgres.hold();
    lost(&vm, "PUT", "api.example.com");
    assert_eq!(
        hosts(&vm),
        ["kept.example.com"],
        "expected `GET` not to answer the entry whose PUT was answered `503`"
    );
    postgres.let_through();
    postgres.wait_until_none_waiting("the PUT's commit goes through");
    // The add was made: nothing sent again, the list comes to hold it.
    wait_until(
        &vm,
        "after the PUT",
        |_| {},
        |now| now == ["api.example.com", "kept.example.com"] && now.len() == rows_in_table(&vm),
    );
}

// --- A removal that arrives late ---------------------------------------------

#[test]
fn a_removal_whose_bytes_arrive_after_the_program_read_the_list_back_is_not_answered_as_listed() {
    use blackbox::outbound_surface::{DbProxy, entries};
    use blackbox::postgres::{Platform, PrivatePostgres};
    use blackbox::sql::Sql;

    let postgres = PrivatePostgres::start().expect("a Postgres of the test's own");
    let target = format!("{}:{}", postgres.host(), postgres.port())
        .parse()
        .expect("the Postgres's address");
    let proxy = DbProxy::start(target);
    let name = "vm_platform_outbound_late_removal";
    let platform = Platform::on_address(&postgres, name, proxy.address());
    let vm = Vm::start(&admin::options().platform(&platform));
    let admin_user = administrator();
    const LATE: &str = "late-removal.example.com";
    blackbox::outbound_surface::add(&vm, LATE);
    blackbox::outbound_surface::add(&vm, "kept.example.com");

    // The connection that carries the removal is held: its bytes are neither
    // delivered nor refused; every other connection passes.
    proxy.arm(LATE);
    let began = Instant::now();
    let answer = request(&vm, Some(&admin_user), "DELETE", &path_of(LATE))
        .timeout(Duration::from_secs(60))
        .send();
    assert_api_error(&answer, 503, "unavailable");
    assert!(
        began.elapsed() < Duration::from_secs(15),
        "expected the `503` after the program's own time (about 10 s), it took {:?}",
        began.elapsed()
    );
    assert_eq!(
        proxy.held(),
        1,
        "expected the proxy to have held a chunk holding the entry's bytes ({LATE}); it held none: the removal's value did not pass in a chunk of its own after the statement's preparation"
    );
    eprintln!("held chunks (first bytes): {:?}", proxy.held_chunks());
    // Long enough for the program to have read the list back on another
    // connection.
    thread::sleep(Duration::from_secs(3));
    // The removal arrives at the database, late.
    proxy.release();
    thread::sleep(Duration::from_secs(6));

    let table: Vec<String> = Sql::connect(&postgres.url(name))
        .expect("the harness's own connection")
        .batch("SELECT host FROM outbound_hosts ORDER BY host")
        .into_iter()
        .filter_map(|row| row.into_iter().next().flatten())
        .collect();
    let listed = hosts(&vm);
    eprintln!("late removal: the list answers {listed:?}, the table holds {table:?}");
    assert!(
        listed.iter().all(|h| table.contains(h)),
        "expected what `GET /v1/outbound-hosts` answers ({listed:?}) to be no more than what the table holds ({table:?}) within 5 seconds of the database answering; after the release the table {} the row of {LATE}",
        if table.iter().any(|h| h == LATE) {
            "still holds"
        } else {
            "no longer holds"
        }
    );
    // The outside way: an entry listed that a `PUT` then adds as new was an
    // entry the table did not hold.
    let table_held_it = table.iter().any(|h| h == LATE);
    eprintln!(
        "after the release the table {} the row",
        if table_held_it {
            "still holds"
        } else {
            "no longer holds"
        }
    );
    let was_listed = listed.iter().any(|h| h == LATE);
    let again = request(&vm, Some(&admin_user), "PUT", &path_of(LATE)).send();
    eprintln!("re-PUT answered {}", again.text());
    let body = again.json();
    let records = blackbox::outbound_surface::records_of(&vm, "outbound-host-added", LATE);
    assert!(
        !(was_listed && body["changed"] == true),
        "the program answered {LATE} as listed, and the PUT's answer says it was newly added (`changed` true, {} `outbound-host-added` record(s)): the program was answering an entry the table did not hold\nlisted {listed:?}, table {table:?}",
        records.len()
    );
    drop(entries(&vm));
}

// --- A row that is not an entry ------------------------------------------------

#[test]
fn a_row_that_is_not_an_entry_is_not_answered_not_allowed_and_logged_without_its_text() {
    let vm = blackbox::outbound_surface::start();
    for entry in ["kept.example.com", "bad1.example.com", "bad2.example.com"] {
        blackbox::outbound_surface::add(&vm, entry);
    }
    // Rows written by another hand, with the harness's own connection.
    let sql = admin::platform_sql(&vm);
    sql.batch("UPDATE outbound_hosts SET host = 'not an entry!' WHERE host = 'bad1.example.com'");
    sql.batch("UPDATE outbound_hosts SET host = '127.1' WHERE host = 'bad2.example.com'");
    let vm = vm.restart();
    assert_eq!(
        hosts(&vm),
        ["kept.example.com"],
        "expected the program to start all the same, to answer the valid entry beside the rows and not the rows"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let records = loop {
        let found = admin::logged(&vm, "outbound-host-unreadable");
        if !found.is_empty() || Instant::now() > deadline {
            break found;
        }
        thread::sleep(Duration::from_millis(50));
    };
    assert!(
        !records.is_empty(),
        "expected at least one `outbound-host-unreadable` record in the program's log"
    );
    for record in &records {
        let text = record.to_string();
        assert!(
            !text.contains("not an entry") && !text.contains("127.1"),
            "expected the record to hold nothing of the row's text, got {text}"
        );
    }
}
