//! A change of who may open an Application whose answer is lost
//! (CONTRACT.md, "Contributors and sharing: Opening an Application", the
//! bullet "A change that is answered `503` `unavailable` may or may not
//! have been made"), with the stand-in sign-in, on a platform database on a
//! Postgres of each test's own.
//!
//! The harness makes that Postgres hold every commit that wrote something
//! (`synchronous_commit = on` for the program's role from the start, then
//! `synchronous_standby_names` naming a standby that does not exist, and a
//! reload): reads are answered, a commit waits. The program gives up on
//! the change after its limit for an operation on the platform database
//! (20 s) and answers `503` `unavailable`; when the harness lets commits
//! through again, the waiting commit lands. From that answer on, nobody is
//! let in on the lists as they were: requests to the Application are `503`
//! (with `retry-after`, answered at once) until the program has read what
//! became of the change; then they are decided by the lists as the
//! database has them. The same at a start: a program started while the
//! change was on its way does not serve by the lists it read then.
//!
//! Each test takes about half a minute (the program's 20 s limit, then a
//! few seconds for the change to be settled).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::postgres::{Platform, PrivatePostgres};
use blackbox::sharing::{
    TRACED, add, assert_cannot_open, assert_opens, create_unshared, keys, remove, share_ok, user,
};
use blackbox::sql::Sql;
use blackbox::{Response, User, Vm, VmOptions, assert_api_error};
use serde_json::{Value, json};

const APP: &str = "board";

/// How long the program may take to answer a request while the platform
/// database holds commits: it does not wait for it.
const QUICK: Duration = Duration::from_secs(2);
/// How long the program has, once commits go through, to have read what
/// became of the change.
const SETTLE: Duration = Duration::from_secs(15);

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn bob() -> User {
    User::new("bob")
}

fn carol() -> User {
    User::new("carol")
}

/// A Postgres of the test's own, a platform database on it, and the
/// program on that database with `board` made by alice.
fn with_board(name: &str) -> (PrivatePostgres, VmOptions, Vm) {
    let postgres = PrivatePostgres::start().unwrap();
    // The harness's Postgres is started with `synchronous_commit=off`, for
    // speed; a commit waits for a standby only with it on, so it is turned
    // on for the role the program connects as (the superuser) before the
    // program opens its first session.
    superuser(&postgres).batch("ALTER ROLE CURRENT_USER SET synchronous_commit = on");
    let platform = Platform::on(&postgres, name);
    let options = VmOptions::new().stand_in_sign_in().platform(&platform);
    let vm = Vm::start(&options);
    create_unshared(&vm, &alice(), APP, TRACED);
    (postgres, options, vm)
}

/// The harness's superuser on that Postgres.
fn superuser(postgres: &PrivatePostgres) -> Sql {
    Sql::connect(&postgres.url("postgres")).unwrap()
}

/// How many lines of the server's log name `synchronous_standby_names`.
fn reloads_seen(postgres: &PrivatePostgres) -> usize {
    postgres
        .server_log()
        .lines()
        .filter(|l| l.contains("synchronous_standby_names"))
        .count()
}

/// Sets `synchronous_standby_names` (or resets it with `None`) and reloads,
/// waiting until the server's log shows the server took it, and a little
/// more for every server process to have taken it before its next command.
fn set_standby(sql: &Sql, postgres: &PrivatePostgres, value: Option<&str>) {
    let before = reloads_seen(postgres);
    match value {
        Some(value) => sql.batch(&format!(
            "ALTER SYSTEM SET synchronous_standby_names = '{value}'"
        )),
        None => sql.batch("ALTER SYSTEM RESET synchronous_standby_names"),
    };
    sql.batch("SELECT pg_reload_conf()");
    let deadline = Instant::now() + Duration::from_secs(10);
    while reloads_seen(postgres) == before {
        assert!(
            Instant::now() < deadline,
            "expected the test Postgres to take the new configuration\n{}",
            postgres.server_log()
        );
        thread::sleep(Duration::from_millis(20));
    }
    thread::sleep(Duration::from_millis(300));
}

/// Makes every commit that wrote something wait.
fn hold_commits(sql: &Sql, postgres: &PrivatePostgres) {
    set_standby(sql, postgres, Some("nobody"));
}

/// Lets commits through again, those waiting included.
fn let_commits_through(sql: &Sql, postgres: &PrivatePostgres) {
    set_standby(sql, postgres, None);
}

