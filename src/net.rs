use std::net::Ipv4Addr;

use local_ip_address::list_afinet_netifas;

/// 判断 IPv4 是否「看起来像」真实的局域网地址。
///
/// 过滤规则（保守，宁可多列也不误杀）：
/// - 127/8 loopback
/// - 169.254/16 link-local（DHCP 失败时的自动配置）
/// - 192.18/15 RFC 2544 benchmark 保留段（192.18.0.0 - 192.19.255.255），
///   部分本地 VPN（OpenVPN / Clash 等）默认用它做内部地址
/// - 198.18/15 RFC 2544 / RFC 6815 benchmark 保留段（198.18.0.0 - 198.19.255.255），
///   同样常被 VPN（Clash / OpenVPN 等）用作内部地址
/// - 192.168.56/21 VirtualBox 默认 host-only 网段（192.168.56.0 - 192.168.63.255）
///
/// 不在此函数过滤 WSL / Docker / Hyper-V —— 它们和正常公司内网都用 172.16/12
/// 或 10/8，IP 段无法可靠区分；改由 `is_virtual_interface` 用接口名识别，
/// 在 `list_local_ipv4s` 里整体排除。
pub fn is_likely_lan(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    if o[0] == 127 {
        return false;
    }
    if o[0] == 169 && o[1] == 254 {
        return false;
    }
    // RFC 2544: 192.18.0.0/15（192.18.x.x - 192.19.x.x）
    if o[0] == 192 && (o[1] & 0xFE) == 18 {
        return false;
    }
    // RFC 2544 / RFC 6815: 198.18.0.0/15（198.18.x.x - 198.19.x.x）
    if o[0] == 198 && (o[1] & 0xFE) == 18 {
        return false;
    }
    // VirtualBox 默认 host-only: 192.168.56.0/21（56-63）
    if o[0] == 192 && o[1] == 168 && (o[2] & 0xF8) == 56 {
        return false;
    }
    true
}

/// 虚拟网卡 / VPN 隧道接口的命名关键字。匹配到的接口默认从候选中排除。
///
/// 取自 Windows / macOS / Linux 上各虚拟化 / 容器 / VPN 工具的常见命名：
/// - `vethernet`：Hyper-V 虚拟网卡（含 WSL2 vEthernet (WSL)、Default Switch 等）
/// - `wsl`：WSL2 子系统网卡（Linux 接口名也可能含 wsl）
/// - `docker`：Docker bridge / NAT
/// - `vmware` / `virtualbox` / `virtual pc`：虚拟机宿主侧虚拟网卡
/// - `hyper-v`：Hyper-V 极少数未走 vEthernet 前缀的情况
/// - `openvpn` / `wireguard` / `tun` / `tap`：用户态 VPN 隧道
/// - `utun`：macOS IKEv2 / WireGuard 隧道接口
/// - `virbr`：libvirt 默认网桥
/// - `br-`：Docker Compose 自定义网桥
fn is_virtual_interface(name: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "wsl",
        "vethernet",
        "docker",
        "vmware",
        "virtualbox",
        "virtual pc",
        "hyper-v",
        "openvpn",
        "wireguard",
        "utun",
        "virbr",
        "br-",
        "tunnel adapter",
        "tap-windows",
    ];
    let lower = name.to_ascii_lowercase();
    KEYWORDS.iter().any(|k| lower.contains(k))
}

