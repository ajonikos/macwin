//! HID usage <-> native key code lookups.
//!
//! The tables in `generated.rs` are produced by `tools/keymap-gen` from
//! Chromium's `dom_code_data.inc` (see THIRD_PARTY_NOTICES). They hold the
//! keyboard page (0x07, usage >= 0x04) and the consumer page (0x0C), each key
//! with at least one native code. Pages 0x00 and 0x01 (Fn, Sleep, WakeUp, ...)
//! are deliberately absent.

#[rustfmt::skip]
mod generated;

use generated::{KEYS, KeyEntry, MAC_TO_HID, WIN_TO_HID};

use crate::types::Hid;

/// `KeyEntry::win` value meaning "no Windows scan code".
const WIN_NONE: u16 = 0;
/// `KeyEntry::mac` value meaning "no macOS key code".
const MAC_NONE: u16 = 0xFFFF;

fn entry(hid: Hid) -> Option<&'static KeyEntry> {
    KEYS.binary_search_by_key(&hid.0, |e| e.hid)
        .ok()
        .map(|i| &KEYS[i])
}

/// The macOS virtual key code (`kVK_*`) of `hid`.
pub fn hid_to_mac(hid: Hid) -> Option<u16> {
    entry(hid).map(|e| e.mac).filter(|&mac| mac != MAC_NONE)
}

/// The HID usage of a macOS virtual key code.
pub fn mac_to_hid(code: u16) -> Option<Hid> {
    MAC_TO_HID
        .get(usize::from(code))
        .copied()
        .filter(|&hid| hid != 0)
        .map(Hid)
}

/// The Windows scan code of `hid` in `KBDLLHOOKSTRUCT` form: the set-1 make
/// code, `| 0xE000` for extended (E0-prefixed) keys. For example NumLock is
/// `0xE045` and Pause is `0x0045`.
pub fn hid_to_win(hid: Hid) -> Option<u16> {
    entry(hid).map(|e| e.win).filter(|&win| win != WIN_NONE)
}

/// The HID usage of a Windows scan code: `scan` is the set-1 make code
/// (`KBDLLHOOKSTRUCT::scanCode`), `extended` the E0 flag (`LLKHF_EXTENDED`).
/// Scan codes above 0xFF have no mapping.
pub fn win_to_hid(scan: u16, extended: bool) -> Option<Hid> {
    if scan > 0xFF {
        return None;
    }
    let index = usize::from(scan) | if extended { 0x100 } else { 0 };
    WIN_TO_HID
        .get(index)
        .copied()
        .filter(|&hid| hid != 0)
        .map(Hid)
}

