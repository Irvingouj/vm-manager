//! The threads file work runs on (ADR 0019, "Off the async workers,
//! bounded"): [`THREADS`] of the program's own, fed by a
//! `std::sync::mpsc::sync_channel` of [`WAITING`] jobs. Opening, reading,
//! writing, renaming and `fsync` block, so none of it runs on an async
//! worker; and not on `tokio::task::spawn_blocking`, whose queue has no
//! bound.
//!
//! Bounds: at most [`THREADS`] jobs run at once, at most [`WAITING`] wait
//! for a thread, and at most [`PER_APPLICATION`] of one Application's wait
//! ("An Application's share of what the program holds"). A job that finds
//! no place among those waiting within [`PLACE_WAIT`] is not handed over
//! and [`Files::run`] answers `None` (the operation fails `unavailable` and
//! touches nothing). The Application's place is taken before the
//! program's, and one wait covers both. A disk that hangs holds at most
//! the [`THREADS`] threads; after that, operations fail so at once once
//! the places are taken.
//!
//! Owned: the threads are made by [`Files::start`] and ended by
//! [`Files::stop`] (the channel closed, each thread joined once it has
//! finished the job it is in, up to a wait), which the program calls as it
//! stops.

use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::json;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};

use crate::log;
use crate::shares::{Kind, Mentions, Place as Shared, Places};

/// Threads that do file work.
pub const THREADS: usize = 8;

/// Jobs that may wait for a thread.
pub const WAITING: usize = 64;

/// Jobs of one Application that may wait for a thread, all its requests
/// together ("An Application's share of what the program holds").
pub const PER_APPLICATION: usize = 16;

/// How long an operation waits for a place among those waiting.
pub const PLACE_WAIT: Duration = Duration::from_secs(5);

/// How long the program waits, as it stops, for the threads to finish the
/// jobs they are in.
pub const STOP_WAIT: Duration = Duration::from_secs(5);

type Job = Box<dyn FnOnce() + Send>;

/// One job handed over, with its place among the waiting, given back when
/// a thread takes it: the Application's and the program's.
struct Handed {
    job: Job,
    share: Option<Shared>,
    place: OwnedSemaphorePermit,
}

/// The file threads.
pub struct Files {
    /// `None` once stopped.
    sender: Mutex<Option<SyncSender<Handed>>>,
    /// The places among the waiting jobs: one is held from the moment a
    /// job is handed over until a thread takes it, so the channel never
    /// holds more than [`WAITING`] and a send never finds it full.
    places: Arc<Semaphore>,
    /// The Application's [`PER_APPLICATION`] places, taken before the
    /// program's.
    share: Places,
    threads: Mutex<Vec<JoinHandle<()>>>,
    /// How long a job waits for a place; [`PLACE_WAIT`], and shorter in
    /// the tests.
    wait: Duration,
}

impl Files {
    /// Starts the [`THREADS`] threads, their Application share writing
    /// `share-full` records through the platform's `mentions`.
    pub fn start(mentions: Arc<Mentions>) -> std::io::Result<Arc<Self>> {
        Self::start_with(mentions, PLACE_WAIT)
    }

    /// [`Files::start`] with the wait a job gives a place, for the tests.
    fn start_with(mentions: Arc<Mentions>, wait: Duration) -> std::io::Result<Arc<Self>> {
        let (sender, receiver) = sync_channel::<Handed>(WAITING);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut threads = Vec::with_capacity(THREADS);
        for n in 0..THREADS {
            let receiver = Arc::clone(&receiver);
            let thread = std::thread::Builder::new()
                .name(format!("vibe-files-{n}"))
                .spawn(move || work(&receiver));
            match thread {
                Ok(thread) => threads.push(thread),
                Err(error) => {
                    // The ones made end when the channel closes.
                    drop(sender);
                    for thread in threads {
                        thread.join().ok();
                    }
                    return Err(error);
                }
            }
        }
        Ok(Arc::new(Self {
            sender: Mutex::new(Some(sender)),
            places: Arc::new(Semaphore::new(WAITING)),
            share: Places::shared(PER_APPLICATION, mentions, Kind::FileJobs),
            threads: Mutex::new(threads),
            wait,
        }))
    }

