//! The files of `deploy/` as read from the repository, in the daily gate
//! (no Docker needed: what Docker shows of them is in the install tests,
//! `install_*.rs`, which the gate does not run).
//!
//! CONTRACT.md, "Installing: one image and a Compose file: The Compose
//! files", "The same on every Compose version":
//!
//! - no Compose file of `deploy/` writes a value Compose is to ask for
//!   (`${NAME:?...}` or `${NAME?...}`) inside another value's default or
//!   replacement (`:-`, `-`, `:+`, `+`), at any depth: Compose 2.38.2 looks
//!   at such an inner value always, 5.0.2 only when the outer one is
//!   missing;
//! - `deploy/README.md` names the Docker Engine and Compose versions the
//!   release's install tests ran with (the hosted runner's, 28.0.4 with
//!   2.38.2, and the install gate's machine's, 29.2.1 with 5.0.2), says of
//!   the minimum it gives (Compose 2.24.1) that it was not run, and no
//!   longer says that Compose asks for `POSTGRES_PASSWORD` (the bundled
//!   Postgres does).
//!
//! The substitutions are read from the files parsed as YAML (so a comment
//! is not read), each string scalar parsed as Compose interpolates it:
//! `$$` is a dollar and begins no substitution, `$NAME` and `${NAME}` are
//! plain, `${NAME<op><text>}` has a text that may hold substitutions of its
//! own.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::release::read_repository_file;
use yaml_rust2::{Yaml, YamlLoader};

const COMPOSE_FILES: [&str; 4] = [
    "deploy/compose.yaml",
    "deploy/compose.own-proxy.yaml",
    "deploy/compose.own-postgres.yaml",
    "deploy/compose.own-tls.yaml",
];

/// One piece of an interpolated value.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Text(String),
    /// `$NAME`, `${NAME}` (`op` empty), or `${NAME<op><body>}`.
    Substitution {
        name: String,
        op: String,
        body: Vec<Piece>,
        /// The whole substitution as written.
        written: String,
    },
}

/// The operators of a braced substitution, longest first.
const OPERATORS: [&str; 6] = [":-", ":?", ":+", "-", "?", "+"];

/// Reads `text` from `at` into pieces, up to the end, or up to the `}`
/// that closes a substitution when `inside` (returned with the place after
/// it). Plain `{`/`}` pairs inside a body are text and are counted, so that
/// only the closing `}` ends it.
fn pieces(text: &[char], mut at: usize, inside: bool) -> Result<(Vec<Piece>, usize), String> {
    let mut found = Vec::new();
    let mut plain = String::new();
    let mut depth = 0_usize;
    let flush = |plain: &mut String, found: &mut Vec<Piece>| {
        if !plain.is_empty() {
            found.push(Piece::Text(std::mem::take(plain)));
        }
    };
    while at < text.len() {
        let c = text[at];
        if c == '$' {
            match text.get(at + 1) {
                Some('$') => {
                    plain.push('$');
                    at += 2;
                }
                Some('{') => {
                    flush(&mut plain, &mut found);
                    let start = at;
                    at += 2;
                    let mut name = String::new();
                    while at < text.len() && (text[at].is_ascii_alphanumeric() || text[at] == '_') {
                        name.push(text[at]);
                        at += 1;
                    }
                    if name.is_empty() {
                        return Err(format!("a `${{` with no name at {start}"));
                    }
                    let rest: String = text[at..text.len().min(at + 2)].iter().collect();
                    let op = OPERATORS
                        .iter()
                        .find(|o| rest.starts_with(**o))
                        .map(|o| (*o).to_owned())
                        .unwrap_or_default();
                    let body = if op.is_empty() {
                        if text.get(at) != Some(&'}') {
                            return Err(format!(
                                "expected `}}` or an operator after `${{{name}` at {start}"
                            ));
                        }
                        at += 1;
                        Vec::new()
                    } else {
                        at += op.chars().count();
                        let (body, after) = pieces(text, at, true)?;
                        at = after;
                        body
                    };
                    found.push(Piece::Substitution {
                        name,
                        op,
                        body,
                        written: text[start..at].iter().collect(),
                    });
                }
                Some(n) if n.is_ascii_alphabetic() || *n == '_' => {
                    flush(&mut plain, &mut found);
                    let start = at;
                    at += 1;
                    let mut name = String::new();
                    while at < text.len() && (text[at].is_ascii_alphanumeric() || text[at] == '_') {
                        name.push(text[at]);
                        at += 1;
                    }
                    found.push(Piece::Substitution {
                        name,
                        op: String::new(),
                        body: Vec::new(),
                        written: text[start..at].iter().collect(),
                    });
                }
                _ => {
                    plain.push('$');
                    at += 1;
                }
            }
            continue;
        }
        if inside && c == '}' {
            if depth == 0 {
                flush(&mut plain, &mut found);
                return Ok((found, at + 1));
            }
            depth -= 1;
        } else if inside && c == '{' {
            depth += 1;
        }
        plain.push(c);
        at += 1;
    }
    if inside {
        return Err("a `${` is not closed".to_owned());
    }
    flush(&mut plain, &mut found);
    Ok((found, at))
}

