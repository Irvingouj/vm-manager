//! `deploy/compose.own-tls.yaml` and what `deploy/README.md` says of it,
//! read from the repository in the daily gate (CONTRACT.md, "The program
//! serves TLS itself: The Compose file"; ticket "The program serves TLS
//! itself with a supplied wildcard certificate": "The Compose file has
//! this as a documented alternative to its reverse proxy"). What Docker
//! does with it is shown by the install tests (`install_own_tls.rs`).
//!
//! Given after `compose.yaml` with `-f`: no `proxy` service; `vibe`
//! published at `${VIBE_BIND:-127.0.0.1}:${VIBE_HTTPS_PORT:-443}` on the
//! port it listens on; `VIBE_TLS_CERT_FILE` and `VIBE_TLS_KEY_FILE` naming
//! `fullchain.pem` and `privkey.pem` of `./certs` mounted read-only at
//! `/etc/vibe/certs`; nothing else of `vibe` changed, its hardening least
//! of all. The files are parsed as YAML (a comment is not read); Compose's
//! own merge (`!reset`, lists appended) is read as Compose 2.24 and later
//! do it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::release::read_repository_file;
use yaml_rust2::{Yaml, YamlLoader};

const OWN_TLS: &str = "deploy/compose.own-tls.yaml";

fn load(file: &str) -> Yaml {
    let text = read_repository_file(file);
    let mut docs = YamlLoader::load_from_str(&text)
        .unwrap_or_else(|e| panic!("expected {file} to be YAML: {e}"));
    assert_eq!(docs.len(), 1, "expected one YAML document in {file}");
    docs.remove(0)
}

fn key(name: &str) -> Yaml {
    Yaml::String(name.to_owned())
}