    /// Runs `job` on a file thread and answers what it answered; `None`
    /// when no place among the waiting was found within the files' wait (or
    /// the threads are stopped): then `job` was not run and never will be.
    /// `application` is whose job it is, when it is an Application's; the
    /// platform's own work has none and takes only the program's place.
    /// The caller bounds the wait for the answer by its own time; a job
    /// whose answer is no longer awaited still runs to its end.
    pub async fn run<T: Send + 'static>(
        &self,
        job: impl FnOnce() -> T + Send + 'static,
        application: Option<&Arc<str>>,
    ) -> Option<oneshot::Receiver<T>> {
        self.run_or_undo(job, drop, application).await
    }

    /// Hands `job` to a file thread now, without waiting for a place: the
    /// job runs even when the caller's future is dropped. `None` when no
    /// place is free now (bounded by [`WAITING`]) or the threads are
    /// stopped; then the job never runs. This is the platform's own work
    /// only: an Application's job waits in [`Files::run`] for its share.
    pub fn hand_off<T: Send + 'static>(
        &self,
        job: impl FnOnce() -> T + Send + 'static,
    ) -> Option<tokio::sync::oneshot::Receiver<T>> {
        let place = Arc::clone(&self.places).try_acquire_owned().ok()?;
        let (reply, answer) = tokio::sync::oneshot::channel();
        let handed = Handed {
            job: Box::new(move || {
                drop(reply.send(job()));
            }),
            share: None,
            place,
        };
        let sender = self
            .sender
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()?;
        match sender.try_send(handed) {
            Ok(()) => Some(answer),
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => None,
        }
    }

    /// As [`Files::run`], but when the job's answer cannot be delivered
    /// (nobody waits for it any more), `undelivered` is given it, on the
    /// file thread: to undo what the job made.
    pub async fn run_or_undo<T: Send + 'static>(
        &self,
        job: impl FnOnce() -> T + Send + 'static,
        undelivered: impl FnOnce(T) + Send + 'static,
        application: Option<&Arc<str>>,
    ) -> Option<oneshot::Receiver<T>> {
        // One deadline for both places: the Application's first, so that
        // one at its share holds none of the program's while it waits.
        let by = tokio::time::Instant::now() + self.wait;
        let share = if let Some(application) = application {
            Some(self.share.place(application, by).await?)
        } else {
            None
        };
        let place =
            match tokio::time::timeout_at(by, Arc::clone(&self.places).acquire_owned()).await {
                Ok(Ok(place)) => place,
                Ok(Err(_)) | Err(_) => {
                    log::record(
                        "storage-busy",
                        json!({ "waiting": WAITING, "threads": THREADS }),
                    );
                    return None;
                }
            };
        let (reply, answer) = oneshot::channel();
        let handed = Handed {
            job: Box::new(move || {
                // The operation may have stopped waiting.
                if let Err(answer) = reply.send(job()) {
                    undelivered(answer);
                }
            }),
            share,
            place,
        };
        let sender = self
            .sender
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()?;
        match sender.try_send(handed) {
            Ok(()) => Some(answer),
            // Neither happens while a place is held and the threads run:
            // answered as no place.
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => None,
        }
    }

    /// Ends the threads: no job is taken any more, the jobs waiting are
    /// dropped (their operations answered `unavailable`), and each thread
    /// is joined once it has finished the job it is in. Waits up to `wait`
    /// in all; a thread still in a system call then (a disk that hangs) is
    /// left to the program's exit, and the log says how many.
    pub async fn stop(&self, wait: Duration) {
        drop(
            self.sender
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
        self.places.close();
        let threads =
            std::mem::take(&mut *self.threads.lock().unwrap_or_else(PoisonError::into_inner));
        let deadline = tokio::time::Instant::now() + wait;
        let mut left = 0_usize;
        for thread in threads {
            while !thread.is_finished() && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            if thread.is_finished() {
                // Finished: joining does not wait.
                thread.join().ok();
            } else {
                left += 1;
            }
        }
        if left > 0 {
            log::record("storage-threads-left", json!({ "threads": left }));
        }
    }
}

/// One thread: takes jobs until the channel is closed and empty.
fn work(receiver: &Mutex<Receiver<Handed>>) {
    loop {
        let handed = {
            let receiver = receiver.lock().unwrap_or_else(PoisonError::into_inner);
            receiver.recv()
        };
        let Ok(Handed { job, share, place }) = handed else {
            return;
        };
        // Taken: its places among the waiting are free.
        drop(share);
        drop(place);
        job();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str) -> Arc<str> {
        Arc::from(name)
    }

    #[tokio::test]
    async fn sixteen_waiting_jobs_of_one_application_leave_no_place_for_its_seventeenth() {
        let files = Files::start_with(Arc::new(Mentions::default()), Duration::from_millis(100))
            .expect("the file threads");
        // The 8 threads are busy, so every job below waits in the channel.
        let (release, waiting) = std::sync::mpsc::sync_channel::<()>(THREADS);
        let waiting = Arc::new(Mutex::new(waiting));
        let mut blockers = Vec::new();
        for _ in 0..THREADS {
            let waiting = Arc::clone(&waiting);
            blockers.push(
                files
                    .run(
                        move || {
                            let waiting = waiting.lock().unwrap_or_else(PoisonError::into_inner);
                            waiting.recv().ok();
                        },
                        None,
                    )
                    .await
                    .expect("a place for a blocker"),
            );
        }
        // A's 16 places are all waiting for a thread.
        let mut a_jobs = Vec::new();
        for _ in 0..PER_APPLICATION {
            a_jobs.push(
                files
                    .run(|| {}, Some(&app("a")))
                    .await
                    .expect("a place among A's 16 waiting jobs"),
            );
        }
        // A's seventeenth finds no place within its wait; B's is handed
        // over at once.
        assert!(
            files.run(|| {}, Some(&app("a"))).await.is_none(),
            "A's 17th job finds no place within its wait"
        );
        assert!(
            files.run(|| {}, Some(&app("b"))).await.is_some(),
            "B's job is handed over while A is at its share"
        );
        // Let the threads take what waits, then stop them.
        for _ in 0..THREADS {
            release.send(()).ok();
        }
        for answer in blockers {
            answer.await.ok();
        }
        for answer in a_jobs {
            answer.await.ok();
        }
        files.stop(STOP_WAIT).await;
    }
}
