//! LAN discovery over UDP broadcast: finding a machine that waits for
//! pairing (so nobody has to type an IP address), and finding the paired
//! peer again after its address changed.
//!
//! A querier broadcasts a [`DiscoveryQuery`] a few times and collects the
//! unicast answers that echo its nonce. Peer queries name the wanted machine
//! by `SHA-256(label || nonce || fingerprint)` tags, so fingerprints never
//! travel in clear and only a machine that knows both fingerprints answers.
//! Answers are hints, never trusted: pairing is still authenticated by the
//! code and sessions by certificate pinning, so a forged answer costs at
//! most one failed attempt. IPv4 only.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use macwin_core::msg::{
    DiscoveryAnswer, DiscoveryMsg, DiscoveryOffer, DiscoveryQuery, DiscoverySeek,
};
use macwin_core::types::Os;
use macwin_proto::{
    MAX_DISCOVERY_LEN, MAX_STRING_LEN, PROTOCOL_VERSION, check_versions, decode_discovery,
    encode_discovery,
};
use ring::digest;
use ring::rand::{SecureRandom, SystemRandom};
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;
use tracing::debug;

use crate::NetError;
use crate::fingerprint::Fingerprint;

/// The UDP port machines answer discovery queries on.
pub const DISCOVERY_PORT: u16 = 47801;
/// How long a query collects answers.
pub const DISCOVERY_WAIT: Duration = Duration::from_millis(1500);
/// Pause between repeats of one query (broadcasts are easily lost).
const RESEND: Duration = Duration::from_millis(400);

const TARGET_LABEL: &[u8] = b"macwin-discovery-target-v1";
const SENDER_LABEL: &[u8] = b"macwin-discovery-sender-v1";
const PROOF_LABEL: &[u8] = b"macwin-discovery-proof-v1";

/// `SHA-256(label || nonce || fingerprint)`; every part has a fixed length.
fn tag(label: &[u8], nonce: &[u8; 16], fp: &Fingerprint) -> [u8; 32] {
    let mut ctx = digest::Context::new(&digest::SHA256);
    ctx.update(label);
    ctx.update(nonce);
    ctx.update(&fp.0);
    let mut out = [0u8; 32];
    out.copy_from_slice(ctx.finish().as_ref());
    out
}

fn new_nonce() -> Result<[u8; 16], NetError> {
    let mut nonce = [0u8; 16];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| NetError::Io(std::io::Error::other("no system randomness")))?;
    Ok(nonce)
}

/// The longest prefix of `s` that fits a protocol string.
fn truncated(s: &str) -> String {
    let mut end = s.len().min(MAX_STRING_LEN);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// What a [`Responder`] answers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Offer {
    /// This machine waits for pairing on QUIC `port`.
    Pairing {
        port: u16,
        os: Os,
        device_name: String,
        fingerprint: Fingerprint,
    },
    /// This machine (`own`) accepts sessions from its paired `peer` on QUIC
    /// `port`.
    Peer {
        port: u16,
        own: Fingerprint,
        peer: Fingerprint,
    },
}

impl Offer {
    /// The answer to `query`, if this offer matches it.
    fn answer(&self, query: &DiscoveryQuery) -> Option<DiscoveryAnswer> {
        check_versions(PROTOCOL_VERSION, query.protocol_version).ok()?;
        let (port, offer) = match (self, &query.seek) {
            (
                Offer::Pairing {
                    port,
                    os,
                    device_name,
                    fingerprint,
                },
                DiscoverySeek::Pairing,
            ) => (
                *port,
                DiscoveryOffer::Pairing {
                    os: *os,
                    device_name: truncated(device_name),
                    fingerprint: fingerprint.0,
                },
            ),
            (Offer::Peer { port, own, peer }, DiscoverySeek::Peer { target, sender })
                if *target == tag(TARGET_LABEL, &query.nonce, own)
                    && *sender == tag(SENDER_LABEL, &query.nonce, peer) =>
            {
                (
                    *port,
                    DiscoveryOffer::Peer {
                        proof: tag(PROOF_LABEL, &query.nonce, own),
                    },
                )
            }
            _ => return None,
        };
        Some(DiscoveryAnswer {
            protocol_version: PROTOCOL_VERSION,
            nonce: query.nonce,
            port,
            offer,
        })
    }
}

/// Answers discovery queries in the background until dropped.
pub struct Responder {
    local: SocketAddr,
    task: JoinHandle<()>,
}

