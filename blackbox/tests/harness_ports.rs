//! The harness's own checks of its starts on ports it chooses (no contract
//! section: how the harness lives beside some 1900 other tests). A port free
//! when the harness looked can be taken by something else before the
//! program binds it; the start that is refused it as taken goes on another
//! port, at most `PORT_TRIES` times, saying so on standard error. Nothing
//! else is tried again: a start refused for another reason fails as before.
//!
//! Each check hands the harness, as its first choice, a port the check
//! itself holds with a listener: the race, made certain.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::cell::Cell;
use std::fs;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::time::Duration;

use blackbox::dev::{
    Dev, PORT_TRIES, assert_run_not_listening, free_port, run_dev_choosing, run_listening_on,
    spawn_piped_listening_choosing,
};
use blackbox::install::docker_refused_port;
use blackbox::local::{Local, OUTSIDER, local_project};
use blackbox::ports::{LOWEST, NAMED, system_range};
use blackbox::vm::{Vm, VmOptions, refused_address};

/// A port of 127.0.0.1 held by a listener of the check's own.
fn held() -> (TcpListener, u16) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

/// A chooser that gives `first`, then free ports; and how many it gave.
fn first_then_free(first: u16) -> (impl FnMut() -> u16, Rc<Cell<usize>>) {
    let given = Rc::new(Cell::new(0));
    let counted = Rc::clone(&given);
    let choose = move || {
        counted.set(counted.get() + 1);
        if counted.get() == 1 {
            first
        } else {
            free_port()
        }
    };
    (choose, given)
}

#[test]
fn a_vibe_dev_start_on_a_port_taken_since_it_was_chosen_goes_on_another_port() {
    let local = Local::new();
    let project = local_project(&local.machine, "ports-board", &[]);
    for json in [true, false] {
        let (listener, taken) = held();
        let (choose, given) = first_then_free(taken);
        let dev = Dev::start_choosing(&local, &project.dir, "ports-board", json, choose);
        assert_ne!(
            dev.port, taken,
            "json {json}: expected `vibe dev` started on a port other than the one held"
        );
        assert_eq!(
            given.get(),
            2,
            "json {json}: expected one port refused and one more chosen"
        );
        assert!(
            dev.url().contains(&format!(":{}", dev.port)),
            "json {json}: expected the address to follow the port: {}",
            dev.url()
        );
        let mut browser = dev.browser();
        browser.choose(OUTSIDER);
        assert_eq!(browser.get("/").status, 200, "json {json}");
        // What listens on its port is the run's; on the port held, not.
        assert!(
            !run_listening_on(&local, dev.port).is_empty(),
            "json {json}: expected the run found listening on {}",
            dev.port
        );
        assert!(run_listening_on(&local, taken).is_empty(), "json {json}");
        let port = dev.port;
        dev.stop("INT").expect_code(0);
        assert_run_not_listening(&local, port, "after the stop");
        drop(listener);
    }
}

#[test]
fn a_vibe_dev_run_to_its_end_on_a_port_taken_since_it_was_chosen_goes_on_another_port() {
    // No Postgres of its own and a mirror where nothing listens: on the
    // port it is given, `vibe dev` then ends postgres-unavailable.
    let closed = {
        let (listener, port) = held();
        drop(listener);
        port
    };
    let local = Local::new()
        .without_bin()
        .with_mirror(&format!("http://127.0.0.1:{closed}"));
    let project = local_project(&local.machine, "ports-board-end", &[]);
    local.watch(&project.dir);
    let (listener, taken) = held();
    let (choose, given) = first_then_free(taken);
    let (port, run) = run_dev_choosing(&local, &project.dir, true, &[], choose);
    assert_ne!(port, taken, "{}", run.describe());
    assert_eq!(
        given.get(),
        2,
        "expected one port refused and one more chosen"
    );
    assert_ne!(
        run.json()["error"]["reason"],
        "port-in-use",
        "{}",
        run.describe()
    );
    assert_eq!(run.code, Some(1), "{}", run.describe());
    drop(listener);
}

#[test]
fn a_vibe_dev_piped_start_on_a_port_taken_since_it_was_chosen_goes_on_another_port() {
    let local = Local::new();
    let project = local_project(&local.machine, "ports-board-piped", &[]);
    local.watch(&project.dir);
    let (listener, taken) = held();
    let (choose, given) = first_then_free(taken);
    let (port, running, reader) = spawn_piped_listening_choosing(&local, &project.dir, choose);
    assert_ne!(port, taken);
    assert_eq!(
        given.get(),
        2,
        "expected one port refused and one more chosen"
    );
    running.signal("INT");
    drop(reader);
    let run = running.wait(Duration::from_secs(60));
    assert!(run.code.is_some(), "{}", run.describe());
    drop(listener);
}

#[test]
fn a_vibe_dev_start_refused_for_another_reason_is_not_tried_again() {
    let local = Local::new();
    let not_a_project = local.machine.root().join("not-a-project");
    fs::create_dir_all(&not_a_project).unwrap();

    let (choose, given) = first_then_free(free_port());
    let (_, run) = run_dev_choosing(&local, &not_a_project, true, &[], choose);
    run.json_error(1, "not-a-project");
    assert_eq!(given.get(), 1, "expected one port chosen, no other tried");

    for json in [true, false] {
        let (choose, given) = first_then_free(free_port());
        let failed = catch_unwind(AssertUnwindSafe(|| {
            Dev::start_choosing(&local, &not_a_project, "nothing", json, choose)
        }));
        let message = match failed {
            Ok(_) => panic!("json {json}: expected `vibe dev` outside a project not to listen"),
            Err(cause) => cause.downcast_ref::<String>().cloned().unwrap_or_default(),
        };
        assert!(
            message.contains("not-a-project") || message.to_lowercase().contains("project"),
            "json {json}: expected the failure to be `vibe dev`'s own, got {message}"
        );
        assert_eq!(
            given.get(),
            1,
            "json {json}: expected one port chosen, no other tried"
        );
    }
}

