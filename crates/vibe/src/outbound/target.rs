//! The address an outbound call goes to, read once (blackbox/CONTRACT.md,
//! "Outbound calls: Who may call what"): `https:` only, with no user or
//! password, and a host. What the Operation log, the refusals and the
//! program's records say of the address is [`Target::text`] and nothing
//! more: the host, and its port when it is not 443, in lower case.

use std::net::IpAddr;

use url::{Host, Url};

/// The longest address a call may have, in bytes
/// ("Outbound calls: Bounds").
pub const ADDRESS_BYTES: usize = 8 * 1024;

/// The longest host a name may be, in bytes.
pub const MOST_HOST_BYTES: usize = 253;

/// The port of `https:`.
pub const HTTPS_PORT: u16 = 443;

/// Where a call goes: an address read as `https:` with a host. No `Debug`:
/// the path and the query may hold a secret.
#[derive(Clone)]
pub struct Target {
    /// The address, without its fragment.
    pub url: Url,
    /// The host: a name (lower case, `xn--` for names outside ASCII), or an
    /// address.
    pub host: Where,
    pub port: u16,
    /// The host and its port when not 443, as the Operation log and a
    /// refusal write it: `api.example.com:8443`, `[2001:db8::7]`.
    pub text: String,
}

/// What the host of an address is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Where {
    Name(String),
    Address(IpAddr),
}

/// An address read as a call's: `Err` is `not-https`, with the host of the
/// address as written when it has one (`text`, as above, with the port
/// when one is written).
pub fn read(text: &str) -> Result<Target, Option<String>> {
    let Ok(mut url) = Url::parse(text) else {
        return Err(None);
    };
    // A host of more than 253 bytes is not a host: not an address at all,
    // so nothing of it is written anywhere.
    if url
        .host_str()
        .is_some_and(|host| host.len() > MOST_HOST_BYTES)
    {
        return Err(None);
    }
    let written = written(&url);
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return Err(written);
    }
    let Some(host) = url.host() else {
        return Err(None);
    };
    let host = match host {
        Host::Domain(name) => Where::Name(name.to_ascii_lowercase()),
        Host::Ipv4(address) => Where::Address(IpAddr::V4(address)),
        Host::Ipv6(address) => Where::Address(IpAddr::V6(address)),
    };
    let port = url.port_or_known_default().unwrap_or(HTTPS_PORT);
    url.set_fragment(None);
    let mut name = url
        .host_str()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if port != HTTPS_PORT {
        name.push(':');
        name.push_str(&port.to_string());
    }
    Ok(Target {
        url,
        host,
        port,
        text: name,
    })
}

/// The host of `url` as written, with its port when one is written.
fn written(url: &Url) -> Option<String> {
    let mut host = url.host_str()?.to_ascii_lowercase();
    if let Some(port) = url.port() {
        host.push(':');
        host.push_str(&port.to_string());
    }
    Some(host)
}

impl Target {
    /// The host for `Host:` and for the list: an address in brackets, a
    /// name as it is. Without the port.
    pub fn host_str(&self) -> &str {
        self.url.host_str().unwrap_or_default()
    }

    /// The value of the `host` header: the host, and the port when it is
    /// not 443.
    pub fn authority(&self) -> &str {
        &self.text
    }

    /// The request target: the path and the query, as the address has them.
    pub fn target(&self) -> String {
        match self.url.query() {
            Some(query) => format!("{}?{query}", self.url.path()),
            None => self.url.path().to_owned(),
        }
    }

    /// Whether `other` is the same host and port: what a redirect must
    /// stay on for the credentials' headers to go on with it.
    pub fn same_origin(&self, other: &Self) -> bool {
        self.port == other.port && self.host == other.host
    }

