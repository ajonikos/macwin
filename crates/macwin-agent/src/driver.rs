//! Session driver: wires a capture backend, the Engine, a [`Link`] and an
//! injector together. Transport-agnostic (it only sees `macwin_net::link`
//! types), so Phase 0b's `spike kvm` plugs the real OS backends in and the
//! tests plug fakes in.
//!
//! Concurrency: one `Arc<Mutex<Core>>`. The capture thread locks it from
//! `CaptureSink::on_event`; the async loop locks it for link messages, ticks
//! and shutdown. Nothing awaits while holding the lock, and the lock order
//! is always Core -> backend internals.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use macwin_core::clipboard::{ClipboardSync, LocalClipboard, RemoteOutcome};
use macwin_core::engine::{
    Action, CloseReason, Engine, EngineConfig, PeerInfo, Timings, TransportRole,
};
use macwin_core::msg::{
    CAP_CLIPBOARD_TEXT, CAP_INPUT_CAPTURE, CAP_INPUT_INJECT, ClipboardMsg, Hello,
};
use macwin_core::types::{CaptureFault, CapturedEvent, Os, RoleKind, ScreenInfo, Side};
use macwin_net::codes;
use macwin_net::link::{Inbound, Link, LinkClosed, LinkSender};
use macwin_platform::{
    CaptureControl, CaptureSink, ClipboardBackend, InputCapture, InputInjector, PlatformError,
    ScreenSource,
};
use macwin_proto::PROTOCOL_VERSION;
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tracing::{debug, info, warn};

use crate::clipboard::{ClipboardWorker, POLL_INTERVAL};

/// Static configuration of one KVM session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub local_os: Os,
    pub os_version: String,
    pub device_name: String,
    /// "The peer is to my left/right".
    pub peer_side: Side,
    /// Apply the Ctrl <-> Cmd/Win remap when this machine controls.
    pub remap: bool,
    pub timings: Timings,
    /// Share plain-text clipboard contents; `None` turns sharing off.
    pub clipboard: Option<ClipboardSettings>,
}

/// Clipboard sharing settings of a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardSettings {
    /// Texts above this size are neither sent nor written.
    pub max_text_bytes: usize,
    /// How often the clipboard is checked for changes.
    pub poll: Duration,
}

impl ClipboardSettings {
    pub fn new(max_text_bytes: usize) -> ClipboardSettings {
        ClipboardSettings {
            max_text_bytes,
            poll: POLL_INTERVAL,
        }
    }
}

/// The OS (or fake) backends of one machine.
pub struct Backends {
    pub capture: Box<dyn InputCapture>,
    pub injector: Box<dyn InputInjector>,
    pub screens: Box<dyn ScreenSource>,
    /// Needed only when the session shares the clipboard.
    pub clipboard: Option<Box<dyn ClipboardBackend>>,
}

/// The Hello a KVM agent sends: it can both control and be controlled, and
/// shares its clipboard when configured to.
pub fn kvm_hello(cfg: &SessionConfig, screens: Vec<ScreenInfo>) -> Hello {
    let mut capabilities = vec![CAP_INPUT_CAPTURE.into(), CAP_INPUT_INJECT.into()];
    if cfg.clipboard.is_some() {
        capabilities.push(CAP_CLIPBOARD_TEXT.into());
    }
    Hello {
        protocol_version: PROTOCOL_VERSION,
        capabilities,
        os: cfg.local_os,
        os_version: cfg.os_version.clone(),
        device_name: cfg.device_name.clone(),
        screens,
        peer_side: Some(cfg.peer_side),
        agent_version: env!("CARGO_PKG_VERSION").into(),
    }
}

/// What the Engine needs from the peer's Hello.
pub fn peer_info(h: &Hello) -> PeerInfo {
    PeerInfo {
        os: h.os,
        screens: h.screens.clone(),
        capabilities: h.capabilities.clone(),
        device_name: h.device_name.clone(),
    }
}

/// An established connection handed to the driver.
pub struct Connected {
    pub link: Link,
    pub peer: Hello,
    pub role: TransportRole,
}

/// Why a session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEnd {
    PeerClosed,
    HeartbeatTimeout,
    ProtocolViolation(String),
    Shutdown,
    /// The emergency chord was pressed: local control is back and the agent
    /// should exit.
    EmergencyStop,
    Transport(String),
    Internal(String),
}

/// Asks a running session to stop; cheap to clone.
#[derive(Clone)]
pub struct Stopper(Arc<Notify>);

