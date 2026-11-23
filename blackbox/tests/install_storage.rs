//! Storage as installed (CONTRACT.md, "Storage: Installing"; ticket
//! "Storage: a shared, named file Resource, on a mounted directory now
//! and S3 later"), against the image `VIBE_INSTALL_IMAGE` names, started
//! by the files of `deploy/` themselves. Run by
//! `scripts/install-gate.sh`, like its neighbours `install_*.rs`; not
//! part of the gate.
//!
//! What is pinned, sentence by sentence:
//!
//! - `deploy/compose.yaml` gains one value and one mount: `VIBE_STORAGE_DIR`
//!   (the host's folder for Storage; `./storage` without it) is mounted
//!   **read-write** at `/var/lib/vibe/storage` (a bind, made by Compose when
//!   it is not there), and `vibe` is started with `--storage-root
//!   /var/lib/vibe/storage`. Everything else of `vibe`'s hardening is as
//!   before: read-only root filesystem, no capability, `no-new-privileges`,
//!   the image's user `65532:65532`, no tmpfs. (What the files say as text
//!   is pinned in `storage_deploy_files.rs`; this file pins what Compose
//!   and Docker do with them.)
//! - a folder holding nothing but `compose.yaml`, `.env` and `certs/`
//!   still comes up and answers; Storage registrations are then refused
//!   (`directory-missing`, or `directory-not-writable` while `storage/` is
//!   not the image user's) until the administrator does what `README.md`
//!   says. (The strict folder is pinned by
//!   `compose_yaml_alone_in_a_folder_with_its_env_and_certificates_comes_up_and_answers`
//!   in `install_variants.rs`; this file's installations hold the identity
//!   provider's authority in `ca/` and the one harness file the contract
//!   allows beside the files, so that a Platform administrator can sign in
//!   and be refused.)
//! - what the install tests show, besides what "What the install tests
//!   show" says: the `docker inspect` line becomes *no mount but
//!   `/etc/vibe/ca` read-only and `/var/lib/vibe/storage` read-write*;
//!   with `storage/` owned by `65532:65532` and `storage/marketing/` made
//!   in it, a Platform administrator registers `marketing` over HTTPS, a
//!   Contributor deploys the fixture Application `fixtures/sdk/files`
//!   (which declares it at `write`), a request as a User of a Group
//!   granted `write` puts a file, the file is on the host at
//!   `storage/marketing/<key>` with the bytes put and the Application's
//!   `get` answers it; after restarting `vibe`, after `down` then `up`,
//!   and after an upgrade to a second tag of the image, the file is there
//!   and `get` answers it; registering a folder that is not there, a
//!   file, and a folder owned by root with mode `0755` is refused
//!   `directory-missing`, `not-a-directory`, `directory-not-writable`;
//!   with `VIBE_STORAGE_DIR` naming another host folder, that folder is
//!   the one used.
//!
//! The fixture project declares two more Resources besides `marketing`:
//! `archive`, another Storage Resource (registered beside it, its folder
//! made and given the same way), and `devops`, a database that is never
//! reached (registering connects to nothing); the files `vibe pull`
//! would write for `devops` are given by hand, as the deploy itself uses
//! the definition the project holds when a database does not answer.
//!
//! `sudo -n` does not work on this machine, so the states that need root
//! (a folder owned by root, `chown 65532:65532`, reading the program's
//! files, which are the image user's with mode `0640`) are made with a
//! container of the test's own instead, as the README's `sudo` commands
//! would.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use blackbox::DOMAIN;
use blackbox::browser::{Browser, describe_steps};
use blackbox::connection::SecretKey;
use blackbox::deploy::{Deployed, commit_all, deploy_with_env, init_repository};
use blackbox::http::Response;
use blackbox::install::{COMPOSE, DOCKER_WAIT, FrontDoor, Install, Installed};
use blackbox::oidc::Person;
use blackbox::project::pulled_by_hand;
use blackbox::shared_database::token;
use blackbox::storage::{MARKETING, b64, postgres_body, storage_body};
use blackbox::structure::copy_project;
use blackbox::tool::Machine;
use serde_json::{Value, json};

/// The sub of the person who is a Platform administrator: her Group is
/// the one the OIDC connection names.
const ADMIN: &str = "00u-admin-4711";

/// The sub of the Builder: a Contributor of the fixture Application and a
/// User of its Grant, through the Group [`WRITERS`].
const BUILDER: &str = "00u-builder-4712";

/// The Group granted `write` on the fixture's Storage Resource.
const WRITERS: &str = "file-writers";

