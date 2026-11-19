//! The Compose files as Compose reads them (CONTRACT.md, "Installing: one
//! image and a Compose file: The Compose files"; ticket "Install: one image
//! and a Compose file, hardened"), without starting anything: `docker
//! compose config` on copies of the files of `deploy/`, unchanged, beside a
//! `.env` of the test's; and `deploy/.env.example` and `deploy/README.md`.
//!
//! These need Docker with Compose and the files of `deploy/`, not the
//! image; they are run by `scripts/install-gate.sh` with the others.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::DOMAIN;
use blackbox::connection::SecretKey;
use blackbox::install::{
    COMPOSE, DOCKER_WAIT, Install, OWN_POSTGRES, OWN_PROXY, UP_WAIT, free_port, image,
    read_deploy_file, tag_of,
};
use blackbox::shared_database::token;
use blackbox::tls::Authority;
use serde_json::Value;

/// The values of the contract's table.
const TABLE: [&str; 10] = [
    "VIBE_DOMAIN",
    "VIBE_SECRET_KEY",
    "POSTGRES_PASSWORD",
    "VIBE_BIND",
    "VIBE_HTTPS_PORT",
    "VIBE_IMAGE",
    "VIBE_DATABASE_URL",
    "VIBE_HTTP_BIND",
    "VIBE_HTTP_PORT",
    "VIBE_CA_FILE",
];

/// A database address of the administrator's own Postgres, for the file
/// that takes one.
const DATABASE_URL: &str = "postgres://vibe:harness-password@db.example.test:5432/vibe";

