//! What the tests of the tool's sign-in through the browser share
//! (CONTRACT.md, "The tool signs in through the browser, and OIDC is
//! connected from the tool: Signing the tool in"):
//!
//! - [`Challenge`]: a verifier and its challenge, made as the tool makes
//!   them, for tests that play the tool's part by hand;
//! - [`forms`] reads the forms of a page, and [`Form::submission`] fills
//!   the one of step 2 as a person would (confirm or decline, with a check
//!   code or none);
//! - [`BrowserCommand`] is the `BROWSER` command the harness gives the
//!   tool: a script that writes the address it is asked to open into a
//!   file of the machine's, for the test to open in its own browser;
//! - [`check_code_in`] and [`tool_address_in`] read what the tool wrote on
//!   standard error, and [`check_code_of`] makes a challenge's check code
//!   as the contract says.

use std::cell::Cell;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::{SecureRandom, SystemRandom};
use sha2::{Digest, Sha256};
use url::Url;

use crate::oidc::s256;
use crate::tool::Machine;

/// How long a test waits for the tool to open the browser.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(20);

/// A verifier (256 random bits in base64url) and its challenge
/// (`BASE64URL(SHA-256(verifier))`). The verifier is never printed.
#[derive(Clone)]
pub struct Challenge {
    verifier: String,
    pub challenge: String,
}

impl Challenge {
    pub fn fresh() -> Self {
        let mut bytes = [0_u8; 32];
        SystemRandom::new()
            .fill(&mut bytes)
            .unwrap_or_else(|_| panic!("the operating system gave no randomness"));
        let verifier = URL_SAFE_NO_PAD.encode(bytes);
        let challenge = s256(&verifier);
        Self {
            verifier,
            challenge,
        }
    }

    /// The verifier. Never print it.
    pub fn verifier(&self) -> &str {
        &self.verifier
    }
}

impl std::fmt::Debug for Challenge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Challenge {{ challenge: {:?}, verifier: <hidden> }}",
            self.challenge
        )
    }
}

/// One field of a form: an `<input>` or a `<button>`.
#[derive(Clone, Debug)]
pub struct Field {
    /// `input` or `button`.
    pub tag: String,
    /// The `type`, lower case: `text` for an input without one, `submit`
    /// for a button without one.
    pub kind: String,
    pub name: Option<String>,
    pub value: String,
    /// A button's text.
    pub label: String,
}

impl Field {
    fn is_submit(&self) -> bool {
        self.kind == "submit" || self.kind == "image"
    }

    fn is_typed(&self) -> bool {
        self.tag == "input"
            && matches!(
                self.kind.as_str(),
                "text" | "search" | "tel" | "number" | "password"
            )
    }

    fn words(&self) -> String {
        format!(
            " {} {} {} ",
            self.name.as_deref().unwrap_or_default(),
            self.value,
            self.label
        )
        .to_ascii_lowercase()
        .replace(|c: char| !c.is_ascii_alphanumeric(), " ")
    }

    /// Whether this submit button says no.
    pub fn declines(&self) -> bool {
        let words = self.words();
        [
            " decline ",
            " deny ",
            " cancel ",
            " no ",
            " not ",
            " reject ",
            " don ",
            " abort ",
        ]
        .iter()
        .any(|w| words.contains(w))
    }
}

/// How a person answers the page of step 2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Confirm,
    Decline,
}

/// A form of a page.
#[derive(Clone, Debug)]
pub struct Form {
    pub action: String,
    pub method: String,
    pub fields: Vec<Field>,
}

impl Form {
    /// The field into which a person types (the check code's), if any.
    pub fn typed_field(&self) -> Option<&Field> {
        self.fields.iter().find(|f| f.is_typed())
    }

