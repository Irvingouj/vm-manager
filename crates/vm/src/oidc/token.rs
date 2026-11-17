//! An ID token checked (blackbox/CONTRACT.md, "Sign-in and sessions: The
//! way through sign-in", step 3, and "Renewal"): a JWS in compact form,
//! signed with `RS256` or `ES256` (nothing else: never `none`, never an
//! HMAC) by a key of the provider's of the right type, chosen by `kid`;
//! `iss` the issuer; `aud` the client id (a string, or an array that holds
//! it, and then with several audiences `azp` the client id); `exp` not past
//! (with [`CLOCK_SKEW`]); `nonce` the one sent, when one was sent. Then the
//! claims the platform reads: `sub`, `name`, `email`, and the Groups.
//!
//! Nothing of the token is in an error: each says only which check failed.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::signature::{self, RsaPublicKeyComponents, UnparsedPublicKey};
use serde_json::{Map, Value};

use super::secret::{Secret, same};

/// The clock difference allowed on `exp`, `nbf` and `iat`, in seconds.
pub const CLOCK_SKEW: i64 = 30;

/// The most Groups a person may have, once repeats are dropped.
pub const MAX_GROUPS: usize = 1000;

/// The longest Group, `sub`, `name` or `email`, in bytes.
pub const MAX_CLAIM_BYTES: usize = 256;

/// The most keys of the provider's that are kept.
pub const MAX_KEYS: usize = 100;

/// One of the provider's public keys.
pub struct Key {
    kid: Option<String>,
    alg: Option<String>,
    material: Material,
}

enum Material {
    /// RSA: the modulus and the exponent, big-endian.
    Rsa { n: Vec<u8>, e: Vec<u8> },
    /// P-256: the uncompressed point, `04 || x || y`.
    P256(Vec<u8>),
}

/// The provider's keys, as its JWKS gave them.
pub struct Keys(Vec<Key>);

impl Keys {
    /// The keys of a JWKS that the platform can use (RSA, and EC on P-256,
    /// meant for signatures), at most [`MAX_KEYS`]; `None` when it is not a
    /// JWKS.
    pub fn from_jwks(jwks: &Value) -> Option<Self> {
        let entries = jwks.get("keys")?.as_array()?;
        let keys = entries
            .iter()
            .filter_map(|entry| key_of(entry.as_object()?))
            .take(MAX_KEYS)
            .collect();
        Some(Self(keys))
    }
}

fn base64url(value: Option<&Value>) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(value?.as_str()?).ok()
}

fn key_of(entry: &Map<String, Value>) -> Option<Key> {
    if entry.get("use").is_some_and(|u| u.as_str() != Some("sig")) {
        return None;
    }
    let text = |name: &str| -> Option<String> {
        entry
            .get(name)
            .and_then(Value::as_str)
            .filter(|t| t.len() <= MAX_CLAIM_BYTES)
            .map(str::to_owned)
    };
    let material = match entry.get("kty").and_then(Value::as_str)? {
        "RSA" => Material::Rsa {
            n: base64url(entry.get("n"))?,
            e: base64url(entry.get("e"))?,
        },
        "EC" => {
            if entry.get("crv").and_then(Value::as_str) != Some("P-256") {
                return None;
            }
            let x = base64url(entry.get("x"))?;
            let y = base64url(entry.get("y"))?;
            if x.len() != 32 || y.len() != 32 {
                return None;
            }
            let mut point = Vec::with_capacity(65);
            point.push(4);
            point.extend_from_slice(&x);
            point.extend_from_slice(&y);
            Material::P256(point)
        }
        _ => return None,
    };
    Some(Key {
        kid: text("kid"),
        alg: text("alg"),
        material,
    })
}

/// What a token must hold besides its signature.
pub struct Expected<'a> {
    pub issuer: &'a str,
    pub client_id: &'a str,
    pub groups_claim: &'a str,
    /// The `nonce` of the authorization request; `None` at a renewal, which
    /// asks none.
    pub nonce: Option<&'a Secret>,
}

