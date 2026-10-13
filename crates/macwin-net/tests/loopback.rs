//! QUIC sessions over 127.0.0.1 with real TLS pinning.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::time::Duration;

use macwin_core::msg::{
    CAP_SPIKE_NET, ControlMsg, DatagramMsg, Hello, InputMsg, KeyMsg, Leave, MouseAbs, Ping,
};
use macwin_core::types::{Hid, Os, Point, Rect, ScreenInfo, Side};
use macwin_net::link::Link;
use macwin_net::{Identity, Inbound, LinkClosed, NetError, Session, codes, connect, listen};
use macwin_proto::{PROTOCOL_VERSION, ProtoError};
use tokio::time::timeout;

const T: Duration = Duration::from_secs(5);

fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn hello(name: &str, os: Os) -> Hello {
    Hello {
        protocol_version: PROTOCOL_VERSION,
        capabilities: vec![CAP_SPIKE_NET.into()],
        os,
        os_version: "test".into(),
        device_name: name.into(),
        screens: vec![ScreenInfo {
            id: 1,
            rect: Rect::new(0, 0, 1440, 900).unwrap(),
            native_per_logical: 1.0,
            primary: true,
        }],
        peer_side: Some(Side::Right),
        agent_version: "0.1.0".into(),
    }
}

struct Pair {
    dialer: Result<Session, NetError>,
    listener: Result<Session, NetError>,
}

/// Runs one listener accept and one dial concurrently.
async fn pair(
    dialer_id: &Identity,
    listener_id: &Identity,
    dialer_pins: macwin_net::Fingerprint,
    listener_pins: macwin_net::Fingerprint,
    dialer_hello: Hello,
    listener_hello: Hello,
) -> Pair {
    let listener = listen(localhost(), listener_id, listener_pins)
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    assert!(addr.ip().is_loopback());
    let server = tokio::spawn(async move { listener.accept(&listener_hello).await });
    let dialer = timeout(
        T,
        connect(localhost(), addr, dialer_id, dialer_pins, &dialer_hello),
    )
    .await
    .expect("dial timed out");
    let listener = timeout(T, server).await.expect("accept timed out").unwrap();
    Pair { dialer, listener }
}

async fn good_pair() -> (Session, Session) {
    let a = Identity::generate().unwrap();
    let b = Identity::generate().unwrap();
    let p = pair(
        &a,
        &b,
        b.fingerprint(),
        a.fingerprint(),
        hello("mac", Os::MacOs),
        hello("pc", Os::Windows),
    )
    .await;
    (p.dialer.unwrap(), p.listener.unwrap())
}

async fn next(link: &mut Link) -> Inbound {
    timeout(T, link.rx.recv())
        .await
        .expect("receive timed out")
        .expect("link rx ended")
}

/// Sends one message per channel from `from` and checks `to` receives all
/// three (datagrams are unordered relative to streams).
async fn exchange(from: &Session, to: &mut Session) {
    let ping = Ping {
        id: 7,
        sent_micros: 99,
    };
    let key = InputMsg::Key(KeyMsg {
        hid: Hid(0x07_0004),
        down: true,
        repeat: false,
    });
    let abs = DatagramMsg::MouseAbs(MouseAbs {
        epoch: 1,
        seq: 1,
        at: Point::new(-3, 4),
    });
    from.link.tx.send_control(ControlMsg::Ping(ping)).unwrap();
    from.link.tx.send_input(key).unwrap();
    from.link.tx.send_datagram(abs);
    let mut got = HashSet::new();
    for _ in 0..3 {
        got.insert(format!("{:?}", next(&mut to.link).await));
    }
    let expected: HashSet<String> = [
        Inbound::Control(ControlMsg::Ping(ping)),
        Inbound::Input(key),
        Inbound::Datagram(abs),
    ]
    .iter()
    .map(|m| format!("{m:?}"))
    .collect();
    assert_eq!(got, expected);
}

