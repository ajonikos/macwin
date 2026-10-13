//! In-memory fakes of every backend trait, for driver and integration tests.
//! All handles are `Clone` and thread-safe.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use macwin_core::types::{
    CaptureFault, CapturedEvent, GrabCommand, Hid, InjectCommand, MouseButton, Point, ScreenInfo,
};

use crate::{
    CLIPBOARD_ORIGIN_FORMAT, CaptureControl, CaptureSink, ClipboardBackend, ClipboardFormat,
    ClipboardInfo, ClipboardSnapshot, InputCapture, InputInjector, LocalClipboard, PlatformError,
    ScreenSource, SecretStore, check_secret_name,
};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panicking test thread must not cascade into every other assertion.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Default)]
struct CaptureState {
    sink: Option<Arc<dyn CaptureSink>>,
    commands: Vec<GrabCommand>,
    lease: Option<Instant>,
    stopped: bool,
    /// The backend was dropped (real backends stop capturing then).
    dropped: bool,
}

/// Fake capture backend. Create with [`FakeCapture::new`] and drive it
/// through the returned [`FakeCaptureHandle`].
pub struct FakeCapture {
    state: Arc<Mutex<CaptureState>>,
}

/// Test-side handle of a [`FakeCapture`].
#[derive(Clone)]
pub struct FakeCaptureHandle {
    state: Arc<Mutex<CaptureState>>,
}

struct FakeControl {
    state: Arc<Mutex<CaptureState>>,
}

impl FakeCapture {
    pub fn new() -> (FakeCapture, FakeCaptureHandle) {
        let state = Arc::new(Mutex::new(CaptureState::default()));
        (
            FakeCapture {
                state: state.clone(),
            },
            FakeCaptureHandle { state },
        )
    }
}

impl InputCapture for FakeCapture {
    fn start(
        &mut self,
        sink: Arc<dyn CaptureSink>,
    ) -> Result<Arc<dyn CaptureControl>, PlatformError> {
        lock(&self.state).sink = Some(sink);
        Ok(Arc::new(FakeControl {
            state: self.state.clone(),
        }))
    }
}

/// Like the real backends, dropping the fake ends its capture: events
/// emitted afterwards are a test failure.
impl Drop for FakeCapture {
    fn drop(&mut self) {
        let sink = {
            let mut s = lock(&self.state);
            s.dropped = true;
            s.stopped = true;
            s.sink.take()
        };
        // Released outside the lock: the sink may own the last session state.
        drop(sink);
    }
}

impl CaptureControl for FakeControl {
    fn command(&self, cmd: GrabCommand) {
        lock(&self.state).commands.push(cmd);
    }

    fn renew_lease(&self, until: Instant) {
        lock(&self.state).lease = Some(until);
    }

    fn stop(&self) {
        lock(&self.state).stopped = true;
    }
}

impl FakeCaptureHandle {
    fn sink(&self) -> Arc<dyn CaptureSink> {
        lock(&self.state)
            .sink
            .clone()
            .expect("FakeCapture::start was not called")
    }

    /// Feeds a physical event to the sink, like a capture thread would, and
    /// returns its deliver/swallow decision. No internal lock is held while
    /// the sink runs, so the sink may call back into the control.
    ///
    /// Panics when the backend was already dropped: a real capture thread
    /// would have stopped.
    pub fn emit(&self, ev: CapturedEvent) -> bool {
        assert!(
            !self.backend_dropped(),
            "event emitted after the capture backend was dropped"
        );
        self.sink().on_event(ev)
    }

    /// Reports a fault to the sink.
    pub fn emit_fault(&self, fault: CaptureFault) {
        assert!(
            !self.backend_dropped(),
            "fault emitted after the capture backend was dropped"
        );
        self.sink().on_fault(fault);
    }

    /// True once the [`FakeCapture`] itself was dropped (it then releases
    /// its sink, like a real backend whose thread ended).
    pub fn backend_dropped(&self) -> bool {
        lock(&self.state).dropped
    }

    pub fn started(&self) -> bool {
        lock(&self.state).sink.is_some()
    }

    /// Every command received, in order.
    pub fn commands(&self) -> Vec<GrabCommand> {
        lock(&self.state).commands.clone()
    }

    /// True when the last Grab was not followed by a Release or Rearm.
    pub fn grabbed(&self) -> bool {
        let state = lock(&self.state);
        matches!(state.commands.last(), Some(GrabCommand::Grab { .. }))
    }

    pub fn lease(&self) -> Option<Instant> {
        lock(&self.state).lease
    }

    pub fn stopped(&self) -> bool {
        lock(&self.state).stopped
    }
}

