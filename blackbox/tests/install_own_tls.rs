//! Installed with `compose.own-tls.yaml`: `vibe` serves TLS itself, no
//! proxy (CONTRACT.md, "The program serves TLS itself: The Compose file",
//! its last point; ticket "The program serves TLS itself with a supplied
//! wildcard certificate").
//!
//! - With `compose.yaml`, `compose.own-tls.yaml` and the three required
//!   values, the installation comes up with no `proxy` container;
//!   `https://api.<domain>/v1/installation` is answered by `vibe` itself
//!   with the wildcard certificate of `certs/`; OIDC is connected, a person
//!   signs in, and the template's Application deployed with the tool
//!   answers her at `https://<application>.<domain>/`; `docker inspect`
//!   shows the same hardening and the two read-only mounts; the
//!   certificate files replaced in `certs/` are served to a new connection
//!   within 10 seconds with no container restarted.
//! - Without the key readable by the image's user, `vibe` does not start
//!   and its log names `VIBE_TLS_KEY_FILE` and says the file cannot be
//!   read.
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

use std::os::unix::fs::PermissionsExt;
use std::thread;
use std::time::{Duration, Instant};

use blackbox::DOMAIN;
use blackbox::browser::describe_steps;
use blackbox::deploy::{Deployed, commit_all, deploy_with_env, init_repository};
use blackbox::install::{COMPOSE, DOCKER_WAIT, FrontDoor, Installed, OWN_TLS, READY_WAIT};
use blackbox::own_tls::{Hello, TAKEN_WITHIN, fingerprint, served_fingerprint, shake};
use blackbox::project::new_project_in_repository;
use blackbox::sessions::{ADA, ada};
use blackbox::tool::Machine;
use serde_json::{Value, json};

/// The template's Application, as the person names it.
const APPLICATION: &str = "greeter";

/// The image's user, by number (the `Dockerfile`'s `USER`).
const IMAGE_USER: u32 = 65532;

fn prepared() -> Installed {
    Installed::prepare(&[COMPOSE, OWN_TLS], &[ada()], FrontDoor::TheProgramItself)
}