#[test]
fn a_vibe_dev_refused_its_port_every_time_fails_after_port_tries() {
    let local = Local::new();
    let project = local_project(&local.machine, "ports-board-full", &[]);
    let holders: Vec<(TcpListener, u16)> = (0..PORT_TRIES).map(|_| held()).collect();
    let ports: Vec<u16> = holders.iter().map(|(_, p)| *p).collect();
    let given = Rc::new(Cell::new(0));
    let counted = Rc::clone(&given);
    let choose = move || {
        let port = ports[counted.get() % ports.len()];
        counted.set(counted.get() + 1);
        port
    };
    let failed = catch_unwind(AssertUnwindSafe(|| {
        Dev::start_choosing(&local, &project.dir, "ports-board-full", true, choose)
    }));
    assert!(failed.is_err(), "expected the start to fail");
    assert_eq!(
        given.get(),
        PORT_TRIES,
        "expected exactly PORT_TRIES ports tried"
    );
    drop(holders);
}

#[test]
fn a_vibe_serve_start_on_a_port_taken_since_it_was_chosen_goes_on_another_port() {
    let (listener, taken) = held();
    let (mut choose, given) = first_then_free(taken);
    let vm = Vm::start_choosing(&VmOptions::new().stand_in_sign_in(), || {
        SocketAddr::from(([127, 0, 0, 1], choose()))
    });
    assert_ne!(vm.address().port(), taken);
    assert_eq!(
        given.get(),
        2,
        "expected one port refused and one more chosen"
    );
    drop(listener);
}

#[test]
fn a_vibe_serve_start_refused_for_another_reason_is_not_tried_again() {
    let (mut choose, given) = first_then_free(free_port());
    let failed = catch_unwind(AssertUnwindSafe(|| {
        Vm::start_choosing(
            &VmOptions::new()
                .stand_in_sign_in()
                .args_extra(&["--no-such-option"]),
            || SocketAddr::from(([127, 0, 0, 1], choose())),
        )
    }));
    assert!(
        failed.is_err(),
        "expected `vibe serve --no-such-option` not to start"
    );
    assert_eq!(given.get(), 1, "expected one port chosen, no other tried");
}

#[test]
fn what_counts_as_a_refused_port_is_the_programs_own_words_for_that_port() {
    let address = SocketAddr::from(([127, 0, 0, 1], 41693));
    // `vibe serve`'s words, seen on this machine (a probe holding the port).
    let serve = "expected `vibe serve` to write a `listening` record, but it exited (exit status: 1) before writing it.\nits standard error:\nvibe serve: cannot listen on 127.0.0.1:41693: Address already in use (os error 98)\n";
    assert!(refused_address(address, serve));
    assert!(!refused_address(
        SocketAddr::from(([127, 0, 0, 1], 41694)),
        serve
    ));
    assert!(!refused_address(
        address,
        "expected `vibe serve` to write a `listening` record, but it did not write it within 30s.\nvibe serve: cannot listen on 127.0.0.1:41693: Address already in use (os error 98)\n"
    ));
    // Docker's words, seen on this machine (`docker run -p` on a held port).
    let docker = "docker: Error response from daemon: failed to set up container networking: driver failed programming external connectivity on endpoint t68-port-probe (9bd7): failed to bind host port 127.0.0.1:53065/tcp: address already in use\n";
    assert!(docker_refused_port(docker, 53065));
    assert!(!docker_refused_port(docker, 5306));
    assert!(!docker_refused_port(
        "Error response from daemon: pull access denied for nothing\n",
        53065
    ));
}

#[test]
fn ports_chosen_for_later_are_outside_the_systems_range_distinct_and_none_named() {
    let range = system_range();
    if let Ok(text) = fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range") {
        let read: Vec<u16> = text
            .split_whitespace()
            .map(|n| n.parse().unwrap())
            .collect();
        assert_eq!(
            (*range.start(), *range.end()),
            (read[0], read[1]),
            "the harness's range is the system's"
        );
    }
    let mut seen = std::collections::HashSet::new();
    for _ in 0..300 {
        let port = free_port();
        assert!(port >= LOWEST, "{port} is privileged");
        assert!(
            !range.contains(&port),
            "{port} is inside the system's own range {range:?}"
        );
        assert!(!NAMED.contains(&port), "{port} is named on purpose");
        assert!(seen.insert(port), "{port} was handed out twice");
    }
    // The other choosers draw from the same ports, never one handed out.
    let other = blackbox::postgres::free_port().unwrap();
    assert!(!seen.contains(&other) && !range.contains(&other));
    let docker = blackbox::install::free_port();
    assert!(!seen.contains(&docker) && docker != other && !range.contains(&docker));
}

#[test]
fn something_else_on_a_port_is_not_taken_for_the_run_listening() {
    let local = Local::new();
    let (listener, port) = held();
    assert!(
        blackbox::local::port_open(port),
        "the control: something answers on {port}"
    );
    assert!(run_listening_on(&local, port).is_empty());
    assert_run_not_listening(&local, port, "a listener of the check's own");
    drop(listener);
}
