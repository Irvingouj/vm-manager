//! The private Postgres of a local run: found in the user's own folder,
//! downloaded once, or the Builder's own (CONTRACT.md, "The local loop: one
//! request as a test User: What a run does", step 1; ticket "Local loop:
//! run the Application on the Builder's machine and send a request as a
//! test User": nothing needs Docker, administrator rights or anything
//! installed by hand; the first run downloads what it needs into the user's
//! own folder).
//!
//! Nothing here reaches the Internet: the archive is served by a mirror of
//! the harness's own on 127.0.0.1 (`VM_LOCAL_POSTGRES_MIRROR`), holding
//! zonky's real archive of Postgres 18.6.0, which the harness fetched once
//! into its cache. The project is a new one, declaring nothing.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::path::Path;

use blackbox::local::{
    DataHome, Local, Mirror, OUTSIDER, Serving, archive_path, holds_postgres_programs,
    local_project,
};
use blackbox::project::stand_ins;
use blackbox::tool::{closed_address, mode_of};
use serde_json::json;

/// A run of the template's page as outsider, which must end with `0` and
/// answer `200`.
fn page_ok(local: &Local, dir: &Path) {
    let requested = local.request_ok(dir, "/", OUTSIDER, &[]);
    assert_eq!(requested.status(), 200, "{}", requested.describe());
    assert_eq!(requested.json(), json!({ "user": OUTSIDER }));
}

/// Fails unless the run ended with `postgres-unavailable`, exit code `1`, a
/// message naming both variables.
fn assert_unavailable(run: &blackbox::tool::Run, context: &str) {
    let message = run.json_error(1, "postgres-unavailable");
    for variable in ["VM_LOCAL_POSTGRES_MIRROR", "VM_LOCAL_POSTGRES_BIN"] {
        assert!(
            message.contains(variable),
            "{context}: expected the message to name {variable}, got {message:?}"
        );
    }
}

#[test]
fn the_first_run_downloads_postgres_once_into_the_users_data_folder() {
    let mirror = Mirror::start(Serving::Archive);
    let local = Local::new().without_bin().with_mirror(&mirror.base_url());
    let project = local_project(&local.machine, "pg-board", &[]);
    let folder = local.postgres_folder();
    assert!(!folder.exists(), "the harness's own check");

    let first = local.request_in_words(&project.dir, "/", OUTSIDER, &[]);
    first.expect_code(0);
    let words = format!("{}\n{}", first.stdout, first.stderr);
    assert!(
        words.to_lowercase().contains("download"),
        "expected the words of the first run to say that Postgres is being downloaded\n{}",
        first.describe()
    );
    assert!(
        words.contains(&folder.display().to_string()),
        "expected the words to say where to ({})\n{}",
        folder.display(),
        first.describe()
    );
    assert_eq!(
        mirror.requests(),
        vec![archive_path()],
        "expected the archive asked for once, by Maven's layout under the mirror's base URL"
    );
    assert!(folder.is_dir(), "expected {} made", folder.display());
    assert_eq!(
        mode_of(&folder),
        0o700,
        "expected the folder made with mode 0700"
    );
    assert!(
        !holds_postgres_programs(&folder).is_empty(),
        "expected Postgres's programs unpacked in {}",
        folder.display()
    );

    page_ok(&local, &project.dir);
    assert_eq!(
        mirror.requests().len(),
        1,
        "expected the second run to download nothing"
    );
    // Another project on the same machine uses the same programs.
    let other = local_project(&local.machine, "pg-other", &[]);
    page_ok(&local, &other.dir);
    assert_eq!(mirror.requests().len(), 1);
}

#[test]
fn without_an_absolute_xdg_data_home_the_folder_is_in_home() {
    for data_home in [
        DataHome::Unset,
        DataHome::Relative("relative/data".to_owned()),
    ] {
        let mirror = Mirror::start(Serving::Archive);
        let local = Local::new()
            .without_bin()
            .with_mirror(&mirror.base_url())
            .with_data_home(data_home.clone());
        let project = local_project(&local.machine, "pg-home", &[]);
        page_ok(&local, &project.dir);
        let folder = local.machine.home().join(".local/share/vm/postgres");
        assert!(
            !holds_postgres_programs(&folder).is_empty(),
            "{data_home:?}: expected Postgres downloaded into {}",
            folder.display()
        );
        assert_eq!(mode_of(&folder), 0o700, "{data_home:?}");
        assert!(
            !local.machine.cwd().join("relative").exists()
                && !local.data_home_dir().join("vm").exists(),
            "{data_home:?}: expected nothing in a relative XDG_DATA_HOME"
        );
    }
}

