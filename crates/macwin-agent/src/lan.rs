//! The local network: this machine's address and where discovery queries go.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

/// The address other machines reach this one at: `local` itself when it is
/// a specific address, else the IP of the default route (found without
/// sending anything: connecting a UDP socket only picks a route).
pub fn reachable_ip(local: SocketAddr) -> Option<IpAddr> {
    if !local.ip().is_unspecified() {
        return Some(local.ip());
    }
    let probe = UdpSocket::bind("0.0.0.0:0").ok()?;
    probe.connect("192.0.2.1:9").ok()?;
    let ip = probe.local_addr().ok()?.ip();
    (!ip.is_unspecified()).then_some(ip)
}

/// Where discovery queries go: the broadcast addresses of the default
/// route's network and loopback, on `port`.
pub fn discovery_targets(port: u16) -> Vec<SocketAddr> {
    let local = reachable_ip(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)));
    macwin_net::discovery::broadcast_targets(port, local)
}