/// What the platform reads of a token that holds.
pub struct Claims {
    pub sub: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub groups: Vec<String>,
}

/// Why a token is not taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// It names a key that is not among the provider's (or names none and
    /// there is none of its type): the keys may be fetched again.
    UnknownKey,
    /// It does not hold; which check failed.
    Invalid(&'static str),
}

/// The algorithms taken.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Alg {
    Rs256,
    Es256,
}

impl Alg {
    fn name(self) -> &'static str {
        match self {
            Self::Rs256 => "RS256",
            Self::Es256 => "ES256",
        }
    }

    fn fits(self, material: &Material) -> bool {
        matches!(
            (self, material),
            (Self::Rs256, Material::Rsa { .. }) | (Self::Es256, Material::P256(_))
        )
    }
}

fn object(part: &str) -> Option<Map<String, Value>> {
    let bytes = URL_SAFE_NO_PAD.decode(part).ok()?;
    match serde_json::from_slice(&bytes).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

/// A time claim in whole seconds; `None` when absent, `Err` when it is not
/// a whole number (a time past `i64` is read as the farthest one).
fn seconds(claims: &Map<String, Value>, name: &str) -> Result<Option<i64>, Refused> {
    match claims.get(name) {
        None => Ok(None),
        Some(value) => value
            .as_i64()
            .or_else(|| value.as_u64().map(|_| i64::MAX))
            .map(Some)
            .ok_or(Refused::Invalid(
                "a time claim is not a whole number of seconds",
            )),
    }
}

/// Checks `token` against `keys` and `expected`.
pub fn verify(token: &str, keys: &Keys, expected: &Expected<'_>) -> Result<Claims, Refused> {
    let mut parts = token.split('.');
    let (Some(header), Some(payload), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(Refused::Invalid("it is not a signed token"));
    };
    let header_fields = object(header).ok_or(Refused::Invalid("its header cannot be read"))?;
    let alg = match header_fields.get("alg").and_then(Value::as_str) {
        Some("RS256") => Alg::Rs256,
        Some("ES256") => Alg::Es256,
        _ => return Err(Refused::Invalid("it is not signed with RS256 or ES256")),
    };
    if header_fields.contains_key("crit") {
        return Err(Refused::Invalid(
            "its header has extensions the platform does not know",
        ));
    }
    let kid = header_fields.get("kid").map(Value::as_str);
    let mut candidates = keys
        .0
        .iter()
        .filter(|k| alg.fits(&k.material) && k.alg.as_deref().is_none_or(|a| a == alg.name()));
    let key = match kid {
        Some(Some(kid)) => candidates
            .find(|k| k.kid.as_deref() == Some(kid))
            .ok_or(Refused::UnknownKey)?,
        Some(None) => return Err(Refused::Invalid("its kid is not text")),
        None => {
            let first = candidates.next().ok_or(Refused::UnknownKey)?;
            if candidates.next().is_some() {
                return Err(Refused::Invalid(
                    "it names no key and the provider has several",
                ));
            }
            first
        }
    };
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| Refused::Invalid("its signature cannot be read"))?;
    // The signing input: the header and the payload as they came, with the
    // dot between them.
    let input_length = header.len() + 1 + payload.len();
    let input = token
        .as_bytes()
        .get(..input_length)
        .ok_or(Refused::Invalid("it is not a signed token"))?;
    let verified = match &key.material {
        Material::Rsa { n, e } => RsaPublicKeyComponents { n, e }
            .verify(&signature::RSA_PKCS1_2048_8192_SHA256, input, &signature)
            .is_ok(),
        Material::P256(point) => UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, point)
            .verify(input, &signature)
            .is_ok(),
    };
    if !verified {
        return Err(Refused::Invalid("its signature does not verify"));
    }
    let claims = object(payload).ok_or(Refused::Invalid("its claims cannot be read"))?;
    check_claims(&claims, expected)?;
    read_claims(&claims, expected.groups_claim)
}

