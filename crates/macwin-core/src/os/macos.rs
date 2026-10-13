//! macOS conversions: modifier flags, scroll units and click counting.

use std::time::{Duration, Instant};

use crate::types::{Hid, ModifierMask, MouseButton, PointF};

// Device-dependent modifier bits (NX_DEVICE*KEYMASK), one per physical key.
pub const LCTRL: u64 = 0x0000_0001;
pub const LSHIFT: u64 = 0x0000_0002;
pub const RSHIFT: u64 = 0x0000_0004;
pub const LCMD: u64 = 0x0000_0008;
pub const RCMD: u64 = 0x0000_0010;
pub const LALT: u64 = 0x0000_0020;
pub const RALT: u64 = 0x0000_0040;
pub const RCTRL: u64 = 0x0000_2000;

// Device-independent bits (kCGEventFlagMask*).
pub const ALPHA_SHIFT: u64 = 0x0001_0000;
pub const SHIFT: u64 = 0x0002_0000;
pub const CONTROL: u64 = 0x0004_0000;
pub const ALTERNATE: u64 = 0x0008_0000;
pub const COMMAND: u64 = 0x0010_0000;

/// macOS virtual key code of CapsLock.
pub const KVK_CAPS_LOCK: u16 = 0x39;
/// macOS virtual key code of Fn.
pub const KVK_FUNCTION: u16 = 0x3F;

/// (key code, device-dependent bit, HID) of the eight modifier keys.
const MODIFIER_KEYS: [(u16, u64, Hid); 8] = [
    (0x3B, LCTRL, Hid::LEFT_CTRL),
    (0x38, LSHIFT, Hid::LEFT_SHIFT),
    (0x3A, LALT, Hid::LEFT_ALT),
    (0x37, LCMD, Hid::LEFT_GUI),
    (0x3E, RCTRL, Hid::RIGHT_CTRL),
    (0x3C, RSHIFT, Hid::RIGHT_SHIFT),
    (0x3D, RALT, Hid::RIGHT_ALT),
    (0x36, RCMD, Hid::RIGHT_GUI),
];

/// What a `kCGEventFlagsChanged` event means for the key that changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagsChange {
    Down,
    Up,
    /// CapsLock: macOS reports only lock-state changes, so forward a full
    /// press (down then up).
    Tap,
}

/// Interprets a flags-changed event for `keycode` with the event's `flags`.
/// Fn and non-modifier key codes yield `None`.
pub fn flags_changed(keycode: u16, flags: u64) -> Option<FlagsChange> {
    if keycode == KVK_CAPS_LOCK {
        return Some(FlagsChange::Tap);
    }
    MODIFIER_KEYS
        .iter()
        .find(|(code, _, _)| *code == keycode)
        .map(|(_, bit, _)| {
            if flags & bit != 0 {
                FlagsChange::Down
            } else {
                FlagsChange::Up
            }
        })
}

/// Event flags for injecting with modifier state `mask`: both the
/// device-dependent and the device-independent bits.
pub fn flags_for(mask: ModifierMask) -> u64 {
    let mut flags = 0;
    for (_, bit, hid) in MODIFIER_KEYS {
        if mask.contains(hid) {
            flags |= bit;
        }
    }
    if mask.intersects(ModifierMask::CTRL) {
        flags |= CONTROL;
    }
    if mask.intersects(ModifierMask::SHIFT) {
        flags |= SHIFT;
    }
    if mask.intersects(ModifierMask::ALT) {
        flags |= ALTERNATE;
    }
    if mask.intersects(ModifierMask::GUI) {
        flags |= COMMAND;
    }
    flags
}

/// Modifier state from event flags. Device-dependent bits win; a family whose
/// device-independent bit is set without any device bit (synthetic events)
/// maps to its left key.
pub fn mask_from_flags(flags: u64) -> ModifierMask {
    let mut mask = ModifierMask::EMPTY;
    for (_, bit, hid) in MODIFIER_KEYS {
        if flags & bit != 0 {
            mask.insert(hid);
        }
    }
    for (independent, family, left) in [
        (CONTROL, ModifierMask::CTRL, Hid::LEFT_CTRL),
        (SHIFT, ModifierMask::SHIFT, Hid::LEFT_SHIFT),
        (ALTERNATE, ModifierMask::ALT, Hid::LEFT_ALT),
        (COMMAND, ModifierMask::GUI, Hid::LEFT_GUI),
    ] {
        if flags & independent != 0 && !mask.intersects(family) {
            mask.insert(left);
        }
    }
    mask
}

/// Converts `kCGScrollWheelEventFixedPtDeltaAxis1/2` (lines, positive = up /
/// left) into macwin wheel units (120 per line, `dy > 0` up, `dx > 0` right).
/// The axis-2 sign is unverified on real hardware.
pub fn scroll_from_fixed(axis1: f64, axis2: f64) -> (i32, i32) {
    let dy = (axis1 * 120.0).round();
    let dx = (-axis2 * 120.0).round();
    (clamp_i32(dx), clamp_i32(dy))
}

fn clamp_i32(v: f64) -> i32 {
    if v.is_nan() {
        0
    } else {
        v.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
    }
}

/// Turns 1/120 wheel units into whole lines, carrying the remainder (with
/// its sign) to the next call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WheelAccumulator {
    remainder: i64,
}