/// The `services` of a file, as (name, definition).
fn services(doc: &Yaml) -> Vec<(String, Yaml)> {
    doc["services"]
        .as_hash()
        .map(|h| {
            h.iter()
                .map(|(k, v)| (k.as_str().unwrap_or_default().to_owned(), v.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a line of `text` says `<indent>proxy: !reset null`: the service
/// taken away when the file is given after `compose.yaml`.
fn resets_the_proxy(text: &str) -> bool {
    text.lines().any(|l| {
        let words: Vec<&str> = l.split_whitespace().collect();
        l.starts_with("  ") && !l.starts_with("   ") && words == ["proxy:", "!reset", "null"]
    })
}

#[test]
fn the_harness_reads_composes_reset_as_compose_own_proxy_yaml_writes_it() {
    let text = read_repository_file("deploy/compose.own-proxy.yaml");
    assert!(resets_the_proxy(&text));
    let doc = load("deploy/compose.own-proxy.yaml");
    assert!(
        services(&doc).iter().any(|(n, _)| n == "proxy"),
        "the harness's own check: `proxy: !reset null` is read as a key `proxy`"
    );
}

#[test]
fn the_own_tls_file_takes_the_proxy_away_and_adds_no_service() {
    let text = read_repository_file(OWN_TLS);
    assert!(
        resets_the_proxy(&text),
        "expected {OWN_TLS} to take the proxy away (`proxy: !reset null` under `services`), as compose.own-proxy.yaml does"
    );
    let names: Vec<String> = services(&load(OWN_TLS))
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    for name in &names {
        assert!(
            name == "proxy" || name == "vibe",
            "expected {OWN_TLS} to change `vibe` and take `proxy` away, nothing else; it has the service {name:?}"
        );
    }
    assert!(names.iter().any(|n| n == "vibe"), "{names:?}");
}

/// The port `vibe` listens on inside its container: `--listen`'s in the
/// command `compose.yaml` gives it (the own-TLS file gives none).
fn listening_port() -> String {
    let compose = load("deploy/compose.yaml");
    let command: Vec<String> = compose["services"]["vibe"]["command"]
        .as_vec()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|w| w.as_str().unwrap_or_default().to_owned())
        .collect();
    let at = command
        .iter()
        .position(|w| w == "--listen")
        .unwrap_or_else(|| {
            panic!("expected --listen in compose.yaml's command of vibe: {command:?}")
        });
    command[at + 1]
        .rsplit(':')
        .next()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn vibe_is_published_at_vibe_bind_and_vibe_https_port_on_the_port_it_listens_on() {
    let doc = load(OWN_TLS);
    let vibe = &doc["services"]["vibe"];
    let ports = vibe["ports"]
        .as_vec()
        .unwrap_or_else(|| panic!("expected {OWN_TLS} to publish vibe (`ports`)"));
    assert_eq!(ports.len(), 1, "expected vibe published once: {ports:?}");
    let port = listening_port();
    let published = &ports[0];
    let ok = match published {
        Yaml::String(short) => {
            *short == format!("${{VIBE_BIND:-127.0.0.1}}:${{VIBE_HTTPS_PORT:-443}}:{port}")
        }
        Yaml::Hash(_) => {
            published["host_ip"].as_str() == Some("${VIBE_BIND:-127.0.0.1}")
                && published["published"].as_str() == Some("${VIBE_HTTPS_PORT:-443}")
                && (published["target"]
                    .as_i64()
                    .map(|t| t.to_string())
                    .as_deref()
                    == Some(port.as_str())
                    || published["target"].as_str() == Some(port.as_str()))
        }
        _ => false,
    };
    assert!(
        ok,
        "expected vibe published at ${{VIBE_BIND:-127.0.0.1}}:${{VIBE_HTTPS_PORT:-443}} on its port {port}, got {published:?}"
    );
    assert!(
        vibe["command"].is_badvalue(),
        "expected {OWN_TLS} to leave vibe's command as compose.yaml gives it (`serve ... --public-scheme https --public-port ...`)"
    );
}

/// The mounts of a service's `volumes`, as (source, target, read-only).
fn mounts(service: &Yaml) -> Vec<(String, String, bool)> {
    service["volumes"]
        .as_vec()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|v| match v {
            Yaml::String(short) => {
                let parts: Vec<&str> = short.split(':').collect();
                (
                    parts.first().copied().unwrap_or_default().to_owned(),
                    parts.get(1).copied().unwrap_or_default().to_owned(),
                    parts
                        .get(2)
                        .is_some_and(|m| m.split(',').any(|o| o == "ro")),
                )
            }
            long => (
                long["source"].as_str().unwrap_or_default().to_owned(),
                long["target"].as_str().unwrap_or_default().to_owned(),
                long["read_only"].as_bool() == Some(true),
            ),
        })
        .collect()
}

#[test]
fn the_two_variables_name_the_files_of_certs_mounted_read_only() {
    let doc = load(OWN_TLS);
    let vibe = &doc["services"]["vibe"];
    let environment = &vibe["environment"];
    let value = |name: &str| -> Option<String> {
        match environment {
            Yaml::Hash(h) => h
                .get(&key(name))
                .and_then(|v| v.as_str().map(str::to_owned)),
            Yaml::Array(items) => items.iter().find_map(|i| {
                i.as_str()?
                    .strip_prefix(&format!("{name}="))
                    .map(str::to_owned)
            }),
            _ => None,
        }
    };
    assert_eq!(
        value("VIBE_TLS_CERT_FILE").as_deref(),
        Some("/etc/vibe/certs/fullchain.pem"),
        "expected VIBE_TLS_CERT_FILE to name the certificate of ./certs as the container sees it"
    );
    assert_eq!(
        value("VIBE_TLS_KEY_FILE").as_deref(),
        Some("/etc/vibe/certs/privkey.pem"),
        "expected VIBE_TLS_KEY_FILE to name the key of ./certs as the container sees it"
    );
    let own = mounts(vibe);
    assert_eq!(
        own,
        vec![("./certs".to_owned(), "/etc/vibe/certs".to_owned(), true)],
        "expected {OWN_TLS} to mount ./certs read-only at /etc/vibe/certs, and nothing else"
    );
    // Merged after compose.yaml (lists are appended): /etc/vibe/ca and
    // /etc/vibe/certs read-only, /var/lib/vibe/storage read-write.
    let compose = load("deploy/compose.yaml");
    let mut all = mounts(&compose["services"]["vibe"]);
    all.extend(own);
    let mut targets: Vec<(String, bool)> = all.into_iter().map(|(_, t, ro)| (t, ro)).collect();
    targets.sort();
    assert_eq!(
        targets,
        vec![
            ("/etc/vibe/ca".to_owned(), true),
            ("/etc/vibe/certs".to_owned(), true),
            ("/var/lib/vibe/storage".to_owned(), false)
        ],
        "expected vibe's mounts, merged, to be /etc/vibe/ca and /etc/vibe/certs read-only and /var/lib/vibe/storage read-write"
    );
}

#[test]
fn the_hardening_of_vibe_is_left_as_compose_yaml_gives_it() {
    let doc = load(OWN_TLS);
    let vibe = doc["services"]["vibe"]
        .as_hash()
        .unwrap_or_else(|| panic!("expected {OWN_TLS} to change the service vibe"));
    for (name, _) in vibe {
        let name = name.as_str().unwrap_or_default();
        assert!(
            ["ports", "environment", "volumes"].contains(&name),
            "expected {OWN_TLS} to change only vibe's ports, environment and mounts (read-only root, no capability, no-new-privileges, the image's user, no tmpfs, its command: as compose.yaml gives them); it sets {name:?}"
        );
    }
}

/// The paragraphs of a text (split at empty lines), white space squashed.
fn paragraphs(text: &str) -> Vec<String> {
    text.split("\n\n")
        .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|p| !p.is_empty())
        .collect()
}

#[test]
fn the_install_document_gives_the_own_tls_file_as_the_alternative_to_the_proxy() {
    let readme = read_repository_file("deploy/README.md");
    let lower = readme.to_lowercase();
    assert!(
        readme.contains("compose.own-tls.yaml"),
        "expected deploy/README.md to name compose.own-tls.yaml"
    );
    assert!(
        readme.lines().any(|l| l.contains("docker compose")
            && l.contains("-f compose.yaml")
            && l.contains("-f compose.own-tls.yaml")
            && l.contains("up -d")),
        "expected deploy/README.md to give the command `docker compose -f compose.yaml -f compose.own-tls.yaml up -d`"
    );
    // The image's user, by number, and the two commands that give it the
    // key.
    let key_commands: Vec<&str> = readme
        .lines()
        .filter(|l| {
            l.contains("privkey.pem")
                && ["chown", "chgrp", "chmod", "setfacl"]
                    .iter()
                    .any(|c| l.contains(c))
        })
        .collect();
    assert!(
        key_commands.len() >= 2 && key_commands.iter().any(|l| l.contains("65532")),
        "expected deploy/README.md to give two commands that make privkey.pem readable by the image's user 65532 (a chown/chgrp naming 65532 and a chmod); found {key_commands:?}"
    );
    // Replacing the certificate: no restart, within 10 seconds, the log.
    for (words, what) in [
        (
            "tls-certificate-loaded",
            "the record that says it was taken",
        ),
        ("docker compose logs vibe", "where to see it"),
        ("10 seconds", "within how long it is taken"),
        ("restart", "that no restart is needed"),
        ("renam", "replacing both files by renaming"),
        ("http/2", "what it lacks beside the proxy, HTTP/2"),
    ] {
        assert!(
            lower.contains(words),
            "expected deploy/README.md to say {what} ({words:?})"
        );
    }
    // Not together with compose.own-proxy.yaml.
    let together = paragraphs(&readme).into_iter().any(|p| {
        let l = p.to_lowercase();
        p.contains("compose.own-tls.yaml")
            && p.contains("compose.own-proxy.yaml")
            && (l.contains("not") || l.contains("never") || l.contains("cannot"))
    });
    assert!(
        together,
        "expected deploy/README.md to say, in one paragraph, that compose.own-tls.yaml cannot be given together with compose.own-proxy.yaml"
    );
}

/// The section of a Markdown text whose heading holds `title`: from that
/// heading to the next heading of the same level or above.
fn section(text: &str, title: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let level = |l: &str| l.chars().take_while(|c| *c == '#').count();
    let at = lines
        .iter()
        .position(|l| level(l) > 0 && l.contains(title))?;
    let own = level(lines[at]);
    let end = lines
        .iter()
        .skip(at + 1)
        .position(|l| (1..=own).contains(&level(l)))
        .map_or(lines.len(), |n| at + 1 + n);
    Some(lines[at..end].join("\n"))
}

#[test]
fn the_install_documents_own_tls_section_names_the_kinds_of_key_taken_with_the_bounds_of_an_rsa_key()
 {
    let readme = read_repository_file("deploy/README.md");
    let title = "The program serves TLS itself";
    let part = section(&readme, title)
        .unwrap_or_else(|| panic!("expected deploy/README.md to have a section \"{title}\""));
    let lower = part.to_lowercase();
    for (words, what) in [
        (&["2048"][..], "the lower bound of an RSA key"),
        (&["4096"][..], "the upper bound of an RSA key"),
        (
            &["p-256", "p256", "secp256r1", "prime256v1"][..],
            "ECDSA on P-256",
        ),
        (&["p-384", "p384", "secp384r1"][..], "ECDSA on P-384"),
        (&["ed25519"][..], "Ed25519"),
    ] {
        assert!(
            words.iter().any(|w| lower.contains(w)),
            "expected the section \"{title}\" of deploy/README.md to name the kinds of key taken: {what} (one of {words:?}); it says:\n{part}"
        );
    }
}
