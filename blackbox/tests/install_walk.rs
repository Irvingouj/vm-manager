//! Installed with the Compose file as downloaded, from a machine with
//! Docker to a signed-in person and back after a restart, `down` and `up`,
//! and an upgrade (CONTRACT.md, "Installing: one image and a Compose file:
//! What the install tests show"; ticket "Install: one image and a Compose
//! file, hardened").
//!
//! One installation tells the story in order, since each step stands on
//! the one before: it comes up with the three required values and answers
//! through the proxy with the wildcard certificate, OIDC not connected;
//! `docker inspect` says it is hardened and only the proxy is published, on
//! loopback; the bundled Postgres holds the role `vibe` as the contract
//! says; `docker compose exec vibe vibe setup` prints the setup credential;
//! the tool outside connects OIDC over HTTPS to an identity provider the
//! container reaches over HTTPS under a private authority given in `ca/`;
//! the person signs the tool in through the browser and deploys the
//! template's Application; the request to `https://<application>.<domain>/`
//! as that person is answered by it; sign-out works by its form; every
//! redirect names `https` and the public port; then restarting `vibe`,
//! `down` and `up`, and a second tag of the image as `VIBE_IMAGE` keep the
//! Application, the connection and the sessions, and let the same person in
//! again.
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

use blackbox::DOMAIN;
use blackbox::browser::{Browser, describe_steps};
use blackbox::deploy::{Deployed, commit_all, deploy_with_env, init_repository};
use blackbox::install::{API_BODY_LIMIT, COMPOSE, DOCKER_WAIT, FrontDoor, Installed};
use blackbox::project::new_project_in_repository;
use blackbox::sessions::{ADA, ada};
use blackbox::tls::{Authority, tls_get};
use blackbox::tool::{Machine, windows_in};
use serde_json::{Value, json};

/// The template's Application, as the person names it.
const APPLICATION: &str = "greeter";

/// The uid of a user as `docker inspect` gives it (`uid` or `uid:gid`).
fn uid_of(user: &str) -> Option<u32> {
    user.split(':').next().and_then(|u| u.parse().ok())
}

/// The words of a container's command line as it was started.
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

