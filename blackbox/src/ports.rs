//! Ports the harness chooses for later: to hand to a program that binds it
//! afterwards (`vibe dev --port`, `vibe serve --listen`, a Postgres, a port
//! Docker publishes), or to keep across a restart, or to name as one where
//! nothing listens.
//!
//! Never one the system hands out itself: every `:0` bind and every outgoing
//! connection of the tests beside this one takes its port from the system's
//! own range (`/proc/sys/net/ipv4/ip_local_port_range` on Linux, 32768 to
//! 60999 by default; `net.inet.ip.portrange.first`/`last` on macOS, 49152
//! to 65535 by default), so a port from there, free when looked at, is soon
//! someone's. A chosen port is drawn at random from above 1023 and outside
//! that range, away from the ports the product, the tests and Docker name
//! ([`NAMED`]), checked that nothing listens on it and that it can be bound,
//! and never handed out twice in one test process. What is left (another
//! test process drawing the same number at the same moment) is what the
//! retries of the starts on a chosen port cover (`crate::dev::PORT_TRIES`).

use std::collections::HashSet;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::ops::RangeInclusive;
use std::process::Command;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

use ring::rand::{SecureRandom, SystemRandom};

/// Ports named on purpose, never chosen: the product's (`vibe dev` without
/// `--port` 7480; the image's 8080 and 8443; HTTPS 443; Postgres 5432), the
/// tests' own texts (4000, 4430, 5000), and Docker's (2375, 2376 the
/// daemon, 2377 and 7946 a swarm, 4789 its overlay networks, 5000 a
/// registry).
pub const NAMED: [u16; 13] = [
    443, 2375, 2376, 2377, 4000, 4430, 4789, 5000, 5432, 7480, 7946, 8080, 8443,
];

/// The lowest port chosen: above the privileged ones.
pub const LOWEST: u16 = 1024;

/// How many draws one choice makes before it gives up.
const DRAWS: usize = 2000;

/// How long the look for a listener waits.
const LOOK: Duration = Duration::from_millis(200);

/// The system's own range of ports it hands out, read once.
pub fn system_range() -> RangeInclusive<u16> {
    static RANGE: OnceLock<RangeInclusive<u16>> = OnceLock::new();
    RANGE.get_or_init(read_system_range).clone()
}

fn read_system_range() -> RangeInclusive<u16> {
    if let Ok(text) = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range") {
        let mut numbers = text
            .split_whitespace()
            .filter_map(|n| n.parse::<u16>().ok());
        if let (Some(first), Some(last)) = (numbers.next(), numbers.next()) {
            return first..=last;
        }
    }
    // macOS (and anything else without that file): what sysctl says, else
    // its default.
    match (
        sysctl("net.inet.ip.portrange.first"),
        sysctl("net.inet.ip.portrange.last"),
    ) {
        (Some(first), Some(last)) if first <= last => first..=last,
        _ => 49152..=65535,
    }
}

fn sysctl(name: &str) -> Option<u16> {
    let mut command = Command::new("sysctl");
    command.args(["-n", name]);
    let finished = crate::program::run_to_exit(command, Duration::from_secs(5)).ok()?;
    if !finished.status.is_some_and(|s| s.success()) {
        return None;
    }
    finished.stdout.trim().parse().ok()
}

/// Whether the harness may choose `port` at all: above 1023, outside the
/// system's range, not named.
pub fn may_choose(port: u16) -> bool {
    port >= LOWEST && !system_range().contains(&port) && !NAMED.contains(&port)
}

/// A port chosen for later, as the module's text says. Fails when none is
/// found in [`DRAWS`] draws.
pub fn chosen() -> Result<u16, String> {
    // The ports handed out in this process: at most the ports there are.
    static GIVEN: OnceLock<Mutex<HashSet<u16>>> = OnceLock::new();
    let given = GIVEN.get_or_init(|| Mutex::new(HashSet::new()));
    let candidates: Vec<u16> = (LOWEST..=u16::MAX).filter(|p| may_choose(*p)).collect();
    let count = u32::try_from(candidates.len()).map_err(|e| e.to_string())?;
    if count == 0 {
        return Err(format!(
            "no port above {} outside the system's range {:?} to choose from",
            LOWEST - 1,
            system_range()
        ));
    }
    let random = SystemRandom::new();
    for _ in 0..DRAWS {
        let mut bytes = [0_u8; 4];
        random
            .fill(&mut bytes)
            .map_err(|_| "no randomness to choose a port with".to_owned())?;
        let index =
            usize::try_from(u32::from_le_bytes(bytes) % count).map_err(|e| e.to_string())?;
        let Some(&port) = candidates.get(index) else {
            continue;
        };
        // Taken in the set before it is looked at, so that two threads of
        // this process never both take it.
        if !given
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(port)
        {
            continue;
        }
        if free(port) {
            return Ok(port);
        }
    }
    Err(format!(
        "found no free port outside the system's range {:?} in {DRAWS} draws",
        system_range()
    ))
}

/// Nothing listens on `port` of 127.0.0.1, and it can be bound on every
/// IPv4 address (and on `::1` where the machine has it).
fn free(port: u16) -> bool {
    let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    if TcpStream::connect_timeout(&loopback, LOOK).is_ok() {
        return false;
    }
    if TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).is_err() {
        return false;
    }
    match TcpListener::bind((Ipv6Addr::LOCALHOST, port)) {
        Ok(_) => true,
        Err(e) => e.kind() != ErrorKind::AddrInUse,
    }
}