impl Responder {
    /// Binds `bind` (normally `0.0.0.0:47801`; a socket bound to a unicast
    /// address misses broadcasts) and answers the queries `offer` matches.
    pub async fn bind(bind: SocketAddr, offer: Offer) -> Result<Responder, NetError> {
        let socket = UdpSocket::bind(bind).await?;
        let local = socket.local_addr()?;
        let task = tokio::spawn(respond(socket, offer));
        Ok(Responder { local, task })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn respond(socket: UdpSocket, offer: Offer) {
    let mut buf = vec![0u8; MAX_DISCOVERY_LEN + 1];
    loop {
        let (n, from) = match socket.recv_from(&mut buf).await {
            Ok(x) => x,
            Err(e) => {
                // Windows reports ICMP "port unreachable" for an earlier
                // answer here; the socket keeps working.
                debug!(error = %e, "discovery receive failed");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let Ok(DiscoveryMsg::Query(query)) = decode_discovery(&buf[..n]) else {
            continue;
        };
        if let Some(answer) = offer.answer(&query) {
            let bytes = encode_discovery(&DiscoveryMsg::Answer(answer));
            match socket.send_to(&bytes, from).await {
                Ok(_) => debug!(%from, "answered a discovery query"),
                Err(e) => debug!(%from, error = %e, "cannot answer a discovery query"),
            }
        }
    }
}

/// A machine that waits for pairing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waiting {
    /// Where its pairing listener is.
    pub addr: SocketAddr,
    pub os: Os,
    pub device_name: String,
    pub fingerprint: Fingerprint,
}

/// Sends `query` to `targets` every [`RESEND`] and hands every answer that
/// echoes its nonce to `on_answer` (with the answer's source address) until
/// `wait` has passed or `on_answer` returns true.
async fn ask(
    targets: &[SocketAddr],
    query: &DiscoveryQuery,
    wait: Duration,
    mut on_answer: impl FnMut(DiscoveryAnswer, SocketAddr) -> bool,
) -> Result<(), NetError> {
    let local = if targets.iter().all(|t| t.ip().is_loopback()) {
        Ipv4Addr::LOCALHOST
    } else {
        Ipv4Addr::UNSPECIFIED
    };
    let socket = UdpSocket::bind((local, 0)).await?;
    socket.set_broadcast(true)?;
    let bytes = encode_discovery(&DiscoveryMsg::Query(query.clone()));
    let deadline = tokio::time::Instant::now() + wait;
    let mut resend = tokio::time::interval(RESEND);
    let mut buf = vec![0u8; MAX_DISCOVERY_LEN + 1];
    loop {
        tokio::select! {
            _ = resend.tick() => {
                for target in targets {
                    if let Err(e) = socket.send_to(&bytes, target).await {
                        debug!(%target, error = %e, "discovery send failed");
                    }
                }
            }
            r = socket.recv_from(&mut buf) => match r {
                Ok((n, from)) => {
                    if let Ok(DiscoveryMsg::Answer(a)) = decode_discovery(&buf[..n])
                        && a.nonce == query.nonce
                        && check_versions(PROTOCOL_VERSION, a.protocol_version).is_ok()
                        && on_answer(a, from)
                    {
                        return Ok(());
                    }
                }
                // Windows reports ICMP "port unreachable" (for example from
                // loopback) as a receive error; keep listening.
                Err(e) => debug!(error = %e, "discovery receive failed"),
            },
            () = tokio::time::sleep_until(deadline) => return Ok(()),
        }
    }
}

/// Asks `targets` for machines waiting for pairing and returns each one
/// (once per fingerprint) that answered within `wait`.
pub async fn find_pairing(
    targets: &[SocketAddr],
    wait: Duration,
) -> Result<Vec<Waiting>, NetError> {
    let query = DiscoveryQuery {
        protocol_version: PROTOCOL_VERSION,
        nonce: new_nonce()?,
        seek: DiscoverySeek::Pairing,
    };
    let mut found: Vec<Waiting> = Vec::new();
    ask(targets, &query, wait, |a, from| {
        if let DiscoveryOffer::Pairing {
            os,
            device_name,
            fingerprint,
        } = a.offer
        {
            let fingerprint = Fingerprint(fingerprint);
            if !found.iter().any(|w| w.fingerprint == fingerprint) {
                found.push(Waiting {
                    addr: SocketAddr::new(from.ip(), a.port),
                    os,
                    device_name,
                    fingerprint,
                });
            }
        }
        false
    })
    .await?;
    Ok(found)
}

/// Asks `targets`, on behalf of the machine `own`, where its paired `peer`
/// accepts sessions; the first valid answer within `wait` wins.
pub async fn find_peer(
    targets: &[SocketAddr],
    own: Fingerprint,
    peer: Fingerprint,
    wait: Duration,
) -> Result<Option<SocketAddr>, NetError> {
    let nonce = new_nonce()?;
    let query = DiscoveryQuery {
        protocol_version: PROTOCOL_VERSION,
        nonce,
        seek: DiscoverySeek::Peer {
            target: tag(TARGET_LABEL, &nonce, &peer),
            sender: tag(SENDER_LABEL, &nonce, &own),
        },
    };
    let proof = tag(PROOF_LABEL, &nonce, &peer);
    let mut found = None;
    ask(targets, &query, wait, |a, from| match a.offer {
        DiscoveryOffer::Peer { proof: p } if p == proof => {
            found = Some(SocketAddr::new(from.ip(), a.port));
            true
        }
        _ => false,
    })
    .await?;
    Ok(found)
}

/// Where queries go, all on `port`: the limited broadcast, the /24
/// broadcast of `local` (the address of the default route, when known; it
/// helps where the limited broadcast leaves on another interface, and most
/// home networks are /24) and loopback.
pub fn broadcast_targets(port: u16, local: Option<IpAddr>) -> Vec<SocketAddr> {
    let mut targets = vec![SocketAddr::from((Ipv4Addr::BROADCAST, port))];
    if let Some(IpAddr::V4(ip)) = local
        && !ip.is_loopback()
        && !ip.is_unspecified()
    {
        let [a, b, c, _] = ip.octets();
        targets.push(SocketAddr::from((Ipv4Addr::new(a, b, c, 255), port)));
    }
    targets.push(SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
    targets
}

/// The address a [`Responder`] should bind for a QUIC listener on `listen`:
/// loopback listeners (tests) answer on loopback, every other one on all
/// interfaces so broadcasts reach it.
pub fn responder_addr(listen: SocketAddr, port: u16) -> SocketAddr {
    if listen.ip().is_loopback() {
        SocketAddr::new(listen.ip(), port)
    } else {
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, port))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    const SHORT: Duration = Duration::from_millis(300);

    fn fp(b: u8) -> Fingerprint {
        Fingerprint([b; 32])
    }

    fn pairing_offer(port: u16, name: &str, f: u8) -> Offer {
        Offer::Pairing {
            port,
            os: Os::Windows,
            device_name: name.into(),
            fingerprint: fp(f),
        }
    }

    fn peer_query(nonce: [u8; 16], target: Fingerprint, sender: Fingerprint) -> DiscoveryQuery {
        DiscoveryQuery {
            protocol_version: PROTOCOL_VERSION,
            nonce,
            seek: DiscoverySeek::Peer {
                target: tag(TARGET_LABEL, &nonce, &target),
                sender: tag(SENDER_LABEL, &nonce, &sender),
            },
        }
    }

    #[test]
    fn tags_depend_on_every_input() {
        let base = tag(TARGET_LABEL, &[1; 16], &fp(2));
        assert_ne!(base, tag(SENDER_LABEL, &[1; 16], &fp(2)));
        assert_ne!(base, tag(TARGET_LABEL, &[3; 16], &fp(2)));
        assert_ne!(base, tag(TARGET_LABEL, &[1; 16], &fp(4)));
    }

    #[test]
    fn offers_answer_only_matching_queries() {
        let pairing = DiscoveryQuery {
            protocol_version: PROTOCOL_VERSION,
            nonce: [7; 16],
            seek: DiscoverySeek::Pairing,
        };
        let waiting = pairing_offer(47800, "pc", 1);
        let a = waiting.answer(&pairing).expect("answers pairing queries");
        assert_eq!((a.nonce, a.port), ([7; 16], 47800));
        assert!(
            matches!(a.offer, DiscoveryOffer::Pairing { fingerprint, .. } if fingerprint == [1; 32])
        );
        // A newer minor version is fine, another major is not.
        let mut newer = pairing.clone();
        newer.protocol_version = PROTOCOL_VERSION + 1;
        assert!(waiting.answer(&newer).is_some());
        let mut other = pairing.clone();
        other.protocol_version = 0x0002_0000;
        assert!(waiting.answer(&other).is_none());

        let running = Offer::Peer {
            port: 5000,
            own: fp(1),
            peer: fp(2),
        };
        assert!(
            running.answer(&pairing).is_none(),
            "not waiting for pairing"
        );
        assert!(waiting.answer(&peer_query([1; 16], fp(1), fp(2))).is_none());
        let a = running
            .answer(&peer_query([1; 16], fp(1), fp(2)))
            .expect("its paired peer asks for it");
        assert_eq!(
            a.offer,
            DiscoveryOffer::Peer {
                proof: tag(PROOF_LABEL, &[1; 16], &fp(1))
            }
        );
        assert!(
            running.answer(&peer_query([1; 16], fp(3), fp(2))).is_none(),
            "another machine"
        );
        assert!(
            running.answer(&peer_query([1; 16], fp(1), fp(3))).is_none(),
            "another asker"
        );
    }

    #[test]
    fn long_names_are_cut_at_a_char_boundary() {
        let name = "é".repeat(200);
        let cut = truncated(&name);
        assert!(cut.len() <= MAX_STRING_LEN && name.starts_with(&cut));
        assert_eq!(truncated("pc"), "pc");
    }

    #[test]
    fn targets_and_responder_addresses() {
        let t = broadcast_targets(47801, Some("192.168.1.37".parse().unwrap()));
        let want: Vec<SocketAddr> = [
            "255.255.255.255:47801",
            "192.168.1.255:47801",
            "127.0.0.1:47801",
        ]
        .iter()
        .map(|s| s.parse().unwrap())
        .collect();
        assert_eq!(t, want);
        assert_eq!(broadcast_targets(9, None).len(), 2);
        assert_eq!(broadcast_targets(9, Some("::1".parse().unwrap())).len(), 2);
        assert_eq!(
            responder_addr("127.0.0.1:47800".parse().unwrap(), 47801),
            "127.0.0.1:47801".parse::<SocketAddr>().unwrap()
        );
        for listen in ["0.0.0.0:47800", "192.168.1.37:47800", "[::]:47800"] {
            assert_eq!(
                responder_addr(listen.parse().unwrap(), 9),
                "0.0.0.0:9".parse::<SocketAddr>().unwrap()
            );
        }
    }

    #[tokio::test]
    async fn waiting_machines_are_found() {
        let a = Responder::bind(LOCAL, pairing_offer(4000, "pc", 1))
            .await
            .unwrap();
        let b = Responder::bind(LOCAL, pairing_offer(4001, "mac", 2))
            .await
            .unwrap();
        // A running machine does not answer pairing queries.
        let c = Responder::bind(
            LOCAL,
            Offer::Peer {
                port: 4002,
                own: fp(3),
                peer: fp(4),
            },
        )
        .await
        .unwrap();
        let targets = [a.local_addr(), b.local_addr(), c.local_addr()];
        let mut found = find_pairing(&targets, SHORT).await.unwrap();
        found.sort_by_key(|w| w.addr.port());
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].addr, "127.0.0.1:4000".parse().unwrap());
        assert_eq!(
            (
                found[0].device_name.as_str(),
                found[0].os,
                found[0].fingerprint
            ),
            ("pc", Os::Windows, fp(1))
        );
        assert_eq!(found[1].fingerprint, fp(2));
    }

    #[tokio::test]
    async fn the_paired_peer_is_found_and_strangers_are_not() {
        let r = Responder::bind(
            LOCAL,
            Offer::Peer {
                port: 4100,
                own: fp(1),
                peer: fp(2),
            },
        )
        .await
        .unwrap();
        // Junk on the port does not stop the responder.
        let junk = UdpSocket::bind(LOCAL).await.unwrap();
        junk.send_to(b"hello", r.local_addr()).await.unwrap();
        let targets = [r.local_addr()];
        let found = find_peer(&targets, fp(2), fp(1), DISCOVERY_WAIT)
            .await
            .unwrap();
        assert_eq!(found, Some("127.0.0.1:4100".parse().unwrap()));
        assert_eq!(
            find_peer(&targets, fp(3), fp(1), SHORT).await.unwrap(),
            None
        );
        assert_eq!(
            find_peer(&targets, fp(2), fp(4), SHORT).await.unwrap(),
            None
        );
        drop(r);
        assert!(find_pairing(&targets, SHORT).await.unwrap().is_empty());
    }
}
