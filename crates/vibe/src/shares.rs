//! What one Application may hold of a bound the program shares out
//! (blackbox/CONTRACT.md, "Resources and Grants in full: An Application's
//! share of what the program holds"): a cap for each Application, taken
//! before the program's own bound and held with it.
//!
//! [`Places`] is a share of a count — open Storage bodies, file jobs
//! waiting for a thread, outbound calls under way. [`Bytes`] is a share of
//! bytes — the arguments of host calls not yet answered — as a plain map,
//! held inside the lock that also holds the program's bytes
//! ([`crate::gateway::ProgramCalls`]), so a call is decided by one lock and
//! one refusal. Both keep one small entry for each Application that holds
//! or waits for something, gone when it holds nothing; taken only under a
//! `std::sync::Mutex`, never across an await. An operation takes its
//! Application's place first, and holds none of the program's while it
//! waits for it: an Application at its share takes nothing from the others
//! by asking for more.
//!
//! A refusal for the share is answered as one for the program's bound is,
//! and [`Mentions`] writes the program's one `share-full` record for it:
//! the first for an Application and kind, then none for a minute; at most
//! [`MENTIONS`] pairs are remembered, past which nothing is written until
//! one is a minute old. A refusal for the program's bound writes none.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::json;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant as Deadline;

use crate::log;

/// How long one refusal for an Application and kind is remembered: the
/// next writes no record before it has passed.
pub const MENTION_EVERY: Duration = Duration::from_secs(60);

/// The most (Application, kind) pairs remembered for the one-a-minute
/// rule; past it nothing is written until one is a minute old.
pub const MENTIONS: usize = 1_024;

/// The bound a share is of: what the `share-full` record names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Storage bodies open at once.
    StorageBodies,
    /// Storage file jobs waiting for a file thread.
    FileJobs,
    /// Outbound calls under way.
    OutboundCalls,
    /// Bytes in the arguments of host calls not yet answered.
    CallArguments,
}

impl Kind {
    /// The word the record gives this kind.
    pub const fn name(self) -> &'static str {
        match self {
            Self::StorageBodies => "storage-bodies",
            Self::FileJobs => "file-jobs",
            Self::OutboundCalls => "outbound-calls",
            Self::CallArguments => "call-arguments",
        }
    }

    /// Which of the maps of [`Said`] this kind is remembered in.
    const fn index(self) -> usize {
        match self {
            Self::StorageBodies => 0,
            Self::FileJobs => 1,
            Self::OutboundCalls => 2,
            Self::CallArguments => 3,
        }
    }
}

/// When each (Application, kind) pair was last written: one map for each
/// kind, looked up by the Application without allocating, and the number of
/// pairs remembered in all.
#[derive(Debug)]
struct Said {
    by_kind: [HashMap<Arc<str>, Instant>; 4],
    len: usize,
}

impl Default for Said {
    fn default() -> Self {
        Self {
            by_kind: std::array::from_fn(|_| HashMap::new()),
            len: 0,
        }
    }
}

/// The `share-full` records written, by Application and kind.
#[derive(Debug, Default)]
pub struct Mentions {
    /// Locked only to decide and write, never across an await. At most
    /// [`MENTIONS`] entries.
    said: Mutex<Said>,
}

impl Mentions {
    /// Writes the `share-full` record of this refusal when it is the first
    /// for `application` and `kind` within the minute; names no User,
    /// Resource or host.
    pub fn record(&self, application: &Arc<str>, kind: Kind) {
        if self.refused(application, kind) {
            log::record(
                "share-full",
                json!({ "application": application.as_ref(), "what": kind.name() }),
            );
        }
    }

    /// Whether a refusal of `application` for `kind` writes a record now,
    /// and remembering it when it does: the first for the pair, then none
    /// for a minute, and at most [`MENTIONS`] pairs remembered.
    pub fn refused(&self, application: &Arc<str>, kind: Kind) -> bool {
        self.decide(application.as_ref(), kind, Instant::now(), || {
            Arc::clone(application)
        })
    }

    /// [`Mentions::refused`] as of `now`, for the tests.
    #[cfg(test)]
    fn refused_at(&self, application: &str, kind: Kind, now: Instant) -> bool {
        self.decide(application, kind, now, || Arc::from(application))
    }

