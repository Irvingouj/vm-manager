//! A connection in its handshake counts against the program's connection
//! bound (CONTRACT.md, "The program serves TLS itself: The two files": "with
//! as many connections held in their handshake as the program serves at
//! once (1,024), one more gets no handshake while they are held, and is
//! served once the program has ended them, so unfinished handshakes never
//! number more than the bound whatever a client opens, and the program is
//! reachable again at most 10 seconds after such a client began").
//!
//! A file of its own: it holds over 1,024 sockets, and so does the program
//! it starts, so it raises this process's soft limit of open files first
//! ([`ensure_open_files`], with `prlimit`; the program inherits it), and
//! fails in words when the hard limit does not allow it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::io::{ErrorKind, Read};
use std::net::TcpStream;
use std::thread;
use std::time::{Duration, Instant};

use blackbox::own_tls::{Serving, begin_handshake, ensure_open_files};
use blackbox::tls::api_name;

/// The connections the program serves at once.
const BOUND: usize = 1024;

/// Open files this process and the program need: the bound's sockets on
/// each side, and room for everything else (Postgres, logs, the harness).
const OPEN_FILES: u64 = 4096;

/// How long after the held connections were all open each may be ended
/// at the latest, counted from then and not from each one's `connect`:
/// the program's 10 seconds begin when it accepts a connection, and a
/// connection the client already believes open may reach the program a
/// retransmission later (the kernel drops the last step of a connection
/// when the listening queue is full and resends its SYN-ACK 1 s, then 3 s
/// later). So: the 10 seconds, the 3 s of two retransmissions, and 2 s for
/// a busy machine. The exact 10 seconds of one handshake is held by
/// `own_tls_serving.rs` (`a_handshake_left_unfinished_is_ended_...`).
const ENDED_WITHIN: Duration = Duration::from_secs(15);

/// The held connections are opened this many at a time, with [`PAUSE`]
/// between, so that the listening queue is not overfilled (which only
/// delays some by the kernel's retransmissions; the limits are wide for
/// that all the same).
const BATCH: usize = 100;
const PAUSE: Duration = Duration::from_millis(50);

/// How long the one more connection may wait to be served, from when the
/// held ones were all open. A place frees when the first connection the
/// program accepted is ended, 10 s after that accept, which came before
/// they were all open; the one more connection is opened a second after,
/// when the queue is empty again, and should its SYN be dropped all the
/// same, the client resends it 1 s, then 3 s later, still within the
/// 10 s it waits for a place. So 10 s, and 5 s for retransmissions and a
/// busy machine.
const SERVED_WITHIN: Duration = Duration::from_secs(15);

/// How long the one more connection must get no answer.
const NO_ANSWER_FOR: Duration = Duration::from_secs(3);

#[test]
fn handshakes_held_open_count_against_the_bound_and_one_more_is_served_once_they_are_ended() {
    ensure_open_files(OPEN_FILES);
    let serving = Serving::start();
    let address = serving.address();

    // The bound's connections, each sending nothing: each in its
    // handshake. Dropped (closed) whatever happens.
    let began = Instant::now();
    let mut held: Vec<(TcpStream, Instant)> = Vec::with_capacity(BOUND);
    for n in 0..BOUND {
        if n > 0 && n % BATCH == 0 {
            thread::sleep(PAUSE);
        }
        let socket = TcpStream::connect_timeout(&address, Duration::from_secs(5))
            .unwrap_or_else(|e| panic!("could not open held connection {n} of {BOUND}: {e}"));
        held.push((socket, Instant::now()));
    }
    let all_open = Instant::now();
    eprintln!(
        "{BOUND} connections that send nothing opened in {:?}",
        all_open - began
    );
    thread::sleep(Duration::from_secs(1));

    // One more, beginning a real handshake: no answer while they are held.
    let mut more = begin_handshake(address, &serving.hello())
        .unwrap_or_else(|e| panic!("could not begin one more handshake: {e}"));
    let answered = more.answered_within(NO_ANSWER_FOR).unwrap_or_else(|e| {
        panic!(
            "expected one more connection to wait while {BOUND} are held in their handshake: {e}"
        )
    });
    assert!(
        !answered,
        "expected no answer to one more ClientHello for {NO_ANSWER_FOR:?} while {BOUND} connections are held in their handshake (a connection in its handshake counts against the bound); it was answered {:?} after the {BOUND} were open",
        all_open.elapsed()
    );

    // Served once the program has ended the held ones.
    let left = (all_open + SERVED_WITHIN).saturating_duration_since(Instant::now());
    let mut shaken = more.finish(left).unwrap_or_else(|e| {
        panic!(
            "expected one more connection served once the held ones are ended, within {SERVED_WITHIN:?} of their opening: {e} ({:?} after)",
            all_open.elapsed()
        )
    });
    assert_eq!(
        shaken.fingerprint(),
        serving.pair.fingerprint(),
        "expected the program's certificate"
    );
    let (status, body) = shaken
        .get(&api_name(), "/v1/installation")
        .unwrap_or_else(|e| panic!("expected an answer on the connection served at last: {e}"));
    let served_after = all_open.elapsed();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    eprintln!(
        "one more connection: handshake and `GET /v1/installation` answered {served_after:?} after the {BOUND} held ones were open"
    );
    assert!(
        served_after <= SERVED_WITHIN,
        "expected one more connection served within {SERVED_WITHIN:?} of the held ones being opened, it was after {served_after:?}"
    );

    // Every held connection was ended by the program, none later than
    // ENDED_WITHIN after they were all open (see there why it is counted
    // so).
    for (n, (socket, opened)) in held.iter_mut().enumerate() {
        let wait = (all_open + ENDED_WITHIN)
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1));
        socket.set_read_timeout(Some(wait)).unwrap();
        let mut buffer = [0_u8; 512];
        let mut came = 0;
        let ended = loop {
            match socket.read(&mut buffer) {
                Ok(0) => break Ok(()),
                // Whatever the program sends (an alert, say) before it closes.
                Ok(k) if came < 4096 => came += k,
                Ok(_) => break Err("kept sending".to_owned()),
                Err(e) if e.kind() == ErrorKind::ConnectionReset => break Ok(()),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    break Err("still open".to_owned());
                }
                Err(e) => break Err(e.to_string()),
            }
        };
        let after = all_open.elapsed();
        let since_its_connect = opened.elapsed();
        if let Err(how) = ended {
            panic!(
                "expected held connection {n} ended by the program within {ENDED_WITHIN:?} of all {BOUND} being open; {after:?} after ({since_its_connect:?} after its own connect), it is {how}"
            );
        }
    }
    eprintln!(
        "all {BOUND} held connections were ended by the program within {ENDED_WITHIN:?} of all being open (all seen ended {:?} after)",
        all_open.elapsed()
    );
}
