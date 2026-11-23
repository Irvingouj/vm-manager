//! What the glue hands the host of a `fetch` call, read and bounded
//! (blackbox/CONTRACT.md, "Outbound calls": "What goes over the wire",
//! "Bounds"), and how a redirect changes the request.

use hyper::header::{HeaderName, HeaderValue};
use serde_json::Value;

use super::target::ADDRESS_BYTES;

/// The most headers a call may set, and the most bytes of them together
/// (the names and the values).
pub const MOST_HEADERS: usize = 100;
pub const MOST_HEADER_BYTES: usize = 64 * 1024;

/// A string of at most this many bytes may travel inside the arguments as
/// a body: the glue pumps a larger one as a stream.
pub const INLINE_BODY_BYTES: usize = 64 * 1024;

/// What a redirect does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Redirect {
    Follow,
    Manual,
    Error,
}

/// The call's body, as the glue named it. No `Debug`: a string may hold
/// what the code means to keep to itself.
#[derive(Clone)]
pub enum BodyArg {
    None,
    /// A string that went with the arguments.
    Text(String),
    /// A stream of the request or of a stored file, fed by the host.
    Native(u64),
    /// A stream the glue pumped the code's own body into, with its length
    /// when it has one (bytes, a long string).
    Pumped {
        id: u64,
        length: Option<u64>,
    },
    /// The code has no more streams of its own to make one with.
    Busy,
}

/// A call's arguments, read. No `Debug`: the headers may hold a credential
/// and the address a secret.
pub struct Args {
    pub url: String,
    pub method: &'static str,
    pub headers: Vec<(HeaderName, HeaderValue)>,
    pub redirect: Redirect,
    pub body: BodyArg,
}

const METHODS: [&str; 7] = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

/// Headers the platform sets itself: given by the code, they are dropped.
pub fn platform_sets(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "host"
            | "connection"
            | "content-length"
            | "transfer-encoding"
            | "upgrade"
            | "te"
            | "trailer"
            | "keep-alive"
            | "expect"
    ) || name.as_str().starts_with("proxy-")
}

/// The method a redirect goes on with, and whether its body is dropped:
/// `303`, and `301` or `302` after a `POST`, go on as `GET` (a `HEAD` stays
/// one) with no body; every other redirect sends the method and the body
/// again.
pub fn after_redirect(status: u16, method: &'static str) -> (&'static str, bool) {
    match (status, method) {
        (303, "HEAD") => ("HEAD", true),
        (303, _) => ("GET", true),
        (301 | 302, "POST") => ("GET", true),
        (_, method) => (method, false),
    }
}

/// Whether a redirect to another host or port keeps this header.
pub fn credential(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "authorization" | "cookie" | "proxy-authorization"
    )
}

/// The arguments of a call, or the sentence saying why they are none.
pub fn read(args: &Value) -> Result<Args, String> {
    let object = args
        .as_object()
        .ok_or_else(|| "the arguments of a fetch are an object".to_owned())?;
    let url = object
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| "`url` is a string".to_owned())?;
    if url.len() > ADDRESS_BYTES {
        return Err("the address is longer than 8 KiB".to_owned());
    }
    let method = match object.get("method") {
        None | Some(Value::Null) => "GET",
        Some(Value::String(given)) => METHODS
            .iter()
            .find(|known| known.eq_ignore_ascii_case(given))
            .copied()
            .ok_or_else(|| {
                "the method is none of GET, HEAD, POST, PUT, PATCH, DELETE and OPTIONS".to_owned()
            })?,
        Some(_) => return Err("`method` is a string".to_owned()),
    };
    let redirect = match object.get("redirect") {
        None | Some(Value::Null) => Redirect::Follow,
        Some(Value::String(word)) => match word.as_str() {
            "follow" => Redirect::Follow,
            "manual" => Redirect::Manual,
            "error" => Redirect::Error,
            _ => return Err("`redirect` is follow, manual or error".to_owned()),
        },
        Some(_) => return Err("`redirect` is a string".to_owned()),
    };
    let headers = read_headers(object.get("headers"))?;
    let body = read_body(object.get("body"))?;
    Ok(Args {
        url: url.to_owned(),
        method,
        headers,
        redirect,
        body,
    })
}