    /// The decision, with the Application's name made only when the pair is
    /// first remembered.
    fn decide(
        &self,
        application: &str,
        kind: Kind,
        now: Instant,
        keep: impl FnOnce() -> Arc<str>,
    ) -> bool {
        let mut said = self.said.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(map) = said.by_kind.get_mut(kind.index()) else {
            return false;
        };
        if let Some(at) = map.get_mut(application) {
            if now.saturating_duration_since(*at) < MENTION_EVERY {
                return false;
            }
            *at = now;
            return true;
        }
        if said.len >= MENTIONS {
            // No room for another pair: one a minute old makes room, and
            // while none is, nothing is written.
            let expired = said.by_kind.iter().enumerate().find_map(|(index, map)| {
                map.iter()
                    .find(|(_, at)| now.saturating_duration_since(**at) >= MENTION_EVERY)
                    .map(|(key, _)| (index, Arc::clone(key)))
            });
            let Some((index, key)) = expired else {
                return false;
            };
            if let Some(map) = said.by_kind.get_mut(index) {
                map.remove(key.as_ref());
                said.len = said.len.saturating_sub(1);
            } else {
                return false;
            }
        }
        let Some(map) = said.by_kind.get_mut(kind.index()) else {
            return false;
        };
        map.insert(keep(), now);
        said.len = said.len.saturating_add(1);
        true
    }

    /// How many pairs are remembered (for the tests).
    #[cfg(test)]
    pub(crate) fn remembered(&self) -> usize {
        self.said.lock().unwrap_or_else(PoisonError::into_inner).len
    }
}

/// What one Application holds of a count, and how many operations (waiting
/// or holding) count on its entry.
#[derive(Debug)]
struct Slot {
    places: Arc<Semaphore>,
    users: usize,
}

/// A share of a count: `cap` places for each Application, taken before the
/// program's own bound. [`Places::place`] waits for one until its deadline;
/// [`Places::try_place`] takes one only if it is free now.
#[derive(Debug)]
pub struct Places {
    cap: usize,
    /// One entry for each Application with an operation waiting or holding;
    /// taken only for a moment, never across an await. At most as many
    /// entries as operations wait or hold (the program's bounds are
    /// bounded).
    entries: Arc<Mutex<HashMap<Arc<str>, Slot>>>,
    mention: Option<(Arc<Mentions>, Kind)>,
}

impl Places {
    /// A share of `cap` places that writes no record (the lookups', and the
    /// tests').
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            entries: Arc::new(Mutex::new(HashMap::new())),
            mention: None,
        }
    }

    /// A share of `cap` places whose refusal writes one `share-full`
    /// record for `kind`.
    pub fn shared(cap: usize, mentions: Arc<Mentions>, kind: Kind) -> Self {
        Self {
            cap,
            entries: Arc::new(Mutex::new(HashMap::new())),
            mention: Some((mentions, kind)),
        }
    }

    /// Counts an operation of `application` and returns its places'
    /// semaphore: its entry stays while the count is above zero. The
    /// Application's name is cloned only when its entry is made, or for
    /// the count that keeps it.
    fn count(&self, application: &Arc<str>) -> (Counted, Arc<Semaphore>) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(slot) = entries.get_mut(application.as_ref()) {
            slot.users = slot.users.saturating_add(1);
            let places = Arc::clone(&slot.places);
            return (
                Counted {
                    entries: Arc::clone(&self.entries),
                    application: Arc::clone(application),
                },
                places,
            );
        }
        let places = Arc::new(Semaphore::new(self.cap));
        entries.insert(
            Arc::clone(application),
            Slot {
                places: Arc::clone(&places),
                users: 1,
            },
        );
        (
            Counted {
                entries: Arc::clone(&self.entries),
                application: Arc::clone(application),
            },
            places,
        )
    }

    /// A place of `application` now, without waiting; `None` when it is at
    /// its share. No record is written: the caller may go on to wait. The
    /// entry and its permit are taken under one lock, and an entry made for
    /// a place that was not free goes with it.
    pub fn try_place(&self, application: &Arc<str>) -> Option<Place> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let made = if entries.contains_key(application.as_ref()) {
            false
        } else {
            entries.insert(
                Arc::clone(application),
                Slot {
                    places: Arc::new(Semaphore::new(self.cap)),
                    users: 0,
                },
            );
            true
        };
        let acquired = {
            let slot = entries.get_mut(application.as_ref())?;
            let acquired = Arc::clone(&slot.places).try_acquire_owned();
            if acquired.is_ok() {
                slot.users = slot.users.saturating_add(1);
            }
            acquired
        };
        match acquired {
            Ok(place) => Some(Place {
                _place: place,
                _counted: Counted {
                    entries: Arc::clone(&self.entries),
                    application: Arc::clone(application),
                },
            }),
            Err(_) => {
                if made {
                    entries.remove(application.as_ref());
                }
                None
            }
        }
    }

    /// A place of `application` before `by`; `None` when its share is full
    /// by then, when one `share-full` record is written. When both are
    /// full, the refusal is the share's: the Application's place is asked
    /// for first.
    pub async fn place(&self, application: &Arc<str>, by: Deadline) -> Option<Place> {
        let (counted, places) = self.count(application);
        match tokio::time::timeout_at(by, places.acquire_owned()).await {
            Ok(Ok(place)) => Some(Place {
                _place: place,
                _counted: counted,
            }),
            Ok(Err(_)) => None,
            Err(_) => {
                drop(counted);
                self.refused(application);
                None
            }
        }
    }

    /// Says that an operation of `application` is refused for this share:
    /// one `share-full` record for the first within the minute.
    pub fn refused(&self, application: &Arc<str>) {
        if let Some((mentions, kind)) = &self.mention {
            mentions.record(application, *kind);
        }
    }

    /// How many Applications have an entry (for the tests).
    #[cfg(test)]
    pub(crate) fn entries(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

/// One Application's count on its entry in a [`Places`], given back when
/// dropped (the entry goes with the last one).
#[derive(Debug)]
struct Counted {
    entries: Arc<Mutex<HashMap<Arc<str>, Slot>>>,
    application: Arc<str>,
}

impl Drop for Counted {
    fn drop(&mut self) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(slot) = entries.get_mut(self.application.as_ref()) {
            slot.users = slot.users.saturating_sub(1);
            if slot.users == 0 {
                entries.remove(self.application.as_ref());
            }
        }
    }
}

