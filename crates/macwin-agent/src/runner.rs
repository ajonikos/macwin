//! `macwin run`: keeps a session with the paired peer up and reconnects
//! after every disconnect. The machine that knows the peer's address dials
//! it; the other one listens. Only the emergency chord or a stop request
//! (Ctrl+C) ends the loop.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use anyhow::{Context, bail};
use macwin_core::engine::TransportRole;
use macwin_core::types::RoleKind;
use macwin_net::{Fingerprint, Identity, NetError, Session, connect, listen};
use macwin_platform::PlatformError;
use tokio::sync::watch;

use crate::driver::{Backends, Connected, SessionConfig, SessionEnd, kvm_hello, spawn_session};

/// First reconnect delay of a dialer.
pub const RETRY_MIN: Duration = Duration::from_secs(1);
/// Longest reconnect delay of a dialer.
pub const RETRY_MAX: Duration = Duration::from_secs(10);
/// Bound on one dial attempt. A QUIC dial to a machine that is off only
/// fails at the idle timeout otherwise; on a LAN the handshake takes
/// milliseconds.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Bound on letting the close of an ended session reach the peer.
const DRAIN: Duration = Duration::from_secs(2);

/// How this machine finds its peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Listen(SocketAddr),
    /// `HOST:PORT`, resolved again before every attempt.
    Dial(String),
}

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub session: SessionConfig,
    pub mode: Mode,
    /// The paired peer's certificate fingerprint.
    pub peer: Fingerprint,
    /// First delay between two dial attempts (doubling up to `retry_max`),
    /// and the pause before redialing after a session ended.
    pub retry_min: Duration,
    /// Longest delay between two dial attempts.
    pub retry_max: Duration,
    /// Bound on one dial attempt.
    pub connect_timeout: Duration,
}

/// Progress reports for the user.
#[derive(Debug, Clone, PartialEq)]
pub enum RunEvent {
    Listening(SocketAddr),
    Dialing(String),
    Connected {
        name: String,
        remote: SocketAddr,
    },
    Role(RoleKind),
    Ended(SessionEnd),
    /// A connection attempt failed; the next one starts after `after`.
    Retrying {
        after: Duration,
        error: String,
    },
}

/// Why [`run`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunExit {
    /// The stop signal was raised.
    Stopped,
    /// The user pressed the emergency chord.
    EmergencyStop,
}

/// Binds the dialer's socket on loopback for loopback peers, otherwise on
/// the unspecified address of the peer's family.
pub fn bind_for(remote: SocketAddr) -> SocketAddr {
    let ip = match (remote.ip().is_loopback(), remote.is_ipv4()) {
        (true, true) => IpAddr::V4(Ipv4Addr::LOCALHOST),
        (true, false) => IpAddr::V6(Ipv6Addr::LOCALHOST),
        (false, true) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        (false, false) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    };
    SocketAddr::new(ip, 0)
}

/// Resolves `HOST:PORT` to its first address.
pub async fn resolve(target: &str) -> anyhow::Result<SocketAddr> {
    tokio::net::lookup_host(target)
        .await
        .with_context(|| format!("cannot resolve {target}"))?
        .next()
        .with_context(|| format!("{target} has no address"))
}

/// Resolves when `stop` is raised (or its sender is gone).
async fn stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|s| *s).await;
}

/// Sleeps for `d`; true when the stop signal came first.
async fn sleep_or_stop(d: Duration, stop: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        () = tokio::time::sleep(d) => false,
        () = stopped(stop) => true,
    }
}