fn read_headers(value: Option<&Value>) -> Result<Vec<(HeaderName, HeaderValue)>, String> {
    let list = match value {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(list)) => list,
        Some(_) => return Err("`headers` is a list of [name, value]".to_owned()),
    };
    if list.len() > MOST_HEADERS {
        return Err("more than 100 headers".to_owned());
    }
    let mut bytes = 0_usize;
    let mut headers = Vec::with_capacity(list.len());
    for pair in list {
        let (Some(name), Some(value)) = (
            pair.get(0).and_then(Value::as_str),
            pair.get(1).and_then(Value::as_str),
        ) else {
            return Err("a header is a [name, value] of strings".to_owned());
        };
        bytes = bytes.saturating_add(name.len()).saturating_add(value.len());
        if bytes > MOST_HEADER_BYTES {
            return Err("the headers are more than 64 KiB together".to_owned());
        }
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| "a header's name cannot be sent".to_owned())?;
        let value = HeaderValue::from_bytes(value.as_bytes())
            .map_err(|_| "a header's value cannot be sent".to_owned())?;
        headers.push((name, value));
    }
    Ok(headers)
}

fn read_body(value: Option<&Value>) -> Result<BodyArg, String> {
    let kinds = "`body` is a string, bytes, a stream, URLSearchParams, FormData or a Blob";
    match value {
        None | Some(Value::Null) => Ok(BodyArg::None),
        Some(Value::String(text)) if text.is_empty() => Ok(BodyArg::Text(String::new())),
        Some(Value::String(text)) => {
            if text.len() > INLINE_BODY_BYTES {
                Err(kinds.to_owned())
            } else {
                Ok(BodyArg::Text(text.clone()))
            }
        }
        Some(Value::Object(marker)) => {
            // A marker is exactly one of `$busy` alone, `$native` alone,
            // `$pumped` with or without `$length`: nothing else.
            let only = |names: &[&str]| {
                marker.len() == names.len() && names.iter().all(|name| marker.contains_key(*name))
            };
            if only(&["$busy"]) && marker.get("$busy") == Some(&Value::Bool(true)) {
                return Ok(BodyArg::Busy);
            }
            if only(&["$native"])
                && let Some(id) = marker.get("$native").and_then(Value::as_u64)
            {
                return Ok(BodyArg::Native(id));
            }
            if only(&["$pumped"]) || only(&["$pumped", "$length"]) {
                let length = match marker.get("$length") {
                    None => None,
                    Some(length) => match length.as_u64() {
                        Some(length) => Some(length),
                        None => return Err(kinds.to_owned()),
                    },
                };
                if let Some(id) = marker.get("$pumped").and_then(Value::as_u64) {
                    return Ok(BodyArg::Pumped { id, length });
                }
            }
            Err(kinds.to_owned())
        }
        Some(_) => Err(kinds.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_call_with_nothing_but_an_address_is_a_get_that_follows() {
        let args = read(&json!({ "url": "https://a.example/" })).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(args.method, "GET");
        assert_eq!(args.redirect, Redirect::Follow);
        assert!(args.headers.is_empty());
        assert!(matches!(args.body, BodyArg::None));
    }

    #[test]
    fn the_seven_methods_are_read_in_any_case_and_no_other() {
        for method in ["get", "Head", "POST", "put", "PATCH", "delete", "options"] {
            let args = read(&json!({ "url": "https://a.example/", "method": method }))
                .unwrap_or_else(|e| panic!("{method}: {e}"));
            assert_eq!(args.method, method.to_ascii_uppercase());
        }
        for method in ["TRACE", "CONNECT", "FOO", "", "GET "] {
            assert!(
                read(&json!({ "url": "https://a.example/", "method": method })).is_err(),
                "{method:?}"
            );
        }
    }

    #[test]
    fn headers_are_bounded_in_number_and_size_and_must_be_sendable() {
        let many: Vec<Value> = (0..101).map(|i| json!([format!("x-{i}"), "v"])).collect();
        assert!(read(&json!({ "url": "https://a/", "headers": many })).is_err());
        let hundred: Vec<Value> = (0..100).map(|i| json!([format!("x-{i}"), "v"])).collect();
        assert!(read(&json!({ "url": "https://a/", "headers": hundred })).is_ok());
        let big = "v".repeat(MOST_HEADER_BYTES);
        assert!(read(&json!({ "url": "https://a/", "headers": [["x-big", big]] })).is_err());
        assert!(read(&json!({ "url": "https://a/", "headers": [["bad name", "v"]] })).is_err());
        assert!(read(&json!({ "url": "https://a/", "headers": [["x", "a\r\nb: c"]] })).is_err());
        assert!(read(&json!({ "url": "https://a/", "headers": [["x", "caf\u{e9}"]] })).is_ok());
    }

    #[test]
    fn an_address_of_more_than_8_kib_is_malformed() {
        let long = format!("https://a.example/{}", "x".repeat(ADDRESS_BYTES));
        assert!(read(&json!({ "url": long })).is_err());
        let exact = format!("https://a.example/{}", "x".repeat(ADDRESS_BYTES - 18));
        assert!(read(&json!({ "url": exact })).is_ok());
    }

    #[test]
    fn the_body_markers_are_read_and_nothing_else() {
        let kind = |body: Value| read(&json!({ "url": "https://a/", "body": body }));
        assert!(matches!(
            kind(json!("text")).map(|a| a.body),
            Ok(BodyArg::Text(_))
        ));
        assert!(matches!(
            kind(json!({ "$native": 4 })).map(|a| a.body),
            Ok(BodyArg::Native(4))
        ));
        assert!(matches!(
            kind(json!({ "$pumped": 5, "$length": 9 })).map(|a| a.body),
            Ok(BodyArg::Pumped {
                id: 5,
                length: Some(9)
            })
        ));
        assert!(matches!(
            kind(json!({ "$busy": true })).map(|a| a.body),
            Ok(BodyArg::Busy)
        ));
        assert!(matches!(
            kind(json!({ "$pumped": 5 })).map(|a| a.body),
            Ok(BodyArg::Pumped {
                id: 5,
                length: None
            })
        ));
        assert!(kind(json!(7)).is_err());
        assert!(kind(json!({ "other": 1 })).is_err());
        // A marker of two kinds, or with a member that is none of its own,
        // is nothing.
        for marker in [
            json!({ "$native": 1, "$pumped": 2 }),
            json!({ "$native": 1, "$busy": true }),
            json!({ "$busy": true, "$pumped": 2 }),
            json!({ "$busy": true, "other": 1 }),
            json!({ "$native": 1, "$length": 3 }),
            json!({ "$native": 1, "x": 0 }),
            json!({ "$pumped": 2, "$length": 3, "x": 0 }),
            json!({ "$pumped": 2, "$length": "3" }),
            json!({ "$length": 3 }),
            json!({ "$busy": false }),
            json!({ "$native": "1" }),
        ] {
            assert!(kind(marker.clone()).is_err(), "{marker} is no marker");
        }
    }

    #[test]
    fn a_redirect_changes_the_method_and_drops_the_body_as_the_contract_says() {
        assert_eq!(after_redirect(303, "POST"), ("GET", true));
        assert_eq!(after_redirect(303, "PUT"), ("GET", true));
        assert_eq!(after_redirect(303, "HEAD"), ("HEAD", true));
        assert_eq!(after_redirect(301, "POST"), ("GET", true));
        assert_eq!(after_redirect(302, "POST"), ("GET", true));
        assert_eq!(after_redirect(301, "GET"), ("GET", false));
        assert_eq!(after_redirect(302, "PUT"), ("PUT", false));
        assert_eq!(after_redirect(307, "POST"), ("POST", false));
        assert_eq!(after_redirect(308, "DELETE"), ("DELETE", false));
    }

    #[test]
    fn the_platform_sets_these_and_a_redirect_drops_these_credentials() {
        for name in [
            "host",
            "connection",
            "content-length",
            "transfer-encoding",
            "upgrade",
            "te",
            "trailer",
            "keep-alive",
            "expect",
            "proxy-authorization",
            "proxy-x",
        ] {
            let name = HeaderName::from_bytes(name.as_bytes()).unwrap_or_else(|e| panic!("{e}"));
            assert!(platform_sets(&name), "{name}");
        }
        let kept = HeaderName::from_static("x-kept");
        assert!(!platform_sets(&kept));
        assert!(credential(&HeaderName::from_static("authorization")));
        assert!(credential(&HeaderName::from_static("cookie")));
        assert!(!credential(&kept));
    }
}
