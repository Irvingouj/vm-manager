//! The bounds on bodies open at once (blackbox/CONTRACT.md, "Storage:
//! Bounds"): **16 for one request, 128 for one Application and 512 in the
//! program**, a body being read, written or sent holding one of each ("An
//! Application's share of what the program holds").
//!
//! [`Bodies`] is the one request's: its 16 places, the body pumps it owns
//! (a get's answer feeding the stream the host hands the code), the put
//! tasks it owns, and the signal of the request's end — a put still
//! running then is given up (`request-ended`, or `application-disabled`
//! at a disable), and the pumps of bodies nobody is reading any more are
//! ended. [`Open`] is the program's 512 and each Application's 128. Both
//! live with the request ([`crate::runner::InFlight`] holds the one,
//! [`Storage`](super::Storage) the other).
//!
//! An operation that would open a body and finds no place takes one now
//! ([`Bodies::try_guard`], which is what a put does at its decision, before
//! its first byte) or waits up to [`WAIT`] ([`Bodies::guard`]), then fails
//! `unavailable` and touches nothing — unless its request ends or its
//! Application is disabled meanwhile, when it is given up at once
//! ([`Place::Ended`]). The Application's place is taken before the
//! program's, and one wait covers both. A place is given back when its
//! [`Guard`] is: a body read to its end, cancelled, or failed frees its own
//! (the pump that feeds it ends and takes its entry out of [`Bodies`]), and
//! in any case the request's end frees the rest — unless the body is being
//! sent as the response, whose pump the answer takes over
//! ([`Bodies::take_for_sending`]) so the sending goes on after the code
//! has ended. Nothing the request held for a finished body stays in
//! [`Bodies`]: a request of a thousand gets holds one entry per get still
//! under way, not per get it ever made.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

use crate::sent::OwnedTask;
use crate::shares::{Kind, Mentions, Place as Shared};

/// Bodies one request may hold open at once.
pub const PER_REQUEST: usize = 16;

/// Bodies the whole program may hold open at once.
pub const IN_PROGRAM: usize = 512;

/// Bodies one Application may hold open at once, all its requests
/// together ("An Application's share of what the program holds").
pub const PER_APPLICATION: usize = 128;

/// How long an operation that would open a body waits for a place, then
/// fails `unavailable`.
pub const WAIT: Duration = Duration::from_secs(5);

/// How long a put whose request ended has to end its own body — its
/// staging file removed — before its task is ended.
pub const PUT_REAP: Duration = Duration::from_secs(30);

/// The program's [`IN_PROGRAM`] places, and its [`PER_APPLICATION`] for
/// each Application. One for the program ([`Storage`](super::Storage)).
#[derive(Debug)]
pub struct Open {
    places: Arc<Semaphore>,
    share: crate::shares::Places,
}

impl Open {
    /// The program's places, sharing out `mentions` for `storage-bodies`.
    pub fn new(mentions: Arc<Mentions>) -> Self {
        Self {
            places: Arc::new(Semaphore::new(IN_PROGRAM)),
            share: crate::shares::Places::shared(PER_APPLICATION, mentions, Kind::StorageBodies),
        }
    }
}

/// One open body's places — its request's, its Application's and the
/// program's — given back when the guard drops: by the body's pump (a
/// get), the put work, or the request's registry.
#[derive(Debug)]
pub struct Guard {
    _request: OwnedSemaphorePermit,
    _share: Shared,
    _program: OwnedSemaphorePermit,
}

/// What a wait for a place came to.
#[derive(Debug)]
pub enum Place {
    /// A place was found.
    Made(Guard),
    /// The request ended (or its Application was disabled) while the wait
    /// went on: the operation is given up with it.
    Ended,
    /// The [`WAIT`] passed without a place.
    Waited,
}

/// A body pump the request's registry owns, and what the answer needs to
/// send the body itself: the task, and the file's size (the
/// `content-length` the platform states for a body it sends).
struct Held {
    task: OwnedTask,
    length: Option<u64>,
}

