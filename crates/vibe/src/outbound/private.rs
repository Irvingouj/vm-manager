//! The ranges the operator of the machine may open for outbound calls
//! (`vibe serve --outbound-private <range>`; blackbox/CONTRACT.md, "Who may
//! call what", the bullet "What the operator of the machine may open"):
//! an address, or a range in CIDR form, at most [`MOST`] times, each parsed
//! once at start and asked with [`PrivateRanges::contains`].
//!
//! Nothing of the addresses that are never called lives here: that is the
//! host's decision (the next stage). This type is only the answer to "did
//! the operator open this address?".
//!
//! An IPv4-mapped IPv6 address (`::ffff:10.9.0.5`) is judged by the IPv4
//! address it carries, so a range of IPv4 addresses opens it too. The other
//! forms that carry an IPv4 address (`64:ff9b::/96`, `2002::/16`) are the
//! caller's to judge: [`PrivateRanges::contains`] reads them as the IPv6
//! addresses they are written as.

use std::net::IpAddr;

/// The most ranges the operator may open.
pub const MOST: usize = 16;

/// One range: an address and how many of its leading bits are fixed.
#[derive(Clone, Copy, Debug)]
struct Range {
    address: IpAddr,
    prefix: u8,
}

impl Range {
    /// `text` as an address (`10.9.1.5`, `[::1]`'s inside) or a range in
    /// CIDR form (`10.9.0.0/24`, `fd00::/8`).
    fn parse(text: &str) -> Result<Self, &'static str> {
        match text.split_once('/') {
            Some((address, prefix)) => {
                let address: IpAddr = address
                    .parse()
                    .map_err(|_| "the part before / is not an IP address")?;
                if prefix.is_empty() || !prefix.bytes().all(|b| b.is_ascii_digit()) {
                    return Err("the part after / is not a whole number");
                }
                let prefix: u8 = prefix
                    .parse()
                    .map_err(|_| "the part after / is not a whole number")?;
                let most = match address {
                    IpAddr::V4(_) => 32,
                    IpAddr::V6(_) => 128,
                };
                if prefix > most {
                    return Err("the part after / is more than the address has bits");
                }
                Ok(Self { address, prefix })
            }
            None => {
                let address: IpAddr = text
                    .parse()
                    .map_err(|_| "it is neither an IP address nor a range in CIDR form")?;
                let prefix = match address {
                    IpAddr::V4(_) => 32,
                    IpAddr::V6(_) => 128,
                };
                Ok(Self { address, prefix })
            }
        }
    }

    /// Whether `address` is in the range, of its own family; an IPv4-mapped
    /// IPv6 address is judged by the IPv4 address it carries as well.
    fn contains(self, address: IpAddr) -> bool {
        match (self.address, address) {
            (IpAddr::V4(base), IpAddr::V4(address)) => {
                let mask = v4_mask(self.prefix);
                u32::from(base) & mask == u32::from(address) & mask
            }
            (IpAddr::V6(base), IpAddr::V6(address)) => {
                let mask = v6_mask(self.prefix);
                u128::from(base) & mask == u128::from(address) & mask
            }
            (IpAddr::V4(_), IpAddr::V6(address)) => address
                .to_ipv4_mapped()
                .is_some_and(|v4| self.contains(IpAddr::V4(v4))),
            (IpAddr::V6(_), IpAddr::V4(_)) => false,
        }
    }
}

/// The mask of `prefix` bits of an IPv4 address: `0` for none, so that no
/// shift is by 32.
fn v4_mask(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - u32::from(prefix))
    }
}

/// The mask of `prefix` bits of an IPv6 address.
fn v6_mask(prefix: u8) -> u128 {
    if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - u32::from(prefix))
    }
}

/// The addresses the operator opened, and how they were written.
#[derive(Clone, Debug, Default)]
pub struct PrivateRanges {
    /// As given, in the order given: the `listening` record's
    /// `outbound_private`.
    given: Vec<String>,
    ranges: Vec<Range>,
}

impl PrivateRanges {
    /// None opened.
    pub fn none() -> Self {
        Self::default()
    }

    /// `values` as the command line gave them. `Err` with why, naming the
    /// value that is not one; a 17th is an error too.
    pub fn parse(values: &[String]) -> Result<Self, String> {
        if values.len() > MOST {
            return Err(format!(
                "at most {MOST} ranges may be opened, {} are given",
                values.len()
            ));
        }
        let mut ranges = Vec::with_capacity(values.len());
        for value in values {
            let range = Range::parse(value)
                .map_err(|why| format!("{value:?} is not an address or a range: {why}"))?;
            ranges.push(range);
        }
        Ok(Self {
            given: values.to_vec(),
            ranges,
        })
    }

