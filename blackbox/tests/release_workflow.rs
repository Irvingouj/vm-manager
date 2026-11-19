//! The release workflow (CONTRACT.md, "The first release: The workflow"),
//! as far as its file shows it, read as YAML: `.github/workflows/` holds
//! `release.yml` and no other file; it starts on a pushed tag
//! `v<digits>.<digits>.<digits>` and by hand, on nothing else; its
//! permissions are `contents: read`, and only the job that publishes has
//! `contents: write` and `packages: write`; it uses no secret but the run's
//! token; every action is GitHub's own, named by a full commit id; it calls
//! each script of "The scripts that make it" and the two gates; the tool
//! is built on Linux and on macOS; and the jobs that publish run only for a
//! tag and need every job that checks.
//!
//! What a run does (that a rehearsal publishes nothing, that the image
//! pushed is the one the install tests passed against, the order of
//! publishing, the glibc ceiling) only a run shows: a rehearsal by hand.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::{BTreeMap, BTreeSet};

use blackbox::release::repository_root;
use serde_json::{Map, Value};
use yaml_rust2::{Yaml, YamlLoader};

const WORKFLOW: &str = ".github/workflows/release.yml";

/// The scripts the workflow calls.
const SCRIPTS: [&str; 6] = [
    "scripts/release-check.sh",
    "scripts/try-tool.sh",
    "scripts/gate.sh",
    "scripts/install-gate.sh",
    "scripts/package-tool.sh",
    "scripts/release-files.sh",
];

/// A YAML value as JSON: keys as text, scalars as themselves.
fn to_json(yaml: &Yaml) -> Value {
    match yaml {
        Yaml::Real(text) => text
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| Value::String(text.clone()), Value::Number),
        Yaml::Integer(n) => Value::from(*n),
        Yaml::String(text) => Value::String(text.clone()),
        Yaml::Boolean(b) => Value::Bool(*b),
        Yaml::Array(items) => Value::Array(items.iter().map(to_json).collect()),
        Yaml::Hash(hash) => {
            let mut map = Map::new();
            for (key, value) in hash {
                let key = match key {
                    Yaml::String(text) | Yaml::Real(text) => text.clone(),
                    Yaml::Integer(n) => n.to_string(),
                    Yaml::Boolean(b) => b.to_string(),
                    Yaml::Null => "null".to_owned(),
                    other => panic!("the harness reads no YAML key like {other:?}"),
                };
                map.insert(key, to_json(value));
            }
            Value::Object(map)
        }
        Yaml::Null => Value::Null,
        Yaml::Alias(_) | Yaml::BadValue => panic!("the harness reads no YAML value like {yaml:?}"),
    }
}

/// The workflow file read as YAML, failing the test when it is not there
/// or not one YAML document.
fn workflow() -> Value {
    let path = repository_root().join(WORKFLOW);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("expected {WORKFLOW} ({}): {e}", path.display()));
    let docs = YamlLoader::load_from_str(&text)
        .unwrap_or_else(|e| panic!("expected {WORKFLOW} to be YAML: {e}"));
    assert_eq!(docs.len(), 1, "expected {WORKFLOW} to be one YAML document");
    let value = to_json(&docs[0]);
    assert!(
        value.is_object(),
        "expected {WORKFLOW} to be a mapping, got {value}"
    );
    value
}

/// The jobs, by their id.
fn jobs(workflow: &Value) -> &Map<String, Value> {
    workflow["jobs"]
        .as_object()
        .unwrap_or_else(|| panic!("expected `jobs:` in {WORKFLOW} to be a mapping"))
}

/// Every text of `value`: its strings, and the keys of its mappings.
fn texts(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::String(text) => found.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|i| texts(i, found)),
        Value::Object(map) => {
            for (key, value) in map {
                found.push(key.clone());
                texts(value, found);
            }
        }
        _ => {}
    }
}