    /// Whether the host is the installation's own: `domain` or a name
    /// beneath it, whatever the port. With `bare` false the domain itself is
    /// not (the local run's `localhost`: only the names beneath it are its
    /// own addresses).
    pub fn is_platform(&self, domain: &str, bare: bool) -> bool {
        let Where::Name(name) = &self.host else {
            return false;
        };
        let name = name.trim_end_matches('.');
        let domain = domain.trim_end_matches('.').to_ascii_lowercase();
        if domain.is_empty() {
            return false;
        }
        if name == domain {
            return bare;
        }
        name.strip_suffix(domain.as_str())
            .is_some_and(|before| before.ends_with('.'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(text: &str) -> Target {
        read(text).unwrap_or_else(|e| panic!("{text:?} is not https: {e:?}"))
    }

    #[test]
    fn an_https_address_is_read_with_its_host_port_path_and_query_as_given() {
        let t = target("HTTPS://LOCALHOST:8443/Path/b%20c?x=1&y=%7Ez#frag");
        assert_eq!(t.text, "localhost:8443");
        assert_eq!(t.port, 8443);
        assert_eq!(t.host, Where::Name("localhost".into()));
        assert_eq!(t.target(), "/Path/b%20c?x=1&y=%7Ez");
        assert_eq!(
            t.url.as_str(),
            "https://localhost:8443/Path/b%20c?x=1&y=%7Ez"
        );
        let t = target("https://api.example.com/");
        assert_eq!(t.text, "api.example.com");
        assert_eq!(t.port, 443);
        assert_eq!(t.authority(), "api.example.com");
        let t = target("https://api.example.com:443/");
        assert_eq!(t.text, "api.example.com", "`:443` is kept without it");
    }

    #[test]
    fn an_address_is_written_as_an_entry_would_write_it() {
        assert_eq!(target("https://[::1]/x").text, "[::1]");
        assert_eq!(
            target("https://[::ffff:7f00:1]:8443/x").text,
            "[::ffff:7f00:1]:8443"
        );
        assert_eq!(target("https://169.254.169.254/x").text, "169.254.169.254");
        assert_eq!(
            target("https://[2001:db8::7]/x").host,
            Where::Address("2001:db8::7".parse().unwrap_or_else(|e| panic!("{e}")))
        );
        // A name outside ASCII in its `xn--` form.
        assert!(
            target("https://b\u{fc}cher.example/")
                .text
                .starts_with("xn--")
        );
    }

    #[test]
    fn what_is_not_https_with_a_host_and_no_user_is_not_https_and_says_its_host() {
        for (text, host) in [
            ("http://localhost:8443/plain", Some("localhost:8443")),
            ("HTTP://Example.com/plain", Some("example.com")),
            ("https://user:pw@localhost:8443/x", Some("localhost:8443")),
            ("https://only-user@localhost/x", Some("localhost")),
            ("ftp://example.com/x", Some("example.com")),
            ("mailto:a@b.c", None),
            ("//localhost/x", None),
            ("localhost", None),
            ("not an address", None),
            ("", None),
        ] {
            assert_eq!(
                read(text).err(),
                Some(host.map(str::to_owned)),
                "{text:?} is not https"
            );
        }
    }

    #[test]
    fn the_installation_is_its_domain_and_what_is_beneath_it_on_any_port() {
        for text in [
            "https://vm.test/",
            "https://api.vm.test/v1",
            "https://app.vm.test:8443/",
            "https://deep.sub.vm.test:4443/x",
            "https://VM.TEST/",
            "https://vm.test./",
            "https://app.vm.test./",
        ] {
            assert!(target(text).is_platform("vm.test", true), "{text}");
        }
        for text in [
            "https://notvm.test/",
            "https://vm.test.evil.example/",
            "https://vm-test/",
            "https://[::1]/",
            "https://127.0.0.1/",
        ] {
            assert!(!target(text).is_platform("vm.test", true), "{text}");
        }
        // The local run: names beneath `localhost` are its own, `localhost`
        // itself is not.
        assert!(target("https://app.localhost:1234/").is_platform("localhost", false));
        assert!(!target("https://localhost:1234/").is_platform("localhost", false));
    }

    #[test]
    fn a_redirect_that_stays_on_the_host_and_port_is_the_same_origin() {
        let a = target("https://a.example/x");
        assert!(a.same_origin(&target("https://A.example:443/y?z")));
        assert!(!a.same_origin(&target("https://a.example:8443/")));
        assert!(!a.same_origin(&target("https://b.example/")));
    }
}
