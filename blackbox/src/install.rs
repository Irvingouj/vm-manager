//! What the install tests share (CONTRACT.md, "Installing: one image and a
//! Compose file"): the image `VIBE_INSTALL_IMAGE` names, the files of
//! `deploy/` copied unchanged to a folder of the test's own, and an
//! installation started from them with Docker Compose, looked at from
//! outside: commands and their output, `docker inspect`, HTTP over TLS
//! through the Compose file's proxy, the containers' logs.
//!
//! - [`image`] is the image under test, and fails at once, saying to run
//!   `scripts/install-gate.sh`, when `VIBE_INSTALL_IMAGE` is not set, Docker
//!   (with Compose) is not there, or the image is not there.
//! - [`docker`] runs one `docker` command with a time limit, in an
//!   environment without the installation's values (so that nothing of the
//!   test's own environment is read by Compose in place of the `.env`).
//! - [`Install`] is one installation: a folder holding copies of
//!   `deploy/compose.yaml` and its two other files byte for byte, a `.env`
//!   the test writes, `certs/fullchain.pem` and `certs/privkey.pem`, `ca/`,
//!   and the one file the contract allows the tests beside them
//!   ([`HARNESS_FILE`], name resolution for `vibe` only); a Compose project
//!   name of its own (`vibe-install-<random>`). Dropping it takes everything
//!   down, passed or failed: `docker compose down --volumes
//!   --remove-orphans`, then whatever still carries the project's label,
//!   the containers and tags of the test's own, and the folder. When the
//!   test is failing, the containers' logs are written first (secrets
//!   masked), since they are all anyone will have to go on.
//! - [`Reach`] is how a person and the tool reach an installation from
//!   this machine: `https://<name>.<domain>:<port>` at `127.0.0.1:<port>`,
//!   over TLS, trusting the authority of the wildcard certificate.
//! - [`OutsideProvider`] is the harness's identity provider as the
//!   installation reaches it: behind a TLS front bound to the Docker
//!   bridge's address (never `0.0.0.0`), with the issuer
//!   `https://idp.harness.test:<port>` and a certificate under a private
//!   authority the installation trusts through `VIBE_CA_FILE` in `ca/`.
//!   The container finds the name through [`HARNESS_FILE`]; the harness's
//!   browser through [`Browser::resolve`].
//!
//! Every `docker` call has a time limit; a call that fails or runs out of
//! time fails the test with the command, its output and the containers'
//! logs. Secrets of a test (the installation's key, the passwords, the
//! setup credential, credentials, the client secret) are made per test and
//! masked in every message.

use std::cell::RefCell;
use std::fs;
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use rustls::pki_types::CertificateDer;
use serde_json::Value;
use url::Url;

use crate::browser::{Browser, Step, describe_steps, url};
use crate::connection::masked;
use crate::http::{Request, Response};
use crate::oidc::{Person, Provider};
use crate::program::run_to_exit;
use crate::shared_database::token;
use crate::tls::{Authority, Bound, Front, Identity};
use crate::tool::{Machine, Run};
use crate::tool_sign_ins::{BrowserCommand, Decision, OPEN_TIMEOUT, forms};
use crate::vm::DOMAIN;

/// What every install test says when it cannot run.
pub const INSTALL_GATE: &str = "the install tests are run by scripts/install-gate.sh, which builds the image from the working tree and gives its name in VIBE_INSTALL_IMAGE (with Docker and Compose on the machine)";

/// The longest one `docker` command may take, but those below.
pub const DOCKER_WAIT: Duration = Duration::from_secs(120);
/// The longest `docker compose up -d` may take: the first one on a machine
/// pulls the proxy's image.
pub const UP_WAIT: Duration = Duration::from_secs(600);
/// The longest an installation may take to answer through its proxy once
/// `up` has returned (Postgres made ready, the platform database made).
pub const READY_WAIT: Duration = Duration::from_secs(180);
/// The longest a Postgres of the test's own may take to take connections.
pub const POSTGRES_READY: Duration = Duration::from_secs(90);
/// The longest a request with a body at the API's limit (16 MiB) may take
/// to be sent and answered.
pub const BODY_WAIT: Duration = Duration::from_secs(60);
/// The API's limit on a request's body (CONTRACT.md, "The platform's API":
/// exactly this is accepted, one byte more is `413` `too-large`).
pub const API_BODY_LIMIT: usize = 16 * 1024 * 1024;
/// The longest `docker export` of the image may take.
pub const EXPORT_WAIT: Duration = Duration::from_secs(300);

/// The Compose file, as downloaded.
pub const COMPOSE: &str = "compose.yaml";
/// The file for the administrator's own reverse proxy.
pub const OWN_PROXY: &str = "compose.own-proxy.yaml";
/// The file for the administrator's own Postgres.
pub const OWN_POSTGRES: &str = "compose.own-postgres.yaml";
/// The file that has `vibe` serve TLS itself in place of the proxy
/// (CONTRACT.md, "The program serves TLS itself: The Compose file").
pub const OWN_TLS: &str = "compose.own-tls.yaml";
/// The one file the tests add: name resolution to the identity provider,
/// for `vibe` only.
pub const HARNESS_FILE: &str = "compose.harness.yaml";

/// The identity provider's name, as the installation and the browser reach
/// it.
pub const IDP_HOST: &str = "idp.harness.test";

/// The image Postgres's own checks are run with (already on the machine:
/// the image the bundled Postgres is, by the contract's version).
pub const POSTGRES_18_IMAGE: &str = "postgres:18";

/// The values of the contract's table, and those of the two other files:
/// none of them is taken from the test's own environment.
pub const VALUES: [&str; 10] = [
    "VIBE_DOMAIN",
    "VIBE_SECRET_KEY",
    "POSTGRES_PASSWORD",
    "VIBE_BIND",
    "VIBE_HTTPS_PORT",
    "VIBE_IMAGE",
    "VIBE_CA_FILE",
    "VIBE_DATABASE_URL",
    "VIBE_HTTP_BIND",
    "VIBE_HTTP_PORT",
];

/// One `docker` command run to its end (or killed at its time limit).
#[derive(Clone, Debug)]
pub struct Ran {
    /// The command as text (never holding a secret: secrets are given
    /// through the environment or files).
    pub command: String,
    /// `None` when it was killed at its time limit, or could not start.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub elapsed: Duration,
}

impl Ran {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// The command, how it ended and what it printed, each of `secrets`
    /// (and anything shaped like a credential) masked.
    pub fn describe(&self, secrets: &[&str]) -> String {
        let ended = match self.code {
            Some(code) => format!("exit code {code}"),
            None => "no exit code: killed at its time limit, or it could not start".to_owned(),
        };
        masked(
            &format!(
                "$ {}\n({ended}, after {:?})\nstandard output:\n{}\nstandard error:\n{}",
                self.command, self.elapsed, self.stdout, self.stderr
            ),
            secrets,
        )
    }
}

/// Runs `docker <args>` in `dir` (or here) with these variables added to
/// an environment from which every value of [`VALUES`] and every
/// `COMPOSE_*` was taken out, killed at `timeout`.
pub fn docker_with(
    dir: Option<&Path>,
    args: &[&str],
    env: &[(&str, &str)],
    timeout: Duration,
) -> Ran {
    let mut command = Command::new("docker");
    command.args(args);
    for (name, _) in std::env::vars_os() {
        let text = name.to_string_lossy();
        if text.starts_with("COMPOSE_") || VALUES.contains(&text.as_ref()) {
            command.env_remove(&name);
        }
    }
    for (name, value) in env {
        command.env(name, value);
    }
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let shown = format!("docker {}", args.join(" "));
    let began = Instant::now();
    match run_to_exit(command, timeout) {
        Ok(finished) => Ran {
            command: shown,
            code: finished.status.and_then(|s| s.code()),
            stdout: finished.stdout,
            stderr: finished.stderr,
            elapsed: began.elapsed(),
        },
        Err(problem) => Ran {
            command: shown,
            code: None,
            stdout: String::new(),
            stderr: problem,
            elapsed: began.elapsed(),
        },
    }
}

/// Runs `docker <args>`, killed at `timeout`.
pub fn docker(args: &[&str], timeout: Duration) -> Ran {
    docker_with(None, args, &[], timeout)
}