/// The `run:` of every step of `job`, one after another.
fn runs_of(job: &Value) -> String {
    job["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s["run"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The scopes a job (or the workflow) may write to, from its
/// `permissions:` (`write-all` is every scope).
fn writes(permissions: &Value) -> BTreeSet<String> {
    match permissions {
        Value::String(all) if all == "write-all" => ["*".to_owned()].into(),
        Value::Object(map) => map
            .iter()
            .filter(|(_, v)| v.as_str() == Some("write"))
            .map(|(k, _)| k.clone())
            .collect(),
        _ => BTreeSet::new(),
    }
}

/// The jobs that publish: those that may write, and those whose steps push
/// an image or make a release.
fn publishing_jobs(workflow: &Value) -> BTreeSet<String> {
    jobs(workflow)
        .iter()
        .filter(|(_, job)| {
            let runs = runs_of(job);
            !writes(&job["permissions"]).is_empty()
                || runs.contains("docker push")
                || runs.contains("--push")
                || runs.contains("gh release")
                || runs.contains("/releases")
        })
        .map(|(id, _)| id.clone())
        .collect()
}

/// The ids a job needs (`needs:` as a string or a list).
fn needs_of(job: &Value) -> Vec<String> {
    match &job["needs"] {
        Value::String(one) => vec![one.clone()],
        Value::Array(many) => many
            .iter()
            .filter_map(|n| n.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

#[test]
fn github_workflows_holds_release_yml_and_no_other_file() {
    let dir = repository_root().join(".github").join("workflows");
    let names = if dir.is_dir() {
        blackbox::release::names_in(&dir)
    } else {
        Vec::new()
    };
    assert_eq!(
        names,
        vec!["release.yml".to_owned()],
        "expected .github/workflows/ to hold release.yml and no other workflow"
    );
    // And none elsewhere that GitHub would run: the folder is the only
    // place it reads workflows from.
    workflow();
}

#[test]
fn the_workflow_starts_on_a_pushed_version_tag_and_by_hand_and_on_nothing_else() {
    let workflow = workflow();
    let on = workflow
        .get("on")
        .unwrap_or_else(|| panic!("expected `on:` in {WORKFLOW}"));
    let events = on.as_object().unwrap_or_else(|| {
        panic!("expected `on:` to be a mapping of `push:` (with its tags) and `workflow_dispatch:`, got {on}")
    });
    let names: BTreeSet<&str> = events.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        BTreeSet::from(["push", "workflow_dispatch"]),
        "expected the workflow to start on a pushed tag and by hand, and on nothing else"
    );
    let push = events["push"].as_object().unwrap_or_else(|| {
        panic!(
            "expected `push:` to be limited to tags, got {}",
            events["push"]
        )
    });
    let filters: BTreeSet<&str> = push.keys().map(String::as_str).collect();
    assert_eq!(
        filters,
        BTreeSet::from(["tags"]),
        "expected `push:` to be filtered by `tags:` only (no branch is pushed to start it)"
    );
    let patterns: Vec<String> = match &push["tags"] {
        Value::String(one) => vec![one.clone()],
        Value::Array(many) => many
            .iter()
            .map(|p| p.as_str().unwrap().to_owned())
            .collect(),
        other => panic!("expected `tags:` to be a pattern or a list of them, got {other}"),
    };
    for tag in ["v0.1.0", "v1.2.3", "v10.20.300"] {
        assert!(
            filter_matches(&patterns, tag),
            "expected the tag filter {patterns:?} to let the version tag {tag} start the workflow"
        );
    }
    for tag in [
        "v0.1",
        "v1",
        "0.1.0",
        "v0.1.0-rc.1",
        "v0.1.0.1",
        "va.b.c",
        "v.1.0",
        "release-v0.1.0",
        "v0.1.0/x",
        "main",
    ] {
        assert!(
            !filter_matches(&patterns, tag),
            "expected the tag filter {patterns:?} not to let {tag:?}, which is not v<digits>.<digits>.<digits>, start the workflow"
        );
    }
    match &events["workflow_dispatch"] {
        Value::Null | Value::Object(_) => {}
        other => panic!("expected `workflow_dispatch:` empty or a mapping, got {other}"),
    }
}

#[test]
fn the_workflow_may_read_its_contents_and_only_the_job_that_publishes_may_write_them_and_packages()
{
    let workflow = workflow();
    let top = workflow
        .get("permissions")
        .unwrap_or_else(|| panic!("expected `permissions:` at the top of {WORKFLOW}"));
    assert_eq!(
        top,
        &serde_json::json!({ "contents": "read" }),
        "expected the workflow's permissions to be `contents: read`"
    );
    let writing: Vec<(&String, BTreeSet<String>)> = jobs(&workflow)
        .iter()
        .map(|(id, job)| (id, writes(&job["permissions"])))
        .filter(|(_, w)| !w.is_empty())
        .collect();
    assert_eq!(
        writing.len(),
        1,
        "expected exactly one job (the one that publishes) to have a write permission, found {writing:?}"
    );
    assert_eq!(
        writing[0].1,
        BTreeSet::from(["contents".to_owned(), "packages".to_owned()]),
        "expected the job that publishes, {}, to write `contents` and `packages` and nothing else",
        writing[0].0
    );
}

#[test]
fn the_workflow_uses_no_secret_but_the_runs_own_token() {
    let workflow = workflow();
    let mut all = Vec::new();
    texts(&workflow, &mut all);
    let mut other = Vec::new();
    for text in &all {
        let squeezed: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        for (at, _) in squeezed.match_indices("secrets") {
            let (_, tail) = squeezed.split_at(at + "secrets".len());
            if tail.starts_with('.') || tail.starts_with('[') {
                let name: String = tail
                    .trim_start_matches(['.', '[', '\''])
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if name != "GITHUB_TOKEN" {
                    other.push(text.clone());
                }
            }
        }
    }
    assert!(
        other.is_empty(),
        "expected no secret but the run's own token (secrets.GITHUB_TOKEN or github.token), found: {other:?}"
    );
    for (id, job) in jobs(&workflow) {
        assert!(
            job.get("secrets").is_none(),
            "expected the job {id} to pass on no secrets"
        );
    }
}

/// Whether `uses` names one of GitHub's own actions by a full commit id.
fn githubs_own_by_commit(uses: &str) -> bool {
    let Some((name, reference)) = uses.split_once('@') else {
        return false;
    };
    let mut parts = name.split('/');
    parts.next() == Some("actions")
        && {
            let rest: Vec<&str> = parts.collect();
            !rest.is_empty()
                && rest.iter().all(|p| {
                    !p.is_empty()
                        && p.chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                })
        }
        && reference.len() == 40
        && reference
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

#[test]
fn every_action_the_workflow_uses_is_githubs_own_named_by_a_full_commit_id() {
    let workflow = workflow();
    let mut used = 0;
    for (id, job) in jobs(&workflow) {
        assert!(
            job.get("uses").is_none(),
            "expected the job {id} to call no other workflow (`uses:` at a job's level)"
        );
        for step in job["steps"].as_array().into_iter().flatten() {
            if let Some(uses) = step.get("uses") {
                let uses = uses.as_str().unwrap_or_default();
                used += 1;
                assert!(
                    githubs_own_by_commit(uses),
                    "expected every action to be one of GitHub's own named by a full commit id (`actions/<name>@<40 hex digits>`), the job {id} uses {uses:?}"
                );
            }
        }
    }
    assert!(
        used > 0,
        "expected the workflow to use actions/checkout at least"
    );
}

#[test]
fn the_workflow_calls_the_release_check_both_gates_the_packing_and_the_release_files() {
    let workflow = workflow();
    let runs: String = jobs(&workflow)
        .values()
        .map(runs_of)
        .collect::<Vec<_>>()
        .join("\n");
    for script in SCRIPTS {
        assert!(
            runs.contains(script),
            "expected a step of the workflow to run {script}"
        );
    }
}

#[test]
fn the_workflow_builds_the_tool_on_linux_and_on_macos() {
    let workflow = workflow();
    let mut runners = Vec::new();
    for job in jobs(&workflow).values() {
        texts(&job["runs-on"], &mut runners);
        texts(&job["strategy"]["matrix"], &mut runners);
    }
    for system in ["ubuntu", "macos"] {
        assert!(
            runners.iter().any(|r| r.contains(system)),
            "expected a job of the workflow on a hosted {system} runner, the runners named are {runners:?}"
        );
    }
}

#[test]
fn the_jobs_that_pack_the_tool_check_a_download_with_the_pages_command_of_each_system() {
    let workflow = workflow();
    let packing: Vec<(&String, String)> = jobs(&workflow)
        .iter()
        .map(|(id, job)| (id, runs_of(job)))
        .filter(|(_, runs)| runs.contains("scripts/package-tool.sh"))
        .collect();
    assert!(
        !packing.is_empty(),
        "expected a job that packs the tool with scripts/package-tool.sh"
    );
    for (command, system) in [("sha256sum", "Linux"), ("shasum -a 256", "macOS")] {
        let found = packing.iter().any(|(_, runs)| {
            runs.lines().any(|line| {
                line.contains(command)
                    && line.contains("--check")
                    && line.contains("--ignore-missing")
            })
        });
        assert!(
            found,
            "expected a step of the job(s) that pack the tool ({:?}) to run the page's check for {system}, `{command} --check --ignore-missing SHA256SUMS`",
            packing.iter().map(|(id, _)| id).collect::<Vec<_>>()
        );
    }
}

#[test]
fn the_jobs_that_publish_run_only_for_a_tag_and_need_every_job_that_checks() {
    let workflow = workflow();
    let jobs = jobs(&workflow);
    let publishing = publishing_jobs(&workflow);
    assert!(
        !publishing.is_empty(),
        "expected a job that publishes (pushes the image, makes the release)"
    );
    for id in &publishing {
        let job = &jobs[id];
        let condition = job["if"].as_str().map(str::to_owned).unwrap_or_else(|| {
            panic!(
                "expected the job {id}, which publishes, to have an `if:` that holds only for a tag"
            )
        });
        for (context, publishes) in [
            (Run::pushed_tag(), true),
            (Run::by_hand("refs/tags/v0.1.0", "tag", "v0.1.0"), true),
            (Run::by_hand("refs/heads/main", "branch", "main"), false),
            (Run::by_hand("refs/heads/v0.1.0", "branch", "v0.1.0"), false),
        ] {
            let decided = evaluate_if(&condition, &context).unwrap_or_else(|problem| {
                panic!("the harness cannot evaluate the `if:` of {id}, {condition:?}: {problem}")
            });
            assert_eq!(
                decided,
                publishes,
                "expected the job {id} (`if: {condition}`) {} for a run {}",
                if publishes { "to run" } else { "not to run" },
                context.describe()
            );
        }

        // Every job that does not publish is needed by it, directly or not.
        let mut needed = BTreeSet::new();
        let mut next: Vec<String> = needs_of(job);
        while let Some(need) = next.pop() {
            if needed.insert(need.clone())
                && let Some(job) = jobs.get(&need)
            {
                next.extend(needs_of(job));
            }
        }
        let missing: Vec<&String> = jobs
            .keys()
            .filter(|other| !publishing.contains(*other) && !needed.contains(*other))
            .collect();
        assert!(
            missing.is_empty(),
            "expected the job {id}, which publishes, to need every job that checks; it does not need {missing:?}"
        );
    }
}

// ---- GitHub's filter patterns, for the tag filter ----

/// One piece of a filter pattern.
#[derive(Debug, Clone)]
enum Piece {
    Char(char),
    Class(Vec<(char, char)>),
    /// `*`: any characters but `/`.
    Star,
    /// `**`: any characters.
    DoubleStar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Times {
    One,
    /// `+`: one or more of what comes before.
    OneOrMore,
    /// `?`: zero or one of what comes before.
    ZeroOrOne,
}

/// A pattern of GitHub's filter syntax (`*`, `**`, `?`, `+`, `[...]`, `\`)
/// read into its pieces.
fn pieces_of(pattern: &str) -> Vec<(Piece, Times)> {
    let mut pieces: Vec<(Piece, Times)> = Vec::new();
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    pieces.push((Piece::Char(next), Times::One));
                }
            }
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next();
                    pieces.push((Piece::DoubleStar, Times::One));
                } else {
                    pieces.push((Piece::Star, Times::One));
                }
            }
            '+' | '?' => {
                let times = if c == '+' {
                    Times::OneOrMore
                } else {
                    Times::ZeroOrOne
                };
                match pieces.last_mut() {
                    Some(last) => last.1 = times,
                    None => pieces.push((Piece::Char(c), Times::One)),
                }
            }
            '[' => {
                let mut ranges = Vec::new();
                let mut class: Vec<char> = Vec::new();
                for inner in chars.by_ref() {
                    if inner == ']' {
                        break;
                    }
                    class.push(inner);
                }
                let mut at = 0;
                while at < class.len() {
                    if at + 2 < class.len() && class[at + 1] == '-' {
                        ranges.push((class[at], class[at + 2]));
                        at += 3;
                    } else {
                        ranges.push((class[at], class[at]));
                        at += 1;
                    }
                }
                pieces.push((Piece::Class(ranges), Times::One));
            }
            other => pieces.push((Piece::Char(other), Times::One)),
        }
    }
    pieces
}

