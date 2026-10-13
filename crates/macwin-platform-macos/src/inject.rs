//! Injection with CGEventPost. Every event carries `INJECT_TAG` so our own
//! capture tap ignores it.

use std::time::{Duration, Instant};

use macwin_core::keymap::hid_to_mac;
use macwin_core::keys::ButtonSet;
use macwin_core::os::INJECT_TAG;
use macwin_core::os::macos::{ClickCounter, WheelAccumulator, flags_for};
use macwin_core::types::{InjectCommand, ModifierMask, MouseButton, Point, PointF, WheelUnit};
use macwin_platform::{InputInjector, PlatformError};
use objc2_app_kit::NSEvent;
use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
    CGEventType, CGMouseButton, CGPreflightPostEventAccess, CGScrollEventUnit,
};

/// CGEventPost injection backend.
pub struct MacInjector {
    source: CFRetained<CGEventSource>,
    buttons: ButtonSet,
    mods: ModifierMask,
    clicks: ClickCounter,
    click_state: [i64; 5],
    wheel_x: WheelAccumulator,
    wheel_y: WheelAccumulator,
    pos: Option<Point>,
    double_click: Duration,
}

// SAFETY: CGEventSource is a CoreFoundation object; CF retain/release are
// thread-safe and the injector only touches it through `&mut self`, so it is
// never used from two threads at once.
unsafe impl Send for MacInjector {}

impl MacInjector {
    /// Fails without the Accessibility permission (posting would silently do
    /// nothing).
    pub fn new() -> Result<MacInjector, PlatformError> {
        if !CGPreflightPostEventAccess() {
            return Err(PlatformError::PermissionDenied(
                "posting events needs Accessibility: System Settings > Privacy & Security > Accessibility".into(),
            ));
        }
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .ok_or_else(|| PlatformError::Backend("cannot create a CGEventSource".into()))?;
        CGEventSource::set_user_data(Some(&source), INJECT_TAG as i64);
        // Injected events must not mute the local mouse and keyboard (the
        // default suppression is 0.25 s), or local takeover would lag.
        CGEventSource::set_local_events_suppression_interval(Some(&source), 0.0);
        let double_click = Duration::from_secs_f64(NSEvent::doubleClickInterval().clamp(0.1, 5.0));
        Ok(MacInjector {
            source,
            buttons: ButtonSet::new(),
            mods: ModifierMask::EMPTY,
            clicks: ClickCounter::new(),
            click_state: [1; 5],
            wheel_x: WheelAccumulator::pixels(),
            wheel_y: WheelAccumulator::pixels(),
            pos: None,
            double_click,
        })
    }

