//! Client IPs behind trusted proxies (SPEC §5.2, §11.10).

use std::net::IpAddr;

use axum::http::HeaderMap;

/// An IPv4 or IPv6 network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    addr: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// Parses `10.0.0.0/8`, `fd00::/8` or a single address.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let (addr, prefix) = match s.split_once('/') {
            Some((a, p)) => (a.parse::<IpAddr>().ok()?, p.parse::<u8>().ok()?),
            None => {
                let a = s.parse::<IpAddr>().ok()?;
                (a, if a.is_ipv4() { 32 } else { 128 })
            }
        };
        let max = if addr.is_ipv4() { 32 } else { 128 };
        (prefix <= max).then_some(Self { addr, prefix })
    }

    /// Parses a comma-separated list; invalid entries are returned as errors.
    pub fn parse_list(s: &str) -> Result<Vec<Self>, String> {
        s.split(',')
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(|x| Self::parse(x).ok_or_else(|| format!("invalid CIDR {x:?}")))
            .collect()
    }

    /// Whether `ip` is inside this network.
    pub fn contains(&self, ip: IpAddr) -> bool {
        fn bits(ip: IpAddr) -> (u128, u8) {
            match ip {
                IpAddr::V4(v) => (u128::from(u32::from(v)), 32),
                IpAddr::V6(v) => (u128::from(v), 128),
            }
        }
        let ip = match ip {
            IpAddr::V6(v) => v.to_ipv4_mapped().map_or(ip, IpAddr::V4),
            v => v,
        };
        let ((a, aw), (b, bw)) = (bits(self.addr), bits(ip));
        if aw != bw {
            return false;
        }
        if self.prefix == 0 {
            return true;
        }
        let shift = u32::from(aw - self.prefix);
        (a >> shift) == (b >> shift)
    }
}

/// The client IP: the socket peer, or, when the peer is a trusted proxy, the
/// right-most address in `X-Forwarded-For` that is not itself trusted.
pub fn client_ip(peer: Option<IpAddr>, headers: &HeaderMap, trusted: &[Cidr]) -> Option<IpAddr> {
    let peer = peer?;
    if !trusted.iter().any(|c| c.contains(peer)) {
        return Some(peer);
    }
    let forwarded: Vec<IpAddr> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|x| x.trim().parse().ok())
        .collect();
    Some(
        forwarded
            .into_iter()
            .rev()
            .find(|ip| !trusted.iter().any(|c| c.contains(*ip)))
            .unwrap_or(peer),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn cidrs() {
        let c = Cidr::parse("10.0.0.0/8").unwrap_or_else(|| unreachable!());
        assert!(c.contains("10.1.2.3".parse().unwrap_or_else(|_| unreachable!())));
        assert!(!c.contains("11.0.0.1".parse().unwrap_or_else(|_| unreachable!())));
        assert!(
            Cidr::parse("fd00::/8")
                .is_some_and(|c| c.contains("fd12::1".parse().unwrap_or_else(|_| unreachable!())))
        );
        assert!(Cidr::parse("1.2.3.4/33").is_none());
        assert!(Cidr::parse_list("10.0.0.0/8, bogus").is_err());
    }

    #[test]
    fn forwarded_only_from_trusted_proxies() {
        let mut h = HeaderMap::new();
        h.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.9, 10.0.0.2"),
        );
        let trusted = Cidr::parse_list("10.0.0.0/8").unwrap_or_default();
        let peer = "10.0.0.1".parse().ok();
        assert_eq!(client_ip(peer, &h, &trusted), "203.0.113.9".parse().ok());
        let untrusted_peer = "198.51.100.7".parse().ok();
        assert_eq!(
            client_ip(untrusted_peer, &h, &trusted),
            untrusted_peer,
            "spoofed header ignored"
        );
    }
}