/// One place of an Application's share, given back when dropped. The permit
/// is declared first so that it goes before the count: the entry is not
/// removed while a permit of it is still held.
#[derive(Debug)]
pub struct Place {
    _place: OwnedSemaphorePermit,
    _counted: Counted,
}

/// A share of bytes: `cap` bytes for each Application, as a plain map with
/// no lock of its own, held by [`crate::gateway::ProgramCalls`] beside the
/// program's bytes so that one lock decides both.
#[derive(Debug)]
pub struct Bytes {
    cap: u64,
    /// The bytes each Application holds; an entry goes when it holds none.
    held: HashMap<Arc<str>, u64>,
}

impl Bytes {
    pub fn new(cap: u64) -> Self {
        Self {
            cap,
            held: HashMap::new(),
        }
    }

    /// Whether `size` bytes of `application` would be taken, taking
    /// nothing. Exactly the cap is taken.
    pub fn would_take(&self, application: &Arc<str>, size: u64) -> bool {
        size == 0
            || self
                .held
                .get(application.as_ref())
                .copied()
                .unwrap_or(0)
                .saturating_add(size)
                <= self.cap
    }

    /// Takes `size` bytes of `application`, which must be within the cap
    /// ([`Bytes::would_take`]).
    pub fn take(&mut self, application: &Arc<str>, size: u64) {
        if size == 0 {
            return;
        }
        if let Some(now) = self.held.get_mut(application.as_ref()) {
            *now = now.saturating_add(size);
        } else {
            // The entry is made once, for the first bytes this Application
            // holds.
            self.held.insert(Arc::clone(application), size);
        }
    }

    /// Gives `size` bytes of `application` back; its entry goes at zero.
    pub fn give_back(&mut self, application: &Arc<str>, size: u64) {
        if let Some(now) = self.held.get_mut(application.as_ref()) {
            *now = now.saturating_sub(size);
            if *now == 0 {
                self.held.remove(application.as_ref());
            }
        }
    }

    /// How many Applications hold bytes (for the tests).
    #[cfg(test)]
    pub(crate) fn entries(&self) -> usize {
        self.held.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str) -> Arc<str> {
        Arc::from(name)
    }

    fn soon(millis: u64) -> Deadline {
        Deadline::now() + Duration::from_millis(millis)
    }

    #[tokio::test]
    async fn a_129th_place_waits_and_is_refused_while_another_applications_is_given_at_once() {
        let places = Places::new(128);
        let held: Vec<Place> = (0..128)
            .map(|n| {
                places
                    .try_place(&app("a"))
                    .unwrap_or_else(|| panic!("place {} of A's 128", n + 1))
            })
            .collect();
        // The 129th of A waits its time and finds none.
        let began = Instant::now();
        let refused = places.place(&app("a"), soon(300)).await;
        assert!(refused.is_none(), "the 129th body of A finds no place");
        assert!(
            began.elapsed() >= Duration::from_millis(290),
            "it waited for its time, not less: {:?}",
            began.elapsed()
        );
        // Meanwhile B's place is free at once.
        let began = Instant::now();
        let other = places.try_place(&app("b")).expect("B's place");
        assert!(began.elapsed() < Duration::from_millis(50));
        drop(other);
        drop(held);
    }

