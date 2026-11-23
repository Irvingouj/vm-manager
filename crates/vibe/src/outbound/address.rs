//! Which addresses an outbound call never connects to, and the decision on
//! the addresses a name resolved to (blackbox/CONTRACT.md, "Outbound calls:
//! Who may call what"): loopback, private, link-local, the shared range,
//! `0.0.0.0/8` and `::`, multicast and reserved, the ranges for documentation
//! and benchmarks, and an IPv6 address that carries an IPv4 one, judged by
//! the address it carries. The operator of the machine may open some of them
//! ([`PrivateRanges`]); nothing else opens any.
//!
//! The decision is on **all** the addresses of a name: one that is never
//! called, and not opened, refuses the call, even when another of its
//! addresses is public. What is connected to afterwards is one of the
//! addresses that were checked, never the name again.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use super::PrivateRanges;

/// The most addresses of one name that are looked at.
pub const MOST_ADDRESSES: usize = 16;

/// The IPv4 address an IPv6 address carries, when it is of one of the forms
/// that carry one: `::ffff:0:0/96` (mapped), `::/96` (compatible),
/// `64:ff9b::/96` (the well-known NAT64 prefix) and `2002::/16` (6to4).
pub fn carried(address: Ipv6Addr) -> Option<Ipv4Addr> {
    let octets = address.octets();
    let [
        o0,
        o1,
        o2,
        o3,
        o4,
        o5,
        o6,
        o7,
        o8,
        o9,
        o10,
        o11,
        o12,
        o13,
        o14,
        o15,
    ] = octets;
    let last = Ipv4Addr::new(o12, o13, o14, o15);
    let zeros = [o0, o1, o2, o3, o4, o5, o6, o7, o8, o9];
    if zeros.iter().all(|byte| *byte == 0)
        && ((o10 == 0xff && o11 == 0xff) || (o10 == 0 && o11 == 0))
    {
        // `::ffff:0:0/96` and `::/96`.
        return Some(last);
    }
    if o0 == 0x00
        && o1 == 0x64
        && o2 == 0xff
        && o3 == 0x9b
        && [o4, o5, o6, o7, o8, o9, o10, o11]
            .iter()
            .all(|byte| *byte == 0)
    {
        return Some(last);
    }
    if o0 == 0x20 && o1 == 0x02 {
        return Some(Ipv4Addr::new(o2, o3, o4, o5));
    }
    None
}

/// Whether the platform never calls this IPv4 address.
fn never_v4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    a == 0
        || a == 10
        || (a == 100 && (64..=127).contains(&b))
        || a == 127
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 0 && (c == 0 || c == 2))
        // The old 6to4 relay anycast range.
        || (a == 192 && b == 88 && c == 99)
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        // Multicast and everything reserved after it, the broadcast
        // address included.
        || a >= 224
}

/// Whether the platform never calls this address, whatever the list says
/// (the operator's own ranges are asked separately).
pub fn never_called(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => never_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = carried(v6) {
                return never_v4(v4);
            }
            let [s0, s1, s2, s3, s4, s5, _, _] = v6.segments();
            // `::1` and `::` carry `0.0.0.x` and are refused above; the
            // other forms: unique local, link-local, the old site-local
            // range, multicast, documentation, and the forms made to carry
            // or reach an IPv4 address that are judged whole, not by the
            // address they carry: `::ffff:0:0:0/96`, `64:ff9b:1::/48` and
            // Teredo's `2001::/32`.
            (s0 & 0xfe00) == 0xfc00
                || (s0 & 0xffc0) == 0xfe80
                || (s0 & 0xffc0) == 0xfec0
                || (s0 & 0xff00) == 0xff00
                || (s0 == 0x2001 && s1 == 0x0db8)
                || (s0 == 0x2001 && s1 == 0)
                || (s0 == 0x64 && s1 == 0xff9b && s2 == 1)
                || (s0 == 0 && s1 == 0 && s2 == 0 && s3 == 0 && s4 == 0xffff && s5 == 0)
        }
    }
}

/// Whether the operator opened `address`: as it is written, or by the IPv4
/// address it carries.
fn opened(address: IpAddr, private: &PrivateRanges) -> bool {
    if private.contains(address) {
        return true;
    }
    match address {
        IpAddr::V6(v6) => carried(v6).is_some_and(|v4| private.contains(IpAddr::V4(v4))),
        IpAddr::V4(_) => false,
    }
}