/// What `docker inspect` says of the running `vibe` container: the same
/// hardening as with the proxy, the two read-only mounts, and that `vibe`
/// alone is published, on `127.0.0.1:VIBE_HTTPS_PORT`.
fn assert_hardened_with_two_mounts_and_published_itself(at: &Installed) {
    let install = &at.install;
    let vibe = install.inspect(&install.container("vibe"));
    let host = &vibe["HostConfig"];
    let context = format!(
        "docker inspect of vibe: HostConfig {} Config.User {} Mounts {}",
        host, vibe["Config"]["User"], vibe["Mounts"]
    );
    assert_eq!(vibe["State"]["Running"], true, "{context}");
    assert_eq!(host["ReadonlyRootfs"], true, "read-only root: {context}");
    let dropped: Vec<String> = host["CapDrop"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|c| c.as_str().unwrap_or_default().to_ascii_uppercase())
        .collect();
    assert!(
        dropped.iter().any(|c| c == "ALL" || c == "CAP_ALL"),
        "every capability dropped: {context}"
    );
    assert!(
        host["CapAdd"].as_array().is_none_or(Vec::is_empty),
        "{context}"
    );
    assert_eq!(host["Privileged"], false, "{context}");
    assert!(
        host["SecurityOpt"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .any(|o| o
                .as_str()
                .unwrap_or_default()
                .replace('=', ":")
                .starts_with("no-new-privileges")),
        "no-new-privileges: {context}"
    );
    let user = vibe["Config"]["User"].as_str().unwrap_or_default();
    assert!(
        user.split(':')
            .next()
            .and_then(|u| u.parse::<u32>().ok())
            .is_some_and(|u| u != 0),
        "a user given as a number that is not 0: {context}"
    );
    assert!(
        host["Tmpfs"]
            .as_object()
            .is_none_or(serde_json::Map::is_empty),
        "no tmpfs: {context}"
    );
    let mut mounts: Vec<(String, bool, String)> = vibe["Mounts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            (
                m["Destination"].as_str().unwrap_or_default().to_owned(),
                m["RW"] == false,
                m["Type"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    mounts.sort();
    assert_eq!(
        mounts,
        vec![
            ("/etc/vibe/ca".to_owned(), true, "bind".to_owned()),
            ("/etc/vibe/certs".to_owned(), true, "bind".to_owned()),
        ],
        "expected two mounts, /etc/vibe/ca and /etc/vibe/certs, both read-only binds: {context}"
    );
    let mut published = Vec::new();
    for (service, id) in install.containers() {
        let inspected = install.inspect(&id);
        if let Some(ports) = inspected["NetworkSettings"]["Ports"].as_object() {
            for (port, bindings) in ports {
                for binding in bindings.as_array().cloned().unwrap_or_default() {
                    published.push((
                        service.clone(),
                        port.clone(),
                        binding["HostIp"].as_str().unwrap_or_default().to_owned(),
                        binding["HostPort"].as_str().unwrap_or_default().to_owned(),
                    ));
                }
            }
        }
    }
    assert_eq!(
        published
            .iter()
            .map(|(s, _, ip, port)| (s.as_str(), ip.as_str(), port.as_str()))
            .collect::<Vec<_>>(),
        vec![("vibe", "127.0.0.1", at.reach.port.to_string().as_str())],
        "expected vibe alone published, on 127.0.0.1:VIBE_HTTPS_PORT: {published:?}"
    );
}

/// When the `vibe` container was started and how often it was restarted.
fn started(at: &Installed) -> (Value, Value) {
    let vibe = at.install.inspect(&at.install.container("vibe"));
    (
        vibe["State"]["StartedAt"].clone(),
        vibe["RestartCount"].clone(),
    )
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn with_compose_own_tls_vibe_serves_https_itself_a_person_signs_in_and_a_replaced_certificate_is_taken()
 {
    let mut at = prepared();

    // It comes up, with no proxy, answered by vibe itself with the
    // wildcard certificate.
    let installation = at.up_and_ready();
    assert_eq!(
        installation,
        json!({ "domain": DOMAIN, "sign_in": "none", "sign_in_address": null }),
        "expected the installation to say no sign-in is connected"
    );
    let services: Vec<String> = at
        .install
        .containers()
        .into_iter()
        .map(|(s, _)| s)
        .collect();
    assert!(
        !services.iter().any(|s| s == "proxy"),
        "expected no proxy container: {services:?}"
    );
    let shaken = shake(
        at.reach.address,
        &Hello::trusting(&format!("api.{DOMAIN}"), &at.proxy_authority),
    )
    .unwrap_or_else(|e| {
        at.install.fail(&format!(
            "expected vibe to end TLS itself with the wildcard certificate: {e}"
        ))
    });
    assert_eq!(
        shaken.fingerprint(),
        fingerprint(at.wildcard.chain()[0].as_ref()),
        "expected the wildcard certificate of certs/"
    );
    assert_hardened_with_two_mounts_and_published_itself(&at);
    let logs = at.install.logs();
    assert!(
        logs.contains("tls-certificate-loaded"),
        "expected vibe's log to say the certificate was loaded"
    );

    // OIDC connected; the person signs the tool in and deploys the
    // template's Application; it answers her at its https address.
    let setup = at.setup_credential();
    let machine = Machine::new();
    let connected = at.connect_oidc(&machine, &setup);
    assert_eq!(connected["connected_by"], "setup-credential", "{connected}");
    let (signed_in, steps) = at
        .reach
        .sign_in_tool(&at.idp, &machine, ADA, at.tool_ca_file());
    assert_eq!(signed_in["user"], ADA, "{signed_in}");
    at.reach
        .assert_public_redirects(&steps, "the tool's sign-in through the browser");
    let project = new_project_in_repository(&machine, APPLICATION);
    init_repository(&project.dir);
    commit_all(&project.dir, "A new project");
    let run = deploy_with_env(
        &machine,
        &project.dir,
        &[],
        &[("VIBE_CA_FILE", at.tool_ca_file())],
    );
    if run.code != Some(0) {
        at.install.fail(&format!(
            "expected `vibe deploy` of the template's Application to end with 0\n{}",
            blackbox::connection::hide(&run, &at.install.secrets()).describe()
        ));
    }
    assert_eq!(Deployed::of(&run).application, APPLICATION);
    let mut browser = at.reach.browser_as(&at.idp, ADA);
    let steps = at.reach.open_application(&mut browser, APPLICATION);
    let last = steps.last().unwrap();
    if last.response.status != 200
        || last.url != at.reach.url(APPLICATION, "/")
        || !last.response.text().contains(ADA)
    {
        at.install.fail(&format!(
            "expected the person to land on {} with 200 and a page that has her id, but the way went:\n{}",
            at.reach.url(APPLICATION, "/"),
            describe_steps(&steps)
        ));
    }
    at.reach
        .assert_public_redirects(&steps, "the Application's first request");

    // The certificate files replaced in certs/ (by renaming): served to a
    // new connection within 10 seconds, no container restarted.
    let before = started(&at);
    let replacement = at.proxy_authority.issue(&[&format!("*.{DOMAIN}")]);
    let want = fingerprint(replacement.chain()[0].as_ref());
    for (name, text) in [
        ("privkey.pem", replacement.key_pem()),
        ("fullchain.pem", replacement.chain_pem()),
    ] {
        let partial = at
            .install
            .write_file(&format!("certs/.{name}.new"), text.as_bytes());
        std::fs::rename(&partial, at.install.dir().join("certs").join(name)).unwrap();
    }
    let changed = Instant::now();
    loop {
        if served_fingerprint(at.reach.address).as_deref() == Ok(want.as_str()) {
            break;
        }
        if changed.elapsed() >= TAKEN_WITHIN {
            at.install.fail(&format!(
                "expected the certificate replaced in certs/ served to a new connection within {TAKEN_WITHIN:?}"
            ));
        }
        thread::sleep(Duration::from_millis(200));
    }
    eprintln!(
        "the certificate replaced in certs/ was served {:?} after",
        changed.elapsed()
    );
    assert_eq!(
        started(&at),
        before,
        "expected no container restarted for the replacement"
    );
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn without_the_key_readable_by_the_images_user_vibe_does_not_start_and_its_log_says_why() {
    // The key is the test's user's, mode 600; the image's user is another.
    let at = prepared();
    let key = at.install.dir().join("certs").join("privkey.pem");
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let own_uid = std::fs::metadata(at.install.dir()).map(|m| {
        use std::os::unix::fs::MetadataExt;
        m.uid()
    });
    assert!(
        own_uid.is_ok_and(|u| u != IMAGE_USER && u != 0),
        "the harness's own check: the key's owner must be neither root nor the image's user {IMAGE_USER}"
    );
    at.install.write_env();
    // `up -d` may end with 0 (vibe is started, then ends) or not; either
    // way vibe must not serve and its log must say why.
    let up = at
        .install
        .compose(&["up", "-d"], blackbox::install::UP_WAIT);
    let deadline = Instant::now() + READY_WAIT;
    let logs = loop {
        let logs = at
            .install
            .compose(&["logs", "--no-color", "vibe"], DOCKER_WAIT);
        let text = format!("{}{}", logs.stdout, logs.stderr);
        if text.contains("VIBE_TLS_KEY_FILE") {
            break text;
        }
        if Instant::now() >= deadline {
            at.install.fail(&format!(
                "expected vibe's log to name VIBE_TLS_KEY_FILE within {READY_WAIT:?} (`up -d`: {})",
                up.describe(&at.install.secrets())
            ));
        }
        thread::sleep(Duration::from_secs(1));
    };
    let lower = logs.to_lowercase();
    assert!(
        lower.contains("read") && lower.contains("/etc/vibe/certs/privkey.pem"),
        "expected vibe's log to say the key's file /etc/vibe/certs/privkey.pem cannot be read:\n{}",
        blackbox::connection::masked(&logs, &at.install.secrets())
    );
    assert!(
        !at.reach.installation().is_ok_and(|a| a.status == 200),
        "expected nothing served without the key"
    );
    assert!(
        !logs.contains("\"event\":\"listening\""),
        "expected vibe never to listen without its key"
    );
}
