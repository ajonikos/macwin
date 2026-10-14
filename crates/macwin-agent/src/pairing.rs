//! The pairing flows shared by `macwin pair` and the tray: waiting with a
//! code (answering discovery meanwhile), finding the waiting machine on the
//! LAN, and storing the result.

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use anyhow::bail;
use macwin_core::types::Side;
use macwin_net::discovery::{self, DISCOVERY_WAIT, Offer, Responder, Waiting, responder_addr};
use macwin_net::pairing::{PairInfo, PairListener, PairedPeer, PairingCode};
use macwin_net::{Fingerprint, NetError};
use tracing::warn;

use crate::config::{Config, PeerConfig, os_name};

/// How long a machine waits for the other one.
pub const PAIR_WINDOW: Duration = Duration::from_secs(600);
/// Failed attempts after which waiting gives up.
pub const MAX_PAIR_ATTEMPTS: u32 = 5;

/// This machine's view of the peer's side, from the local choice and the
/// peer's ("its peer is on its left" means "it is on my right"). The flag is
/// set when both chose the same side; the local choice wins then.
pub fn agree_side(local: Option<Side>, peer_view: Option<Side>) -> (Option<Side>, bool) {
    match (local, peer_view) {
        (Some(l), Some(p)) => (Some(l), l == p),
        (Some(l), None) => (Some(l), false),
        (None, p) => (p.map(Side::opposite), false),
    }
}

/// Answers "who is waiting for pairing?" while a pairing listener waits on
/// `local`. `Ok(None)` when discovery is off.
pub async fn pairing_responder(
    config: &Config,
    local: SocketAddr,
    info: &PairInfo,
    own: Fingerprint,
) -> Result<Option<Responder>, NetError> {
    if !config.discovery {
        return Ok(None);
    }
    let offer = Offer::Pairing {
        port: local.port(),
        os: info.os,
        device_name: info.device_name.clone(),
        fingerprint: own,
    };
    Responder::bind(responder_addr(local, config.discovery_port), offer)
        .await
        .map(Some)
}

/// Waits on `listener` for the other machine. `show` presents each code,
/// with the reason the previous one was used up (`None` for the first):
/// a code is burned by every attempt that reached the exchange, up to
/// [`MAX_PAIR_ATTEMPTS`] attempts within [`PAIR_WINDOW`].
pub async fn wait_for_pairing(
    listener: &PairListener,
    info: &PairInfo,
    mut show: impl FnMut(&PairingCode, Option<&str>),
) -> anyhow::Result<PairedPeer> {
    let deadline = tokio::time::Instant::now() + PAIR_WINDOW;
    let mut code = PairingCode::generate()?;
    show(&code, None);
    let mut failures = 0;
    loop {
        match tokio::time::timeout_at(deadline, listener.accept(&code, info)).await {
            Err(_) => bail!(
                "no machine paired within {} minutes",
                PAIR_WINDOW.as_secs() / 60
            ),
            Ok(Ok(peer)) => return Ok(peer),
            Ok(Err(NetError::PairingFailed(why))) => {
                failures += 1;
                if failures >= MAX_PAIR_ATTEMPTS {
                    bail!("{failures} failed attempts (the last one: {why}); start pairing again");
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
                code = PairingCode::generate()?;
                show(&code, Some(&why));
            }
            Ok(Err(NetError::EndpointClosed)) => bail!("the pairing listener closed"),
            Ok(Err(e)) => warn!(error = %e, "a connection attempt failed; still waiting"),
        }
    }
}

/// The one machine on the LAN that waits for pairing (other than `own`),
/// asked for at `targets`.
pub async fn find_waiting(
    config: &Config,
    own: Fingerprint,
    targets: &[SocketAddr],
) -> anyhow::Result<Waiting> {
    if !config.discovery {
        bail!(
            "LAN discovery is off (macwin config set discovery on); pass the other machine's \
             address: macwin pair HOST --code CODE"
        );
    }
    let mut found = discovery::find_pairing(targets, DISCOVERY_WAIT).await?;
    found.retain(|w| w.fingerprint != own);
    match found.len() {
        0 => bail!(
            "no machine on this network is waiting for pairing: start pairing on the other \
             machine first, or pass its address: macwin pair HOST --code CODE"
        ),
        1 => Ok(found.remove(0)),
        _ => {
            let list: Vec<String> = found
                .iter()
                .map(|w| format!("  \"{}\" ({}) at {}", w.device_name, os_name(w.os), w.addr))
                .collect();
            bail!(
                "several machines are waiting for pairing:\n{}\npass the address of the right \
                 one: macwin pair ADDRESS --code CODE",
                list.join("\n")
            )
        }
    }
}

/// Stores a completed pairing in the config in `dir`: the peer, its side
/// and, on the dialing side, its address. Returns the name of the machine
/// this pairing replaced, if any.
pub fn store_pairing(
    dir: &Path,
    peer: &PairedPeer,
    side: Option<Side>,
    address: Option<String>,
) -> anyhow::Result<Option<String>> {
    let mut config = Config::load(dir)?;
    let replaced = config
        .peer
        .as_ref()
        .filter(|old| old.fingerprint != peer.fingerprint)
        .map(|old| old.name.clone());
    config.peer = Some(PeerConfig {
        fingerprint: peer.fingerprint,
        name: peer.device_name.clone(),
        os: peer.os,
        side,
        address,
    });
    config.save(dir)?;
    Ok(replaced)
}

#[cfg(test)]
mod tests {
    use macwin_core::types::Os;

    use super::*;

    #[test]
    fn sides_agree() {
        use Side::{Left, Right};
        assert_eq!(agree_side(None, None), (None, false));
        assert_eq!(agree_side(None, Some(Left)), (Some(Right), false));
        assert_eq!(agree_side(Some(Left), None), (Some(Left), false));
        assert_eq!(agree_side(Some(Left), Some(Right)), (Some(Left), false));
        assert_eq!(agree_side(Some(Left), Some(Left)), (Some(Left), true));
    }

    #[test]
    fn pairings_are_stored_and_replace_the_old_peer() {
        let dir = tempfile::tempdir().unwrap();
        let peer = |b: u8, name: &str| PairedPeer {
            fingerprint: Fingerprint([b; 32]),
            os: Os::Windows,
            device_name: name.into(),
            peer_side: None,
            remote: "127.0.0.1:1".parse().unwrap(),
        };
        let first = store_pairing(dir.path(), &peer(1, "pc"), Some(Side::Left), None).unwrap();
        assert_eq!(first, None);
        let again = store_pairing(dir.path(), &peer(1, "pc"), None, None).unwrap();
        assert_eq!(again, None, "the same machine again");
        let replaced = store_pairing(
            dir.path(),
            &peer(2, "pc2"),
            Some(Side::Right),
            Some("10.0.0.2:47800".into()),
        )
        .unwrap();
        assert_eq!(replaced.as_deref(), Some("pc"));
        let p = Config::load(dir.path()).unwrap().peer.unwrap();
        assert_eq!((p.name.as_str(), p.side), ("pc2", Some(Side::Right)));
        assert_eq!(p.address.as_deref(), Some("10.0.0.2:47800"));
    }
}
