//! In-memory fakes of every backend trait, for driver and integration tests.
//! All handles are `Clone` and thread-safe.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use macwin_core::types::{
    CaptureFault, CapturedEvent, GrabCommand, Hid, InjectCommand, MouseButton, Point, ScreenInfo,
};

use crate::{
    CaptureControl, CaptureSink, InputCapture, InputInjector, PlatformError, ScreenSource,
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
    pub fn emit(&self, ev: CapturedEvent) -> bool {
        self.sink().on_event(ev)
    }

    /// Reports a fault to the sink.
    pub fn emit_fault(&self, fault: CaptureFault) {
        self.sink().on_fault(fault);
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
