use std::net::Ipv4Addr;

use local_ip_address::local_ip;

/// 找一个非 loopback 的局域网 IPv4。
pub fn get_local_ipv4() -> Option<Ipv4Addr> {
    local_ip().ok().and_then(|ip| match ip {
        std::net::IpAddr::V4(v4) if !v4.is_loopback() => Some(v4),
        _ => None,
    })
}
