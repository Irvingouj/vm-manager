//! Usage: how many requests each User made of each Application, by UTC day
//! (blackbox/CONTRACT.md, "What Platform administrators see and stop:
//! Usage: requests and Users").
//!
//! Interface: [`Usage::count`], by Ingress, for each request an
//! Application's code was given; [`start`] once, whose [`Writer`] (owned by
//! crate::platform) writes what was counted to the platform database's
//! `application_usage`. The list of every Application reads it
//! (crate::registry).
//!
//! Counting is never in a request's way: a count is added to a map in
//! memory under a lock held for that alone, and nothing waits on the
//! database. The map holds at most [`MAX_WAITING`] (Application, day, User);
//! a request of one more is not counted, and the program's log says how
//! many were not (`usage-dropped`, at most once every [`DROPPED_EVERY`]).
//! The writer takes the whole map every [`WRITE_EVERY`] and adds it to the
//! table in one transaction; a write that fails is not tried again (it may
//! have been made: a count is never more than what happened), and the log
//! says how many it held. Rows older than the 30 days are deleted every
//! [`DELETE_EVERY`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_postgres::Client;

use crate::log;
use crate::platform_db::{DbError, PlatformDatabase};

/// The most (Application, day, User) waiting to be written at once.
pub const MAX_WAITING: usize = 10_000;

/// How often what was counted is written: well within the 10 seconds the
/// contract allows, so that a count shows within its 15.
pub const WRITE_EVERY: Duration = Duration::from_secs(5);

/// The longest one write may take; past it, it has failed.
const WRITE_TIME: Duration = Duration::from_secs(10);

/// The least time between two `usage-dropped` records.
pub const DROPPED_EVERY: Duration = Duration::from_secs(10);

/// How often rows older than the 30 days are deleted (the contract asks
/// for at least once a day).
const DELETE_EVERY: Duration = Duration::from_secs(60 * 60);

/// The counts of one Application on one UTC day (days since 1970-01-01),
/// by User id.
type Counts = HashMap<(Arc<str>, i32), HashMap<String, i64>>;

/// What was counted and not yet written.
#[derive(Default)]
struct Waiting {
    counts: Counts,
    /// How many (Application, day, User) `counts` holds: at most
    /// [`MAX_WAITING`].
    keys: usize,
    /// Requests not counted since they were last taken.
    dropped: u64,
}

/// The counts waiting in memory. Its lock is held to add one count or to
/// take them all, never across an await.
#[derive(Default)]
pub struct Usage {
    waiting: Mutex<Waiting>,
}

impl Usage {
    /// Counts one request of `user` to `application`, today (UTC). Never
    /// waits: when [`MAX_WAITING`] other keys wait, it is not counted.
    pub fn count(&self, application: &Arc<str>, user: &str) {
        let Some(day) = today() else {
            return;
        };
        let mut waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
        let Waiting {
            counts,
            keys,
            dropped,
        } = &mut *waiting;
        let full = *keys >= MAX_WAITING;
        let of_day = counts.entry((Arc::clone(application), day)).or_default();
        if let Some(count) = of_day.get_mut(user) {
            *count = count.saturating_add(1);
        } else if full {
            *dropped = dropped.saturating_add(1);
        } else {
            // Copied once per (Application, day, User) every few seconds,
            // not at each request.
            of_day.insert(user.to_owned(), 1);
            *keys += 1;
        }
    }

    /// Everything waiting, taken out, and the requests not counted since
    /// the last time this was asked.
    fn take(&self) -> (Counts, u64) {
        let mut waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
        let counts = std::mem::take(&mut waiting.counts);
        waiting.keys = 0;
        let dropped = std::mem::take(&mut waiting.dropped);
        (counts, dropped)
    }
}

/// Today, UTC, as days since 1970-01-01; `None` with a clock before 1970.
fn today() -> Option<i32> {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    i32::try_from(seconds / 86_400).ok()
}

/// The task that writes the counts: ended when this is dropped.
pub struct Writer {
    task: JoinHandle<()>,
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Starts the writer on the current runtime.
pub fn start(database: Arc<PlatformDatabase>) -> (Arc<Usage>, Writer) {
    let usage = Arc::new(Usage::default());
    let task = tokio::spawn(write_all(Arc::clone(&usage), database));
    (usage, Writer { task })
}

async fn write_all(usage: Arc<Usage>, database: Arc<PlatformDatabase>) {
    let mut tick = tokio::time::interval(WRITE_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut dropped = 0_u64;
    let mut reported: Option<Instant> = None;
    let mut deleted: Option<Instant> = None;
    loop {
        tick.tick().await;
        let (counts, newly_dropped) = usage.take();
        dropped = dropped.saturating_add(newly_dropped);
        if dropped > 0 && reported.is_none_or(|at| at.elapsed() >= DROPPED_EVERY) {
            log::record("usage-dropped", json!({ "requests": dropped }));
            dropped = 0;
            reported = Some(Instant::now());
        }
        let delete = deleted.is_none_or(|at| at.elapsed() >= DELETE_EVERY);
        if counts.is_empty() && !delete {
            continue;
        }
        let requests: i64 = counts
            .values()
            .flat_map(HashMap::values)
            .fold(0, |sum, n| sum.saturating_add(*n));
        let written = tokio::time::timeout(WRITE_TIME, write(&database, &counts, delete)).await;
        match written {
            Ok(Ok(())) => {
                if delete {
                    deleted = Some(Instant::now());
                }
            }
            Ok(Err(error)) => failed(requests, &error.to_string()),
            Err(_) => failed(requests, "the write did not end in time"),
        }
    }
}

/// A write of `requests` counts that failed: they are not tried again.
fn failed(requests: i64, error: &str) {
    log::record(
        "usage-write-failed",
        json!({ "requests": requests, "error": error }),
    );
}

/// Adds `counts` to `application_usage` in one transaction, and when
/// `delete`, deletes the rows older than the 30 days.
async fn write(database: &PlatformDatabase, counts: &Counts, delete: bool) -> Result<(), DbError> {
    let mut applications: Vec<&str> = Vec::new();
    let mut days: Vec<i32> = Vec::new();
    let mut users: Vec<&str> = Vec::new();
    let mut requests: Vec<i64> = Vec::new();
    for ((application, day), of_day) in counts {
        for (user, count) in of_day {
            applications.push(application);
            days.push(*day);
            users.push(user);
            requests.push(*count);
        }
    }
    database
        .pool()
        .run(async |client: &mut Client| {
            let transaction = client.transaction().await.map_err(DbError::from)?;
            if !applications.is_empty() {
                transaction
                    .execute(
                        "INSERT INTO application_usage AS u (application, day, user_id, requests)
                         SELECT a, DATE '1970-01-01' + d, w, r
                         FROM unnest($1::text[], $2::int4[], $3::text[], $4::int8[]) AS t (a, d, w, r)
                         ON CONFLICT (application, day, user_id)
                         DO UPDATE SET requests = u.requests + EXCLUDED.requests",
                        &[&applications, &days, &users, &requests],
                    )
                    .await
                    .map_err(DbError::from)?;
            }
            if delete {
                transaction
                    .execute(
                        "DELETE FROM application_usage
                         WHERE day < (now() AT TIME ZONE 'UTC')::date - 29",
                        &[],
                    )
                    .await
                    .map_err(DbError::from)?;
            }
            transaction.commit().await.map_err(DbError::from)
        })
        .await
}
