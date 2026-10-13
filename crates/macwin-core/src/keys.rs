//! Pressed-state tracking for keys and mouse buttons.

use std::collections::BTreeSet;

use crate::types::{Hid, InjectCommand, ModifierMask, MouseButton, Point};

/// A set of pressed keys with deterministic iteration order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeySet(BTreeSet<Hid>);

impl KeySet {
    pub fn new() -> KeySet {
        KeySet::default()
    }

    /// Returns true if the key was not yet present.
    pub fn insert(&mut self, hid: Hid) -> bool {
        self.0.insert(hid)
    }

    /// Returns true if the key was present.
    pub fn remove(&mut self, hid: Hid) -> bool {
        self.0.remove(&hid)
    }

    pub fn contains(&self, hid: Hid) -> bool {
        self.0.contains(&hid)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = Hid> + '_ {
        self.0.iter().copied()
    }

    /// The modifier keys in the set as a mask.
    pub fn modifier_mask(&self) -> ModifierMask {
        let mut m = ModifierMask::EMPTY;
        for hid in self.iter() {
            m.insert(hid);
        }
        m
    }

    /// Keys in `self` that are not in `other`.
    pub fn difference(&self, other: &KeySet) -> KeySet {
        KeySet(self.0.difference(&other.0).copied().collect())
    }
}

impl FromIterator<Hid> for KeySet {
    fn from_iter<I: IntoIterator<Item = Hid>>(iter: I) -> KeySet {
        KeySet(iter.into_iter().collect())
    }
}

/// A set of pressed mouse buttons.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ButtonSet(u8);

impl ButtonSet {
    pub fn new() -> ButtonSet {
        ButtonSet::default()
    }

    /// Returns true if the button was not yet present.
    pub fn insert(&mut self, b: MouseButton) -> bool {
        let was = self.contains(b);
        self.0 |= 1 << b.bit();
        !was
    }

    /// Returns true if the button was present.
    pub fn remove(&mut self, b: MouseButton) -> bool {
        let was = self.contains(b);
        self.0 &= !(1 << b.bit());
        was
    }

    pub fn contains(self, b: MouseButton) -> bool {
        self.0 & (1 << b.bit()) != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn clear(&mut self) {
        self.0 = 0;
    }

    pub fn iter(self) -> impl Iterator<Item = MouseButton> {
        MouseButton::ALL
            .into_iter()
            .filter(move |&b| self.contains(b))
    }
}

/// Keys and buttons a target has injected and not yet released.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pressed {
    pub keys: KeySet,
    pub buttons: ButtonSet,
}

impl Pressed {
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.buttons.is_empty()
    }

    /// Releases everything: button ups at `at` first, then non-modifier key
    /// ups, then modifier key ups (so a held Ctrl+C never degrades into a
    /// stray shortcut). Leaves the sets empty; a second call is a no-op.
    pub fn release_all(&mut self, at: Point) -> Vec<InjectCommand> {
        let mut out = Vec::with_capacity(self.keys.len() + 5);
        for button in self.buttons.iter() {
            out.push(InjectCommand::Button {
                button,
                down: false,
                at,
            });
        }
        let up = |hid| InjectCommand::Key {
            hid,
            down: false,
            repeat: false,
        };
        out.extend(self.keys.iter().filter(|h| !h.is_modifier()).map(up));
        out.extend(self.keys.iter().filter(|h| h.is_modifier()).map(up));
        self.keys.clear();
        self.buttons.clear();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_C: Hid = Hid(0x07_0006);
    const KEY_A: Hid = Hid(0x07_0004);

    #[test]
    fn key_set_basics() {
        let mut s = KeySet::new();
        assert!(s.insert(KEY_C));
        assert!(!s.insert(KEY_C));
        assert!(s.insert(Hid::LEFT_CTRL));
        assert!(s.contains(KEY_C) && s.len() == 2);
        assert_eq!(s.modifier_mask(), ModifierMask(0b1));
        let other: KeySet = [KEY_C].into_iter().collect();
        assert_eq!(s.difference(&other), [Hid::LEFT_CTRL].into_iter().collect());
        assert!(s.remove(KEY_C) && !s.remove(KEY_C));
    }

    #[test]
    fn button_set_basics() {
        let mut b = ButtonSet::new();
        assert!(b.is_empty());
        assert!(b.insert(MouseButton::Right));
        assert!(!b.insert(MouseButton::Right));
        assert!(b.insert(MouseButton::Back));
        assert_eq!(
            b.iter().collect::<Vec<_>>(),
            vec![MouseButton::Right, MouseButton::Back]
        );
        assert!(b.remove(MouseButton::Right) && !b.remove(MouseButton::Right));
    }

    #[test]
    fn release_order_and_idempotence() {
        let mut p = Pressed::default();
        p.keys.insert(Hid::LEFT_CTRL);
        p.keys.insert(KEY_C);
        p.keys.insert(Hid::RIGHT_SHIFT);
        p.keys.insert(KEY_A);
        p.buttons.insert(MouseButton::Left);
        p.buttons.insert(MouseButton::Middle);
        let at = Point::new(5, 6);
        let key_up = |hid| InjectCommand::Key {
            hid,
            down: false,
            repeat: false,
        };
        assert_eq!(
            p.release_all(at),
            vec![
                InjectCommand::Button {
                    button: MouseButton::Left,
                    down: false,
                    at
                },
                InjectCommand::Button {
                    button: MouseButton::Middle,
                    down: false,
                    at
                },
                key_up(KEY_A),
                key_up(KEY_C),
                key_up(Hid::LEFT_CTRL),
                key_up(Hid::RIGHT_SHIFT),
            ]
        );
        assert!(p.is_empty());
        assert_eq!(p.release_all(at), vec![]);
    }
}