#[derive(Default)]
struct InjectorState {
    commands: Vec<InjectCommand>,
    keys: BTreeSet<Hid>,
    buttons: BTreeSet<MouseButton>,
    last_pos: Option<Point>,
    unmatched_ups: usize,
}

/// Fake injection backend that records commands and tracks what is held.
pub struct FakeInjector {
    state: Arc<Mutex<InjectorState>>,
}

/// Test-side handle of a [`FakeInjector`].
#[derive(Clone)]
pub struct FakeInjectorHandle {
    state: Arc<Mutex<InjectorState>>,
}

impl FakeInjector {
    pub fn new() -> (FakeInjector, FakeInjectorHandle) {
        let state = Arc::new(Mutex::new(InjectorState::default()));
        (
            FakeInjector {
                state: state.clone(),
            },
            FakeInjectorHandle { state },
        )
    }
}

impl InputInjector for FakeInjector {
    fn inject(&mut self, cmd: InjectCommand) -> Result<(), PlatformError> {
        let mut s = lock(&self.state);
        s.commands.push(cmd);
        match cmd {
            InjectCommand::MoveTo(p) => s.last_pos = Some(p),
            InjectCommand::Button { button, down, at } => {
                s.last_pos = Some(at);
                if down {
                    s.buttons.insert(button);
                } else if !s.buttons.remove(&button) {
                    s.unmatched_ups += 1;
                }
            }
            InjectCommand::Key { hid, down, .. } => {
                if down {
                    s.keys.insert(hid);
                } else if !s.keys.remove(&hid) {
                    s.unmatched_ups += 1;
                }
            }
            InjectCommand::Wheel { .. } => {}
        }
        Ok(())
    }
}

impl FakeInjectorHandle {
    /// Every command received, in order.
    pub fn commands(&self) -> Vec<InjectCommand> {
        lock(&self.state).commands.clone()
    }

    /// Keys currently held down by injection.
    pub fn pressed_keys(&self) -> BTreeSet<Hid> {
        lock(&self.state).keys.clone()
    }

    /// Buttons currently held down by injection.
    pub fn pressed_buttons(&self) -> Vec<MouseButton> {
        lock(&self.state).buttons.iter().copied().collect()
    }

    pub fn last_pos(&self) -> Option<Point> {
        lock(&self.state).last_pos
    }

    /// Ups for keys/buttons that were not held (should stay 0).
    pub fn unmatched_ups(&self) -> usize {
        lock(&self.state).unmatched_ups
    }

    /// Forgets recorded commands (held state is kept).
    pub fn clear(&self) {
        lock(&self.state).commands.clear();
    }
}

/// Fake screen source returning a fixed list.
#[derive(Debug, Clone)]
pub struct FakeScreens(pub Vec<ScreenInfo>);

