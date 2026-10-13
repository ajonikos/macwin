//! Implementations of the `macwin` subcommands.

use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use macwin_core::engine::{Timings, TransportRole};
use macwin_core::keymap;
use macwin_core::types::{
    CaptureFault, CapturedEvent, GrabCommand, Hid, InjectCommand, Point, ScreenInfo,
};
use macwin_net::{Identity, NetError, Session, config_dir, connect, listen};
use macwin_platform::{CaptureSink, ClipboardInfo};
use tracing::warn;

use crate::cli::{SpikeCaptureArgs, SpikeKvmArgs, SpikeNetArgs};
use crate::driver::{Connected, SessionConfig, SessionEnd, kvm_hello, spawn_session};
use crate::spike_net::{self, SpikeOpts, spike_hello};
use crate::{device_name, host_os, platform};

/// The emergency chord, as shown to users.
pub const EMERGENCY_CHORD: &str = "Ctrl+Alt+Shift+Esc (Control+Option+Shift+Esc on a Mac)";

/// Loads (or creates) this machine's identity.
pub fn load_identity() -> anyhow::Result<(Identity, PathBuf)> {
    let dir = config_dir(|k| std::env::var_os(k))?;
    let (id, created) =
        Identity::load_or_create(&dir).with_context(|| format!("identity in {}", dir.display()))?;
    if created {
        eprintln!("created a new identity in {}", dir.display());
    }
    Ok((id, dir))
}

pub fn identity() -> anyhow::Result<()> {
    let (id, dir) = load_identity()?;
    println!("fingerprint: {}", id.fingerprint());
    println!("config dir: {}", dir.display());
    Ok(())
}

/// Binds the dialer's socket on loopback for loopback peers, otherwise on the
/// unspecified address of the peer's family.
pub fn bind_for(remote: SocketAddr) -> SocketAddr {
    let ip = match (remote.ip().is_loopback(), remote.is_ipv4()) {
        (true, true) => IpAddr::V4(Ipv4Addr::LOCALHOST),
        (true, false) => IpAddr::V6(Ipv6Addr::LOCALHOST),
        (false, true) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        (false, false) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    };
    SocketAddr::new(ip, 0)
}

async fn resolve(target: &str) -> anyhow::Result<SocketAddr> {
    tokio::net::lookup_host(target)
        .await
        .with_context(|| format!("cannot resolve {target}"))?
        .next()
        .with_context(|| format!("{target} has no address"))
}

/// Listens until one peer completes the handshake.
async fn accept_one(
    addr: SocketAddr,
    id: &Identity,
    args_fp: macwin_net::Fingerprint,
    hello: &macwin_core::msg::Hello,
) -> anyhow::Result<Session> {
    let listener = listen(addr, id, args_fp)
        .await
        .with_context(|| format!("cannot listen on {addr}"))?;
    println!("listening on {}", listener.local_addr()?);
    std::io::stdout().flush()?;
    loop {
        match listener.accept(hello).await {
            Ok(s) => return Ok(s),
            Err(NetError::EndpointClosed) => bail!("the listener closed"),
            Err(e) => warn!(error = %e, "a peer failed the handshake; still listening"),
        }
    }
}

pub async fn spike_net(args: SpikeNetArgs) -> anyhow::Result<()> {
    let os =
        host_os().ok_or_else(|| anyhow!("unsupported OS: macwin runs on macOS and Windows"))?;
    let (id, _) = load_identity()?;
    println!("local fingerprint: {}", id.fingerprint());
    let hello = spike_hello(os, &device_name());
    if let Some(addr) = args.listen {
        let session = accept_one(addr, &id, args.peer_fp, &hello).await?;
        println!(
            "peer connected: {} ({})",
            session.peer.device_name, session.remote
        );
        spike_net::run_listener(session).await?;
        println!("peer closed the session");
        return Ok(());
    }
    let remote = resolve(
        &args
            .connect
            .context("either --listen or --connect is required")?,
    )
    .await?;
    let session = connect(bind_for(remote), remote, &id, args.peer_fp, &hello)
        .await
        .with_context(|| format!("cannot connect to {remote}"))?;
    println!(
        "connected to {} ({})",
        session.peer.device_name, session.remote
    );
    let opts = SpikeOpts {
        count: args.count.max(1),
        interval: Duration::from_millis(args.interval_ms),
    };
    let report = spike_net::run_dialer(session, opts).await?;
    println!("{report}");
    Ok(())
}

// ---------- probes ----------

fn describe_screen(s: &ScreenInfo) -> String {
    format!(
        "screen {}: {}x{} at ({}, {}), scale {:.2}{}",
        s.id,
        s.rect.width(),
        s.rect.height(),
        s.rect.left(),
        s.rect.top(),
        s.native_per_logical,
        if s.primary { ", primary" } else { "" }
    )
}