/// Whether `pieces` match the whole of `text`.
fn pieces_match(pieces: &[(Piece, Times)], text: &[char]) -> bool {
    let Some(((piece, times), rest)) = pieces.split_first() else {
        return text.is_empty();
    };
    let one = |c: char| match piece {
        Piece::Char(want) => c == *want,
        Piece::Class(ranges) => ranges.iter().any(|(a, b)| (*a..=*b).contains(&c)),
        Piece::Star => c != '/',
        Piece::DoubleStar => true,
    };
    match piece {
        Piece::Star | Piece::DoubleStar => {
            // Any number of characters it takes, the fewest first.
            for taken in 0..=text.len() {
                if taken > 0 && !one(text[taken - 1]) {
                    return false;
                }
                if pieces_match(rest, &text[taken..]) {
                    return true;
                }
            }
            false
        }
        _ => {
            let (least, most) = match times {
                Times::One => (1, 1),
                Times::OneOrMore => (1, usize::MAX),
                Times::ZeroOrOne => (0, 1),
            };
            let mut taken = 0;
            while taken < most && taken < text.len() && one(text[taken]) {
                taken += 1;
            }
            (least..=taken)
                .rev()
                .any(|n| pieces_match(rest, &text[n..]))
        }
    }
}

/// Whether a tag passes a list of GitHub filter patterns: the last pattern
/// that matches decides, a `!` pattern excluding.
fn filter_matches(patterns: &[String], tag: &str) -> bool {
    let text: Vec<char> = tag.chars().collect();
    let mut passes = false;
    for pattern in patterns {
        let (negated, pattern) = match pattern.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, pattern.as_str()),
        };
        if pieces_match(&pieces_of(pattern), &text) {
            passes = !negated;
        }
    }
    passes
}