/// The other Storage Resource the fixture declares, at `read`.
const ARCHIVE: &str = "archive";

/// The database Resource the fixture declares, never reached.
const DEVOPS: &str = "devops";

/// The host folder of Storage as the Compose file mounts it by default.
const HOST_STORAGE: &str = "storage";

/// Where the program sees the storage root, as the Compose file gives it.
const STORAGE_ROOT: &str = "/var/lib/vibe/storage";

/// The image's user, as the contract names it.
const IMAGE_USER: &str = "65532:65532";

/// The image of the containers of the test's own that need root: `sudo
/// -n` does not work on this machine, so they do what the README's
/// `sudo` does (making a folder as root, `chown`, writing and reading
/// the program's files).
const ROOT_BOX: &str = "busybox:1.37.0";

/// The Application of the fixture project `fixtures/sdk/files`, as its
/// Manifest names it.
const APP: &str = "sdk-files";

/// How long a request to the deployed fixture may take: its context runs
/// the whole bundle before the handler.
const REQUEST_WAIT: Duration = Duration::from_secs(30);

/// Gives a host folder back to this test's user when the test ends, pass
/// or fail: the folder (or what a test planted in it) is the image user's
/// or root's with mode `0755`, which this test's own user cannot remove
/// the contents of, and the harness removes the installation's whole
/// folder when the test ends. A test makes it before the first `up`
/// (`up_and_ready`), since Compose may make the folder as root, and
/// declares it after the installation, so that it is dropped before the
/// installation it cleans in. While the folder is not there it does
/// nothing: a container run with a folder that is not there would make it
/// as root, the very state this gives back.
struct Given(PathBuf);

impl Drop for Given {
    fn drop(&mut self) {
        if !self.0.exists() {
            return;
        }
        let uid = blackbox::storage::my_uid().to_string();
        let volume = format!("{}:/d", self.0.display());
        let mut words: Vec<&str> = vec![
            "run", "--rm", "--pull", "never", "--volume", &volume, ROOT_BOX,
        ];
        words.push("chown");
        words.push("-R");
        words.push(&uid);
        words.push("/d");
        let ran = blackbox::install::docker(&words, DOCKER_WAIT);
        if !ran.ok() && !std::thread::panicking() {
            panic!(
                "expected to give {} back to this test's user when the test ended\n{}",
                self.0.display(),
                ran.describe(&[])
            );
        }
    }
}

/// The Platform administrator: in the Group the OIDC connection names.
fn administrator() -> Person {
    Person::new(ADMIN)
        .named("Ada Admin")
        .email("ada-admin@example.test")
        .in_groups(&[blackbox::scenario::ADMINISTRATORS])
}

/// The Builder: deploys the fixture and uses it; their Group holds the
/// Grant the fixture's put needs.
fn builder() -> Person {
    Person::new(BUILDER)
        .named("Bo Builder")
        .email("bo@example.test")
        .in_groups(&[WRITERS])
}

/// Runs `busybox <words>` with the host folder `dir` mounted at `/d`, in
/// a container of the test's own as root; fails the test with what it
/// printed unless it ends with `0`.
fn as_root(at: &Installed, dir: &Path, words: &[&str]) {
    let volume = format!("{}:/d", dir.display());
    let ran = at
        .install
        .run_own(&["--volume", &volume], ROOT_BOX, words, &[], DOCKER_WAIT);
    assert!(
        ran.ok(),
        "expected `busybox {}` on the host folder {} (mounted at /d), in a container of the test's own as root, to end with 0\n{}",
        words.join(" "),
        dir.display(),
        ran.describe(&[])
    );
}

/// The bytes of the host file `relative` under `dir`, read the same way:
/// the program's files are the image user's with mode `0640`, so this
/// test's own user cannot read them.
fn bytes_of(at: &Installed, dir: &Path, relative: &str) -> Vec<u8> {
    let path = format!("/d/{relative}");
    let volume = format!("{}:/d", dir.display());
    let ran = at.install.run_own(
        &["--volume", &volume],
        ROOT_BOX,
        &["cat", &path],
        &[],
        DOCKER_WAIT,
    );
    assert!(
        ran.ok(),
        "expected to read {relative} under {} with a container of the test's own as root\n{}",
        dir.display(),
        ran.describe(&[])
    );
    ran.stdout.into_bytes()
}

/// `POST /v1/resources` over HTTPS as the Platform administrator.
fn register(at: &Installed, credential: &str, body: &Value) -> Response {
    at.reach
        .api("/v1/resources")
        .method("POST")
        .json(body)
        .bearer(credential)
        .send()
}