fn parse(text: &str) -> Result<Vec<Piece>, String> {
    let chars: Vec<char> = text.chars().collect();
    pieces(&chars, 0, false).map(|(found, _)| found)
}

/// Every required substitution (`:?`, `?`) inside the default or
/// replacement (`:-`, `-`, `:+`, `+`) of another, at any depth: `(the
/// outer as written, the inner as written)`.
fn required_inside_defaults(pieces: &[Piece]) -> Vec<(String, String)> {
    fn required_in(pieces: &[Piece], found: &mut Vec<String>) {
        for piece in pieces {
            if let Piece::Substitution {
                op, body, written, ..
            } = piece
            {
                if op == ":?" || op == "?" {
                    found.push(written.clone());
                }
                required_in(body, found);
            }
        }
    }
    let mut found = Vec::new();
    for piece in pieces {
        if let Piece::Substitution {
            op, body, written, ..
        } = piece
        {
            if [":-", "-", ":+", "+"].contains(&op.as_str()) {
                let mut inner = Vec::new();
                required_in(body, &mut inner);
                found.extend(inner.into_iter().map(|i| (written.clone(), i)));
            }
            found.extend(required_inside_defaults(body));
        }
    }
    found
}

/// Every string of a YAML document: scalars and keys.
fn strings(yaml: &Yaml, found: &mut Vec<String>) {
    match yaml {
        Yaml::String(s) | Yaml::Real(s) => found.push(s.clone()),
        Yaml::Array(items) => items.iter().for_each(|i| strings(i, found)),
        Yaml::Hash(hash) => {
            for (key, value) in hash {
                strings(key, found);
                strings(value, found);
            }
        }
        _ => {}
    }
}