/// The harness's own check of [`filter_matches`], against the examples of
/// GitHub's documentation of filter patterns.
#[test]
fn the_harness_reads_githubs_filter_patterns() {
    let one = |p: &str, t: &str| filter_matches(&[p.to_owned()], t);
    assert!(one("v[0-9]+.[0-9]+.[0-9]+", "v10.1.3"));
    assert!(!one("v[0-9]+.[0-9]+.[0-9]+", "v10.1.3-rc"));
    assert!(one("v2*", "v2.0.1"));
    assert!(!one("v2*", "v2/x"));
    assert!(one("v2**", "v2/x"));
    assert!(one("v*.*.*", "va.b.c"));
    assert!(one("v1.?", "v1."));
    assert!(one("v1.?", "v1") && one("v1.?", "v1.") && !one("v1.?", "v1.0"));
    assert!(one("v[12].[0-9]+.[0-9]+", "v1.10.1"));
    assert!(!one("v[12].[0-9]+.[0-9]+", "v3.1.1"));
    assert!(!filter_matches(
        &["v*".to_owned(), "!v*-rc".to_owned()],
        "v1-rc"
    ));
    assert!(filter_matches(
        &["v*".to_owned(), "!v*-rc".to_owned()],
        "v1"
    ));
}

// ---- GitHub's expressions, for a job's `if:` ----