fn check_claims(claims: &Map<String, Value>, expected: &Expected<'_>) -> Result<(), Refused> {
    if claims.get("iss").and_then(Value::as_str) != Some(expected.issuer) {
        return Err(Refused::Invalid("its iss is not the issuer"));
    }
    let audience_holds = match claims.get("aud") {
        Some(Value::String(aud)) => aud == expected.client_id,
        Some(Value::Array(auds)) => {
            let holds = auds.iter().any(|a| a.as_str() == Some(expected.client_id));
            let azp = claims.get("azp").and_then(Value::as_str);
            holds && (auds.len() == 1 || azp == Some(expected.client_id))
        }
        _ => false,
    };
    if !audience_holds {
        return Err(Refused::Invalid("its aud is not the client id"));
    }
    if claims
        .get("azp")
        .is_some_and(|azp| azp.as_str() != Some(expected.client_id))
    {
        return Err(Refused::Invalid("its azp is not the client id"));
    }
    let now = now_seconds();
    let exp = seconds(claims, "exp")?.ok_or(Refused::Invalid("it has no exp"))?;
    if now > exp.saturating_add(CLOCK_SKEW) {
        return Err(Refused::Invalid("it has expired"));
    }
    if seconds(claims, "nbf")?.is_some_and(|nbf| nbf > now.saturating_add(CLOCK_SKEW)) {
        return Err(Refused::Invalid("it is not valid yet"));
    }
    if seconds(claims, "iat")?.is_some_and(|iat| iat > now.saturating_add(CLOCK_SKEW)) {
        return Err(Refused::Invalid("it was issued in the future"));
    }
    if let Some(nonce) = expected.nonce {
        let given = claims
            .get("nonce")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !same(given.as_bytes(), nonce.expose().as_bytes()) {
            return Err(Refused::Invalid("its nonce is not the one sent"));
        }
    }
    Ok(())
}

/// Text of at most [`MAX_CLAIM_BYTES`], else nothing.
fn short_text(claims: &Map<String, Value>, name: &str) -> Option<String> {
    claims
        .get(name)
        .and_then(Value::as_str)
        .filter(|t| t.len() <= MAX_CLAIM_BYTES)
        .map(str::to_owned)
}

fn read_claims(claims: &Map<String, Value>, groups_claim: &str) -> Result<Claims, Refused> {
    let sub = claims
        .get("sub")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= MAX_CLAIM_BYTES && !s.chars().any(char::is_control))
        .ok_or(Refused::Invalid("its sub is missing, empty or too long"))?
        .to_owned();
    Ok(Claims {
        sub,
        name: short_text(claims, "name"),
        email: short_text(claims, "email"),
        groups: groups(claims.get(groups_claim))?,
    })
}

/// The Groups claim read: an array of strings, or one string (one Group);
/// absent or of another shape, no Groups. Entries trimmed, empty ones
/// dropped, repeats kept once, in order; more than [`MAX_GROUPS`] once
/// repeats are dropped, or one longer than [`MAX_CLAIM_BYTES`], and the
/// token is not taken.
pub fn groups(claim: Option<&Value>) -> Result<Vec<String>, Refused> {
    let entries: Vec<&str> = match claim {
        Some(Value::String(one)) => vec![one.as_str()],
        Some(Value::Array(all)) => {
            let strings: Option<Vec<&str>> = all.iter().map(Value::as_str).collect();
            strings.unwrap_or_default()
        }
        _ => Vec::new(),
    };
    let mut kept: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for entry in entries.into_iter().map(str::trim) {
        if entry.is_empty() {
            continue;
        }
        if entry.len() > MAX_CLAIM_BYTES {
            return Err(Refused::Invalid("a Group is longer than 256 bytes"));
        }
        if seen.insert(entry) {
            if kept.len() >= MAX_GROUPS {
                return Err(Refused::Invalid("there are more than 1,000 Groups"));
            }
            kept.push(entry.to_owned());
        }
    }
    Ok(kept)
}
