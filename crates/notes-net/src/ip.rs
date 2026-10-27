//! `ipaddress.ip_address(v).is_global` of CPython 3.14.
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

fn v4_in(addr: Ipv4Addr, net: [u8; 4], prefix: u32) -> bool {
    let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix) };
    (u32::from(addr) & mask) == (u32::from(Ipv4Addr::from(net)) & mask)
}

fn v6_in(addr: Ipv6Addr, net: Ipv6Addr, prefix: u32) -> bool {
    let mask = if prefix == 0 { 0 } else { u128::MAX << (128 - prefix) };
    (u128::from(addr) & mask) == (u128::from(net) & mask)
}

/// `IPv4Address.is_private`
fn v4_private(a: Ipv4Addr) -> bool {
    const NETS: [([u8; 4], u32); 14] = [
        ([0, 0, 0, 0], 8), ([10, 0, 0, 0], 8), ([127, 0, 0, 0], 8), ([169, 254, 0, 0], 16),
        ([172, 16, 0, 0], 12), ([192, 0, 0, 0], 24), ([192, 0, 0, 170], 31), ([192, 0, 2, 0], 24),
        ([192, 168, 0, 0], 16), ([198, 18, 0, 0], 15), ([198, 51, 100, 0], 24), ([203, 0, 113, 0], 24),
        ([240, 0, 0, 0], 4), ([255, 255, 255, 255], 32),
    ];
    const EXCEPTIONS: [([u8; 4], u32); 2] = [([192, 0, 0, 9], 32), ([192, 0, 0, 10], 32)];
    NETS.iter().any(|&(n, p)| v4_in(a, n, p)) && EXCEPTIONS.iter().all(|&(n, p)| !v4_in(a, n, p))
}

/// `IPv4Address.is_global`
fn v4_global(a: Ipv4Addr) -> bool {
    !v4_in(a, [100, 64, 0, 0], 10) && !v4_private(a)
}

const fn v6(a: u16, b: u16, c: u16, h: u16) -> Ipv6Addr {
    Ipv6Addr::new(a, b, c, 0, 0, 0, 0, h)
}

/// `IPv6Address._private_networks`: ::1, ::, ::ffff:0:0/96, 64:ff9b:1::/48,
/// 100::/64, 2001::/23, 2001:db8::/32, 2002::/16, 3fff::/20, fc00::/7,
/// fe80::/10 (constants, so no parse can fail open at runtime).
const V6_PRIVATE: [(Ipv6Addr, u32); 11] = [
    (v6(0, 0, 0, 1), 128), (v6(0, 0, 0, 0), 128), (Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0, 0), 96),
    (v6(0x64, 0xff9b, 1, 0), 48), (v6(0x100, 0, 0, 0), 64), (v6(0x2001, 0, 0, 0), 23),
    (v6(0x2001, 0xdb8, 0, 0), 32), (v6(0x2002, 0, 0, 0), 16), (v6(0x3fff, 0, 0, 0), 20),
    (v6(0xfc00, 0, 0, 0), 7), (v6(0xfe80, 0, 0, 0), 10),
];
/// `IPv6Address._private_networks_exceptions`: 2001:1::1, 2001:1::2,
/// 2001:3::/32, 2001:4:112::/48, 2001:20::/28, 2001:30::/28.
const V6_EXCEPTIONS: [(Ipv6Addr, u32); 6] = [
    (v6(0x2001, 1, 0, 1), 128), (v6(0x2001, 1, 0, 2), 128), (v6(0x2001, 3, 0, 0), 32),
    (v6(0x2001, 4, 0x112, 0), 48), (v6(0x2001, 0x20, 0, 0), 28), (v6(0x2001, 0x30, 0, 0), 28),
];

/// `IPv6Address.is_global`
fn v6_global(a: Ipv6Addr) -> bool {
    let bits = u128::from(a);
    if bits >> 32 == 0xffff {
        return v4_global(Ipv4Addr::from(bits as u32));
    }
    let private = V6_PRIVATE.iter().any(|&(n, p)| v6_in(a, n, p)) && V6_EXCEPTIONS.iter().all(|&(n, p)| !v6_in(a, n, p));
    !private
}

/// `ipaddress.ip_address(value).is_global` (a scope id does not matter).
pub fn is_global(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(a) => v4_global(a),
        IpAddr::V6(a) => v6_global(a),
    }
}

#[cfg(test)]
mod tests {
    use super::is_global;

    #[test]
    fn matches_python_ipaddress() {
        // Expected values from CPython 3.14 `ipaddress.ip_address(v).is_global`.
        let cases = [
            ("8.8.8.8", true), ("127.0.0.1", false), ("10.1.2.3", false), ("100.64.0.1", false),
            ("100.63.255.255", true), ("192.0.0.9", true), ("192.0.0.8", false), ("172.31.255.255", false),
            ("172.32.0.0", true), ("224.0.0.1", true), ("255.255.255.255", false), ("0.1.2.3", false),
            ("::1", false), ("::", false), ("2001:4860:4860::8888", true), ("::ffff:8.8.8.8", true),
            ("::ffff:127.0.0.1", false), ("fe80::1", false), ("fd00::1", false), ("2001:db8::1", false),
            ("2001:20::1", true), ("2001:1::1", true), ("2001::1", false), ("2002::1", false), ("ff02::1", true),
            ("64:ff9b::808:808", true), ("64:ff9b:1::1", false), ("3fff::1", false),
        ];
        for (text, expected) in cases {
            assert_eq!(is_global(text.parse().unwrap()), expected, "{text}");
        }
    }
}

#[cfg(test)]
mod table_tests {
    use super::*;

    /// The const tables equal the textual networks of CPython's ipaddress.
    #[test]
    fn v6_tables_match_cpython_text() {
        let private = ["::1", "::", "::ffff:0:0", "64:ff9b:1::", "100::", "2001::", "2001:db8::", "2002::", "3fff::", "fc00::", "fe80::"];
        let exceptions = ["2001:1::1", "2001:1::2", "2001:3::", "2001:4:112::", "2001:20::", "2001:30::"];
        for (text, (net, _)) in private.iter().zip(V6_PRIVATE.iter()) {
            assert_eq!(text.parse::<Ipv6Addr>().unwrap(), *net, "{text}");
        }
        for (text, (net, _)) in exceptions.iter().zip(V6_EXCEPTIONS.iter()) {
            assert_eq!(text.parse::<Ipv6Addr>().unwrap(), *net, "{text}");
        }
    }
}