impl Stopper {
    /// Graceful stop (Leave to the peer, release, close).
    pub fn shutdown(&self) {
        self.0.notify_one();
    }
}

/// A running session.
pub struct SessionHandle {
    /// The current role, updated after every Engine step.
    pub role: watch::Receiver<RoleKind>,
    shutdown: Arc<Notify>,
    link: LinkSender,
    join: JoinHandle<SessionEnd>,
}

impl SessionHandle {
    /// Asks for a graceful stop (Leave to the peer, release, close).
    pub fn shutdown(&self) {
        self.shutdown.notify_one();
    }

    /// A stop handle that outlives [`SessionHandle::join`].
    pub fn stopper(&self) -> Stopper {
        Stopper(self.shutdown.clone())
    }

    /// The session's link, for [`LinkSender::wait_closed`] before exiting.
    pub fn link(&self) -> LinkSender {
        self.link.clone()
    }

    /// Waits for the session to end.
    pub async fn join(self) -> SessionEnd {
        self.join
            .await
            .unwrap_or_else(|e| SessionEnd::Internal(format!("driver task failed: {e}")))
    }

    pub fn is_finished(&self) -> bool {
        self.join.is_finished()
    }
}

struct Core {
    engine: Engine,
    injector: Box<dyn InputInjector>,
    capture: Option<Arc<dyn CaptureControl>>,
    link: LinkSender,
    role_tx: watch::Sender<RoleKind>,
    end: Option<SessionEnd>,
}

fn lock(core: &Mutex<Core>) -> MutexGuard<'_, Core> {
    // A panic elsewhere must not leave input grabbed: keep going.
    core.lock().unwrap_or_else(|e| e.into_inner())
}

fn end_for_reason(r: CloseReason) -> SessionEnd {
    match r {
        CloseReason::HeartbeatTimeout => SessionEnd::HeartbeatTimeout,
        CloseReason::ProtocolViolation(s) => SessionEnd::ProtocolViolation(s.to_string()),
        CloseReason::Shutdown => SessionEnd::Shutdown,
        CloseReason::Internal(s) => SessionEnd::Internal(s.to_string()),
    }
}

fn end_for_link(c: LinkClosed) -> SessionEnd {
    match c {
        LinkClosed::PeerClosed { .. } => SessionEnd::PeerClosed,
        LinkClosed::LocalClosed { code } => {
            SessionEnd::Internal(format!("link closed locally (code {code})"))
        }
        LinkClosed::TimedOut => SessionEnd::Transport("the connection timed out".into()),
        LinkClosed::Transport(s) => SessionEnd::Transport(s),
        LinkClosed::ProtocolViolation(s) => SessionEnd::ProtocolViolation(s),
    }
}

impl Core {
    fn set_end(&mut self, end: SessionEnd) {
        if self.end.is_none() {
            self.end = Some(end);
        }
    }

    /// Executes Engine actions in order.
    fn dispatch(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::SendControl(m) => {
                    if let Err(e) = self.link.send_control(m) {
                        self.set_end(SessionEnd::Internal(format!(
                            "cannot send a control message: {e}"
                        )));
                    }
                }
                Action::SendInput(m) => {
                    if let Err(e) = self.link.send_input(m) {
                        self.set_end(SessionEnd::Internal(format!(
                            "cannot send an input message: {e}"
                        )));
                    }
                }
                Action::SendDatagram(m) => self.link.send_datagram(m),
                Action::Inject(cmd) => {
                    if let Err(e) = self.injector.inject(cmd) {
                        warn!(error = %e, "injection failed");
                    }
                }
                Action::Capture(cmd) => {
                    if let Some(c) = &self.capture {
                        c.command(cmd);
                    }
                }
                Action::RenewLease(until) => {
                    if let Some(c) = &self.capture {
                        c.renew_lease(until);
                    }
                }
                Action::Close(reason) => {
                    self.link.close(codes::code_for(&reason));
                    self.set_end(end_for_reason(reason));
                }
            }
        }
        self.role_tx.send_replace(self.engine.role());
    }
}

/// Clipboard sharing of a running session.
struct Clip {
    sync: ClipboardSync,
    worker: ClipboardWorker,
}

impl Clip {
    fn on_local(&mut self, content: LocalClipboard, link: &LinkSender) {
        let out = self.sync.on_local_change(content);
        debug!(kind = ?out.kind, "local clipboard change");
        if let Some(m) = out.send
            && let Err(e) = link.send_clipboard(m)
        {
            debug!(error = %e, "clipboard update not sent");
        }
    }