impl WheelAccumulator {
    pub fn new() -> WheelAccumulator {
        WheelAccumulator::default()
    }

    /// Adds `delta` (1/120 units) and returns the whole lines to scroll now.
    pub fn push(&mut self, delta: i32) -> i32 {
        let total = self.remainder + i64::from(delta);
        let lines = total / 120; // truncates toward zero
        self.remainder = total - lines * 120;
        lines.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
    }

    pub fn remainder(&self) -> i32 {
        self.remainder as i32
    }
}

/// Counts clicks for `kCGMouseEventClickState`: the same button pressed
/// again within the double-click interval and within 4 points of the
/// previous press continues the sequence.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ClickCounter {
    last: Option<(MouseButton, PointF, Instant, i64)>,
}

/// Maximum distance (points) between presses of one click sequence.
pub const CLICK_RADIUS: f64 = 4.0;

impl ClickCounter {
    pub fn new() -> ClickCounter {
        ClickCounter::default()
    }

    /// Records a button press and returns its click count (1, 2, 3, ...).
    pub fn on_down(
        &mut self,
        button: MouseButton,
        pos: PointF,
        now: Instant,
        interval: Duration,
    ) -> i64 {
        let count = match self.last {
            Some((b, p, t, n))
                if b == button
                    && now.saturating_duration_since(t) <= interval
                    && (pos.x - p.x).hypot(pos.y - p.y) <= CLICK_RADIUS =>
            {
                n + 1
            }
            _ => 1,
        };
        self.last = Some((button, pos, now, count));
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_changed_for_modifiers() {
        for (code, bit, _) in MODIFIER_KEYS {
            assert_eq!(
                flags_changed(code, bit | CONTROL),
                Some(FlagsChange::Down),
                "{code:#x}"
            );
            assert_eq!(
                flags_changed(code, CONTROL),
                Some(FlagsChange::Up),
                "{code:#x}"
            );
        }
        assert_eq!(
            flags_changed(KVK_CAPS_LOCK, ALPHA_SHIFT),
            Some(FlagsChange::Tap)
        );
        assert_eq!(flags_changed(KVK_CAPS_LOCK, 0), Some(FlagsChange::Tap));
        assert_eq!(flags_changed(KVK_FUNCTION, 0x80_0000), None);
        assert_eq!(flags_changed(0x00, LCTRL), None, "KeyA is no modifier");
    }

    #[test]
    fn flags_and_masks_round_trip() {
        for raw in 0..=255u8 {
            let mask = ModifierMask(raw);
            assert_eq!(mask_from_flags(flags_for(mask)), mask, "{raw:#010b}");
        }
        let f = flags_for(ModifierMask(0b0001_0010)); // LShift + RCtrl
        assert_eq!(f, LSHIFT | RCTRL | SHIFT | CONTROL);
        // Synthetic flags without device bits map to the left keys.
        assert_eq!(mask_from_flags(COMMAND | SHIFT), ModifierMask(0b0000_1010));
    }

    #[test]
    fn scroll_conversion() {
        assert_eq!(scroll_from_fixed(1.0, 0.0), (0, 120));
        assert_eq!(scroll_from_fixed(-0.5, 0.0), (0, -60));
        assert_eq!(scroll_from_fixed(0.0, 1.0), (-120, 0));
        assert_eq!(scroll_from_fixed(f64::NAN, f64::INFINITY), (i32::MIN, 0));
    }

    #[test]
    fn wheel_accumulator_carries_remainder() {
        let mut w = WheelAccumulator::new();
        assert_eq!(w.push(60), 0);
        assert_eq!(w.push(60), 1);
        assert_eq!(w.remainder(), 0);
        assert_eq!(w.push(250), 2);
        assert_eq!(w.remainder(), 10);
        assert_eq!(w.push(-130), -1);
        assert_eq!(w.remainder(), 0);
        assert_eq!(w.push(-30), 0);
        assert_eq!(w.remainder(), -30);
        assert_eq!(w.push(-90), -1);
    }

    #[test]
    fn click_counter_interval_and_distance() {
        let t0 = Instant::now();
        let iv = Duration::from_millis(500);
        let ms = Duration::from_millis(1);
        let p = PointF::new(100.0, 100.0);
        let mut c = ClickCounter::new();
        assert_eq!(c.on_down(MouseButton::Left, p, t0, iv), 1);
        assert_eq!(
            c.on_down(
                MouseButton::Left,
                PointF::new(103.0, 102.0),
                t0 + 200 * ms,
                iv
            ),
            2
        );
        assert_eq!(c.on_down(MouseButton::Left, p, t0 + 400 * ms, iv), 3);
        // Too late.
        assert_eq!(c.on_down(MouseButton::Left, p, t0 + 1000 * ms, iv), 1);
        // Too far.
        assert_eq!(
            c.on_down(
                MouseButton::Left,
                PointF::new(110.0, 100.0),
                t0 + 1100 * ms,
                iv
            ),
            1
        );
        // Other button.
        assert_eq!(
            c.on_down(
                MouseButton::Right,
                PointF::new(110.0, 100.0),
                t0 + 1200 * ms,
                iv
            ),
            1
        );
    }
}
