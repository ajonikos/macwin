//! Emergency chord detector: Ctrl+Alt+Shift+Esc (Control+Option+Shift+Esc on
//! a Mac keyboard), either side of each modifier.
//!
//! The capture backend runs this on its own thread against PHYSICAL key
//! codes, before any swallowing and before remapping. When it fires, the
//! backend releases the grab locally right away and reports
//! `CaptureFault::EmergencyChord`; the agent then exits. This must keep
//! working even if the Engine, the network or the peer is broken, so the
//! detector is pure, allocation-free and independent of the Engine.
//!
//! Choice of chord: no OS reserves Ctrl+Alt+Shift+Esc (Windows reserves
//! Ctrl+Shift+Esc for Task Manager and macOS Cmd+Option+Esc for Force Quit;
//! while grabbed, the backend swallows the modifiers and Esc, so neither of
//! those fires). The task brief offered Ctrl+Alt/Option+Shift+Q as an
//! example; Esc was kept because it reads as "escape" and needs no layout.

use crate::types::{Hid, ModifierMask};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmergencyDetector {
    mods: ModifierMask,
    /// Set after firing until Esc is released: one fire per press.
    latched: bool,
}

impl EmergencyDetector {
    pub fn new() -> EmergencyDetector {
        EmergencyDetector::default()
    }

    /// Feeds one physical key event. `snapshot` is the full modifier state
    /// reported with the event (macOS derives it from the event flags); when
    /// present it replaces the tracked state, which fixes modifiers pressed
    /// before capture started. Returns true exactly when the chord fires.
    pub fn on_key(
        &mut self,
        hid: Hid,
        down: bool,
        repeat: bool,
        snapshot: Option<ModifierMask>,
    ) -> bool {
        match snapshot {
            Some(s) => self.mods = s,
            None if hid.is_modifier() => {
                if down {
                    self.mods.insert(hid);
                } else {
                    self.mods.remove(hid);
                }
            }
            None => {}
        }
        if hid != Hid::ESCAPE {
            return false;
        }
        if !down {
            self.latched = false;
            return false;
        }
        if repeat || self.latched || !chord_held(self.mods) {
            return false;
        }
        self.latched = true;
        true
    }

    /// The tracked physical modifier state.
    pub fn modifiers(&self) -> ModifierMask {
        self.mods
    }
}

fn chord_held(m: ModifierMask) -> bool {
    m.intersects(ModifierMask::CTRL)
        && m.intersects(ModifierMask::ALT)
        && m.intersects(ModifierMask::SHIFT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(d: &mut EmergencyDetector, hid: Hid) -> bool {
        d.on_key(hid, true, false, None)
    }

    fn release(d: &mut EmergencyDetector, hid: Hid) -> bool {
        d.on_key(hid, false, false, None)
    }

    #[test]
    fn all_side_combinations_fire() {
        for ctrl in [Hid::LEFT_CTRL, Hid::RIGHT_CTRL] {
            for alt in [Hid::LEFT_ALT, Hid::RIGHT_ALT] {
                for shift in [Hid::LEFT_SHIFT, Hid::RIGHT_SHIFT] {
                    let mut d = EmergencyDetector::new();
                    assert!(!press(&mut d, ctrl));
                    assert!(!press(&mut d, alt));
                    assert!(!press(&mut d, shift));
                    assert!(press(&mut d, Hid::ESCAPE), "{ctrl:?} {alt:?} {shift:?}");
                }
            }
        }
    }

    #[test]
    fn partial_chords_do_not_fire() {
        let all = [Hid::LEFT_CTRL, Hid::LEFT_ALT, Hid::LEFT_SHIFT];
        for missing in 0..3 {
            let mut d = EmergencyDetector::new();
            for (i, &m) in all.iter().enumerate() {
                if i != missing {
                    press(&mut d, m);
                }
            }
            assert!(!press(&mut d, Hid::ESCAPE), "missing {:?}", all[missing]);
        }
        // GUI does not substitute for any of them.
        let mut d = EmergencyDetector::new();
        press(&mut d, Hid::LEFT_GUI);
        press(&mut d, Hid::LEFT_ALT);
        press(&mut d, Hid::LEFT_SHIFT);
        assert!(!press(&mut d, Hid::ESCAPE));
    }

    #[test]
    fn repeats_and_released_modifiers_do_not_fire() {
        let mut d = EmergencyDetector::new();
        press(&mut d, Hid::LEFT_CTRL);
        press(&mut d, Hid::LEFT_ALT);
        press(&mut d, Hid::LEFT_SHIFT);
        assert!(!d.on_key(Hid::ESCAPE, true, true, None), "repeat flag");
        release(&mut d, Hid::LEFT_ALT);
        assert!(!press(&mut d, Hid::ESCAPE), "Alt released before Esc");
    }

    #[test]
    fn one_fire_per_press() {
        let mut d = EmergencyDetector::new();
        press(&mut d, Hid::RIGHT_CTRL);
        press(&mut d, Hid::RIGHT_ALT);
        press(&mut d, Hid::RIGHT_SHIFT);
        assert!(press(&mut d, Hid::ESCAPE));
        // Windows-style repeated downs without the repeat flag.
        assert!(!press(&mut d, Hid::ESCAPE));
        assert!(!press(&mut d, Hid::ESCAPE));
        release(&mut d, Hid::ESCAPE);
        assert!(press(&mut d, Hid::ESCAPE), "fires again after release");
    }

    #[test]
    fn snapshot_overrides_tracked_state() {
        let mut d = EmergencyDetector::new();
        // Modifiers were held before capture started: only the snapshot knows.
        let held = ModifierMask(0b0000_0111); // LCtrl, LShift, LAlt
        assert!(d.on_key(Hid::ESCAPE, true, false, Some(held)));
        assert_eq!(d.modifiers(), held);
        release(&mut d, Hid::ESCAPE);
        // A snapshot without Alt wins over the tracked state.
        assert!(!d.on_key(Hid::ESCAPE, true, false, Some(ModifierMask(0b0000_0011))));
    }

    #[test]
    fn non_escape_keys_never_fire() {
        let mut d = EmergencyDetector::new();
        press(&mut d, Hid::LEFT_CTRL);
        press(&mut d, Hid::LEFT_ALT);
        press(&mut d, Hid::LEFT_SHIFT);
        assert!(!press(&mut d, Hid(0x07_0014))); // KeyQ
    }
}
