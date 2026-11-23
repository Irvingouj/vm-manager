//! An entry of the outbound list, and a host a call may be made to
//! (blackbox/CONTRACT.md, "Outbound calls: The list"): one reading and one
//! normal form, shared by the API, the tool's own check before it asks, and
//! the host deciding a call.
//!
//! A text is read as one of the table's forms: `api.example.com`,
//! `api.example.com:8443`, `*.example.com`, `203.0.113.7` or
//! `[2001:db8::7]`, each address with or without a port. Kept: lower case,
//! a name outside ASCII in its `xn--` form, no `:443`, an IPv6 address in
//! brackets. [`Entry::at`] reads the host of a call the same way, so that a
//! decision compares what one reading produced with what the other did.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The most bytes a name has.
const MAX_NAME_BYTES: usize = 253;

/// The most bytes a label has.
const MAX_LABEL_BYTES: usize = 63;

/// The port an entry without one is on: the one `https` is.
const HTTPS_PORT: u16 = 443;

/// The most bytes a host asked for in the Operation log's filter may have
/// (blackbox/CONTRACT.md, "The Operation log": "1 to 260 bytes, what a
/// call's host with its port could be"; a call's host is never a wildcard).
pub const MAX_FILTER_BYTES: usize = 260;

/// Why a text is not an entry (or not a host).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryProblem {
    /// Nothing was given (and an empty entry is `/v1/outbound-hosts/`).
    Empty,
    /// A character an entry never holds: a space, `/`, `@`, `?`, a scheme's
    /// `:`, a tab.
    NotAnEntry,
    /// A port outside 1 to 65535, or not written as digits.
    Port,
    /// A name that is not 1 to 253 bytes of labels of 1 to 63 letters,
    /// digits and `-`, a label not starting or ending with `-`.
    Name,
    /// A `*` where a wildcard does not go: not alone before a dot and a
    /// name of at least two labels.
    Wildcard,
    /// An address written otherwise than `203.0.113.7` or `[2001:db8::7]`.
    Address,
    /// A name whose last label is a number: a resolver may read it as an
    /// address the administrator did not mean (`127.1`, `0x7f.0.0.1`).
    Number,
}

impl EntryProblem {
    /// What is wrong, in one sentence, for a Coding agent.
    pub fn words(self) -> &'static str {
        match self {
            Self::Empty => "the entry is empty",
            Self::NotAnEntry => {
                "the entry holds a character a host does not: no scheme, path, query, user or space"
            }
            Self::Port => "the port is not a number from 1 to 65535",
            Self::Name => {
                "a name is 1 to 253 bytes of labels of 1 to 63 letters, digits and hyphens, a label not starting or ending with a hyphen"
            }
            Self::Wildcard => {
                "a wildcard is `*.` before a name of at least two labels, and nothing else in the entry is a `*`"
            }
            Self::Address => {
                "an address is written as 203.0.113.7, or as an IPv6 address in brackets such as [2001:db8::7], with its port after it"
            }
            Self::Number => {
                "a name's last label is not a number (digits, or 0x and hexadecimal digits): a resolver may read it as an address"
            }
        }
    }
}

impl fmt::Display for EntryProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.words())
    }
}

/// The API's and the tool's words for a text that is not an entry: what is
/// wrong, then what an entry is.
pub fn problem_words(problem: EntryProblem) -> String {
    format!(
        "{}: an entry is a host — a name such as api.example.com, a name with a port such as api.example.com:8443, a wildcard such as *.example.com, or an address such as 203.0.113.7 or [2001:db8::7], with or without a port",
        problem.words()
    )
}

/// What a host is, as the list holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Host {
    /// A name, exactly that one.
    Name(String),
    /// `*.` and the name beneath it; not that name itself.
    Wildcard(String),
    /// An address, called by its address.
    Address(IpAddr),
}

/// One entry of the list, or the host of a call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// As the list writes it: lower case, `xn--`, no `:443`, brackets
    /// around an IPv6 address.
    text: String,
    port: u16,
    host: Host,
}