    /// The `application/x-www-form-urlencoded` body a browser sends when
    /// the person answers so: every hidden input, the typed field with
    /// `code` (or as it is), and the button that says yes (the only one, or
    /// the one that does not say no) or no. `None` when the form has no
    /// such button.
    pub fn submission(&self, decision: Decision, code: Option<&str>) -> Option<String> {
        let submits: Vec<&Field> = self.fields.iter().filter(|f| f.is_submit()).collect();
        let button = match decision {
            Decision::Confirm => match submits.as_slice() {
                [one] if !one.declines() => Some(*one),
                many => many.iter().find(|f| !f.declines()).copied(),
            },
            Decision::Decline => submits.iter().find(|f| f.declines()).copied(),
        }?;
        let mut body = url::form_urlencoded::Serializer::new(String::new());
        for field in &self.fields {
            let Some(name) = &field.name else { continue };
            if field.is_submit() {
                continue;
            }
            if field.is_typed() {
                body.append_pair(name, code.unwrap_or(&field.value));
            } else if field.tag == "input"
                && !matches!(
                    field.kind.as_str(),
                    "checkbox" | "radio" | "button" | "reset"
                )
            {
                body.append_pair(name, &field.value);
            }
        }
        if let Some(name) = &button.name {
            body.append_pair(name, &button.value);
        }
        Some(body.finish())
    }
}

/// The forms of a page, in order.
pub fn forms(html: &str) -> Vec<Form> {
    let lower = html.to_ascii_lowercase();
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = lower.get(from..).and_then(|r| r.find("<form")) {
        let start = from + at;
        let tag_end = lower
            .get(start..)
            .and_then(|r| r.find('>'))
            .map_or(lower.len(), |e| start + e);
        let end = lower
            .get(tag_end..)
            .and_then(|r| r.find("</form"))
            .map_or(lower.len(), |e| tag_end + e);
        let tag = html.get(start..tag_end).unwrap_or_default();
        let inside = html.get(tag_end..end).unwrap_or_default();
        found.push(Form {
            action: attribute(tag, "action").unwrap_or_default(),
            method: attribute(tag, "method")
                .unwrap_or_else(|| "get".to_owned())
                .to_ascii_lowercase(),
            fields: fields(inside),
        });
        from = end.max(start + 1);
    }
    found
}

fn fields(html: &str) -> Vec<Field> {
    let lower = html.to_ascii_lowercase();
    let mut found = Vec::new();
    let mut from = 0;
    loop {
        let input = lower.get(from..).and_then(|r| r.find("<input"));
        let button = lower.get(from..).and_then(|r| r.find("<button"));
        let (at, tag) = match (input, button) {
            (Some(i), Some(b)) if i < b => (from + i, "input"),
            (Some(_), Some(b)) => (from + b, "button"),
            (Some(i), None) => (from + i, "input"),
            (None, Some(b)) => (from + b, "button"),
            (None, None) => break,
        };
        let tag_end = lower
            .get(at..)
            .and_then(|r| r.find('>'))
            .map_or(lower.len(), |e| at + e);
        let text = html.get(at..tag_end).unwrap_or_default();
        let (label, next) = if tag == "button" {
            let close = lower
                .get(tag_end..)
                .and_then(|r| r.find("</button"))
                .map_or(lower.len(), |e| tag_end + e);
            (
                strip_tags(html.get(tag_end + 1..close).unwrap_or_default()),
                close,
            )
        } else {
            (String::new(), tag_end)
        };
        let default_kind = if tag == "button" { "submit" } else { "text" };
        found.push(Field {
            tag: tag.to_owned(),
            kind: attribute(text, "type")
                .unwrap_or_else(|| default_kind.to_owned())
                .to_ascii_lowercase(),
            name: attribute(text, "name"),
            value: attribute(text, "value").unwrap_or_default(),
            label,
        });
        from = next.max(at + 1);
    }
    found
}

