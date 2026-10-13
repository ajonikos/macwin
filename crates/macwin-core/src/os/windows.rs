//! Windows conversions: scan codes, injection codes and absolute mouse
//! coordinates.

use crate::keymap;
use crate::types::Hid;

/// `XBUTTON1` (the Back button) in `MOUSEINPUT::mouseData`.
pub const XBUTTON1: u32 = 1;
/// `XBUTTON2` (the Forward button) in `MOUSEINPUT::mouseData`.
pub const XBUTTON2: u32 = 2;

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

#[cfg(test)]
mod tests {
    use super::*;

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