/// What a run is, as an `if:` sees it.
struct Run {
    values: BTreeMap<&'static str, String>,
}

impl Run {
    fn pushed_tag() -> Self {
        Self {
            values: BTreeMap::from([
                ("github.event_name", "push".to_owned()),
                ("github.ref", "refs/tags/v0.1.0".to_owned()),
                ("github.ref_type", "tag".to_owned()),
                ("github.ref_name", "v0.1.0".to_owned()),
                ("github.event.ref", "refs/tags/v0.1.0".to_owned()),
            ]),
        }
    }

    fn by_hand(reference: &str, kind: &str, name: &str) -> Self {
        Self {
            values: BTreeMap::from([
                ("github.event_name", "workflow_dispatch".to_owned()),
                ("github.ref", reference.to_owned()),
                ("github.ref_type", kind.to_owned()),
                ("github.ref_name", name.to_owned()),
                ("github.event.ref", reference.to_owned()),
            ]),
        }
    }

    fn describe(&self) -> String {
        format!("{:?}", self.values)
    }
}

/// A value of an expression.
#[derive(Debug, Clone, PartialEq)]
enum Val {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
}

impl Val {
    fn truthy(&self) -> bool {
        match self {
            Val::Null => false,
            Val::Bool(b) => *b,
            Val::Num(n) => *n != 0.0 && !n.is_nan(),
            Val::Str(s) => !s.is_empty(),
        }
    }