impl ScreenSource for FakeScreens {
    fn screens(&self) -> Result<Vec<ScreenInfo>, PlatformError> {
        Ok(self.0.clone())
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
enum FakeContent {
    #[default]
    Empty,
    Text(String),
    Sensitive(String),
    Image,
}

#[derive(Default)]
struct ClipboardState {
    count: u64,
    content: FakeContent,
    ours: bool,
    writes: Vec<String>,
    text_reads: usize,
}

/// Fake system clipboard with macwin's origin-marker semantics.
pub struct FakeClipboard {
    state: Arc<Mutex<ClipboardState>>,
}

/// Test-side handle of a [`FakeClipboard`]: plays the user and other apps.
#[derive(Clone)]
pub struct FakeClipboardHandle {
    state: Arc<Mutex<ClipboardState>>,
}

impl FakeClipboard {
    pub fn new() -> (FakeClipboard, FakeClipboardHandle) {
        let state = Arc::new(Mutex::new(ClipboardState::default()));
        (
            FakeClipboard {
                state: state.clone(),
            },
            FakeClipboardHandle { state },
        )
    }
}

impl FakeClipboardHandle {
    fn set(&self, content: FakeContent) {
        let mut s = lock(&self.state);
        s.count += 1;
        s.content = content;
        s.ours = false;
    }

    /// The user copies text in another app.
    pub fn copy_text(&self, text: &str) {
        self.set(FakeContent::Text(text.into()));
    }

    /// A password manager copies a concealed text.
    pub fn copy_sensitive(&self, text: &str) {
        self.set(FakeContent::Sensitive(text.into()));
    }

    /// The user copies an image.
    pub fn copy_image(&self) {
        self.set(FakeContent::Image);
    }

    /// A clipboard manager re-copies the current content without macwin's
    /// origin marker.
    pub fn recopy(&self) {
        let content = lock(&self.state).content.clone();
        self.set(content);
    }

    /// The plain text on the clipboard (sensitive text included).
    pub fn text(&self) -> Option<String> {
        match &lock(&self.state).content {
            FakeContent::Text(t) | FakeContent::Sensitive(t) => Some(t.clone()),
            FakeContent::Empty | FakeContent::Image => None,
        }
    }

    /// Every text macwin wrote, in order.
    pub fn writes(&self) -> Vec<String> {
        lock(&self.state).writes.clone()
    }

    /// How many times macwin read text content.
    pub fn text_reads(&self) -> usize {
        lock(&self.state).text_reads
    }

    pub fn change_count(&self) -> u64 {
        lock(&self.state).count
    }
}

impl ClipboardBackend for FakeClipboard {
    fn change_count(&self) -> Result<u64, PlatformError> {
        Ok(lock(&self.state).count)
    }

    fn inspect(&self) -> Result<ClipboardInfo, PlatformError> {
        let s = lock(&self.state);
        let mut formats = Vec::new();
        match &s.content {
            FakeContent::Empty => {}
            FakeContent::Text(t) => formats.push(ClipboardFormat {
                name: "text".into(),
                size: Some(t.len()),
            }),
            FakeContent::Sensitive(t) => {
                formats.push(ClipboardFormat {
                    name: "text".into(),
                    size: Some(t.len()),
                });
                formats.push(ClipboardFormat {
                    name: crate::MAC_SENSITIVE_TYPES[0].into(),
                    size: Some(0),
                });
            }
            FakeContent::Image => formats.push(ClipboardFormat {
                name: "image".into(),
                size: None,
            }),
        }
        if s.ours {
            formats.push(ClipboardFormat {
                name: CLIPBOARD_ORIGIN_FORMAT.into(),
                size: Some(1),
            });
        }
        Ok(ClipboardInfo {
            change_count: s.count,
            formats,
            ours: s.ours,
        })
    }

    fn read_text(&self) -> Result<Option<String>, PlatformError> {
        let mut s = lock(&self.state);
        s.text_reads += 1;
        Ok(match &s.content {
            FakeContent::Text(t) | FakeContent::Sensitive(t) => Some(t.clone()),
            FakeContent::Empty | FakeContent::Image => None,
        })
    }

    fn snapshot(&self, max_text_bytes: usize) -> Result<ClipboardSnapshot, PlatformError> {
        let mut s = lock(&self.state);
        let content = if s.ours {
            LocalClipboard::Ours
        } else {
            match s.content.clone() {
                FakeContent::Sensitive(_) => LocalClipboard::Sensitive,
                FakeContent::Empty | FakeContent::Image => LocalClipboard::NoText,
                FakeContent::Text(t) if t.len() > max_text_bytes => {
                    LocalClipboard::TooLarge { bytes: t.len() }
                }
                FakeContent::Text(t) => {
                    s.text_reads += 1;
                    LocalClipboard::Text(t)
                }
            }
        };
        Ok(ClipboardSnapshot {
            change_count: s.count,
            content,
        })
    }

    fn write_text(&self, text: &str) -> Result<u64, PlatformError> {
        let mut s = lock(&self.state);
        s.count += 1;
        s.content = FakeContent::Text(text.into());
        s.ours = true;
        s.writes.push(text.into());
        Ok(s.count)
    }
}

/// In-memory [`SecretStore`]; clones share the same secrets.
#[derive(Debug, Clone, Default)]
pub struct MemorySecretStore {
    secrets: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
}

impl MemorySecretStore {
    pub fn new() -> MemorySecretStore {
        MemorySecretStore::default()
    }
}

impl SecretStore for MemorySecretStore {
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>, PlatformError> {
        check_secret_name(name)?;
        Ok(lock(&self.secrets).get(name).cloned())
    }

    fn store(&self, name: &str, secret: &[u8]) -> Result<(), PlatformError> {
        check_secret_name(name)?;
        lock(&self.secrets).insert(name.into(), secret.to_vec());
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<(), PlatformError> {
        check_secret_name(name)?;
        lock(&self.secrets).remove(name);
        Ok(())
    }

    fn describe(&self, name: &str) -> String {
        format!("memory ({name})")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use macwin_core::types::{PointF, Rect};

    use super::*;

    /// A sink that grabs on every event through the control it is given:
    /// exercises the re-entrant call path.
    struct GrabbingSink {
        control: OnceLock<Arc<dyn CaptureControl>>,
    }

    impl CaptureSink for GrabbingSink {
        fn on_event(&self, _ev: CapturedEvent) -> bool {
            self.control
                .get()
                .expect("control set")
                .command(GrabCommand::Grab {
                    exit: Point::new(1, 2),
                });
            false
        }

        fn on_fault(&self, _fault: CaptureFault) {
            self.control
                .get()
                .expect("control set")
                .command(GrabCommand::Rearm);
        }
    }

    #[test]
    fn sink_can_call_back_into_the_control_during_emit() {
        let (mut capture, handle) = FakeCapture::new();
        let sink = Arc::new(GrabbingSink {
            control: OnceLock::new(),
        });
        let control = capture.start(sink.clone()).unwrap();
        assert!(sink.control.set(control.clone()).is_ok());
        assert!(handle.started());
        let ev = CapturedEvent::MouseMove {
            pos: PointF::new(0.0, 0.0),
            delta: PointF::new(1.0, 0.0),
        };
        assert!(!handle.emit(ev));
        assert!(handle.grabbed());
        handle.emit_fault(CaptureFault::TapDisabled);
        assert!(!handle.grabbed());
        let now = Instant::now();
        control.renew_lease(now);
        assert_eq!(handle.lease(), Some(now));
        control.stop();
        assert!(handle.stopped());
        assert_eq!(
            handle.commands(),
            vec![
                GrabCommand::Grab {
                    exit: Point::new(1, 2)
                },
                GrabCommand::Rearm
            ]
        );
    }

    #[test]
    fn injector_tracks_pressed_state() {
        let (mut inj, handle) = FakeInjector::new();
        let a = Hid(0x07_0004);
        let at = Point::new(3, 4);
        inj.inject(InjectCommand::Key {
            hid: a,
            down: true,
            repeat: false,
        })
        .unwrap();
        inj.inject(InjectCommand::Key {
            hid: a,
            down: true,
            repeat: true,
        })
        .unwrap();
        inj.inject(InjectCommand::Button {
            button: MouseButton::Left,
            down: true,
            at,
        })
        .unwrap();
        assert_eq!(handle.pressed_keys(), [a].into_iter().collect());
        assert_eq!(handle.pressed_buttons(), vec![MouseButton::Left]);
        assert_eq!(handle.last_pos(), Some(at));
        inj.inject(InjectCommand::Key {
            hid: a,
            down: false,
            repeat: false,
        })
        .unwrap();
        inj.inject(InjectCommand::Key {
            hid: a,
            down: false,
            repeat: false,
        })
        .unwrap();
        inj.inject(InjectCommand::Button {
            button: MouseButton::Left,
            down: false,
            at,
        })
        .unwrap();
        assert!(handle.pressed_keys().is_empty() && handle.pressed_buttons().is_empty());
        assert_eq!(handle.unmatched_ups(), 1);
        assert_eq!(handle.commands().len(), 6);
        handle.clear();
        assert!(handle.commands().is_empty());
    }

    #[test]
    fn fake_clipboard_classifies_like_a_real_backend() {
        let (cb, user) = FakeClipboard::new();
        let snap = |cb: &FakeClipboard| cb.snapshot(5).unwrap().content;
        assert_eq!(snap(&cb), LocalClipboard::NoText);
        user.copy_text("hello");
        assert_eq!(snap(&cb), LocalClipboard::Text("hello".into()));
        user.copy_text("longer");
        assert_eq!(snap(&cb), LocalClipboard::TooLarge { bytes: 6 });
        user.copy_sensitive("pw");
        assert_eq!(snap(&cb), LocalClipboard::Sensitive);
        user.copy_image();
        assert_eq!(snap(&cb), LocalClipboard::NoText);
        let reads = user.text_reads();
        let count = cb.write_text("abc").unwrap();
        assert_eq!(count, user.change_count());
        assert_eq!(snap(&cb), LocalClipboard::Ours);
        assert_eq!(user.text_reads(), reads, "own writes are never read");
        assert!(cb.inspect().unwrap().ours);
        user.recopy();
        assert_eq!(snap(&cb), LocalClipboard::Text("abc".into()));
        assert_eq!(user.writes(), vec!["abc".to_string()]);
        assert_eq!(cb.read_text().unwrap().as_deref(), Some("abc"));
    }

    #[test]
    fn memory_secret_store_is_shared_between_clones() {
        let a = MemorySecretStore::new();
        let b = a.clone();
        a.store("identity", b"k").unwrap();
        assert_eq!(b.load("identity").unwrap().as_deref(), Some(&b"k"[..]));
        b.delete("identity").unwrap();
        assert_eq!(a.load("identity").unwrap(), None);
        assert!(a.store("../x", b"k").is_err());
    }

    #[test]
    fn fake_screens_returns_its_list() {
        let s = ScreenInfo {
            id: 1,
            rect: Rect::new(0, 0, 10, 10).unwrap(),
            native_per_logical: 1.0,
            primary: true,
        };
        assert_eq!(FakeScreens(vec![s]).screens().unwrap(), vec![s]);
    }
}
