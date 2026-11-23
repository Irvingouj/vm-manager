//! How many calls are under way at once (blackbox/CONTRACT.md, "Outbound
//! calls: Bounds"): 8 for one request, of which 64 more may wait their
//! turn (a 73rd is `unavailable` at once), and 256 in the program, where a
//! call that finds no place waits at most [`PROGRAM_WAIT`].

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Calls of one request under way at once.
pub const PER_REQUEST: usize = 8;

/// Calls of one request that may wait for their turn beside those under
/// way: the 73rd call of a request is answered `unavailable` at once.
pub const WAITING: usize = 64;

/// Calls under way in the program.
pub const IN_PROGRAM: usize = 256;

/// Names being resolved at once in the program, a lookup given up but not
/// yet ended among them.
pub const LOOKUPS: usize = 64;

/// Of them, at most this many for one Application, so that one whose names
/// do not resolve cannot take every place from the others.
pub const LOOKUPS_PER_APPLICATION: usize = 16;

/// How long a call waits for a place among [`IN_PROGRAM`].
pub const PROGRAM_WAIT: Duration = Duration::from_secs(5);

/// The calls of one request: its 8 turns and the count of calls it has
/// (under way or waiting), at most 72.
#[derive(Debug)]
pub struct Turns {
    turns: Arc<Semaphore>,
    enrolled: Arc<AtomicUsize>,
}

impl Default for Turns {
    fn default() -> Self {
        Self {
            turns: Arc::new(Semaphore::new(PER_REQUEST)),
            enrolled: Arc::new(AtomicUsize::new(0)),
        }
    }
}

/// One call's place in its request's count, given back when it drops.
#[derive(Debug)]
pub struct Enrolled {
    turns: Arc<Semaphore>,
    enrolled: Arc<AtomicUsize>,
}

impl Turns {
    /// Counts one more call of the request, or `None` when it already has
    /// 72 (the 73rd is `unavailable`).
    pub fn enroll(&self) -> Option<Enrolled> {
        let mut now = self.enrolled.load(Ordering::SeqCst);
        loop {
            if now >= PER_REQUEST + WAITING {
                return None;
            }
            match self
                .enrolled
                .compare_exchange(now, now + 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => {
                    return Some(Enrolled {
                        turns: Arc::clone(&self.turns),
                        enrolled: Arc::clone(&self.enrolled),
                    });
                }
                Err(seen) => now = seen,
            }
        }
    }
}

impl Enrolled {
    /// Waits for one of the request's 8 turns. The caller ends the wait by
    /// dropping this future (the request's end).
    pub async fn turn(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.turns).acquire_owned().await.ok()
    }
}

