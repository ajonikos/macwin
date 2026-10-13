//! Windows conversions: scan codes, injection codes and absolute mouse
//! coordinates.

use super::pixels_to_notch120;
use crate::keymap;
use crate::types::{Hid, MouseButton, WheelUnit};

/// `XBUTTON1` (the Back button) in `MOUSEINPUT::mouseData`.
pub const XBUTTON1: u32 = 1;
/// `XBUTTON2` (the Forward button) in `MOUSEINPUT::mouseData`.
pub const XBUTTON2: u32 = 2;

// Window messages seen by low-level hooks.
pub const WM_KEYDOWN: u32 = 0x0100;
pub const WM_KEYUP: u32 = 0x0101;
pub const WM_SYSKEYDOWN: u32 = 0x0104;
pub const WM_SYSKEYUP: u32 = 0x0105;
pub const WM_MOUSEMOVE: u32 = 0x0200;
pub const WM_LBUTTONDOWN: u32 = 0x0201;
pub const WM_LBUTTONUP: u32 = 0x0202;
pub const WM_RBUTTONDOWN: u32 = 0x0204;
pub const WM_RBUTTONUP: u32 = 0x0205;
pub const WM_MBUTTONDOWN: u32 = 0x0207;
pub const WM_MBUTTONUP: u32 = 0x0208;
pub const WM_MOUSEWHEEL: u32 = 0x020A;
pub const WM_XBUTTONDOWN: u32 = 0x020B;
pub const WM_XBUTTONUP: u32 = 0x020C;
pub const WM_MOUSEHWHEEL: u32 = 0x020E;

/// `Some(down)` for the keyboard messages a `WH_KEYBOARD_LL` hook receives.
pub fn classify_key(msg: u32) -> Option<bool> {
    match msg {
        WM_KEYDOWN | WM_SYSKEYDOWN => Some(true),
        WM_KEYUP | WM_SYSKEYUP => Some(false),
        _ => None,
    }
}

/// A `WH_MOUSE_LL` message, decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LowLevelMouse {
    Move,
    Button {
        button: MouseButton,
        down: bool,
    },
    /// 120 = one detent; `dy > 0` is away from the user, `dx > 0` is right.
    Wheel {
        dx: i32,
        dy: i32,
    },
}

/// The signed high word of `MSLLHOOKSTRUCT::mouseData`.
pub fn high_word_signed(mouse_data: u32) -> i32 {
    i32::from((mouse_data >> 16) as u16 as i16)
}

/// Decodes a low-level mouse message; `None` for anything else.
pub fn classify_mouse(msg: u32, mouse_data: u32) -> Option<LowLevelMouse> {
    let button = |button, down| Some(LowLevelMouse::Button { button, down });
    match msg {
        WM_MOUSEMOVE => Some(LowLevelMouse::Move),
        WM_LBUTTONDOWN => button(MouseButton::Left, true),
        WM_LBUTTONUP => button(MouseButton::Left, false),
        WM_RBUTTONDOWN => button(MouseButton::Right, true),
        WM_RBUTTONUP => button(MouseButton::Right, false),
        WM_MBUTTONDOWN => button(MouseButton::Middle, true),
        WM_MBUTTONUP => button(MouseButton::Middle, false),
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            let b = match (mouse_data >> 16) & 0xFFFF {
                XBUTTON1 => MouseButton::Back,
                XBUTTON2 => MouseButton::Forward,
                _ => return None,
            };
            button(b, msg == WM_XBUTTONDOWN)
        }
        WM_MOUSEWHEEL => Some(LowLevelMouse::Wheel {
            dx: 0,
            dy: high_word_signed(mouse_data),
        }),
        WM_MOUSEHWHEEL => Some(LowLevelMouse::Wheel {
            dx: high_word_signed(mouse_data),
            dy: 0,
        }),
        _ => None,
    }
}

/// The HID usage of a low-level keyboard hook event. `scan` is
/// `KBDLLHOOKSTRUCT::scanCode`, `extended` is `LLKHF_EXTENDED`.
///
/// Returns `None` for scan code 0 and for codes without a key, which drops
/// the fake LShift (E0 2A) Windows wraps around some keys and the fake LCtrl
/// (scan code 0x21D) it sends before AltGr.
pub fn scan_to_hid(scan: u32, extended: bool) -> Option<Hid> {
    if scan == 0 || scan > 0xFF {
        return None;
    }
    keymap::win_to_hid(scan as u16, extended)
}

/// How to inject a key with `SendInput`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WinKey {
    /// `KEYEVENTF_SCANCODE` with `wScan = sc` (+ `KEYEVENTF_EXTENDEDKEY`).
    Scancode { sc: u16, extended: bool },
    /// Virtual-key injection (`wVk = vk`).
    Vk { vk: u16, extended: bool },
}

const PAUSE: Hid = Hid(0x07_0048);
const NUM_LOCK: Hid = Hid(0x07_0053);

/// The injection code for `hid`. Pause and NumLock are injected by virtual
/// key because their scan codes collide (both are 0x45, distinguished only by
/// E1/E0 prefixes that scan-code injection cannot express reliably).
pub fn key_injection(hid: Hid) -> Option<WinKey> {
    match hid {
        PAUSE => Some(WinKey::Vk {
            vk: 0x13,
            extended: false,
        }),
        NUM_LOCK => Some(WinKey::Vk {
            vk: 0x90,
            extended: true,
        }),
        _ => keymap::hid_to_win(hid).map(|win| WinKey::Scancode {
            sc: win & 0xFF,
            extended: win >> 8 == 0xE0,
        }),
    }
}

