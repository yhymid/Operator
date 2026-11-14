//! Guards for user-supplied URLs handed to yt-dlp: the bots run on a VPS next to local
//! services (Lavalink, Bot API server, cloud metadata), so links to private addresses are refused.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::time::Duration;

const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        // 100.64.0.0/10 carrier-grade NAT
        || (a == 100 && (64..128).contains(&b))
        // 192.0.0.0/24 protocol assignments
        || (a == 192 && b == 0 && c == 0)
        // 198.18.0.0/15 benchmarking
        || (a == 198 && (18..20).contains(&b))
        // 240.0.0.0/4 reserved
        || a >= 240)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let first = ip.segments()[0];
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
        // 2001:db8::/32 documentation
        || (first == 0x2001 && ip.segments()[1] == 0x0db8)
        // 64:ff9b::/96 NAT64 and ::/96 IPv4-compatible: the embedded address is not checked
        || first == 0x0064
        || ip.segments()[..6].iter().all(|s| *s == 0))
}

/// `true` for addresses reachable on the public internet.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

/// Resolves `host` (a URL host, IPv6 may be in brackets) and fails when it is not a public
/// address. Every resolved address must be public, so a name pointing at both is refused.
///
/// This does not stop a public site from redirecting yt-dlp to a private address; it closes
/// the direct route (`http://127.0.0.1:2333/...`, `http://169.254.169.254/...`, LAN hosts).
pub async fn ensure_public_host(host: &str, port: u16) -> Result<(), &'static str> {
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    if host.is_empty() {
        return Err("Brak hosta w adresie.");
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return if is_public_ip(ip) {
            Ok(())
        } else {
            Err("Adresy lokalne i prywatne są niedozwolone.")
        };
    }
    let lookup = tokio::task::spawn_blocking(move || {
        (host.as_str(), port)
            .to_socket_addrs()
            .map(|addrs| addrs.map(|a| a.ip()).collect::<Vec<_>>())
    });
    let addrs = match tokio::time::timeout(LOOKUP_TIMEOUT, lookup).await {
        Ok(Ok(Ok(addrs))) if !addrs.is_empty() => addrs,
        Ok(Ok(_)) => return Err("Nie znaleziono serwera pod tym adresem."),
        _ => return Err("Nie udało się sprawdzić adresu serwera."),
    };
    if addrs.iter().all(|ip| is_public_ip(*ip)) {
        Ok(())
    } else {
        Err("Adresy lokalne i prywatne są niedozwolone.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn classifies_addresses() {
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "2001:db8::1",
        ] {
            assert!(!is_public_ip(ip(private)), "{private} must be refused");
        }
        for public in [
            "9.9.9.9",
            "1.1.1.1",
            "2a00:1450:4001::200e",
            "::ffff:8.8.8.8",
        ] {
            assert!(is_public_ip(ip(public)), "{public} must be allowed");
        }
    }

    #[tokio::test]
    async fn literal_hosts_need_no_lookup() {
        assert!(ensure_public_host("127.0.0.1", 80).await.is_err());
        assert!(ensure_public_host("[::1]", 80).await.is_err());
        assert!(ensure_public_host("localhost", 80).await.is_err());
        assert!(ensure_public_host("1.1.1.1", 443).await.is_ok());
        assert!(ensure_public_host("", 443).await.is_err());
    }
}
