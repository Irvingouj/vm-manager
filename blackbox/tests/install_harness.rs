//! The install tests' harness (src/install.rs) proven without the image:
//! what the install tests lean on works with Docker as it is on the
//! machine, so that an install test that fails says something of the
//! image or of `deploy/`, not of the harness.
//!
//! - The identity provider's TLS front, bound to the Docker bridge's
//!   address, is reached by its name over TLS, trusting the authority in
//!   `ca/`, from a service of a Compose project given the harness's one
//!   extra file ([`harness_file`]), on the project's own network, as the
//!   installation's `vibe` reaches it.
//! - An installation is taken down whatever happens: containers, networks
//!   and volumes of the project, containers and tags of the test's own.
//!
//! They need Docker (the image `postgres:18` is pulled when the machine
//! does not have it),
//! not the image under test; they are run by `scripts/install-gate.sh`
//! with the install tests.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::install::{
    DOCKER_WAIT, IDP_HOST, Install, OutsideProvider, POSTGRES_18_IMAGE, harness_file,
};
use blackbox::tls::Handshake;

/// A service named `vibe` (as the harness file names it) that runs
/// `/probe.sh` with `sh`, the folder's `ca/` where the installation has it.
fn probe_compose() -> String {
    format!(
        "services:\n  vibe:\n    image: {POSTGRES_18_IMAGE}\n    pull_policy: never\n    entrypoint: [\"sh\", \"/probe.sh\"]\n    volumes:\n      - ./ca:/etc/vibe/ca:ro\n      - ./probe.sh:/probe.sh:ro\n"
    )
}

#[test]
#[ignore = "needs Docker: scripts/install-gate.sh"]
fn the_identity_provider_on_the_bridge_is_reached_by_its_name_over_tls_from_a_compose_service() {
    let idp = OutsideProvider::start(&[]);
    let port = idp.front.port();
    let mut install = Install::with_compose_text(&probe_compose());
    install.add_harness_file(&harness_file(idp.bridge));
    install.ca("identity-provider.pem", &idp.authority);
    install.write_file(
        "probe.sh",
        format!(
            "printf 'GET /.well-known/openid-configuration HTTP/1.1\\r\\nHost: {IDP_HOST}:{port}\\r\\nConnection: close\\r\\n\\r\\n' | openssl s_client -quiet -verify_return_error -CAfile /etc/vibe/ca/identity-provider.pem -servername {IDP_HOST} -connect {IDP_HOST}:{port}\n"
        )
        .as_bytes(),
    );
    install.write_env();
    let ran = install.compose(&["run", "--rm", "-T", "vibe"], DOCKER_WAIT);
    let issuer = format!("\"issuer\":\"https://{IDP_HOST}:{port}\"");
    if !ran.stdout.replace(' ', "").contains(&issuer) {
        install.fail(&format!(
            "expected the service to read the provider's configuration naming {issuer} over TLS\n{}\n{}",
            ran.describe(&[]),
            idp.front.describe()
        ));
    }
    let seen = idp.front.settled();
    assert!(
        seen.iter()
            .any(|s| s.handshake == Handshake::Completed
                && s.server_name.as_deref() == Some(IDP_HOST)),
        "expected a completed handshake for {IDP_HOST}\n{}",
        idp.front.describe()
    );
    assert!(idp.configuration_asked());
    install.take_down_now();
}

/// A project with a service that keeps running, a named volume and its own
/// network, and a container and a tag of the test's own beside it.
fn something_of_everything() -> Install {
    let mut install = Install::with_compose_text(&format!(
        "services:\n  postgres:\n    image: {POSTGRES_18_IMAGE}\n    pull_policy: never\n    environment:\n      POSTGRES_PASSWORD: ${{POSTGRES_PASSWORD:?the harness sets it}}\n    volumes:\n      - data:/var/lib/postgresql\nvolumes:\n  data: {{}}\n"
    ));
    install.set_secret("POSTGRES_PASSWORD", &blackbox::shared_database::token());
    install.up();
    install.start_own(&["--entrypoint", "sleep"], POSTGRES_18_IMAGE, &["600"], &[]);
    install.second_tag(POSTGRES_18_IMAGE);
    let left = install.left_behind();
    for kind in ["container", "network", "volume", "tag"] {
        assert!(
            left.iter().any(|l| l.starts_with(kind)),
            "expected the project to have made a {kind}, it has {left:?}"
        );
    }
    assert!(
        left.iter().filter(|l| l.starts_with("container")).count() >= 2,
        "expected the service's container and the test's own, got {left:?}"
    );
    install
}