impl Entry {
    /// An entry, as the list takes it: a name, a wildcard or an address,
    /// each with or without a port.
    pub fn parse(text: &str) -> Result<Self, EntryProblem> {
        if text.is_empty() {
            return Err(EntryProblem::Empty);
        }
        let (wildcard, rest) = match text.strip_prefix("*.") {
            Some(rest) => (true, rest),
            None => {
                if text.contains('*') {
                    return Err(EntryProblem::Wildcard);
                }
                (false, text)
            }
        };
        if rest.is_empty() {
            return Err(EntryProblem::Wildcard);
        }
        let (host, port) = parts(rest, wildcard)?;
        Ok(Self {
            text: text_of(&host, port),
            port,
            host,
        })
    }

    /// The host of a call, `host` alone (an IPv6 address in brackets, as an
    /// entry writes it; brackets around it are taken too), on `port`. No
    /// wildcard: a call is to one host.
    pub fn at(host: &str, port: u16) -> Result<Self, EntryProblem> {
        if port == 0 {
            return Err(EntryProblem::Port);
        }
        if host.is_empty() {
            return Err(EntryProblem::Empty);
        }
        if host.contains('*') {
            return Err(EntryProblem::Wildcard);
        }
        let kind = if let Some(rest) = host.strip_prefix('[') {
            let close = rest.find(']').ok_or(EntryProblem::Address)?;
            let inside = rest.get(..close).ok_or(EntryProblem::Address)?;
            let after = rest
                .get(close..)
                .and_then(|after| after.strip_prefix(']'))
                .ok_or(EntryProblem::Address)?;
            if !after.is_empty() {
                return Err(EntryProblem::Port);
            }
            let address: Ipv6Addr = inside.parse().map_err(|_| EntryProblem::Address)?;
            Host::Address(IpAddr::V6(address))
        } else if let Ok(address) = host.parse::<Ipv6Addr>() {
            Host::Address(IpAddr::V6(address))
        } else if let Ok(address) = host.parse::<Ipv4Addr>() {
            Host::Address(IpAddr::V4(address))
        } else if host.contains(':') || host.contains('[') || host.contains(']') {
            return Err(EntryProblem::NotAnEntry);
        } else {
            Host::Name(name_of(host)?)
        };
        Ok(Self {
            text: text_of(&kind, port),
            port,
            host: kind,
        })
    }

    /// The entry as the list writes it.
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Whether this entry allows `asked`: the same name and port, an
    /// address equal to it, or a wildcard with one label or more beneath
    /// it and the same port.
    pub fn allows(&self, asked: &Self) -> bool {
        if self.port != asked.port {
            return false;
        }
        match (&self.host, &asked.host) {
            (Host::Name(ours), Host::Name(theirs)) => ours == theirs,
            (Host::Address(ours), Host::Address(theirs)) => ours == theirs,
            (Host::Wildcard(base), Host::Name(theirs)) => {
                match theirs.len().checked_sub(base.len() + 1) {
                    Some(_) => theirs
                        .strip_suffix(base.as_str())
                        .is_some_and(|before| before.ends_with('.')),
                    None => false,
                }
            }
            _ => false,
        }
    }
}

/// The host and port of `rest`, which has no `*.` before it.
fn parts(rest: &str, wildcard: bool) -> Result<(Host, u16), EntryProblem> {
    if let Some(after_open) = rest.strip_prefix('[') {
        if wildcard {
            return Err(EntryProblem::Wildcard);
        }
        let close = after_open.find(']').ok_or(EntryProblem::Address)?;
        let inside = after_open.get(..close).ok_or(EntryProblem::Address)?;
        let after = after_open
            .get(close..)
            .and_then(|after| after.strip_prefix(']'))
            .ok_or(EntryProblem::Address)?;
        let address: Ipv6Addr = inside.parse().map_err(|_| EntryProblem::Address)?;
        let port = match after {
            "" => HTTPS_PORT,
            other => port_of(other.strip_prefix(':').ok_or(EntryProblem::Port)?)?,
        };
        return Ok((Host::Address(IpAddr::V6(address)), port));
    }
    let (name, port) = match rest.split_once(':') {
        Some((name, digits)) => (name, port_of(digits)?),
        None => (rest, HTTPS_PORT),
    };
    if name.contains(['[', ']', ':']) {
        return Err(EntryProblem::Address);
    }
    if wildcard {
        // A wildcard is over a name, and an address is called by its
        // address: `*.203.0.113.7` is no entry, and a second `*` never is.
        if name.contains('*') || name.parse::<Ipv4Addr>().is_ok() {
            return Err(EntryProblem::Wildcard);
        }
        let base = name_of(name)?;
        if base.split('.').count() < 2 {
            return Err(EntryProblem::Wildcard);
        }
        return Ok((Host::Wildcard(base), port));
    }
    if let Ok(address) = name.parse::<Ipv4Addr>() {
        return Ok((Host::Address(IpAddr::V4(address)), port));
    }
    Ok((Host::Name(name_of(name)?), port))
}