/// Runs `docker <args>`; fails the test, saying what ran and what it
/// printed, unless it ends with `0`.
pub fn docker_ok(args: &[&str], timeout: Duration) -> Ran {
    let ran = docker(args, timeout);
    assert!(
        ran.ok(),
        "expected this docker command to end with 0\n{}",
        ran.describe(&[])
    );
    ran
}

/// Fails the test at once, saying to run `scripts/install-gate.sh`, unless
/// `docker compose version` answers.
pub fn docker_present() {
    let ran = docker(&["compose", "version"], DOCKER_WAIT);
    assert!(
        ran.ok(),
        "Docker with Compose is not there (`docker compose version` did not answer): {INSTALL_GATE}\n{}",
        ran.describe(&[])
    );
}

/// The image under test: `VIBE_INSTALL_IMAGE`. Fails the test at once,
/// saying to run `scripts/install-gate.sh`, when it is not set, when Docker
/// is not there, or when the image is not there.
pub fn image() -> String {
    let image = std::env::var("VIBE_INSTALL_IMAGE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| panic!("VIBE_INSTALL_IMAGE is not set: {INSTALL_GATE}"));
    docker_present();
    let ran = docker(
        &["image", "inspect", "--format", "{{.Id}}", &image],
        DOCKER_WAIT,
    );
    assert!(
        ran.ok(),
        "the image {image:?} that VIBE_INSTALL_IMAGE names is not there (`docker image inspect` failed): {INSTALL_GATE}\n{}",
        ran.describe(&[])
    );
    image
}

/// How long one `docker pull` of a public image may take.
pub const PULL_WAIT: Duration = Duration::from_secs(600);

/// Makes sure a public image the harness runs a container of its own from
/// (`postgres:17`, `postgres:18`) is on the machine: pulled when it is not,
/// at most [`PULL_WAIT`], failing the test saying the pull failed. So no
/// test depends on what the machine already holds or on an earlier test
/// (whose Compose pulled an image). The image under test
/// (`VIBE_INSTALL_IMAGE`) is never pulled: it must be on the machine,
/// built by `scripts/install-gate.sh`.
pub fn public_image(image: &str) {
    let present = || {
        docker(
            &["image", "inspect", "--format", "{{.Id}}", image],
            DOCKER_WAIT,
        )
        .ok()
    };
    if present() {
        return;
    }
    let under_test = std::env::var("VIBE_INSTALL_IMAGE").ok();
    assert!(
        under_test.as_deref() != Some(image),
        "the image under test {image:?} is not on the machine, and is never pulled: {INSTALL_GATE}"
    );
    let ran = docker(&["pull", "--quiet", image], PULL_WAIT);
    assert!(
        ran.ok() && present(),
        "the harness's own container needs the public image {image:?}, which is not on this machine, and pulling it failed (`docker pull`, at most {PULL_WAIT:?})\n{}",
        ran.describe(&[])
    );
}

/// The images a Compose text of the harness's own names (its `image:`
/// lines).
pub fn images_named_in(compose_text: &str) -> Vec<String> {
    compose_text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("image:"))
        .map(|i| i.trim().trim_matches(['"', '\'']).to_owned())
        .filter(|i| !i.is_empty())
        .collect()
}

/// `docker image inspect <image>`, its one object.
pub fn inspect_image(image: &str) -> Value {
    let ran = docker_ok(&["image", "inspect", image], DOCKER_WAIT);
    first_of(&ran)
}

/// The first object of the JSON array a `docker inspect` printed.
fn first_of(ran: &Ran) -> Value {
    let all: Value = serde_json::from_str(&ran.stdout).unwrap_or_else(|e| {
        panic!(
            "expected a JSON array from `{}` ({e})\n{}",
            ran.command,
            ran.describe(&[])
        )
    });
    all.get(0).cloned().unwrap_or_else(|| {
        panic!(
            "expected one object from `{}`\n{}",
            ran.command,
            ran.describe(&[])
        )
    })
}

/// The address of the Docker bridge's gateway on this machine: where a
/// container reaches the host (`docker network inspect bridge`).
pub fn bridge_address() -> IpAddr {
    let ran = docker_ok(
        &[
            "network",
            "inspect",
            "bridge",
            "--format",
            "{{(index .IPAM.Config 0).Gateway}}",
        ],
        DOCKER_WAIT,
    );
    let text = ran.stdout.trim();
    let ip: IpAddr = text.parse().unwrap_or_else(|e| {
        panic!(
            "expected the bridge's gateway address from docker, got {text:?} ({e})\n{}",
            ran.describe(&[])
        )
    });
    assert!(
        !ip.is_unspecified() && !ip.is_loopback(),
        "the bridge's gateway address {ip} cannot be the one a container reaches the host at"
    );
    ip
}

/// `deploy/<name>` in the repository, failing the test when it is not
/// there.
pub fn deploy_file(name: &str) -> PathBuf {
    let path = crate::sdk::repository_root().join("deploy").join(name);
    assert!(
        path.is_file(),
        "deploy/{name} is not there ({}): the Compose files are the ticket's (CONTRACT.md, \"Installing: one image and a Compose file\")",
        path.display()
    );
    path
}