/// What of `project` is on the machine: anything with its label, or whose
/// name begins with it (the test's own containers, its tag).
fn anything_of(project: &str) -> Vec<String> {
    let label = format!("label=com.docker.compose.project={project}");
    let mut found = Vec::new();
    for args in [
        vec!["ps", "--all", "--quiet", "--filter", label.as_str()],
        vec!["network", "ls", "--quiet", "--filter", label.as_str()],
        vec!["volume", "ls", "--quiet", "--filter", label.as_str()],
    ] {
        found.extend(
            blackbox::install::docker_ok(&args, DOCKER_WAIT)
                .stdout
                .split_whitespace()
                .map(str::to_owned),
        );
    }
    let named = format!("name={project}");
    found.extend(
        blackbox::install::docker_ok(&["ps", "--all", "--quiet", "--filter", &named], DOCKER_WAIT)
            .stdout
            .split_whitespace()
            .map(str::to_owned),
    );
    let images = blackbox::install::docker_ok(
        &[
            "image",
            "ls",
            "--quiet",
            "--filter",
            &format!("reference={project}"),
        ],
        DOCKER_WAIT,
    );
    found.extend(images.stdout.split_whitespace().map(str::to_owned));
    found
}

#[test]
#[ignore = "needs Docker: scripts/install-gate.sh"]
fn an_installation_is_taken_down_when_the_test_passes() {
    let install = something_of_everything();
    let project = install.project.clone();
    let dir = install.dir().to_path_buf();
    drop(install);
    assert_eq!(anything_of(&project), Vec::<String>::new());
    assert!(
        !dir.exists(),
        "expected the folder {} removed",
        dir.display()
    );
}

#[test]
#[ignore = "needs Docker: scripts/install-gate.sh"]
fn an_installation_is_taken_down_when_the_test_fails() {
    let project = std::sync::Mutex::new(String::new());
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let install = something_of_everything();
        project.lock().unwrap().clone_from(&install.project);
        install.fail("a failure of the harness's own making");
    }));
    let message = failed
        .err()
        .and_then(|e| e.downcast_ref::<String>().cloned())
        .unwrap_or_default();
    assert!(
        message.contains("a failure of the harness's own making")
            && message.contains("docker compose logs"),
        "expected the failure to carry the containers' logs, got {message}"
    );
    let project = project.lock().unwrap().clone();
    assert!(!project.is_empty());
    assert_eq!(anything_of(&project), Vec::<String>::new());
}

