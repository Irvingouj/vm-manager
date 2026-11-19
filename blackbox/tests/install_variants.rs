//! The two other Compose files (CONTRACT.md, "Installing: one image and a
//! Compose file: The Compose files" and "What the install tests show";
//! ticket "Install: one image and a Compose file, hardened"):
//!
//! - with `compose.own-postgres.yaml` and `VIBE_DATABASE_URL` naming a
//!   Postgres 18 outside the files (a container of the test's own,
//!   published on the Docker bridge's address, with a role that may create
//!   databases and roles), the installation comes up and takes a sign-in's
//!   first step; naming a Postgres 17, it is refused as "One Postgres
//!   version" says, the container's log says so, and nothing is written;
//! - with `compose.own-proxy.yaml`, behind another reverse proxy that ends
//!   TLS (a front of the test's before the published plain port), the
//!   installation answers and its redirects name the public address.
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

use blackbox::DOMAIN;
use blackbox::browser::describe_steps;
use blackbox::install::{COMPOSE, DOCKER_WAIT, FrontDoor, Installed, OWN_POSTGRES, OWN_PROXY};
use blackbox::sessions::{ADA, ada};
use blackbox::tool::Machine;
use serde_json::json;

/// OIDC connected (setup in the container, then the tool outside), then a
/// person's browser opens an Application's address: `302` to the sign-in
/// address, then `302` to the identity provider, every address public.
fn takes_a_sign_ins_first_step(at: &mut Installed) {
    let setup = at.setup_credential();
    let machine = Machine::new();
    let connected = at.connect_oidc(&machine, &setup);
    assert_eq!(connected["connected_by"], "setup-credential", "{connected}");
    let mut browser = at.reach.browser_as(&at.idp, ADA);
    let first = browser.open(&at.reach.url("any-application", "/"));
    let to_sign_in = first.is_redirect()
        && first.location().host_str() == Some(format!("auth.{DOMAIN}").as_str())
        && first.location().path() == "/sign-in";
    if !to_sign_in {
        at.install.fail(&format!(
            "expected a page of an Application to be sent to the sign-in address\n{}",
            first.response.describe()
        ));
    }
    let second = browser.follow(&first);
    let to_provider = second.is_redirect()
        && second
            .location()
            .as_str()
            .starts_with(at.idp.provider.issuer());
    if !to_provider {
        at.install.fail(&format!(
            "expected the sign-in address to send the browser to the identity provider\n{}",
            describe_steps(&[first, second])
        ));
    }
    at.reach
        .assert_public_redirects(&[first, second], "a sign-in's first step");
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn with_the_administrators_postgres_18_the_installation_comes_up_and_takes_a_sign_ins_first_step() {
    let mut at = Installed::prepare(&[COMPOSE, OWN_POSTGRES], &[ada()], FrontDoor::TheFilesProxy);
    let (name, url) = at
        .install
        .administrators_postgres(at.idp.bridge, "postgres:18");
    at.install.set_secret("VIBE_DATABASE_URL", &url);
    let installation = at.up_and_ready();
    assert_eq!(installation["sign_in"], "none", "{installation}");
    let services: Vec<String> = at
        .install
        .containers()
        .into_iter()
        .map(|(s, _)| s)
        .collect();
    assert!(
        !services.iter().any(|s| s == "postgres"),
        "expected no postgres service with the administrator's Postgres, got {services:?}"
    );
    assert_eq!(
        at.install.own_psql(
            &name,
            "select pg_get_userbyid(datdba) from pg_database where datname = 'vibe'"
        ),
        "vibe",
        "expected the platform database made in the administrator's Postgres by the role given"
    );
    takes_a_sign_ins_first_step(&mut at);
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn with_the_administrators_postgres_17_vibe_is_refused_and_its_log_says_so() {
    let mut at = Installed::prepare(&[COMPOSE, OWN_POSTGRES], &[ada()], FrontDoor::TheFilesProxy);
    let (name, url) = at
        .install
        .administrators_postgres(at.idp.bridge, "postgres:17");
    at.install.set_secret("VIBE_DATABASE_URL", &url);
    at.install.up();
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let logs = at
            .install
            .compose(&["logs", "--no-color", "vibe"], DOCKER_WAIT);
        let refused = logs.stdout.lines().chain(logs.stderr.lines()).any(|l| {
            l.contains("\"event\":\"postgres-version-unsupported\"")
                && l.contains("\"found\":\"17.")
                && l.contains("\"needed\":\"18\"")
        });
        if refused {
            break;
        }
        if Instant::now() >= deadline {
            at.install.fail(
                "expected vibe's log to say `postgres-version-unsupported`, found 17.x, needed 18",
            );
        }
        thread::sleep(Duration::from_secs(1));
    }
    assert!(
        !matches!(at.reach.installation(), Ok(answer) if answer.status == 200),
        "expected the installation not to serve against a Postgres 17"
    );
    assert_eq!(
        at.install.own_psql(
            &name,
            "select count(*) from pg_database where datname = 'vibe'"
        ),
        "0",
        "expected nothing written to the Postgres 17: no platform database"
    );
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn behind_another_proxy_that_ends_tls_the_installation_answers_and_its_redirects_name_the_public_address()
 {
    let mut at = Installed::prepare(&[COMPOSE, OWN_PROXY], &[ada()], FrontDoor::AnotherProxy);
    let installation = at.up_and_ready();
    assert_eq!(
        installation,
        json!({ "domain": DOMAIN, "sign_in": "none", "sign_in_address": null }),
        "expected the installation to answer through the other proxy"
    );
    // Only the program's port is published, on loopback, plain HTTP.
    let http_port = at.install.value("VIBE_HTTP_PORT").unwrap().to_owned();
    let mut published = Vec::new();
    for (service, id) in at.install.containers() {
        let inspected = at.install.inspect(&id);
        if let Some(ports) = inspected["NetworkSettings"]["Ports"].as_object() {
            for bindings in ports.values() {
                for binding in bindings.as_array().cloned().unwrap_or_default() {
                    published.push((
                        service.clone(),
                        binding["HostIp"].as_str().unwrap_or_default().to_owned(),
                        binding["HostPort"].as_str().unwrap_or_default().to_owned(),
                    ));
                }
            }
        }
    }
    assert_eq!(
        published,
        vec![("vibe".to_owned(), "127.0.0.1".to_owned(), http_port.clone())],
        "expected only vibe's own port published, on 127.0.0.1:VIBE_HTTP_PORT"
    );
    let plain = blackbox::Request::new(
        std::net::SocketAddr::from(([127, 0, 0, 1], http_port.parse::<u16>().unwrap())),
        "/v1/installation",
        &at.reach.host("api"),
    )
    .send();
    assert_eq!(
        plain.status,
        200,
        "expected plain HTTP on VIBE_HTTP_PORT\n{}",
        plain.describe()
    );
    takes_a_sign_ins_first_step(&mut at);
    let front = at.front.as_ref().unwrap();
    assert!(
        front
            .hosts()
            .iter()
            .all(|h| h.ends_with(&format!(".{DOMAIN}:{}", at.reach.port))),
        "expected every request through the other proxy to name the public address\n{}",
        front.describe()
    );
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn compose_yaml_alone_in_a_folder_with_its_env_and_certificates_comes_up_and_answers() {
    use blackbox::install::{Install, Reach, free_port, image};
    let image = image();
    let mut install = Install::alone(&[COMPOSE]);
    let authority = blackbox::tls::Authority::new("Harness authority of the wildcard certificate");
    install.certificates(&authority.issue(&[&format!("*.{DOMAIN}")]));
    let port = free_port();
    install.set("VIBE_DOMAIN", DOMAIN);
    install.set_secret(
        "VIBE_SECRET_KEY",
        blackbox::connection::SecretKey::fresh().expose(),
    );
    install.set_secret("POSTGRES_PASSWORD", &blackbox::shared_database::token());
    install.set("VIBE_HTTPS_PORT", &port.to_string());
    install.set("VIBE_IMAGE", &image);
    install.write_env();
    // Nothing but compose.yaml, .env and certs/ with the two files.
    let mut held: Vec<String> = Vec::new();
    for entry in walk(install.dir()) {
        held.push(
            entry
                .strip_prefix(install.dir())
                .unwrap()
                .display()
                .to_string(),
        );
    }
    held.sort();
    assert_eq!(
        held,
        [
            ".env",
            "certs",
            "certs/fullchain.pem",
            "certs/privkey.pem",
            "compose.yaml"
        ],
        "the harness's folder holds more than the contract's"
    );
    install.up();
    let installation = Reach::at(port, &authority).wait_ready(&install);
    assert_eq!(
        installation,
        json!({ "domain": DOMAIN, "sign_in": "none", "sign_in_address": null })
    );
}

/// Every file and folder under `dir`.
fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(walk(&path));
        }
        found.push(path);
    }
    found
}
