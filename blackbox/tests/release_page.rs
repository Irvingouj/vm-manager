//! The page (CONTRACT.md, "The first release: The page and the notes"):
//! `docs/getting-started.md` exists and the repository's `README.md` leads
//! to it; it shows each step from nothing to an Application opened in the
//! browser by its commands, with the release's published files; every
//! command line of `vibe` in its code is one the real program has (the
//! command, and each option shown in that command's `--help`); and it
//! says how macOS's quarantine mark is removed.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::collections::BTreeMap;
use std::process::Command;

use blackbox::program::vm_binary;
use blackbox::project::{code_in, help_commands};
use blackbox::release::{COMMAND_TIMEOUT, TARGETS, VERSION, read_repository_file, run};

const PAGE: &str = "docs/getting-started.md";

fn page() -> String {
    read_repository_file(PAGE)
}

/// The fenced code blocks of a Markdown text.
fn fenced_blocks(markdown: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut fenced: Option<String> = None;
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            match fenced.take() {
                Some(block) => found.push(block),
                None => fenced = Some(String::new()),
            }
            continue;
        }
        if let Some(block) = fenced.as_mut() {
            block.push_str(line);
            block.push('\n');
        }
    }
    found
}

/// The words of one shell command line, split at blanks outside quotes,
/// the quotes taken off; a `#` beginning a word begins a comment.
fn words_of(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in line.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                started = true;
            }
            None if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            None if c == '#' && !started => break,
            None => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// Whether a word starts the program: `vibe`, or a path to it.
fn is_vibe(word: &str) -> bool {
    word == "vibe" || word.ends_with("/vibe")
}