pub fn probe_screens() -> anyhow::Result<()> {
    platform::init();
    for s in platform::screens().screens()? {
        println!("{}", describe_screen(&s));
    }
    Ok(())
}

pub fn probe_permissions(prompt: bool) {
    print!("{}", platform::permissions_report(prompt));
}

fn print_clipboard(i: &ClipboardInfo) {
    println!(
        "change {}: {} format(s){}",
        i.change_count,
        i.formats.len(),
        if i.ours { ", written by macwin" } else { "" }
    );
    for f in &i.formats {
        match f.size {
            Some(n) => println!("  {} ({n} bytes)", f.name),
            None => println!("  {}", f.name),
        }
    }
}

pub async fn probe_clipboard(watch: bool) -> anyhow::Result<()> {
    let cb = platform::clipboard();
    println!("clipboard formats and sizes only; the content is never shown");
    print_clipboard(&cb.inspect()?);
    if !watch {
        return Ok(());
    }
    println!("watching for changes (Ctrl+C to stop)");
    let mut last = cb.change_count()?;
    let mut tick = tokio::time::interval(Duration::from_millis(300));
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let now = cb.change_count()?;
                if now != last {
                    last = now;
                    print_clipboard(&cb.inspect()?);
                }
            }
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

// ---------- local spikes ----------

fn primary_center(screens: &[ScreenInfo]) -> Point {
    let s = screens.iter().find(|s| s.primary).unwrap_or(&screens[0]);
    Point::new(
        s.rect.left() + s.rect.width() / 2,
        s.rect.top() + s.rect.height() / 2,
    )
}

fn describe_key(hid: Hid) -> String {
    match keymap::name(hid) {
        Some(n) => format!("{n} ({:#08x})", hid.0),
        None => format!("{:#08x}", hid.0),
    }
}

/// Prints events (mouse moves at most 10 per second) and swallows them when
/// grabbing.
struct PrintSink {
    swallow: bool,
    faults: Mutex<mpsc::Sender<CaptureFault>>,
    last_move: Mutex<Option<Instant>>,
}

impl CaptureSink for PrintSink {
    fn on_event(&self, ev: CapturedEvent) -> bool {
        match ev {
            CapturedEvent::MouseMove { pos, delta } => {
                let mut last = self.last_move.lock().unwrap_or_else(|e| e.into_inner());
                if last.is_none_or(|t| t.elapsed() >= Duration::from_millis(100)) {
                    *last = Some(Instant::now());
                    println!(
                        "move   pos ({:.1}, {:.1}) delta ({:.1}, {:.1})",
                        pos.x, pos.y, delta.x, delta.y
                    );
                }
            }
            CapturedEvent::Button { button, down, pos } => {
                println!(
                    "button {button:?} {} at ({:.1}, {:.1})",
                    if down { "down" } else { "up" },
                    pos.x,
                    pos.y
                );
            }
            CapturedEvent::Wheel { dx, dy, .. } => println!("wheel  dx {dx} dy {dy}"),
            CapturedEvent::Key { hid, down, repeat } => println!(
                "key    {} {}{}",
                describe_key(hid),
                if down { "down" } else { "up" },
                if repeat { " (repeat)" } else { "" }
            ),
        }
        !self.swallow
    }

    fn on_fault(&self, fault: CaptureFault) {
        let _ = self
            .faults
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(fault);
    }
}

pub async fn spike_capture(args: SpikeCaptureArgs) -> anyhow::Result<()> {
    platform::init();
    let screens = platform::screens().screens()?;
    let center = primary_center(&screens);
    let (fault_tx, fault_rx) = mpsc::channel();
    let sink = Arc::new(PrintSink {
        swallow: args.grab,
        faults: Mutex::new(fault_tx),
        last_move: Mutex::new(None),
    });
    let mut capture = platform::capture();
    let control = capture.start(sink)?;
    if args.grab {
        println!(
            "GRABBING for {} s: local input is swallowed and the cursor is hidden at the screen center.\n\
             Press {EMERGENCY_CHORD} to release early.",
            args.seconds
        );
        control.command(GrabCommand::Grab { exit: center });
    } else {
        println!(
            "printing local input for {} s (Ctrl+C to stop); nothing is swallowed",
            args.seconds
        );
    }
    let deadline = Instant::now() + Duration::from_secs(args.seconds);
    let mut outcome = "timeout";
    while Instant::now() < deadline {
        if let Ok(fault) = fault_rx.try_recv() {
            println!("fail-safe fired: {fault:?}");
            outcome = "fail-safe";
            break;
        }
        if args.grab {
            control.renew_lease(Instant::now() + Duration::from_secs(1));
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(200)) => {}
            _ = tokio::signal::ctrl_c() => { outcome = "Ctrl+C"; break; }
        }
    }
    if args.grab {
        control.command(GrabCommand::Release { warp_to: center });
    }
    control.stop();
    drop(capture);
    println!("capture stopped ({outcome})");
    Ok(())
}