/// `POST /v1/resources/<resource>/grants` over HTTPS as the Platform
/// administrator, which must be taken (`200` with `{ resource, group,
/// access }`).
fn grant(at: &Installed, credential: &str, resource: &str, group: &str, access: &str) {
    let answer = at
        .reach
        .api(&format!("/v1/resources/{resource}/grants"))
        .method("POST")
        .json(&json!({ "group": group, "access": access }))
        .bearer(credential)
        .send();
    let masked = blackbox::connection::masked(&answer.describe(), &[credential]);
    assert_eq!(
        answer.status, 200,
        "expected `200` granting {group:?} {access:?} on {resource:?} over HTTPS\n{masked}"
    );
    assert_eq!(
        answer.json(),
        json!({ "resource": resource, "group": group, "access": access }),
        "expected the answer of a Grant to be {{ resource, group, access }}\n{masked}"
    );
}

/// Fails unless registering `body` over HTTPS is refused with this status
/// and reason.
fn assert_refused(at: &Installed, credential: &str, body: &Value, status: u16, reason: &str) {
    let answer = register(at, credential, body);
    let json = answer.json();
    let found = json["error"]["reason"].as_str().unwrap_or_default();
    let masked = blackbox::connection::masked(&answer.describe(), &[credential]);
    assert_eq!(
        (answer.status, found),
        (status, reason),
        "expected registering {:?} over HTTPS to be refused {status} {reason}\n{masked}",
        body["name"]
    );
}

/// Connects OIDC as the walk does: the setup credential from inside the
/// container, then the tool outside over HTTPS.
fn connect_oidc(at: &mut Installed) {
    let setup = at.setup_credential();
    let machine = Machine::new();
    let connected = at.connect_oidc(&machine, &setup);
    assert_eq!(
        connected["connected_by"], "setup-credential",
        "expected OIDC connected with the setup credential: {connected}"
    );
}

/// The tool signed in on a machine of its own over HTTPS as the Platform
/// administrator, and that tool's credential; the sign-in's answer says
/// they are one.
fn administrator_signed_in(at: &mut Installed) -> (Machine, String) {
    let machine = Machine::new();
    let (signed_in, _) = at
        .reach
        .sign_in_tool(&at.idp, &machine, ADMIN, at.tool_ca_file());
    assert_eq!(
        signed_in["platform_administrator"], true,
        "expected the person of the administrators' Group to be a Platform administrator: {signed_in}"
    );
    let credential = at.reach.kept_credential(&machine);
    at.install.hide(&credential);
    (machine, credential)
}

/// The words of the container's command line as it was started.
fn command_line(inspected: &Value) -> Vec<String> {
    let mut words = vec![inspected["Path"].as_str().unwrap_or_default().to_owned()];
    words.extend(
        inspected["Args"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|w| w.as_str().unwrap_or_default().to_owned()),
    );
    words
}

/// Fails unless `words` holds `option` followed by `value`.
fn assert_option(words: &[String], option: &str, value: &str) {
    assert!(
        words.windows(2).any(|w| w[0] == option && w[1] == value),
        "expected `{option} {value}` in vibe's command line {words:?}"
    );
}