/// The walk's harness without Docker and without the image: the
/// harness's own `vibe serve` stands for the container (`--public-scheme
/// https --public-port <the front's port>`, the installation's key, the
/// provider's authority in `VIBE_CA_FILE`) behind a TLS front with the
/// wildcard certificate, as the Compose file's proxy will be; the identity
/// provider behind its front on `127.0.0.1` under its name. Through
/// [`Reach`]: OIDC connected by the tool over HTTPS, the tool signed in
/// through the browser, the template's Application deployed, the person
/// let in on `https://<application>.<domain>:<port>/`, redirects public,
/// sign-out by its form. So a failure of the walk against the image says
/// something of the image or of `deploy/`.
#[test]
#[ignore = "the install tests' harness, run with them: scripts/install-gate.sh"]
fn the_walks_harness_signs_in_deploys_and_signs_out_through_a_tls_front_before_the_program() {
    use blackbox::connection::{Connection, SecretKey, hide, setup_credential};
    use blackbox::deploy::{Deployed, commit_all, deploy_with_env, init_repository};
    use blackbox::install::Reach;
    use blackbox::project::new_project_in_repository;
    use blackbox::sessions::{ADA, ada};
    use blackbox::tls::{Authority, Bound, Files, text_of};
    use blackbox::tool::Machine;
    use blackbox::{DOMAIN, Vm, VmOptions};

    let idp = OutsideProvider::start_on(
        std::net::IpAddr::from([127, 0, 0, 1]),
        "localhost",
        &[ada()],
    );
    let files = Files::new();
    let idp_ca = files.authorities("identity-provider.pem", &[&idp.authority]);
    let wildcard_authority = Authority::new("Harness authority of the wildcard certificate");
    let tool_ca = text_of(&files.authorities("wildcard.pem", &[&wildcard_authority]));
    let bound = Bound::new();
    let port = bound.port().to_string();
    let vm = Vm::start(
        &VmOptions::new()
            .args_extra(&["--public-scheme", "https", "--public-port", &port])
            .env("VIBE_SECRET_KEY", SecretKey::fresh().expose())
            .env("VIBE_CA_FILE", &text_of(&idp_ca)),
    );
    let front = bound.tls(
        &wildcard_authority.issue(&[&format!("*.{DOMAIN}")]),
        vm.address(),
    );
    let reach = Reach::at(front.port(), &wildcard_authority);
    let installation = reach.installation().unwrap();
    assert_eq!(
        installation.json()["sign_in"],
        "none",
        "{}",
        installation.describe()
    );

    let setup = setup_credential(vm.platform());
    let connection = Connection::to(&idp.provider);
    let machine = Machine::new();
    let mut args = vec!["admin".to_owned(), "connect-oidc".to_owned()];
    args.extend(reach.place());
    args.extend(connection.tool_args());
    args.push("--json".to_owned());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let ran = machine.run_with_env(
        &refs,
        &[
            ("VIBE_SETUP_CREDENTIAL", setup.expose()),
            ("VIBE_OIDC_CLIENT_SECRET", connection.secret().expose()),
            ("VIBE_CA_FILE", tool_ca.as_str()),
        ],
    );
    hide(&ran, &[setup.expose(), connection.secret().expose()]).ok_json();
    assert_eq!(
        reach.installation().unwrap().json()["sign_in_address"],
        reach.origin("auth")
    );

    let (signed_in, steps) = reach.sign_in_tool(&idp, &machine, ADA, &tool_ca);
    assert_eq!(signed_in["user"], ADA);
    reach.assert_public_redirects(&steps, "the tool's sign-in");

    let project = new_project_in_repository(&machine, "greeter");
    init_repository(&project.dir);
    commit_all(&project.dir, "A new project");
    let run = deploy_with_env(
        &machine,
        &project.dir,
        &[],
        &[("VIBE_CA_FILE", tool_ca.as_str())],
    );
    assert_eq!(Deployed::of(&run).application, "greeter");

    let mut browser = reach.browser_as(&idp, ADA);
    let steps = reach.open_application(&mut browser, "greeter");
    let last = steps.last().unwrap();
    assert!(
        last.response.status == 200 && last.response.text().contains(ADA),
        "{}",
        blackbox::browser::describe_steps(&steps)
    );
    reach.assert_public_redirects(&steps, "the first sign-in");
    assert!(
        browser
            .jar
            .get(&format!("greeter.{DOMAIN}"), "__Host-vm-session")
            .is_some()
    );
    let (_, posted) = reach.sign_out(&mut browser);
    assert_eq!(
        posted.response.status,
        200,
        "{}",
        posted.response.describe()
    );
    let again = browser.open(&reach.url("greeter", "/"));
    assert!(again.is_redirect(), "{}", again.response.describe());
    reach.assert_public_redirects(&[again], "after sign-out");
    drop(front);
}

