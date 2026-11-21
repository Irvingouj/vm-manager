//! The threads file work runs on (ADR 0019, "Off the async workers,
//! bounded"): [`THREADS`] of the program's own, fed by a
//! `std::sync::mpsc::sync_channel` of [`WAITING`] jobs. Opening, reading,
//! writing, renaming and `fsync` block, so none of it runs on an async
//! worker; and not on `tokio::task::spawn_blocking`, whose queue has no
//! bound.
//!
//! Bounds: at most [`THREADS`] jobs run at once and at most [`WAITING`]
//! wait for a thread. A job that finds no place among those waiting within
//! [`PLACE_WAIT`] is not handed over and [`Files::run`] answers `None`
//! (the operation fails `unavailable` and touches nothing). A disk that
//! hangs holds at most the [`THREADS`] threads; after that, operations fail
//! so at once once the places are taken.
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

/// Threads that do file work.
pub const THREADS: usize = 8;

/// Jobs that may wait for a thread.
pub const WAITING: usize = 64;

/// How long an operation waits for a place among those waiting.
pub const PLACE_WAIT: Duration = Duration::from_secs(5);

/// How long the program waits, as it stops, for the threads to finish the
/// jobs they are in.
pub const STOP_WAIT: Duration = Duration::from_secs(5);

type Job = Box<dyn FnOnce() + Send>;

/// One job handed over, with its place among the waiting, given back when
/// a thread takes it.
struct Handed {
    job: Job,
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
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl Files {
    /// Starts the [`THREADS`] threads.
    pub fn start() -> std::io::Result<Arc<Self>> {
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
            threads: Mutex::new(threads),
        }))
    }

    /// Runs `job` on a file thread and answers what it answered; `None`
    /// when no place among the waiting was found within [`PLACE_WAIT`] (or
    /// the threads are stopped): then `job` was not run and never will be.
    /// The caller bounds the wait for the answer by its own time; a job
    /// whose answer is no longer awaited still runs to its end.
    pub async fn run<T: Send + 'static>(
        &self,
        job: impl FnOnce() -> T + Send + 'static,
    ) -> Option<oneshot::Receiver<T>> {
        let places = Arc::clone(&self.places);
        let place = match tokio::time::timeout(PLACE_WAIT, places.acquire_owned()).await {
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
                reply.send(job()).ok();
            }),
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
        let Ok(Handed { job, place }) = handed else {
            return;
        };
        // Taken: its place among the waiting is free.
        drop(place);
        job();
    }
}