#[test]
fn a_download_that_fails_is_postgres_unavailable_and_leaves_nothing_that_looks_complete() {
    let local = Local::new().without_bin();
    let project = local_project(&local.machine, "pg-broken", &[]);
    let folder = local.postgres_folder();
    let closed = format!("http://{}/maven2", closed_address());
    let cut = Mirror::start(Serving::CutShort);
    let other = Mirror::start(Serving::OtherBytes);
    let missing = Mirror::start(Serving::Missing);
    for (case, url) in [
        ("cut short", cut.base_url()),
        ("another checksum", other.base_url()),
        ("not found on the mirror", missing.base_url()),
        ("a mirror that is not there", closed),
    ] {
        let run = local.request_with_env(
            &project.dir,
            "/",
            OUTSIDER,
            &[],
            &[("VM_LOCAL_POSTGRES_MIRROR", url.as_str())],
        );
        assert_unavailable(&run, case);
        assert_eq!(
            holds_postgres_programs(&folder),
            Vec::<std::path::PathBuf>::new(),
            "{case}: expected nothing left in {} that looks complete",
            folder.display()
        );
        assert!(
            local.processes().is_empty(),
            "{case}: expected nothing left running"
        );
    }
    assert!(
        !other.requests().is_empty() && !cut.requests().is_empty(),
        "the harness's own check: the mirrors were asked"
    );
    // The next good run works.
    let good = Mirror::start(Serving::Archive);
    let run = local.request_with_env(
        &project.dir,
        "/",
        OUTSIDER,
        &[],
        &[("VM_LOCAL_POSTGRES_MIRROR", good.base_url().as_str())],
    );
    blackbox::local::Requested::of(&run);
    assert_eq!(good.requests(), vec![archive_path()]);
}

#[test]
fn a_download_larger_than_256_mib_is_postgres_unavailable() {
    let huge = Mirror::start(Serving::Huge);
    let local = Local::new().without_bin().with_mirror(&huge.base_url());
    let project = local_project(&local.machine, "pg-huge", &[]);
    let run = local.request(&project.dir, "/", OUTSIDER, &[]);
    assert_unavailable(&run, "an archive of 300 MiB");
    assert_eq!(
        holds_postgres_programs(&local.postgres_folder()),
        Vec::<std::path::PathBuf>::new()
    );
}

#[test]
fn a_postgres_the_builder_has_is_used_and_nothing_is_downloaded() {
    let mirror = Mirror::start(Serving::Archive);
    let local = Local::new().with_mirror(&mirror.base_url());
    let project = local_project(&local.machine, "pg-own", &[]);
    page_ok(&local, &project.dir);
    assert!(
        mirror.requests().is_empty(),
        "expected nothing downloaded with VM_LOCAL_POSTGRES_BIN, got {:?}",
        mirror.requests()
    );
    assert!(
        !local.postgres_folder().exists(),
        "expected nothing made in the user's data folder"
    );
}

#[test]
fn a_bin_folder_without_the_programs_or_of_another_version_is_postgres_unavailable() {
    let mirror = Mirror::start(Serving::Archive);
    let probe = Local::new();
    let empty = stand_ins(&probe.machine.root().join("no-programs"), &[]);
    let says_15 = r#"case "$1" in
  --version|-V) echo "postgres (PostgreSQL) 15.4" ;;
  *) echo "a stand-in for Postgres 15 that runs nothing" >&2; exit 1 ;;
esac"#;
    let old = stand_ins(
        &probe.machine.root().join("postgres-15"),
        &[
            ("postgres", says_15),
            (
                "initdb",
                &says_15.replace("postgres (PostgreSQL)", "initdb (PostgreSQL)"),
            ),
            (
                "pg_ctl",
                &says_15.replace("postgres (PostgreSQL)", "pg_ctl (PostgreSQL)"),
            ),
        ],
    );
    for (case, bin) in [("no programs", &empty), ("Postgres 15", &old)] {
        let local = Local::new()
            .with_bin(bin.to_str().unwrap())
            .with_mirror(&mirror.base_url());
        let project = local_project(&local.machine, "pg-wrong", &[]);
        let run = local.request(&project.dir, "/", OUTSIDER, &[]);
        let message = run.json_error(1, "postgres-unavailable");
        assert!(
            message.contains("VM_LOCAL_POSTGRES_BIN"),
            "{case}: expected the message to name VM_LOCAL_POSTGRES_BIN, got {message:?}"
        );
        assert!(
            !project.dir.join(".vm/local").exists() || local.processes().is_empty(),
            "{case}"
        );
    }
    assert!(
        mirror.requests().is_empty(),
        "expected nothing downloaded in place of the Postgres named, got {:?}",
        mirror.requests()
    );
}