/// The text of a piece of HTML without its tags, entities decoded,
/// whitespace squashed.
pub fn strip_tags(html: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                text.push(' ');
            }
            _ if !in_tag => text.push(c),
            _ => {}
        }
    }
    decode_entities(&text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn decode_entities(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The value of an attribute of a tag (`name="v"`, `name='v'` or
/// `name=v`), entities decoded; `Some("")` for one given without a value.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower.get(from..).and_then(|r| r.find(name)) {
        let start = from + at;
        from = start + name.len();
        let before = lower.get(..start).and_then(|b| b.chars().last());
        if !before.is_some_and(char::is_whitespace) {
            continue;
        }
        let rest = tag.get(start + name.len()..).unwrap_or_default();
        let trimmed = rest.trim_start();
        let Some(value) = trimmed.strip_prefix('=') else {
            if trimmed.starts_with(|c: char| c.is_whitespace() || c == '>' || c == '/')
                || trimmed.is_empty()
                || rest.starts_with(char::is_whitespace)
            {
                return Some(String::new());
            }
            continue;
        };
        let value = value.trim_start();
        let raw = match value.chars().next() {
            Some(q @ ('"' | '\'')) => value.get(1..).unwrap_or_default().split(q).next(),
            _ => value.split(|c: char| c.is_whitespace() || c == '>').next(),
        };
        return Some(decode_entities(raw.unwrap_or_default()));
    }
    None
}

/// The `BROWSER` command the harness gives the tool: a script on the
/// machine that appends its last argument (the address to open) to a file
/// and ends at once, or, made with [`BrowserCommand::lingering`], then goes
/// on running as a browser does (ended when this is dropped, by the pid it
/// wrote).
pub struct BrowserCommand {
    script: PathBuf,
    opened: PathBuf,
    /// Where a lingering command writes its pid.
    pid: Option<PathBuf>,
    /// How many of the addresses opened a wait has returned.
    taken: Cell<usize>,
}

impl BrowserCommand {
    /// Writes the script into this machine's directory.
    pub fn on(machine: &Machine) -> Self {
        Self::write(machine, false)
    }

    /// As [`BrowserCommand::on`], a command that writes the address, then
    /// does not end: it sleeps (its standard streams on `/dev/null`) until
    /// this is dropped.
    pub fn lingering(machine: &Machine) -> Self {
        Self::write(machine, true)
    }

    fn write(machine: &Machine, lingering: bool) -> Self {
        let script = machine.root().join("harness-browser");
        let opened = machine.root().join("harness-browser-opened");
        let pid = lingering.then(|| machine.root().join("harness-browser-pids"));
        let mut text = format!(
            "#!/bin/sh\nfor last in \"$@\"; do :; done\nprintf '%s\\n' \"$last\" >> '{}'\n",
            opened.display()
        );
        if let Some(pid) = &pid {
            text.push_str(&format!(
                "printf '%s\\n' \"$$\" >> '{}'\nexec sleep 600 </dev/null >/dev/null 2>&1\n",
                pid.display()
            ));
        }
        fs::write(&script, text)
            .unwrap_or_else(|e| panic!("cannot write {}: {e}", script.display()));
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755))
            .unwrap_or_else(|e| panic!("cannot make {} runnable: {e}", script.display()));
        // What was opened before this command was made (an earlier
        // sign-in on the machine) is never a wait's to return.
        let before = fs::read_to_string(&opened)
            .map(|t| t.lines().count())
            .unwrap_or(0);
        Self {
            script,
            opened,
            pid,
            taken: Cell::new(before),
        }
    }

    /// `BROWSER=<the script>`, for the tool's environment.
    pub fn env(&self) -> (&'static str, String) {
        ("BROWSER", self.script.display().to_string())
    }

    /// Every address the tool asked the browser to open so far.
    pub fn opened(&self) -> Vec<String> {
        fs::read_to_string(&self.opened)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The pids of the lingering commands still running.
    pub fn running(&self) -> Vec<u32> {
        let Some(pid) = &self.pid else {
            return Vec::new();
        };
        fs::read_to_string(pid)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .filter(|p: &u32| PathBuf::from(format!("/proc/{p}")).exists())
            .collect()
    }

    /// Waits at most `timeout` for the tool to open an address that no
    /// earlier wait returned and that was not opened before this command
    /// was made; returns the newest of those, or `None`. So each sign-in on
    /// the machine is answered with its own address.
    pub fn wait_opened(&self, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let opened = self.opened();
            if opened.len() > self.taken.get() {
                self.taken.set(opened.len());
                return opened.last().cloned();
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for BrowserCommand {
    fn drop(&mut self) {
        for pid in self.running() {
            // Only the pids this command wrote, one by one.
            drop(
                std::process::Command::new("kill")
                    .arg(pid.to_string())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status(),
            );
        }
    }
}

/// The check code of a sign-in begun with this challenge, made as the
/// contract says: the first 40 bits of SHA-256 of `vm: the check code of a
/// sign-in of the tool`, a zero byte and the challenge's 32 bytes, five
/// bits a character in `0123456789ABCDEFGHJKMNPQRSTVWXYZ`, as `XXXX-XXXX`.
/// `None` when the challenge is not 32 bytes in base64url.
pub fn check_code_of(challenge: &str) -> Option<String> {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let bytes = URL_SAFE_NO_PAD.decode(challenge).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(b"vm: the check code of a sign-in of the tool");
    hasher.update([0_u8]);
    hasher.update(&bytes);
    let digest = hasher.finalize();
    let mut bits: u64 = 0;
    for byte in digest.iter().take(5) {
        bits = (bits << 8) | u64::from(*byte);
    }
    let mut code = String::new();
    for i in 0..8_u32 {
        let index = usize::try_from((bits >> (35 - 5 * i)) & 0x1f).ok()?;
        if i == 4 {
            code.push('-');
        }
        code.push(char::from(*ALPHABET.get(index)?));
    }
    Some(code)
}

/// The check code (`XXXX-XXXX`: four letters or digits, a hyphen, four
/// more) in what the tool wrote, if there is one.
pub fn check_code_in(text: &str) -> Option<String> {
    text.split(|c: char| c.is_whitespace() || "\"'`()[]{}<>,;:.".contains(c))
        .find(|word| {
            let bytes = word.as_bytes();
            bytes.len() == 9
                && bytes.get(4) == Some(&b'-')
                && bytes
                    .iter()
                    .enumerate()
                    .all(|(i, b)| i == 4 || b.is_ascii_alphanumeric())
        })
        .map(str::to_owned)
}

/// The first address of `<origin>/tool?...` in what the tool wrote.
pub fn tool_address_in(text: &str, origin: &str) -> Option<Url> {
    let start = text.find(&format!("{origin}/tool?"))?;
    let rest = text.get(start..)?;
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '>' || c == '`')
        .unwrap_or(rest.len());
    Url::parse(rest.get(..end)?).ok()
}

/// The name of this machine, as the system has it.
pub fn machine_name() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|n| n.trim().to_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"<!doctype html><p>Sign in Ada &amp; co?</p>
<form method="post" action="/tool">
  <input type="hidden" name="sign_in" value="abc&amp;d">
  <label>Code <input name=code autocomplete=off></label>
  <button type="submit" name="answer" value="confirm">Yes, sign in</button>
  <button type="submit" name="answer" value="decline">No, do not sign in</button>
</form>"#;

    #[test]
    fn a_form_is_read_and_answered_as_a_person_would() {
        let found = forms(PAGE);
        assert_eq!(found.len(), 1);
        let form = &found[0];
        assert_eq!(form.action, "/tool");
        assert_eq!(form.method, "post");
        assert_eq!(form.fields.len(), 4);
        assert_eq!(
            form.submission(Decision::Confirm, Some("ABCD-1234"))
                .unwrap(),
            "sign_in=abc%26d&code=ABCD-1234&answer=confirm"
        );
        assert_eq!(
            form.submission(Decision::Decline, None).unwrap(),
            "sign_in=abc%26d&code=&answer=decline"
        );
    }

    #[test]
    fn a_lone_button_confirms() {
        let page = r#"<form method=post action="/tool"><input type=hidden name=t value=1><input type=submit value="Continue"></form>"#;
        let form = &forms(page)[0];
        assert_eq!(form.submission(Decision::Confirm, None).unwrap(), "t=1");
        assert!(form.submission(Decision::Decline, None).is_none());
    }

    #[test]
    fn the_check_code_and_the_address_are_found_in_what_the_tool_wrote() {
        let text = "Open http://auth.vm.test:4000/tool?challenge=a-b_c&machine=box on any device\nand type the code WXYZ-2345.\n";
        assert_eq!(check_code_in(text).as_deref(), Some("WXYZ-2345"));
        let at = tool_address_in(text, "http://auth.vm.test:4000").unwrap();
        assert_eq!(at.path(), "/tool");
        assert_eq!(at.query(), Some("challenge=a-b_c&machine=box"));
        assert!(check_code_in("no code here, ABCD-12345 nor AB-CD").is_none());
    }

    #[test]
    fn a_challenge_is_the_s256_of_its_verifier() {
        let c = Challenge::fresh();
        assert_eq!(c.challenge.len(), 43);
        assert_eq!(c.verifier().len(), 43);
        assert_eq!(s256(c.verifier()), c.challenge);
        assert!(!format!("{c:?}").contains(c.verifier()));
    }
}
