//! Modifier remapping between a Mac keyboard and a Windows target (and back).
//!
//! The Mac's Command key and the PC's Windows key are both the HID GUI usage,
//! so one swap of Ctrl <-> GUI per side gives exactly:
//! - Mac keyboard -> Windows target: Cmd -> Ctrl, Ctrl -> Win, Option -> Alt;
//! - Windows keyboard -> Mac target: Ctrl -> Cmd, Win -> Ctrl, Alt -> Option.
//!
//! Left and right are preserved. The controller applies the remap to every
//! forwarded key and to `ModifierSync`; the target injects verbatim. The
//! emergency chord always uses physical, pre-remap codes.

use crate::types::{Hid, ModifierMask, Os};

/// Key remapping applied by the controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Remap {
    swap_ctrl_gui: bool,
}

impl Remap {
    /// `src` is the controller's OS, `dst` the target's. The swap is active
    /// only when `enabled` and the OSes differ.
    pub fn new(src: Os, dst: Os, enabled: bool) -> Remap {
        Remap {
            swap_ctrl_gui: enabled && src != dst,
        }
    }

    /// The identity remap.
    pub fn identity() -> Remap {
        Remap {
            swap_ctrl_gui: false,
        }
    }

    pub fn is_identity(self) -> bool {
        !self.swap_ctrl_gui
    }

    pub fn map_hid(self, hid: Hid) -> Hid {
        if !self.swap_ctrl_gui {
            return hid;
        }
        match hid {
            Hid::LEFT_CTRL => Hid::LEFT_GUI,
            Hid::LEFT_GUI => Hid::LEFT_CTRL,
            Hid::RIGHT_CTRL => Hid::RIGHT_GUI,
            Hid::RIGHT_GUI => Hid::RIGHT_CTRL,
            other => other,
        }
    }

    pub fn map_mask(self, mask: ModifierMask) -> ModifierMask {
        let mut out = ModifierMask::EMPTY;
        for hid in mask.keys() {
            out.insert(self.map_hid(hid));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn mac_keyboard_to_windows() {
        let r = Remap::new(Os::MacOs, Os::Windows, true);
        assert_eq!(r.map_hid(Hid::LEFT_GUI), Hid::LEFT_CTRL, "Cmd -> Ctrl");
        assert_eq!(
            r.map_hid(Hid::RIGHT_GUI),
            Hid::RIGHT_CTRL,
            "right Cmd -> right Ctrl"
        );
        assert_eq!(r.map_hid(Hid::LEFT_CTRL), Hid::LEFT_GUI, "Ctrl -> Win");
        assert_eq!(r.map_hid(Hid::RIGHT_CTRL), Hid::RIGHT_GUI);
        assert_eq!(r.map_hid(Hid::LEFT_ALT), Hid::LEFT_ALT, "Option -> Alt");
        assert_eq!(r.map_hid(Hid::RIGHT_ALT), Hid::RIGHT_ALT);
        assert_eq!(r.map_hid(Hid::LEFT_SHIFT), Hid::LEFT_SHIFT);
        assert_eq!(r.map_hid(Hid(0x07_0006)), Hid(0x07_0006), "KeyC unchanged");
    }

    #[test]
    fn windows_keyboard_to_mac() {
        let r = Remap::new(Os::Windows, Os::MacOs, true);
        assert_eq!(r.map_hid(Hid::LEFT_CTRL), Hid::LEFT_GUI, "Ctrl -> Cmd");
        assert_eq!(r.map_hid(Hid::LEFT_GUI), Hid::LEFT_CTRL, "Win -> Ctrl");
        assert_eq!(r.map_hid(Hid::LEFT_ALT), Hid::LEFT_ALT, "Alt -> Option");
        assert_eq!(r.map_hid(Hid::RIGHT_CTRL), Hid::RIGHT_GUI);
    }

    #[test]
    fn identity_when_disabled_or_same_os() {
        for r in [
            Remap::new(Os::MacOs, Os::Windows, false),
            Remap::new(Os::MacOs, Os::MacOs, true),
            Remap::new(Os::Windows, Os::Windows, true),
            Remap::identity(),
        ] {
            assert!(r.is_identity());
            for hid in Hid::MODIFIERS {
                assert_eq!(r.map_hid(hid), hid);
            }
        }
    }

    #[test]
    fn mask_mapping() {
        let r = Remap::new(Os::MacOs, Os::Windows, true);
        let mut m = ModifierMask::EMPTY;
        m.insert(Hid::LEFT_GUI);
        m.insert(Hid::LEFT_SHIFT);
        let mut expected = ModifierMask::EMPTY;
        expected.insert(Hid::LEFT_CTRL);
        expected.insert(Hid::LEFT_SHIFT);
        assert_eq!(r.map_mask(m), expected);
    }

    proptest! {
        #[test]
        fn prop_remap_is_an_involution(raw in any::<u32>(), mask in any::<u8>(), mac_src in any::<bool>(), enabled in any::<bool>()) {
            let (src, dst) = if mac_src { (Os::MacOs, Os::Windows) } else { (Os::Windows, Os::MacOs) };
            let r = Remap::new(src, dst, enabled);
            let hid = Hid(raw);
            prop_assert_eq!(r.map_hid(r.map_hid(hid)), hid);
            prop_assert_eq!(r.map_mask(r.map_mask(ModifierMask(mask))), ModifierMask(mask));
            // Both directions agree with each other.
            let back = Remap::new(dst, src, enabled);
            prop_assert_eq!(back.map_hid(r.map_hid(hid)), hid);
        }
    }
}