/// `text` as a port: digits, 1 to 65535.
fn port_of(text: &str) -> Result<u16, EntryProblem> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(EntryProblem::Port);
    }
    let port: u16 = text.parse().map_err(|_| EntryProblem::Port)?;
    if port == 0 {
        return Err(EntryProblem::Port);
    }
    Ok(port)
}

/// `name` in its kept form: lower case, `xn--` for what is outside ASCII,
/// and one of the list's names.
fn name_of(name: &str) -> Result<String, EntryProblem> {
    if name.is_empty() {
        return Err(EntryProblem::Name);
    }
    // What is ASCII must be a name's own character already: anything else
    // (a space, a slash, a tab) is not made a name by IDNA.
    let stray = name
        .chars()
        .any(|c| c.is_ascii() && !(c.is_ascii_alphanumeric() || c == '.' || c == '-'));
    if stray {
        return Err(EntryProblem::NotAnEntry);
    }
    let ascii = if name.is_ascii() {
        name.to_ascii_lowercase()
    } else {
        match url::Host::parse(name) {
            Ok(url::Host::Domain(domain)) => domain,
            _ => return Err(EntryProblem::Name),
        }
    };
    if !valid_name(&ascii) {
        return Err(EntryProblem::Name);
    }
    // A resolver may read a last label that is a number as an address the
    // administrator did not mean (blackbox/CONTRACT.md, "The list": an
    // address is written in its usual form only).
    if last_label_is_a_number(&ascii) {
        return Err(EntryProblem::Number);
    }
    Ok(ascii)
}

/// Whether the last label of `name` is a number: all digits, or `0x` and
/// hexadecimal digits, or `0x` alone (lower case: [`name_of`] lowercases
/// before this).
fn last_label_is_a_number(name: &str) -> bool {
    let last = name.rsplit_once('.').map_or(name, |(_, last)| last);
    if !last.is_empty() && last.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    match last.strip_prefix("0x") {
        Some(hex) => hex.is_empty() || hex.bytes().all(|b| b.is_ascii_hexdigit()),
        None => false,
    }
}

/// Whether `name` is 1 to 253 bytes of labels of 1 to 63 letters, digits
/// and `-`, no label starting or ending with `-`.
fn valid_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return false;
    }
    name.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= MAX_LABEL_BYTES
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