/// How many commits are waiting.
fn commits_waiting(sql: &Sql) -> usize {
    sql.value("SELECT count(*) FROM pg_stat_activity WHERE wait_event = 'SyncRep'")
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Waits until at least one commit is waiting.
fn wait_for_a_waiting_commit(sql: &Sql, context: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while commits_waiting(sql) == 0 {
        assert!(
            Instant::now() < deadline,
            "{context}: expected the change's commit to be waiting at the test Postgres"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

/// `GET /hello` of `board` as this User, not a page; `None` when there is
/// no answer at all (the program not listening).
fn try_open(vm: &Vm, user: &User) -> Option<Response> {
    vm.request(APP, "/hello")
        .as_user(user)
        .header("accept", "application/json")
        .timeout(Duration::from_secs(30))
        .try_send()
        .ok()
}

/// Whether the Application answered: its code's `200` naming the User.
fn answered_by_the_application(response: &Response, user: &User) -> bool {
    response.status == 200 && response.text().contains(&format!("Hello, {}!", user.id))
}

/// `GET /hello` as this User, which must be answered within [`QUICK`].
fn open_quickly(vm: &Vm, user: &User, context: &str) -> Response {
    let began = Instant::now();
    let response = try_open(vm, user)
        .unwrap_or_else(|| vm.fail(&format!("{context}: no answer for {:?}", user.id)));
    let took = began.elapsed();
    assert!(
        took < QUICK,
        "{context}: expected {:?} answered without waiting for the platform database, it took {took:?}\n{}",
        user.id,
        response.describe()
    );
    response
}

/// Fails unless this is the `503` of an Application whose lists the
/// program cannot tell: with `retry-after`.
fn assert_503(response: &Response, user: &User, context: &str) {
    assert!(
        response.status == 503 && response.header("retry-after").is_some(),
        "{context}: expected `503` with `retry-after` for {:?}, nobody let in on a guess, got\n{}",
        user.id,
        response.describe()
    );
}

/// The lists as alice (the creator) reads them through the API.
fn lists(vm: &Vm) -> Value {
    let answer = vm.api_as(&alice()).get_application(APP);
    answer.expect_status(200);
    answer.json()
}

/// Sends a change of sharing as alice while commits are held: the program
/// gives up on it and answers `503` `unavailable`.
fn change_lost(vm: &Vm, body: &Value) {
    let answer = vm
        .api_as(&alice())
        .request(&format!("/v1/applications/{APP}/sharing"))
        .method("POST")
        .json(body)
        .timeout(Duration::from_secs(60))
        .send();
    assert_api_error(&answer, 503, "unavailable");
}

/// Asks, for a few seconds, as each of `users`: each answer is the `503`,
/// given at once.
fn assert_503_for_a_while(vm: &Vm, users: &[User], context: &str) {
    for round in 0..5 {
        for user in users {
            let response = open_quickly(vm, user, &format!("{context}, round {round}"));
            assert_503(&response, user, &format!("{context}, round {round}"));
        }
        thread::sleep(Duration::from_millis(300));
    }
}

/// After commits go through: asks as `refused` and `opener` until
/// `refused` gets the `403` and `opener` opens it, within [`SETTLE`];
/// meanwhile `refused` is never answered by the Application and each
/// answer is the `503` or the settled one.
fn settle(vm: &Vm, refused: &User, opener: &User, context: &str) {
    let deadline = Instant::now() + SETTLE;
    loop {
        let r = open_quickly(vm, refused, context);
        assert!(
            !answered_by_the_application(&r, refused),
            "{context}: {:?} was answered by the Application\n{}",
            refused.id,
            r.describe()
        );
        assert!(
            r.status == 403 || r.status == 503,
            "{context}: expected `503` or `403` for {:?}, got\n{}",
            refused.id,
            r.describe()
        );
        let o = open_quickly(vm, opener, context);
        assert!(
            answered_by_the_application(&o, opener) || o.status == 503,
            "{context}: expected `503` or the Application for {:?}, got\n{}",
            opener.id,
            o.describe()
        );
        if r.status == 403 && o.status == 200 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{context}: expected the change settled within {SETTLE:?} once commits went through: {:?} `403`, {:?} opening it; last answers\n{}\n{}",
            refused.id,
            opener.id,
            r.describe(),
            o.describe()
        );
        thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn an_unshare_whose_answer_is_lost_lets_the_person_in_never_again() {
    let (postgres, _options, vm) = with_board("vm_platform_lost_unshare");
    share_ok(&vm.api_as(&alice()), &vm, APP, &add(&[user("bob")]));
    assert_opens(&vm, &bob(), APP, "bob, shared with");
    let sql = superuser(&postgres);

    hold_commits(&sql, &postgres);
    change_lost(&vm, &remove(&[user("bob")]));
    assert!(
        commits_waiting(&sql) > 0,
        "expected the unshare's commit still on its way after its `503`"
    );
    // From that answer on: `503` for everyone, at once, bob and alice
    // alike.
    assert_503_for_a_while(&vm, &[bob(), alice()], "while the commit waits");

    let_commits_through(&sql, &postgres);
    settle(&vm, &bob(), &alice(), "once commits go through");
    let after = lists(&vm);
    assert_eq!(
        after["shared_with"],
        json!([]),
        "the change was made after all"
    );
    for round in 0..5 {
        assert_cannot_open(&vm, &bob(), APP, &format!("bob, settled, round {round}"));
        assert_opens(
            &vm,
            &alice(),
            APP,
            &format!("alice, settled, round {round}"),
        );
    }
}

#[test]
fn a_share_whose_answer_is_lost_is_not_refused_for_ever() {
    let (postgres, _options, vm) = with_board("vm_platform_lost_share");
    assert_cannot_open(&vm, &carol(), APP, "carol, not shared with");
    let sql = superuser(&postgres);

    hold_commits(&sql, &postgres);
    change_lost(&vm, &add(&[user("carol")]));
    assert_503_for_a_while(&vm, &[carol(), alice()], "while the commit waits");

    let_commits_through(&sql, &postgres);
    let deadline = Instant::now() + SETTLE;
    loop {
        let response = open_quickly(&vm, &carol(), "once commits go through");
        if answered_by_the_application(&response, &carol()) {
            break;
        }
        assert!(
            response.status == 503,
            "expected `503` until the change is settled, then carol in, got\n{}",
            response.describe()
        );
        assert!(
            Instant::now() < deadline,
            "expected carol to open it within {SETTLE:?} once commits went through, got\n{}",
            response.describe()
        );
        thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(keys(&lists(&vm)["shared_with"]), vec!["user:carol"]);
    assert_opens(&vm, &alice(), APP, "alice, settled");
}

#[test]
fn a_program_started_while_an_unshare_is_on_its_way_never_lets_the_person_in() {
    let (postgres, options, vm) = with_board("vm_platform_lost_at_start");
    share_ok(&vm.api_as(&alice()), &vm, APP, &add(&[user("bob")]));
    assert_opens(&vm, &bob(), APP, "bob, shared with");
    let sql = superuser(&postgres);

    hold_commits(&sql, &postgres);
    // The unshare is sent; once its commit waits, the program is killed.
    let request = vm
        .api_as(&alice())
        .request(&format!("/v1/applications/{APP}/sharing"))
        .method("POST")
        .json(&remove(&[user("bob")]))
        .timeout(Duration::from_secs(60));
    thread::scope(|scope| {
        let sending = scope.spawn(|| request.try_send());
        wait_for_a_waiting_commit(&sql, "the unshare");
        drop(vm);
        // Whatever it got (most likely no answer at all).
        drop(sending.join());
    });

    // Started again while the commit is still on its way: it may not
    // start, not listen, or answer `503`; bob is never answered by the
    // Application.
    match Vm::try_start(&options, Duration::from_secs(20)) {
        Ok(vm) => {
            for round in 0..10 {
                if let Some(response) = try_open(&vm, &bob()) {
                    assert!(
                        !answered_by_the_application(&response, &bob()),
                        "round {round}: bob was let in by lists read while the unshare was on its way\n{}",
                        response.describe()
                    );
                }
                thread::sleep(Duration::from_millis(300));
            }
            assert!(
                commits_waiting(&sql) > 0,
                "expected the unshare's commit still waiting"
            );
            let_commits_through(&sql, &postgres);
            settle(
                &vm,
                &bob(),
                &alice(),
                "started meanwhile, once commits go through",
            );
            assert_eq!(lists(&vm)["shared_with"], json!([]));
        }
        Err(_) => {
            // It did not start while the commit waited: allowed. Once
            // commits go through, a program starts and serves by the lists
            // as the database has them.
            let_commits_through(&sql, &postgres);
            let vm = Vm::start(&options);
            settle(&vm, &bob(), &alice(), "started after commits go through");
            assert_eq!(lists(&vm)["shared_with"], json!([]));
        }
    }
}