/// A folder with these files and a `.env` of the three required values
/// (and `VIBE_DATABASE_URL` when `compose.own-postgres.yaml` is among
/// them), nothing else set.
fn with_required(files: &[&str]) -> Install {
    let mut install = Install::new(files);
    install.set("VIBE_DOMAIN", DOMAIN);
    install.set_secret("VIBE_SECRET_KEY", SecretKey::fresh().expose());
    install.set_secret("POSTGRES_PASSWORD", &token());
    if files.contains(&OWN_POSTGRES) {
        install.set_secret("VIBE_DATABASE_URL", DATABASE_URL);
    }
    install
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

/// Fails unless `docker compose config` with the `.env` as set fails
/// naming `name`.
fn assert_refused_naming(install: &Install, name: &str) {
    install.write_env();
    let ran = install.compose(&["config"], DOCKER_WAIT);
    assert!(
        !ran.ok() && ran.stderr.contains(name),
        "expected `docker compose config` to refuse without {name}, naming it\n{}",
        ran.describe(&install.secrets())
    );
}

/// The services of a configuration, sorted.
fn services(config: &Value) -> Vec<String> {
    let mut names: Vec<String> = config["services"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    names.sort();
    names
}

/// `(host_ip, published, target)` of each port a service publishes.
fn published(config: &Value, service: &str) -> Vec<(String, String, String)> {
    config["services"][service]["ports"]
        .as_array()
        .map(|ports| {
            ports
                .iter()
                .map(|p| {
                    let text = |v: &Value| match v {
                        Value::String(s) => s.clone(),
                        Value::Null => String::new(),
                        other => other.to_string(),
                    };
                    (
                        text(&p["host_ip"]),
                        text(&p["published"]),
                        text(&p["target"]),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The service's entrypoint and command, as one list of words.
fn command_of(config: &Value, service: &str) -> Vec<String> {
    let words = |v: &Value| -> Vec<String> {
        v.as_array()
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

/// Fails unless `words` holds `option` followed by `value`.
fn assert_option(words: &[String], option: &str, value: &str) {
    let found = words.windows(2).any(|w| w[0] == option && w[1] == value);
    assert!(
        found,
        "expected `{option} {value}` in vibe's command {words:?}"
    );
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn compose_refuses_each_missing_required_value_naming_it() {
    for name in ["VIBE_DOMAIN", "VIBE_SECRET_KEY"] {
        let mut install = with_required(&[COMPOSE]);
        install.unset(name);
        assert_refused_naming(&install, name);
    }
    // With them, the file as downloaded is taken.
    configured(&with_required(&[COMPOSE]));
    // POSTGRES_PASSWORD is not one Compose asks for: the bundled Postgres
    // does (CONTRACT.md, "The Compose files"; see
    // without_postgres_password_the_bundled_postgres_does_not_start_and_vibe_is_never_started).
    // Not in the .env (and never in the environment the harness gives
    // docker), or an empty line of it: `config` takes the file.
    let mut install = with_required(&[COMPOSE]);
    install.unset("POSTGRES_PASSWORD");
    configured(&install);
    let mut install = with_required(&[COMPOSE]);
    install.set("POSTGRES_PASSWORD", "");
    configured(&install);
}

/// How long `docker compose up -d` may take to end when the bundled
/// Postgres does not start.
const REFUSED_UP_WAIT: std::time::Duration = std::time::Duration::from_secs(180);

/// `docker compose up -d` of `compose.yaml` with the two values Compose
/// asks for and `postgres_password` as the `.env`'s line of it (`None`: no
/// line at all), and what follows from the contract: `config` passes, `up`
/// ends other than `0` within three minutes, `vibe` was never started, and
/// the `postgres` service's log names `POSTGRES_PASSWORD`. Everything is
/// taken down with its volumes when `install` is dropped, whatever
/// happens.
fn assert_postgres_asks_for_its_password(postgres_password: Option<&str>) {
    let image = image();
    let mut install = with_required(&[COMPOSE]);
    install.unset("POSTGRES_PASSWORD");
    install.set("VIBE_IMAGE", &image);
    install.set("VIBE_HTTPS_PORT", &free_port().to_string());
    let authority = Authority::new("Harness authority of the wildcard certificate");
    install.certificates(&authority.issue(&[&format!("*.{DOMAIN}")]));
    install.write_env();
    if let Some(line) = postgres_password {
        let env = std::fs::read_to_string(install.dir().join(".env")).unwrap();
        install.write_file(".env", format!("{env}{line}\n").as_bytes());
    }
    let case = postgres_password.map_or_else(
        || "no POSTGRES_PASSWORD in .env".to_owned(),
        |line| format!("the line {line:?} in .env"),
    );

    let config = install.compose(&["config", "--quiet"], DOCKER_WAIT);
    assert!(
        config.ok(),
        "expected `docker compose config` to take compose.yaml with {case}: Compose does not ask for POSTGRES_PASSWORD\n{}",
        config.describe(&install.secrets())
    );

    let up = install.compose(&["up", "-d"], REFUSED_UP_WAIT);
    assert!(
        up.code.is_some_and(|c| c != 0),
        "expected `docker compose up -d` with {case} to end other than 0 within {REFUSED_UP_WAIT:?}\n{}",
        up.describe(&install.secrets())
    );

    for (service, id) in install.containers() {
        if service != "vibe" {
            continue;
        }
        let state = &install.inspect(&id)["State"];
        let status = state["Status"].as_str().unwrap_or_default();
        let started = state["StartedAt"].as_str().unwrap_or_default();
        assert!(
            status == "created" && started.starts_with("0001-01-01"),
            "expected the vibe container never started with {case}: its state is {status:?}, started at {started:?}"
        );
    }

    let logs = install.compose(&["logs", "--no-color", "postgres"], DOCKER_WAIT);
    assert!(
        logs.ok() && logs.stdout.contains("POSTGRES_PASSWORD"),
        "expected `docker compose logs postgres` (ending with 0) to name POSTGRES_PASSWORD with {case}: the bundled Postgres itself asks for it\n{}",
        logs.describe(&install.secrets())
    );
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn without_postgres_password_the_bundled_postgres_does_not_start_and_vibe_is_never_started() {
    assert_postgres_asks_for_its_password(None);
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn with_an_empty_postgres_password_the_bundled_postgres_does_not_start_and_vibe_is_never_started() {
    assert_postgres_asks_for_its_password(Some("POSTGRES_PASSWORD="));
}

/// The state of the `postgres` service's container: `(its id, State)`.
fn postgres_state(install: &Install) -> (String, Value) {
    let id = install
        .containers()
        .into_iter()
        .find(|(service, _)| service == "postgres")
        .map(|(_, id)| id)
        .unwrap_or_else(|| install.fail("expected a postgres container"));
    let state = install.inspect(&id)["State"].clone();
    (id, state)
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn postgres_password_taken_out_of_a_made_postgres_leaves_it_never_healthy_and_its_health_check_says_so()
 {
    use std::time::{Duration, Instant};
    let mut install = with_required(&[COMPOSE]);
    install.write_env();
    // The bundled Postgres made and healthy with the password.
    install.compose_ok(&["up", "-d", "--wait", "postgres"], UP_WAIT);
    let (before, state) = postgres_state(&install);
    assert_eq!(
        state["Health"]["Status"], "healthy",
        "expected the bundled Postgres healthy with POSTGRES_PASSWORD set: {state}"
    );

    // The value taken out of the .env; postgres started again alone.
    install.unset("POSTGRES_PASSWORD");
    install.write_env();
    install.compose_ok(&["up", "-d", "--no-deps", "postgres"], DOCKER_WAIT);

    // Within 30 s its health check's output names POSTGRES_PASSWORD; it is
    // never healthy meanwhile, nor over 10 s more.
    let named_by = Instant::now() + Duration::from_secs(30);
    let mut named_at: Option<Instant> = None;
    let mut last: Value;
    loop {
        let (id, state) = postgres_state(&install);
        let status = state["Health"]["Status"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if id != before || named_at.is_some() {
            assert_ne!(
                status, "healthy",
                "expected the Postgres started again without POSTGRES_PASSWORD never healthy: {state}"
            );
        }
        let named = state["Health"]["Log"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|entry| {
                entry["Output"]
                    .as_str()
                    .is_some_and(|o| o.contains("POSTGRES_PASSWORD"))
            });
        let now = Instant::now();
        if named && named_at.is_none() {
            named_at = Some(now);
        }
        last = state;
        match named_at {
            None if now >= named_by => install.fail(&format!(
                "expected the health check's output (docker inspect: State.Health.Log[].Output) of the Postgres started again without POSTGRES_PASSWORD to name it within 30 s; its state: {last}"
            )),
            Some(at) if now.duration_since(at) >= Duration::from_secs(10) => break,
            _ => std::thread::sleep(Duration::from_millis(500)),
        }
    }
    assert_ne!(
        last["Health"]["Status"], "healthy",
        "expected the Postgres never healthy: {last}"
    );
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn with_the_administrators_postgres_compose_refuses_a_missing_database_url_naming_it() {
    // POSTGRES_PASSWORD stays set, so that nothing else is missing; the
    // message is only to name VIBE_DATABASE_URL (it may name others too).
    let mut install = with_required(&[COMPOSE, OWN_POSTGRES]);
    install.unset("VIBE_DATABASE_URL");
    assert_refused_naming(&install, "VIBE_DATABASE_URL");
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn with_the_administrators_postgres_no_postgres_password_is_asked() {
    let mut install = with_required(&[COMPOSE, OWN_POSTGRES]);
    install.unset("POSTGRES_PASSWORD");
    configured(&install);
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn as_downloaded_only_the_proxy_is_published_on_loopback_443_and_vibe_serves_behind_it() {
    let install = with_required(&[COMPOSE]);
    let key = install.value("VIBE_SECRET_KEY").unwrap().to_owned();
    let config = configured(&install);
    assert_eq!(services(&config), ["postgres", "proxy", "vibe"]);
    let proxy = published(&config, "proxy");
    assert_eq!(
        proxy.len(),
        1,
        "expected the proxy to publish one port, got {proxy:?}"
    );
    assert_eq!(
        (proxy[0].0.as_str(), proxy[0].1.as_str()),
        ("127.0.0.1", "443"),
        "expected 127.0.0.1:443 without VIBE_BIND and VIBE_HTTPS_PORT, got {proxy:?}"
    );
    for service in ["vibe", "postgres"] {
        assert_eq!(
            published(&config, service),
            vec![],
            "expected {service} published nowhere"
        );
    }
    let command = command_of(&config, "vibe");
    assert!(
        command.iter().any(|w| w == "serve"),
        "expected `serve` in {command:?}"
    );
    assert_option(&command, "--domain", DOMAIN);
    assert_option(&command, "--listen", "0.0.0.0:8080");
    assert_option(&command, "--public-scheme", "https");
    assert_option(&command, "--public-port", "443");
    assert!(
        !command.iter().any(|w| w.contains("stand-in")),
        "expected never --stand-in-sign-in: {command:?}"
    );
    let environment = &config["services"]["vibe"]["environment"];
    for name in ["VIBE_SECRET_KEY", "VIBE_DATABASE_URL"] {
        assert!(
            environment[name].as_str().is_some_and(|v| !v.is_empty()),
            "expected {name} in vibe's environment"
        );
    }
    assert_eq!(environment["VIBE_SECRET_KEY"].as_str(), Some(key.as_str()));
    let password = install.value("POSTGRES_PASSWORD").unwrap();
    assert!(
        !command
            .iter()
            .any(|w| w.contains(&key) || w.contains(password)),
        "expected the key and the database's password in no command line"
    );
    // The bundled Postgres keeps its data in a named volume.
    let volumes = config["services"]["postgres"]["volumes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        volumes.iter().any(|v| v["type"] == "volume"),
        "expected the bundled Postgres's data in a named volume, got {volumes:?}"
    );
    // vibe has one mount: ca/ read-only at /etc/vibe/ca.
    let mounts = config["services"]["vibe"]["volumes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        mounts.len(),
        1,
        "expected one mount of vibe's, got {mounts:?}"
    );
    assert_eq!(mounts[0]["type"], "bind");
    assert_eq!(mounts[0]["target"], "/etc/vibe/ca");
    assert_eq!(mounts[0]["read_only"], true);
    let source = mounts[0]["source"].as_str().unwrap_or_default();
    assert_eq!(
        std::path::Path::new(source),
        install.dir().join("ca"),
        "expected the folder ca/ beside the Compose file mounted"
    );
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn vibe_bind_and_the_https_port_are_what_is_published() {
    let mut install = with_required(&[COMPOSE]);
    install.set("VIBE_BIND", "0.0.0.0");
    install.set("VIBE_HTTPS_PORT", "8443");
    let config = configured(&install);
    let proxy = published(&config, "proxy");
    assert_eq!(proxy.len(), 1, "{proxy:?}");
    assert_eq!(
        (proxy[0].0.as_str(), proxy[0].1.as_str()),
        ("0.0.0.0", "8443")
    );
    assert_option(&command_of(&config, "vibe"), "--public-port", "8443");
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn behind_the_administrators_proxy_there_is_no_proxy_and_vibe_is_published_on_loopback_8080() {
    let config = configured(&with_required(&[COMPOSE, OWN_PROXY]));
    assert_eq!(services(&config), ["postgres", "vibe"]);
    assert_eq!(
        published(&config, "vibe"),
        vec![("127.0.0.1".to_owned(), "8080".to_owned(), "8080".to_owned())]
    );
    assert_eq!(published(&config, "postgres"), vec![]);
    let mut install = with_required(&[COMPOSE, OWN_PROXY]);
    install.set("VIBE_HTTP_BIND", "127.0.0.2");
    install.set("VIBE_HTTP_PORT", "18080");
    install.set("VIBE_HTTPS_PORT", "8443");
    let config = configured(&install);
    assert_eq!(
        published(&config, "vibe"),
        vec![(
            "127.0.0.2".to_owned(),
            "18080".to_owned(),
            "8080".to_owned()
        )]
    );
    let command = command_of(&config, "vibe");
    assert_option(&command, "--public-scheme", "https");
    assert_option(&command, "--public-port", "8443");
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn with_the_administrators_postgres_there_is_no_postgres_and_no_volume() {
    let config = configured(&with_required(&[COMPOSE, OWN_POSTGRES]));
    assert_eq!(services(&config), ["proxy", "vibe"]);
    assert!(
        config["volumes"]
            .as_object()
            .is_none_or(serde_json::Map::is_empty),
        "expected no volume, got {}",
        config["volumes"]
    );
    assert_eq!(
        config["services"]["vibe"]["environment"]["VIBE_DATABASE_URL"],
        DATABASE_URL
    );
    // The two other files together.
    let both = configured(&with_required(&[COMPOSE, OWN_PROXY, OWN_POSTGRES]));
    assert_eq!(services(&both), ["vibe"]);
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn each_image_the_files_name_carries_a_version_and_none_is_latest() {
    for files in [
        vec![COMPOSE],
        vec![COMPOSE, OWN_PROXY],
        vec![COMPOSE, OWN_POSTGRES],
    ] {
        let install = with_required(&files);
        install.write_env();
        let ran = install.compose(&["config", "--images"], DOCKER_WAIT);
        assert!(ran.ok(), "{}", ran.describe(&install.secrets()));
        let images: Vec<&str> = ran
            .stdout
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect();
        assert!(!images.is_empty(), "expected images from {files:?}");
        for image in &images {
            let tag = tag_of(image);
            assert!(
                tag.as_deref()
                    .is_some_and(|t| !t.is_empty() && t != "latest"),
                "expected {image:?} (of {files:?}, VIBE_IMAGE not set) to be named with its version, never latest"
            );
        }
    }
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn the_env_example_lists_every_value_with_how_to_make_the_secrets() {
    let example = read_deploy_file(".env.example");
    for name in TABLE {
        assert!(
            example.contains(name),
            "expected deploy/.env.example to list {name}"
        );
    }
    for how in ["openssl rand -base64 32", "openssl rand -hex 24"] {
        assert!(
            example.contains(how),
            "expected deploy/.env.example to say how to make a secret: {how:?}"
        );
    }
}

/// `text` with every run of white space one space.
fn squashed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn the_readme_writes_out_each_command_of_the_install() {
    let readme = squashed(&read_deploy_file("README.md"));
    for command in [
        "openssl rand -base64 32",
        "openssl rand -hex 24",
        "docker compose up -d",
        "docker compose exec vibe vibe setup",
        "vibe admin connect-oidc",
        "/callback",
        "docker compose -f compose.yaml -f compose.own-proxy.yaml up -d",
        "docker compose -f compose.yaml -f compose.own-postgres.yaml up -d",
        "docker compose pull",
        "docker compose exec postgres psql -U postgres",
        "template1",
    ] {
        assert!(
            readme.contains(command),
            "expected deploy/README.md to write out {command:?}"
        );
    }
    assert!(
        readme.to_ascii_uppercase().contains("REVOKE CONNECT"),
        "expected deploy/README.md to say to revoke CONNECT on postgres and template1 from PUBLIC on a Postgres the administrator brings"
    );
}