/// The arguments of each `vibe` command line in the code blocks of
/// `markdown`: a command that is `vibe` (or a path to it, after variables
/// and `sudo`), and `docker compose exec <service> vibe ...`. Lines ending
/// in `\` go on; `$ ` and `% ` prompts are taken off; `&&`, `||`, `|` and
/// `;` separate commands. A line whose first argument is a version is the
/// output of `vibe --version`, not a command.
fn vibe_command_lines(markdown: &str) -> Vec<Vec<String>> {
    let mut found = Vec::new();
    for block in fenced_blocks(markdown) {
        let joined = block.replace("\\\n", " ");
        for line in joined.lines() {
            let line = line.trim();
            let line = line
                .strip_prefix("$ ")
                .or_else(|| line.strip_prefix("% "))
                .unwrap_or(line);
            let words = words_of(line);
            let mut commands: Vec<Vec<String>> = vec![Vec::new()];
            for word in words {
                if ["&&", "||", "|", ";"].contains(&word.as_str()) {
                    commands.push(Vec::new());
                } else if let Some(stripped) = word.strip_suffix(';') {
                    commands.last_mut().unwrap().push(stripped.to_owned());
                    commands.push(Vec::new());
                } else {
                    commands.last_mut().unwrap().push(word);
                }
            }
            for command in commands {
                let start = command
                    .iter()
                    .position(|w| !(w.contains('=') && !w.starts_with('-')) && w != "sudo")
                    .unwrap_or(command.len());
                let command = &command[start..];
                let args = if command.first().is_some_and(|w| is_vibe(w)) {
                    Some(command[1..].to_vec())
                } else if command.first().is_some_and(|w| w == "docker")
                    && let Some(exec) = command.iter().position(|w| w == "exec")
                {
                    let mut rest = command[exec + 1..]
                        .iter()
                        .skip_while(|w| w.starts_with('-'));
                    rest.next(); // the service
                    let rest: Vec<String> = rest.cloned().collect();
                    if rest.first().is_some_and(|w| is_vibe(w)) {
                        Some(rest[1..].to_vec())
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some(args) = args {
                    let is_output = args
                        .first()
                        .is_some_and(|a| a.starts_with(|c: char| c.is_ascii_digit()));
                    if !is_output {
                        found.push(args);
                    }
                }
            }
        }
    }
    found
}

/// `vibe <path> --help` of the real program, asked once per path.
struct Helps(BTreeMap<Vec<String>, Option<String>>);

impl Helps {
    fn of(&mut self, path: &[String]) -> Option<String> {
        self.0
            .entry(path.to_vec())
            .or_insert_with(|| {
                let mut command = Command::new(vm_binary());
                command.args(path).arg("--help");
                let ran = run(command, COMMAND_TIMEOUT);
                ran.ok().then_some(ran.stdout)
            })
            .clone()
    }
}

/// Whether `help` lists the option `option` (`--name` or `-x`).
fn help_lists(help: &str, option: &str) -> bool {
    help.match_indices(option).any(|(at, _)| {
        let (head, tail) = help.split_at(at);
        let before = head.chars().next_back();
        let after = tail.chars().nth(option.chars().count());
        before.is_none_or(|c| c.is_whitespace() || c == ',' || c == '[')
            && after.is_none_or(|c| c.is_whitespace() || matches!(c, ',' | '=' | '[' | '<' | ']'))
    })
}

/// What is wrong with one `vibe` command line, by the real program's help:
/// a command it does not have, or an option the command's help does not
/// list.
fn problems_of(args: &[String], helps: &mut Helps) -> Vec<String> {
    let shown = format!("vibe {}", args.join(" "));
    let mut path: Vec<String> = Vec::new();
    let mut at = 0;
    while let Some(word) = args.get(at) {
        if word.starts_with('-') {
            break;
        }
        let Some(help) = helps.of(&path) else {
            return vec![format!("{shown}: `vibe {} --help` fails", path.join(" "))];
        };
        if help_commands(&help).contains(word) {
            path.push(word.clone());
            at += 1;
        } else {
            break;
        }
    }
    let mut problems = Vec::new();
    if path.is_empty() && args.first().is_some_and(|a| !a.starts_with('-')) {
        problems.push(format!("{shown}: the program has no command {:?}", args[0]));
        return problems;
    }
    let Some(help) = helps.of(&path) else {
        return vec![format!("{shown}: `vibe {} --help` fails", path.join(" "))];
    };
    for word in &args[at..] {
        if word == "--" {
            break;
        }
        if !word.starts_with('-') || word.len() < 2 {
            continue;
        }
        let option = word.split('=').next().unwrap_or(word);
        if ["--help", "-h", "--version", "-V"].contains(&option) {
            continue;
        }
        if !help_lists(&help, option) {
            problems.push(format!(
                "{shown}: `vibe {} --help` lists no option {option}",
                path.join(" ")
            ));
        }
    }
    problems
}

#[test]
fn the_getting_started_page_exists_and_the_readme_leads_to_it() {
    let text = page();
    assert!(!text.trim().is_empty(), "expected {PAGE} not to be empty");
    let readme = read_repository_file("README.md");
    let linked = readme.match_indices("](").any(|(at, _)| {
        let (_, tail) = readme.split_at(at + 2);
        let target: String = tail.chars().take_while(|c| *c != ')').collect();
        let target = target.split('#').next().unwrap_or_default();
        target.trim_start_matches("./") == PAGE
    });
    assert!(
        linked,
        "expected the repository's README.md to link to {PAGE} (`[...]({PAGE})`)"
    );
}

#[test]
fn the_page_shows_each_step_from_compose_to_sharing_by_its_commands() {
    let text = page();
    let code = code_in(&text).join("\n");
    for command in [
        "docker compose",
        "vibe setup",
        "vibe admin connect-oidc",
        "vibe sign-in",
        "vibe new",
        "vibe dev",
        "vibe deploy",
        "vibe share",
    ] {
        assert!(
            code.contains(command),
            "expected the page to show `{command}` in its code"
        );
    }
    let release = format!("/releases/download/v{VERSION}/");
    let mut files = vec![
        "compose.yaml".to_owned(),
        "env.example".to_owned(),
        "SHA256SUMS".to_owned(),
    ];
    files.extend(TARGETS.iter().map(|t| format!("vibe-{VERSION}-{t}.tar.gz")));
    for file in &files {
        assert!(
            code.contains(&format!("{release}{file}")),
            "expected the page to download {file} from the release (`...{release}{file}`)"
        );
    }
    let lines: Vec<&str> = code.lines().collect();
    for (check, what) in [("sha256sum", "Linux"), ("shasum -a 256", "macOS")] {
        assert!(
            lines
                .iter()
                .any(|l| l.contains(check) && l.contains("--check") && l.contains("SHA256SUMS")),
            "expected the page to check the download against SHA256SUMS on {what} (`{check} ... --check SHA256SUMS`)"
        );
    }
    assert!(
        lines.iter().any(|l| l.contains("tar ") && l.contains('x')),
        "expected the page to unpack the archive with tar"
    );
    assert!(
        text.contains("/callback"),
        "expected the page to give the redirect address to register at the identity provider (https://auth.<domain>/callback)"
    );
}

#[test]
fn every_vibe_command_line_of_the_page_is_one_the_program_has() {
    let text = page();
    let lines = vibe_command_lines(&text);
    assert!(
        lines.len() >= 7,
        "expected the page's code blocks to show the program's command lines, found {lines:?}"
    );
    let mut helps = Helps(BTreeMap::new());
    let problems: Vec<String> = lines
        .iter()
        .flat_map(|args| problems_of(args, &mut helps))
        .collect();
    assert!(
        problems.is_empty(),
        "expected every command line of vibe on the page to be one the program has:\n  {}",
        problems.join("\n  ")
    );
}

#[test]
fn the_page_says_how_macos_quarantine_mark_is_removed() {
    let text = page();
    assert!(
        code_in(&text)
            .iter()
            .any(|c| c.contains("xattr -d com.apple.quarantine")),
        "expected the page to show `xattr -d com.apple.quarantine vibe` for a program downloaded with a browser on macOS"
    );
}

/// The harness's own check of [`vibe_command_lines`] and [`problems_of`],
/// against the real program.
#[test]
fn the_harness_finds_vibe_command_lines_and_checks_them_against_the_program() {
    let markdown = "Text `vibe nothing` is not a block.\n\n```sh\n$ vibe sign-in --domain example.com\ndocker compose exec vibe vibe setup\nFOO=1 sudo ./vibe-0.1.0-x/vibe --version && vibe admin connect-oidc \\\n  --domain example.com # a comment --nothing\nvibe 0.1.0\ncurl -L -o vibe.tar.gz https://x/vibe\nvibe no-such-command\nvibe sign-in --no-such-option\n```\n";
    let lines = vibe_command_lines(markdown);
    let shown: Vec<String> = lines.iter().map(|a| a.join(" ")).collect();
    assert_eq!(
        shown,
        vec![
            "sign-in --domain example.com",
            "setup",
            "--version",
            "admin connect-oidc --domain example.com",
            "no-such-command",
            "sign-in --no-such-option",
        ]
    );
    let mut helps = Helps(BTreeMap::new());
    for good in &lines[..4] {
        assert_eq!(
            problems_of(good, &mut helps),
            Vec::<String>::new(),
            "{good:?}"
        );
    }
    for bad in &lines[4..] {
        assert!(!problems_of(bad, &mut helps).is_empty(), "{bad:?}");
    }
}