/// 列出所有「看起来像」局域网的 IPv4，按地址排序去重。
///
/// 优先排除虚拟网卡（WSL / Docker / Hyper-V / VMware / VPN 隧道等，
/// 靠 `is_virtual_interface` 用接口名识别）；若全部网卡都被识别为虚拟
/// 网卡，则回退到不过滤，保证用户至少能看到候选 IP。
pub fn list_local_ipv4s() -> Vec<Ipv4Addr> {
    let all: Vec<(String, Ipv4Addr)> = list_afinet_netifas()
        .map(|interfaces| {
            interfaces
                .into_iter()
                .filter_map(|(name, ip)| match ip {
                    std::net::IpAddr::V4(v4) if is_likely_lan(v4) => Some((name, v4)),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    let mut physical: Vec<Ipv4Addr> = all
        .iter()
        .filter(|(name, _)| !is_virtual_interface(name))
        .map(|(_, ip)| *ip)
        .collect();
    if physical.is_empty() {
        physical = all.into_iter().map(|(_, ip)| ip).collect();
    }
    physical.sort();
    physical.dedup();
    physical
}

/// 在候选 IP 中按子网前缀过滤。
///
/// `prefer_subnet` 例如 "192.168.20" 或 "192.168.20." 都可以——只做
/// 字符串前缀匹配。返回过滤后的列表；若没匹配项则返回原候选列表
/// （让用户看到所有可选项，而不是空）。
pub fn filter_by_subnet<'a>(ips: &'a [Ipv4Addr], prefer_subnet: &str) -> Vec<Ipv4Addr> {
    let prefix = prefer_subnet.trim();
    if prefix.is_empty() {
        return ips.to_vec();
    }
    let matched: Vec<Ipv4Addr> = ips
        .iter()
        .filter(|ip| ip.to_string().starts_with(prefix))
        .copied()
        .collect();
    if matched.is_empty() {
        ips.to_vec()
    } else {
        matched
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn rejects_loopback() {
        assert!(!is_likely_lan(Ipv4Addr::new(127, 0, 0, 1)));
        assert!(!is_likely_lan(Ipv4Addr::new(127, 255, 255, 255)));
    }

    #[test]
    fn rejects_link_local() {
        assert!(!is_likely_lan(Ipv4Addr::new(169, 254, 0, 1)));
        assert!(!is_likely_lan(Ipv4Addr::new(169, 254, 255, 255)));
    }

    #[test]
    fn rejects_virtualbox_default_range() {
        // VirtualBox 默认 192.168.56.0/21 → 56-63
        assert!(!is_likely_lan(Ipv4Addr::new(192, 168, 56, 1)));
        assert!(!is_likely_lan(Ipv4Addr::new(192, 168, 57, 100)));
        assert!(!is_likely_lan(Ipv4Addr::new(192, 168, 63, 254)));
    }

    #[test]
    fn rejects_rfc2544_benchmark_range() {
        // RFC 2544: 192.18.0.0/15（192.18-192.19），部分 VPN 用此段
        assert!(!is_likely_lan(Ipv4Addr::new(192, 18, 0, 1)));
        assert!(!is_likely_lan(Ipv4Addr::new(192, 18, 200, 50)));
        assert!(!is_likely_lan(Ipv4Addr::new(192, 19, 255, 254)));
    }

    #[test]
    fn rejects_rfc6815_benchmark_range() {
        // RFC 2544 / RFC 6815: 198.18.0.0/15（198.18-198.19），Clash / OpenVPN 用此段
        assert!(!is_likely_lan(Ipv4Addr::new(198, 18, 0, 1)));
        assert!(!is_likely_lan(Ipv4Addr::new(198, 18, 200, 50)));
        assert!(!is_likely_lan(Ipv4Addr::new(198, 19, 255, 254)));
    }

    #[test]
    fn accepts_192_168_0_outside_virtualbox_range() {
        // 192.168.0.x / 192.168.1.x 是常见家庭路由器段，不应误杀
        assert!(is_likely_lan(Ipv4Addr::new(192, 168, 0, 1)));
        assert!(is_likely_lan(Ipv4Addr::new(192, 168, 1, 100)));
    }

    #[test]
    fn accepts_normal_private_ranges() {
        assert!(is_likely_lan(Ipv4Addr::new(192, 168, 1, 1)));
        assert!(is_likely_lan(Ipv4Addr::new(192, 168, 20, 175)));
        assert!(is_likely_lan(Ipv4Addr::new(192, 168, 100, 50)));
        assert!(is_likely_lan(Ipv4Addr::new(10, 0, 0, 1)));
        assert!(is_likely_lan(Ipv4Addr::new(172, 16, 0, 1)));
    }

    #[test]
    fn accepts_public_ips() {
        // 不过滤公网 IP（万一用户真在公网上跑）
        assert!(is_likely_lan(Ipv4Addr::new(8, 8, 8, 8)));
        assert!(is_likely_lan(Ipv4Addr::new(1, 1, 1, 1)));
    }

    #[test]
    fn filter_by_subnet_matches_prefix() {
        let ips = vec![
            Ipv4Addr::new(192, 168, 20, 175),
            Ipv4Addr::new(192, 168, 56, 1), // 不应出现（VirtualBox），但测试 filter 逻辑用
            Ipv4Addr::new(10, 0, 0, 1),
        ];
        let filtered = filter_by_subnet(&ips, "192.168.20");
        assert_eq!(filtered, vec![Ipv4Addr::new(192, 168, 20, 175)]);
    }

    #[test]
    fn filter_by_subnet_returns_all_when_no_match() {
        let ips = vec![
            Ipv4Addr::new(192, 168, 20, 175),
            Ipv4Addr::new(10, 0, 0, 1),
        ];
        let filtered = filter_by_subnet(&ips, "172.16");
        // 不匹配 → 返回所有候选（让用户看到全部可选）
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn filter_by_subnet_empty_prefix_returns_all() {
        let ips = vec![Ipv4Addr::new(192, 168, 1, 1)];
        assert_eq!(filter_by_subnet(&ips, ""), ips);
        assert_eq!(filter_by_subnet(&ips, "   "), ips);
    }

    #[test]
    fn detects_wsl_vethernet_interface() {
        // Windows 上 Hyper-V / WSL2 网卡命名格式
        assert!(is_virtual_interface("vEthernet (WSL)"));
        assert!(is_virtual_interface("vEthernet (Default Switch)"));
        assert!(is_virtual_interface("vEthernet (LAN)"));
    }

    #[test]
    fn detects_docker_vmware_interfaces() {
        assert!(is_virtual_interface("DockerNAT"));
        assert!(is_virtual_interface("VMware Network Adapter VMnet1"));
        assert!(is_virtual_interface("VirtualBox Host-Only Ethernet Adapter"));
        assert!(is_virtual_interface("docker0"));
        assert!(is_virtual_interface("br-internal_net"));
        assert!(is_virtual_interface("virbr0"));
    }

    #[test]
    fn detects_vpn_tunnel_interfaces() {
        assert!(is_virtual_interface("OpenVPN TAP-Windows6"));
        assert!(is_virtual_interface("utun0"));
        assert!(is_virtual_interface("WireGuard Adapter"));
    }

    #[test]
    fn keeps_real_nic_names() {
        // 真实网卡常见命名（中英文）都不应误杀
        assert!(!is_virtual_interface("以太网"));
        assert!(!is_virtual_interface("Ethernet"));
        assert!(!is_virtual_interface("Wi-Fi"));
        assert!(!is_virtual_interface("无线网络连接"));
        assert!(!is_virtual_interface("en0"));
        assert!(!is_virtual_interface("eth0"));
    }
}