    fn on_remote(&mut self, m: ClipboardMsg) {
        match self.sync.on_remote(m) {
            RemoteOutcome::Write(text) => {
                debug!(bytes = text.len(), "writing the peer's clipboard text");
                self.worker.write(text);
            }
            other => debug!(outcome = ?other, "peer clipboard update"),
        }
    }
}

/// Starts clipboard sharing when both sides want it.
fn start_clipboard(
    settings: Option<ClipboardSettings>,
    backend: Option<Box<dyn ClipboardBackend>>,
    peer: &Hello,
    role: TransportRole,
) -> Option<(Clip, mpsc::Receiver<LocalClipboard>)> {
    let settings = settings?;
    let Some(backend) = backend else {
        warn!("clipboard sharing is off: no clipboard backend");
        return None;
    };
    if !peer.has_capability(CAP_CLIPBOARD_TEXT) {
        info!("clipboard sharing is off: the peer does not share its clipboard");
        return None;
    }
    match ClipboardWorker::start(backend, settings.max_text_bytes, settings.poll) {
        Ok((worker, events)) => {
            info!("clipboard sharing is on");
            let sync = ClipboardSync::new(role, settings.max_text_bytes);
            Some((Clip { sync, worker }, events))
        }
        Err(e) => {
            warn!(error = %e, "clipboard sharing is off: the clipboard is unavailable");
            None
        }
    }
}

/// A started capture backend and its control. The backend must live as long
/// as the session: real backends stop their capture thread when dropped.
struct Capture {
    control: Arc<dyn CaptureControl>,
    _backend: Box<dyn InputCapture>,
}