    #[test]
    fn a_place_given_back_is_free_again() {
        let places = Places::new(2);
        let first = places.try_place(&app("a")).expect("the first place");
        let second = places.try_place(&app("a")).expect("the second place");
        assert!(
            places.try_place(&app("a")).is_none(),
            "A's third finds none"
        );
        drop(first);
        assert!(
            places.try_place(&app("a")).is_some(),
            "the place given back is free again"
        );
        drop(second);
    }

    #[tokio::test]
    async fn no_entry_is_left_when_nothing_is_held() {
        let places = Places::new(1);
        {
            let _held = places.try_place(&app("a")).expect("a place");
            assert_eq!(places.entries(), 1, "A has an entry while it holds one");
        }
        assert_eq!(places.entries(), 0, "the entry goes with the last place");
        assert!(
            places.place(&app("a"), soon(50)).await.is_some(),
            "an entry is made again for the next wait"
        );
        assert_eq!(places.entries(), 0, "and gone again after it");
        // A wait that finds nothing leaves no entry either.
        let _held = places.try_place(&app("a")).expect("a place");
        assert!(places.place(&app("a"), soon(50)).await.is_none());
        assert_eq!(places.entries(), 1, "only the holder's entry is left");
        // A missed try at a full share leaves no entry of its own.
        let _second = places.try_place(&app("b")).expect("B's own place");
        assert!(
            places.try_place(&app("b")).is_none(),
            "B's second finds none"
        );
        assert_eq!(places.entries(), 2, "B's entry is kept by its holder");
    }

    #[test]
    fn exactly_the_cap_of_bytes_is_taken_and_the_entry_goes_at_zero() {
        let mut bytes = Bytes::new(64);
        let a = app("a");
        assert!(bytes.would_take(&a, 60));
        bytes.take(&a, 60);
        assert!(!bytes.would_take(&a, 5), "5 more would pass 64");
        assert!(bytes.would_take(&a, 4), "64 exactly is taken");
        bytes.take(&a, 4);
        assert_eq!(bytes.entries(), 1);
        assert!(!bytes.would_take(&a, 1), "one more would pass 64");
        bytes.give_back(&a, 4);
        assert!(bytes.would_take(&a, 4), "given back is free");
        let b = app("b");
        assert!(
            bytes.would_take(&b, 64),
            "another Application has its own 64"
        );
        bytes.take(&b, 64);
        bytes.give_back(&a, 60);
        assert_eq!(bytes.entries(), 1, "B's entry stays while B holds 64");
        bytes.give_back(&b, 64);
        assert_eq!(bytes.entries(), 0);
        // A call of no bytes holds nothing and makes no entry.
        bytes.take(&app("c"), 0);
        assert_eq!(bytes.entries(), 0);
    }

    #[test]
    fn the_first_refusal_for_an_application_and_kind_writes_one_record_a_minute() {
        let mentions = Mentions::default();
        let a = app("a");
        let b = app("b");
        let now = Instant::now();
        assert!(
            mentions.refused_at("a", Kind::StorageBodies, now),
            "the first refusal writes"
        );
        assert!(
            !mentions.refused_at("a", Kind::StorageBodies, now + Duration::from_secs(59)),
            "none for the next within the minute"
        );
        assert!(
            mentions.refused_at("b", Kind::StorageBodies, now + Duration::from_secs(59)),
            "B's first is its own"
        );
        assert!(
            mentions.refused_at("a", Kind::StorageBodies, now + Duration::from_secs(60)),
            "one a minute later writes again"
        );
        assert!(
            mentions.refused_at("a", Kind::OutboundCalls, now + Duration::from_secs(60)),
            "another kind is its own pair"
        );
        // The production path, with the names kept by reference until a
        // pair is first remembered.
        assert!(mentions.refused(&a, Kind::FileJobs));
        assert!(!mentions.refused(&a, Kind::FileJobs));
        assert!(mentions.refused(&b, Kind::FileJobs));
    }

    #[test]
    fn at_most_1024_pairs_are_remembered_and_one_a_minute_old_makes_room() {
        let mentions = Mentions::default();
        let now = Instant::now();
        for n in 0..MENTIONS {
            assert!(
                mentions.refused_at(&format!("app-{n}"), Kind::FileJobs, now),
                "pair {n} writes"
            );
        }
        assert_eq!(mentions.remembered(), MENTIONS);
        assert!(
            !mentions.refused_at("one-more", Kind::FileJobs, now + Duration::from_secs(1)),
            "past 1,024 with none a minute old, nothing is written"
        );
        assert_eq!(mentions.remembered(), MENTIONS, "and nothing is kept");
        assert!(
            mentions.refused_at("one-more", Kind::FileJobs, now + Duration::from_secs(60)),
            "one a minute old makes room"
        );
        assert_eq!(mentions.remembered(), MENTIONS);
    }
}
