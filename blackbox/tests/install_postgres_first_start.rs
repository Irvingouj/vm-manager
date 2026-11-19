//! A first start of the bundled Postgres that was interrupted (CONTRACT.md,
//! "Installing: one image and a Compose file": the bundled Postgres; ticket
//! "Install: one image and a Compose file, hardened").
//!
//! The image Postgres is made from makes the database folder first
//! (`initdb`, which ends with "Success. You can now start the database
//! server") and runs the first start's set-up after it. The test starts the
//! `postgres` service alone, follows its log and kills its container the
//! moment that line is printed, then starts it again: the role `vibe` must
//! not be there (else the kill came too late, and it tries again on a fresh
//! volume, a few times). Then the service must not become healthy, `docker
//! compose up -d` must not bring `vibe` to running, and the superuser must
//! still be let in inside the container; after `down --volumes` and `up -d`
//! it comes up whole.
//!
//! Against the image `VIBE_INSTALL_IMAGE` names; run by
//! `scripts/install-gate.sh`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::install::{COMPOSE, DOCKER_WAIT, FrontDoor, Installed, UP_WAIT};
use blackbox::sessions::ada;

/// The last line `initdb` prints: the database folder is made, the first
/// start's set-up has not run yet.
const MADE: &str = "Success. You can now start the database server";
/// How many times the moment is tried for.
const TRIES: usize = 5;
/// How long the service is watched for becoming healthy.
const HEALTH_WAIT: Duration = Duration::from_secs(45);

/// `psql` as the superuser inside the `postgres` container, waited for
/// (at most a minute) until it answers; its output.
fn psql_when_up(at: &Installed, sql: &str) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let ran = at.install.exec(
            "postgres",
            &[
                "psql", "-X", "-A", "-t", "-U", "postgres", "-d", "postgres", "-c", sql,
            ],
        );
        if ran.ok() {
            return Ok(ran.stdout.trim().to_owned());
        }
        if Instant::now() >= deadline {
            return Err(ran.describe(&at.install.secrets()));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn a_bundled_postgres_whose_first_start_was_interrupted_stays_unhealthy_and_vibe_is_not_started() {
    let at = Installed::prepare(&[COMPOSE], &[ada()], FrontDoor::TheFilesProxy);
    let mut hit = None;
    let mut seen = Vec::new();
    for attempt in 1..=TRIES {
        at.install.compose_ok(&["up", "-d", "postgres"], UP_WAIT);
        let id = at.install.container("postgres");
        let killed = at
            .install
            .kill_when_logged(&id, MADE, Duration::from_secs(60));
        if let Err(problem) = killed {
            at.install.fail(&format!("try {attempt}: {problem}"));
        }
        at.install.compose_ok(&["up", "-d", "postgres"], UP_WAIT);
        let roles = psql_when_up(&at, "select count(*) from pg_roles where rolname = 'vibe'")
            .unwrap_or_else(|problem| {
                at.install.fail(&format!(
                    "try {attempt}: expected the superuser let in inside the container after the interrupted first start\n{problem}"
                ))
            });
        seen.push(format!("try {attempt}: role vibe count {roles}"));
        if roles == "0" {
            hit = Some(attempt);
            break;
        }
        // Too late: the set-up had run. Again, on a fresh volume.
        at.install
            .compose_ok(&["down", "--volumes", "--timeout", "5"], DOCKER_WAIT);
    }
    let Some(attempt) = hit else {
        at.install.fail(&format!(
            "the kill never came between the database folder being made and the first start's set-up in {TRIES} tries: {seen:?}"
        ));
    };
    eprintln!("the moment was hit at try {attempt} of {TRIES}: {seen:?}");

    // The service does not become healthy.
    let id = at.install.container("postgres");
    let deadline = Instant::now() + HEALTH_WAIT;
    while Instant::now() < deadline {
        let health = at.install.health(&id);
        if health == "healthy" {
            at.install.fail(
                "expected a bundled Postgres whose first start was interrupted never to be healthy, it became healthy",
            );
        }
        thread::sleep(Duration::from_secs(1));
    }
    // `up -d` does not bring vibe to running (it may end with an error,
    // or be ended at its time limit).
    let up = at.install.compose(&["up", "-d"], Duration::from_secs(120));
    let running = at
        .install
        .containers()
        .into_iter()
        .filter(|(service, _)| service == "vibe")
        .any(|(_, id)| at.install.inspect(&id)["State"]["Running"] == true);
    if running {
        at.install.fail(&format!(
            "expected vibe not started on a Postgres whose first start was interrupted\n{}",
            up.describe(&at.install.secrets())
        ));
    }
    // The administrator can look: the superuser is let in inside.
    let me = psql_when_up(&at, "select current_user").unwrap_or_else(|p| at.install.fail(&p));
    assert_eq!(me, "postgres");

    // Started again from nothing, it comes up whole.
    at.install
        .compose_ok(&["down", "--volumes", "--timeout", "5"], DOCKER_WAIT);
    let installation = at.up_and_ready();
    assert_eq!(installation["sign_in"], "none", "{installation}");
    assert_eq!(
        at.install.psql(
            "postgres",
            "select count(*) from pg_roles where rolname = 'vibe'"
        ),
        "1"
    );
}