    /// Whether the operator opened `address`.
    pub fn contains(&self, address: IpAddr) -> bool {
        self.ranges.iter().any(|range| range.contains(address))
    }

    /// The ranges as they were given, in their order.
    pub fn as_given(&self) -> &[String] {
        &self.given
    }

    /// Whether nothing was opened.
    pub fn is_none(&self) -> bool {
        self.ranges.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(values: &[&str]) -> PrivateRanges {
        let values: Vec<String> = values.iter().map(|v| (*v).to_owned()).collect();
        PrivateRanges::parse(&values).unwrap_or_else(|e| panic!("{values:?}: {e}"))
    }

    fn address(text: &str) -> IpAddr {
        text.parse().unwrap_or_else(|e| panic!("{text:?}: {e}"))
    }

    #[test]
    fn a_range_contains_its_addresses_and_nothing_beside_them() {
        let opened = parse(&["10.9.0.0/24"]);
        assert!(opened.contains(address("10.9.0.1")));
        assert!(opened.contains(address("10.9.0.255")));
        assert!(!opened.contains(address("10.9.1.1")));
        assert!(!opened.contains(address("10.8.255.255")));
        let opened = parse(&["fd00::/8"]);
        assert!(opened.contains(address("fd12:3456::1")));
        assert!(!opened.contains(address("fe80::1")));
        // A range of one family holds nothing of the other.
        assert!(!opened.contains(address("10.9.0.1")));
        assert!(!parse(&["10.9.0.0/24"]).contains(address("fd12:3456::1")));
    }

    #[test]
    fn an_ipv4_mapped_ipv6_address_is_judged_by_the_address_it_carries() {
        let opened = parse(&["10.9.0.0/24", "::1"]);
        assert!(opened.contains(address("::ffff:10.9.0.5")));
        assert!(!opened.contains(address("::ffff:10.9.1.5")));
        assert!(!opened.contains(address("::ffff:127.0.0.1")));
        // A range of IPv6 addresses reads it as the IPv6 address it is.
        let v6 = parse(&["::ffff:0:0/96"]);
        assert!(v6.contains(address("::ffff:10.9.0.5")));
        assert!(!v6.contains(address("10.9.0.5")));
        // The other forms that carry an IPv4 address are the caller's to
        // judge: a range of IPv4 addresses does not open them.
        assert!(!opened.contains(address("64:ff9b::10.9.0.5")));
        assert!(!opened.contains(address("2002:a09:5::")));
        assert!(parse(&["64:ff9b::/96"]).contains(address("64:ff9b::10.9.0.5")));
    }

    #[test]
    fn an_address_alone_contains_only_itself() {
        let opened = parse(&["169.254.169.254", "::1"]);
        assert!(opened.contains(address("169.254.169.254")));
        assert!(!opened.contains(address("169.254.169.253")));
        assert!(opened.contains(address("::1")));
        assert!(!opened.contains(address("::2")));
    }

    #[test]
    fn a_whole_family_is_opened_by_its_zero_prefix() {
        assert!(parse(&["0.0.0.0/0"]).contains(address("203.0.113.7")));
        assert!(parse(&["::/0"]).contains(address("2001:db8::7")));
    }

    #[test]
    fn the_ranges_are_kept_as_they_were_given_in_their_order() {
        let given = ["10.9.0.0/24", "10.9.1.5", "::1"];
        assert_eq!(parse(&given).as_given(), given);
        assert!(PrivateRanges::none().as_given().is_empty());
        assert!(PrivateRanges::none().is_none());
    }

    #[test]
    fn what_is_not_an_address_or_a_range_is_refused() {
        let sixteen: Vec<String> = (0..16).map(|n| format!("10.{n}.0.0/16")).collect();
        assert!(PrivateRanges::parse(&sixteen).is_ok());
        let seventeen: Vec<String> = (0..17).map(|n| format!("10.{n}.0.0/16")).collect();
        assert!(PrivateRanges::parse(&seventeen).is_err());
        for bad in [
            "10.0.0.0/33",
            "10.0.0.0/",
            "10.0.0.0/-1",
            "10.0.0.0/8/8",
            "10.0.0/8",
            "300.0.0.0/8",
            "::/129",
            "banana",
            "example.com",
            "api.example.com/24",
            "http://10.0.0.1",
            "10.0.0.0 /8",
            "",
        ] {
            let values = vec![bad.to_owned()];
            assert!(PrivateRanges::parse(&values).is_err(), "{bad:?}");
        }
    }
}