/// Runs sessions with the peer until stopped. `make_backends` is called
/// once per connection attempt; `on_event` reports progress.
pub async fn run(
    opts: RunOptions,
    id: Identity,
    mut make_backends: impl FnMut() -> Result<Backends, PlatformError>,
    mut on_event: impl FnMut(RunEvent),
    mut stop: watch::Receiver<bool>,
) -> anyhow::Result<RunExit> {
    let listener = match &opts.mode {
        Mode::Listen(addr) => {
            let l = listen(*addr, &id, opts.peer)
                .await
                .with_context(|| format!("cannot listen on {addr}"))?;
            on_event(RunEvent::Listening(l.local_addr()?));
            Some(l)
        }
        Mode::Dial(_) => None,
    };
    let retry_min = opts.retry_min.min(opts.retry_max);
    let mut backoff = retry_min;
    loop {
        if *stop.borrow() {
            return Ok(RunExit::Stopped);
        }
        let backends = make_backends().context("cannot start the input backends")?;
        let screens = backends
            .screens
            .screens()
            .context("cannot read the screens")?;
        let hello = kvm_hello(&opts.session, screens);
        let attempt: Result<(Session, TransportRole), anyhow::Error> = match (&opts.mode, &listener)
        {
            (Mode::Listen(_), Some(l)) => tokio::select! {
                r = l.accept(&hello) => match r {
                    Ok(s) => Ok((s, TransportRole::Listener)),
                    Err(NetError::EndpointClosed) => bail!("the listener closed"),
                    Err(e) => Err(e.into()),
                },
                () = stopped(&mut stop) => return Ok(RunExit::Stopped),
            },
            (Mode::Dial(target), _) => {
                if backoff == retry_min {
                    on_event(RunEvent::Dialing(target.clone()));
                }
                let dial = async {
                    let remote = resolve(target).await?;
                    let s = tokio::time::timeout(
                        opts.connect_timeout,
                        connect(bind_for(remote), remote, &id, opts.peer, &hello),
                    )
                    .await
                    .map_err(|_| anyhow::anyhow!("no answer from {remote}"))?
                    .with_context(|| format!("cannot connect to {remote}"))?;
                    anyhow::Ok((s, TransportRole::Dialer))
                };
                tokio::select! {
                    r = dial => r,
                    () = stopped(&mut stop) => return Ok(RunExit::Stopped),
                }
            }
            (Mode::Listen(_), None) => unreachable!("listen mode always has a listener"),
        };
        let (session, role) = match attempt {
            Ok(x) => x,
            Err(e) => {
                let error = format!("{e:#}");
                match opts.mode {
                    // A peer failed the handshake: keep listening.
                    Mode::Listen(_) => on_event(RunEvent::Retrying {
                        after: Duration::ZERO,
                        error,
                    }),
                    Mode::Dial(_) => {
                        on_event(RunEvent::Retrying {
                            after: backoff,
                            error,
                        });
                        if sleep_or_stop(backoff, &mut stop).await {
                            return Ok(RunExit::Stopped);
                        }
                        backoff = (backoff * 2).min(opts.retry_max);
                    }
                }
                continue;
            }
        };
        backoff = retry_min;
        on_event(RunEvent::Connected {
            name: session.peer.device_name.clone(),
            remote: session.remote,
        });
        let handle = spawn_session(
            opts.session.clone(),
            backends,
            Connected {
                link: session.link,
                peer: session.peer,
                role,
            },
        )
        .context("cannot start the session")?;
        let stopper = handle.stopper();
        let link = handle.link();
        let mut roles = handle.role.clone();
        let join = handle.join();
        tokio::pin!(join);
        let end = loop {
            tokio::select! {
                end = &mut join => break end,
                changed = roles.changed() => match changed {
                    Ok(()) => on_event(RunEvent::Role(*roles.borrow_and_update())),
                    Err(_) => break (&mut join).await,
                },
                () = stopped(&mut stop) => {
                    stopper.shutdown();
                    break (&mut join).await;
                }
            }
        };
        let _ = tokio::time::timeout(DRAIN, link.wait_closed()).await;
        on_event(RunEvent::Ended(end.clone()));
        if end == SessionEnd::EmergencyStop {
            return Ok(RunExit::EmergencyStop);
        }
        if *stop.borrow() {
            return Ok(RunExit::Stopped);
        }
        // Give a restarting peer a moment before dialing again.
        if matches!(opts.mode, Mode::Dial(_)) && sleep_or_stop(retry_min, &mut stop).await {
            return Ok(RunExit::Stopped);
        }
    }
}