#[test]
#[ignore = "needs Docker: scripts/install-gate.sh"]
fn the_postgres_helpers_reach_a_postgres_service_inside_its_container_and_over_its_network() {
    let install = something_of_everything();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !install
        .exec(
            "postgres",
            &[
                "psql",
                "-U",
                "postgres",
                "-h",
                "127.0.0.1",
                "-c",
                "select 1",
            ],
        )
        .ok()
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the harness's Postgres did not start"
        );
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    assert_eq!(install.psql("postgres", "select current_user"), "postgres");
    let password = install.value("POSTGRES_PASSWORD").unwrap().to_owned();
    let over = install.psql_over_network("postgres", &password, "postgres");
    assert!(
        over.ok() && over.stdout.trim() == "postgres",
        "{}",
        over.describe(&install.secrets())
    );
    let wrong = install.psql_over_network("postgres", "not-the-password", "postgres");
    assert!(
        !wrong.ok()
            && wrong
                .stderr
                .contains("password authentication failed for user \"postgres\""),
        "{}",
        wrong.describe(&install.secrets())
    );
    let services: Vec<String> = install.containers().into_iter().map(|(s, _)| s).collect();
    assert_eq!(services, ["postgres"]);
    install.take_down_now();
}

#[test]
#[ignore = "needs Docker: scripts/install-gate.sh"]
fn the_administrators_postgres_of_the_test_is_reached_from_a_container_by_the_bridges_address() {
    let mut install = Install::with_compose_text(&format!(
        "services:\n  idle:\n    image: {POSTGRES_18_IMAGE}\n    pull_policy: never\n"
    ));
    let bridge = blackbox::install::bridge_address();
    let (name, url) = install.administrators_postgres(bridge, POSTGRES_18_IMAGE);
    assert_eq!(
        install.own_psql(
            &name,
            "select rolsuper, rolcreatedb, rolcreaterole from pg_roles where rolname = 'vibe'"
        ),
        "f|t|t"
    );
    // The address names the platform database `vibe`, which the program
    // makes; the role reaches `postgres` meanwhile.
    let reach = format!("{}/postgres", url.strip_suffix("/vibe").unwrap());
    install.hide(&reach);
    let ran = install.run_own(
        &["-e", "DATABASE"],
        POSTGRES_18_IMAGE,
        &[
            "sh",
            "-c",
            "psql \"$DATABASE\" -X -A -t -c 'select current_user'",
        ],
        &[("DATABASE", reach.as_str())],
        DOCKER_WAIT,
    );
    assert!(
        ran.ok() && ran.stdout.trim() == "vibe",
        "{}",
        ran.describe(&install.secrets())
    );
    install.take_down_now();
}

/// The harness's own check of [`blackbox::install::images_named_in`].
#[test]
fn the_images_a_compose_text_names_are_found() {
    assert_eq!(
        blackbox::install::images_named_in(
            "services:\n  a:\n    image: postgres:18\n    pull_policy: never\n  b:\n    image: \"caddy:2\"\n"
        ),
        vec!["postgres:18".to_owned(), "caddy:2".to_owned()]
    );
}

/// A tiny public image this harness uses nowhere else, for the check below.
const TINY_PUBLIC_IMAGE: &str = "hello-world:linux";

/// Removes [`TINY_PUBLIC_IMAGE`] by name when dropped, only when this test
/// pulled it (it was not on the machine before).
struct PulledHere(bool);

impl Drop for PulledHere {
    fn drop(&mut self) {
        if self.0 {
            drop(blackbox::install::docker(
                &["image", "rm", TINY_PUBLIC_IMAGE],
                DOCKER_WAIT,
            ));
        }
    }
}

#[test]
#[ignore = "needs Docker: scripts/install-gate.sh"]
fn a_container_of_the_tests_own_from_a_public_image_the_machine_lacks_is_pulled_first() {
    let present =
        || blackbox::install::docker(&["image", "inspect", TINY_PUBLIC_IMAGE], DOCKER_WAIT).ok();
    let _pulled_here = PulledHere(!present());
    let install = Install::with_compose_text("services: {}\n");
    let ran = install.run_own(&[], TINY_PUBLIC_IMAGE, &[], &[], DOCKER_WAIT);
    assert!(
        ran.ok() && ran.stdout.contains("Hello from Docker"),
        "expected a container of the test's own from {TINY_PUBLIC_IMAGE} to run, pulled when the machine did not have it\n{}",
        ran.describe(&[])
    );
    assert!(present(), "expected {TINY_PUBLIC_IMAGE} on the machine now");
}