/// The DOM `KeyboardEvent.code` name of `hid`, for example `"KeyA"`.
pub fn name(hid: Hid) -> Option<&'static str> {
    entry(hid).map(|e| e.code).filter(|code| !code.is_empty())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// Splits a `hid_to_win` value into `win_to_hid` arguments.
    fn win_parts(win: u16) -> (u16, bool) {
        (win & 0xFF, win >> 8 == 0xE0)
    }

    /// Asserts name, both forward lookups and both reverse lookups of one key.
    #[track_caller]
    fn check(hid: u32, code: &str, win: Option<u16>, mac: Option<u16>) {
        let hid = Hid(hid);
        assert_eq!(name(hid), Some(code), "name of {hid:?}");
        assert_eq!(hid_to_win(hid), win, "hid_to_win({code})");
        assert_eq!(hid_to_mac(hid), mac, "hid_to_mac({code})");
        if let Some(win) = win {
            let (scan, extended) = win_parts(win);
            assert_eq!(
                win_to_hid(scan, extended),
                Some(hid),
                "win_to_hid for {code}"
            );
        }
        if let Some(mac) = mac {
            assert_eq!(mac_to_hid(mac), Some(hid), "mac_to_hid for {code}");
        }
    }

    #[test]
    fn keymap_tables_sorted_and_unique() {
        assert!(KEYS.len() >= 100, "only {} keys", KEYS.len());
        assert!(
            KEYS.windows(2).all(|w| w[0].hid < w[1].hid),
            "KEYS not strictly sorted"
        );
        let mut wins = BTreeSet::new();
        let mut macs = BTreeSet::new();
        for e in &KEYS {
            let hid = Hid(e.hid);
            assert!(
                (hid.page() == 0x07 && hid.usage() >= 0x04) || hid.page() == 0x0C,
                "unexpected usage {:#x}",
                e.hid
            );
            assert!(
                e.win != WIN_NONE || e.mac != MAC_NONE,
                "{:#x} has no native code",
                e.hid
            );
            assert!(
                e.win == WIN_NONE || wins.insert(e.win),
                "duplicate win {:#x}",
                e.win
            );
            assert!(
                e.mac == MAC_NONE || macs.insert(e.mac),
                "duplicate mac {:#x}",
                e.mac
            );
        }
    }

    #[test]
    fn keymap_round_trips_for_every_mapped_entry() {
        for e in &KEYS {
            let hid = Hid(e.hid);
            if let Some(mac) = hid_to_mac(hid) {
                assert_eq!(mac_to_hid(mac), Some(hid), "mac {mac:#x}");
            }
            if let Some(win) = hid_to_win(hid) {
                let (scan, extended) = win_parts(win);
                assert_eq!(win_to_hid(scan, extended), Some(hid), "win {win:#x}");
            }
        }
        // Every reverse-table slot points back at a key with that native code.
        for (code, &raw) in MAC_TO_HID.iter().enumerate() {
            if raw != 0 {
                assert_eq!(
                    hid_to_mac(Hid(raw)).map(usize::from),
                    Some(code),
                    "mac slot {code:#x}"
                );
            }
        }
        for (index, &raw) in WIN_TO_HID.iter().enumerate() {
            if raw != 0 {
                let (scan, extended) = win_parts(hid_to_win(Hid(raw)).unwrap());
                let expected = usize::from(scan) | if extended { 0x100 } else { 0 };
                assert_eq!(expected, index, "win slot {index:#x}");
            }
        }
    }

    #[test]
    fn keymap_design_spot_checks() {
        check(0x07_0004, "KeyA", Some(0x1E), Some(0x00));
        check(0x07_00E0, "ControlLeft", Some(0x1D), Some(0x3B));
        check(0x07_00E3, "MetaLeft", Some(0xE05B), Some(0x37));
        check(0x07_0058, "NumpadEnter", Some(0xE01C), Some(0x4C));
        check(0x07_0046, "PrintScreen", Some(0xE037), None);
        check(0x07_0048, "Pause", Some(0x0045), None);
        check(0x07_0053, "NumLock", Some(0xE045), Some(0x47));
        check(0x07_0064, "IntlBackslash", Some(0x56), Some(0x0A));
        check(0x07_0050, "ArrowLeft", Some(0xE04B), Some(0x7B));
    }

    #[test]
    fn keymap_letters() {
        // (Windows set-1 scan code, macOS kVK_ANSI_*) for A..=Z.
        let codes: [(u16, u16); 26] = [
            (0x1E, 0x00),
            (0x30, 0x0B),
            (0x2E, 0x08),
            (0x20, 0x02),
            (0x12, 0x0E),
            (0x21, 0x03),
            (0x22, 0x05),
            (0x23, 0x04),
            (0x17, 0x22),
            (0x24, 0x26),
            (0x25, 0x28),
            (0x26, 0x25),
            (0x32, 0x2E),
            (0x31, 0x2D),
            (0x18, 0x1F),
            (0x19, 0x23),
            (0x10, 0x0C),
            (0x13, 0x0F),
            (0x1F, 0x01),
            (0x14, 0x11),
            (0x16, 0x20),
            (0x2F, 0x09),
            (0x11, 0x0D),
            (0x2D, 0x07),
            (0x15, 0x10),
            (0x2C, 0x06),
        ];
        for ((letter, hid), (win, mac)) in ('A'..='Z').zip(0x07_0004..).zip(codes) {
            check(hid, &format!("Key{letter}"), Some(win), Some(mac));
        }
    }

    #[test]
    fn keymap_digits() {
        // HID orders 1..=9 then 0.
        let codes: [(char, u16, u16); 10] = [
            ('1', 0x02, 0x12),
            ('2', 0x03, 0x13),
            ('3', 0x04, 0x14),
            ('4', 0x05, 0x15),
            ('5', 0x06, 0x17),
            ('6', 0x07, 0x16),
            ('7', 0x08, 0x1A),
            ('8', 0x09, 0x1C),
            ('9', 0x0A, 0x19),
            ('0', 0x0B, 0x1D),
        ];
        for (hid, (digit, win, mac)) in (0x07_001E..).zip(codes) {
            check(hid, &format!("Digit{digit}"), Some(win), Some(mac));
        }
    }

    #[test]
    fn keymap_modifiers() {
        check(Hid::LEFT_CTRL.0, "ControlLeft", Some(0x1D), Some(0x3B));
        check(Hid::LEFT_SHIFT.0, "ShiftLeft", Some(0x2A), Some(0x38));
        check(Hid::LEFT_ALT.0, "AltLeft", Some(0x38), Some(0x3A));
        check(Hid::LEFT_GUI.0, "MetaLeft", Some(0xE05B), Some(0x37));
        check(Hid::RIGHT_CTRL.0, "ControlRight", Some(0xE01D), Some(0x3E));
        check(Hid::RIGHT_SHIFT.0, "ShiftRight", Some(0x36), Some(0x3C));
        check(Hid::RIGHT_ALT.0, "AltRight", Some(0xE038), Some(0x3D));
        check(Hid::RIGHT_GUI.0, "MetaRight", Some(0xE05C), Some(0x36));
    }

    #[test]
    fn keymap_arrows() {
        check(0x07_004F, "ArrowRight", Some(0xE04D), Some(0x7C));
        check(0x07_0050, "ArrowLeft", Some(0xE04B), Some(0x7B));
        check(0x07_0051, "ArrowDown", Some(0xE050), Some(0x7D));
        check(0x07_0052, "ArrowUp", Some(0xE048), Some(0x7E));
    }

    #[test]
    fn keymap_function_keys() {
        // (Windows scan code, macOS kVK_F*) for F1..=F12.
        let codes: [(u16, u16); 12] = [
            (0x3B, 0x7A),
            (0x3C, 0x78),
            (0x3D, 0x63),
            (0x3E, 0x76),
            (0x3F, 0x60),
            (0x40, 0x61),
            (0x41, 0x62),
            (0x42, 0x64),
            (0x43, 0x65),
            (0x44, 0x6D),
            (0x57, 0x67),
            (0x58, 0x6F),
        ];
        for ((n, hid), (win, mac)) in (1..=12).zip(0x07_003A..).zip(codes) {
            check(hid, &format!("F{n}"), Some(win), Some(mac));
        }
    }

    #[test]
    fn keymap_iso_intl_backslash() {
        check(0x07_0064, "IntlBackslash", Some(0x56), Some(0x0A));
        assert_eq!(mac_to_hid(0x0A), Some(Hid(0x07_0064)));
    }

    #[test]
    fn keymap_extended_windows_keys() {
        // (scan code with E0 prefix, key, key reached by the same byte without E0).
        let cases: [(u16, &str, Option<&str>); 18] = [
            (0xE01D, "ControlRight", Some("ControlLeft")),
            (0xE038, "AltRight", Some("AltLeft")),
            (0xE048, "ArrowUp", Some("Numpad8")),
            (0xE04B, "ArrowLeft", Some("Numpad4")),
            (0xE04D, "ArrowRight", Some("Numpad6")),
            (0xE050, "ArrowDown", Some("Numpad2")),
            (0xE052, "Insert", Some("Numpad0")),
            (0xE053, "Delete", Some("NumpadDecimal")),
            (0xE047, "Home", Some("Numpad7")),
            (0xE04F, "End", Some("Numpad1")),
            (0xE049, "PageUp", Some("Numpad9")),
            (0xE051, "PageDown", Some("Numpad3")),
            (0xE035, "NumpadDivide", Some("Slash")),
            (0xE01C, "NumpadEnter", Some("Enter")),
            (0xE05B, "MetaLeft", None),
            (0xE05C, "MetaRight", None),
            (0xE037, "PrintScreen", Some("NumpadMultiply")),
            (0xE045, "NumLock", Some("Pause")),
        ];
        for (sc, extended_name, plain_name) in cases {
            let hid = win_to_hid(sc & 0xFF, true).unwrap_or_else(|| panic!("{sc:#x} unmapped"));
            assert_eq!(name(hid), Some(extended_name), "{sc:#x}");
            assert_eq!(hid_to_win(hid), Some(sc), "{extended_name}");
            assert_eq!(
                win_to_hid(sc & 0xFF, false).and_then(name),
                plain_name,
                "{sc:#x} without E0"
            );
        }
        assert_eq!(
            hid_to_win(Hid(0x07_0048)),
            Some(0x0045),
            "Pause is not extended"
        );
    }

    #[test]
    fn keymap_unmapped_inputs() {
        assert_eq!(mac_to_hid(0x7F), None);
        assert_eq!(mac_to_hid(0x80), None);
        assert_eq!(mac_to_hid(MAC_NONE), None);
        assert_eq!(win_to_hid(0, false), None);
        assert_eq!(win_to_hid(0, true), None);
        // Out-of-range scan codes never alias onto a real key (0x21D is the
        // fake LCtrl Windows sends before AltGr).
        assert_eq!(win_to_hid(0x21D, false), None);
        assert_eq!(win_to_hid(0x11D, true), None);
        // Excluded pages and entries without native codes.
        for hid in [
            Hid(0x00_0000),
            Hid(0x01_0082),
            Hid(0x01_0097),
            Hid(0x07_0001),
            Hid(0x07_0074),
        ] {
            assert_eq!(
                (hid_to_win(hid), hid_to_mac(hid), name(hid)),
                (None, None, None),
                "{hid:?}"
            );
        }
    }
}