#[tokio::test(flavor = "multi_thread")]
async fn hello_exchange_and_all_channels_both_ways() {
    let (mut a, mut b) = good_pair().await;
    assert_eq!(a.peer, hello("pc", Os::Windows));
    assert_eq!(b.peer, hello("mac", Os::MacOs));
    assert!(a.remote.ip().is_loopback() && b.remote.ip().is_loopback());
    exchange(&a, &mut b).await;
    exchange(&b, &mut a).await;
    a.link.tx.close(codes::NORMAL);
    assert_eq!(
        next(&mut b.link).await,
        Inbound::Closed(LinkClosed::PeerClosed { code: 0 })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dialer_with_wrong_pin_is_rejected_on_both_sides() {
    let a = Identity::generate().unwrap();
    let b = Identity::generate().unwrap();
    let stranger = Identity::generate().unwrap();
    let p = pair(
        &a,
        &b,
        stranger.fingerprint(),
        a.fingerprint(),
        hello("a", Os::MacOs),
        hello("b", Os::Windows),
    )
    .await;
    assert!(
        p.dialer.is_err(),
        "dialer must reject the server certificate"
    );
    assert!(p.listener.is_err(), "listener must not get a session");
}

#[tokio::test(flavor = "multi_thread")]
async fn listener_with_wrong_pin_is_rejected_on_both_sides() {
    let a = Identity::generate().unwrap();
    let b = Identity::generate().unwrap();
    let stranger = Identity::generate().unwrap();
    let p = pair(
        &a,
        &b,
        b.fingerprint(),
        stranger.fingerprint(),
        hello("a", Os::MacOs),
        hello("b", Os::Windows),
    )
    .await;
    assert!(
        p.listener.is_err(),
        "listener must reject the client certificate"
    );
    // TLS 1.3 lets the client finish first; it fails during stream setup.
    assert!(p.dialer.is_err(), "dialer must not get a session");
}

#[tokio::test(flavor = "multi_thread")]
async fn major_version_mismatch_fails_on_both_sides() {
    let a = Identity::generate().unwrap();
    let b = Identity::generate().unwrap();
    let mut old = hello("a", Os::MacOs);
    old.protocol_version = 0x0002_0000;
    let p = pair(
        &a,
        &b,
        b.fingerprint(),
        a.fingerprint(),
        old,
        hello("b", Os::Windows),
    )
    .await;
    assert!(
        matches!(
            p.dialer,
            Err(NetError::Proto(ProtoError::IncompatibleVersion { .. }))
        ),
        "dialer: {:?}",
        p.dialer.err()
    );
    assert!(
        matches!(
            p.listener,
            Err(NetError::Proto(ProtoError::IncompatibleVersion { .. }))
        ),
        "listener: {:?}",
        p.listener.err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn graceful_close_delivers_everything_in_order_first() {
    let (mut a, mut b) = good_pair().await;
    for epoch in 0..100 {
        a.link
            .tx
            .send_input(InputMsg::Leave(Leave { epoch }))
            .unwrap();
    }
    a.link.tx.close(codes::NORMAL);
    assert_eq!(
        a.link.tx.send_input(InputMsg::Leave(Leave { epoch: 1000 })),
        Err(macwin_net::LinkError::Closed),
        "no sends after close"
    );
    for epoch in 0..100 {
        assert_eq!(
            next(&mut b.link).await,
            Inbound::Input(InputMsg::Leave(Leave { epoch }))
        );
    }
    assert_eq!(
        next(&mut b.link).await,
        Inbound::Closed(LinkClosed::PeerClosed { code: 0 })
    );
    assert_eq!(
        next(&mut a.link).await,
        Inbound::Closed(LinkClosed::LocalClosed { code: 0 })
    );
    timeout(T, a.link.tx.wait_closed()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn immediate_close_reports_the_code() {
    let (mut a, mut b) = good_pair().await;
    b.link.tx.close_now(codes::INTERNAL);
    assert_eq!(
        next(&mut a.link).await,
        Inbound::Closed(LinkClosed::PeerClosed { code: 5 })
    );
    assert_eq!(
        next(&mut b.link).await,
        Inbound::Closed(LinkClosed::LocalClosed { code: 5 })
    );
    assert!(b.link.tx.is_closed());
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_pair_round_trip() {
    let (a, mut b) = Link::memory_pair();
    a.tx.send_input(InputMsg::Leave(Leave { epoch: 9 }))
        .unwrap();
    assert_eq!(
        next(&mut b).await,
        Inbound::Input(InputMsg::Leave(Leave { epoch: 9 }))
    );
    a.tx.close(codes::NORMAL);
    assert_eq!(
        next(&mut b).await,
        Inbound::Closed(LinkClosed::PeerClosed { code: 0 })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn persisted_identities_reconnect_with_the_same_pins() {
    let (a, bundle_a) = Identity::generate_bundle().unwrap();
    let b = Identity::generate().unwrap();
    // What a secret store hands back after a restart.
    let a2 = Identity::from_pem_bundle(bundle_a.as_bytes()).unwrap();
    assert_eq!(a2.fingerprint(), a.fingerprint());
    let p = pair(
        &a2,
        &b,
        b.fingerprint(),
        a.fingerprint(),
        hello("a", Os::MacOs),
        hello("b", Os::Windows),
    )
    .await;
    assert!(p.dialer.is_ok() && p.listener.is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn clipboard_updates_cross_on_their_own_streams() {
    use macwin_core::msg::{ClipboardMsg, ClipboardText};
    let (a, mut b) = good_pair().await;
    // 1 MB: far beyond the 64 KiB stream frame limit.
    let big = "z".repeat(1 << 20);
    let msgs = [
        ClipboardMsg::Text(ClipboardText {
            clock: 1,
            text: big,
        }),
        ClipboardMsg::Other { clock: 2 },
    ];
    for m in &msgs {
        a.link.tx.send_clipboard(m.clone()).unwrap();
    }
    // Heartbeat-like control traffic is not stuck behind the big text.
    a.link
        .tx
        .send_control(ControlMsg::Ping(Ping {
            id: 1,
            sent_micros: 1,
        }))
        .unwrap();
    let mut got = Vec::new();
    while got.len() < 2 {
        match next(&mut b.link).await {
            Inbound::Clipboard(m) => got.push(m),
            Inbound::Control(_) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    // Separate streams may arrive in any order.
    got.sort_by_key(|m| match m {
        ClipboardMsg::Text(t) => t.clock,
        ClipboardMsg::Other { clock } => *clock,
    });
    assert_eq!(got, msgs);
    // Oversized updates are refused locally.
    let huge = ClipboardMsg::Text(ClipboardText {
        clock: 3,
        text: "x".repeat(macwin_proto::MAX_CLIPBOARD_TEXT + 1),
    });
    assert_eq!(
        a.link.tx.send_clipboard(huge),
        Err(macwin_net::LinkError::TooLarge)
    );
    a.link.tx.close(codes::NORMAL);
    loop {
        if let Inbound::Closed(c) = next(&mut b.link).await {
            assert_eq!(c, LinkClosed::PeerClosed { code: 0 });
            break;
        }
    }
}

mod pairing {
    use macwin_core::types::{Os, Side};
    use macwin_net::pairing::{PairInfo, PairedPeer, PairingCode, pair_connect, pair_listen};
    use macwin_net::{Identity, NetError};

    use super::{T, localhost};

    fn info(name: &str, os: Os, side: Option<Side>) -> PairInfo {
        PairInfo {
            os,
            device_name: name.into(),
            peer_side: side,
        }
    }

    /// One listener attempt with `listener_code`, one dial with `dial_code`.
    async fn attempt(
        dialer: &Identity,
        listener: &Identity,
        dial_code: &str,
        listener_code: &str,
    ) -> (Result<PairedPeer, NetError>, Result<PairedPeer, NetError>) {
        let l = pair_listen(localhost(), listener).await.unwrap();
        let addr = l.local_addr().unwrap();
        let code = PairingCode::parse(listener_code).unwrap();
        let server = tokio::spawn(async move {
            let r = l
                .accept(&code, &info("pc", Os::Windows, Some(Side::Left)))
                .await;
            l.shutdown().await;
            r
        });
        let d = tokio::time::timeout(
            T,
            pair_connect(
                localhost(),
                addr,
                dialer,
                &PairingCode::parse(dial_code).unwrap(),
                &info("mac", Os::MacOs, None),
            ),
        )
        .await
        .expect("dial timed out");
        let l = tokio::time::timeout(T, server)
            .await
            .expect("listener timed out")
            .unwrap();
        (d, l)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_right_code_exchanges_fingerprints() {
        let mac = Identity::generate().unwrap();
        let pc = Identity::generate().unwrap();
        let (d, l) = attempt(&mac, &pc, "123456", "123-456").await;
        let d = d.unwrap();
        let l = l.unwrap();
        assert_eq!(d.fingerprint, pc.fingerprint());
        assert_eq!(l.fingerprint, mac.fingerprint());
        assert_eq!((d.os, d.device_name.as_str()), (Os::Windows, "pc"));
        assert_eq!((l.os, l.device_name.as_str()), (Os::MacOs, "mac"));
        assert_eq!(d.peer_side, Some(Side::Left));
        assert_eq!(l.peer_side, None);
        assert!(d.remote.ip().is_loopback());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_wrong_code_fails_on_both_sides() {
        let mac = Identity::generate().unwrap();
        let pc = Identity::generate().unwrap();
        let (d, l) = attempt(&mac, &pc, "000001", "000002").await;
        let d = d.unwrap_err();
        let l = l.unwrap_err();
        assert!(
            matches!(&l, NetError::PairingFailed(m) if m.contains("wrong code")),
            "{l}"
        );
        assert!(matches!(&d, NetError::PairingFailed(_)), "{d}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_machine_cannot_pair_with_itself() {
        let same = Identity::generate().unwrap();
        let (d, l) = attempt(&same, &same, "123456", "123456").await;
        assert!(matches!(d, Err(NetError::PairingFailed(_))));
        assert!(matches!(l, Err(NetError::PairingFailed(_))));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_dialer_cannot_reach_a_pairing_listener() {
        // Different ALPN: the TLS handshake itself fails, the code stays unused.
        let mac = Identity::generate().unwrap();
        let pc = Identity::generate().unwrap();
        let l = pair_listen(localhost(), &pc).await.unwrap();
        let addr = l.local_addr().unwrap();
        let code = PairingCode::parse("111111").unwrap();
        let server =
            tokio::spawn(async move { l.accept(&code, &info("pc", Os::Windows, None)).await });
        let r = macwin_net::connect(
            localhost(),
            addr,
            &mac,
            pc.fingerprint(),
            &super::hello("mac", Os::MacOs),
        )
        .await;
        assert!(r.is_err());
        let l = tokio::time::timeout(T, server).await.unwrap().unwrap();
        assert!(
            matches!(&l, Err(e) if !matches!(e, NetError::PairingFailed(_))),
            "{l:?}"
        );
    }
}
