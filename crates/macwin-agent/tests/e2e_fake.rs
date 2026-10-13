//! Two drivers with fake backends talking over real QUIC on 127.0.0.1.
//!
//! A = Mac 1440x900 (peer on its right, dialer), B = Windows 1920x1080 (peer
//! on its left, listener).

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use macwin_agent::driver::{
    Backends, Connected, SessionConfig, SessionEnd, SessionHandle, kvm_hello, spawn_session,
};
use macwin_core::engine::{Timings, TransportRole};
use macwin_core::msg::{ButtonMsg, Enter, InputMsg, KeyMsg};
use macwin_core::types::{
    CapturedEvent, GrabCommand, Hid, MouseButton, Os, Point, PointF, Rect, RoleKind, ScreenInfo,
    Side,
};
use macwin_net::{Identity, Session, codes, connect, listen};
use macwin_platform::testing::{
    FakeCapture, FakeCaptureHandle, FakeInjector, FakeInjectorHandle, FakeScreens,
};

const T: Duration = Duration::from_secs(5);
const KEY_A: Hid = Hid(0x07_0004);

fn localhost() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn screens(w: i32, h: i32) -> Vec<ScreenInfo> {
    vec![ScreenInfo {
        id: 1,
        rect: Rect::new(0, 0, w, h).unwrap(),
        native_per_logical: 1.0,
        primary: true,
    }]
}

fn cfg(os: Os, name: &str, side: Side) -> SessionConfig {
    SessionConfig {
        local_os: os,
        os_version: "test".into(),
        device_name: name.into(),
        peer_side: side,
        remap: true,
        timings: Timings::default(),
    }
}

fn mac_cfg() -> SessionConfig {
    cfg(Os::MacOs, "mac", Side::Right)
}

fn win_cfg() -> SessionConfig {
    cfg(Os::Windows, "pc", Side::Left)
}

struct Peer {
    handle: SessionHandle,
    capture: FakeCaptureHandle,
    injector: FakeInjectorHandle,
}

impl Peer {
    fn role(&self) -> RoleKind {
        *self.handle.role.borrow()
    }
}

fn spawn_peer(
    cfg: SessionConfig,
    scr: Vec<ScreenInfo>,
    session: Session,
    role: TransportRole,
) -> Peer {
    let (capture, capture_handle) = FakeCapture::new();
    let (injector, injector_handle) = FakeInjector::new();
    let backends = Backends {
        capture: Box::new(capture),
        injector: Box::new(injector),
        screens: Box::new(FakeScreens(scr)),
    };
    let handle = spawn_session(
        cfg,
        backends,
        Connected {
            link: session.link,
            peer: session.peer,
            role,
        },
    )
    .unwrap();
    Peer {
        handle,
        capture: capture_handle,
        injector: injector_handle,
    }
}

/// Connects the two QUIC sessions: (dialer side A, listener side B).
async fn sessions(
    a_hello: macwin_core::msg::Hello,
    b_hello: macwin_core::msg::Hello,
) -> (Session, Session) {
    let a_id = Identity::generate().unwrap();
    let b_id = Identity::generate().unwrap();
    let listener = listen(localhost(), &b_id, a_id.fingerprint())
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { listener.accept(&b_hello).await.unwrap() });
    let a = tokio::time::timeout(
        T,
        connect(localhost(), addr, &a_id, b_id.fingerprint(), &a_hello),
    )
    .await
    .unwrap()
    .unwrap();
    let b = tokio::time::timeout(T, server).await.unwrap().unwrap();
    (a, b)
}

async fn start() -> (Peer, Peer) {
    let a_hello = kvm_hello(&mac_cfg(), screens(1440, 900));
    let b_hello = kvm_hello(&win_cfg(), screens(1920, 1080));
    let (a, b) = sessions(a_hello.clone(), b_hello.clone()).await;
    // 1. Each side sees exactly the other's Hello.
    assert_eq!(a.peer, b_hello);
    assert_eq!(b.peer, a_hello);
    (
        spawn_peer(mac_cfg(), screens(1440, 900), a, TransportRole::Dialer),
        spawn_peer(win_cfg(), screens(1920, 1080), b, TransportRole::Listener),
    )
}