pub async fn spike_inject() -> anyhow::Result<()> {
    platform::init();
    let screens = platform::screens().screens()?;
    let c = primary_center(&screens);
    let mut injector = platform::injector()?;
    println!(
        "In 3 seconds the cursor draws a 200 px square around the screen center, then \"hello\" is typed\n\
         with physical keys (the active layout decides the letters: a Russian layout gives \"руддщ\").\n\
         Focus a text field now."
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    let half = 100;
    let corners = [
        Point::new(c.x - half, c.y - half),
        Point::new(c.x + half, c.y - half),
        Point::new(c.x + half, c.y + half),
        Point::new(c.x - half, c.y + half),
        Point::new(c.x - half, c.y - half),
    ];
    injector.inject(InjectCommand::MoveTo(corners[0]))?;
    for w in corners.windows(2) {
        for step in 1..=25 {
            let p = Point::new(
                w[0].x + (w[1].x - w[0].x) * step / 25,
                w[0].y + (w[1].y - w[0].y) * step / 25,
            );
            injector.inject(InjectCommand::MoveTo(p))?;
            tokio::time::sleep(Duration::from_millis(8)).await;
        }
    }
    // HID usages of h, e, l, l, o.
    for usage in [0x0B, 0x08, 0x0F, 0x0F, 0x12] {
        let hid = Hid(0x07_0000 | usage);
        injector.inject(InjectCommand::Key {
            hid,
            down: true,
            repeat: false,
        })?;
        tokio::time::sleep(Duration::from_millis(30)).await;
        injector.inject(InjectCommand::Key {
            hid,
            down: false,
            repeat: false,
        })?;
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    println!("done");
    Ok(())
}

// ---------- KVM ----------

pub async fn spike_kvm(args: SpikeKvmArgs) -> anyhow::Result<()> {
    let os =
        host_os().ok_or_else(|| anyhow!("unsupported OS: macwin runs on macOS and Windows"))?;
    platform::init();
    let (id, _) = load_identity()?;
    println!("local fingerprint: {}", id.fingerprint());
    let backends = platform::backends().context("cannot start the input backends")?;
    let screens = backends.screens.screens()?;
    for s in &screens {
        println!("{}", describe_screen(s));
    }
    let cfg = SessionConfig {
        local_os: os,
        os_version: String::new(),
        device_name: device_name(),
        peer_side: args.peer_side.into(),
        remap: !args.no_remap,
        timings: Timings::default(),
    };
    let hello = kvm_hello(&cfg, screens);
    let (session, role) = match args.listen {
        Some(addr) => (
            accept_one(addr, &id, args.peer_fp, &hello).await?,
            TransportRole::Listener,
        ),
        None => {
            let remote = resolve(
                &args
                    .connect
                    .context("either --listen or --connect is required")?,
            )
            .await?;
            let s = connect(bind_for(remote), remote, &id, args.peer_fp, &hello)
                .await
                .with_context(|| format!("cannot connect to {remote}"))?;
            (s, TransportRole::Dialer)
        }
    };
    println!(
        "connected to {} ({}). Move the cursor over the {:?} edge to cross.\n\
         Emergency release: {EMERGENCY_CHORD}. Ctrl+C here stops the session.",
        session.peer.device_name, session.remote, args.peer_side
    );
    let handle = spawn_session(
        cfg,
        backends,
        Connected {
            link: session.link,
            peer: session.peer,
            role,
        },
    )?;
    let stopper = handle.stopper();
    let link = handle.link();
    let mut roles = handle.role.clone();
    tokio::spawn(async move {
        while roles.changed().await.is_ok() {
            let role = *roles.borrow_and_update();
            println!("role: {role:?}");
        }
    });
    let join = handle.join();
    tokio::pin!(join);
    let end = tokio::select! {
        end = &mut join => end,
        _ = tokio::signal::ctrl_c() => {
            stopper.shutdown();
            join.await
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(2), link.wait_closed()).await;
    println!("session ended: {end:?}");
    match end {
        SessionEnd::Shutdown | SessionEnd::EmergencyStop | SessionEnd::PeerClosed => Ok(()),
        other => Err(anyhow!("the session failed: {other:?}")),
    }
}