    fn post(&self, e: Option<CFRetained<CGEvent>>) -> Result<(), PlatformError> {
        let e = e.ok_or_else(|| PlatformError::Backend("cannot create a CGEvent".into()))?;
        CGEvent::set_integer_value_field(
            Some(&e),
            CGEventField::EventSourceUserData,
            INJECT_TAG as i64,
        );
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&e));
        Ok(())
    }

    fn move_to(&mut self, p: Point) -> Result<(), PlatformError> {
        let (t, b) = if self.buttons.contains(MouseButton::Left) {
            (CGEventType::LeftMouseDragged, CGMouseButton::Left)
        } else if self.buttons.contains(MouseButton::Right) {
            (CGEventType::RightMouseDragged, CGMouseButton::Right)
        } else if !self.buttons.is_empty() {
            (CGEventType::OtherMouseDragged, CGMouseButton::Center)
        } else {
            (CGEventType::MouseMoved, CGMouseButton::Left)
        };
        let e = CGEvent::new_mouse_event(Some(&self.source), t, point(p), b);
        if let (Some(e), Some(prev)) = (&e, self.pos) {
            // Apps that read relative motion (games, 3D tools) see it too.
            CGEvent::set_integer_value_field(
                Some(e),
                CGEventField::MouseEventDeltaX,
                i64::from(p.x - prev.x),
            );
            CGEvent::set_integer_value_field(
                Some(e),
                CGEventField::MouseEventDeltaY,
                i64::from(p.y - prev.y),
            );
        }
        self.pos = Some(p);
        self.post(e)
    }

    fn button(&mut self, button: MouseButton, down: bool, at: Point) -> Result<(), PlatformError> {
        let (t, cg) = match (button, down) {
            (MouseButton::Left, true) => (CGEventType::LeftMouseDown, CGMouseButton::Left),
            (MouseButton::Left, false) => (CGEventType::LeftMouseUp, CGMouseButton::Left),
            (MouseButton::Right, true) => (CGEventType::RightMouseDown, CGMouseButton::Right),
            (MouseButton::Right, false) => (CGEventType::RightMouseUp, CGMouseButton::Right),
            (_, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center),
            (_, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center),
        };
        let slot = usize::from(button.bit());
        if down {
            self.buttons.insert(button);
            self.click_state[slot] = self.clicks.on_down(
                button,
                PointF::new(f64::from(at.x), f64::from(at.y)),
                Instant::now(),
                self.double_click,
            );
        } else {
            self.buttons.remove(button);
        }
        let e = CGEvent::new_mouse_event(Some(&self.source), t, point(at), cg);
        if let Some(e) = &e {
            CGEvent::set_integer_value_field(
                Some(e),
                CGEventField::MouseEventClickState,
                self.click_state[slot],
            );
            let number = match button {
                MouseButton::Left => 0,
                MouseButton::Right => 1,
                MouseButton::Middle => 2,
                MouseButton::Back => 3,
                MouseButton::Forward => 4,
            };
            CGEvent::set_integer_value_field(Some(e), CGEventField::MouseEventButtonNumber, number);
        }
        self.pos = Some(at);
        self.post(e)
    }

    fn wheel(&mut self, dx: i32, dy: i32, unit: WheelUnit) -> Result<(), PlatformError> {
        // Pixels, not lines: high-resolution deltas (precision touchpads)
        // stay smooth instead of being rounded to whole lines.
        let (px_x, px_y) = match unit {
            WheelUnit::Notch120 => (self.wheel_x.push(dx), self.wheel_y.push(dy)),
            WheelUnit::Pixel => (dx, dy),
        };
        if px_x == 0 && px_y == 0 {
            return Ok(());
        }
        // wheel1 is vertical (up > 0); wheel2 is horizontal with left > 0
        // (sign unverified on hardware, mirrors scroll_from_fixed).
        let e = CGEvent::new_scroll_wheel_event2(
            Some(&self.source),
            CGScrollEventUnit::Pixel,
            2,
            px_y,
            -px_x,
            0,
        );
        self.post(e)
    }

    fn key(
        &mut self,
        hid: macwin_core::types::Hid,
        down: bool,
        repeat: bool,
    ) -> Result<(), PlatformError> {
        let Some(code) = hid_to_mac(hid) else {
            return Ok(()); // no macOS key for this usage
        };
        if hid.is_modifier() {
            if down {
                self.mods.insert(hid);
            } else {
                self.mods.remove(hid);
            }
        }
        let e = CGEvent::new_keyboard_event(Some(&self.source), code, down);
        if let Some(e) = &e {
            // Keep the machine's Caps Lock state: the injected modifiers
            // would otherwise clear it for this event.
            let caps = CGEventSource::flags_state(CGEventSourceStateID::HIDSystemState).0
                & CGEventFlags::MaskAlphaShift.0;
            CGEvent::set_flags(Some(e), CGEventFlags(flags_for(self.mods) | caps));
            if repeat {
                CGEvent::set_integer_value_field(Some(e), CGEventField::KeyboardEventAutorepeat, 1);
            }
        }
        self.post(e)
    }
}

fn point(p: Point) -> CGPoint {
    CGPoint::new(f64::from(p.x), f64::from(p.y))
}

impl InputInjector for MacInjector {
    fn inject(&mut self, cmd: InjectCommand) -> Result<(), PlatformError> {
        match cmd {
            InjectCommand::MoveTo(p) => self.move_to(p),
            InjectCommand::Button { button, down, at } => self.button(button, down, at),
            InjectCommand::Wheel { dx, dy, unit } => self.wheel(dx, dy, unit),
            InjectCommand::Key { hid, down, repeat } => self.key(hid, down, repeat),
        }
    }
}