/// The text of `deploy/<name>`.
pub fn read_deploy_file(name: &str) -> String {
    let path = deploy_file(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The harness's own file beside the Compose files: `vibe` finds the
/// identity provider's name at the bridge's address. Nothing else of
/// `vibe`, `postgres` or `proxy` is changed by it.
pub fn harness_file(bridge: IpAddr) -> String {
    format!(
        "# Added by the install tests (blackbox/src/install.rs): what stands in\n# for the outside world, name resolution to the identity provider.\nservices:\n  vibe:\n    extra_hosts:\n      - \"{IDP_HOST}:{bridge}\"\n"
    )
}

/// Whether `said` (what `docker run` or `docker compose up` printed when it
/// failed) is Docker refusing to publish `port` because something holds it
/// ("failed to bind host port <ip>:<port>/tcp: address already in use", or
/// "Bind for <ip>:<port> failed: port is already allocated"). Nothing else
/// counts.
pub fn docker_refused_port(said: &str, port: u16) -> bool {
    said.lines().any(|line| {
        line.contains(&format!(":{port}/tcp: address already in use"))
            || line.contains(&format!(":{port} failed: port is already allocated"))
    })
}

/// A port of `127.0.0.1` that is free now.
pub fn free_port() -> u16 {
    crate::postgres::free_port().unwrap_or_else(|e| panic!("{e}"))
}

/// One installation started with Compose from the files of `deploy/`.
pub struct Install {
    pub project: String,
    dir: PathBuf,
    files: Vec<String>,
    values: Vec<(String, String)>,
    secrets: Vec<String>,
    own_containers: RefCell<Vec<String>>,
    own_tags: RefCell<Vec<String>>,
    /// The `.env` values that are ports the harness chose and nothing has
    /// published yet: the first [`Install::up`] that finds one taken
    /// chooses another ([`Install::choose_port`]).
    chosen_ports: Vec<String>,
}

impl Install {
    /// A folder holding copies of these files of `deploy/` (the first is
    /// `compose.yaml`), unchanged byte for byte, an empty `ca/` and
    /// `certs/`, and no `.env` yet. Fails at once when Docker is not there
    /// or a file is not.
    pub fn new(files: &[&str]) -> Self {
        Self::with_folders(files, &["ca", "certs"])
    }

    /// As [`Install::new`], with no `ca/` folder: `compose.yaml` alone, as
    /// downloaded, beside the `.env` and `certs/` and nothing else.
    pub fn alone(files: &[&str]) -> Self {
        Self::with_folders(files, &["certs"])
    }

    fn with_folders(files: &[&str], folders: &[&str]) -> Self {
        docker_present();
        let id = token();
        let dir = std::env::temp_dir().join(format!("vm-blackbox-install-{id}"));
        let mut made = vec![dir.clone()];
        made.extend(folders.iter().map(|f| dir.join(f)));
        for sub in made {
            fs::create_dir_all(&sub)
                .unwrap_or_else(|e| panic!("cannot make {}: {e}", sub.display()));
            set_mode(&sub, 0o755);
        }
        let mut install = Self {
            project: format!("vibe-install-{id}"),
            dir,
            files: Vec::new(),
            values: Vec::new(),
            secrets: Vec::new(),
            own_containers: RefCell::new(Vec::new()),
            own_tags: RefCell::new(Vec::new()),
            chosen_ports: Vec::new(),
        };
        for name in files {
            let from = deploy_file(name);
            let to = install.dir.join(name);
            let bytes =
                fs::read(&from).unwrap_or_else(|e| panic!("cannot read {}: {e}", from.display()));
            fs::write(&to, &bytes).unwrap_or_else(|e| panic!("cannot write {}: {e}", to.display()));
            set_mode(&to, 0o644);
            install.files.push((*name).to_owned());
        }
        install
    }

    /// For the harness's own tests: a folder as [`Install::new`] makes, with
    /// a `compose.yaml` of this text in place of `deploy/`'s.
    /// Every image the text names is made sure of first ([`public_image`]),
    /// so that a text with `pull_policy: never` does not depend on what the
    /// machine holds.
    pub fn with_compose_text(text: &str) -> Self {
        for image in images_named_in(text) {
            public_image(&image);
        }
        let mut install = Self::new(&[]);
        install.write_file(COMPOSE, text.as_bytes());
        install.files.push(COMPOSE.to_owned());
        install
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Adds the harness's own file ([`HARNESS_FILE`]) with this text, given
    /// last with `-f`.
    pub fn add_harness_file(&mut self, text: &str) {
        self.write_file(HARNESS_FILE, text.as_bytes());
        self.files.push(HARNESS_FILE.to_owned());
    }

    /// Sets a value of the `.env` (written by [`Install::write_env`]).
    pub fn set(&mut self, name: &str, value: &str) {
        self.values.retain(|(n, _)| n != name);
        self.values.push((name.to_owned(), value.to_owned()));
    }

    /// As [`Install::set`], a value never shown in a message.
    pub fn set_secret(&mut self, name: &str, value: &str) {
        self.set(name, value);
        self.hide(value);
    }

    /// Takes a value out of the `.env`.
    pub fn unset(&mut self, name: &str) {
        self.values.retain(|(n, _)| n != name);
    }

    /// The value of the `.env` of this name, if set.
    pub fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// Masks this text in every message from now on.
    pub fn hide(&mut self, secret: &str) {
        if !secret.is_empty() {
            self.secrets.push(secret.to_owned());
        }
    }

    /// The secrets to mask.
    pub fn secrets(&self) -> Vec<&str> {
        self.secrets.iter().map(String::as_str).collect()
    }

    /// Writes the `.env` with the values set, one `NAME='value'` a line.
    pub fn write_env(&self) {
        let text: String = self
            .values
            .iter()
            .map(|(n, v)| {
                assert!(
                    !v.contains('\'') && !v.contains('\n'),
                    "the harness writes no quote or line end in a value"
                );
                format!("{n}='{v}'\n")
            })
            .collect();
        self.write_file(".env", text.as_bytes());
    }

    /// Writes a file into the folder (mode `0644`: the containers read it
    /// as users of their own).
    pub fn write_file(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.dir.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .unwrap_or_else(|e| panic!("cannot make {}: {e}", parent.display()));
        }
        let mut file = fs::File::create(&path)
            .unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
        file.write_all(bytes)
            .unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
        set_mode(&path, 0o644);
        path
    }

    /// `certs/fullchain.pem` and `certs/privkey.pem`: the administrator's
    /// wildcard certificate.
    pub fn certificates(&self, identity: &Identity) {
        self.write_file("certs/fullchain.pem", identity.chain_pem().as_bytes());
        self.write_file("certs/privkey.pem", identity.key_pem().as_bytes());
    }

    /// `ca/<file>`: an authority the installation is to trust; returns the
    /// path the container sees it at (`/etc/vibe/ca/<file>`).
    pub fn ca(&self, file: &str, authority: &Authority) -> String {
        self.write_file(&format!("ca/{file}"), authority.pem().as_bytes());
        format!("/etc/vibe/ca/{file}")
    }

    /// The arguments of `docker compose` for this project: its name, its
    /// folder, and each file with `-f`.
    fn compose_args(&self, args: &[&str]) -> Vec<String> {
        let mut all = vec![
            "compose".to_owned(),
            "--project-name".to_owned(),
            self.project.clone(),
            "--project-directory".to_owned(),
            self.dir.display().to_string(),
        ];
        for file in &self.files {
            all.push("-f".to_owned());
            all.push(self.dir.join(file).display().to_string());
        }
        all.extend(args.iter().map(|a| (*a).to_owned()));
        all
    }

    /// Runs `docker compose <args>` for this project, in its folder.
    pub fn compose(&self, args: &[&str], timeout: Duration) -> Ran {
        self.compose_with(args, &[], timeout)
    }

    /// As [`Install::compose`], with these variables in the environment of
    /// `docker` (for `exec -e NAME`).
    pub fn compose_with(&self, args: &[&str], env: &[(&str, &str)], timeout: Duration) -> Ran {
        let all = self.compose_args(args);
        let refs: Vec<&str> = all.iter().map(String::as_str).collect();
        docker_with(Some(&self.dir), &refs, env, timeout)
    }

    /// As [`Install::compose`]; fails the test (with the containers' logs)
    /// unless it ends with `0`.
    pub fn compose_ok(&self, args: &[&str], timeout: Duration) -> Ran {
        let ran = self.compose(args, timeout);
        if !ran.ok() {
            self.fail(&format!(
                "expected `docker compose {}` to end with 0\n{}",
                args.join(" "),
                ran.describe(&self.secrets())
            ));
        }
        ran
    }

    /// Sets the `.env` value `name` to a port of 127.0.0.1 free now and
    /// returns it: a port the harness chose, which something else may take
    /// before Compose publishes it. The first [`Install::up`] that is
    /// refused it as taken chooses another.
    pub fn choose_port(&mut self, name: &str) -> u16 {
        let port = free_port();
        self.set(name, &port.to_string());
        if !self.chosen_ports.iter().any(|n| n == name) {
            self.chosen_ports.push(name.to_owned());
        }
        port
    }

    /// `docker compose up -d`; fails the test (with the containers' logs)
    /// unless it ends with `0`. When Docker refuses to publish a port the
    /// harness chose ([`Install::choose_port`]) because something else
    /// holds it, another is chosen, `.env` written again, and `up -d` run
    /// again, at most [`crate::dev::PORT_TRIES`] times, saying so on
    /// standard error. Once up, the ports are the installation's: a later
    /// `up` (after a `down`, an upgrade) is never moved to another port.
    pub fn up(&mut self) {
        let mut attempt = 1;
        loop {
            self.write_env();
            let ran = self.compose(&["up", "-d"], UP_WAIT);
            if ran.ok() {
                self.chosen_ports.clear();
                return;
            }
            let said = format!("{}\n{}", ran.stdout, ran.stderr);
            let refused = self.chosen_ports.iter().find_map(|name| {
                let port = self.value(name)?.parse::<u16>().ok()?;
                docker_refused_port(&said, port).then(|| (name.clone(), port))
            });
            match refused {
                Some((name, port)) if attempt < crate::dev::PORT_TRIES => {
                    crate::dev::say_port_taken(
                        &format!("`docker compose up` ({name})"),
                        port,
                        attempt,
                    );
                    attempt += 1;
                    self.set(&name, &free_port().to_string());
                }
                _ => self.fail(&format!(
                    "expected `docker compose up -d` to end with 0\n{}",
                    ran.describe(&self.secrets())
                )),
            }
        }
    }

    /// The containers' logs (`docker compose logs`), secrets masked.
    pub fn logs(&self) -> String {
        let ran = self.compose(&["logs", "--no-color"], DOCKER_WAIT);
        masked(&format!("{}{}", ran.stdout, ran.stderr), &self.secrets())
    }

    /// The containers' logs as they are, not masked: to look for a secret
    /// in. Never print it.
    pub fn raw_logs(&self) -> String {
        let ran = self.compose(&["logs", "--no-color"], DOCKER_WAIT);
        assert!(
            ran.ok(),
            "cannot read the containers' logs\n{}",
            ran.describe(&self.secrets())
        );
        format!("{}{}", ran.stdout, ran.stderr)
    }

    /// Fails the test with `message`, the project's containers and their
    /// logs.
    pub fn fail(&self, message: &str) -> ! {
        let ps = self.compose(&["ps", "--all"], DOCKER_WAIT);
        panic!(
            "{}\n\n== the installation {} (folder {}):\n{}\n== docker compose logs:\n{}",
            masked(message, &self.secrets()),
            self.project,
            self.dir.display(),
            ps.describe(&self.secrets()),
            self.logs()
        );
    }

    /// The id of the container of this service (running or not); fails
    /// when there is none.
    pub fn container(&self, service: &str) -> String {
        let ran = self.compose_ok(&["ps", "--all", "--quiet", service], DOCKER_WAIT);
        let id = ran
            .stdout
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
        if id.is_empty() {
            self.fail(&format!("expected a container of the service {service:?}"));
        }
        id
    }

    /// Every container of the project, with its service: `(service, id)`.
    pub fn containers(&self) -> Vec<(String, String)> {
        let ran = docker_ok(
            &[
                "ps",
                "--all",
                "--filter",
                &format!("label=com.docker.compose.project={}", self.project),
                "--format",
                "{{.Label \"com.docker.compose.service\"}} {{.ID}}",
            ],
            DOCKER_WAIT,
        );
        ran.stdout
            .lines()
            .filter_map(|l| l.split_once(' '))
            .map(|(s, i)| (s.to_owned(), i.to_owned()))
            .collect()
    }

    /// The services Compose runs for this project, as `docker compose
    /// config --services` names them.
    pub fn services(&self) -> Vec<String> {
        let ran = self.compose_ok(&["config", "--services"], DOCKER_WAIT);
        let mut services: Vec<String> = ran.stdout.lines().map(str::to_owned).collect();
        services.sort();
        services
    }

    /// `docker inspect <id>`, its one object.
    pub fn inspect(&self, id: &str) -> Value {
        let ran = docker(&["inspect", id], DOCKER_WAIT);
        if !ran.ok() {
            self.fail(&format!(
                "expected `docker inspect` to answer\n{}",
                ran.describe(&self.secrets())
            ));
        }
        first_of(&ran)
    }

    /// `docker compose exec -T <service> <args>`.
    pub fn exec(&self, service: &str, args: &[&str]) -> Ran {
        let mut all = vec!["exec", "-T", service];
        all.extend_from_slice(args);
        self.compose(&all, DOCKER_WAIT)
    }

    /// `psql` as the superuser inside the `postgres` container, in database
    /// `database`; fails unless it ends with `0`. Returns its output,
    /// unaligned and without headers, trimmed.
    pub fn psql(&self, database: &str, sql: &str) -> String {
        let ran = self.exec(
            "postgres",
            &[
                "psql",
                "-X",
                "-A",
                "-t",
                "-v",
                "ON_ERROR_STOP=1",
                "-U",
                "postgres",
                "-d",
                database,
                "-c",
                sql,
            ],
        );
        if !ran.ok() {
            self.fail(&format!(
                "expected psql as the superuser inside the postgres container to answer\n{}",
                ran.describe(&self.secrets())
            ));
        }
        ran.stdout.trim().to_owned()
    }

    /// The first network the container of this service is on.
    pub fn network_of(&self, service: &str) -> String {
        let inspected = self.inspect(&self.container(service));
        inspected["NetworkSettings"]["Networks"]
            .as_object()
            .and_then(|n| n.keys().next().cloned())
            .unwrap_or_else(|| self.fail(&format!("expected {service} to be on a network")))
    }

    /// Runs a container of the test's own (`docker run --rm --pull never
    /// --name <project>-own-<random> <options> <image> <command>`), removed
    /// when the test ends whatever happens, with these variables in
    /// `docker`'s environment (given to the container with `-e NAME`). The
    /// public `image` is pulled first when the machine does not have it
    /// ([`public_image`]).
    pub fn run_own(
        &self,
        options: &[&str],
        image: &str,
        command: &[&str],
        env: &[(&str, &str)],
        timeout: Duration,
    ) -> Ran {
        public_image(image);
        let name = format!("{}-own-{}", self.project, token());
        self.own_containers.borrow_mut().push(name.clone());
        let mut all = vec!["run", "--rm", "--pull", "never", "--name", name.as_str()];
        all.extend_from_slice(options);
        all.push(image);
        all.extend_from_slice(command);
        docker_with(None, &all, env, timeout)
    }

    /// Starts a container of the test's own in the background (`docker run
    /// -d --pull never --name <project>-own-<random> <options> <image>
    /// <command>`); returns its name. Removed when the test ends. The
    /// public `image` is pulled first when the machine does not have it
    /// ([`public_image`]).
    pub fn start_own(
        &self,
        options: &[&str],
        image: &str,
        command: &[&str],
        env: &[(&str, &str)],
    ) -> String {
        self.try_start_own(options, image, command, env)
            .unwrap_or_else(|ran| {
                self.fail(&format!(
                    "expected a container of the test's own to start\n{}",
                    ran.describe(&self.secrets())
                ))
            })
    }

    /// As [`Install::start_own`]: its name, or how `docker run` ended when
    /// it did not start (the container, made or not, is removed when the
    /// test ends all the same).
    pub fn try_start_own(
        &self,
        options: &[&str],
        image: &str,
        command: &[&str],
        env: &[(&str, &str)],
    ) -> Result<String, Ran> {
        public_image(image);
        let name = format!("{}-own-{}", self.project, token());
        self.own_containers.borrow_mut().push(name.clone());
        let mut all = vec!["run", "-d", "--pull", "never", "--name", name.as_str()];
        all.extend_from_slice(options);
        all.push(image);
        all.extend_from_slice(command);
        let ran = docker_with(None, &all, env, DOCKER_WAIT);
        if ran.ok() { Ok(name) } else { Err(ran) }
    }

    /// `psql` from a container of the test's own on the network of the
    /// `postgres` service, over the network to `postgres:5432` as `user`
    /// with `password`, in `database`: `select current_user`. The run as
    /// it ended.
    pub fn psql_over_network(&self, user: &str, password: &str, database: &str) -> Ran {
        let network = self.network_of("postgres");
        self.run_own(
            &[
                "--network",
                &network,
                "-e",
                "PGPASSWORD",
                "-e",
                "PGCONNECT_TIMEOUT=10",
            ],
            POSTGRES_18_IMAGE,
            &[
                "psql",
                "-X",
                "-A",
                "-t",
                "-h",
                "postgres",
                "-U",
                user,
                "-d",
                database,
                "-c",
                "select current_user",
            ],
            &[("PGPASSWORD", password)],
            DOCKER_WAIT,
        )
    }

    /// Starts a Postgres of the test's own from `image` (`postgres:18`,
    /// `postgres:17`; pulled when the machine does not have it), published on `bridge` (the
    /// Docker bridge's address) at a free port, with a role `vibe` that may
    /// log in and create databases and roles, made by a script of the
    /// image's first start (so that no password is on a command line).
    /// Returns the container's name and a `VIBE_DATABASE_URL` naming it by
    /// that address, the platform database `vibe`. Removed when the test
    /// ends.
    pub fn administrators_postgres(&mut self, bridge: IpAddr, image: &str) -> (String, String) {
        let superuser_password = token();
        let password = token();
        self.hide(&superuser_password);
        self.hide(&password);
        let script = self.write_file(
            &format!("own-postgres-{}/init.sql", token()),
            format!("create role vibe login createdb createrole password '{password}';\n")
                .as_bytes(),
        );
        if let Some(folder) = script.parent() {
            set_mode(folder, 0o755);
        }
        // A port the harness chooses: when Docker cannot publish it because
        // something else took it since, another, at most PORT_TRIES times.
        let mut attempt = 1;
        let (port, name) = loop {
            let port = free_port();
            let started = self.try_start_own(
                &[
                    "-e",
                    "POSTGRES_PASSWORD",
                    "--publish",
                    &format!("{bridge}:{port}:5432"),
                    "--volume",
                    &format!(
                        "{}:/docker-entrypoint-initdb.d/init.sql:ro",
                        script.display()
                    ),
                ],
                image,
                &[],
                &[("POSTGRES_PASSWORD", superuser_password.as_str())],
            );
            match started {
                Ok(name) => break (port, name),
                Err(ran)
                    if attempt < crate::dev::PORT_TRIES
                        && docker_refused_port(
                            &format!("{}\n{}", ran.stdout, ran.stderr),
                            port,
                        ) =>
                {
                    crate::dev::say_port_taken(
                        "`docker run` (the test's own Postgres)",
                        port,
                        attempt,
                    );
                    attempt += 1;
                }
                Err(ran) => self.fail(&format!(
                    "expected a container of the test's own to start\n{}",
                    ran.describe(&self.secrets())
                )),
            }
        };
        // Over TCP only once the first start's script has run and the
        // server has started for good.
        let deadline = Instant::now() + POSTGRES_READY;
        loop {
            let ran = docker(
                &[
                    "exec",
                    &name,
                    "psql",
                    "-X",
                    "-A",
                    "-t",
                    "-U",
                    "postgres",
                    "-h",
                    "127.0.0.1",
                    "-c",
                    "select count(*) from pg_roles where rolname = 'vibe'",
                ],
                DOCKER_WAIT,
            );
            if ran.ok() && ran.stdout.trim() == "1" {
                break;
            }
            if Instant::now() >= deadline {
                self.fail(&format!(
                    "expected the test's own Postgres ({image}) to take connections, with the role vibe, within {POSTGRES_READY:?}\n{}",
                    ran.describe(&self.secrets())
                ));
            }
            thread::sleep(Duration::from_millis(500));
        }
        (
            name,
            format!("postgres://vibe:{password}@{bridge}:{port}/vibe"),
        )
    }

    /// `psql` as the superuser in a Postgres of the test's own (by its
    /// container's name); fails unless it answers. Its output, trimmed.
    pub fn own_psql(&self, name: &str, sql: &str) -> String {
        let ran = docker(
            &[
                "exec", name, "psql", "-X", "-A", "-t", "-U", "postgres", "-c", sql,
            ],
            DOCKER_WAIT,
        );
        if !ran.ok() {
            self.fail(&format!(
                "expected psql in the test's own Postgres to answer\n{}",
                ran.describe(&self.secrets())
            ));
        }
        ran.stdout.trim().to_owned()
    }

    /// Follows the log of the container `id` (`docker logs --follow`, from
    /// its beginning) and kills the container (`docker kill`) from the
    /// reading thread the moment a line holding `needle` is read, at most
    /// `timeout` after the call. Returns the line, or why not.
    pub fn kill_when_logged(
        &self,
        id: &str,
        needle: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        use std::io::BufRead;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};
        let mut following = Command::new("docker")
            .args(["logs", "--follow", id])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot follow the log: {e}"))?;
        let killed = Arc::new(AtomicBool::new(false));
        let found: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let readers: Vec<thread::JoinHandle<()>> = [
            following
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
            following
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        ]
        .into_iter()
        .flatten()
        .map(|stream| {
            let killed = Arc::clone(&killed);
            let found = Arc::clone(&found);
            let id = id.to_owned();
            let needle = needle.to_owned();
            thread::spawn(move || {
                for line in std::io::BufReader::new(stream).lines() {
                    let Ok(line) = line else { break };
                    if line.contains(&needle) && !killed.swap(true, Ordering::SeqCst) {
                        drop(
                            Command::new("docker")
                                .args(["kill", &id])
                                .stdout(std::process::Stdio::null())
                                .stderr(std::process::Stdio::null())
                                .status(),
                        );
                        if let Ok(mut kept) = found.lock() {
                            *kept = Some(line);
                        }
                    }
                }
            })
        })
        .collect();
        let deadline = Instant::now() + timeout;
        while !killed.load(Ordering::SeqCst) && Instant::now() < deadline {
            if following.try_wait().ok().flatten().is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        // Give the reading thread the time of its `docker kill`.
        let settle = Instant::now() + DOCKER_WAIT;
        while killed.load(Ordering::SeqCst)
            && found.lock().map(|f| f.is_none()).unwrap_or(false)
            && Instant::now() < settle
        {
            thread::sleep(Duration::from_millis(5));
        }
        drop(following.kill());
        drop(following.wait());
        for reader in readers {
            drop(reader.join());
        }
        let line = found.lock().ok().and_then(|f| f.clone());
        line.ok_or_else(|| {
            format!("no line holding {needle:?} in the log of {id} within {timeout:?}")
        })
    }

    /// The health of the container `id` as Docker says it (`healthy`,
    /// `starting`, `unhealthy`), or `none` when it has no health check.
    pub fn health(&self, id: &str) -> String {
        let inspected = self.inspect(id);
        inspected["State"]["Health"]["Status"]
            .as_str()
            .unwrap_or("none")
            .to_owned()
    }

    /// A second tag of `image`, of the test's own (removed when the test
    /// ends): the same image under another name, as an upgrade names it.
    pub fn second_tag(&self, image: &str) -> String {
        let tag = format!("{}:upgrade", self.project);
        docker_ok(&["tag", image, &tag], DOCKER_WAIT);
        self.own_tags.borrow_mut().push(tag.clone());
        tag
    }

    /// Takes everything of the test's down. Called by `Drop`; safe to call
    /// twice.
    fn take_down(&self) {
        drop(self.compose(
            &["down", "--volumes", "--remove-orphans", "--timeout", "10"],
            DOCKER_WAIT,
        ));
        let label = format!("label=com.docker.compose.project={}", self.project);
        let left = docker(&["ps", "--all", "--quiet", "--filter", &label], DOCKER_WAIT);
        for id in left.stdout.split_whitespace() {
            drop(docker(&["rm", "--force", "--volumes", id], DOCKER_WAIT));
        }
        for name in self.own_containers.borrow().iter() {
            let found = docker(
                &[
                    "ps",
                    "--all",
                    "--quiet",
                    "--filter",
                    &format!("name=^{name}$"),
                ],
                DOCKER_WAIT,
            );
            if !found.stdout.trim().is_empty() {
                drop(docker(&["rm", "--force", "--volumes", name], DOCKER_WAIT));
            }
        }
        let networks = docker(
            &["network", "ls", "--quiet", "--filter", &label],
            DOCKER_WAIT,
        );
        for id in networks.stdout.split_whitespace() {
            drop(docker(&["network", "rm", id], DOCKER_WAIT));
        }
        let volumes = docker(
            &["volume", "ls", "--quiet", "--filter", &label],
            DOCKER_WAIT,
        );
        for id in volumes.stdout.split_whitespace() {
            drop(docker(&["volume", "rm", "--force", id], DOCKER_WAIT));
        }
        for tag in self.own_tags.borrow().iter() {
            drop(docker(&["image", "rm", tag], DOCKER_WAIT));
        }
    }

    /// What of this project is still on the machine: containers, networks
    /// and volumes with its label, containers and tags of the test's own.
    pub fn left_behind(&self) -> Vec<String> {
        let label = format!("label=com.docker.compose.project={}", self.project);
        let mut left = Vec::new();
        for (kind, args) in [
            (
                "container",
                ["ps", "--all", "--quiet", "--filter", label.as_str()],
            ),
            (
                "network",
                ["network", "ls", "--quiet", "--filter", label.as_str()],
            ),
            (
                "volume",
                ["volume", "ls", "--quiet", "--filter", label.as_str()],
            ),
        ] {
            let ran = docker_ok(&args, DOCKER_WAIT);
            left.extend(
                ran.stdout
                    .split_whitespace()
                    .map(|id| format!("{kind} {id}")),
            );
        }
        for name in self.own_containers.borrow().iter() {
            let ran = docker_ok(
                &[
                    "ps",
                    "--all",
                    "--quiet",
                    "--filter",
                    &format!("name=^{name}$"),
                ],
                DOCKER_WAIT,
            );
            if !ran.stdout.trim().is_empty() {
                left.push(format!("container {name}"));
            }
        }
        for tag in self.own_tags.borrow().iter() {
            if docker(&["image", "inspect", tag], DOCKER_WAIT).ok() {
                left.push(format!("tag {tag}"));
            }
        }
        left
    }

    /// Takes everything down now (as dropping does), and fails unless
    /// nothing is left.
    pub fn take_down_now(&self) {
        self.take_down();
        let left = self.left_behind();
        assert!(
            left.is_empty(),
            "expected nothing of {} left on the machine, found {left:?}",
            self.project
        );
    }
}

impl Drop for Install {
    fn drop(&mut self) {
        if thread::panicking() {
            eprintln!(
                "== the installation {} at the failure (folder {}):\n{}",
                self.project,
                self.dir.display(),
                self.logs()
            );
        }
        self.take_down();
        drop(fs::remove_dir_all(&self.dir));
    }
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .unwrap_or_else(|e| panic!("cannot set the mode of {}: {e}", path.display()));
}

/// How this machine reaches an installation over HTTPS: every name of the
/// domain at `address` (`127.0.0.1:<port>`), TLS asking for the name,
/// trusting the authority of the wildcard certificate; the public port
/// `port` in every address.
#[derive(Clone, Debug)]
pub struct Reach {
    pub address: SocketAddr,
    pub port: u16,
    pub authority: CertificateDer<'static>,
}

impl Reach {
    /// The installation at `127.0.0.1:<port>`, the public port the same.
    pub fn at(port: u16, authority: &Authority) -> Self {
        Self {
            address: SocketAddr::from(([127, 0, 0, 1], port)),
            port,
            authority: authority.der(),
        }
    }

    /// `<name>.<domain>:<port>`, as `Host` names it.
    pub fn host(&self, name: &str) -> String {
        format!("{name}.{DOMAIN}:{}", self.port)
    }

    /// `https://<name>.<domain>:<port>`.
    pub fn origin(&self, name: &str) -> String {
        format!("https://{}", self.host(name))
    }

    /// `https://<name>.<domain>:<port><target>`.
    pub fn url(&self, name: &str, target: &str) -> Url {
        url(&format!("{}{target}", self.origin(name)))
    }

    /// A request to `https://api.<domain>:<port><target>`, over TLS.
    pub fn api(&self, target: &str) -> Request {
        Request::new(self.address, target, &self.host("api")).over_tls(
            &format!("api.{DOMAIN}"),
            std::slice::from_ref(&self.authority),
        )
    }

    /// `POST https://api.<domain>:<port><target>` over TLS with a JSON body
    /// of exactly `size` bytes (not JSON the API takes: `x` repeated),
    /// given [`BODY_WAIT`]; the caller says who sends it.
    pub fn post_of_size(&self, target: &str, size: usize) -> Request {
        self.api(target)
            .method("POST")
            .timeout(BODY_WAIT)
            .header("content-type", "application/json")
            .body(vec![b'x'; size])
    }

    /// The tool's credential kept on `machine`: of the runs of
    /// credential-like text in its credential files, the one with which
    /// the installation answers `GET /v1/me` through the front door.
    pub fn kept_credential(&self, machine: &Machine) -> String {
        let mut signing_in = Vec::new();
        for file in machine.credential_files() {
            let bytes =
                fs::read(&file).unwrap_or_else(|e| panic!("cannot read {}: {e}", file.display()));
            for candidate in crate::tool::credential_like_runs(&bytes) {
                let answer = self.api("/v1/me").bearer(&candidate).try_send();
                if answer.is_ok_and(|a| a.status == 200) {
                    signing_in.push(candidate);
                }
            }
        }
        assert_eq!(
            signing_in.len(),
            1,
            "expected one credential kept on the machine with which the installation answers GET /v1/me"
        );
        signing_in.remove(0)
    }

    /// `GET https://api.<domain>:<port>/v1/installation`.
    pub fn installation(&self) -> Result<Response, String> {
        self.api("/v1/installation").try_send()
    }

    /// Waits until `GET /v1/installation` answers `200` through the proxy,
    /// at most [`READY_WAIT`]; returns its JSON. Fails with the containers'
    /// logs when it does not.
    pub fn wait_ready(&self, install: &Install) -> Value {
        let deadline = Instant::now() + READY_WAIT;
        loop {
            let last = match self.installation() {
                Ok(answer) if answer.status == 200 => return answer.json(),
                Ok(answer) => answer.describe(),
                Err(problem) => problem,
            };
            if Instant::now() >= deadline {
                install.fail(&format!(
                    "expected https://api.{DOMAIN}:{}/v1/installation to answer 200 through the proxy within {READY_WAIT:?}; the last try: {last}",
                    self.port
                ));
            }
            thread::sleep(Duration::from_millis(500));
        }
    }

    /// A browser of a person: TLS to the installation at this address, the
    /// identity provider's name at the bridge's address, both authorities
    /// trusted, signed in at the provider as `sub`.
    pub fn browser_as(&self, provider: &OutsideProvider, sub: &str) -> Browser {
        let mut browser = Browser::behind_tls(self.address, self.authority.clone());
        provider.reached_by(&mut browser);
        browser.sign_in_at_provider(&provider.provider, sub);
        browser
    }

    /// The tool's options for this installation: `--domain <domain> --port
    /// <port> --resolve 127.0.0.1`.
    pub fn place(&self) -> Vec<String> {
        crate::tls::place_at(self.port)
    }

    /// Fails unless every redirect among these steps that goes to a name
    /// of the installation's domain names `https` and the public port,
    /// and unless the one to the identity provider gives the redirect
    /// address `https://auth.<domain>:<port>/callback`.
    pub fn assert_public_redirects(&self, steps: &[Step], context: &str) {
        let suffix = format!(".{DOMAIN}");
        for step in steps.iter().filter(|s| s.is_redirect()) {
            let raw = step.response.header("location").unwrap_or_default();
            let to = step.location();
            let ours = to.host_str().is_some_and(|h| h.ends_with(&suffix));
            if ours && (to.scheme() != "https" || to.port() != Some(self.port)) {
                panic!(
                    "{context}: expected every redirect within the installation to name https and the public port {}, but one named {:?}\n{}",
                    self.port,
                    crate::http::mask_values(raw),
                    describe_steps(steps)
                );
            }
            if let Some(redirect) = to
                .query_pairs()
                .find(|(n, _)| n == "redirect_uri")
                .map(|(_, v)| v.into_owned())
            {
                assert_eq!(
                    redirect,
                    format!("{}/callback", self.origin("auth")),
                    "{context}: expected the redirect address given to the identity provider to be the public one\n{}",
                    describe_steps(steps)
                );
            }
        }
    }

    /// Opens `https://<application>.<domain>:<port>/` in this browser and
    /// follows every redirect; returns the steps (the last is the answer).
    pub fn open_application(&self, browser: &mut Browser, application: &str) -> Vec<Step> {
        browser.navigate(&self.url(application, "/"))
    }

    /// Signs this browser out by the form of `<sign-in address>/sign-out`,
    /// as a person does; returns the page and the step of the `POST`.
    pub fn sign_out(&self, browser: &mut Browser) -> (Step, Step) {
        let page = browser.open(&self.url("auth", "/sign-out"));
        let form = forms(&page.response.text()).into_iter().find_map(|form| {
            let action = page.url.join(&form.action).ok()?;
            let posts = form.method == "post"
                && action.host_str() == Some(format!("auth.{DOMAIN}").as_str())
                && action.path() == "/sign-out";
            posts.then_some((form, action))
        });
        let Some((form, action)) = form.filter(|_| page.response.status == 200) else {
            panic!(
                "expected the sign-out page, `200` with a form that posts to /sign-out\n{}",
                masked(&page.response.describe(), &[])
            );
        };
        let body = form.submission(Decision::Confirm, None).unwrap_or_default();
        let request = browser.submit_form(&page.url, &page.response, &action, &body);
        let response = browser.send(&action, &request);
        (
            page,
            Step {
                url: action,
                response,
            },
        )
    }

    /// Signs the tool on `machine` in as this person through the browser,
    /// over HTTPS (`--domain --port --resolve`, `VIBE_CA_FILE` naming
    /// `ca_file`), as a Builder does: `vibe sign-in --json` with the
    /// harness's `BROWSER`; the person's browser opens what the tool
    /// opened, goes through the provider, confirms, and is sent to the
    /// tool, which must end with `0`. Returns what the tool printed and the
    /// browser's steps.
    pub fn sign_in_tool(
        &self,
        provider: &OutsideProvider,
        machine: &Machine,
        sub: &str,
        ca_file: &str,
    ) -> (Value, Vec<Step>) {
        let opener = BrowserCommand::on(machine);
        let (name, value) = opener.env();
        let mut args = vec!["sign-in".to_owned()];
        args.extend(self.place());
        args.push("--json".to_owned());
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let env = [(name, value.as_str()), ("VIBE_CA_FILE", ca_file)];
        let running = machine.spawn_into_files(&machine.cwd(), &refs, &env);
        let Some(opened) = opener.wait_opened(OPEN_TIMEOUT) else {
            let stderr = running.stderr_so_far();
            drop(running);
            panic!(
                "expected `vibe sign-in` to open the browser with BROWSER within {OPEN_TIMEOUT:?}; it wrote on standard error:\n{}",
                masked(&stderr, &[])
            );
        };
        let opened = url(&opened);
        assert!(
            opened
                .as_str()
                .starts_with(&format!("{}/tool?", self.origin("auth"))),
            "expected the tool to open the sign-in address {}/tool?..., it opened {}",
            self.origin("auth"),
            crate::http::mask_values(opened.as_str())
        );
        let mut browser = self.browser_as(provider, sub);
        let mut steps = browser.navigate(&opened);
        let page = steps.last().cloned().unwrap_or_else(|| panic!("no steps"));
        let form = forms(&page.response.text()).into_iter().find_map(|form| {
            let action = page.url.join(&form.action).ok()?;
            let posts = form.method == "post" && action.path() == "/tool";
            posts.then_some((form, action))
        });
        let Some((form, action)) = form.filter(|_| page.response.status == 200) else {
            panic!(
                "expected the way through /tool to end with a 200 page holding a form that posts to /tool, but it went:\n{}",
                crate::sessions::masked_steps(&steps)
            );
        };
        let body = form
            .submission(Decision::Confirm, None)
            .unwrap_or_else(|| panic!("expected a button to confirm on the page"));
        let request = browser.submit_form(&page.url, &page.response, &action, &body);
        let response = browser.send(&action, &request);
        let confirmed = Step {
            url: action,
            response,
        };
        steps.push(confirmed.clone());
        assert!(
            confirmed.is_redirect()
                && confirmed.location().host_str() == Some("127.0.0.1")
                && confirmed.location().scheme() == "http",
            "expected the confirmed sign-in to send the browser to the tool on 127.0.0.1\n{}",
            masked(&confirmed.response.describe(), &[])
        );
        let at_tool = browser.follow(&confirmed);
        let run: Run = crate::connection::hide(&running.wait(Duration::from_secs(30)), &[]);
        assert!(
            at_tool.response.status == 200 && run.code == Some(0),
            "expected the tool to answer the browser 200 and `vibe sign-in` to end with 0\nthe browser at the tool:\n{}\nthe tool:\n{}",
            masked(&at_tool.response.describe(), &[]),
            run.describe()
        );
        (run.json(), steps)
    }
}

/// The harness's identity provider as an installation in Docker reaches
/// it: behind a TLS front on the Docker bridge's address, its issuer
/// `https://idp.harness.test:<port>`, its certificate for that name under
/// [`OutsideProvider::authority`].
pub struct OutsideProvider {
    // The front first, so that it ends before the provider does.
    pub front: Front,
    pub provider: Provider,
    pub authority: Authority,
    /// Where its front listens.
    pub bridge: IpAddr,
    /// The name of its issuer's host: [`IDP_HOST`].
    pub host: String,
}

impl OutsideProvider {
    /// Starts the provider with these people, its front on the bridge's
    /// address.
    pub fn start(people: &[Person]) -> Self {
        Self::start_on(bridge_address(), IDP_HOST, people)
    }

    /// As [`OutsideProvider::start`], its front on `bridge` and its issuer
    /// on `host` (`127.0.0.1` and `localhost` for the harness's own test of
    /// a walk without Docker, where the program finds the name through the
    /// system).
    pub fn start_on(bridge: IpAddr, host: &str, people: &[Person]) -> Self {
        let authority = Authority::new("Harness identity provider authority");
        let bound = Bound::on(bridge);
        let issuer = format!("https://{host}:{}", bound.port());
        let provider = Provider::start_with_issuer(people, &issuer);
        let identity = authority.issue(&[host]);
        let front = bound.tls(&identity, provider.address());
        Self {
            front,
            provider,
            authority,
            bridge,
            host: host.to_owned(),
        }
    }

    /// The harness's own Compose file for this provider ([`harness_file`]).
    pub fn harness_file(&self) -> String {
        harness_file(self.bridge)
    }

    /// Makes `browser` reach the provider: its name at the bridge's
    /// address, its authority trusted.
    pub fn reached_by(&self, browser: &mut Browser) {
        browser.resolve(&self.host, self.bridge);
        browser.trust(self.authority.der());
    }

    /// Whether the provider was asked for its configuration (by the
    /// installation: no one else asks).
    pub fn configuration_asked(&self) -> bool {
        !self
            .provider
            .requests_to("/.well-known/openid-configuration")
            .is_empty()
    }
}

/// How the installation's front door is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrontDoor {
    /// The Compose file's proxy, published on `127.0.0.1:VIBE_HTTPS_PORT`
    /// with the wildcard certificate of `certs/`.
    TheFilesProxy,
    /// `compose.own-proxy.yaml`: the program's own port published on
    /// `127.0.0.1:VIBE_HTTP_PORT`, and a TLS front of the test's on
    /// `127.0.0.1:VIBE_HTTPS_PORT` before it, with the same certificate:
    /// another reverse proxy that ends TLS and passes `Host` on unchanged.
    AnotherProxy,
    /// `compose.own-tls.yaml`: `vibe` itself published on
    /// `127.0.0.1:VIBE_HTTPS_PORT`, ending TLS with the wildcard
    /// certificate of `certs/`.
    TheProgramItself,
}

/// An installation as an install test makes one: the image under test,
/// the files of `deploy/` given (`compose.yaml` first), a `.env` with the
/// three required values, a free `VIBE_HTTPS_PORT`, `VIBE_IMAGE` and
/// `VIBE_CA_FILE` naming the identity provider's authority in `ca/`; the
/// wildcard certificate for `*.<domain>` under a test authority in
/// `certs/`; the identity provider on the bridge's address with the
/// harness's one extra file; and, for the tool outside, that authority's
/// certificate in a file for `VIBE_CA_FILE`. Not started.
pub struct Installed {
    // The installation first, so that it is taken down before the rest
    // goes.
    pub install: Install,
    pub reach: Reach,
    pub idp: OutsideProvider,
    pub proxy_authority: Authority,
    /// The wildcard certificate of `certs/` (and its key, never printed).
    pub wildcard: Identity,
    pub image: String,
    /// The test's own front before the program, with
    /// [`FrontDoor::AnotherProxy`].
    pub front: Option<Front>,
    /// The folder of `tool_ca_file`, kept as long as the installation.
    _tool_ca: crate::tls::Files,
    tool_ca_file: String,
}

impl Installed {
    /// Prepares an installation from these files of `deploy/`, with these
    /// people at the identity provider, its front door as said.
    pub fn prepare(files: &[&str], people: &[Person], door: FrontDoor) -> Self {
        let image = image();
        let idp = OutsideProvider::start(people);
        let mut install = Install::new(files);
        install.add_harness_file(&idp.harness_file());
        let proxy_authority = Authority::new("Harness authority of the wildcard certificate");
        let wildcard = proxy_authority.issue(&[&format!("*.{DOMAIN}")]);
        install.certificates(&wildcard);
        let ca = install.ca("identity-provider.pem", &idp.authority);
        install.set("VIBE_DOMAIN", DOMAIN);
        install.set_secret(
            "VIBE_SECRET_KEY",
            crate::connection::SecretKey::fresh().expose(),
        );
        install.set_secret("POSTGRES_PASSWORD", &token());
        install.set("VIBE_IMAGE", &image);
        install.set("VIBE_CA_FILE", &ca);
        // The ports Compose publishes are chosen ([`Install::choose_port`]):
        // the first `up` chooses again if one was taken since, and
        // [`Installed::up`] follows it. The front's port is held by the
        // front itself from now on.
        let (port, front) = match door {
            FrontDoor::TheFilesProxy | FrontDoor::TheProgramItself => {
                (install.choose_port("VIBE_HTTPS_PORT"), None)
            }
            FrontDoor::AnotherProxy => {
                let http_port = install.choose_port("VIBE_HTTP_PORT");
                let bound = Bound::new();
                let port = bound.port();
                install.set("VIBE_HTTPS_PORT", &port.to_string());
                let front = bound.tls(&wildcard, SocketAddr::from(([127, 0, 0, 1], http_port)));
                (port, Some(front))
            }
        };
        install.write_env();
        let tool_ca = crate::tls::Files::new();
        let tool_ca_file = crate::tls::text_of(
            &tool_ca.authorities("wildcard-authority.pem", &[&proxy_authority]),
        );
        let reach = Reach::at(port, &proxy_authority);
        Self {
            install,
            reach,
            idp,
            proxy_authority,
            wildcard,
            image,
            front,
            _tool_ca: tool_ca,
            tool_ca_file,
        }
    }

    /// The file the tool outside names in `VIBE_CA_FILE`: the authority of
    /// the wildcard certificate.
    pub fn tool_ca_file(&self) -> &str {
        &self.tool_ca_file
    }

    /// `docker compose up -d` ([`Install::up`]); when it had to choose
    /// another port for one taken since [`Installed::prepare`], the front
    /// door follows: [`Installed::reach`] for the Compose file's proxy, the
    /// test's front's target for another proxy.
    pub fn up(&mut self) {
        self.install.up();
        if let Some(front) = &self.front {
            if let Some(http) = self
                .install
                .value("VIBE_HTTP_PORT")
                .and_then(|p| p.parse::<u16>().ok())
                && front.target().port() != http
            {
                front.retarget(SocketAddr::from(([127, 0, 0, 1], http)));
            }
        } else if let Some(https) = self
            .install
            .value("VIBE_HTTPS_PORT")
            .and_then(|p| p.parse::<u16>().ok())
            && https != self.reach.port
        {
            self.reach = Reach::at(https, &self.proxy_authority);
        }
    }

    /// [`Installed::up`], then waits until `GET /v1/installation` answers
    /// through the front door; returns its JSON.
    pub fn up_and_ready(&mut self) -> Value {
        self.up();
        self.reach.wait_ready(&self.install)
    }

    /// `docker compose exec vibe vibe setup`: fails unless it ends with
    /// `0`, prints the setup credential alone on standard output, and says
    /// on standard error to use it with `vibe admin connect-oidc`. Returns
    /// the credential, masked from now on.
    pub fn setup_credential(&mut self) -> crate::connection::SetupCredential {
        let ran = self.install.exec("vibe", &["vibe", "setup"]);
        let credential = ran.stdout.trim().to_owned();
        self.install.hide(&credential);
        let alone = !credential.contains(char::is_whitespace)
            && credential.starts_with(crate::connection::SETUP_PREFIX);
        if !ran.ok() || !alone || !ran.stderr.contains("vibe admin connect-oidc") {
            self.install.fail(&format!(
                "expected `docker compose exec vibe vibe setup` to end with 0, print the setup credential alone on standard output, and name `vibe admin connect-oidc` on standard error\n{}",
                ran.describe(&self.install.secrets())
            ));
        }
        crate::connection::SetupCredential::of(&credential)
    }

    /// Runs `vibe <args>` on `machine` with `VIBE_CA_FILE` naming the
    /// wildcard certificate's authority and these variables besides;
    /// fails (with the containers' logs) unless it ends with `0`, and
    /// returns what it printed (run with `--json`). Each of the
    /// installation's secrets and of `secrets` is masked.
    pub fn tool_ok(
        &mut self,
        machine: &Machine,
        args: &[&str],
        env: &[(&str, &str)],
        secrets: &[&str],
    ) -> Value {
        for secret in secrets {
            self.install.hide(secret);
        }
        let mut all: Vec<(&str, &str)> = vec![("VIBE_CA_FILE", self.tool_ca_file.as_str())];
        all.extend_from_slice(env);
        let ran = machine.run_with_env(args, &all);
        let shown = crate::connection::hide(&ran, &self.install.secrets());
        if ran.code != Some(0) {
            self.install.fail(&format!(
                "expected `vibe {}` to end with 0\n{}",
                args.join(" "),
                shown.describe()
            ));
        }
        shown.json()
    }

    /// Connects OIDC as the install document says: `vibe admin
    /// connect-oidc` with the setup credential, from the tool outside, over
    /// HTTPS (`--domain --port --resolve`), to the identity provider as
    /// the installation reaches it. Returns what it printed.
    pub fn connect_oidc(
        &mut self,
        machine: &Machine,
        setup: &crate::connection::SetupCredential,
    ) -> Value {
        let connection = crate::connection::Connection::to(&self.idp.provider);
        let mut args = vec!["admin".to_owned(), "connect-oidc".to_owned()];
        args.extend(self.reach.place());
        args.extend(connection.tool_args());
        args.push("--json".to_owned());
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let secret = connection.secret().expose().to_owned();
        self.tool_ok(
            machine,
            &refs,
            &[
                ("VIBE_SETUP_CREDENTIAL", setup.expose()),
                ("VIBE_OIDC_CLIENT_SECRET", secret.as_str()),
            ],
            &[setup.expose(), secret.as_str()],
        )
    }
}

/// Every name of the image's own: what an image reference's tag is, if it
/// has one (`registry/name:tag` → `tag`), or `@` and its digest.
pub fn tag_of(reference: &str) -> Option<String> {
    if let Some((_, digest)) = reference.split_once('@') {
        return Some(format!("@{digest}"));
    }
    let last = reference.rsplit('/').next().unwrap_or(reference);
    last.split_once(':').map(|(_, tag)| tag.to_owned())
}

/// The limit of open files the Compose file gives the `vibe` service
/// (CONTRACT.md, "Installing: one image and a Compose file": "The open
/// files"), soft and hard.
pub const VIBE_OPEN_FILES: u64 = 65536;

/// Fails unless the running `vibe` container (its `docker inspect`,
/// `inspected`) has `nofile` [`VIBE_OPEN_FILES`] soft and hard in
/// `HostConfig.Ulimits`, and its program logged `open-files-limit` with
/// that soft limit and nothing raised (`raised_from` null) and no
/// `open-files-limit-low`.
pub fn assert_open_files(install: &Install, inspected: &Value) {
    let ulimits = &inspected["HostConfig"]["Ulimits"];
    let nofile = ulimits
        .as_array()
        .and_then(|all| all.iter().find(|u| u["Name"] == "nofile"));
    assert!(
        nofile.is_some_and(|u| u["Soft"].as_u64() == Some(VIBE_OPEN_FILES)
            && u["Hard"].as_u64() == Some(VIBE_OPEN_FILES)),
        "expected HostConfig.Ulimits of the vibe container to hold nofile {VIBE_OPEN_FILES} soft and hard, it is {ulimits}"
    );
    let logs = install.logs();
    let records: Vec<Value> = logs
        .lines()
        .filter_map(|l| l.find('{').and_then(|at| l.get(at..)))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect();
    let said = records.iter().find(|r| r["event"] == "open-files-limit");
    assert!(
        said.is_some_and(|r| r["soft"].as_u64() == Some(VIBE_OPEN_FILES)
            && r["hard"].as_u64() == Some(VIBE_OPEN_FILES)
            && r["raised_from"].is_null()),
        "expected the program in the vibe container to log `open-files-limit` with soft and hard {VIBE_OPEN_FILES} and `raised_from` null, it logged {said:?}"
    );
    assert!(
        !records.iter().any(|r| r["event"] == "open-files-limit-low"),
        "expected no `open-files-limit-low` from the program in the vibe container"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reference_has_a_tag_after_its_last_slash_only() {
        assert_eq!(tag_of("postgres:18").as_deref(), Some("18"));
        assert_eq!(tag_of("ghcr.io/o/vibe:0.1.0").as_deref(), Some("0.1.0"));
        assert_eq!(tag_of("localhost:5000/vibe"), None);
        assert_eq!(tag_of("caddy"), None);
        assert_eq!(tag_of("caddy@sha256:abc").as_deref(), Some("@sha256:abc"));
    }

    #[test]
    fn the_harness_file_adds_the_providers_name_to_vibe_only() {
        let text = harness_file(IpAddr::from([172, 17, 0, 1]));
        assert!(
            text.contains("  vibe:\n    extra_hosts:\n      - \"idp.harness.test:172.17.0.1\"\n")
        );
        assert!(!text.contains("postgres:") && !text.contains("proxy:"));
    }
}