async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + T;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn mv(x: f64, y: f64) -> CapturedEvent {
    CapturedEvent::MouseMove {
        pos: PointF::new(x, y),
        delta: PointF::default(),
    }
}

fn delta(dx: f64, dy: f64) -> CapturedEvent {
    CapturedEvent::MouseMove {
        pos: PointF::default(),
        delta: PointF::new(dx, dy),
    }
}

fn key(hid: Hid, down: bool) -> CapturedEvent {
    CapturedEvent::Key {
        hid,
        down,
        repeat: false,
    }
}

/// A pushes its cursor over its right edge onto B.
async fn a_controls_b(a: &Peer, b: &Peer) {
    assert!(
        !a.capture.emit(mv(1439.5, 450.0)),
        "the crossing move is swallowed"
    );
    assert_eq!(a.role(), RoleKind::Controlling);
    assert_eq!(
        a.capture.commands().last(),
        Some(&GrabCommand::Grab {
            exit: Point::new(1439, 450)
        })
    );
    wait_until("B controlled", || b.role() == RoleKind::Controlled).await;
    wait_until("entry MoveTo on B", || {
        b.injector.last_pos() == Some(Point::new(0, 540))
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn crossing_from_a_to_b_with_motion_and_remap() {
    let (a, b) = start().await;
    a_controls_b(&a, &b).await;
    assert!(!a.capture.emit(delta(10.0, -5.0)));
    wait_until("datagram motion", || {
        b.injector.last_pos() == Some(Point::new(10, 535))
    })
    .await;
    // Cmd on the Mac keyboard arrives as Ctrl on the PC.
    assert!(!a.capture.emit(key(Hid::LEFT_GUI, true)));
    wait_until("LeftCtrl held on B", || {
        b.injector.pressed_keys().contains(&Hid::LEFT_CTRL)
    })
    .await;
    assert!(!b.injector.pressed_keys().contains(&Hid::LEFT_GUI));
    assert!(!a.capture.emit(key(Hid::LEFT_GUI, false)));
    wait_until("LeftCtrl released on B", || {
        b.injector.pressed_keys().is_empty()
    })
    .await;
    assert_eq!(b.injector.unmatched_ups(), 0);
    a.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn crossing_from_b_to_a() {
    let (a, b) = start().await;
    assert!(!b.capture.emit(mv(0.0, 540.0)));
    assert_eq!(b.role(), RoleKind::Controlling);
    assert_eq!(
        b.capture.commands().last(),
        Some(&GrabCommand::Grab {
            exit: Point::new(0, 540)
        })
    );
    wait_until("A controlled", || a.role() == RoleKind::Controlled).await;
    wait_until("entry MoveTo on A", || {
        a.injector.last_pos() == Some(Point::new(1439, 450))
    })
    .await;
    // Back over the Mac's right edge: B gets its cursor back just inside its left edge.
    assert!(!b.capture.emit(delta(5.0, 0.0)));
    assert_eq!(b.role(), RoleKind::Idle);
    assert_eq!(
        b.capture.commands().last(),
        Some(&GrabCommand::Release {
            warp_to: Point::new(2, 540)
        })
    );
    wait_until("A idle", || a.role() == RoleKind::Idle).await;
    b.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn physical_input_on_the_target_takes_control_back() {
    let (a, b) = start().await;
    a_controls_b(&a, &b).await;
    assert!(
        b.capture.emit(key(KEY_A, true)),
        "the key reaches B's own OS"
    );
    assert_eq!(b.role(), RoleKind::Idle);
    wait_until("A released", || {
        a.capture.commands().last()
            == Some(&GrabCommand::Release {
                warp_to: Point::new(1437, 450),
            })
    })
    .await;
    wait_until("A idle", || a.role() == RoleKind::Idle).await;
    a.handle.shutdown();
}

/// A raw (driverless) dialer against B's driver.
async fn raw_a_and_driver_b() -> (Session, Peer) {
    let a_hello = kvm_hello(&mac_cfg(), screens(1440, 900));
    let b_hello = kvm_hello(&win_cfg(), screens(1920, 1080));
    let (a, b) = sessions(a_hello, b_hello).await;
    (
        a,
        spawn_peer(win_cfg(), screens(1920, 1080), b, TransportRole::Listener),
    )
}

fn press_shift_and_button(a: &Session) {
    a.link
        .tx
        .send_input(InputMsg::Enter(Enter {
            epoch: 1,
            at: Point::new(0, 540),
        }))
        .unwrap();
    a.link
        .tx
        .send_input(InputMsg::Key(KeyMsg {
            hid: Hid::LEFT_SHIFT,
            down: true,
            repeat: false,
        }))
        .unwrap();
    a.link
        .tx
        .send_input(InputMsg::Button(ButtonMsg {
            button: MouseButton::Left,
            down: true,
            at: Point::new(5, 5),
        }))
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn peer_disconnect_releases_everything() {
    let (a, b) = raw_a_and_driver_b().await;
    press_shift_and_button(&a);
    wait_until("B holds shift + button", || {
        b.injector.pressed_keys().contains(&Hid::LEFT_SHIFT)
            && b.injector.pressed_buttons() == vec![MouseButton::Left]
    })
    .await;
    a.link.tx.close_now(codes::INTERNAL);
    wait_until("B released", || {
        b.injector.pressed_keys().is_empty() && b.injector.pressed_buttons().is_empty()
    })
    .await;
    let end = tokio::time::timeout(T, b.handle.join()).await.unwrap();
    assert_eq!(end, SessionEnd::PeerClosed);
}

#[tokio::test(flavor = "multi_thread")]
async fn silent_peer_hits_the_heartbeat_timeout() {
    let (a, b) = raw_a_and_driver_b().await;
    press_shift_and_button(&a);
    wait_until("B holds shift", || {
        b.injector.pressed_keys().contains(&Hid::LEFT_SHIFT)
    })
    .await;
    let silent_since = Instant::now();
    // A keeps the QUIC connection alive (keep-alives) but its "Engine" is silent.
    wait_until("B released after the timeout", || {
        b.injector.pressed_keys().is_empty()
    })
    .await;
    let waited = silent_since.elapsed();
    assert!(
        waited >= Duration::from_millis(1300) && waited <= Duration::from_secs(3),
        "released after {waited:?}"
    );
    assert!(b.injector.pressed_buttons().is_empty());
    let end = tokio::time::timeout(T, b.handle.join()).await.unwrap();
    assert_eq!(end, SessionEnd::HeartbeatTimeout);
    drop(a);
}

#[tokio::test(flavor = "multi_thread")]
async fn emergency_chord_stops_the_session_and_releases_the_peer() {
    let (a, b) = start().await;
    a_controls_b(&a, &b).await;
    assert!(!a.capture.emit(key(KEY_A, true)));
    wait_until("B holds KeyA", || {
        b.injector.pressed_keys().contains(&KEY_A)
    })
    .await;
    // The backend already released the grab locally when it reports this.
    a.capture
        .emit_fault(macwin_core::types::CaptureFault::EmergencyChord);
    let a_end = tokio::time::timeout(T, a.handle.join()).await.unwrap();
    assert_eq!(a_end, SessionEnd::EmergencyStop);
    assert!(a.capture.stopped());
    wait_until("B released", || b.injector.pressed_keys().is_empty()).await;
    let b_end = tokio::time::timeout(T, b.handle.join()).await.unwrap();
    assert_eq!(b_end, SessionEnd::PeerClosed);
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_while_controlling_releases_the_peer() {
    let (a, b) = start().await;
    a_controls_b(&a, &b).await;
    assert!(!a.capture.emit(key(KEY_A, true)));
    wait_until("B holds KeyA", || {
        b.injector.pressed_keys().contains(&KEY_A)
    })
    .await;
    a.handle.shutdown();
    let a_end = tokio::time::timeout(T, a.handle.join()).await.unwrap();
    assert_eq!(a_end, SessionEnd::Shutdown);
    assert!(a.capture.stopped());
    assert!(
        matches!(
            a.capture.commands().last(),
            Some(GrabCommand::Release { .. })
        ),
        "A ungrabbed"
    );
    wait_until("B released", || b.injector.pressed_keys().is_empty()).await;
    let b_end = tokio::time::timeout(T, b.handle.join()).await.unwrap();
    assert_eq!(b_end, SessionEnd::PeerClosed);
    assert_eq!(b.injector.unmatched_ups(), 0);
}
