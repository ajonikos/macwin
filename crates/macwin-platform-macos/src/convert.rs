//! CGEvent -> `CapturedEvent` conversion.

use macwin_core::keymap::mac_to_hid;
use macwin_core::os::macos::{
    FlagsChange, KVK_CAPS_LOCK, flags_changed, scroll_from_fixed, scroll_from_points,
};
use macwin_core::types::{CapturedEvent, MouseButton, PointF, WheelUnit};
use objc2_core_graphics::{CGEvent, CGEventField, CGEventMask, CGEventType};

/// Event types the tap listens to.
pub(crate) const TAPPED: [CGEventType; 14] = [
    CGEventType::MouseMoved,
    CGEventType::LeftMouseDown,
    CGEventType::LeftMouseUp,
    CGEventType::LeftMouseDragged,
    CGEventType::RightMouseDown,
    CGEventType::RightMouseUp,
    CGEventType::RightMouseDragged,
    CGEventType::OtherMouseDown,
    CGEventType::OtherMouseUp,
    CGEventType::OtherMouseDragged,
    CGEventType::ScrollWheel,
    CGEventType::KeyDown,
    CGEventType::KeyUp,
    CGEventType::FlagsChanged,
];

pub(crate) fn event_mask() -> CGEventMask {
    TAPPED.iter().fold(0, |m, t| m | (1u64 << t.0))
}

fn int(e: &CGEvent, f: CGEventField) -> i64 {
    CGEvent::integer_value_field(Some(e), f)
}

fn double(e: &CGEvent, f: CGEventField) -> f64 {
    CGEvent::double_value_field(Some(e), f)
}

fn pos(e: &CGEvent) -> PointF {
    let p = CGEvent::location(Some(e));
    PointF::new(p.x, p.y)
}

/// Button of an "other mouse" event from its button number.
fn other_button(e: &CGEvent) -> Option<MouseButton> {
    match int(e, CGEventField::MouseEventButtonNumber) {
        2 => Some(MouseButton::Middle),
        3 => Some(MouseButton::Back),
        4 => Some(MouseButton::Forward),
        _ => None,
    }
}