/// What `docker inspect` says of the running `vibe` container (CONTRACT.md,
/// "Storage: Installing"): the line of "What the install tests show"
/// becomes *no mount but `/etc/vibe/ca` read-only and
/// `/var/lib/vibe/storage` read-write*, and everything else of `vibe`'s
/// hardening is as before: read-only root filesystem, no capability,
/// `no-new-privileges`, the image's user `65532:65532`, no tmpfs, with
/// `--storage-root /var/lib/vibe/storage` on its command line.
fn assert_mounted_storage(at: &Installed, host_folder: &Path) {
    let install = &at.install;
    let vibe = install.inspect(&install.container("vibe"));
    let host = &vibe["HostConfig"];
    let context = || {
        format!(
            "docker inspect of vibe: HostConfig {} Config.User {} Mounts {}",
            host, vibe["Config"]["User"], vibe["Mounts"]
        )
    };
    assert_eq!(vibe["State"]["Running"], true, "{}", context());

    // Everything else of `vibe`'s hardening is as before.
    assert_eq!(
        host["ReadonlyRootfs"],
        true,
        "expected a read-only root filesystem: {}",
        context()
    );
    let dropped: Vec<String> = host["CapDrop"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|c| c.as_str().unwrap_or_default().to_ascii_uppercase())
        .collect();
    assert!(
        dropped.iter().any(|c| c == "ALL" || c == "CAP_ALL"),
        "expected every capability dropped: {}",
        context()
    );
    assert!(
        host["CapAdd"].as_array().is_none_or(Vec::is_empty),
        "expected no capability added: {}",
        context()
    );
    assert_eq!(host["Privileged"], false, "{}", context());
    let security: Vec<String> = host["SecurityOpt"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|o| o.as_str().unwrap_or_default().replace('=', ":"))
        .collect();
    assert!(
        security
            .iter()
            .any(|o| o == "no-new-privileges" || o == "no-new-privileges:true"),
        "expected no-new-privileges: {}",
        context()
    );
    assert_eq!(
        vibe["Config"]["User"],
        json!(IMAGE_USER),
        "expected the image's user {IMAGE_USER}: {}",
        context()
    );
    assert!(
        host["Tmpfs"]
            .as_object()
            .is_none_or(serde_json::Map::is_empty),
        "expected no tmpfs: {}",
        context()
    );

    // No mount but /etc/vibe/ca read-only and /var/lib/vibe/storage
    // read-write.
    let mounts = vibe["Mounts"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        mounts.len(),
        2,
        "expected two mounts of vibe's, /etc/vibe/ca read-only and {STORAGE_ROOT} read-write: {}",
        context()
    );
    let by_destination = |destination: &str| {
        mounts
            .iter()
            .find(|m| m["Destination"] == destination)
            .unwrap_or_else(|| panic!("expected a mount at {destination}: {}", context()))
    };
    let ca = by_destination("/etc/vibe/ca");
    assert_eq!(ca["Type"], "bind", "{}", context());
    assert_eq!(
        ca["RW"],
        false,
        "expected /etc/vibe/ca read-only: {}",
        context()
    );
    let storage = by_destination(STORAGE_ROOT);
    assert_eq!(storage["Type"], "bind", "{}", context());
    assert_eq!(
        storage["RW"],
        true,
        "expected {STORAGE_ROOT} read-write: {}",
        context()
    );
    let source = storage["Source"].as_str().unwrap_or_default();
    assert_eq!(
        Path::new(source),
        host_folder,
        "expected the storage mount's source to be the host folder {}: {}",
        host_folder.display(),
        context()
    );

    // Started with `--storage-root /var/lib/vibe/storage`, and nothing
    // else of its command line changed.
    let words = command_line(&vibe);
    assert!(
        words.iter().any(|w| w == "serve"),
        "expected `serve` in {words:?}"
    );
    assert_option(&words, "--domain", DOMAIN);
    assert_option(&words, "--listen", "0.0.0.0:8080");
    assert_option(&words, "--public-scheme", "https");
    assert_option(&words, "--public-port", &at.reach.port.to_string());
    assert_option(&words, "--storage-root", STORAGE_ROOT);
    assert!(
        !words.iter().any(|w| w.contains("stand-in")),
        "expected never --stand-in-sign-in: {words:?}"
    );
}

/// `POST /put?key=<key>` with `bytes` as the Application's User in
/// `browser`; the JSON answer, which must be `200`.
fn put_through(at: &Installed, browser: &mut Browser, key: &str, bytes: &[u8]) -> Value {
    let url = at.reach.url(APP, &format!("/put?key={key}"));
    let request = browser
        .request("POST", &url)
        .header("content-type", "application/octet-stream")
        .body(bytes)
        .timeout(REQUEST_WAIT);
    let response = browser.send(&url, &request);
    if response.status != 200 {
        at.install.fail(&format!(
            "expected `200` putting {key:?} through {APP} as the User in the browser, got\n{}",
            response.describe()
        ));
    }
    response.json()
}

/// `GET /get?key=<key>` as the Application's User in `browser`; the JSON
/// answer, which must be `200`.
fn get_through(at: &Installed, browser: &mut Browser, key: &str) -> Value {
    let url = at.reach.url(APP, &format!("/get?key={key}"));
    let request = browser.request("GET", &url).timeout(REQUEST_WAIT);
    let response = browser.send(&url, &request);
    if response.status != 200 {
        at.install.fail(&format!(
            "expected `200` from {APP} for GET /get?key={key}, got\n{}",
            response.describe()
        ));
    }
    response.json()
}