    fn number(&self) -> f64 {
        match self {
            Val::Null => 0.0,
            Val::Bool(b) => f64::from(u8::from(*b)),
            Val::Num(n) => *n,
            Val::Str(s) if s.trim().is_empty() => 0.0,
            Val::Str(s) => s.trim().parse().unwrap_or(f64::NAN),
        }
    }

    fn text(&self) -> String {
        match self {
            Val::Null => String::new(),
            Val::Bool(b) => b.to_string(),
            Val::Num(n) => n.to_string(),
            Val::Str(s) => s.clone(),
        }
    }

    fn equals(&self, other: &Val) -> bool {
        match (self, other) {
            (Val::Str(a), Val::Str(b)) => a.eq_ignore_ascii_case(b),
            (Val::Null, Val::Null) => true,
            (Val::Bool(a), Val::Bool(b)) => a == b,
            _ => self.number() == other.number(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Str(String),
    Num(f64),
    Word(String),
    Op(&'static str),
}

fn tokens_of(text: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        if c.is_whitespace() {
            at += 1;
        } else if c == '\'' {
            let mut s = String::new();
            at += 1;
            loop {
                match chars.get(at) {
                    None => return Err("a string is not closed".to_owned()),
                    Some('\'') if chars.get(at + 1) == Some(&'\'') => {
                        s.push('\'');
                        at += 2;
                    }
                    Some('\'') => {
                        at += 1;
                        break;
                    }
                    Some(other) => {
                        s.push(*other);
                        at += 1;
                    }
                }
            }
            tokens.push(Token::Str(s));
        } else if c.is_ascii_digit() {
            let start = at;
            while at < chars.len() && (chars[at].is_ascii_digit() || chars[at] == '.') {
                at += 1;
            }
            let s: String = chars[start..at].iter().collect();
            tokens.push(Token::Num(
                s.parse().map_err(|_| format!("a number {s:?}"))?,
            ));
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = at;
            while at < chars.len()
                && (chars[at].is_ascii_alphanumeric() || matches!(chars[at], '_' | '-' | '.'))
            {
                at += 1;
            }
            tokens.push(Token::Word(chars[start..at].iter().collect()));
        } else {
            let two: String = chars[at..(at + 2).min(chars.len())].iter().collect();
            let op = ["==", "!=", "<=", ">=", "&&", "||"]
                .into_iter()
                .find(|o| *o == two);
            if let Some(op) = op {
                tokens.push(Token::Op(op));
                at += 2;
            } else {
                let op = ["(", ")", ",", "!", "<", ">"]
                    .into_iter()
                    .find(|o| o.starts_with(c))
                    .ok_or_else(|| format!("an operator {c:?}"))?;
                tokens.push(Token::Op(op));
                at += 1;
            }
        }
    }
    Ok(tokens)
}

/// A reader of an expression's tokens that evaluates as it reads.
struct Reader<'a> {
    tokens: Vec<Token>,
    at: usize,
    run: &'a Run,
    /// A status function other than `success()` was called.
    status: Option<String>,
    /// `success()` was called.
    success: bool,
}

impl Reader<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn eat(&mut self, op: &str) -> bool {
        if matches!(self.peek(), Some(Token::Op(o)) if *o == op) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn or(&mut self) -> Result<Val, String> {
        let mut left = self.and()?;
        while self.eat("||") {
            let right = self.and()?;
            left = if left.truthy() { left } else { right };
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Val, String> {
        let mut left = self.compare()?;
        while self.eat("&&") {
            let right = self.compare()?;
            left = if left.truthy() { right } else { left };
        }
        Ok(left)
    }

    fn compare(&mut self) -> Result<Val, String> {
        let left = self.unary()?;
        if let Some(Token::Op(op)) = self.peek().cloned()
            && ["==", "!=", "<", ">", "<=", ">="].contains(&op)
        {
            self.at += 1;
            let right = self.unary()?;
            let (a, b) = (left.number(), right.number());
            return Ok(Val::Bool(match op {
                "==" => left.equals(&right),
                "!=" => !left.equals(&right),
                "<" => a < b,
                ">" => a > b,
                "<=" => a <= b,
                _ => a >= b,
            }));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Val, String> {
        if self.eat("!") {
            return Ok(Val::Bool(!self.unary()?.truthy()));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Val, String> {
        let token = self
            .peek()
            .cloned()
            .ok_or_else(|| "the expression ends too soon".to_owned())?;
        self.at += 1;
        match token {
            Token::Str(s) => Ok(Val::Str(s)),
            Token::Num(n) => Ok(Val::Num(n)),
            Token::Op("(") => {
                let value = self.or()?;
                if !self.eat(")") {
                    return Err("a `(` is not closed".to_owned());
                }
                Ok(value)
            }
            Token::Op(op) => Err(format!("{op:?} where a value was expected")),
            Token::Word(word) => {
                if self.eat("(") {
                    let mut args = Vec::new();
                    if !self.eat(")") {
                        loop {
                            args.push(self.or()?);
                            if self.eat(")") {
                                break;
                            }
                            if !self.eat(",") {
                                return Err(format!("the arguments of {word}()"));
                            }
                        }
                    }
                    return self.call(&word, &args);
                }
                match word.as_str() {
                    "true" => Ok(Val::Bool(true)),
                    "false" => Ok(Val::Bool(false)),
                    "null" => Ok(Val::Null),
                    path if path.starts_with("github.") => Ok(self
                        .run
                        .values
                        .get(path)
                        .map_or(Val::Null, |v| Val::Str(v.clone()))),
                    path if path.starts_with("inputs.") || path.starts_with("needs.") => {
                        Ok(Val::Str(String::new()))
                    }
                    other => Err(format!("the harness knows no value {other:?}")),
                }
            }
        }
    }

    fn call(&mut self, name: &str, args: &[Val]) -> Result<Val, String> {
        let text = |at: usize| {
            args.get(at)
                .map(Val::text)
                .unwrap_or_default()
                .to_lowercase()
        };
        Ok(match name.to_ascii_lowercase().as_str() {
            "startswith" => Val::Bool(text(0).starts_with(&text(1))),
            "endswith" => Val::Bool(text(0).ends_with(&text(1))),
            "contains" => Val::Bool(text(0).contains(&text(1))),
            "success" => {
                self.success = true;
                Val::Bool(true)
            }
            "always" | "failure" | "cancelled" => {
                self.status = Some(format!("{name}()"));
                Val::Bool(name == "always")
            }
            other => return Err(format!("the harness knows no function {other}()")),
        })
    }
}

/// Whether a job with this `if:` runs in `run` when every job it needs has
/// passed. A status function other than `success()` is refused: with one
/// the job could run when a job it needs failed.
fn evaluate_if(condition: &str, run: &Run) -> Result<bool, String> {
    let trimmed = condition.trim();
    let inner = trimmed
        .strip_prefix("${{")
        .and_then(|t| t.strip_suffix("}}"))
        .unwrap_or(trimmed);
    let mut reader = Reader {
        tokens: tokens_of(inner)?,
        at: 0,
        run,
        status: None,
        success: false,
    };
    let value = reader.or()?;
    if reader.at != reader.tokens.len() {
        return Err(format!("left unread: {:?}", &reader.tokens[reader.at..]));
    }
    if let Some(status) = reader.status {
        return Err(format!(
            "it calls {status}, with which the job could run although a job it needs failed"
        ));
    }
    Ok(value.truthy())
}

/// The harness's own check of [`evaluate_if`].
#[test]
fn the_harness_evaluates_a_jobs_if() {
    let tag = Run::pushed_tag();
    let main = Run::by_hand("refs/heads/main", "branch", "main");
    for (condition, on_tag, on_main) in [
        ("startsWith(github.ref, 'refs/tags/v')", true, false),
        ("${{ github.ref_type == 'tag' }}", true, false),
        (
            "github.event_name == 'push' || startsWith(github.ref, 'refs/tags/')",
            true,
            false,
        ),
        ("!startsWith(github.ref, 'refs/tags/')", false, true),
        ("github.ref_name != ''", true, true),
        ("(github.ref_type == 'tag') && success()", true, false),
    ] {
        assert_eq!(evaluate_if(condition, &tag), Ok(on_tag), "{condition}");
        assert_eq!(evaluate_if(condition, &main), Ok(on_main), "{condition}");
    }
    assert!(evaluate_if("always() && github.ref_type == 'tag'", &tag).is_err());
    assert!(evaluate_if("!cancelled() && github.ref_type == 'tag'", &tag).is_err());
}