/// `host` on `port` as an entry writes it.
fn text_of(host: &Host, port: u16) -> String {
    let mut text = match host {
        Host::Name(name) => name.clone(),
        Host::Wildcard(base) => format!("*.{base}"),
        Host::Address(address) => match address {
            IpAddr::V4(address) => address.to_string(),
            IpAddr::V6(address) => format!("[{address}]"),
        },
    };
    if port != HTTPS_PORT {
        text.push(':');
        text.push_str(&port.to_string());
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each: what is given, what is kept.
    const KEPT: [(&str, &str); 16] = [
        ("api.example.com", "api.example.com"),
        ("api.example.com:8443", "api.example.com:8443"),
        ("*.example.com", "*.example.com"),
        ("*.example.com:8443", "*.example.com:8443"),
        ("203.0.113.7", "203.0.113.7"),
        ("203.0.113.7:8443", "203.0.113.7:8443"),
        ("[2001:db8::7]", "[2001:db8::7]"),
        ("[2001:db8::7]:8443", "[2001:db8::7]:8443"),
        ("Mixed.Example.ORG", "mixed.example.org"),
        ("bücher.example", "xn--bcher-kva.example"),
        ("a-b.example.net", "a-b.example.net"),
        ("1st.example.net", "1st.example.net"),
        ("*.co.uk", "*.co.uk"),
        ("edge.example.com:1", "edge.example.com:1"),
        ("edge.example.com:65535", "edge.example.com:65535"),
        ("localhost", "localhost"),
    ];

    #[test]
    fn every_form_of_the_lists_table_is_read_and_kept() {
        for (given, kept) in KEPT {
            let entry = Entry::parse(given).unwrap_or_else(|e| panic!("{given:?}: {}", e.words()));
            assert_eq!(entry.text(), kept, "{given:?}");
            // What is kept is the same entry.
            let again = Entry::parse(kept).unwrap_or_else(|e| panic!("{kept:?}: {}", e.words()));
            assert_eq!(again, entry, "{kept:?}");
        }
    }

    #[test]
    fn the_same_entry_written_otherwise_is_the_same_entry() {
        for (given, kept) in [
            ("API.EXAMPLE.COM", "api.example.com"),
            ("api.example.com:443", "api.example.com"),
            ("*.example.com:443", "*.example.com"),
            ("203.0.113.7:443", "203.0.113.7"),
            ("[2001:db8::7]:443", "[2001:db8::7]"),
            ("xn--bcher-kva.example", "xn--bcher-kva.example"),
            ("XN--BCHER-KVA.EXAMPLE", "xn--bcher-kva.example"),
            ("[2001:DB8::7]", "[2001:db8::7]"),
        ] {
            let entry = Entry::parse(given).unwrap_or_else(|e| panic!("{given:?}: {}", e.words()));
            assert_eq!(entry.text(), kept, "{given:?}");
        }
    }

    #[test]
    fn a_name_at_253_bytes_is_taken_and_one_byte_more_is_not() {
        let name = |bytes: usize| {
            let mut labels: Vec<String> = Vec::new();
            let mut left = bytes;
            loop {
                let this = left.min(63);
                labels.push("a".repeat(this));
                left -= this;
                if left == 0 {
                    break;
                }
                // The dot after this label.
                left -= 1;
            }
            labels.join(".")
        };
        let longest = name(253);
        assert_eq!(longest.len(), 253);
        assert!(Entry::parse(&longest).is_ok(), "{longest}");
        let too_long = name(254);
        assert_eq!(too_long.len(), 254);
        assert!(Entry::parse(&too_long).is_err(), "{too_long}");
    }

    #[test]
    fn what_is_not_an_entry_is_refused() {
        let long_label = format!("{}.example.com", "a".repeat(64));
        let cases: [(&str, EntryProblem); 36] = [
            ("", EntryProblem::Empty),
            ("-a.example.com", EntryProblem::Name),
            ("a-.example.com", EntryProblem::Name),
            ("a.-b.example.com", EntryProblem::Name),
            (long_label.as_str(), EntryProblem::Name),
            ("*.com", EntryProblem::Wildcard),
            ("*", EntryProblem::Wildcard),
            ("a.*.example.com", EntryProblem::Wildcard),
            ("*example.com", EntryProblem::Wildcard),
            ("*.*.example.com", EntryProblem::Wildcard),
            ("a..example.com", EntryProblem::Name),
            (".example.com", EntryProblem::Name),
            ("a_b.example.com", EntryProblem::NotAnEntry),
            ("api.example.com:0", EntryProblem::Port),
            ("api.example.com:65536", EntryProblem::Port),
            ("api.example.com:abc", EntryProblem::Port),
            ("api.example.com:", EntryProblem::Port),
            ("api.example.com:-1", EntryProblem::Port),
            ("api.example.com:443:443", EntryProblem::Port),
            ("[2001:db8::7]:0", EntryProblem::Port),
            ("203.0.113.7:65536", EntryProblem::Port),
            ("https://api.example.com", EntryProblem::Port),
            ("http://api.example.com", EntryProblem::Port),
            ("api.example.com/v1", EntryProblem::NotAnEntry),
            ("api.example.com/", EntryProblem::NotAnEntry),
            ("api.example.com?x=1", EntryProblem::NotAnEntry),
            ("user@api.example.com", EntryProblem::NotAnEntry),
            ("user:pw@api.example.com", EntryProblem::Port),
            ("api example.com", EntryProblem::NotAnEntry),
            (" api.example.com", EntryProblem::NotAnEntry),
            ("api.example.com ", EntryProblem::NotAnEntry),
            ("*[2001:db8::7]", EntryProblem::Wildcard),
            ("*.[2001:db8::7]", EntryProblem::Wildcard),
            ("2001:db8::7:8443", EntryProblem::Port),
            ("2001:db8::7", EntryProblem::Port),
            ("[203.0.113.7]", EntryProblem::Address),
        ];
        for (given, problem) in cases {
            assert_eq!(Entry::parse(given).err(), Some(problem), "{given:?}");
        }
        for given in ["[2001:db8::7", "[not-an-address]", "a\tb.example.com"] {
            assert!(Entry::parse(given).is_err(), "{given:?}");
        }
    }

    #[test]
    fn a_name_whose_last_label_is_a_number_is_not_a_name() {
        for given in [
            "127.1",
            "0x7f.0.0.1",
            "2130706433",
            "1.2.3",
            "010.0.0.1",
            "203.0.113.010",
            "example.123",
            "example.0x1f",
            "example.0X1F",
            "example.0x",
            "*.example.123",
        ] {
            assert_eq!(
                Entry::parse(given).err(),
                Some(EntryProblem::Number),
                "{given:?}"
            );
            assert!(
                Entry::at(given.trim_start_matches("*."), 443).is_err(),
                "{given:?} as a call's host"
            );
        }
        // A name that only looks like a number is a name; an address is an
        // address.
        for given in [
            "203.0.113.7",
            "1.example.com",
            "123.example.com",
            "example.com1",
            "x0x1f.example.org",
            "example.0xzz",
        ] {
            assert!(Entry::parse(given).is_ok(), "{given:?}");
        }
    }

    #[test]
    fn a_wildcard_of_the_longest_name_and_a_port_is_an_entry() {
        let name = format!(
            "{}.{}.{}.{}",
            "a".repeat(63),
            "a".repeat(63),
            "a".repeat(63),
            "a".repeat(61)
        );
        assert_eq!(name.len(), 253);
        let entry = format!("*.{name}:65535");
        assert_eq!(entry.len(), 261, "`*.` + 253 + `:65535`");
        let parsed = Entry::parse(&entry).unwrap_or_else(|e| panic!("{}", e.words()));
        assert_eq!(parsed.text(), entry);
        // The 260-byte bound is the Operation log's filter's (a call's host
        // with its port, never a wildcard), not an entry's.
        assert!(MAX_FILTER_BYTES < entry.len());
    }

    #[test]
    fn a_calls_host_is_read_as_the_list_writes_it_and_a_port_in_it_is_refused() {
        let entry = Entry::at("API.Example.COM", 8443).unwrap();
        assert_eq!(entry.text(), "api.example.com:8443");
        assert_eq!(entry.port(), 8443);
        assert_eq!(
            Entry::at("api.example.com", 443).unwrap().text(),
            "api.example.com"
        );
        assert_eq!(
            Entry::at("2001:db8::7", 443).unwrap().text(),
            "[2001:db8::7]"
        );
        assert_eq!(
            Entry::at("[2001:db8::7]", 8443).unwrap().text(),
            "[2001:db8::7]:8443"
        );
        assert!(Entry::at("*.example.com", 443).is_err());
        assert!(Entry::at("api.example.com:8443", 443).is_err());
        assert!(Entry::at("api.example.com", 0).is_err());
    }

    #[test]
    fn the_problem_says_what_an_entry_is() {
        let words = problem_words(EntryProblem::Name);
        for named in ["entry", "host", "name", "address"] {
            assert!(words.contains(named), "{words}");
        }
    }
}