impl Drop for Enrolled {
    fn drop(&mut self) {
        self.enrolled.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The program's 256 places.
#[derive(Debug)]
pub struct Program {
    places: Arc<Semaphore>,
    lookups: Lookups,
}

impl Default for Program {
    fn default() -> Self {
        Self {
            places: Arc::new(Semaphore::new(IN_PROGRAM)),
            lookups: Lookups::new(LOOKUPS, LOOKUPS_PER_APPLICATION),
        }
    }
}

/// The system's resolver (`getaddrinfo`), at most `most` addresses.
fn system_resolver(
    name: &str,
    port: u16,
    most: usize,
) -> Result<Vec<std::net::IpAddr>, NotResolved> {
    std::net::ToSocketAddrs::to_socket_addrs(&(name, port))
        .map(|found| found.map(|socket| socket.ip()).take(most).collect())
        .map_err(|_| NotResolved::Failed)
}

/// What one Application holds of the lookups: its own places, and how many
/// lookups (waiting or under way) count on the entry, which goes when none
/// does.
#[derive(Debug)]
struct Slot {
    places: Arc<Semaphore>,
    users: usize,
}

/// The names being resolved: at most `total` in the program and at most
/// `per_application` for one Application, a lookup given up but not yet
/// ended among them.
#[derive(Debug)]
pub struct Lookups {
    total: Arc<Semaphore>,
    per_application: usize,
    /// One entry for each Application with a lookup waiting or under way;
    /// taken only for a moment, never across an await. At most as many
    /// entries as lookups wait or run (the program's calls are bounded).
    applications: Arc<std::sync::Mutex<std::collections::HashMap<String, Slot>>>,
}

/// An Application's count on its entry in [`Lookups`], given back when
/// dropped (the entry goes with the last one).
struct Counted {
    applications: Arc<std::sync::Mutex<std::collections::HashMap<String, Slot>>>,
    application: String,
}

impl Drop for Counted {
    fn drop(&mut self) {
        let mut applications = self
            .applications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(slot) = applications.get_mut(&self.application) {
            slot.users = slot.users.saturating_sub(1);
            if slot.users == 0 {
                applications.remove(&self.application);
            }
        }
    }
}

impl Lookups {
    pub fn new(total: usize, per_application: usize) -> Self {
        Self {
            total: Arc::new(Semaphore::new(total)),
            per_application,
            applications: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// How many Applications have an entry (for the tests).
    #[cfg(test)]
    fn entries(&self) -> usize {
        self.applications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Counts a lookup of `application` and returns its places' semaphore.
    fn count(&self, application: &str) -> (Counted, Arc<Semaphore>) {
        let mut applications = self
            .applications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = applications
            .entry(application.to_owned())
            .or_insert_with(|| Slot {
                places: Arc::new(Semaphore::new(self.per_application)),
                users: 0,
            });
        slot.users += 1;
        let places = Arc::clone(&slot.places);
        (
            Counted {
                applications: Arc::clone(&self.applications),
                application: application.to_owned(),
            },
            places,
        )
    }

    /// The addresses `resolver` finds for `name`, at most `most`, before
    /// `by`. A call that finds no place, among its Application's and the
    /// program's, before `by` is [`NotResolved::Time`].
    ///
    /// The lookup is a blocking call (`getaddrinfo`) that nothing can
    /// cancel, run on the blocking pool by `spawn_blocking`. Its closure
    /// owns both places and the Application's count: a lookup given up at
    /// `by` keeps them (and its thread) until the resolver returns, so that
    /// abandoned lookups are counted and cannot pile up. Its handle is not
    /// kept beyond `by`, because the task ends by itself (the resolver's own
    /// time) and owns nothing but its places and the name.
    pub async fn resolve<R>(
        &self,
        application: &str,
        name: &str,
        port: u16,
        most: usize,
        by: tokio::time::Instant,
        resolver: R,
    ) -> Result<Vec<std::net::IpAddr>, NotResolved>
    where
        R: FnOnce(&str, u16, usize) -> Result<Vec<std::net::IpAddr>, NotResolved> + Send + 'static,
    {
        let (counted, places) = self.count(application);
        // The Application's own place first: one that is at its 16 does not
        // wait holding one of the program's.
        let own = tokio::time::timeout_at(by, places.acquire_owned())
            .await
            .map_err(|_| NotResolved::Time)?
            .map_err(|_| NotResolved::Failed)?;
        let place = tokio::time::timeout_at(by, Arc::clone(&self.total).acquire_owned())
            .await
            .map_err(|_| NotResolved::Time)?
            .map_err(|_| NotResolved::Failed)?;
        let name = name.to_owned();
        let lookup = tokio::task::spawn_blocking(move || {
            let found = resolver(name.as_str(), port, most);
            drop(place);
            drop(own);
            drop(counted);
            found
        });
        match tokio::time::timeout_at(by, lookup).await {
            Ok(Ok(found)) => found,
            Ok(Err(_)) => Err(NotResolved::Failed),
            Err(_) => Err(NotResolved::Time),
        }
    }
}

/// Why a name was not resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotResolved {
    /// No room among the 64 lookups, or no answer, by the time given.
    Time,
    /// The system's resolver found no address.
    Failed,
}

impl Program {
    /// The addresses of `name` for a call of `application`, at most
    /// `most`, resolved by the system before `by`; see [`Lookups::resolve`].
    pub async fn resolve(
        &self,
        application: &str,
        name: &str,
        port: u16,
        most: usize,
        by: tokio::time::Instant,
    ) -> Result<Vec<std::net::IpAddr>, NotResolved> {
        self.lookups
            .resolve(application, name, port, most, by, system_resolver)
            .await
    }

    /// A place, waiting up to [`PROGRAM_WAIT`]; `None` when there is none.
    pub async fn place(&self) -> Option<OwnedSemaphorePermit> {
        tokio::time::timeout(PROGRAM_WAIT, Arc::clone(&self.places).acquire_owned())
            .await
            .ok()?
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_has_at_most_seventy_two_calls_and_a_given_back_place_is_free_again() {
        let turns = Turns::default();
        let held: Vec<_> = (0..PER_REQUEST + WAITING).map(|_| turns.enroll()).collect();
        assert!(held.iter().all(Option::is_some), "72 calls are counted");
        assert!(turns.enroll().is_none(), "the 73rd is not");
        drop(held);
        assert!(turns.enroll().is_some(), "places given back are free");
    }

    #[tokio::test]
    async fn eight_calls_are_under_way_and_the_ninth_waits_for_a_turn() {
        let turns = Turns::default();
        let calls: Vec<_> = (0..9).filter_map(|_| turns.enroll()).collect();
        assert_eq!(calls.len(), 9);
        let mut taken = Vec::new();
        for call in calls.iter().take(PER_REQUEST) {
            taken.push(call.turn().await);
        }
        let ninth = calls.iter().last();
        let waited = match ninth {
            Some(call) => tokio::time::timeout(Duration::from_millis(50), call.turn())
                .await
                .is_err(),
            None => false,
        };
        assert!(waited, "the ninth waits");
        drop(taken);
    }

    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::mpsc as std_mpsc;
    use std::time::Instant as Wall;

    /// What releases a hanging resolver: sent to (or dropped), it answers.
    type Release = std_mpsc::SyncSender<()>;

    fn answering(_: &str, _: u16, _: usize) -> Result<Vec<IpAddr>, NotResolved> {
        Ok(vec![IpAddr::V4(Ipv4Addr::new(9, 9, 9, 9))])
    }

    fn soon(millis: u64) -> tokio::time::Instant {
        tokio::time::Instant::now() + Duration::from_millis(millis)
    }

    /// Starts a lookup of `application` that hangs until released, and
    /// gives up on it at once (as a call whose time ran out does).
    async fn hang(lookups: &Arc<Lookups>, application: &str) -> Release {
        let (release, wait) = std_mpsc::sync_channel::<()>(1);
        // A resolver that hangs until released, then answers one address.
        let resolver = move |_: &str, _: u16, _: usize| {
            wait.recv().ok();
            Ok(vec![IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))])
        };
        let lookups = Arc::clone(lookups);
        let application = application.to_owned();
        let call = tokio::spawn(async move {
            lookups
                .resolve(&application, "x.example.com", 443, 16, soon(50), resolver)
                .await
        });
        // The caller gives up when its time runs out; the lookup stays.
        assert_eq!(call.await.unwrap(), Err(NotResolved::Time));
        release
    }

    #[tokio::test]
    async fn an_application_has_sixteen_lookups_and_the_seventeenth_waits_its_time_for_room() {
        let lookups = Arc::new(Lookups::new(LOOKUPS, LOOKUPS_PER_APPLICATION));
        let mut held = Vec::new();
        for _ in 0..16 {
            held.push(hang(&lookups, "a").await);
        }
        // The 17th of A finds no room: `Time` after its time, not before.
        let began = Wall::now();
        let seventeenth = lookups
            .resolve("a", "x.example.com", 443, 16, soon(300), answering)
            .await;
        assert_eq!(seventeenth, Err(NotResolved::Time));
        assert!(
            began.elapsed() >= Duration::from_millis(290),
            "it waited for its time, not less: {:?}",
            began.elapsed()
        );
        // Meanwhile B is answered at once.
        let began = Wall::now();
        let other = lookups
            .resolve("b", "x.example.com", 443, 16, soon(5_000), answering)
            .await;
        assert!(other.is_ok(), "B is answered");
        assert!(began.elapsed() < Duration::from_millis(250));
        drop(held);
    }

    #[tokio::test]
    async fn sixty_four_lookups_across_four_applications_leave_no_room_for_a_fifth() {
        let lookups = Arc::new(Lookups::new(LOOKUPS, LOOKUPS_PER_APPLICATION));
        let mut held = Vec::new();
        for application in ["a", "b", "c", "d"] {
            for _ in 0..16 {
                held.push(hang(&lookups, application).await);
            }
        }
        let fifth = lookups
            .resolve("e", "x.example.com", 443, 16, soon(200), answering)
            .await;
        assert_eq!(fifth, Err(NotResolved::Time), "the program's 64 are held");
        drop(held);
    }

    #[tokio::test]
    async fn a_lookup_given_up_keeps_its_places_until_the_resolver_returns_and_nothing_is_left() {
        let lookups = Arc::new(Lookups::new(2, 1));
        let release = hang(&lookups, "a").await;
        // Given up, still counted for A: A's next finds no room.
        assert_eq!(
            lookups
                .resolve("a", "x", 443, 16, soon(100), answering)
                .await,
            Err(NotResolved::Time)
        );
        assert_eq!(lookups.entries(), 1, "A's entry is kept while it is held");
        // The resolver returns: both counts are free again.
        release.send(()).unwrap();
        let mut freed = false;
        for _ in 0..100 {
            if lookups
                .resolve("a", "x", 443, 16, soon(100), answering)
                .await
                .is_ok()
            {
                freed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(freed, "A's place and the program's came back");
        let mut gone = false;
        for _ in 0..100 {
            if lookups.entries() == 0 {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(gone, "no entry is left once the count is zero");
    }
}