/// The bodies one request holds open (in
/// [`InFlight`](crate::runner::InFlight)): its 16 places, the pumps and
/// put tasks it owns, and the signal of its end.
pub struct Bodies {
    /// One permit per open body of this request.
    places: Arc<Semaphore>,
    /// The bodies' pumps, by their stream id. Locked only to put in, take
    /// out or look, never across an await.
    held: std::sync::Mutex<Vec<(u64, Held)>>,
    /// The puts under way: their tasks, by an id of this registry's own,
    /// so that one still running when its request ends is given its bound
    /// and ended after it (never left detached).
    puts: std::sync::Mutex<Vec<(u64, OwnedTask)>>,
    /// The tasks that give a given-up put its bound and end it after; held
    /// so that this registry (which they also hold, briefly) outlives them
    /// and they are ended with it.
    reapers: std::sync::Mutex<Vec<OwnedTask>>,
    /// The next put id.
    next_put: AtomicU64,
    /// The request is over: nothing more opens, the puts are given up,
    /// the pumps nobody reads any more are ended.
    ended: watch::Sender<bool>,
}

impl Default for Bodies {
    fn default() -> Self {
        let (ended, _) = watch::channel(false);
        Self {
            places: Arc::new(Semaphore::new(PER_REQUEST)),
            held: std::sync::Mutex::new(Vec::new()),
            puts: std::sync::Mutex::new(Vec::new()),
            reapers: std::sync::Mutex::new(Vec::new()),
            next_put: AtomicU64::new(1),
            ended,
        }
    }
}

impl Bodies {
    /// A place now, without waiting: the request's, the Application's and
    /// the program's, or `None` when any has none free (or the request is
    /// over). This is how a put takes its place at its decision, before its
    /// first byte. No record is written: the put goes on to wait for a
    /// place, and is refused only when that wait ends.
    pub fn try_guard(&self, program: &Open, application: &Arc<str>) -> Option<Guard> {
        if self.is_ended() {
            return None;
        }
        let request = Arc::clone(&self.places).try_acquire_owned().ok()?;
        // The Application's place before the program's: one at its share
        // takes nothing from the others.
        let Some(share) = program.share.try_place(application) else {
            drop(request);
            return None;
        };
        match Arc::clone(&program.places).try_acquire_owned() {
            Ok(place) => Some(Guard {
                _request: request,
                _share: share,
                _program: place,
            }),
            Err(_) => {
                drop(share);
                drop(request);
                None
            }
        }
    }

    /// A place for one more open body, waiting up to [`WAIT`] for the
    /// request's, the Application's and the program's — one deadline for
    /// them all — and ended at once when the request does (or its
    /// Application is disabled), so that an operation waiting for a place
    /// is given up with its request, not answered `unavailable` five
    /// seconds later.
    pub async fn guard(&self, program: &Open, application: &Arc<str>) -> Place {
        let deadline = Instant::now() + WAIT;
        let deadline = tokio::time::Instant::from_std(deadline);
        let request = tokio::select! {
            biased;
            () = self.ended() => return Place::Ended,
            waited = tokio::time::timeout_at(deadline, {
                let places = Arc::clone(&self.places);
                async move { places.acquire_owned().await }
            }) => match waited {
                Ok(Ok(place)) => place,
                Ok(Err(_)) | Err(_) => return Place::Waited,
            },
        };
        let share = tokio::select! {
            biased;
            () = self.ended() => {
                drop(request);
                return Place::Ended;
            }
            waited = program.share.place(application, deadline) => match waited {
                Some(place) => place,
                None => {
                    drop(request);
                    return Place::Waited;
                }
            },
        };
        let program = tokio::select! {
            biased;
            () = self.ended() => {
                drop(share);
                drop(request);
                return Place::Ended;
            }
            waited = tokio::time::timeout_at(deadline, {
                let places = Arc::clone(&program.places);
                async move { places.acquire_owned().await }
            }) => match waited {
                Ok(Ok(place)) => place,
                Ok(Err(_)) => {
                    drop(share);
                    drop(request);
                    return Place::Waited;
                }
                Err(_) => {
                    // The request's and the Application's places are given
                    // back at once; the operation failed `unavailable` and
                    // touched nothing.
                    drop(share);
                    drop(request);
                    return Place::Waited;
                }
            },
        };
        Place::Made(Guard {
            _request: request,
            _share: share,
            _program: program,
        })
    }