/// Appends the macwin events for one CGEvent (CapsLock yields two). Events
/// without a mapping (unknown keys, Fn, extra mouse buttons) yield none and
/// are delivered locally untouched.
pub(crate) fn convert(t: CGEventType, e: &CGEvent, out: &mut Vec<CapturedEvent>) {
    let button = |button, down| CapturedEvent::Button {
        button,
        down,
        pos: pos(e),
    };
    match t {
        CGEventType::MouseMoved
        | CGEventType::LeftMouseDragged
        | CGEventType::RightMouseDragged
        | CGEventType::OtherMouseDragged => out.push(CapturedEvent::MouseMove {
            pos: pos(e),
            delta: PointF::new(
                double(e, CGEventField::MouseEventDeltaX),
                double(e, CGEventField::MouseEventDeltaY),
            ),
        }),
        CGEventType::LeftMouseDown => out.push(button(MouseButton::Left, true)),
        CGEventType::LeftMouseUp => out.push(button(MouseButton::Left, false)),
        CGEventType::RightMouseDown => out.push(button(MouseButton::Right, true)),
        CGEventType::RightMouseUp => out.push(button(MouseButton::Right, false)),
        CGEventType::OtherMouseDown | CGEventType::OtherMouseUp => {
            if let Some(b) = other_button(e) {
                out.push(button(b, t == CGEventType::OtherMouseDown));
            }
        }
        CGEventType::ScrollWheel => {
            // Trackpads and the Magic Mouse scroll continuously, in pixels
            // (momentum included); wheels in lines.
            let (dx, dy, unit) = if int(e, CGEventField::ScrollWheelEventIsContinuous) != 0 {
                let (dx, dy) = scroll_from_points(
                    int(e, CGEventField::ScrollWheelEventPointDeltaAxis1),
                    int(e, CGEventField::ScrollWheelEventPointDeltaAxis2),
                );
                (dx, dy, WheelUnit::Pixel)
            } else {
                let (dx, dy) = scroll_from_fixed(
                    double(e, CGEventField::ScrollWheelEventFixedPtDeltaAxis1),
                    double(e, CGEventField::ScrollWheelEventFixedPtDeltaAxis2),
                );
                (dx, dy, WheelUnit::Notch120)
            };
            if dx != 0 || dy != 0 {
                out.push(CapturedEvent::Wheel { dx, dy, unit });
            }
        }
        CGEventType::KeyDown | CGEventType::KeyUp => {
            let code = int(e, CGEventField::KeyboardEventKeycode) as u16;
            if let Some(hid) = mac_to_hid(code) {
                out.push(CapturedEvent::Key {
                    hid,
                    down: t == CGEventType::KeyDown,
                    repeat: int(e, CGEventField::KeyboardEventAutorepeat) != 0,
                });
            }
        }
        CGEventType::FlagsChanged => {
            let code = int(e, CGEventField::KeyboardEventKeycode) as u16;
            let Some(hid) = mac_to_hid(code) else {
                return;
            };
            let key = |down| CapturedEvent::Key {
                hid,
                down,
                repeat: false,
            };
            match flags_changed(code, CGEvent::flags(Some(e)).0) {
                Some(FlagsChange::Down) => out.push(key(true)),
                Some(FlagsChange::Up) => out.push(key(false)),
                Some(FlagsChange::Tap) => {
                    debug_assert_eq!(code, KVK_CAPS_LOCK);
                    out.push(key(true));
                    out.push(key(false));
                }
                None => {}
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    //! Events are created but never posted: no permission is involved.

    use macwin_core::types::Hid;
    use objc2_core_foundation::CGPoint;
    use objc2_core_graphics::{CGEventFlags, CGMouseButton, CGScrollEventUnit};

    use super::*;

    fn converted(t: CGEventType, e: &CGEvent) -> Vec<CapturedEvent> {
        let mut out = Vec::new();
        convert(t, e, &mut out);
        out
    }

    #[test]
    fn mask_covers_every_tapped_type() {
        let m = event_mask();
        for t in TAPPED {
            assert_ne!(m & (1 << t.0), 0, "{t:?}");
        }
        assert_eq!(m & (1 << CGEventType::TabletPointer.0), 0);
    }

    #[test]
    fn keys_map_to_hid() {
        let e = CGEvent::new_keyboard_event(None, 0x00, true).unwrap(); // kVK_ANSI_A
        assert_eq!(
            converted(CGEventType::KeyDown, &e),
            vec![CapturedEvent::Key {
                hid: Hid(0x07_0004),
                down: true,
                repeat: false
            }]
        );
        CGEvent::set_integer_value_field(Some(&e), CGEventField::KeyboardEventAutorepeat, 1);
        assert_eq!(
            converted(CGEventType::KeyDown, &e),
            vec![CapturedEvent::Key {
                hid: Hid(0x07_0004),
                down: true,
                repeat: true
            }]
        );
        let unknown = CGEvent::new_keyboard_event(None, 0x7F, true).unwrap();
        assert!(converted(CGEventType::KeyDown, &unknown).is_empty());
    }

    #[test]
    fn modifier_flags_changes() {
        let shift = CGEvent::new_keyboard_event(None, 0x38, true).unwrap(); // kVK_Shift
        CGEvent::set_flags(Some(&shift), CGEventFlags(0x2 | 0x20000));
        assert_eq!(
            converted(CGEventType::FlagsChanged, &shift),
            vec![CapturedEvent::Key {
                hid: Hid::LEFT_SHIFT,
                down: true,
                repeat: false
            }]
        );
        CGEvent::set_flags(Some(&shift), CGEventFlags(0));
        assert_eq!(
            converted(CGEventType::FlagsChanged, &shift),
            vec![CapturedEvent::Key {
                hid: Hid::LEFT_SHIFT,
                down: false,
                repeat: false
            }]
        );
        let caps = CGEvent::new_keyboard_event(None, KVK_CAPS_LOCK, true).unwrap();
        let caps_hid = mac_to_hid(KVK_CAPS_LOCK).unwrap();
        assert_eq!(
            converted(CGEventType::FlagsChanged, &caps),
            vec![
                CapturedEvent::Key {
                    hid: caps_hid,
                    down: true,
                    repeat: false
                },
                CapturedEvent::Key {
                    hid: caps_hid,
                    down: false,
                    repeat: false
                },
            ]
        );
    }

    #[test]
    fn mouse_buttons_and_motion() {
        let at = CGPoint::new(100.0, 200.0);
        let down =
            CGEvent::new_mouse_event(None, CGEventType::LeftMouseDown, at, CGMouseButton::Left)
                .unwrap();
        assert_eq!(
            converted(CGEventType::LeftMouseDown, &down),
            vec![CapturedEvent::Button {
                button: MouseButton::Left,
                down: true,
                pos: PointF::new(100.0, 200.0)
            }]
        );
        let other =
            CGEvent::new_mouse_event(None, CGEventType::OtherMouseUp, at, CGMouseButton::Center)
                .unwrap();
        CGEvent::set_integer_value_field(Some(&other), CGEventField::MouseEventButtonNumber, 3);
        assert_eq!(
            converted(CGEventType::OtherMouseUp, &other),
            vec![CapturedEvent::Button {
                button: MouseButton::Back,
                down: false,
                pos: PointF::new(100.0, 200.0)
            }]
        );
        let moved =
            CGEvent::new_mouse_event(None, CGEventType::MouseMoved, at, CGMouseButton::Left)
                .unwrap();
        CGEvent::set_integer_value_field(Some(&moved), CGEventField::MouseEventDeltaX, 7);
        CGEvent::set_integer_value_field(Some(&moved), CGEventField::MouseEventDeltaY, -3);
        match converted(CGEventType::MouseMoved, &moved).as_slice() {
            [CapturedEvent::MouseMove { pos, delta }] => {
                assert_eq!(*pos, PointF::new(100.0, 200.0));
                assert_eq!((delta.x, delta.y), (7.0, -3.0));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn scroll_lines_become_notch_units() {
        let e =
            CGEvent::new_scroll_wheel_event2(None, CGScrollEventUnit::Line, 1, 1, 0, 0).unwrap();
        CGEvent::set_integer_value_field(Some(&e), CGEventField::ScrollWheelEventIsContinuous, 0);
        assert_eq!(
            converted(CGEventType::ScrollWheel, &e),
            vec![CapturedEvent::Wheel {
                dx: 0,
                dy: 120,
                unit: WheelUnit::Notch120
            }]
        );
    }

    #[test]
    fn continuous_scrolls_stay_in_pixels() {
        let e =
            CGEvent::new_scroll_wheel_event2(None, CGScrollEventUnit::Pixel, 2, -7, 3, 0).unwrap();
        CGEvent::set_integer_value_field(Some(&e), CGEventField::ScrollWheelEventIsContinuous, 1);
        assert_eq!(
            converted(CGEventType::ScrollWheel, &e),
            vec![CapturedEvent::Wheel {
                dx: -3,
                dy: -7,
                unit: WheelUnit::Pixel
            }]
        );
    }
}