/// What `docker inspect` says of the running `vibe` container and of what
/// is published.
fn assert_hardened_and_published_on_loopback_only(at: &Installed) {
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
    let user = vibe["Config"]["User"].as_str().unwrap_or_default();
    assert!(
        uid_of(user).is_some_and(|uid| uid != 0),
        "expected a user given as a number that is not 0: {}",
        context()
    );
    assert!(
        host["Tmpfs"]
            .as_object()
            .is_none_or(serde_json::Map::is_empty),
        "expected no tmpfs: {}",
        context()
    );
    let mounts = vibe["Mounts"].as_array().cloned().unwrap_or_default();
    assert_eq!(mounts.len(), 1, "expected one mount: {}", context());
    assert_eq!(mounts[0]["Destination"], "/etc/vibe/ca", "{}", context());
    assert_eq!(
        mounts[0]["RW"],
        false,
        "expected /etc/vibe/ca read-only: {}",
        context()
    );
    assert_eq!(mounts[0]["Type"], "bind", "{}", context());
    assert_eq!(
        host["RestartPolicy"]["Name"],
        "unless-stopped",
        "expected vibe restarted unless stopped: {}",
        context()
    );
    let words = command_line(&vibe);
    assert!(
        words.iter().any(|w| w == "serve"),
        "expected `serve` in {words:?}"
    );
    assert_option(&words, "--domain", DOMAIN);
    assert_option(&words, "--listen", "0.0.0.0:8080");
    assert_option(&words, "--public-scheme", "https");
    assert_option(&words, "--public-port", &at.reach.port.to_string());
    assert!(!words.iter().any(|w| w.contains("stand-in")), "{words:?}");
    let key = install.value("VIBE_SECRET_KEY").unwrap();
    let password = install.value("POSTGRES_PASSWORD").unwrap();
    assert!(
        !words
            .iter()
            .any(|w| w.contains(key) || w.contains(password)),
        "expected the installation's key and the database's address in no command line"
    );
    let environment: Vec<String> = vibe["Config"]["Env"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|e| e.as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(
        environment
            .iter()
            .any(|e| *e == format!("VIBE_SECRET_KEY={key}")),
        "expected the installation's key in vibe's environment"
    );
    assert!(
        environment
            .iter()
            .any(|e| e.starts_with("VIBE_DATABASE_URL=") && e.len() > "VIBE_DATABASE_URL=".len()),
        "expected the database's address in vibe's environment"
    );
    // Nothing but the proxy is published, on loopback.
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
    let shown = format!("{published:?}");
    assert_eq!(
        published.len(),
        1,
        "expected one port published, by the proxy: {shown}"
    );
    assert_eq!(
        (
            published[0].0.as_str(),
            published[0].2.as_str(),
            published[0].3.as_str()
        ),
        ("proxy", "127.0.0.1", at.reach.port.to_string().as_str()),
        "expected only the proxy published, on 127.0.0.1:VIBE_HTTPS_PORT: {shown}"
    );
}

/// The bundled Postgres: the role `vibe`, `PUBLIC` and the superuser.
fn assert_the_bundled_postgres(at: &Installed) {
    let install = &at.install;
    assert_eq!(
        install.psql(
            "postgres",
            "select rolsuper, rolcreatedb, rolcreaterole, rolcanlogin from pg_roles where rolname = 'vibe'"
        ),
        "f|t|t|t",
        "expected the role vibe: no superuser, may create databases and roles, may log in"
    );
    assert_eq!(
        install.psql(
            "postgres",
            "select datname from pg_database where datname = 'vibe'"
        ),
        "vibe",
        "expected the platform database vibe"
    );
    for database in ["postgres", "template1"] {
        assert_eq!(
            install.psql(
                "postgres",
                &format!("select has_database_privilege('public', '{database}', 'CONNECT')")
            ),
            "f",
            "expected CONNECT on {database} revoked from PUBLIC"
        );
    }
    let password = install.value("POSTGRES_PASSWORD").unwrap().to_owned();
    let secrets = install.secrets();
    // The role vibe reaches the platform database over the network with
    // POSTGRES_PASSWORD: the refusals below are of the role, not of the way.
    let vibe = install.psql_over_network("vibe", &password, "vibe");
    assert!(
        vibe.ok() && vibe.stdout.trim() == "vibe",
        "expected the role vibe to connect over the network with POSTGRES_PASSWORD\n{}",
        vibe.describe(&secrets)
    );
    for database in ["postgres", "template1"] {
        let refused = install.psql_over_network("vibe", &password, database);
        assert!(
            !refused.ok()
                && (refused.stderr.contains("CONNECT privilege")
                    || refused.stderr.contains("permission denied for database")),
            "expected the role vibe refused CONNECT on {database}\n{}",
            refused.describe(&secrets)
        );
    }
    let superuser = install.psql_over_network("postgres", &password, "postgres");
    assert!(
        !superuser.ok()
            && (superuser.stderr.contains("pg_hba.conf")
                || superuser
                    .stderr
                    .contains("password authentication failed for user \"postgres\"")),
        "expected the superuser postgres refused over the network\n{}",
        superuser.describe(&secrets)
    );
    assert_eq!(
        install.psql(
            "postgres",
            "select current_user || '|' || (select rolsuper::text from pg_roles where rolname = current_user)"
        ),
        "postgres|true",
        "expected the superuser let in inside the container"
    );
    // The platform connects as vibe, never as a superuser.
    assert_eq!(
        install.psql(
            "vibe",
            "select coalesce(string_agg(distinct tableowner, ','), '') from pg_tables where schemaname not in ('pg_catalog', 'information_schema')"
        ),
        "vibe",
        "expected every table of the platform database made by the role vibe"
    );
    assert_eq!(
        install.psql(
            "postgres",
            "select count(*) from pg_stat_activity a join pg_roles r on r.oid = a.usesysid where r.rolsuper and a.client_addr is not null"
        ),
        "0",
        "expected no superuser connected over the network"
    );
}

/// The person's browser opens the Application; fails unless it lands with
/// `200` on its address, a page that has the person's id, every redirect
/// naming the public address. Returns the number of steps it took.
fn opens_the_application(at: &Installed, browser: &mut Browser, context: &str) -> usize {
    let steps = at.reach.open_application(browser, APPLICATION);
    let last = steps.last().unwrap();
    if last.response.status != 200
        || last.url != at.reach.url(APPLICATION, "/")
        || !last.response.text().contains(ADA)
    {
        at.install.fail(&format!(
            "{context}: expected the person to land on {} with 200 and a page that has their id {ADA}, but the way went:\n{}",
            at.reach.url(APPLICATION, "/"),
            describe_steps(&steps)
        ));
    }
    at.reach.assert_public_redirects(&steps, context);
    steps.len()
}

/// After a restart, `down` and `up`, or an upgrade: the connection is
/// there, the browser that kept its cookies is still in (no redirect), the
/// same person is let in again by a fresh sign-in, and the tool's
/// credential still lists the Application.
fn kept(at: &mut Installed, kept_browser: &mut Browser, machine: &Machine, context: &str) {
    let installation = at.reach.wait_ready(&at.install);
    assert_eq!(
        installation["sign_in"], "oidc",
        "{context}: expected the OIDC connection kept: {installation}"
    );
    let steps = opens_the_application(at, kept_browser, context);
    assert_eq!(
        steps, 1,
        "{context}: expected the session kept: the Application answered with no sign-in"
    );
    let mut fresh = at.reach.browser_as(&at.idp, ADA);
    opens_the_application(at, &mut fresh, context);
    let listed = at.tool_ok(machine, &["applications", "--json"], &[], &[]);
    assert!(
        listed["applications"]
            .as_array()
            .is_some_and(|a| a.iter().any(|x| x["name"] == APPLICATION)),
        "{context}: expected the tool's credential kept and the Application listed: {listed}"
    );
}

#[test]
#[ignore = "needs Docker and a built image: scripts/install-gate.sh"]
fn installed_as_downloaded_a_person_signs_in_deploys_and_is_let_in_again_after_restart_down_up_and_an_upgrade()
 {
    let mut at = Installed::prepare(&[COMPOSE], &[ada()], FrontDoor::TheFilesProxy);

    // It comes up with the three required values and answers through the
    // proxy with the wildcard certificate; no sign-in is connected.
    let installation = at.up_and_ready();
    assert_eq!(
        installation,
        json!({ "domain": DOMAIN, "sign_in": "none", "sign_in_address": null }),
        "expected the installation to say no sign-in is connected"
    );
    let api = format!("api.{DOMAIN}");
    let stranger = Authority::new("An authority nobody installed");
    assert!(
        tls_get(
            at.reach.address,
            &api,
            &[&stranger],
            "/v1/installation",
            &at.reach.host("api")
        )
        .is_err(),
        "expected the proxy's certificate to be the wildcard one, under the test's authority only"
    );

    assert_hardened_and_published_on_loopback_only(&at);
    assert_the_bundled_postgres(&at);

    // The setup credential, then OIDC connected from the tool outside.
    let setup = at.setup_credential();
    let machine = Machine::new();
    let connected = at.connect_oidc(&machine, &setup);
    assert_eq!(connected["connected_by"], "setup-credential", "{connected}");
    assert_eq!(connected["issuer"], at.idp.provider.issuer(), "{connected}");
    assert!(
        at.idp.configuration_asked(),
        "expected the installation to have asked the identity provider for its configuration over HTTPS\n{}",
        at.idp.front.describe()
    );
    let installation = at.reach.wait_ready(&at.install);
    assert_eq!(
        installation,
        json!({ "domain": DOMAIN, "sign_in": "oidc", "sign_in_address": at.reach.origin("auth") }),
        "expected the sign-in address to be the public one"
    );

    // The person signs the tool in through the browser and deploys the
    // template's Application.
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
    let deployed = Deployed::of(&run);
    assert_eq!(deployed.application, APPLICATION);
    assert_eq!(deployed.version, 1);

    // A body over the API's limit, through the proxy as a signed-in
    // person, is the platform's own refusal; one at the limit is not
    // refused for its size.
    let credential = at.reach.kept_credential(&machine);
    at.install.hide(&credential);
    for (size, over) in [(API_BODY_LIMIT + 1, true), (API_BODY_LIMIT, false)] {
        let answer = at
            .reach
            .post_of_size("/v1/applications", size)
            .bearer(&credential)
            .try_send();
        let answer = match answer {
            Ok(answer) => answer,
            Err(problem) => at.install.fail(&format!(
                "expected the platform to answer a body of {size} bytes through the proxy, got no answer: {problem}"
            )),
        };
        let reason = serde_json::from_slice::<Value>(answer.body())
            .ok()
            .and_then(|b| b["error"]["reason"].as_str().map(str::to_owned));
        let refused_for_size = answer.status == 413 && reason.as_deref() == Some("too-large");
        if over != refused_for_size || reason.is_none() {
            at.install.fail(&format!(
                "expected a body of {size} bytes through the proxy to be answered by the platform's own JSON {}\n{}",
                if over { "refusal 413 too-large" } else { "answer, not refused for its size" },
                blackbox::connection::masked(&answer.describe(), &[&credential])
            ));
        }
    }

    // The walking skeleton's request, as installed.
    let mut browser = at.reach.browser_as(&at.idp, ADA);
    opens_the_application(&at, &mut browser, "the first sign-in");
    let session = browser
        .jar
        .get(&format!("{APPLICATION}.{DOMAIN}"), "__Host-vm-session");
    assert!(
        session.is_some_and(|c| c.secure && c.http_only),
        "expected the Application's session in a __Host- cookie, Secure and HttpOnly; the jar holds {:?} (refused {:?})",
        browser.jar.all(),
        browser.jar.rejected()
    );

    // The Application's own database and its roles.
    let database = at.install.psql(
        "postgres",
        &format!(
            "select datname from pg_database where shobj_description(oid, 'pg_database') = 'vibe application {APPLICATION}'"
        ),
    );
    assert!(
        database.starts_with("vibe_app_") && !database.contains('\n'),
        "expected one own database vibe_app_<n> for {APPLICATION}, got {database:?}"
    );
    assert_eq!(
        at.install.psql(
            "postgres",
            &format!(
                "select string_agg(rolname, ',' order by rolname) from pg_roles where rolname in ('{database}_owner', '{database}_runtime')"
            )
        ),
        format!("{database}_owner,{database}_runtime"),
        "expected the own database's roles"
    );

    // A second browser of the same person, kept through what follows.
    let mut kept_browser = at.reach.browser_as(&at.idp, ADA);
    opens_the_application(&at, &mut kept_browser, "a second browser");

    // Sign-out by its form.
    let (page, posted) = at.reach.sign_out(&mut browser);
    at.reach
        .assert_public_redirects(&[page, posted.clone()], "sign-out");
    assert_eq!(
        posted.response.status,
        200,
        "{}",
        posted.response.describe()
    );
    let at_application = at.reach.url(APPLICATION, "/");
    let request = browser
        .request("GET", &at_application)
        .header("accept", "application/json");
    let after = browser.send(&at_application, &request);
    assert_eq!(
        after.status,
        401,
        "expected no session on the Application after sign-out\n{}",
        after.describe()
    );
    let again = browser.open(&at_application);
    assert!(
        again.is_redirect()
            && again.location().host_str() == Some(format!("auth.{DOMAIN}").as_str())
            && again.location().path() == "/sign-in",
        "expected a page of the Application to go through sign-in again\n{}",
        again.response.describe()
    );
    at.reach.assert_public_redirects(&[again], "after sign-out");
    assert_eq!(
        opens_the_application(&at, &mut kept_browser, "the other browser after sign-out"),
        1
    );

    // The setup credential and the client secret are in no container's log.
    let logs = at.install.raw_logs();
    let client_secret = at.idp.provider.client_secret().expose().to_owned();
    for (what, secret) in [
        ("the setup credential", setup.expose()),
        ("the client secret", client_secret.as_str()),
    ] {
        let found = windows_in(&[("docker compose logs".to_owned(), logs.clone())], secret);
        assert!(
            found.is_empty(),
            "expected {what} in no container's log, found at {found:?}"
        );
    }

    // Restarting vibe.
    at.install.compose_ok(&["restart", "vibe"], DOCKER_WAIT);
    kept(
        &mut at,
        &mut kept_browser,
        &machine,
        "after `docker compose restart vibe`",
    );

    // `down`, then `up`.
    at.install.compose_ok(&["down"], DOCKER_WAIT);
    at.install.up();
    kept(
        &mut at,
        &mut kept_browser,
        &machine,
        "after `docker compose down` and `up -d`",
    );

    // An upgrade: VIBE_IMAGE names a second tag of the image.
    let image = at.image.clone();
    let upgraded = at.install.second_tag(&image);
    at.install.set("VIBE_IMAGE", &upgraded);
    at.install.up();
    let vibe = at.install.inspect(&at.install.container("vibe"));
    assert_eq!(
        vibe["Config"]["Image"], upgraded,
        "expected vibe started from the image VIBE_IMAGE now names"
    );
    kept(
        &mut at,
        &mut kept_browser,
        &machine,
        "after an upgrade to a second tag of the image",
    );
}