/// Converts a virtual-screen x (or y) pixel into `SendInput`'s absolute
/// `0..=65535` range: `ceil((x - v_left) * 65536 / v_width)`, clamped.
/// Exact when Windows maps back with `pixel = floor(n * width / 65536)`.
pub fn normalize_absolute(x: i32, v_left: i32, v_width: i32) -> i32 {
    let width = i64::from(v_width.max(1));
    let offset = i64::from(x) - i64::from(v_left);
    let n = (offset * 65_536 + width - 1).div_euclid(width);
    n.clamp(0, 65_535) as i32
}

/// A wheel delta in `MOUSEINPUT::mouseData` units: 120 per notch, smaller
/// values scroll smoothly in apps that support high-resolution wheels.
pub fn wheel_data(delta: i32, unit: WheelUnit) -> i32 {
    match unit {
        WheelUnit::Notch120 => delta,
        WheelUnit::Pixel => pixels_to_notch120(delta),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_data_scales_pixels() {
        assert_eq!(wheel_data(-120, WheelUnit::Notch120), -120);
        assert_eq!(wheel_data(40, WheelUnit::Pixel), 120, "a notch of pixels");
        assert_eq!(wheel_data(-1, WheelUnit::Pixel), -3);
        assert_eq!(wheel_data(i32::MAX, WheelUnit::Pixel), i32::MAX);
    }

    #[test]
    fn scan_codes() {
        assert_eq!(scan_to_hid(0x1E, false), Some(Hid(0x07_0004)), "KeyA");
        assert_eq!(scan_to_hid(0x1D, true), Some(Hid::RIGHT_CTRL));
        assert_eq!(scan_to_hid(0, false), None);
        assert_eq!(scan_to_hid(0x2A, true), None, "fake LShift E0 2A");
        assert_eq!(scan_to_hid(0x21D, false), None, "AltGr fake LCtrl");
        assert_eq!(scan_to_hid(0x2A, false), Some(Hid::LEFT_SHIFT));
    }

    #[test]
    fn injection_codes() {
        assert_eq!(
            key_injection(Hid(0x07_0004)),
            Some(WinKey::Scancode {
                sc: 0x1E,
                extended: false
            })
        );
        assert_eq!(
            key_injection(Hid::RIGHT_ALT),
            Some(WinKey::Scancode {
                sc: 0x38,
                extended: true
            })
        );
        assert_eq!(
            key_injection(PAUSE),
            Some(WinKey::Vk {
                vk: 0x13,
                extended: false
            })
        );
        assert_eq!(
            key_injection(NUM_LOCK),
            Some(WinKey::Vk {
                vk: 0x90,
                extended: true
            })
        );
        assert_eq!(key_injection(Hid(0x07_0001)), None);
    }

    #[test]
    fn hook_messages() {
        assert_eq!(classify_key(WM_KEYDOWN), Some(true));
        assert_eq!(classify_key(WM_SYSKEYDOWN), Some(true));
        assert_eq!(classify_key(WM_KEYUP), Some(false));
        assert_eq!(classify_key(WM_SYSKEYUP), Some(false));
        assert_eq!(classify_key(WM_MOUSEMOVE), None);

        assert_eq!(classify_mouse(WM_MOUSEMOVE, 0), Some(LowLevelMouse::Move));
        assert_eq!(
            classify_mouse(WM_RBUTTONUP, 0),
            Some(LowLevelMouse::Button {
                button: MouseButton::Right,
                down: false
            })
        );
        assert_eq!(
            classify_mouse(WM_XBUTTONDOWN, XBUTTON2 << 16),
            Some(LowLevelMouse::Button {
                button: MouseButton::Forward,
                down: true
            })
        );
        assert_eq!(classify_mouse(WM_XBUTTONUP, 7 << 16), None);
        // One detent toward the user is -120 in the high word.
        let toward = u32::from((-120i16) as u16) << 16;
        assert_eq!(
            classify_mouse(WM_MOUSEWHEEL, toward),
            Some(LowLevelMouse::Wheel { dx: 0, dy: -120 })
        );
        assert_eq!(
            classify_mouse(WM_MOUSEHWHEEL, 30 << 16),
            Some(LowLevelMouse::Wheel { dx: 30, dy: 0 })
        );
        assert_eq!(classify_mouse(0x0001, 0), None);
    }

    /// The pixel Windows lands on for an absolute value, under the floor model.
    fn denormalize(n: i32, v_left: i32, v_width: i32) -> i32 {
        v_left + (i64::from(n) * i64::from(v_width) / 65_536) as i32
    }

    #[test]
    fn normalize_absolute_round_trips() {
        // Every pixel of every width up to 3000, then boundary and strided
        // pixels for widths up to 20000.
        for width in 1..=20_000 {
            let check = |x: i32| {
                let n = normalize_absolute(x, -1234, width);
                assert!((0..=65_535).contains(&n));
                assert_eq!(denormalize(n, -1234, width), x, "x {x} width {width}");
            };
            if width <= 3000 {
                for px in 0..width {
                    check(-1234 + px);
                }
            } else {
                for px in (0..64)
                    .chain(width - 64..width)
                    .chain((0..width).step_by(37))
                {
                    check(-1234 + px);
                }
            }
        }
        assert_eq!(normalize_absolute(-5000, 0, 1920), 0);
        assert_eq!(normalize_absolute(5000, 0, 1920), 65_535);
    }
}