    /// Whether the request is over.
    pub fn is_ended(&self) -> bool {
        *self.ended.borrow()
    }

    /// Ends when the request does (`wait_for`, so a request already over
    /// ends it at once): what a put still running selects on, to be
    /// given up.
    pub async fn ended(&self) {
        let mut ended = self.ended.subscribe();
        drop(ended.wait_for(|ended| *ended).await);
    }

    /// The request's end as a [`GivenUp`](super::GivenUp): handed to a
    /// put, whose backend ends its body when it fires.
    pub fn given_up(&self) -> super::GivenUp {
        self.ended.subscribe()
    }

    /// Holds the pump `task` of the body `id` — with `length`, the size of
    /// the file it feeds, when the answer needs it — until the body ends:
    /// the task is ended (and the place the body holds, which the task
    /// itself carries, given back) at the request's end, unless the answer
    /// takes it over to send the body
    /// ([`Bodies::take_for_sending`]). A pump that ended takes its own
    /// entry out again ([`Bodies::release`]).
    pub fn hold(&self, id: u64, task: OwnedTask, length: Option<u64>) {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.is_ended() {
            // The request is over and the answer with it: nothing will
            // read this body, and nobody will take its pump over.
            drop(task);
            return;
        }
        held.push((id, Held { task, length }));
    }