#[test]
fn no_compose_file_asks_for_a_value_inside_another_values_default() {
    let mut wrong = Vec::new();
    for file in COMPOSE_FILES {
        let text = read_repository_file(file);
        let docs = YamlLoader::load_from_str(&text)
            .unwrap_or_else(|e| panic!("expected {file} to be YAML: {e}"));
        let mut all = Vec::new();
        docs.iter().for_each(|d| strings(d, &mut all));
        for value in all {
            let parsed = parse(&value)
                .unwrap_or_else(|problem| panic!("{file}: cannot read {value:?}: {problem}"));
            for (outer, inner) in required_inside_defaults(&parsed) {
                wrong.push(format!("{file}: {inner} is inside the default of {outer}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "expected no value Compose is to ask for (`${{NAME:?...}}`) inside another value's default or replacement, since Compose versions differ in whether they look at it (CONTRACT.md, \"The same on every Compose version\"); found:\n  {}",
        wrong.join("\n  ")
    );
}

/// The harness's own check of [`parse`] and [`required_inside_defaults`].
#[test]
fn the_harness_reads_composes_substitutions() {
    let inner =
        |text: &str| -> Vec<(String, String)> { required_inside_defaults(&parse(text).unwrap()) };
    assert_eq!(
        inner("${A:-x${B:?e}y}"),
        vec![("${A:-x${B:?e}y}".to_owned(), "${B:?e}".to_owned())]
    );
    assert_eq!(inner("${A:+${B?e}}").len(), 1);
    assert_eq!(inner("${A-${B:?e}}").len(), 1);
    // At any depth: both A's and C's default hold B.
    let deep = inner("${A-${C:-pre ${B?e}}}");
    assert!(
        deep.iter()
            .any(|(o, i)| o.starts_with("${A-") && i == "${B?e}")
            && deep
                .iter()
                .any(|(o, i)| o.starts_with("${C:-") && i == "${B?e}"),
        "{deep:?}"
    );
    for fine in [
        "${A:?message}",
        "${A:-x} ${B:?e}",
        "$A ${B:-$C} ${D}",
        "${A:?needs ${B:-x}}",
        // An escaped dollar begins no substitution.
        "${A:-$${B:?e}}",
        "$${A:-${B:?e}}",
        "cat \"$${PGDATA}/pg_hba.conf\" > \"$${PGDATA}/x\"",
        // Plain braces in a default are text.
        "${A:-{literal}}${B:?e}",
        "a $ alone, $1, and text",
    ] {
        assert_eq!(inner(fine), Vec::new(), "{fine}");
    }
    assert_eq!(
        parse("$${A:-${B:?e}}").unwrap(),
        vec![
            Piece::Text("${A:-".to_owned()),
            Piece::Substitution {
                name: "B".to_owned(),
                op: ":?".to_owned(),
                body: vec![Piece::Text("e".to_owned())],
                written: "${B:?e}".to_owned(),
            },
            Piece::Text("}".to_owned()),
        ]
    );
    assert!(parse("${A:-x").is_err());
    assert!(parse("${:-x}").is_err());
}

/// `text` with every run of white space one space.
fn squashed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn the_install_document_names_the_docker_versions_the_install_tests_ran_with() {
    let readme = read_repository_file("deploy/README.md");
    for (engine, compose, which) in [
        ("28.0.4", "2.38.2", "the release workflow's hosted runner"),
        ("29.2.1", "5.0.2", "the install gate's machine"),
    ] {
        assert!(
            readme
                .lines()
                .any(|line| line.contains(engine) && line.contains(compose)),
            "expected deploy/README.md to name Docker Engine {engine} with Compose {compose} ({which}) together, on one line, as versions the install tests ran with"
        );
    }
    // The minimum it gives was read from Docker's notes, not run: "not
    // run" within a few hundred characters of the minimum Compose version.
    let text = squashed(&readme).to_lowercase();
    let minimum: Vec<usize> = text.match_indices("2.24.1").map(|(at, _)| at).collect();
    let near = text
        .match_indices("not run")
        .any(|(at, _)| minimum.iter().any(|m| m.abs_diff(at) <= 300));
    assert!(
        near,
        "expected deploy/README.md to say of the minimum it gives (Compose 2.24.1) that it was not run (the words \"not run\" near \"2.24.1\")"
    );
}

#[test]
fn the_install_document_no_longer_says_compose_asks_for_postgres_password() {
    let readme = read_repository_file("deploy/README.md");
    let wrong: Vec<&str> = readme
        .lines()
        .filter(|line| {
            let lower = line.to_lowercase();
            let compose_asks = lower.contains("compose")
                && (lower.contains("refuse")
                    || lower.contains("asks")
                    || lower.contains("names the value"));
            let of_the_password = line.contains("POSTGRES_PASSWORD")
                || lower.contains("of the three")
                || lower.contains("the three values");
            compose_asks && of_the_password
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "expected deploy/README.md not to say that Compose asks for POSTGRES_PASSWORD (the bundled Postgres does, CONTRACT.md \"The Compose files\"); these lines say it:\n  {}",
        wrong.join("\n  ")
    );
}