/// `docker compose config --format json` with the `.env` as set; fails
/// unless it ends with `0`.
fn configured(install: &Install) -> Value {
    install.write_env();
    let ran = install.compose(&["config", "--format", "json"], DOCKER_WAIT);
    assert!(
        ran.ok(),
        "expected `docker compose config` to take the files with the values set\n{}",
        ran.describe(&install.secrets())
    );
    serde_json::from_str(&ran.stdout).unwrap_or_else(|e| {
        panic!(
            "expected JSON from `docker compose config --format json` ({e})\n{}",
            ran.describe(&install.secrets())
        )
    })
}

/// The service's command as one list of words.
fn command_of(config: &Value, service: &str) -> Vec<String> {
    let words = |value: &Value| -> Vec<String> {
        value
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|w| w.as_str().unwrap_or_default().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut all = words(&config["services"][service]["entrypoint"]);
    all.extend(words(&config["services"][service]["command"]));
    all
}

/// The volume of the service mounted at `target`: its whole object.
fn volume_at(config: &Value, service: &str, target: &str) -> Value {
    config["services"][service]["volumes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|v| v["target"] == target)
        .unwrap_or_else(|| panic!("expected a volume of {service}'s mounted at {target}"))
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn compose_gains_the_storage_value_and_mount_and_starts_vibe_with_the_storage_root() {
    // The files as Compose reads them: no image and nothing started (what
    // the files themselves say is pinned in `storage_deploy_files.rs`).
    let mut install = Install::new(&[COMPOSE]);
    install.set("VIBE_DOMAIN", DOMAIN);
    install.set_secret("VIBE_SECRET_KEY", SecretKey::fresh().expose());
    install.set_secret("POSTGRES_PASSWORD", &token());

    // Without `VIBE_STORAGE_DIR`: the host folder is `./storage`, the one
    // beside the Compose file, mounted read-write at the storage root.
    let config = configured(&install);
    let command = command_of(&config, "vibe");
    assert!(
        command
            .windows(2)
            .any(|w| w[0] == "--storage-root" && w[1] == STORAGE_ROOT),
        "expected `--storage-root {STORAGE_ROOT}` in vibe's command {command:?} (CONTRACT.md, \"Storage: Installing\")"
    );
    let ca = volume_at(&config, "vibe", "/etc/vibe/ca");
    assert_eq!(ca["type"], "bind", "the ca bind: {ca}");
    assert_eq!(ca["read_only"], true, "the ca bind: {ca}");
    assert_eq!(
        Path::new(ca["source"].as_str().unwrap_or_default()),
        install.dir().join("ca"),
        "expected the folder ca/ beside the Compose file mounted"
    );
    let storage = volume_at(&config, "vibe", STORAGE_ROOT);
    assert_eq!(storage["type"], "bind", "expected a bind: {storage}");
    assert_ne!(
        storage["read_only"], true,
        "expected the storage mount read-write: {storage}"
    );
    assert_eq!(
        Path::new(storage["source"].as_str().unwrap_or_default()),
        install.dir().join(HOST_STORAGE),
        "expected the host folder ./storage without VIBE_STORAGE_DIR"
    );

    // Everything else of `vibe`'s hardening is as before.
    let vibe = &config["services"]["vibe"];
    assert_eq!(
        vibe["read_only"], true,
        "expected a read-only root filesystem: {vibe}"
    );
    let cap_drop = vibe["cap_drop"].as_array().cloned().unwrap_or_default();
    let dropped: Vec<&str> = cap_drop
        .iter()
        .map(|c| c.as_str().unwrap_or_default())
        .collect();
    assert!(
        dropped.contains(&"ALL"),
        "expected every capability dropped: {vibe}"
    );
    assert!(
        vibe["cap_add"].as_array().is_none_or(Vec::is_empty),
        "expected no capability added: {vibe}"
    );
    let security: Vec<String> = vibe["security_opt"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|o| o.as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(
        security.iter().any(|o| o == "no-new-privileges:true"),
        "expected no-new-privileges: {vibe}"
    );

    // With `VIBE_STORAGE_DIR` naming another host folder, that folder is
    // the one mounted.
    let elsewhere = install.dir().join("elsewhere");
    install.set("VIBE_STORAGE_DIR", &elsewhere.display().to_string());
    let config = configured(&install);
    let storage = volume_at(&config, "vibe", STORAGE_ROOT);
    assert_eq!(
        Path::new(storage["source"].as_str().unwrap_or_default()),
        elsewhere,
        "expected the host folder VIBE_STORAGE_DIR names to be the one mounted at {STORAGE_ROOT}"
    );
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn storage_mounted_read_write_the_file_a_request_puts_is_on_the_host_and_answered_across_restart_down_up_and_an_upgrade()
 {
    let mut at = Installed::prepare(
        &[COMPOSE],
        &[administrator(), builder()],
        FrontDoor::TheFilesProxy,
    );

    // The folder is given back on every ending, the guard made before the
    // first `up`: Compose makes `storage/` as root, and if bringing the
    // installation up panics nothing else would give it back before the
    // harness removes the installation's folder.
    let storage = at.install.dir().join(HOST_STORAGE);
    let _given_back = Given(storage.clone());

    // `storage/` owned by `65532:65532`, with the folders of the two
    // Storage Resources the fixture declares made in it: the state the
    // contract's story begins from. The folders are the test's own to
    // make (Compose mounts what is there); the chown is done in a
    // container as root, as the README's `sudo chown 65532:65532 storage`.
    for folder in [MARKETING, ARCHIVE] {
        let dir = storage.join(folder);
        fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("cannot make {}: {e}", dir.display()));
    }
    as_root(&at, &storage, &["chown", "-R", IMAGE_USER, "/d"]);

    // The installation comes up with the three required values.
    let installation = at.up_and_ready();
    assert_eq!(
        installation,
        json!({ "domain": DOMAIN, "sign_in": "none", "sign_in_address": null }),
        "expected the installation to answer before OIDC is connected"
    );

    // The `docker inspect` line: no mount but `/etc/vibe/ca` read-only and
    // `/var/lib/vibe/storage` read-write, the hardening as before, the
    // image's user `65532:65532`, `--storage-root` on the command line.
    assert_mounted_storage(&at, &storage);

    // A Platform administrator signs the tool in over HTTPS and registers
    // `marketing` on the folder as the program sees it.
    connect_oidc(&mut at);
    let (_admin_machine, admin) = administrator_signed_in(&mut at);
    let marketing = register(
        &at,
        &admin,
        &storage_body(MARKETING, &format!("{STORAGE_ROOT}/{MARKETING}")),
    );
    assert_eq!(
        marketing.status,
        201,
        "expected `201` registering {MARKETING} over HTTPS as a Platform administrator\n{}",
        blackbox::connection::masked(&marketing.describe(), &[&admin])
    );
    // `archive`, the fixture's other Storage Resource, and `devops`, its
    // database Resource (never reached: registering connects to nothing),
    // so that the deploy has every Resource the fixture declares.
    let archive = register(
        &at,
        &admin,
        &storage_body(ARCHIVE, &format!("{STORAGE_ROOT}/{ARCHIVE}")),
    );
    assert_eq!(
        archive.status,
        201,
        "expected `201` registering {ARCHIVE} over HTTPS\n{}",
        blackbox::connection::masked(&archive.describe(), &[&admin])
    );
    let devops = register(&at, &admin, &postgres_body(DEVOPS));
    assert_eq!(
        devops.status,
        201,
        "expected `201` registering {DEVOPS} (a database never reached) over HTTPS\n{}",
        blackbox::connection::masked(&devops.describe(), &[&admin])
    );
    // The Group of the fixture's Users is granted `write` on it (and what
    // its deploy needs on the two others).
    grant(&at, &admin, MARKETING, WRITERS, "write");
    grant(&at, &admin, ARCHIVE, WRITERS, "read");

    // A Contributor deploys the fixture Application `fixtures/sdk/files`,
    // which declares `marketing` at `write`: the project is copied as it
    // is, beside the pulled files of the database it declares but never
    // reaches, written as `vibe pull` writes them (a deploy pulls each
    // declared Resource again; for a database that does not answer it
    // warns and uses the definition the project holds).
    let machine = Machine::new();
    let (signed_in, _) = at
        .reach
        .sign_in_tool(&at.idp, &machine, BUILDER, at.tool_ca_file());
    assert_eq!(signed_in["user"], BUILDER, "{signed_in}");
    let project = copy_project("files");
    project.write("migrations/.gitkeep", "");
    pulled_by_hand(&project, DEVOPS);
    init_repository(&project.dir);
    commit_all(&project.dir, "The fixture of Storage, as it is");
    let run = deploy_with_env(
        &machine,
        &project.dir,
        &[],
        &[("VIBE_CA_FILE", at.tool_ca_file())],
    );
    if run.code != Some(0) {
        at.install.fail(&format!(
            "expected `vibe deploy` of the fixture project fixtures/sdk/files, which declares {MARKETING} at `write`, to end with 0\n{}",
            blackbox::connection::hide(&run, &at.install.secrets()).describe()
        ));
    }
    let deployed = Deployed::of(&run);
    assert_eq!(deployed.application, APP);
    assert_eq!(deployed.version, 1);
    assert!(
        deployed.created,
        "expected this deploy to create the Application: {}",
        deployed.run.describe()
    );

    // A request as a User of a Group granted `write` puts a file. The
    // browser goes to the Application first: the sign-in dance gives the
    // Application its session cookie, and the get of a key nothing holds
    // answers null.
    let key = "hello.txt";
    let body = "hello from the install test";
    let mut browser = at.reach.browser_as(&at.idp, BUILDER);
    let steps = browser.navigate(&at.reach.url(APP, &format!("/get?key={key}")));
    let last = steps.last().unwrap();
    assert_eq!(
        last.response.status,
        200,
        "expected the signed-in Builder to reach {APP}'s get, which answers null for a file nothing put\n{}",
        describe_steps(&steps)
    );
    assert_eq!(
        last.response.json()["file"],
        Value::Null,
        "expected no file yet, so a get of {key:?} answers null"
    );

    let put = put_through(&at, &mut browser, key, body.as_bytes());
    assert_eq!(put["info"]["key"], json!(key), "{put}");
    assert_eq!(put["info"]["size"], json!(body.len()), "{put}");

    // The file is on the host at storage/marketing/<key> with the bytes
    // put.
    let file = storage.join(MARKETING).join(key);
    assert!(
        file.is_file(),
        "expected the file on the host at {} with the bytes put",
        file.display()
    );
    assert_eq!(
        bytes_of(&at, &storage, &format!("{MARKETING}/{key}")),
        body.as_bytes(),
        "expected {} to hold the bytes put",
        file.display()
    );

    // And the Application's `get` answers it.
    let got = get_through(&at, &mut browser, key);
    assert_eq!(got["file"]["info"]["key"], json!(key), "{got}");
    assert_eq!(
        got["file"]["bodyB64"],
        json!(b64(body.as_bytes())),
        "expected the Application's get to answer the bytes put: {got}"
    );

    // After restarting `vibe`, after `down` then `up`, and after an
    // upgrade to a second tag of the image: the file is there and `get`
    // answers it.
    let still = |at: &Installed, browser: &mut Browser, what: &str| {
        assert_eq!(
            bytes_of(at, &storage, &format!("{MARKETING}/{key}")),
            body.as_bytes(),
            "{what}: expected the file to be on the host with the bytes put"
        );
        let got = get_through(at, browser, key);
        assert_eq!(
            got["file"]["bodyB64"],
            json!(b64(body.as_bytes())),
            "{what}: expected the Application's `get` to answer the file"
        );
    };

    at.install.compose_ok(&["restart", "vibe"], DOCKER_WAIT);
    at.reach.wait_ready(&at.install);
    still(&at, &mut browser, "after restarting vibe");

    at.install.compose_ok(&["down"], DOCKER_WAIT);
    at.install.up();
    at.reach.wait_ready(&at.install);
    still(&at, &mut browser, "after `down` then `up`");

    let upgraded = at.install.second_tag(&at.image);
    at.install.set("VIBE_IMAGE", &upgraded);
    at.install.up();
    at.reach.wait_ready(&at.install);
    let vibe = at.install.inspect(&at.install.container("vibe"));
    assert_eq!(
        vibe["Config"]["Image"], upgraded,
        "expected vibe started from the image VIBE_IMAGE now names"
    );
    still(
        &at,
        &mut browser,
        "after an upgrade to a second tag of the image",
    );
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn registrations_are_refused_until_the_administrator_makes_and_gives_the_folder() {
    let mut at = Installed::prepare(&[COMPOSE], &[administrator()], FrontDoor::TheFilesProxy);
    // The folder is given back on every ending, the guard made before the
    // first `up`: Compose makes `storage/` as root, and if bringing the
    // installation up panics nothing else would give it back before the
    // harness removes the installation's folder. Nothing is made for
    // Storage otherwise: the installation comes up and answers.
    let storage = at.install.dir().join(HOST_STORAGE);
    let _given_back = Given(storage.clone());
    let installation = at.up_and_ready();
    assert_eq!(
        installation,
        json!({ "domain": DOMAIN, "sign_in": "none", "sign_in_address": null }),
        "expected the installation to come up and answer"
    );

    // The bind is made by Compose when the folder is not there: `storage/`
    // is root's, not the image user's.
    assert!(
        storage.is_dir(),
        "expected Compose to make {} for the storage bind (CONTRACT.md, \"Storage: Installing\")",
        storage.display()
    );
    let made = fs::symlink_metadata(&storage).unwrap();
    assert_eq!(
        made.uid(),
        0,
        "expected the folder Compose made to be root's, not the image user's"
    );

    connect_oidc(&mut at);
    let (_machine, admin) = administrator_signed_in(&mut at);

    // Registering a folder that is not there: `directory-missing`.
    assert_refused(
        &at,
        &admin,
        &storage_body(MARKETING, &format!("{STORAGE_ROOT}/{MARKETING}")),
        422,
        "directory-missing",
    );

    // A file in the storage root: `not-a-directory`.
    as_root(&at, &storage, &["touch", "/d/a_file"]);
    assert_refused(
        &at,
        &admin,
        &storage_body("over_a_file", &format!("{STORAGE_ROOT}/a_file")),
        422,
        "not-a-directory",
    );

    // A folder owned by root with mode `0755` — the administrator's plain
    // `mkdir storage/marketing`, while `storage/` is not the image user's
    // — : `directory-not-writable`.
    as_root(&at, &storage, &["mkdir", &format!("/d/{MARKETING}")]);
    let folder = fs::symlink_metadata(storage.join(MARKETING)).unwrap();
    assert_eq!(
        (folder.uid(), folder.mode() & 0o777),
        (0, 0o755),
        "expected a folder owned by root with mode 0755"
    );
    assert_refused(
        &at,
        &admin,
        &storage_body(MARKETING, &format!("{STORAGE_ROOT}/{MARKETING}")),
        422,
        "directory-not-writable",
    );

    // Until the administrator does what the README says — the folder
    // given to the image's user, the `mkdir` and `chown` way — the
    // registration is taken.
    as_root(
        &at,
        &storage,
        &["chown", IMAGE_USER, &format!("/d/{MARKETING}")],
    );
    let answer = register(
        &at,
        &admin,
        &storage_body(MARKETING, &format!("{STORAGE_ROOT}/{MARKETING}")),
    );
    assert_eq!(
        answer.status,
        201,
        "expected registering {MARKETING} over HTTPS to be taken once its folder is the image user's\n{}",
        blackbox::connection::masked(&answer.describe(), &[&admin])
    );
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn vibe_storage_dir_names_the_host_folder_that_is_used() {
    let mut at = Installed::prepare(&[COMPOSE], &[administrator()], FrontDoor::TheFilesProxy);
    // The folder is given back on every ending, the guard made before the
    // first `up` and before it is given to the image's user.
    let elsewhere = at.install.dir().join("elsewhere");
    let _given_back = Given(elsewhere.clone());
    // Another host folder, as the value names it, with the folder of the
    // Resource made in it and given to the image's user.
    let marketing = elsewhere.join(MARKETING);
    fs::create_dir_all(&marketing)
        .unwrap_or_else(|e| panic!("cannot make {}: {e}", marketing.display()));
    as_root(&at, &elsewhere, &["chown", "-R", IMAGE_USER, "/d"]);
    at.install
        .set("VIBE_STORAGE_DIR", &elsewhere.display().to_string());

    let installation = at.up_and_ready();
    assert_eq!(
        installation["sign_in"], "none",
        "expected the installation to come up and answer: {installation}"
    );

    // The default folder is not made: the value replaced it.
    let not_made = at.install.dir().join(HOST_STORAGE);
    assert!(
        !not_made.exists(),
        "expected no {} beside the Compose file: VIBE_STORAGE_DIR names another host folder",
        not_made.display()
    );

    // The mount is of the named folder, read-write, with `--storage-root`
    // on the command line.
    assert_mounted_storage(&at, &elsewhere);

    // And that folder is the one used: a Resource registered on a folder
    // of it is taken, and the program's own files are in it.
    connect_oidc(&mut at);
    let (_machine, admin) = administrator_signed_in(&mut at);
    let answer = register(
        &at,
        &admin,
        &storage_body(MARKETING, &format!("{STORAGE_ROOT}/{MARKETING}")),
    );
    assert_eq!(
        answer.status,
        201,
        "expected `201` registering {MARKETING} on a folder of the one VIBE_STORAGE_DIR names\n{}",
        blackbox::connection::masked(&answer.describe(), &[&admin])
    );
    assert!(
        marketing.join(".vibe-resource").is_file(),
        "expected the program's `.vibe-resource` in the named folder's Resource directory"
    );
    assert!(
        marketing.join(".vibe-staging").is_dir(),
        "expected the program's `.vibe-staging/` in the named folder's Resource directory"
    );
}