/// Whether the call is refused `address-not-allowed` for these addresses:
/// at least one of them is never called and was not opened by the operator.
/// No address at all refuses too (nothing is checked, so nothing is called).
pub fn refuses(addresses: &[IpAddr], private: &PrivateRanges) -> bool {
    addresses.is_empty()
        || addresses
            .iter()
            .any(|address| never_called(*address) && !opened(*address, private))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap_or_else(|e| panic!("{text:?}: {e}"))
    }

    fn ranges(values: &[&str]) -> PrivateRanges {
        let values: Vec<String> = values.iter().map(|v| (*v).to_owned()).collect();
        PrivateRanges::parse(&values).unwrap_or_else(|e| panic!("{values:?}: {e}"))
    }

    #[test]
    fn every_kind_the_contract_names_is_never_called() {
        for text in [
            "127.0.0.1",
            "127.255.255.254",
            "10.0.0.1",
            "10.255.255.255",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.0.1",
            "192.168.255.255",
            "169.254.169.254",
            "169.254.0.1",
            "100.64.0.1",
            "100.127.255.255",
            "0.0.0.0",
            "0.1.2.3",
            "224.0.0.1",
            "239.255.255.255",
            "240.0.0.1",
            "255.255.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.1",
            "203.0.113.7",
            "::1",
            "::",
            "fc00::1",
            "fd12:3456:789a::1",
            "fe80::1",
            "febf::1",
            "ff02::1",
            "2001:db8::7",
            "2001:db8:ffff::1",
            "::ffff:7f00:1",
            "::ffff:a00:1",
            "::ffff:a9fe:a9fe",
            "64:ff9b::7f00:1",
            "64:ff9b::a00:1",
            "2002:7f00:1::",
            "2002:a9fe:a9fe::",
            "::ffff:0:7f00:1",
            "::ffff:0:8.8.8.8",
            "64:ff9b:1::7f00:1",
            "64:ff9b:1:ffff::1",
            "2001::1",
            "2001:0:4136:e378:8000:63bf:3fff:fdd2",
            "fec0::1",
            "feff::1",
            "192.88.99.1",
        ] {
            assert!(never_called(ip(text)), "{text} is never called");
        }
    }

    #[test]
    fn a_public_address_is_called_and_the_edges_of_the_ranges_are_exact() {
        for text in [
            "8.8.8.8",
            "93.184.216.34",
            "172.15.255.255",
            "172.32.0.0",
            "100.63.255.255",
            "100.128.0.0",
            "169.253.255.255",
            "169.255.0.0",
            "192.0.1.1",
            "192.0.3.1",
            "198.17.255.255",
            "198.20.0.0",
            "198.51.99.1",
            "198.51.101.1",
            "203.0.112.1",
            "203.0.114.1",
            "223.255.255.255",
            "2606:4700::1111",
            "2001:db9::1",
            "2001:4860:4860::8888",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
            "2002:808:808::",
            "fbff::1",
            "2001:1::1",
            "64:ff9b:2::1",
            "::ffff:1:0:0",
        ] {
            assert!(!never_called(ip(text)), "{text} may be called");
        }
    }

    #[test]
    fn the_forms_that_carry_an_ipv4_address_say_which_one() {
        let v6 = |text: &str| match ip(text) {
            IpAddr::V6(v6) => v6,
            IpAddr::V4(_) => panic!("{text} is not IPv6"),
        };
        assert_eq!(
            carried(v6("::ffff:10.9.0.5")),
            Some(Ipv4Addr::new(10, 9, 0, 5))
        );
        assert_eq!(carried(v6("::10.9.0.5")), Some(Ipv4Addr::new(10, 9, 0, 5)));
        assert_eq!(
            carried(v6("64:ff9b::10.9.0.5")),
            Some(Ipv4Addr::new(10, 9, 0, 5))
        );
        assert_eq!(
            carried(v6("2002:a09:5::")),
            Some(Ipv4Addr::new(10, 9, 0, 5))
        );
        assert_eq!(carried(v6("2606:4700::1111")), None);
        assert_eq!(carried(v6("64:ff9b:1::1")), None);
        assert_eq!(carried(v6("::1:0:0:0:ffff:1")), None);
    }

    #[test]
    fn one_address_never_called_among_the_name_s_answers_refuses_the_call() {
        let none = PrivateRanges::none();
        assert!(!refuses(&[ip("8.8.8.8"), ip("2606:4700::1111")], &none));
        assert!(refuses(&[ip("8.8.8.8"), ip("10.0.0.1")], &none));
        assert!(refuses(&[ip("2606:4700::1111"), ip("::1")], &none));
        assert!(refuses(&[], &none), "no address, nothing to connect to");
    }

    #[test]
    fn the_operator_opens_what_it_names_and_only_when_every_address_is_open_or_public() {
        let open = ranges(&["127.0.0.1/32", "::1/128"]);
        assert!(!refuses(&[ip("::1"), ip("127.0.0.1")], &open));
        assert!(refuses(&[ip("127.0.0.2")], &open));
        assert!(refuses(&[ip("127.0.0.1"), ip("10.0.0.1")], &open));
        // An IPv4 range opens the IPv6 forms that carry one of its
        // addresses, and nothing else.
        let gateway = ranges(&["10.9.0.0/24"]);
        assert!(!refuses(&[ip("::ffff:10.9.0.5")], &gateway));
        assert!(!refuses(&[ip("64:ff9b::10.9.0.5")], &gateway));
        assert!(!refuses(&[ip("2002:a09:5::")], &gateway));
        assert!(refuses(&[ip("::ffff:10.9.1.5")], &gateway));
        assert!(refuses(&[ip("64:ff9b::10.9.1.5")], &gateway));
        assert!(refuses(&[ip("2002:a09:105::")], &gateway));
        assert!(refuses(&[ip("10.9.1.5")], &gateway));
    }

    #[test]
    fn the_forms_judged_whole_are_opened_by_a_range_of_their_own_form_only() {
        // `::ffff:127.0.0.1` is judged by the IPv4 address it carries.
        assert!(!refuses(
            &[ip("::ffff:127.0.0.1")],
            &ranges(&["127.0.0.1/32"])
        ));
        let wide = ranges(&["0.0.0.0/0"]);
        for text in ["::ffff:0:7f00:1", "64:ff9b:1::7f00:1", "2001::1", "fec0::1"] {
            assert!(refuses(&[ip(text)], &wide), "{text} is not opened by IPv4");
        }
        assert!(!refuses(&[ip("2001::1")], &ranges(&["2001::/32"])));
        assert!(!refuses(&[ip("fec0::1")], &ranges(&["fec0::/10"])));
        assert!(!refuses(&[ip("192.88.99.1")], &ranges(&["192.88.99.0/24"])));
        assert!(refuses(&[ip("2001::1")], &ranges(&["fec0::/10"])));
    }
}
