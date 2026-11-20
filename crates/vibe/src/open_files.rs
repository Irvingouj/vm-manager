//! The program's limit of open files (CONTRACT.md, "Serving": "The open
//! files it needs").
//!
//! `vibe serve` holds up to [`crate::server::CONNECTIONS`] connections at
//! once, each an open file, beside its connections to Postgres, its files
//! and its listening socket. A process is often started with a soft limit
//! of 1,024 open files (a shell, a systemd service, a container under the
//! Docker daemon's default), fewer than that. So at start it raises its
//! own soft limit towards the hard one, which an unprivileged process may
//! do, and says in one log line what the limit is; when even the hard
//! limit is below what its bounds need it says so in words, and serves all
//! the same.

use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
use serde_json::json;

use crate::log;

/// The open files the program's bounds need: the 1,024 connections, the
/// database pools, its files, with room.
pub const NEEDED: u64 = 4096;

/// The soft limit asked for when the hard limit allows it: as many as the
/// Compose file sets (`deploy/compose.yaml`, `ulimits`), and no more, so
/// that a hard limit of millions does not become the soft one.
pub const WANTED: u64 = 65536;

/// What the soft limit should become, given the limit now; `None` when it
/// is to stay as it is (already enough, or unlimited). Towards the hard
/// limit, at most [`WANTED`].
pub fn target(limit: Rlimit) -> Option<u64> {
    let soft = limit.current?;
    let wanted = limit.maximum.map_or(WANTED, |hard| hard.min(WANTED));
    (soft < NEEDED && wanted > soft).then_some(wanted)
}

/// Raises the soft limit of open files when it is below [`NEEDED`], and
/// logs `open-files-limit` (and `open-files-limit-low` when even the hard
/// limit is below [`NEEDED`]). Never fails the start.
pub fn raise_and_log() {
    let before = getrlimit(Resource::Nofile);
    let mut raised_from = None;
    let mut failed = None;
    if let Some(wanted) = target(before) {
        // macOS refuses a soft limit above its own per-process maximum
        // even when the hard limit is unlimited: then the need alone.
        let tries = [Some(wanted), (wanted > NEEDED).then_some(NEEDED)];
        for soft in tries.into_iter().flatten() {
            let asked = Rlimit {
                current: Some(soft),
                maximum: before.maximum,
            };
            match setrlimit(Resource::Nofile, asked) {
                Ok(()) => {
                    raised_from = before.current;
                    failed = None;
                    break;
                }
                Err(error) => failed = Some(error.to_string()),
            }
        }
    }
    let now = getrlimit(Resource::Nofile);
    let mut fields = json!({
        "soft": now.current,
        "hard": now.maximum,
        "raised_from": raised_from,
    });
    if let (Some(error), Some(object)) = (&failed, fields.as_object_mut()) {
        object.insert("raise_failed".into(), error.as_str().into());
    }
    log::record("open-files-limit", fields);
    let enough = |limit: Option<u64>| limit.is_none_or(|l| l >= NEEDED);
    if !enough(now.current) {
        let shown = |limit: Option<u64>| limit.map_or("unlimited".to_owned(), |l| l.to_string());
        log::record(
            "open-files-limit-low",
            json!({
                "soft": now.current,
                "hard": now.maximum,
                "needed": NEEDED,
                "message": format!(
                    "the program may open {} files (hard limit {}), fewer than the {NEEDED} its bounds need (1,024 connections at once, the database's connections, its files): it serves all the same, but may fail to accept connections before its bound; raise the limit of open files of the program's process to {NEEDED} or more (with Docker Compose: `ulimits: nofile` on the service, as deploy/compose.yaml does; with systemd: LimitNOFILE=; in a shell: ulimit -n, after raising the hard limit as root)",
                    shown(now.current),
                    shown(now.maximum),
                ),
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limit(soft: Option<u64>, hard: Option<u64>) -> Rlimit {
        Rlimit {
            current: soft,
            maximum: hard,
        }
    }

    #[test]
    fn a_low_soft_limit_is_raised_towards_the_hard_one_at_most_to_what_is_wanted() {
        assert_eq!(target(limit(Some(1024), Some(524_288))), Some(WANTED));
        assert_eq!(target(limit(Some(1024), Some(8192))), Some(8192));
        assert_eq!(target(limit(Some(1024), None)), Some(WANTED));
        assert_eq!(target(limit(Some(256), Some(2048))), Some(2048));
    }

    #[test]
    fn an_enough_or_unraisable_soft_limit_stays() {
        assert_eq!(target(limit(Some(4096), Some(524_288))), None);
        assert_eq!(target(limit(Some(100_000), None)), None);
        assert_eq!(target(limit(None, None)), None);
        assert_eq!(target(limit(Some(1024), Some(1024))), None);
    }
}