    /// Whether the pump of the body `id` is still held here: the code holds
    /// the body. A body given to a response to be sent is not (its pump was
    /// taken over), nor one that ended.
    pub fn holds(&self, id: u64) -> bool {
        self.held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|(held, _)| *held == id)
    }

    /// The size the pump of the body `id` was held with, when it is held
    /// and one was given (a stored file's body: its size).
    pub fn length_of(&self, id: u64) -> Option<u64> {
        self.held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|(held, _)| *held == id)
            .and_then(|(_, held)| held.length)
    }

    /// The pump of the body `id` has ended: its entry goes, and with it the
    /// handle of a task that has ended — dropping it ends a task that is
    /// returning in this very poll, and there is nothing left in it to end.
    /// A body read to its end, cancelled or failed therefore leaves no
    /// entry here.
    pub fn release(&self, id: u64) {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(at) = held.iter().position(|(held, _)| *held == id) {
            held.remove(at);
        }
    }

    /// Takes the pump of the body `id`, for a response that sends the
    /// body itself: the sending goes on after the code has ended, so the
    /// request's end must not end it. Answers the task and the file's
    /// size, the `content-length` the platform states; `None` when no
    /// such body is held (the request's own body given back unread, whose
    /// length the request stated, or a body already taken).
    pub fn take_for_sending(&self, id: u64) -> Option<(OwnedTask, Option<u64>)> {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let at = held.iter().position(|(held, _)| *held == id)?;
        let (_, taken) = held.remove(at);
        let Held { task, length } = taken;
        Some((task, length))
    }

    /// Takes the put task `task` under this registry's ownership, which
    /// ends it in every case: normally when it answers, or — if its work
    /// is dropped before that (the program stopping, the operation's own
    /// task cancelled) — at the request's end, given  [`PUT_REAP`] to
    /// remove its staging file and ended after that. `None` when the
    /// request is already over: the put never starts.
    pub fn own_put(&self, task: OwnedTask) -> Option<u64> {
        let mut puts = self
            .puts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.is_ended() {
            return None;
        }
        let id = self.next_put.fetch_add(1, Ordering::Relaxed);
        puts.push((id, task));
        Some(id)
    }

    /// The put `id` has ended (answered, its staging file removed or
    /// landed): its task goes, its handle dropped.
    pub fn release_put(&self, id: u64) {
        let mut puts = self
            .puts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(at) = puts.iter().position(|(put, _)| *put == id) {
            puts.remove(at);
        }
    }

    /// The request is over: the puts still running see it (their `ended`
    /// watch fires and their backends end their bodies, staging removed)
    /// and are given [`PUT_REAP`] to do it before their tasks are ended;
    /// the pumps of bodies nobody will read any more are ended (a body
    /// being sent had its pump taken, and is not held here).
    pub fn end(self: &Arc<Self>) {
        self.ended.send_replace(true);
        let held = std::mem::take(
            &mut *self
                .held
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        drop(held);
        let puts = std::mem::take(
            &mut *self
                .puts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for (_, task) in puts {
            // The reaper holds this registry, so this one lives until the
            // reaper is done — the request's other parts do not end it
            // first. It never waits longer than the bound.
            let keeper = Arc::clone(self);
            let reaper = OwnedTask::new(tokio::spawn(async move {
                task.wait(PUT_REAP).await;
                drop(keeper);
            }));
            self.reapers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(reaper);
        }
    }
}

impl Drop for Bodies {
    fn drop(&mut self) {
        // A backstop, for a request never given to the code (answered
        // `Busy`, disabled while it waited), whose operations held
        // nothing, and for a body opened after the end: everything still
        // here is ended at once, no reaping — there is no runtime to reap
        // on any more, and nothing of a put is left that a next load of
        // the Resource would not clear.
        self.ended.send_replace(true);
        let held = std::mem::take(
            &mut *self
                .held
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        drop(held);
        let puts = std::mem::take(
            &mut *self
                .puts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        drop(puts);
        let reapers = std::mem::take(
            &mut *self
                .reapers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        drop(reapers);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str) -> Arc<str> {
        Arc::from(name)
    }

    fn open() -> Open {
        Open::new(Arc::new(Mentions::default()))
    }

    /// A task that never ends by itself: only its owner's drop does.
    fn forever() -> OwnedTask {
        OwnedTask::new(tokio::spawn(std::future::pending::<()>()))
    }

    #[tokio::test]
    async fn a_guard_takes_one_place_of_the_request_and_one_of_the_program() {
        let bodies = Bodies::default();
        let program = open();
        let guards: Vec<Guard> = {
            let mut guards = Vec::new();
            for index in 0..PER_REQUEST {
                let guard = bodies
                    .try_guard(&program, &app("a"))
                    .unwrap_or_else(|| panic!("place {index} of this request's own"));
                guards.push(guard);
            }
            guards
        };
        assert!(
            bodies.try_guard(&program, &app("a")).is_none(),
            "the 17th body of one request finds no place now"
        );
        assert!(
            matches!(bodies.guard(&program, &app("a")).await, Place::Waited),
            "the 17th body of one request finds no place within the wait"
        );
        drop(guards);
        assert!(
            bodies.try_guard(&program, &app("a")).is_some(),
            "the places are given back when the guards drop"
        );
    }

    #[tokio::test]
    async fn the_programs_places_run_out_and_each_request_still_gets_its_own() {
        // 32 requests of 16 bodies each fill the program's 512; one more
        // finds no place, whatever request it is of.
        let program = open();
        let requests: Vec<Bodies> = (0..32).map(|_| Bodies::default()).collect();
        let guards: Vec<Guard> = {
            let mut guards = Vec::new();
            for (n, bodies) in requests.iter().enumerate() {
                let application: Arc<str> = Arc::from(format!("app-{n}"));
                for _ in 0..PER_REQUEST {
                    guards.push(
                        bodies
                            .try_guard(&program, &application)
                            .unwrap_or_else(|| panic!("a place in the program, request {n}")),
                    );
                }
            }
            guards
        };
        let bodies = Bodies::default();
        assert!(
            bodies.try_guard(&program, &app("app-32")).is_none(),
            "one more than the program holds finds no place now"
        );
        drop(guards);
        assert!(
            bodies.try_guard(&program, &app("app-32")).is_some(),
            "the places are given back when the guards drop"
        );
    }

    #[tokio::test]
    async fn an_application_at_its_share_waits_without_holding_a_place_of_the_program() {
        let program = open();
        let holders: Vec<Bodies> = (0..8).map(|_| Bodies::default()).collect();
        let mut guards = Vec::new();
        for bodies in &holders {
            for _ in 0..PER_REQUEST {
                guards.push(
                    bodies
                        .try_guard(&program, &app("a"))
                        .expect("a place of A's 128"),
                );
            }
        }
        let bodies = Bodies::default();
        assert!(
            bodies.try_guard(&program, &app("a")).is_none(),
            "A's 129th finds no place now"
        );
        let other = Bodies::default();
        let held_by_holders = PER_APPLICATION;
        let a = app("a");
        let (place, ()) = tokio::join!(bodies.guard(&program, &a), async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(
                program.places.available_permits(),
                IN_PROGRAM - held_by_holders,
                "A's wait takes none of the program's places its holders do not hold"
            );
            let b = other
                .try_guard(&program, &app("b"))
                .expect("B's place while A waits");
            drop(b);
            assert_eq!(
                program.places.available_permits(),
                IN_PROGRAM - held_by_holders
            );
        });
        assert!(
            matches!(place, Place::Waited),
            "A's 129th finds no place within the wait"
        );
        assert_eq!(
            program.places.available_permits(),
            IN_PROGRAM - held_by_holders,
            "the wait took no program place of its own"
        );
        drop(guards);
        assert!(
            bodies.try_guard(&program, &app("a")).is_some(),
            "A's places are free again"
        );
    }

    #[tokio::test]
    async fn a_wait_for_a_place_ends_with_the_request() {
        let bodies = Arc::new(Bodies::default());
        let program = open();
        let guards: Vec<Guard> = (0..PER_REQUEST)
            .map(|_| bodies.try_guard(&program, &app("a")).expect("a place"))
            .collect();
        {
            // The 17th waits; the request ends under it.
            let waiting_bodies = Arc::clone(&bodies);
            let program = open();
            let waiting = tokio::spawn(async move {
                let program = program;
                waiting_bodies.guard(&program, &app("a")).await
            });
            tokio::time::sleep(Duration::from_millis(50)).await;
            bodies.end();
            let place = tokio::time::timeout(WAIT, waiting)
                .await
                .expect("the wait ended with the request, not after its five seconds")
                .expect("the waiting task ended");
            assert!(
                matches!(place, Place::Ended),
                "the wait is given up with the request"
            );
        }
        drop(guards);
    }

    #[tokio::test]
    async fn a_pump_that_ended_takes_its_own_entry_out() {
        let bodies = Arc::new(Bodies::default());
        let program = open();
        let guard = Arc::new(bodies.try_guard(&program, &app("a")).expect("a place"));
        let pump = bodies_for_pump(&bodies, guard);
        bodies.hold(7, pump, Some(1024));
        assert_eq!(bodies.held.lock().expect("the lock").len(), 1);
        bodies.release(7);
        assert!(
            bodies.held.lock().expect("the lock").is_empty(),
            "a pump that ended leaves no entry"
        );
        // The answer takes the pump of the body it sends; a body already
        // taken is not taken again.
        let guard = Arc::new(bodies.try_guard(&program, &app("a")).expect("a place"));
        bodies.hold(8, bodies_for_pump(&bodies, guard), Some(2));
        assert!(
            bodies.take_for_sending(8).is_some_and(|(task, length)| {
                drop(task);
                length == Some(2)
            }),
            "the pump of the body being sent is taken"
        );
        assert!(bodies.take_for_sending(8).is_none(), "taken once");
    }

    /// A pump task that sleeps: takes the registry, as the real one does.
    fn bodies_for_pump(bodies: &Arc<Bodies>, guard: Arc<Guard>) -> OwnedTask {
        let bodies = Arc::clone(bodies);
        OwnedTask::new(tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            drop(guard);
            drop(bodies);
        }))
    }

    #[tokio::test]
    async fn a_put_is_owned_from_its_start_and_none_starts_after_the_end() {
        let bodies = Arc::new(Bodies::default());
        let id = bodies.own_put(forever()).expect("the request is not over");
        assert_eq!(
            bodies.puts.lock().expect("the lock").len(),
            1,
            "the put task is held by the request"
        );
        bodies.release_put(id);
        assert!(
            bodies.puts.lock().expect("the lock").is_empty(),
            "a put that ended leaves no task behind"
        );
        // A put owned when the request ends is given its bound and reaped;
        // the registry lives until the reaper is done (it holds it), and no
        // put starts after the end.
        bodies.own_put(forever());
        bodies.end();
        assert!(
            bodies.puts.lock().expect("the lock").is_empty(),
            "the puts are taken for their reaping at the end"
        );
        assert!(
            bodies.own_put(forever()).is_none(),
            "a put that starts after its request's end never starts"
        );
        assert!(
            bodies.try_guard(&open(), &app("a")).is_none(),
            "and no body opens after it either"
        );
    }
}