/// The next local clipboard change, or never when sharing is off.
async fn next_local_clip(
    events: &mut Option<mpsc::Receiver<LocalClipboard>>,
) -> Option<LocalClipboard> {
    match events {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

struct Sink {
    core: Arc<Mutex<Core>>,
    wake: Arc<Notify>,
}

impl CaptureSink for Sink {
    fn on_event(&self, ev: CapturedEvent) -> bool {
        let mut core = lock(&self.core);
        if core.end.is_some() {
            // The session is over: local input stays local.
            return true;
        }
        let out = core.engine.on_local_event(Instant::now(), ev);
        core.dispatch(out.actions);
        if core.end.is_some() {
            self.wake.notify_one();
        }
        out.deliver
    }

    fn on_fault(&self, fault: CaptureFault) {
        warn!(?fault, "capture fault");
        let mut core = lock(&self.core);
        let now = Instant::now();
        let actions = core.engine.on_capture_fault(now, fault);
        core.dispatch(actions);
        if fault == CaptureFault::EmergencyChord {
            // The user wants out: end the session cleanly (the peer releases
            // whatever we injected there) and let the agent exit. The reason is
            // recorded first so the Engine's Close(Shutdown) does not replace it.
            core.set_end(SessionEnd::EmergencyStop);
            let actions = core.engine.shutdown(now);
            core.dispatch(actions);
        }
        if core.end.is_some() {
            self.wake.notify_one();
        }
    }
}

/// Starts the session: reads the screens, builds the Engine, starts capture
/// and runs the driver loop on the current tokio runtime.
pub fn spawn_session(
    cfg: SessionConfig,
    backends: Backends,
    conn: Connected,
) -> Result<SessionHandle, PlatformError> {
    let Backends {
        mut capture,
        injector,
        screens,
        clipboard,
    } = backends;
    let Connected { link, peer, role } = conn;
    let Link { tx, rx } = link;
    let fail = |tx: &LinkSender, e: PlatformError| {
        tx.close_now(codes::INTERNAL);
        e
    };
    let local_screens = screens.screens().map_err(|e| fail(&tx, e))?;
    let engine = Engine::new(EngineConfig {
        local_os: cfg.local_os,
        local_screens,
        peer_side: cfg.peer_side,
        remap: cfg.remap,
        timings: cfg.timings,
    })
    .map_err(|e| {
        fail(
            &tx,
            PlatformError::Backend(format!("invalid local screens: {e}")),
        )
    })?;
    if peer.peer_side == Some(cfg.peer_side) {
        warn!(
            side = ?cfg.peer_side,
            "both machines place their peer on the same side; check the peer-side settings"
        );
    }
    let (role_tx, role_rx) = watch::channel(RoleKind::Idle);
    let core = Arc::new(Mutex::new(Core {
        engine,
        injector,
        capture: None,
        link: tx.clone(),
        role_tx,
        end: None,
    }));
    let wake = Arc::new(Notify::new());
    let control = capture
        .start(Arc::new(Sink {
            core: core.clone(),
            wake: wake.clone(),
        }))
        .map_err(|e| fail(&tx, e))?;
    {
        let mut c = lock(&core);
        c.capture = Some(control.clone());
        let actions = c
            .engine
            .on_connected(Instant::now(), peer_info(&peer), role);
        c.dispatch(actions);
    }
    let clip = start_clipboard(cfg.clipboard, clipboard, &peer, role);
    let shutdown = Arc::new(Notify::new());
    // The run task owns the capture backend: dropping it stops capturing.
    let join = tokio::spawn(run(
        core,
        rx,
        cfg.timings,
        shutdown.clone(),
        wake,
        Capture {
            control,
            _backend: capture,
        },
        tx.clone(),
        clip,
    ));
    Ok(SessionHandle {
        role: role_rx,
        shutdown,
        link: tx,
        join,
    })
}

#[allow(clippy::too_many_arguments)]
async fn run(
    core: Arc<Mutex<Core>>,
    mut rx: mpsc::Receiver<Inbound>,
    timings: Timings,
    shutdown: Arc<Notify>,
    wake: Arc<Notify>,
    capture: Capture,
    link: LinkSender,
    clip: Option<(Clip, mpsc::Receiver<LocalClipboard>)>,
) -> SessionEnd {
    let mut tick = tokio::time::interval(timings.tick);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let (mut clip, mut clip_events) = match clip {
        Some((c, events)) => (Some(c), Some(events)),
        None => (None, None),
    };
    loop {
        tokio::select! {
            content = next_local_clip(&mut clip_events) => {
                match (content, clip.as_mut()) {
                    (Some(content), Some(clip)) => clip.on_local(content, &link),
                    _ => {
                        warn!("the clipboard thread stopped; clipboard sharing is off");
                        clip_events = None;
                    }
                }
            }
            msg = rx.recv() => {
                let now = Instant::now();
                let mut c = lock(&core);
                match msg {
                    Some(Inbound::Clipboard(m)) => match clip.as_mut() {
                        Some(clip) => clip.on_remote(m),
                        None => debug!("clipboard update ignored: sharing is off"),
                    },
                    Some(Inbound::Control(m)) => {
                        let a = c.engine.on_control(now, m);
                        c.dispatch(a);
                    }
                    Some(Inbound::Input(m)) => {
                        let a = c.engine.on_input(now, m);
                        c.dispatch(a);
                    }
                    Some(Inbound::Datagram(m)) => {
                        let a = c.engine.on_datagram(now, m);
                        c.dispatch(a);
                    }
                    Some(Inbound::Closed(reason)) => {
                        info!(?reason, "link closed");
                        let a = c.engine.on_disconnected(now);
                        c.dispatch(a);
                        c.set_end(end_for_link(reason));
                    }
                    None => {
                        let a = c.engine.on_disconnected(now);
                        c.dispatch(a);
                        c.set_end(SessionEnd::Transport("the link went away".into()));
                    }
                }
            }
            _ = tick.tick() => {
                let mut c = lock(&core);
                let a = c.engine.on_tick(Instant::now());
                c.dispatch(a);
            }
            () = shutdown.notified() => {
                let mut c = lock(&core);
                let a = c.engine.shutdown(Instant::now());
                c.dispatch(a);
                c.set_end(SessionEnd::Shutdown);
            }
            () = wake.notified() => {}
        }
        let mut c = lock(&core);
        if let Some(end) = c.end.clone() {
            // Make sure nothing stays injected or grabbed, whatever ended it.
            if c.engine.has_session() {
                let a = c.engine.on_disconnected(Instant::now());
                c.dispatch(a);
            }
            if !c.link.is_closed() {
                c.link.close_now(codes::INTERNAL);
            }
            // Core -> control -> backend state -> sink -> Core is a cycle:
            // break it so the session (and its connection) can be freed.
            c.capture = None;
            drop(c);
            capture.control.stop();
            let clip = clip.take();
            // Dropping the backends joins their threads: keep that off the
            // async workers.
            tokio::task::spawn_blocking(move || {
                drop(capture);
                drop(clip);
            });
            info!(?end, "session ended");
            return end;
        }
    }
}
